//! Drift guard between the Rust snapshot DTOs and the renderer's mirror types.
//!
//! `apps/desktop/src/generated/ipc.ts` is generated and owned by the shared IPC
//! contract. The desktop-shell snapshot shapes live in
//! `apps/desktop/src/state/ipcTypes.ts` and are written by hand, so this test
//! serialises every DTO and fails if a field name or enum value the backend
//! actually emits is missing from that file.

use std::collections::BTreeSet;

use git_repo_migrator_application::planning::TargetState;
use git_repo_migrator_application::verification::{AggregateStatus, VerificationEvidence};
use git_repo_migrator_application::{BatchControl, IpcError};
use git_repo_migrator_domain::{ErrorCategory, Fidelity, RepoTaskState};
use git_repo_migrator_platform_core::{PlatformKind, RepositoryVisibility};
use serde::Serialize;
use serde_json::Value;

use crate::commands::COMMAND_WHITELIST;
use crate::dto::{
    BatchSnapshot, CapabilitySummary, CheckpointSummary, CleanupState, ConnectionRole,
    ConnectionSnapshot, FieldMappingRow, LogEntry, MigrationSnapshot, MigrationStage,
    ModuleFidelityRow, PermissionLevel, PlanAction, PlanSnapshot, PreflightMetrics, PreflightRow,
    RefPolicySummary, ReportMetrics, ReportRowSnapshot, ReportSnapshot, RepositoryPage,
    RepositorySnapshot, ResumableBatch, TaskSnapshot, SNAPSHOT_SCHEMA_VERSION,
};
use crate::state::{ExportOutcome, RetryOutcome, RetryRejection};

const RENDERER_TYPES: &str = include_str!("../../src/state/ipcTypes.ts");
const RENDERER_CLIENT: &str = include_str!("../../src/state/ipcClient.ts");
/// Generated and owned by the shared contract; `ipcTypes.ts` re-exports from it,
/// so both files together are the renderer's view of the contract.
const RENDERER_GENERATED: &str = include_str!("../../src/generated/ipc.ts");

fn renderer_declares(needle: &str) -> bool {
    RENDERER_TYPES.contains(needle) || RENDERER_GENERATED.contains(needle)
}

fn enum_text<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(text)) => text,
        other => panic!("expected a string enum, got {other:?}"),
    }
}

fn collect_keys(value: &Value, keys: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                keys.insert(key.clone());
                collect_keys(child, keys);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_keys(item, keys);
            }
        }
        _ => {}
    }
}

fn sample_snapshot() -> MigrationSnapshot {
    let capability = CapabilitySummary {
        module: "issues".into(),
        supported: false,
        permitted: false,
        required_scopes: vec!["repo".into()],
        fidelity: Fidelity::ReadOnlyArchive,
        reason: Some("目标不支持写入".into()),
        degradation: Some("将只迁移 Git 数据".into()),
    };
    let module_row = ModuleFidelityRow {
        module: "issues".into(),
        fidelity: Fidelity::Unsupported,
        reason: Some("无 API".into()),
        confirmation_required: true,
    };
    let error = IpcError {
        code: "ipc.network".into(),
        category: ErrorCategory::Network,
        retryable: true,
        stage: "git".into(),
        safe_message: "连接中断".into(),
        action: "请重试".into(),
    };

    MigrationSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        revision: 7,
        connections: vec![ConnectionSnapshot {
            id: "source".into(),
            role: ConnectionRole::Source,
            platform: PlatformKind::GenericGit,
            endpoint: "https://git.test".into(),
            credential_ref: Some("ref".into()),
            authenticated: true,
            account_name: Some("ops".into()),
            instance_version: Some("1.0".into()),
            tls_trusted: true,
            capabilities: vec![capability],
        }],
        repositories: vec![RepositorySnapshot {
            id: "r1".into(),
            connection_id: "source".into(),
            source_url: "https://git.test/a.git".into(),
            name: "a".into(),
            namespace: "ops".into(),
            visibility: RepositoryVisibility::Private,
            permission: PermissionLevel::GitOnly,
            updated_at_epoch_seconds: Some(1),
            git_capable: true,
            platform_capable: false,
            target_state: TargetState::NonEmpty,
            target_url: Some("https://git.target/a".into()),
            target_name: Some("a".into()),
            selectable: true,
            unselectable_reason: Some("权限不足".into()),
        }],
        active_preview: Some(crate::dto::PlanPreviewSnapshot {
            preview_id: "p1".into(),
            metrics: PreflightMetrics {
                total: 1,
                executable: 1,
                blocked: 0,
                warnings: 0,
                create: 0,
                reuse: 1,
                skip: 0,
            },
            rows: vec![PreflightRow {
                repository_id: "r1".into(),
                source_url: "https://git.test/a.git".into(),
                target_url: "https://git.target/a".into(),
                target_name: "a".into(),
                action: PlanAction::SkipNonEmpty,
                permission: PermissionLevel::FullMigration,
                target_state: TargetState::Empty,
                module_fidelity: vec![module_row.clone()],
                disk_estimate_bytes: 1,
                blocking_reason: Some("阻断".into()),
                suggested_action: Some("建议".into()),
                field_mapping: vec![FieldMappingRow {
                    field: "visibility".into(),
                    source_value: Some("private".into()),
                    target_value: None,
                    result: "不支持".into(),
                }],
            }],
            blocking: vec!["阻断".into()],
            warnings: vec!["警告".into()],
            capability_snapshot_hash: "hash".into(),
            requires_confirmation: true,
            confirmation_phrase: Some("a".into()),
            ref_policy: RefPolicySummary {
                mode: "git_heads_tags_only".into(),
                allowed_refspecs: vec!["refs/heads/*:refs/heads/*".into()],
                excluded_refs: vec!["refs/pull/*".into()],
                explanation: "说明".into(),
            },
            selected_count: 1,
            excluded_count: 0,
        }),
        active_plan: Some(PlanSnapshot {
            plan_id: "plan".into(),
            plan_hash: "hash".into(),
            status: "frozen".into(),
            repository_count: 1,
            capability_snapshot_hash: "hash".into(),
            dangerous_confirmed: true,
            created_at_ms: 1,
        }),
        active_batch: Some(BatchSnapshot {
            batch_id: "batch".into(),
            plan_id: "plan".into(),
            plan_hash: "hash".into(),
            control: BatchControl::Paused,
            concurrency: 2,
            total: 1,
            completed: 0,
            failed: 0,
            started_at_ms: Some(1),
            ended_at_ms: None,
            tasks: vec![TaskSnapshot {
                task_id: "task".into(),
                repository_id: "r1".into(),
                source_url: "https://git.test/a.git".into(),
                target_url: "https://git.target/a".into(),
                stage: MigrationStage::PlatformData,
                state: RepoTaskState::RetryableFailed,
                attempt: 1,
                progress_completed: 1,
                progress_total: Some(2),
                retryable: true,
                error: Some(error.clone()),
                last_checkpoint: Some(CheckpointSummary {
                    stage: "git".into(),
                    transition: "heartbeat".into(),
                    attempt: 1,
                    resumable: true,
                    created_at_ms: 1,
                }),
                updated_at_ms: 1,
            }],
            logs: vec![LogEntry {
                task_id: "task".into(),
                level: "error".into(),
                stage: "git".into(),
                code: "ipc.network".into(),
                safe_message: "连接中断".into(),
                created_at_ms: 1,
            }],
        }),
        report: Some(ReportSnapshot {
            batch_id: "batch".into(),
            metrics: ReportMetrics {
                complete_success: 1,
                git_success_platform_partial: 1,
                retryable_failure: 1,
                permission_or_conflict_skip: 1,
            },
            rows: vec![ReportRowSnapshot {
                task_id: "task".into(),
                source_url: "https://git.test/a.git".into(),
                target_url: "https://git.target/a".into(),
                status: AggregateStatus::Partial,
                completed_at_ms: Some(1),
                git_verified: true,
                lfs_verified: false,
                metadata_verified: false,
                modules: vec![module_row],
                error_code: Some("ipc.network".into()),
                evidence: VerificationEvidence {
                    refs_checked: 1,
                    refs_missing: 0,
                    lfs_checked: 1,
                    lfs_missing: 1,
                    metadata_checked: true,
                    excluded_refs: vec!["refs/pull/1/head".into()],
                },
                unmapped_fields: vec!["assignee".into()],
                archive_path: Some("archive".into()),
                source_links: vec!["https://git.test/a/issues/1".into()],
                next_action: Some("下一步".into()),
            }],
            cleanup: CleanupState::CleanupFailed {
                path: "tmp".into(),
                reason: "占用".into(),
            },
        }),
        resumable: vec![ResumableBatch {
            batch_id: "batch".into(),
            plan_id: "plan".into(),
            pending: 1,
            plan_hash_matches: true,
            credential_recheck_required: true,
            capability_recheck_required: false,
        }],
    }
}

fn contract_of<T: Serialize>(value: &T) -> BTreeSet<String> {
    let json = serde_json::to_value(value).expect("DTO must serialise");
    let mut keys = BTreeSet::new();
    collect_keys(&json, &mut keys);
    keys
}

#[test]
fn renderer_types_cover_every_snapshot_field() {
    let keys = contract_of(&sample_snapshot());
    let missing: Vec<_> = keys.iter().filter(|key| !renderer_declares(key)).collect();
    assert!(
        missing.is_empty(),
        "the renderer is missing snapshot fields: {missing:?}"
    );
}

/// Every value the backend can put in an enum column must exist as a literal in
/// the renderer's union types, so the UI can never render a state it has no
/// wording for.
#[test]
fn renderer_types_cover_every_enum_value_the_backend_emits() {
    let mut values: Vec<String> = Vec::new();
    values.extend([ConnectionRole::Source, ConnectionRole::Target].map(|v| enum_text(&v)));
    values.extend(
        [
            PermissionLevel::FullMigration,
            PermissionLevel::GitOnly,
            PermissionLevel::Insufficient,
        ]
        .map(|v| enum_text(&v)),
    );
    values.extend(
        [
            MigrationStage::Preflight,
            MigrationStage::PrepareTarget,
            MigrationStage::Git,
            MigrationStage::Lfs,
            MigrationStage::Metadata,
            MigrationStage::PlatformData,
            MigrationStage::Verify,
            MigrationStage::Complete,
        ]
        .map(|v| enum_text(&v)),
    );
    values.extend(
        [
            PlanAction::Create,
            PlanAction::ReuseEmpty,
            PlanAction::SkipNonEmpty,
            PlanAction::Overwrite,
            PlanAction::Rename,
            PlanAction::Blocked,
        ]
        .map(|v| enum_text(&v)),
    );
    values.extend(
        [
            Fidelity::NativeRebuild,
            Fidelity::ReadOnlyArchive,
            Fidelity::Unsupported,
        ]
        .map(|v| enum_text(&v)),
    );
    values.extend(
        [
            TargetState::Unknown,
            TargetState::Missing,
            TargetState::Empty,
            TargetState::NonEmpty,
            TargetState::Inaccessible,
        ]
        .map(|v| enum_text(&v)),
    );
    values.extend(
        [
            RepoTaskState::Planned,
            RepoTaskState::Preflighted,
            RepoTaskState::Preparing,
            RepoTaskState::Git,
            RepoTaskState::Lfs,
            RepoTaskState::Metadata,
            RepoTaskState::PlatformModules,
            RepoTaskState::Verifying,
            RepoTaskState::Succeeded,
            RepoTaskState::Partial,
            RepoTaskState::RetryableFailed,
            RepoTaskState::Skipped,
        ]
        .map(|v| enum_text(&v)),
    );
    values.extend(
        [
            BatchControl::Running,
            BatchControl::Paused,
            BatchControl::Cancelled,
            BatchControl::Completed,
        ]
        .map(|v| enum_text(&v)),
    );
    values.extend(
        [
            AggregateStatus::Succeeded,
            AggregateStatus::Partial,
            AggregateStatus::Failed,
            AggregateStatus::RetryableFailed,
            AggregateStatus::Skipped,
        ]
        .map(|v| enum_text(&v)),
    );
    values.extend(
        [
            ErrorCategory::Auth,
            ErrorCategory::Permission,
            ErrorCategory::Conflict,
            ErrorCategory::RateLimited,
            ErrorCategory::Network,
            ErrorCategory::Validation,
            ErrorCategory::Disk,
            ErrorCategory::Unsupported,
            ErrorCategory::Git,
            ErrorCategory::Verification,
        ]
        .map(|v| enum_text(&v)),
    );
    values.extend(
        [
            PlatformKind::Github,
            PlatformKind::Gitlab,
            PlatformKind::Gitee,
            PlatformKind::Gitea,
            PlatformKind::Forgejo,
            PlatformKind::GenericGit,
            PlatformKind::Unknown,
        ]
        .map(|v| enum_text(&v)),
    );
    values.extend(
        [
            RepositoryVisibility::Public,
            RepositoryVisibility::Internal,
            RepositoryVisibility::Private,
            RepositoryVisibility::Unknown,
        ]
        .map(|v| enum_text(&v)),
    );
    // `CleanupState` is an internally tagged enum; its tags are contract too.
    for cleanup in [
        CleanupState::Cleaned,
        CleanupState::RetainedTempDirectory {
            path: String::new(),
        },
        CleanupState::CleanupFailed {
            path: String::new(),
            reason: String::new(),
        },
    ] {
        let tag = serde_json::to_value(&cleanup).expect("cleanup serialises");
        values.push(
            tag.get("type")
                .and_then(Value::as_str)
                .expect("cleanup carries a tag")
                .to_owned(),
        );
    }

    let missing: Vec<_> = values
        .iter()
        .filter(|value| !renderer_declares(&format!("\"{value}\"")))
        .collect();
    assert!(
        missing.is_empty(),
        "the renderer is missing enum values: {missing:?}"
    );
}

#[test]
fn renderer_types_cover_secondary_response_shapes() {
    let responses = [
        contract_of(&RepositoryPage {
            items: vec![],
            next_cursor: Some("c".into()),
            total_count: Some(1),
            loaded: 1,
            warnings: vec!["w".into()],
        }),
        contract_of(&RetryOutcome {
            retried: vec!["task".into()],
            rejected: vec![RetryRejection {
                task_id: "task".into(),
                reason: "权限".into(),
            }],
            batch: sample_snapshot().active_batch.expect("batch"),
        }),
        contract_of(&ExportOutcome {
            path: "p".into(),
            bytes_written: 1,
            row_count: 1,
        }),
        contract_of(&CleanupState::RetainedTempDirectory { path: "tmp".into() }),
    ];
    for keys in responses {
        let missing: Vec<_> = keys.iter().filter(|key| !renderer_declares(key)).collect();
        assert!(
            missing.is_empty(),
            "the renderer is missing response fields: {missing:?}"
        );
    }
}

/// The renderer must not be able to name a command the backend does not
/// register, and every registered command must be reachable from the client.
#[test]
fn renderer_command_list_matches_the_backend_whitelist() {
    for command in COMMAND_WHITELIST {
        assert!(
            RENDERER_CLIENT.contains(&format!("\"{command}\"")),
            "ipcClient.ts does not expose the command {command}"
        );
    }
    let declared = RENDERER_CLIENT
        .split("export const COMMANDS = [")
        .nth(1)
        .and_then(|rest| rest.split("] as const").next())
        .expect("ipcClient.ts must declare COMMANDS");
    let count = declared.matches('"').count() / 2;
    assert_eq!(
        count,
        COMMAND_WHITELIST.len(),
        "renderer declares {count} commands but the backend registers {}",
        COMMAND_WHITELIST.len()
    );
}

/// A stale renderer must refuse to interpret a newer snapshot.
#[test]
fn renderer_pins_the_snapshot_schema_version() {
    assert!(
        RENDERER_TYPES.contains(&format!(
            "SUPPORTED_SNAPSHOT_SCHEMA_VERSION = {SNAPSHOT_SCHEMA_VERSION}"
        )),
        "ipcTypes.ts must pin schema version {SNAPSHOT_SCHEMA_VERSION}"
    );
}

#[test]
fn renderer_listens_on_the_same_event_channel() {
    assert!(RENDERER_TYPES.contains(crate::events::MIGRATION_EVENT));
}
