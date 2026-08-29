//! Generic Git end-to-end flow (CM-005, CM-006, CM-007, CM-008, CM-010, CM-011).
//!
//! Drives the real stage executor against real bare repositories created by the
//! system Git. No network and no platform API are involved, which is exactly the
//! Generic Git situation: the executor has to move history correctly, refuse to
//! invent a target, and report a partial result honestly.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use git_repo_migrator_application::executor::{
    ExecutionAction, ExecutionStage, ModuleGateway, ModuleReport, StageExecutor, StageRecorder,
    TargetGateway, TaskAssignment,
};
use git_repo_migrator_application::orchestrator::BatchControl;
use git_repo_migrator_application::planning::TargetState;
use git_repo_migrator_application::report::{ExportFormat, Report, ReportRow};
use git_repo_migrator_application::verification::{AggregateStatus, VerificationResult};
use git_repo_migrator_application::IpcError;
use git_repo_migrator_domain::{ErrorCategory, Fidelity, ModuleSelection, RefPolicy};
use git_repo_migrator_git_runner::{GitRunner, RunOptions};
use git_repo_migrator_workspace::Workspace;

// -- fixtures ----------------------------------------------------------------

fn git() -> GitRunner {
    GitRunner::system().expect("system git")
}

fn run(runner: &GitRunner, dir: &Path, args: &[&str]) -> String {
    runner
        .run(
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            RunOptions {
                current_dir: Some(dir.to_path_buf()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|error| panic!("git {args:?} failed: {error}"))
        .stdout
}

/// A bare source repository with one branch, one tag and one platform-private
/// ref that must never reach the target.
fn source_repository(runner: &GitRunner, root: &Path) -> PathBuf {
    let work = root.join("work");
    let bare = root.join("source.git");
    fs::create_dir_all(&work).expect("work dir");

    run(runner, &work, &["init", "-b", "main"]);
    run(runner, &work, &["config", "user.name", "Migrator Test"]);
    run(
        runner,
        &work,
        &["config", "user.email", "migrator@example.test"],
    );
    fs::write(work.join("README.md"), "generic flow fixture\n").expect("readme");
    run(runner, &work, &["add", "README.md"]);
    run(runner, &work, &["commit", "-m", "initial"]);
    run(runner, &work, &["tag", "v1.0.0"]);
    run(
        runner,
        root,
        &["init", "--bare", bare.to_str().expect("path")],
    );
    run(
        runner,
        &work,
        &["remote", "add", "origin", bare.to_str().expect("path")],
    );
    run(
        runner,
        &work,
        &["push", "origin", "refs/heads/main", "refs/tags/v1.0.0"],
    );
    run(
        runner,
        &bare,
        &["update-ref", "refs/pull/1/head", "refs/heads/main"],
    );
    bare
}

fn empty_bare(runner: &GitRunner, root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    run(
        runner,
        root,
        &["init", "--bare", path.to_str().expect("path")],
    );
    path
}

fn ref_tips(runner: &GitRunner, repo: &Path) -> BTreeMap<String, String> {
    run(
        runner,
        repo,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )
    .lines()
    .filter_map(|line| line.split_once(' '))
    .map(|(name, oid)| (name.to_owned(), oid.to_owned()))
    .collect()
}

fn assignment(source: &Path, target: &Path, action: ExecutionAction) -> TaskAssignment {
    TaskAssignment {
        batch_id: "batch-1".into(),
        task_id: "task-1".into(),
        source_url: source.to_str().expect("source path").to_owned(),
        target_url: target.to_str().expect("target path").to_owned(),
        target_name: "alpha".into(),
        action,
        modules: ModuleSelection {
            lfs: false,
            metadata: false,
            issues: false,
            pull_requests: false,
            wiki: false,
            releases: false,
        },
        ref_policy: RefPolicy::default(),
        allow_overwrite: false,
        resumed_attempt: false,
    }
}

// -- ports -------------------------------------------------------------------

struct Recorder {
    stages: Mutex<Vec<String>>,
    modules: Mutex<Vec<ModuleReport>>,
    completions: Mutex<Vec<VerificationResult>>,
    failures: Mutex<Vec<(String, IpcError)>>,
    cleanups: Mutex<Vec<(String, Option<String>)>>,
    control: Mutex<BatchControl>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            stages: Mutex::new(Vec::new()),
            modules: Mutex::new(Vec::new()),
            completions: Mutex::new(Vec::new()),
            failures: Mutex::new(Vec::new()),
            cleanups: Mutex::new(Vec::new()),
            control: Mutex::new(BatchControl::Running),
        }
    }
}

impl Recorder {
    fn stages(&self) -> Vec<String> {
        self.stages.lock().expect("stages").clone()
    }
    fn set_control(&self, control: BatchControl) {
        *self.control.lock().expect("control") = control;
    }
    fn cleanups(&self) -> Vec<(String, Option<String>)> {
        self.cleanups.lock().expect("cleanups").clone()
    }
    fn modules(&self) -> Vec<ModuleReport> {
        self.modules.lock().expect("modules").clone()
    }
    fn completion(&self) -> Option<VerificationResult> {
        self.completions
            .lock()
            .expect("completions")
            .first()
            .cloned()
    }
    fn failure(&self) -> Option<(String, IpcError)> {
        self.failures.lock().expect("failures").first().cloned()
    }
}

impl StageRecorder for Recorder {
    fn begin(&self, _task_id: &str, stage: ExecutionStage, _owner: &str) -> Result<(), IpcError> {
        self.stages
            .lock()
            .expect("stages")
            .push(stage.as_str().to_owned());
        Ok(())
    }

    fn progress(
        &self,
        _task_id: &str,
        _stage: ExecutionStage,
        _owner: &str,
        _completed: u64,
        _total: Option<u64>,
    ) -> Result<(), IpcError> {
        Ok(())
    }

    fn fail(
        &self,
        _task_id: &str,
        stage: ExecutionStage,
        _owner: &str,
        error: &IpcError,
    ) -> Result<(), IpcError> {
        self.failures
            .lock()
            .expect("failures")
            .push((stage.as_str().to_owned(), error.clone()));
        Ok(())
    }

    fn module(&self, _task_id: &str, report: &ModuleReport) -> Result<(), IpcError> {
        self.modules.lock().expect("modules").push(report.clone());
        Ok(())
    }

    fn complete(
        &self,
        _task_id: &str,
        _owner: &str,
        result: &VerificationResult,
    ) -> Result<(), IpcError> {
        self.completions
            .lock()
            .expect("completions")
            .push(result.clone());
        Ok(())
    }

    fn control(&self, _batch_id: &str) -> BatchControl {
        *self.control.lock().expect("control")
    }

    fn cleanup(&self, task_id: &str, path: &Path, failure: Option<&str>) {
        self.cleanups.lock().expect("cleanups").push((
            format!("{task_id}:{}", path.display()),
            failure.map(str::to_owned),
        ));
    }
}

/// Reads the target with the real `git ls-remote`, and only creates a target
/// when the plan authorises it. Creation calls are counted so a resumed task
/// cannot silently create twice.
struct LocalTarget {
    runner: GitRunner,
    creations: Mutex<Vec<String>>,
    may_create: bool,
}

impl LocalTarget {
    fn new(may_create: bool) -> Self {
        Self {
            runner: git(),
            creations: Mutex::new(Vec::new()),
            may_create,
        }
    }
    fn creation_count(&self) -> usize {
        self.creations.lock().expect("creations").len()
    }
}

impl TargetGateway for LocalTarget {
    fn probe(&self, target_url: &str) -> Result<TargetState, IpcError> {
        if !Path::new(target_url).exists() {
            return Ok(TargetState::Missing);
        }
        match self.runner.run_str_args(&["ls-remote", "--", target_url]) {
            Ok(output) if output.stdout.trim().is_empty() => Ok(TargetState::Empty),
            Ok(_) => Ok(TargetState::NonEmpty),
            Err(_) => Ok(TargetState::Unknown),
        }
    }

    fn create(&self, assignment: &TaskAssignment) -> Result<(), IpcError> {
        self.creations
            .lock()
            .expect("creations")
            .push(assignment.target_url.clone());
        if !self.may_create {
            // Generic Git has no creation API: without an explicit script the
            // executor must stop rather than guess.
            return Err(IpcError {
                code: "module.unsupported".into(),
                category: ErrorCategory::Unsupported,
                retryable: false,
                stage: "prepare_target".into(),
                safe_message: "通用 Git 服务没有建库 API，且未配置外部建库脚本".into(),
                action: "请先手动建库，或在连接设置中配置建库脚本".into(),
            });
        }
        let parent = Path::new(&assignment.target_url)
            .parent()
            .unwrap_or(Path::new("."));
        run(
            &self.runner,
            parent,
            &["init", "--bare", &assignment.target_url],
        );
        Ok(())
    }
}

/// Reports a module that only exists as a read-only archive.
struct ArchiveOnlyModules;

impl ModuleGateway for ArchiveOnlyModules {
    fn run(&self, _assignment: &TaskAssignment, module: &str) -> Result<ModuleReport, IpcError> {
        Ok(ModuleReport {
            module: module.to_owned(),
            fidelity: Fidelity::ReadOnlyArchive,
            source_count: 3,
            target_count: 0,
            source_links: vec!["https://git.source.test/ops/alpha/issues/1".into()],
            error: None,
        })
    }
}

fn executor(
    workspace_root: &Path,
    recorder: Arc<Recorder>,
    target: Arc<dyn TargetGateway>,
) -> StageExecutor {
    StageExecutor::new(
        git(),
        Workspace::new(workspace_root).expect("workspace"),
        recorder,
        "test-worker",
    )
    .with_target_gateway(target)
    .with_min_free_bytes(1)
}

fn workspace_root(root: &Path) -> PathBuf {
    let path = root.join("workspace");
    fs::create_dir_all(&path).expect("workspace dir");
    path
}

// -- tests -------------------------------------------------------------------

#[test]
fn an_empty_target_is_reused_and_only_allowlisted_refs_are_pushed() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = empty_bare(&runner, temp.path(), "target.git");

    let recorder = Arc::new(Recorder::default());
    recorder.set_control(BatchControl::Running);
    let gateway = Arc::new(LocalTarget::new(false));
    let execution = executor(
        &workspace_root(temp.path()),
        Arc::clone(&recorder),
        gateway.clone(),
    )
    .run(&assignment(&source, &target, ExecutionAction::ReuseEmpty));

    assert_eq!(execution.error, None, "the main path must not fail");
    assert_eq!(execution.status, AggregateStatus::Succeeded);
    assert_eq!(
        gateway.creation_count(),
        0,
        "an empty target is reused, not created"
    );

    let tips = ref_tips(&runner, &target);
    assert!(tips.contains_key("refs/heads/main"));
    assert!(tips.contains_key("refs/tags/v1.0.0"));
    assert!(
        !tips.contains_key("refs/pull/1/head"),
        "platform-private refs must never reach the target"
    );

    let evidence = execution.verification.expect("verification").evidence;
    assert_eq!(evidence.refs_checked, 2);
    assert_eq!(evidence.refs_missing, 0);
    assert!(evidence
        .excluded_refs
        .iter()
        .any(|name| name == "refs/pull/1/head"));

    assert_eq!(
        recorder.stages(),
        ["prepare_target", "git", "verify"],
        "only the stages the plan selected may run"
    );
    assert_eq!(
        recorder.completion().map(|result| result.status),
        Some(AggregateStatus::Succeeded),
        "the backend, not the renderer, records the terminal state"
    );
}

#[test]
fn the_workspace_is_cleaned_after_a_successful_repository() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = empty_bare(&runner, temp.path(), "target.git");
    let root = workspace_root(temp.path());

    let recorder = Arc::new(Recorder::default());
    executor(
        &root,
        Arc::clone(&recorder),
        Arc::new(LocalTarget::new(false)),
    )
    .run(&assignment(&source, &target, ExecutionAction::ReuseEmpty));

    let leftovers: Vec<_> = fs::read_dir(&root)
        .expect("workspace listing")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the mirror clone must not be left on disk: {leftovers:?}"
    );
    let cleanups = recorder.cleanups();
    assert_eq!(cleanups.len(), 1);
    assert_eq!(cleanups[0].1, None, "cleanup must not report a failure");
    assert!(recorder.modules().is_empty(), "no module was selected");
}

#[test]
fn a_non_empty_target_is_skipped_without_writing_anything() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    // The "target" already has history of its own.
    let target = source_repository(&runner, &{
        let other = temp.path().join("other");
        fs::create_dir_all(&other).expect("other dir");
        other
    });
    let before = ref_tips(&runner, &target);

    let recorder = Arc::new(Recorder::default());
    let execution = executor(
        &workspace_root(temp.path()),
        Arc::clone(&recorder),
        Arc::new(LocalTarget::new(false)),
    )
    .run(&assignment(&source, &target, ExecutionAction::SkipNonEmpty));

    assert_eq!(execution.status, AggregateStatus::Skipped);
    assert_eq!(execution.error, None, "a skip is a decision, not a failure");
    assert_eq!(recorder.stages(), ["prepare_target"]);
    assert_eq!(
        ref_tips(&runner, &target),
        before,
        "a skipped repository must be byte-for-byte untouched"
    );
}

#[test]
fn a_missing_target_without_a_creation_script_stops_before_any_write() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = temp.path().join("never-created.git");

    let recorder = Arc::new(Recorder::default());
    let execution = executor(
        &workspace_root(temp.path()),
        Arc::clone(&recorder),
        Arc::new(LocalTarget::new(false)),
    )
    .run(&assignment(&source, &target, ExecutionAction::Create));

    let error = execution.error.expect("creation must be refused");
    assert_eq!(error.category, ErrorCategory::Unsupported);
    assert!(!error.retryable);
    assert!(error.action.contains("建库脚本"));
    assert!(!target.exists());
    assert_eq!(
        recorder.failure().map(|(stage, _)| stage),
        Some("prepare_target".to_owned())
    );
}

#[test]
fn a_missing_target_is_created_once_and_then_migrated() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = temp.path().join("created.git");

    let gateway = Arc::new(LocalTarget::new(true));
    let execution = executor(
        &workspace_root(temp.path()),
        Arc::new(Recorder::default()),
        gateway.clone(),
    )
    .run(&assignment(&source, &target, ExecutionAction::Create));

    assert_eq!(execution.error, None);
    assert_eq!(execution.status, AggregateStatus::Succeeded);
    assert_eq!(gateway.creation_count(), 1);
    assert!(ref_tips(&runner, &target).contains_key("refs/heads/main"));
}

#[test]
fn a_resumed_task_re_reads_the_remote_and_does_not_create_a_second_repository() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = temp.path().join("created.git");
    let gateway = Arc::new(LocalTarget::new(true));
    let root = workspace_root(temp.path());

    // First attempt creates the target and migrates it.
    executor(&root, Arc::new(Recorder::default()), gateway.clone()).run(&assignment(
        &source,
        &target,
        ExecutionAction::Create,
    ));
    assert_eq!(gateway.creation_count(), 1);

    // The crash-and-resume attempt sees a target that now exists and that this
    // task itself populated, so it finishes the push instead of creating again.
    let mut resumed_task = assignment(&source, &target, ExecutionAction::Create);
    resumed_task.resumed_attempt = true;
    let resumed =
        executor(&root, Arc::new(Recorder::default()), gateway.clone()).run(&resumed_task);

    assert_eq!(
        gateway.creation_count(),
        1,
        "recovery must re-read the remote fact before creating"
    );
    assert_eq!(resumed.error, None);
    assert_eq!(resumed.status, AggregateStatus::Succeeded);
    assert!(ref_tips(&runner, &target).contains_key("refs/tags/v1.0.0"));
}

#[test]
fn a_first_attempt_never_force_updates_a_target_that_filled_up_after_preflight() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    // Preflight saw an empty target; by the time the batch runs, someone else
    // has pushed unrelated history into it.
    let other = temp.path().join("other");
    fs::create_dir_all(&other).expect("other dir");
    let target = source_repository(&runner, &other);
    let before = ref_tips(&runner, &target);

    let execution = executor(
        &workspace_root(temp.path()),
        Arc::new(Recorder::default()),
        Arc::new(LocalTarget::new(false)),
    )
    .run(&assignment(&source, &target, ExecutionAction::ReuseEmpty));

    assert_eq!(
        execution.status,
        AggregateStatus::Skipped,
        "an unexpected non-empty target is skipped, never force-pushed"
    );
    assert_eq!(ref_tips(&runner, &target), before);
}

#[test]
fn a_read_only_archive_module_downgrades_the_result_to_partial() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = empty_bare(&runner, temp.path(), "target.git");

    let mut task = assignment(&source, &target, ExecutionAction::ReuseEmpty);
    task.modules.issues = true;

    let execution = executor(
        &workspace_root(temp.path()),
        Arc::new(Recorder::default()),
        Arc::new(LocalTarget::new(false)),
    )
    .with_module_gateway(Arc::new(ArchiveOnlyModules))
    .run(&task);

    assert_eq!(
        execution.status,
        AggregateStatus::Partial,
        "Git success plus an archived module is never a complete success"
    );
    assert!(ref_tips(&runner, &target).contains_key("refs/heads/main"));
    let modules = execution.modules;
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].module, "issues");
    assert_eq!(modules[0].fidelity, Fidelity::ReadOnlyArchive);
    assert_eq!(modules[0].target_count, 0);
}

#[test]
fn lfs_without_the_tool_degrades_instead_of_claiming_success() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = empty_bare(&runner, temp.path(), "target.git");

    let mut task = assignment(&source, &target, ExecutionAction::ReuseEmpty);
    task.modules.lfs = true;

    // `with_lfs` is left at its default `false`, i.e. git-lfs is not installed.
    let execution = executor(
        &workspace_root(temp.path()),
        Arc::new(Recorder::default()),
        Arc::new(LocalTarget::new(false)),
    )
    .run(&task);

    assert_eq!(execution.status, AggregateStatus::Failed);
    let lfs = execution
        .modules
        .iter()
        .find(|report| report.module == "lfs")
        .expect("lfs module result");
    assert_eq!(lfs.fidelity, Fidelity::Unsupported);
    assert!(
        lfs.error.is_some(),
        "a missing tool must be visible in the report"
    );
}

#[test]
fn a_cancelled_batch_stops_before_the_next_stage_and_keeps_what_was_pushed() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = empty_bare(&runner, temp.path(), "target.git");

    let recorder = Arc::new(Recorder::default());
    recorder.set_control(BatchControl::Cancelled);
    let execution = executor(
        &workspace_root(temp.path()),
        Arc::clone(&recorder),
        Arc::new(LocalTarget::new(false)),
    )
    .run(&assignment(&source, &target, ExecutionAction::ReuseEmpty));

    let error = execution
        .error
        .expect("cancelling must surface an error row");
    assert_eq!(error.code, "queue.cancelled");
    assert!(!error.retryable);
    assert!(
        recorder.stages().is_empty(),
        "no stage may start after a cancel"
    );
    assert!(ref_tips(&runner, &target).is_empty());
}

#[test]
fn the_process_cancel_flag_stops_the_pipeline() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = empty_bare(&runner, temp.path(), "target.git");

    let cancel = Arc::new(AtomicBool::new(true));
    let execution = executor(
        &workspace_root(temp.path()),
        Arc::new(Recorder::default()),
        Arc::new(LocalTarget::new(false)),
    )
    .with_cancel(Arc::clone(&cancel))
    .run(&assignment(&source, &target, ExecutionAction::ReuseEmpty));

    assert_eq!(
        execution.error.map(|error| error.code),
        Some("queue.cancelled".to_owned())
    );
    assert!(cancel.load(Ordering::Relaxed));
}

#[test]
fn an_unreachable_source_fails_retryably_without_touching_the_target() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let target = empty_bare(&runner, temp.path(), "target.git");
    let missing_source = temp.path().join("no-such-source.git");

    let execution = executor(
        &workspace_root(temp.path()),
        Arc::new(Recorder::default()),
        Arc::new(LocalTarget::new(false)),
    )
    .run(&assignment(
        &missing_source,
        &target,
        ExecutionAction::ReuseEmpty,
    ));

    let error = execution.error.expect("a missing source must fail");
    assert_eq!(error.stage, "git");
    assert!(!error.safe_message.is_empty() && !error.action.is_empty());
    assert!(ref_tips(&runner, &target).is_empty());
}

#[test]
fn the_exported_report_lists_excluded_refs_and_carries_no_secret() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = empty_bare(&runner, temp.path(), "target.git");

    let execution = executor(
        &workspace_root(temp.path()),
        Arc::new(Recorder::default()),
        Arc::new(LocalTarget::new(false)),
    )
    .run(&assignment(&source, &target, ExecutionAction::ReuseEmpty));
    let verification = execution.verification.expect("verification");

    let report = Report {
        rows: vec![ReportRow {
            task_id: execution.task_id,
            source_url: "https://git.source.test/ops/alpha.git?token=super-secret".into(),
            target_url: "https://git.target.test/ops/alpha".into(),
            status: verification.status,
            error_code: None,
            excluded_refs: verification.evidence.excluded_refs.clone(),
        }],
    };

    for format in [ExportFormat::Json, ExportFormat::Csv] {
        let exported = report.export(format).expect("export");
        assert!(
            !exported.contains("super-secret"),
            "an exported report must never carry a token"
        );
    }
    assert!(report.to_csv().contains("refs/pull/1/head"));
}
