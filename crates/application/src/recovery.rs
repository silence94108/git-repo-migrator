use crate::orchestrator::QueueTask;
use git_repo_migrator_domain::RepoTaskState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryDecision {
    pub task_id: String,
    pub action: RecoveryAction,
    pub reason: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    Resume,
    Repreflight,
    ManualReview,
}
pub fn recover(
    tasks: &[QueueTask],
    plan_hash_matches: bool,
    capabilities_fresh: bool,
) -> Vec<RecoveryDecision> {
    tasks
        .iter()
        .map(|t| {
            let (action, reason) = if !plan_hash_matches || !capabilities_fresh {
                (RecoveryAction::Repreflight, "计划或能力快照已变化")
            } else if matches!(
                t.state,
                RepoTaskState::Succeeded | RepoTaskState::Partial | RepoTaskState::Skipped
            ) {
                (RecoveryAction::ManualReview, "任务已结束，保留结果")
            } else {
                (RecoveryAction::Resume, "检查点可恢复")
            };
            RecoveryDecision {
                task_id: t.id.clone(),
                action,
                reason: reason.into(),
            }
        })
        .collect()
}
