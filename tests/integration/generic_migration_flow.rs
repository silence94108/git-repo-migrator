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

use git_repo_migrator_application::archive::{ArchiveDocument, ArchiveItem};
use git_repo_migrator_application::executor::{
    ExecutionAction, ExecutionStage, ModuleGateway, ModuleReport, StageExecutor, StageRecorder,
    TargetGateway, TaskAssignment, TaskCompletion, TempDirOutcome, WorkspacePolicy,
};
use git_repo_migrator_application::orchestrator::BatchControl;
use git_repo_migrator_application::planning::TargetState;
use git_repo_migrator_application::report::{ExportFormat, Report, ReportRow};
use git_repo_migrator_application::verification::AggregateStatus;
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

/// A source repository whose working tree holds one LFS-tracked binary. The
/// file content has to look like a pointer on disk only *after* `git add`
/// clean-filtering; the raw bytes are what LFS stores server-side.
fn lfs_source_repository(runner: &GitRunner, root: &Path) -> PathBuf {
    let work = root.join("lfs-work");
    let bare = root.join("lfs-source.git");
    fs::create_dir_all(&work).expect("work dir");

    run(runner, &work, &["init", "-b", "main"]);
    run(runner, &work, &["config", "user.name", "Migrator Test"]);
    run(
        runner,
        &work,
        &["config", "user.email", "migrator@example.test"],
    );
    fs::write(
        work.join(".gitattributes"),
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    )
    .expect("gitattributes");
    // LFS rejects content that already looks like a pointer file, so the blob
    // must carry ordinary binary bytes before the clean filter runs.
    fs::write(work.join("asset.bin"), [7_u8; 4096]).expect("lfs asset");
    run(runner, &work, &["add", ".gitattributes", "asset.bin"]);
    run(runner, &work, &["commit", "-m", "lfs fixture"]);
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
    run(runner, &work, &["push", "origin", "refs/heads/main"]);
    bare
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
    completions: Mutex<Vec<TaskCompletion>>,
    failures: Mutex<Vec<(String, IpcError)>>,
    cleanups: Mutex<Vec<(String, TempDirOutcome)>>,
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
    fn cleanups(&self) -> Vec<(String, TempDirOutcome)> {
        self.cleanups.lock().expect("cleanups").clone()
    }
    fn modules(&self) -> Vec<ModuleReport> {
        self.modules.lock().expect("modules").clone()
    }
    fn completion(&self) -> Option<TaskCompletion> {
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
        completion: &TaskCompletion,
    ) -> Result<(), IpcError> {
        self.completions
            .lock()
            .expect("completions")
            .push(completion.clone());
        Ok(())
    }

    fn control(&self, _batch_id: &str) -> BatchControl {
        *self.control.lock().expect("control")
    }

    fn cleanup(&self, task_id: &str, path: &Path, outcome: TempDirOutcome) {
        self.cleanups
            .lock()
            .expect("cleanups")
            .push((format!("{task_id}:{}", path.display()), outcome));
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

/// Reports a module that only exists as a read-only archive, complete with the
/// document the executor is expected to persist.
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
            archive: Some(ArchiveDocument::new(
                "",
                "",
                "ops/alpha",
                git_repo_migrator_platform_core::PlatformModule::Issues,
                vec![ArchiveItem {
                    source_id: "1".into(),
                    source_url: "https://git.source.test/ops/alpha/issues/1".into(),
                    title: "崩溃后镜像残留".into(),
                    body: "重启批次后残留目录未被清理".into(),
                    source_author: "alice".into(),
                    state: "open".into(),
                    attachments: vec![],
                    metadata: Default::default(),
                }],
            )),
            unmapped_fields: vec!["reactions".into(), "sprints".into()],
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

/// Same as `executor`, but the runner carries the LFS executable so the
/// `lfs push` stage actually reaches `git-lfs` instead of refusing on the
/// allowlist.
fn lfs_capable_executor(
    workspace_root: &Path,
    recorder: Arc<Recorder>,
    target: Arc<dyn TargetGateway>,
    runner: GitRunner,
) -> StageExecutor {
    StageExecutor::new(
        runner,
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
        recorder
            .completion()
            .map(|result| result.verification.status),
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
    assert_eq!(
        cleanups[0].1,
        TempDirOutcome::Cleaned,
        "cleanup must not report a failure"
    );
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
    let root = workspace_root(temp.path());

    let mut task = assignment(&source, &target, ExecutionAction::ReuseEmpty);
    task.modules.issues = true;

    let recorder = Arc::new(Recorder::default());
    let execution = executor(
        &root,
        Arc::clone(&recorder),
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

    // The archive itself must reach the disk, bound to this batch and task,
    // and the completion must point at its directory (FR-011/CM-008).
    let archive_path = root
        .join("archives")
        .join("batch-1")
        .join("task-1")
        .join("issues.json");
    let contents = fs::read_to_string(&archive_path).unwrap_or_else(|error| {
        panic!(
            "archive must be persisted ({}): {error}",
            archive_path.display()
        )
    });
    assert!(contents.contains("https://git.source.test/ops/alpha/issues/1"));
    assert_eq!(
        recorder
            .completion()
            .and_then(|completion| completion.archive_dir),
        Some("archives/batch-1/task-1".to_owned()),
        "the report must mark where the archive lives"
    );
    assert_eq!(
        recorder
            .completion()
            .map(|completion| completion.unmapped_fields),
        Some(vec!["reactions".to_owned(), "sprints".to_owned()]),
        "unmapped source fields flow into the completion"
    );
    assert!(
        modules[0].archive.as_ref().expect("bound archive").task_id == "task-1",
        "the executor binds the archive to the task, not to the adapter's placeholder ids"
    );
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

/// R-9: the LFS success path with a real `git-lfs`. The fixture's pointer is
/// fetched from the source and pushed to the target, and the report counts the
/// object that actually moved — not a number LFS printed (it prints nothing on
/// stdout for `fetch`/`push --all`).
#[test]
fn lfs_objects_travel_to_the_target_when_the_tool_is_present() {
    // The absence of git-lfs is a supported environment; that case is covered
    // above, so this test skips itself instead of failing there.
    let Ok(runner) = git().clone().with_lfs(if cfg!(windows) {
        "git-lfs.exe"
    } else {
        "git-lfs"
    }) else {
        eprintln!("git-lfs not found; skipping the LFS success-path test");
        return;
    };

    let temp = tempfile::tempdir().expect("temp");
    let source = lfs_source_repository(&runner, temp.path());
    let target = empty_bare(&runner, temp.path(), "lfs-target.git");

    let mut task = assignment(&source, &target, ExecutionAction::ReuseEmpty);
    task.modules.lfs = true;

    let recorder = Arc::new(Recorder::default());
    let execution = lfs_capable_executor(
        &workspace_root(temp.path()),
        Arc::clone(&recorder),
        Arc::new(LocalTarget::new(false)),
        runner.clone(),
    )
    .with_lfs(true)
    .run(&task);

    assert_eq!(
        execution.error, None,
        "the LFS round trip must not fail the task"
    );
    assert_eq!(execution.status, AggregateStatus::Succeeded);

    let lfs = execution
        .modules
        .iter()
        .find(|report| report.module == "lfs")
        .expect("lfs module result");
    assert_eq!(lfs.fidelity, Fidelity::NativeRebuild);
    assert_eq!(lfs.source_count, 1, "one LFS object was counted");
    assert_eq!(lfs.target_count, 1);
    assert_eq!(lfs.error, None);

    // The object must genuinely live in the target's LFS store, not merely in
    // the (now deleted) mirror: verify through a fresh clone of the target.
    // The clone pins `main` because a bare target's HEAD still points at the
    // `init` default, and the pull takes no argument — a path argument would
    // be looked up as a remote *name* instead of a URL.
    let checkout = temp.path().join("verification");
    fs::create_dir_all(&checkout).expect("checkout dir");
    run(
        &runner,
        &checkout,
        &[
            "clone",
            "-b",
            "main",
            "--",
            target.to_str().expect("target path"),
            "verify",
        ],
    );
    let verify_dir = checkout.join("verify");
    run(&runner, &verify_dir, &["lfs", "pull"]);
    let content = fs::read(verify_dir.join("asset.bin")).expect("restored asset");
    assert_eq!(
        content,
        vec![7_u8; 4096],
        "the restored bytes must be the original object, not the pointer"
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
fn a_failed_task_under_reuse_policy_retains_the_mirror_and_the_report_marks_it() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let target = empty_bare(&runner, temp.path(), "target.git");
    let missing_source = temp.path().join("no-such-source.git");
    let root = workspace_root(temp.path());

    let recorder = Arc::new(Recorder::default());
    let execution = executor(
        &root,
        Arc::clone(&recorder),
        Arc::new(LocalTarget::new(false)),
    )
    .run(&assignment(
        &missing_source,
        &target,
        ExecutionAction::ReuseEmpty,
    ));

    // A missing source classifies as `git.not_found` (Conflict), whose terminal
    // state is Skipped: the operator must fix the address, not wait for a
    // retry. The retention below is what happens to the workspace either way.
    assert_eq!(execution.status, AggregateStatus::Skipped);
    let leftovers: Vec<_> = fs::read_dir(&root)
        .expect("workspace listing")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp-"))
        .collect();
    assert!(
        !leftovers.is_empty(),
        "the default Reuse policy keeps the failed mirror for inspection"
    );
    let cleanups = recorder.cleanups();
    assert_eq!(cleanups.len(), 1);
    assert_eq!(
        cleanups[0].1,
        TempDirOutcome::Retained,
        "a deliberate retention is an outcome, not a missing cleanup"
    );
}

#[test]
fn a_failed_task_under_clean_policy_deletes_the_mirror() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let target = empty_bare(&runner, temp.path(), "target.git");
    let missing_source = temp.path().join("no-such-source.git");
    let root = workspace_root(temp.path());

    let recorder = Arc::new(Recorder::default());
    let execution = executor(
        &root,
        Arc::clone(&recorder),
        Arc::new(LocalTarget::new(false)),
    )
    .with_workspace_policy(WorkspacePolicy::Clean)
    .run(&assignment(
        &missing_source,
        &target,
        ExecutionAction::ReuseEmpty,
    ));

    // Same classification as above: a missing source ends Skipped, and the
    // Clean policy still deletes the failed attempt's mirror.
    assert_eq!(execution.status, AggregateStatus::Skipped);
    let leftovers: Vec<_> = fs::read_dir(&root)
        .expect("workspace listing")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the Clean policy deletes the mirror even after a failure: {leftovers:?}"
    );
    assert_eq!(
        recorder.cleanups()[0].1,
        TempDirOutcome::Cleaned,
        "the mirror went away, so the outcome is a plain cleanup"
    );
}

#[test]
fn a_resumed_task_under_clean_policy_purges_stale_directories_of_the_same_task() {
    let temp = tempfile::tempdir().expect("temp");
    let runner = git();
    let source = source_repository(&runner, temp.path());
    let target = empty_bare(&runner, temp.path(), "target.git");
    let root = workspace_root(temp.path());

    // A crashed earlier attempt of task-1 left its mirror behind.
    let stale = root.join(".tmp-task-1-crash-leftover");
    fs::create_dir_all(&stale).expect("stale dir");
    fs::write(stale.join("packed-refs"), "stale\n").expect("stale marker");
    // Another task's leftover must survive: only this task's dirs are purged.
    let other_task = root.join(".tmp-task-2-other");
    fs::create_dir_all(&other_task).expect("other task dir");

    let mut task = assignment(&source, &target, ExecutionAction::ReuseEmpty);
    task.resumed_attempt = true;
    let execution = executor(
        &root,
        Arc::new(Recorder::default()),
        Arc::new(LocalTarget::new(false)),
    )
    .with_workspace_policy(WorkspacePolicy::Clean)
    .run(&task);

    assert_eq!(execution.error, None);
    assert!(
        !stale.exists(),
        "the resumed attempt must purge this task's stale mirror before cloning"
    );
    assert!(
        other_task.exists(),
        "purging may never touch another task's directories"
    );
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
