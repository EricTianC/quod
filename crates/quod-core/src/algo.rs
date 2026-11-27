//! Stateless `quod-algo` library: read-only `I1` graph analysis & `C0` dispatch scoring.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::evaluator::{ActorProfile, MatchDecision};
use crate::fsm::{EdgeKind, RouteState, TaskRole, TaskState};
use crate::graph::{RouteView, TaskEdgeView, TaskNodeView, VerifiedOverlayView};
use crate::ir::{DispatchAlgo, DispatchPolicyConfig};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiRankEntry {
    pub task_id: String,
    pub route_id: Option<String>,
    pub task_role: TaskRole,
    pub state: TaskState,
    pub voi_score: f64,
    pub downstream_blocked_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StallAlert {
    pub task_id: String,
    pub route_id: Option<String>,
    pub failed_attempts: u64,
    pub timeout_count: u64,
    pub distinct_failed_lineages: usize,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CouplingWarning {
    pub route_id: String,
    pub task_ids: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReusableOverlayHint {
    pub module_name: String,
    pub source_task_id: Option<String>,
    pub source_route_state: Option<RouteState>,
    pub dependent_task_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "algo", rename_all = "snake_case")]
pub enum AlgoQueryResult {
    VoiRank { entries: Vec<VoiRankEntry> },
    DetectStalls { alerts: Vec<StallAlert> },
    CheckCoupling { warnings: Vec<CouplingWarning> },
    MineReusable { hints: Vec<ReusableOverlayHint> },
}

/// Ranks dispatchable tasks by `(1 - p) * priority * (1 + 0.5 * downstream_unblocks) / c`.
pub fn voi_rank(
    tasks: &[TaskNodeView],
    edges: &[TaskEdgeView],
    limit: usize,
) -> Vec<VoiRankEntry> {
    let mut downstream_counts: HashMap<&str, usize> = HashMap::new();
    let task_map: HashMap<&str, &TaskNodeView> =
        tasks.iter().map(|t| (t.task_id.as_str(), t)).collect();
    for edge in edges {
        if edge.edge_kind == EdgeKind::DependsOn
            && task_map
                .get(edge.to_task_id.as_str())
                .is_some_and(|t| t.state == TaskState::Blocked)
        {
            *downstream_counts
                .entry(edge.from_task_id.as_str())
                .or_insert(0) += 1;
        }
    }

    let mut ranked: Vec<VoiRankEntry> = tasks
        .iter()
        .filter(|t| t.is_dispatchable())
        .map(|t| {
            let unblocks = *downstream_counts.get(t.task_id.as_str()).unwrap_or(&0);
            let voi_score = t.smith_score() * (1.0 + 0.5 * unblocks as f64);
            VoiRankEntry {
                task_id: t.task_id.clone(),
                route_id: t.route_id.clone(),
                task_role: t.task_role,
                state: t.state,
                voi_score,
                downstream_blocked_count: unblocks,
            }
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.voi_score
            .total_cmp(&a.voi_score)
            .then_with(|| a.task_id.cmp(&b.task_id))
    });
    ranked.truncate(limit.max(1));
    ranked
}

/// Detects tasks stalling across multiple heterogeneous lineages or repeated timeouts.
pub fn detect_stalls(tasks: &[TaskNodeView]) -> Vec<StallAlert> {
    let mut alerts = Vec::new();
    for t in tasks {
        if t.state.is_terminal() || t.state == TaskState::Blocked {
            continue;
        }
        let lineages = t.telemetry.failed_lineages.len();
        let is_stalled = t.state == TaskState::StalledSoft
            || lineages >= 2
            || t.telemetry.failed_attempts >= 4
            || t.telemetry.timeout_count >= 2;
        if is_stalled {
            alerts.push(StallAlert {
                task_id: t.task_id.clone(),
                route_id: t.route_id.clone(),
                failed_attempts: t.telemetry.failed_attempts,
                timeout_count: t.telemetry.timeout_count,
                distinct_failed_lineages: lineages,
                reason: format!(
                    "state={:?}, failed_attempts={}, distinct_lineages={}, timeouts={}",
                    t.state, t.telemetry.failed_attempts, lineages, t.telemetry.timeout_count
                ),
            });
        }
    }
    alerts.sort_by(|a, b| b.failed_attempts.cmp(&a.failed_attempts));
    alerts
}

/// Flags sibling tasks in the same `Route` that look like existential-witness splits (`∃`/`Exists`).
pub fn check_coupling(tasks: &[TaskNodeView]) -> Vec<CouplingWarning> {
    let mut by_route: HashMap<&str, Vec<&TaskNodeView>> = HashMap::new();
    for t in tasks {
        if !t.state.is_terminal()
            && matches!(t.task_role, TaskRole::Lemma | TaskRole::AuxLemma)
            && let Some(route_id) = &t.route_id
        {
            by_route.entry(route_id.as_str()).or_default().push(t);
        }
    }

    let mut warnings = Vec::new();
    for (route_id, group) in by_route {
        let existential_tasks: Vec<String> = group
            .iter()
            .filter(|t| {
                t.spec_statement.contains('∃')
                    || t.spec_statement.contains("Exists")
                    || t.spec_statement.contains("Nonempty")
            })
            .map(|t| t.task_id.clone())
            .collect();
        if existential_tasks.len() >= 2 {
            warnings.push(CouplingWarning {
                route_id: route_id.to_string(),
                task_ids: existential_tasks,
                reason: "Multiple open existential/witness tasks in the same route; verify witnesses are decoupled or bundle via WitnessBundle".into(),
            });
        }
    }
    warnings.sort_by(|a, b| a.route_id.cmp(&b.route_id));
    warnings
}

/// Identifies verified task overlays (especially from pruned/refuted routes) not yet widely reused.
pub fn mine_reusable_overlays(
    overlays: &[VerifiedOverlayView],
    tasks: &[TaskNodeView],
    routes: &[RouteView],
) -> Vec<ReusableOverlayHint> {
    let route_states: HashMap<&str, RouteState> =
        routes.iter().map(|r| (r.route_id.as_str(), r.state)).collect();
    let task_routes: HashMap<&str, Option<&str>> = tasks
        .iter()
        .map(|t| (t.task_id.as_str(), t.route_id.as_deref()))
        .collect();

    let mut import_counts: HashMap<&str, usize> = HashMap::new();
    for t in tasks {
        for imp in &t.curated_imports {
            *import_counts.entry(imp.as_str()).or_insert(0) += 1;
        }
    }

    let mut hints = Vec::new();
    for ov in overlays {
        let source_route_state = ov
            .source_task_id
            .as_deref()
            .and_then(|tid| task_routes.get(tid).copied().flatten())
            .and_then(|rid| route_states.get(rid).copied());
        let deps = *import_counts.get(ov.module_name.as_str()).unwrap_or(&0);
        if matches!(
            source_route_state,
            Some(RouteState::Pruned | RouteState::Refuted)
        ) || deps == 0
        {
            hints.push(ReusableOverlayHint {
                module_name: ov.module_name.clone(),
                source_task_id: ov.source_task_id.clone(),
                source_route_state,
                dependent_task_count: deps,
            });
        }
    }
    hints.sort_by(|a, b| a.module_name.cmp(&b.module_name));
    hints
}

/// Stateless `C0` dispatch scorer governed by `DispatchPolicyConfig`.
pub fn score_task_for_actor(
    policy: &DispatchPolicyConfig,
    actor: &ActorProfile,
    task: &TaskNodeView,
    active_clusters_on_task: &[String],
    explore_slot: bool,
) -> MatchDecision {
    let cluster_set: HashSet<&str> = active_clusters_on_task
        .iter()
        .map(String::as_str)
        .collect();
    let same_lineage = cluster_set.contains(actor.lineage_cluster.as_str());

    let lineage_factor = match policy.algo {
        DispatchAlgo::LineageDiversity => {
            if same_lineage && !explore_slot {
                0.08
            } else {
                1.25
            }
        }
        _ => {
            if same_lineage && !explore_slot {
                0.15
            } else {
                1.0
            }
        }
    };

    let desired_tier = match task.expected_cost_c {
        c if c >= 4.0 => 4,
        c if c >= 2.0 => 3,
        c if c >= 1.0 => 2,
        _ => 1,
    };
    let tier_delta = (actor.tier as i16 - desired_tier as i16).abs() as f64;
    let tier_fit = (1.0 - 0.2 * tier_delta).max(0.2);

    let preferred_bonus = if task.preferred_lineages.is_empty()
        || task
            .preferred_lineages
            .iter()
            .any(|l| l == &actor.lineage_cluster)
    {
        1.15
    } else {
        0.75
    };

    let base = match policy.algo {
        DispatchAlgo::PriorityQueue => task.priority.max(0.05) * lineage_factor * preferred_bonus,
        DispatchAlgo::TierMatched => {
            (0.4 * actor.ema_accept_rate.clamp(0.0, 1.0) + 0.6 * tier_fit)
                * lineage_factor
                * preferred_bonus
                * task.priority.max(0.05)
        }
        DispatchAlgo::WeightedVoi | DispatchAlgo::LineageDiversity => {
            (0.6 * actor.ema_accept_rate.clamp(0.0, 1.0) + 0.4 * tier_fit)
                * lineage_factor
                * preferred_bonus
                * task.smith_score().clamp(0.1, 20.0)
        }
    };

    let propensity = if explore_slot {
        policy.exploration_rate.clamp(0.01, 0.5)
    } else {
        1.0 - policy.exploration_rate.clamp(0.01, 0.5)
    };

    MatchDecision {
        score: base,
        propensity_score: propensity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::fsm::OverlayKind;
    use crate::graph::TaskTelemetryView;
    use chrono::Utc;

    fn sample_task(id: &str, state: TaskState, prio: f64, p: f64, c: f64) -> TaskNodeView {
        TaskNodeView {
            task_id: id.into(),
            project_id: "p1".into(),
            epoch_id: 1,
            route_id: Some("r1".into()),
            task_role: TaskRole::Lemma,
            state,
            priority: prio,
            racer_cap_k: 2,
            lease_ttl_secs: None,
            min_tier: 1,
            preferred_lineages: vec![],
            prior_p: p,
            expected_cost_c: c,
            spec_decl_name: format!("Spec.{id}"),
            spec_statement: "True".into(),
            target_inline: true,
            candidate_decl: "candidate".into(),
            curated_imports: vec![],
            module_hint: None,
            credit_bounty: 50,
            telemetry: TaskTelemetryView::default(),
        }
    }

    #[test]
    fn voi_rank_boosts_unblocking_tasks_and_stalls_detected() {
        let mut t1 = sample_task("t1", TaskState::Open, 1.0, 0.5, 1.0);
        t1.telemetry.failed_lineages = vec!["claude".into(), "gpt".into()];
        let t2 = sample_task("t2", TaskState::Open, 1.0, 0.5, 1.0);
        let merge = sample_task("merge", TaskState::Blocked, 2.0, 0.5, 1.0);
        let edges = vec![TaskEdgeView {
            from_task_id: "t1".into(),
            to_task_id: "merge".into(),
            edge_kind: EdgeKind::DependsOn,
        }];

        let ranked = voi_rank(&[t1.clone(), t2, merge], &edges, 10);
        assert_eq!(ranked[0].task_id, "t1");
        assert_eq!(ranked[0].downstream_blocked_count, 1);

        let stalls = detect_stalls(&[t1]);
        assert_eq!(stalls.len(), 1);
        assert_eq!(stalls[0].distinct_failed_lineages, 2);
    }

    #[test]
    fn reusable_overlays_and_dispatch_scoring() {
        let routes = vec![RouteView {
            route_id: "r_dead".into(),
            project_id: "p1".into(),
            epoch_id: 1,
            title: "Dead route".into(),
            rationale: String::new(),
            state: RouteState::Refuted,
            created_by: "orch".into(),
            created_at: Utc::now(),
        }];
        let mut t = sample_task("q1", TaskState::Proved, 1.0, 0.5, 1.0);
        t.route_id = Some("r_dead".into());
        let ov = VerifiedOverlayView {
            project_id: "p1".into(),
            epoch_id: 1,
            module_name: "QuodOverlay_q1".into(),
            overlay_kind: OverlayKind::ProvedTask,
            source_task_id: Some("q1".into()),
            source_actor_id: "w1".into(),
            blob_sha256: "a".repeat(64),
            imports: vec![],
            compile_ms: 120,
            created_at: Utc::now(),
        };
        let hints = mine_reusable_overlays(&[ov], &[t.clone()], &routes);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].source_route_state, Some(RouteState::Refuted));

        let actor = ActorProfile {
            actor_id: "w1".into(),
            roles: vec![Role::Worker],
            project_scopes: vec![],
            lineage_cluster: "claude".into(),
            tier: 3,
            skill_tags: vec![],
            enabled: true,
            ema_accept_rate: 0.8,
            display_name: None,
            git_email: None,
            credit_points: 0,
            total_probes: 0,
            probe_passes: 0,
            total_submissions: 0,
            accepted_submissions: 0,
            rejected_submissions: 0,
            avg_compile_ms: None,
        };
        let fresh = score_task_for_actor(&DispatchPolicyConfig::default(), &actor, &t, &[], false);
        let dup = score_task_for_actor(
            &DispatchPolicyConfig::default(),
            &actor,
            &t,
            &["claude".into()],
            false,
        );
        assert!(fresh.score > dup.score * 4.0);
    }
}
