//! Queue fault contract (CM-007, T-015).
//!
//! The orchestrator is the only thing that decides whether a repository gets
//! touched again. These tests pin the failure directions that would otherwise
//! be invisible until a real migration: work scheduled after a pause or a
//! cancel, a non-retryable failure quietly requeued, or a finished repository
//! resumed after a crash and therefore created or pushed twice.

use git_repo_migrator_application::orchestrator::{
    BatchControl, Orchestrator, QueueTask, RetryDecision,
};
use git_repo_migrator_application::recovery::{recover, RecoveryAction};
use git_repo_migrator_domain::RepoTaskState;

fn task(id: &str, state: RepoTaskState, retryable: bool) -> QueueTask {
    QueueTask {
        id: id.to_owned(),
        state,
        attempt: 0,
        retryable,
    }
}

fn planned(id: &str) -> QueueTask {
    task(id, RepoTaskState::Planned, true)
}

#[test]
fn pause_stops_scheduling_while_work_remains() {
    let mut queue = Orchestrator::new([planned("a"), planned("b")]);
    let first = queue.next_task().expect("first task starts");
    assert_eq!(first.id, "a");

    queue.pause();
    assert_eq!(queue.control(), BatchControl::Paused);
    assert!(
        queue.next_task().is_none(),
        "a paused batch must not open a new stage"
    );

    // The in-flight repository still reports its own outcome; pausing is not
    // cancelling, so the batch stays paused with work left over.
    queue.complete("a", RepoTaskState::Succeeded);
    assert_eq!(queue.control(), BatchControl::Paused);
    assert!(queue.next_task().is_none());

    queue.resume();
    assert_eq!(queue.next_task().map(|item| item.id), Some("b".to_owned()));
}

#[test]
fn resume_is_only_valid_from_paused() {
    let mut queue = Orchestrator::new([planned("a")]);
    queue.cancel();
    queue.resume();
    assert_eq!(
        queue.control(),
        BatchControl::Cancelled,
        "resume must never revive a cancelled batch"
    );
    assert!(queue.next_task().is_none());
}

#[test]
fn cancel_stops_scheduling_and_blocks_every_retry() {
    let mut queue = Orchestrator::new([planned("a"), planned("b")]);
    queue.next_task().expect("a starts");
    queue.complete("a", RepoTaskState::RetryableFailed);

    queue.cancel();
    assert!(queue.next_task().is_none());
    assert!(
        !queue.retry("a", RetryDecision::Retry),
        "a cancelled batch must not schedule another attempt"
    );
    assert_eq!(queue.task("a").map(|item| item.attempt), Some(0));
}

#[test]
fn cancel_never_rolls_back_a_repository_that_already_finished() {
    let mut queue = Orchestrator::new([planned("a"), planned("b")]);
    queue.next_task();
    queue.complete("a", RepoTaskState::Succeeded);

    queue.cancel();

    assert_eq!(
        queue.task("a").map(|item| item.state),
        Some(RepoTaskState::Succeeded),
        "cancelling stops future work; it does not undo a completed migration"
    );
    assert_eq!(
        queue.task("b").map(|item| item.state),
        Some(RepoTaskState::Planned)
    );
}

#[test]
fn a_non_retryable_failure_is_never_requeued() {
    // Auth and permission failures come back with `retryable: false`; retrying
    // them just burns rate limit and hides the real fix from the operator.
    let mut queue = Orchestrator::new([task("a", RepoTaskState::Planned, false), planned("b")]);
    queue.next_task().expect("a starts");
    queue.complete("a", RepoTaskState::Skipped);

    assert!(!queue.retry("a", RetryDecision::Retry));
    assert_eq!(
        queue.task("a").map(|item| item.state),
        Some(RepoTaskState::Skipped)
    );
    assert_eq!(
        queue.next_task().map(|item| item.id),
        Some("b".to_owned()),
        "the rest of the batch keeps running"
    );
    assert!(queue.next_task().is_none());
}

#[test]
fn a_do_not_retry_decision_is_honoured_for_a_retryable_task() {
    let mut queue = Orchestrator::new([planned("a"), planned("b")]);
    queue.next_task().expect("a starts");
    queue.complete("a", RepoTaskState::RetryableFailed);

    assert!(!queue.retry("a", RetryDecision::DoNotRetry));
    assert_eq!(queue.task("a").map(|item| item.attempt), Some(0));
    assert_eq!(queue.next_task().map(|item| item.id), Some("b".to_owned()));
    assert!(queue.next_task().is_none());
}

#[test]
fn retry_requeues_the_task_once_per_call_and_counts_the_attempt() {
    let mut queue = Orchestrator::new([planned("a")]);
    queue.next_task().expect("a starts");
    queue.complete("a", RepoTaskState::RetryableFailed);
    assert_eq!(queue.control(), BatchControl::Completed);

    assert!(queue.retry("a", RetryDecision::Retry));
    assert_eq!(queue.task("a").map(|item| item.attempt), Some(1));
    assert_eq!(
        queue.control(),
        BatchControl::Running,
        "retrying from the report page must reopen a batch that ran dry"
    );

    assert_eq!(queue.next_task().map(|item| item.id), Some("a".to_owned()));
    assert!(
        queue.next_task().is_none(),
        "one retry must enqueue exactly one attempt"
    );
}

#[test]
fn retry_does_not_restart_a_paused_batch() {
    let mut queue = Orchestrator::new([planned("a"), planned("b")]);
    queue.next_task().expect("a starts");
    queue.complete("a", RepoTaskState::RetryableFailed);
    queue.pause();

    assert!(queue.retry("a", RetryDecision::Retry));
    assert_eq!(queue.control(), BatchControl::Paused);
    assert!(
        queue.next_task().is_none(),
        "a retry queues the attempt; only resume may start it"
    );
}

#[test]
fn retrying_an_unknown_task_creates_nothing() {
    let mut queue = Orchestrator::new([planned("a")]);
    assert!(!queue.retry("ghost", RetryDecision::Retry));
    assert!(queue.task("ghost").is_none());
    assert_eq!(queue.next_task().map(|item| item.id), Some("a".to_owned()));
    assert!(queue.next_task().is_none());
}

#[test]
fn the_batch_completes_only_after_the_last_active_task_reports() {
    let mut queue = Orchestrator::new([planned("a"), planned("b")]);
    queue.next_task();
    queue.next_task();
    assert_eq!(queue.control(), BatchControl::Running);

    queue.complete("a", RepoTaskState::Succeeded);
    assert_eq!(
        queue.control(),
        BatchControl::Running,
        "a still-running repository must keep the batch open"
    );

    queue.complete("b", RepoTaskState::Partial);
    assert_eq!(queue.control(), BatchControl::Completed);
}

#[test]
fn completing_a_task_twice_does_not_reopen_the_queue() {
    // A crash between the remote write and the checkpoint write replays the
    // completion; the second call must be a no-op rather than new work.
    let mut queue = Orchestrator::new([planned("a")]);
    queue.next_task();
    queue.complete("a", RepoTaskState::Succeeded);
    queue.complete("a", RepoTaskState::Succeeded);

    assert_eq!(queue.control(), BatchControl::Completed);
    assert!(queue.next_task().is_none());
    assert_eq!(
        queue.task("a").map(|item| item.state),
        Some(RepoTaskState::Succeeded)
    );
}

#[test]
fn a_changed_plan_hash_sends_every_task_back_to_preflight() {
    let tasks = [planned("a"), task("b", RepoTaskState::Succeeded, false)];
    let decisions = recover(&tasks, false, true);

    assert_eq!(decisions.len(), 2);
    for decision in &decisions {
        assert_eq!(
            decision.action,
            RecoveryAction::Repreflight,
            "an edited plan must never be resumed straight into a remote write"
        );
        assert!(!decision.reason.is_empty());
    }
}

#[test]
fn a_stale_capability_snapshot_sends_every_task_back_to_preflight() {
    let tasks = [planned("a")];
    let decisions = recover(&tasks, true, false);
    assert_eq!(decisions[0].action, RecoveryAction::Repreflight);
}

#[test]
fn finished_repositories_are_reviewed_manually_instead_of_resumed() {
    // Resuming any of these would create or push a second time.
    let tasks = [
        task("done", RepoTaskState::Succeeded, false),
        task("partial", RepoTaskState::Partial, true),
        task("skipped", RepoTaskState::Skipped, false),
    ];
    let decisions = recover(&tasks, true, true);

    for decision in &decisions {
        assert_eq!(
            decision.action,
            RecoveryAction::ManualReview,
            "{} must not be resumed automatically",
            decision.task_id
        );
    }
}

#[test]
fn unfinished_repositories_resume_from_their_checkpoint() {
    let tasks = [
        task("planned", RepoTaskState::Planned, true),
        task("git", RepoTaskState::Git, true),
        task("verifying", RepoTaskState::Verifying, true),
        task("failed", RepoTaskState::RetryableFailed, true),
    ];
    let decisions = recover(&tasks, true, true);

    assert_eq!(decisions.len(), tasks.len());
    for decision in &decisions {
        assert_eq!(decision.action, RecoveryAction::Resume);
    }
}

#[test]
fn recovery_covers_every_task_and_is_repeatable() {
    let tasks = [
        planned("a"),
        task("b", RepoTaskState::Succeeded, false),
        task("c", RepoTaskState::RetryableFailed, true),
    ];

    let first = recover(&tasks, true, true);
    let second = recover(&tasks, true, true);

    assert_eq!(
        first, second,
        "a repeated recovery scan must not drift between restarts"
    );
    let ids: Vec<&str> = first
        .iter()
        .map(|decision| decision.task_id.as_str())
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
}
