//! Quod HTTP coordinator library (`quod-hub`) exposing multi-project `/api` control-plane
//! routes, unified Ed25519 Actor RBAC, minimal-import Spec gating, Stage 0 gating, and `I1` IR/algo APIs.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use quod_core::{
    ActorAuthContext, ActorProfile, AlgoQueryResult, ApplyIrRequest, ApplyIrResponse,
    AuditEpochRequest, AuditVerdict, ClaimLeaseRequest, CreateEpochRequest, CreateInviteRequest,
    CreateProjectRequest, DispatchAlgo, DispatchPolicyConfig, EpochStatusView, EventRecordView,
    GraphIrOp, HEADER_ACTOR_ID, HEADER_ADMIN_TOKEN, HEADER_SIGNATURE, HEADER_TIMESTAMP,
    HeartbeatResponse, InviteTokenView, JoinActorRequest, LeaderboardEntryView, OverlaySourceView,
    ProjectGraphView, ProjectSummaryView, RegisterActorRequest, RegisterBundleRequest, Role,
    SubmissionKind, SubmissionStatusView, SubmitRequest, SubmitResponse, TaskPacket, TaskRole,
    ToolchainBundle, WakeupDigestView, WhoAmIResponse, generate_keypair_hex, verify_request,
};
use quod_sandbox::{Stage0Policy, check_spec_minimal_imports, check_stage0, sha256_hex};
use quod_store::{
    BlobStore, NewSubmissionInput, QuodStore, StoreError, VerificationOutcomeInput,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const WEB_INDEX_HTML: &str = include_str!("../web/index.html");
const WEB_OPS_HTML: &str = include_str!("../web/ops.html");
const WEB_STYLES_CSS: &str = include_str!("../web/styles.css");
const WEB_APP_JS: &str = include_str!("../web/app.js");
const INSTALL_SH: &str = include_str!("../../../deploy/install.sh");
const JOIN_MD: &str = include_str!("../../../docs/JOIN.md");
const SKILL_ROUTER_MD: &str = include_str!("../../../skills/quod/SKILL.md");
const SKILL_WORKER_MD: &str = include_str!("../../../skills/quod-worker/SKILL.md");
const SKILL_WORKER_CLI_REF_MD: &str =
    include_str!("../../../skills/quod-worker/references/cli-reference.md");
const SKILL_WORKER_EXIT_CODES_MD: &str =
    include_str!("../../../skills/quod-worker/references/exit-codes.md");
const SKILL_WORKER_TASK_GUIDE_MD: &str =
    include_str!("../../../skills/quod-worker/references/task-guide.md");
const SKILL_ORCHESTRATOR_MD: &str = include_str!("../../../skills/quod-orchestrator/SKILL.md");
const SKILL_AUDITOR_MD: &str = include_str!("../../../skills/quod-auditor/SKILL.md");
const SKILL_BLUEPRINTER_MD: &str = include_str!("../../../skills/quod-blueprinter/SKILL.md");

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn QuodStore>,
    pub blobs: Arc<dyn BlobStore>,
    pub admin_token: String,
    pub db_configured: bool,
    pub lease_ttl_secs: i64,
    pub max_auth_skew_secs: i64,
    pub open_worker_join: bool,
    pub max_active_leases_per_actor: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    pub status: u16,
    pub code: String,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    hint: Option<String>,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        let code = match status {
            StatusCode::UNAUTHORIZED => "unauthorized",
            StatusCode::FORBIDDEN => "forbidden",
            StatusCode::NOT_FOUND => "not_found",
            StatusCode::CONFLICT => "conflict",
            StatusCode::UNPROCESSABLE_ENTITY => "unprocessable_entity",
            StatusCode::BAD_REQUEST => "bad_request",
            _ => "internal_error",
        };
        Self {
            status,
            code,
            message: message.into(),
            hint: None,
        }
    }

    fn with_code_and_hint(
        status: StatusCode,
        code: &'static str,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            hint: Some(hint.into()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                status: self.status.as_u16(),
                code: self.code.to_string(),
                error: self.message,
                hint: self.hint,
            }),
        )
            .into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound(m) => Self::new(StatusCode::NOT_FOUND, m),
            StoreError::Conflict(m) => {
                if m.contains("maximum number of active leases") {
                    Self::with_code_and_hint(
                        StatusCode::CONFLICT,
                        "lease_limit_reached",
                        m,
                        "Submit against or wait for your existing active lease to expire before claiming another task.",
                    )
                } else {
                    Self::new(StatusCode::CONFLICT, m)
                }
            }
            StoreError::Forbidden(m) => {
                if m.contains("separation of duties") {
                    Self::with_code_and_hint(
                        StatusCode::FORBIDDEN,
                        "self_audit_forbidden",
                        m,
                        "Have a different actor with the Auditor role review and sign off this epoch.",
                    )
                } else if m.contains("invite code required") {
                    Self::with_code_and_hint(
                        StatusCode::FORBIDDEN,
                        "invite_required",
                        m,
                        "Obtain an invite code via 'quod invite' from an Admin or project Blueprinter and pass --invite <CODE>.",
                    )
                } else {
                    Self::new(StatusCode::FORBIDDEN, m)
                }
            }
            StoreError::BadRequest(m) => Self::new(StatusCode::BAD_REQUEST, m),
            other => Self::new(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        }
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    db_connected: bool,
}

#[derive(Serialize)]
struct StatusResponse {
    service: &'static str,
    version: &'static str,
    schema_version: u32,
    db_configured: bool,
    stage0_max_bytes: usize,
    open_worker_join: bool,
    max_active_leases_per_actor: usize,
    online_agents: usize,
    active_leases: usize,
    registered_agents: usize,
}

#[derive(Deserialize)]
struct EventsQuery {
    since_seq: Option<i64>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct AlgoQuery {
    limit: Option<usize>,
}

async fn verify_actor_signature(
    state: &AppState,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<ActorAuthContext, ApiError> {
    let actor_id = headers
        .get(HEADER_ACTOR_ID)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing x-quod-actor-id"))?;
    let ts_raw = headers
        .get(HEADER_TIMESTAMP)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing x-quod-timestamp"))?;
    let ts: i64 = ts_raw
        .parse()
        .map_err(|_| ApiError::new(StatusCode::UNAUTHORIZED, "invalid x-quod-timestamp"))?;
    let sig = headers
        .get(HEADER_SIGNATURE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing x-quod-signature"))?;

    let creds = state
        .store
        .get_actor_auth(actor_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "unregistered actor"))?;

    let now_secs = Utc::now().timestamp();
    verify_request(
        &creds.pubkey_ed25519,
        method,
        path,
        ts,
        now_secs,
        state.max_auth_skew_secs,
        body,
        sig,
    )
    .map_err(|e| {
        ApiError::with_code_and_hint(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            e.to_string(),
            "Provide valid x-quod-actor-id, x-quod-timestamp, and Ed25519 x-quod-signature headers.",
        )
    })?;

    if !creds.auth.enabled {
        return Err(ApiError::with_code_and_hint(
            StatusCode::FORBIDDEN,
            "actor_disabled",
            "actor is disabled",
            "Ask an Admin or Orchestrator with authority to re-enable this actor.",
        ));
    }

    Ok(creds.auth)
}

fn require_role(
    auth: &ActorAuthContext,
    allowed_roles: &[Role],
    project_id: Option<&str>,
) -> Result<(), ApiError> {
    if !auth.has_any_role(allowed_roles) {
        return Err(ApiError::with_code_and_hint(
            StatusCode::FORBIDDEN,
            "role_denied",
            format!(
                "actor '{}' lacks required role ({:?}); has {:?}",
                auth.actor_id, allowed_roles, auth.roles
            ),
            "Run 'quod whoami' to inspect your authorized roles and project_scopes.",
        ));
    }
    if let Some(pid) = project_id
        && !auth.can_access_project(pid)
    {
        return Err(ApiError::with_code_and_hint(
            StatusCode::FORBIDDEN,
            "project_scope_denied",
            format!("actor '{}' is not scoped to project '{pid}'", auth.actor_id),
            "Request project scope access from an Admin or target a project listed in 'quod whoami'.",
        ));
    }
    Ok(())
}

async fn healthz(State(state): State<Arc<AppState>>) -> (StatusCode, Json<HealthResponse>) {
    let db_connected = state.store.health_check().await.is_ok();
    let status = if state.db_configured && !db_connected {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(HealthResponse {
            status: if status == StatusCode::OK {
                "ok"
            } else {
                "degraded"
            },
            service: "quod-hub",
            db_connected,
        }),
    )
}

async fn web_index() -> Html<&'static str> {
    Html(WEB_INDEX_HTML)
}

async fn web_ops() -> Html<&'static str> {
    Html(WEB_OPS_HTML)
}

async fn web_styles() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], WEB_STYLES_CSS)
}

async fn web_app_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        WEB_APP_JS,
    )
}

async fn web_install_sh() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        INSTALL_SH,
    )
}

async fn api_skills_index() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "install_script": "/install.sh",
        "cli_command": "quod skill <router|worker|orchestrator|auditor|blueprinter>",
        "skills": {
            "router": "/skills/quod/SKILL.md",
            "worker": "/skills/quod-worker/SKILL.md",
            "orchestrator": "/skills/quod-orchestrator/SKILL.md",
            "auditor": "/skills/quod-auditor/SKILL.md",
            "blueprinter": "/skills/quod-blueprinter/SKILL.md",
        },
        "worker_references": [
            "/skills/quod-worker/references/cli-reference.md",
            "/skills/quod-worker/references/exit-codes.md",
            "/skills/quod-worker/references/task-guide.md",
        ]
    }))
}

async fn web_skill_md(Path(name): Path<String>) -> Result<impl IntoResponse, ApiError> {
    let body = match name.as_str() {
        "quod" | "router" => SKILL_ROUTER_MD,
        "quod-worker" | "worker" => SKILL_WORKER_MD,
        "quod-orchestrator" | "orchestrator" => SKILL_ORCHESTRATOR_MD,
        "quod-auditor" | "auditor" => SKILL_AUDITOR_MD,
        "quod-blueprinter" | "blueprinter" => SKILL_BLUEPRINTER_MD,
        _ => {
            return Err(ApiError::with_code_and_hint(
                StatusCode::NOT_FOUND,
                "skill_not_found",
                format!("unknown skill '{name}'"),
                "use one of: quod, quod-worker, quod-orchestrator, quod-auditor, quod-blueprinter",
            ));
        }
    };
    Ok(([(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], body))
}

async fn web_worker_ref_md(Path(file): Path<String>) -> Result<impl IntoResponse, ApiError> {
    let body = match file.as_str() {
        "cli-reference.md" => SKILL_WORKER_CLI_REF_MD,
        "exit-codes.md" => SKILL_WORKER_EXIT_CODES_MD,
        "task-guide.md" => SKILL_WORKER_TASK_GUIDE_MD,
        _ => {
            return Err(ApiError::with_code_and_hint(
                StatusCode::NOT_FOUND,
                "skill_ref_not_found",
                format!("unknown worker reference '{file}'"),
                "use one of: cli-reference.md, exit-codes.md, task-guide.md",
            ));
        }
    };
    Ok(([(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], body))
}

async fn web_join_md() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], JOIN_MD)
}

async fn web_join_txt() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], JOIN_MD)
}

#[derive(Debug, Deserialize)]
struct LeaderboardQuery {
    project: Option<String>,
    #[serde(default = "default_leaderboard_limit")]
    limit: usize,
}

fn default_leaderboard_limit() -> usize {
    50
}

async fn api_leaderboard(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LeaderboardQuery>,
) -> Result<Json<Vec<LeaderboardEntryView>>, ApiError> {
    let project = q.project.as_deref().filter(|s| !s.trim().is_empty());
    let rows = state.store.leaderboard(project, q.limit).await?;
    Ok(Json(rows))
}

async fn api_status(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    let policy = Stage0Policy::default();
    let metrics = state.store.live_metrics().await.unwrap_or_default();
    Json(StatusResponse {
        service: "quod-hub",
        version: env!("CARGO_PKG_VERSION"),
        schema_version: 2,
        db_configured: state.db_configured,
        stage0_max_bytes: policy.max_bytes,
        open_worker_join: state.open_worker_join,
        max_active_leases_per_actor: state.max_active_leases_per_actor,
        online_agents: metrics.online_agents,
        active_leases: metrics.active_leases,
        registered_agents: metrics.registered_agents,
    })
}

async fn api_whoami(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Json<WhoAmIResponse>, ApiError> {
    let auth = verify_actor_signature(&state, &headers, "GET", uri.path(), &[]).await?;
    let info = state
        .store
        .whoami(&auth.actor_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "actor not found"))?;
    Ok(Json(info))
}

async fn api_list_projects(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ProjectSummaryView>>, ApiError> {
    Ok(Json(state.store.list_projects().await?))
}

async fn api_epoch_status(
    State(state): State<Arc<AppState>>,
    Path((project_id, epoch_id)): Path<(String, u32)>,
) -> Result<Json<EpochStatusView>, ApiError> {
    let view = state
        .store
        .epoch_status(&project_id, epoch_id)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                format!("epoch ({project_id}, {epoch_id}) not found"),
            )
        })?;
    Ok(Json(view))
}

async fn api_project_graph(
    State(state): State<Arc<AppState>>,
    Path((project_id, epoch_id)): Path<(String, u32)>,
) -> Result<Json<ProjectGraphView>, ApiError> {
    let snap = state
        .store
        .project_graph(&project_id, epoch_id)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                format!("epoch ({project_id}, {epoch_id}) not found"),
            )
        })?;
    Ok(Json(snap))
}

async fn api_epoch_events(
    State(state): State<Arc<AppState>>,
    Path((project_id, epoch_id)): Path<(String, u32)>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<EventRecordView>>, ApiError> {
    let events = state
        .store
        .recent_events(&project_id, epoch_id, q.since_seq, q.limit.unwrap_or(50).min(500))
        .await?;
    Ok(Json(events))
}

async fn api_overlay_source(
    State(state): State<Arc<AppState>>,
    Path((project_id, epoch_id, module_name)): Path<(String, u32, String)>,
) -> Result<Json<OverlaySourceView>, ApiError> {
    let raw_or_sha = state
        .store
        .get_overlay_blob(&project_id, epoch_id, &module_name)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                format!("overlay {module_name} not found"),
            )
        })?;

    let (blob_sha256, code_utf8) = if raw_or_sha.len() == 64
        && raw_or_sha.bytes().all(|b| b.is_ascii_hexdigit())
    {
        match state.blobs.get_bytes(&raw_or_sha) {
            Ok(bytes) => (raw_or_sha, String::from_utf8_lossy(&bytes).into_owned()),
            Err(_) => (String::new(), raw_or_sha),
        }
    } else {
        (String::new(), raw_or_sha)
    };

    Ok(Json(OverlaySourceView {
        project_id,
        epoch_id,
        module_name,
        blob_sha256,
        code_utf8,
    }))
}

async fn admin_register_actor(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Json<ActorProfile>, ApiError> {
    let bootstrap_ok = headers
        .get(HEADER_ADMIN_TOKEN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|tok| tok == state.admin_token);
    if !bootstrap_ok {
        let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
        require_role(&auth, &[Role::Admin], None)?;
    }
    let req: RegisterActorRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let profile = state.store.register_actor(&req).await?;
    Ok(Json(profile))
}

async fn admin_register_bundle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Json<ToolchainBundle>, ApiError> {
    let bootstrap_ok = headers
        .get(HEADER_ADMIN_TOKEN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|tok| tok == state.admin_token);
    if !bootstrap_ok {
        let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
        require_role(&auth, &[Role::Admin, Role::Blueprinter], None)?;
    }
    let req: RegisterBundleRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let bundle = state.store.register_bundle(&req).await?;
    Ok(Json(bundle))
}

async fn create_invite(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Json<InviteTokenView>, ApiError> {
    let bootstrap_ok = headers
        .get(HEADER_ADMIN_TOKEN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|tok| tok == state.admin_token);
    let mut req: CreateInviteRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    if req.roles.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invite must specify at least one role",
        ));
    }
    let created_by = if bootstrap_ok {
        "admin-bootstrap".to_string()
    } else {
        let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
        require_role(&auth, &[Role::Admin, Role::Blueprinter], None)?;
        if !auth.has_role(Role::Admin) {
            if req
                .roles
                .iter()
                .any(|r| !matches!(r, Role::Worker | Role::Orchestrator))
            {
                return Err(ApiError::with_code_and_hint(
                    StatusCode::FORBIDDEN,
                    "delegation_role_denied",
                    "Blueprinter invites may only grant Worker or Orchestrator roles",
                    "Request Admin intervention to onboard Blueprinter, Auditor, or Admin actors.",
                ));
            }
            if req.project_scopes.is_empty()
                || req
                    .project_scopes
                    .iter()
                    .any(|p| !auth.can_access_project(p))
            {
                return Err(ApiError::with_code_and_hint(
                    StatusCode::FORBIDDEN,
                    "delegation_scope_required",
                    "Blueprinter invites must specify explicit project_scopes within the issuer's authorized projects",
                    "Pass one or more --project-scope values that your Blueprinter account manages.",
                ));
            }
            if let Some(t) = req.initial_tier {
                req.initial_tier = Some(t.clamp(1, 3));
            }
        }
        auth.actor_id
    };
    let invite = state.store.create_invite(&created_by, &req).await?;
    Ok(Json(invite))
}

async fn join_actor(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JoinActorRequest>,
) -> Result<Json<ActorProfile>, ApiError> {
    let profile = state.store.join_actor(&req, state.open_worker_join).await?;
    Ok(Json(profile))
}

async fn blueprint_create_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Json<ProjectSummaryView>, ApiError> {
    let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
    let req: CreateProjectRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    require_role(&auth, &[Role::Blueprinter, Role::Admin], Some(&req.project_id))?;
    let summary = state.store.create_project(&auth.actor_id, &req).await?;
    Ok(Json(summary))
}

async fn blueprint_create_epoch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    Path(project_id): Path<String>,
    body: Bytes,
) -> Result<Json<EpochStatusView>, ApiError> {
    let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
    require_role(&auth, &[Role::Blueprinter, Role::Admin], Some(&project_id))?;
    let mut req: CreateEpochRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    if let Some(code) = &req.spec_code {
        let violations = check_spec_minimal_imports(code);
        if !violations.is_empty() {
            return Err(ApiError::with_code_and_hint(
                StatusCode::UNPROCESSABLE_ENTITY,
                "minimal_import_violation",
                format!("Spec minimal-import gate failed: {}", violations[0].detail),
                "Replace blanket 'import Mathlib' with fine-grained module imports (e.g. Mathlib.Data.Int.ModEq).",
            ));
        }
        let sha = state
            .blobs
            .put_bytes(code.as_bytes())
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if req.spec_sha256.is_empty() {
            req.spec_sha256 = sha;
        }
    }
    let status = state
        .store
        .create_epoch(&project_id, &auth.actor_id, &req)
        .await?;
    Ok(Json(status))
}

async fn audit_epoch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    Path((project_id, epoch_id)): Path<(String, u32)>,
    body: Bytes,
) -> Result<Json<EpochStatusView>, ApiError> {
    let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
    require_role(&auth, &[Role::Auditor, Role::Admin], Some(&project_id))?;
    let req: AuditEpochRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let status = state
        .store
        .audit_epoch(&project_id, epoch_id, &auth.actor_id, &req)
        .await?;
    Ok(Json(status))
}

async fn blueprint_freeze_epoch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    Path((project_id, epoch_id)): Path<(String, u32)>,
    body: Bytes,
) -> Result<Json<EpochStatusView>, ApiError> {
    let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
    require_role(&auth, &[Role::Blueprinter, Role::Admin], Some(&project_id))?;
    let status = state
        .store
        .freeze_epoch(&project_id, epoch_id, &auth.actor_id)
        .await?;
    Ok(Json(status))
}

async fn orch_digest(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    Path((project_id, epoch_id)): Path<(String, u32)>,
) -> Result<Json<WakeupDigestView>, ApiError> {
    let auth = verify_actor_signature(&state, &headers, "GET", uri.path(), &[]).await?;
    require_role(&auth, &[Role::Orchestrator, Role::Admin], Some(&project_id))?;
    let digest = state
        .store
        .wakeup_digest(&project_id, epoch_id)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                format!("epoch ({project_id}, {epoch_id}) not found"),
            )
        })?;
    Ok(Json(digest))
}

async fn orch_apply_ir(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    Path((project_id, epoch_id)): Path<(String, u32)>,
    body: Bytes,
) -> Result<Json<ApplyIrResponse>, ApiError> {
    let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
    require_role(&auth, &[Role::Orchestrator, Role::Admin], Some(&project_id))?;
    let req: ApplyIrRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let res = state
        .store
        .apply_ir(&project_id, epoch_id, &auth.actor_id, &req, state.blobs.as_ref())
        .await?;
    Ok(Json(res))
}

async fn orch_run_algo(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    Path((project_id, epoch_id, algo_name)): Path<(String, u32, String)>,
    Query(q): Query<AlgoQuery>,
) -> Result<Json<AlgoQueryResult>, ApiError> {
    let auth = verify_actor_signature(&state, &headers, "GET", uri.path(), &[]).await?;
    require_role(&auth, &[Role::Orchestrator, Role::Admin], Some(&project_id))?;
    let res = state
        .store
        .run_algo(&project_id, epoch_id, &algo_name, q.limit.unwrap_or(10))
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                format!("epoch ({project_id}, {epoch_id}) not found"),
            )
        })?;
    Ok(Json(res))
}

async fn worker_claim_lease(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    Path(project_id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
    require_role(&auth, &[Role::Worker, Role::Admin], Some(&project_id))?;
    let req: ClaimLeaseRequest = if body.is_empty() {
        ClaimLeaseRequest::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?
    };
    match state
        .store
        .claim_lease(
            &project_id,
            &auth.actor_id,
            &req,
            state.lease_ttl_secs,
            state.max_active_leases_per_actor,
        )
        .await?
    {
        Some(packet) => Ok((StatusCode::OK, Json::<TaskPacket>(packet)).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

async fn worker_heartbeat_lease(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    Path(lease_id): Path<Uuid>,
    body: Bytes,
) -> Result<Json<HeartbeatResponse>, ApiError> {
    let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
    require_role(&auth, &[Role::Worker, Role::Admin], None)?;
    let resp = state
        .store
        .heartbeat_lease(lease_id, &auth.actor_id, state.lease_ttl_secs)
        .await?;
    Ok(Json(resp))
}

async fn worker_submit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    Path(project_id): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<SubmitResponse>), ApiError> {
    let auth = verify_actor_signature(&state, &headers, "POST", uri.path(), &body).await?;
    require_role(&auth, &[Role::Worker, Role::Admin], Some(&project_id))?;
    let req: SubmitRequest = serde_json::from_slice(&body)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

    let stage0 = check_stage0(&req.code_utf8, Some(&req.spec_sha256), &Stage0Policy::default());
    let blob_sha256 = state
        .blobs
        .put_bytes(req.code_utf8.as_bytes())
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let diagnostics: Vec<String> = stage0
        .violations
        .into_iter()
        .map(|v| format!("line {}: {} ({})", v.line, v.detail, v.snippet))
        .collect();
    let received_at_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64;

    let resp = state
        .store
        .create_submission(NewSubmissionInput {
            submission_id: Uuid::now_v7(),
            project_id,
            actor_id: auth.actor_id,
            req,
            blob_sha256,
            received_at_ns,
            stage0_passed: stage0.passed,
            stage0_exit_code: stage0.exit_code,
            stage0_diagnostics: diagnostics,
        })
        .await?;

    let status = if stage0.passed {
        StatusCode::ACCEPTED
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    Ok((status, Json(resp)))
}

async fn worker_get_submission(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
    Path(submission_id): Path<Uuid>,
) -> Result<Json<SubmissionStatusView>, ApiError> {
    let _auth = verify_actor_signature(&state, &headers, "GET", uri.path(), &[]).await?;
    let sub = state
        .store
        .get_submission(submission_id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "submission not found"))?;
    Ok(Json(sub))
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(web_index))
        .route("/ops", get(web_ops))
        .route("/assets/styles.css", get(web_styles))
        .route("/assets/app.js", get(web_app_js))
        .route("/install.sh", get(web_install_sh))
        .route("/JOIN.md", get(web_join_md))
        .route("/JOIN.txt", get(web_join_txt))
        .route("/skills", get(api_skills_index))
        .route("/skills/{name}/SKILL.md", get(web_skill_md))
        .route("/skills/quod-worker/references/{file}", get(web_worker_ref_md))
        .route("/healthz", get(healthz))
        .route("/api/status", get(api_status))
        .route("/api/leaderboard", get(api_leaderboard))
        .route("/api/whoami", get(api_whoami))
        .route(
            "/api/projects",
            get(api_list_projects).post(blueprint_create_project),
        )
        .route(
            "/api/projects/{project_id}/epochs",
            post(blueprint_create_epoch),
        )
        .route(
            "/api/projects/{project_id}/epochs/{epoch_id}/audit",
            post(audit_epoch),
        )
        .route(
            "/api/projects/{project_id}/epochs/{epoch_id}/freeze",
            post(blueprint_freeze_epoch),
        )
        .route(
            "/api/projects/{project_id}/epochs/{epoch_id}/status",
            get(api_epoch_status),
        )
        .route(
            "/api/projects/{project_id}/epochs/{epoch_id}/graph",
            get(api_project_graph),
        )
        .route(
            "/api/projects/{project_id}/epochs/{epoch_id}/events",
            get(api_epoch_events),
        )
        .route(
            "/api/projects/{project_id}/epochs/{epoch_id}/overlays/{module_name}/source",
            get(api_overlay_source),
        )
        .route(
            "/api/projects/{project_id}/epochs/{epoch_id}/orch/digest",
            get(orch_digest),
        )
        .route(
            "/api/projects/{project_id}/epochs/{epoch_id}/orch/ir",
            post(orch_apply_ir),
        )
        .route(
            "/api/projects/{project_id}/epochs/{epoch_id}/algo/{algo_name}",
            get(orch_run_algo),
        )
        .route("/api/admin/actors", post(admin_register_actor))
        .route("/api/admin/bundles", post(admin_register_bundle))
        .route("/api/invites", post(create_invite))
        .route("/api/actors/join", post(join_actor))
        .route(
            "/api/projects/{project_id}/leases/claim",
            post(worker_claim_lease),
        )
        .route(
            "/api/leases/{lease_id}/heartbeat",
            post(worker_heartbeat_lease),
        )
        .route(
            "/api/projects/{project_id}/submissions",
            post(worker_submit),
        )
        .route(
            "/api/submissions/{submission_id}",
            get(worker_get_submission),
        )
        .with_state(state)
}

/// Seeds realistic multi-project, multi-route demo data for local portal preview (`--seed-demo`).
pub async fn seed_demo_data(store: &dyn QuodStore, blobs: &dyn BlobStore) -> anyhow::Result<()> {
    if !store.list_projects().await?.is_empty() {
        return Ok(());
    }

    store
        .register_bundle(&RegisterBundleRequest {
            bundle_id: "mathlib-v4.36.0".into(),
            lean_toolchain: "leanprover/lean4:v4.36.0".into(),
            mathlib_rev: "v4.36.0".into(),
            toolchain_dir: "/opt/quod/lean".into(),
            lean_paths: vec![],
            bundle_sha256: "bundle-demo-v436".into(),
        })
        .await?;

    for (id, roles, cluster, tier, display_name, git_email) in [
        ("bp-demo", vec![Role::Blueprinter], "human", 4, None, None),
        ("aud-demo", vec![Role::Auditor], "human", 4, None, None),
        ("orch-demo", vec![Role::Orchestrator], "claude-opus", 4, None, None),
        (
            "worker-opus",
            vec![Role::Worker],
            "claude-opus",
            3,
            Some("Alice (Opus)"),
            Some("alice@users.noreply.github.com"),
        ),
        (
            "worker-gemini",
            vec![Role::Worker],
            "gemini-pro",
            3,
            Some("Bob (Gemini)"),
            Some("bob@users.noreply.github.com"),
        ),
    ] {
        let (_, pk) = generate_keypair_hex();
        store
            .register_actor(&RegisterActorRequest {
                actor_id: id.into(),
                pubkey_ed25519: pk,
                roles,
                project_scopes: vec![],
                lineage_cluster: cluster.into(),
                initial_tier: Some(tier),
                skill_tags: vec!["lean4".into(), "number-theory".into()],
                display_name: display_name.map(str::to_string),
                git_email: git_email.map(str::to_string),
            })
            .await?;
    }

    // Project 1: erdos-479 (Active showcase with Routes, Verified Overlays, Refutation & Active Lease)
    store
        .create_project(
            "bp-demo",
            &CreateProjectRequest {
                project_id: "erdos-479".into(),
                title: "Erdős 问题 479：2^n ≡ k (mod n) 是否有无穷多解".into(),
                description: "对任意整数 k ≠ 1，证明存在无穷多个正整数 n 满足 2^n 与 k 模 n 同余；下方展示由外部 Agent 分拆攻关的证明思路与已通过 Lean 4 核验的引理。".into(),
                spec_namespace: "Quod.Epoch1".into(),
                default_bundle_id: Some("mathlib-v4.36.0".into()),
            },
        )
        .await?;

    let spec_code = "\
import Mathlib.Data.Int.ModEq\n\
import Mathlib.Data.Set.Finite.Basic\n\n\
namespace Quod.Epoch1\n\n\
def IsGoodK (k : Int) : Prop :=\n\
  { n : Nat | (2 ^ n : Int) ≡ k [ZMOD (n : Int)] }.Infinite\n\n\
def erdos_479_stmt : Prop :=\n\
  ∀ k : Int, k ≠ 1 → IsGoodK k\n\n\
end Quod.Epoch1\n";
    let spec_sha = blobs.put_bytes(spec_code.as_bytes())?;

    store
        .create_epoch(
            "erdos-479",
            "bp-demo",
            &CreateEpochRequest {
                epoch_id: 1,
                bundle_id: "mathlib-v4.36.0".into(),
                base_commit: "demo-head".into(),
                spec_module: "Quod.Epoch1.Spec".into(),
                spec_decl_name: "Quod.Epoch1.erdos_479_stmt".into(),
                spec_statement: "∀ k : Int, k ≠ 1 → IsGoodK k".into(),
                spec_sha256: spec_sha.clone(),
                spec_code: Some(spec_code.into()),
                root_task_id: "root".into(),
                auto_activate: false,
            },
        )
        .await?;

    store
        .audit_epoch(
            "erdos-479",
            1,
            "aud-demo",
            &AuditEpochRequest {
                approve: true,
                comment: "Minimal Mathlib imports and faithful k ≠ 1 quantifier order verified.".into(),
            },
        )
        .await?;

    let def_overlay_code = "\
import Quod.Epoch1.Spec\n\n\
def pow2Residue (n m : Nat) : Nat := (2 ^ n) % m\n\n\
def LiftingWitness (k : Int) (p : Nat) : Prop :=\n\
  p.Prime ∧ ∃ e : Nat, (2 ^ e : Int) ≡ k [ZMOD p]\n";

    store
        .apply_ir(
            "erdos-479",
            1,
            "orch-demo",
            &ApplyIrRequest {
                ops: vec![
                    GraphIrOp::PublishDefOverlay {
                        module_name: "QuodDef_Pow2Mod".into(),
                        code_utf8: def_overlay_code.into(),
                    },
                    GraphIrOp::CreateRoute {
                        route_id: "route-lifting".into(),
                        title: "素数阶提升 + 中国剩余定理构造".into(),
                        rationale: "通过辅助模数性质与 CRT 构造无穷多组满足同余条件的指数 n，并拆分为独立引理并行证明。".into(),
                    },
                    GraphIrOp::SpawnTask {
                        task_id: "lemma_base_cases".into(),
                        route_id: Some("route-lifting".into()),
                        task_role: TaskRole::Lemma,
                        spec_decl_name: "lemma_base_cases".into(),
                        spec_statement: "∀ k : Int, k ≤ 0 → k ≠ 1 → Quod.Epoch1.IsGoodK k".into(),
                        target_inline: true,
                        candidate_decl: "proof_base_cases".into(),
                        curated_imports: vec!["Quod.Epoch1.Spec".into()],
                        depends_on: vec![],
                        refutes_task_id: None,
                        module_hint: Some("Lifting".into()),
                        priority: 9.0,
                        prior_p: 0.85,
                        expected_cost_c: 1.0,
                        racer_cap_k: 2,
                        min_tier: 1,
                        preferred_lineages: vec![],
                        credit_bounty: Some(80),
                    },
                    GraphIrOp::SpawnTask {
                        task_id: "lemma_padic_valuation".into(),
                        route_id: Some("route-lifting".into()),
                        task_role: TaskRole::Lemma,
                        spec_decl_name: "lemma_padic_valuation".into(),
                        spec_statement: "∀ k : Int, k ≥ 2 → ∃ p : Nat, LiftingWitness k p".into(),
                        target_inline: true,
                        candidate_decl: "proof_padic_val".into(),
                        curated_imports: vec!["Quod.Epoch1.Spec".into(), "QuodDef_Pow2Mod".into()],
                        depends_on: vec![],
                        refutes_task_id: None,
                        module_hint: Some("Lifting".into()),
                        priority: 8.5,
                        prior_p: 0.72,
                        expected_cost_c: 1.2,
                        racer_cap_k: 2,
                        min_tier: 2,
                        preferred_lineages: vec![],
                        credit_bounty: Some(120),
                    },
                    GraphIrOp::SpawnTask {
                        task_id: "conn_lifting_suff".into(),
                        route_id: Some("route-lifting".into()),
                        task_role: TaskRole::Connector,
                        spec_decl_name: "conn_lifting_suff".into(),
                        spec_statement: "(∀ k : Int, k ≤ 0 → k ≠ 1 → Quod.Epoch1.IsGoodK k) → (∀ k : Int, k ≥ 2 → Quod.Epoch1.IsGoodK k) → Quod.Epoch1.erdos_479_stmt".into(),
                        target_inline: true,
                        candidate_decl: "proof_conn_lifting".into(),
                        curated_imports: vec!["Quod.Epoch1.Spec".into()],
                        depends_on: vec![],
                        refutes_task_id: None,
                        module_hint: Some("Lifting".into()),
                        priority: 10.0,
                        prior_p: 0.95,
                        expected_cost_c: 0.4,
                        racer_cap_k: 1,
                        min_tier: 1,
                        preferred_lineages: vec![],
                        credit_bounty: Some(60),
                    },
                    GraphIrOp::SpawnTask {
                        task_id: "lemma_order_growth".into(),
                        route_id: Some("route-lifting".into()),
                        task_role: TaskRole::Lemma,
                        spec_decl_name: "lemma_order_growth".into(),
                        spec_statement: "∀ k : Int, k ≥ 2 → LiftingWitness k 3 → Quod.Epoch1.IsGoodK k".into(),
                        target_inline: true,
                        candidate_decl: "proof_order_growth".into(),
                        curated_imports: vec!["Quod.Epoch1.Spec".into(), "QuodDef_Pow2Mod".into()],
                        depends_on: vec!["lemma_padic_valuation".into()],
                        refutes_task_id: None,
                        module_hint: Some("Lifting".into()),
                        priority: 7.5,
                        prior_p: 0.6,
                        expected_cost_c: 1.8,
                        racer_cap_k: 2,
                        min_tier: 2,
                        preferred_lineages: vec!["claude-opus".into()],
                        credit_bounty: Some(180),
                    },
                    GraphIrOp::SpawnTask {
                        task_id: "merge_lifting_root".into(),
                        route_id: Some("route-lifting".into()),
                        task_role: TaskRole::Merge,
                        spec_decl_name: "Quod.Epoch1.erdos_479_stmt".into(),
                        spec_statement: String::new(),
                        target_inline: false,
                        candidate_decl: "candidate".into(),
                        curated_imports: vec!["Quod.Epoch1.Spec".into()],
                        depends_on: vec![
                            "lemma_base_cases".into(),
                            "conn_lifting_suff".into(),
                            "lemma_order_growth".into(),
                        ],
                        refutes_task_id: None,
                        module_hint: Some("Lifting".into()),
                        priority: 12.0,
                        prior_p: 0.9,
                        expected_cost_c: 0.8,
                        racer_cap_k: 2,
                        min_tier: 2,
                        preferred_lineages: vec![],
                        credit_bounty: Some(250),
                    },
                    GraphIrOp::CreateRoute {
                        route_id: "route-elementary".into(),
                        title: "单一固定模数试探（已被反例排除）".into(),
                        rationale: "检验能否用统一固定模数覆盖所有 k，已被具体反例推翻并自动剪枝。".into(),
                    },
                    GraphIrOp::SpawnTask {
                        task_id: "claim_uniform_bound".into(),
                        route_id: Some("route-elementary".into()),
                        task_role: TaskRole::Lemma,
                        spec_decl_name: "claim_uniform_bound".into(),
                        spec_statement: "∀ k n : Nat, (2 ^ n) % k = 1".into(),
                        target_inline: true,
                        candidate_decl: "proof_uniform_bound".into(),
                        curated_imports: vec![],
                        depends_on: vec![],
                        refutes_task_id: None,
                        module_hint: None,
                        priority: 2.0,
                        prior_p: 0.2,
                        expected_cost_c: 1.0,
                        racer_cap_k: 1,
                        min_tier: 1,
                        preferred_lineages: vec![],
                        credit_bounty: Some(60),
                    },
                    GraphIrOp::SpawnTask {
                        task_id: "refute_uniform_bound".into(),
                        route_id: Some("route-elementary".into()),
                        task_role: TaskRole::Refutation,
                        spec_decl_name: "claim_uniform_bound".into(),
                        spec_statement: "∀ k n : Nat, (2 ^ n) % k = 1".into(),
                        target_inline: true,
                        candidate_decl: "candidate".into(),
                        curated_imports: vec![],
                        depends_on: vec![],
                        refutes_task_id: Some("claim_uniform_bound".into()),
                        module_hint: None,
                        priority: 5.0,
                        prior_p: 0.7,
                        expected_cost_c: 0.6,
                        racer_cap_k: 1,
                        min_tier: 1,
                        preferred_lineages: vec![],
                        credit_bounty: Some(150),
                    },
                    GraphIrOp::CreateRoute {
                        route_id: "route-analytic".into(),
                        title: "解析筛法渐近估计（已搁置剪枝）".into(),
                        rationale: "渐近密度思路成本过高，已让位给构造性更好的 CRT 提升方案。".into(),
                    },
                    GraphIrOp::SpawnTask {
                        task_id: "lemma_sieve_density".into(),
                        route_id: Some("route-analytic".into()),
                        task_role: TaskRole::Lemma,
                        spec_decl_name: "lemma_sieve_density".into(),
                        spec_statement: "True".into(),
                        target_inline: true,
                        candidate_decl: "proof_sieve".into(),
                        curated_imports: vec![],
                        depends_on: vec![],
                        refutes_task_id: None,
                        module_hint: None,
                        priority: 1.0,
                        prior_p: 0.3,
                        expected_cost_c: 3.0,
                        racer_cap_k: 1,
                        min_tier: 2,
                        preferred_lineages: vec![],
                        credit_bounty: Some(200),
                    },
                    GraphIrOp::SetDispatchPolicy {
                        config: DispatchPolicyConfig {
                            algo: DispatchAlgo::WeightedVoi,
                            exploration_rate: 0.08,
                            allow_cross_route_parallel: true,
                        },
                    },
                ],
                dry_run: false,
            },
            blobs,
        )
        .await?;

    // Complete DefOverlay job
    if let Some(job_def) = store.claim_next_verification_job("verifier-demo").await? {
        let sha = sha256_hex(def_overlay_code.as_bytes());
        store
            .record_verification_outcome(VerificationOutcomeInput {
                job_id: job_def.job_id,
                submission_id: job_def.submission_id,
                verdict: AuditVerdict {
                    ok: true,
                    exit_code: 0,
                    kind: SubmissionKind::DefOverlay,
                    code_sha256: sha,
                    compile_ms: 145,
                    diagnostics: vec![],
                },
            })
            .await?;
    }

    // Complete 3 proved tasks + 1 refutation to populate Overlays & unblock lemma_order_growth
    for (task_id, actor_id, kind, code, compile_ms) in [
        (
            "conn_lifting_suff",
            "worker-opus",
            SubmissionKind::Proof,
            "import Quod.Epoch1.Spec\ntheorem proof_conn_lifting : (∀ k : Int, k ≤ 0 → k ≠ 1 → Quod.Epoch1.IsGoodK k) → (∀ k : Int, k ≥ 2 → Quod.Epoch1.IsGoodK k) → Quod.Epoch1.erdos_479_stmt := by\n  intro hneg hpos k hk\n  omega\n",
            92u64,
        ),
        (
            "lemma_base_cases",
            "worker-gemini",
            SubmissionKind::Proof,
            "import Quod.Epoch1.Spec\ntheorem proof_base_cases : ∀ k : Int, k ≤ 0 → k ≠ 1 → Quod.Epoch1.IsGoodK k := by\n  intro k hk hne\n  omega\n",
            184u64,
        ),
        (
            "lemma_padic_valuation",
            "worker-opus",
            SubmissionKind::Proof,
            "import Quod.Epoch1.Spec\nimport QuodDef_Pow2Mod\ntheorem proof_padic_val : ∀ k : Int, k ≥ 2 → ∃ p : Nat, LiftingWitness k p := by\n  intro k hk\n  exact ⟨3, by decide, 0, by decide⟩\n",
            268u64,
        ),
        (
            "refute_uniform_bound",
            "worker-gemini",
            SubmissionKind::Refutation,
            "theorem candidate : ¬ (∀ k n : Nat, (2 ^ n) % k = 1) := by\n  intro h\n  have h0 := h 2 1\n  norm_num at h0\n",
            64u64,
        ),
    ] {
        if let Some(pkt) = store
            .claim_lease(
                "erdos-479",
                actor_id,
                &ClaimLeaseRequest {
                    preferred_task_id: Some(task_id.into()),
                    preferred_route_id: None,
                },
                900,
                2,
            )
            .await?
        {
            let sha = blobs.put_bytes(code.as_bytes())?;
            let sub = store
                .create_submission(NewSubmissionInput {
                    submission_id: Uuid::now_v7(),
                    project_id: "erdos-479".into(),
                    actor_id: actor_id.into(),
                    req: SubmitRequest {
                        epoch_id: 1,
                        task_id: task_id.into(),
                        lease_id: pkt.lease.unwrap().lease_id,
                        kind,
                        spec_sha256: spec_sha.clone(),
                        code_utf8: code.into(),
                        idea_attr_id: None,
                    },
                    blob_sha256: sha.clone(),
                    received_at_ns: 1,
                    stage0_passed: true,
                    stage0_exit_code: 0,
                    stage0_diagnostics: vec![],
                })
                .await?;
            if let Some(job) = store.claim_next_verification_job("verifier-demo").await? {
                store
                    .record_verification_outcome(VerificationOutcomeInput {
                        job_id: job.job_id,
                        submission_id: sub.submission_id,
                        verdict: AuditVerdict {
                            ok: true,
                            exit_code: 0,
                            kind,
                            code_sha256: sha,
                            compile_ms,
                            diagnostics: vec![],
                        },
                    })
                    .await?;
            }
        }
    }

    // Prune route-analytic and let worker-opus actively race on unlocked lemma_order_growth
    store
        .apply_ir(
            "erdos-479",
            1,
            "orch-demo",
            &ApplyIrRequest {
                ops: vec![GraphIrOp::PruneRoute {
                    route_id: "route-analytic".into(),
                    reason: "Subsumed by prime lifting + CRT construction".into(),
                }],
                dry_run: false,
            },
            blobs,
        )
        .await?;

    let _active_lease = store
        .claim_lease(
            "erdos-479",
            "worker-opus",
            &ClaimLeaseRequest {
                preferred_task_id: Some("lemma_order_growth".into()),
                preferred_route_id: None,
            },
            900,
            2,
        )
        .await?;

    // Project 2: smoke-audit-demo (PENDING_AUDIT showcase)
    store
        .create_project(
            "bp-demo",
            &CreateProjectRequest {
                project_id: "smoke-audit-demo".into(),
                title: "待审核示例课题：自然数加法交换律".into(),
                description: "该示例展示课题立项后等待独立审核员确认规范无空真漏洞的状态。".into(),
                spec_namespace: "Quod.AuditDemo".into(),
                default_bundle_id: Some("mathlib-v4.36.0".into()),
            },
        )
        .await?;
    let audit_spec = "theorem audit_demo_stmt : ∀ a b : Nat, a + b = b + a := Nat.add_comm\n";
    let audit_sha = blobs.put_bytes(audit_spec.as_bytes())?;
    store
        .create_epoch(
            "smoke-audit-demo",
            "bp-demo",
            &CreateEpochRequest {
                epoch_id: 1,
                bundle_id: "mathlib-v4.36.0".into(),
                base_commit: "demo-head".into(),
                spec_module: "Quod.AuditDemo.Spec".into(),
                spec_decl_name: "audit_demo_stmt".into(),
                spec_statement: "∀ a b : Nat, a + b = b + a".into(),
                spec_sha256: audit_sha,
                spec_code: Some(audit_spec.into()),
                root_task_id: "root".into(),
                auto_activate: false,
            },
        )
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use axum::body::Body;
    use axum::http::Request;
    use quod_core::{
        GraphIrOp, ProjectStatus, RouteState, SubmissionKind, TaskRole, generate_keypair_hex,
        sign_request,
    };
    use quod_store::{LocalFsBlobStore, MemoryStore};
    use quod_verifier::{RunnerMode, VerifierConfig, process_one_job};
    use tower::ServiceExt;

    use super::*;

    async fn signed_req(
        app: &Router,
        method: &str,
        path: &str,
        actor_id: &str,
        sk: &str,
        body: Vec<u8>,
    ) -> axum::response::Response {
        let ts = Utc::now().timestamp();
        let sig = sign_request(sk, method, path, ts, &body).unwrap();
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header(HEADER_ACTOR_ID, actor_id)
            .header(HEADER_TIMESTAMP, ts.to_string())
            .header(HEADER_SIGNATURE, sig)
            .body(Body::from(body))
            .unwrap();
        app.clone().oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn end_to_end_rbac_multi_role_and_lean_verification() {
        let store_concrete = MemoryStore::new();
        let store: Arc<dyn QuodStore> = Arc::new(store_concrete);
        let blob_tmp = tempfile::tempdir().unwrap();
        let blobs: Arc<dyn BlobStore> = Arc::new(LocalFsBlobStore::new(blob_tmp.path()));

        let state = Arc::new(AppState {
            store: store.clone(),
            blobs: blobs.clone(),
            admin_token: "test-admin".into(),
            db_configured: false,
            lease_ttl_secs: 600,
            max_auth_skew_secs: 60,
            open_worker_join: false,
            max_active_leases_per_actor: 1,
        });
        let app = build_router(state);

        // Register role-separated actors via bootstrap admin token:
        // bp (Blueprinter), aud (Auditor), orch (Orchestrator), worker (Worker).
        let (sk_bp, pk_bp) = generate_keypair_hex();
        let (sk_aud, pk_aud) = generate_keypair_hex();
        let (sk_orch, pk_orch) = generate_keypair_hex();
        let (sk_w, pk_w) = generate_keypair_hex();

        for (id, pk, roles) in [
            // Give bp-1 BOTH Blueprinter and Auditor roles so we can verify HTTP-level
            // separation-of-duties (`self_audit_forbidden`) cannot be bypassed via dual roles.
            ("bp-1", pk_bp, vec![Role::Blueprinter, Role::Auditor]),
            ("aud-1", pk_aud, vec![Role::Auditor]),
            ("orch-1", pk_orch, vec![Role::Orchestrator]),
            ("w-1", pk_w, vec![Role::Worker]),
        ] {
            let body = serde_json::to_vec(&RegisterActorRequest {
                actor_id: id.into(),
                pubkey_ed25519: pk,
                roles,
                project_scopes: vec![],
                lineage_cluster: "test".into(),
                initial_tier: Some(3),
                skill_tags: vec![],
                display_name: None,
                git_email: None,
            })
            .unwrap();
            let resp = app
                .clone()
                .oneshot(
                    Request::post("/api/admin/actors")
                        .header("content-type", "application/json")
                        .header(HEADER_ADMIN_TOKEN, "test-admin")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }

        // Register toolchain bundle via bootstrap token
        let bundle_body = serde_json::to_vec(&RegisterBundleRequest {
            bundle_id: "lean-4.36.0".into(),
            lean_toolchain: "leanprover/lean4:v4.36.0".into(),
            mathlib_rev: "none".into(),
            toolchain_dir: String::new(),
            lean_paths: vec![],
            bundle_sha256: "b1".into(),
        })
        .unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/admin/bundles")
                    .header("content-type", "application/json")
                    .header(HEADER_ADMIN_TOKEN, "test-admin")
                    .body(Body::from(bundle_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // RBAC check: worker CANNOT create project (403 Forbidden)
        let proj_body = serde_json::to_vec(&CreateProjectRequest {
            project_id: "smoke".into(),
            title: "Smoke".into(),
            description: "e2e".into(),
            spec_namespace: "Quod.Epoch0".into(),
            default_bundle_id: Some("lean-4.36.0".into()),
        })
        .unwrap();
        let denied = signed_req(&app, "POST", "/api/projects", "w-1", &sk_w, proj_body.clone()).await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let denied_err: ErrorBody = serde_json::from_slice(
            &axum::body::to_bytes(denied.into_body(), 64 * 1024).await.unwrap(),
        )
        .unwrap();
        assert_eq!(denied_err.code, "role_denied");
        assert!(denied_err.hint.is_some());

        // Blueprinter creates project
        let created = signed_req(&app, "POST", "/api/projects", "bp-1", &sk_bp, proj_body).await;
        assert_eq!(created.status(), StatusCode::OK);

        // Minimal-import gate rejects blanket `import Mathlib`
        let bad_epoch_body = serde_json::to_vec(&CreateEpochRequest {
            epoch_id: 1,
            bundle_id: "lean-4.36.0".into(),
            base_commit: "HEAD".into(),
            spec_module: "Spec".into(),
            spec_decl_name: "Quod.Epoch0.smoke_stmt".into(),
            spec_statement: String::new(),
            spec_sha256: String::new(),
            spec_code: Some("import Mathlib\ndef bad : Prop := True".into()),
            root_task_id: "root".into(),
            auto_activate: false,
        })
        .unwrap();
        let bad_ep = signed_req(
            &app,
            "POST",
            "/api/projects/smoke/epochs",
            "bp-1",
            &sk_bp,
            bad_epoch_body,
        )
        .await;
        assert_eq!(bad_ep.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // Blueprinter proposes valid Epoch 1 (PendingAudit)
        let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("epoch0-smoke");
        let spec_code = std::fs::read_to_string(root_dir.join("Spec.lean")).unwrap();
        let good_epoch_body = serde_json::to_vec(&CreateEpochRequest {
            epoch_id: 1,
            bundle_id: "lean-4.36.0".into(),
            base_commit: "HEAD".into(),
            spec_module: "Spec".into(),
            spec_decl_name: "Quod.Epoch0.smoke_stmt".into(),
            spec_statement: "∀ n : Nat, (n + 1) + (n + 1) = 2 * n + 2".into(),
            spec_sha256: "spec-sha".into(),
            spec_code: Some(spec_code),
            root_task_id: "root".into(),
            auto_activate: false,
        })
        .unwrap();
        let prop_ep = signed_req(
            &app,
            "POST",
            "/api/projects/smoke/epochs",
            "bp-1",
            &sk_bp,
            good_epoch_body,
        )
        .await;
        assert_eq!(prop_ep.status(), StatusCode::OK);

        // Blueprinter cannot self-audit their own proposed Epoch (403 self_audit_forbidden)
        let self_audit_body = serde_json::to_vec(&AuditEpochRequest {
            approve: true,
            comment: "self approval attempt".into(),
        })
        .unwrap();
        let self_aud_resp = signed_req(
            &app,
            "POST",
            "/api/projects/smoke/epochs/1/audit",
            "bp-1",
            &sk_bp,
            self_audit_body,
        )
        .await;
        assert_eq!(self_aud_resp.status(), StatusCode::FORBIDDEN);
        let self_aud_err: ErrorBody = serde_json::from_slice(
            &axum::body::to_bytes(self_aud_resp.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(self_aud_err.code, "self_audit_forbidden");

        // Auditor approves Epoch 1 -> Active
        let audit_body = serde_json::to_vec(&AuditEpochRequest {
            approve: true,
            comment: "Valid statement".into(),
        })
        .unwrap();
        let aud_resp = signed_req(
            &app,
            "POST",
            "/api/projects/smoke/epochs/1/audit",
            "aud-1",
            &sk_aud,
            audit_body,
        )
        .await;
        assert_eq!(aud_resp.status(), StatusCode::OK);

        // Blueprinter cannot mint Auditor/global invite (403 delegation_role_denied),
        // but CAN mint a project-scoped Worker invite and join a new agent via /api/actors/join.
        let bad_inv_body = serde_json::to_vec(&CreateInviteRequest {
            roles: vec![Role::Auditor],
            project_scopes: vec!["smoke".into()],
            max_uses: 1,
            expires_in_secs: 3600,
            initial_tier: Some(2),
            note: "illegal".into(),
        })
        .unwrap();
        let bad_inv = signed_req(&app, "POST", "/api/invites", "bp-1", &sk_bp, bad_inv_body).await;
        assert_eq!(bad_inv.status(), StatusCode::FORBIDDEN);
        let bad_inv_err: ErrorBody = serde_json::from_slice(
            &axum::body::to_bytes(bad_inv.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(bad_inv_err.code, "delegation_role_denied");

        let ok_inv_body = serde_json::to_vec(&CreateInviteRequest {
            roles: vec![Role::Worker],
            project_scopes: vec!["smoke".into()],
            max_uses: 1,
            expires_in_secs: 3600,
            initial_tier: Some(2),
            note: "smoke worker".into(),
        })
        .unwrap();
        let ok_inv = signed_req(&app, "POST", "/api/invites", "bp-1", &sk_bp, ok_inv_body).await;
        assert_eq!(ok_inv.status(), StatusCode::OK);
        let issued: InviteTokenView = serde_json::from_slice(
            &axum::body::to_bytes(ok_inv.into_body(), 64 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();

        // Uninvited join is rejected when open_worker_join == false
        let (sk_w2, pk_w2) = generate_keypair_hex();
        let uninvited_body = serde_json::to_vec(&JoinActorRequest {
            actor_id: "w-joined".into(),
            pubkey_ed25519: pk_w2.clone(),
            lineage_cluster: "deepseek".into(),
            invite_code: None,
            skill_tags: vec![],
            display_name: None,
            git_email: None,
        })
        .unwrap();
        let uninvited_resp = app
            .clone()
            .oneshot(
                Request::post("/api/actors/join")
                    .header("content-type", "application/json")
                    .body(Body::from(uninvited_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(uninvited_resp.status(), StatusCode::FORBIDDEN);

        // Invited join succeeds and returns scoped Worker profile; verify w-joined can sign /api/whoami
        let join_body = serde_json::to_vec(&JoinActorRequest {
            actor_id: "w-joined".into(),
            pubkey_ed25519: pk_w2,
            lineage_cluster: "deepseek".into(),
            invite_code: Some(issued.invite_code),
            skill_tags: vec!["lean4".into()],
            display_name: None,
            git_email: None,
        })
        .unwrap();
        let join_resp = app
            .clone()
            .oneshot(
                Request::post("/api/actors/join")
                    .header("content-type", "application/json")
                    .body(Body::from(join_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(join_resp.status(), StatusCode::OK);
        let whoami_joined = signed_req(&app, "GET", "/api/whoami", "w-joined", &sk_w2, vec![]).await;
        assert_eq!(whoami_joined.status(), StatusCode::OK);

        // Orchestrator checks digest and applies IR: CreateRoute + DefOverlay + Lemma + Merge
        let dig_resp = signed_req(
            &app,
            "GET",
            "/api/projects/smoke/epochs/1/orch/digest",
            "orch-1",
            &sk_orch,
            vec![],
        )
        .await;
        assert_eq!(dig_resp.status(), StatusCode::OK);

        let def_code = std::fs::read_to_string(root_dir.join("samples/def_overlay_ok.lean")).unwrap();
        let ir_body = serde_json::to_vec(&ApplyIrRequest {
            ops: vec![
                GraphIrOp::CreateRoute {
                    route_id: "route-a".into(),
                    title: "Via helper def".into(),
                    rationale: "Def + Lemma + Merge".into(),
                },
                GraphIrOp::PublishDefOverlay {
                    module_name: "QuodDef_01".into(),
                    code_utf8: def_code,
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
            dry_run: true,
        })
        .unwrap();
        let dry_resp = signed_req(
            &app,
            "POST",
            "/api/projects/smoke/epochs/1/orch/ir",
            "orch-1",
            &sk_orch,
            ir_body.clone(),
        )
        .await;
        assert_eq!(dry_resp.status(), StatusCode::OK);
        let dry_parsed: ApplyIrResponse = serde_json::from_slice(
            &axum::body::to_bytes(dry_resp.into_body(), 1024 * 1024).await.unwrap(),
        )
        .unwrap();
        assert!(dry_parsed.dry_run);
        assert_eq!(dry_parsed.created_routes, vec!["route-a".to_string()]);

        let mut ir_req: ApplyIrRequest = serde_json::from_slice(&ir_body).unwrap();
        ir_req.dry_run = false;
        let ir_resp = signed_req(
            &app,
            "POST",
            "/api/projects/smoke/epochs/1/orch/ir",
            "orch-1",
            &sk_orch,
            serde_json::to_vec(&ir_req).unwrap(),
        )
        .await;
        assert_eq!(ir_resp.status(), StatusCode::OK);

        // Cursor-based incremental event stream check
        let ev_resp = app
            .clone()
            .oneshot(
                Request::get("/api/projects/smoke/epochs/1/events?since_seq=1&limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ev_resp.status(), StatusCode::OK);
        let evs: Vec<EventRecordView> = serde_json::from_slice(
            &axum::body::to_bytes(ev_resp.into_body(), 1024 * 1024).await.unwrap(),
        )
        .unwrap();
        assert!(evs.iter().all(|e| e.event_seq > 1));

        // Run verifier on DefOverlay
        let accepted_dir = tempfile::tempdir().unwrap();
        let vcfg = VerifierConfig {
            runner_mode: RunnerMode::Local,
            spec_file: Some(root_dir.join("Spec.lean")),
            accepted_olean_dir: accepted_dir.path().to_path_buf(),
            timeout_secs: 180,
            ..Default::default()
        };
        assert!(process_one_job(&store, &blobs, &vcfg).await.unwrap());

        // Worker claims lemma_a, submits, and verifier runs
        let claim_resp = signed_req(
            &app,
            "POST",
            "/api/projects/smoke/leases/claim",
            "w-1",
            &sk_w,
            b"{}".to_vec(),
        )
        .await;
        assert_eq!(claim_resp.status(), StatusCode::OK);
        let pkt_a: TaskPacket = serde_json::from_slice(
            &axum::body::to_bytes(claim_resp.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(pkt_a.task_id, "lemma_a");

        let la_code = std::fs::read_to_string(root_dir.join("samples/lemma_a.lean")).unwrap();
        let sub_a_body = serde_json::to_vec(&SubmitRequest {
            epoch_id: 1,
            task_id: "lemma_a".into(),
            lease_id: pkt_a.lease.unwrap().lease_id,
            kind: SubmissionKind::Proof,
            spec_sha256: "spec-sha".into(),
            code_utf8: la_code,
            idea_attr_id: None,
        })
        .unwrap();
        let sub_a_resp = signed_req(
            &app,
            "POST",
            "/api/projects/smoke/submissions",
            "w-1",
            &sk_w,
            sub_a_body,
        )
        .await;
        assert_eq!(sub_a_resp.status(), StatusCode::ACCEPTED);
        assert!(process_one_job(&store, &blobs, &vcfg).await.unwrap());

        // Worker claims unlocked merge_a, submits proof, closes project!
        let claim_m = signed_req(
            &app,
            "POST",
            "/api/projects/smoke/leases/claim",
            "w-1",
            &sk_w,
            b"{}".to_vec(),
        )
        .await;
        assert_eq!(claim_m.status(), StatusCode::OK);
        let pkt_m: TaskPacket = serde_json::from_slice(
            &axum::body::to_bytes(claim_m.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
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
        let sub_m_body = serde_json::to_vec(&SubmitRequest {
            epoch_id: 1,
            task_id: "merge_a".into(),
            lease_id: pkt_m.lease.unwrap().lease_id,
            kind: SubmissionKind::Proof,
            spec_sha256: "spec-sha".into(),
            code_utf8: merge_code,
            idea_attr_id: None,
        })
        .unwrap();
        let sub_m_resp = signed_req(
            &app,
            "POST",
            "/api/projects/smoke/submissions",
            "w-1",
            &sk_w,
            sub_m_body,
        )
        .await;
        assert_eq!(sub_m_resp.status(), StatusCode::ACCEPTED);
        assert!(process_one_job(&store, &blobs, &vcfg).await.unwrap());

        // Public graph confirms ProjectStatus::Proved and RouteState::Won
        let g_resp = app
            .clone()
            .oneshot(
                Request::get("/api/projects/smoke/epochs/1/graph")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(g_resp.status(), StatusCode::OK);
        let g: ProjectGraphView = serde_json::from_slice(
            &axum::body::to_bytes(g_resp.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(g.project_status, ProjectStatus::Proved);
        assert_eq!(g.routes[0].state, RouteState::Won);

        // Portal assets & /api/status respond cleanly
        let idx = app.clone().oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(idx.status(), StatusCode::OK);
        let ops = app.clone().oneshot(Request::get("/ops").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(ops.status(), StatusCode::OK);
        let css = app.clone().oneshot(Request::get("/assets/styles.css").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(css.status(), StatusCode::OK);
        let js = app.clone().oneshot(Request::get("/assets/app.js").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(js.status(), StatusCode::OK);
        let st_resp = app.clone().oneshot(Request::get("/api/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(st_resp.status(), StatusCode::OK);
        let st_json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(st_resp.into_body(), 64 * 1024).await.unwrap(),
        )
        .unwrap();
        assert_eq!(st_json["open_worker_join"], false);
        assert_eq!(st_json["max_active_leases_per_actor"], 1);
        assert!(st_json["registered_agents"].as_u64().unwrap() >= 1);
        assert!(st_json["online_agents"].is_u64());
        assert!(st_json["active_leases"].is_u64());

        let inst_resp = app
            .clone()
            .oneshot(Request::get("/install.sh").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(inst_resp.status(), StatusCode::OK);
        let inst_text = String::from_utf8(
            axum::body::to_bytes(inst_resp.into_body(), 64 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(inst_text.starts_with("#!/usr/bin/env bash"));

        let skills_idx = app
            .clone()
            .oneshot(Request::get("/skills").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(skills_idx.status(), StatusCode::OK);
        let skills_json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(skills_idx.into_body(), 64 * 1024).await.unwrap(),
        )
        .unwrap();
        assert_eq!(skills_json["install_script"], "/install.sh");

        for skill_path in [
            "/skills/quod-worker/SKILL.md",
            "/skills/worker/SKILL.md",
            "/skills/quod-worker/references/cli-reference.md",
        ] {
            let sk_resp = app
                .clone()
                .oneshot(Request::get(skill_path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(sk_resp.status(), StatusCode::OK);
        }
        let missing_sk = app
            .clone()
            .oneshot(Request::get("/skills/nonexistent/SKILL.md").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(missing_sk.status(), StatusCode::NOT_FOUND);

        for join_path in ["/JOIN.md", "/JOIN.txt"] {
            let jr = app
                .clone()
                .oneshot(Request::get(join_path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(jr.status(), StatusCode::OK);
            let txt = String::from_utf8(
                axum::body::to_bytes(jr.into_body(), 128 * 1024)
                    .await
                    .unwrap()
                    .to_vec(),
            )
            .unwrap();
            assert!(txt.contains("QUOD_ACTOR_ID"));
        }

        let lb_resp = app
            .clone()
            .oneshot(
                Request::get("/api/leaderboard?project=smoke&limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(lb_resp.status(), StatusCode::OK);
        let lb_rows: Vec<LeaderboardEntryView> = serde_json::from_slice(
            &axum::body::to_bytes(lb_resp.into_body(), 64 * 1024).await.unwrap(),
        )
        .unwrap();
        assert!(!lb_rows.is_empty());
        assert_eq!(lb_rows[0].actor_id, "w-1");
        assert!(lb_rows[0].credit_points > 0);
    }
}
