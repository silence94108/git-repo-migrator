// Generated from crates/application/src/ipc_contract.rs. Do not edit manually.
export type PlatformKind = "github" | "gitlab" | "gitee" | "gitea" | "forgejo" | "generic_git" | "unknown";
export type Fidelity = "native_rebuild" | "read_only_archive" | "unsupported";
export type PlatformModule = "metadata" | "issues" | "pull_requests" | "merge_requests" | "wiki" | "releases" | "release_assets";
export type ErrorCategory = "auth" | "permission" | "conflict" | "rate_limited" | "network" | "validation" | "disk" | "unsupported" | "git" | "verification";
export type RepositoryScope = "owned" | "administered" | "participated" | "all_accessible";
export type RepositoryVisibility = "public" | "internal" | "private" | "unknown";

export interface DiscoveryQuery { scope: RepositoryScope; search?: string | null; visibility?: RepositoryVisibility | null; include_archived: boolean; cursor?: string | null; page_size: number; }
export interface ConnectionTestInput { endpoint: string; platform_hint?: PlatformKind | null; credential_ref?: string | null; }
export interface RepositoryDiscoverInput { connection_id: string; query: DiscoveryQuery; }
export interface PlanPreviewInput { selected_repository_ids: string[]; conflict_policy: string; modules: string[]; }
export interface BatchStartInput { plan_id: string; concurrency: number; workspace_policy: string; }
export interface TaskRetryInput { batch_id: string; task_ids: string[]; }
export interface ReportExportInput { batch_id: string; format: string; path: string; }
export interface IpcError { code: string; category: ErrorCategory; retryable: boolean; stage: string; safe_message: string; action: string; }

export type MigrationEvent =
  | { type: "batch_started"; batch_id: string }
  | { type: "task_stage_changed"; batch_id: string; task_id: string; stage: string }
  | { type: "task_progress"; batch_id: string; task_id: string; completed: number; total?: number | null }
  | { type: "task_warning"; batch_id: string; task_id: string; code: string; safe_message: string }
  | { type: "task_completed"; batch_id: string; task_id: string; status: string; fidelity: Fidelity[] }
  | { type: "batch_completed"; batch_id: string; status: string };
