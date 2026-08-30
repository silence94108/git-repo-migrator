//! Native shell for the Git Repo Migrator window.
//!
//! Wiring only: the state store, the command whitelist and the event channel.
//! No business rule lives here.

pub mod commands;
pub mod discovery;
pub mod dto;
pub mod errors;
pub mod events;
pub mod platform_gateway;
pub mod ports;
pub mod runner;
pub mod snapshot;
pub mod state;

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod flow_tests;
#[cfg(test)]
mod runner_tests;

use std::sync::Arc;

use state::AppState;

/// SQLite file inside the Tauri app-data directory. Migration state must
/// survive a crash, so it is never kept in a temporary directory.
const STORE_FILE: &str = "migration-state.sqlite3";
/// Mirror clones and other large temporaries live here, next to the state file
/// and inside the per-user app-data directory.
const WORKSPACE_DIR: &str = "workspace";

fn build_state<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> (
    AppState,
    std::path::PathBuf,
    Arc<git_repo_migrator_credential_store::CredentialStore>,
) {
    let data_dir = tauri::Manager::path(app)
        .app_data_dir()
        .ok()
        .inspect(|dir| {
            let _ = std::fs::create_dir_all(dir);
        });

    let state = match data_dir.as_ref().map(|dir| dir.join(STORE_FILE)) {
        Some(path) => AppState::open(&path).or_else(|_| AppState::in_memory()),
        None => AppState::in_memory(),
    }
    .expect("the local state store must be creatable");

    // The probe is optional: without a usable system Git the target state stays
    // unknown, which blocks the plan instead of risking a blind write.
    let state = match ports::GitLsRemoteProbe::system() {
        Ok(probe) => state.with_target_probe(Arc::new(probe)),
        Err(_) => state,
    };
    // API discovery goes through the real transport; the credential store is the
    // only thing that ever holds a token, and it is not reachable from a command.
    let credentials = Arc::new(git_repo_migrator_credential_store::CredentialStore::new());
    let state = state.with_discovery(Arc::new(discovery::ApiDiscoveryGateway::new(Arc::clone(
        &credentials,
    ))));
    // Connection testing probes the real platform API: a wrong token or an
    // unreachable instance is reported as such instead of a canned table.
    let state = state.with_connection_tester(Arc::new(discovery::ApiConnectionTester::new(
        Arc::clone(&credentials),
    )));
    let workspace = data_dir
        .map(|dir| dir.join(WORKSPACE_DIR))
        .unwrap_or_else(|| std::env::temp_dir().join("git-repo-migrator-workspace"));
    (state, workspace, credentials)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let (state, workspace, credentials) = build_state(&handle);
            let state = Arc::new(state);
            // The pool holds a weak handle so the state is not kept alive by
            // its own workers. It shares the credential store with discovery
            // and connection testing, so the workers resolve exactly the
            // references the operator authorised.
            let launcher = Arc::new(runner::ThreadPoolLauncher::new(
                Arc::downgrade(&state),
                Arc::new(events::TauriEventSink::new(handle)),
                workspace,
                credentials,
            ));
            state.install_launcher(launcher);
            tauri::Manager::manage(app, state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::migration_snapshot,
            commands::connection_test,
            commands::connection_save,
            commands::connection_authorize,
            commands::repository_discover,
            commands::repository_import,
            commands::repository_probe_target,
            commands::repository_set_mapping,
            commands::plan_preview,
            commands::plan_freeze,
            commands::batch_start,
            commands::batch_pause,
            commands::batch_resume,
            commands::batch_cancel,
            commands::task_retry,
            commands::report_snapshot,
            commands::report_export,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Git Repo Migrator");
}
