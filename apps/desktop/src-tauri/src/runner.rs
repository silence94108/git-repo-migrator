//! The worker pool that turns a started batch into real Git work.
//!
//! This is the only place in the desktop crate that owns threads, a workspace
//! directory or a Git process. It sits behind `ports::BatchLauncher`, so the
//! command tests can exercise a whole batch lifecycle without spawning
//! anything, and the renderer has no way to reach it at all.
//!
//! Workers exit as soon as a batch has no runnable row left. Restarting them is
//! `launch`'s job, which `AppState` calls on start, on resume and on retry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use git_repo_migrator_application::executor::{
    ExecutionStage, ModuleReport, StageExecutor, StageRecorder, TargetGateway, TaskAssignment,
};
use git_repo_migrator_application::planning::TargetState;
use git_repo_migrator_application::verification::{AggregateStatus, VerificationResult};
use git_repo_migrator_application::{BatchControl, IpcError};
use git_repo_migrator_domain::ErrorCategory;
use git_repo_migrator_git_runner::{GitExecutable, GitRunner, RunOptions};
use git_repo_migrator_workspace::Workspace;

use crate::dto::{CleanupState, MigrationStage};
use crate::errors;
use crate::events::{self, EventSink};
use crate::ports::BatchLauncher;
use crate::snapshot::VerifySummary;
use crate::state::{AppState, ModuleOutcome};

/// How long a worker waits for the state handle before giving up. Only reached
/// while the application is shutting down.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(50);

fn migration_stage(stage: ExecutionStage) -> MigrationStage {
    match stage {
        ExecutionStage::Preflight => MigrationStage::Preflight,
        ExecutionStage::PrepareTarget => MigrationStage::PrepareTarget,
        ExecutionStage::Git => MigrationStage::Git,
        ExecutionStage::Lfs => MigrationStage::Lfs,
        ExecutionStage::Metadata => MigrationStage::Metadata,
        ExecutionStage::PlatformData => MigrationStage::PlatformData,
        ExecutionStage::Verify => MigrationStage::Verify,
        ExecutionStage::Complete => MigrationStage::Complete,
    }
}

fn status_text(status: AggregateStatus) -> &'static str {
    match status {
        AggregateStatus::Succeeded => "succeeded",
        AggregateStatus::Partial => "partial",
        AggregateStatus::Skipped => "skipped",
        AggregateStatus::Failed | AggregateStatus::RetryableFailed => "retryable_failed",
    }
}

/// Bridges the executor's stage reports into SQLite writes and UI events.
///
/// Every method writes the durable row first and only then emits; a dropped
/// event leaves the UI stale, never wrong.
pub struct AppRecorder {
    state: Arc<AppState>,
    events: Arc<dyn EventSink>,
    batch_id: String,
}

impl AppRecorder {
    pub fn new(state: Arc<AppState>, events: Arc<dyn EventSink>, batch_id: String) -> Self {
        Self {
            state,
            events,
            batch_id,
        }
    }

    fn revision(&self) -> u64 {
        self.state.revision()
    }
}

impl StageRecorder for AppRecorder {
    fn begin(&self, task_id: &str, stage: ExecutionStage, owner: &str) -> Result<(), IpcError> {
        self.state
            .begin_stage(task_id, migration_stage(stage), owner)?;
        self.events.emit(&events::stage_changed(
            self.revision(),
            &self.batch_id,
            task_id,
            migration_stage(stage),
        ));
        Ok(())
    }

    fn progress(
        &self,
        task_id: &str,
        stage: ExecutionStage,
        owner: &str,
        completed: u64,
        total: Option<u64>,
    ) -> Result<(), IpcError> {
        self.state
            .report_progress(task_id, migration_stage(stage), owner, completed, total)?;
        self.events.emit(&events::progress(
            self.revision(),
            &self.batch_id,
            task_id,
            completed,
            total,
        ));
        Ok(())
    }

    fn fail(
        &self,
        task_id: &str,
        stage: ExecutionStage,
        owner: &str,
        error: &IpcError,
    ) -> Result<(), IpcError> {
        self.state
            .fail_stage(task_id, migration_stage(stage), owner, error)?;
        self.events.emit(&events::warning(
            self.revision(),
            &self.batch_id,
            task_id,
            error,
        ));
        Ok(())
    }

    fn module(&self, task_id: &str, report: &ModuleReport) -> Result<(), IpcError> {
        self.state.record_module_result(
            task_id,
            &ModuleOutcome {
                module: &report.module,
                fidelity: report.fidelity,
                source_count: report.source_count,
                target_count: report.target_count,
                error: report.error.as_ref(),
                source_links: &report.source_links,
            },
        )?;
        if let Some(error) = &report.error {
            // A degraded module is a warning, not a task failure: the operator
            // has to see it without the repository being marked as broken.
            self.events.emit(&events::warning(
                self.revision(),
                &self.batch_id,
                task_id,
                error,
            ));
        }
        Ok(())
    }

    fn complete(
        &self,
        task_id: &str,
        owner: &str,
        result: &VerificationResult,
    ) -> Result<(), IpcError> {
        let summary = VerifySummary {
            git_verified: result.git_ok,
            lfs_verified: result.lfs_ok,
            metadata_verified: result.metadata_ok,
            evidence: result.evidence.clone(),
            unmapped_fields: Vec::new(),
            archive_path: None,
            next_action: None,
        };
        self.state
            .complete_task(task_id, owner, &summary, result.status)?;
        self.events.emit(&events::task_completed(
            self.revision(),
            &self.batch_id,
            task_id,
            status_text(result.status),
            result.fidelity.clone(),
        ));
        Ok(())
    }

    fn control(&self, batch_id: &str) -> BatchControl {
        self.state.batch_control(batch_id)
    }

    fn cleanup(&self, _task_id: &str, path: &Path, failure: Option<&str>) {
        let state = match failure {
            None => CleanupState::Cleaned,
            Some(reason) => CleanupState::CleanupFailed {
                path: path.display().to_string(),
                reason: reason.to_owned(),
            },
        };
        self.state.set_cleanup_state(state);
    }
}

/// One batch's live workers.
struct BatchHandle {
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicUsize>,
}

/// Spawns one OS thread per configured worker. Git work is blocking and
/// process-bound, so threads rather than an async runtime keep the cancel and
/// timeout semantics of `GitRunner` intact.
pub struct ThreadPoolLauncher {
    state: Weak<AppState>,
    events: Arc<dyn EventSink>,
    workspace_root: PathBuf,
    batches: Mutex<HashMap<String, BatchHandle>>,
}

impl ThreadPoolLauncher {
    pub fn new(
        state: Weak<AppState>,
        events: Arc<dyn EventSink>,
        workspace_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            state,
            events,
            workspace_root: workspace_root.into(),
            batches: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, BatchHandle>> {
        match self.batches.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl BatchLauncher for ThreadPoolLauncher {
    fn launch(&self, batch_id: &str, concurrency: u16) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut batches = self.lock();
        if let Some(handle) = batches.get(batch_id) {
            if handle.running.load(Ordering::Acquire) > 0 {
                // Already running: resume and retry both call this, and neither
                // may double the worker count.
                handle.cancel.store(false, Ordering::Release);
                return;
            }
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicUsize::new(usize::from(concurrency.max(1))));
        batches.insert(
            batch_id.to_owned(),
            BatchHandle {
                cancel: Arc::clone(&cancel),
                running: Arc::clone(&running),
            },
        );
        drop(batches);

        self.events
            .emit(&events::batch_started(state.revision(), batch_id));

        for index in 0..usize::from(concurrency.max(1)) {
            let worker = Worker {
                state: Arc::clone(&state),
                events: Arc::clone(&self.events),
                batch_id: batch_id.to_owned(),
                owner: format!("worker-{batch_id}-{index}"),
                workspace_root: self.workspace_root.clone(),
                cancel: Arc::clone(&cancel),
                running: Arc::clone(&running),
            };
            // A worker that cannot be spawned must not leave the counter high,
            // or the batch would look busy forever.
            if std::thread::Builder::new()
                .name(format!("migrator-{index}"))
                .spawn(move || worker.run())
                .is_err()
            {
                running.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }

    fn cancel(&self, batch_id: &str) {
        if let Some(handle) = self.lock().get(batch_id) {
            handle.cancel.store(true, Ordering::Release);
        }
    }
}

/// Target facts for the executor, read with the same `git ls-remote` probe the
/// preflight page uses. Creation is not wired to a platform API yet, so it
/// returns an actionable refusal instead of guessing.
struct GitTargetGateway {
    probe: crate::ports::GitLsRemoteProbe,
}

impl TargetGateway for GitTargetGateway {
    fn probe(&self, target_url: &str) -> Result<TargetState, IpcError> {
        crate::ports::TargetProbe::probe(&self.probe, target_url)
    }

    fn create(&self, _assignment: &TaskAssignment) -> Result<(), IpcError> {
        Err(errors::unsupported(
            "prepare_target",
            "本版本不会自动创建目标仓库",
            "请先在目标平台手动建库（保持为空），然后重新预检并选择「复用空仓库」",
        ))
    }
}

struct Worker {
    state: Arc<AppState>,
    events: Arc<dyn EventSink>,
    batch_id: String,
    owner: String,
    workspace_root: PathBuf,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicUsize>,
}

impl Worker {
    fn run(self) {
        let executor = match self.build_executor() {
            Ok(executor) => Some(executor),
            Err(error) => {
                // No usable Git: fail every claimed repository with an
                // actionable error instead of leaving the queue at `planned`.
                self.drain_with_error(&error);
                None
            }
        };
        if let Some(executor) = executor {
            self.work(&executor);
        }
        if self.running.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.announce_end();
        }
    }

    fn work(&self, executor: &StageExecutor) {
        while !self.cancel.load(Ordering::Acquire) {
            match self.state.claim_next_task(&self.batch_id, &self.owner) {
                Ok(Some(assignment)) => {
                    executor.run(&assignment);
                }
                // Nothing runnable left, or the batch is no longer running.
                Ok(None) => break,
                Err(_) => {
                    std::thread::sleep(SHUTDOWN_GRACE);
                    break;
                }
            }
        }
    }

    fn drain_with_error(&self, error: &IpcError) {
        while let Ok(Some(assignment)) = self.state.claim_next_task(&self.batch_id, &self.owner) {
            let _ =
                self.state
                    .fail_stage(&assignment.task_id, MigrationStage::Git, &self.owner, error);
            self.events.emit(&events::warning(
                self.state.revision(),
                &self.batch_id,
                &assignment.task_id,
                error,
            ));
        }
    }

    fn announce_end(&self) {
        let control = self.state.batch_control(&self.batch_id);
        let status = match control {
            BatchControl::Completed => "completed",
            BatchControl::Cancelled => "cancelled",
            BatchControl::Paused => "paused",
            BatchControl::Running => "running",
        };
        self.events.emit(&events::batch_completed(
            self.state.revision(),
            &self.batch_id,
            status,
        ));
    }

    fn build_executor(&self) -> Result<StageExecutor, IpcError> {
        std::fs::create_dir_all(&self.workspace_root).map_err(|error| {
            errors::error(
                "workspace.io",
                ErrorCategory::Disk,
                true,
                "git",
                format!("无法创建工作区目录：{error}"),
                "请确认应用数据目录可写后重试",
            )
        })?;
        let workspace = Workspace::new(&self.workspace_root).map_err(|error| {
            errors::error(
                "workspace.io",
                ErrorCategory::Disk,
                true,
                "git",
                format!("工作区不可用：{error}"),
                "请确认应用数据目录可写后重试",
            )
        })?;
        let git = GitRunner::system().map_err(|error| {
            errors::error(
                "git.missing",
                ErrorCategory::Validation,
                false,
                "git",
                format!("找不到可用的 Git：{error}"),
                "请安装 Git for Windows 并确认 git.exe 在 PATH 中，然后重试该批次",
            )
        })?;

        let lfs_available = detect_lfs(&git);
        let git = if lfs_available {
            git.clone()
                .with_lfs(lfs_executable())
                .unwrap_or_else(|_| git.clone())
        } else {
            git
        };

        let recorder = Arc::new(AppRecorder::new(
            Arc::clone(&self.state),
            Arc::clone(&self.events),
            self.batch_id.clone(),
        ));
        let mut executor = StageExecutor::new(git, workspace, recorder, self.owner.clone())
            .with_cancel(Arc::clone(&self.cancel))
            .with_lfs(lfs_available);
        // Without a usable probe the executor keeps the target state unknown and
        // blocks before any write, which is the safe direction.
        if let Ok(probe) = crate::ports::GitLsRemoteProbe::system() {
            executor = executor.with_target_gateway(Arc::new(GitTargetGateway { probe }));
        }
        Ok(executor)
    }
}

fn lfs_executable() -> &'static str {
    if cfg!(windows) {
        "git-lfs.exe"
    } else {
        "git-lfs"
    }
}

/// Probes for `git-lfs` once per worker. Absence is a degradation the report
/// has to show, not a crash, so the result is a plain boolean.
fn detect_lfs(git: &GitRunner) -> bool {
    let Ok(runner) = git.clone().with_lfs(lfs_executable()) else {
        return false;
    };
    runner
        .run_executable(
            GitExecutable::GitLfs,
            &["version".to_owned()],
            RunOptions {
                timeout: Duration::from_secs(10),
                ..RunOptions::default()
            },
        )
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_executor_stage_maps_to_a_dto_stage() {
        for (stage, expected) in [
            (ExecutionStage::Preflight, MigrationStage::Preflight),
            (ExecutionStage::PrepareTarget, MigrationStage::PrepareTarget),
            (ExecutionStage::Git, MigrationStage::Git),
            (ExecutionStage::Lfs, MigrationStage::Lfs),
            (ExecutionStage::Metadata, MigrationStage::Metadata),
            (ExecutionStage::PlatformData, MigrationStage::PlatformData),
            (ExecutionStage::Verify, MigrationStage::Verify),
            (ExecutionStage::Complete, MigrationStage::Complete),
        ] {
            assert_eq!(migration_stage(stage), expected);
            // The two enums are declared in different crates; the wire value is
            // what actually has to agree.
            assert_eq!(
                serde_json::to_value(stage).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
    }

    #[test]
    fn a_failed_aggregate_is_reported_as_retryable_not_as_success() {
        assert_eq!(status_text(AggregateStatus::Succeeded), "succeeded");
        assert_eq!(status_text(AggregateStatus::Partial), "partial");
        assert_eq!(status_text(AggregateStatus::Skipped), "skipped");
        assert_eq!(status_text(AggregateStatus::Failed), "retryable_failed");
        assert_eq!(
            status_text(AggregateStatus::RetryableFailed),
            "retryable_failed"
        );
    }
}
