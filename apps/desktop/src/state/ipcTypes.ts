/**
 * Snapshot types mirroring `apps/desktop/src-tauri/src/dto.rs`.
 *
 * The enums and command payloads owned by the shared contract are re-exported
 * from `../generated/ipc`, which is generated from Rust and must not be edited
 * by hand. Only the desktop-shell snapshot shapes live here, and the desktop
 * crate has a drift test that fails if a field name in `dto.rs` is missing from
 * this file.
 */

export type {
  ConnectionAuthorizeInput,
  ConnectionAuthorizeOutcome,
  ConnectionTestInput,
  DiscoveryQuery,
  ErrorCategory,
  Fidelity,
  IpcError,
  MigrationEvent,
  PlatformKind,
  PlatformModule,
  RepositoryScope,
  RepositoryVisibility,
} from "../generated/ipc";

import type {
  Fidelity,
  IpcError,
  MigrationEvent,
  PlatformKind,
  RepositoryVisibility,
} from "../generated/ipc";

export type ConnectionRole = "source" | "target";
export type PermissionLevel = "full_migration" | "git_only" | "insufficient";
export type TargetState = "unknown" | "missing" | "empty" | "non_empty" | "inaccessible";
export type PlanAction =
  | "create"
  | "reuse_empty"
  | "skip_non_empty"
  | "overwrite"
  | "rename"
  | "blocked";
export type MigrationStage =
  | "preflight"
  | "prepare_target"
  | "git"
  | "lfs"
  | "metadata"
  | "platform_data"
  | "verify"
  | "complete";
export type RepoTaskState =
  | "planned"
  | "preflighted"
  | "preparing"
  | "git"
  | "lfs"
  | "metadata"
  | "platform_modules"
  | "verifying"
  | "succeeded"
  | "partial"
  | "retryable_failed"
  | "skipped";
export type BatchControl = "running" | "paused" | "cancelled" | "completed";
export type AggregateStatus =
  | "succeeded"
  | "partial"
  | "failed"
  | "retryable_failed"
  | "skipped";

export interface CapabilitySummary {
  module: string;
  supported: boolean;
  permitted: boolean;
  required_scopes: string[];
  fidelity: Fidelity;
  reason: string | null;
  degradation: string | null;
}

export interface ConnectionSnapshot {
  id: string;
  role: ConnectionRole;
  platform: PlatformKind;
  endpoint: string;
  credential_ref: string | null;
  authenticated: boolean;
  account_name: string | null;
  instance_version: string | null;
  tls_trusted: boolean;
  capabilities: CapabilitySummary[];
}

export interface RepositorySnapshot {
  id: string;
  connection_id: string;
  source_url: string;
  name: string;
  namespace: string;
  visibility: RepositoryVisibility;
  permission: PermissionLevel;
  updated_at_epoch_seconds: number | null;
  git_capable: boolean;
  platform_capable: boolean;
  target_state: TargetState;
  target_url: string | null;
  target_name: string | null;
  selectable: boolean;
  unselectable_reason: string | null;
}

export interface RepositoryPage {
  items: RepositorySnapshot[];
  next_cursor: string | null;
  total_count: number | null;
  loaded: number;
  warnings: string[];
}

export interface ModuleFidelityRow {
  module: string;
  fidelity: Fidelity;
  reason: string | null;
  confirmation_required: boolean;
}

export interface FieldMappingRow {
  field: string;
  source_value: string | null;
  target_value: string | null;
  result: string;
}

export interface RefPolicySummary {
  mode: string;
  allowed_refspecs: string[];
  excluded_refs: string[];
  explanation: string;
}

export interface PreflightRow {
  repository_id: string;
  source_url: string;
  target_url: string;
  target_name: string;
  action: PlanAction;
  permission: PermissionLevel;
  target_state: TargetState;
  module_fidelity: ModuleFidelityRow[];
  disk_estimate_bytes: number;
  blocking_reason: string | null;
  suggested_action: string | null;
  field_mapping: FieldMappingRow[];
}

export interface PreflightMetrics {
  total: number;
  executable: number;
  blocked: number;
  warnings: number;
  create: number;
  reuse: number;
  skip: number;
}

export interface PlanPreviewSnapshot {
  preview_id: string;
  metrics: PreflightMetrics;
  rows: PreflightRow[];
  blocking: string[];
  warnings: string[];
  capability_snapshot_hash: string;
  requires_confirmation: boolean;
  confirmation_phrase: string | null;
  ref_policy: RefPolicySummary;
  selected_count: number;
  excluded_count: number;
}

export interface PlanSnapshot {
  plan_id: string;
  plan_hash: string;
  status: string;
  repository_count: number;
  capability_snapshot_hash: string;
  dangerous_confirmed: boolean;
  created_at_ms: number;
}

export interface CheckpointSummary {
  stage: string;
  transition: string;
  attempt: number;
  resumable: boolean;
  created_at_ms: number;
}

export interface TaskSnapshot {
  task_id: string;
  repository_id: string;
  source_url: string;
  target_url: string;
  stage: MigrationStage;
  state: RepoTaskState;
  attempt: number;
  progress_completed: number;
  progress_total: number | null;
  retryable: boolean;
  error: IpcError | null;
  last_checkpoint: CheckpointSummary | null;
  updated_at_ms: number;
}

export interface LogEntry {
  task_id: string;
  level: string;
  stage: string;
  code: string;
  safe_message: string;
  created_at_ms: number;
}

export interface BatchSnapshot {
  batch_id: string;
  plan_id: string;
  plan_hash: string;
  control: BatchControl;
  concurrency: number;
  total: number;
  completed: number;
  failed: number;
  started_at_ms: number | null;
  ended_at_ms: number | null;
  tasks: TaskSnapshot[];
  logs: LogEntry[];
}

export interface ResumableBatch {
  batch_id: string;
  plan_id: string;
  pending: number;
  plan_hash_matches: boolean;
  credential_recheck_required: boolean;
  capability_recheck_required: boolean;
}

export type CleanupState =
  | { type: "cleaned" }
  | { type: "retained_temp_directory"; path: string }
  | { type: "cleanup_failed"; path: string; reason: string };

export interface VerificationEvidence {
  refs_checked: number;
  refs_missing: number;
  lfs_checked: number;
  lfs_missing: number;
  metadata_checked: boolean;
  excluded_refs: string[];
}

export interface ReportRowSnapshot {
  task_id: string;
  source_url: string;
  target_url: string;
  status: AggregateStatus;
  completed_at_ms: number | null;
  git_verified: boolean;
  lfs_verified: boolean;
  metadata_verified: boolean;
  modules: ModuleFidelityRow[];
  error_code: string | null;
  evidence: VerificationEvidence;
  unmapped_fields: string[];
  archive_path: string | null;
  source_links: string[];
  next_action: string | null;
}

export interface ReportMetrics {
  complete_success: number;
  git_success_platform_partial: number;
  retryable_failure: number;
  permission_or_conflict_skip: number;
}

export interface ReportSnapshot {
  batch_id: string;
  metrics: ReportMetrics;
  rows: ReportRowSnapshot[];
  cleanup: CleanupState;
}

export interface MigrationSnapshot {
  schema_version: number;
  revision: number;
  connections: ConnectionSnapshot[];
  repositories: RepositorySnapshot[];
  active_preview: PlanPreviewSnapshot | null;
  active_plan: PlanSnapshot | null;
  active_batch: BatchSnapshot | null;
  report: ReportSnapshot | null;
  resumable: ResumableBatch[];
}

export interface ConnectionSaveInput {
  role: ConnectionRole;
  endpoint: string;
  platform_hint: PlatformKind | null;
  credential_ref: string | null;
  trust_fingerprint_sha256: string | null;
}

export interface RepositoryImportInput {
  connection_id: string;
  urls: string;
}

export interface RepositoryImportIssue {
  line: number;
  value: string;
  message: string;
}

export interface RepositoryImportReport {
  imported: number;
  duplicate_count: number;
  issues: RepositoryImportIssue[];
}

export interface RepositoryMappingInput {
  repository_id: string;
  target_url: string;
  target_name: string | null;
}

export interface PlanPreviewRequest {
  selected_repository_ids: string[];
  excluded_repository_ids: string[];
  mappings: RepositoryMappingInput[];
  reuse_empty: boolean;
  skip_non_empty: boolean;
  auto_rename: boolean;
  allow_overwrite: boolean;
  include_archived_refs: boolean;
  module_lfs: boolean;
  module_metadata: boolean;
  module_issues: boolean;
  module_pull_requests: boolean;
  module_wiki: boolean;
  module_releases: boolean;
}

export interface PlanFreezeInput {
  preview_id: string;
  confirmation_text: string | null;
  acknowledged_fidelity: string[];
}

export interface BatchIdInput {
  batch_id: string;
}

export interface TargetProbeInput {
  repository_id: string;
  target_url: string;
}

export interface RetryRejection {
  task_id: string;
  reason: string;
}

export interface RetryOutcome {
  retried: string[];
  rejected: RetryRejection[];
  batch: BatchSnapshot;
}

export interface ExportOutcome {
  path: string;
  bytes_written: number;
  row_count: number;
}

export interface EventEnvelope {
  revision: number;
  event: MigrationEvent;
}

/** Single event channel; matches `events::MIGRATION_EVENT`. */
export const MIGRATION_EVENT = "migration://event";

/** Schema version this renderer understands. */
export const SUPPORTED_SNAPSHOT_SCHEMA_VERSION = 1;
