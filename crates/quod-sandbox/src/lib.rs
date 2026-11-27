//! Stage 0 static policy verification, Stage 1 `AuditHarness` Lean 4.36.0 execution
//! (`LocalLeanRunner` and `bwrap` + cgroup v2 `BwrapRunner`) for Quod.

pub mod harness;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use quod_core::{AuditVerdict, SubmissionKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::process::Command;

pub use harness::{HarnessSpec, parse_audit_json, render_audit_harness};

pub const EXIT_OK: i32 = 0;
pub const EXIT_SPEC_DRIFT: i32 = 10;
pub const EXIT_SPEC_HASH_MISMATCH: i32 = 11;
pub const EXIT_KERNEL_FAILED: i32 = 20;
pub const EXIT_RESOURCE_LIMIT: i32 = 21;
pub const EXIT_FORBIDDEN_TOKEN: i32 = 30;
pub const EXIT_HYGIENE_FAILED: i32 = 31;
pub const EXIT_SIZE_LIMIT: i32 = 32;
pub const EXIT_INFRA_ERROR: i32 = 40;

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("failed to launch sandbox command: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("invalid identifier: {0}")]
    InvalidIdentifier(String),
}

/// Configurable Stage 0 static scanner policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage0Policy {
    pub max_bytes: usize,
    pub forbidden_tokens: Vec<String>,
    pub expected_spec_sha256: Option<String>,
}

impl Default for Stage0Policy {
    fn default() -> Self {
        Self {
            max_bytes: 262_144,
            forbidden_tokens: vec![
                "axiom",
                "opaque",
                "unsafe",
                "implemented_by",
                "extern",
                "native_decide",
                "sorry",
                "admit",
                "#eval",
                "#print",
                "#check",
                "elab",
                "macro_rules",
                "IO.",
                "System.",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            expected_spec_sha256: None,
        }
    }
}

/// Single diagnostic violation detected during Stage 0 or Spec import scanning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage0Violation {
    pub kind: String,
    pub detail: String,
    pub line: usize,
    pub snippet: String,
}

/// Structured output emitted by `check_stage0` and `quod verify --stage0-only`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage0Report {
    pub passed: bool,
    pub exit_code: i32,
    pub code_sha256: String,
    pub violations: Vec<Stage0Violation>,
}

fn is_lean_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'\''
}

fn line_contains_token(line: &str, token: &str) -> bool {
    let bytes = line.as_bytes();
    let tok_bytes = token.as_bytes();
    if tok_bytes.is_empty() || bytes.len() < tok_bytes.len() {
        return false;
    }

    let check_end_boundary = !token.ends_with('.');

    let mut idx = 0;
    while let Some(rel) = line[idx..].find(token) {
        let start = idx + rel;
        let end = start + tok_bytes.len();
        let left_ok = start == 0 || !is_lean_ident_char(bytes[start - 1]);
        let right_ok = !check_end_boundary || end == bytes.len() || !is_lean_ident_char(bytes[end]);
        if left_ok && right_ok {
            return true;
        }
        idx = start + 1;
    }
    false
}

/// Computes SHA-256 hex digest of UTF-8 source text.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Enforces the minimal-import invariant on L0–L2 Spec files (rejects blanket `import Mathlib`).
pub fn check_spec_minimal_imports(code: &str) -> Vec<Stage0Violation> {
    let mut violations = Vec::new();
    for (idx, raw) in code.lines().enumerate() {
        let stripped = raw.split("--").next().unwrap_or("").trim();
        if stripped == "import Mathlib" {
            violations.push(Stage0Violation {
                kind: "blanket_mathlib_import".into(),
                detail: "Spec files must use minimal fine-grained Mathlib imports instead of blanket `import Mathlib`".into(),
                line: idx + 1,
                snippet: raw.trim().to_string(),
            });
        }
    }
    violations
}

/// Executes Stage 0 static hygiene, size, spec-hash, and forbidden-token verification.
pub fn check_stage0(
    code: &str,
    actual_spec_sha256: Option<&str>,
    policy: &Stage0Policy,
) -> Stage0Report {
    let code_sha256 = sha256_hex(code.as_bytes());
    let mut violations = Vec::new();
    let mut exit_code = EXIT_OK;

    if let Some(expected) = &policy.expected_spec_sha256
        && actual_spec_sha256 != Some(expected.as_str())
    {
        exit_code = EXIT_SPEC_HASH_MISMATCH;
        violations.push(Stage0Violation {
            kind: "spec_hash_mismatch".into(),
            detail: format!("expected spec_sha256={expected}, got {actual_spec_sha256:?}"),
            line: 0,
            snippet: String::new(),
        });
    }

    if code.len() > policy.max_bytes {
        if exit_code == EXIT_OK {
            exit_code = EXIT_SIZE_LIMIT;
        }
        violations.push(Stage0Violation {
            kind: "size_limit".into(),
            detail: format!(
                "submission length {} bytes exceeds limit {} bytes",
                code.len(),
                policy.max_bytes
            ),
            line: 0,
            snippet: String::new(),
        });
    }

    for (line_idx, raw_line) in code.lines().enumerate() {
        for token in &policy.forbidden_tokens {
            if line_contains_token(raw_line, token) {
                if exit_code == EXIT_OK {
                    exit_code = EXIT_FORBIDDEN_TOKEN;
                }
                violations.push(Stage0Violation {
                    kind: "forbidden_token".into(),
                    detail: format!("forbidden token `{token}`"),
                    line: line_idx + 1,
                    snippet: raw_line.trim().to_string(),
                });
            }
        }
    }

    Stage0Report {
        passed: violations.is_empty(),
        exit_code,
        code_sha256,
        violations,
    }
}

/// Request passed to a Stage 1 `SandboxRunner` (`BwrapRunner`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRequest {
    pub task_id: String,
    pub candidate_module: String,
    pub submission_file: PathBuf,
    pub timeout_secs: u64,
}

/// Result returned by Stage 1 sandboxed Lean kernel execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxOutcome {
    pub exit_code: i32,
    pub compile_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

/// Abstraction over Stage 1 isolated verification backends (`bwrap`, container, etc.).
pub trait SandboxRunner: Send + Sync {
    fn run(
        &self,
        req: SandboxRequest,
    ) -> Pin<Box<dyn Future<Output = Result<SandboxOutcome, SandboxError>> + Send + '_>>;
}

/// Host paths and cgroup v2 resource constraints for `BwrapRunner`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BwrapConfig {
    pub toolchain_dir: PathBuf,
    pub epoch_lean_paths: Vec<PathBuf>,
    pub accepted_olean_dir: Option<PathBuf>,
    pub harness_path: PathBuf,
    pub cpu_quota: String,
    pub memory_max: String,
    pub tasks_max: u32,
}

impl Default for BwrapConfig {
    fn default() -> Self {
        Self {
            toolchain_dir: PathBuf::from("/opt/quod/lean"),
            epoch_lean_paths: vec![PathBuf::from("/opt/quod/epoch_lib")],
            accepted_olean_dir: Some(PathBuf::from("/opt/quod/accepted_lib")),
            harness_path: PathBuf::from("/opt/quod/harness/AuditHarness.lean"),
            cpu_quota: "200%".into(),
            memory_max: "6G".into(),
            tasks_max: 64,
        }
    }
}

/// `systemd-run --user --scope` + `timeout` + `bwrap` zero-trust runner.
#[derive(Debug, Clone)]
pub struct BwrapRunner {
    config: BwrapConfig,
}

impl BwrapRunner {
    pub fn new(config: BwrapConfig) -> Self {
        Self { config }
    }

    /// Builds exact argv passed to `systemd-run`.
    pub fn build_command_args(&self, req: &SandboxRequest) -> Result<Vec<String>, SandboxError> {
        if req.task_id.is_empty()
            || !req
                .task_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(SandboxError::InvalidIdentifier(req.task_id.clone()));
        }
        if req.candidate_module.is_empty()
            || !req
                .candidate_module
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
            || req.candidate_module.starts_with('.')
            || req.candidate_module.ends_with('.')
            || req.candidate_module.contains("..")
        {
            return Err(SandboxError::InvalidIdentifier(
                req.candidate_module.clone(),
            ));
        }

        let candidate_rel = req.candidate_module.replace('.', "/");
        let target_in_sandbox = format!("/work/{candidate_rel}.lean");
        let olean_out = format!("/work/build/{candidate_rel}.olean");
        let olean_parent = Path::new(&olean_out)
            .parent()
            .unwrap_or_else(|| Path::new("/work/build"))
            .display()
            .to_string();
        let shell_script = format!(
            "mkdir -p {olean_parent} && lean -o {olean_out} {target_in_sandbox} && lean /opt/harness/AuditHarness.lean"
        );

        let mut args = vec![
            "--user".into(),
            "--scope".into(),
            "--quiet".into(),
            "-p".into(),
            format!("CPUQuota={}", self.config.cpu_quota),
            "-p".into(),
            format!("MemoryMax={}", self.config.memory_max),
            "-p".into(),
            format!("TasksMax={}", self.config.tasks_max),
            "timeout".into(),
            "--kill-after=5s".into(),
            format!("{}s", req.timeout_secs.max(1)),
            "bwrap".into(),
            "--unshare-all".into(),
            "--die-with-parent".into(),
            "--clearenv".into(),
            "--ro-bind".into(),
            "/usr".into(),
            "/usr".into(),
            "--ro-bind".into(),
            "/bin".into(),
            "/bin".into(),
            "--ro-bind".into(),
            "/lib".into(),
            "/lib".into(),
            "--ro-bind".into(),
            "/lib64".into(),
            "/lib64".into(),
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
            "--ro-bind".into(),
            self.config.toolchain_dir.display().to_string(),
            "/opt/lean".into(),
        ];

        let mut sandbox_lean_paths = Vec::new();
        for (idx, host_dir) in self.config.epoch_lean_paths.iter().enumerate() {
            let mount_point = format!("/opt/epoch_lib/p{idx}");
            args.extend([
                "--ro-bind".into(),
                host_dir.display().to_string(),
                mount_point.clone(),
            ]);
            sandbox_lean_paths.push(mount_point);
        }

        if let Some(accepted) = &self.config.accepted_olean_dir {
            args.extend([
                "--ro-bind".into(),
                accepted.display().to_string(),
                "/opt/accepted_lib".into(),
            ]);
            sandbox_lean_paths.push("/opt/accepted_lib".into());
        }
        sandbox_lean_paths.push("/work/build".into());
        let lean_path = sandbox_lean_paths.join(":");

        args.extend([
            "--ro-bind".into(),
            self.config.harness_path.display().to_string(),
            "/opt/harness/AuditHarness.lean".into(),
            "--tmpfs".into(),
            "/work".into(),
            "--ro-bind".into(),
            req.submission_file.display().to_string(),
            target_in_sandbox,
            "--chdir".into(),
            "/work".into(),
            "--setenv".into(),
            "PATH".into(),
            "/opt/lean/bin:/usr/bin".into(),
            "--setenv".into(),
            "LEAN_PATH".into(),
            lean_path,
            "/bin/sh".into(),
            "-c".into(),
            shell_script,
        ]);

        Ok(args)
    }
}

impl SandboxRunner for BwrapRunner {
    fn run(
        &self,
        req: SandboxRequest,
    ) -> Pin<Box<dyn Future<Output = Result<SandboxOutcome, SandboxError>> + Send + '_>> {
        Box::pin(async move {
            let args = self.build_command_args(&req)?;
            let started = Instant::now();
            let output = Command::new("systemd-run").args(&args).output().await?;
            let compile_ms = started.elapsed().as_millis() as u64;
            let raw_code = output.status.code().unwrap_or(EXIT_INFRA_ERROR);
            let mapped_code = match raw_code {
                0 => EXIT_OK,
                124 | 137 => EXIT_RESOURCE_LIMIT,
                EXIT_SPEC_DRIFT | EXIT_SPEC_HASH_MISMATCH | EXIT_KERNEL_FAILED => raw_code,
                _ => EXIT_KERNEL_FAILED,
            };
            Ok(SandboxOutcome {
                exit_code: mapped_code,
                compile_ms,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        })
    }
}

/// Request executed by [`LocalLeanRunner`] (used by `quod verify` and local `quod-verifier`).
#[derive(Debug, Clone)]
pub struct LocalVerifyRequest {
    pub task_id: String,
    pub kind: SubmissionKind,
    pub candidate_code: String,
    pub candidate_module: String,
    pub candidate_decl: String,
    pub spec_file: Option<PathBuf>,
    pub spec_module: String,
    pub spec_decl: String,
    pub inline_target_prop: Option<String>,
    pub extra_imports: Vec<String>,
    pub lean_paths: Vec<PathBuf>,
    pub expected_spec_sha256: Option<String>,
    pub actual_spec_sha256: Option<String>,
    pub timeout_secs: u64,
    pub stage0_only: bool,
    /// Optional path to persist compiled candidate `.olean` upon passing Stage 1.
    pub persist_olean_dir: Option<PathBuf>,
}

impl Default for LocalVerifyRequest {
    fn default() -> Self {
        Self {
            task_id: "local".into(),
            kind: SubmissionKind::Proof,
            candidate_code: String::new(),
            candidate_module: "Candidate".into(),
            candidate_decl: "candidate".into(),
            spec_file: None,
            spec_module: "Spec".into(),
            spec_decl: "Spec.target".into(),
            inline_target_prop: None,
            extra_imports: Vec::new(),
            lean_paths: Vec::new(),
            expected_spec_sha256: None,
            actual_spec_sha256: None,
            timeout_secs: 60,
            stage0_only: false,
            persist_olean_dir: None,
        }
    }
}

/// Executes Stage 0 checks followed by real local `lean` kernel compilation and `AuditHarness`.
#[derive(Debug, Clone)]
pub struct LocalLeanRunner {
    pub lean_bin: PathBuf,
    pub policy: Stage0Policy,
}

impl Default for LocalLeanRunner {
    fn default() -> Self {
        Self {
            lean_bin: PathBuf::from("lean"),
            policy: Stage0Policy::default(),
        }
    }
}

impl LocalLeanRunner {
    pub fn new(lean_bin: impl Into<PathBuf>) -> Self {
        Self {
            lean_bin: lean_bin.into(),
            policy: Stage0Policy::default(),
        }
    }

    pub async fn verify(&self, req: &LocalVerifyRequest) -> Result<AuditVerdict, SandboxError> {
        let mut policy = self.policy.clone();
        policy.expected_spec_sha256 = req.expected_spec_sha256.clone();

        let stage0 = check_stage0(&req.candidate_code, req.actual_spec_sha256.as_deref(), &policy);
        if !stage0.passed || req.stage0_only {
            let diagnostics = stage0
                .violations
                .into_iter()
                .map(|v| format!("line {}: {} ({})", v.line, v.detail, v.snippet))
                .collect();
            return Ok(AuditVerdict {
                ok: stage0.passed,
                exit_code: stage0.exit_code,
                kind: req.kind,
                code_sha256: stage0.code_sha256,
                compile_ms: 0,
                diagnostics,
            });
        }

        let tmp = tempfile::tempdir()?;
        let build_dir = tmp.path().join("build");
        std::fs::create_dir_all(&build_dir)?;

        let mut search_paths: Vec<PathBuf> = vec![build_dir.clone()];
        search_paths.extend(req.lean_paths.iter().cloned());
        let lean_path_env = join_lean_paths(&search_paths);

        let started = Instant::now();
        let timeout = Duration::from_secs(req.timeout_secs.max(1));

        // 1. Copy & compile spec_file into build/<spec_module>.olean if provided.
        if let Some(spec_path) = &req.spec_file {
            let local_spec = module_lean_path(tmp.path(), &req.spec_module);
            if let Some(parent) = local_spec.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(spec_path, &local_spec)?;
            let spec_olean = module_olean_path(&build_dir, &req.spec_module);
            if let Some(parent) = spec_olean.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if let Some(err_verdict) = self
                .run_lean_step(
                    tmp.path(),
                    &[
                        "-o",
                        &spec_olean.to_string_lossy(),
                        &local_spec.to_string_lossy(),
                    ],
                    &lean_path_env,
                    timeout,
                    req.kind,
                    &stage0.code_sha256,
                    started,
                )
                .await?
            {
                return Ok(err_verdict);
            }
        }

        // 2. Write & compile candidate source into build/<candidate_module>.olean.
        let candidate_src = module_lean_path(tmp.path(), &req.candidate_module);
        if let Some(parent) = candidate_src.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&candidate_src, &req.candidate_code)?;
        let candidate_olean = module_olean_path(&build_dir, &req.candidate_module);
        if let Some(parent) = candidate_olean.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(err_verdict) = self
            .run_lean_step(
                tmp.path(),
                &[
                    "-o",
                    &candidate_olean.to_string_lossy(),
                    &candidate_src.to_string_lossy(),
                ],
                &lean_path_env,
                timeout,
                req.kind,
                &stage0.code_sha256,
                started,
            )
            .await?
        {
            return Ok(err_verdict);
        }

        // 3. Render & execute AuditHarness.lean against compiled candidate.
        let harness_src = render_audit_harness(&HarnessSpec {
            kind: req.kind,
            spec_module: if req.kind == SubmissionKind::DefOverlay {
                String::new()
            } else {
                req.spec_module.clone()
            },
            spec_decl: req.spec_decl.clone(),
            inline_target_prop: req.inline_target_prop.clone(),
            extra_imports: req.extra_imports.clone(),
            candidate_module: req.candidate_module.clone(),
            candidate_decl: req.candidate_decl.clone(),
        });
        let harness_path = tmp.path().join("AuditHarness.lean");
        std::fs::write(&harness_path, harness_src)?;

        let mut cmd = Command::new(&self.lean_bin);
        cmd.current_dir(tmp.path())
            .arg(&harness_path)
            .env("LEAN_PATH", &lean_path_env)
            .kill_on_drop(true);

        let output = match tokio::time::timeout(timeout, cmd.output()).await {
            Ok(res) => res?,
            Err(_) => {
                return Ok(AuditVerdict {
                    ok: false,
                    exit_code: EXIT_RESOURCE_LIMIT,
                    kind: req.kind,
                    code_sha256: stage0.code_sha256,
                    compile_ms: started.elapsed().as_millis() as u64,
                    diagnostics: vec![format!("Lean AuditHarness timed out after {}s", timeout.as_secs())],
                });
            }
        };

        let compile_ms = started.elapsed().as_millis() as u64;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if !output.status.success() {
            let mut diags = Vec::new();
            for line in stdout.lines().chain(stderr.lines()) {
                let t = line.trim();
                if !t.is_empty() {
                    diags.push(t.to_string());
                }
            }
            let exit_code = if diags.iter().any(|d| d.contains("QUOD_AUDIT_EMPTY_OVERLAY")) {
                EXIT_HYGIENE_FAILED
            } else {
                EXIT_KERNEL_FAILED
            };
            return Ok(AuditVerdict {
                ok: false,
                exit_code,
                kind: req.kind,
                code_sha256: stage0.code_sha256,
                compile_ms,
                diagnostics: diags,
            });
        }

        if !parse_audit_json(&stdout) {
            return Ok(AuditVerdict {
                ok: false,
                exit_code: EXIT_INFRA_ERROR,
                kind: req.kind,
                code_sha256: stage0.code_sha256,
                compile_ms,
                diagnostics: vec![format!("missing QUOD_AUDIT_JSON in stdout: {stdout}")],
            });
        }

        if let Some(persist_dir) = &req.persist_olean_dir {
            let dest = module_olean_path(persist_dir, &req.candidate_module);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&candidate_olean, &dest)?;
        }

        Ok(AuditVerdict {
            ok: true,
            exit_code: EXIT_OK,
            kind: req.kind,
            code_sha256: stage0.code_sha256,
            compile_ms,
            diagnostics: Vec::new(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_lean_step(
        &self,
        work_dir: &Path,
        args: &[&str],
        lean_path: &str,
        timeout: Duration,
        kind: SubmissionKind,
        code_sha256: &str,
        started: Instant,
    ) -> Result<Option<AuditVerdict>, SandboxError> {
        let mut cmd = Command::new(&self.lean_bin);
        cmd.current_dir(work_dir)
            .args(args)
            .env("LEAN_PATH", lean_path)
            .kill_on_drop(true);
        let output = match tokio::time::timeout(timeout, cmd.output()).await {
            Ok(res) => res?,
            Err(_) => {
                return Ok(Some(AuditVerdict {
                    ok: false,
                    exit_code: EXIT_RESOURCE_LIMIT,
                    kind,
                    code_sha256: code_sha256.to_string(),
                    compile_ms: started.elapsed().as_millis() as u64,
                    diagnostics: vec![format!(
                        "Lean compilation timed out after {}s",
                        timeout.as_secs()
                    )],
                }));
            }
        };
        if output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let diagnostics = stdout
            .lines()
            .chain(stderr.lines())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        Ok(Some(AuditVerdict {
            ok: false,
            exit_code: EXIT_KERNEL_FAILED,
            kind,
            code_sha256: code_sha256.to_string(),
            compile_ms: started.elapsed().as_millis() as u64,
            diagnostics,
        }))
    }
}

fn module_lean_path(root: &Path, module: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in module.split('.') {
        p.push(seg);
    }
    p.set_extension("lean");
    p
}

fn module_olean_path(root: &Path, module: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in module.split('.') {
        p.push(seg);
    }
    p.set_extension("olean");
    p
}

fn join_lean_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch0_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("epoch0-smoke")
    }

    #[test]
    fn stage0_and_minimal_import_checks() {
        let policy = Stage0Policy {
            expected_spec_sha256: Some("abc".into()),
            ..Default::default()
        };

        let clean = "theorem not_sorry_lemma : 1 + 1 = 2 := by decide\n";
        let ok = check_stage0(clean, Some("abc"), &policy);
        assert!(ok.passed);
        assert_eq!(ok.exit_code, EXIT_OK);

        let dirty = "axiom cheat : False\ntheorem t : False := by sorry\n";
        let bad = check_stage0(dirty, Some("abc"), &policy);
        assert!(!bad.passed);
        assert_eq!(bad.exit_code, EXIT_FORBIDDEN_TOKEN);
        assert_eq!(bad.violations.len(), 2);

        let drift = check_stage0(clean, Some("wrong"), &policy);
        assert!(!drift.passed);
        assert_eq!(drift.exit_code, EXIT_SPEC_HASH_MISMATCH);

        assert!(!check_spec_minimal_imports("import Mathlib\ndef x := 1\n").is_empty());
        assert!(check_spec_minimal_imports("import Mathlib.Data.Int.ModEq\ndef x := 1\n").is_empty());
    }

    #[test]
    fn bwrap_command_builds_expected_isolation_flags() {
        let runner = BwrapRunner::new(BwrapConfig {
            epoch_lean_paths: vec![PathBuf::from("/opt/m1"), PathBuf::from("/opt/m2")],
            ..Default::default()
        });
        let args = runner
            .build_command_args(&SandboxRequest {
                task_id: "task_001".into(),
                candidate_module: "QuodCand_1".into(),
                submission_file: PathBuf::from("/tmp/task_001.lean"),
                timeout_secs: 45,
            })
            .unwrap();
        assert!(args.contains(&"bwrap".to_string()));
        assert!(args.contains(&"--unshare-all".to_string()));
        assert!(args.contains(&"/work/QuodCand_1.lean".to_string()));
        assert!(args.contains(
            &"/opt/epoch_lib/p0:/opt/epoch_lib/p1:/opt/accepted_lib:/work/build".to_string()
        ));
    }

    #[tokio::test]
    async fn local_lean_runner_audits_overlay_and_merge_flow() {
        let root = epoch0_dir();
        let spec_file = root.join("Spec.lean");
        let runner = LocalLeanRunner::default();
        let overlay_dir = tempfile::tempdir().unwrap();

        // 1. Direct proof
        let proof_code = std::fs::read_to_string(root.join("samples/proof_ok.lean")).unwrap();
        let res = runner
            .verify(&LocalVerifyRequest {
                task_id: "root".into(),
                kind: SubmissionKind::Proof,
                candidate_code: proof_code,
                spec_file: Some(spec_file.clone()),
                spec_module: "Spec".into(),
                spec_decl: "Quod.Epoch0.smoke_stmt".into(),
                timeout_secs: 180,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(res.ok, "proof_ok failed: {:?}", res.diagnostics);

        // 2. Refutation
        let ref_code = std::fs::read_to_string(root.join("samples/refutation_ok.lean")).unwrap();
        let res = runner
            .verify(&LocalVerifyRequest {
                task_id: "false_t".into(),
                kind: SubmissionKind::Refutation,
                candidate_code: ref_code,
                spec_file: Some(spec_file.clone()),
                spec_module: "Spec".into(),
                spec_decl: "Quod.Epoch0.false_stmt".into(),
                timeout_secs: 180,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(res.ok, "refutation_ok failed: {:?}", res.diagnostics);

        // 3. DefOverlay -> Lemma A + Lemma B -> Merge Task
        let def_code =
            std::fs::read_to_string(root.join("samples/def_overlay_ok.lean")).unwrap();
        let def_res = runner
            .verify(&LocalVerifyRequest {
                task_id: "def_01".into(),
                kind: SubmissionKind::DefOverlay,
                candidate_code: def_code,
                candidate_module: "QuodDef_01".into(),
                persist_olean_dir: Some(overlay_dir.path().to_path_buf()),
                timeout_secs: 180,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(def_res.ok, "def_overlay_ok failed: {:?}", def_res.diagnostics);

        let lemma_a = std::fs::read_to_string(root.join("samples/lemma_a.lean")).unwrap();
        let la_res = runner
            .verify(&LocalVerifyRequest {
                task_id: "lemma_a".into(),
                kind: SubmissionKind::Proof,
                candidate_code: lemma_a,
                candidate_module: "QuodOverlay_a".into(),
                candidate_decl: "proof_lemma_a".into(),
                spec_module: String::new(),
                inline_target_prop: Some(
                    "∀ n : Nat, (n + 1) + (n + 1) = doublePlusTwo n".into(),
                ),
                extra_imports: vec!["QuodDef_01".into()],
                lean_paths: vec![overlay_dir.path().to_path_buf()],
                persist_olean_dir: Some(overlay_dir.path().to_path_buf()),
                timeout_secs: 180,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(la_res.ok, "lemma_a failed: {:?}", la_res.diagnostics);

        let lemma_b = std::fs::read_to_string(root.join("samples/lemma_b.lean")).unwrap();
        let lb_res = runner
            .verify(&LocalVerifyRequest {
                task_id: "lemma_b".into(),
                kind: SubmissionKind::Proof,
                candidate_code: lemma_b,
                candidate_module: "QuodOverlay_b".into(),
                candidate_decl: "proof_lemma_b".into(),
                spec_module: String::new(),
                inline_target_prop: Some("∀ n : Nat, doublePlusTwo n = 2 * n + 2".into()),
                extra_imports: vec!["QuodDef_01".into()],
                lean_paths: vec![overlay_dir.path().to_path_buf()],
                persist_olean_dir: Some(overlay_dir.path().to_path_buf()),
                timeout_secs: 180,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(lb_res.ok, "lemma_b failed: {:?}", lb_res.diagnostics);

        let merge_code = std::fs::read_to_string(root.join("samples/merge_ok.lean")).unwrap();
        let merge_res = runner
            .verify(&LocalVerifyRequest {
                task_id: "merge_root".into(),
                kind: SubmissionKind::Proof,
                candidate_code: merge_code,
                candidate_module: "QuodOverlay_merge".into(),
                candidate_decl: "candidate".into(),
                spec_file: Some(spec_file.clone()),
                spec_module: "Spec".into(),
                spec_decl: "Quod.Epoch0.smoke_stmt".into(),
                extra_imports: vec![
                    "QuodDef_01".into(),
                    "QuodOverlay_a".into(),
                    "QuodOverlay_b".into(),
                ],
                lean_paths: vec![overlay_dir.path().to_path_buf()],
                timeout_secs: 180,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(merge_res.ok, "merge_ok failed: {:?}", merge_res.diagnostics);

        // 4. Sorry blocked at Stage 0
        let sorry_code = std::fs::read_to_string(root.join("samples/bad_sorry.lean")).unwrap();
        let sorry_res = runner
            .verify(&LocalVerifyRequest {
                task_id: "root".into(),
                kind: SubmissionKind::Proof,
                candidate_code: sorry_code,
                spec_file: Some(spec_file),
                spec_module: "Spec".into(),
                spec_decl: "Quod.Epoch0.smoke_stmt".into(),
                timeout_secs: 180,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!sorry_res.ok);
        assert_eq!(sorry_res.exit_code, EXIT_FORBIDDEN_TOKEN);
    }
}
