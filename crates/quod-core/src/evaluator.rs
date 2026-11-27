//! Actor capability profiles and pluggable M4 evaluator interface.

use serde::{Deserialize, Serialize};

use crate::auth::Role;
use crate::graph::TaskNodeView;

/// Unified actor profile & objective telemetry stored in `actors`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActorProfile {
    pub actor_id: String,
    pub roles: Vec<Role>,
    pub project_scopes: Vec<String>,
    pub lineage_cluster: String,
    /// Capability tier in `1..=4` (1 = scout, 4 = flagship), adjustable via `I1` hints or M4 evaluator.
    pub tier: u8,
    pub skill_tags: Vec<String>,
    pub enabled: bool,
    pub ema_accept_rate: f64,
    pub display_name: Option<String>,
    pub git_email: Option<String>,
    pub credit_points: i64,
    pub total_probes: u64,
    pub probe_passes: u64,
    pub total_submissions: u64,
    pub accepted_submissions: u64,
    pub rejected_submissions: u64,
    pub avg_compile_ms: Option<u64>,
}

impl ActorProfile {
    pub fn probe_pass_rate(&self) -> f64 {
        if self.total_probes == 0 {
            1.0
        } else {
            self.probe_passes as f64 / self.total_probes as f64
        }
    }
}

/// Outcome observation emitted after task verification completes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutcomeRecord {
    pub actor_id: String,
    pub task_id: String,
    pub is_canary_probe: bool,
    pub accepted: bool,
    pub compile_ms: Option<u64>,
    pub used_exploration_slot: bool,
}

/// Score & inverse propensity recorded onto `leases.propensity_score`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchDecision {
    pub score: f64,
    pub propensity_score: f64,
}

/// Pluggable capability evaluator interface for M4 (`quod-evaluator`).
pub trait CapabilityEvaluator {
    fn get_actor_profile(&self, actor_id: &str) -> Option<&ActorProfile>;

    fn match_score(
        &self,
        actor: &ActorProfile,
        task: &TaskNodeView,
        active_clusters_on_task: &[String],
        explore_slot: bool,
    ) -> MatchDecision;

    fn failure_escalation_weight(&self, actor: &ActorProfile) -> f64;

    fn record_outcome(&mut self, outcome: &OutcomeRecord);
}

/// Baseline non-parametric 4-tier evaluator.
#[derive(Debug, Clone)]
pub struct NonParametricTierEvaluator {
    profiles: std::collections::HashMap<String, ActorProfile>,
    pub ema_alpha: f64,
    pub probe_drift_tolerance: f64,
    pub baseline_probe_rate: f64,
    pub exploration_rate: f64,
}

impl Default for NonParametricTierEvaluator {
    fn default() -> Self {
        Self {
            profiles: std::collections::HashMap::new(),
            ema_alpha: 0.2,
            probe_drift_tolerance: 0.04,
            baseline_probe_rate: 0.90,
            exploration_rate: 0.05,
        }
    }
}

impl NonParametricTierEvaluator {
    pub fn upsert_profile(&mut self, profile: ActorProfile) {
        self.profiles.insert(profile.actor_id.clone(), profile);
    }

    fn desired_tier_for_task(task: &TaskNodeView) -> u8 {
        match task.expected_cost_c {
            c if c >= 4.0 => 4,
            c if c >= 2.0 => 3,
            c if c >= 1.0 => 2,
            _ => 1,
        }
    }
}

impl CapabilityEvaluator for NonParametricTierEvaluator {
    fn get_actor_profile(&self, actor_id: &str) -> Option<&ActorProfile> {
        self.profiles.get(actor_id)
    }

    fn match_score(
        &self,
        actor: &ActorProfile,
        task: &TaskNodeView,
        active_clusters_on_task: &[String],
        explore_slot: bool,
    ) -> MatchDecision {
        let cluster_already_racing = active_clusters_on_task
            .iter()
            .any(|c| c == &actor.lineage_cluster);

        let lineage_factor = if cluster_already_racing && !explore_slot {
            0.15
        } else {
            1.0
        };

        let target_tier = Self::desired_tier_for_task(task);
        let tier_delta = (actor.tier as i16 - target_tier as i16).abs() as f64;
        let tier_fit = (1.0 - 0.2 * tier_delta).max(0.2);

        let base_score = (0.6 * actor.ema_accept_rate.clamp(0.0, 1.0) + 0.4 * tier_fit)
            * lineage_factor
            * task.smith_score().clamp(0.1, 10.0);

        let propensity = if explore_slot {
            self.exploration_rate
        } else {
            1.0 - self.exploration_rate
        };

        MatchDecision {
            score: base_score,
            propensity_score: propensity,
        }
    }

    fn failure_escalation_weight(&self, actor: &ActorProfile) -> f64 {
        let tier_clamped = actor.tier.clamp(1, 4) as f64;
        tier_clamped * (0.5 + actor.ema_accept_rate.clamp(0.0, 1.0))
    }

    fn record_outcome(&mut self, outcome: &OutcomeRecord) {
        let Some(profile) = self.profiles.get_mut(&outcome.actor_id) else {
            return;
        };
        let y = if outcome.accepted { 1.0 } else { 0.0 };
        profile.total_submissions += 1;
        if outcome.accepted {
            profile.accepted_submissions += 1;
        } else {
            profile.rejected_submissions += 1;
        }
        profile.ema_accept_rate =
            (1.0 - self.ema_alpha) * profile.ema_accept_rate + self.ema_alpha * y;

        if outcome.is_canary_probe {
            profile.total_probes += 1;
            if outcome.accepted {
                profile.probe_passes += 1;
            }
            if profile.total_probes >= 5
                && profile.probe_pass_rate() + self.probe_drift_tolerance < self.baseline_probe_rate
                && profile.tier > 1
            {
                profile.tier -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::{TaskRole, TaskState};
    use crate::graph::TaskTelemetryView;

    fn sample_task() -> TaskNodeView {
        TaskNodeView {
            task_id: "t1".into(),
            project_id: "p1".into(),
            epoch_id: 1,
            route_id: None,
            task_role: TaskRole::Lemma,
            state: TaskState::Open,
            priority: 1.0,
            racer_cap_k: 2,
            lease_ttl_secs: None,
            min_tier: 1,
            preferred_lineages: vec![],
            prior_p: 0.4,
            expected_cost_c: 2.0,
            spec_decl_name: "Thm.main".into(),
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
    fn lineage_dedup_and_canary_downgrade() {
        let mut eval = NonParametricTierEvaluator::default();
        let a1 = ActorProfile {
            actor_id: "a1".into(),
            roles: vec![Role::Worker],
            project_scopes: vec![],
            lineage_cluster: "claude".into(),
            tier: 3,
            skill_tags: vec![],
            enabled: true,
            ema_accept_rate: 0.7,
            display_name: None,
            git_email: None,
            credit_points: 0,
            total_probes: 4,
            probe_passes: 2,
            total_submissions: 4,
            accepted_submissions: 2,
            rejected_submissions: 2,
            avg_compile_ms: Some(900),
        };
        eval.upsert_profile(a1.clone());

        let task = sample_task();
        let fresh = eval.match_score(&a1, &task, &["gpt".into()], false);
        let dup = eval.match_score(&a1, &task, &["claude".into()], false);
        assert!(fresh.score > dup.score * 4.0);

        eval.record_outcome(&OutcomeRecord {
            actor_id: "a1".into(),
            task_id: "probe-1".into(),
            is_canary_probe: true,
            accepted: false,
            compile_ms: Some(1200),
            used_exploration_slot: false,
        });
        assert_eq!(eval.get_actor_profile("a1").unwrap().tier, 2);
    }
}
