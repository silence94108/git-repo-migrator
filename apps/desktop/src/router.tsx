/**
 * Six-step hash router.
 *
 * Hash routing is used deliberately: the Tauri window loads from a custom
 * protocol where history routing needs extra navigation permissions, and the app
 * has a fixed set of six steps. A route whose prerequisites are not satisfied in
 * the backend snapshot redirects to the furthest step that is, so a bookmark or a
 * reload can never land on a step the backend has no state for.
 */

import { useCallback, useEffect, useState } from "react";

import { AppShell, STEPS } from "./components/AppShell";
import type { StepId } from "./components/AppShell";
import { ConnectionView } from "./features/connection/ConnectionView";
import { DiscoveryView } from "./features/discovery/DiscoveryView";
import { MappingView } from "./features/planning/MappingView";
import { PreflightView } from "./features/planning/PreflightView";
import { QueueView } from "./features/queue/QueueView";
import { ReportView } from "./features/report/ReportView";
import { getMigrationStore, stepUnlocked, useMigrationState } from "./state/migrationStore";
import type { MigrationStore } from "./state/migrationStore";

const ROUTE_TO_STEP = new Map<string, StepId>(STEPS.map((step) => [step.route, step.id]));
const STEP_TO_ROUTE = new Map<StepId, string>(STEPS.map((step) => [step.id, step.route]));

export function parseRoute(hash: string): StepId {
  const route = hash.replace(/^#/, "") || "/connections";
  return ROUTE_TO_STEP.get(route) ?? "connections";
}

export function routeFor(step: StepId): string {
  return STEP_TO_ROUTE.get(step) ?? "/connections";
}

const PAGE_TITLES: Record<StepId, { title: string; eyebrow: string }> = {
  connections: { title: "连接 Git 平台", eyebrow: "第 1 步 · 连接" },
  repositories: { title: "选择要迁移的仓库", eyebrow: "第 2 步 · 选择仓库" },
  mapping: { title: "映射与迁移策略", eyebrow: "第 3 步 · 映射与策略" },
  preflight: { title: "预检与计划冻结", eyebrow: "第 4 步 · 预检" },
  queue: { title: "迁移队列", eyebrow: "第 5 步 · 执行" },
  report: { title: "迁移报告", eyebrow: "第 6 步 · 报告" },
};

export function AppRouter({ store = getMigrationStore() }: { store?: MigrationStore }) {
  const state = useMigrationState(store);
  const [requested, setRequested] = useState<StepId>(() =>
    parseRoute(typeof window === "undefined" ? "" : window.location.hash),
  );

  useEffect(() => {
    void store.refresh();
  }, [store]);

  useEffect(() => {
    const onHashChange = () => setRequested(parseRoute(window.location.hash));
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  const navigate = useCallback((step: StepId) => {
    setRequested(step);
    if (typeof window !== "undefined") {
      window.location.hash = routeFor(step);
    }
  }, []);

  // Falls back to the furthest reachable step rather than rendering a page whose
  // prerequisite state does not exist.
  const current = stepUnlocked(state.snapshot, requested)
    ? requested
    : ([...STEPS].reverse().find((step) => stepUnlocked(state.snapshot, step.id))?.id ??
      "connections");

  const { title, eyebrow } = PAGE_TITLES[current];

  return (
    <AppShell
      state={state}
      current={current}
      title={title}
      eyebrow={eyebrow}
      onNavigate={navigate}
      onRefresh={() => void store.refresh()}
    >
      {current === "connections" ? <ConnectionView store={store} state={state} /> : null}
      {current === "repositories" ? (
        <DiscoveryView store={store} state={state} onContinue={() => navigate("mapping")} />
      ) : null}
      {current === "mapping" ? (
        <MappingView store={store} state={state} onPreflight={() => navigate("preflight")} />
      ) : null}
      {current === "preflight" ? (
        <PreflightView store={store} state={state} onStarted={() => navigate("queue")} />
      ) : null}
      {current === "queue" ? (
        <QueueView store={store} state={state} onViewReport={() => navigate("report")} />
      ) : null}
      {current === "report" ? (
        <ReportView store={store} state={state} onRetry={() => navigate("queue")} />
      ) : null}
    </AppShell>
  );
}
