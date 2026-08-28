/**
 * Log drawer.
 *
 * Everything shown here already passed backend redaction — the entries are the
 * `log_event` rows, which carry a stable code and a safe message, never a raw
 * command line or response body. Copying is offered per entry so an operator can
 * paste a code into a ticket without pasting anything sensitive.
 */

import { useMemo, useState } from "react";
import { Copy } from "lucide-react";

import { Badge, Drawer } from "../../components/primitives";
import type { LogEntry } from "../../state/ipcTypes";

const LEVEL_TONES: Record<string, "error" | "warning" | "info" | "neutral"> = {
  error: "error",
  warn: "warning",
  warning: "warning",
  info: "info",
};

/** Entries are capped so a long-running batch cannot freeze the drawer. */
export const LOG_WINDOW = 200;

export function filterLogs(
  logs: LogEntry[],
  filters: { taskId: string; stage: string; level: string; code: string },
): LogEntry[] {
  return logs.filter((entry) => {
    if (filters.taskId && entry.task_id !== filters.taskId) return false;
    if (filters.stage && entry.stage !== filters.stage) return false;
    if (filters.level && entry.level !== filters.level) return false;
    if (filters.code && !entry.code.includes(filters.code)) return false;
    return true;
  });
}

export function LogDrawer({
  open,
  logs,
  taskId,
  onClose,
}: {
  open: boolean;
  logs: LogEntry[];
  taskId: string;
  onClose: () => void;
}) {
  const [level, setLevel] = useState("");
  const [stage, setStage] = useState("");
  const [code, setCode] = useState("");
  const [autoScroll, setAutoScroll] = useState(true);

  const filtered = useMemo(
    () => filterLogs(logs, { taskId, stage, level, code }),
    [logs, taskId, stage, level, code],
  );
  const visible = filtered.slice(0, LOG_WINDOW);

  const stages = [...new Set(logs.map((entry) => entry.stage))];
  const levels = [...new Set(logs.map((entry) => entry.level))];

  return (
    <Drawer open={open} title={taskId ? `日志：${taskId}` : "批次日志"} onClose={onClose}>
      <div className="button-row">
        <label className="field">
          <span className="field-label">级别</span>
          <select value={level} onChange={(event) => setLevel(event.target.value)}>
            <option value="">全部</option>
            {levels.map((item) => (
              <option key={item} value={item}>
                {item}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span className="field-label">阶段</span>
          <select value={stage} onChange={(event) => setStage(event.target.value)}>
            <option value="">全部</option>
            {stages.map((item) => (
              <option key={item} value={item}>
                {item}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span className="field-label">错误代码</span>
          <input
            type="search"
            value={code}
            onChange={(event) => setCode(event.target.value)}
            placeholder="ipc.network"
          />
        </label>
      </div>

      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={autoScroll}
          aria-label="自动滚动到最新日志"
          onChange={(event) => setAutoScroll(event.target.checked)}
        />
        <span>自动滚动到最新日志</span>
      </label>

      <p className="caption">
        日志已脱敏：不包含令牌、密码、私钥或完整响应体。
        {filtered.length > visible.length
          ? ` 共 ${filtered.length} 条，仅显示最新 ${visible.length} 条。`
          : ` 共 ${filtered.length} 条。`}
      </p>

      {visible.length === 0 ? (
        <p className="caption">当前筛选条件下没有日志。</p>
      ) : (
        <ul className="log-list">
          {visible.map((entry) => (
            <li className="log-entry" key={`${entry.task_id}:${entry.created_at_ms}:${entry.code}`}>
              <span className="button-row">
                <Badge tone={LEVEL_TONES[entry.level] ?? "neutral"} label={entry.level} />
                <span className="mono">{entry.stage}</span>
                <span className="mono">{entry.code}</span>
              </span>
              <span>{entry.safe_message}</span>
              <span className="button-row">
                <span className="caption">
                  {new Date(entry.created_at_ms).toISOString().replace("T", " ").slice(0, 19)}
                </span>
                <button
                  type="button"
                  className="button button-link"
                  aria-label={`复制错误代码 ${entry.code}`}
                  onClick={() => void navigator.clipboard?.writeText(entry.code)}
                >
                  <Copy size={12} aria-hidden /> 复制错误代码
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}
    </Drawer>
  );
}
