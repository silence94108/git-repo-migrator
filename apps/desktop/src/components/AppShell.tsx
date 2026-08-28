/**
 * Application shell: a 224px step sidebar, a 56px toolbar and the page body.
 *
 * Step availability is derived from the backend snapshot, never from local
 * navigation history, so the operator cannot walk into a step whose prerequisite
 * state does not exist yet. Locked steps say why in their tooltip.
 */

import type { ReactNode } from "react";
import { Check, GitBranch, Lock, LockKeyhole, RefreshCw, ShieldCheck } from "lucide-react";

import { Alert, ErrorAlert, Spinner } from "./primitives";
import type { MigrationState } from "../state/migrationStore";
import { stepUnlocked } from "../state/migrationStore";

export const STEPS = [
  { id: "connections", route: "/connections", title: "连接", note: "配置源平台和目标平台" },
  {
    id: "repositories",
    route: "/repositories",
    title: "选择仓库",
    note: "筛选、全选并排除例外",
  },
  { id: "mapping", route: "/mapping", title: "映射与策略", note: "命名、模块和冲突规则" },
  { id: "preflight", route: "/preflight", title: "预检", note: "权限、目标状态和阻断项" },
  { id: "queue", route: "/queue", title: "迁移队列", note: "暂停、恢复和失败重试" },
  { id: "report", route: "/report", title: "报告", note: "校验结果和本地导出" },
] as const;

export type StepId = (typeof STEPS)[number]["id"];

const LOCK_REASONS: Record<StepId, string> = {
  connections: "",
  repositories: "请先完成源平台和目标平台连接",
  mapping: "请先发现或导入至少一个仓库",
  preflight: "请先发现或导入至少一个仓库",
  queue: "请先冻结迁移计划",
  report: "请先启动一个迁移批次",
};

export function AppShell({
  state,
  current,
  title,
  eyebrow,
  onNavigate,
  onRefresh,
  children,
}: {
  state: MigrationState;
  current: StepId;
  title: string;
  eyebrow: string;
  onNavigate: (step: StepId) => void;
  onRefresh: () => void;
  children: ReactNode;
}) {
  const currentIndex = STEPS.findIndex((step) => step.id === current);
  const stale = state.staleRevision > (state.snapshot?.revision ?? 0);

  return (
    <div className="app-shell">
      <aside className="app-sidebar">
        <div className="app-brand">
          <GitBranch size={20} aria-hidden />
          <span>Git Repo Migrator</span>
        </div>
        <nav className="stepper" aria-label="迁移步骤">
          {STEPS.map((step, index) => {
            const unlocked = stepUnlocked(state.snapshot, step.id);
            const done = index < currentIndex && unlocked;
            const isCurrent = step.id === current;
            return (
              <button
                type="button"
                key={step.id}
                className="step"
                aria-current={isCurrent ? "step" : undefined}
                disabled={!unlocked && !isCurrent}
                title={unlocked ? step.note : LOCK_REASONS[step.id]}
                onClick={() => onNavigate(step.id)}
              >
                <span className="step-index" aria-hidden>
                  {done ? <Check size={12} /> : unlocked ? index + 1 : <Lock size={11} />}
                </span>
                <span>
                  <span className="step-title">{step.title}</span>
                  <span className="step-note">
                    {unlocked ? step.note : LOCK_REASONS[step.id]}
                  </span>
                </span>
              </button>
            );
          })}
        </nav>
        <p className="privacy-note">
          <LockKeyhole size={15} aria-hidden />
          <span>代码与令牌仅在本机处理；导出文件不含令牌。</span>
        </p>
      </aside>

      <main className="app-main">
        <header className="app-toolbar">
          <div>
            <p className="caption">{eyebrow}</p>
            <h1>{title}</h1>
          </div>
          <div className="button-row">
            {state.status === "loading" ? <Spinner label="读取本地状态" /> : null}
            <span className="badge" data-tone="info">
              <ShieldCheck size={13} aria-hidden />
              <span>本地模式</span>
            </span>
            <button type="button" className="button button-secondary" onClick={onRefresh}>
              <RefreshCw size={15} aria-hidden /> 刷新
            </button>
          </div>
        </header>

        <div className="app-content">
          {state.schemaMismatch ? (
            <Alert
              tone="error"
              title="本地状态库版本与界面不一致"
              action="请升级应用后再继续；不要在版本不一致时启动迁移。"
            />
          ) : null}
          {stale ? (
            <Alert
              tone="info"
              title="收到新的进度事件，界面可能不是最新"
              action="点击右上角「刷新」从本地状态库重新读取；进度以状态库为准。"
            />
          ) : null}
          {state.error && !state.schemaMismatch ? <ErrorAlert error={state.error} /> : null}
          {children}
        </div>
      </main>
    </div>
  );
}
