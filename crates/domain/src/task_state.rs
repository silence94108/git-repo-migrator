use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoTaskState {
    Planned,
    Preflighted,
    Preparing,
    Git,
    Lfs,
    Metadata,
    PlatformModules,
    Verifying,
    Succeeded,
    Partial,
    RetryableFailed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTransition {
    StartPreflight,
    PrepareTarget,
    StartGit,
    StartLfs,
    StartMetadata,
    StartPlatformModules,
    StartVerification,
    Succeed,
    Partial,
    Retry,
    Skip,
}

impl RepoTaskState {
    pub fn transition(self, event: TaskTransition) -> Option<Self> {
        use RepoTaskState::*;
        use TaskTransition::*;
        match (self, event) {
            (Planned, StartPreflight) => Some(Preflighted),
            (Preflighted, PrepareTarget) => Some(Preparing),
            (Preparing, StartGit) => Some(Git),
            (Git, StartLfs) => Some(Lfs),
            (Git, StartMetadata) => Some(Metadata),
            (Lfs, StartMetadata) => Some(Metadata),
            (Metadata, StartPlatformModules) => Some(PlatformModules),
            (PlatformModules, StartVerification) => Some(Verifying),
            (Metadata, StartVerification) => Some(Verifying),
            (Verifying, Succeed) => Some(Succeeded),
            (Verifying, TaskTransition::Partial) => Some(RepoTaskState::Partial),
            (_, Retry) => Some(RetryableFailed),
            (_, Skip) => Some(Skipped),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_illegal_transition() {
        assert_eq!(
            RepoTaskState::Succeeded.transition(TaskTransition::StartGit),
            None
        );
    }
}
