//! Contract tests for the IPC boundary.
//!
//! These drive `AppState` exactly as the commands do, so they cover the real
//! SQLite writes, the real plan-hash re-verification and the real event
//! envelopes without needing a window, a network or a platform token.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use git_repo_migrator_application::ipc_contract::{
    BatchStartInput, ConnectionTestInput, ReportExportInput, TaskRetryInput,
};
use git_repo_migrator_application::planning::TargetState;
use git_repo_migrator_application::verification::{AggregateStatus, VerificationEvidence};
use git_repo_migrator_application::{BatchControl, IpcError};
use git_repo_migrator_domain::{ErrorCategory, Fidelity, RepoTaskState};
use git_repo_migrator_platform_core::PlatformKind;
use rusqlite::params;

use crate::dto::{
    BatchIdInput, CleanupState, ConnectionRole, ConnectionSaveInput, MigrationStage, PlanAction,
    PlanFreezeInput, PlanPreviewRequest, RepositoryImportInput, TargetProbeInput,
};
use crate::errors;
use crate::events::{self, EventSink, RecordingSink};
use crate::ports::{Clock, ExportSink, TargetProbe};
use crate::snapshot::VerifySummary;
use crate::state::{AppState, ModuleOutcome};

// -- doubles ---------------------------------------------------------------

/// Advances by a fixed step per call so ordering, lease windows and ids stay
/// deterministic without depending on the wall clock.
struct StepClock {
    now: AtomicI64,
}

impl StepClock {
    fn new() -> Self {
        Self {
            now: AtomicI64::new(1_700_000_000_000),
        }
    }
}

impl Clock for StepClock {
    fn now_ms(&self) -> i64 {
        self.now.fetch_add(250, Ordering::Relaxed)
    }
}

struct StubProbe {
    state: Mutex<TargetState>,
}

impl StubProbe {
    fn new(state: TargetState) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }
    fn set(&self, state: TargetState) {
        *self.state.lock().expect("probe lock") = state;
    }
}

impl TargetProbe for StubProbe {
    fn probe(&self, _target_url: &str) -> Result<TargetState, IpcError> {
        Ok(*self.state.lock().expect("probe lock"))
    }
}

#[derive(Default)]
struct MemoryExportSink {
    written: Mutex<Vec<(String, String)>>,
}

impl ExportSink for MemoryExportSink {
    fn write(&self, path: &std::path::Path, contents: &str) -> Result<(), String> {
        self.written
            .lock()
            .expect("sink lock")
            .push((path.to_string_lossy().into_owned(), contents.to_owned()));
        Ok(())
    }
}

// -- fixtures --------------------------------------------------------------

struct Harness {
    state: AppState,
    probe: Arc<StubProbe>,
    exports: Arc<MemoryExportSink>,
}

fn harness(target_state: TargetState) -> Harness {
    let probe = Arc::new(StubProbe::new(target_state));
    let exports = Arc::new(MemoryExportSink::default());
    let state = AppState::in_memory()
        .expect("in-memory store")
        .with_clock(Arc::new(StepClock::new()))
        .with_target_probe(probe.clone())
        .with_export_sink(exports.clone());
    Harness {
        state,
        probe,
        exports,
    }
}

fn save_connections(state: &AppState) {
    for (role, endpoint) in [
        (ConnectionRole::Source, "https://git.source.test"),
        (ConnectionRole::Target, "https://git.target.test"),
    ] {
        state
            .save_connection(&ConnectionSaveInput {
                role,
                endpoint: endpoint.to_owned(),
                platform_hint: Some(PlatformKind::GenericGit),
                credential_ref: Some("credential/windows/demo".to_owned()),
                trust_fingerprint_sha256: None,
            })
            .expect("connection saved");
    }
}

fn import(state: &AppState, urls: &str) {
    state
        .import_repositories(&RepositoryImportInput {
            connection_id: "source".to_owned(),
            urls: urls.to_owned(),
        })
        .expect("urls imported");
}

/// Maps every imported repository to a target under the same name and probes it.
fn map_and_probe(state: &AppState) -> Vec<String> {
    let mut ids = Vec::new();
    for repository in state.repositories().expect("repositories") {
        let target = format!("https://git.target.test/ops/{}", repository.name);
        state
            .set_mapping(&repository.id, &target, None)
            .expect("mapping applied");
        state
            .probe_target(&TargetProbeInput {
                repository_id: repository.id.clone(),
                target_url: target,
            })
            .expect("target probed");
        ids.push(repository.id);
    }
    ids
}

fn preview_request(selected: &[String]) -> PlanPreviewRequest {
    PlanPreviewRequest {
        selected_repository_ids: selected.to_vec(),
        excluded_repository_ids: vec![],
        mappings: vec![],
        reuse_empty: true,
        skip_non_empty: true,
        auto_rename: true,
        allow_overwrite: false,
        include_archived_refs: false,
        module_lfs: false,
        module_metadata: false,
        module_issues: false,
        module_pull_requests: false,
        module_wiki: false,
        module_releases: false,
    }
}

fn network_error() -> IpcError {
    errors::error(
        "ipc.network",
        ErrorCategory::Network,
        true,
        "git",
        "推送过程中连接中断",
        "请检查网络后重试该仓库",
    )
}

fn permission_error() -> IpcError {
    errors::error(
        "platform.permission",
        ErrorCategory::Permission,
        false,
        "prepare_target",
        "凭据对目标命名空间没有写入权限",
        "请授予写入权限或排除该仓库",
    )
}

// -- tests -----------------------------------------------------------------

#[test]
fn generic_git_flow_reaches_a_report_and_a_redacted_export() {
    let Harness { state, exports, .. } = harness(TargetState::Empty);
    save_connections(&state);
    import(&state, "https://git.source.test/ops/alpha.git\n");
    let ids = map_and_probe(&state);

    let preview = state
        .preview_plan(&preview_request(&ids))
        .expect("preview built");
    assert!(preview.blocking.is_empty(), "{:?}", preview.blocking);
    assert_eq!(preview.metrics.reuse, 1);
    assert_eq!(preview.rows[0].action, PlanAction::ReuseEmpty);
    assert!(!preview.requires_confirmation);

    let plan = state
        .freeze_plan(&PlanFreezeInput {
            preview_id: preview.preview_id.clone(),
            confirmation_text: None,
            acknowledged_fidelity: vec![],
        })
        .expect("plan frozen");
    assert_eq!(plan.status, "frozen");

    let batch = state
        .start_batch(&BatchStartInput {
            plan_id: plan.plan_id.clone(),
            concurrency: 4,
            workspace_policy: "reuse".to_owned(),
        })
        .expect("batch started");
    assert_eq!(batch.total, 1);
    assert_eq!(batch.control, BatchControl::Running);

    let task_id = batch.tasks[0].task_id.clone();
    let owner = "worker-1";
    for stage in [
        MigrationStage::Preflight,
        MigrationStage::PrepareTarget,
        MigrationStage::Git,
        MigrationStage::Verify,
    ] {
        state
            .begin_stage(&task_id, stage, owner)
            .expect("stage started");
    }
    state
        .record_module_result(
            &task_id,
            &ModuleOutcome {
                module: "git",
                fidelity: Fidelity::NativeRebuild,
                source_count: 7,
                target_count: 7,
                error: None,
                source_links: &[],
            },
        )
        .expect("module recorded");
    state
        .complete_task(
            &task_id,
            owner,
            &VerifySummary {
                git_verified: true,
                lfs_verified: true,
                metadata_verified: true,
                evidence: VerificationEvidence {
                    refs_checked: 7,
                    excluded_refs: vec!["refs/pull/1/head".to_owned()],
                    ..VerificationEvidence::default()
                },
                ..VerifySummary::default()
            },
            AggregateStatus::Succeeded,
        )
        .expect("task completed");

    let report = state.report(&batch.batch_id).expect("report");
    assert_eq!(report.metrics.complete_success, 1);
    assert_eq!(report.metrics.git_success_platform_partial, 0);
    assert!(report.rows[0].git_verified);
    assert_eq!(report.rows[0].evidence.refs_checked, 7);
    assert_eq!(report.rows[0].evidence.excluded_refs.len(), 1);

    let outcome = state
        .export_report(&ReportExportInput {
            batch_id: batch.batch_id.clone(),
            format: "csv".to_owned(),
            path: absolute_csv_path(),
        })
        .expect("export succeeded");
    assert_eq!(outcome.row_count, 1);
    let written = exports.written.lock().expect("sink lock");
    assert_eq!(written.len(), 1);
    assert!(written[0].1.contains("refs/pull/1/head"));
    for forbidden in ["token", "authorization", "password"] {
        assert!(
            !written[0].1.to_ascii_lowercase().contains(forbidden),
            "export leaked {forbidden}"
        );
    }
}

fn absolute_csv_path() -> String {
    let dir = std::env::temp_dir();
    dir.join("git-repo-migrator-report.csv")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn unknown_target_state_blocks_the_plan_and_the_freeze() {
    let Harness { state, probe, .. } = harness(TargetState::Unknown);
    save_connections(&state);
    import(&state, "https://git.source.test/ops/alpha.git\n");
    probe.set(TargetState::Unknown);
    let ids = map_and_probe(&state);

    let preview = state
        .preview_plan(&preview_request(&ids))
        .expect("preview built");
    assert_eq!(preview.metrics.blocked, 1);
    assert_eq!(preview.metrics.executable, 0);
    assert_eq!(preview.rows[0].action, PlanAction::Blocked);
    assert!(preview.rows[0]
        .suggested_action
        .as_deref()
        .is_some_and(|action| action.contains("探测目标")));

    let error = state
        .freeze_plan(&PlanFreezeInput {
            preview_id: preview.preview_id,
            confirmation_text: None,
            acknowledged_fidelity: vec![],
        })
        .expect_err("blocked plan must not freeze");
    assert_eq!(error.category, ErrorCategory::Conflict);
}

#[test]
fn non_empty_target_is_skipped_by_default() {
    let Harness { state, .. } = harness(TargetState::NonEmpty);
    save_connections(&state);
    import(&state, "https://git.source.test/ops/alpha.git\n");
    let ids = map_and_probe(&state);

    let preview = state
        .preview_plan(&preview_request(&ids))
        .expect("preview built");
    assert_eq!(preview.rows[0].action, PlanAction::SkipNonEmpty);
    assert_eq!(preview.metrics.skip, 1);
    assert!(!preview.requires_confirmation);
    assert!(preview
        .warnings
        .iter()
        .any(|warning| warning.contains("默认跳过")));
}

#[test]
fn overwrite_requires_the_phrase_the_backend_issued() {
    let Harness { state, .. } = harness(TargetState::NonEmpty);
    save_connections(&state);
    import(&state, "https://git.source.test/ops/alpha.git\n");
    let ids = map_and_probe(&state);

    let mut request = preview_request(&ids);
    request.allow_overwrite = true;
    request.skip_non_empty = false;
    let preview = state.preview_plan(&request).expect("preview built");
    assert_eq!(preview.rows[0].action, PlanAction::Overwrite);
    assert!(preview.requires_confirmation);
    let phrase = preview
        .confirmation_phrase
        .clone()
        .expect("a confirmation phrase must be issued");
    assert_eq!(phrase, "alpha");

    for wrong in [None, Some(String::new()), Some("alpha ".to_owned())] {
        let error = state
            .freeze_plan(&PlanFreezeInput {
                preview_id: preview.preview_id.clone(),
                confirmation_text: wrong.clone(),
                acknowledged_fidelity: vec![],
            })
            .expect_err("wrong confirmation must be rejected");
        assert_eq!(error.category, ErrorCategory::Validation);
    }

    let plan = state
        .freeze_plan(&PlanFreezeInput {
            preview_id: preview.preview_id,
            confirmation_text: Some(phrase),
            acknowledged_fidelity: vec![],
        })
        .expect("confirmed plan freezes");
    assert!(plan.dangerous_confirmed);
}

#[test]
fn degraded_modules_must_be_acknowledged_before_a_freeze() {
    let Harness { state, .. } = harness(TargetState::Empty);
    save_connections(&state);
    import(&state, "https://git.source.test/ops/alpha.git\n");
    let ids = map_and_probe(&state);

    let mut request = preview_request(&ids);
    request.module_issues = true;
    let preview = state.preview_plan(&request).expect("preview built");
    let issues = preview.rows[0]
        .module_fidelity
        .iter()
        .find(|row| row.module == "issues")
        .expect("issues row present");
    // Generic Git has no platform data API, so the module is explicitly
    // unsupported rather than silently dropped.
    assert_eq!(issues.fidelity, Fidelity::Unsupported);
    assert!(issues.confirmation_required);

    let error = state
        .freeze_plan(&PlanFreezeInput {
            preview_id: preview.preview_id.clone(),
            confirmation_text: None,
            acknowledged_fidelity: vec![],
        })
        .expect_err("unacknowledged degradation must block the freeze");
    assert_eq!(error.category, ErrorCategory::Validation);

    state
        .freeze_plan(&PlanFreezeInput {
            preview_id: preview.preview_id,
            confirmation_text: None,
            acknowledged_fidelity: vec!["issues".to_owned()],
        })
        .expect("acknowledged degradation freezes");
}

#[test]
fn select_all_covers_the_filtered_set_not_the_current_page() {
    let Harness { state, .. } = harness(TargetState::Empty);
    save_connections(&state);
    let urls = (0..100)
        .map(|index| format!("https://git.source.test/ops/repo{index}.git"))
        .collect::<Vec<_>>()
        .join("\n");
    import(&state, &urls);
    let ids = map_and_probe(&state);
    assert_eq!(ids.len(), 100);

    let mut request = preview_request(&ids);
    request.excluded_repository_ids = ids.iter().take(3).cloned().collect();
    let preview = state.preview_plan(&request).expect("preview built");
    assert_eq!(preview.selected_count, 97);
    assert_eq!(preview.excluded_count, 3);
    assert_eq!(preview.metrics.total, 97);
    assert_eq!(preview.metrics.executable, 97);
}

#[test]
fn a_tampered_plan_row_can_never_start_a_batch() {
    let Harness { state, .. } = harness(TargetState::Empty);
    save_connections(&state);
    import(&state, "https://git.source.test/ops/alpha.git\n");
    let ids = map_and_probe(&state);
    let preview = state
        .preview_plan(&preview_request(&ids))
        .expect("preview built");
    let plan = state
        .freeze_plan(&PlanFreezeInput {
            preview_id: preview.preview_id,
            confirmation_text: None,
            acknowledged_fidelity: vec![],
        })
        .expect("plan frozen");

    // Simulate an out-of-band edit of the frozen plan.
    state
        .with_connection_for_test(|connection| {
            connection.execute(
                "UPDATE plan SET selection_json = replace(selection_json, 'ops/alpha', 'ops/evil')
                 WHERE id = ?1",
                params![plan.plan_id],
            )
        })
        .expect("tamper applied");

    let error = state
        .start_batch(&BatchStartInput {
            plan_id: plan.plan_id,
            concurrency: 1,
            workspace_policy: "reuse".to_owned(),
        })
        .expect_err("a tampered plan must not start");
    assert_eq!(error.category, ErrorCategory::Conflict);
    assert!(error.safe_message.contains("哈希"));
}

#[test]
fn a_stale_capability_snapshot_sends_the_operator_back_to_preflight() {
    let Harness { state, .. } = harness(TargetState::Empty);
    save_connections(&state);
    import(&state, "https://git.source.test/ops/alpha.git\n");
    let ids = map_and_probe(&state);
    let preview = state
        .preview_plan(&preview_request(&ids))
        .expect("preview built");
    let plan = state
        .freeze_plan(&PlanFreezeInput {
            preview_id: preview.preview_id,
            confirmation_text: None,
            acknowledged_fidelity: vec![],
        })
        .expect("plan frozen");

    // Re-pointing the target at a different platform changes its capabilities.
    state
        .save_connection(&ConnectionSaveInput {
            role: ConnectionRole::Target,
            endpoint: "https://gitlab.target.test".to_owned(),
            platform_hint: Some(PlatformKind::Gitlab),
            credential_ref: Some("credential/windows/demo".to_owned()),
            trust_fingerprint_sha256: None,
        })
        .expect("target reconnected");

    let error = state
        .start_batch(&BatchStartInput {
            plan_id: plan.plan_id,
            concurrency: 1,
            workspace_policy: "reuse".to_owned(),
        })
        .expect_err("a stale capability snapshot must block the start");
    assert_eq!(error.category, ErrorCategory::Conflict);
    assert!(error.safe_message.contains("能力快照"));
}

#[test]
fn workspace_policy_and_concurrency_are_validated_and_clamped() {
    let Harness { state, .. } = harness(TargetState::Empty);
    save_connections(&state);
    import(&state, "https://git.source.test/ops/alpha.git\n");
    let ids = map_and_probe(&state);
    let preview = state
        .preview_plan(&preview_request(&ids))
        .expect("preview built");
    let plan = state
        .freeze_plan(&PlanFreezeInput {
            preview_id: preview.preview_id,
            confirmation_text: None,
            acknowledged_fidelity: vec![],
        })
        .expect("plan frozen");

    let error = state
        .start_batch(&BatchStartInput {
            plan_id: plan.plan_id.clone(),
            concurrency: 1,
            workspace_policy: "rm -rf".to_owned(),
        })
        .expect_err("unknown workspace policy must be rejected");
    assert_eq!(error.category, ErrorCategory::Validation);

    let batch = state
        .start_batch(&BatchStartInput {
            plan_id: plan.plan_id,
            concurrency: 999,
            workspace_policy: "clean".to_owned(),
        })
        .expect("batch started");
    assert_eq!(batch.concurrency, 8);
}

/// Records what the GUI hands the native entry process. The point of the test
/// is what is *absent*: no secret ever reaches this port.
#[derive(Default)]
struct RecordingEntry {
    launched: Mutex<Vec<String>>,
    fail: Mutex<bool>,
}

impl RecordingEntry {
    fn launched(&self) -> Vec<String> {
        self.launched.lock().expect("entry lock").clone()
    }
}

impl crate::ports::IdentityEntryLauncher for RecordingEntry {
    fn launch(&self, name: &str) -> Result<(), IpcError> {
        if *self.fail.lock().expect("entry lock") {
            return Err(errors::error(
                "credential.companion_missing",
                ErrorCategory::Validation,
                false,
                "connection",
                "找不到凭据录入程序",
                "请重新安装应用",
            ));
        }
        self.launched
            .lock()
            .expect("entry lock")
            .push(name.to_owned());
        Ok(())
    }
}

/// The credential boundary in one test: a name goes in, a reference comes out,
/// and the entry process — not the GUI — is what reads the token.
#[test]
fn authorizing_a_connection_moves_a_name_in_and_a_reference_out() {
    let entry = Arc::new(RecordingEntry::default());
    let state = AppState::in_memory()
        .expect("in-memory store")
        .with_clock(Arc::new(StepClock::new()))
        .with_identity_entry(entry.clone());

    let outcome = state
        .authorize_connection(
            &git_repo_migrator_application::ipc_contract::ConnectionAuthorizeInput {
                name: "source".to_owned(),
            },
        )
        .expect("entry launched");

    assert_eq!(entry.launched(), ["source"]);
    assert!(outcome.credential_ref.starts_with("credential/windows/"));
    let serialised = serde_json::to_string(&outcome).expect("serialised");
    for forbidden in ["token", "secret", "password", "ghp_"] {
        assert!(
            !serialised.contains(forbidden),
            "{forbidden} leaked into the authorize outcome"
        );
    }
}

#[test]
fn a_credential_name_that_could_be_read_as_a_flag_never_reaches_the_entry_process() {
    let entry = Arc::new(RecordingEntry::default());
    let state = AppState::in_memory()
        .expect("in-memory store")
        .with_identity_entry(entry.clone());

    for name in ["", "--help", "../../etc", "a b", "name;calc.exe"] {
        let error = state
            .authorize_connection(
                &git_repo_migrator_application::ipc_contract::ConnectionAuthorizeInput {
                    name: name.to_owned(),
                },
            )
            .expect_err("an unsafe name must be refused");
        assert_eq!(error.category, ErrorCategory::Validation);
    }
    assert!(
        entry.launched().is_empty(),
        "no process may be started for a rejected name"
    );
}

#[test]
fn a_missing_entry_program_is_reported_with_a_next_step() {
    let entry = Arc::new(RecordingEntry::default());
    *entry.fail.lock().expect("entry lock") = true;
    let state = AppState::in_memory()
        .expect("in-memory store")
        .with_identity_entry(entry);

    let error = state
        .authorize_connection(
            &git_repo_migrator_application::ipc_contract::ConnectionAuthorizeInput {
                name: "source".to_owned(),
            },
        )
        .expect_err("a missing companion must surface");
    assert_eq!(error.code, "credential.companion_missing");
    assert!(!error.action.is_empty());
}

#[derive(Default)]
struct RecordingLauncher {
    calls: Mutex<Vec<(String, String, u16)>>,
}

impl RecordingLauncher {
    fn calls(&self) -> Vec<(String, String, u16)> {
        self.calls.lock().expect("launcher lock").clone()
    }
}

impl crate::ports::BatchLauncher for RecordingLauncher {
    fn launch(&self, batch_id: &str, concurrency: u16) {
        self.calls.lock().expect("launcher lock").push((
            "launch".to_owned(),
            batch_id.to_owned(),
            concurrency,
        ));
    }
    fn cancel(&self, batch_id: &str) {
        self.calls.lock().expect("launcher lock").push((
            "cancel".to_owned(),
            batch_id.to_owned(),
            0,
        ));
    }
}

/// The queue commands must be what starts and stops the worker pool. Without
/// this link a started batch would sit at `planned` forever, which is exactly
/// the gap Wave 5 left open.
#[test]
fn the_queue_commands_drive_the_worker_pool() {
    let Harness { state, .. } = harness(TargetState::Empty);
    let launcher = Arc::new(RecordingLauncher::default());
    state.install_launcher(launcher.clone());

    let batch = started_batch(&state, 2);
    let id = batch.batch_id.clone();
    assert_eq!(
        launcher.calls(),
        vec![("launch".to_owned(), id.clone(), 2)],
        "starting a batch must start its workers"
    );

    state
        .set_control(
            &BatchIdInput {
                batch_id: id.clone(),
            },
            BatchControl::Paused,
        )
        .expect("paused");
    assert_eq!(
        launcher.calls().len(),
        1,
        "pausing lets the workers drain; it must not start more"
    );

    state
        .set_control(
            &BatchIdInput {
                batch_id: id.clone(),
            },
            BatchControl::Running,
        )
        .expect("resumed");
    state
        .set_control(
            &BatchIdInput {
                batch_id: id.clone(),
            },
            BatchControl::Cancelled,
        )
        .expect("cancelled");

    assert_eq!(
        launcher.calls(),
        vec![
            ("launch".to_owned(), id.clone(), 2),
            ("launch".to_owned(), id.clone(), 2),
            ("cancel".to_owned(), id, 0),
        ]
    );
}

#[test]
fn a_paused_batch_reports_paused_and_cancel_keeps_finished_work() {
    let Harness { state, .. } = harness(TargetState::Empty);
    let batch = started_batch(&state, 2);
    let first = batch.tasks[0].task_id.clone();

    let paused = state
        .set_control(
            &BatchIdInput {
                batch_id: batch.batch_id.clone(),
            },
            BatchControl::Paused,
        )
        .expect("paused");
    assert_eq!(paused.control, BatchControl::Paused);

    // Pausing twice is a conflict rather than a silent no-op.
    let error = state
        .set_control(
            &BatchIdInput {
                batch_id: batch.batch_id.clone(),
            },
            BatchControl::Paused,
        )
        .expect_err("double pause must conflict");
    assert_eq!(error.category, ErrorCategory::Conflict);

    state
        .set_control(
            &BatchIdInput {
                batch_id: batch.batch_id.clone(),
            },
            BatchControl::Running,
        )
        .expect("resumed");
    finish_task(&state, &first, AggregateStatus::Succeeded);

    let cancelled = state
        .set_control(
            &BatchIdInput {
                batch_id: batch.batch_id.clone(),
            },
            BatchControl::Cancelled,
        )
        .expect("cancelled");
    assert_eq!(cancelled.control, BatchControl::Cancelled);
    // The finished repository keeps its terminal state; cancelling never rolls
    // a completed migration back.
    let finished = cancelled
        .tasks
        .iter()
        .find(|task| task.task_id == first)
        .expect("finished task present");
    assert_eq!(finished.state, RepoTaskState::Succeeded);
    assert_eq!(cancelled.completed, 1);
}

#[test]
fn retry_only_touches_retryable_failures() {
    let Harness { state, .. } = harness(TargetState::Empty);
    let batch = started_batch(&state, 2);
    let retryable = batch.tasks[0].task_id.clone();
    let permission_denied = batch.tasks[1].task_id.clone();

    state
        .begin_stage(&retryable, MigrationStage::Git, "worker-1")
        .expect("stage started");
    state
        .fail_stage(
            &retryable,
            MigrationStage::Git,
            "worker-1",
            &network_error(),
        )
        .expect("failure recorded");
    state
        .begin_stage(
            &permission_denied,
            MigrationStage::PrepareTarget,
            "worker-2",
        )
        .expect("stage started");
    state
        .fail_stage(
            &permission_denied,
            MigrationStage::PrepareTarget,
            "worker-2",
            &permission_error(),
        )
        .expect("failure recorded");

    let outcome = state
        .retry_tasks(&TaskRetryInput {
            batch_id: batch.batch_id.clone(),
            task_ids: vec![retryable.clone(), permission_denied.clone()],
        })
        .expect("retry evaluated");
    assert_eq!(outcome.retried, vec![retryable.clone()]);
    assert_eq!(outcome.rejected.len(), 1);
    assert_eq!(outcome.rejected[0].task_id, permission_denied);

    let requeued = outcome
        .batch
        .tasks
        .iter()
        .find(|task| task.task_id == retryable)
        .expect("retried task present");
    assert_eq!(requeued.state, RepoTaskState::Planned);
    assert_eq!(requeued.attempt, 1);
    // A permission failure is a skip, not a retry candidate.
    let skipped = outcome
        .batch
        .tasks
        .iter()
        .find(|task| task.task_id == permission_denied)
        .expect("skipped task present");
    assert_eq!(skipped.state, RepoTaskState::Skipped);
}

#[test]
fn a_dropped_event_is_corrected_by_the_next_snapshot() {
    let Harness { state, .. } = harness(TargetState::Empty);
    let batch = started_batch(&state, 1);
    let task_id = batch.tasks[0].task_id.clone();
    let sink = RecordingSink::default();

    state
        .begin_stage(&task_id, MigrationStage::Git, "worker-1")
        .expect("stage started");
    // The first progress event is emitted...
    sink.emit(&events::progress(
        state.revision(),
        &batch.batch_id,
        &task_id,
        3,
        Some(10),
    ));
    state
        .report_progress(&task_id, MigrationStage::Git, "worker-1", 3, Some(10))
        .expect("progress recorded");
    // ...and the second is dropped on the way to the renderer.
    state
        .report_progress(&task_id, MigrationStage::Git, "worker-1", 9, Some(10))
        .expect("progress recorded");

    let observed = sink.envelopes();
    assert_eq!(observed.len(), 1);
    let snapshot = state.snapshot().expect("snapshot");
    assert!(snapshot.revision > observed[0].revision);
    let task = &snapshot
        .active_batch
        .expect("active batch")
        .tasks
        .into_iter()
        .find(|task| task.task_id == task_id)
        .expect("task present");
    // SQLite, not the event stream, is what the UI renders.
    assert_eq!(task.progress_completed, 9);
    assert_eq!(task.progress_total, Some(10));
    assert_eq!(task.stage, MigrationStage::Git);
}

#[test]
fn git_success_with_a_degraded_module_is_reported_as_partial() {
    let Harness { state, .. } = harness(TargetState::Empty);
    let batch = started_batch(&state, 1);
    let task_id = batch.tasks[0].task_id.clone();

    state
        .begin_stage(&task_id, MigrationStage::Verify, "worker-1")
        .expect("stage started");
    state
        .record_module_result(
            &task_id,
            &ModuleOutcome {
                module: "issues",
                fidelity: Fidelity::ReadOnlyArchive,
                source_count: 12,
                target_count: 0,
                error: None,
                source_links: &["https://git.source.test/ops/repo0/issues/1".to_owned()],
            },
        )
        .expect("module recorded");
    state
        .complete_task(
            &task_id,
            "worker-1",
            &VerifySummary {
                git_verified: true,
                lfs_verified: true,
                metadata_verified: false,
                archive_path: Some("archive/repo0.jsonl".to_owned()),
                unmapped_fields: vec!["assignee".to_owned()],
                ..VerifySummary::default()
            },
            AggregateStatus::Partial,
        )
        .expect("task completed");

    let report = state.report(&batch.batch_id).expect("report");
    assert_eq!(report.metrics.complete_success, 0);
    assert_eq!(report.metrics.git_success_platform_partial, 1);
    let row = &report.rows[0];
    assert_eq!(row.status, AggregateStatus::Partial);
    assert!(row.git_verified);
    assert!(!row.metadata_verified);
    assert_eq!(row.archive_path.as_deref(), Some("archive/repo0.jsonl"));
    assert_eq!(row.unmapped_fields, vec!["assignee".to_owned()]);
    let issues = row
        .modules
        .iter()
        .find(|module| module.module == "issues")
        .expect("issues module recorded");
    // An archive is never presented as a native rebuild.
    assert_eq!(issues.fidelity, Fidelity::ReadOnlyArchive);
    assert!(issues.confirmation_required);
    assert_eq!(row.source_links.len(), 1);
}

#[test]
fn an_unfinished_batch_produces_no_result_rows() {
    let Harness { state, .. } = harness(TargetState::Empty);
    let batch = started_batch(&state, 2);
    state
        .begin_stage(&batch.tasks[0].task_id, MigrationStage::Git, "worker-1")
        .expect("stage started");

    let report = state.report(&batch.batch_id).expect("report");
    assert!(report.rows.is_empty());
    assert_eq!(report.metrics.complete_success, 0);
    assert_eq!(report.cleanup, CleanupState::Cleaned);
}

#[test]
fn interrupted_work_is_offered_for_resume_with_a_credential_recheck() {
    let Harness { state, .. } = harness(TargetState::Empty);
    let batch = started_batch(&state, 1);
    state
        .begin_stage(&batch.tasks[0].task_id, MigrationStage::Git, "worker-1")
        .expect("stage started");

    let snapshot = state.snapshot().expect("snapshot");
    let resumable = snapshot
        .resumable
        .iter()
        .find(|entry| entry.batch_id == batch.batch_id)
        .expect("batch offered for resume");
    assert_eq!(resumable.pending, 1);
    assert!(resumable.credential_recheck_required);
}

#[test]
fn endpoints_and_credential_references_reject_inline_secrets() {
    let Harness { state, .. } = harness(TargetState::Empty);

    for endpoint in [
        "https://user:pass@git.example.test",
        "ftp://git.example.test",
        "not a url",
        "",
    ] {
        let error = state
            .save_connection(&ConnectionSaveInput {
                role: ConnectionRole::Source,
                endpoint: endpoint.to_owned(),
                platform_hint: Some(PlatformKind::GenericGit),
                credential_ref: None,
                trust_fingerprint_sha256: None,
            })
            .expect_err("unsafe endpoint must be rejected");
        assert_eq!(error.category, ErrorCategory::Validation);
    }

    for credential in ["ghp_abcdefghijklmnopqrstuvwxyz", "Bearer abc", "token=abc"] {
        let error = state
            .test_connection(&ConnectionTestInput {
                endpoint: "https://git.example.test".to_owned(),
                platform_hint: Some(PlatformKind::Github),
                credential_ref: Some(credential.to_owned()),
            })
            .expect_err("a plaintext token must be rejected");
        assert_eq!(error.category, ErrorCategory::Validation);
    }
}

#[test]
fn self_signed_trust_requires_a_full_sha256_fingerprint() {
    let Harness { state, .. } = harness(TargetState::Empty);
    let error = state
        .save_connection(&ConnectionSaveInput {
            role: ConnectionRole::Source,
            endpoint: "https://git.internal.test".to_owned(),
            platform_hint: Some(PlatformKind::Gitea),
            credential_ref: None,
            trust_fingerprint_sha256: Some("aa:bb".to_owned()),
        })
        .expect_err("a truncated fingerprint must be rejected");
    assert_eq!(error.category, ErrorCategory::Validation);

    let connection = state
        .save_connection(&ConnectionSaveInput {
            role: ConnectionRole::Source,
            endpoint: "https://git.internal.test".to_owned(),
            platform_hint: Some(PlatformKind::Gitea),
            credential_ref: None,
            trust_fingerprint_sha256: Some("a".repeat(64)),
        })
        .expect("a pinned fingerprint is accepted");
    assert!(connection.tls_trusted);
    assert!(connection.credential_ref.is_none());
}

#[test]
fn import_reports_invalid_lines_and_deduplicates() {
    let Harness { state, .. } = harness(TargetState::Empty);
    save_connections(&state);
    let report = state
        .import_repositories(&RepositoryImportInput {
            connection_id: "source".to_owned(),
            urls: [
                "https://git.source.test/ops/alpha.git",
                "https://git.source.test/ops/alpha.git",
                "https://user:pass@git.source.test/ops/beta.git",
                "javascript:alert(1)",
                "# a comment",
                "",
            ]
            .join("\n"),
        })
        .expect("import evaluated");
    assert_eq!(report.imported, 1);
    assert_eq!(report.duplicate_count, 1);
    assert_eq!(report.issues.len(), 2);
    assert!(report.issues.iter().any(|issue| issue.line == 3));
}

#[test]
fn export_rejects_relative_paths_and_unknown_formats() {
    let Harness { state, .. } = harness(TargetState::Empty);
    let batch = started_batch(&state, 1);

    for (format, path) in [
        ("csv", "relative/report.csv"),
        ("json", &absolute_csv_path()),
        ("xml", &absolute_csv_path()),
    ] {
        let error = state
            .export_report(&ReportExportInput {
                batch_id: batch.batch_id.clone(),
                format: format.to_owned(),
                path: path.to_owned(),
            })
            .expect_err("invalid export must be rejected");
        assert_eq!(error.category, ErrorCategory::Validation);
    }
}

#[test]
fn discovery_without_a_transport_reports_unsupported_instead_of_zero_results() {
    let Harness { state, .. } = harness(TargetState::Empty);
    save_connections(&state);
    let error = state
        .discover_repositories(
            "source",
            &git_repo_migrator_platform_core::DiscoveryQuery {
                scope: git_repo_migrator_platform_core::RepositoryScope::AllAccessible,
                search: None,
                visibility: None,
                include_archived: false,
                cursor: None,
                page_size: 50,
            },
        )
        .expect_err("discovery is not available without a transport");
    assert_eq!(error.category, ErrorCategory::Unsupported);
    assert!(error.action.contains("手动 URL 导入"));
}

// -- shared flow helpers ---------------------------------------------------

fn started_batch(state: &AppState, repositories: usize) -> crate::dto::BatchSnapshot {
    save_connections(state);
    let urls = (0..repositories)
        .map(|index| format!("https://git.source.test/ops/repo{index}.git"))
        .collect::<Vec<_>>()
        .join("\n");
    import(state, &urls);
    let ids = map_and_probe(state);
    let preview = state
        .preview_plan(&preview_request(&ids))
        .expect("preview built");
    let plan = state
        .freeze_plan(&PlanFreezeInput {
            preview_id: preview.preview_id,
            confirmation_text: None,
            acknowledged_fidelity: vec![],
        })
        .expect("plan frozen");
    state
        .start_batch(&BatchStartInput {
            plan_id: plan.plan_id,
            concurrency: 2,
            workspace_policy: "reuse".to_owned(),
        })
        .expect("batch started")
}

fn finish_task(state: &AppState, task_id: &str, status: AggregateStatus) {
    state
        .begin_stage(task_id, MigrationStage::Verify, "worker-1")
        .expect("stage started");
    state
        .complete_task(task_id, "worker-1", &VerifySummary::default(), status)
        .expect("task completed");
}
