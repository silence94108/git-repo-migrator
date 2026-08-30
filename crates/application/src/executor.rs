//! The stage executor: the only thing that turns a started batch into real work.
//!
//! `orchestrator.rs` decides *whether* a repository may run; this module decides
//! *what actually happens* to it — prepare the target, mirror-clone the source,
//! push the allowlisted refs, run the optional modules, verify the result and
//! clean up. Everything that touches the outside world is a port, so the whole
//! pipeline is exercisable against local bare repositories with no network.
//!
//! Three rules are enforced here rather than left to a caller:
//!
//! * the remote fact is re-read before every write, so a resumed task never
//!   creates or overwrites a repository twice;
//! * only `RefPolicy`-allowlisted refs are ever pushed — there is no code path
//!   that reaches `push --mirror` or `--prune`;
//! * a stage that fails records the failure through the recorder before it
//!   returns, so a crash can never leave the queue claiming work is running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use git_repo_migrator_domain::{ErrorCategory, Fidelity, ModuleSelection, RefPolicy};
use git_repo_migrator_git_runner::{
    discover_refs, push_allowlisted_refs, verify_refs, GitError, GitExecutable, GitRunner,
    RefEntry, RunOptions,
};
use git_repo_migrator_workspace::{Workspace, WorkspaceError};
use serde::{Deserialize, Serialize};

use crate::archive::ArchiveDocument;
use crate::ipc_contract::IpcError;
use crate::orchestrator::BatchControl;
use crate::planning::TargetState;
use crate::verification::{AggregateStatus, VerificationEvidence, VerificationResult};

/// Stages a task moves through. The wire values match `MigrationStage` in the
/// desktop DTO layer; `stage_names_match_the_ipc_contract` in
/// `apps/desktop/src-tauri/src/contract_tests.rs` keeps the two in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStage {
    Preflight,
    PrepareTarget,
    Git,
    Lfs,
    Metadata,
    PlatformData,
    Verify,
    Complete,
}

impl ExecutionStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::PrepareTarget => "prepare_target",
            Self::Git => "git",
            Self::Lfs => "lfs",
            Self::Metadata => "metadata",
            Self::PlatformData => "platform_data",
            Self::Verify => "verify",
            Self::Complete => "complete",
        }
    }
}

/// What preflight decided to do with this repository. Values match `PlanAction`
/// in the desktop DTO layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionAction {
    Create,
    ReuseEmpty,
    SkipNonEmpty,
    Overwrite,
    Rename,
    Blocked,
}

impl ExecutionAction {
    pub fn parse(value: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(value.to_owned())).ok()
    }

    /// Whether this action is allowed to create a repository that is missing.
    fn may_create(self) -> bool {
        matches!(self, Self::Create | Self::Rename)
    }
}

/// One repository's execution input, resolved from the frozen plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAssignment {
    pub batch_id: String,
    pub task_id: String,
    pub source_url: String,
    pub target_url: String,
    pub target_name: String,
    pub action: ExecutionAction,
    pub modules: ModuleSelection,
    pub ref_policy: RefPolicy,
    /// Mirrors `ConflictPolicy::allow_overwrite`; kept explicit so the executor
    /// can never infer permission to overwrite from the action alone.
    pub allow_overwrite: bool,
    /// True when this task already reached the Git stage in an earlier attempt.
    ///
    /// It is the only thing that separates "the target is non-empty because *we*
    /// pushed to it before the crash" from "someone else wrote to the target
    /// after preflight". The first may continue; the second must not be
    /// force-updated without an explicit overwrite policy.
    pub resumed_attempt: bool,
}

impl TaskAssignment {
    /// Optional modules the operator selected, in execution order.
    pub fn selected_platform_modules(&self) -> Vec<&'static str> {
        let mut modules = Vec::new();
        if self.modules.issues {
            modules.push("issues");
        }
        if self.modules.pull_requests {
            modules.push("pull_requests");
        }
        if self.modules.wiki {
            modules.push("wiki");
        }
        if self.modules.releases {
            modules.push("releases");
        }
        modules
    }
}

/// What the operator asked to happen to the workspace (FR-011).
///
/// * `Reuse` — a failed attempt keeps its temporary mirror so the operator can
///   inspect what was fetched; the report marks the retained path.
/// * `Clean` — stale temporary directories of the same task are purged before
///   the clone, and the mirror is deleted even after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkspacePolicy {
    #[default]
    Reuse,
    Clean,
}

impl WorkspacePolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "reuse" => Some(Self::Reuse),
            "clean" => Some(Self::Clean),
            _ => None,
        }
    }
}

/// One platform module's outcome, as the executor observed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleReport {
    pub module: String,
    pub fidelity: Fidelity,
    pub source_count: u64,
    pub target_count: u64,
    pub source_links: Vec<String>,
    pub error: Option<IpcError>,
    /// The read-only archive a `ReadOnlyArchive` module produced. The executor
    /// persists it under the workspace before the task may complete; claiming a
    /// fidelity without delivering the document would be a lie.
    pub archive: Option<ArchiveDocument>,
    /// Source content that could not be mapped onto the target, with the
    /// reason. Flows into the report's unmapped-fields column.
    pub unmapped_fields: Vec<String>,
}

impl ModuleReport {
    pub fn unsupported(module: &str, reason: impl Into<String>) -> Self {
        Self {
            module: module.to_owned(),
            fidelity: Fidelity::Unsupported,
            source_count: 0,
            target_count: 0,
            source_links: Vec::new(),
            error: Some(unsupported_error(module, reason)),
            archive: None,
            unmapped_fields: Vec::new(),
        }
    }
}

/// Everything the verify stage hands to the recorder at completion time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletion {
    pub verification: VerificationResult,
    /// Workspace-relative directory holding this task's archive documents,
    /// present only when at least one module archived something.
    pub archive_dir: Option<String>,
    pub unmapped_fields: Vec<String>,
}

impl TaskCompletion {
    pub fn from_verification(verification: VerificationResult) -> Self {
        Self {
            verification,
            archive_dir: None,
            unmapped_fields: Vec::new(),
        }
    }
}

/// What happened to a task's temporary directory (FR-011: the report has to
/// show the cleanup result, including a deliberate retention).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TempDirOutcome {
    Cleaned,
    Retained,
    Failed(String),
}

/// Result of one repository's run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskExecution {
    pub task_id: String,
    pub status: AggregateStatus,
    pub stage: ExecutionStage,
    pub verification: Option<VerificationResult>,
    pub modules: Vec<ModuleReport>,
    pub error: Option<IpcError>,
}

// -- ports -------------------------------------------------------------------

/// Where stage transitions go. The desktop implementation writes SQLite rows and
/// emits events; the tests collect them in memory.
pub trait StageRecorder: Send + Sync {
    fn begin(&self, task_id: &str, stage: ExecutionStage, owner: &str) -> Result<(), IpcError>;
    fn progress(
        &self,
        task_id: &str,
        stage: ExecutionStage,
        owner: &str,
        completed: u64,
        total: Option<u64>,
    ) -> Result<(), IpcError>;
    fn fail(
        &self,
        task_id: &str,
        stage: ExecutionStage,
        owner: &str,
        error: &IpcError,
    ) -> Result<(), IpcError>;
    fn module(&self, task_id: &str, report: &ModuleReport) -> Result<(), IpcError>;
    fn complete(
        &self,
        task_id: &str,
        owner: &str,
        completion: &TaskCompletion,
    ) -> Result<(), IpcError>;
    /// Current batch control. Polled between stages so pause and cancel take
    /// effect at a checkpoint instead of mid-push.
    fn control(&self, batch_id: &str) -> BatchControl;
    /// Reports what happened to the task's temporary directory.
    fn cleanup(&self, task_id: &str, path: &Path, outcome: TempDirOutcome);
}

/// Reads and, when the plan says so, creates the target repository.
pub trait TargetGateway: Send + Sync {
    fn probe(&self, target_url: &str) -> Result<TargetState, IpcError>;
    /// Must be idempotent: a resumed task calls this only after `probe`
    /// reported `Missing`, and a second call for the same target must not
    /// produce a second repository.
    fn create(&self, assignment: &TaskAssignment) -> Result<(), IpcError>;
}

/// Runs one optional platform module. Implementations report the honest
/// fidelity; returning `NativeRebuild` for an archive is a contract violation.
pub trait ModuleGateway: Send + Sync {
    fn run(&self, assignment: &TaskAssignment, module: &str) -> Result<ModuleReport, IpcError>;
}

/// Default gateway for a service with no API: it refuses to invent a target.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPlatformApi;

impl TargetGateway for NoPlatformApi {
    fn probe(&self, _target_url: &str) -> Result<TargetState, IpcError> {
        Ok(TargetState::Unknown)
    }

    fn create(&self, _assignment: &TaskAssignment) -> Result<(), IpcError> {
        Err(unsupported_error(
            "prepare_target",
            "该服务没有建库 API，且未配置外部建库脚本",
        ))
    }
}

impl ModuleGateway for NoPlatformApi {
    fn run(&self, _assignment: &TaskAssignment, module: &str) -> Result<ModuleReport, IpcError> {
        Ok(ModuleReport::unsupported(module, "该服务没有平台数据 API"))
    }
}

// -- executor ----------------------------------------------------------------

/// Minimum free space demanded before a mirror clone starts. A clone that fills
/// the volume corrupts the workspace for every other task in the batch.
const DEFAULT_MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;

pub struct StageExecutor {
    git: GitRunner,
    workspace: Workspace,
    recorder: Arc<dyn StageRecorder>,
    target: Arc<dyn TargetGateway>,
    modules: Arc<dyn ModuleGateway>,
    owner: String,
    cancel: Arc<AtomicBool>,
    timeout: Duration,
    min_free_bytes: u64,
    lfs_available: bool,
    workspace_policy: WorkspacePolicy,
}

impl StageExecutor {
    pub fn new(
        git: GitRunner,
        workspace: Workspace,
        recorder: Arc<dyn StageRecorder>,
        owner: impl Into<String>,
    ) -> Self {
        Self {
            git,
            workspace,
            recorder,
            target: Arc::new(NoPlatformApi),
            modules: Arc::new(NoPlatformApi),
            owner: owner.into(),
            cancel: Arc::new(AtomicBool::new(false)),
            timeout: Duration::from_secs(30 * 60),
            min_free_bytes: DEFAULT_MIN_FREE_BYTES,
            lfs_available: false,
            workspace_policy: WorkspacePolicy::Reuse,
        }
    }

    pub fn with_target_gateway(mut self, gateway: Arc<dyn TargetGateway>) -> Self {
        self.target = gateway;
        self
    }

    pub fn with_module_gateway(mut self, gateway: Arc<dyn ModuleGateway>) -> Self {
        self.modules = gateway;
        self
    }

    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_min_free_bytes(mut self, bytes: u64) -> Self {
        self.min_free_bytes = bytes;
        self
    }

    /// Declares whether `git-lfs` is installed. When it is not, the LFS module
    /// degrades to `Unsupported` with a reason instead of failing the task.
    pub fn with_lfs(mut self, available: bool) -> Self {
        self.lfs_available = available;
        self
    }

    /// Sets what happens to the workspace across attempts (FR-011).
    pub fn with_workspace_policy(mut self, policy: WorkspacePolicy) -> Self {
        self.workspace_policy = policy;
        self
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// Runs one repository to a terminal state. Never panics and never returns
    /// without having recorded either a failure or a completion.
    pub fn run(&self, assignment: &TaskAssignment) -> TaskExecution {
        let mut modules = Vec::new();
        match self.execute(assignment, &mut modules) {
            Ok(execution) => execution,
            Err(StageFailure { stage, error }) => {
                let _ = self
                    .recorder
                    .fail(&assignment.task_id, stage, &self.owner, &error);
                TaskExecution {
                    task_id: assignment.task_id.clone(),
                    status: terminal_status(&error),
                    stage,
                    verification: None,
                    modules,
                    error: Some(error),
                }
            }
        }
    }

    fn execute(
        &self,
        assignment: &TaskAssignment,
        modules: &mut Vec<ModuleReport>,
    ) -> Result<TaskExecution, StageFailure> {
        let mirror = self.prepare_target(assignment)?;
        let Some(mirror) = mirror else {
            // `skip_non_empty` is a decision, not a failure: no bytes were
            // written to the target and the report says exactly why.
            let result = skipped_result();
            let completion = TaskCompletion::from_verification(result.clone());
            self.record_complete(assignment, &completion)?;
            return Ok(TaskExecution {
                task_id: assignment.task_id.clone(),
                status: AggregateStatus::Skipped,
                stage: ExecutionStage::PrepareTarget,
                verification: Some(result),
                modules: modules.clone(),
                error: None,
            });
        };

        let outcome = self.run_stages(assignment, &mirror.repo, modules);
        // Archives are persisted even when a later stage failed: the items were
        // fetched and the report may still point at them. An archive that never
        // reached disk fails the task instead of silently disappearing.
        let archive_dir = match self.persist_archives(modules) {
            Ok(dir) => dir,
            Err(failure) => {
                self.cleanup_after(assignment, &mirror.temp_root, true);
                return Err(failure);
            }
        };
        // The workspace is cleaned whether or not the stages succeeded, so a
        // failed batch does not leave the disk full for the next attempt —
        // unless the operator chose to keep failed mirrors (FR-011).
        let failed = outcome.is_err();
        self.cleanup_after(assignment, &mirror.temp_root, failed);
        let source_refs = outcome?;

        let verification = self.verify(assignment, &source_refs, modules)?;
        let completion = TaskCompletion {
            unmapped_fields: unmapped_fields(modules),
            archive_dir,
            verification: verification.clone(),
        };
        self.record_complete(assignment, &completion)?;
        Ok(TaskExecution {
            task_id: assignment.task_id.clone(),
            status: verification.status,
            stage: ExecutionStage::Complete,
            verification: Some(verification),
            modules: modules.clone(),
            error: None,
        })
    }

    /// Runs the stages that need the local mirror, returning the source refs.
    fn run_stages(
        &self,
        assignment: &TaskAssignment,
        mirror: &Path,
        modules: &mut Vec<ModuleReport>,
    ) -> Result<Vec<RefEntry>, StageFailure> {
        let source_refs = self.git_stage(assignment, mirror)?;
        self.lfs_stage(assignment, mirror, modules)?;
        self.metadata_stage(assignment, modules)?;
        self.platform_data_stage(assignment, modules)?;
        Ok(source_refs)
    }

    // -- stages --------------------------------------------------------------

    /// Re-reads the remote fact, then creates the target only when the plan says
    /// to and the remote really is missing. Returns `None` when the repository
    /// is skipped without any write.
    fn prepare_target(
        &self,
        assignment: &TaskAssignment,
    ) -> Result<Option<MirrorPaths>, StageFailure> {
        let stage = ExecutionStage::PrepareTarget;
        self.guard(assignment, stage)?;
        self.begin(assignment, stage)?;

        if assignment.action == ExecutionAction::Blocked {
            return Err(StageFailure {
                stage,
                error: conflict_error(
                    stage,
                    "预检已把该仓库标记为阻断项",
                    "请返回预检页处理阻断原因后重新冻结计划",
                ),
            });
        }
        if assignment.action == ExecutionAction::SkipNonEmpty {
            // The frozen plan is the authority: preflight already saw a
            // non-empty target and decided not to write to it.
            return Ok(None);
        }

        let state = self
            .target
            .probe(&assignment.target_url)
            .map_err(|error| StageFailure { stage, error })?;

        match state {
            TargetState::Unknown | TargetState::Inaccessible => {
                return Err(StageFailure {
                    stage,
                    error: conflict_error(
                        stage,
                        "无法确认目标仓库状态，已停止在写入之前",
                        "请检查目标地址与凭据后重新探测",
                    ),
                })
            }
            TargetState::Missing => {
                if !assignment.action.may_create() {
                    return Err(StageFailure {
                        stage,
                        error: conflict_error(
                            stage,
                            "目标仓库不存在，但计划没有授权创建",
                            "请重新预检并选择创建目标，或手动建库后重试",
                        ),
                    });
                }
                self.target
                    .create(assignment)
                    .map_err(|error| StageFailure { stage, error })?;
                // Creation is confirmed by re-reading the remote, so a lost
                // response never turns into a second repository.
                match self
                    .target
                    .probe(&assignment.target_url)
                    .map_err(|error| StageFailure { stage, error })?
                {
                    TargetState::Empty | TargetState::NonEmpty => {}
                    _ => {
                        return Err(StageFailure {
                            stage,
                            error: retryable_error(
                                "target.create_unconfirmed",
                                ErrorCategory::Network,
                                stage,
                                "建库请求已发出，但目标仍不可读；未继续写入",
                                "请稍后重试；恢复时会先检查目标是否已存在，不会重复创建",
                            ),
                        })
                    }
                }
            }
            TargetState::NonEmpty if !assignment.allow_overwrite && !assignment.resumed_attempt => {
                // Content appeared between preflight and now. Continuing would
                // force-update refs nobody authorised us to touch.
                return Ok(None);
            }
            TargetState::NonEmpty | TargetState::Empty => {}
        }

        self.workspace
            .preflight_space(self.min_free_bytes)
            .map_err(|error| StageFailure {
                stage,
                error: workspace_error(stage, &error),
            })?;
        if self.workspace_policy == WorkspacePolicy::Clean {
            // Under `clean`, leftovers from a crashed attempt of *this* task are
            // removed before the clone. The prefix is task-specific, so a
            // concurrent task in another worker is never touched.
            self.workspace
                .purge_temp_for(&sanitize_id(&assignment.task_id))
                .map_err(|error| StageFailure {
                    stage,
                    error: workspace_error(stage, &error),
                })?;
        }
        let temp_root = self
            .workspace
            .temp_dir(&sanitize_id(&assignment.task_id))
            .map_err(|error| StageFailure {
                stage,
                error: workspace_error(stage, &error),
            })?;
        Ok(Some(MirrorPaths {
            repo: temp_root.join("mirror.git"),
            temp_root,
        }))
    }

    fn git_stage(
        &self,
        assignment: &TaskAssignment,
        mirror: &Path,
    ) -> Result<Vec<RefEntry>, StageFailure> {
        let stage = ExecutionStage::Git;
        self.guard(assignment, stage)?;
        self.begin(assignment, stage)?;

        // A mirror clone is a complete *read*. It never implies that every ref
        // is pushed: the push refspecs below are built from the ref policy.
        self.run_git(
            &[
                "clone".to_owned(),
                "--mirror".to_owned(),
                "--".to_owned(),
                assignment.source_url.clone(),
                path_arg(mirror)?,
            ],
            None,
            stage,
        )?;
        self.report(assignment, stage, 1, Some(3))?;

        let source_refs =
            discover_refs(&self.git, mirror, &assignment.ref_policy).map_err(|error| {
                StageFailure {
                    stage,
                    error: git_error(stage, &error),
                }
            })?;
        self.report(assignment, stage, 2, Some(3))?;

        push_allowlisted_refs(
            &self.git,
            mirror,
            &assignment.target_url,
            &source_refs,
            &assignment.ref_policy,
        )
        .map_err(|error| StageFailure {
            stage,
            error: git_error(stage, &error),
        })?;
        self.report(assignment, stage, 3, Some(3))?;
        Ok(source_refs)
    }

    fn lfs_stage(
        &self,
        assignment: &TaskAssignment,
        mirror: &Path,
        modules: &mut Vec<ModuleReport>,
    ) -> Result<(), StageFailure> {
        if !assignment.modules.lfs {
            return Ok(());
        }
        let stage = ExecutionStage::Lfs;
        self.guard(assignment, stage)?;
        self.begin(assignment, stage)?;

        if !self.lfs_available {
            // Missing tooling degrades the module; it does not silently drop
            // LFS objects while reporting a complete success.
            let mut report = ModuleReport::unsupported("lfs", "本机未安装 git-lfs，LFS 对象未迁移");
            report.unmapped_fields.push("lfs_objects".to_owned());
            self.record_module(assignment, report, modules)?;
            return Ok(());
        }

        // The commands run the `git-lfs` executable directly (`run_lfs`), so
        // they carry no `lfs` prefix — that prefix belongs to `git lfs`, and
        // `git-lfs lfs fetch` is an unknown command.
        let fetch = self.run_lfs(&["fetch".to_owned(), "--all".to_owned()], Some(mirror));
        let push = fetch.and_then(|_| {
            self.run_lfs(
                &[
                    "push".to_owned(),
                    "--all".to_owned(),
                    lfs_remote_url(&assignment.target_url),
                ],
                Some(mirror),
            )
        });
        let report = match push {
            Ok(_) => {
                // `push --all` moves every fetched object, so the mirror's count
                // is both the source and the target figure. Counting the real
                // object files keeps the report honest even when LFS reports
                // nothing on stdout.
                let objects = count_lfs_objects(mirror);
                ModuleReport {
                    module: "lfs".to_owned(),
                    fidelity: Fidelity::NativeRebuild,
                    source_count: objects,
                    target_count: objects,
                    source_links: Vec::new(),
                    error: None,
                    archive: None,
                    unmapped_fields: Vec::new(),
                }
            }
            Err(error) => {
                let mut report = ModuleReport {
                    module: "lfs".to_owned(),
                    fidelity: Fidelity::Unsupported,
                    source_count: count_lfs_objects(mirror),
                    target_count: 0,
                    source_links: Vec::new(),
                    error: Some(git_error(stage, &error)),
                    archive: None,
                    unmapped_fields: Vec::new(),
                };
                report.unmapped_fields.push("lfs_objects".to_owned());
                report
            }
        };
        self.record_module(assignment, report, modules)
    }

    fn metadata_stage(
        &self,
        assignment: &TaskAssignment,
        modules: &mut Vec<ModuleReport>,
    ) -> Result<(), StageFailure> {
        if !assignment.modules.metadata {
            return Ok(());
        }
        let stage = ExecutionStage::Metadata;
        self.guard(assignment, stage)?;
        self.begin(assignment, stage)?;
        let report = self
            .modules
            .run(assignment, "metadata")
            .unwrap_or_else(|error| ModuleReport {
                module: "metadata".to_owned(),
                fidelity: Fidelity::Unsupported,
                source_count: 0,
                target_count: 0,
                source_links: Vec::new(),
                error: Some(error),
                archive: None,
                unmapped_fields: Vec::new(),
            });
        self.record_module(assignment, report, modules)
    }

    fn platform_data_stage(
        &self,
        assignment: &TaskAssignment,
        modules: &mut Vec<ModuleReport>,
    ) -> Result<(), StageFailure> {
        let selected = assignment.selected_platform_modules();
        if selected.is_empty() {
            return Ok(());
        }
        let stage = ExecutionStage::PlatformData;
        self.guard(assignment, stage)?;
        self.begin(assignment, stage)?;

        let total = u64::try_from(selected.len()).unwrap_or(0);
        for (index, module) in selected.iter().enumerate() {
            let report = self
                .modules
                .run(assignment, module)
                .unwrap_or_else(|error| ModuleReport {
                    module: (*module).to_owned(),
                    fidelity: Fidelity::Unsupported,
                    source_count: 0,
                    target_count: 0,
                    source_links: Vec::new(),
                    error: Some(error),
                    archive: None,
                    unmapped_fields: Vec::new(),
                });
            self.record_module(assignment, report, modules)?;
            let completed = u64::try_from(index + 1).unwrap_or(0);
            self.report(assignment, stage, completed, Some(total))?;
        }
        Ok(())
    }

    /// Re-reads the target and compares every allowlisted source ref against it.
    /// Nothing here trusts the push exit code alone.
    fn verify(
        &self,
        assignment: &TaskAssignment,
        source_refs: &[RefEntry],
        modules: &[ModuleReport],
    ) -> Result<VerificationResult, StageFailure> {
        let stage = ExecutionStage::Verify;
        self.guard(assignment, stage)?;
        self.begin(assignment, stage)?;

        let output = self.run_git(
            &[
                "ls-remote".to_owned(),
                "--".to_owned(),
                assignment.target_url.clone(),
            ],
            None,
            stage,
        )?;
        let target_refs = parse_ls_remote(&output);
        let verification = verify_refs(source_refs, &target_refs);

        let allowlisted = u32::try_from(
            source_refs
                .iter()
                .filter(|entry| {
                    entry.decision == git_repo_migrator_domain::RefPolicyDecision::Allow
                })
                .count(),
        )
        .unwrap_or(u32::MAX);
        let missing = u32::try_from(verification.missing.len() + verification.mismatched.len())
            .unwrap_or(u32::MAX);

        let lfs = modules.iter().find(|report| report.module == "lfs");
        let metadata = modules.iter().find(|report| report.module == "metadata");
        let evidence = VerificationEvidence {
            refs_checked: allowlisted,
            refs_missing: missing,
            lfs_checked: lfs
                .map(|report| u32::try_from(report.source_count).unwrap_or(0))
                .unwrap_or(0),
            lfs_missing: u32::from(lfs.is_some_and(|report| report.error.is_some())),
            metadata_checked: metadata.is_some_and(|report| report.error.is_none()),
            excluded_refs: verification.excluded.clone(),
        };
        let fidelity: Vec<Fidelity> = modules.iter().map(|report| report.fidelity).collect();
        Ok(VerificationResult::aggregate(
            verification.matched,
            lfs.is_none_or(|report| report.error.is_none()),
            metadata.is_none_or(|report| report.error.is_none()),
            evidence,
            fidelity,
        ))
    }

    // -- plumbing ------------------------------------------------------------

    /// Stops before a stage when the batch was paused or cancelled, or when the
    /// process-wide cancel flag was raised.
    fn guard(
        &self,
        assignment: &TaskAssignment,
        stage: ExecutionStage,
    ) -> Result<(), StageFailure> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(StageFailure {
                stage,
                error: cancelled_error(stage),
            });
        }
        match self.recorder.control(&assignment.batch_id) {
            BatchControl::Running => Ok(()),
            BatchControl::Cancelled => Err(StageFailure {
                stage,
                error: cancelled_error(stage),
            }),
            BatchControl::Paused | BatchControl::Completed => Err(StageFailure {
                stage,
                error: retryable_error(
                    "queue.paused",
                    ErrorCategory::Conflict,
                    stage,
                    "批次已暂停，当前阶段没有开始",
                    "恢复批次后会从该检查点继续",
                ),
            }),
        }
    }

    fn begin(
        &self,
        assignment: &TaskAssignment,
        stage: ExecutionStage,
    ) -> Result<(), StageFailure> {
        self.recorder
            .begin(&assignment.task_id, stage, &self.owner)
            .map_err(|error| StageFailure { stage, error })
    }

    fn report(
        &self,
        assignment: &TaskAssignment,
        stage: ExecutionStage,
        completed: u64,
        total: Option<u64>,
    ) -> Result<(), StageFailure> {
        self.recorder
            .progress(&assignment.task_id, stage, &self.owner, completed, total)
            .map_err(|error| StageFailure { stage, error })
    }

    fn record_module(
        &self,
        assignment: &TaskAssignment,
        mut report: ModuleReport,
        modules: &mut Vec<ModuleReport>,
    ) -> Result<(), StageFailure> {
        // Adapters have no batch or task ids of their own; the executor binds
        // every archive to the task that will persist it, so what the report
        // records and what lands on disk carry the same identity.
        if let Some(document) = report.archive.as_mut() {
            let repository = document.repository.clone();
            *document =
                document
                    .clone()
                    .rebind(&assignment.batch_id, &assignment.task_id, repository);
        }
        self.recorder
            .module(&assignment.task_id, &report)
            .map_err(|error| StageFailure {
                stage: ExecutionStage::PlatformData,
                error,
            })?;
        modules.push(report);
        Ok(())
    }

    fn record_complete(
        &self,
        assignment: &TaskAssignment,
        completion: &TaskCompletion,
    ) -> Result<(), StageFailure> {
        self.recorder
            .complete(&assignment.task_id, &self.owner, completion)
            .map_err(|error| StageFailure {
                stage: ExecutionStage::Complete,
                error,
            })
    }

    /// Writes every module's archive document under the workspace, returning
    /// the task's archive directory when at least one document was written.
    fn persist_archives(&self, modules: &[ModuleReport]) -> Result<Option<String>, StageFailure> {
        let mut archive_dir: Option<String> = None;
        for report in modules {
            let Some(document) = &report.archive else {
                continue;
            };
            // `ArchiveDocument::new` sanitises every field, so what reaches the
            // disk has already been through the redaction rules (CM-004).
            //
            // The report string is built from the (already safe-segmented) ids
            // with forward slashes: it is a workspace-relative path shown in the
            // report, not a filesystem operation, and a backslash would leak the
            // platform's separator into a wire value.
            let relative_dir = format!("archives/{}/{}", document.batch_id, document.task_id);
            let file_name = document
                .retention_path()
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned);
            let dir = self
                .workspace
                .child(&relative_dir)
                .map_err(|error| StageFailure {
                    stage: ExecutionStage::PlatformData,
                    error: workspace_error(ExecutionStage::PlatformData, &error),
                })?;
            let contents = serde_json::to_vec_pretty(document).map_err(|error| StageFailure {
                stage: ExecutionStage::PlatformData,
                error: IpcError {
                    code: "archive.serialize".to_owned(),
                    category: ErrorCategory::Disk,
                    retryable: false,
                    stage: ExecutionStage::PlatformData.as_str().to_owned(),
                    safe_message: format!("归档文档无法序列化：{error}"),
                    action: "请重试该仓库；已推送的 Git 数据不受影响".to_owned(),
                },
            })?;
            let Some(file_name) = file_name else {
                return Err(StageFailure {
                    stage: ExecutionStage::PlatformData,
                    error: IpcError {
                        code: "archive.path_encoding".to_owned(),
                        category: ErrorCategory::Disk,
                        retryable: false,
                        stage: ExecutionStage::PlatformData.as_str().to_owned(),
                        safe_message: "归档文件名包含无法写入的字符".to_owned(),
                        action: "请把工作区改到只含 ASCII 字符的目录后重试".to_owned(),
                    },
                });
            };
            std::fs::write(dir.join(file_name), contents).map_err(|error| StageFailure {
                stage: ExecutionStage::PlatformData,
                error: IpcError {
                    code: "archive.write".to_owned(),
                    category: ErrorCategory::Disk,
                    retryable: true,
                    stage: ExecutionStage::PlatformData.as_str().to_owned(),
                    safe_message: format!("归档文档写入失败：{error}"),
                    action: "请确认工作区可写并有足够空间后重试该仓库".to_owned(),
                },
            })?;
            archive_dir.get_or_insert(relative_dir);
        }
        Ok(archive_dir)
    }

    /// Cleans (or, per policy, retains) the task's temporary directory and
    /// records the outcome either way.
    fn cleanup_after(&self, assignment: &TaskAssignment, temp_root: &Path, failed: bool) {
        if failed && self.workspace_policy == WorkspacePolicy::Reuse {
            // FR-011: the operator chose to keep failed mirrors; the report
            // marks the retained path instead of silently deleting evidence.
            self.recorder
                .cleanup(&assignment.task_id, temp_root, TempDirOutcome::Retained);
            return;
        }
        let outcome = match self.workspace.cleanup_temp(temp_root) {
            Ok(()) => TempDirOutcome::Cleaned,
            Err(error) => TempDirOutcome::Failed(error.to_string()),
        };
        self.recorder
            .cleanup(&assignment.task_id, temp_root, outcome);
    }

    fn run_git(
        &self,
        args: &[String],
        current_dir: Option<&Path>,
        stage: ExecutionStage,
    ) -> Result<String, StageFailure> {
        self.git
            .run(args, self.options(current_dir))
            .map(|output| output.stdout)
            .map_err(|error| StageFailure {
                stage,
                error: git_error(stage, &error),
            })
    }

    fn run_lfs(&self, args: &[String], current_dir: Option<&Path>) -> Result<String, GitError> {
        self.git
            .run_executable(GitExecutable::GitLfs, args, self.options(current_dir))
            .map(|output| output.stdout)
    }

    fn options(&self, current_dir: Option<&Path>) -> RunOptions {
        let mut env = BTreeMap::new();
        // A GUI child process must never open an interactive credential prompt;
        // an unauthenticated call has to fail fast and become a visible error.
        env.insert("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned());
        RunOptions {
            timeout: self.timeout,
            cancel: Some(Arc::clone(&self.cancel)),
            current_dir: current_dir.map(Path::to_path_buf),
            env,
        }
    }
}

struct MirrorPaths {
    temp_root: PathBuf,
    repo: PathBuf,
}

struct StageFailure {
    stage: ExecutionStage,
    error: IpcError,
}

fn skipped_result() -> VerificationResult {
    VerificationResult {
        status: AggregateStatus::Skipped,
        git_ok: false,
        lfs_ok: false,
        metadata_ok: false,
        fidelity: Vec::new(),
        evidence: VerificationEvidence::default(),
    }
}

/// Fields the source carried that the target cannot hold, across all modules.
fn unmapped_fields(modules: &[ModuleReport]) -> Vec<String> {
    let mut fields: Vec<String> = modules
        .iter()
        .flat_map(|report| report.unmapped_fields.iter().cloned())
        .collect();
    fields.sort();
    fields.dedup();
    fields
}

/// Counts LFS object files inside a mirror's LFS storage, i.e. the objects that
/// were (or would have been) moved.
fn count_lfs_objects(mirror: &Path) -> u64 {
    fn visit(dir: &Path, count: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, count);
            } else {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    visit(&mirror.join("lfs").join("objects"), &mut count);
    count
}

/// The remote `git lfs push` is pointed at. A local-path target (which
/// `GenericGitUrl` deliberately accepts) has to become a `file://` URL: the
/// standalone file transfer agent refuses a bare path with "no valid file://
/// URLs found", even though every other Git command accepts it unchanged.
fn lfs_remote_url(target_url: &str) -> String {
    let path = Path::new(target_url);
    if !path.is_absolute() {
        return target_url.to_owned();
    }
    let mut url = String::from("file://");
    for component in path.components() {
        match component {
            // `C:` keeps its colon and becomes `/C:`, which is what git-lfs
            // expects on Windows; a Unix root contributes nothing extra.
            std::path::Component::Prefix(prefix) => {
                url.push('/');
                url.push_str(&prefix.as_os_str().to_string_lossy());
            }
            std::path::Component::RootDir => {}
            component => {
                url.push('/');
                url.push_str(&component.as_os_str().to_string_lossy());
            }
        }
    }
    url
}

fn terminal_status(error: &IpcError) -> AggregateStatus {
    if matches!(
        error.category,
        ErrorCategory::Permission | ErrorCategory::Conflict
    ) {
        AggregateStatus::Skipped
    } else {
        AggregateStatus::RetryableFailed
    }
}

fn parse_ls_remote(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let oid = parts.next()?;
            let name = parts.next()?;
            Some((name.to_owned(), oid.to_owned()))
        })
        .collect()
}

/// Temp directory names are derived from the task id, so anything that could
/// leave the workspace is replaced before it reaches the file system.
fn sanitize_id(task_id: &str) -> String {
    let cleaned: String = task_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if cleaned.trim_matches('-').is_empty() {
        "task".to_owned()
    } else {
        cleaned
    }
}

/// Turns a workspace path into a Git command argument.
///
/// `fs::canonicalize` returns a `\\?\`-prefixed verbatim path on Windows.
/// `Command` accepts it as a working directory, but Git appends its own
/// forward-slash suffixes to a path it receives as an *argument* and then fails
/// to open the result, so the prefix is removed before it reaches argv.
fn git_path(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{unc}"));
    }
    Some(text.strip_prefix(r"\\?\").unwrap_or(text).to_owned())
}

fn path_arg(path: &Path) -> Result<String, StageFailure> {
    git_path(path).ok_or_else(|| StageFailure {
        stage: ExecutionStage::Git,
        error: IpcError {
            code: "workspace.path_encoding".to_owned(),
            category: ErrorCategory::Disk,
            retryable: false,
            stage: ExecutionStage::Git.as_str().to_owned(),
            safe_message: "工作区路径包含无法传给 Git 的字符".to_owned(),
            action: "请把工作区改到只含 ASCII 字符的目录后重试".to_owned(),
        },
    })
}

// -- error mapping -----------------------------------------------------------

fn retryable_error(
    code: &str,
    category: ErrorCategory,
    stage: ExecutionStage,
    safe_message: &str,
    action: &str,
) -> IpcError {
    IpcError {
        code: code.to_owned(),
        category,
        retryable: !matches!(
            category,
            ErrorCategory::Auth | ErrorCategory::Permission | ErrorCategory::Validation
        ),
        stage: stage.as_str().to_owned(),
        safe_message: safe_message.to_owned(),
        action: action.to_owned(),
    }
}

fn conflict_error(stage: ExecutionStage, safe_message: &str, action: &str) -> IpcError {
    IpcError {
        code: "target.conflict".to_owned(),
        category: ErrorCategory::Conflict,
        retryable: false,
        stage: stage.as_str().to_owned(),
        safe_message: safe_message.to_owned(),
        action: action.to_owned(),
    }
}

fn cancelled_error(stage: ExecutionStage) -> IpcError {
    IpcError {
        code: "queue.cancelled".to_owned(),
        category: ErrorCategory::Conflict,
        retryable: false,
        stage: stage.as_str().to_owned(),
        safe_message: "批次已取消，该阶段没有写入目标".to_owned(),
        action: "已完成的仓库不会回滚；如需继续请创建新批次".to_owned(),
    }
}

fn unsupported_error(stage: &str, reason: impl Into<String>) -> IpcError {
    IpcError {
        code: "module.unsupported".to_owned(),
        category: ErrorCategory::Unsupported,
        retryable: false,
        stage: stage.to_owned(),
        safe_message: reason.into(),
        action: "该模块不会写入目标；报告会标记为未迁移".to_owned(),
    }
}

fn workspace_error(stage: ExecutionStage, error: &WorkspaceError) -> IpcError {
    let (code, category, action) = match error {
        WorkspaceError::InsufficientSpace { .. } => (
            "workspace.disk_full",
            ErrorCategory::Disk,
            "请清理磁盘或把工作区改到空间更大的卷后重试",
        ),
        WorkspaceError::AlreadyLocked => (
            "workspace.locked",
            ErrorCategory::Conflict,
            "另一个迁移进程正在使用该工作区；请等待它结束",
        ),
        WorkspaceError::OutsideRoot => (
            "workspace.outside_root",
            ErrorCategory::Validation,
            "请重新选择工作区目录",
        ),
        WorkspaceError::Io(_) => (
            "workspace.io",
            ErrorCategory::Disk,
            "请确认工作区目录可写后重试",
        ),
    };
    IpcError {
        code: code.to_owned(),
        category,
        retryable: matches!(category, ErrorCategory::Disk | ErrorCategory::Conflict),
        stage: stage.as_str().to_owned(),
        safe_message: format!("工作区不可用：{error}"),
        action: action.to_owned(),
    }
}

/// Maps a Git failure onto a category the queue can act on. Auth, permission and
/// conflict never come back retryable, so a batch cannot spin on them.
fn git_error(stage: ExecutionStage, error: &GitError) -> IpcError {
    let (code, category, retryable, action) = match error {
        GitError::InvalidExecutable(_) => (
            "git.missing",
            ErrorCategory::Validation,
            false,
            "请安装 Git 并确认它在 PATH 中，然后重试",
        ),
        GitError::InvalidArgument(_) => (
            "git.unsafe_argument",
            ErrorCategory::Validation,
            false,
            "地址不得包含用户名或令牌；请改用 Windows 凭据管理器",
        ),
        GitError::Io(_) => (
            "git.io",
            ErrorCategory::Disk,
            true,
            "请确认工作区可写并有足够空间后重试",
        ),
        GitError::Timeout { .. } => (
            "git.timeout",
            ErrorCategory::Network,
            true,
            "请检查网络或提高超时后重试；已推送的引用不会重复推送",
        ),
        GitError::Cancelled { .. } => (
            "git.cancelled",
            ErrorCategory::Conflict,
            false,
            "已完成的引用保留在目标上；如需继续请重试该仓库",
        ),
        GitError::Failed { stderr, .. } => classify_git_failure(stderr),
    };
    IpcError {
        code: code.to_owned(),
        category,
        retryable,
        stage: stage.as_str().to_owned(),
        // `GitRunner` already redacts registered secrets from stderr.
        safe_message: format!("Git 命令失败：{error}"),
        action: action.to_owned(),
    }
}

/// Classifies a Git failure from its stderr.
///
/// The patterns are deliberately phrase-level rather than bare status numbers:
/// a temp directory name or an object id can contain "401" and would otherwise
/// turn a disk error into a permanent auth failure the operator cannot clear.
fn classify_git_failure(stderr: &str) -> (&'static str, ErrorCategory, bool, &'static str) {
    let lowered = stderr.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lowered.contains(needle));

    if has(&[
        "authentication failed",
        "could not read username",
        "could not read password",
        "invalid username or password",
        "401 unauthorized",
        "http 401",
        "terminal prompts disabled",
    ]) {
        return (
            "git.auth",
            ErrorCategory::Auth,
            false,
            "请在 Windows 凭据管理器中更新该服务的令牌后重试",
        );
    }
    if has(&[
        "permission denied",
        "access denied",
        "403 forbidden",
        "http 403",
        "you do not have permission",
        "permission to",
    ]) {
        return (
            "git.permission",
            ErrorCategory::Permission,
            false,
            "请为该凭据授予目标命名空间的写入权限，或排除该仓库",
        );
    }
    if has(&[
        "non-fast-forward",
        "updates were rejected",
        "! [rejected]",
        "already exists",
    ]) {
        return (
            "git.rejected",
            ErrorCategory::Conflict,
            false,
            "目标已有不同历史；请改用新目标或显式确认覆盖策略",
        );
    }
    if has(&[
        "could not resolve host",
        "connection refused",
        "connection reset",
        "connection timed out",
        "operation timed out",
        "ssl certificate",
        "tls handshake",
        "unable to access",
    ]) {
        return (
            "git.network",
            ErrorCategory::Network,
            true,
            "请检查网络、代理或证书配置后重试",
        );
    }
    if has(&[
        "repository not found",
        "does not appear to be a git repository",
    ]) {
        return (
            "git.not_found",
            ErrorCategory::Conflict,
            false,
            "请确认源或目标地址存在后重新预检",
        );
    }
    (
        "git.failed",
        ErrorCategory::Git,
        true,
        "请查看日志抽屉中的脱敏输出后重试该仓库",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_and_action_wire_values_match_the_dto_layer() {
        for (stage, text) in [
            (ExecutionStage::Preflight, "preflight"),
            (ExecutionStage::PrepareTarget, "prepare_target"),
            (ExecutionStage::Git, "git"),
            (ExecutionStage::Lfs, "lfs"),
            (ExecutionStage::Metadata, "metadata"),
            (ExecutionStage::PlatformData, "platform_data"),
            (ExecutionStage::Verify, "verify"),
            (ExecutionStage::Complete, "complete"),
        ] {
            assert_eq!(stage.as_str(), text);
            assert_eq!(serde_json::to_value(stage).unwrap(), text);
        }
        assert_eq!(
            ExecutionAction::parse("reuse_empty"),
            Some(ExecutionAction::ReuseEmpty)
        );
        assert_eq!(ExecutionAction::parse("nonsense"), None);
    }

    #[test]
    fn only_create_and_rename_may_create_a_missing_target() {
        assert!(ExecutionAction::Create.may_create());
        assert!(ExecutionAction::Rename.may_create());
        for action in [
            ExecutionAction::ReuseEmpty,
            ExecutionAction::SkipNonEmpty,
            ExecutionAction::Overwrite,
            ExecutionAction::Blocked,
        ] {
            assert!(!action.may_create(), "{action:?} must not create a target");
        }
    }

    #[test]
    fn auth_and_permission_failures_are_never_retryable() {
        for stderr in [
            "fatal: Authentication failed for 'https://example.test'",
            "remote: Permission to ops/repo.git denied to build-bot.",
            "remote: HTTP 403 forbidden while accessing https://example.test",
            "fatal: could not read Username for 'https://example.test': terminal prompts disabled",
        ] {
            let error = git_error(
                ExecutionStage::Git,
                &GitError::Failed {
                    code: Some(128),
                    stderr: stderr.to_owned(),
                    stdout: String::new(),
                },
            );
            assert!(!error.retryable, "{stderr} must not be blindly retried");
            assert!(!error.action.is_empty());
        }
    }

    /// A nanosecond timestamp in a workspace path used to contain "401" and
    /// turn a disk error into a permanent auth failure.
    #[test]
    fn a_status_code_inside_a_path_does_not_forge_an_auth_failure() {
        let error = git_error(
            ExecutionStage::Git,
            &GitError::Failed {
                code: Some(128),
                stderr: "fatal: could not open '.tmp-task-1-1787964844010272500/mirror.git/HEAD' \
                         for writing: No such file or directory"
                    .to_owned(),
                stdout: String::new(),
            },
        );
        assert_eq!(error.category, ErrorCategory::Git);
        assert!(error.retryable);
    }

    #[test]
    fn windows_verbatim_paths_are_stripped_before_reaching_git() {
        assert_eq!(
            git_path(Path::new(r"\\?\C:\work\mirror.git")).as_deref(),
            Some(r"C:\work\mirror.git")
        );
        assert_eq!(
            git_path(Path::new(r"\\?\UNC\server\share\mirror.git")).as_deref(),
            Some(r"\\server\share\mirror.git")
        );
        assert_eq!(
            git_path(Path::new("/tmp/mirror.git")).as_deref(),
            Some("/tmp/mirror.git")
        );
    }

    #[test]
    fn network_failures_stay_retryable() {
        let error = git_error(
            ExecutionStage::Git,
            &GitError::Failed {
                code: Some(128),
                stderr: "fatal: unable to access: Could not resolve host: git.test".to_owned(),
                stdout: String::new(),
            },
        );
        assert!(error.retryable);
        assert_eq!(error.category, ErrorCategory::Network);
    }

    #[test]
    fn ls_remote_output_is_parsed_into_ref_tips() {
        let parsed = parse_ls_remote(
            "aaa111\tHEAD\nbbb222\trefs/heads/main\nccc333\trefs/tags/v1.0.0\ngarbage\n",
        );
        assert_eq!(
            parsed.get("refs/heads/main").map(String::as_str),
            Some("bbb222")
        );
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn task_ids_never_escape_the_workspace_through_a_temp_name() {
        assert_eq!(sanitize_id("../../etc"), "------etc");
        assert_eq!(sanitize_id("task-1"), "task-1");
        assert_eq!(sanitize_id("///"), "task");
    }

    #[test]
    fn selected_modules_follow_the_operator_selection() {
        let assignment = TaskAssignment {
            batch_id: "b".into(),
            task_id: "t".into(),
            source_url: "https://s/r.git".into(),
            target_url: "https://t/r.git".into(),
            target_name: "r".into(),
            action: ExecutionAction::ReuseEmpty,
            modules: ModuleSelection {
                lfs: true,
                metadata: true,
                issues: true,
                pull_requests: false,
                wiki: false,
                releases: true,
            },
            ref_policy: RefPolicy::default(),
            allow_overwrite: false,
            resumed_attempt: false,
        };
        assert_eq!(
            assignment.selected_platform_modules(),
            ["issues", "releases"]
        );
    }
}
