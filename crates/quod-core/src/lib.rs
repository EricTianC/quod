//! Core domain types, Ed25519 RBAC, state machines, Graph IR, `quod-algo`,
//! and Z-channel packet schemas for Quod.

pub mod algo;
pub mod auth;
pub mod evaluator;
pub mod fsm;
pub mod graph;
pub mod ir;
pub mod packet;

pub use algo::{
    AlgoQueryResult, CouplingWarning, ReusableOverlayHint, StallAlert, VoiRankEntry,
    check_coupling, detect_stalls, mine_reusable_overlays, score_task_for_actor, voi_rank,
};
pub use auth::{
    ActorAuthContext, AuthError, HEADER_ACTOR_ID, HEADER_ADMIN_TOKEN, HEADER_SIGNATURE,
    HEADER_TIMESTAMP, Role, canonical_signing_payload, generate_keypair_hex,
    public_key_from_secret_hex, sign_request, validate_pubkey_hex, verify_request,
};
pub use evaluator::{
    ActorProfile, CapabilityEvaluator, MatchDecision, NonParametricTierEvaluator, OutcomeRecord,
};
pub use fsm::{
    EdgeKind, EpochEvent, EpochState, OverlayKind, ProjectStatus, RouteState, SubmissionEvent,
    SubmissionKind, SubmissionState, TaskEvent, TaskRole, TaskState, TransitionError,
};
pub use graph::{
    ProjectGraphView, RouteView, TaskEdgeView, TaskNodeView, TaskTelemetryView, VerifiedOverlayView,
    compute_default_bounty,
};
pub use ir::{ApplyIrRequest, ApplyIrResponse, DispatchAlgo, DispatchPolicyConfig, GraphIrOp};
pub use packet::{
    AuditEpochRequest, AuditVerdict, ClaimLeaseRequest, CreateEpochRequest, CreateInviteRequest,
    CreateProjectRequest, CreditLedgerEntryView, EpochStatusView, EventRecordView,
    GraveyardEntryView, HeartbeatResponse, InviteTokenView, JoinActorRequest, LeaderboardEntryView,
    LeaseInfo, OverlaySourceView, ProjectSummaryView, RegisterActorRequest, RegisterBundleRequest,
    ResourceLimits, SealedSubmissionEnvelope, SubmissionStatusView, SubmitRequest, SubmitResponse,
    TaskPacket, ToolchainBundle, WakeupDigestView, WhoAmIResponse,
};
