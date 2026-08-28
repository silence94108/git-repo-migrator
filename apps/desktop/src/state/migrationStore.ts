/**
 * The single source of renderer state.
 *
 * The rule this file exists to enforce: **the SQLite snapshot is authoritative
 * and events only schedule a refresh.** An event never mutates a task row in
 * memory, so losing, duplicating or reordering events can leave the UI briefly
 * stale but never wrong. `staleRevision` tracks the gap so the UI can say so.
 */

import { useSyncExternalStore } from "react";

import { call, createTauriBridge, toIpcError } from "./ipcClient";
import type { IpcResult, MigrationBridge } from "./ipcClient";
import { emptyDraft } from "./planDraft";
import type { PlanDraft } from "./planDraft";
import { SUPPORTED_SNAPSHOT_SCHEMA_VERSION } from "./ipcTypes";
import type {
  BatchSnapshot,
  ConnectionSaveInput,
  ConnectionSnapshot,
  ConnectionTestInput,
  CapabilitySummary,
  DiscoveryQuery,
  EventEnvelope,
  ExportOutcome,
  IpcError,
  MigrationSnapshot,
  PlanFreezeInput,
  PlanPreviewRequest,
  PlanPreviewSnapshot,
  PlanSnapshot,
  ReportSnapshot,
  RepositoryImportInput,
  RepositoryImportReport,
  RepositoryMappingInput,
  RepositoryPage,
  RepositorySnapshot,
  RetryOutcome,
  TargetProbeInput,
} from "./ipcTypes";

export type StoreStatus = "idle" | "loading" | "ready" | "error";

export interface MigrationState {
  status: StoreStatus;
  snapshot: MigrationSnapshot | null;
  error: IpcError | null;
  /** Highest revision seen on an event but not yet present in a snapshot. */
  staleRevision: number;
  /** Number of events observed; used by the UI to explain a stale banner. */
  eventCount: number;
  schemaMismatch: boolean;
  /** Operator intent for the not-yet-frozen plan. Never authoritative. */
  draft: PlanDraft;
}

const initialState: MigrationState = {
  status: "idle",
  snapshot: null,
  error: null,
  staleRevision: 0,
  eventCount: 0,
  schemaMismatch: false,
  draft: emptyDraft,
};

export class MigrationStore {
  private state: MigrationState = initialState;
  private readonly listeners = new Set<() => void>();
  private unsubscribeBridge: (() => void) | undefined;

  constructor(private readonly bridge: MigrationBridge) {}

  getState = (): MigrationState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    if (this.listeners.size === 1) {
      this.unsubscribeBridge = this.bridge.subscribe(this.onEvent);
    }
    return () => {
      this.listeners.delete(listener);
      if (this.listeners.size === 0) {
        this.unsubscribeBridge?.();
        this.unsubscribeBridge = undefined;
      }
    };
  };

  private setState(patch: Partial<MigrationState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }

  /**
   * Events carry no payload the UI trusts: only the revision is used, to decide
   * whether the snapshot on screen is behind.
   */
  private onEvent = (envelope: EventEnvelope): void => {
    const current = this.state.snapshot?.revision ?? 0;
    this.setState({
      eventCount: this.state.eventCount + 1,
      staleRevision:
        envelope.revision > current
          ? Math.max(this.state.staleRevision, envelope.revision)
          : this.state.staleRevision,
    });
  };

  /** True when an event reported progress the current snapshot does not include. */
  isStale = (): boolean => {
    const current = this.state.snapshot?.revision ?? 0;
    return this.state.staleRevision > current;
  };

  refresh = async (): Promise<IpcResult<MigrationSnapshot>> => {
    if (this.state.status === "idle") this.setState({ status: "loading" });
    const result = await call<MigrationSnapshot>(
      this.bridge,
      "migration_snapshot",
      "snapshot",
    );
    if (!result.ok) {
      this.setState({ status: "error", error: result.error });
      return result;
    }
    const snapshot = result.value;
    const schemaMismatch = snapshot.schema_version !== SUPPORTED_SNAPSHOT_SCHEMA_VERSION;
    this.setState({
      status: "ready",
      snapshot,
      error: schemaMismatch
        ? toIpcError(
            `本地状态库版本为 ${snapshot.schema_version}，界面支持 ${SUPPORTED_SNAPSHOT_SCHEMA_VERSION}`,
            "snapshot",
            "unsupported",
          )
        : null,
      schemaMismatch,
      staleRevision: Math.max(this.state.staleRevision, snapshot.revision),
    });
    return result;
  };

  /** Runs a command and re-reads the authoritative snapshot afterwards. */
  private async mutate<T>(
    command: Parameters<typeof call>[1],
    stage: string,
    input?: unknown,
  ): Promise<IpcResult<T>> {
    const result = await call<T>(this.bridge, command, stage, input);
    if (result.ok) {
      this.setState({ error: null });
      await this.refresh();
    } else {
      this.setState({ error: result.error });
    }
    return result;
  }

  /**
   * Runs a read-only command. Failures surface as store errors so the view can
   * render them, but no snapshot refresh is triggered — nothing changed.
   */
  private async query<T>(
    command: Parameters<typeof call>[1],
    stage: string,
    input?: unknown,
  ): Promise<IpcResult<T>> {
    const result = await call<T>(this.bridge, command, stage, input);
    this.setState({ error: result.ok ? null : result.error });
    return result;
  }

  testConnection = (input: ConnectionTestInput): Promise<IpcResult<CapabilitySummary[]>> =>
    call<CapabilitySummary[]>(this.bridge, "connection_test", "connection", { input });

  saveConnection = (input: ConnectionSaveInput): Promise<IpcResult<ConnectionSnapshot>> =>
    this.mutate<ConnectionSnapshot>("connection_save", "connection", { input });

  discoverRepositories = (
    connectionId: string,
    query: DiscoveryQuery,
  ): Promise<IpcResult<RepositoryPage>> =>
    this.mutate<RepositoryPage>("repository_discover", "discovery", {
      input: { connection_id: connectionId, query },
    });

  importRepositories = (
    input: RepositoryImportInput,
  ): Promise<IpcResult<RepositoryImportReport>> =>
    this.mutate<RepositoryImportReport>("repository_import", "discovery", { input });

  probeTarget = (input: TargetProbeInput): Promise<IpcResult<RepositorySnapshot>> =>
    this.mutate<RepositorySnapshot>("repository_probe_target", "preflight", { input });

  setMapping = (input: RepositoryMappingInput): Promise<IpcResult<RepositorySnapshot>> =>
    this.mutate<RepositorySnapshot>("repository_set_mapping", "mapping", { input });

  previewPlan = (input: PlanPreviewRequest): Promise<IpcResult<PlanPreviewSnapshot>> =>
    this.mutate<PlanPreviewSnapshot>("plan_preview", "preflight", { input });

  freezePlan = (input: PlanFreezeInput): Promise<IpcResult<PlanSnapshot>> =>
    this.mutate<PlanSnapshot>("plan_freeze", "preflight", { input });

  startBatch = (
    planId: string,
    concurrency: number,
    workspacePolicy: "reuse" | "clean",
  ): Promise<IpcResult<BatchSnapshot>> =>
    this.mutate<BatchSnapshot>("batch_start", "queue", {
      input: { plan_id: planId, concurrency, workspace_policy: workspacePolicy },
    });

  pauseBatch = (batchId: string): Promise<IpcResult<BatchSnapshot>> =>
    this.mutate<BatchSnapshot>("batch_pause", "queue", { input: { batch_id: batchId } });

  resumeBatch = (batchId: string): Promise<IpcResult<BatchSnapshot>> =>
    this.mutate<BatchSnapshot>("batch_resume", "queue", { input: { batch_id: batchId } });

  cancelBatch = (batchId: string): Promise<IpcResult<BatchSnapshot>> =>
    this.mutate<BatchSnapshot>("batch_cancel", "queue", { input: { batch_id: batchId } });

  retryTasks = (batchId: string, taskIds: string[]): Promise<IpcResult<RetryOutcome>> =>
    this.mutate<RetryOutcome>("task_retry", "queue", {
      input: { batch_id: batchId, task_ids: taskIds },
    });

  loadReport = (batchId: string): Promise<IpcResult<ReportSnapshot>> =>
    this.query<ReportSnapshot>("report_snapshot", "report", { input: { batch_id: batchId } });

  exportReport = (
    batchId: string,
    format: "json" | "csv" | "mapping",
    path: string,
  ): Promise<IpcResult<ExportOutcome>> =>
    this.query<ExportOutcome>("report_export", "report", {
      input: { batch_id: batchId, format, path },
    });

  clearError = (): void => this.setState({ error: null });

  updateDraft = (patch: Partial<PlanDraft>): void => {
    this.setState({ draft: { ...this.state.draft, ...patch } });
  };
}

let sharedStore: MigrationStore | undefined;

export function getMigrationStore(): MigrationStore {
  sharedStore ??= new MigrationStore(createTauriBridge());
  return sharedStore;
}

/** Test/E2E seam: replaces the process-wide store. */
export function setMigrationStore(store: MigrationStore): void {
  sharedStore = store;
}

export function useMigrationState(store: MigrationStore): MigrationState {
  return useSyncExternalStore(store.subscribe, store.getState, store.getState);
}

// -- derived selectors -----------------------------------------------------

export function connectionFor(
  snapshot: MigrationSnapshot | null,
  role: "source" | "target",
): ConnectionSnapshot | null {
  return snapshot?.connections.find((connection) => connection.role === role) ?? null;
}

/**
 * A step is only reachable once its prerequisite state exists in the snapshot,
 * so the renderer cannot skip ahead of what the backend has actually recorded.
 */
export function stepUnlocked(
  snapshot: MigrationSnapshot | null,
  step: "connections" | "repositories" | "mapping" | "preflight" | "queue" | "report",
): boolean {
  if (step === "connections") return true;
  const source = connectionFor(snapshot, "source");
  const target = connectionFor(snapshot, "target");
  const connected = Boolean(source && target);
  switch (step) {
    case "repositories":
      return connected;
    case "mapping":
      return connected && (snapshot?.repositories.length ?? 0) > 0;
    case "preflight":
      return connected && (snapshot?.repositories.length ?? 0) > 0;
    case "queue":
      return Boolean(snapshot?.active_plan) || Boolean(snapshot?.active_batch);
    case "report":
      return Boolean(snapshot?.active_batch);
    default:
      return false;
  }
}
