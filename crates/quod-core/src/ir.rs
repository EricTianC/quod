//! Closed, strongly-typed Proof-Search Graph IR primitives submitted by `I1` (Orchestrator).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::fsm::TaskRole;

/// Pluggable T0 real-time dispatch algorithm executed by `C0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DispatchAlgo {
    #[default]
    WeightedVoi,
    LineageDiversity,
    TierMatched,
    PriorityQueue,
}

/// Active T0 dispatch policy stored per project/epoch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchPolicyConfig {
    #[serde(default)]
    pub algo: DispatchAlgo,
    #[serde(default = "default_exploration_rate")]
    pub exploration_rate: f64,
    #[serde(default = "default_true")]
    pub allow_cross_route_parallel: bool,
}

impl Default for DispatchPolicyConfig {
    fn default() -> Self {
        Self {
            algo: DispatchAlgo::WeightedVoi,
            exploration_rate: default_exploration_rate(),
            allow_cross_route_parallel: true,
        }
    }
}

fn default_exploration_rate() -> f64 {
    0.05
}

fn default_true() -> bool {
    true
}

fn default_candidate_decl() -> String {
    "candidate".to_string()
}

fn default_priority() -> f64 {
    1.0
}

fn default_prior_p() -> f64 {
    0.5
}

fn default_cost_c() -> f64 {
    1.0
}

fn default_racer_cap_k() -> u8 {
    2
}

fn default_min_tier() -> u8 {
    1
}

/// Closed set of declarative IR operations accepted by `C1` (`POST /api/.../orch/ir`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GraphIrOp {
    CreateRoute {
        route_id: String,
        title: String,
        #[serde(default)]
        rationale: String,
    },
    PruneRoute {
        route_id: String,
        reason: String,
    },
    SpawnTask {
        task_id: String,
        #[serde(default)]
        route_id: Option<String>,
        task_role: TaskRole,
        spec_decl_name: String,
        #[serde(default)]
        spec_statement: String,
        #[serde(default)]
        target_inline: bool,
        #[serde(default = "default_candidate_decl")]
        candidate_decl: String,
        #[serde(default)]
        curated_imports: Vec<String>,
        #[serde(default)]
        depends_on: Vec<String>,
        #[serde(default)]
        refutes_task_id: Option<String>,
        #[serde(default)]
        module_hint: Option<String>,
        #[serde(default = "default_priority")]
        priority: f64,
        #[serde(default = "default_prior_p")]
        prior_p: f64,
        #[serde(default = "default_cost_c")]
        expected_cost_c: f64,
        #[serde(default = "default_racer_cap_k")]
        racer_cap_k: u8,
        #[serde(default = "default_min_tier")]
        min_tier: u8,
        #[serde(default)]
        preferred_lineages: Vec<String>,
        #[serde(default)]
        credit_bounty: Option<u32>,
    },
    LinkDependency {
        upstream_task_id: String,
        downstream_task_id: String,
    },
    PublishDefOverlay {
        module_name: String,
        code_utf8: String,
    },
    SetDispatchPolicy {
        config: DispatchPolicyConfig,
    },
    PatchNodeBudget {
        task_id: String,
        #[serde(default)]
        priority: Option<f64>,
        #[serde(default)]
        racer_cap_k: Option<u8>,
        #[serde(default)]
        lease_ttl_secs: Option<i64>,
        #[serde(default)]
        min_tier: Option<u8>,
        #[serde(default)]
        preferred_lineages: Option<Vec<String>>,
        #[serde(default)]
        expected_cost_c: Option<f64>,
        #[serde(default)]
        prior_p: Option<f64>,
        #[serde(default)]
        credit_bounty: Option<u32>,
    },
    PatchActorHint {
        actor_id: String,
        #[serde(default)]
        tier: Option<u8>,
        #[serde(default)]
        skill_tags: Option<Vec<String>>,
        #[serde(default)]
        enabled: Option<bool>,
    },
}

/// Request submitting an atomic batch of Graph IR operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyIrRequest {
    pub ops: Vec<GraphIrOp>,
    #[serde(default)]
    pub dry_run: bool,
}

/// Result returned by `C1` after validating and applying an IR batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyIrResponse {
    pub applied_ops: usize,
    pub dry_run: bool,
    pub created_routes: Vec<String>,
    pub spawned_tasks: Vec<String>,
    pub unlocked_tasks: Vec<String>,
    pub overlay_job_ids: Vec<Uuid>,
    pub dispatch_policy: DispatchPolicyConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ir_ops_roundtrip_json() {
        let req = ApplyIrRequest {
            ops: vec![
                GraphIrOp::CreateRoute {
                    route_id: "route-a".into(),
                    title: "Parity split".into(),
                    rationale: "Split k=0 and k!=0".into(),
                },
                GraphIrOp::SpawnTask {
                    task_id: "merge_a".into(),
                    route_id: Some("route-a".into()),
                    task_role: TaskRole::Merge,
                    spec_decl_name: "Quod.Epoch1.erdos_479_stmt".into(),
                    spec_statement: String::new(),
                    target_inline: false,
                    candidate_decl: "candidate".into(),
                    curated_imports: vec![],
                    depends_on: vec!["lemma_1".into(), "lemma_2".into()],
                    refutes_task_id: None,
                    module_hint: Some("Chapter1".into()),
                    priority: 2.0,
                    prior_p: 0.7,
                    expected_cost_c: 1.0,
                    racer_cap_k: 2,
                    min_tier: 1,
                    preferred_lineages: vec![],
                    credit_bounty: Some(150),
                },
                GraphIrOp::SetDispatchPolicy {
                    config: DispatchPolicyConfig {
                        algo: DispatchAlgo::LineageDiversity,
                        exploration_rate: 0.1,
                        allow_cross_route_parallel: true,
                    },
                },
            ],
            dry_run: false,
        };
        let raw = serde_json::to_string(&req).unwrap();
        let parsed: ApplyIrRequest = serde_json::from_str(&raw).unwrap();
        assert_eq!(req, parsed);

        let dry: ApplyIrRequest = serde_json::from_str(r#"{"ops":[],"dry_run":true}"#).unwrap();
        assert!(dry.dry_run);
        let default_req: ApplyIrRequest = serde_json::from_str(r#"{"ops":[]}"#).unwrap();
        assert!(!default_req.dry_run);
    }
}
