//! Dual-layer `E1` graph projections: `Verified Overlays` + `Route`-grouped pure `Prop` Task DAG.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::fsm::{
    EdgeKind, EpochState, OverlayKind, ProjectStatus, RouteState, TaskRole, TaskState,
};
use crate::ir::DispatchPolicyConfig;
use crate::packet::LeaseInfo;

/// Exploration route grouping tasks inside an epoch's Task DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteView {
    pub route_id: String,
    pub project_id: String,
    pub epoch_id: u32,
    pub title: String,
    pub rationale: String,
    pub state: RouteState,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// Objective per-task execution telemetry collected by `C0` for `I1`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TaskTelemetryView {
    pub total_attempts: u64,
    pub failed_attempts: u64,
    pub timeout_count: u64,
    pub failed_lineages: Vec<String>,
    pub last_compile_ms: Option<u64>,
}

/// A pure `Prop` task node inside the `Route`-grouped Task DAG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskNodeView {
    pub task_id: String,
    pub project_id: String,
    pub epoch_id: u32,
    pub route_id: Option<String>,
    pub task_role: TaskRole,
    pub state: TaskState,
    pub priority: f64,
    pub racer_cap_k: u8,
    pub lease_ttl_secs: Option<i64>,
    pub min_tier: u8,
    pub preferred_lineages: Vec<String>,
    pub prior_p: f64,
    pub expected_cost_c: f64,
    pub spec_decl_name: String,
    pub spec_statement: String,
    pub target_inline: bool,
    pub candidate_decl: String,
    pub curated_imports: Vec<String>,
    pub module_hint: Option<String>,
    #[serde(default)]
    pub credit_bounty: u32,
    pub telemetry: TaskTelemetryView,
}

/// Compute default Credit bounty from difficulty (`expected_cost_c`), `priority`, and stall retries.
pub fn compute_default_bounty(expected_cost_c: f64, priority: f64, failed_attempts: u64) -> u32 {
    let base = expected_cost_c.max(0.2) * 50.0 * priority.clamp(0.5, 3.0);
    let stall_mult = (1.0 + 0.25 * (failed_attempts.min(4) as f64)).min(2.0);
    let rounded = (((base * stall_mult) / 5.0).round() * 5.0) as i64;
    rounded.clamp(10, 5000) as u32
}

impl TaskNodeView {
    /// Effective Credit bounty awarded when this task passes kernel verification.
    pub fn effective_bounty(&self) -> u32 {
        if self.credit_bounty > 0 {
            self.credit_bounty
        } else {
            compute_default_bounty(
                self.expected_cost_c,
                self.priority,
                self.telemetry.failed_attempts,
            )
        }
    }

    /// Baseline Smith/VoI score `priority * (1 - p) / c`.
    pub fn smith_score(&self) -> f64 {
        let p = self.prior_p.clamp(0.0, 1.0);
        let c = self.expected_cost_c.max(1e-6);
        let prio = self.priority.max(0.05);
        prio * (1.0 - p) / c
    }

    /// True if the task can currently receive new worker leases.
    pub fn is_dispatchable(&self) -> bool {
        matches!(
            self.state,
            TaskState::Open | TaskState::Racing | TaskState::StalledSoft
        )
    }
}

/// Directed 1-to-1 scheduling/refutation edge inside `E1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEdgeView {
    pub from_task_id: String,
    pub to_task_id: String,
    pub edge_kind: EdgeKind,
}

/// Append-only verified Lean module registered in `E1`'s truth layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedOverlayView {
    pub project_id: String,
    pub epoch_id: u32,
    pub module_name: String,
    pub overlay_kind: OverlayKind,
    pub source_task_id: Option<String>,
    pub source_actor_id: String,
    pub blob_sha256: String,
    pub imports: Vec<String>,
    pub compile_ms: u64,
    pub created_at: DateTime<Utc>,
}

/// Full read-only project/epoch dual-layer snapshot for CLI, `I1`, and Public Web Portal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectGraphView {
    pub project_id: String,
    pub project_status: ProjectStatus,
    pub epoch_id: u32,
    pub epoch_state: EpochState,
    pub bundle_id: String,
    pub lean_toolchain: String,
    pub spec_module: String,
    pub spec_decl_name: String,
    pub root_task_id: String,
    pub dispatch_policy: DispatchPolicyConfig,
    pub routes: Vec<RouteView>,
    pub tasks: Vec<TaskNodeView>,
    pub edges: Vec<TaskEdgeView>,
    pub overlays: Vec<VerifiedOverlayView>,
    pub active_leases: Vec<LeaseInfo>,
    pub graveyard_count: usize,
}
