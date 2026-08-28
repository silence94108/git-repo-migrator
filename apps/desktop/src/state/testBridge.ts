/**
 * In-memory backend double plus snapshot fixtures.
 *
 * The double speaks the same command whitelist as the Tauri bridge and records
 * every call, so the view tests assert on what the renderer *sent* rather than on
 * an internal mock. Fixtures mirror the shapes produced by
 * `apps/desktop/src-tauri/src/snapshot.rs`.
 */

import type { CommandName, MigrationBridge } from "./ipcClient";
import type {
  BatchSnapshot,
  ConnectionSnapshot,
  EventEnvelope,
  IpcError,
  LogEntry,
  MigrationSnapshot,
  PlanPreviewSnapshot,
  PlanSnapshot,
  PreflightRow,
  ReportRowSnapshot,
  ReportSnapshot,
  RepositorySnapshot,
  TaskSnapshot,
} from "./ipcTypes";

export interface RecordedCall {
  command: CommandName;
  input: unknown;
}

type Handler = (input: unknown) => unknown;

export class FakeBridge implements MigrationBridge {
  readonly calls: RecordedCall[] = [];
  private readonly handlers = new Map<CommandName, Handler>();
  private readonly listeners = new Set<(envelope: EventEnvelope) => void>();
  private snapshot: MigrationSnapshot;

  constructor(snapshot: MigrationSnapshot) {
    this.snapshot = snapshot;
  }

  setSnapshot(snapshot: MigrationSnapshot): void {
    this.snapshot = snapshot;
  }

  currentSnapshot(): MigrationSnapshot {
    return this.snapshot;
  }

  on(command: CommandName, handler: Handler): this {
    this.handlers.set(command, handler);
    return this;
  }

  /** Makes a command fail with a real `IpcError`, as the backend would. */
  failWith(command: CommandName, error: IpcError): this {
    this.handlers.set(command, () => {
      throw error;
    });
    return this;
  }

  emit(envelope: EventEnvelope): void {
    for (const listener of this.listeners) listener(envelope);
  }

  invoke<T>(command: CommandName, input?: unknown): Promise<T> {
    this.calls.push({ command, input });
    if (command === "migration_snapshot" && !this.handlers.has(command)) {
      return Promise.resolve(this.snapshot as T);
    }
    const handler = this.handlers.get(command);
    if (!handler) {
      return Promise.reject(notImplemented(command));
    }
    try {
      return Promise.resolve(handler(input) as T);
    } catch (cause) {
      return Promise.reject(cause);
    }
  }

  subscribe(listener: (envelope: EventEnvelope) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  inputFor(command: CommandName): unknown {
    return this.calls.filter((call) => call.command === command).at(-1)?.input;
  }

  countOf(command: CommandName): number {
    return this.calls.filter((call) => call.command === command).length;
  }
}

function notImplemented(command: CommandName): IpcError {
  return {
    code: "test.not_implemented",
    category: "validation",
    retryable: false,
    stage: "test",
    safe_message: `测试桩未实现命令 ${command}`,
    action: "请在测试中通过 on() 注册该命令",
  };
}

export function ipcError(patch: Partial<IpcError> = {}): IpcError {
  return {
    code: "platform.permission",
    category: "permission",
    retryable: false,
    stage: "prepare_target",
    safe_message: "凭据对目标命名空间没有写入权限",
    action: "请授予写入权限或排除该仓库",
    ...patch,
  };
}

export function connection(patch: Partial<ConnectionSnapshot> = {}): ConnectionSnapshot {
  return {
    id: patch.role === "target" ? "target" : "source",
    role: "source",
    platform: "generic_git",
    endpoint: "https://git.source.test",
    credential_ref: "git-repo-migrator/source",
    authenticated: true,
    account_name: "ops",
    instance_version: null,
    tls_trusted: true,
    capabilities: [
      {
        module: "git_write",
        supported: true,
        permitted: true,
        required_scopes: ["repo:write"],
        fidelity: "native_rebuild",
        reason: null,
        degradation: null,
      },
      {
        module: "issues",
        supported: false,
        permitted: false,
        required_scopes: [],
        fidelity: "unsupported",
        reason: "通用 Git 服务没有平台数据 API",
        degradation: "该模块不会写入目标；将只迁移 Git 数据",
      },
    ],
    ...patch,
  };
}

export function repository(patch: Partial<RepositorySnapshot> = {}): RepositorySnapshot {
  const name = patch.name ?? "alpha";
  return {
    id: `https://git.source.test/ops/${name}.git`,
    connection_id: "source",
    source_url: `https://git.source.test/ops/${name}.git`,
    name,
    namespace: "ops",
    visibility: "private",
    permission: "full_migration",
    updated_at_epoch_seconds: 1_760_000_000,
    git_capable: true,
    platform_capable: false,
    target_state: "empty",
    target_url: `https://git.target.test/ops/${name}`,
    target_name: name,
    selectable: true,
    unselectable_reason: null,
    ...patch,
  };
}

export function repositories(count: number): RepositorySnapshot[] {
  return Array.from({ length: count }, (_, index) => repository({ name: `repo${index}` }));
}

export function preflightRow(patch: Partial<PreflightRow> = {}): PreflightRow {
  const name = patch.target_name ?? "alpha";
  return {
    repository_id: `https://git.source.test/ops/${name}.git`,
    source_url: `https://git.source.test/ops/${name}.git`,
    target_url: `https://git.target.test/ops/${name}`,
    target_name: name,
    action: "reuse_empty",
    permission: "full_migration",
    target_state: "empty",
    module_fidelity: [],
    disk_estimate_bytes: 0,
    blocking_reason: null,
    suggested_action: null,
    field_mapping: [
      {
        field: "visibility",
        source_value: "private",
        target_value: null,
        result: "目标平台不支持写入可见性；保持目标现状",
      },
    ],
    ...patch,
  };
}

export function preview(patch: Partial<PlanPreviewSnapshot> = {}): PlanPreviewSnapshot {
  const rows = patch.rows ?? [preflightRow()];
  return {
    preview_id: "preview-1",
    metrics: {
      total: rows.length,
      executable: rows.filter((row) => row.action !== "blocked").length,
      blocked: rows.filter((row) => row.action === "blocked").length,
      warnings: 0,
      create: rows.filter((row) => row.action === "create").length,
      reuse: rows.filter((row) => row.action === "reuse_empty").length,
      skip: rows.filter((row) => row.action === "skip_non_empty").length,
    },
    blocking: [],
    warnings: [],
    capability_snapshot_hash: "cap-hash",
    requires_confirmation: false,
    confirmation_phrase: null,
    ref_policy: {
      mode: "git_heads_tags_only",
      allowed_refspecs: ["refs/heads/*:refs/heads/*", "refs/tags/*:refs/tags/*"],
      excluded_refs: ["refs/pull/*", "refs/merge-requests/*"],
      explanation: "默认只迁移 refs/heads 与 refs/tags；平台私有 refs 不会写入目标。",
    },
    selected_count: rows.length,
    excluded_count: 0,
    ...patch,
    rows,
  };
}

export function plan(patch: Partial<PlanSnapshot> = {}): PlanSnapshot {
  return {
    plan_id: "plan-1",
    plan_hash: "a".repeat(64),
    status: "frozen",
    repository_count: 1,
    capability_snapshot_hash: "cap-hash",
    dangerous_confirmed: false,
    created_at_ms: 1_760_000_000_000,
    ...patch,
  };
}

export function task(patch: Partial<TaskSnapshot> = {}): TaskSnapshot {
  const name = patch.task_id ?? "task-1";
  return {
    task_id: name,
    repository_id: "https://git.source.test/ops/alpha.git",
    source_url: "https://git.source.test/ops/alpha.git",
    target_url: "https://git.target.test/ops/alpha",
    stage: "git",
    state: "git",
    attempt: 0,
    progress_completed: 3,
    progress_total: 10,
    retryable: false,
    error: null,
    last_checkpoint: {
      stage: "git",
      transition: "heartbeat",
      attempt: 0,
      resumable: true,
      created_at_ms: 1_760_000_001_000,
    },
    updated_at_ms: 1_760_000_001_000,
    ...patch,
  };
}

export function logEntry(patch: Partial<LogEntry> = {}): LogEntry {
  return {
    task_id: "task-1",
    level: "error",
    stage: "git",
    code: "ipc.network",
    safe_message: "推送过程中连接中断",
    created_at_ms: 1_760_000_002_000,
    ...patch,
  };
}

export function batch(patch: Partial<BatchSnapshot> = {}): BatchSnapshot {
  const tasks = patch.tasks ?? [task()];
  return {
    batch_id: "batch-1",
    plan_id: "plan-1",
    plan_hash: "a".repeat(64),
    control: "running",
    concurrency: 2,
    total: tasks.length,
    completed: tasks.filter((item) =>
      ["succeeded", "partial", "skipped"].includes(item.state),
    ).length,
    failed: tasks.filter((item) => item.state === "retryable_failed").length,
    started_at_ms: 1_760_000_000_500,
    ended_at_ms: null,
    logs: [],
    ...patch,
    tasks,
  };
}

export function reportRow(patch: Partial<ReportRowSnapshot> = {}): ReportRowSnapshot {
  return {
    task_id: "task-1",
    source_url: "https://git.source.test/ops/alpha.git",
    target_url: "https://git.target.test/ops/alpha",
    status: "succeeded",
    completed_at_ms: 1_760_000_003_000,
    git_verified: true,
    lfs_verified: true,
    metadata_verified: true,
    modules: [],
    error_code: null,
    evidence: {
      refs_checked: 7,
      refs_missing: 0,
      lfs_checked: 2,
      lfs_missing: 0,
      metadata_checked: true,
      excluded_refs: ["refs/pull/1/head"],
    },
    unmapped_fields: [],
    archive_path: null,
    source_links: [],
    next_action: null,
    ...patch,
  };
}

export function report(patch: Partial<ReportSnapshot> = {}): ReportSnapshot {
  const rows = patch.rows ?? [reportRow()];
  return {
    batch_id: "batch-1",
    metrics: {
      complete_success: rows.filter((row) => row.status === "succeeded").length,
      git_success_platform_partial: rows.filter((row) => row.status === "partial").length,
      retryable_failure: rows.filter((row) => row.status === "retryable_failed").length,
      permission_or_conflict_skip: rows.filter((row) => row.status === "skipped").length,
    },
    cleanup: { type: "cleaned" },
    ...patch,
    rows,
  };
}

export function snapshot(patch: Partial<MigrationSnapshot> = {}): MigrationSnapshot {
  return {
    schema_version: 1,
    revision: 1,
    connections: [],
    repositories: [],
    active_preview: null,
    active_plan: null,
    active_batch: null,
    report: null,
    resumable: [],
    ...patch,
  };
}

/** A snapshot with both connections saved, i.e. past the connection step. */
export function connectedSnapshot(patch: Partial<MigrationSnapshot> = {}): MigrationSnapshot {
  return snapshot({
    connections: [
      connection({ role: "source", id: "source" }),
      connection({
        role: "target",
        id: "target",
        endpoint: "https://git.target.test",
        credential_ref: "git-repo-migrator/target",
      }),
    ],
    ...patch,
  });
}
