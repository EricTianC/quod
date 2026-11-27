//! `quod` — unified Agent-First CLI for `I0` Workers, `I1` Orchestrators,
//! `I2-B` Blueprinters, `I2-A` Auditors, and Admins on Quod.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use quod_core::{
    AlgoQueryResult, ApplyIrRequest, ApplyIrResponse, AuditEpochRequest, ClaimLeaseRequest,
    CreateEpochRequest, CreateInviteRequest, CreateProjectRequest, EpochStatusView,
    HEADER_ACTOR_ID, HEADER_ADMIN_TOKEN, HEADER_SIGNATURE, HEADER_TIMESTAMP, JoinActorRequest,
    LeaderboardEntryView, OverlaySourceView, ProjectGraphView, ProjectSummaryView,
    RegisterActorRequest, RegisterBundleRequest, Role, SubmissionKind, SubmissionState,
    SubmissionStatusView, SubmitRequest, SubmitResponse, TaskPacket, WakeupDigestView,
    WhoAmIResponse, generate_keypair_hex, public_key_from_secret_hex, sign_request,
};
use quod_sandbox::{
    LocalLeanRunner, LocalVerifyRequest, check_spec_minimal_imports, sha256_hex,
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum CliKind {
    Proof,
    Refutation,
    DefOverlay,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum CliSkill {
    Router,
    Worker,
    Orchestrator,
    Auditor,
    Blueprinter,
}

const SKILL_ROUTER: &str = include_str!("../../../skills/quod/SKILL.md");
const SKILL_WORKER: &str = include_str!("../../../skills/quod-worker/SKILL.md");
const SKILL_WORKER_CLI_REF: &str =
    include_str!("../../../skills/quod-worker/references/cli-reference.md");
const SKILL_WORKER_EXIT_CODES: &str =
    include_str!("../../../skills/quod-worker/references/exit-codes.md");
const SKILL_WORKER_TASK_GUIDE: &str =
    include_str!("../../../skills/quod-worker/references/task-guide.md");
const SKILL_ORCHESTRATOR: &str = include_str!("../../../skills/quod-orchestrator/SKILL.md");
const SKILL_AUDITOR: &str = include_str!("../../../skills/quod-auditor/SKILL.md");
const SKILL_BLUEPRINTER: &str = include_str!("../../../skills/quod-blueprinter/SKILL.md");

impl From<CliKind> for SubmissionKind {
    fn from(value: CliKind) -> Self {
        match value {
            CliKind::Proof => SubmissionKind::Proof,
            CliKind::Refutation => SubmissionKind::Refutation,
            CliKind::DefOverlay => SubmissionKind::DefOverlay,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "quod",
    version,
    about = "Quod Agent-First Lean 4 collaboration CLI"
)]
struct Cli {
    /// Coordinator base URL (e.g., http://127.0.0.1:8080).
    #[arg(long, env = "QUOD_HUB_URL", default_value = "http://127.0.0.1:8080")]
    hub_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Generate an Ed25519 keypair JSON file for a Quod actor.
    Keygen {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Probe authorized roles and active projects for an actor keypair (`GET /api/whoami`).
    Whoami {
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: String,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: PathBuf,
    },
    /// List registered mathematical projects (`GET /api/projects`).
    Projects,
    /// Inspect an epoch status summary.
    Status {
        #[arg(long, env = "QUOD_PROJECT_ID", default_value = "erdos-479")]
        project: String,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
    },
    /// Inspect a project's Route-grouped Task DAG and Verified Overlays.
    Graph {
        #[arg(long, env = "QUOD_PROJECT_ID", default_value = "erdos-479")]
        project: String,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
        #[arg(long)]
        json: bool,
    },
    /// Inspect recent control-plane events for a project epoch.
    Events {
        #[arg(long, env = "QUOD_PROJECT_ID", default_value = "erdos-479")]
        project: String,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
        #[arg(long)]
        since_seq: Option<i64>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Read source code of a Spec or Verified Overlay module (`GET /api/projects/{p}/epochs/{e}/overlays/{m}/source`).
    Overlay {
        #[arg(long, env = "QUOD_PROJECT_ID", default_value = "erdos-479")]
        project: String,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
        #[arg(long)]
        module: String,
        /// Print only raw Lean source code (`code_utf8`) instead of JSON metadata.
        #[arg(long)]
        raw: bool,
    },
    /// Query or wait on verification status for a submission (`GET /api/submissions/{id}`).
    Submission {
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: String,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: PathBuf,
        submission_id: Uuid,
        /// Poll until Stage 1/2 verification reaches a terminal state.
        #[arg(long)]
        wait: bool,
        #[arg(long, default_value_t = 500)]
        poll_ms: u64,
        #[arg(long, default_value_t = 180)]
        timeout_secs: u64,
    },
    /// Claim a leased pure-Prop TaskPacket (`I0`) and materialize TASK.md + target_file.
    Fetch {
        #[arg(long, env = "QUOD_PROJECT_ID", default_value = "erdos-479")]
        project: String,
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: String,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: PathBuf,
        #[arg(long)]
        task_id: Option<String>,
        #[arg(long)]
        route_id: Option<String>,
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
    /// Run unified local preflight (Stage 0 + optional minimal-import check + Lean 4 kernel).
    Verify {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "proof")]
        kind: CliKind,
        #[arg(long)]
        spec_file: Option<PathBuf>,
        #[arg(long, default_value = "Spec")]
        spec_module: String,
        #[arg(long, default_value = "Spec.target")]
        spec_decl: String,
        #[arg(long, default_value = "candidate")]
        candidate_decl: String,
        #[arg(long)]
        inline_target_prop: Option<String>,
        #[arg(long = "extra-import")]
        extra_imports: Vec<String>,
        #[arg(long = "lean-path")]
        lean_paths: Vec<PathBuf>,
        #[arg(long)]
        expected_spec_sha256: Option<String>,
        #[arg(long)]
        spec_sha256: Option<String>,
        #[arg(long, default_value = "lean")]
        lean_bin: PathBuf,
        #[arg(long, default_value_t = 60)]
        timeout_secs: u64,
        #[arg(long)]
        stage0_only: bool,
        #[arg(long)]
        check_spec_imports: bool,
    },
    /// Send a signed lease keep-alive heartbeat (`I0`).
    Heartbeat {
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: String,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: PathBuf,
        #[arg(long)]
        lease_id: Uuid,
    },
    /// Submit a verified Proof or Refutation artifact against an active task lease (`I0`).
    Submit {
        #[arg(long, env = "QUOD_PROJECT_ID", default_value = "erdos-479")]
        project: String,
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: String,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: PathBuf,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
        #[arg(long)]
        task_id: String,
        #[arg(long)]
        lease_id: Uuid,
        #[arg(long, value_enum, default_value = "proof")]
        kind: CliKind,
        #[arg(long, default_value = "")]
        spec_sha256: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        idea_attr_id: Option<String>,
        /// Wait for Stage 1/2 verification completion and exit with deterministic verdict code.
        #[arg(long)]
        wait: bool,
        #[arg(long, default_value_t = 500)]
        poll_ms: u64,
        #[arg(long, default_value_t = 180)]
        wait_timeout_secs: u64,
    },
    /// `I1` Orchestrator commands (digest inspection and Graph IR submission).
    Orch {
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: String,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: PathBuf,
        #[command(subcommand)]
        action: OrchCommand,
    },
    /// Run a read-only `quod-algo` graph analysis query (`I1`).
    Algo {
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: String,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: PathBuf,
        #[arg(long, env = "QUOD_PROJECT_ID", default_value = "erdos-479")]
        project: String,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Algorithm name: voi-rank | detect-stalls | check-coupling | mine-reusable
        algo_name: String,
    },
    /// `I2-B` Blueprinter commands (create project, propose epoch, freeze epoch).
    Blueprint {
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: String,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: PathBuf,
        #[command(subcommand)]
        action: BlueprintCommand,
    },
    /// `I2-A` Independent Auditor commands (approve/reject PendingAudit epochs).
    Audit {
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: String,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: PathBuf,
        #[command(subcommand)]
        action: AuditCommand,
    },
    /// Administrative actor & bundle registration (`Admin` signature or `--admin-token` bootstrap).
    Admin {
        #[arg(long, env = "QUOD_ADMIN_TOKEN")]
        admin_token: Option<String>,
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: Option<String>,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: Option<PathBuf>,
        #[command(subcommand)]
        action: AdminCommand,
    },
    /// Generate/load a local Ed25519 keypair and self-register with Quod Hub (`POST /api/actors/join`).
    Join {
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: String,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: PathBuf,
        #[arg(long)]
        invite: Option<String>,
        #[arg(long, default_value = "external")]
        lineage_cluster: String,
        #[arg(long = "skill-tag", value_delimiter = ',')]
        skill_tags: Vec<String>,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        git_email: Option<String>,
    },
    /// Issue a scoped invite token (`Admin` any role; `Blueprinter` project-scoped Worker/Orchestrator only).
    Invite {
        #[arg(long, env = "QUOD_ADMIN_TOKEN")]
        admin_token: Option<String>,
        #[arg(long, env = "QUOD_ACTOR_ID")]
        actor_id: Option<String>,
        #[arg(long, env = "QUOD_KEY_FILE")]
        key_file: Option<PathBuf>,
        #[arg(long = "role", value_delimiter = ',')]
        roles: Vec<String>,
        #[arg(long = "project-scope", value_delimiter = ',')]
        project_scopes: Vec<String>,
        #[arg(long, default_value_t = 1)]
        max_uses: u32,
        #[arg(long, default_value_t = 86_400)]
        expires_in_secs: i64,
        #[arg(long)]
        tier: Option<u8>,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Print embedded role SKILL.md documentation (works offline in any directory).
    Skill {
        /// Role skill to print (defaults to the router skill).
        #[arg(value_enum, default_value_t = CliSkill::Router)]
        role: CliSkill,
        /// Append worker reference guides (cli-reference, exit-codes, task-guide) when role=worker.
        #[arg(long)]
        with_refs: bool,
        /// List available embedded skills and exit.
        #[arg(long)]
        list: bool,
    },
    /// Show the Credit points leaderboard (`GET /api/leaderboard`).
    Leaderboard {
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum OrchCommand {
    /// Fetch the compressed `WakeupDigest` (`GET /api/projects/{p}/epochs/{e}/orch/digest`).
    Digest {
        #[arg(long, env = "QUOD_PROJECT_ID", default_value = "erdos-479")]
        project: String,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
    },
    /// Apply a JSON file of `GraphIrOp` operations (`POST /api/projects/{p}/epochs/{e}/orch/ir`).
    Apply {
        #[arg(long, env = "QUOD_PROJECT_ID", default_value = "erdos-479")]
        project: String,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
        #[arg(long)]
        file: PathBuf,
        /// Validate IR operations and cycle invariants without mutating state.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
enum BlueprintCommand {
    /// Create a new mathematical project.
    InitProject {
        #[arg(long)]
        project_id: String,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        spec_namespace: String,
        #[arg(long)]
        default_bundle_id: Option<String>,
    },
    /// Propose a new Epoch (requires Auditor approval unless `--auto-activate`).
    ProposeEpoch {
        #[arg(long, env = "QUOD_PROJECT_ID")]
        project: String,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
        #[arg(long)]
        bundle_id: String,
        #[arg(long, default_value = "HEAD")]
        base_commit: String,
        #[arg(long)]
        spec_module: String,
        #[arg(long)]
        spec_decl_name: String,
        #[arg(long, default_value = "")]
        spec_statement: String,
        #[arg(long)]
        spec_file: Option<PathBuf>,
        #[arg(long, default_value = "")]
        spec_sha256: String,
        #[arg(long, default_value = "root")]
        root_task_id: String,
        #[arg(long)]
        auto_activate: bool,
    },
    /// Freeze an active epoch (`Active -> Freezing`).
    FreezeEpoch {
        #[arg(long, env = "QUOD_PROJECT_ID")]
        project: String,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
    },
}

#[derive(Debug, Subcommand)]
enum AuditCommand {
    /// Approve or reject a `PendingAudit` epoch.
    Epoch {
        #[arg(long, env = "QUOD_PROJECT_ID")]
        project: String,
        #[arg(long, default_value_t = 1)]
        epoch_id: u32,
        #[arg(long)]
        approve: bool,
        #[arg(long, default_value = "")]
        comment: String,
    },
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Register an allowlisted Actor Ed25519 identity with explicit RBAC roles.
    RegisterActor {
        #[arg(long)]
        actor_id: String,
        #[arg(long)]
        pubkey: String,
        #[arg(long = "role", value_delimiter = ',')]
        roles: Vec<String>,
        #[arg(long = "project-scope", value_delimiter = ',')]
        project_scopes: Vec<String>,
        #[arg(long)]
        lineage_cluster: String,
        #[arg(long)]
        tier: Option<u8>,
        #[arg(long = "skill-tag", value_delimiter = ',')]
        skill_tags: Vec<String>,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        git_email: Option<String>,
    },
    /// Register a shared read-only Lean/Mathlib `ToolchainBundle`.
    RegisterBundle {
        #[arg(long)]
        bundle_id: String,
        #[arg(long, default_value = "leanprover/lean4:v4.36.0")]
        lean_toolchain: String,
        #[arg(long, default_value = "v4.36.0")]
        mathlib_rev: String,
        #[arg(long)]
        toolchain_dir: String,
        #[arg(long = "lean-path", value_delimiter = ':')]
        lean_paths: Vec<String>,
        #[arg(long, default_value = "local")]
        bundle_sha256: String,
    },
}

#[derive(Deserialize)]
struct KeyFile {
    secret_hex: String,
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn expand_tilde(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        if let Some(home) = user_home_dir() {
            return home;
        }
    } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"))
        && let Some(home) = user_home_dir()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

fn load_secret_hex(path: &Path) -> anyhow::Result<String> {
    let resolved = expand_tilde(path);
    let raw = std::fs::read_to_string(&resolved)?;
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        let parsed: KeyFile = serde_json::from_str(trimmed)?;
        Ok(parsed.secret_hex)
    } else {
        Ok(trimmed.to_string())
    }
}

async fn signed_request(
    client: &reqwest::Client,
    hub_url: &str,
    method: reqwest::Method,
    path: &str,
    actor_id: &str,
    secret_hex: &str,
    body: &[u8],
) -> anyhow::Result<reqwest::Response> {
    let ts = Utc::now().timestamp();
    let sign_path = path.split('?').next().unwrap_or(path);
    let sig = sign_request(secret_hex, method.as_str(), sign_path, ts, body)?;
    let url = format!("{}{}", hub_url.trim_end_matches('/'), path);
    let mut builder = client
        .request(method, url)
        .header(HEADER_ACTOR_ID, actor_id)
        .header(HEADER_TIMESTAMP, ts.to_string())
        .header(HEADER_SIGNATURE, sig);
    if !body.is_empty() {
        builder = builder
            .header("content-type", "application/json")
            .body(body.to_vec());
    }
    Ok(builder.send().await?)
}

async fn ensure_success(
    resp: reqwest::Response,
) -> anyhow::Result<Result<reqwest::Response, ExitCode>> {
    let status = resp.status();
    if status.is_success() {
        return Ok(Ok(resp));
    }
    let body = resp.text().await.unwrap_or_default();
    if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&body) {
        eprintln!("{}", serde_json::to_string_pretty(&json_val)?);
    } else if !body.is_empty() {
        eprintln!("{body}");
    } else {
        eprintln!("HTTP {status}");
    }
    let code = match status {
        reqwest::StatusCode::UNPROCESSABLE_ENTITY => quod_sandbox::EXIT_FORBIDDEN_TOKEN as u8,
        _ => quod_sandbox::EXIT_INFRA_ERROR as u8,
    };
    Ok(Err(ExitCode::from(code)))
}

fn submission_terminal_exit_code(state: SubmissionState) -> Option<ExitCode> {
    match state {
        SubmissionState::Merged => Some(ExitCode::SUCCESS),
        SubmissionState::RejectedSpecDrift
        | SubmissionState::RejectedRaceLost
        | SubmissionState::StaleEpoch => Some(ExitCode::from(quod_sandbox::EXIT_SPEC_DRIFT as u8)),
        SubmissionState::RejectedSecurity => {
            Some(ExitCode::from(quod_sandbox::EXIT_FORBIDDEN_TOKEN as u8))
        }
        SubmissionState::RejectedKernel => {
            Some(ExitCode::from(quod_sandbox::EXIT_KERNEL_FAILED as u8))
        }
        SubmissionState::RejectedHygiene => {
            Some(ExitCode::from(quod_sandbox::EXIT_HYGIENE_FAILED as u8))
        }
        SubmissionState::RejectedResourceLimit => {
            Some(ExitCode::from(quod_sandbox::EXIT_RESOURCE_LIMIT as u8))
        }
        SubmissionState::SealedReceived
        | SubmissionState::Stage0SpecOk
        | SubmissionState::Stage1KernelOk
        | SubmissionState::Stage2HygieneOk => None,
    }
}

async fn fetch_or_wait_submission(
    client: &reqwest::Client,
    base: &str,
    actor_id: &str,
    sk: &str,
    submission_id: Uuid,
    wait: bool,
    poll_ms: u64,
    timeout_secs: u64,
) -> anyhow::Result<ExitCode> {
    let path = format!("/api/submissions/{submission_id}");
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs.max(1));
    loop {
        let resp = signed_request(
            client,
            base,
            reqwest::Method::GET,
            &path,
            actor_id,
            sk,
            &[],
        )
        .await?;
        let ok_resp = match ensure_success(resp).await? {
            Ok(r) => r,
            Err(code) => return Ok(code),
        };
        let status_view: SubmissionStatusView = ok_resp.json().await?;
        let terminal = submission_terminal_exit_code(status_view.state);
        if !wait || terminal.is_some() {
            println!("{}", serde_json::to_string_pretty(&status_view)?);
            return Ok(terminal.unwrap_or(ExitCode::SUCCESS));
        }
        if tokio::time::Instant::now() >= deadline {
            println!("{}", serde_json::to_string_pretty(&status_view)?);
            eprintln!("timed out waiting for submission {submission_id}");
            return Ok(ExitCode::from(quod_sandbox::EXIT_RESOURCE_LIMIT as u8));
        }
        tokio::time::sleep(std::time::Duration::from_millis(poll_ms.max(50))).await;
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(quod_sandbox::EXIT_INFRA_ERROR as u8)
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let client = reqwest::Client::new();
    let base = cli.hub_url.trim_end_matches('/').to_string();

    match cli.command {
        Commands::Keygen { out } => {
            let (sk, pk) = generate_keypair_hex();
            let _ = public_key_from_secret_hex(&sk)?;
            let payload = serde_json::json!({
                "secret_hex": sk,
                "public_hex": pk,
            });
            let pretty = serde_json::to_string_pretty(&payload)?;
            if let Some(path) = out {
                let path = expand_tilde(&path);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, format!("{pretty}\n"))?;
            }
            println!("{pretty}");
            Ok(ExitCode::SUCCESS)
        }
        Commands::Whoami { actor_id, key_file } => {
            let sk = load_secret_hex(&key_file)?;
            let resp = signed_request(
                &client,
                &base,
                reqwest::Method::GET,
                "/api/whoami",
                &actor_id,
                &sk,
                &[],
            )
            .await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let view: WhoAmIResponse = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&view)?);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Projects => {
            let resp = client.get(format!("{base}/api/projects")).send().await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let projs: Vec<ProjectSummaryView> = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&projs)?);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Status { project, epoch_id } => {
            let resp = client
                .get(format!(
                    "{base}/api/projects/{project}/epochs/{epoch_id}/status"
                ))
                .send()
                .await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let view: EpochStatusView = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&view)?);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Graph {
            project,
            epoch_id,
            json,
        } => {
            let resp = client
                .get(format!(
                    "{base}/api/projects/{project}/epochs/{epoch_id}/graph"
                ))
                .send()
                .await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let snap: ProjectGraphView = resp.json().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&snap)?);
            } else {
                print_graph_ascii(&snap);
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Events {
            project,
            epoch_id,
            since_seq,
            limit,
        } => {
            let mut url = format!(
                "{base}/api/projects/{project}/epochs/{epoch_id}/events?limit={limit}"
            );
            if let Some(seq) = since_seq {
                url.push_str(&format!("&since_seq={seq}"));
            }
            let resp = client.get(url).send().await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let events: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&events)?);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Overlay {
            project,
            epoch_id,
            module,
            raw,
        } => {
            let resp = client
                .get(format!(
                    "{base}/api/projects/{project}/epochs/{epoch_id}/overlays/{module}/source"
                ))
                .send()
                .await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let view: OverlaySourceView = resp.json().await?;
            if raw {
                print!("{}", view.code_utf8);
            } else {
                println!("{}", serde_json::to_string_pretty(&view)?);
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Submission {
            actor_id,
            key_file,
            submission_id,
            wait,
            poll_ms,
            timeout_secs,
        } => {
            let sk = load_secret_hex(&key_file)?;
            fetch_or_wait_submission(
                &client,
                &base,
                &actor_id,
                &sk,
                submission_id,
                wait,
                poll_ms,
                timeout_secs,
            )
            .await
        }
        Commands::Fetch {
            project,
            actor_id,
            key_file,
            task_id,
            route_id,
            out_dir,
        } => {
            let sk = load_secret_hex(&key_file)?;
            let body = serde_json::to_vec(&ClaimLeaseRequest {
                preferred_task_id: task_id,
                preferred_route_id: route_id,
            })?;
            let path = format!("/api/projects/{project}/leases/claim");
            let resp = signed_request(
                &client,
                &base,
                reqwest::Method::POST,
                &path,
                &actor_id,
                &sk,
                &body,
            )
            .await?;
            if resp.status() == reqwest::StatusCode::NO_CONTENT {
                println!("null");
                return Ok(ExitCode::SUCCESS);
            }
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let packet: TaskPacket = resp.json().await?;
            if let Some(dir) = out_dir {
                let dir = expand_tilde(&dir);
                std::fs::create_dir_all(&dir)?;
                std::fs::write(
                    dir.join("task_packet.json"),
                    serde_json::to_string_pretty(&packet)?,
                )?;
                let target_path = dir.join(&packet.target_file);
                if let Some(p) = target_path.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::write(&target_path, &packet.transitive_slice)?;
                std::fs::write(dir.join("TASK.md"), render_task_md(&packet))?;
            }
            println!("{}", serde_json::to_string_pretty(&packet)?);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Verify {
            file,
            kind,
            spec_file,
            spec_module,
            spec_decl,
            candidate_decl,
            inline_target_prop,
            extra_imports,
            lean_paths,
            expected_spec_sha256,
            spec_sha256,
            lean_bin,
            timeout_secs,
            stage0_only,
            check_spec_imports,
        } => {
            let file = expand_tilde(&file);
            let spec_file = spec_file.map(|p| expand_tilde(&p));
            let lean_paths: Vec<PathBuf> = lean_paths.into_iter().map(|p| expand_tilde(&p)).collect();
            let lean_bin = expand_tilde(&lean_bin);
            let code = std::fs::read_to_string(&file)?;
            if check_spec_imports {
                let imp_viols = check_spec_minimal_imports(&code);
                if !imp_viols.is_empty() {
                    println!("{}", serde_json::to_string_pretty(&imp_viols)?);
                    return Ok(ExitCode::from(quod_sandbox::EXIT_HYGIENE_FAILED as u8));
                }
            }
            let runner = LocalLeanRunner::new(lean_bin);
            let verdict = runner
                .verify(&LocalVerifyRequest {
                    task_id: "local".into(),
                    kind: kind.into(),
                    candidate_code: code,
                    candidate_module: "Candidate".into(),
                    candidate_decl,
                    spec_file,
                    spec_module,
                    spec_decl,
                    inline_target_prop,
                    extra_imports,
                    lean_paths,
                    expected_spec_sha256,
                    actual_spec_sha256: spec_sha256,
                    timeout_secs,
                    stage0_only,
                    persist_olean_dir: None,
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&verdict)?);
            Ok(ExitCode::from(verdict.exit_code as u8))
        }
        Commands::Heartbeat {
            actor_id,
            key_file,
            lease_id,
        } => {
            let sk = load_secret_hex(&key_file)?;
            let path = format!("/api/leases/{lease_id}/heartbeat");
            let resp = signed_request(
                &client,
                &base,
                reqwest::Method::POST,
                &path,
                &actor_id,
                &sk,
                &[],
            )
            .await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let json: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&json)?);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Submit {
            project,
            actor_id,
            key_file,
            epoch_id,
            task_id,
            lease_id,
            kind,
            spec_sha256,
            file,
            idea_attr_id,
            wait,
            poll_ms,
            wait_timeout_secs,
        } => {
            let sk = load_secret_hex(&key_file)?;
            let file = expand_tilde(&file);
            let code_utf8 = std::fs::read_to_string(&file)?;
            let body = serde_json::to_vec(&SubmitRequest {
                epoch_id,
                task_id,
                lease_id,
                kind: kind.into(),
                spec_sha256,
                code_utf8,
                idea_attr_id,
            })?;
            let path = format!("/api/projects/{project}/submissions");
            let resp = signed_request(
                &client,
                &base,
                reqwest::Method::POST,
                &path,
                &actor_id,
                &sk,
                &body,
            )
            .await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let sub_resp: SubmitResponse = resp.json().await?;
            if wait {
                fetch_or_wait_submission(
                    &client,
                    &base,
                    &actor_id,
                    &sk,
                    sub_resp.submission_id,
                    true,
                    poll_ms,
                    wait_timeout_secs,
                )
                .await
            } else {
                println!("{}", serde_json::to_string_pretty(&sub_resp)?);
                Ok(ExitCode::SUCCESS)
            }
        }
        Commands::Orch {
            actor_id,
            key_file,
            action,
        } => {
            let sk = load_secret_hex(&key_file)?;
            match action {
                OrchCommand::Digest { project, epoch_id } => {
                    let path = format!("/api/projects/{project}/epochs/{epoch_id}/orch/digest");
                    let resp = signed_request(
                        &client,
                        &base,
                        reqwest::Method::GET,
                        &path,
                        &actor_id,
                        &sk,
                        &[],
                    )
                    .await?;
                    let resp = match ensure_success(resp).await? {
                        Ok(r) => r,
                        Err(code) => return Ok(code),
                    };
                    let digest: WakeupDigestView = resp.json().await?;
                    println!("{}", serde_json::to_string_pretty(&digest)?);
                }
                OrchCommand::Apply {
                    project,
                    epoch_id,
                    file,
                    dry_run,
                } => {
                    let file = expand_tilde(&file);
                    let raw = std::fs::read_to_string(&file)?;
                    let mut req: ApplyIrRequest = serde_json::from_str(&raw)?;
                    req.dry_run = req.dry_run || dry_run;
                    let body = serde_json::to_vec(&req)?;
                    let path = format!("/api/projects/{project}/epochs/{epoch_id}/orch/ir");
                    let resp = signed_request(
                        &client,
                        &base,
                        reqwest::Method::POST,
                        &path,
                        &actor_id,
                        &sk,
                        &body,
                    )
                    .await?;
                    let resp = match ensure_success(resp).await? {
                        Ok(r) => r,
                        Err(code) => return Ok(code),
                    };
                    let res: ApplyIrResponse = resp.json().await?;
                    println!("{}", serde_json::to_string_pretty(&res)?);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Algo {
            actor_id,
            key_file,
            project,
            epoch_id,
            limit,
            algo_name,
        } => {
            let sk = load_secret_hex(&key_file)?;
            let path = format!("/api/projects/{project}/epochs/{epoch_id}/algo/{algo_name}?limit={limit}");
            let resp = signed_request(
                &client,
                &base,
                reqwest::Method::GET,
                &path,
                &actor_id,
                &sk,
                &[],
            )
            .await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let res: AlgoQueryResult = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&res)?);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Blueprint {
            actor_id,
            key_file,
            action,
        } => {
            let sk = load_secret_hex(&key_file)?;
            match action {
                BlueprintCommand::InitProject {
                    project_id,
                    title,
                    description,
                    spec_namespace,
                    default_bundle_id,
                } => {
                    let body = serde_json::to_vec(&CreateProjectRequest {
                        project_id,
                        title,
                        description,
                        spec_namespace,
                        default_bundle_id,
                    })?;
                    let resp = signed_request(
                        &client,
                        &base,
                        reqwest::Method::POST,
                        "/api/projects",
                        &actor_id,
                        &sk,
                        &body,
                    )
                    .await?;
                    let resp = match ensure_success(resp).await? {
                        Ok(r) => r,
                        Err(code) => return Ok(code),
                    };
                    println!("{}", resp.text().await?);
                }
                BlueprintCommand::ProposeEpoch {
                    project,
                    epoch_id,
                    bundle_id,
                    base_commit,
                    spec_module,
                    spec_decl_name,
                    spec_statement,
                    spec_file,
                    mut spec_sha256,
                    root_task_id,
                    auto_activate,
                } => {
                    let spec_code = match spec_file {
                        Some(p) => {
                            let p = expand_tilde(&p);
                            let code = std::fs::read_to_string(&p)?;
                            if spec_sha256.is_empty() {
                                spec_sha256 = sha256_hex(code.as_bytes());
                            }
                            Some(code)
                        }
                        None => None,
                    };
                    let body = serde_json::to_vec(&CreateEpochRequest {
                        epoch_id,
                        bundle_id,
                        base_commit,
                        spec_module,
                        spec_decl_name,
                        spec_statement,
                        spec_sha256,
                        spec_code,
                        root_task_id,
                        auto_activate,
                    })?;
                    let path = format!("/api/projects/{project}/epochs");
                    let resp = signed_request(
                        &client,
                        &base,
                        reqwest::Method::POST,
                        &path,
                        &actor_id,
                        &sk,
                        &body,
                    )
                    .await?;
                    let resp = match ensure_success(resp).await? {
                        Ok(r) => r,
                        Err(code) => return Ok(code),
                    };
                    println!("{}", resp.text().await?);
                }
                BlueprintCommand::FreezeEpoch { project, epoch_id } => {
                    let path = format!("/api/projects/{project}/epochs/{epoch_id}/freeze");
                    let resp = signed_request(
                        &client,
                        &base,
                        reqwest::Method::POST,
                        &path,
                        &actor_id,
                        &sk,
                        b"{}",
                    )
                    .await?;
                    let resp = match ensure_success(resp).await? {
                        Ok(r) => r,
                        Err(code) => return Ok(code),
                    };
                    println!("{}", resp.text().await?);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Audit {
            actor_id,
            key_file,
            action,
        } => {
            let sk = load_secret_hex(&key_file)?;
            match action {
                AuditCommand::Epoch {
                    project,
                    epoch_id,
                    approve,
                    comment,
                } => {
                    let body = serde_json::to_vec(&AuditEpochRequest { approve, comment })?;
                    let path = format!("/api/projects/{project}/epochs/{epoch_id}/audit");
                    let resp = signed_request(
                        &client,
                        &base,
                        reqwest::Method::POST,
                        &path,
                        &actor_id,
                        &sk,
                        &body,
                    )
                    .await?;
                    let resp = match ensure_success(resp).await? {
                        Ok(r) => r,
                        Err(code) => return Ok(code),
                    };
                    println!("{}", resp.text().await?);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Admin {
            admin_token,
            actor_id,
            key_file,
            action,
        } => {
            let (path, body) = match action {
                AdminCommand::RegisterActor {
                    actor_id: target_id,
                    pubkey,
                    roles,
                    project_scopes,
                    lineage_cluster,
                    tier,
                    skill_tags,
                    display_name,
                    git_email,
                } => {
                    let parsed_roles: Vec<Role> = if roles.is_empty() {
                        vec![Role::Worker]
                    } else {
                        roles
                            .iter()
                            .map(|r| r.parse::<Role>().map_err(anyhow::Error::msg))
                            .collect::<anyhow::Result<Vec<_>>>()?
                    };
                    let body = serde_json::to_vec(&RegisterActorRequest {
                        actor_id: target_id,
                        pubkey_ed25519: pubkey,
                        roles: parsed_roles,
                        project_scopes,
                        lineage_cluster,
                        initial_tier: tier,
                        skill_tags,
                        display_name,
                        git_email,
                    })?;
                    ("/api/admin/actors", body)
                }
                AdminCommand::RegisterBundle {
                    bundle_id,
                    lean_toolchain,
                    mathlib_rev,
                    toolchain_dir,
                    lean_paths,
                    bundle_sha256,
                } => {
                    let body = serde_json::to_vec(&RegisterBundleRequest {
                        bundle_id,
                        lean_toolchain,
                        mathlib_rev,
                        toolchain_dir,
                        lean_paths,
                        bundle_sha256,
                    })?;
                    ("/api/admin/bundles", body)
                }
            };

            let resp = if let (Some(aid), Some(kf)) = (actor_id, key_file) {
                let sk = load_secret_hex(&kf)?;
                signed_request(
                    &client,
                    &base,
                    reqwest::Method::POST,
                    path,
                    &aid,
                    &sk,
                    &body,
                )
                .await?
            } else {
                let tok = admin_token.ok_or_else(|| {
                    anyhow::anyhow!("provide --admin-token or (--actor-id + --key-file)")
                })?;
                client
                    .post(format!("{base}{path}"))
                    .header("content-type", "application/json")
                    .header(HEADER_ADMIN_TOKEN, tok)
                    .body(body)
                    .send()
                    .await?
            };
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            println!("{}", resp.text().await?);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Join {
            actor_id,
            key_file,
            invite,
            lineage_cluster,
            skill_tags,
            display_name,
            git_email,
        } => {
            let key_file = expand_tilde(&key_file);
            let pk = if key_file.exists() {
                let sk = load_secret_hex(&key_file)?;
                public_key_from_secret_hex(&sk)?
            } else {
                let (sk, pk) = generate_keypair_hex();
                if let Some(parent) = key_file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let key_json = serde_json::to_string_pretty(&serde_json::json!({
                    "secret_hex": sk,
                    "public_hex": pk,
                }))?;
                std::fs::write(&key_file, format!("{key_json}\n"))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600))?;
                }
                pk
            };
            let body = serde_json::to_vec(&JoinActorRequest {
                actor_id,
                pubkey_ed25519: pk,
                lineage_cluster,
                invite_code: invite,
                skill_tags,
                display_name,
                git_email,
            })?;
            let resp = client
                .post(format!("{base}/api/actors/join"))
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            println!("{}", resp.text().await?);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Invite {
            admin_token,
            actor_id,
            key_file,
            roles,
            project_scopes,
            max_uses,
            expires_in_secs,
            tier,
            note,
        } => {
            let parsed_roles: Vec<Role> = if roles.is_empty() {
                vec![Role::Worker]
            } else {
                roles
                    .iter()
                    .map(|r| r.parse::<Role>().map_err(anyhow::Error::msg))
                    .collect::<anyhow::Result<Vec<_>>>()?
            };
            let body = serde_json::to_vec(&CreateInviteRequest {
                roles: parsed_roles,
                project_scopes,
                max_uses,
                expires_in_secs,
                initial_tier: tier,
                note,
            })?;
            let resp = if let (Some(aid), Some(kf)) = (actor_id, key_file) {
                let sk = load_secret_hex(&kf)?;
                signed_request(
                    &client,
                    &base,
                    reqwest::Method::POST,
                    "/api/invites",
                    &aid,
                    &sk,
                    &body,
                )
                .await?
            } else {
                let tok = admin_token.ok_or_else(|| {
                    anyhow::anyhow!("provide --admin-token or (--actor-id + --key-file)")
                })?;
                client
                    .post(format!("{base}/api/invites"))
                    .header("content-type", "application/json")
                    .header(HEADER_ADMIN_TOKEN, tok)
                    .body(body)
                    .send()
                    .await?
            };
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            println!("{}", resp.text().await?);
            Ok(ExitCode::SUCCESS)
        }
        Commands::Skill {
            role,
            with_refs,
            list,
        } => {
            if list {
                for name in ["router", "worker", "orchestrator", "auditor", "blueprinter"] {
                    println!("{name}");
                }
                return Ok(ExitCode::SUCCESS);
            }
            let base_doc = match role {
                CliSkill::Router => SKILL_ROUTER,
                CliSkill::Worker => SKILL_WORKER,
                CliSkill::Orchestrator => SKILL_ORCHESTRATOR,
                CliSkill::Auditor => SKILL_AUDITOR,
                CliSkill::Blueprinter => SKILL_BLUEPRINTER,
            };
            print!("{base_doc}");
            if role == CliSkill::Worker && with_refs {
                print!("\n---\n{SKILL_WORKER_CLI_REF}\n---\n{SKILL_WORKER_EXIT_CODES}\n---\n{SKILL_WORKER_TASK_GUIDE}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Leaderboard {
            project,
            limit,
            json,
        } => {
            let mut url = format!("{base}/api/leaderboard?limit={}", limit.clamp(1, 200));
            if let Some(proj) = project.as_deref().filter(|s| !s.trim().is_empty()) {
                url.push_str(&format!("&project={proj}"));
            }
            let resp = client.get(url).send().await?;
            let resp = match ensure_success(resp).await? {
                Ok(r) => r,
                Err(code) => return Ok(code),
            };
            let rows: Vec<LeaderboardEntryView> = resp.json().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!("(no credit entries yet)");
            } else {
                println!(
                    "{:<4}  {:<18}  {:<20}  {:<14}  {:>4}  {:>8}  {:>6}  {:>6}",
                    "#", "ACTOR", "NAME", "LINEAGE", "TIER", "CREDITS", "PROOFS", "REFUTE"
                );
                for (idx, row) in rows.iter().enumerate() {
                    println!(
                        "{:<4}  {:<18}  {:<20}  {:<14}  T{:<3}  {:>8}  {:>6}  {:>6}",
                        idx + 1,
                        row.actor_id,
                        row.display_name.as_deref().unwrap_or("-"),
                        row.lineage_cluster,
                        row.tier,
                        row.credit_points,
                        row.verified_tasks,
                        row.verified_refutations
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn print_graph_ascii(snap: &ProjectGraphView) {
    println!(
        "Project {} [{:?}] | Epoch {} [{:?}] bundle={} ({}) spec={} algo={:?} graveyard={}",
        snap.project_id,
        snap.project_status,
        snap.epoch_id,
        snap.epoch_state,
        snap.bundle_id,
        snap.lean_toolchain,
        snap.spec_decl_name,
        snap.dispatch_policy.algo,
        snap.graveyard_count
    );
    println!("Routes ({}):", snap.routes.len());
    for r in &snap.routes {
        println!("  - {:<16} [{:?}] {}", r.route_id, r.state, r.title);
    }
    println!("Tasks ({}):", snap.tasks.len());
    for t in &snap.tasks {
        println!(
            "  - {:<18} route={:<12} [{:?}] state={:?} prio={:.1} p={:.2} c={:.2} tier>={} bounty=+{}pts",
            t.task_id,
            t.route_id.as_deref().unwrap_or("-"),
            t.task_role,
            t.state,
            t.priority,
            t.prior_p,
            t.expected_cost_c,
            t.min_tier,
            t.credit_bounty
        );
    }
    println!("Edges ({}):", snap.edges.len());
    for e in &snap.edges {
        println!("  - {} --[{:?}]--> {}", e.from_task_id, e.edge_kind, e.to_task_id);
    }
    println!("Verified Overlays ({}):", snap.overlays.len());
    for o in &snap.overlays {
        println!(
            "  - {} [{:?}] src={:?} ({}ms)",
            o.module_name, o.overlay_kind, o.source_task_id, o.compile_ms
        );
    }
    println!("Active Leases ({}):", snap.active_leases.len());
    for l in &snap.active_leases {
        println!(
            "  - {} actor={} propensity={:.2} expires={}",
            l.lease_id, l.actor_id, l.propensity_score, l.expires_at
        );
    }
}

fn render_task_md(packet: &TaskPacket) -> String {
    let lease_id = packet
        .lease
        .as_ref()
        .map(|l| l.lease_id.to_string())
        .unwrap_or_else(|| "<UNLEASED>".to_string());
    let verify_kind = match packet.task_role {
        quod_core::TaskRole::Refutation => "refutation",
        _ => "proof",
    };
    let imports = if packet.curated_imports.is_empty() {
        "- (none)".to_string()
    } else {
        packet
            .curated_imports
            .iter()
            .map(|l| format!("- `{l}`"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let graveyard = if packet.graveyard_constraints.is_empty() {
        "- (none recorded yet)".to_string()
    } else {
        packet
            .graveyard_constraints
            .iter()
            .map(|g| format!("- {g}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r#"# Quod Task: `{task_id}` (Project `{project_id}`, Epoch {epoch_id})

- **Task Role**: `{task_role:?}`
- **Route**: `{route_id}`
- **Credit Bounty**: `+{credit_bounty} pts`
- **Target File**: `{target_file}`
- **Spec SHA-256**: `{spec_sha256}`
- **Active Lease ID**: `{lease_id}`

## Commands
1. Verify locally (Stage 0 forbids `sorry`, `admit`, `axiom`, `opaque`, `unsafe`, `implemented_by`, `extern`, `native_decide`, `#eval`, `#print`, `#check`, `elab`, `macro_rules`, `IO.`, `System.`):
   ```bash
   quod verify {target_file} --kind {verify_kind} --stage0-only
   ```
2. Renew lease if needed:
   ```bash
   quod heartbeat --lease-id {lease_id}
   ```
3. Submit once local checks pass:
   ```bash
   quod submit --project {project_id} --epoch-id {epoch_id} --task-id {task_id} --lease-id {lease_id} --kind {verify_kind} --spec-sha256 {spec_sha256} --file {target_file}
   ```

## Curated Imports (Spec + Upstream Verified Overlays)
{imports}

## Authoritative Graveyard Constraints
{graveyard}
"#,
        task_id = packet.task_id,
        project_id = packet.project_id,
        epoch_id = packet.epoch_id,
        task_role = packet.task_role,
        route_id = packet.route_id.as_deref().unwrap_or("(unrouted)"),
        credit_bounty = packet.credit_bounty,
        target_file = packet.target_file,
        spec_sha256 = packet.spec_sha256,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_subcommands() {
        let verify_cli = Cli::try_parse_from([
            "quod",
            "verify",
            "Candidate.lean",
            "--kind",
            "def-overlay",
            "--check-spec-imports",
            "--stage0-only",
        ])
        .unwrap();
        assert!(matches!(verify_cli.command, Commands::Verify { .. }));

        let algo_cli = Cli::try_parse_from([
            "quod",
            "algo",
            "--actor-id",
            "orch-1",
            "--key-file",
            "k.json",
            "--project",
            "erdos-479",
            "voi-rank",
        ])
        .unwrap();
        assert!(matches!(algo_cli.command, Commands::Algo { .. }));

        let kg = Cli::try_parse_from(["quod", "keygen"]).unwrap();
        assert!(matches!(kg.command, Commands::Keygen { .. }));

        let ov = Cli::try_parse_from([
            "quod",
            "overlay",
            "--project",
            "erdos-479",
            "--epoch-id",
            "1",
            "--module",
            "Quod.Epoch1.Spec",
            "--raw",
        ])
        .unwrap();
        assert!(matches!(ov.command, Commands::Overlay { raw: true, .. }));

        let sub_wait = Cli::try_parse_from([
            "quod",
            "submission",
            "--actor-id",
            "w-1",
            "--key-file",
            "k.json",
            "01930000-0000-7000-8000-000000000001",
            "--wait",
        ])
        .unwrap();
        assert!(matches!(sub_wait.command, Commands::Submission { wait: true, .. }));

        let ev_cur = Cli::try_parse_from([
            "quod",
            "events",
            "--project",
            "erdos-479",
            "--since-seq",
            "42",
        ])
        .unwrap();
        assert!(matches!(ev_cur.command, Commands::Events { since_seq: Some(42), .. }));

        let ir_dry = Cli::try_parse_from([
            "quod",
            "orch",
            "--actor-id",
            "orch-1",
            "--key-file",
            "k.json",
            "apply",
            "--project",
            "erdos-479",
            "--file",
            "plan.json",
            "--dry-run",
        ])
        .unwrap();
        assert!(matches!(
            ir_dry.command,
            Commands::Orch {
                action: OrchCommand::Apply { dry_run: true, .. },
                ..
            }
        ));

        let join_cli = Cli::try_parse_from([
            "quod",
            "join",
            "--actor-id",
            "w-01",
            "--key-file",
            "w.json",
            "--invite",
            "quod_inv_123",
            "--lineage-cluster",
            "deepseek",
        ])
        .unwrap();
        assert!(matches!(join_cli.command, Commands::Join { .. }));

        let inv_cli = Cli::try_parse_from([
            "quod",
            "invite",
            "--actor-id",
            "bp-1",
            "--key-file",
            "bp.json",
            "--role",
            "worker,orchestrator",
            "--project-scope",
            "erdos-479",
            "--max-uses",
            "5",
        ])
        .unwrap();
        assert!(matches!(inv_cli.command, Commands::Invite { max_uses: 5, .. }));
    }

    #[test]
    fn render_task_md_includes_task_fields() {
        let pkt = TaskPacket {
            schema_version: 2,
            project_id: "erdos-479".into(),
            epoch_id: 1,
            task_id: "merge_a".into(),
            route_id: Some("route-a".into()),
            task_role: quod_core::TaskRole::Merge,
            module_hint: Some("Chapter1".into()),
            credit_bounty: 180,
            bundle_id: "mathlib-v4.36.0".into(),
            spec_module: "Quod.Epoch1.Spec".into(),
            spec_sha256: "abc123".into(),
            target_file: "Tasks/merge_a.lean".into(),
            transitive_slice: "-- slice".into(),
            curated_imports: vec!["Quod.Epoch1.Spec".into(), "QuodOverlay_a_1234".into()],
            graveyard_constraints: vec!["[kernel_rejected] timeout".into()],
            resource_limits: Default::default(),
            lease: None,
        };
        let md = render_task_md(&pkt);
        assert!(md.contains("merge_a"));
        assert!(md.contains("+180 pts"));
        assert!(md.contains("QuodOverlay_a_1234"));
        assert!(md.contains("quod submit --project erdos-479"));

        let expanded = expand_tilde(Path::new("~/.config/quod/alice.json"));
        let expanded_win = expand_tilde(Path::new("~\\.config\\quod\\alice.json"));
        if let Some(home) = user_home_dir() {
            assert_eq!(expanded, home.join(".config/quod/alice.json"));
            assert_eq!(expanded_win, home.join(".config\\quod\\alice.json"));
        }

        let join_attr = Cli::try_parse_from([
            "quod",
            "join",
            "--actor-id",
            "alice",
            "--key-file",
            "~/.config/quod/alice.json",
            "--lineage-cluster",
            "claude-opus",
            "--display-name",
            "Alice",
            "--git-email",
            "alice@users.noreply.github.com",
        ])
        .unwrap();
        assert!(matches!(
            join_attr.command,
            Commands::Join {
                display_name: Some(_),
                git_email: Some(_),
                ..
            }
        ));

        let lb_cli = Cli::try_parse_from([
            "quod",
            "leaderboard",
            "--project",
            "erdos-479",
            "--limit",
            "10",
            "--json",
        ])
        .unwrap();
        assert!(matches!(
            lb_cli.command,
            Commands::Leaderboard {
                limit: 10,
                json: true,
                ..
            }
        ));

        let sk_list = Cli::try_parse_from(["quod", "skill", "--list"]).unwrap();
        assert!(matches!(sk_list.command, Commands::Skill { list: true, .. }));

        let sk_worker = Cli::try_parse_from(["quod", "skill", "worker", "--with-refs"]).unwrap();
        assert!(matches!(
            sk_worker.command,
            Commands::Skill {
                role: CliSkill::Worker,
                with_refs: true,
                list: false
            }
        ));

        for doc in [
            SKILL_ROUTER,
            SKILL_WORKER,
            SKILL_WORKER_CLI_REF,
            SKILL_WORKER_EXIT_CODES,
            SKILL_WORKER_TASK_GUIDE,
            SKILL_ORCHESTRATOR,
            SKILL_AUDITOR,
            SKILL_BLUEPRINTER,
        ] {
            assert!(!doc.trim().is_empty());
        }
    }
}
