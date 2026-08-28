//! Progress event mapping.
//!
//! Events are hints, not state. Each envelope carries the store revision that
//! produced it so the renderer can tell whether its snapshot is behind, and no
//! event body may contain a secret, a raw platform response or a file path
//! outside the product workspace.

use std::sync::Mutex;

use git_repo_migrator_application::{IpcError, MigrationEvent};
use git_repo_migrator_domain::Fidelity;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime};

use crate::dto::MigrationStage;

/// Single channel for every migration event. One name keeps the renderer's
/// listener surface auditable.
pub const MIGRATION_EVENT: &str = "migration://event";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Store revision at emit time. The renderer refetches whenever this is
    /// ahead of the revision in its last snapshot.
    pub revision: u64,
    pub event: MigrationEvent,
}

pub trait EventSink: Send + Sync {
    fn emit(&self, envelope: &EventEnvelope);
}

/// Emits through the Tauri window. A failed emit is not fatal: the renderer
/// re-reads the authoritative snapshot on its next poll.
pub struct TauriEventSink<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> TauriEventSink<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        Self { app }
    }
}

impl<R: Runtime> EventSink for TauriEventSink<R> {
    fn emit(&self, envelope: &EventEnvelope) {
        let _ = self.app.emit(MIGRATION_EVENT, envelope);
    }
}

/// Collects envelopes instead of emitting them, so event mapping can be
/// asserted without a window.
#[derive(Debug, Default)]
pub struct RecordingSink {
    envelopes: Mutex<Vec<EventEnvelope>>,
}

impl RecordingSink {
    pub fn envelopes(&self) -> Vec<EventEnvelope> {
        self.envelopes
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, envelope: &EventEnvelope) {
        if let Ok(mut guard) = self.envelopes.lock() {
            guard.push(envelope.clone());
        }
    }
}

fn stage_text(stage: MigrationStage) -> String {
    match serde_json::to_value(stage) {
        Ok(serde_json::Value::String(text)) => text,
        _ => String::new(),
    }
}

pub fn batch_started(revision: u64, batch_id: &str) -> EventEnvelope {
    EventEnvelope {
        revision,
        event: MigrationEvent::BatchStarted {
            batch_id: batch_id.to_owned(),
        },
    }
}

pub fn stage_changed(
    revision: u64,
    batch_id: &str,
    task_id: &str,
    stage: MigrationStage,
) -> EventEnvelope {
    EventEnvelope {
        revision,
        event: MigrationEvent::TaskStageChanged {
            batch_id: batch_id.to_owned(),
            task_id: task_id.to_owned(),
            stage: stage_text(stage),
        },
    }
}

pub fn progress(
    revision: u64,
    batch_id: &str,
    task_id: &str,
    completed: u64,
    total: Option<u64>,
) -> EventEnvelope {
    EventEnvelope {
        revision,
        event: MigrationEvent::TaskProgress {
            batch_id: batch_id.to_owned(),
            task_id: task_id.to_owned(),
            completed,
            total,
        },
    }
}

/// Only the stable code and the safe message travel to the renderer; the
/// category, action and full context stay in the snapshot and the log table.
pub fn warning(revision: u64, batch_id: &str, task_id: &str, error: &IpcError) -> EventEnvelope {
    EventEnvelope {
        revision,
        event: MigrationEvent::TaskWarning {
            batch_id: batch_id.to_owned(),
            task_id: task_id.to_owned(),
            code: error.code.clone(),
            safe_message: error.safe_message.clone(),
        },
    }
}

pub fn task_completed(
    revision: u64,
    batch_id: &str,
    task_id: &str,
    status: &str,
    fidelity: Vec<Fidelity>,
) -> EventEnvelope {
    EventEnvelope {
        revision,
        event: MigrationEvent::TaskCompleted {
            batch_id: batch_id.to_owned(),
            task_id: task_id.to_owned(),
            status: status.to_owned(),
            fidelity,
        },
    }
}

pub fn batch_completed(revision: u64, batch_id: &str, status: &str) -> EventEnvelope {
    EventEnvelope {
        revision,
        event: MigrationEvent::BatchCompleted {
            batch_id: batch_id.to_owned(),
            status: status.to_owned(),
        },
    }
}

/// Field names an event payload must never contain.
const FORBIDDEN_EVENT_KEYS: [&str; 8] = [
    "token",
    "access_token",
    "password",
    "secret",
    "private_key",
    "cookie",
    "authorization",
    "response_body",
];

/// Returns the forbidden keys found in a serialised envelope. Used by tests and
/// by a debug assertion on the emit path.
pub fn forbidden_keys(envelope: &EventEnvelope) -> Vec<&'static str> {
    let json = serde_json::to_string(envelope)
        .unwrap_or_default()
        .to_ascii_lowercase();
    FORBIDDEN_EVENT_KEYS
        .into_iter()
        .filter(|key| json.contains(key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_repo_migrator_domain::ErrorCategory;

    #[test]
    fn every_event_variant_is_free_of_secret_fields() {
        let error = IpcError {
            code: "platform.rate_limited".into(),
            category: ErrorCategory::RateLimited,
            retryable: true,
            stage: "git".into(),
            safe_message: "触发限流，将在 30 秒后重试".into(),
            action: "无需操作".into(),
        };
        let envelopes = vec![
            batch_started(1, "batch-1"),
            stage_changed(2, "batch-1", "task-1", MigrationStage::Git),
            progress(3, "batch-1", "task-1", 5, Some(10)),
            warning(4, "batch-1", "task-1", &error),
            task_completed(
                5,
                "batch-1",
                "task-1",
                "partial",
                vec![Fidelity::ReadOnlyArchive],
            ),
            batch_completed(6, "batch-1", "completed"),
        ];
        for envelope in &envelopes {
            assert!(
                forbidden_keys(envelope).is_empty(),
                "event leaked a forbidden key: {envelope:?}"
            );
        }
    }

    #[test]
    fn envelope_revision_lets_the_renderer_detect_a_missed_event() {
        let sink = RecordingSink::default();
        sink.emit(&batch_started(7, "batch-1"));
        sink.emit(&progress(9, "batch-1", "task-1", 1, None));
        let recorded = sink.envelopes();
        assert_eq!(recorded.len(), 2);
        // Revision 8 was never observed; the gap is what triggers a refetch.
        assert_eq!(recorded[0].revision, 7);
        assert_eq!(recorded[1].revision, 9);
    }

    #[test]
    fn stage_text_matches_the_contract_enum() {
        assert_eq!(stage_text(MigrationStage::PlatformData), "platform_data");
        assert_eq!(stage_text(MigrationStage::PrepareTarget), "prepare_target");
    }
}
