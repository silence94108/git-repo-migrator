//! Native shell for the Git Repo Migrator window.
//!
//! Wiring only: the state store, the command whitelist and the event channel.
//! No business rule lives here.

pub mod commands;
pub mod dto;
pub mod errors;
pub mod events;
pub mod ports;
pub mod snapshot;
pub mod state;

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod flow_tests;

use std::sync::Arc;

use state::AppState;

/// SQLite file inside the Tauri app-data directory. Migration state must
/// survive a crash, so it is never kept in a temporary directory.
const STORE_FILE: &str = "migration-state.sqlite3";

fn build_state<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> AppState {
    let path = tauri::Manager::path(app)
        .app_data_dir()
        .map(|dir| {
            let _ = std::fs::create_dir_all(&dir);
            dir.join(STORE_FILE)
        })
        .ok();

    let state = match path {
        Some(path) => AppState::open(&path).or_else(|_| AppState::in_memory()),
        None => AppState::in_memory(),
    }
    .expect("the local state store must be creatable");

    // The probe is optional: without a usable system Git the target state stays
    // unknown, which blocks the plan instead of risking a blind write.
    match ports::GitLsRemoteProbe::system() {
        Ok(probe) => state.with_target_probe(Arc::new(probe)),
        Err(_) => state,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let state = build_state(&app.handle().clone());
            tauri::Manager::manage(app, state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::migration_snapshot,
            commands::connection_test,
            commands::connection_save,
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
