//! PostgreSQL 16 & in-memory event-sourced control-plane stores and
//! content-addressed blob storage for Quod.

pub mod memory;
pub mod pg;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use quod_core::{
    ActorAuthContext, ActorProfile, AlgoQueryResult, ApplyIrRequest, ApplyIrResponse,
    AuditEpochRequest, AuditVerdict, ClaimLeaseRequest, CreateEpochRequest, CreateInviteRequest,
    CreateProjectRequest, EpochStatusView, EventRecordView, HeartbeatResponse, InviteTokenView,
    JoinActorRequest, LeaderboardEntryView, ProjectGraphView, ProjectSummaryView,
    RegisterActorRequest, RegisterBundleRequest, SubmissionKind, SubmissionState,
    SubmissionStatusView, SubmitRequest, SubmitResponse, TaskPacket, TaskState, ToolchainBundle,
    WakeupDigestView, WhoAmIResponse,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub use memory::MemoryStore;
pub use pg::PgEventStore;

pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("filesystem blob store error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid content digest format: {0}")]
    InvalidDigest(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("invalid state transition: {0}")]
    Transition(#[from] quod_core::TransitionError),
}

pub(crate) fn validate_actor_id(actor_id: &str) -> Result<(), StoreError> {
    if actor_id.is_empty()
        || actor_id.len() > 64
        || !actor_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return Err(StoreError::BadRequest(
            "actor_id must be 1..=64 ASCII characters [a-zA-Z0-9._-]".into(),
        ));
    }
    Ok(())
}

/// Canonical append-only event record persisted in `events`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: Uuid,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub project_id: String,
    pub epoch_id: i32,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub idempotency_key: String,
    pub actor_id: String,
    pub payload: serde_json::Value,
}

/// Stored actor credentials + authorization context returned by `get_actor_auth`.
#[derive(Debug, Clone)]
pub struct ActorCredentials {
    pub pubkey_ed25519: String,
    pub auth: ActorAuthContext,
}

/// Input recorded when an actor sends a submission passing or failing Stage 0.
#[derive(Debug, Clone)]
pub struct NewSubmissionInput {
    pub submission_id: Uuid,
    pub project_id: String,
    pub actor_id: String,
    pub req: SubmitRequest,
    pub blob_sha256: String,
    pub received_at_ns: i64,
    pub stage0_passed: bool,
    pub stage0_exit_code: i32,
    pub stage0_diagnostics: Vec<String>,
}

/// Queued Stage 1 verification job consumed by `quod-verifier`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationJob {
    pub job_id: Uuid,
    pub submission_id: Uuid,
    pub project_id: String,
    pub epoch_id: u32,
    pub task_id: String,
    pub actor_id: String,
    pub kind: SubmissionKind,
    pub bundle_id: String,
    pub spec_module: String,
    pub spec_decl: String,
    pub spec_statement: String,
    pub target_inline: bool,
    pub candidate_module: String,
    pub candidate_decl: String,
    pub blob_sha256: String,
    pub curated_imports: Vec<String>,
}

/// Authoritative Stage 1/2 verdict submitted back by `quod-verifier`.
#[derive(Debug, Clone)]
pub struct VerificationOutcomeInput {
    pub job_id: Uuid,
    pub submission_id: Uuid,
    pub verdict: AuditVerdict,
}

/// State mutation summary returned after recording a verification outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationOutcomeResult {
    pub submission_state: SubmissionState,
    pub task_state: TaskState,
    pub unlocked_tasks: Vec<String>,
    pub overlay_module: Option<String>,
    pub project_proved: bool,
}

/// Real-time Hub-wide active agent & lease counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubLiveMetrics {
    pub online_agents: usize,
    pub active_leases: usize,
    pub registered_agents: usize,
}

/// Shared asynchronous control-plane & verifier store contract for Quod.
pub trait QuodStore: Send + Sync {
    fn register_actor(&self, req: &RegisterActorRequest) -> StoreFuture<'_, ActorProfile>;
    fn get_actor(&self, actor_id: &str) -> StoreFuture<'_, Option<ActorProfile>>;
    fn get_actor_auth(&self, actor_id: &str) -> StoreFuture<'_, Option<ActorCredentials>>;
    fn whoami(&self, actor_id: &str) -> StoreFuture<'_, Option<WhoAmIResponse>>;

    fn register_bundle(&self, req: &RegisterBundleRequest) -> StoreFuture<'_, ToolchainBundle>;
    fn get_bundle(&self, bundle_id: &str) -> StoreFuture<'_, Option<ToolchainBundle>>;

    fn create_project(
        &self,
        actor_id: &str,
        req: &CreateProjectRequest,
    ) -> StoreFuture<'_, ProjectSummaryView>;
    fn list_projects(&self) -> StoreFuture<'_, Vec<ProjectSummaryView>>;

    fn create_epoch(
        &self,
        project_id: &str,
        actor_id: &str,
        req: &CreateEpochRequest,
    ) -> StoreFuture<'_, EpochStatusView>;
    fn audit_epoch(
        &self,
        project_id: &str,
        epoch_id: u32,
        auditor_id: &str,
        req: &AuditEpochRequest,
    ) -> StoreFuture<'_, EpochStatusView>;
    fn freeze_epoch(
        &self,
        project_id: &str,
        epoch_id: u32,
        actor_id: &str,
    ) -> StoreFuture<'_, EpochStatusView>;

    fn apply_ir(
        &self,
        project_id: &str,
        epoch_id: u32,
        actor_id: &str,
        req: &ApplyIrRequest,
        blobs: &dyn BlobStore,
    ) -> StoreFuture<'_, ApplyIrResponse>;

    fn create_invite(
        &self,
        created_by: &str,
        req: &CreateInviteRequest,
    ) -> StoreFuture<'_, InviteTokenView>;
    fn join_actor(
        &self,
        req: &JoinActorRequest,
        open_worker_join: bool,
    ) -> StoreFuture<'_, ActorProfile>;

    fn claim_lease(
        &self,
        project_id: &str,
        actor_id: &str,
        req: &ClaimLeaseRequest,
        default_ttl_secs: i64,
        max_active_leases_per_actor: usize,
    ) -> StoreFuture<'_, Option<TaskPacket>>;
    fn heartbeat_lease(
        &self,
        lease_id: Uuid,
        actor_id: &str,
        extend_secs: i64,
    ) -> StoreFuture<'_, HeartbeatResponse>;

    fn create_submission(&self, input: NewSubmissionInput) -> StoreFuture<'_, SubmitResponse>;
    fn get_submission(
        &self,
        submission_id: Uuid,
    ) -> StoreFuture<'_, Option<SubmissionStatusView>>;

    fn claim_next_verification_job(
        &self,
        verifier_id: &str,
    ) -> StoreFuture<'_, Option<VerificationJob>>;
    fn record_verification_outcome(
        &self,
        input: VerificationOutcomeInput,
    ) -> StoreFuture<'_, VerificationOutcomeResult>;

    fn epoch_status(
        &self,
        project_id: &str,
        epoch_id: u32,
    ) -> StoreFuture<'_, Option<EpochStatusView>>;
    fn project_graph(
        &self,
        project_id: &str,
        epoch_id: u32,
    ) -> StoreFuture<'_, Option<ProjectGraphView>>;
    fn wakeup_digest(
        &self,
        project_id: &str,
        epoch_id: u32,
    ) -> StoreFuture<'_, Option<WakeupDigestView>>;
    fn run_algo(
        &self,
        project_id: &str,
        epoch_id: u32,
        algo_name: &str,
        limit: usize,
    ) -> StoreFuture<'_, Option<AlgoQueryResult>>;
    fn recent_events(
        &self,
        project_id: &str,
        epoch_id: u32,
        since_seq: Option<i64>,
        limit: usize,
    ) -> StoreFuture<'_, Vec<EventRecordView>>;
    fn get_overlay_blob(
        &self,
        project_id: &str,
        epoch_id: u32,
        module_name: &str,
    ) -> StoreFuture<'_, Option<String>>;
    fn leaderboard(
        &self,
        project_id: Option<&str>,
        limit: usize,
    ) -> StoreFuture<'_, Vec<LeaderboardEntryView>>;
    fn live_metrics(&self) -> StoreFuture<'_, HubLiveMetrics>;
    fn health_check(&self) -> StoreFuture<'_, ()>;
}

/// Content-addressed artifact store (Stage 0/1 sealed code blobs, `.olean` bundles).
pub trait BlobStore: Send + Sync {
    fn put_bytes(&self, bytes: &[u8]) -> Result<String, StoreError>;
    fn get_bytes(&self, sha256: &str) -> Result<Vec<u8>, StoreError>;
    fn contains(&self, sha256: &str) -> Result<bool, StoreError>;
}

/// Local-filesystem content-addressed blob repository (`<root>/<2-hex>/<sha256>`).
#[derive(Debug, Clone)]
pub struct LocalFsBlobStore {
    root: PathBuf,
}

impl LocalFsBlobStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn blob_path(&self, sha256: &str) -> Result<PathBuf, StoreError> {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(StoreError::InvalidDigest(sha256.to_string()));
        }
        Ok(self.root.join(&sha256[..2]).join(sha256))
    }
}

impl BlobStore for LocalFsBlobStore {
    fn put_bytes(&self, bytes: &[u8]) -> Result<String, StoreError> {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hex::encode(hasher.finalize());
        let path = self.blob_path(&digest)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            std::fs::write(&path, bytes)?;
        }
        Ok(digest)
    }

    fn get_bytes(&self, sha256: &str) -> Result<Vec<u8>, StoreError> {
        let path = self.blob_path(sha256)?;
        Ok(std::fs::read(path)?)
    }

    fn contains(&self, sha256: &str) -> Result<bool, StoreError> {
        let path = self.blob_path(sha256)?;
        Ok(path.exists())
    }
}

/// Helper generating a collision-free Lean module name for an overlay/task.
pub fn overlay_module_name(kind: SubmissionKind, task_id: &str, submission_id: Uuid) -> String {
    let short = &submission_id.simple().to_string()[..8];
    let sanitized: String = task_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    match kind {
        SubmissionKind::DefOverlay => sanitized,
        SubmissionKind::Proof => format!("QuodOverlay_{sanitized}_{short}"),
        SubmissionKind::Refutation => format!("QuodRefute_{sanitized}_{short}"),
    }
}

/// Sanitizes an arbitrary task ID into a valid Lean declaration suffix.
pub fn sanitize_ident(raw: &str) -> String {
    let out: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("t_{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_blob_store_roundtrip_and_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let store = LocalFsBlobStore::new(tmp.path());

        let payload = b"theorem foo : True := trivial\n";
        let sha1 = store.put_bytes(payload).unwrap();
        let sha2 = store.put_bytes(payload).unwrap();
        assert_eq!(sha1, sha2);
        assert!(store.contains(&sha1).unwrap());
        assert_eq!(store.get_bytes(&sha1).unwrap(), payload);
        assert!(store.blob_path("invalid").is_err());
    }
}
