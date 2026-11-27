//! Finite state machines for Quod (`Project`, `Epoch`, `Route`, `Task`, `Submission`).

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Top-level status of a mathematical `Project`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProjectStatus {
    Active,
    Proved,
    Archived,
}

/// Lifecycle states of an `Epoch` aggregate (`PendingAudit -> Active -> Freezing -> Migrating -> Archived`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EpochState {
    PendingAudit,
    Active,
    Freezing,
    Migrating,
    Archived,
}

/// Events driving the `Epoch` state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpochEvent {
    ApproveAudit,
    RejectAudit,
    BeginFreeze,
    StartMigration,
    CompleteArchive,
}

/// Status of an orchestration `Route` grouping tasks toward a goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RouteState {
    Active,
    Won,
    Pruned,
    Refuted,
}

/// Tactical role of a pure `Prop` task inside a `Route`-grouped Task DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRole {
    Root,
    Lemma,
    Connector,
    Merge,
    Refutation,
    AuxLemma,
    WitnessBundle,
    Porting,
}

/// Lifecycle states of a pure `Prop` `Task` aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskState {
    Blocked,
    Open,
    Racing,
    StalledSoft,
    Proved,
    RefutedHard,
    Pruned,
    StaleEpoch,
}

/// Events driving the `Task` state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEvent {
    UnblockDependencies,
    AcquireLease,
    AllLeasesExpired,
    AcceptProof,
    HardRefuted,
    Prune,
    FreezeEpoch,
    MigrateToNewEpoch,
}

/// Kind of Lean artifact submitted into the verification pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionKind {
    Proof,
    Refutation,
    DefOverlay,
}

/// Verification pipeline states of a `Submission` aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SubmissionState {
    SealedReceived,
    Stage0SpecOk,
    Stage1KernelOk,
    Stage2HygieneOk,
    Merged,
    RejectedSpecDrift,
    RejectedSecurity,
    RejectedKernel,
    RejectedHygiene,
    RejectedResourceLimit,
    RejectedRaceLost,
    StaleEpoch,
}

/// Events driving the `Submission` verification state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionEvent {
    PassStage0,
    FailStage0SpecDrift,
    FailStage0Security,
    PassStage1Kernel,
    FailStage1Kernel,
    FailStage1ResourceLimit,
    PassStage2Hygiene,
    FailStage2Hygiene,
    MergeWinner,
    LoseRace,
    FreezeEpoch,
}

/// Kind of directed dependency edge in the `E1` Task DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    DependsOn,
    Refutes,
}

/// Layer classification of an append-only verified Lean module in `E1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayKind {
    Spec,
    DefOverlay,
    ProvedTask,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("invalid transition in {machine}: state {from} cannot handle event {event}")]
pub struct TransitionError {
    pub machine: &'static str,
    pub from: String,
    pub event: String,
}

impl TransitionError {
    fn new(machine: &'static str, from: impl std::fmt::Debug, event: impl std::fmt::Debug) -> Self {
        Self {
            machine,
            from: format!("{from:?}"),
            event: format!("{event:?}"),
        }
    }
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Proved | Self::RefutedHard | Self::Pruned | Self::StaleEpoch
        )
    }

    pub fn transition(self, event: TaskEvent) -> Result<Self, TransitionError> {
        use TaskEvent::*;
        use TaskState::*;

        let next = match (self, event) {
            (Blocked, UnblockDependencies) => Open,
            (Open | Racing | StalledSoft, AcquireLease) => Racing,
            (Racing, AllLeasesExpired) => StalledSoft,
            (Open | Racing | StalledSoft, AcceptProof) => Proved,
            (Blocked | Open | Racing | StalledSoft, HardRefuted) => RefutedHard,
            (Blocked | Open | Racing | StalledSoft, Prune) => Pruned,
            (Blocked | Open | Racing | StalledSoft, FreezeEpoch) => StaleEpoch,
            (StaleEpoch, MigrateToNewEpoch) => Open,
            _ => return Err(TransitionError::new("Task", self, event)),
        };
        Ok(next)
    }
}

impl SubmissionState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Merged
                | Self::RejectedSpecDrift
                | Self::RejectedSecurity
                | Self::RejectedKernel
                | Self::RejectedHygiene
                | Self::RejectedResourceLimit
                | Self::RejectedRaceLost
                | Self::StaleEpoch
        )
    }

    pub fn transition(self, event: SubmissionEvent) -> Result<Self, TransitionError> {
        use SubmissionEvent::*;
        use SubmissionState::*;

        let next = match (self, event) {
            (SealedReceived, PassStage0) => Stage0SpecOk,
            (SealedReceived, FailStage0SpecDrift) => RejectedSpecDrift,
            (SealedReceived, FailStage0Security) => RejectedSecurity,
            (Stage0SpecOk, PassStage1Kernel) => Stage1KernelOk,
            (Stage0SpecOk, FailStage1Kernel) => RejectedKernel,
            (Stage0SpecOk, FailStage1ResourceLimit) => RejectedResourceLimit,
            (Stage1KernelOk, PassStage2Hygiene) => Stage2HygieneOk,
            (Stage1KernelOk, FailStage2Hygiene) => RejectedHygiene,
            (Stage2HygieneOk, MergeWinner) => Merged,
            (Stage2HygieneOk, LoseRace) => RejectedRaceLost,
            (SealedReceived | Stage0SpecOk | Stage1KernelOk | Stage2HygieneOk, FreezeEpoch) => {
                StaleEpoch
            }
            _ => return Err(TransitionError::new("Submission", self, event)),
        };
        Ok(next)
    }
}

impl EpochState {
    pub fn transition(self, event: EpochEvent) -> Result<Self, TransitionError> {
        use EpochEvent::*;
        use EpochState::*;

        let next = match (self, event) {
            (PendingAudit, ApproveAudit) => Active,
            (PendingAudit, RejectAudit) => Archived,
            (Active, BeginFreeze) => Freezing,
            (Freezing, StartMigration) => Migrating,
            (Migrating, CompleteArchive) => Archived,
            _ => return Err(TransitionError::new("Epoch", self, event)),
        };
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_happy_path_unblock_and_freeze() {
        let s = TaskState::Blocked
            .transition(TaskEvent::UnblockDependencies)
            .unwrap()
            .transition(TaskEvent::AcquireLease)
            .unwrap()
            .transition(TaskEvent::AcceptProof)
            .unwrap();
        assert_eq!(s, TaskState::Proved);
        assert!(s.is_terminal());

        let migrated = TaskState::Racing
            .transition(TaskEvent::FreezeEpoch)
            .unwrap()
            .transition(TaskEvent::MigrateToNewEpoch)
            .unwrap();
        assert_eq!(migrated, TaskState::Open);

        assert!(TaskState::Proved.transition(TaskEvent::AcquireLease).is_err());
        assert!(TaskState::Blocked.transition(TaskEvent::AcquireLease).is_err());
    }

    #[test]
    fn epoch_and_submission_transitions() {
        let active = EpochState::PendingAudit
            .transition(EpochEvent::ApproveAudit)
            .unwrap();
        assert_eq!(active, EpochState::Active);

        let archived = active
            .transition(EpochEvent::BeginFreeze)
            .unwrap()
            .transition(EpochEvent::StartMigration)
            .unwrap()
            .transition(EpochEvent::CompleteArchive)
            .unwrap();
        assert_eq!(archived, EpochState::Archived);
        assert!(archived.transition(EpochEvent::BeginFreeze).is_err());

        let merged = SubmissionState::SealedReceived
            .transition(SubmissionEvent::PassStage0)
            .unwrap()
            .transition(SubmissionEvent::PassStage1Kernel)
            .unwrap()
            .transition(SubmissionEvent::PassStage2Hygiene)
            .unwrap()
            .transition(SubmissionEvent::MergeWinner)
            .unwrap();
        assert_eq!(merged, SubmissionState::Merged);
    }
}
