/**
 * Shared UI primitives.
 *
 * Every status here renders text *and* an icon, every drawer and dialog moves
 * focus in and restores it on close, and every colour comes from a token in
 * `styles/tokens.css`.
 */

import { useEffect, useId, useRef } from "react";
import type { ReactNode } from "react";
import {
  AlertTriangle,
  Archive,
  CheckCircle2,
  CircleSlash,
  Info,
  Loader2,
  PauseCircle,
  RotateCcw,
  ShieldAlert,
  X,
  XCircle,
} from "lucide-react";

import type {
  AggregateStatus,
  Fidelity,
  IpcError,
  PermissionLevel,
  RepoTaskState,
  TargetState,
} from "../state/ipcTypes";

export type Tone = "success" | "warning" | "error" | "info" | "neutral";

const ICONS: Record<Tone, typeof Info> = {
  success: CheckCircle2,
  warning: AlertTriangle,
  error: XCircle,
  info: Info,
  neutral: CircleSlash,
};

export function Badge({
  tone,
  label,
  title,
  icon,
}: {
  tone: Tone;
  label: string;
  title?: string;
  icon?: ReactNode;
}) {
  const Icon = ICONS[tone];
  return (
    <span className="badge" data-tone={tone} title={title ?? label}>
      {icon ?? <Icon size={13} aria-hidden />}
      {/* The text is the accessible status; the icon is decoration. */}
      <span>{label}</span>
    </span>
  );
}

const TASK_STATE_LABELS: Record<RepoTaskState, { tone: Tone; label: string }> = {
  planned: { tone: "neutral", label: "排队中" },
  preflighted: { tone: "info", label: "预检完成" },
  preparing: { tone: "info", label: "准备目标" },
  git: { tone: "info", label: "推送 Git" },
  lfs: { tone: "info", label: "推送 LFS" },
  metadata: { tone: "info", label: "写入元数据" },
  platform_modules: { tone: "info", label: "迁移平台数据" },
  verifying: { tone: "info", label: "校验中" },
  succeeded: { tone: "success", label: "完整成功" },
  partial: { tone: "warning", label: "平台数据部分失败" },
  retryable_failed: { tone: "error", label: "可重试失败" },
  skipped: { tone: "neutral", label: "权限/冲突跳过" },
};

export function TaskStateBadge({ state }: { state: RepoTaskState }) {
  const { tone, label } = TASK_STATE_LABELS[state];
  return <Badge tone={tone} label={label} />;
}

const RESULT_LABELS: Record<AggregateStatus, { tone: Tone; label: string }> = {
  succeeded: { tone: "success", label: "完整成功" },
  partial: { tone: "warning", label: "Git 成功 · 平台数据部分失败" },
  failed: { tone: "error", label: "失败" },
  retryable_failed: { tone: "error", label: "可重试失败" },
  skipped: { tone: "neutral", label: "权限/冲突跳过" },
};

export function ResultBadge({ status }: { status: AggregateStatus }) {
  const { tone, label } = RESULT_LABELS[status];
  return <Badge tone={tone} label={label} />;
}

const PERMISSION_LABELS: Record<PermissionLevel, { tone: Tone; label: string; hint: string }> =
  {
    full_migration: {
      tone: "success",
      label: "完整迁移",
      hint: "凭据具备读取与管理权限",
    },
    git_only: {
      tone: "warning",
      label: "仅 Git 数据",
      hint: "凭据可读取仓库，但不能写入平台数据",
    },
    insufficient: {
      tone: "error",
      label: "权限不足",
      hint: "凭据对该仓库没有读取或推送权限，无法选择",
    },
  };

export function PermissionBadge({ level }: { level: PermissionLevel }) {
  const { tone, label, hint } = PERMISSION_LABELS[level];
  return <Badge tone={tone} label={label} title={hint} />;
}

const FIDELITY_LABELS: Record<Fidelity, { tone: Tone; label: string; icon: ReactNode }> = {
  native_rebuild: {
    tone: "success",
    label: "原生重建",
    icon: <CheckCircle2 size={13} aria-hidden />,
  },
  read_only_archive: {
    tone: "warning",
    label: "只读归档",
    icon: <Archive size={13} aria-hidden />,
  },
  unsupported: {
    tone: "neutral",
    label: "不支持",
    icon: <CircleSlash size={13} aria-hidden />,
  },
};

export function FidelityBadge({ fidelity, reason }: { fidelity: Fidelity; reason?: string | null }) {
  const { tone, label, icon } = FIDELITY_LABELS[fidelity];
  return <Badge tone={tone} label={label} title={reason ?? label} icon={icon} />;
}

const TARGET_STATE_LABELS: Record<TargetState, { tone: Tone; label: string }> = {
  unknown: { tone: "warning", label: "待复检" },
  missing: { tone: "info", label: "不存在" },
  empty: { tone: "success", label: "空仓库" },
  non_empty: { tone: "warning", label: "非空" },
  inaccessible: { tone: "error", label: "不可访问" },
};

export function TargetStateBadge({ state }: { state: TargetState }) {
  const { tone, label } = TARGET_STATE_LABELS[state];
  return <Badge tone={tone} label={label} />;
}

/**
 * Blocking problems are announced assertively; everything else politely, per
 * ui-spec §7.3.
 */
export function Alert({
  tone,
  title,
  children,
  code,
  action,
}: {
  tone: Tone;
  title: string;
  children?: ReactNode;
  code?: string | null;
  action?: string | null;
}) {
  const Icon = ICONS[tone];
  return (
    <div
      className="alert"
      data-tone={tone}
      role={tone === "error" ? "alert" : "status"}
      aria-live={tone === "error" ? "assertive" : "polite"}
    >
      <p className="alert-title">
        <Icon size={15} aria-hidden /> {title}
      </p>
      {children}
      {action ? <p>{action}</p> : null}
      {code ? <p className="alert-meta">错误代码：{code}</p> : null}
    </div>
  );
}

export function ErrorAlert({ error }: { error: IpcError }) {
  return (
    <Alert
      tone={error.retryable ? "warning" : "error"}
      title={error.safe_message}
      action={error.action}
      code={error.code}
    >
      <p className="caption">
        影响范围：{error.stage} 阶段 · 分类 {error.category}
        {error.retryable ? " · 可重试" : " · 不可自动重试"}
      </p>
    </Alert>
  );
}

export interface Metric {
  label: string;
  value: number | string;
  onSelect?: () => void;
  tone?: Tone;
}

export function MetricStrip({ metrics, label }: { metrics: Metric[]; label: string }) {
  return (
    <ul className="metric-strip" aria-label={label}>
      {metrics.map((metric) => (
        <li className="metric" key={metric.label}>
          {metric.onSelect ? (
            <button type="button" onClick={metric.onSelect}>
              <span className="metric-value">{metric.value}</span>
              <span className="metric-label">{metric.label}</span>
            </button>
          ) : (
            <>
              <span className="metric-value">{metric.value}</span>
              <span className="metric-label">{metric.label}</span>
            </>
          )}
        </li>
      ))}
    </ul>
  );
}

/**
 * Shows `completed/total` alongside the bar. When the total is unknown the label
 * says "处理中" rather than inventing a percentage.
 */
export function Progress({
  completed,
  total,
  label,
}: {
  completed: number;
  total: number | null;
  label: string;
}) {
  const percent = total && total > 0 ? Math.min(100, Math.round((completed / total) * 100)) : null;
  return (
    <div className="progress" aria-live="polite">
      <div
        className="progress-track"
        role="progressbar"
        aria-label={label}
        aria-valuenow={percent ?? undefined}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuetext={percent === null ? "处理中" : `${percent}%`}
      >
        <div className="progress-fill" style={{ width: `${percent ?? 8}%` }} />
      </div>
      <span className="caption">
        {percent === null ? "处理中" : `${percent}%`} · {completed}/{total ?? "?"}
      </span>
    </div>
  );
}

export function Spinner({ label }: { label: string }) {
  return (
    <span className="badge" data-tone="info" role="status" aria-busy="true">
      <Loader2 size={13} aria-hidden />
      <span>{label}</span>
    </span>
  );
}

export function PausedBadge() {
  return <Badge tone="warning" label="已暂停" icon={<PauseCircle size={13} aria-hidden />} />;
}

export function RetryableBadge() {
  return <Badge tone="error" label="可重试" icon={<RotateCcw size={13} aria-hidden />} />;
}

export function DangerBadge({ label }: { label: string }) {
  return <Badge tone="error" label={label} icon={<ShieldAlert size={13} aria-hidden />} />;
}

function useDismissable(open: boolean, onClose: () => void) {
  const container = useRef<HTMLDivElement | null>(null);
  const restoreTo = useRef<Element | null>(null);

  useEffect(() => {
    if (!open) return undefined;
    restoreTo.current = document.activeElement;
    const focusable = container.current?.querySelector<HTMLElement>(
      'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
    );
    (focusable ?? container.current)?.focus();

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        onClose();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      // Focus returns to whatever opened the surface.
      if (restoreTo.current instanceof HTMLElement) restoreTo.current.focus();
    };
  }, [open, onClose]);

  return container;
}

export function Drawer({
  open,
  title,
  onClose,
  children,
}: {
  open: boolean;
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const container = useDismissable(open, onClose);
  const titleId = useId();
  if (!open) return null;
  return (
    <div
      className="drawer"
      role="dialog"
      aria-modal="false"
      aria-labelledby={titleId}
      ref={container}
      tabIndex={-1}
    >
      <div className="drawer-header">
        <h2 id={titleId}>{title}</h2>
        <button
          type="button"
          className="button button-secondary"
          onClick={onClose}
          aria-label="关闭抽屉"
        >
          <X size={15} aria-hidden />
        </button>
      </div>
      {children}
    </div>
  );
}

export function Dialog({
  open,
  title,
  description,
  onClose,
  children,
}: {
  open: boolean;
  title: string;
  description: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const container = useDismissable(open, onClose);
  const titleId = useId();
  const descriptionId = useId();
  if (!open) return null;
  return (
    <div className="dialog">
      <div
        className="dialog-body"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={descriptionId}
        ref={container}
        tabIndex={-1}
      >
        <h2 id={titleId}>{title}</h2>
        <p id={descriptionId}>{description}</p>
        {children}
      </div>
    </div>
  );
}

export function Field({
  label,
  hint,
  error,
  children,
}: {
  label: string;
  hint?: string;
  error?: string | null;
  children: (ids: { inputId: string; describedBy: string | undefined }) => ReactNode;
}) {
  const inputId = useId();
  const hintId = useId();
  const errorId = useId();
  const describedBy = [hint ? hintId : null, error ? errorId : null]
    .filter(Boolean)
    .join(" ");
  return (
    <div className="field">
      <label className="field-label" htmlFor={inputId}>
        {label}
      </label>
      {children({ inputId, describedBy: describedBy || undefined })}
      {hint ? (
        <p className="caption" id={hintId}>
          {hint}
        </p>
      ) : null}
      {error ? (
        <p className="field-error" id={errorId}>
          <XCircle size={13} aria-hidden /> {error}
        </p>
      ) : null}
    </div>
  );
}

export function EmptyState({
  title,
  description,
  action,
}: {
  title: string;
  description: string;
  action?: ReactNode;
}) {
  return (
    <div className="empty-state">
      <h3>{title}</h3>
      <p className="caption">{description}</p>
      {action}
    </div>
  );
}

/** Long URLs are truncated in tables but always available in full on hover. */
export function UrlCell({ url }: { url: string }) {
  return (
    <span className="cell-url" title={url}>
      {url}
    </span>
  );
}
