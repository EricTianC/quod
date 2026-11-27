use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use chrono::{Duration, Utc};
use quod_core::algo::{
    check_coupling, detect_stalls, mine_reusable_overlays, score_task_for_actor, voi_rank,
};
use quod_core::{
    ActorAuthContext, ActorProfile, AlgoQueryResult, ApplyIrRequest, ApplyIrResponse,
    AuditEpochRequest, ClaimLeaseRequest, CreateEpochRequest, CreateInviteRequest,
    CreateProjectRequest, DispatchPolicyConfig, EpochEvent, EpochState, EpochStatusView,
    EventRecordView, GraphIrOp, GraveyardEntryView, HeartbeatResponse, InviteTokenView,
    JoinActorRequest, LeaderboardEntryView, LeaseInfo, ProjectGraphView, ProjectStatus,
    ProjectSummaryView, RegisterActorRequest, RegisterBundleRequest, Role, RouteState, RouteView,
    SubmissionKind, SubmissionState, SubmissionStatusView, SubmitResponse, TaskEdgeView,
    TaskEvent, TaskNodeView, TaskPacket, TaskRole, TaskState, TaskTelemetryView, ToolchainBundle,
    VerifiedOverlayView, WakeupDigestView, WhoAmIResponse, compute_default_bounty,
    validate_pubkey_hex,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    ActorCredentials, BlobStore, HubLiveMetrics, NewSubmissionInput, QuodStore, StoreError,
    StoreFuture, VerificationJob, VerificationOutcomeInput, VerificationOutcomeResult,
    overlay_module_name, validate_actor_id,
};

/// PostgreSQL 16 control-plane & verifier store using runtime `sqlx::query(...)` only.
#[derive(Debug, Clone)]
pub struct PgEventStore {
    pool: PgPool,
}

impl PgEventStore {
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn run_migrations(&self) -> Result<(), StoreError> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    async fn append_event(
        tx: &mut Transaction<'_, Postgres>,
        project_id: &str,
        epoch_id: i32,
        aggregate_type: &str,
        aggregate_id: &str,
        event_type: &str,
        actor_id: &str,
        payload: serde_json::Value,
    ) -> Result<(), StoreError> {
        let event_id = Uuid::now_v7();
        let idem_key = format!("{project_id}:{epoch_id}:{aggregate_type}:{aggregate_id}:{event_id}");
        sqlx::query(
            r#"
            INSERT INTO events (event_id, project_id, epoch_id, aggregate_type, aggregate_id, event_type, idempotency_key, actor_id, payload)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (idempotency_key) DO NOTHING
            "#,
        )
        .bind(event_id)
        .bind(project_id)
        .bind(epoch_id)
        .bind(aggregate_type)
        .bind(aggregate_id)
        .bind(event_type)
        .bind(idem_key)
        .bind(actor_id)
        .bind(payload)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

fn parse_enum<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, StoreError> {
    Ok(serde_json::from_value(serde_json::Value::String(
        raw.to_string(),
    ))?)
}

fn encode_enum<T: serde::Serialize>(val: &T) -> Result<String, StoreError> {
    let v = serde_json::to_value(val)?;
    Ok(v.as_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string()))
}

fn row_to_actor_profile(r: &sqlx::postgres::PgRow) -> Result<ActorProfile, StoreError> {
    let roles: Vec<Role> = serde_json::from_value(r.get("roles"))?;
    let project_scopes: Vec<String> = serde_json::from_value(r.get("project_scopes"))?;
    let skill_tags: Vec<String> = serde_json::from_value(r.get("skill_tags"))?;
    let avg_compile_ms: Option<i64> = r.get("avg_compile_ms");
    Ok(ActorProfile {
        actor_id: r.get("actor_id"),
        display_name: r.get("display_name"),
        git_email: r.get("git_email"),
        credit_points: r.get::<i64, _>("credit_points"),
        roles,
        project_scopes,
        lineage_cluster: r.get("lineage_cluster"),
        tier: r.get::<i16, _>("tier").clamp(1, 4) as u8,
        skill_tags,
        enabled: r.get("enabled"),
        ema_accept_rate: r.get("ema_accept_rate"),
        total_probes: r.get::<i64, _>("total_probes").max(0) as u64,
        probe_passes: r.get::<i64, _>("probe_passes").max(0) as u64,
        total_submissions: r.get::<i64, _>("total_submissions").max(0) as u64,
        accepted_submissions: r.get::<i64, _>("accepted_submissions").max(0) as u64,
        rejected_submissions: r.get::<i64, _>("rejected_submissions").max(0) as u64,
        avg_compile_ms: avg_compile_ms.map(|v| v.max(0) as u64),
    })
}

fn row_to_task_node(r: &sqlx::postgres::PgRow) -> Result<TaskNodeView, StoreError> {
    let preferred_lineages: Vec<String> = serde_json::from_value(r.get("preferred_lineages"))?;
    let curated_imports: Vec<String> = serde_json::from_value(r.get("curated_imports"))?;
    let failed_lineages: Vec<String> = serde_json::from_value(r.get("failed_lineages"))?;
    let last_compile_ms: Option<i64> = r.get("last_compile_ms");
    Ok(TaskNodeView {
        task_id: r.get("task_id"),
        project_id: r.get("project_id"),
        epoch_id: r.get::<i32, _>("epoch_id") as u32,
        route_id: r.get("route_id"),
        task_role: parse_enum(&r.get::<String, _>("task_role"))?,
        state: parse_enum(&r.get::<String, _>("state"))?,
        priority: r.get("priority"),
        racer_cap_k: r.get::<i16, _>("racer_cap_k").max(1) as u8,
        lease_ttl_secs: r.get("lease_ttl_secs"),
        min_tier: r.get::<i16, _>("min_tier").clamp(1, 4) as u8,
        credit_bounty: r.get::<i32, _>("credit_bounty").max(0) as u32,
        preferred_lineages,
        prior_p: r.get("prior_p"),
        expected_cost_c: r.get("expected_cost_c"),
        spec_decl_name: r.get("spec_decl_name"),
        spec_statement: r.get("spec_statement"),
        target_inline: r.get("target_inline"),
        candidate_decl: r.get("candidate_decl"),
        curated_imports,
        module_hint: r.get("module_hint"),
        telemetry: TaskTelemetryView {
            total_attempts: r.get::<i64, _>("total_attempts").max(0) as u64,
            failed_attempts: r.get::<i64, _>("failed_attempts").max(0) as u64,
            timeout_count: r.get::<i64, _>("timeout_count").max(0) as u64,
            failed_lineages,
            last_compile_ms: last_compile_ms.map(|v| v.max(0) as u64),
        },
    })
}

async fn has_depends_on_cycle_pg(
    tx: &mut Transaction<'_, Postgres>,
    project_id: &str,
    epoch_id: i32,
    from: &str,
    to: &str,
) -> Result<bool, StoreError> {
    if from == to {
        return Ok(true);
    }
    let rows = sqlx::query(
        "SELECT from_task_id, to_task_id FROM task_edges WHERE project_id = $1 AND epoch_id = $2 AND edge_kind = 'depends_on'",
    )
    .bind(project_id)
    .bind(epoch_id)
    .fetch_all(&mut **tx)
    .await?;

    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for r in rows {
        adj.entry(r.get("from_task_id"))
            .or_default()
            .push(r.get("to_task_id"));
    }
    let mut q = VecDeque::new();
    let mut seen = HashSet::new();
    q.push_back(to.to_string());
    seen.insert(to.to_string());
    while let Some(cur) = q.pop_front() {
        if cur == from {
            return Ok(true);
        }
        if let Some(nexts) = adj.get(&cur) {
            for nxt in nexts {
                if seen.insert(nxt.clone()) {
                    q.push_back(nxt.clone());
                }
            }
        }
    }
    Ok(false)
}

async fn all_deps_proved_pg(
    tx: &mut Transaction<'_, Postgres>,
    project_id: &str,
    epoch_id: i32,
    task_id: &str,
) -> Result<bool, StoreError> {
    let row = sqlx::query(
        r#"
        SELECT COUNT(*) AS unproved_cnt
        FROM task_edges e
        JOIN tasks t
          ON t.project_id = e.project_id
         AND t.epoch_id = e.epoch_id
         AND t.task_id = e.from_task_id
        WHERE e.project_id = $1
          AND e.epoch_id = $2
          AND e.to_task_id = $3
          AND e.edge_kind = 'depends_on'
          AND t.state <> 'PROVED'
        "#,
    )
    .bind(project_id)
    .bind(epoch_id)
    .bind(task_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.get::<i64, _>("unproved_cnt") == 0)
}

impl QuodStore for PgEventStore {
    fn register_actor(&self, req: &RegisterActorRequest) -> StoreFuture<'_, ActorProfile> {
        let req = req.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let roles = if req.roles.is_empty() {
                vec![Role::Worker]
            } else {
                req.roles.clone()
            };
            let tier = req.initial_tier.unwrap_or(1).clamp(1, 4) as i16;
            let row = sqlx::query(
                r#"
                INSERT INTO actors (actor_id, pubkey_ed25519, roles, project_scopes, lineage_cluster, tier, skill_tags, display_name, git_email, enabled)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, TRUE)
                ON CONFLICT (actor_id) DO UPDATE SET
                    pubkey_ed25519 = EXCLUDED.pubkey_ed25519,
                    roles = EXCLUDED.roles,
                    project_scopes = EXCLUDED.project_scopes,
                    lineage_cluster = EXCLUDED.lineage_cluster,
                    tier = EXCLUDED.tier,
                    skill_tags = EXCLUDED.skill_tags,
                    display_name = COALESCE(EXCLUDED.display_name, actors.display_name),
                    git_email = COALESCE(EXCLUDED.git_email, actors.git_email)
                RETURNING *
                "#,
            )
            .bind(&req.actor_id)
            .bind(&req.pubkey_ed25519)
            .bind(serde_json::to_value(&roles)?)
            .bind(serde_json::to_value(&req.project_scopes)?)
            .bind(&req.lineage_cluster)
            .bind(tier)
            .bind(serde_json::to_value(&req.skill_tags)?)
            .bind(&req.display_name)
            .bind(&req.git_email)
            .fetch_one(&mut *tx)
            .await?;

            Self::append_event(
                &mut tx,
                "_global",
                0,
                "actor",
                &req.actor_id,
                "ActorRegistered",
                "admin",
                serde_json::json!({ "roles": roles, "lineage_cluster": req.lineage_cluster }),
            )
            .await?;
            tx.commit().await?;
            row_to_actor_profile(&row)
        })
    }

    fn get_actor(&self, actor_id: &str) -> StoreFuture<'_, Option<ActorProfile>> {
        let actor_id = actor_id.to_string();
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM actors WHERE actor_id = $1")
                .bind(&actor_id)
                .fetch_optional(&self.pool)
                .await?;
            row.as_ref().map(row_to_actor_profile).transpose()
        })
    }

    fn get_actor_auth(&self, actor_id: &str) -> StoreFuture<'_, Option<ActorCredentials>> {
        let actor_id = actor_id.to_string();
        Box::pin(async move {
            let row = sqlx::query(
                "SELECT actor_id, pubkey_ed25519, roles, project_scopes, enabled FROM actors WHERE actor_id = $1",
            )
            .bind(&actor_id)
            .fetch_optional(&self.pool)
            .await?;
            let Some(r) = row else {
                return Ok(None);
            };
            Ok(Some(ActorCredentials {
                pubkey_ed25519: r.get("pubkey_ed25519"),
                auth: ActorAuthContext {
                    actor_id: r.get("actor_id"),
                    roles: serde_json::from_value(r.get("roles"))?,
                    project_scopes: serde_json::from_value(r.get("project_scopes"))?,
                    enabled: r.get("enabled"),
                },
            }))
        })
    }

    fn whoami(&self, actor_id: &str) -> StoreFuture<'_, Option<WhoAmIResponse>> {
        let actor_id = actor_id.to_string();
        Box::pin(async move {
            let Some(profile) = self.get_actor(&actor_id).await? else {
                return Ok(None);
            };
            let auth = ActorAuthContext {
                actor_id: profile.actor_id.clone(),
                roles: profile.roles.clone(),
                project_scopes: profile.project_scopes.clone(),
                enabled: profile.enabled,
            };
            let projs = self.list_projects().await?;
            let active_projects = projs
                .into_iter()
                .filter(|p| p.status == ProjectStatus::Active && auth.can_access_project(&p.project_id))
                .map(|p| p.project_id)
                .collect();
            Ok(Some(WhoAmIResponse {
                actor_id: profile.actor_id,
                display_name: profile.display_name,
                git_email: profile.git_email,
                credit_points: profile.credit_points,
                roles: profile.roles,
                project_scopes: profile.project_scopes,
                lineage_cluster: profile.lineage_cluster,
                tier: profile.tier,
                enabled: profile.enabled,
                active_projects,
            }))
        })
    }

    fn create_invite(
        &self,
        created_by: &str,
        req: &CreateInviteRequest,
    ) -> StoreFuture<'_, InviteTokenView> {
        let created_by = created_by.to_string();
        let req = req.clone();
        Box::pin(async move {
            if req.roles.is_empty() {
                return Err(StoreError::BadRequest(
                    "invite must grant at least one role".into(),
                ));
            }
            if req.max_uses == 0 {
                return Err(StoreError::BadRequest("max_uses must be >= 1".into()));
            }
            if req.expires_in_secs < 60 {
                return Err(StoreError::BadRequest(
                    "expires_in_secs must be >= 60".into(),
                ));
            }
            let invite_code = format!("quod_inv_{}", Uuid::now_v7().simple());
            let initial_tier = req.initial_tier.unwrap_or(1).clamp(1, 4);
            let expires_at = Utc::now() + Duration::seconds(req.expires_in_secs);

            let mut tx = self.pool.begin().await?;
            sqlx::query(
                r#"
                INSERT INTO invite_tokens (invite_code, roles, project_scopes, max_uses, used_count, initial_tier, created_by, note, expires_at)
                VALUES ($1, $2, $3, $4, 0, $5, $6, $7, $8)
                "#,
            )
            .bind(&invite_code)
            .bind(serde_json::to_value(&req.roles)?)
            .bind(serde_json::to_value(&req.project_scopes)?)
            .bind(req.max_uses as i32)
            .bind(initial_tier as i16)
            .bind(&created_by)
            .bind(&req.note)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;

            let scope_proj = req
                .project_scopes
                .first()
                .cloned()
                .unwrap_or_else(|| "_global".into());
            Self::append_event(
                &mut tx,
                &scope_proj,
                0,
                "invite",
                &invite_code,
                "InviteCreated",
                &created_by,
                serde_json::json!({
                    "roles": req.roles,
                    "project_scopes": req.project_scopes,
                    "max_uses": req.max_uses,
                    "initial_tier": initial_tier,
                }),
            )
            .await?;
            tx.commit().await?;

            Ok(InviteTokenView {
                invite_code,
                roles: req.roles,
                project_scopes: req.project_scopes,
                max_uses: req.max_uses,
                used_count: 0,
                initial_tier,
                created_by,
                note: req.note,
                expires_at,
            })
        })
    }

    fn join_actor(
        &self,
        req: &JoinActorRequest,
        open_worker_join: bool,
    ) -> StoreFuture<'_, ActorProfile> {
        let req = req.clone();
        Box::pin(async move {
            validate_actor_id(&req.actor_id)?;
            validate_pubkey_hex(&req.pubkey_ed25519)
                .map_err(|e| StoreError::BadRequest(e.to_string()))?;
            if req.lineage_cluster.trim().is_empty() {
                return Err(StoreError::BadRequest(
                    "lineage_cluster cannot be empty".into(),
                ));
            }

            let mut tx = self.pool.begin().await?;
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT actor_id FROM actors WHERE actor_id = $1 OR pubkey_ed25519 = $2 LIMIT 1",
            )
            .bind(&req.actor_id)
            .bind(&req.pubkey_ed25519)
            .fetch_optional(&mut *tx)
            .await?;
            if existing.is_some() {
                return Err(StoreError::Conflict(
                    "actor_id or pubkey_ed25519 is already registered".into(),
                ));
            }

            let (roles, project_scopes, tier, via) = match req.invite_code.as_deref() {
                Some(code) => {
                    let row = sqlx::query(
                        "SELECT roles, project_scopes, max_uses, used_count, initial_tier, expires_at FROM invite_tokens WHERE invite_code = $1 FOR UPDATE",
                    )
                    .bind(code)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| StoreError::Forbidden("invalid invite_code".into()))?;
                    let expires_at: chrono::DateTime<Utc> = row.get("expires_at");
                    if Utc::now() >= expires_at {
                        return Err(StoreError::Forbidden("invite_code has expired".into()));
                    }
                    let max_uses: i32 = row.get("max_uses");
                    let used_count: i32 = row.get("used_count");
                    if used_count >= max_uses {
                        return Err(StoreError::Conflict(
                            "invite_code has reached its maximum uses".into(),
                        ));
                    }
                    let roles: Vec<Role> = serde_json::from_value(row.get("roles"))?;
                    let project_scopes: Vec<String> =
                        serde_json::from_value(row.get("project_scopes"))?;
                    let initial_tier: i16 = row.get("initial_tier");
                    sqlx::query(
                        "UPDATE invite_tokens SET used_count = used_count + 1 WHERE invite_code = $1",
                    )
                    .bind(code)
                    .execute(&mut *tx)
                    .await?;
                    (
                        roles,
                        project_scopes,
                        initial_tier.clamp(1, 4) as u8,
                        format!("invite:{code}"),
                    )
                }
                None => {
                    if !open_worker_join {
                        return Err(StoreError::Forbidden(
                            "invite code required: open worker self-registration is disabled"
                                .into(),
                        ));
                    }
                    (
                        vec![Role::Worker],
                        vec![],
                        1u8,
                        "open_worker_join".to_string(),
                    )
                }
            };

            let row = sqlx::query(
                r#"
                INSERT INTO actors (actor_id, pubkey_ed25519, roles, project_scopes, lineage_cluster, tier, skill_tags, display_name, git_email, enabled)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, TRUE)
                RETURNING *
                "#,
            )
            .bind(&req.actor_id)
            .bind(&req.pubkey_ed25519)
            .bind(serde_json::to_value(&roles)?)
            .bind(serde_json::to_value(&project_scopes)?)
            .bind(&req.lineage_cluster)
            .bind(tier as i16)
            .bind(serde_json::to_value(&req.skill_tags)?)
            .bind(&req.display_name)
            .bind(&req.git_email)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| match &e {
                sqlx::Error::Database(db) if db.is_unique_violation() => {
                    StoreError::Conflict("actor_id or pubkey_ed25519 is already registered".into())
                }
                _ => StoreError::Database(e),
            })?;

            Self::append_event(
                &mut tx,
                "_global",
                0,
                "actor",
                &req.actor_id,
                "ActorJoined",
                &req.actor_id,
                serde_json::json!({
                    "roles": roles,
                    "project_scopes": project_scopes,
                    "tier": tier,
                    "via": via,
                }),
            )
            .await?;
            tx.commit().await?;
            row_to_actor_profile(&row)
        })
    }

    fn register_bundle(&self, req: &RegisterBundleRequest) -> StoreFuture<'_, ToolchainBundle> {
        let req = req.clone();
        Box::pin(async move {
            sqlx::query(
                r#"
                INSERT INTO toolchain_bundles (bundle_id, lean_toolchain, mathlib_rev, toolchain_dir, lean_paths, bundle_sha256)
                VALUES ($1, $2, $3, $4, $5, $6)
                ON CONFLICT (bundle_id) DO UPDATE SET
                    lean_toolchain = EXCLUDED.lean_toolchain,
                    mathlib_rev = EXCLUDED.mathlib_rev,
                    toolchain_dir = EXCLUDED.toolchain_dir,
                    lean_paths = EXCLUDED.lean_paths,
                    bundle_sha256 = EXCLUDED.bundle_sha256
                "#,
            )
            .bind(&req.bundle_id)
            .bind(&req.lean_toolchain)
            .bind(&req.mathlib_rev)
            .bind(&req.toolchain_dir)
            .bind(serde_json::to_value(&req.lean_paths)?)
            .bind(&req.bundle_sha256)
            .execute(&self.pool)
            .await?;

            Ok(ToolchainBundle {
                bundle_id: req.bundle_id,
                lean_toolchain: req.lean_toolchain,
                mathlib_rev: req.mathlib_rev,
                toolchain_dir: req.toolchain_dir,
                lean_paths: req.lean_paths,
                bundle_sha256: req.bundle_sha256,
            })
        })
    }

    fn get_bundle(&self, bundle_id: &str) -> StoreFuture<'_, Option<ToolchainBundle>> {
        let bundle_id = bundle_id.to_string();
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM toolchain_bundles WHERE bundle_id = $1")
                .bind(&bundle_id)
                .fetch_optional(&self.pool)
                .await?;
            let Some(r) = row else {
                return Ok(None);
            };
            Ok(Some(ToolchainBundle {
                bundle_id: r.get("bundle_id"),
                lean_toolchain: r.get("lean_toolchain"),
                mathlib_rev: r.get("mathlib_rev"),
                toolchain_dir: r.get("toolchain_dir"),
                lean_paths: serde_json::from_value(r.get("lean_paths"))?,
                bundle_sha256: r.get("bundle_sha256"),
            }))
        })
    }

    fn create_project(
        &self,
        actor_id: &str,
        req: &CreateProjectRequest,
    ) -> StoreFuture<'_, ProjectSummaryView> {
        let actor_id = actor_id.to_string();
        let req = req.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let row = sqlx::query(
                r#"
                INSERT INTO projects (project_id, title, description, spec_namespace, status, default_bundle_id, created_by)
                VALUES ($1, $2, $3, $4, 'ACTIVE', $5, $6)
                RETURNING *
                "#,
            )
            .bind(&req.project_id)
            .bind(&req.title)
            .bind(&req.description)
            .bind(&req.spec_namespace)
            .bind(&req.default_bundle_id)
            .bind(&actor_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| match &e {
                sqlx::Error::Database(db) if db.is_unique_violation() => {
                    StoreError::Conflict(format!("project {} already exists", req.project_id))
                }
                _ => StoreError::Database(e),
            })?;

            Self::append_event(
                &mut tx,
                &req.project_id,
                0,
                "project",
                &req.project_id,
                "ProjectCreated",
                &actor_id,
                serde_json::json!({ "title": req.title }),
            )
            .await?;
            tx.commit().await?;

            Ok(ProjectSummaryView {
                project_id: row.get("project_id"),
                title: row.get("title"),
                description: row.get("description"),
                spec_namespace: row.get("spec_namespace"),
                status: parse_enum(&row.get::<String, _>("status"))?,
                active_epoch_id: None,
                created_by: row.get("created_by"),
                created_at: row.get("created_at"),
            })
        })
    }

    fn list_projects(&self) -> StoreFuture<'_, Vec<ProjectSummaryView>> {
        Box::pin(async move {
            let rows = sqlx::query(
                r#"
                SELECT p.*,
                       (SELECT e.epoch_id FROM epochs e WHERE e.project_id = p.project_id AND e.state = 'ACTIVE' LIMIT 1) AS active_epoch_id
                FROM projects p
                ORDER BY p.project_id
                "#,
            )
            .fetch_all(&self.pool)
            .await?;
            let mut out = Vec::with_capacity(rows.len());
            for r in rows {
                let active: Option<i32> = r.get("active_epoch_id");
                out.push(ProjectSummaryView {
                    project_id: r.get("project_id"),
                    title: r.get("title"),
                    description: r.get("description"),
                    spec_namespace: r.get("spec_namespace"),
                    status: parse_enum(&r.get::<String, _>("status"))?,
                    active_epoch_id: active.map(|v| v as u32),
                    created_by: r.get("created_by"),
                    created_at: r.get("created_at"),
                });
            }
            Ok(out)
        })
    }

    fn create_epoch(
        &self,
        project_id: &str,
        actor_id: &str,
        req: &CreateEpochRequest,
    ) -> StoreFuture<'_, EpochStatusView> {
        let project_id = project_id.to_string();
        let actor_id = actor_id.to_string();
        let req = req.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let proj_exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE project_id = $1)")
                    .bind(&project_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if !proj_exists {
                return Err(StoreError::NotFound(format!("project {project_id} not found")));
            }
            let bundle_exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM toolchain_bundles WHERE bundle_id = $1)",
            )
            .bind(&req.bundle_id)
            .fetch_one(&mut *tx)
            .await?;
            if !bundle_exists {
                return Err(StoreError::NotFound(format!(
                    "toolchain bundle {} not found",
                    req.bundle_id
                )));
            }

            let state = if req.auto_activate {
                EpochState::Active
            } else {
                EpochState::PendingAudit
            };
            let state_str = encode_enum(&state)?;
            sqlx::query(
                r#"
                INSERT INTO epochs (
                    project_id, epoch_id, state, bundle_id, base_commit, spec_module,
                    spec_decl_name, spec_statement, spec_sha256, root_task_id,
                    dispatch_policy, created_by, audited_by
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                "#,
            )
            .bind(&project_id)
            .bind(req.epoch_id as i32)
            .bind(&state_str)
            .bind(&req.bundle_id)
            .bind(&req.base_commit)
            .bind(&req.spec_module)
            .bind(&req.spec_decl_name)
            .bind(&req.spec_statement)
            .bind(&req.spec_sha256)
            .bind(&req.root_task_id)
            .bind(serde_json::to_value(DispatchPolicyConfig::default())?)
            .bind(&actor_id)
            .bind(if req.auto_activate { Some(actor_id.clone()) } else { None })
            .execute(&mut *tx)
            .await
            .map_err(|e| match &e {
                sqlx::Error::Database(db) if db.is_unique_violation() => {
                    StoreError::Conflict("epoch already exists or project already has an ACTIVE epoch".into())
                }
                _ => StoreError::Database(e),
            })?;

            if req.spec_code.is_some() {
                sqlx::query(
                    r#"
                    INSERT INTO overlays (project_id, epoch_id, module_name, overlay_kind, source_task_id, source_actor_id, blob_sha256, imports, compile_ms)
                    VALUES ($1, $2, $3, 'spec', NULL, $4, $5, '[]'::jsonb, 0)
                    ON CONFLICT DO NOTHING
                    "#,
                )
                .bind(&project_id)
                .bind(req.epoch_id as i32)
                .bind(&req.spec_module)
                .bind(&actor_id)
                .bind(&req.spec_sha256)
                .execute(&mut *tx)
                .await?;
            }

            let imports = serde_json::to_value(vec![req.spec_module.clone()])?;
            let root_bounty = compute_default_bounty(2.0, 1.0, 0) as i32;
            sqlx::query(
                r#"
                INSERT INTO tasks (
                    project_id, epoch_id, task_id, route_id, task_role, state,
                    priority, racer_cap_k, min_tier, credit_bounty, preferred_lineages, prior_p,
                    expected_cost_c, spec_decl_name, spec_statement, target_inline,
                    candidate_decl, curated_imports, module_hint
                )
                VALUES ($1, $2, $3, NULL, 'root', 'OPEN', 1.0, 2, 1, $8, '[]'::jsonb, 0.5, 2.0, $4, $5, FALSE, 'candidate', $6, $7)
                "#,
            )
            .bind(&project_id)
            .bind(req.epoch_id as i32)
            .bind(&req.root_task_id)
            .bind(&req.spec_decl_name)
            .bind(&req.spec_statement)
            .bind(imports)
            .bind(&req.spec_module)
            .bind(root_bounty)
            .execute(&mut *tx)
            .await?;

            Self::append_event(
                &mut tx,
                &project_id,
                req.epoch_id as i32,
                "epoch",
                &req.epoch_id.to_string(),
                if req.auto_activate {
                    "EpochActivated"
                } else {
                    "EpochProposed"
                },
                &actor_id,
                serde_json::json!({ "root_task_id": req.root_task_id }),
            )
            .await?;
            tx.commit().await?;

            self.epoch_status(&project_id, req.epoch_id)
                .await?
                .ok_or_else(|| StoreError::NotFound("created epoch missing".into()))
        })
    }

    fn audit_epoch(
        &self,
        project_id: &str,
        epoch_id: u32,
        auditor_id: &str,
        req: &AuditEpochRequest,
    ) -> StoreFuture<'_, EpochStatusView> {
        let project_id = project_id.to_string();
        let auditor_id = auditor_id.to_string();
        let req = req.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let row = sqlx::query(
                "SELECT state, created_by FROM epochs WHERE project_id = $1 AND epoch_id = $2 FOR UPDATE",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                StoreError::NotFound(format!("epoch ({project_id}, {epoch_id}) not found"))
            })?;
            let created_by: String = row.get("created_by");
            if created_by == auditor_id {
                return Err(StoreError::Forbidden(format!(
                    "separation of duties: epoch proposer '{created_by}' cannot audit their own epoch"
                )));
            }
            let current: EpochState = parse_enum(&row.get::<String, _>("state"))?;
            let next = current.transition(if req.approve {
                EpochEvent::ApproveAudit
            } else {
                EpochEvent::RejectAudit
            })?;
            let next_str = encode_enum(&next)?;
            sqlx::query(
                "UPDATE epochs SET state = $1, audited_by = $2 WHERE project_id = $3 AND epoch_id = $4",
            )
            .bind(&next_str)
            .bind(&auditor_id)
            .bind(&project_id)
            .bind(epoch_id as i32)
            .execute(&mut *tx)
            .await
            .map_err(|e| match &e {
                sqlx::Error::Database(db) if db.is_unique_violation() => {
                    StoreError::Conflict(format!("project {project_id} already has an ACTIVE epoch"))
                }
                _ => StoreError::Database(e),
            })?;

            Self::append_event(
                &mut tx,
                &project_id,
                epoch_id as i32,
                "epoch",
                &epoch_id.to_string(),
                if req.approve {
                    "EpochAuditApproved"
                } else {
                    "EpochAuditRejected"
                },
                &auditor_id,
                serde_json::json!({ "comment": req.comment }),
            )
            .await?;
            tx.commit().await?;

            self.epoch_status(&project_id, epoch_id)
                .await?
                .ok_or_else(|| StoreError::NotFound("epoch missing".into()))
        })
    }

    fn freeze_epoch(
        &self,
        project_id: &str,
        epoch_id: u32,
        actor_id: &str,
    ) -> StoreFuture<'_, EpochStatusView> {
        let project_id = project_id.to_string();
        let actor_id = actor_id.to_string();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let row = sqlx::query(
                "SELECT state FROM epochs WHERE project_id = $1 AND epoch_id = $2 FOR UPDATE",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                StoreError::NotFound(format!("epoch ({project_id}, {epoch_id}) not found"))
            })?;
            let current: EpochState = parse_enum(&row.get::<String, _>("state"))?;
            let next = current.transition(EpochEvent::BeginFreeze)?;
            sqlx::query("UPDATE epochs SET state = $1 WHERE project_id = $2 AND epoch_id = $3")
                .bind(encode_enum(&next)?)
                .bind(&project_id)
                .bind(epoch_id as i32)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE tasks SET state = 'STALE_EPOCH', updated_at = now() WHERE project_id = $1 AND epoch_id = $2 AND state NOT IN ('PROVED','REFUTED_HARD','PRUNED','STALE_EPOCH')",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE leases SET status = 'STALE_EPOCH' WHERE project_id = $1 AND epoch_id = $2 AND status = 'ACTIVE'",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .execute(&mut *tx)
            .await?;

            Self::append_event(
                &mut tx,
                &project_id,
                epoch_id as i32,
                "epoch",
                &epoch_id.to_string(),
                "EpochFrozen",
                &actor_id,
                serde_json::json!({}),
            )
            .await?;
            tx.commit().await?;

            self.epoch_status(&project_id, epoch_id)
                .await?
                .ok_or_else(|| StoreError::NotFound("epoch missing".into()))
        })
    }

    fn apply_ir(
        &self,
        project_id: &str,
        epoch_id: u32,
        actor_id: &str,
        req: &ApplyIrRequest,
        blobs: &dyn BlobStore,
    ) -> StoreFuture<'_, ApplyIrResponse> {
        let project_id = project_id.to_string();
        let actor_id = actor_id.to_string();
        let req = req.clone();
        let mut pre_stored_blobs = Vec::new();
        for op in &req.ops {
            if let GraphIrOp::PublishDefOverlay { code_utf8, .. } = op {
                if req.dry_run {
                    pre_stored_blobs.push("dry-run".to_string());
                } else {
                    match blobs.put_bytes(code_utf8.as_bytes()) {
                        Ok(sha) => pre_stored_blobs.push(sha),
                        Err(e) => return Box::pin(async move { Err(e) }),
                    }
                }
            }
        }

        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let ep_row = sqlx::query(
                "SELECT state, bundle_id, spec_module, spec_decl_name, dispatch_policy FROM epochs WHERE project_id = $1 AND epoch_id = $2 FOR UPDATE",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                StoreError::NotFound(format!("epoch ({project_id}, {epoch_id}) not found"))
            })?;

            let ep_state: EpochState = parse_enum(&ep_row.get::<String, _>("state"))?;
            if ep_state != EpochState::Active {
                return Err(StoreError::Conflict(format!(
                    "epoch ({project_id}, {epoch_id}) is {ep_state:?}, not ACTIVE"
                )));
            }
            let bundle_id: String = ep_row.get("bundle_id");
            let spec_module: String = ep_row.get("spec_module");
            let spec_decl_name_ep: String = ep_row.get("spec_decl_name");
            let mut dispatch_policy: DispatchPolicyConfig =
                serde_json::from_value(ep_row.get("dispatch_policy"))?;

            let mut created_routes = Vec::new();
            let mut spawned_tasks = Vec::new();
            let mut unlocked_tasks = Vec::new();
            let mut overlay_job_ids = Vec::new();
            let mut blob_iter = pre_stored_blobs.into_iter();

            for op in &req.ops {
                match op {
                    GraphIrOp::CreateRoute {
                        route_id,
                        title,
                        rationale,
                    } => {
                        sqlx::query(
                            "INSERT INTO routes (project_id, epoch_id, route_id, title, rationale, state, created_by) VALUES ($1, $2, $3, $4, $5, 'ACTIVE', $6)",
                        )
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(route_id)
                        .bind(title)
                        .bind(rationale)
                        .bind(&actor_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| match &e {
                            sqlx::Error::Database(db) if db.is_unique_violation() => {
                                StoreError::Conflict(format!("route {route_id} already exists"))
                            }
                            _ => StoreError::Database(e),
                        })?;
                        created_routes.push(route_id.clone());
                        Self::append_event(
                            &mut tx,
                            &project_id,
                            epoch_id as i32,
                            "route",
                            route_id,
                            "RouteCreated",
                            &actor_id,
                            serde_json::json!({ "title": title }),
                        )
                        .await?;
                    }
                    GraphIrOp::PruneRoute { route_id, reason } => {
                        let res = sqlx::query(
                            "UPDATE routes SET state = 'PRUNED' WHERE project_id = $1 AND epoch_id = $2 AND route_id = $3",
                        )
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(route_id)
                        .execute(&mut *tx)
                        .await?;
                        if res.rows_affected() == 0 {
                            return Err(StoreError::NotFound(format!("route {route_id} not found")));
                        }
                        sqlx::query(
                            "UPDATE tasks SET state = 'PRUNED', updated_at = now() WHERE project_id = $1 AND epoch_id = $2 AND route_id = $3 AND state NOT IN ('PROVED','REFUTED_HARD','PRUNED','STALE_EPOCH')",
                        )
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(route_id)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query(
                            "UPDATE leases SET status = 'PRUNED' WHERE project_id = $1 AND epoch_id = $2 AND status = 'ACTIVE' AND task_id IN (SELECT task_id FROM tasks WHERE project_id = $1 AND epoch_id = $2 AND route_id = $3)",
                        )
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(route_id)
                        .execute(&mut *tx)
                        .await?;
                        Self::append_event(
                            &mut tx,
                            &project_id,
                            epoch_id as i32,
                            "route",
                            route_id,
                            "RoutePruned",
                            &actor_id,
                            serde_json::json!({ "reason": reason }),
                        )
                        .await?;
                    }
                    GraphIrOp::SpawnTask {
                        task_id,
                        route_id,
                        task_role,
                        spec_decl_name,
                        spec_statement,
                        target_inline,
                        candidate_decl,
                        curated_imports,
                        depends_on,
                        refutes_task_id,
                        module_hint,
                        priority,
                        prior_p,
                        expected_cost_c,
                        racer_cap_k,
                        min_tier,
                        preferred_lineages,
                        credit_bounty,
                    } => {
                        if let Some(rid) = route_id {
                            let r_row = sqlx::query(
                                "SELECT state FROM routes WHERE project_id = $1 AND epoch_id = $2 AND route_id = $3",
                            )
                            .bind(&project_id)
                            .bind(epoch_id as i32)
                            .bind(rid)
                            .fetch_optional(&mut *tx)
                            .await?
                            .ok_or_else(|| StoreError::NotFound(format!("route {rid} not found")))?;
                            let rs: RouteState = parse_enum(&r_row.get::<String, _>("state"))?;
                            if rs != RouteState::Active {
                                return Err(StoreError::Conflict(format!(
                                    "route {rid} is {rs:?}, cannot spawn task"
                                )));
                            }
                        }

                        let mut imports = curated_imports.clone();
                        if !*target_inline && !imports.contains(&spec_module) {
                            imports.insert(0, spec_module.clone());
                        }
                        let mut all_deps_proved = true;
                        for dep in depends_on {
                            let dep_row = sqlx::query(
                                "SELECT state FROM tasks WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3",
                            )
                            .bind(&project_id)
                            .bind(epoch_id as i32)
                            .bind(dep)
                            .fetch_optional(&mut *tx)
                            .await?
                            .ok_or_else(|| {
                                StoreError::NotFound(format!("dependency task {dep} not found"))
                            })?;
                            let ds: TaskState = parse_enum(&dep_row.get::<String, _>("state"))?;
                            if ds != TaskState::Proved {
                                all_deps_proved = false;
                            } else {
                                let ov_rows = sqlx::query(
                                    "SELECT module_name FROM overlays WHERE project_id = $1 AND epoch_id = $2 AND source_task_id = $3",
                                )
                                .bind(&project_id)
                                .bind(epoch_id as i32)
                                .bind(dep)
                                .fetch_all(&mut *tx)
                                .await?;
                                for ov in ov_rows {
                                    let m: String = ov.get("module_name");
                                    if !imports.contains(&m) {
                                        imports.push(m);
                                    }
                                }
                            }
                        }
                        if let Some(target_tid) = refutes_task_id {
                            let exists: bool = sqlx::query_scalar(
                                "SELECT EXISTS(SELECT 1 FROM tasks WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3)",
                            )
                            .bind(&project_id)
                            .bind(epoch_id as i32)
                            .bind(target_tid)
                            .fetch_one(&mut *tx)
                            .await?;
                            if !exists {
                                return Err(StoreError::NotFound(format!(
                                    "refutation target {target_tid} not found"
                                )));
                            }
                        }

                        let initial_state = if all_deps_proved {
                            if !depends_on.is_empty() {
                                unlocked_tasks.push(task_id.clone());
                            }
                            TaskState::Open
                        } else {
                            TaskState::Blocked
                        };

                        let resolved_bounty = credit_bounty
                            .unwrap_or_else(|| compute_default_bounty(*expected_cost_c, *priority, 0))
                            as i32;
                        sqlx::query(
                            r#"
                            INSERT INTO tasks (
                                project_id, epoch_id, task_id, route_id, task_role, state,
                                priority, racer_cap_k, min_tier, credit_bounty, preferred_lineages, prior_p,
                                expected_cost_c, spec_decl_name, spec_statement, target_inline,
                                candidate_decl, curated_imports, module_hint
                            )
                            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $19, $10, $11, $12, $13, $14, $15, $16, $17, $18)
                            "#,
                        )
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(task_id)
                        .bind(route_id)
                        .bind(encode_enum(task_role)?)
                        .bind(encode_enum(&initial_state)?)
                        .bind(*priority)
                        .bind((*racer_cap_k).max(1) as i16)
                        .bind((*min_tier).clamp(1, 4) as i16)
                        .bind(serde_json::to_value(preferred_lineages)?)
                        .bind(*prior_p)
                        .bind(*expected_cost_c)
                        .bind(spec_decl_name)
                        .bind(spec_statement)
                        .bind(*target_inline)
                        .bind(candidate_decl)
                        .bind(serde_json::to_value(&imports)?)
                        .bind(module_hint)
                        .bind(resolved_bounty)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| match &e {
                            sqlx::Error::Database(db) if db.is_unique_violation() => {
                                StoreError::Conflict(format!("task {task_id} already exists"))
                            }
                            _ => StoreError::Database(e),
                        })?;

                        for dep in depends_on {
                            sqlx::query(
                                "INSERT INTO task_edges (project_id, epoch_id, from_task_id, to_task_id, edge_kind) VALUES ($1, $2, $3, $4, 'depends_on') ON CONFLICT DO NOTHING",
                            )
                            .bind(&project_id)
                            .bind(epoch_id as i32)
                            .bind(dep)
                            .bind(task_id)
                            .execute(&mut *tx)
                            .await?;
                        }
                        if let Some(target_tid) = refutes_task_id {
                            sqlx::query(
                                "INSERT INTO task_edges (project_id, epoch_id, from_task_id, to_task_id, edge_kind) VALUES ($1, $2, $3, $4, 'refutes') ON CONFLICT DO NOTHING",
                            )
                            .bind(&project_id)
                            .bind(epoch_id as i32)
                            .bind(task_id)
                            .bind(target_tid)
                            .execute(&mut *tx)
                            .await?;
                        }
                        spawned_tasks.push(task_id.clone());
                        Self::append_event(
                            &mut tx,
                            &project_id,
                            epoch_id as i32,
                            "task",
                            task_id,
                            "TaskSpawned",
                            &actor_id,
                            serde_json::json!({ "role": task_role, "state": initial_state }),
                        )
                        .await?;
                    }
                    GraphIrOp::LinkDependency {
                        upstream_task_id,
                        downstream_task_id,
                    } => {
                        let up_row = sqlx::query(
                            "SELECT state FROM tasks WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3",
                        )
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(upstream_task_id)
                        .fetch_optional(&mut *tx)
                        .await?
                        .ok_or_else(|| {
                            StoreError::NotFound(format!("task {upstream_task_id} not found"))
                        })?;
                        let down_row = sqlx::query(
                            "SELECT state, curated_imports FROM tasks WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 FOR UPDATE",
                        )
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(downstream_task_id)
                        .fetch_optional(&mut *tx)
                        .await?
                        .ok_or_else(|| {
                            StoreError::NotFound(format!("task {downstream_task_id} not found"))
                        })?;
                        if has_depends_on_cycle_pg(
                            &mut tx,
                            &project_id,
                            epoch_id as i32,
                            upstream_task_id,
                            downstream_task_id,
                        )
                        .await?
                        {
                            return Err(StoreError::Conflict(format!(
                                "dependency cycle detected linking {upstream_task_id} -> {downstream_task_id}"
                            )));
                        }
                        sqlx::query(
                            "INSERT INTO task_edges (project_id, epoch_id, from_task_id, to_task_id, edge_kind) VALUES ($1, $2, $3, $4, 'depends_on') ON CONFLICT DO NOTHING",
                        )
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(upstream_task_id)
                        .bind(downstream_task_id)
                        .execute(&mut *tx)
                        .await?;

                        let up_state: TaskState = parse_enum(&up_row.get::<String, _>("state"))?;
                        let mut down_state: TaskState =
                            parse_enum(&down_row.get::<String, _>("state"))?;
                        if up_state != TaskState::Proved && down_state == TaskState::Open {
                            down_state = TaskState::Blocked;
                        }
                        let mut imports: Vec<String> =
                            serde_json::from_value(down_row.get("curated_imports"))?;
                        let ov_rows = sqlx::query(
                            "SELECT module_name FROM overlays WHERE project_id = $1 AND epoch_id = $2 AND source_task_id = $3",
                        )
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(upstream_task_id)
                        .fetch_all(&mut *tx)
                        .await?;
                        for ov in ov_rows {
                            let m: String = ov.get("module_name");
                            if !imports.contains(&m) {
                                imports.push(m);
                            }
                        }
                        sqlx::query(
                            "UPDATE tasks SET state = $1, curated_imports = $2, updated_at = now() WHERE project_id = $3 AND epoch_id = $4 AND task_id = $5",
                        )
                        .bind(encode_enum(&down_state)?)
                        .bind(serde_json::to_value(&imports)?)
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(downstream_task_id)
                        .execute(&mut *tx)
                        .await?;
                        Self::append_event(
                            &mut tx,
                            &project_id,
                            epoch_id as i32,
                            "task",
                            downstream_task_id,
                            "DependencyLinked",
                            &actor_id,
                            serde_json::json!({ "upstream_task_id": upstream_task_id }),
                        )
                        .await?;
                    }
                    GraphIrOp::PublishDefOverlay {
                        module_name,
                        code_utf8: _,
                    } => {
                        let blob_sha256 = blob_iter.next().ok_or_else(|| {
                            StoreError::BadRequest("missing pre-stored def overlay blob".into())
                        })?;
                        let sub_id = Uuid::now_v7();
                        let job_id = Uuid::now_v7();
                        let mod_name = overlay_module_name(
                            SubmissionKind::DefOverlay,
                            module_name,
                            sub_id,
                        );
                        let sub_sha = format!("{sub_id}");
                        sqlx::query(
                            r#"
                            INSERT INTO submissions (submission_id, submission_sha256, project_id, epoch_id, task_id, actor_id, kind, state, blob_sha256, received_at_ns)
                            VALUES ($1, $2, $3, $4, '_def_overlay', $5, 'def_overlay', 'STAGE0_SPEC_OK', $6, 0)
                            "#,
                        )
                        .bind(sub_id)
                        .bind(sub_sha)
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(&actor_id)
                        .bind(&blob_sha256)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query(
                            r#"
                            INSERT INTO verification_jobs (job_id, submission_id, project_id, epoch_id, task_id, actor_id, kind, bundle_id, spec_module, spec_decl, spec_statement, target_inline, candidate_module, candidate_decl, blob_sha256, curated_imports)
                            VALUES ($1, $2, $3, $4, '_def_overlay', $5, 'def_overlay', $6, $7, $8, '', FALSE, $9, '', $10, '[]'::jsonb)
                            "#,
                        )
                        .bind(job_id)
                        .bind(sub_id)
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(&actor_id)
                        .bind(&bundle_id)
                        .bind(&spec_module)
                        .bind(&spec_decl_name_ep)
                        .bind(&mod_name)
                        .bind(&blob_sha256)
                        .execute(&mut *tx)
                        .await?;
                        overlay_job_ids.push(job_id);
                        Self::append_event(
                            &mut tx,
                            &project_id,
                            epoch_id as i32,
                            "overlay",
                            &mod_name,
                            "DefOverlayQueued",
                            &actor_id,
                            serde_json::json!({ "job_id": job_id }),
                        )
                        .await?;
                    }
                    GraphIrOp::SetDispatchPolicy { config } => {
                        dispatch_policy = config.clone();
                        sqlx::query(
                            "UPDATE epochs SET dispatch_policy = $1 WHERE project_id = $2 AND epoch_id = $3",
                        )
                        .bind(serde_json::to_value(config)?)
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .execute(&mut *tx)
                        .await?;
                        Self::append_event(
                            &mut tx,
                            &project_id,
                            epoch_id as i32,
                            "epoch",
                            &epoch_id.to_string(),
                            "DispatchPolicyUpdated",
                            &actor_id,
                            serde_json::to_value(config)?,
                        )
                        .await?;
                    }
                    GraphIrOp::PatchNodeBudget {
                        task_id,
                        priority,
                        racer_cap_k,
                        lease_ttl_secs,
                        min_tier,
                        preferred_lineages,
                        expected_cost_c,
                        prior_p,
                        credit_bounty,
                    } => {
                        let t_row = sqlx::query(
                            "SELECT * FROM tasks WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 FOR UPDATE",
                        )
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(task_id)
                        .fetch_optional(&mut *tx)
                        .await?
                        .ok_or_else(|| StoreError::NotFound(format!("task {task_id} not found")))?;
                        let mut task = row_to_task_node(&t_row)?;
                        if let Some(v) = priority {
                            task.priority = *v;
                        }
                        if let Some(v) = racer_cap_k {
                            task.racer_cap_k = (*v).max(1);
                        }
                        if let Some(v) = lease_ttl_secs {
                            task.lease_ttl_secs = Some((*v).max(30));
                        }
                        if let Some(v) = min_tier {
                            task.min_tier = (*v).clamp(1, 4);
                        }
                        if let Some(v) = preferred_lineages {
                            task.preferred_lineages = v.clone();
                        }
                        if let Some(v) = expected_cost_c {
                            task.expected_cost_c = *v;
                        }
                        if let Some(v) = prior_p {
                            task.prior_p = *v;
                        }
                        if let Some(v) = credit_bounty {
                            task.credit_bounty = *v;
                        }
                        sqlx::query(
                            r#"
                            UPDATE tasks
                            SET priority = $1, racer_cap_k = $2, lease_ttl_secs = $3, min_tier = $4,
                                preferred_lineages = $5, expected_cost_c = $6, prior_p = $7, credit_bounty = $8, updated_at = now()
                            WHERE project_id = $9 AND epoch_id = $10 AND task_id = $11
                            "#,
                        )
                        .bind(task.priority)
                        .bind(task.racer_cap_k as i16)
                        .bind(task.lease_ttl_secs)
                        .bind(task.min_tier as i16)
                        .bind(serde_json::to_value(&task.preferred_lineages)?)
                        .bind(task.expected_cost_c)
                        .bind(task.prior_p)
                        .bind(task.credit_bounty as i32)
                        .bind(&project_id)
                        .bind(epoch_id as i32)
                        .bind(task_id)
                        .execute(&mut *tx)
                        .await?;
                        Self::append_event(
                            &mut tx,
                            &project_id,
                            epoch_id as i32,
                            "task",
                            task_id,
                            "NodeBudgetPatched",
                            &actor_id,
                            serde_json::json!({ "priority": task.priority, "racer_cap_k": task.racer_cap_k, "min_tier": task.min_tier }),
                        )
                        .await?;
                    }
                    GraphIrOp::PatchActorHint {
                        actor_id: target_actor,
                        tier,
                        skill_tags,
                        enabled,
                    } => {
                        let a_row = sqlx::query("SELECT * FROM actors WHERE actor_id = $1 FOR UPDATE")
                            .bind(target_actor)
                            .fetch_optional(&mut *tx)
                            .await?
                            .ok_or_else(|| {
                                StoreError::NotFound(format!("actor {target_actor} not found"))
                            })?;
                        let mut prof = row_to_actor_profile(&a_row)?;
                        if let Some(t) = tier {
                            prof.tier = (*t).clamp(1, 4);
                        }
                        if let Some(tags) = skill_tags {
                            prof.skill_tags = tags.clone();
                        }
                        sqlx::query(
                            "UPDATE actors SET tier = $1, skill_tags = $2 WHERE actor_id = $3",
                        )
                        .bind(prof.tier as i16)
                        .bind(serde_json::to_value(&prof.skill_tags)?)
                        .bind(target_actor)
                        .execute(&mut *tx)
                        .await?;
                        if let Some(en) = enabled {
                            if *en {
                                sqlx::query(
                                    "DELETE FROM project_actor_blocks WHERE project_id = $1 AND actor_id = $2",
                                )
                                .bind(&project_id)
                                .bind(target_actor)
                                .execute(&mut *tx)
                                .await?;
                            } else {
                                sqlx::query(
                                    "INSERT INTO project_actor_blocks (project_id, actor_id, blocked_by, reason) VALUES ($1, $2, $3, 'orchestrator_hint') ON CONFLICT (project_id, actor_id) DO UPDATE SET blocked_by = EXCLUDED.blocked_by",
                                )
                                .bind(&project_id)
                                .bind(target_actor)
                                .bind(&actor_id)
                                .execute(&mut *tx)
                                .await?;
                            }
                        }
                        let is_blocked: Option<String> = sqlx::query_scalar(
                            "SELECT actor_id FROM project_actor_blocks WHERE project_id = $1 AND actor_id = $2",
                        )
                        .bind(&project_id)
                        .bind(target_actor)
                        .fetch_optional(&mut *tx)
                        .await?;
                        let project_enabled = is_blocked.is_none();
                        Self::append_event(
                            &mut tx,
                            &project_id,
                            epoch_id as i32,
                            "actor",
                            target_actor,
                            "ActorHintPatched",
                            &actor_id,
                            serde_json::json!({ "tier": prof.tier, "project_enabled": project_enabled }),
                        )
                        .await?;
                    }
                }
            }

            if req.dry_run {
                tx.rollback().await?;
            } else {
                tx.commit().await?;
            }
            Ok(ApplyIrResponse {
                applied_ops: req.ops.len(),
                dry_run: req.dry_run,
                created_routes,
                spawned_tasks,
                unlocked_tasks,
                overlay_job_ids,
                dispatch_policy,
            })
        })
    }

    fn claim_lease(
        &self,
        project_id: &str,
        actor_id: &str,
        req: &ClaimLeaseRequest,
        default_ttl_secs: i64,
        max_active_leases_per_actor: usize,
    ) -> StoreFuture<'_, Option<TaskPacket>> {
        let project_id = project_id.to_string();
        let actor_id = actor_id.to_string();
        let req = req.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let now = Utc::now();
            let a_row = sqlx::query("SELECT * FROM actors WHERE actor_id = $1")
                .bind(&actor_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| StoreError::NotFound(format!("actor {actor_id} not registered")))?;
            let actor = row_to_actor_profile(&a_row)?;
            if !actor.enabled {
                return Err(StoreError::Forbidden(format!("actor {actor_id} is disabled")));
            }
            let blocked: Option<String> = sqlx::query_scalar(
                "SELECT actor_id FROM project_actor_blocks WHERE project_id = $1 AND actor_id = $2",
            )
            .bind(&project_id)
            .bind(&actor_id)
            .fetch_optional(&mut *tx)
            .await?;
            if blocked.is_some() {
                return Err(StoreError::Forbidden(format!(
                    "actor {actor_id} is blocked on project {project_id}"
                )));
            }

            let ep_row = sqlx::query(
                "SELECT epoch_id, bundle_id, spec_module, spec_sha256, dispatch_policy FROM epochs WHERE project_id = $1 AND state = 'ACTIVE' LIMIT 1",
            )
            .bind(&project_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(ep_row) = ep_row else {
                return Ok(None);
            };
            let epoch_id: i32 = ep_row.get("epoch_id");
            let bundle_id: String = ep_row.get("bundle_id");
            let spec_module: String = ep_row.get("spec_module");
            let spec_sha256: String = ep_row.get("spec_sha256");
            let dispatch_policy: DispatchPolicyConfig =
                serde_json::from_value(ep_row.get("dispatch_policy"))?;

            let expired_rows = sqlx::query(
                "UPDATE leases SET status = 'EXPIRED' WHERE project_id = $1 AND epoch_id = $2 AND status = 'ACTIVE' AND expires_at < $3 RETURNING task_id",
            )
            .bind(&project_id)
            .bind(epoch_id)
            .bind(now)
            .fetch_all(&mut *tx)
            .await?;

            for er in expired_rows {
                let tid: String = er.get("task_id");
                sqlx::query(
                    "UPDATE tasks SET timeout_count = timeout_count + 1, updated_at = now() WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3",
                )
                .bind(&project_id)
                .bind(epoch_id)
                .bind(&tid)
                .execute(&mut *tx)
                .await?;
                let active_cnt: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM leases WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 AND status = 'ACTIVE'",
                )
                .bind(&project_id)
                .bind(epoch_id)
                .bind(&tid)
                .fetch_one(&mut *tx)
                .await?;
                if active_cnt == 0 {
                    sqlx::query(
                        "UPDATE tasks SET state = 'STALLED_SOFT', updated_at = now() WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 AND state = 'RACING'",
                    )
                    .bind(&project_id)
                    .bind(epoch_id)
                    .bind(&tid)
                    .execute(&mut *tx)
                    .await?;
                }
            }

            let active_for_actor: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM leases WHERE project_id = $1 AND epoch_id = $2 AND actor_id = $3 AND status = 'ACTIVE'",
            )
            .bind(&project_id)
            .bind(epoch_id)
            .bind(&actor_id)
            .fetch_one(&mut *tx)
            .await?;
            if active_for_actor >= max_active_leases_per_actor.max(1) as i64 {
                return Err(StoreError::Conflict(format!(
                    "actor {actor_id} already holds the maximum number of active leases ({active_for_actor}) in project {project_id}"
                )));
            }

            let task_rows = sqlx::query(
                "SELECT * FROM tasks WHERE project_id = $1 AND epoch_id = $2 AND state IN ('OPEN', 'RACING', 'STALLED_SOFT')",
            )
            .bind(&project_id)
            .bind(epoch_id)
            .fetch_all(&mut *tx)
            .await?;

            let mut best: Option<(TaskNodeView, f64, f64)> = None;
            for tr in task_rows {
                let task = row_to_task_node(&tr)?;
                if actor.tier < task.min_tier {
                    continue;
                }
                if let Some(pref) = &req.preferred_task_id
                    && &task.task_id != pref
                {
                    continue;
                }
                if let Some(pref_route) = &req.preferred_route_id
                    && task.route_id.as_deref() != Some(pref_route.as_str())
                {
                    continue;
                }
                let active_clusters: Vec<String> = sqlx::query_scalar(
                    "SELECT lineage_cluster FROM leases WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 AND status = 'ACTIVE'",
                )
                .bind(&project_id)
                .bind(epoch_id)
                .bind(&task.task_id)
                .fetch_all(&mut *tx)
                .await?;
                if active_clusters.len() >= task.racer_cap_k as usize {
                    continue;
                }
                let decision = score_task_for_actor(
                    &dispatch_policy,
                    &actor,
                    &task,
                    &active_clusters,
                    false,
                );
                let replace = best
                    .as_ref()
                    .is_none_or(|(_, s, _)| decision.score > *s);
                if replace {
                    best = Some((task, decision.score, decision.propensity_score));
                }
            }

            let Some((chosen, _, propensity)) = best else {
                tx.commit().await?;
                return Ok(None);
            };

            let ttl = chosen.lease_ttl_secs.unwrap_or(default_ttl_secs).max(30);
            let lease_id = Uuid::now_v7();
            let expires_at = now + Duration::seconds(ttl);
            sqlx::query(
                r#"
                INSERT INTO leases (lease_id, project_id, epoch_id, task_id, actor_id, lineage_cluster, propensity_score, issued_at, expires_at, status)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'ACTIVE')
                "#,
            )
            .bind(lease_id)
            .bind(&project_id)
            .bind(epoch_id)
            .bind(&chosen.task_id)
            .bind(&actor_id)
            .bind(&actor.lineage_cluster)
            .bind(propensity)
            .bind(now)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;

            let next_state = chosen.state.transition(TaskEvent::AcquireLease)?;
            sqlx::query(
                "UPDATE tasks SET state = $1, updated_at = now() WHERE project_id = $2 AND epoch_id = $3 AND task_id = $4",
            )
            .bind(encode_enum(&next_state)?)
            .bind(&project_id)
            .bind(epoch_id)
            .bind(&chosen.task_id)
            .execute(&mut *tx)
            .await?;

            let gy_rows = sqlx::query(
                "SELECT failure_kind, summary FROM graveyard_entries WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 ORDER BY entry_seq DESC LIMIT 20",
            )
            .bind(&project_id)
            .bind(epoch_id)
            .bind(&chosen.task_id)
            .fetch_all(&mut *tx)
            .await?;
            let graveyard_constraints: Vec<String> = gy_rows
                .into_iter()
                .map(|r| {
                    format!(
                        "[{}] {}",
                        r.get::<String, _>("failure_kind"),
                        r.get::<String, _>("summary")
                    )
                })
                .collect();

            let mut import_lines = String::new();
            for imp in &chosen.curated_imports {
                import_lines.push_str(&format!("import {imp}\n"));
            }
            if import_lines.is_empty() {
                import_lines.push_str(&format!("import {spec_module}\n"));
            }
            let decl_ty = if chosen.task_role == TaskRole::Refutation {
                if chosen.target_inline && !chosen.spec_statement.is_empty() {
                    format!("¬ ({})", chosen.spec_statement)
                } else {
                    format!("¬ {}", chosen.spec_decl_name)
                }
            } else if chosen.target_inline && !chosen.spec_statement.is_empty() {
                chosen.spec_statement.clone()
            } else {
                chosen.spec_decl_name.clone()
            };
            let slice = format!(
                "{import_lines}-- Project: {project_id} | Task: {} ({:?})\ntheorem {} : {decl_ty} := by\n  sorry\n",
                chosen.task_id, chosen.task_role, chosen.candidate_decl
            );

            Self::append_event(
                &mut tx,
                &project_id,
                epoch_id,
                "lease",
                &lease_id.to_string(),
                "LeaseClaimed",
                &actor_id,
                serde_json::json!({ "task_id": chosen.task_id, "propensity": propensity }),
            )
            .await?;
            tx.commit().await?;

            Ok(Some(TaskPacket {
                schema_version: 2,
                project_id,
                epoch_id: epoch_id as u32,
                task_id: chosen.task_id.clone(),
                route_id: chosen.route_id.clone(),
                task_role: chosen.task_role,
                module_hint: chosen.module_hint.clone(),
                credit_bounty: chosen.credit_bounty,
                bundle_id,
                spec_module,
                spec_sha256,
                target_file: format!("Tasks/{}.lean", chosen.task_id),
                transitive_slice: slice,
                curated_imports: chosen.curated_imports,
                graveyard_constraints,
                resource_limits: Default::default(),
                lease: Some(LeaseInfo {
                    lease_id,
                    actor_id,
                    issued_at: now,
                    expires_at,
                    propensity_score: propensity,
                }),
            }))
        })
    }

    fn heartbeat_lease(
        &self,
        lease_id: Uuid,
        actor_id: &str,
        extend_secs: i64,
    ) -> StoreFuture<'_, HeartbeatResponse> {
        let actor_id = actor_id.to_string();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let row = sqlx::query(
                "SELECT project_id, epoch_id, actor_id, status FROM leases WHERE lease_id = $1 FOR UPDATE",
            )
            .bind(lease_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound("lease not found".into()))?;
            let owner: String = row.get("actor_id");
            if owner != actor_id {
                return Err(StoreError::Conflict("lease owned by another actor".into()));
            }
            let status: String = row.get("status");
            if status != "ACTIVE" {
                return Err(StoreError::Conflict(format!("lease is {status}")));
            }
            let expires_at = Utc::now() + Duration::seconds(extend_secs.max(30));
            sqlx::query("UPDATE leases SET expires_at = $1 WHERE lease_id = $2")
                .bind(expires_at)
                .bind(lease_id)
                .execute(&mut *tx)
                .await?;
            let project_id: String = row.get("project_id");
            let epoch_id: i32 = row.get("epoch_id");
            Self::append_event(
                &mut tx,
                &project_id,
                epoch_id,
                "lease",
                &lease_id.to_string(),
                "LeaseHeartbeat",
                &actor_id,
                serde_json::json!({ "expires_at": expires_at }),
            )
            .await?;
            tx.commit().await?;
            Ok(HeartbeatResponse {
                lease_id,
                expires_at,
                status,
            })
        })
    }

    fn create_submission(&self, input: NewSubmissionInput) -> StoreFuture<'_, SubmitResponse> {
        Box::pin(async move {
            if input.req.kind == SubmissionKind::DefOverlay {
                return Err(StoreError::BadRequest(
                    "DefOverlay must be published via Orchestrator/Blueprinter IR".into(),
                ));
            }
            let mut tx = self.pool.begin().await?;
            let blocked: Option<String> = sqlx::query_scalar(
                "SELECT actor_id FROM project_actor_blocks WHERE project_id = $1 AND actor_id = $2",
            )
            .bind(&input.project_id)
            .bind(&input.actor_id)
            .fetch_optional(&mut *tx)
            .await?;
            if blocked.is_some() {
                return Err(StoreError::Forbidden(format!(
                    "actor {} is blocked on project {}",
                    input.actor_id, input.project_id
                )));
            }
            let lease_row = sqlx::query(
                "SELECT project_id, epoch_id, task_id, actor_id, status FROM leases WHERE lease_id = $1",
            )
            .bind(input.req.lease_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound("lease not found".into()))?;
            if lease_row.get::<String, _>("actor_id") != input.actor_id {
                return Err(StoreError::Conflict("lease owned by another actor".into()));
            }
            let l_status: String = lease_row.get("status");
            if l_status != "ACTIVE" {
                return Err(StoreError::Conflict(format!("lease is {l_status}")));
            }
            if lease_row.get::<String, _>("project_id") != input.project_id
                || lease_row.get::<i32, _>("epoch_id") != input.req.epoch_id as i32
                || lease_row.get::<String, _>("task_id") != input.req.task_id
            {
                return Err(StoreError::Conflict(
                    "lease does not match (project_id, epoch_id, task_id)".into(),
                ));
            }

            let ep_row = sqlx::query(
                "SELECT state, bundle_id, spec_module, spec_sha256 FROM epochs WHERE project_id = $1 AND epoch_id = $2",
            )
            .bind(&input.project_id)
            .bind(input.req.epoch_id as i32)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound("epoch not found".into()))?;
            let ep_state: EpochState = parse_enum(&ep_row.get::<String, _>("state"))?;
            let bundle_id: String = ep_row.get("bundle_id");
            let spec_module: String = ep_row.get("spec_module");
            let ep_spec_sha: String = ep_row.get("spec_sha256");

            let t_row = sqlx::query(
                "SELECT * FROM tasks WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3",
            )
            .bind(&input.project_id)
            .bind(input.req.epoch_id as i32)
            .bind(&input.req.task_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound("task not found".into()))?;
            let task = row_to_task_node(&t_row)?;

            let state = if ep_state != EpochState::Active {
                SubmissionState::StaleEpoch
            } else if input.req.spec_sha256 != ep_spec_sha {
                SubmissionState::RejectedSpecDrift
            } else if input.stage0_passed {
                SubmissionState::Stage0SpecOk
            } else if input.stage0_exit_code == 11 {
                SubmissionState::RejectedSpecDrift
            } else {
                SubmissionState::RejectedSecurity
            };

            let reject_detail = if state == SubmissionState::Stage0SpecOk {
                None
            } else {
                Some(serde_json::json!({ "diagnostics": input.stage0_diagnostics }))
            };
            let sub_sha = format!("{}", input.submission_id);

            sqlx::query(
                r#"
                INSERT INTO submissions (submission_id, submission_sha256, project_id, epoch_id, task_id, actor_id, kind, state, blob_sha256, received_at_ns, idea_attr_id, reject_detail)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                "#,
            )
            .bind(input.submission_id)
            .bind(sub_sha)
            .bind(&input.project_id)
            .bind(input.req.epoch_id as i32)
            .bind(&input.req.task_id)
            .bind(&input.actor_id)
            .bind(encode_enum(&input.req.kind)?)
            .bind(encode_enum(&state)?)
            .bind(&input.blob_sha256)
            .bind(input.received_at_ns)
            .bind(&input.req.idea_attr_id)
            .bind(&reject_detail)
            .execute(&mut *tx)
            .await?;

            if state == SubmissionState::Stage0SpecOk {
                let job_id = Uuid::now_v7();
                let cand_mod = overlay_module_name(
                    input.req.kind,
                    &input.req.task_id,
                    input.submission_id,
                );
                sqlx::query(
                    r#"
                    INSERT INTO verification_jobs (job_id, submission_id, project_id, epoch_id, task_id, actor_id, kind, bundle_id, spec_module, spec_decl, spec_statement, target_inline, candidate_module, candidate_decl, blob_sha256, curated_imports)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
                    "#,
                )
                .bind(job_id)
                .bind(input.submission_id)
                .bind(&input.project_id)
                .bind(input.req.epoch_id as i32)
                .bind(&input.req.task_id)
                .bind(&input.actor_id)
                .bind(encode_enum(&input.req.kind)?)
                .bind(bundle_id)
                .bind(spec_module)
                .bind(&task.spec_decl_name)
                .bind(&task.spec_statement)
                .bind(task.target_inline)
                .bind(cand_mod)
                .bind(&task.candidate_decl)
                .bind(&input.blob_sha256)
                .bind(serde_json::to_value(&task.curated_imports)?)
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO graveyard_entries (project_id, epoch_id, task_id, failure_kind, summary) VALUES ($1, $2, $3, 'stage0_rejected', $4)",
                )
                .bind(&input.project_id)
                .bind(input.req.epoch_id as i32)
                .bind(&input.req.task_id)
                .bind(input.stage0_diagnostics.join("; "))
                .execute(&mut *tx)
                .await?;
            }

            Self::append_event(
                &mut tx,
                &input.project_id,
                input.req.epoch_id as i32,
                "submission",
                &input.submission_id.to_string(),
                "SubmissionReceived",
                &input.actor_id,
                serde_json::json!({ "task_id": input.req.task_id, "state": state }),
            )
            .await?;
            tx.commit().await?;

            Ok(SubmitResponse {
                submission_id: input.submission_id,
                state,
                exit_code: if state == SubmissionState::Stage0SpecOk {
                    0
                } else {
                    input.stage0_exit_code
                },
                diagnostics: input.stage0_diagnostics,
            })
        })
    }

    fn get_submission(
        &self,
        submission_id: Uuid,
    ) -> StoreFuture<'_, Option<SubmissionStatusView>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM submissions WHERE submission_id = $1")
                .bind(submission_id)
                .fetch_optional(&self.pool)
                .await?;
            let Some(r) = row else {
                return Ok(None);
            };
            let compile_ms: Option<i64> = r.get("compile_ms");
            Ok(Some(SubmissionStatusView {
                submission_id: r.get("submission_id"),
                project_id: r.get("project_id"),
                epoch_id: r.get::<i32, _>("epoch_id") as u32,
                task_id: r.get("task_id"),
                actor_id: r.get("actor_id"),
                kind: parse_enum(&r.get::<String, _>("kind"))?,
                state: parse_enum(&r.get::<String, _>("state"))?,
                compile_ms: compile_ms.map(|v| v.max(0) as u64),
                reject_detail: r.get("reject_detail"),
            }))
        })
    }

    fn claim_next_verification_job(
        &self,
        verifier_id: &str,
    ) -> StoreFuture<'_, Option<VerificationJob>> {
        let verifier_id = verifier_id.to_string();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let row = sqlx::query(
                r#"
                WITH picked AS (
                    SELECT job_id
                    FROM verification_jobs
                    WHERE status = 'PENDING'
                    ORDER BY created_at ASC
                    LIMIT 1
                    FOR UPDATE SKIP LOCKED
                )
                UPDATE verification_jobs vj
                SET status = 'RUNNING', claimed_by = $1, claimed_at = now()
                FROM picked
                WHERE vj.job_id = picked.job_id
                RETURNING vj.*
                "#,
            )
            .bind(&verifier_id)
            .fetch_optional(&mut *tx)
            .await?;
            tx.commit().await?;
            let Some(r) = row else {
                return Ok(None);
            };
            Ok(Some(VerificationJob {
                job_id: r.get("job_id"),
                submission_id: r.get("submission_id"),
                project_id: r.get("project_id"),
                epoch_id: r.get::<i32, _>("epoch_id") as u32,
                task_id: r.get("task_id"),
                actor_id: r.get("actor_id"),
                kind: parse_enum(&r.get::<String, _>("kind"))?,
                bundle_id: r.get("bundle_id"),
                spec_module: r.get("spec_module"),
                spec_decl: r.get("spec_decl"),
                spec_statement: r.get("spec_statement"),
                target_inline: r.get("target_inline"),
                candidate_module: r.get("candidate_module"),
                candidate_decl: r.get("candidate_decl"),
                blob_sha256: r.get("blob_sha256"),
                curated_imports: serde_json::from_value(r.get("curated_imports"))?,
            }))
        })
    }

    fn record_verification_outcome(
        &self,
        input: VerificationOutcomeInput,
    ) -> StoreFuture<'_, VerificationOutcomeResult> {
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let job_row = sqlx::query("SELECT * FROM verification_jobs WHERE job_id = $1 FOR UPDATE")
                .bind(input.job_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| StoreError::NotFound("verification job not found".into()))?;
            let project_id: String = job_row.get("project_id");
            let epoch_id: i32 = job_row.get("epoch_id");
            let task_id: String = job_row.get("task_id");
            let actor_id: String = job_row.get("actor_id");
            let kind: SubmissionKind = parse_enum(&job_row.get::<String, _>("kind"))?;
            let candidate_module: String = job_row.get("candidate_module");
            let blob_sha256: String = job_row.get("blob_sha256");
            let curated_imports: serde_json::Value = job_row.get("curated_imports");

            sqlx::query("UPDATE verification_jobs SET status = $1 WHERE job_id = $2")
                .bind(if input.verdict.ok { "DONE" } else { "FAILED" })
                .bind(input.job_id)
                .execute(&mut *tx)
                .await?;

            let actor_row = sqlx::query("SELECT * FROM actors WHERE actor_id = $1 FOR UPDATE")
                .bind(&actor_id)
                .fetch_optional(&mut *tx)
                .await?;
            let mut actor_lineage = "unknown".to_string();
            if let Some(ar) = actor_row {
                let mut prof = row_to_actor_profile(&ar)?;
                actor_lineage = prof.lineage_cluster.clone();
                prof.total_submissions += 1;
                let y = if input.verdict.ok {
                    prof.accepted_submissions += 1;
                    1.0
                } else {
                    prof.rejected_submissions += 1;
                    0.0
                };
                prof.ema_accept_rate = 0.8 * prof.ema_accept_rate + 0.2 * y;
                let new_avg = match prof.avg_compile_ms {
                    Some(prev) => (prev * 4 + input.verdict.compile_ms) / 5,
                    None => input.verdict.compile_ms,
                };
                sqlx::query(
                    r#"
                    UPDATE actors
                    SET total_submissions = $1, accepted_submissions = $2, rejected_submissions = $3,
                        ema_accept_rate = $4, avg_compile_ms = $5
                    WHERE actor_id = $6
                    "#,
                )
                .bind(prof.total_submissions as i64)
                .bind(prof.accepted_submissions as i64)
                .bind(prof.rejected_submissions as i64)
                .bind(prof.ema_accept_rate)
                .bind(new_avg as i64)
                .bind(&actor_id)
                .execute(&mut *tx)
                .await?;
            }

            if kind == SubmissionKind::DefOverlay {
                let sub_state = if input.verdict.ok {
                    SubmissionState::Merged
                } else if input.verdict.exit_code == 21 {
                    SubmissionState::RejectedResourceLimit
                } else {
                    SubmissionState::RejectedKernel
                };
                sqlx::query(
                    "UPDATE submissions SET state = $1, compile_ms = $2, reject_detail = $3 WHERE submission_id = $4",
                )
                .bind(encode_enum(&sub_state)?)
                .bind(input.verdict.compile_ms as i64)
                .bind(if input.verdict.ok {
                    None
                } else {
                    Some(serde_json::json!({ "diagnostics": input.verdict.diagnostics }))
                })
                .bind(input.submission_id)
                .execute(&mut *tx)
                .await?;
                let overlay_module = if input.verdict.ok {
                    sqlx::query(
                        r#"
                        INSERT INTO overlays (project_id, epoch_id, module_name, overlay_kind, source_task_id, source_actor_id, blob_sha256, imports, compile_ms)
                        VALUES ($1, $2, $3, 'def_overlay', NULL, $4, $5, $6, $7)
                        ON CONFLICT (project_id, epoch_id, module_name) DO NOTHING
                        "#,
                    )
                    .bind(&project_id)
                    .bind(epoch_id)
                    .bind(&candidate_module)
                    .bind(&actor_id)
                    .bind(&blob_sha256)
                    .bind(&curated_imports)
                    .bind(input.verdict.compile_ms as i64)
                    .execute(&mut *tx)
                    .await?;
                    Self::append_event(
                        &mut tx,
                        &project_id,
                        epoch_id,
                        "overlay",
                        &candidate_module,
                        "DefOverlayVerified",
                        &actor_id,
                        serde_json::json!({ "compile_ms": input.verdict.compile_ms }),
                    )
                    .await?;
                    Some(candidate_module)
                } else {
                    None
                };
                tx.commit().await?;
                return Ok(VerificationOutcomeResult {
                    submission_state: sub_state,
                    task_state: TaskState::Proved,
                    unlocked_tasks: vec![],
                    overlay_module,
                    project_proved: false,
                });
            }

            let t_row = sqlx::query(
                "SELECT * FROM tasks WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 FOR UPDATE",
            )
            .bind(&project_id)
            .bind(epoch_id)
            .bind(&task_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound("task not found".into()))?;
            let mut task = row_to_task_node(&t_row)?;
            let task_state_before = task.state;

            if !input.verdict.ok {
                let fail_state = match input.verdict.exit_code {
                    21 => SubmissionState::RejectedResourceLimit,
                    31 => SubmissionState::RejectedHygiene,
                    _ => SubmissionState::RejectedKernel,
                };
                sqlx::query(
                    "UPDATE submissions SET state = $1, compile_ms = $2, reject_detail = $3 WHERE submission_id = $4",
                )
                .bind(encode_enum(&fail_state)?)
                .bind(input.verdict.compile_ms as i64)
                .bind(serde_json::json!({ "diagnostics": input.verdict.diagnostics }))
                .bind(input.submission_id)
                .execute(&mut *tx)
                .await?;

                task.telemetry.total_attempts += 1;
                task.telemetry.failed_attempts += 1;
                if input.verdict.exit_code == 21 {
                    task.telemetry.timeout_count += 1;
                }
                if !task.telemetry.failed_lineages.contains(&actor_lineage) {
                    task.telemetry.failed_lineages.push(actor_lineage);
                }
                sqlx::query(
                    r#"
                    UPDATE tasks
                    SET total_attempts = $1, failed_attempts = $2, timeout_count = $3,
                        failed_lineages = $4, last_compile_ms = $5, updated_at = now()
                    WHERE project_id = $6 AND epoch_id = $7 AND task_id = $8
                    "#,
                )
                .bind(task.telemetry.total_attempts as i64)
                .bind(task.telemetry.failed_attempts as i64)
                .bind(task.telemetry.timeout_count as i64)
                .bind(serde_json::to_value(&task.telemetry.failed_lineages)?)
                .bind(input.verdict.compile_ms as i64)
                .bind(&project_id)
                .bind(epoch_id)
                .bind(&task_id)
                .execute(&mut *tx)
                .await?;

                let summary = input
                    .verdict
                    .diagnostics
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ");
                sqlx::query(
                    "INSERT INTO graveyard_entries (project_id, epoch_id, task_id, failure_kind, summary) VALUES ($1, $2, $3, 'kernel_rejected', $4)",
                )
                .bind(&project_id)
                .bind(epoch_id)
                .bind(&task_id)
                .bind(summary)
                .execute(&mut *tx)
                .await?;

                Self::append_event(
                    &mut tx,
                    &project_id,
                    epoch_id,
                    "submission",
                    &input.submission_id.to_string(),
                    "VerificationRejected",
                    &actor_id,
                    serde_json::json!({ "task_id": task_id, "exit_code": input.verdict.exit_code }),
                )
                .await?;
                tx.commit().await?;
                return Ok(VerificationOutcomeResult {
                    submission_state: fail_state,
                    task_state: task_state_before,
                    unlocked_tasks: vec![],
                    overlay_module: None,
                    project_proved: false,
                });
            }

            if task_state_before.is_terminal() {
                sqlx::query(
                    "UPDATE submissions SET state = 'REJECTED_RACE_LOST', compile_ms = $1 WHERE submission_id = $2",
                )
                .bind(input.verdict.compile_ms as i64)
                .bind(input.submission_id)
                .execute(&mut *tx)
                .await?;
                Self::append_event(
                    &mut tx,
                    &project_id,
                    epoch_id,
                    "submission",
                    &input.submission_id.to_string(),
                    "RaceLost",
                    &actor_id,
                    serde_json::json!({ "task_id": task_id }),
                )
                .await?;
                tx.commit().await?;
                return Ok(VerificationOutcomeResult {
                    submission_state: SubmissionState::RejectedRaceLost,
                    task_state: task_state_before,
                    unlocked_tasks: vec![],
                    overlay_module: None,
                    project_proved: false,
                });
            }

            sqlx::query(
                "UPDATE submissions SET state = 'MERGED', compile_ms = $1 WHERE submission_id = $2",
            )
            .bind(input.verdict.compile_ms as i64)
            .bind(input.submission_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE submissions SET state = 'REJECTED_RACE_LOST' WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 AND submission_id <> $4 AND state IN ('SEALED_RECEIVED', 'STAGE0_SPEC_OK', 'STAGE1_KERNEL_OK', 'STAGE2_HYGIENE_OK')",
            )
            .bind(&project_id)
            .bind(epoch_id)
            .bind(&task_id)
            .bind(input.submission_id)
            .execute(&mut *tx)
            .await?;

            let new_task_state = task_state_before.transition(TaskEvent::AcceptProof)?;
            sqlx::query(
                "UPDATE tasks SET state = $1, total_attempts = total_attempts + 1, last_compile_ms = $2, updated_at = now() WHERE project_id = $3 AND epoch_id = $4 AND task_id = $5",
            )
            .bind(encode_enum(&new_task_state)?)
            .bind(input.verdict.compile_ms as i64)
            .bind(&project_id)
            .bind(epoch_id)
            .bind(&task_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE leases SET status = 'COMPLETED' WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 AND status = 'ACTIVE'",
            )
            .bind(&project_id)
            .bind(epoch_id)
            .bind(&task_id)
            .execute(&mut *tx)
            .await?;

            let award_points = task.effective_bounty() as i64;
            if matches!(kind, SubmissionKind::Proof | SubmissionKind::Refutation)
                && award_points > 0
            {
                let reason = match kind {
                    SubmissionKind::Refutation => "refutation_accepted",
                    _ => "proof_accepted",
                };
                sqlx::query(
                    r#"
                    INSERT INTO credit_ledger (entry_id, project_id, epoch_id, task_id, submission_id, actor_id, points, reason)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                    "#,
                )
                .bind(Uuid::now_v7())
                .bind(&project_id)
                .bind(epoch_id)
                .bind(&task_id)
                .bind(input.submission_id)
                .bind(&actor_id)
                .bind(award_points)
                .bind(reason)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE actors SET credit_points = credit_points + $1 WHERE actor_id = $2",
                )
                .bind(award_points)
                .bind(&actor_id)
                .execute(&mut *tx)
                .await?;
                Self::append_event(
                    &mut tx,
                    &project_id,
                    epoch_id,
                    "actor",
                    &actor_id,
                    "CreditAwarded",
                    &actor_id,
                    serde_json::json!({
                        "task_id": task_id,
                        "points": award_points,
                        "reason": reason,
                    }),
                )
                .await?;
            }

            let mut unlocked_tasks = Vec::new();
            let mut overlay_module = None;
            let mut project_proved = false;

            if kind == SubmissionKind::Proof {
                sqlx::query(
                    r#"
                    INSERT INTO overlays (project_id, epoch_id, module_name, overlay_kind, source_task_id, source_actor_id, blob_sha256, imports, compile_ms)
                    VALUES ($1, $2, $3, 'proved_task', $4, $5, $6, $7, $8)
                    ON CONFLICT (project_id, epoch_id, module_name) DO NOTHING
                    "#,
                )
                .bind(&project_id)
                .bind(epoch_id)
                .bind(&candidate_module)
                .bind(&task_id)
                .bind(&actor_id)
                .bind(&blob_sha256)
                .bind(&curated_imports)
                .bind(input.verdict.compile_ms as i64)
                .execute(&mut *tx)
                .await?;
                overlay_module = Some(candidate_module.clone());

                let down_rows = sqlx::query(
                    "SELECT to_task_id FROM task_edges WHERE project_id = $1 AND epoch_id = $2 AND from_task_id = $3 AND edge_kind = 'depends_on'",
                )
                .bind(&project_id)
                .bind(epoch_id)
                .bind(&task_id)
                .fetch_all(&mut *tx)
                .await?;
                for dr in down_rows {
                    let down_id: String = dr.get("to_task_id");
                    let d_row = sqlx::query(
                        "SELECT state, curated_imports FROM tasks WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 FOR UPDATE",
                    )
                    .bind(&project_id)
                    .bind(epoch_id)
                    .bind(&down_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                    if let Some(d_r) = d_row {
                        let mut imports: Vec<String> =
                            serde_json::from_value(d_r.get("curated_imports"))?;
                        if !imports.contains(&candidate_module) {
                            imports.push(candidate_module.clone());
                        }
                        let mut d_state: TaskState = parse_enum(&d_r.get::<String, _>("state"))?;
                        if d_state == TaskState::Blocked
                            && all_deps_proved_pg(&mut tx, &project_id, epoch_id, &down_id).await?
                        {
                            d_state = d_state.transition(TaskEvent::UnblockDependencies)?;
                            unlocked_tasks.push(down_id.clone());
                            Self::append_event(
                                &mut tx,
                                &project_id,
                                epoch_id,
                                "task",
                                &down_id,
                                "TaskUnlocked",
                                "system",
                                serde_json::json!({ "by_upstream": task_id }),
                            )
                            .await?;
                        }
                        sqlx::query(
                            "UPDATE tasks SET state = $1, curated_imports = $2, updated_at = now() WHERE project_id = $3 AND epoch_id = $4 AND task_id = $5",
                        )
                        .bind(encode_enum(&d_state)?)
                        .bind(serde_json::to_value(&imports)?)
                        .bind(&project_id)
                        .bind(epoch_id)
                        .bind(&down_id)
                        .execute(&mut *tx)
                        .await?;
                    }
                }

                let ep_info = sqlx::query(
                    "SELECT root_task_id, spec_decl_name FROM epochs WHERE project_id = $1 AND epoch_id = $2",
                )
                .bind(&project_id)
                .bind(epoch_id)
                .fetch_one(&mut *tx)
                .await?;
                let root_id: String = ep_info.get("root_task_id");
                let ep_decl: String = ep_info.get("spec_decl_name");
                let closes_root = task_id == root_id
                    || (task.task_role == TaskRole::Merge
                        && task.spec_decl_name == ep_decl
                        && !task.target_inline);
                if closes_root {
                    project_proved = true;
                    if task_id != root_id {
                        sqlx::query(
                            "UPDATE tasks SET state = 'PROVED', updated_at = now() WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 AND state NOT IN ('PROVED','REFUTED_HARD','PRUNED','STALE_EPOCH')",
                        )
                        .bind(&project_id)
                        .bind(epoch_id)
                        .bind(&root_id)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query(
                            "UPDATE leases SET status = 'COMPLETED' WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 AND status = 'ACTIVE'",
                        )
                        .bind(&project_id)
                        .bind(epoch_id)
                        .bind(&root_id)
                        .execute(&mut *tx)
                        .await?;
                    }
                    sqlx::query("UPDATE projects SET status = 'PROVED' WHERE project_id = $1")
                        .bind(&project_id)
                        .execute(&mut *tx)
                        .await?;
                    if let Some(won_rid) = &task.route_id {
                        sqlx::query(
                            "UPDATE routes SET state = 'WON' WHERE project_id = $1 AND epoch_id = $2 AND route_id = $3",
                        )
                        .bind(&project_id)
                        .bind(epoch_id)
                        .bind(won_rid)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query(
                            "UPDATE routes SET state = 'PRUNED' WHERE project_id = $1 AND epoch_id = $2 AND route_id <> $3 AND state = 'ACTIVE'",
                        )
                        .bind(&project_id)
                        .bind(epoch_id)
                        .bind(won_rid)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query(
                            "UPDATE tasks SET state = 'PRUNED', updated_at = now() WHERE project_id = $1 AND epoch_id = $2 AND (route_id IS NULL OR route_id <> $3) AND state NOT IN ('PROVED','REFUTED_HARD','PRUNED','STALE_EPOCH')",
                        )
                        .bind(&project_id)
                        .bind(epoch_id)
                        .bind(won_rid)
                        .execute(&mut *tx)
                        .await?;
                    }
                    Self::append_event(
                        &mut tx,
                        &project_id,
                        epoch_id,
                        "project",
                        &project_id,
                        "RootProved",
                        &actor_id,
                        serde_json::json!({ "winning_task": task_id, "winning_route": task.route_id }),
                    )
                    .await?;
                }
            } else if kind == SubmissionKind::Refutation {
                let target_rows = sqlx::query(
                    "SELECT to_task_id FROM task_edges WHERE project_id = $1 AND epoch_id = $2 AND from_task_id = $3 AND edge_kind = 'refutes'",
                )
                .bind(&project_id)
                .bind(epoch_id)
                .bind(&task_id)
                .fetch_all(&mut *tx)
                .await?;
                for tr in target_rows {
                    let target_id: String = tr.get("to_task_id");
                    let target_route: Option<String> = sqlx::query_scalar(
                        "UPDATE tasks SET state = 'REFUTED_HARD', updated_at = now() WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 RETURNING route_id",
                    )
                    .bind(&project_id)
                    .bind(epoch_id)
                    .bind(&target_id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .flatten();
                    sqlx::query(
                        "UPDATE leases SET status = 'REFUTED' WHERE project_id = $1 AND epoch_id = $2 AND task_id = $3 AND status = 'ACTIVE'",
                    )
                    .bind(&project_id)
                    .bind(epoch_id)
                    .bind(&target_id)
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query(
                        "INSERT INTO graveyard_entries (project_id, epoch_id, task_id, failure_kind, summary) VALUES ($1, $2, $3, 'hard_refutation', $4)",
                    )
                    .bind(&project_id)
                    .bind(epoch_id)
                    .bind(&target_id)
                    .bind(format!("Refuted by {task_id} (actor={actor_id})"))
                    .execute(&mut *tx)
                    .await?;
                    if let Some(rid) = target_route {
                        sqlx::query(
                            "UPDATE routes SET state = 'REFUTED' WHERE project_id = $1 AND epoch_id = $2 AND route_id = $3",
                        )
                        .bind(&project_id)
                        .bind(epoch_id)
                        .bind(&rid)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query(
                            "UPDATE tasks SET state = 'PRUNED', updated_at = now() WHERE project_id = $1 AND epoch_id = $2 AND route_id = $3 AND state NOT IN ('PROVED','REFUTED_HARD','PRUNED','STALE_EPOCH')",
                        )
                        .bind(&project_id)
                        .bind(epoch_id)
                        .bind(&rid)
                        .execute(&mut *tx)
                        .await?;
                    }
                    Self::append_event(
                        &mut tx,
                        &project_id,
                        epoch_id,
                        "task",
                        &target_id,
                        "TaskRefuted",
                        &actor_id,
                        serde_json::json!({ "refutation_task": task_id }),
                    )
                    .await?;
                }
            }

            Self::append_event(
                &mut tx,
                &project_id,
                epoch_id,
                "task",
                &task_id,
                "TaskVerified",
                &actor_id,
                serde_json::json!({ "kind": kind, "compile_ms": input.verdict.compile_ms }),
            )
            .await?;
            tx.commit().await?;

            Ok(VerificationOutcomeResult {
                submission_state: SubmissionState::Merged,
                task_state: new_task_state,
                unlocked_tasks,
                overlay_module,
                project_proved,
            })
        })
    }

    fn epoch_status(
        &self,
        project_id: &str,
        epoch_id: u32,
    ) -> StoreFuture<'_, Option<EpochStatusView>> {
        let project_id = project_id.to_string();
        Box::pin(async move {
            let ep = sqlx::query(
                r#"
                SELECT e.*, tb.lean_toolchain
                FROM epochs e
                JOIN toolchain_bundles tb ON tb.bundle_id = e.bundle_id
                WHERE e.project_id = $1 AND e.epoch_id = $2
                "#,
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_optional(&self.pool)
            .await?;
            let Some(ep) = ep else {
                return Ok(None);
            };
            let counts_rows = sqlx::query(
                "SELECT state, COUNT(*) AS cnt FROM tasks WHERE project_id = $1 AND epoch_id = $2 GROUP BY state",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_all(&self.pool)
            .await?;
            let mut task_counts = BTreeMap::new();
            for r in counts_rows {
                task_counts.insert(r.get("state"), r.get::<i64, _>("cnt").max(0) as usize);
            }
            let active_leases: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM leases WHERE project_id = $1 AND epoch_id = $2 AND status = 'ACTIVE'",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_one(&self.pool)
            .await?;
            let pending_verification_jobs: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM verification_jobs WHERE project_id = $1 AND epoch_id = $2 AND status IN ('PENDING','RUNNING')",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_one(&self.pool)
            .await?;
            let overlays_total: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM overlays WHERE project_id = $1 AND epoch_id = $2",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_one(&self.pool)
            .await?;
            let routes_total: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM routes WHERE project_id = $1 AND epoch_id = $2",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_one(&self.pool)
            .await?;

            Ok(Some(EpochStatusView {
                project_id,
                epoch_id,
                state: parse_enum(&ep.get::<String, _>("state"))?,
                bundle_id: ep.get("bundle_id"),
                lean_toolchain: ep.get("lean_toolchain"),
                spec_decl_name: ep.get("spec_decl_name"),
                root_task_id: ep.get("root_task_id"),
                dispatch_policy: serde_json::from_value(ep.get("dispatch_policy"))?,
                task_counts,
                active_leases: active_leases.max(0) as usize,
                pending_verification_jobs: pending_verification_jobs.max(0) as usize,
                overlays_total: overlays_total.max(0) as usize,
                routes_total: routes_total.max(0) as usize,
            }))
        })
    }

    fn project_graph(
        &self,
        project_id: &str,
        epoch_id: u32,
    ) -> StoreFuture<'_, Option<ProjectGraphView>> {
        let project_id = project_id.to_string();
        Box::pin(async move {
            let ep = sqlx::query(
                r#"
                SELECT e.*, p.status AS project_status, tb.lean_toolchain
                FROM epochs e
                JOIN projects p ON p.project_id = e.project_id
                JOIN toolchain_bundles tb ON tb.bundle_id = e.bundle_id
                WHERE e.project_id = $1 AND e.epoch_id = $2
                "#,
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_optional(&self.pool)
            .await?;
            let Some(ep) = ep else {
                return Ok(None);
            };

            let route_rows = sqlx::query(
                "SELECT * FROM routes WHERE project_id = $1 AND epoch_id = $2 ORDER BY route_id",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_all(&self.pool)
            .await?;
            let mut routes = Vec::with_capacity(route_rows.len());
            for r in route_rows {
                routes.push(RouteView {
                    route_id: r.get("route_id"),
                    project_id: r.get("project_id"),
                    epoch_id: r.get::<i32, _>("epoch_id") as u32,
                    title: r.get("title"),
                    rationale: r.get("rationale"),
                    state: parse_enum(&r.get::<String, _>("state"))?,
                    created_by: r.get("created_by"),
                    created_at: r.get("created_at"),
                });
            }

            let task_rows = sqlx::query(
                "SELECT * FROM tasks WHERE project_id = $1 AND epoch_id = $2 ORDER BY task_id",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_all(&self.pool)
            .await?;
            let mut tasks = Vec::with_capacity(task_rows.len());
            for tr in task_rows {
                tasks.push(row_to_task_node(&tr)?);
            }

            let edge_rows = sqlx::query(
                "SELECT from_task_id, to_task_id, edge_kind FROM task_edges WHERE project_id = $1 AND epoch_id = $2",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_all(&self.pool)
            .await?;
            let mut edges = Vec::with_capacity(edge_rows.len());
            for er in edge_rows {
                edges.push(TaskEdgeView {
                    from_task_id: er.get("from_task_id"),
                    to_task_id: er.get("to_task_id"),
                    edge_kind: parse_enum(&er.get::<String, _>("edge_kind"))?,
                });
            }

            let ov_rows = sqlx::query(
                "SELECT * FROM overlays WHERE project_id = $1 AND epoch_id = $2 ORDER BY created_at ASC",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_all(&self.pool)
            .await?;
            let mut overlays = Vec::with_capacity(ov_rows.len());
            for orow in ov_rows {
                overlays.push(VerifiedOverlayView {
                    project_id: orow.get("project_id"),
                    epoch_id: orow.get::<i32, _>("epoch_id") as u32,
                    module_name: orow.get("module_name"),
                    overlay_kind: parse_enum(&orow.get::<String, _>("overlay_kind"))?,
                    source_task_id: orow.get("source_task_id"),
                    source_actor_id: orow.get("source_actor_id"),
                    blob_sha256: orow.get("blob_sha256"),
                    imports: serde_json::from_value(orow.get("imports"))?,
                    compile_ms: orow.get::<i64, _>("compile_ms").max(0) as u64,
                    created_at: orow.get("created_at"),
                });
            }

            let lease_rows = sqlx::query(
                "SELECT lease_id, actor_id, issued_at, expires_at, propensity_score FROM leases WHERE project_id = $1 AND epoch_id = $2 AND status = 'ACTIVE'",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_all(&self.pool)
            .await?;
            let active_leases = lease_rows
                .into_iter()
                .map(|r| LeaseInfo {
                    lease_id: r.get("lease_id"),
                    actor_id: r.get("actor_id"),
                    issued_at: r.get("issued_at"),
                    expires_at: r.get("expires_at"),
                    propensity_score: r.get("propensity_score"),
                })
                .collect();

            let graveyard_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM graveyard_entries WHERE project_id = $1 AND epoch_id = $2",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_one(&self.pool)
            .await?;

            Ok(Some(ProjectGraphView {
                project_id,
                project_status: parse_enum(&ep.get::<String, _>("project_status"))?,
                epoch_id,
                epoch_state: parse_enum(&ep.get::<String, _>("state"))?,
                bundle_id: ep.get("bundle_id"),
                lean_toolchain: ep.get("lean_toolchain"),
                spec_module: ep.get("spec_module"),
                spec_decl_name: ep.get("spec_decl_name"),
                root_task_id: ep.get("root_task_id"),
                dispatch_policy: serde_json::from_value(ep.get("dispatch_policy"))?,
                routes,
                tasks,
                edges,
                overlays,
                active_leases,
                graveyard_count: graveyard_count.max(0) as usize,
            }))
        })
    }

    fn wakeup_digest(
        &self,
        project_id: &str,
        epoch_id: u32,
    ) -> StoreFuture<'_, Option<WakeupDigestView>> {
        let project_id = project_id.to_string();
        Box::pin(async move {
            let Some(g) = self.project_graph(&project_id, epoch_id).await? else {
                return Ok(None);
            };
            let mut task_counts = BTreeMap::new();
            let mut open_tasks = 0usize;
            let mut blocked_tasks = 0usize;
            for t in &g.tasks {
                let key = encode_enum(&t.state)?;
                *task_counts.entry(key).or_insert(0) += 1;
                if t.is_dispatchable() {
                    open_tasks += 1;
                }
                if t.state == TaskState::Blocked {
                    blocked_tasks += 1;
                }
            }
            let blocked_rows: Vec<String> = sqlx::query_scalar(
                "SELECT actor_id FROM project_actor_blocks WHERE project_id = $1",
            )
            .bind(&project_id)
            .fetch_all(&self.pool)
            .await?;
            let blocked_set: HashSet<String> = blocked_rows.into_iter().collect();
            let actor_rows = sqlx::query("SELECT * FROM actors ORDER BY actor_id")
                .fetch_all(&self.pool)
                .await?;
            let mut actors = Vec::new();
            for ar in actor_rows {
                let mut prof = row_to_actor_profile(&ar)?;
                if prof.project_scopes.is_empty()
                    || prof.project_scopes.iter().any(|p| p == &project_id)
                {
                    prof.enabled = prof.enabled && !blocked_set.contains(&prof.actor_id);
                    actors.push(prof);
                }
            }
            let gy_rows = sqlx::query(
                "SELECT * FROM graveyard_entries WHERE project_id = $1 AND epoch_id = $2 ORDER BY entry_seq DESC LIMIT 15",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .fetch_all(&self.pool)
            .await?;
            let recent_graveyard = gy_rows
                .into_iter()
                .map(|r| GraveyardEntryView {
                    project_id: r.get("project_id"),
                    epoch_id: r.get::<i32, _>("epoch_id") as u32,
                    task_id: r.get("task_id"),
                    failure_kind: r.get("failure_kind"),
                    summary: r.get("summary"),
                    created_at: r.get("created_at"),
                })
                .collect();

            Ok(Some(WakeupDigestView {
                project_id,
                epoch_id,
                epoch_state: g.epoch_state,
                dispatch_policy: g.dispatch_policy,
                task_counts,
                open_tasks,
                blocked_tasks,
                top_voi_tasks: voi_rank(&g.tasks, &g.edges, 10),
                stall_alerts: detect_stalls(&g.tasks),
                coupling_warnings: check_coupling(&g.tasks),
                reusable_overlays: mine_reusable_overlays(&g.overlays, &g.tasks, &g.routes),
                actors,
                recent_graveyard,
            }))
        })
    }

    fn run_algo(
        &self,
        project_id: &str,
        epoch_id: u32,
        algo_name: &str,
        limit: usize,
    ) -> StoreFuture<'_, Option<AlgoQueryResult>> {
        let project_id = project_id.to_string();
        let algo_name = algo_name.to_string();
        Box::pin(async move {
            let Some(g) = self.project_graph(&project_id, epoch_id).await? else {
                return Ok(None);
            };
            let res = match algo_name.as_str() {
                "voi-rank" | "voi_rank" => AlgoQueryResult::VoiRank {
                    entries: voi_rank(&g.tasks, &g.edges, limit),
                },
                "detect-stalls" | "detect_stalls" => AlgoQueryResult::DetectStalls {
                    alerts: detect_stalls(&g.tasks),
                },
                "check-coupling" | "check_coupling" => AlgoQueryResult::CheckCoupling {
                    warnings: check_coupling(&g.tasks),
                },
                "mine-reusable" | "mine_reusable" => AlgoQueryResult::MineReusable {
                    hints: mine_reusable_overlays(&g.overlays, &g.tasks, &g.routes),
                },
                other => {
                    return Err(StoreError::BadRequest(format!(
                        "unknown quod-algo query '{other}'"
                    )));
                }
            };
            Ok(Some(res))
        })
    }

    fn recent_events(
        &self,
        project_id: &str,
        epoch_id: u32,
        since_seq: Option<i64>,
        limit: usize,
    ) -> StoreFuture<'_, Vec<EventRecordView>> {
        let project_id = project_id.to_string();
        Box::pin(async move {
            let mut out: Vec<EventRecordView> = match since_seq {
                Some(after_seq) => {
                    let rows = sqlx::query(
                        r#"
                        SELECT event_seq, event_id, occurred_at, project_id, epoch_id, aggregate_type, aggregate_id, event_type, actor_id, payload
                        FROM events
                        WHERE project_id = $1 AND epoch_id = $2 AND event_seq > $3
                        ORDER BY event_seq ASC
                        LIMIT $4
                        "#,
                    )
                    .bind(&project_id)
                    .bind(epoch_id as i32)
                    .bind(after_seq)
                    .bind(limit.min(500) as i64)
                    .fetch_all(&self.pool)
                    .await?;
                    rows.into_iter()
                        .map(|r| EventRecordView {
                            event_seq: r.get("event_seq"),
                            event_id: r.get("event_id"),
                            occurred_at: r.get("occurred_at"),
                            project_id: r.get("project_id"),
                            epoch_id: r.get("epoch_id"),
                            aggregate_type: r.get("aggregate_type"),
                            aggregate_id: r.get("aggregate_id"),
                            event_type: r.get("event_type"),
                            actor_id: r.get("actor_id"),
                            payload: r.get("payload"),
                        })
                        .collect()
                }
                None => {
                    let rows = sqlx::query(
                        r#"
                        SELECT event_seq, event_id, occurred_at, project_id, epoch_id, aggregate_type, aggregate_id, event_type, actor_id, payload
                        FROM events
                        WHERE project_id = $1 AND epoch_id = $2
                        ORDER BY event_seq DESC
                        LIMIT $3
                        "#,
                    )
                    .bind(&project_id)
                    .bind(epoch_id as i32)
                    .bind(limit.min(500) as i64)
                    .fetch_all(&self.pool)
                    .await?;
                    let mut recent: Vec<EventRecordView> = rows
                        .into_iter()
                        .map(|r| EventRecordView {
                            event_seq: r.get("event_seq"),
                            event_id: r.get("event_id"),
                            occurred_at: r.get("occurred_at"),
                            project_id: r.get("project_id"),
                            epoch_id: r.get("epoch_id"),
                            aggregate_type: r.get("aggregate_type"),
                            aggregate_id: r.get("aggregate_id"),
                            event_type: r.get("event_type"),
                            actor_id: r.get("actor_id"),
                            payload: r.get("payload"),
                        })
                        .collect();
                    recent.reverse();
                    recent
                }
            };
            let _ = &mut out;
            Ok(out)
        })
    }

    fn get_overlay_blob(
        &self,
        project_id: &str,
        epoch_id: u32,
        module_name: &str,
    ) -> StoreFuture<'_, Option<String>> {
        let project_id = project_id.to_string();
        let module_name = module_name.to_string();
        Box::pin(async move {
            let sha: Option<String> = sqlx::query_scalar(
                "SELECT blob_sha256 FROM overlays WHERE project_id = $1 AND epoch_id = $2 AND module_name = $3",
            )
            .bind(&project_id)
            .bind(epoch_id as i32)
            .bind(&module_name)
            .fetch_optional(&self.pool)
            .await?;
            Ok(sha)
        })
    }

    fn leaderboard(
        &self,
        project_id: Option<&str>,
        limit: usize,
    ) -> StoreFuture<'_, Vec<LeaderboardEntryView>> {
        let project_id = project_id.map(str::to_string);
        let limit = limit.clamp(1, 200) as i64;
        Box::pin(async move {
            let rows = sqlx::query(
                r#"
                SELECT
                    l.actor_id,
                    a.display_name,
                    a.lineage_cluster,
                    a.tier,
                    COALESCE(SUM(l.points), 0)::BIGINT AS credit_points,
                    COUNT(*) FILTER (WHERE l.reason <> 'refutation_accepted')::BIGINT AS verified_tasks,
                    COUNT(*) FILTER (WHERE l.reason = 'refutation_accepted')::BIGINT AS verified_refutations
                FROM credit_ledger l
                JOIN actors a ON a.actor_id = l.actor_id
                WHERE ($1::TEXT IS NULL OR l.project_id = $1)
                GROUP BY l.actor_id, a.display_name, a.lineage_cluster, a.tier
                ORDER BY credit_points DESC, verified_tasks DESC, l.actor_id ASC
                LIMIT $2
                "#,
            )
            .bind(project_id.as_deref())
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|r| LeaderboardEntryView {
                    actor_id: r.get("actor_id"),
                    display_name: r.get("display_name"),
                    lineage_cluster: r.get("lineage_cluster"),
                    tier: r.get::<i16, _>("tier").clamp(1, 4) as u8,
                    credit_points: r.get("credit_points"),
                    verified_tasks: r.get::<i64, _>("verified_tasks").max(0) as u64,
                    verified_refutations: r.get::<i64, _>("verified_refutations").max(0) as u64,
                })
                .collect())
        })
    }

    fn live_metrics(&self) -> StoreFuture<'_, HubLiveMetrics> {
        Box::pin(async move {
            let row = sqlx::query(
                r#"
                SELECT
                    (SELECT COUNT(DISTINCT actor_id) FROM leases WHERE status = 'ACTIVE' AND expires_at > now()) AS online_agents,
                    (SELECT COUNT(*) FROM leases WHERE status = 'ACTIVE' AND expires_at > now()) AS active_leases,
                    (SELECT COUNT(*) FROM actors WHERE enabled = TRUE) AS registered_agents
                "#,
            )
            .fetch_one(&self.pool)
            .await?;
            let online_agents: i64 = row.try_get("online_agents")?;
            let active_leases: i64 = row.try_get("active_leases")?;
            let registered_agents: i64 = row.try_get("registered_agents")?;
            Ok(HubLiveMetrics {
                online_agents: online_agents.max(0) as usize,
                active_leases: active_leases.max(0) as usize,
                registered_agents: registered_agents.max(0) as usize,
            })
        })
    }

    fn health_check(&self) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            sqlx::query("SELECT 1").execute(&self.pool).await?;
            Ok(())
        })
    }
}
