/**
 * Migration queue.
 *
 * The table is rendered from the snapshot, never from accumulated events, so a
 * dropped event only delays the display until the next refresh. Retry is offered
 * exclusively for rows the backend marked retryable, and cancelling is spelled
 * out as "does not roll back what already finished".
 */

import { useMemo, useState } from "react";
import { Ban, FileText, PauseCircle, PlayCircle, RotateCcw } from "lucide-react";

import {
  Alert,
  Badge,
  Dialog,
  EmptyState,
  ErrorAlert,
  MetricStrip,
  Progress,
  Spinner,
  TaskStateBadge,
  UrlCell,
} from "../../components/primitives";
import { LogDrawer } from "./LogDrawer";
import type { MigrationState, MigrationStore } from "../../state/migrationStore";
import type { MigrationStage, RepoTaskState, TaskSnapshot } from "../../state/ipcTypes";

const STAGE_LABELS: Record<MigrationStage, string> = {
  preflight: "预检",
  prepare_target: "创建/复用",
  git: "Git",
  lfs: "LFS",
  metadata: "元数据",
  platform_data: "平台数据",
  verify: "校验",
  complete: "完成",
};

const STATUS_FILTERS: Array<{ value: RepoTaskState | "all"; label: string }> = [
  { value: "all", label: "全部状态" },
  { value: "planned", label: "排队中" },
  { value: "git", label: "运行中" },
  { value: "succeeded", label: "完整成功" },
  { value: "partial", label: "平台数据部分失败" },
  { value: "retryable_failed", label: "可重试失败" },
  { value: "skipped", label: "权限/冲突跳过" },
];

function duration(task: TaskSnapshot, startedAt: number | null): string {
  if (!startedAt) return "—";
  const seconds = Math.max(0, Math.round((task.updated_at_ms - startedAt) / 1000));
  return `${seconds}s`;
}

export function QueueView({
  store,
  state,
  onViewReport,
}: {
  store: MigrationStore;
  state: MigrationState;
  onViewReport: () => void;
}) {
  const batch = state.snapshot?.active_batch ?? null;
  const resumable = state.snapshot?.resumable ?? [];
  const [statusFilter, setStatusFilter] = useState<RepoTaskState | "all">("all");
  const [stageFilter, setStageFilter] = useState<MigrationStage | "all">("all");
  const [codeFilter, setCodeFilter] = useState("");
  const [logTask, setLogTask] = useState<string | null>(null);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const [busy, setBusy] = useState(false);
  const [retryNotice, setRetryNotice] = useState<string[]>([]);

  const tasks = useMemo(() => {
    const rows = batch?.tasks ?? [];
    return rows.filter((task) => {
      if (statusFilter !== "all" && task.state !== statusFilter) return false;
      if (stageFilter !== "all" && task.stage !== stageFilter) return false;
      if (codeFilter && !(task.error?.code ?? "").includes(codeFilter)) return false;
      return true;
    });
  }, [batch, statusFilter, stageFilter, codeFilter]);

  const retryable = useMemo(
    () => tasks.filter((task) => task.state === "retryable_failed" && task.retryable),
    [tasks],
  );

  if (!batch) {
    return (
      <EmptyState
        title="尚未启动任何批次"
        description="预检通过并冻结计划后，队列会在这里显示每个仓库的阶段与进度。"
      />
    );
  }

  const control = async (action: "pause" | "resume" | "cancel") => {
    setBusy(true);
    if (action === "pause") await store.pauseBatch(batch.batch_id);
    if (action === "resume") await store.resumeBatch(batch.batch_id);
    if (action === "cancel") await store.cancelBatch(batch.batch_id);
    setBusy(false);
    setConfirmCancel(false);
  };

  const retryFailed = async () => {
    setBusy(true);
    const result = await store.retryTasks(
      batch.batch_id,
      retryable.map((task) => task.task_id),
    );
    setBusy(false);
    if (result.ok) {
      setRetryNotice(
        result.value.rejected.map((item) => `${item.task_id}：${item.reason}`),
      );
    }
  };

  const paused = batch.control === "paused";
  const finished = batch.control === "completed" || batch.control === "cancelled";

  return (
    <>
      {resumable.length > 0 && batch.control !== "completed" ? (
        <Alert
          tone="warning"
          title={`发现 ${resumable.length} 个未完成批次`}
          action="继续之前会重新检查凭据、目标可达性和平台能力；已完成的仓库不会重复迁移。"
        >
          <ul className="log-list">
            {resumable.map((entry) => (
              <li className="log-entry" key={entry.batch_id}>
                <span className="mono">{entry.batch_id}</span>
                <span>剩余 {entry.pending} 个仓库</span>
                <span>
                  {entry.credential_recheck_required ? "需要复检凭据 · " : ""}
                  {entry.capability_recheck_required ? "需要复检平台能力" : "能力快照有效"}
                </span>
              </li>
            ))}
          </ul>
        </Alert>
      ) : null}

      <section className="panel" aria-label="批次工具栏">
        <div className="button-row">
          <span className="mono">{batch.batch_id}</span>
          {paused ? <Badge tone="warning" label="已暂停" /> : null}
          {batch.control === "cancelled" ? <Badge tone="neutral" label="已取消" /> : null}
          {batch.control === "completed" ? <Badge tone="success" label="已完成" /> : null}
          <span className="caption">并发 {batch.concurrency}</span>
        </div>

        <Progress
          completed={batch.completed}
          total={batch.total}
          label="批次总体进度"
        />

        <MetricStrip
          label="批次统计"
          metrics={[
            { label: "总数", value: batch.total },
            { label: "已完成", value: batch.completed },
            { label: "失败", value: batch.failed },
            { label: "可重试", value: retryable.length },
          ]}
        />

        <div className="button-row">
          <button
            type="button"
            className="button button-secondary"
            disabled={busy || finished || paused}
            onClick={() => void control("pause")}
          >
            <PauseCircle size={15} aria-hidden /> 暂停
          </button>
          <button
            type="button"
            className="button button-secondary"
            disabled={busy || finished || !paused}
            onClick={() => void control("resume")}
          >
            <PlayCircle size={15} aria-hidden /> 继续
          </button>
          <button
            type="button"
            className="button button-danger"
            disabled={busy || finished}
            onClick={() => setConfirmCancel(true)}
          >
            <Ban size={15} aria-hidden /> 取消批次
          </button>
          <button
            type="button"
            className="button button-secondary"
            disabled={busy || retryable.length === 0}
            title={retryable.length === 0 ? "当前没有可重试的失败项" : undefined}
            onClick={() => void retryFailed()}
          >
            {busy ? <Spinner label="处理中" /> : <RotateCcw size={15} aria-hidden />}
            只重试可重试失败（{retryable.length}）
          </button>
          <button type="button" className="button button-secondary" onClick={onViewReport}>
            <FileText size={15} aria-hidden /> 查看报告
          </button>
        </div>

        {paused ? (
          <Alert
            tone="info"
            title="已暂停：不会启动新的阶段"
            action="正在进行的阶段会跑完并写入检查点，继续后从检查点恢复。"
          />
        ) : null}
        {retryNotice.length > 0 ? (
          <Alert
            tone="warning"
            title={`${retryNotice.length} 个任务未被重试`}
            action="权限、冲突和校验类失败不会被盲目重试，请先按建议动作处理。"
          >
            <ul className="log-list">
              {retryNotice.map((notice) => (
                <li className="log-entry" key={notice}>
                  {notice}
                </li>
              ))}
            </ul>
          </Alert>
        ) : null}
        {state.error ? <ErrorAlert error={state.error} /> : null}
      </section>

      <section className="panel" aria-label="队列筛选">
        <div className="two-column">
          <label className="field">
            <span className="field-label">状态</span>
            <select
              value={statusFilter}
              onChange={(event) => setStatusFilter(event.target.value as RepoTaskState | "all")}
            >
              {STATUS_FILTERS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span className="field-label">阶段</span>
            <select
              value={stageFilter}
              onChange={(event) => setStageFilter(event.target.value as MigrationStage | "all")}
            >
              <option value="all">全部阶段</option>
              {Object.entries(STAGE_LABELS).map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span className="field-label">错误代码</span>
            <input
              type="search"
              value={codeFilter}
              placeholder="ipc.network"
              onChange={(event) => setCodeFilter(event.target.value)}
            />
          </label>
        </div>
      </section>

      <section className="panel" aria-label="队列明细">
        <div className="table-scroll">
          <table aria-rowcount={tasks.length}>
            <thead>
              <tr>
                <th scope="col">仓库</th>
                <th scope="col">阶段</th>
                <th scope="col">状态</th>
                <th scope="col">进度</th>
                <th scope="col" className="col-secondary">
                  重试次数
                </th>
                <th scope="col" className="col-secondary">
                  最近检查点
                </th>
                <th scope="col" className="col-secondary">
                  耗时
                </th>
                <th scope="col">操作</th>
              </tr>
            </thead>
            <tbody>
              {tasks.map((task) => (
                <tr key={task.task_id}>
                  <td data-label="仓库">
                    <UrlCell url={task.source_url} />
                    <br />
                    <UrlCell url={task.target_url} />
                  </td>
                  <td data-label="阶段">{STAGE_LABELS[task.stage]}</td>
                  <td data-label="状态">
                    <TaskStateBadge state={task.state} />
                    {task.error ? (
                      <span className="step-note">
                        {task.error.safe_message}（{task.error.code}）
                      </span>
                    ) : null}
                  </td>
                  <td data-label="进度">
                    <Progress
                      completed={task.progress_completed}
                      total={task.progress_total}
                      label={`${task.source_url} 的进度`}
                    />
                  </td>
                  <td data-label="重试次数" className="col-secondary">
                    {task.attempt}
                  </td>
                  <td data-label="最近检查点" className="col-secondary">
                    {task.last_checkpoint
                      ? `${task.last_checkpoint.stage} · ${task.last_checkpoint.transition}`
                      : "—"}
                  </td>
                  <td data-label="耗时" className="col-secondary">
                    {duration(task, batch.started_at_ms)}
                  </td>
                  <td data-label="操作">
                    <button
                      type="button"
                      className="button button-link"
                      onClick={() => setLogTask(task.task_id)}
                    >
                      查看日志
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {tasks.length === 0 ? (
          <p className="caption">当前筛选条件下没有任务。</p>
        ) : null}
      </section>

      <LogDrawer
        open={logTask !== null}
        logs={batch.logs}
        taskId={logTask ?? ""}
        onClose={() => setLogTask(null)}
      />

      <Dialog
        open={confirmCancel}
        title="取消迁移批次"
        description={`将停止调度剩余任务。已完成的 ${batch.completed} 个仓库不会回滚，目标上的引用也不会被删除。`}
        onClose={() => setConfirmCancel(false)}
      >
        <div className="button-row">
          <button
            type="button"
            className="button button-danger"
            disabled={busy}
            onClick={() => void control("cancel")}
          >
            确认取消
          </button>
          <button
            type="button"
            className="button button-secondary"
            onClick={() => setConfirmCancel(false)}
          >
            继续迁移
          </button>
        </div>
      </Dialog>
    </>
  );
}
