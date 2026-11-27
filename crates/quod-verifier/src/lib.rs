//! Standalone bounded verification daemon library for Quod (`quod-verifier`).
//! Claims pending `verification_jobs`, executes Stage 0 + Stage 1 (`LocalLeanRunner` or `BwrapRunner`),
//! persists verified `.olean` overlay modules into `accepted_olean/<project_id>/epoch_<epoch_id>/`,
//! and unlocks downstream `Blocked` tasks (such as `Merge` tasks).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::ValueEnum;
use quod_core::{AuditVerdict, SubmissionKind};
use quod_sandbox::{
    BwrapConfig, BwrapRunner, HarnessSpec, LocalLeanRunner, LocalVerifyRequest, SandboxRequest,
    SandboxRunner, Stage0Policy, check_stage0, join_lean_paths, module_olean_path,
    parse_audit_json, render_audit_harness, sha256_hex,
};
use quod_store::{BlobStore, QuodStore, VerificationOutcomeInput};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RunnerMode {
    Local,
    Bwrap,
}

#[derive(Debug, Clone)]
pub struct VerifierConfig {
    pub verifier_id: String,
    pub runner_mode: RunnerMode,
    pub lean_bin: PathBuf,
    pub spec_file: Option<PathBuf>,
    pub toolchain_dir: PathBuf,
    pub epoch_lean_paths: Vec<PathBuf>,
    pub accepted_olean_dir: PathBuf,
    pub timeout_secs: u64,
}

impl Default for VerifierConfig {
    fn default() -> Self {
        Self {
            verifier_id: "verifier-01".into(),
            runner_mode: RunnerMode::Local,
            lean_bin: PathBuf::from("lean"),
            spec_file: None,
            toolchain_dir: PathBuf::from("/opt/quod/lean"),
            epoch_lean_paths: vec![PathBuf::from("/opt/quod/epoch_lib")],
            accepted_olean_dir: PathBuf::from("/var/lib/quod/accepted_olean"),
            timeout_secs: 60,
        }
    }
}

fn dir_has_direct_olean(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .filter_map(|e| e.ok().map(|ent| ent.path()))
            .any(|p| p.is_file() && p.extension().is_some_and(|ext| ext == "olean"))
    })
}

fn dir_contains_package_olean(dir: &Path) -> bool {
    dir_has_direct_olean(dir)
        || std::fs::read_dir(dir).is_ok_and(|entries| {
            entries
                .filter_map(|e| e.ok().map(|ent| ent.path()))
                .any(|p| p.is_dir() && dir_has_direct_olean(&p))
        })
}

/// Expands Lean search root directories to also include immediate package subdirectories
/// (e.g. `/opt/quod/epoch_lib/mathlib`, `/opt/quod/epoch_lib/aesop`) that hold `.olean` artifacts.
pub fn expand_lean_search_paths(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        if !out.contains(root) {
            out.push(root.clone());
        }
        if let Ok(entries) = std::fs::read_dir(root) {
            let mut children: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|ent| ent.path()))
                .filter(|p| p.is_dir() && dir_contains_package_olean(p))
                .collect();
            children.sort();
            for child in children {
                if !out.contains(&child) {
                    out.push(child);
                }
            }
        }
    }
    out
}

fn persist_olean_file(src: &Path, dest: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_dest = dest.with_extension(format!("olean.tmp.{}", Uuid::now_v7().simple()));
    std::fs::copy(src, &tmp_dest)?;
    std::fs::rename(&tmp_dest, dest)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn ensure_spec_olean(
    store: &Arc<dyn QuodStore>,
    blobs: &Arc<dyn BlobStore>,
    cfg: &VerifierConfig,
    toolchain_dir: &Path,
    base_lean_paths: &[PathBuf],
    epoch_accepted_dir: &Path,
    project_id: &str,
    epoch_id: u32,
    spec_module: &str,
) -> anyhow::Result<bool> {
    if spec_module.is_empty() {
        return Ok(false);
    }
    let dest = module_olean_path(epoch_accepted_dir, spec_module);
    if dest.exists() || base_lean_paths.iter().any(|p| module_olean_path(p, spec_module).exists()) {
        return Ok(true);
    }

    let spec_src = if let Some(raw_or_sha) = store
        .get_overlay_blob(project_id, epoch_id, spec_module)
        .await?
    {
        if raw_or_sha.len() == 64 && raw_or_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            match blobs.get_bytes(&raw_or_sha) {
                Ok(bytes) => Some(String::from_utf8(bytes)?),
                Err(_) => cfg
                    .spec_file
                    .as_deref()
                    .filter(|p| p.exists())
                    .map(std::fs::read_to_string)
                    .transpose()?,
            }
        } else {
            Some(raw_or_sha)
        }
    } else if let Some(spec_path) = &cfg.spec_file {
        if spec_path.exists() {
            Some(std::fs::read_to_string(spec_path)?)
        } else {
            None
        }
    } else {
        None
    };

    let Some(code) = spec_src else {
        return Ok(false);
    };

    let tmp = tempfile::tempdir()?;
    let spec_file = tmp.path().join("Spec.lean");
    std::fs::write(&spec_file, &code)?;

    match cfg.runner_mode {
        RunnerMode::Bwrap => {
            let staging_dir = tmp.path().join("build");
            std::fs::create_dir_all(&staging_dir)?;
            let runner = BwrapRunner::new(BwrapConfig {
                toolchain_dir: toolchain_dir.to_path_buf(),
                epoch_lean_paths: base_lean_paths.to_vec(),
                accepted_olean_dir: Some(epoch_accepted_dir.to_path_buf()),
                output_olean_dir: Some(staging_dir.clone()),
                ..Default::default()
            });
            let outcome = runner
                .run(SandboxRequest {
                    task_id: format!("spec-epoch-{epoch_id}"),
                    candidate_module: spec_module.to_string(),
                    submission_file: spec_file,
                    timeout_secs: cfg.timeout_secs,
                    skip_harness: true,
                })
                .await?;
            if outcome.exit_code != 0 {
                anyhow::bail!(
                    "failed to compile Spec module '{spec_module}' in bwrap (exit={}): {}{}",
                    outcome.exit_code,
                    outcome.stdout,
                    outcome.stderr
                );
            }
            let staged = module_olean_path(&staging_dir, spec_module);
            if !staged.exists() {
                anyhow::bail!(
                    "Spec compilation exited 0 but staged .olean missing at {}",
                    staged.display()
                );
            }
            persist_olean_file(&staged, &dest)?;
        }
        RunnerMode::Local => {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp_dest = dest.with_extension(format!("olean.tmp.{}", Uuid::now_v7().simple()));
            let mut search_paths = base_lean_paths.to_vec();
            search_paths.push(epoch_accepted_dir.to_path_buf());
            let lean_path_env = join_lean_paths(&search_paths);

            let mut cmd = tokio::process::Command::new(&cfg.lean_bin);
            cmd.current_dir(tmp.path())
                .arg("-o")
                .arg(&tmp_dest)
                .arg(&spec_file)
                .env("LEAN_PATH", &lean_path_env)
                .kill_on_drop(true);
            let output = tokio::time::timeout(
                std::time::Duration::from_secs(cfg.timeout_secs.max(1)),
                cmd.output(),
            )
            .await??;
            if !output.status.success() {
                let _ = std::fs::remove_file(&tmp_dest);
                anyhow::bail!(
                    "failed to compile Spec module '{spec_module}' locally: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::fs::rename(&tmp_dest, &dest)?;
        }
    }

    info!(project_id, epoch_id, spec_module, "compiled and cached epoch Spec.olean");
    Ok(true)
}

/// Claims and processes at most one pending `VerificationJob`. Returns `Ok(true)` if a job ran.
pub async fn process_one_job(
    store: &Arc<dyn QuodStore>,
    blobs: &Arc<dyn BlobStore>,
    cfg: &VerifierConfig,
) -> anyhow::Result<bool> {
    let Some(job) = store.claim_next_verification_job(&cfg.verifier_id).await? else {
        return Ok(false);
    };

    let bytes = blobs.get_bytes(&job.blob_sha256)?;
    let code = String::from_utf8(bytes)?;

    let bundle = store.get_bundle(&job.bundle_id).await.ok().flatten();
    let epoch_accepted_dir = cfg
        .accepted_olean_dir
        .join(&job.project_id)
        .join(format!("epoch_{}", job.epoch_id));
    std::fs::create_dir_all(&epoch_accepted_dir)?;

    let mut raw_roots: Vec<PathBuf> = Vec::new();
    if let Some(b) = &bundle {
        for p in &b.lean_paths {
            let pb = PathBuf::from(p);
            if !raw_roots.contains(&pb) {
                raw_roots.push(pb);
            }
        }
    }
    for p in &cfg.epoch_lean_paths {
        if !raw_roots.contains(p) {
            raw_roots.push(p.clone());
        }
    }
    let base_lean_paths = expand_lean_search_paths(&raw_roots);

    let toolchain_dir = bundle
        .as_ref()
        .map(|b| PathBuf::from(&b.toolchain_dir))
        .filter(|p| p.exists())
        .unwrap_or_else(|| cfg.toolchain_dir.clone());

    let spec_olean_ready = ensure_spec_olean(
        store,
        blobs,
        cfg,
        &toolchain_dir,
        &base_lean_paths,
        &epoch_accepted_dir,
        &job.project_id,
        job.epoch_id,
        &job.spec_module,
    )
    .await?;

    let extra_imports: Vec<String> = job
        .curated_imports
        .iter()
        .filter(|m| *m != &job.spec_module && *m != &job.candidate_module)
        .cloned()
        .collect();

    let persist_olean_dir = match job.kind {
        SubmissionKind::Proof | SubmissionKind::DefOverlay => Some(epoch_accepted_dir.clone()),
        SubmissionKind::Refutation => None,
    };

    let verdict = match cfg.runner_mode {
        RunnerMode::Local => {
            let mut local_lean_paths = base_lean_paths;
            if epoch_accepted_dir.exists() && !local_lean_paths.contains(&epoch_accepted_dir) {
                local_lean_paths.push(epoch_accepted_dir.clone());
            }
            let runner = LocalLeanRunner::new(&cfg.lean_bin);
            runner
                .verify(&LocalVerifyRequest {
                    task_id: job.task_id.clone(),
                    kind: job.kind,
                    candidate_code: code,
                    candidate_module: job.candidate_module.clone(),
                    candidate_decl: job.candidate_decl.clone(),
                    spec_file: if spec_olean_ready {
                        None
                    } else {
                        cfg.spec_file.clone()
                    },
                    spec_module: job.spec_module.clone(),
                    spec_decl: job.spec_decl.clone(),
                    inline_target_prop: if job.target_inline {
                        Some(job.spec_statement.clone())
                    } else {
                        None
                    },
                    extra_imports,
                    lean_paths: local_lean_paths,
                    expected_spec_sha256: None,
                    actual_spec_sha256: None,
                    timeout_secs: cfg.timeout_secs,
                    stage0_only: false,
                    persist_olean_dir,
                })
                .await?
        }
        RunnerMode::Bwrap => {
            run_bwrap_job(
                cfg,
                toolchain_dir,
                base_lean_paths,
                epoch_accepted_dir,
                persist_olean_dir,
                &job.task_id,
                job.kind,
                &code,
                &job.candidate_module,
                &job.candidate_decl,
                &job.spec_module,
                &job.spec_decl,
                if job.target_inline {
                    Some(job.spec_statement.clone())
                } else {
                    None
                },
                extra_imports,
            )
            .await?
        }
    };

    info!(
        job_id = %job.job_id,
        submission_id = %job.submission_id,
        project_id = %job.project_id,
        task_id = %job.task_id,
        ok = verdict.ok,
        exit_code = verdict.exit_code,
        compile_ms = verdict.compile_ms,
        "verified submission"
    );

    let result = store
        .record_verification_outcome(VerificationOutcomeInput {
            job_id: job.job_id,
            submission_id: job.submission_id,
            verdict,
        })
        .await?;

    if !result.unlocked_tasks.is_empty() {
        info!(unlocked = ?result.unlocked_tasks, "unlocked downstream dependent tasks");
    }
    if result.project_proved {
        info!(project_id = %job.project_id, "project root goal PROVED");
    }

    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn run_bwrap_job(
    cfg: &VerifierConfig,
    toolchain_dir: PathBuf,
    epoch_lean_paths: Vec<PathBuf>,
    epoch_accepted_dir: PathBuf,
    persist_olean_dir: Option<PathBuf>,
    task_id: &str,
    kind: SubmissionKind,
    code: &str,
    candidate_module: &str,
    candidate_decl: &str,
    spec_module: &str,
    spec_decl: &str,
    inline_target_prop: Option<String>,
    extra_imports: Vec<String>,
) -> anyhow::Result<AuditVerdict> {
    let stage0 = check_stage0(code, None, &Stage0Policy::default());
    if !stage0.passed {
        return Ok(AuditVerdict {
            ok: false,
            exit_code: stage0.exit_code,
            kind,
            code_sha256: stage0.code_sha256,
            compile_ms: 0,
            diagnostics: stage0
                .violations
                .into_iter()
                .map(|v| format!("line {}: {}", v.line, v.detail))
                .collect(),
        });
    }

    let tmp = tempfile::tempdir()?;
    let staging_dir = tmp.path().join("build");
    std::fs::create_dir_all(&staging_dir)?;
    let sub_file = tmp.path().join("submission.lean");
    let harness_file = tmp.path().join("AuditHarness.lean");
    std::fs::write(&sub_file, code)?;
    let harness_src = render_audit_harness(&HarnessSpec {
        kind,
        spec_module: if kind == SubmissionKind::DefOverlay {
            String::new()
        } else {
            spec_module.to_string()
        },
        spec_decl: spec_decl.to_string(),
        inline_target_prop,
        extra_imports,
        candidate_module: candidate_module.to_string(),
        candidate_decl: candidate_decl.to_string(),
    });
    std::fs::write(&harness_file, harness_src)?;

    let runner = BwrapRunner::new(BwrapConfig {
        toolchain_dir,
        epoch_lean_paths,
        accepted_olean_dir: Some(epoch_accepted_dir),
        output_olean_dir: Some(staging_dir.clone()),
        harness_path: harness_file,
        ..Default::default()
    });
    let outcome = runner
        .run(SandboxRequest {
            task_id: task_id.to_string(),
            candidate_module: candidate_module.to_string(),
            submission_file: sub_file,
            timeout_secs: cfg.timeout_secs,
            skip_harness: false,
        })
        .await?;

    let mut ok = outcome.exit_code == 0 && parse_audit_json(&outcome.stdout);
    let mut exit_code = outcome.exit_code;
    let mut diagnostics = Vec::new();
    if !ok {
        for l in outcome.stdout.lines().chain(outcome.stderr.lines()) {
            let t = l.trim();
            if !t.is_empty() {
                diagnostics.push(t.to_string());
            }
        }
        warn!(task_id, exit_code = outcome.exit_code, "bwrap verification failed");
    } else if let Some(persist_dir) = persist_olean_dir {
        let staged = module_olean_path(&staging_dir, candidate_module);
        if !staged.exists() {
            ok = false;
            exit_code = quod_sandbox::EXIT_INFRA_ERROR;
            diagnostics.push(format!(
                "bwrap audit passed but staged .olean missing at {}",
                staged.display()
            ));
            warn!(task_id, candidate_module, "missing staged .olean after bwrap verification");
        } else {
            persist_olean_file(&staged, &module_olean_path(&persist_dir, candidate_module))?;
        }
    }

    Ok(AuditVerdict {
        ok,
        exit_code,
        kind,
        code_sha256: sha256_hex(code.as_bytes()),
        compile_ms: outcome.compile_ms,
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quod_core::{
        ApplyIrRequest, ClaimLeaseRequest, CreateEpochRequest, CreateProjectRequest, GraphIrOp,
        ProjectStatus, RegisterActorRequest, RegisterBundleRequest, Role, SubmitRequest,
        TaskRole, generate_keypair_hex,
    };
    use quod_store::{LocalFsBlobStore, MemoryStore, NewSubmissionInput};
    use uuid::Uuid;

    #[tokio::test]
    async fn process_jobs_end_to_end_def_overlay_lemmas_and_merge() {
        let store_concrete = MemoryStore::new();
        let store: Arc<dyn QuodStore> = Arc::new(store_concrete);
        let blob_tmp = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> = Arc::new(LocalFsBlobStore::new(blob_tmp.path()));

        let (_, pk_w) = generate_keypair_hex();
        store
            .register_actor(&RegisterActorRequest {
                actor_id: "w1".into(),
                pubkey_ed25519: pk_w,
                roles: vec![Role::Worker, Role::Orchestrator, Role::Blueprinter, Role::Admin],
                project_scopes: vec![],
                lineage_cluster: "test".into(),
                initial_tier: Some(3),
                skill_tags: vec![],
                display_name: None,
                git_email: None,
            })
            .await
            .unwrap();

        store
            .register_bundle(&RegisterBundleRequest {
                bundle_id: "lean-4.36.0".into(),
                lean_toolchain: "leanprover/lean4:v4.36.0".into(),
                mathlib_rev: "none".into(),
                toolchain_dir: String::new(),
                lean_paths: vec![],
                bundle_sha256: "b0".into(),
            })
            .await
            .unwrap();
        store
            .create_project(
                "w1",
                &CreateProjectRequest {
                    project_id: "smoke".into(),
                    title: "Smoke".into(),
                    description: "e2e".into(),
                    spec_namespace: "Quod.Epoch0".into(),
                    default_bundle_id: Some("lean-4.36.0".into()),
                },
            )
            .await
            .unwrap();
        store
            .create_epoch(
                "smoke",
                "w1",
                &CreateEpochRequest {
                    epoch_id: 1,
                    bundle_id: "lean-4.36.0".into(),
                    base_commit: "0000".into(),
                    spec_module: "Spec".into(),
                    spec_decl_name: "Quod.Epoch0.smoke_stmt".into(),
                    spec_statement: "∀ n : Nat, (n + 1) + (n + 1) = 2 * n + 2".into(),
                    spec_sha256: "sha".into(),
                    spec_code: None,
                    root_task_id: "root".into(),
                    auto_activate: true,
                },
            )
            .await
            .unwrap();

        let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("epoch0-smoke");

        let accepted_tmp = tempfile::tempdir().unwrap();
        let cfg = VerifierConfig {
            spec_file: Some(root_dir.join("Spec.lean")),
            accepted_olean_dir: accepted_tmp.path().to_path_buf(),
            timeout_secs: 180,
            ..Default::default()
        };

        // 1. Publish DefOverlay via IR
        let def_code = std::fs::read_to_string(root_dir.join("samples/def_overlay_ok.lean")).unwrap();
        store
            .apply_ir(
                "smoke",
                1,
                "w1",
                &ApplyIrRequest {
                    ops: vec![GraphIrOp::PublishDefOverlay {
                        module_name: "QuodDef_01".into(),
                        code_utf8: def_code,
                    }],
                    dry_run: false,
                },
                blobs.as_ref(),
            )
            .await
            .unwrap();
        assert!(process_one_job(&store, &blobs, &cfg).await.unwrap());

        // 2. Spawn Route A: lemma_a + merge_a (depending on lemma_a)
        store
            .apply_ir(
                "smoke",
                1,
                "w1",
                &ApplyIrRequest {
                    ops: vec![
                        GraphIrOp::CreateRoute {
                            route_id: "route-a".into(),
                            title: "Via doublePlusTwo".into(),
                            rationale: "Helper def + lemma + merge".into(),
                        },
                        GraphIrOp::SpawnTask {
                            task_id: "lemma_a".into(),
                            route_id: Some("route-a".into()),
                            task_role: TaskRole::Lemma,
                            spec_decl_name: "lemma_a".into(),
                            spec_statement: "∀ n : Nat, (n + 1) + (n + 1) = doublePlusTwo n".into(),
                            target_inline: true,
                            candidate_decl: "proof_lemma_a".into(),
                            curated_imports: vec!["QuodDef_01".into()],
                            depends_on: vec![],
                            refutes_task_id: None,
                            module_hint: None,
                            priority: 10.0,
                            prior_p: 0.7,
                            expected_cost_c: 1.0,
                            racer_cap_k: 2,
                            min_tier: 1,
                            preferred_lineages: vec![],
                            credit_bounty: None,
                        },
                        GraphIrOp::SpawnTask {
                            task_id: "merge_a".into(),
                            route_id: Some("route-a".into()),
                            task_role: TaskRole::Merge,
                            spec_decl_name: "Quod.Epoch0.smoke_stmt".into(),
                            spec_statement: String::new(),
                            target_inline: false,
                            candidate_decl: "candidate".into(),
                            curated_imports: vec!["QuodDef_01".into()],
                            depends_on: vec!["lemma_a".into()],
                            refutes_task_id: None,
                            module_hint: None,
                            priority: 20.0,
                            prior_p: 0.8,
                            expected_cost_c: 1.0,
                            racer_cap_k: 2,
                            min_tier: 1,
                            preferred_lineages: vec![],
                            credit_bounty: None,
                        },
                    ],
                    dry_run: false,
                },
                blobs.as_ref(),
            )
            .await
            .unwrap();

        // Claim & prove lemma_a
        let pkt_a = store
            .claim_lease("smoke", "w1", &ClaimLeaseRequest::default(), 60, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pkt_a.task_id, "lemma_a");
        let la_code = std::fs::read_to_string(root_dir.join("samples/lemma_a.lean")).unwrap();
        let la_sha = blobs.put_bytes(la_code.as_bytes()).unwrap();
        store
            .create_submission(NewSubmissionInput {
                submission_id: Uuid::now_v7(),
                project_id: "smoke".into(),
                actor_id: "w1".into(),
                req: SubmitRequest {
                    epoch_id: 1,
                    task_id: "lemma_a".into(),
                    lease_id: pkt_a.lease.unwrap().lease_id,
                    kind: SubmissionKind::Proof,
                    spec_sha256: "sha".into(),
                    code_utf8: la_code,
                    idea_attr_id: None,
                },
                blob_sha256: la_sha,
                received_at_ns: 1,
                stage0_passed: true,
                stage0_exit_code: 0,
                stage0_diagnostics: vec![],
            })
            .await
            .unwrap();
        assert!(process_one_job(&store, &blobs, &cfg).await.unwrap());

        // Merge task is now unlocked and carries lemma_a's overlay module!
        let pkt_m = store
            .claim_lease("smoke", "w1", &ClaimLeaseRequest::default(), 60, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pkt_m.task_id, "merge_a");
        let ov_mod = pkt_m
            .curated_imports
            .iter()
            .find(|m| m.starts_with("QuodOverlay_lemma_a_"))
            .unwrap()
            .clone();

        let merge_code = format!(
            "import Spec\nimport QuodDef_01\nimport {ov_mod}\ntheorem candidate : Quod.Epoch0.smoke_stmt := by\n  intro n\n  have h1 := proof_lemma_a n\n  dsimp [doublePlusTwo] at h1\n  omega\n"
        );
        let m_sha = blobs.put_bytes(merge_code.as_bytes()).unwrap();
        store
            .create_submission(NewSubmissionInput {
                submission_id: Uuid::now_v7(),
                project_id: "smoke".into(),
                actor_id: "w1".into(),
                req: SubmitRequest {
                    epoch_id: 1,
                    task_id: "merge_a".into(),
                    lease_id: pkt_m.lease.unwrap().lease_id,
                    kind: SubmissionKind::Proof,
                    spec_sha256: "sha".into(),
                    code_utf8: merge_code,
                    idea_attr_id: None,
                },
                blob_sha256: m_sha,
                received_at_ns: 2,
                stage0_passed: true,
                stage0_exit_code: 0,
                stage0_diagnostics: vec![],
            })
            .await
            .unwrap();
        assert!(process_one_job(&store, &blobs, &cfg).await.unwrap());

        let g = store.project_graph("smoke", 1).await.unwrap().unwrap();
        assert_eq!(g.project_status, ProjectStatus::Proved);
    }

    #[test]
    fn expand_lean_search_paths_discovers_package_subdirs() {
        let root = tempfile::tempdir().unwrap();
        let pkg_a = root.path().join("mathlib");
        let pkg_b = root.path().join("aesop");
        let empty_dir = root.path().join("docs");
        std::fs::create_dir_all(&pkg_a).unwrap();
        std::fs::create_dir_all(&pkg_b).unwrap();
        std::fs::create_dir_all(&empty_dir).unwrap();
        std::fs::write(pkg_a.join("Mathlib.olean"), b"dummy").unwrap();
        std::fs::write(pkg_b.join("Aesop.olean"), b"dummy").unwrap();

        let expanded = expand_lean_search_paths(&[root.path().to_path_buf()]);
        assert_eq!(
            expanded,
            vec![root.path().to_path_buf(), pkg_b, pkg_a]
        );
    }

    #[tokio::test]
    async fn process_job_auto_compiles_spec_from_overlay_blob_without_spec_file() {
        let store: Arc<dyn QuodStore> = Arc::new(MemoryStore::new());
        let blob_tmp = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> = Arc::new(LocalFsBlobStore::new(blob_tmp.path()));

        let (_, pk_w) = generate_keypair_hex();
        store
            .register_actor(&RegisterActorRequest {
                actor_id: "w1".into(),
                pubkey_ed25519: pk_w,
                roles: vec![Role::Worker, Role::Orchestrator, Role::Blueprinter, Role::Admin],
                project_scopes: vec![],
                lineage_cluster: "test".into(),
                initial_tier: Some(3),
                skill_tags: vec![],
                display_name: None,
                git_email: None,
            })
            .await
            .unwrap();
        store
            .register_bundle(&RegisterBundleRequest {
                bundle_id: "lean-4.36.0".into(),
                lean_toolchain: "leanprover/lean4:v4.36.0".into(),
                mathlib_rev: "none".into(),
                toolchain_dir: String::new(),
                lean_paths: vec![],
                bundle_sha256: "b0".into(),
            })
            .await
            .unwrap();
        store
            .create_project(
                "w1",
                &CreateProjectRequest {
                    project_id: "dyn-spec".into(),
                    title: "DynSpec".into(),
                    description: "auto-compile spec".into(),
                    spec_namespace: "Quod.Epoch0".into(),
                    default_bundle_id: Some("lean-4.36.0".into()),
                },
            )
            .await
            .unwrap();

        let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("epoch0-smoke");
        let spec_src = std::fs::read_to_string(root_dir.join("Spec.lean")).unwrap();
        let spec_sha = blobs.put_bytes(spec_src.as_bytes()).unwrap();
        store
            .create_epoch(
                "dyn-spec",
                "w1",
                &CreateEpochRequest {
                    epoch_id: 1,
                    bundle_id: "lean-4.36.0".into(),
                    base_commit: "0000".into(),
                    spec_module: "Spec".into(),
                    spec_decl_name: "Quod.Epoch0.smoke_stmt".into(),
                    spec_statement: "∀ n : Nat, (n + 1) + (n + 1) = 2 * n + 2".into(),
                    spec_sha256: spec_sha,
                    spec_code: Some(spec_src),
                    root_task_id: "root".into(),
                    auto_activate: true,
                },
            )
            .await
            .unwrap();

        let accepted_tmp = tempfile::tempdir().unwrap();
        let cfg = VerifierConfig {
            spec_file: None,
            accepted_olean_dir: accepted_tmp.path().to_path_buf(),
            timeout_secs: 180,
            ..Default::default()
        };

        let pkt = store
            .claim_lease("dyn-spec", "w1", &ClaimLeaseRequest::default(), 60, 1)
            .await
            .unwrap()
            .unwrap();
        let proof_code = std::fs::read_to_string(root_dir.join("samples/proof_ok.lean")).unwrap();
        let p_sha = blobs.put_bytes(proof_code.as_bytes()).unwrap();
        store
            .create_submission(NewSubmissionInput {
                submission_id: Uuid::now_v7(),
                project_id: "dyn-spec".into(),
                actor_id: "w1".into(),
                req: SubmitRequest {
                    epoch_id: 1,
                    task_id: "root".into(),
                    lease_id: pkt.lease.unwrap().lease_id,
                    kind: SubmissionKind::Proof,
                    spec_sha256: pkt.spec_sha256,
                    code_utf8: proof_code,
                    idea_attr_id: None,
                },
                blob_sha256: p_sha,
                received_at_ns: 1,
                stage0_passed: true,
                stage0_exit_code: 0,
                stage0_diagnostics: vec![],
            })
            .await
            .unwrap();

        assert!(process_one_job(&store, &blobs, &cfg).await.unwrap());
        assert!(
            accepted_tmp
                .path()
                .join("dyn-spec/epoch_1/Spec.olean")
                .exists()
        );
        let g = store.project_graph("dyn-spec", 1).await.unwrap().unwrap();
        assert_eq!(g.project_status, ProjectStatus::Proved);
    }
}
