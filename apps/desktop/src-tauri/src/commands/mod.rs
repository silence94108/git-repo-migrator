//! The complete IPC command whitelist.
//!
//! Nothing outside this module is callable from the renderer. There is no shell
//! command, no arbitrary file read/write, no credential read and no direct
//! platform API passthrough. Every command is a thin wrapper: all validation and
//! all state transitions live in `crate::state`, which is also what the tests
//! exercise.

use git_repo_migrator_application::ipc_contract::{
    BatchStartInput, ConnectionAuthorizeInput, ConnectionTestInput, ReportExportInput,
    RepositoryDiscoverInput, TaskRetryInput,
};
use git_repo_migrator_application::{BatchControl, IpcError};
use std::sync::Arc;

use tauri::State;

use crate::dto::{
    BatchIdInput, BatchSnapshot, CapabilitySummary, ConnectionSaveInput, ConnectionSnapshot,
    MigrationSnapshot, PlanFreezeInput, PlanPreviewRequest, PlanPreviewSnapshot, PlanSnapshot,
    ReportSnapshot, RepositoryImportInput, RepositoryImportReport, RepositoryMappingInput,
    RepositoryPage, RepositorySnapshot, TargetProbeInput,
};
use crate::state::{AppState, AuthorizeOutcome, ExportOutcome, RetryOutcome};

/// Names registered with `tauri::generate_handler!`. Kept as data so a test can
/// assert the surface never grows a dangerous capability by accident.
pub const COMMAND_WHITELIST: [&str; 17] = [
    "migration_snapshot",
    "connection_test",
    "connection_save",
    "connection_authorize",
    "repository_discover",
    "repository_import",
    "repository_probe_target",
    "repository_set_mapping",
    "plan_preview",
    "plan_freeze",
    "batch_start",
    "batch_pause",
    "batch_resume",
    "batch_cancel",
    "task_retry",
    "report_snapshot",
    "report_export",
];

/// The authoritative renderer state. Called on mount, on window focus and
/// whenever an event envelope reports a revision ahead of the last snapshot.
#[tauri::command]
pub fn migration_snapshot(state: State<'_, Arc<AppState>>) -> Result<MigrationSnapshot, IpcError> {
    state.snapshot()
}

#[tauri::command]
pub fn connection_test(
    state: State<'_, Arc<AppState>>,
    input: ConnectionTestInput,
) -> Result<Vec<CapabilitySummary>, IpcError> {
    state.test_connection(&input)
}

#[tauri::command]
pub fn connection_save(
    state: State<'_, Arc<AppState>>,
    input: ConnectionSaveInput,
) -> Result<ConnectionSnapshot, IpcError> {
    state.save_connection(&input)
}

/// Opens the native credential-entry window.
///
/// This is the closest the command surface comes to a credential capability, and
/// it deliberately stops short of one: the input is a name, the output is a
/// reference, and the token is read by a separate console process that the
/// renderer cannot talk to.
#[tauri::command]
pub fn connection_authorize(
    state: State<'_, Arc<AppState>>,
    input: ConnectionAuthorizeInput,
) -> Result<AuthorizeOutcome, IpcError> {
    state.authorize_connection(&input)
}

#[tauri::command]
pub fn repository_discover(
    state: State<'_, Arc<AppState>>,
    input: RepositoryDiscoverInput,
) -> Result<RepositoryPage, IpcError> {
    state.discover_repositories(&input.connection_id, &input.query)
}

#[tauri::command]
pub fn repository_import(
    state: State<'_, Arc<AppState>>,
    input: RepositoryImportInput,
) -> Result<RepositoryImportReport, IpcError> {
    state.import_repositories(&input)
}

#[tauri::command]
pub fn repository_probe_target(
    state: State<'_, Arc<AppState>>,
    input: TargetProbeInput,
) -> Result<RepositorySnapshot, IpcError> {
    state.probe_target(&input)
}

#[tauri::command]
pub fn repository_set_mapping(
    state: State<'_, Arc<AppState>>,
    input: RepositoryMappingInput,
) -> Result<RepositorySnapshot, IpcError> {
    state.set_mapping(
        &input.repository_id,
        &input.target_url,
        input.target_name.as_deref(),
    )
}

#[tauri::command]
pub fn plan_preview(
    state: State<'_, Arc<AppState>>,
    input: PlanPreviewRequest,
) -> Result<PlanPreviewSnapshot, IpcError> {
    state.preview_plan(&input)
}

/// Destructive-plan gate. The backend re-checks the confirmation phrase it
/// issued and every degraded module acknowledgement; the renderer cannot
/// self-authorise.
#[tauri::command]
pub fn plan_freeze(
    state: State<'_, Arc<AppState>>,
    input: PlanFreezeInput,
) -> Result<PlanSnapshot, IpcError> {
    state.freeze_plan(&input)
}

/// Re-verifies the persisted plan hash, the capability snapshot and every
/// target state before a single remote write is scheduled.
#[tauri::command]
pub fn batch_start(
    state: State<'_, Arc<AppState>>,
    input: BatchStartInput,
) -> Result<BatchSnapshot, IpcError> {
    state.start_batch(&input)
}

#[tauri::command]
pub fn batch_pause(
    state: State<'_, Arc<AppState>>,
    input: BatchIdInput,
) -> Result<BatchSnapshot, IpcError> {
    state.set_control(&input, BatchControl::Paused)
}

#[tauri::command]
pub fn batch_resume(
    state: State<'_, Arc<AppState>>,
    input: BatchIdInput,
) -> Result<BatchSnapshot, IpcError> {
    state.set_control(&input, BatchControl::Running)
}

/// Cancelling stops scheduling. It never rolls back a repository that already
/// completed, and never deletes refs on the target.
#[tauri::command]
pub fn batch_cancel(
    state: State<'_, Arc<AppState>>,
    input: BatchIdInput,
) -> Result<BatchSnapshot, IpcError> {
    state.set_control(&input, BatchControl::Cancelled)
}

#[tauri::command]
pub fn task_retry(
    state: State<'_, Arc<AppState>>,
    input: TaskRetryInput,
) -> Result<RetryOutcome, IpcError> {
    state.retry_tasks(&input)
}

#[tauri::command]
pub fn report_snapshot(
    state: State<'_, Arc<AppState>>,
    input: BatchIdInput,
) -> Result<ReportSnapshot, IpcError> {
    state.report(&input.batch_id)
}

#[tauri::command]
pub fn report_export(
    state: State<'_, Arc<AppState>>,
    input: ReportExportInput,
) -> Result<ExportOutcome, IpcError> {
    state.export_report(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_exposes_no_shell_file_or_credential_capability() {
        for name in COMMAND_WHITELIST {
            let lowered = name.to_ascii_lowercase();
            for forbidden in [
                "shell",
                "exec",
                "spawn",
                "eval",
                "read_file",
                "write_file",
                "fs_",
                "path_",
                "credential",
                "secret",
                "token",
            ] {
                assert!(
                    !lowered.contains(forbidden),
                    "command {name} exposes a forbidden capability: {forbidden}"
                );
            }
        }
    }

    #[test]
    fn whitelist_has_no_duplicates_and_covers_every_step() {
        let mut sorted = COMMAND_WHITELIST.to_vec();
        sorted.sort_unstable();
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(sorted, deduped, "duplicate command name in the whitelist");

        for prefix in [
            "migration_",
            "connection_",
            "repository_",
            "plan_",
            "batch_",
            "task_",
            "report_",
        ] {
            assert!(
                COMMAND_WHITELIST
                    .iter()
                    .any(|name| name.starts_with(prefix)),
                "no command covers {prefix}"
            );
        }
    }

    /// The authorize command may only move a *name* and a *reference*. If its
    /// payload ever grew a secret field the whole credential boundary would be
    /// gone, so the shape is pinned here as well as in `ipc_contract.rs`.
    #[test]
    fn authorize_carries_a_name_in_and_a_reference_out() {
        let input: ConnectionAuthorizeInput =
            serde_json::from_str(r#"{"name":"source"}"#).expect("name accepted");
        assert_eq!(input.name, "source");
        assert!(serde_json::from_str::<ConnectionAuthorizeInput>(
            r#"{"name":"source","token":"ghp-secret"}"#
        )
        .is_err());

        let outcome = AuthorizeOutcome {
            credential_ref: "credential/windows/abc12345".to_owned(),
            instructions: "已打开凭据录入窗口".to_owned(),
        };
        let json = serde_json::to_string(&outcome).expect("serialised");
        for forbidden in ["token", "secret", "password"] {
            assert!(!json.contains(forbidden), "{forbidden} leaked into {json}");
        }
    }

    /// Stage recording is backend-only. If one of these ever appears in the
    /// whitelist the renderer could fabricate a successful migration.
    #[test]
    fn stage_recording_is_not_reachable_from_the_renderer() {
        for backend_only in [
            "begin_stage",
            "report_progress",
            "fail_stage",
            "complete_task",
            "record_module_result",
            "set_cleanup_state",
        ] {
            assert!(
                !COMMAND_WHITELIST.contains(&backend_only),
                "{backend_only} must not be callable from the renderer"
            );
        }
    }
}
