//! Control-plane API payloads, TaskPacket / Z-channel schemas, and WakeupDigest views for Quod.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::algo::{CouplingWarning, ReusableOverlayHint, StallAlert, VoiRankEntry};
use crate::auth::Role;
use crate::evaluator::ActorProfile;
use crate::fsm::{EpochState, ProjectStatus, SubmissionKind, SubmissionState, TaskRole};
use crate::ir::DispatchPolicyConfig;

/// Sandbox execution quotas attached to every `TaskPacket`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu_cores: u32,
    pub memory_mb: u64,
    pub timeout_secs: u64,
    pub max_heartbeats: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpu_cores: 2,
            memory_mb: 6144,
            timeout_secs: 60,
            max_heartbeats: 300_000,
        }
    }
}

/// Active lease metadata binding a dispatched pure-`Prop` task to an actor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaseInfo {
    pub lease_id: Uuid,
    pub actor_id: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub propensity_score: f64,
}

/// Self-contained work unit delivered across the Z-channel to an external `I0` worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskPacket {
    pub schema_version: u32,
    pub project_id: String,
    pub epoch_id: u32,
    pub task_id: String,
    pub route_id: Option<String>,
    pub task_role: TaskRole,
    pub module_hint: Option<String>,
    pub bundle_id: String,
    pub spec_module: String,
    pub spec_sha256: String,
    pub target_file: String,
    pub transitive_slice: String,
    pub curated_imports: Vec<String>,
    pub graveyard_constraints: Vec<String>,
    pub resource_limits: ResourceLimits,
    #[serde(default)]
    pub credit_bounty: u32,
    pub lease: Option<LeaseInfo>,
}

/// Sealed artifact returned by an actor across the Z-channel for Stage 0/1/2 auditing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSubmissionEnvelope {
    pub submission_id: Uuid,
    pub project_id: String,
    pub epoch_id: u32,
    pub task_id: String,
    pub actor_id: String,
    pub kind: SubmissionKind,
    pub lease_id: Uuid,
    pub spec_sha256: String,
    pub code_utf8: String,
    pub blob_sha256: String,
    pub idea_attr_id: Option<String>,
    pub submitted_at: DateTime<Utc>,
}

/// Authoritative verification outcome produced by `quod-sandbox` / `quod-verifier`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditVerdict {
    pub ok: bool,
    pub exit_code: i32,
    pub kind: SubmissionKind,
    pub code_sha256: String,
    pub compile_ms: u64,
    pub diagnostics: Vec<String>,
}

/// Response returned by `GET /api/whoami` so any agent can confirm its authorized roles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhoAmIResponse {
    pub actor_id: String,
    pub roles: Vec<Role>,
    pub project_scopes: Vec<String>,
    pub lineage_cluster: String,
    pub tier: u8,
    pub enabled: bool,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub git_email: Option<String>,
    #[serde(default)]
    pub credit_points: i64,
    pub active_projects: Vec<String>,
}

/// Request to register or update an allowlisted `Actor` Ed25519 identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterActorRequest {
    pub actor_id: String,
    pub pubkey_ed25519: String,
    pub roles: Vec<Role>,
    #[serde(default)]
    pub project_scopes: Vec<String>,
    pub lineage_cluster: String,
    #[serde(default)]
    pub initial_tier: Option<u8>,
    #[serde(default)]
    pub skill_tags: Vec<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub git_email: Option<String>,
}

/// Shared read-only Lean/Mathlib `.olean` bundle metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainBundle {
    pub bundle_id: String,
    pub lean_toolchain: String,
    pub mathlib_rev: String,
    pub toolchain_dir: String,
    pub lean_paths: Vec<String>,
    pub bundle_sha256: String,
}

/// Request registering a shared `ToolchainBundle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterBundleRequest {
    pub bundle_id: String,
    pub lean_toolchain: String,
    pub mathlib_rev: String,
    pub toolchain_dir: String,
    pub lean_paths: Vec<String>,
    pub bundle_sha256: String,
}

/// Request creating a new mathematical `Project`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProjectRequest {
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub spec_namespace: String,
    #[serde(default)]
    pub default_bundle_id: Option<String>,
}

/// Summary view of a `Project`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummaryView {
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub spec_namespace: String,
    pub status: ProjectStatus,
    pub active_epoch_id: Option<u32>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// Request proposing/initializing an `Epoch` for a `Project`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateEpochRequest {
    pub epoch_id: u32,
    pub bundle_id: String,
    pub base_commit: String,
    pub spec_module: String,
    pub spec_decl_name: String,
    pub spec_statement: String,
    pub spec_sha256: String,
    #[serde(default)]
    pub spec_code: Option<String>,
    pub root_task_id: String,
    #[serde(default)]
    pub auto_activate: bool,
}

/// Request by `I2-A` (Auditor) to approve or reject a `PendingAudit` epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEpochRequest {
    pub approve: bool,
    #[serde(default)]
    pub comment: String,
}

/// Worker request to claim the next highest-scoring eligible pure-`Prop` task lease.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimLeaseRequest {
    #[serde(default)]
    pub preferred_task_id: Option<String>,
    #[serde(default)]
    pub preferred_route_id: Option<String>,
}

/// Response returned after refreshing a lease heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub lease_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub status: String,
}

/// Worker HTTP payload submitting a candidate Lean proof or refutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitRequest {
    pub epoch_id: u32,
    pub task_id: String,
    pub lease_id: Uuid,
    pub kind: SubmissionKind,
    pub spec_sha256: String,
    pub code_utf8: String,
    #[serde(default)]
    pub idea_attr_id: Option<String>,
}

/// Synchronous Stage 0 response returned on `POST /api/projects/{id}/submissions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitResponse {
    pub submission_id: Uuid,
    pub state: SubmissionState,
    pub exit_code: i32,
    pub diagnostics: Vec<String>,
}

/// Pollable status projection for a submission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmissionStatusView {
    pub submission_id: Uuid,
    pub project_id: String,
    pub epoch_id: u32,
    pub task_id: String,
    pub actor_id: String,
    pub kind: SubmissionKind,
    pub state: SubmissionState,
    pub compile_ms: Option<u64>,
    pub reject_detail: Option<serde_json::Value>,
}

/// Authoritative negative-knowledge constraint derived solely from Hub/Verifier facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardEntryView {
    pub project_id: String,
    pub epoch_id: u32,
    pub task_id: String,
    pub failure_kind: String,
    pub summary: String,
    pub created_at: DateTime<Utc>,
}

/// Compact summary returned for an epoch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpochStatusView {
    pub project_id: String,
    pub epoch_id: u32,
    pub state: EpochState,
    pub bundle_id: String,
    pub lean_toolchain: String,
    pub spec_decl_name: String,
    pub root_task_id: String,
    pub dispatch_policy: DispatchPolicyConfig,
    pub task_counts: BTreeMap<String, usize>,
    pub active_leases: usize,
    pub pending_verification_jobs: usize,
    pub overlays_total: usize,
    pub routes_total: usize,
}

/// Compressed `I1` Orchestrator wakeup digest (zero raw Lean compiler error traces).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WakeupDigestView {
    pub project_id: String,
    pub epoch_id: u32,
    pub epoch_state: EpochState,
    pub dispatch_policy: DispatchPolicyConfig,
    pub task_counts: BTreeMap<String, usize>,
    pub open_tasks: usize,
    pub blocked_tasks: usize,
    pub top_voi_tasks: Vec<VoiRankEntry>,
    pub stall_alerts: Vec<StallAlert>,
    pub coupling_warnings: Vec<CouplingWarning>,
    pub reusable_overlays: Vec<ReusableOverlayHint>,
    pub actors: Vec<ActorProfile>,
    pub recent_graveyard: Vec<GraveyardEntryView>,
}

/// Event stream entry returned by `GET /api/projects/{project_id}/epochs/{epoch_id}/events`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecordView {
    pub event_seq: i64,
    pub event_id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub project_id: String,
    pub epoch_id: i32,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub actor_id: String,
    pub payload: serde_json::Value,
}

/// Source code view of a verified overlay module (for Public Web Portal inspection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlaySourceView {
    pub project_id: String,
    pub epoch_id: u32,
    pub module_name: String,
    pub blob_sha256: String,
    pub code_utf8: String,
}

fn default_invite_max_uses() -> u32 {
    1
}

fn default_invite_ttl_secs() -> i64 {
    86_400
}

/// Request creating a scoped or role-bounded invite code (`POST /api/invites`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateInviteRequest {
    pub roles: Vec<Role>,
    #[serde(default)]
    pub project_scopes: Vec<String>,
    #[serde(default = "default_invite_max_uses")]
    pub max_uses: u32,
    #[serde(default = "default_invite_ttl_secs")]
    pub expires_in_secs: i64,
    #[serde(default)]
    pub initial_tier: Option<u8>,
    #[serde(default)]
    pub note: String,
}

/// Issued invite token view returned on `POST /api/invites`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteTokenView {
    pub invite_code: String,
    pub roles: Vec<Role>,
    pub project_scopes: Vec<String>,
    pub max_uses: u32,
    pub used_count: u32,
    pub initial_tier: u8,
    pub created_by: String,
    pub note: String,
    pub expires_at: DateTime<Utc>,
}

/// Self-registration request binding an Ed25519 pubkey via invite code or open worker join (`POST /api/actors/join`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinActorRequest {
    pub actor_id: String,
    pub pubkey_ed25519: String,
    pub lineage_cluster: String,
    #[serde(default)]
    pub invite_code: Option<String>,
    #[serde(default)]
    pub skill_tags: Vec<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub git_email: Option<String>,
}

/// Append-only kernel-settled Credit ledger entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditLedgerEntryView {
    pub entry_id: Uuid,
    pub project_id: String,
    pub epoch_id: u32,
    pub task_id: String,
    pub submission_id: Option<Uuid>,
    pub actor_id: String,
    pub points: i64,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

/// Aggregated contributor Credit leaderboard row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderboardEntryView {
    pub actor_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub lineage_cluster: String,
    pub tier: u8,
    pub credit_points: i64,
    pub verified_tasks: u64,
    pub verified_refutations: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_packet_json_roundtrip() {
        let pkt = TaskPacket {
            schema_version: 2,
            project_id: "erdos-479".into(),
            epoch_id: 1,
            task_id: "task-001".into(),
            route_id: Some("route-a".into()),
            task_role: TaskRole::Lemma,
            module_hint: Some("Quod.Epoch1.Spec".into()),
            bundle_id: "mathlib-v4.36.0".into(),
            spec_module: "Quod.Epoch1.Spec".into(),
            spec_sha256: "abc123".into(),
            target_file: "Tasks/task_001.lean".into(),
            transitive_slice: "theorem candidate : 1 + 1 = 2 := by sorry".into(),
            curated_imports: vec!["Quod.Epoch1.Spec".into()],
            graveyard_constraints: vec!["[kernel_rejected] timeout".into()],
            resource_limits: ResourceLimits::default(),
            credit_bounty: 100,
            lease: None,
        };
        let raw = serde_json::to_string(&pkt).unwrap();
        let decoded: TaskPacket = serde_json::from_str(&raw).unwrap();
        assert_eq!(pkt, decoded);
    }
}
