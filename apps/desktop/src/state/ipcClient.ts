/**
 * The only way the renderer talks to the backend.
 *
 * There is no direct file, shell or credential access here — just the command
 * whitelist declared in `apps/desktop/src-tauri/src/commands/mod.rs`. Errors are
 * always normalised into an `IpcError` so a view never has to render
 * "unknown error".
 */

import { MIGRATION_EVENT } from "./ipcTypes";
import type { EventEnvelope, ErrorCategory, IpcError } from "./ipcTypes";

export const COMMANDS = [
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

export type CommandName = (typeof COMMANDS)[number];

export interface MigrationBridge {
  invoke<T>(command: CommandName, input?: unknown): Promise<T>;
  subscribe(listener: (envelope: EventEnvelope) => void): () => void;
}

export type IpcResult<T> = { ok: true; value: T } | { ok: false; error: IpcError };

function isIpcError(value: unknown): value is IpcError {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.code === "string" &&
    typeof candidate.category === "string" &&
    typeof candidate.retryable === "boolean" &&
    typeof candidate.safe_message === "string" &&
    typeof candidate.action === "string"
  );
}

/**
 * Turns anything thrown across the IPC boundary into a renderable error. The
 * fallback still names a category, a stage and a next action so the UI never
 * shows a dead end.
 */
export function toIpcError(
  cause: unknown,
  stage: string,
  category: ErrorCategory = "validation",
): IpcError {
  if (isIpcError(cause)) return cause;
  const detail =
    cause instanceof Error ? cause.message : typeof cause === "string" ? cause : "";
  return {
    code: "renderer.bridge_failure",
    category,
    retryable: true,
    stage,
    safe_message: detail
      ? `与本地服务通信失败：${detail}`
      : "与本地服务通信失败，界面显示的可能不是最新状态",
    action: "请点击刷新重新读取本地状态库；迁移进度保存在本机，不会丢失",
  };
}

export async function call<T>(
  bridge: MigrationBridge,
  command: CommandName,
  stage: string,
  input?: unknown,
): Promise<IpcResult<T>> {
  try {
    const value = await bridge.invoke<T>(command, input);
    return { ok: true, value };
  } catch (cause) {
    return { ok: false, error: toIpcError(cause, stage) };
  }
}

/** Production bridge. Imported lazily so tests never load the Tauri runtime. */
export function createTauriBridge(): MigrationBridge {
  return {
    // Callers pass the command's *argument object* (e.g. `{ input: {...} }`,
    // matching the Rust parameter name `input`); Tauri matches its arguments
    // by name, so the object is forwarded verbatim. Wrapping it here once
    // more produced `{ input: { input: {...} } }`, which the commands'
    // `deny_unknown_fields` structs reject — every payload-carrying command
    // in the packaged app failed with "unknown field `input`" until this
    // was caught by the desktop E2E run.
    async invoke<T>(command: CommandName, input?: unknown): Promise<T> {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<T>(command, input as Record<string, unknown> | undefined);
    },
    subscribe(listener) {
      let dispose: (() => void) | undefined;
      let cancelled = false;
      void import("@tauri-apps/api/event").then(async ({ listen }) => {
        const unlisten = await listen<EventEnvelope>(MIGRATION_EVENT, (event) => {
          listener(event.payload);
        });
        if (cancelled) {
          unlisten();
        } else {
          dispose = unlisten;
        }
      });
      return () => {
        cancelled = true;
        dispose?.();
      };
    },
  };
}
