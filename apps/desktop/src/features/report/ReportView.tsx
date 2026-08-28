/**
 * Report step.
 *
 * The four result classes are kept strictly separate: a repository whose Git data
 * pushed cleanly but whose platform data only archived is "部分失败", never
 * "完整成功". Exports go through the backend, which redacts before writing.
 */

import { useMemo, useState } from "react";
import { Download, FolderOpen } from "lucide-react";

import {
  Alert,
  Badge,
  EmptyState,
  ErrorAlert,
  FidelityBadge,
  MetricStrip,
  ResultBadge,
  Spinner,
  UrlCell,
} from "../../components/primitives";
import { EvidenceDrawer } from "./EvidenceDrawer";
import type { MigrationState, MigrationStore } from "../../state/migrationStore";
import type { AggregateStatus, ExportOutcome, ReportRowSnapshot } from "../../state/ipcTypes";

type Filter = AggregateStatus | "all";

const FORMATS: Array<{ id: "json" | "csv" | "mapping"; label: string; suffix: string }> = [
  { id: "json", label: "导出 JSON", suffix: ".json" },
  { id: "csv", label: "导出 CSV", suffix: ".csv" },
  { id: "mapping", label: "导出映射清单", suffix: ".csv" },
];

export function ReportView({
  store,
  state,
  onRetry,
}: {
  store: MigrationStore;
  state: MigrationState;
  onRetry: () => void;
}) {
  const report = state.snapshot?.report ?? null;
  const batch = state.snapshot?.active_batch ?? null;
  const [filter, setFilter] = useState<Filter>("all");
  const [detail, setDetail] = useState<ReportRowSnapshot | null>(null);
  const [exportDir, setExportDir] = useState("");
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<ExportOutcome | null>(null);

  const rows = useMemo(
    () => (report?.rows ?? []).filter((row) => filter === "all" || row.status === filter),
    [report, filter],
  );

  if (!report || report.rows.length === 0) {
    return (
      <EmptyState
        title={batch ? "批次尚未产生结果" : "还没有可展示的报告"}
        description={
          batch
            ? "仓库完成校验后才会计入报告；进行中的任务不会被计为成功。"
            : "启动一次迁移批次后，这里会显示四类结果、校验证据和导出入口。"
        }
      />
    );
  }

  const runExport = async (format: "json" | "csv" | "mapping", suffix: string) => {
    if (!exportDir.trim()) return;
    setBusy(true);
    setOutcome(null);
    const separator = exportDir.includes("\\") ? "\\" : "/";
    const path = `${exportDir.replace(/[\\/]+$/, "")}${separator}${report.batch_id}-${format}${suffix}`;
    const result = await store.exportReport(report.batch_id, format, path);
    setBusy(false);
    if (result.ok) setOutcome(result.value);
  };

  return (
    <>
      <section className="panel" aria-label="结果摘要">
        <MetricStrip
          label="结果摘要"
          metrics={[
            {
              label: "完整成功",
              value: report.metrics.complete_success,
              onSelect: () => setFilter("succeeded"),
            },
            {
              label: "Git 成功 · 平台数据部分失败",
              value: report.metrics.git_success_platform_partial,
              onSelect: () => setFilter("partial"),
            },
            {
              label: "可重试失败",
              value: report.metrics.retryable_failure,
              onSelect: () => setFilter("retryable_failed"),
            },
            {
              label: "权限/冲突跳过",
              value: report.metrics.permission_or_conflict_skip,
              onSelect: () => setFilter("skipped"),
            },
          ]}
        />
        <div className="button-row">
          <span className="caption">当前筛选：</span>
          <Badge
            tone="info"
            label={filter === "all" ? "全部结果" : filter}
          />
          <button
            type="button"
            className="button button-link"
            onClick={() => setFilter("all")}
          >
            清除筛选
          </button>
        </div>
        {report.metrics.git_success_platform_partial > 0 ? (
          <Alert
            tone="warning"
            title={`${report.metrics.git_success_platform_partial} 个仓库的 Git 数据已成功，但平台数据只做了归档或未迁移`}
            action="请打开证据详情查看归档路径和未映射字段；这些仓库不算完整成功。"
          />
        ) : null}
        {state.error ? <ErrorAlert error={state.error} /> : null}
      </section>

      <section className="panel" aria-label="结果明细">
        <div className="table-scroll">
          <table aria-rowcount={rows.length}>
            <thead>
              <tr>
                <th scope="col">源仓库</th>
                <th scope="col">目标仓库</th>
                <th scope="col">最终状态</th>
                <th scope="col">Git</th>
                <th scope="col" className="col-secondary">
                  LFS
                </th>
                <th scope="col" className="col-secondary">
                  元数据
                </th>
                <th scope="col">平台模块</th>
                <th scope="col" className="col-secondary">
                  错误代码
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => (
                <tr key={row.task_id}>
                  <td data-label="源仓库">
                    <button
                      type="button"
                      className="button button-link"
                      onClick={() => setDetail(row)}
                    >
                      查看证据
                    </button>
                    <br />
                    <UrlCell url={row.source_url} />
                  </td>
                  <td data-label="目标仓库">
                    <UrlCell url={row.target_url} />
                  </td>
                  <td data-label="最终状态">
                    <ResultBadge status={row.status} />
                  </td>
                  <td data-label="Git">
                    <Badge
                      tone={row.git_verified ? "success" : "error"}
                      label={row.git_verified ? "已校验" : "未通过"}
                    />
                  </td>
                  <td data-label="LFS" className="col-secondary">
                    <Badge
                      tone={row.lfs_verified ? "success" : "error"}
                      label={row.lfs_verified ? "已校验" : "未通过"}
                    />
                  </td>
                  <td data-label="元数据" className="col-secondary">
                    <Badge
                      tone={row.metadata_verified ? "success" : "warning"}
                      label={row.metadata_verified ? "已校验" : "未校验"}
                    />
                  </td>
                  <td data-label="平台模块">
                    {row.modules.length === 0
                      ? "仅 Git 数据"
                      : row.modules.map((module) => (
                          <FidelityBadge
                            key={module.module}
                            fidelity={module.fidelity}
                            reason={`${module.module}：${module.reason ?? ""}`}
                          />
                        ))}
                  </td>
                  <td data-label="错误代码" className="col-secondary mono">
                    {row.error_code ?? "—"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {rows.length === 0 ? (
          <p className="caption">该分类下没有仓库。</p>
        ) : null}
      </section>

      <section className="panel" aria-label="导出">
        <h2>导出</h2>
        <p className="caption">
          导出文件不含令牌、密码或认证头，但会包含仓库 URL、错误信息和本地临时目录位置，
          请按内部规定存放。
        </p>
        <label className="field">
          <span className="field-label">
            <FolderOpen size={13} aria-hidden /> 导出目录（绝对路径）
          </span>
          <input
            type="text"
            value={exportDir}
            placeholder="D:\\migration-reports"
            onChange={(event) => setExportDir(event.target.value)}
          />
        </label>
        <div className="button-row">
          {FORMATS.map((format) => (
            <button
              type="button"
              key={format.id}
              className="button button-secondary"
              disabled={busy || !exportDir.trim()}
              title={!exportDir.trim() ? "请先填写导出目录" : undefined}
              onClick={() => void runExport(format.id, format.suffix)}
            >
              {busy ? <Spinner label="导出中" /> : <Download size={15} aria-hidden />}
              {format.label}
            </button>
          ))}
        </div>
        {outcome ? (
          <Alert
            tone="success"
            title={`已导出 ${outcome.row_count} 行到本地文件`}
            action="文件保存在本机，未上传到任何服务。"
          >
            <p className="mono">{outcome.path}</p>
          </Alert>
        ) : null}
      </section>

      <section className="panel" aria-label="临时目录">
        {report.cleanup.type === "cleaned" ? (
          <Alert tone="success" title="临时工作目录已清理" />
        ) : report.cleanup.type === "retained_temp_directory" ? (
          <Alert
            tone="warning"
            title="按设置保留了临时工作目录"
            action="目录包含仓库副本，请在确认迁移无误后手动删除。"
          >
            <p className="mono">{report.cleanup.path}</p>
          </Alert>
        ) : (
          <Alert
            tone="error"
            title="临时工作目录清理失败"
            action="请手动删除下列目录；应用不会删除该目录以外的任何内容。"
          >
            <p className="mono">{report.cleanup.path}</p>
            <p>{report.cleanup.reason}</p>
          </Alert>
        )}
      </section>

      <EvidenceDrawer row={detail} onClose={() => setDetail(null)} onRetry={onRetry} />
    </>
  );
}
