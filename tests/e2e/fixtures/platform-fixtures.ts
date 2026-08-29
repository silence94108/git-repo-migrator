/**
 * Shared E2E fixtures (T-030 owner file).
 *
 * Two things live here and nothing else:
 *
 * 1. **Fixture data** describing a Generic Git migration — repositories,
 *    preflight rows, queue tasks and a report. None of it contains a token, a
 *    password or an authorisation header: the security spec scans every request
 *    the page makes and a fixture secret would make that assertion vacuous.
 * 2. **`installBridge`**, which puts a scripted backend on `window` before the
 *    app boots. The bridge speaks exactly the command whitelist from
 *    `apps/desktop/src-tauri/src/commands/mod.rs`, records every call, and lets
 *    a spec script failures, rate limits and crashes.
 *
 * `tests/e2e/recovery-and-rate-limit.spec.ts` and the other T-031 specs reuse
 * this file without modifying it.
 */

import type { Page } from "@playwright/test";

export const PREVIEW_URL = process.env.E2E_BASE_URL ?? "http://127.0.0.1:4173";

/** Every command the renderer is allowed to call. Mirrors the Rust whitelist. */
export const COMMAND_WHITELIST = [
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
] as const;

export type CommandName = (typeof COMMAND_WHITELIST)[number];

export interface RecordedCall {
  command: string;
  input: unknown;
}

/** A scripted failure for one command, applied on the nth call. */
export interface ScriptedFailure {
  command: CommandName;
  onCall?: number;
  error: Record<string, unknown>;
}

export interface BridgeOptions {
  /** Initial snapshot. Defaults to a connected, 3-repository source. */
  snapshot?: Record<string, unknown>;
  failures?: ScriptedFailure[];
  /** Repository count for the capacity spec. */
  repositoryCount?: number;
  /** Marks the batch as interrupted so the resume banner appears on boot. */
  resumable?: boolean;
}

// -- fixture data ------------------------------------------------------------

export function capability(
  module: string,
  patch: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    module,
    supported: true,
    permitted: true,
    required_scopes: ["repo:write"],
    fidelity: "native_rebuild",
    reason: null,
    degradation: null,
    ...patch,
  };
}

export function connection(role: "source" | "target"): Record<string, unknown> {
  return {
    id: role,
    role,
    platform: "generic_git",
    endpoint: role === "source" ? "https://git.source.test" : "https://git.target.test",
    // A reference, never a token — this is the whole point of CM-004.
    credential_ref: `credential/windows/${role === "source" ? "1a2b3c4d" : "5e6f7a8b"}`,
    authenticated: true,
    account_name: "ops",
    instance_version: "2.45.0",
    tls_trusted: true,
    capabilities: [
      capability("git_write"),
      capability("lfs"),
      capability("issues", {
        supported: false,
        permitted: false,
        required_scopes: [],
        fidelity: "unsupported",
        reason: "通用 Git 服务没有平台数据 API",
        degradation: "该模块不会写入目标；将只迁移 Git 数据",
      }),
    ],
  };
}

/**
 * `index === 1` is the repository the operator excludes, `index === 2` has a
 * non-empty target, and `index === 3` does not exist yet. Everything else is a
 * plain empty-target reuse.
 */
export function repository(index: number): Record<string, unknown> {
  const name = `repo-${String(index).padStart(3, "0")}`;
  const targetState = index === 2 ? "non_empty" : index === 3 ? "missing" : "empty";
  return {
    id: `https://git.source.test/ops/${name}.git`,
    connection_id: "source",
    source_url: `https://git.source.test/ops/${name}.git`,
    name,
    namespace: "ops",
    visibility: "private",
    permission: index === 4 ? "git_only" : "full_migration",
    updated_at_epoch_seconds: 1_760_000_000 - index,
    git_capable: true,
    platform_capable: false,
    target_state: targetState,
    target_url: `https://git.target.test/ops/${name}`,
    target_name: name,
    selectable: true,
    unselectable_reason: null,
  };
}

export function repositories(count: number): Record<string, unknown>[] {
  return Array.from({ length: count }, (_, index) => repository(index));
}

function preflightRow(source: Record<string, unknown>): Record<string, unknown> {
  const state = source.target_state as string;
  const action =
    state === "non_empty" ? "skip_non_empty" : state === "missing" ? "create" : "reuse_empty";
  return {
    repository_id: source.id,
    source_url: source.source_url,
    target_url: source.target_url,
    target_name: source.target_name,
    action,
    permission: source.permission,
    target_state: state,
    module_fidelity: [
      { module: "git", fidelity: "native_rebuild", reason: null, confirmation_required: false },
      {
        module: "issues",
        fidelity: "unsupported",
        reason: "通用 Git 服务没有平台数据 API",
        confirmation_required: true,
      },
    ],
    disk_estimate_bytes: 4_194_304,
    blocking_reason: null,
    suggested_action: state === "non_empty" ? "目标已有提交，默认跳过" : null,
    field_mapping: [
      {
        field: "visibility",
        source_value: "private",
        target_value: null,
        result: "目标平台不支持写入可见性；保持目标现状",
      },
    ],
  };
}

export const REF_POLICY = {
  mode: "git_heads_tags_only",
  allowed_refspecs: ["refs/heads/*:refs/heads/*", "refs/tags/*:refs/tags/*"],
  excluded_refs: ["refs/pull/*", "refs/merge-requests/*"],
  explanation: "默认只迁移 refs/heads 与 refs/tags；平台私有 refs 不会写入目标。",
};

export function preview(selected: Record<string, unknown>[]): Record<string, unknown> {
  const rows = selected.map(preflightRow);
  const count = (action: string) => rows.filter((row) => row.action === action).length;
  return {
    preview_id: "preview-e2e",
    metrics: {
      total: rows.length,
      executable: rows.filter((row) => row.action !== "blocked").length,
      blocked: 0,
      warnings: count("skip_non_empty"),
      create: count("create"),
      reuse: count("reuse_empty"),
      skip: count("skip_non_empty"),
    },
    blocking: [],
    warnings: rows
      .filter((row) => row.action === "skip_non_empty")
      .map((row) => `目标非空，默认跳过：${row.target_url as string}`),
    capability_snapshot_hash: "cap-e2e",
    requires_confirmation: false,
    confirmation_phrase: null,
    ref_policy: REF_POLICY,
    selected_count: rows.length,
    excluded_count: 0,
    rows,
  };
}

export function plan(repositoryCount: number): Record<string, unknown> {
  return {
    plan_id: "plan-e2e",
    plan_hash: "e".repeat(64),
    status: "frozen",
    repository_count: repositoryCount,
    capability_snapshot_hash: "cap-e2e",
    dangerous_confirmed: false,
    created_at_ms: 1_760_000_000_000,
  };
}

export function task(
  index: number,
  patch: Record<string, unknown> = {},
): Record<string, unknown> {
  const name = `repo-${String(index).padStart(3, "0")}`;
  return {
    task_id: `task-${index}`,
    repository_id: `https://git.source.test/ops/${name}.git`,
    source_url: `https://git.source.test/ops/${name}.git`,
    target_url: `https://git.target.test/ops/${name}`,
    stage: "git",
    state: "git",
    attempt: 0,
    progress_completed: 2,
    progress_total: 3,
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

export function batch(tasks: Record<string, unknown>[]): Record<string, unknown> {
  const done = ["succeeded", "partial", "skipped"];
  return {
    batch_id: "batch-e2e",
    plan_id: "plan-e2e",
    plan_hash: "e".repeat(64),
    control: "running",
    concurrency: 2,
    total: tasks.length,
    completed: tasks.filter((item) => done.includes(item.state as string)).length,
    failed: tasks.filter((item) => item.state === "retryable_failed").length,
    started_at_ms: 1_760_000_000_500,
    ended_at_ms: null,
    logs: [],
    tasks,
  };
}

export function reportRow(
  index: number,
  patch: Record<string, unknown> = {},
): Record<string, unknown> {
  const name = `repo-${String(index).padStart(3, "0")}`;
  return {
    task_id: `task-${index}`,
    source_url: `https://git.source.test/ops/${name}.git`,
    target_url: `https://git.target.test/ops/${name}`,
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

export function report(rows: Record<string, unknown>[]): Record<string, unknown> {
  const count = (status: string) => rows.filter((row) => row.status === status).length;
  return {
    batch_id: "batch-e2e",
    metrics: {
      complete_success: count("succeeded"),
      git_success_platform_partial: count("partial"),
      retryable_failure: count("retryable_failed"),
      permission_or_conflict_skip: count("skipped"),
    },
    cleanup: { type: "cleaned" },
    rows,
  };
}

export function connectedSnapshot(
  repositoryCount = 3,
  patch: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    schema_version: 1,
    revision: 1,
    connections: [connection("source"), connection("target")],
    repositories: repositories(repositoryCount),
    active_preview: null,
    active_plan: null,
    active_batch: null,
    report: null,
    resumable: [],
    ...patch,
  };
}

export function emptySnapshot(): Record<string, unknown> {
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
  };
}

export function ipcError(patch: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    code: "ipc.network",
    category: "network",
    retryable: true,
    stage: "git",
    safe_message: "推送过程中连接中断",
    action: "请检查网络后重试该仓库",
    ...patch,
  };
}

// -- bridge installation -----------------------------------------------------

/**
 * Installs the scripted backend before the page boots.
 *
 * The bridge lives entirely inside the page: `addInitScript` serialises the
 * options, so everything it needs must be plain JSON. Calls are recorded on
 * `window.__e2eCalls`, which is what the security spec reads to prove no secret
 * ever crossed the boundary.
 */
export async function installBridge(page: Page, options: BridgeOptions = {}): Promise<void> {
  const payload = {
    snapshot: options.snapshot ?? connectedSnapshot(options.repositoryCount ?? 3),
    failures: options.failures ?? [],
    refPolicy: REF_POLICY,
  };

  await page.addInitScript((data) => {
    const calls: RecordedCall[] = [];
    const counts = new Map<string, number>();
    const listeners = new Set<(envelope: unknown) => void>();
    let snapshot = data.snapshot as Record<string, unknown>;

    const scripted = (command: string, attempt: number) =>
      (data.failures as ScriptedFailure[]).find(
        (failure) => failure.command === command && (failure.onCall ?? 1) === attempt,
      );

    const bump = () => {
      snapshot = { ...snapshot, revision: (snapshot.revision as number) + 1 };
    };

    // Everything below runs inside the page, so it may not reference anything
    // from the module scope; the derivations are written out in full.
    const buildPreview = (selected: any[]) => {
      const rows = selected.map((source) => {
        const state = source.target_state as string;
        const action =
          state === "non_empty"
            ? "skip_non_empty"
            : state === "missing"
              ? "create"
              : "reuse_empty";
        return {
          repository_id: source.id,
          source_url: source.source_url,
          target_url: source.target_url,
          target_name: source.target_name,
          action,
          permission: source.permission,
          target_state: state,
          module_fidelity: [
            {
              module: "git",
              fidelity: "native_rebuild",
              reason: null,
              confirmation_required: false,
            },
            {
              module: "issues",
              fidelity: "unsupported",
              reason: "通用 Git 服务没有平台数据 API",
              confirmation_required: true,
            },
          ],
          disk_estimate_bytes: 4194304,
          blocking_reason: null,
          suggested_action: state === "non_empty" ? "目标已有提交，默认跳过" : null,
          field_mapping: [
            {
              field: "visibility",
              source_value: "private",
              target_value: null,
              result: "目标平台不支持写入可见性；保持目标现状",
            },
          ],
        };
      });
      const count = (action: string) => rows.filter((row) => row.action === action).length;
      return {
        preview_id: "preview-e2e",
        metrics: {
          total: rows.length,
          executable: rows.filter((row) => row.action !== "blocked").length,
          blocked: 0,
          warnings: count("skip_non_empty"),
          create: count("create"),
          reuse: count("reuse_empty"),
          skip: count("skip_non_empty"),
        },
        blocking: [],
        warnings: rows
          .filter((row) => row.action === "skip_non_empty")
          .map((row) => `目标非空，默认跳过：${row.target_url as string}`),
        capability_snapshot_hash: "cap-e2e",
        requires_confirmation: false,
        confirmation_phrase: null,
        ref_policy: data.refPolicy,
        selected_count: rows.length,
        excluded_count: 0,
        rows,
      };
    };

    const buildTask = (row: any, index: number) => ({
      task_id: `task-${index}`,
      repository_id: row.repository_id,
      source_url: row.source_url,
      target_url: row.target_url,
      stage: "git",
      state: "git",
      attempt: 0,
      progress_completed: 2,
      progress_total: 3,
      retryable: false,
      error: null,
      last_checkpoint: {
        stage: "git",
        transition: "heartbeat",
        attempt: 0,
        resumable: true,
        created_at_ms: 1760000001000,
      },
      updated_at_ms: 1760000001000,
    });

    const buildBatch = (tasks: any[]) => {
      const done = ["succeeded", "partial", "skipped"];
      return {
        batch_id: "batch-e2e",
        plan_id: "plan-e2e",
        plan_hash: "e".repeat(64),
        control: "running",
        concurrency: 2,
        total: tasks.length,
        completed: tasks.filter((item) => done.includes(item.state)).length,
        failed: tasks.filter((item) => item.state === "retryable_failed").length,
        started_at_ms: 1760000000500,
        ended_at_ms: null,
        logs: [],
        tasks,
      };
    };

    const executableRows = () =>
      ((snapshot.active_preview as any)?.rows ?? []).filter(
        (row: any) => row.action !== "skip_non_empty",
      );

    /** Handlers mutate the snapshot the way the Rust state machine would. */
    const handlers: Record<string, (input: any) => unknown> = {
      migration_snapshot: () => snapshot,
      connection_test: () => (snapshot.connections as any[])[0]?.capabilities ?? [],
      connection_save: (input) => {
        bump();
        return (snapshot.connections as any[]).find((item) => item.role === input.role);
      },
      connection_authorize: (input) => ({
        credential_ref: `credential/windows/${input.name === "source" ? "1a2b3c4d" : "5e6f7a8b"}`,
        instructions: "已打开凭据录入窗口。请在该窗口中粘贴令牌两次；界面不会收到令牌本身。",
      }),
      repository_discover: () => ({
        items: snapshot.repositories,
        next_cursor: null,
        total_count: (snapshot.repositories as unknown[]).length,
        warnings: [],
      }),
      repository_import: () => ({ imported: 0, duplicate_count: 0, issues: [] }),
      repository_probe_target: (input) =>
        (snapshot.repositories as any[]).find((item) => item.id === input.repository_id),
      repository_set_mapping: (input) =>
        (snapshot.repositories as any[]).find((item) => item.id === input.repository_id),
      plan_preview: (input) => {
        const excluded = new Set<string>(input.excluded_repository_ids ?? []);
        const selected = (snapshot.repositories as any[]).filter(
          (item) =>
            (input.selected_repository_ids ?? []).includes(item.id) && !excluded.has(item.id),
        );
        const value = buildPreview(selected);
        value.excluded_count = excluded.size;
        snapshot = { ...snapshot, active_preview: value };
        bump();
        return value;
      },
      plan_freeze: () => {
        const value = {
          plan_id: "plan-e2e",
          plan_hash: "e".repeat(64),
          status: "frozen",
          repository_count: executableRows().length,
          capability_snapshot_hash: "cap-e2e",
          dangerous_confirmed: false,
          created_at_ms: 1760000000000,
        };
        snapshot = { ...snapshot, active_plan: value };
        bump();
        return value;
      },
      batch_start: () => {
        const value = buildBatch(executableRows().map(buildTask));
        snapshot = { ...snapshot, active_batch: value };
        bump();
        return value;
      },
      batch_pause: () => {
        const value = { ...(snapshot.active_batch as any), control: "paused" };
        snapshot = { ...snapshot, active_batch: value };
        bump();
        return value;
      },
      batch_resume: () => {
        const value = { ...(snapshot.active_batch as any), control: "running" };
        snapshot = { ...snapshot, active_batch: value };
        bump();
        return value;
      },
      batch_cancel: () => {
        const value = { ...(snapshot.active_batch as any), control: "cancelled" };
        snapshot = { ...snapshot, active_batch: value };
        bump();
        return value;
      },
      task_retry: (input) => {
        const current = snapshot.active_batch as any;
        const retried: string[] = [];
        const rejected: { task_id: string; reason: string }[] = [];
        const tasks = current.tasks.map((item: any) => {
          if (!input.task_ids.includes(item.task_id)) return item;
          if (item.state !== "retryable_failed") {
            rejected.push({
              task_id: item.task_id,
              reason: `任务状态为 ${item.state}，只有可重试失败才能重试`,
            });
            return item;
          }
          retried.push(item.task_id);
          return { ...item, state: "git", stage: "git", attempt: item.attempt + 1, error: null };
        });
        const value = {
          ...current,
          control: "running",
          tasks,
          failed: tasks.filter((item: any) => item.state === "retryable_failed").length,
        };
        snapshot = { ...snapshot, active_batch: value };
        bump();
        return { retried, rejected, batch: value };
      },
      report_snapshot: () => snapshot.report,
      report_export: (input) => ({
        path: input.path,
        bytes_written: 2048,
        row_count: ((snapshot.report as any)?.rows ?? []).length,
      }),
    };

    const bridge = {
      invoke(command: string, input?: unknown) {
        calls.push({ command, input });
        const attempt = (counts.get(command) ?? 0) + 1;
        counts.set(command, attempt);

        const failure = scripted(command, attempt);
        if (failure) return Promise.reject(failure.error);

        const handler = handlers[command];
        if (!handler) {
          return Promise.reject({
            code: "e2e.unknown_command",
            category: "validation",
            retryable: false,
            stage: "e2e",
            safe_message: `E2E 桩未实现命令 ${command}`,
            action: "请在 platform-fixtures.ts 中补充该命令",
          });
        }
        try {
          return Promise.resolve(handler((input as any)?.input ?? input));
        } catch (cause) {
          return Promise.reject(cause);
        }
      },
      subscribe(listener: (envelope: unknown) => void) {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
    };

    (window as any).__migrationBridge = bridge;
    (window as any).__e2eCalls = calls;
    (window as any).__e2eEmit = (envelope: unknown) => {
      for (const listener of listeners) listener(envelope);
    };
    (window as any).__e2eSetSnapshot = (next: Record<string, unknown>) => {
      snapshot = next;
    };
    (window as any).__e2eSnapshot = () => snapshot;
  }, payload);
}

/** Everything the renderer sent, for the security and contract assertions. */
export async function recordedCalls(page: Page): Promise<RecordedCall[]> {
  return page.evaluate(() => (window as any).__e2eCalls ?? []);
}

export async function setSnapshot(
  page: Page,
  snapshot: Record<string, unknown>,
): Promise<void> {
  await page.evaluate((next) => (window as any).__e2eSetSnapshot(next), snapshot);
}

export async function emit(page: Page, envelope: Record<string, unknown>): Promise<void> {
  await page.evaluate((value) => (window as any).__e2eEmit(value), envelope);
}
