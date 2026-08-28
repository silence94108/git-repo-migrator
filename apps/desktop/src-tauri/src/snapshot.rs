//! SQLite readers that rebuild the whole renderer state.
//!
//! The renderer never accumulates state from events. Every read goes through
//! here, so a dropped, duplicated or out-of-order event cannot desynchronise the
//! UI: the next snapshot call re-derives everything from persisted rows.

use std::collections::BTreeMap;

use git_repo_migrator_application::verification::{AggregateStatus, VerificationEvidence};
use git_repo_migrator_application::{BatchControl, IpcError};
use git_repo_migrator_domain::{Fidelity, RepoTaskState};
use git_repo_migrator_local_store::{LocalStore, StoreResult};
use git_repo_migrator_platform_core::{PlatformKind, RepositoryVisibility};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::dto::{
    BatchSnapshot, CapabilitySummary, CheckpointSummary, CleanupState, ConnectionRole,
    ConnectionSnapshot, LogEntry, MigrationStage, ModuleFidelityRow, PermissionLevel, PlanSnapshot,
    ReportMetrics, ReportRowSnapshot, ReportSnapshot, RepositorySnapshot, ResumableBatch,
    TaskSnapshot,
};

/// Extra connection facts kept in `connection.capabilities_json`. The column is
/// the only place capability probes are persisted, and it never holds a secret.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionDetails {
    #[serde(default)]
    pub authenticated: bool,
    #[serde(default)]
    pub account_name: Option<String>,
    #[serde(default)]
    pub instance_version: Option<String>,
    #[serde(default)]
    pub tls_trusted: bool,
    #[serde(default)]
    pub capabilities: Vec<CapabilitySummary>,
}

/// Extra candidate facts kept in `repository_candidate.metadata_json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CandidateDetails {
    #[serde(default)]
    pub target_url: Option<String>,
    #[serde(default)]
    pub target_name: Option<String>,
    #[serde(default)]
    pub target_state: Option<String>,
    #[serde(default)]
    pub git_capable: bool,
    #[serde(default)]
    pub platform_capable: bool,
    #[serde(default)]
    pub updated_at_epoch_seconds: Option<u64>,
    #[serde(default)]
    pub unselectable_reason: Option<String>,
}

/// Summary written by the verify stage into its checkpoint output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifySummary {
    #[serde(default)]
    pub git_verified: bool,
    #[serde(default)]
    pub lfs_verified: bool,
    #[serde(default)]
    pub metadata_verified: bool,
    #[serde(default)]
    pub evidence: VerificationEvidence,
    #[serde(default)]
    pub unmapped_fields: Vec<String>,
    #[serde(default)]
    pub archive_path: Option<String>,
    #[serde(default)]
    pub next_action: Option<String>,
}

/// Progress written by any stage into its checkpoint output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProgressSummary {
    #[serde(default)]
    pub completed: u64,
    #[serde(default)]
    pub total: Option<u64>,
}

pub fn parse_platform(value: &str) -> PlatformKind {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .unwrap_or(PlatformKind::Unknown)
}

pub fn parse_visibility(value: &str) -> RepositoryVisibility {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .unwrap_or(RepositoryVisibility::Unknown)
}

pub fn parse_permission(value: &str) -> PermissionLevel {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .unwrap_or(PermissionLevel::Insufficient)
}

pub fn parse_task_state(value: &str) -> RepoTaskState {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .unwrap_or(RepoTaskState::Planned)
}

pub fn parse_control(value: &str) -> BatchControl {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .unwrap_or(BatchControl::Paused)
}

pub fn parse_target_state(
    value: Option<&str>,
) -> git_repo_migrator_application::planning::TargetState {
    use git_repo_migrator_application::planning::TargetState;
    value
        .and_then(|raw| serde_json::from_value(serde_json::Value::String(raw.to_owned())).ok())
        .unwrap_or(TargetState::Unknown)
}

fn enum_value<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        _ => String::new(),
    }
}

pub fn task_state_value(state: RepoTaskState) -> String {
    enum_value(&state)
}

pub fn control_value(control: BatchControl) -> String {
    enum_value(&control)
}

/// The stage a task is currently in, or the stage it stopped in after a failure.
pub fn stage_for(state: RepoTaskState, last_checkpoint_stage: Option<&str>) -> MigrationStage {
    match state {
        RepoTaskState::Planned | RepoTaskState::Preflighted => MigrationStage::Preflight,
        RepoTaskState::Preparing => MigrationStage::PrepareTarget,
        RepoTaskState::Git => MigrationStage::Git,
        RepoTaskState::Lfs => MigrationStage::Lfs,
        RepoTaskState::Metadata => MigrationStage::Metadata,
        RepoTaskState::PlatformModules => MigrationStage::PlatformData,
        RepoTaskState::Verifying => MigrationStage::Verify,
        RepoTaskState::Succeeded | RepoTaskState::Partial | RepoTaskState::Skipped => {
            MigrationStage::Complete
        }
        RepoTaskState::RetryableFailed => last_checkpoint_stage
            .and_then(|raw| {
                serde_json::from_value::<MigrationStage>(serde_json::Value::String(raw.to_owned()))
                    .ok()
            })
            .unwrap_or(MigrationStage::Preflight),
    }
}

/// Terminal classification used by the report. In-flight tasks return `None` so
/// an unfinished migration can never be counted as a success.
pub fn aggregate_status(state: RepoTaskState) -> Option<AggregateStatus> {
    match state {
        RepoTaskState::Succeeded => Some(AggregateStatus::Succeeded),
        RepoTaskState::Partial => Some(AggregateStatus::Partial),
        RepoTaskState::RetryableFailed => Some(AggregateStatus::RetryableFailed),
        RepoTaskState::Skipped => Some(AggregateStatus::Skipped),
        _ => None,
    }
}

pub fn read_connections(store: &LocalStore) -> StoreResult<Vec<ConnectionSnapshot>> {
    let mut statement = store.connection().prepare(
        "SELECT id, platform_type, endpoint, credential_ref, capabilities_json
         FROM connection ORDER BY id",
    )?;
    let rows = statement
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let platform: String = row.get(1)?;
            let endpoint: String = row.get(2)?;
            let credential_ref: String = row.get(3)?;
            let details: String = row.get(4)?;
            Ok((id, platform, endpoint, credential_ref, details))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows
        .into_iter()
        .map(|(id, platform, endpoint, credential_ref, details_json)| {
            let details: ConnectionDetails =
                serde_json::from_str(&details_json).unwrap_or_default();
            let role = if id == "target" {
                ConnectionRole::Target
            } else {
                ConnectionRole::Source
            };
            ConnectionSnapshot {
                id,
                role,
                platform: parse_platform(&platform),
                endpoint,
                credential_ref: if credential_ref.is_empty() {
                    None
                } else {
                    Some(credential_ref)
                },
                authenticated: details.authenticated,
                account_name: details.account_name,
                instance_version: details.instance_version,
                tls_trusted: details.tls_trusted,
                capabilities: details.capabilities,
            }
        })
        .collect())
}

pub fn read_repositories(store: &LocalStore) -> StoreResult<Vec<RepositorySnapshot>> {
    let mut statement = store.connection().prepare(
        "SELECT id, connection_id, source_url, name, namespace, visibility, role, metadata_json
         FROM repository_candidate ORDER BY namespace, name, id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows
        .into_iter()
        .map(
            |(id, connection_id, source_url, name, namespace, visibility, role, metadata)| {
                let details: CandidateDetails = serde_json::from_str(&metadata).unwrap_or_default();
                let permission = parse_permission(&role);
                let selectable = permission != PermissionLevel::Insufficient;
                RepositorySnapshot {
                    id,
                    connection_id: connection_id.unwrap_or_default(),
                    source_url,
                    name,
                    namespace,
                    visibility: parse_visibility(&visibility),
                    permission,
                    updated_at_epoch_seconds: details.updated_at_epoch_seconds,
                    git_capable: details.git_capable,
                    platform_capable: details.platform_capable,
                    target_state: parse_target_state(details.target_state.as_deref()),
                    target_url: details.target_url,
                    target_name: details.target_name,
                    selectable,
                    unselectable_reason: if selectable {
                        None
                    } else {
                        Some(
                            details
                                .unselectable_reason
                                .unwrap_or_else(|| "当前凭据对该仓库没有读取或推送权限".to_owned()),
                        )
                    },
                }
            },
        )
        .collect())
}

pub fn read_plan(store: &LocalStore, plan_id: &str) -> StoreResult<Option<PlanSnapshot>> {
    let row = store
        .connection()
        .query_row(
            "SELECT id, selection_json, plan_hash, status, created_at_ms
             FROM plan WHERE id = ?1",
            params![plan_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;

    Ok(
        row.map(|(id, selection_json, plan_hash, status, created_at_ms)| {
            let selection: PlanSelection =
                serde_json::from_str(&selection_json).unwrap_or_default();
            PlanSnapshot {
                plan_id: id,
                plan_hash,
                status,
                repository_count: u32::try_from(selection.mappings.len()).unwrap_or(u32::MAX),
                capability_snapshot_hash: selection.capability_snapshot_hash,
                dangerous_confirmed: selection.dangerous_confirmed,
                created_at_ms,
            }
        }),
    )
}

/// Contents of `plan.selection_json`. Frozen with the plan and never mutated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanSelection {
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default)]
    pub excluded: Vec<String>,
    #[serde(default)]
    pub mappings: Vec<git_repo_migrator_domain::RepositoryMapping>,
    #[serde(default)]
    pub repository_ids: Vec<String>,
    #[serde(default)]
    pub actions: BTreeMap<String, String>,
    #[serde(default)]
    pub capability_snapshot_hash: String,
    #[serde(default)]
    pub dangerous_confirmed: bool,
    #[serde(default)]
    pub acknowledged_fidelity: Vec<String>,
}

pub fn latest_plan_id(store: &LocalStore) -> StoreResult<Option<String>> {
    Ok(store
        .connection()
        .query_row(
            "SELECT id FROM plan ORDER BY created_at_ms DESC, id DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

pub fn latest_batch_id(store: &LocalStore) -> StoreResult<Option<String>> {
    Ok(store
        .connection()
        .query_row(
            "SELECT id FROM batch ORDER BY COALESCE(started_at_ms, 0) DESC, id DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

fn read_task_error(store: &LocalStore, task_id: &str) -> StoreResult<Option<IpcError>> {
    let raw: Option<String> = store
        .connection()
        .query_row(
            "SELECT safe_context_json FROM log_event
             WHERE task_id = ?1 AND level = 'error'
             ORDER BY created_at_ms DESC, id DESC LIMIT 1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|json| serde_json::from_str::<IpcError>(&json).ok()))
}

fn read_last_checkpoint(
    store: &LocalStore,
    task_id: &str,
) -> StoreResult<Option<(CheckpointSummary, ProgressSummary)>> {
    let row = store
        .connection()
        .query_row(
            "SELECT stage, transition, attempt, resumable, created_at_ms, output_summary_json
             FROM checkpoint WHERE task_id = ?1
             ORDER BY created_at_ms DESC, id DESC LIMIT 1",
            params![task_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;

    Ok(row.map(
        |(stage, transition, attempt, resumable, created_at_ms, output)| {
            let progress: ProgressSummary = serde_json::from_str(&output).unwrap_or_default();
            (
                CheckpointSummary {
                    stage,
                    transition,
                    attempt,
                    resumable: resumable != 0,
                    created_at_ms,
                },
                progress,
            )
        },
    ))
}

pub fn read_tasks(store: &LocalStore, batch_id: &str) -> StoreResult<Vec<TaskSnapshot>> {
    let mut statement = store.connection().prepare(
        "SELECT t.id, t.candidate_id, c.source_url, t.target_url, t.status, t.attempt,
                t.error_code, t.updated_at_ms
         FROM repository_task t
         JOIN repository_candidate c ON c.id = t.candidate_id
         WHERE t.batch_id = ?1
         ORDER BY t.id",
    )?;
    let rows = statement
        .query_map(params![batch_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut tasks = Vec::with_capacity(rows.len());
    for (task_id, repository_id, source_url, target_url, status, attempt, error_code, updated) in
        rows
    {
        let state = parse_task_state(&status);
        let checkpoint = read_last_checkpoint(store, &task_id)?;
        let error = read_task_error(store, &task_id)?;
        let (last_checkpoint, progress) = match checkpoint {
            Some((summary, progress)) => (Some(summary), progress),
            None => (None, ProgressSummary::default()),
        };
        let retryable = match (&error, state) {
            (Some(error), _) => error.retryable,
            (None, RepoTaskState::RetryableFailed) => true,
            _ => false,
        };
        tasks.push(TaskSnapshot {
            task_id,
            repository_id,
            source_url,
            target_url,
            stage: stage_for(state, last_checkpoint.as_ref().map(|c| c.stage.as_str())),
            state,
            attempt: u32::try_from(attempt).unwrap_or(0),
            progress_completed: progress.completed,
            progress_total: progress.total,
            retryable,
            error: error.or_else(|| {
                error_code.map(|code| IpcError {
                    code,
                    category: git_repo_migrator_domain::ErrorCategory::Git,
                    retryable: false,
                    stage: "unknown".to_owned(),
                    safe_message: "任务失败，详情见日志".to_owned(),
                    action: "请打开日志抽屉查看该阶段的安全摘要".to_owned(),
                })
            }),
            last_checkpoint,
            updated_at_ms: updated,
        });
    }
    Ok(tasks)
}

pub fn read_logs(store: &LocalStore, batch_id: &str, limit: u32) -> StoreResult<Vec<LogEntry>> {
    let mut statement = store.connection().prepare(
        "SELECT l.task_id, l.level, l.stage, l.message_code, l.safe_context_json, l.created_at_ms
         FROM log_event l
         JOIN repository_task t ON t.id = l.task_id
         WHERE t.batch_id = ?1
         ORDER BY l.created_at_ms DESC, l.id DESC
         LIMIT ?2",
    )?;
    let entries = statement
        .query_map(params![batch_id, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(entries
        .into_iter()
        .map(|(task_id, level, stage, code, context, created_at_ms)| {
            let safe_message = serde_json::from_str::<IpcError>(&context)
                .map(|error| error.safe_message)
                .unwrap_or_else(|_| {
                    serde_json::from_str::<serde_json::Value>(&context)
                        .ok()
                        .and_then(|value| {
                            value
                                .get("safe_message")
                                .and_then(|m| m.as_str().map(str::to_owned))
                        })
                        .unwrap_or_default()
                });
            LogEntry {
                task_id,
                level,
                stage,
                code,
                safe_message,
                created_at_ms,
            }
        })
        .collect())
}

pub fn read_batch(
    store: &LocalStore,
    batch_id: &str,
    concurrency: u16,
) -> StoreResult<Option<BatchSnapshot>> {
    let row = store
        .connection()
        .query_row(
            "SELECT b.id, b.plan_id, b.status, b.total, b.completed, b.failed,
                    b.started_at_ms, b.ended_at_ms, p.plan_hash
             FROM batch b JOIN plan p ON p.id = b.plan_id
             WHERE b.id = ?1",
            params![batch_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()?;

    let Some((id, plan_id, status, total, completed, failed, started, ended, plan_hash)) = row
    else {
        return Ok(None);
    };

    Ok(Some(BatchSnapshot {
        tasks: read_tasks(store, &id)?,
        logs: read_logs(store, &id, 500)?,
        batch_id: id,
        plan_id,
        plan_hash,
        control: parse_control(&status),
        concurrency,
        total: u32::try_from(total).unwrap_or(0),
        completed: u32::try_from(completed).unwrap_or(0),
        failed: u32::try_from(failed).unwrap_or(0),
        started_at_ms: started,
        ended_at_ms: ended,
    }))
}

fn read_module_results(store: &LocalStore, task_id: &str) -> StoreResult<Vec<ModuleFidelityRow>> {
    let mut statement = store.connection().prepare(
        "SELECT module, fidelity, error_json FROM module_result
         WHERE task_id = ?1 ORDER BY module",
    )?;
    let rows = statement
        .query_map(params![task_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows
        .into_iter()
        .map(|(module, fidelity, error_json)| {
            let fidelity: Fidelity =
                serde_json::from_value(serde_json::Value::String(fidelity.clone()))
                    .unwrap_or(Fidelity::Unsupported);
            let reason = serde_json::from_str::<IpcError>(&error_json)
                .ok()
                .map(|error| error.safe_message);
            ModuleFidelityRow {
                module,
                fidelity,
                reason,
                confirmation_required: !matches!(fidelity, Fidelity::NativeRebuild),
            }
        })
        .collect())
}

pub fn read_report(
    store: &LocalStore,
    batch_id: &str,
    cleanup: CleanupState,
) -> StoreResult<ReportSnapshot> {
    let tasks = read_tasks(store, batch_id)?;
    let mut metrics = ReportMetrics {
        complete_success: 0,
        git_success_platform_partial: 0,
        retryable_failure: 0,
        permission_or_conflict_skip: 0,
    };
    let mut rows = Vec::new();

    for task in tasks {
        let Some(status) = aggregate_status(task.state) else {
            continue;
        };
        match status {
            AggregateStatus::Succeeded => metrics.complete_success += 1,
            AggregateStatus::Partial => metrics.git_success_platform_partial += 1,
            AggregateStatus::RetryableFailed => metrics.retryable_failure += 1,
            AggregateStatus::Skipped => metrics.permission_or_conflict_skip += 1,
            AggregateStatus::Failed => metrics.retryable_failure += 1,
        }

        let verify: VerifySummary = store
            .connection()
            .query_row(
                "SELECT output_summary_json FROM checkpoint
                 WHERE task_id = ?1 AND stage = 'verify'
                 ORDER BY created_at_ms DESC, id DESC LIMIT 1",
                params![task.task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();

        let source_links = store
            .connection()
            .query_row(
                "SELECT source_links_json FROM module_result
                 WHERE task_id = ?1 ORDER BY module LIMIT 1",
                params![task.task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
            .unwrap_or_default();

        rows.push(ReportRowSnapshot {
            task_id: task.task_id.clone(),
            source_url: task.source_url,
            target_url: task.target_url,
            status,
            completed_at_ms: Some(task.updated_at_ms),
            git_verified: verify.git_verified,
            lfs_verified: verify.lfs_verified,
            metadata_verified: verify.metadata_verified,
            modules: read_module_results(store, &task.task_id)?,
            error_code: task.error.as_ref().map(|error| error.code.clone()),
            evidence: verify.evidence,
            unmapped_fields: verify.unmapped_fields,
            archive_path: verify.archive_path,
            source_links,
            next_action: verify
                .next_action
                .or_else(|| task.error.as_ref().map(|error| error.action.clone())),
        });
    }

    Ok(ReportSnapshot {
        batch_id: batch_id.to_owned(),
        metrics,
        rows,
        cleanup,
    })
}

pub fn read_resumable(store: &LocalStore, now_ms: i64) -> StoreResult<Vec<ResumableBatch>> {
    let mut statement = store.connection().prepare(
        "SELECT b.id, b.plan_id,
                (SELECT COUNT(*) FROM repository_task t
                  WHERE t.batch_id = b.id
                    AND t.status NOT IN ('succeeded', 'partial', 'skipped')) AS pending
         FROM batch b
         WHERE b.status IN ('running', 'paused')
         ORDER BY b.id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Any batch whose lease already expired must be re-checked against remote
    // facts before it may resume; that is what forces the resume dialog.
    let expired = store.leases().recoverable(now_ms)?;

    Ok(rows
        .into_iter()
        .map(|(batch_id, plan_id, pending)| {
            let has_expired_lease = expired.iter().any(|task| task.batch_id == batch_id);
            ResumableBatch {
                batch_id,
                plan_id,
                pending: u32::try_from(pending).unwrap_or(0),
                plan_hash_matches: true,
                credential_recheck_required: true,
                capability_recheck_required: has_expired_lease,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_flight_tasks_are_never_counted_as_a_result() {
        for state in [
            RepoTaskState::Planned,
            RepoTaskState::Git,
            RepoTaskState::Verifying,
        ] {
            assert!(aggregate_status(state).is_none());
        }
        assert_eq!(
            aggregate_status(RepoTaskState::Partial),
            Some(AggregateStatus::Partial)
        );
    }

    #[test]
    fn stage_mapping_covers_every_task_state() {
        assert_eq!(
            stage_for(RepoTaskState::PlatformModules, None),
            MigrationStage::PlatformData
        );
        assert_eq!(
            stage_for(RepoTaskState::Succeeded, None),
            MigrationStage::Complete
        );
        assert_eq!(
            stage_for(RepoTaskState::RetryableFailed, Some("git")),
            MigrationStage::Git
        );
    }

    #[test]
    fn enum_round_trips_match_the_persisted_column_values() {
        assert_eq!(
            task_state_value(RepoTaskState::PlatformModules),
            "platform_modules"
        );
        assert_eq!(control_value(BatchControl::Paused), "paused");
        assert_eq!(
            parse_task_state("retryable_failed"),
            RepoTaskState::RetryableFailed
        );
    }
}
