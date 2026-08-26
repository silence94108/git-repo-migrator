use git_repo_migrator_domain::RepoTaskState;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchControl {
    Running,
    Paused,
    Cancelled,
    Completed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueTask {
    pub id: String,
    pub state: RepoTaskState,
    pub attempt: u32,
    pub retryable: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    Retry,
    DoNotRetry,
}
pub struct Orchestrator {
    control: BatchControl,
    queue: VecDeque<String>,
    tasks: BTreeMap<String, QueueTask>,
    active: BTreeSet<String>,
}
impl Orchestrator {
    pub fn new(tasks: impl IntoIterator<Item = QueueTask>) -> Self {
        let mut map = BTreeMap::new();
        let mut q = VecDeque::new();
        for t in tasks {
            q.push_back(t.id.clone());
            map.insert(t.id.clone(), t);
        }
        Self {
            control: BatchControl::Running,
            queue: q,
            tasks: map,
            active: BTreeSet::new(),
        }
    }
    pub fn control(&self) -> BatchControl {
        self.control
    }
    pub fn pause(&mut self) {
        if self.control == BatchControl::Running {
            self.control = BatchControl::Paused;
        }
    }
    pub fn resume(&mut self) {
        if self.control == BatchControl::Paused {
            self.control = BatchControl::Running;
        }
    }
    pub fn cancel(&mut self) {
        self.control = BatchControl::Cancelled;
    }
    pub fn next_task(&mut self) -> Option<QueueTask> {
        if self.control != BatchControl::Running {
            return None;
        }
        let id = self.queue.pop_front()?;
        self.active.insert(id.clone());
        self.tasks.get(&id).cloned()
    }
    pub fn complete(&mut self, id: &str, state: RepoTaskState) {
        if let Some(t) = self.tasks.get_mut(id) {
            t.state = state;
            self.active.remove(id);
        }
        if self.queue.is_empty() && self.active.is_empty() {
            self.control = BatchControl::Completed;
        }
    }
    pub fn retry(&mut self, id: &str, decision: RetryDecision) -> bool {
        if decision != RetryDecision::Retry || self.control == BatchControl::Cancelled {
            return false;
        }
        let Some(t) = self.tasks.get_mut(id) else {
            return false;
        };
        if !t.retryable {
            return false;
        }
        t.attempt = t.attempt.saturating_add(1);
        t.state = RepoTaskState::RetryableFailed;
        self.queue.push_back(id.to_owned());
        true
    }
    pub fn task(&self, id: &str) -> Option<&QueueTask> {
        self.tasks.get(id)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pause_does_not_start_new_stage() {
        let t = QueueTask {
            id: "a".into(),
            state: RepoTaskState::Planned,
            attempt: 0,
            retryable: true,
        };
        let mut o = Orchestrator::new([t]);
        o.pause();
        assert!(o.next_task().is_none());
        o.resume();
        assert!(o.next_task().is_some());
    }
    #[test]
    fn auth_like_nonretryable_stays_done() {
        let t = QueueTask {
            id: "a".into(),
            state: RepoTaskState::RetryableFailed,
            attempt: 1,
            retryable: false,
        };
        let mut o = Orchestrator::new([t]);
        assert!(!o.retry("a", RetryDecision::Retry));
    }
}
