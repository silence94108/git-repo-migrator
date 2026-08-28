//! Snapshot and command DTOs for the desktop IPC boundary.
//!
//! Every structure here is derived from SQLite state or from the shared
//! contract types in `git_repo_migrator_application::ipc_contract`. Secrets are
//! representable only as an opaque `credential_ref`; no field carries a token,
//! cookie, private key or raw platform response.

use git_repo_migrator_application::planning::TargetState;
use git_repo_migrator_application::verification::{AggregateStatus, VerificationEvidence};
use git_repo_migrator_application::{BatchControl, IpcError};
use git_repo_migrator_domain::{Fidelity, RepoTaskState};
use git_repo_migrator_platform_core::{PlatformKind, RepositoryVisibility};
use serde::{Deserialize, Serialize};

/// Schema version of the whole snapshot payload. The renderer refuses to render
/// a snapshot it does not understand instead of guessing field semantics.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionRole {
    Source,
    Target,
}

/// Permission level shown as a badge. Mirrors the ui-spec `PermissionBadge`
/// values so the UI never invents its own vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLevel {
    FullMigration,
    GitOnly,
    Insufficient,
}

/// The eight execution stages from the ui-design queue specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStage {
    Preflight,
    PrepareTarget,
    Git,
    Lfs,
    Metadata,
    PlatformData,
    Verify,
    Complete,
}

/// Planned action for one repository. `Blocked` is explicit so the UI can never
/// present an unresolved row as executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    Create,
    ReuseEmpty,
    SkipNonEmpty,
    Overwrite,
    Rename,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySummary {
    pub module: String,
    pub supported: bool,
    pub permitted: bool,
    pub required_scopes: Vec<String>,
    pub fidelity: Fidelity,
    pub reason: Option<String>,
    pub degradation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionSnapshot {
    pub id: String,
    pub role: ConnectionRole,
    pub platform: PlatformKind,
    pub endpoint: String,
    /// Opaque Windows Credential Manager reference. Never a secret value.
    pub credential_ref: Option<String>,
    pub authenticated: bool,
    pub account_name: Option<String>,
    pub instance_version: Option<String>,
    pub tls_trusted: bool,
    pub capabilities: Vec<CapabilitySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositorySnapshot {
    pub id: String,
    pub connection_id: String,
    pub source_url: String,
    pub name: String,
    pub namespace: String,
    pub visibility: RepositoryVisibility,
    pub permission: PermissionLevel,
    pub updated_at_epoch_seconds: Option<u64>,
    pub git_capable: bool,
    pub platform_capable: bool,
    pub target_state: TargetState,
    pub target_url: Option<String>,
    pub target_name: Option<String>,
    /// False when permissions are insufficient; the UI disables the checkbox.
    pub selectable: bool,
    pub unselectable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryPage {
    pub items: Vec<RepositorySnapshot>,
    pub next_cursor: Option<String>,
    /// Total matching the filter on the source platform, when the platform
    /// reports it. `None` means "unknown", never "zero".
    pub total_count: Option<u64>,
    pub loaded: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleFidelityRow {
    pub module: String,
    pub fidelity: Fidelity,
    pub reason: Option<String>,
    /// `read_only_archive` and `unsupported` need an explicit acknowledgement
    /// before a plan may be frozen.
    pub confirmation_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldMappingRow {
    pub field: String,
    pub source_value: Option<String>,
    pub target_value: Option<String>,
    pub result: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefPolicySummary {
    pub mode: String,
    pub allowed_refspecs: Vec<String>,
    pub excluded_refs: Vec<String>,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightRow {
    pub repository_id: String,
    pub source_url: String,
    pub target_url: String,
    pub target_name: String,
    pub action: PlanAction,
    pub permission: PermissionLevel,
    pub target_state: TargetState,
    pub module_fidelity: Vec<ModuleFidelityRow>,
    pub disk_estimate_bytes: u64,
    pub blocking_reason: Option<String>,
    pub suggested_action: Option<String>,
    pub field_mapping: Vec<FieldMappingRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightMetrics {
    pub total: u32,
    pub executable: u32,
    pub blocked: u32,
    pub warnings: u32,
    pub create: u32,
    pub reuse: u32,
    pub skip: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPreviewSnapshot {
    pub preview_id: String,
    pub metrics: PreflightMetrics,
    pub rows: Vec<PreflightRow>,
    pub blocking: Vec<String>,
    pub warnings: Vec<String>,
    pub capability_snapshot_hash: String,
    pub requires_confirmation: bool,
    /// Text the operator must retype for a destructive plan. `None` when the
    /// plan needs no destructive confirmation.
    pub confirmation_phrase: Option<String>,
    pub ref_policy: RefPolicySummary,
    pub selected_count: u32,
    pub excluded_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSnapshot {
    pub plan_id: String,
    pub plan_hash: String,
    pub status: String,
    pub repository_count: u32,
    pub capability_snapshot_hash: String,
    pub dangerous_confirmed: bool,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointSummary {
    pub stage: String,
    pub transition: String,
    pub attempt: i64,
    pub resumable: bool,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub task_id: String,
    pub repository_id: String,
    pub source_url: String,
    pub target_url: String,
    pub stage: MigrationStage,
    pub state: RepoTaskState,
    pub attempt: u32,
    pub progress_completed: u64,
    pub progress_total: Option<u64>,
    pub retryable: bool,
    pub error: Option<IpcError>,
    pub last_checkpoint: Option<CheckpointSummary>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub task_id: String,
    pub level: String,
    pub stage: String,
    pub code: String,
    pub safe_message: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchSnapshot {
    pub batch_id: String,
    pub plan_id: String,
    pub plan_hash: String,
    pub control: BatchControl,
    pub concurrency: u16,
    pub total: u32,
    pub completed: u32,
    pub failed: u32,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub tasks: Vec<TaskSnapshot>,
    pub logs: Vec<LogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumableBatch {
    pub batch_id: String,
    pub plan_id: String,
    pub pending: u32,
    pub plan_hash_matches: bool,
    pub credential_recheck_required: bool,
    pub capability_recheck_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CleanupState {
    Cleaned,
    RetainedTempDirectory { path: String },
    CleanupFailed { path: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportRowSnapshot {
    pub task_id: String,
    pub source_url: String,
    pub target_url: String,
    pub status: AggregateStatus,
    pub completed_at_ms: Option<i64>,
    pub git_verified: bool,
    pub lfs_verified: bool,
    pub metadata_verified: bool,
    pub modules: Vec<ModuleFidelityRow>,
    pub error_code: Option<String>,
    pub evidence: VerificationEvidence,
    pub unmapped_fields: Vec<String>,
    pub archive_path: Option<String>,
    pub source_links: Vec<String>,
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportMetrics {
    pub complete_success: u32,
    pub git_success_platform_partial: u32,
    pub retryable_failure: u32,
    pub permission_or_conflict_skip: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSnapshot {
    pub batch_id: String,
    pub metrics: ReportMetrics,
    pub rows: Vec<ReportRowSnapshot>,
    pub cleanup: CleanupState,
}

/// The authoritative renderer state. Events only hint that a fresh snapshot
/// should be fetched; losing an event can never desynchronise the UI because
/// this payload is rebuilt from SQLite on every call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationSnapshot {
    pub schema_version: u32,
    /// Monotonic counter bumped on every state mutation. The renderer compares
    /// it with the revision carried by the last event it observed.
    pub revision: u64,
    pub connections: Vec<ConnectionSnapshot>,
    pub repositories: Vec<RepositorySnapshot>,
    pub active_preview: Option<PlanPreviewSnapshot>,
    pub active_plan: Option<PlanSnapshot>,
    pub active_batch: Option<BatchSnapshot>,
    pub report: Option<ReportSnapshot>,
    pub resumable: Vec<ResumableBatch>,
}

// ---------------------------------------------------------------------------
// Command inputs that extend the shared contract. All of them reject unknown
// fields so a renderer can never smuggle a secret through an extra key.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSaveInput {
    pub role: ConnectionRole,
    pub endpoint: String,
    pub platform_hint: Option<PlatformKind>,
    pub credential_ref: Option<String>,
    /// Explicit acknowledgement of a self-signed certificate fingerprint.
    /// Acknowledging still performs full validation against the pinned value.
    pub trust_fingerprint_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryImportInput {
    pub connection_id: String,
    pub urls: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryImportReport {
    pub imported: u32,
    pub duplicate_count: u32,
    pub issues: Vec<RepositoryImportIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryImportIssue {
    pub line: u32,
    pub value: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMappingInput {
    pub repository_id: String,
    pub target_url: String,
    pub target_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanPreviewRequest {
    pub selected_repository_ids: Vec<String>,
    pub excluded_repository_ids: Vec<String>,
    pub mappings: Vec<RepositoryMappingInput>,
    pub reuse_empty: bool,
    pub skip_non_empty: bool,
    pub auto_rename: bool,
    pub allow_overwrite: bool,
    pub include_archived_refs: bool,
    pub module_lfs: bool,
    pub module_metadata: bool,
    pub module_issues: bool,
    pub module_pull_requests: bool,
    pub module_wiki: bool,
    pub module_releases: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanFreezeInput {
    pub preview_id: String,
    /// Must equal `PlanPreviewSnapshot::confirmation_phrase` when the preview
    /// requires a destructive confirmation.
    pub confirmation_text: Option<String>,
    /// Modules degraded to `read_only_archive` or `unsupported` that the
    /// operator explicitly acknowledged.
    pub acknowledged_fidelity: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchIdInput {
    pub batch_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetProbeInput {
    pub repository_id: String,
    pub target_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_inputs_reject_unknown_secret_fields() {
        let json = r#"{"role":"source","endpoint":"https://git.example.com","platform_hint":null,"credential_ref":null,"trust_fingerprint_sha256":null,"token":"secret"}"#;
        assert!(serde_json::from_str::<ConnectionSaveInput>(json).is_err());
    }

    #[test]
    fn connection_snapshot_never_carries_a_secret_field() {
        let snapshot = ConnectionSnapshot {
            id: "c1".into(),
            role: ConnectionRole::Source,
            platform: PlatformKind::GenericGit,
            endpoint: "https://git.example.com".into(),
            credential_ref: Some("credential/windows/abc".into()),
            authenticated: true,
            account_name: Some("ops".into()),
            instance_version: None,
            tls_trusted: true,
            capabilities: vec![],
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        for forbidden in [
            "token",
            "password",
            "private_key",
            "cookie",
            "authorization",
        ] {
            assert!(
                !json.to_ascii_lowercase().contains(forbidden),
                "snapshot leaked {forbidden}"
            );
        }
    }

    #[test]
    fn plan_action_and_stage_serialize_as_contract_values() {
        assert_eq!(
            serde_json::to_string(&PlanAction::SkipNonEmpty).unwrap(),
            "\"skip_non_empty\""
        );
        assert_eq!(
            serde_json::to_string(&MigrationStage::PlatformData).unwrap(),
            "\"platform_data\""
        );
        assert_eq!(
            serde_json::to_string(&PermissionLevel::GitOnly).unwrap(),
            "\"git_only\""
        );
    }
}
