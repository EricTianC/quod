use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};

use chrono::{Duration, Utc};
use quod_core::algo::{
    check_coupling, detect_stalls, mine_reusable_overlays, score_task_for_actor, voi_rank,
};
use quod_core::{
    ActorAuthContext, ActorProfile, AlgoQueryResult, ApplyIrRequest, ApplyIrResponse,
    AuditEpochRequest, ClaimLeaseRequest, CreateEpochRequest, CreateInviteRequest,
    CreateProjectRequest, DispatchPolicyConfig, EdgeKind, EpochEvent, EpochState, EpochStatusView,
    EventRecordView, GraphIrOp, GraveyardEntryView, HeartbeatResponse, InviteTokenView,
    JoinActorRequest, LeaderboardEntryView, LeaseInfo, OverlayKind, ProjectGraphView,
    ProjectStatus, ProjectSummaryView, RegisterActorRequest, RegisterBundleRequest, Role,
    RouteState, RouteView, SubmissionKind, SubmissionState, SubmissionStatusView, SubmitResponse,
    TaskEdgeView, TaskEvent, TaskNodeView, TaskPacket, TaskRole, TaskState, TaskTelemetryView,
    ToolchainBundle, VerifiedOverlayView, WakeupDigestView, WhoAmIResponse, compute_default_bounty,
    validate_pubkey_hex,
};
use uuid::Uuid;

use crate::{
    ActorCredentials, BlobStore, EventEnvelope, HubLiveMetrics, NewSubmissionInput, QuodStore,
    StoreError, StoreFuture, VerificationJob, VerificationOutcomeInput, VerificationOutcomeResult,
    overlay_module_name, validate_actor_id,
};
use quod_core::CreditLedgerEntryView;

#[derive(Debug, Clone)]
struct StoredActor {
    pubkey_ed25519: String,
    profile: ActorProfile,
}

#[derive(Debug, Clone)]
struct StoredProject {
    summary: ProjectSummaryView,
}

#[derive(Debug, Clone)]
struct StoredEpoch {
    project_id: String,
    epoch_id: u32,
    state: EpochState,
    bundle_id: String,
    spec_module: String,
    spec_decl_name: String,
    spec_sha256: String,
    root_task_id: String,
    dispatch_policy: DispatchPolicyConfig,
    created_by: String,
    audited_by: Option<String>,
}

#[derive(Debug, Clone)]
struct StoredLease {
    info: LeaseInfo,
    project_id: String,
    epoch_id: u32,
    task_id: String,
    lineage_cluster: String,
    status: String,
}

#[derive(Debug, Clone)]
struct StoredSubmission {
    submission_id: Uuid,
    project_id: String,
    epoch_id: u32,
    task_id: String,
    actor_id: String,
    kind: SubmissionKind,
    state: SubmissionState,
    compile_ms: Option<u64>,
    reject_detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
struct StoredJob {
    job: VerificationJob,
    status: String,
    claimed_by: Option<String>,
}

#[derive(Debug, Default, Clone)]
struct MemState {
    next_event_seq: i64,
    events: Vec<(i64, EventEnvelope)>,
    bundles: HashMap<String, ToolchainBundle>,
    actors: HashMap<String, StoredActor>,
    projects: HashMap<String, StoredProject>,
    epochs: HashMap<(String, u32), StoredEpoch>,
    routes: HashMap<(String, u32, String), RouteView>,
    tasks: HashMap<(String, u32, String), TaskNodeView>,
    edges: Vec<(String, u32, TaskEdgeView)>,
    overlays: HashMap<(String, u32, String), VerifiedOverlayView>,
    overlay_sources: HashMap<(String, u32, String), String>,
    leases: HashMap<Uuid, StoredLease>,
    submissions: HashMap<Uuid, StoredSubmission>,
    jobs: HashMap<Uuid, StoredJob>,
    graveyard: Vec<GraveyardEntryView>,
    invites: HashMap<String, InviteTokenView>,
    project_actor_blocks: HashSet<(String, String)>,
    credit_ledger: Vec<CreditLedgerEntryView>,
}

impl MemState {
    fn emit_event(
        &mut self,
        project_id: &str,
        epoch_id: i32,
        aggregate_type: &str,
        aggregate_id: &str,
        event_type: &str,
        actor_id: &str,
        payload: serde_json::Value,
    ) {
        self.next_event_seq += 1;
        let seq = self.next_event_seq;
        self.events.push((
            seq,
            EventEnvelope {
                event_id: Uuid::now_v7(),
                occurred_at: Utc::now(),
                project_id: project_id.to_string(),
                epoch_id,
                aggregate_type: aggregate_type.to_string(),
                aggregate_id: aggregate_id.to_string(),
                event_type: event_type.to_string(),
                idempotency_key: format!("mem:{seq}:{}", Uuid::now_v7()),
                actor_id: actor_id.to_string(),
                payload,
            },
        ));
    }

    fn record_graveyard(
        &mut self,
        project_id: &str,
        epoch_id: u32,
        task_id: &str,
        failure_kind: &str,
        summary: String,
    ) {
        self.graveyard.push(GraveyardEntryView {
            project_id: project_id.to_string(),
            epoch_id,
            task_id: task_id.to_string(),
            failure_kind: failure_kind.to_string(),
            summary,
            created_at: Utc::now(),
        });
    }

    fn expire_task_leases(&mut self, project_id: &str, epoch_id: u32, task_id: &str, status: &str) {
        for lease in self.leases.values_mut() {
            if lease.project_id == project_id
                && lease.epoch_id == epoch_id
                && lease.task_id == task_id
                && lease.status == "ACTIVE"
            {
                lease.status = status.to_string();
            }
        }
    }

    fn has_depends_on_cycle(&self, project_id: &str, epoch_id: u32, from: &str, to: &str) -> bool {
        if from == to {
            return true;
        }
        // Check if `from` is reachable starting from `to` following DependsOn edges.
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for (p, e, edge) in &self.edges {
            if p == project_id && *e == epoch_id && edge.edge_kind == EdgeKind::DependsOn {
                adj.entry(edge.from_task_id.as_str())
                    .or_default()
                    .push(edge.to_task_id.as_str());
            }
        }
        let mut q = VecDeque::new();
        let mut seen = HashSet::new();
        q.push_back(to);
        seen.insert(to);
        while let Some(cur) = q.pop_front() {
            if cur == from {
                return true;
            }
            if let Some(nexts) = adj.get(cur) {
                for &nxt in nexts {
                    if seen.insert(nxt) {
                        q.push_back(nxt);
                    }
                }
            }
        }
        false
    }

    fn all_dependencies_proved(&self, project_id: &str, epoch_id: u32, task_id: &str) -> bool {
        for (p, e, edge) in &self.edges {
            if p == project_id
                && *e == epoch_id
                && edge.edge_kind == EdgeKind::DependsOn
                && edge.to_task_id == task_id
            {
                let upstream_proved = self
                    .tasks
                    .get(&(project_id.to_string(), epoch_id, edge.from_task_id.clone()))
                    .is_some_and(|t| t.state == TaskState::Proved);
                if !upstream_proved {
                    return false;
                }
            }
        }
        true
    }

    fn active_epoch_for_project(&self, project_id: &str) -> Option<StoredEpoch> {
        self.epochs
            .values()
            .find(|e| e.project_id == project_id && e.state == EpochState::Active)
            .cloned()
    }

    fn build_epoch_status(&self, epoch: &StoredEpoch) -> EpochStatusView {
        let mut task_counts: BTreeMap<String, usize> = BTreeMap::new();
        for t in self.tasks.values() {
            if t.project_id == epoch.project_id && t.epoch_id == epoch.epoch_id {
                let key = serde_json::to_value(t.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("{:?}", t.state));
                *task_counts.entry(key).or_insert(0) += 1;
            }
        }
        let active_leases = self
            .leases
            .values()
            .filter(|l| {
                l.project_id == epoch.project_id
                    && l.epoch_id == epoch.epoch_id
                    && l.status == "ACTIVE"
            })
            .count();
        let pending_verification_jobs = self
            .jobs
            .values()
            .filter(|j| {
                j.job.project_id == epoch.project_id
                    && j.job.epoch_id == epoch.epoch_id
                    && (j.status == "PENDING" || j.status == "RUNNING")
            })
            .count();
        let overlays_total = self
            .overlays
            .keys()
            .filter(|(p, e, _)| p == &epoch.project_id && *e == epoch.epoch_id)
            .count();
        let routes_total = self
            .routes
            .keys()
            .filter(|(p, e, _)| p == &epoch.project_id && *e == epoch.epoch_id)
            .count();
        let lean_toolchain = self
            .bundles
            .get(&epoch.bundle_id)
            .map(|b| b.lean_toolchain.clone())
            .unwrap_or_else(|| "leanprover/lean4:v4.36.0".into());

        EpochStatusView {
            project_id: epoch.project_id.clone(),
            epoch_id: epoch.epoch_id,
            state: epoch.state,
            bundle_id: epoch.bundle_id.clone(),
            lean_toolchain,
            spec_decl_name: epoch.spec_decl_name.clone(),
            root_task_id: epoch.root_task_id.clone(),
            dispatch_policy: epoch.dispatch_policy.clone(),
            task_counts,
            active_leases,
            pending_verification_jobs,
            overlays_total,
            routes_total,
        }
    }
}

/// Thread-safe in-memory store mirroring `PgEventStore` semantics for unit & HTTP integration tests.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    inner: Arc<RwLock<MemState>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl QuodStore for MemoryStore {
    fn register_actor(&self, req: &RegisterActorRequest) -> StoreFuture<'_, ActorProfile> {
        let req = req.clone();
        Box::pin(async move {
            let mut st = self.inner.write().unwrap();
            let tier = req.initial_tier.unwrap_or(1).clamp(1, 4);
            let roles = if req.roles.is_empty() {
                vec![Role::Worker]
            } else {
                req.roles.clone()
            };
            let existing = st.actors.get(&req.actor_id).map(|a| a.profile.clone());
            let profile = ActorProfile {
                actor_id: req.actor_id.clone(),
                roles,
                project_scopes: req.project_scopes.clone(),
                lineage_cluster: req.lineage_cluster.clone(),
                tier,
                skill_tags: req.skill_tags.clone(),
                enabled: existing.as_ref().map(|p| p.enabled).unwrap_or(true),
                ema_accept_rate: existing.as_ref().map(|p| p.ema_accept_rate).unwrap_or(0.5),
                display_name: req
                    .display_name
                    .clone()
                    .or_else(|| existing.as_ref().and_then(|p| p.display_name.clone())),
                git_email: req
                    .git_email
                    .clone()
                    .or_else(|| existing.as_ref().and_then(|p| p.git_email.clone())),
                credit_points: existing.as_ref().map(|p| p.credit_points).unwrap_or(0),
                total_probes: existing.as_ref().map(|p| p.total_probes).unwrap_or(0),
                probe_passes: existing.as_ref().map(|p| p.probe_passes).unwrap_or(0),
                total_submissions: existing.as_ref().map(|p| p.total_submissions).unwrap_or(0),
                accepted_submissions: existing
                    .as_ref()
                    .map(|p| p.accepted_submissions)
                    .unwrap_or(0),
                rejected_submissions: existing
                    .as_ref()
                    .map(|p| p.rejected_submissions)
                    .unwrap_or(0),
                avg_compile_ms: existing.and_then(|p| p.avg_compile_ms),
            };
            st.actors.insert(
                req.actor_id.clone(),
                StoredActor {
                    pubkey_ed25519: req.pubkey_ed25519.clone(),
                    profile: profile.clone(),
                },
            );
            st.emit_event(
                "_global",
                0,
                "actor",
                &req.actor_id,
                "ActorRegistered",
                "admin",
                serde_json::json!({ "roles": profile.roles, "lineage_cluster": req.lineage_cluster }),
            );
            Ok(profile)
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
            let view = InviteTokenView {
                invite_code: invite_code.clone(),
                roles: req.roles.clone(),
                project_scopes: req.project_scopes.clone(),
                max_uses: req.max_uses,
                used_count: 0,
                initial_tier,
                created_by: created_by.clone(),
                note: req.note.clone(),
                expires_at,
            };
            let mut st = self.inner.write().unwrap();
            st.invites.insert(invite_code.clone(), view.clone());
            let scope_proj = req
                .project_scopes
                .first()
                .cloned()
                .unwrap_or_else(|| "_global".into());
            st.emit_event(
                &scope_proj,
                0,
                "invite",
                &invite_code,
                "InviteCreated",
                &created_by,
                serde_json::json!({
                    "roles": view.roles,
                    "project_scopes": view.project_scopes,
                    "max_uses": view.max_uses,
                    "initial_tier": view.initial_tier,
                }),
            );
            Ok(view)
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

            let mut st = self.inner.write().unwrap();
            if st.actors.contains_key(&req.actor_id) {
                return Err(StoreError::Conflict(format!(
                    "actor_id '{}' is already registered",
                    req.actor_id
                )));
            }
            if st
                .actors
                .values()
                .any(|a| a.pubkey_ed25519 == req.pubkey_ed25519)
            {
                return Err(StoreError::Conflict(
                    "pubkey_ed25519 is already registered to another actor".into(),
                ));
            }

            let (roles, project_scopes, tier, via) = match req.invite_code.as_deref() {
                Some(code) => {
                    let now = Utc::now();
                    let inv = st
                        .invites
                        .get_mut(code)
                        .ok_or_else(|| StoreError::Forbidden("invalid invite_code".into()))?;
                    if now >= inv.expires_at {
                        return Err(StoreError::Forbidden("invite_code has expired".into()));
                    }
                    if inv.used_count >= inv.max_uses {
                        return Err(StoreError::Conflict(
                            "invite_code has reached its maximum uses".into(),
                        ));
                    }
                    inv.used_count += 1;
                    (
                        inv.roles.clone(),
                        inv.project_scopes.clone(),
                        inv.initial_tier,
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
                        1,
                        "open_worker_join".to_string(),
                    )
                }
            };

            let profile = ActorProfile {
                actor_id: req.actor_id.clone(),
                roles: roles.clone(),
                project_scopes: project_scopes.clone(),
                lineage_cluster: req.lineage_cluster.clone(),
                tier,
                skill_tags: req.skill_tags.clone(),
                enabled: true,
                ema_accept_rate: 0.5,
                display_name: req.display_name.clone(),
                git_email: req.git_email.clone(),
                credit_points: 0,
                total_probes: 0,
                probe_passes: 0,
                total_submissions: 0,
                accepted_submissions: 0,
                rejected_submissions: 0,
                avg_compile_ms: None,
            };
            st.actors.insert(
                req.actor_id.clone(),
                StoredActor {
                    pubkey_ed25519: req.pubkey_ed25519.clone(),
                    profile: profile.clone(),
                },
            );
            st.emit_event(
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
            );
            Ok(profile)
        })
    }

    fn get_actor(&self, actor_id: &str) -> StoreFuture<'_, Option<ActorProfile>> {
        let actor_id = actor_id.to_string();
        Box::pin(async move {
            let st = self.inner.read().unwrap();
            Ok(st.actors.get(&actor_id).map(|a| a.profile.clone()))
        })
    }

    fn get_actor_auth(&self, actor_id: &str) -> StoreFuture<'_, Option<ActorCredentials>> {
        let actor_id = actor_id.to_string();
        Box::pin(async move {
            let st = self.inner.read().unwrap();
            Ok(st.actors.get(&actor_id).map(|a| ActorCredentials {
                pubkey_ed25519: a.pubkey_ed25519.clone(),
                auth: ActorAuthContext {
                    actor_id: a.profile.actor_id.clone(),
                    roles: a.profile.roles.clone(),
                    project_scopes: a.profile.project_scopes.clone(),
                    enabled: a.profile.enabled,
                },
            }))
        })
    }

    fn whoami(&self, actor_id: &str) -> StoreFuture<'_, Option<WhoAmIResponse>> {
        let actor_id = actor_id.to_string();
        Box::pin(async move {
            let st = self.inner.read().unwrap();
            let Some(actor) = st.actors.get(&actor_id) else {
                return Ok(None);
            };
            let auth = ActorAuthContext {
                actor_id: actor.profile.actor_id.clone(),
                roles: actor.profile.roles.clone(),
                project_scopes: actor.profile.project_scopes.clone(),
                enabled: actor.profile.enabled,
            };
            let mut active_projects: Vec<String> = st
                .projects
                .values()
                .filter(|p| {
                    p.summary.status == ProjectStatus::Active
                        && auth.can_access_project(&p.summary.project_id)
                })
                .map(|p| p.summary.project_id.clone())
                .collect();
            active_projects.sort();
            Ok(Some(WhoAmIResponse {
                actor_id: actor.profile.actor_id.clone(),
                roles: actor.profile.roles.clone(),
                project_scopes: actor.profile.project_scopes.clone(),
                lineage_cluster: actor.profile.lineage_cluster.clone(),
                tier: actor.profile.tier,
                enabled: actor.profile.enabled,
                display_name: actor.profile.display_name.clone(),
                git_email: actor.profile.git_email.clone(),
                credit_points: actor.profile.credit_points,
                active_projects,
            }))
        })
    }

    fn register_bundle(&self, req: &RegisterBundleRequest) -> StoreFuture<'_, ToolchainBundle> {
        let req = req.clone();
        Box::pin(async move {
            let mut st = self.inner.write().unwrap();
            let bundle = ToolchainBundle {
                bundle_id: req.bundle_id.clone(),
                lean_toolchain: req.lean_toolchain,
                mathlib_rev: req.mathlib_rev,
                toolchain_dir: req.toolchain_dir,
                lean_paths: req.lean_paths,
                bundle_sha256: req.bundle_sha256,
            };
            st.bundles.insert(req.bundle_id.clone(), bundle.clone());
            Ok(bundle)
        })
    }

    fn get_bundle(&self, bundle_id: &str) -> StoreFuture<'_, Option<ToolchainBundle>> {
        let bundle_id = bundle_id.to_string();
        Box::pin(async move {
            let st = self.inner.read().unwrap();
            Ok(st.bundles.get(&bundle_id).cloned())
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
            let mut st = self.inner.write().unwrap();
            if st.projects.contains_key(&req.project_id) {
                return Err(StoreError::Conflict(format!(
                    "project {} already exists",
                    req.project_id
                )));
            }
            let summary = ProjectSummaryView {
                project_id: req.project_id.clone(),
                title: req.title,
                description: req.description,
                spec_namespace: req.spec_namespace,
                status: ProjectStatus::Active,
                active_epoch_id: None,
                created_by: actor_id.clone(),
                created_at: Utc::now(),
            };
            st.projects.insert(
                req.project_id.clone(),
                StoredProject {
                    summary: summary.clone(),
                },
            );
            st.emit_event(
                &req.project_id,
                0,
                "project",
                &req.project_id,
                "ProjectCreated",
                &actor_id,
                serde_json::json!({ "title": summary.title }),
            );
            Ok(summary)
        })
    }

    fn list_projects(&self) -> StoreFuture<'_, Vec<ProjectSummaryView>> {
        Box::pin(async move {
            let st = self.inner.read().unwrap();
            let mut list: Vec<ProjectSummaryView> = st
                .projects
                .values()
                .map(|p| {
                    let mut s = p.summary.clone();
                    s.active_epoch_id = st
                        .active_epoch_for_project(&s.project_id)
                        .map(|e| e.epoch_id);
                    s
                })
                .collect();
            list.sort_by(|a, b| a.project_id.cmp(&b.project_id));
            Ok(list)
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
            let mut st = self.inner.write().unwrap();
            if !st.projects.contains_key(&project_id) {
                return Err(StoreError::NotFound(format!(
                    "project {project_id} not found"
                )));
            }
            if !st.bundles.contains_key(&req.bundle_id) {
                return Err(StoreError::NotFound(format!(
                    "toolchain bundle {} not found",
                    req.bundle_id
                )));
            }
            if st.epochs.contains_key(&(project_id.clone(), req.epoch_id)) {
                return Err(StoreError::Conflict(format!(
                    "epoch ({project_id}, {}) already exists",
                    req.epoch_id
                )));
            }
            if req.auto_activate && st.active_epoch_for_project(&project_id).is_some() {
                return Err(StoreError::Conflict(format!(
                    "project {project_id} already has an ACTIVE epoch"
                )));
            }
            let state = if req.auto_activate {
                EpochState::Active
            } else {
                EpochState::PendingAudit
            };
            let epoch = StoredEpoch {
                project_id: project_id.clone(),
                epoch_id: req.epoch_id,
                state,
                bundle_id: req.bundle_id.clone(),
                spec_module: req.spec_module.clone(),
                spec_decl_name: req.spec_decl_name.clone(),
                spec_sha256: req.spec_sha256.clone(),
                root_task_id: req.root_task_id.clone(),
                dispatch_policy: DispatchPolicyConfig::default(),
                created_by: actor_id.clone(),
                audited_by: if req.auto_activate {
                    Some(actor_id.clone())
                } else {
                    None
                },
            };
            st.epochs
                .insert((project_id.clone(), req.epoch_id), epoch.clone());

            if let Some(code) = &req.spec_code {
                st.overlays.insert(
                    (project_id.clone(), req.epoch_id, req.spec_module.clone()),
                    VerifiedOverlayView {
                        project_id: project_id.clone(),
                        epoch_id: req.epoch_id,
                        module_name: req.spec_module.clone(),
                        overlay_kind: OverlayKind::Spec,
                        source_task_id: None,
                        source_actor_id: actor_id.clone(),
                        blob_sha256: req.spec_sha256.clone(),
                        imports: vec![],
                        compile_ms: 0,
                        created_at: Utc::now(),
                    },
                );
                st.overlay_sources.insert(
                    (project_id.clone(), req.epoch_id, req.spec_module.clone()),
                    code.clone(),
                );
            }

            st.tasks.insert(
                (project_id.clone(), req.epoch_id, req.root_task_id.clone()),
                TaskNodeView {
                    task_id: req.root_task_id.clone(),
                    project_id: project_id.clone(),
                    epoch_id: req.epoch_id,
                    route_id: None,
                    task_role: TaskRole::Root,
                    state: TaskState::Open,
                    priority: 1.0,
                    racer_cap_k: 2,
                    lease_ttl_secs: None,
                    min_tier: 1,
                    preferred_lineages: vec![],
                    prior_p: 0.5,
                    expected_cost_c: 2.0,
                    spec_decl_name: req.spec_decl_name.clone(),
                    spec_statement: req.spec_statement.clone(),
                    target_inline: false,
                    candidate_decl: "candidate".into(),
                    curated_imports: vec![req.spec_module.clone()],
                    module_hint: Some(req.spec_module.clone()),
                    credit_bounty: compute_default_bounty(2.0, 1.0, 0),
                    telemetry: TaskTelemetryView::default(),
                },
            );
            st.emit_event(
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
                serde_json::json!({ "root_task_id": req.root_task_id, "bundle_id": req.bundle_id }),
            );
            Ok(st.build_epoch_status(&epoch))
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
            let mut st = self.inner.write().unwrap();
            let (current_state, created_by) = st
                .epochs
                .get(&(project_id.clone(), epoch_id))
                .map(|e| (e.state, e.created_by.clone()))
                .ok_or_else(|| {
                    StoreError::NotFound(format!("epoch ({project_id}, {epoch_id}) not found"))
                })?;

            if created_by == auditor_id {
                return Err(StoreError::Forbidden(format!(
                    "separation of duties: epoch proposer '{created_by}' cannot audit their own epoch"
                )));
            }

            if req.approve
                && st
                    .active_epoch_for_project(&project_id)
                    .is_some_and(|e| e.epoch_id != epoch_id)
            {
                return Err(StoreError::Conflict(format!(
                    "project {project_id} already has an ACTIVE epoch"
                )));
            }

            let next_state = current_state.transition(if req.approve {
                EpochEvent::ApproveAudit
            } else {
                EpochEvent::RejectAudit
            })?;

            let epoch_mut = st.epochs.get_mut(&(project_id.clone(), epoch_id)).unwrap();
            epoch_mut.state = next_state;
            epoch_mut.audited_by = Some(auditor_id.clone());
            let epoch_clone = epoch_mut.clone();

            st.emit_event(
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
            );
            Ok(st.build_epoch_status(&epoch_clone))
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
            let mut st = self.inner.write().unwrap();
            let current = st
                .epochs
                .get(&(project_id.clone(), epoch_id))
                .map(|e| e.state)
                .ok_or_else(|| {
                    StoreError::NotFound(format!("epoch ({project_id}, {epoch_id}) not found"))
                })?;
            let next = current.transition(EpochEvent::BeginFreeze)?;
            let epoch_clone = {
                let ep = st.epochs.get_mut(&(project_id.clone(), epoch_id)).unwrap();
                ep.state = next;
                ep.clone()
            };
            for t in st.tasks.values_mut() {
                if t.project_id == project_id && t.epoch_id == epoch_id && !t.state.is_terminal() {
                    t.state = t.state.transition(TaskEvent::FreezeEpoch)?;
                }
            }
            for l in st.leases.values_mut() {
                if l.project_id == project_id && l.epoch_id == epoch_id && l.status == "ACTIVE" {
                    l.status = "STALE_EPOCH".into();
                }
            }
            st.emit_event(
                &project_id,
                epoch_id as i32,
                "epoch",
                &epoch_id.to_string(),
                "EpochFrozen",
                &actor_id,
                serde_json::json!({}),
            );
            Ok(st.build_epoch_status(&epoch_clone))
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
        // Pre-store DefOverlay code blobs before acquiring lock
        let mut pre_stored_blobs: Vec<String> = Vec::new();
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
            let mut st_guard = self.inner.write().unwrap();
            let mut st = st_guard.clone();
            let epoch = st
                .epochs
                .get(&(project_id.clone(), epoch_id))
                .cloned()
                .ok_or_else(|| {
                    StoreError::NotFound(format!("epoch ({project_id}, {epoch_id}) not found"))
                })?;
            if epoch.state != EpochState::Active {
                return Err(StoreError::Conflict(format!(
                    "epoch ({project_id}, {epoch_id}) is {:?}, not ACTIVE",
                    epoch.state
                )));
            }

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
                        let key = (project_id.clone(), epoch_id, route_id.clone());
                        if st.routes.contains_key(&key) {
                            return Err(StoreError::Conflict(format!(
                                "route {route_id} already exists"
                            )));
                        }
                        st.routes.insert(
                            key,
                            RouteView {
                                route_id: route_id.clone(),
                                project_id: project_id.clone(),
                                epoch_id,
                                title: title.clone(),
                                rationale: rationale.clone(),
                                state: RouteState::Active,
                                created_by: actor_id.clone(),
                                created_at: Utc::now(),
                            },
                        );
                        created_routes.push(route_id.clone());
                        st.emit_event(
                            &project_id,
                            epoch_id as i32,
                            "route",
                            route_id,
                            "RouteCreated",
                            &actor_id,
                            serde_json::json!({ "title": title }),
                        );
                    }
                    GraphIrOp::PruneRoute { route_id, reason } => {
                        let key = (project_id.clone(), epoch_id, route_id.clone());
                        let route = st
                            .routes
                            .get_mut(&key)
                            .ok_or_else(|| StoreError::NotFound(format!("route {route_id} not found")))?;
                        route.state = RouteState::Pruned;

                        let mut to_prune = Vec::new();
                        for t in st.tasks.values_mut() {
                            if t.project_id == project_id
                                && t.epoch_id == epoch_id
                                && t.route_id.as_deref() == Some(route_id.as_str())
                                && !t.state.is_terminal()
                            {
                                t.state = t.state.transition(TaskEvent::Prune)?;
                                to_prune.push(t.task_id.clone());
                            }
                        }
                        for tid in to_prune {
                            st.expire_task_leases(&project_id, epoch_id, &tid, "PRUNED");
                        }
                        st.emit_event(
                            &project_id,
                            epoch_id as i32,
                            "route",
                            route_id,
                            "RoutePruned",
                            &actor_id,
                            serde_json::json!({ "reason": reason }),
                        );
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
                        let tkey = (project_id.clone(), epoch_id, task_id.clone());
                        if st.tasks.contains_key(&tkey) {
                            return Err(StoreError::Conflict(format!(
                                "task {task_id} already exists"
                            )));
                        }
                        if let Some(rid) = route_id {
                            let r = st
                                .routes
                                .get(&(project_id.clone(), epoch_id, rid.clone()))
                                .ok_or_else(|| StoreError::NotFound(format!("route {rid} not found")))?;
                            if r.state != RouteState::Active {
                                return Err(StoreError::Conflict(format!(
                                    "route {rid} is {:?}, cannot spawn task",
                                    r.state
                                )));
                            }
                        }
                        let mut imports = curated_imports.clone();
                        if !*target_inline && !imports.contains(&epoch.spec_module) {
                            imports.insert(0, epoch.spec_module.clone());
                        }
                        let mut all_deps_proved = true;
                        for dep in depends_on {
                            let dep_task = st
                                .tasks
                                .get(&(project_id.clone(), epoch_id, dep.clone()))
                                .ok_or_else(|| {
                                    StoreError::NotFound(format!("dependency task {dep} not found"))
                                })?
                                .clone();
                            if dep_task.state != TaskState::Proved {
                                all_deps_proved = false;
                            } else {
                                for ov in st.overlays.values() {
                                    if ov.project_id == project_id
                                        && ov.epoch_id == epoch_id
                                        && ov.source_task_id.as_deref() == Some(dep.as_str())
                                        && !imports.contains(&ov.module_name)
                                    {
                                        imports.push(ov.module_name.clone());
                                    }
                                }
                            }
                        }
                        if let Some(target_tid) = refutes_task_id
                            && !st
                                .tasks
                                .contains_key(&(project_id.clone(), epoch_id, target_tid.clone()))
                        {
                            return Err(StoreError::NotFound(format!(
                                "refutation target task {target_tid} not found"
                            )));
                        }

                        let initial_state = if all_deps_proved {
                            if !depends_on.is_empty() {
                                unlocked_tasks.push(task_id.clone());
                            }
                            TaskState::Open
                        } else {
                            TaskState::Blocked
                        };

                        st.tasks.insert(
                            tkey,
                            TaskNodeView {
                                task_id: task_id.clone(),
                                project_id: project_id.clone(),
                                epoch_id,
                                route_id: route_id.clone(),
                                task_role: *task_role,
                                state: initial_state,
                                priority: *priority,
                                racer_cap_k: (*racer_cap_k).max(1),
                                lease_ttl_secs: None,
                                min_tier: (*min_tier).clamp(1, 4),
                                preferred_lineages: preferred_lineages.clone(),
                                prior_p: *prior_p,
                                expected_cost_c: *expected_cost_c,
                                spec_decl_name: spec_decl_name.clone(),
                                spec_statement: spec_statement.clone(),
                                target_inline: *target_inline,
                                candidate_decl: candidate_decl.clone(),
                                curated_imports: imports,
                                module_hint: module_hint.clone(),
                                credit_bounty: credit_bounty
                                    .filter(|b| *b > 0)
                                    .unwrap_or_else(|| compute_default_bounty(*expected_cost_c, *priority, 0)),
                                telemetry: TaskTelemetryView::default(),
                            },
                        );

                        for dep in depends_on {
                            st.edges.push((
                                project_id.clone(),
                                epoch_id,
                                TaskEdgeView {
                                    from_task_id: dep.clone(),
                                    to_task_id: task_id.clone(),
                                    edge_kind: EdgeKind::DependsOn,
                                },
                            ));
                        }
                        if let Some(target_tid) = refutes_task_id {
                            st.edges.push((
                                project_id.clone(),
                                epoch_id,
                                TaskEdgeView {
                                    from_task_id: task_id.clone(),
                                    to_task_id: target_tid.clone(),
                                    edge_kind: EdgeKind::Refutes,
                                },
                            ));
                        }
                        spawned_tasks.push(task_id.clone());
                        st.emit_event(
                            &project_id,
                            epoch_id as i32,
                            "task",
                            task_id,
                            "TaskSpawned",
                            &actor_id,
                            serde_json::json!({ "role": task_role, "state": initial_state, "depends_on": depends_on }),
                        );
                    }
                    GraphIrOp::LinkDependency {
                        upstream_task_id,
                        downstream_task_id,
                    } => {
                        let up_key = (project_id.clone(), epoch_id, upstream_task_id.clone());
                        let down_key = (project_id.clone(), epoch_id, downstream_task_id.clone());
                        let up_task = st
                            .tasks
                            .get(&up_key)
                            .ok_or_else(|| {
                                StoreError::NotFound(format!("task {upstream_task_id} not found"))
                            })?
                            .clone();
                        if !st.tasks.contains_key(&down_key) {
                            return Err(StoreError::NotFound(format!(
                                "task {downstream_task_id} not found"
                            )));
                        }
                        if st.has_depends_on_cycle(
                            &project_id,
                            epoch_id,
                            upstream_task_id,
                            downstream_task_id,
                        ) {
                            return Err(StoreError::Conflict(format!(
                                "dependency cycle detected linking {upstream_task_id} -> {downstream_task_id}"
                            )));
                        }
                        let already = st.edges.iter().any(|(p, e, edge)| {
                            p == &project_id
                                && *e == epoch_id
                                && edge.edge_kind == EdgeKind::DependsOn
                                && &edge.from_task_id == upstream_task_id
                                && &edge.to_task_id == downstream_task_id
                        });
                        if !already {
                            st.edges.push((
                                project_id.clone(),
                                epoch_id,
                                TaskEdgeView {
                                    from_task_id: upstream_task_id.clone(),
                                    to_task_id: downstream_task_id.clone(),
                                    edge_kind: EdgeKind::DependsOn,
                                },
                            ));
                        }
                        let ov_mods: Vec<String> = st
                            .overlays
                            .values()
                            .filter(|ov| {
                                ov.project_id == project_id
                                    && ov.epoch_id == epoch_id
                                    && ov.source_task_id.as_deref() == Some(upstream_task_id.as_str())
                            })
                            .map(|ov| ov.module_name.clone())
                            .collect();
                        if let Some(down_mut) = st.tasks.get_mut(&down_key) {
                            if up_task.state != TaskState::Proved
                                && down_mut.state == TaskState::Open
                            {
                                down_mut.state = TaskState::Blocked;
                            }
                            for m in ov_mods {
                                if !down_mut.curated_imports.contains(&m) {
                                    down_mut.curated_imports.push(m);
                                }
                            }
                        }
                        st.emit_event(
                            &project_id,
                            epoch_id as i32,
                            "task",
                            downstream_task_id,
                            "DependencyLinked",
                            &actor_id,
                            serde_json::json!({ "upstream_task_id": upstream_task_id }),
                        );
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
                        st.submissions.insert(
                            sub_id,
                            StoredSubmission {
                                submission_id: sub_id,
                                project_id: project_id.clone(),
                                epoch_id,
                                task_id: "_def_overlay".into(),
                                actor_id: actor_id.clone(),
                                kind: SubmissionKind::DefOverlay,
                                state: SubmissionState::Stage0SpecOk,
                                compile_ms: None,
                                reject_detail: None,
                            },
                        );
                        st.jobs.insert(
                            job_id,
                            StoredJob {
                                job: VerificationJob {
                                    job_id,
                                    submission_id: sub_id,
                                    project_id: project_id.clone(),
                                    epoch_id,
                                    task_id: "_def_overlay".into(),
                                    actor_id: actor_id.clone(),
                                    kind: SubmissionKind::DefOverlay,
                                    bundle_id: epoch.bundle_id.clone(),
                                    spec_module: epoch.spec_module.clone(),
                                    spec_decl: epoch.spec_decl_name.clone(),
                                    spec_statement: String::new(),
                                    target_inline: false,
                                    candidate_module: mod_name.clone(),
                                    candidate_decl: String::new(),
                                    blob_sha256,
                                    curated_imports: vec![],
                                },
                                status: "PENDING".into(),
                                claimed_by: None,
                            },
                        );
                        overlay_job_ids.push(job_id);
                        st.emit_event(
                            &project_id,
                            epoch_id as i32,
                            "overlay",
                            &mod_name,
                            "DefOverlayQueued",
                            &actor_id,
                            serde_json::json!({ "job_id": job_id }),
                        );
                    }
                    GraphIrOp::SetDispatchPolicy { config } => {
                        if let Some(ep) = st.epochs.get_mut(&(project_id.clone(), epoch_id)) {
                            ep.dispatch_policy = config.clone();
                        }
                        st.emit_event(
                            &project_id,
                            epoch_id as i32,
                            "epoch",
                            &epoch_id.to_string(),
                            "DispatchPolicyUpdated",
                            &actor_id,
                            serde_json::to_value(config)?,
                        );
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
                        let tkey = (project_id.clone(), epoch_id, task_id.clone());
                        let task = st.tasks.get_mut(&tkey).ok_or_else(|| {
                            StoreError::NotFound(format!("task {task_id} not found"))
                        })?;
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
                            task.credit_bounty = (*v).max(1);
                        }
                        let (ev_prio, ev_cap, ev_tier, ev_bounty) = (
                            task.priority,
                            task.racer_cap_k,
                            task.min_tier,
                            task.effective_bounty(),
                        );
                        st.emit_event(
                            &project_id,
                            epoch_id as i32,
                            "task",
                            task_id,
                            "NodeBudgetPatched",
                            &actor_id,
                            serde_json::json!({ "priority": ev_prio, "racer_cap_k": ev_cap, "min_tier": ev_tier, "credit_bounty": ev_bounty }),
                        );
                    }
                    GraphIrOp::PatchActorHint {
                        actor_id: target_actor,
                        tier,
                        skill_tags,
                        enabled,
                    } => {
                        if !st.actors.contains_key(target_actor) {
                            return Err(StoreError::NotFound(format!(
                                "actor {target_actor} not found"
                            )));
                        }
                        if let Some(en) = enabled {
                            if *en {
                                st.project_actor_blocks
                                    .remove(&(project_id.clone(), target_actor.clone()));
                            } else {
                                st.project_actor_blocks
                                    .insert((project_id.clone(), target_actor.clone()));
                            }
                        }
                        let actor = st.actors.get_mut(target_actor).unwrap();
                        if let Some(t) = tier {
                            actor.profile.tier = (*t).clamp(1, 4);
                        }
                        if let Some(tags) = skill_tags {
                            actor.profile.skill_tags = tags.clone();
                        }
                        let ev_tier = actor.profile.tier;
                        let project_enabled = !st
                            .project_actor_blocks
                            .contains(&(project_id.clone(), target_actor.clone()));
                        st.emit_event(
                            &project_id,
                            epoch_id as i32,
                            "actor",
                            target_actor,
                            "ActorHintPatched",
                            &actor_id,
                            serde_json::json!({ "tier": ev_tier, "project_enabled": project_enabled }),
                        );
                    }
                }
            }

            let updated_policy = st
                .epochs
                .get(&(project_id, epoch_id))
                .map(|e| e.dispatch_policy.clone())
                .unwrap_or_default();

            if !req.dry_run {
                *st_guard = st;
            }

            Ok(ApplyIrResponse {
                applied_ops: req.ops.len(),
                dry_run: req.dry_run,
                created_routes,
                spawned_tasks,
                unlocked_tasks,
                overlay_job_ids,
                dispatch_policy: updated_policy,
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
            let mut st = self.inner.write().unwrap();
            let now = Utc::now();
            let actor = st
                .actors
                .get(&actor_id)
                .ok_or_else(|| StoreError::NotFound(format!("actor {actor_id} not registered")))?
                .clone();
            if !actor.profile.enabled {
                return Err(StoreError::Forbidden(format!(
                    "actor {actor_id} is disabled"
                )));
            }
            if st
                .project_actor_blocks
                .contains(&(project_id.clone(), actor_id.clone()))
            {
                return Err(StoreError::Forbidden(format!(
                    "actor {actor_id} is blocked on project {project_id}"
                )));
            }
            let Some(epoch) = st.active_epoch_for_project(&project_id) else {
                return Ok(None);
            };

            // Expire stale leases and record timeout telemetry.
            let mut expired_tasks: Vec<String> = Vec::new();
            for l in st.leases.values_mut() {
                if l.project_id == project_id
                    && l.epoch_id == epoch.epoch_id
                    && l.status == "ACTIVE"
                    && l.info.expires_at < now
                {
                    l.status = "EXPIRED".into();
                    expired_tasks.push(l.task_id.clone());
                }
            }
            for tid in expired_tasks {
                let still_active = st.leases.values().any(|l| {
                    l.project_id == project_id
                        && l.epoch_id == epoch.epoch_id
                        && l.task_id == tid
                        && l.status == "ACTIVE"
                });
                if let Some(t) = st
                    .tasks
                    .get_mut(&(project_id.clone(), epoch.epoch_id, tid))
                {
                    t.telemetry.timeout_count += 1;
                    if !still_active && t.state == TaskState::Racing {
                        t.state = TaskState::StalledSoft;
                    }
                }
            }

            let active_for_actor = st
                .leases
                .values()
                .filter(|l| {
                    l.project_id == project_id
                        && l.epoch_id == epoch.epoch_id
                        && l.info.actor_id == actor_id
                        && l.status == "ACTIVE"
                })
                .count();
            if active_for_actor >= max_active_leases_per_actor.max(1) {
                return Err(StoreError::Conflict(format!(
                    "actor {actor_id} already holds the maximum number of active leases ({active_for_actor}) in project {project_id}"
                )));
            }

            let mut best: Option<(TaskNodeView, f64, f64)> = None;
            let tasks: Vec<TaskNodeView> = st
                .tasks
                .values()
                .filter(|t| t.project_id == project_id && t.epoch_id == epoch.epoch_id)
                .cloned()
                .collect();

            for task in tasks {
                if !task.is_dispatchable() || actor.profile.tier < task.min_tier {
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

                let active_clusters: Vec<String> = st
                    .leases
                    .values()
                    .filter(|l| {
                        l.project_id == project_id
                            && l.epoch_id == epoch.epoch_id
                            && l.task_id == task.task_id
                            && l.status == "ACTIVE"
                    })
                    .map(|l| l.lineage_cluster.clone())
                    .collect();

                if active_clusters.len() >= task.racer_cap_k as usize {
                    continue;
                }

                let decision = score_task_for_actor(
                    &epoch.dispatch_policy,
                    &actor.profile,
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
                return Ok(None);
            };

            let ttl = chosen.lease_ttl_secs.unwrap_or(default_ttl_secs).max(30);
            let lease_id = Uuid::now_v7();
            let expires_at = now + Duration::seconds(ttl);
            let info = LeaseInfo {
                lease_id,
                actor_id: actor_id.clone(),
                issued_at: now,
                expires_at,
                propensity_score: propensity,
            };
            st.leases.insert(
                lease_id,
                StoredLease {
                    info: info.clone(),
                    project_id: project_id.clone(),
                    epoch_id: epoch.epoch_id,
                    task_id: chosen.task_id.clone(),
                    lineage_cluster: actor.profile.lineage_cluster.clone(),
                    status: "ACTIVE".into(),
                },
            );

            if let Some(t) = st.tasks.get_mut(&(
                project_id.clone(),
                epoch.epoch_id,
                chosen.task_id.clone(),
            )) {
                t.state = t.state.transition(TaskEvent::AcquireLease)?;
            }

            let graveyard_constraints: Vec<String> = st
                .graveyard
                .iter()
                .filter(|g| {
                    g.project_id == project_id
                        && g.epoch_id == epoch.epoch_id
                        && g.task_id == chosen.task_id
                })
                .map(|g| format!("[{}] {}", g.failure_kind, g.summary))
                .collect();

            let mut import_lines = String::new();
            for imp in &chosen.curated_imports {
                import_lines.push_str(&format!("import {imp}\n"));
            }
            if import_lines.is_empty() {
                import_lines.push_str(&format!("import {}\n", epoch.spec_module));
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

            st.emit_event(
                &project_id,
                epoch.epoch_id as i32,
                "lease",
                &lease_id.to_string(),
                "LeaseClaimed",
                &actor_id,
                serde_json::json!({ "task_id": chosen.task_id, "propensity": propensity }),
            );

                let bounty = chosen.effective_bounty();
                Ok(Some(TaskPacket {
                schema_version: 2,
                project_id: project_id.clone(),
                epoch_id: epoch.epoch_id,
                task_id: chosen.task_id.clone(),
                route_id: chosen.route_id.clone(),
                task_role: chosen.task_role,
                module_hint: chosen.module_hint.clone(),
                bundle_id: epoch.bundle_id.clone(),
                spec_module: epoch.spec_module.clone(),
                spec_sha256: epoch.spec_sha256.clone(),
                target_file: format!("Tasks/{}.lean", chosen.task_id),
                transitive_slice: slice,
                curated_imports: chosen.curated_imports,
                graveyard_constraints,
                resource_limits: Default::default(),
                credit_bounty: bounty,
                lease: Some(info),
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
            let mut st = self.inner.write().unwrap();
            let now = Utc::now();
            let (project_id, epoch_id, expires_at, status) = {
                let lease = st
                    .leases
                    .get_mut(&lease_id)
                    .ok_or_else(|| StoreError::NotFound("lease not found".into()))?;
                if lease.info.actor_id != actor_id {
                    return Err(StoreError::Conflict("lease owned by another actor".into()));
                }
                if lease.status != "ACTIVE" {
                    return Err(StoreError::Conflict(format!(
                        "lease is {}, cannot heartbeat",
                        lease.status
                    )));
                }
                lease.info.expires_at = now + Duration::seconds(extend_secs.max(30));
                (
                    lease.project_id.clone(),
                    lease.epoch_id,
                    lease.info.expires_at,
                    lease.status.clone(),
                )
            };
            st.emit_event(
                &project_id,
                epoch_id as i32,
                "lease",
                &lease_id.to_string(),
                "LeaseHeartbeat",
                &actor_id,
                serde_json::json!({ "expires_at": expires_at }),
            );
            Ok(HeartbeatResponse {
                lease_id,
                expires_at,
                status,
            })
        })
    }

    fn create_submission(&self, input: NewSubmissionInput) -> StoreFuture<'_, SubmitResponse> {
        Box::pin(async move {
            let mut st = self.inner.write().unwrap();
            if st
                .project_actor_blocks
                .contains(&(input.project_id.clone(), input.actor_id.clone()))
            {
                return Err(StoreError::Forbidden(format!(
                    "actor {} is blocked on project {}",
                    input.actor_id, input.project_id
                )));
            }
            let lease = st
                .leases
                .get(&input.req.lease_id)
                .ok_or_else(|| StoreError::NotFound("lease not found".into()))?
                .clone();
            if lease.info.actor_id != input.actor_id {
                return Err(StoreError::Conflict("lease owned by another actor".into()));
            }
            if lease.status != "ACTIVE" {
                return Err(StoreError::Conflict(format!("lease is {}", lease.status)));
            }
            if lease.project_id != input.project_id
                || lease.epoch_id != input.req.epoch_id
                || lease.task_id != input.req.task_id
            {
                return Err(StoreError::Conflict(
                    "lease does not match (project_id, epoch_id, task_id)".into(),
                ));
            }
            if input.req.kind == SubmissionKind::DefOverlay {
                return Err(StoreError::BadRequest(
                    "DefOverlay must be published via Orchestrator/Blueprinter IR".into(),
                ));
            }

            let epoch = st
                .epochs
                .get(&(input.project_id.clone(), input.req.epoch_id))
                .ok_or_else(|| StoreError::NotFound("epoch not found".into()))?
                .clone();
            let task = st
                .tasks
                .get(&(
                    input.project_id.clone(),
                    input.req.epoch_id,
                    input.req.task_id.clone(),
                ))
                .ok_or_else(|| StoreError::NotFound("task not found".into()))?
                .clone();

            let state = if epoch.state != EpochState::Active {
                SubmissionState::StaleEpoch
            } else if input.req.spec_sha256 != epoch.spec_sha256 {
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

            st.submissions.insert(
                input.submission_id,
                StoredSubmission {
                    submission_id: input.submission_id,
                    project_id: input.project_id.clone(),
                    epoch_id: input.req.epoch_id,
                    task_id: input.req.task_id.clone(),
                    actor_id: input.actor_id.clone(),
                    kind: input.req.kind,
                    state,
                    compile_ms: None,
                    reject_detail,
                },
            );

            if state == SubmissionState::Stage0SpecOk {
                let job_id = Uuid::now_v7();
                let cand_mod = overlay_module_name(
                    input.req.kind,
                    &input.req.task_id,
                    input.submission_id,
                );
                st.jobs.insert(
                    job_id,
                    StoredJob {
                        job: VerificationJob {
                            job_id,
                            submission_id: input.submission_id,
                            project_id: input.project_id.clone(),
                            epoch_id: input.req.epoch_id,
                            task_id: input.req.task_id.clone(),
                            actor_id: input.actor_id.clone(),
                            kind: input.req.kind,
                            bundle_id: epoch.bundle_id.clone(),
                            spec_module: epoch.spec_module.clone(),
                            spec_decl: task.spec_decl_name.clone(),
                            spec_statement: task.spec_statement.clone(),
                            target_inline: task.target_inline,
                            candidate_module: cand_mod,
                            candidate_decl: task.candidate_decl.clone(),
                            blob_sha256: input.blob_sha256,
                            curated_imports: task.curated_imports.clone(),
                        },
                        status: "PENDING".into(),
                        claimed_by: None,
                    },
                );
            } else {
                st.record_graveyard(
                    &input.project_id,
                    input.req.epoch_id,
                    &input.req.task_id,
                    "stage0_rejected",
                    input.stage0_diagnostics.join("; "),
                );
            }

            st.emit_event(
                &input.project_id,
                input.req.epoch_id as i32,
                "submission",
                &input.submission_id.to_string(),
                "SubmissionReceived",
                &input.actor_id,
                serde_json::json!({ "task_id": input.req.task_id, "state": state }),
            );

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
            let st = self.inner.read().unwrap();
            Ok(st.submissions.get(&submission_id).map(|s| SubmissionStatusView {
                submission_id: s.submission_id,
                project_id: s.project_id.clone(),
                epoch_id: s.epoch_id,
                task_id: s.task_id.clone(),
                actor_id: s.actor_id.clone(),
                kind: s.kind,
                state: s.state,
                compile_ms: s.compile_ms,
                reject_detail: s.reject_detail.clone(),
            }))
        })
    }

    fn claim_next_verification_job(
        &self,
        verifier_id: &str,
    ) -> StoreFuture<'_, Option<VerificationJob>> {
        let verifier_id = verifier_id.to_string();
        Box::pin(async move {
            let mut st = self.inner.write().unwrap();
            let mut keys: Vec<Uuid> = st
                .jobs
                .values()
                .filter(|j| j.status == "PENDING")
                .map(|j| j.job.job_id)
                .collect();
            keys.sort();
            let Some(jid) = keys.first().copied() else {
                return Ok(None);
            };
            let job = st.jobs.get_mut(&jid).unwrap();
            job.status = "RUNNING".into();
            job.claimed_by = Some(verifier_id);
            Ok(Some(job.job.clone()))
        })
    }

    fn record_verification_outcome(
        &self,
        input: VerificationOutcomeInput,
    ) -> StoreFuture<'_, VerificationOutcomeResult> {
        Box::pin(async move {
            let mut st = self.inner.write().unwrap();
            let job = st
                .jobs
                .get(&input.job_id)
                .ok_or_else(|| StoreError::NotFound("verification job not found".into()))?
                .job
                .clone();

            if let Some(j) = st.jobs.get_mut(&input.job_id) {
                j.status = if input.verdict.ok {
                    "DONE".into()
                } else {
                    "FAILED".into()
                };
            }

            // Update actor objective telemetry
            let actor_lineage = st
                .actors
                .get(&job.actor_id)
                .map(|a| a.profile.lineage_cluster.clone())
                .unwrap_or_else(|| "unknown".into());
            if let Some(actor) = st.actors.get_mut(&job.actor_id) {
                actor.profile.total_submissions += 1;
                let y = if input.verdict.ok {
                    actor.profile.accepted_submissions += 1;
                    1.0
                } else {
                    actor.profile.rejected_submissions += 1;
                    0.0
                };
                actor.profile.ema_accept_rate =
                    0.8 * actor.profile.ema_accept_rate + 0.2 * y;
                actor.profile.avg_compile_ms = Some(match actor.profile.avg_compile_ms {
                    Some(prev) => (prev * 4 + input.verdict.compile_ms) / 5,
                    None => input.verdict.compile_ms,
                });
            }

            // DefOverlay path (no task node associated)
            if job.kind == SubmissionKind::DefOverlay {
                let sub_state = if input.verdict.ok {
                    SubmissionState::Merged
                } else if input.verdict.exit_code == 21 {
                    SubmissionState::RejectedResourceLimit
                } else {
                    SubmissionState::RejectedKernel
                };
                if let Some(s) = st.submissions.get_mut(&input.submission_id) {
                    s.state = sub_state;
                    s.compile_ms = Some(input.verdict.compile_ms);
                    if !input.verdict.ok {
                        s.reject_detail =
                            Some(serde_json::json!({ "diagnostics": input.verdict.diagnostics }));
                    }
                }
                let overlay_module = if input.verdict.ok {
                    st.overlays.insert(
                        (
                            job.project_id.clone(),
                            job.epoch_id,
                            job.candidate_module.clone(),
                        ),
                        VerifiedOverlayView {
                            project_id: job.project_id.clone(),
                            epoch_id: job.epoch_id,
                            module_name: job.candidate_module.clone(),
                            overlay_kind: OverlayKind::DefOverlay,
                            source_task_id: None,
                            source_actor_id: job.actor_id.clone(),
                            blob_sha256: job.blob_sha256.clone(),
                            imports: job.curated_imports.clone(),
                            compile_ms: input.verdict.compile_ms,
                            created_at: Utc::now(),
                        },
                    );
                    st.emit_event(
                        &job.project_id,
                        job.epoch_id as i32,
                        "overlay",
                        &job.candidate_module,
                        "DefOverlayVerified",
                        &job.actor_id,
                        serde_json::json!({ "compile_ms": input.verdict.compile_ms }),
                    );
                    Some(job.candidate_module.clone())
                } else {
                    None
                };
                return Ok(VerificationOutcomeResult {
                    submission_state: sub_state,
                    task_state: TaskState::Proved,
                    unlocked_tasks: vec![],
                    overlay_module,
                    project_proved: false,
                });
            }

            let tkey = (job.project_id.clone(), job.epoch_id, job.task_id.clone());
            let task_state_before = st
                .tasks
                .get(&tkey)
                .ok_or_else(|| StoreError::NotFound("task not found".into()))?
                .state;

            if let Some(t) = st.tasks.get_mut(&tkey) {
                t.telemetry.total_attempts += 1;
                t.telemetry.last_compile_ms = Some(input.verdict.compile_ms);
            }

            if !input.verdict.ok {
                let fail_state = match input.verdict.exit_code {
                    21 => SubmissionState::RejectedResourceLimit,
                    31 => SubmissionState::RejectedHygiene,
                    _ => SubmissionState::RejectedKernel,
                };
                if let Some(s) = st.submissions.get_mut(&input.submission_id) {
                    s.state = fail_state;
                    s.compile_ms = Some(input.verdict.compile_ms);
                    s.reject_detail =
                        Some(serde_json::json!({ "diagnostics": input.verdict.diagnostics }));
                }
                if let Some(t) = st.tasks.get_mut(&tkey) {
                    t.telemetry.failed_attempts += 1;
                    if input.verdict.exit_code == 21 {
                        t.telemetry.timeout_count += 1;
                    }
                    if !t.telemetry.failed_lineages.contains(&actor_lineage) {
                        t.telemetry.failed_lineages.push(actor_lineage);
                    }
                }
                let summary = input
                    .verdict
                    .diagnostics
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ");
                st.record_graveyard(
                    &job.project_id,
                    job.epoch_id,
                    &job.task_id,
                    "kernel_rejected",
                    summary,
                );
                st.emit_event(
                    &job.project_id,
                    job.epoch_id as i32,
                    "submission",
                    &input.submission_id.to_string(),
                    "VerificationRejected",
                    &job.actor_id,
                    serde_json::json!({ "task_id": job.task_id, "exit_code": input.verdict.exit_code }),
                );
                return Ok(VerificationOutcomeResult {
                    submission_state: fail_state,
                    task_state: task_state_before,
                    unlocked_tasks: vec![],
                    overlay_module: None,
                    project_proved: false,
                });
            }

            // First-Verified-Valid Wins!
            if task_state_before.is_terminal() {
                if let Some(s) = st.submissions.get_mut(&input.submission_id) {
                    s.state = SubmissionState::RejectedRaceLost;
                    s.compile_ms = Some(input.verdict.compile_ms);
                }
                st.emit_event(
                    &job.project_id,
                    job.epoch_id as i32,
                    "submission",
                    &input.submission_id.to_string(),
                    "RaceLost",
                    &job.actor_id,
                    serde_json::json!({ "task_id": job.task_id }),
                );
                return Ok(VerificationOutcomeResult {
                    submission_state: SubmissionState::RejectedRaceLost,
                    task_state: task_state_before,
                    unlocked_tasks: vec![],
                    overlay_module: None,
                    project_proved: false,
                });
            }

            if let Some(s) = st.submissions.get_mut(&input.submission_id) {
                s.state = SubmissionState::Merged;
                s.compile_ms = Some(input.verdict.compile_ms);
            }
            for s in st.submissions.values_mut() {
                if s.submission_id != input.submission_id
                    && s.project_id == job.project_id
                    && s.epoch_id == job.epoch_id
                    && s.task_id == job.task_id
                    && !s.state.is_terminal()
                {
                    s.state = SubmissionState::RejectedRaceLost;
                }
            }

            let new_task_state = task_state_before.transition(TaskEvent::AcceptProof)?;
            let (task_route_id, award_points) = {
                let t = st.tasks.get_mut(&tkey).unwrap();
                t.state = new_task_state;
                (t.route_id.clone(), t.effective_bounty() as i64)
            };
            st.expire_task_leases(&job.project_id, job.epoch_id, &job.task_id, "COMPLETED");

            if matches!(job.kind, SubmissionKind::Proof | SubmissionKind::Refutation)
                && award_points > 0
            {
                let reason = match job.kind {
                    SubmissionKind::Refutation => "refutation_accepted",
                    _ => "proof_accepted",
                };
                st.credit_ledger.push(CreditLedgerEntryView {
                    entry_id: Uuid::now_v7(),
                    project_id: job.project_id.clone(),
                    epoch_id: job.epoch_id,
                    task_id: job.task_id.clone(),
                    submission_id: Some(input.submission_id),
                    actor_id: job.actor_id.clone(),
                    points: award_points,
                    reason: reason.to_string(),
                    created_at: Utc::now(),
                });
                if let Some(a) = st.actors.get_mut(&job.actor_id) {
                    a.profile.credit_points += award_points;
                }
                st.emit_event(
                    &job.project_id,
                    job.epoch_id as i32,
                    "actor",
                    &job.actor_id,
                    "CreditAwarded",
                    &job.actor_id,
                    serde_json::json!({
                        "task_id": job.task_id,
                        "points": award_points,
                        "reason": reason,
                    }),
                );
            }

            let mut unlocked_tasks = Vec::new();
            let mut overlay_module = None;
            let mut project_proved = false;

            match job.kind {
                SubmissionKind::Proof => {
                    st.overlays.insert(
                        (
                            job.project_id.clone(),
                            job.epoch_id,
                            job.candidate_module.clone(),
                        ),
                        VerifiedOverlayView {
                            project_id: job.project_id.clone(),
                            epoch_id: job.epoch_id,
                            module_name: job.candidate_module.clone(),
                            overlay_kind: OverlayKind::ProvedTask,
                            source_task_id: Some(job.task_id.clone()),
                            source_actor_id: job.actor_id.clone(),
                            blob_sha256: job.blob_sha256.clone(),
                            imports: job.curated_imports.clone(),
                            compile_ms: input.verdict.compile_ms,
                            created_at: Utc::now(),
                        },
                    );
                    overlay_module = Some(job.candidate_module.clone());

                    // Inject overlay into downstream DependsOn tasks & auto-unlock if ready.
                    let downstream_ids: Vec<String> = st
                        .edges
                        .iter()
                        .filter(|(p, e, edge)| {
                            p == &job.project_id
                                && *e == job.epoch_id
                                && edge.edge_kind == EdgeKind::DependsOn
                                && edge.from_task_id == job.task_id
                        })
                        .map(|(_, _, edge)| edge.to_task_id.clone())
                        .collect();

                    for down_id in downstream_ids {
                        let ready = st.all_dependencies_proved(&job.project_id, job.epoch_id, &down_id);
                        if let Some(down) = st.tasks.get_mut(&(
                            job.project_id.clone(),
                            job.epoch_id,
                            down_id.clone(),
                        )) {
                            if !down.curated_imports.contains(&job.candidate_module) {
                                down.curated_imports.push(job.candidate_module.clone());
                            }
                            if ready && down.state == TaskState::Blocked {
                                down.state = down.state.transition(TaskEvent::UnblockDependencies)?;
                                unlocked_tasks.push(down_id.clone());
                            }
                        }
                    }

                    for u in &unlocked_tasks {
                        st.emit_event(
                            &job.project_id,
                            job.epoch_id as i32,
                            "task",
                            u,
                            "TaskUnlocked",
                            "system",
                            serde_json::json!({ "by_upstream": job.task_id }),
                        );
                    }

                    // Check if Root task (or a Merge closing root_task_id) completed.
                    let root_id = st
                        .epochs
                        .get(&(job.project_id.clone(), job.epoch_id))
                        .map(|e| e.root_task_id.clone())
                        .unwrap_or_default();
                    let closes_root = job.task_id == root_id
                        || st
                            .tasks
                            .get(&tkey)
                            .is_some_and(|t| {
                                t.task_role == TaskRole::Merge
                                    && t.spec_decl_name
                                        == st
                                            .epochs
                                            .get(&(job.project_id.clone(), job.epoch_id))
                                            .map(|e| e.spec_decl_name.as_str())
                                            .unwrap_or("")
                                    && !t.target_inline
                            });

                    if closes_root {
                        project_proved = true;
                        if job.task_id != root_id
                            && let Some(root_t) = st.tasks.get_mut(&(
                                job.project_id.clone(),
                                job.epoch_id,
                                root_id.clone(),
                            ))
                            && !root_t.state.is_terminal()
                        {
                            root_t.state = TaskState::Proved;
                            st.expire_task_leases(&job.project_id, job.epoch_id, &root_id, "COMPLETED");
                        }
                        if let Some(proj) = st.projects.get_mut(&job.project_id) {
                            proj.summary.status = ProjectStatus::Proved;
                        }
                        if let Some(won_rid) = &task_route_id {
                            for r in st.routes.values_mut() {
                                if r.project_id == job.project_id && r.epoch_id == job.epoch_id {
                                    if &r.route_id == won_rid {
                                        r.state = RouteState::Won;
                                    } else if r.state == RouteState::Active {
                                        r.state = RouteState::Pruned;
                                    }
                                }
                            }
                            let mut pruned_ids = Vec::new();
                            for t in st.tasks.values_mut() {
                                if t.project_id == job.project_id
                                    && t.epoch_id == job.epoch_id
                                    && !t.state.is_terminal()
                                    && t.route_id.as_deref() != Some(won_rid.as_str())
                                {
                                    t.state = TaskState::Pruned;
                                    pruned_ids.push(t.task_id.clone());
                                }
                            }
                            for pid in pruned_ids {
                                st.expire_task_leases(&job.project_id, job.epoch_id, &pid, "PRUNED");
                            }
                        }
                        st.emit_event(
                            &job.project_id,
                            job.epoch_id as i32,
                            "project",
                            &job.project_id,
                            "RootProved",
                            &job.actor_id,
                            serde_json::json!({ "winning_task": job.task_id, "winning_route": task_route_id }),
                        );
                    }
                }
                SubmissionKind::Refutation => {
                    let targets: Vec<String> = st
                        .edges
                        .iter()
                        .filter(|(p, e, edge)| {
                            p == &job.project_id
                                && *e == job.epoch_id
                                && edge.edge_kind == EdgeKind::Refutes
                                && edge.from_task_id == job.task_id
                        })
                        .map(|(_, _, edge)| edge.to_task_id.clone())
                        .collect();

                    for target_id in targets {
                        let route_to_refute = if let Some(target) = st.tasks.get_mut(&(
                            job.project_id.clone(),
                            job.epoch_id,
                            target_id.clone(),
                        )) {
                            if !target.state.is_terminal() {
                                target.state = target.state.transition(TaskEvent::HardRefuted)?;
                            }
                            target.route_id.clone()
                        } else {
                            None
                        };
                        st.expire_task_leases(
                            &job.project_id,
                            job.epoch_id,
                            &target_id,
                            "REFUTED",
                        );
                        st.record_graveyard(
                            &job.project_id,
                            job.epoch_id,
                            &target_id,
                            "hard_refutation",
                            format!("Refuted by {} (actor={})", job.task_id, job.actor_id),
                        );

                        if let Some(rid) = route_to_refute {
                            if let Some(r) = st.routes.get_mut(&(
                                job.project_id.clone(),
                                job.epoch_id,
                                rid.clone(),
                            )) {
                                r.state = RouteState::Refuted;
                            }
                            let mut pruned = Vec::new();
                            for t in st.tasks.values_mut() {
                                if t.project_id == job.project_id
                                    && t.epoch_id == job.epoch_id
                                    && t.route_id.as_deref() == Some(rid.as_str())
                                    && !t.state.is_terminal()
                                {
                                    t.state = TaskState::Pruned;
                                    pruned.push(t.task_id.clone());
                                }
                            }
                            for pid in pruned {
                                st.expire_task_leases(&job.project_id, job.epoch_id, &pid, "PRUNED");
                            }
                        }

                        st.emit_event(
                            &job.project_id,
                            job.epoch_id as i32,
                            "task",
                            &target_id,
                            "TaskRefuted",
                            &job.actor_id,
                            serde_json::json!({ "refutation_task": job.task_id }),
                        );
                    }
                }
                SubmissionKind::DefOverlay => {}
            }

            st.emit_event(
                &job.project_id,
                job.epoch_id as i32,
                "task",
                &job.task_id,
                "TaskVerified",
                &job.actor_id,
                serde_json::json!({ "kind": job.kind, "compile_ms": input.verdict.compile_ms }),
            );

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
            let st = self.inner.read().unwrap();
            let Some(epoch) = st.epochs.get(&(project_id, epoch_id)) else {
                return Ok(None);
            };
            Ok(Some(st.build_epoch_status(epoch)))
        })
    }

    fn project_graph(
        &self,
        project_id: &str,
        epoch_id: u32,
    ) -> StoreFuture<'_, Option<ProjectGraphView>> {
        let project_id = project_id.to_string();
        Box::pin(async move {
            let st = self.inner.read().unwrap();
            let Some(proj) = st.projects.get(&project_id) else {
                return Ok(None);
            };
            let Some(epoch) = st.epochs.get(&(project_id.clone(), epoch_id)) else {
                return Ok(None);
            };
            let lean_toolchain = st
                .bundles
                .get(&epoch.bundle_id)
                .map(|b| b.lean_toolchain.clone())
                .unwrap_or_else(|| "leanprover/lean4:v4.36.0".into());

            let mut routes: Vec<RouteView> = st
                .routes
                .values()
                .filter(|r| r.project_id == project_id && r.epoch_id == epoch_id)
                .cloned()
                .collect();
            routes.sort_by(|a, b| a.route_id.cmp(&b.route_id));

            let mut tasks: Vec<TaskNodeView> = st
                .tasks
                .values()
                .filter(|t| t.project_id == project_id && t.epoch_id == epoch_id)
                .cloned()
                .collect();
            tasks.sort_by(|a, b| a.task_id.cmp(&b.task_id));

            let edges: Vec<TaskEdgeView> = st
                .edges
                .iter()
                .filter(|(p, e, _)| p == &project_id && *e == epoch_id)
                .map(|(_, _, edge)| edge.clone())
                .collect();

            let mut overlays: Vec<VerifiedOverlayView> = st
                .overlays
                .values()
                .filter(|o| o.project_id == project_id && o.epoch_id == epoch_id)
                .cloned()
                .collect();
            overlays.sort_by(|a, b| a.created_at.cmp(&b.created_at));

            let active_leases: Vec<LeaseInfo> = st
                .leases
                .values()
                .filter(|l| {
                    l.project_id == project_id && l.epoch_id == epoch_id && l.status == "ACTIVE"
                })
                .map(|l| l.info.clone())
                .collect();

            let graveyard_count = st
                .graveyard
                .iter()
                .filter(|g| g.project_id == project_id && g.epoch_id == epoch_id)
                .count();

            Ok(Some(ProjectGraphView {
                project_id,
                project_status: proj.summary.status,
                epoch_id,
                epoch_state: epoch.state,
                bundle_id: epoch.bundle_id.clone(),
                lean_toolchain,
                spec_module: epoch.spec_module.clone(),
                spec_decl_name: epoch.spec_decl_name.clone(),
                root_task_id: epoch.root_task_id.clone(),
                dispatch_policy: epoch.dispatch_policy.clone(),
                routes,
                tasks,
                edges,
                overlays,
                active_leases,
                graveyard_count,
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
            let graph = self.project_graph(&project_id, epoch_id).await?;
            let Some(g) = graph else {
                return Ok(None);
            };
            let st = self.inner.read().unwrap();
            let mut task_counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut open_tasks = 0usize;
            let mut blocked_tasks = 0usize;
            for t in &g.tasks {
                let key = serde_json::to_value(t.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("{:?}", t.state));
                *task_counts.entry(key).or_insert(0) += 1;
                if t.is_dispatchable() {
                    open_tasks += 1;
                }
                if t.state == TaskState::Blocked {
                    blocked_tasks += 1;
                }
            }
            let actors: Vec<ActorProfile> = st
                .actors
                .values()
                .filter(|a| {
                    a.profile.project_scopes.is_empty()
                        || a.profile.project_scopes.iter().any(|p| p == &project_id)
                })
                .map(|a| {
                    let mut p = a.profile.clone();
                    p.enabled = p.enabled
                        && !st
                            .project_actor_blocks
                            .contains(&(project_id.clone(), p.actor_id.clone()));
                    p
                })
                .collect();
            let recent_graveyard: Vec<GraveyardEntryView> = st
                .graveyard
                .iter()
                .filter(|gr| gr.project_id == project_id && gr.epoch_id == epoch_id)
                .rev()
                .take(15)
                .cloned()
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
            let st = self.inner.read().unwrap();
            let out: Vec<EventRecordView> = match since_seq {
                Some(after_seq) => st
                    .events
                    .iter()
                    .filter(|(seq, e)| {
                        *seq > after_seq && e.project_id == project_id && e.epoch_id == epoch_id as i32
                    })
                    .take(limit)
                    .map(|(seq, e)| EventRecordView {
                        event_seq: *seq,
                        event_id: e.event_id,
                        occurred_at: e.occurred_at,
                        project_id: e.project_id.clone(),
                        epoch_id: e.epoch_id,
                        aggregate_type: e.aggregate_type.clone(),
                        aggregate_id: e.aggregate_id.clone(),
                        event_type: e.event_type.clone(),
                        actor_id: e.actor_id.clone(),
                        payload: e.payload.clone(),
                    })
                    .collect(),
                None => {
                    let mut recent: Vec<EventRecordView> = st
                        .events
                        .iter()
                        .filter(|(_, e)| e.project_id == project_id && e.epoch_id == epoch_id as i32)
                        .rev()
                        .take(limit)
                        .map(|(seq, e)| EventRecordView {
                            event_seq: *seq,
                            event_id: e.event_id,
                            occurred_at: e.occurred_at,
                            project_id: e.project_id.clone(),
                            epoch_id: e.epoch_id,
                            aggregate_type: e.aggregate_type.clone(),
                            aggregate_id: e.aggregate_id.clone(),
                            event_type: e.event_type.clone(),
                            actor_id: e.actor_id.clone(),
                            payload: e.payload.clone(),
                        })
                        .collect();
                    recent.reverse();
                    recent
                }
            };
            Ok(out)
        })
    }

    fn get_overlay_blob(
        &self,
        project_id: &str,
        epoch_id: u32,
        module_name: &str,
    ) -> StoreFuture<'_, Option<String>> {
        let key = (
            project_id.to_string(),
            epoch_id,
            module_name.to_string(),
        );
        Box::pin(async move {
            let st = self.inner.read().unwrap();
            if let Some(src) = st.overlay_sources.get(&key) {
                return Ok(Some(src.clone()));
            }
            Ok(st.overlays.get(&key).map(|o| o.blob_sha256.clone()))
        })
    }

    fn leaderboard(
        &self,
        project_id: Option<&str>,
        limit: usize,
    ) -> StoreFuture<'_, Vec<LeaderboardEntryView>> {
        let project_id = project_id.map(str::to_string);
        let limit = limit.clamp(1, 200);
        Box::pin(async move {
            let st = self.inner.read().unwrap();
            let mut agg: HashMap<&str, (i64, u64, u64)> = HashMap::new();
            for entry in &st.credit_ledger {
                if let Some(ref pid) = project_id
                    && &entry.project_id != pid
                {
                    continue;
                }
                let rec = agg.entry(entry.actor_id.as_str()).or_insert((0, 0, 0));
                rec.0 += entry.points;
                if entry.reason == "refutation_accepted" {
                    rec.2 += 1;
                } else {
                    rec.1 += 1;
                }
            }
            let mut rows: Vec<LeaderboardEntryView> = agg
                .into_iter()
                .map(|(aid, (pts, proofs, refuts))| {
                    let profile = st.actors.get(aid).map(|a| &a.profile);
                    LeaderboardEntryView {
                        actor_id: aid.to_string(),
                        display_name: profile.and_then(|p| p.display_name.clone()),
                        lineage_cluster: profile
                            .map(|p| p.lineage_cluster.clone())
                            .unwrap_or_else(|| "unknown".into()),
                        tier: profile.map(|p| p.tier).unwrap_or(1),
                        credit_points: pts,
                        verified_tasks: proofs,
                        verified_refutations: refuts,
                    }
                })
                .collect();
            rows.sort_by(|a, b| {
                b.credit_points
                    .cmp(&a.credit_points)
                    .then_with(|| b.verified_tasks.cmp(&a.verified_tasks))
                    .then_with(|| a.actor_id.cmp(&b.actor_id))
            });
            rows.truncate(limit);
            Ok(rows)
        })
    }

    fn live_metrics(&self) -> StoreFuture<'_, HubLiveMetrics> {
        Box::pin(async move {
            let st = self.inner.read().unwrap();
            let now = Utc::now();
            let mut active_actors = HashSet::new();
            let mut active_leases = 0usize;
            for lease in st.leases.values() {
                if lease.status == "ACTIVE" && lease.info.expires_at > now {
                    active_leases += 1;
                    active_actors.insert(lease.info.actor_id.as_str());
                }
            }
            let registered_agents = st.actors.values().filter(|a| a.profile.enabled).count();
            Ok(HubLiveMetrics {
                online_agents: active_actors.len(),
                active_leases,
                registered_agents,
            })
        })
    }

    fn health_check(&self) -> StoreFuture<'_, ()> {
        Box::pin(async move { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalFsBlobStore;
    use quod_core::{AuditVerdict, DispatchAlgo, SubmitRequest, generate_keypair_hex};

    async fn setup_project_and_actors() -> (MemoryStore, tempfile::TempDir, LocalFsBlobStore) {
        let store = MemoryStore::new();
        let tmp = tempfile::tempdir().unwrap();
        let blobs = LocalFsBlobStore::new(tmp.path());

        let (_, pk_bp) = generate_keypair_hex();
        let (_, pk_aud) = generate_keypair_hex();
        let (_, pk_orch) = generate_keypair_hex();
        let (_, pk_w1) = generate_keypair_hex();
        let (_, pk_w2) = generate_keypair_hex();

        store
            .register_actor(&RegisterActorRequest {
                actor_id: "bp-1".into(),
                pubkey_ed25519: pk_bp,
                roles: vec![Role::Blueprinter],
                project_scopes: vec![],
                lineage_cluster: "human".into(),
                initial_tier: Some(4),
                skill_tags: vec![],
                display_name: None,
                git_email: None,
            })
            .await
            .unwrap();
        store
            .register_actor(&RegisterActorRequest {
                actor_id: "aud-1".into(),
                pubkey_ed25519: pk_aud,
                roles: vec![Role::Auditor],
                project_scopes: vec![],
                lineage_cluster: "human".into(),
                initial_tier: Some(4),
                skill_tags: vec![],
                display_name: None,
                git_email: None,
            })
            .await
            .unwrap();
        store
            .register_actor(&RegisterActorRequest {
                actor_id: "orch-1".into(),
                pubkey_ed25519: pk_orch,
                roles: vec![Role::Orchestrator],
                project_scopes: vec![],
                lineage_cluster: "claude".into(),
                initial_tier: Some(4),
                skill_tags: vec![],
                display_name: None,
                git_email: None,
            })
            .await
            .unwrap();
        store
            .register_actor(&RegisterActorRequest {
                actor_id: "w1".into(),
                pubkey_ed25519: pk_w1,
                roles: vec![Role::Worker],
                project_scopes: vec![],
                lineage_cluster: "claude".into(),
                initial_tier: Some(3),
                skill_tags: vec![],
                display_name: Some("Worker One".into()),
                git_email: None,
            })
            .await
            .unwrap();
        store
            .register_actor(&RegisterActorRequest {
                actor_id: "w2".into(),
                pubkey_ed25519: pk_w2,
                roles: vec![Role::Worker],
                project_scopes: vec![],
                lineage_cluster: "gpt".into(),
                initial_tier: Some(2),
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
                toolchain_dir: "/opt/lean".into(),
                lean_paths: vec![],
                bundle_sha256: "b1".into(),
            })
            .await
            .unwrap();

        store
            .create_project(
                "bp-1",
                &CreateProjectRequest {
                    project_id: "smoke".into(),
                    title: "Smoke".into(),
                    description: "Smoke test".into(),
                    spec_namespace: "Quod.Epoch0".into(),
                    default_bundle_id: Some("lean-4.36.0".into()),
                },
            )
            .await
            .unwrap();

        let pending = store
            .create_epoch(
                "smoke",
                "bp-1",
                &CreateEpochRequest {
                    epoch_id: 1,
                    bundle_id: "lean-4.36.0".into(),
                    base_commit: "HEAD".into(),
                    spec_module: "Spec".into(),
                    spec_decl_name: "Quod.Epoch0.smoke_stmt".into(),
                    spec_statement: String::new(),
                    spec_sha256: "spec-sha".into(),
                    spec_code: Some("def smoke_stmt : Prop := True".into()),
                    root_task_id: "root".into(),
                    auto_activate: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(pending.state, EpochState::PendingAudit);

        let active = store
            .audit_epoch(
                "smoke",
                1,
                "aud-1",
                &AuditEpochRequest {
                    approve: true,
                    comment: "LGTM".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(active.state, EpochState::Active);

        (store, tmp, blobs)
    }

    #[tokio::test]
    async fn route_merge_unlock_cycle_check_and_refutation_pruning() {
        let (store, _tmp, blobs) = setup_project_and_actors().await;

        // Spawn Route A with two lemmas + blocked Merge task, plus Route B to be refuted.
        let ir = ApplyIrRequest {
            ops: vec![
                GraphIrOp::CreateRoute {
                    route_id: "route-a".into(),
                    title: "Two-step split".into(),
                    rationale: "Prove h1, h2 then merge".into(),
                },
                GraphIrOp::SpawnTask {
                    task_id: "lemma_a".into(),
                    route_id: Some("route-a".into()),
                    task_role: TaskRole::Lemma,
                    spec_decl_name: "lemma_a".into(),
                    spec_statement: "1 + 1 = 2".into(),
                    target_inline: true,
                    candidate_decl: "proof_lemma_a".into(),
                    curated_imports: vec![],
                    depends_on: vec![],
                    refutes_task_id: None,
                    module_hint: None,
                    priority: 5.0,
                    prior_p: 0.7,
                    expected_cost_c: 1.0,
                    racer_cap_k: 2,
                    min_tier: 1,
                    preferred_lineages: vec![],
                    credit_bounty: Some(120),
                },
                GraphIrOp::SpawnTask {
                    task_id: "merge_a".into(),
                    route_id: Some("route-a".into()),
                    task_role: TaskRole::Merge,
                    spec_decl_name: "Quod.Epoch0.smoke_stmt".into(),
                    spec_statement: String::new(),
                    target_inline: false,
                    candidate_decl: "candidate".into(),
                    curated_imports: vec![],
                    depends_on: vec!["lemma_a".into()],
                    refutes_task_id: None,
                    module_hint: None,
                    priority: 10.0,
                    prior_p: 0.8,
                    expected_cost_c: 1.0,
                    racer_cap_k: 2,
                    min_tier: 1,
                    preferred_lineages: vec![],
                    credit_bounty: None,
                },
                GraphIrOp::CreateRoute {
                    route_id: "route-b".into(),
                    title: "False conjecture route".into(),
                    rationale: "Probe claim".into(),
                },
                GraphIrOp::SpawnTask {
                    task_id: "false_q".into(),
                    route_id: Some("route-b".into()),
                    task_role: TaskRole::Lemma,
                    spec_decl_name: "false_q".into(),
                    spec_statement: "∀ n : Nat, n + 1 = n".into(),
                    target_inline: true,
                    candidate_decl: "proof_false_q".into(),
                    curated_imports: vec![],
                    depends_on: vec![],
                    refutes_task_id: None,
                    module_hint: None,
                    priority: 1.0,
                    prior_p: 0.2,
                    expected_cost_c: 1.0,
                    racer_cap_k: 2,
                    min_tier: 1,
                    preferred_lineages: vec![],
                    credit_bounty: None,
                },
                GraphIrOp::SpawnTask {
                    task_id: "refute_false_q".into(),
                    route_id: Some("route-b".into()),
                    task_role: TaskRole::Refutation,
                    spec_decl_name: "false_q".into(),
                    spec_statement: "∀ n : Nat, n + 1 = n".into(),
                    target_inline: true,
                    candidate_decl: "candidate".into(),
                    curated_imports: vec![],
                    depends_on: vec![],
                    refutes_task_id: Some("false_q".into()),
                    module_hint: None,
                    priority: 4.0,
                    prior_p: 0.5,
                    expected_cost_c: 1.0,
                    racer_cap_k: 2,
                    min_tier: 1,
                    preferred_lineages: vec![],
                    credit_bounty: Some(95),
                },
                GraphIrOp::SetDispatchPolicy {
                    config: quod_core::DispatchPolicyConfig {
                        algo: DispatchAlgo::PriorityQueue,
                        exploration_rate: 0.05,
                        allow_cross_route_parallel: true,
                    },
                },
            ],
            dry_run: false,
        };
        // Dry-run validates and reports without mutating routes, tasks, or events.
        let before_events = store.recent_events("smoke", 1, None, 100).await.unwrap();
        let mut dry_ir = ir.clone();
        dry_ir.dry_run = true;
        let dry_res = store.apply_ir("smoke", 1, "orch-1", &dry_ir, &blobs).await.unwrap();
        assert!(dry_res.dry_run);
        assert_eq!(dry_res.created_routes.len(), 2);
        let after_dry_events = store.recent_events("smoke", 1, None, 100).await.unwrap();
        assert_eq!(before_events.len(), after_dry_events.len());

        let ir_res = store.apply_ir("smoke", 1, "orch-1", &ir, &blobs).await.unwrap();
        assert!(!ir_res.dry_run);
        assert_eq!(ir_res.created_routes.len(), 2);

        let last_seq = before_events.last().map(|e| e.event_seq).unwrap_or(0);
        let delta_events = store.recent_events("smoke", 1, Some(last_seq), 100).await.unwrap();
        assert!(!delta_events.is_empty());
        assert!(delta_events.iter().all(|e| e.event_seq > last_seq));

        // Cycle detection rejects merge_a -> lemma_a
        let cyc = store
            .apply_ir(
                "smoke",
                1,
                "orch-1",
                &ApplyIrRequest {
                    ops: vec![GraphIrOp::LinkDependency {
                        upstream_task_id: "merge_a".into(),
                        downstream_task_id: "lemma_a".into(),
                    }],
                    dry_run: false,
                },
                &blobs,
            )
            .await;
        assert!(cyc.is_err());

        // Refute false_q -> route-b becomes REFUTED
        let pkt_ref = store
            .claim_lease(
                "smoke",
                "w2",
                &ClaimLeaseRequest {
                    preferred_task_id: Some("refute_false_q".into()),
                    preferred_route_id: None,
                },
                900,
                1,
            )
            .await
            .unwrap()
            .unwrap();
        let ref_code = "theorem candidate : ¬ (∀ n : Nat, n + 1 = n) := by intro h; have := h 0; omega";
        let ref_sha = blobs.put_bytes(ref_code.as_bytes()).unwrap();
        let sub_ref = store
            .create_submission(NewSubmissionInput {
                submission_id: Uuid::now_v7(),
                project_id: "smoke".into(),
                actor_id: "w2".into(),
                req: SubmitRequest {
                    epoch_id: 1,
                    task_id: "refute_false_q".into(),
                    lease_id: pkt_ref.lease.unwrap().lease_id,
                    kind: SubmissionKind::Refutation,
                    spec_sha256: "spec-sha".into(),
                    code_utf8: ref_code.into(),
                    idea_attr_id: None,
                },
                blob_sha256: ref_sha.clone(),
                received_at_ns: 1,
                stage0_passed: true,
                stage0_exit_code: 0,
                stage0_diagnostics: vec![],
            })
            .await
            .unwrap();
        let job_ref = store.claim_next_verification_job("v1").await.unwrap().unwrap();
        store
            .record_verification_outcome(VerificationOutcomeInput {
                job_id: job_ref.job_id,
                submission_id: sub_ref.submission_id,
                verdict: AuditVerdict {
                    ok: true,
                    exit_code: 0,
                    kind: SubmissionKind::Refutation,
                    code_sha256: ref_sha,
                    compile_ms: 50,
                    diagnostics: vec![],
                },
            })
            .await
            .unwrap();

        let g1 = store.project_graph("smoke", 1).await.unwrap().unwrap();
        let rb = g1.routes.iter().find(|r| r.route_id == "route-b").unwrap();
        assert_eq!(rb.state, RouteState::Refuted);
        let fq = g1.tasks.iter().find(|t| t.task_id == "false_q").unwrap();
        assert_eq!(fq.state, TaskState::RefutedHard);

        // Solve lemma_a -> merge_a auto-unlocks with overlay import!
        let pkt_a = store
            .claim_lease("smoke", "w1", &ClaimLeaseRequest::default(), 900, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pkt_a.task_id, "lemma_a");

        let a_sha = blobs.put_bytes(b"theorem proof_lemma_a : 1 + 1 = 2 := rfl").unwrap();
        let sub_a = store
            .create_submission(NewSubmissionInput {
                submission_id: Uuid::now_v7(),
                project_id: "smoke".into(),
                actor_id: "w1".into(),
                req: SubmitRequest {
                    epoch_id: 1,
                    task_id: "lemma_a".into(),
                    lease_id: pkt_a.lease.unwrap().lease_id,
                    kind: SubmissionKind::Proof,
                    spec_sha256: "spec-sha".into(),
                    code_utf8: "theorem proof_lemma_a : 1 + 1 = 2 := rfl".into(),
                    idea_attr_id: None,
                },
                blob_sha256: a_sha.clone(),
                received_at_ns: 2,
                stage0_passed: true,
                stage0_exit_code: 0,
                stage0_diagnostics: vec![],
            })
            .await
            .unwrap();
        let job_a = store.claim_next_verification_job("v1").await.unwrap().unwrap();
        let out_a = store
            .record_verification_outcome(VerificationOutcomeInput {
                job_id: job_a.job_id,
                submission_id: sub_a.submission_id,
                verdict: AuditVerdict {
                    ok: true,
                    exit_code: 0,
                    kind: SubmissionKind::Proof,
                    code_sha256: a_sha,
                    compile_ms: 40,
                    diagnostics: vec![],
                },
            })
            .await
            .unwrap();
        assert_eq!(out_a.unlocked_tasks, vec!["merge_a".to_string()]);

        // Claim unlocked merge_a and verify it imports lemma_a's overlay!
        let pkt_m = store
            .claim_lease("smoke", "w1", &ClaimLeaseRequest::default(), 900, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pkt_m.task_id, "merge_a");
        assert!(pkt_m
            .curated_imports
            .iter()
            .any(|m| m.starts_with("QuodOverlay_lemma_a_")));

        let lb = store.leaderboard(Some("smoke"), 10).await.unwrap();
        assert_eq!(lb[0].actor_id, "w1");
        assert_eq!(lb[0].credit_points, 120);
        assert_eq!(lb[0].verified_tasks, 1);
        assert_eq!(lb[1].actor_id, "w2");
        assert_eq!(lb[1].credit_points, 95);
        assert_eq!(lb[1].verified_refutations, 1);
    }

    #[tokio::test]
    async fn invites_open_join_sod_and_project_scoped_blocking() {
        let (store, _tmp, blobs) = setup_project_and_actors().await;

        // Second project to verify project-scoped blocking does not bleed globally
        store
            .create_project(
                "bp-1",
                &CreateProjectRequest {
                    project_id: "other".into(),
                    title: "Other".into(),
                    description: "".into(),
                    spec_namespace: "Quod.Other".into(),
                    default_bundle_id: Some("lean-4.36.0".into()),
                },
            )
            .await
            .unwrap();
        store
            .create_epoch(
                "other",
                "bp-1",
                &CreateEpochRequest {
                    epoch_id: 1,
                    bundle_id: "lean-4.36.0".into(),
                    base_commit: "HEAD".into(),
                    spec_module: "Spec".into(),
                    spec_decl_name: "Quod.Other.stmt".into(),
                    spec_statement: "True".into(),
                    spec_sha256: "s2".into(),
                    spec_code: None,
                    root_task_id: "root".into(),
                    auto_activate: false,
                },
            )
            .await
            .unwrap();

        // Separation of duties: bp-1 cannot audit their own epoch on `other`
        let self_audit = store
            .audit_epoch(
                "other",
                1,
                "bp-1",
                &AuditEpochRequest {
                    approve: true,
                    comment: "self".into(),
                },
            )
            .await;
        assert!(matches!(self_audit, Err(StoreError::Forbidden(_))));

        // aud-1 can approve `other`
        store
            .audit_epoch(
                "other",
                1,
                "aud-1",
                &AuditEpochRequest {
                    approve: true,
                    comment: "ok".into(),
                },
            )
            .await
            .unwrap();

        // Open join disabled -> rejects without invite; enabled -> creates tier-1 worker
        let (_, pk_new) = generate_keypair_hex();
        let closed = store
            .join_actor(
                &JoinActorRequest {
                    actor_id: "w-new".into(),
                    pubkey_ed25519: pk_new.clone(),
                    lineage_cluster: "qwen".into(),
                    invite_code: None,
                    skill_tags: vec![],
                    display_name: None,
                    git_email: None,
                },
                false,
            )
            .await;
        assert!(matches!(closed, Err(StoreError::Forbidden(_))));

        let joined = store
            .join_actor(
                &JoinActorRequest {
                    actor_id: "w-new".into(),
                    pubkey_ed25519: pk_new.clone(),
                    lineage_cluster: "qwen".into(),
                    invite_code: None,
                    skill_tags: vec![],
                    display_name: Some("New Worker".into()),
                    git_email: Some("new@users.noreply.github.com".into()),
                },
                true,
            )
            .await
            .unwrap();
        assert_eq!(joined.roles, vec![Role::Worker]);
        assert_eq!(joined.tier, 1);
        assert_eq!(joined.display_name.as_deref(), Some("New Worker"));

        // Single-use invite token -> redeems once, rejects second use & duplicate pubkey
        let inv = store
            .create_invite(
                "bp-1",
                &CreateInviteRequest {
                    roles: vec![Role::Orchestrator],
                    project_scopes: vec!["smoke".into()],
                    max_uses: 1,
                    expires_in_secs: 3600,
                    initial_tier: Some(2),
                    note: "orch".into(),
                },
            )
            .await
            .unwrap();
        let (_, pk_o2) = generate_keypair_hex();
        let (_, pk_o3) = generate_keypair_hex();
        let o2 = store
            .join_actor(
                &JoinActorRequest {
                    actor_id: "orch-2".into(),
                    pubkey_ed25519: pk_o2,
                    lineage_cluster: "claude".into(),
                    invite_code: Some(inv.invite_code.clone()),
                    skill_tags: vec![],
                    display_name: None,
                    git_email: None,
                },
                false,
            )
            .await
            .unwrap();
        assert_eq!(o2.roles, vec![Role::Orchestrator]);
        assert_eq!(o2.project_scopes, vec!["smoke".to_string()]);
        let exhausted = store
            .join_actor(
                &JoinActorRequest {
                    actor_id: "orch-3".into(),
                    pubkey_ed25519: pk_o3,
                    lineage_cluster: "claude".into(),
                    invite_code: Some(inv.invite_code),
                    skill_tags: vec![],
                    display_name: None,
                    git_email: None,
                },
                false,
            )
            .await;
        assert!(matches!(exhausted, Err(StoreError::Conflict(_))));

        // Active lease cap & project-scoped PatchActorHint blocking
        let _p_other = store
            .claim_lease("other", "w1", &ClaimLeaseRequest::default(), 900, 1)
            .await
            .unwrap()
            .unwrap();
        let capped = store
            .claim_lease("other", "w1", &ClaimLeaseRequest::default(), 900, 1)
            .await;
        assert!(matches!(capped, Err(StoreError::Conflict(_))));

        store
            .apply_ir(
                "smoke",
                1,
                "orch-1",
                &ApplyIrRequest {
                    ops: vec![GraphIrOp::PatchActorHint {
                        actor_id: "w1".into(),
                        tier: None,
                        skill_tags: None,
                        enabled: Some(false),
                    }],
                    dry_run: false,
                },
                &blobs,
            )
            .await
            .unwrap();
        let blocked_smoke = store
            .claim_lease("smoke", "w1", &ClaimLeaseRequest::default(), 900, 2)
            .await;
        assert!(matches!(blocked_smoke, Err(StoreError::Forbidden(_))));
        // Global profile remains enabled and `other` project still works!
        assert!(store.get_actor("w1").await.unwrap().unwrap().enabled);
        let ok_other = store
            .claim_lease("other", "w1", &ClaimLeaseRequest::default(), 900, 2)
            .await
            .unwrap();
        assert!(ok_other.is_some());
    }
}
