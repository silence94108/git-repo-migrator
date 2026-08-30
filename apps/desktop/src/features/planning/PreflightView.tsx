/**
 * Preflight step: the last stop before anything is written to a target.
 *
 * "开始迁移" is enabled only when the backend reports zero blocking rows, every
 * degraded module has been acknowledged, and — for a destructive plan — the
 * operator has retyped the phrase the backend issued. All three are re-checked
 * server-side; this page can only refuse, never authorise.
 */

import { useMemo, useState } from "react";
import { PlayCircle, RefreshCw, Search } from "lucide-react";

import {
  Alert,
  Badge,
  Drawer,
  EmptyState,
  ErrorAlert,
  FidelityBadge,
  MetricStrip,
  PermissionBadge,
  Spinner,
  TargetStateBadge,
  UrlCell,
} from "../../components/primitives";
import type { MigrationState, MigrationStore } from "../../state/migrationStore";
import type { PlanAction, PreflightRow } from "../../state/ipcTypes";

const ACTION_LABELS: Record<PlanAction, { tone: "success" | "warning" | "error" | "info" | "neutral"; label: string }> =
  {
    create: { tone: "info", label: "创建目标" },
    reuse_empty: { tone: "success", label: "复用空仓库" },
    skip_non_empty: { tone: "neutral", label: "跳过非空目标" },
    overwrite: { tone: "error", label: "覆盖目标" },
    rename: { tone: "warning", label: "改名后创建" },
    blocked: { tone: "error", label: "阻断" },
  };

export function PreflightView({
  store,
  state,
  onStarted,
}: {
  store: MigrationStore;
  state: MigrationState;
  onStarted: () => void;
}) {
  const preview = state.snapshot?.active_preview ?? null;
  const draft = state.draft;
  const [onlyProblems, setOnlyProblems] = useState(false);
  const [confirmation, setConfirmation] = useState("");
  const [busy, setBusy] = useState<"probe" | "freeze" | "start" | null>(null);
  const [detail, setDetail] = useState<PreflightRow | null>(null);

  const degradedModules = useMemo(() => {
    const seen = new Map<string, { module: string; reason: string | null }>();
    for (const row of preview?.rows ?? []) {
      for (const module of row.module_fidelity) {
        if (module.confirmation_required) {
          seen.set(module.module, { module: module.module, reason: module.reason });
        }
      }
    }
    return [...seen.values()];
  }, [preview]);

  const unconfirmed = degradedModules.filter(
    (module) => !draft.acknowledgedFidelity.includes(module.module),
  );
  const needsProbe = (preview?.rows ?? []).filter(
    (row) => row.target_state === "unknown" || row.target_state === "inaccessible",
  );

  if (!preview) {
    return (
      <EmptyState
        title="尚未生成预检"
        description="请返回「映射与策略」步骤，设置目标命名和策略后运行预检。"
      />
    );
  }

  const rows = onlyProblems
    ? preview.rows.filter((row) => row.action === "blocked" || row.blocking_reason)
    : preview.rows;

  const phrase = preview.confirmation_phrase ?? "";
  const confirmationSatisfied = !preview.requires_confirmation || confirmation === phrase;
  const canStart =
    preview.metrics.blocked === 0 && unconfirmed.length === 0 && confirmationSatisfied;

  const probeAll = async () => {
    setBusy("probe");
    for (const row of needsProbe) {
      const result = await store.probeTarget({
        repository_id: row.repository_id,
        target_url: row.target_url,
      });
      if (!result.ok) break;
    }
    setBusy(null);
  };

  const startMigration = async () => {
    setBusy("freeze");
    const frozen = await store.freezePlan({
      preview_id: preview.preview_id,
      confirmation_text: preview.requires_confirmation ? confirmation : null,
      acknowledged_fidelity: draft.acknowledgedFidelity,
    });
    if (!frozen.ok) {
      setBusy(null);
      return;
    }
    setBusy("start");
    const started = await store.startBatch(
      frozen.value.plan_id,
      draft.concurrency,
      draft.workspacePolicy,
    );
    setBusy(null);
    if (started.ok) onStarted();
  };

  return (
    <>
      <section className="panel" aria-label="预检摘要">
        <MetricStrip
          label="预检摘要"
          metrics={[
            { label: "计划仓库总数", value: preview.metrics.total },
            { label: "可执行", value: preview.metrics.executable },
            { label: "阻断", value: preview.metrics.blocked },
            { label: "警告", value: preview.metrics.warnings },
            { label: "将创建", value: preview.metrics.create },
            { label: "复用空仓", value: preview.metrics.reuse },
            { label: "跳过非空", value: preview.metrics.skip },
          ]}
        />
        <p className="caption">
          已选择 {preview.selected_count} 个仓库，排除 {preview.excluded_count} 个。
          能力快照 <span className="mono">{preview.capability_snapshot_hash.slice(0, 12)}</span>
        </p>

        {preview.blocking.map((reason) => (
          <Alert
            key={reason}
            tone="error"
            title={reason}
            action="阻断项必须修正或排除；不会被静默跳过。"
          />
        ))}
        {preview.warnings.map((warning) => (
          <Alert key={warning} tone="warning" title={warning} />
        ))}
        {state.error ? <ErrorAlert error={state.error} /> : null}
      </section>

      <section className="panel" aria-label="引用策略摘要">
        <h2>将写入的引用</h2>
        <p className="caption">{preview.ref_policy.explanation}</p>
        <dl className="definition-list">
          <dt>策略</dt>
          <dd className="mono">{preview.ref_policy.mode}</dd>
          <dt>允许的 refspec</dt>
          <dd className="mono">{preview.ref_policy.allowed_refspecs.join(" · ")}</dd>
          <dt>已排除的平台私有 refs</dt>
          <dd className="mono">
            {preview.ref_policy.excluded_refs.length > 0
              ? preview.ref_policy.excluded_refs.join(" · ")
              : "无（已选择归档到本地报告）"}
          </dd>
          <dt>临时工作区</dt>
          <dd>
            {draft.workspacePolicy === "reuse"
              ? "复用残留镜像（重试不重新克隆）"
              : "重试前清理工作区（每次重新克隆）"}
            <span className="step-note"> 可在「映射与策略」步骤调整。</span>
          </dd>
        </dl>
      </section>

      {degradedModules.length > 0 ? (
        <section className="panel" aria-label="保真度确认">
          <h2>模块保真度确认</h2>
          <p className="caption">
            下列模块无法在目标平台原生重建。确认后它们只会在本地归档或标记为未迁移，
            不会伪装成目标平台的可交互条目。
          </p>
          {degradedModules.map((module) => (
            <label className="checkbox-row" key={module.module}>
              <input
                type="checkbox"
                checked={draft.acknowledgedFidelity.includes(module.module)}
                aria-label={`确认模块 ${module.module} 的降级处理`}
                onChange={(event) =>
                  store.updateDraft({
                    acknowledgedFidelity: event.target.checked
                      ? [...draft.acknowledgedFidelity, module.module]
                      : draft.acknowledgedFidelity.filter((item) => item !== module.module),
                  })
                }
              />
              <span>
                {module.module}
                <span className="step-note">{module.reason ?? "目标平台不支持该模块"}</span>
              </span>
            </label>
          ))}
        </section>
      ) : null}

      {preview.requires_confirmation ? (
        <section className="panel" aria-label="危险操作确认">
          <div className="danger-switch">
            <p className="alert-title">覆盖迁移需要二次确认</p>
            <p>
              该计划会替换目标仓库中已有的分支和 Tag。请输入确认文本
              <strong className="mono"> {phrase} </strong>
              以继续；确认由后端校验。
            </p>
            <label className="field">
              <span className="field-label">确认文本</span>
              <input
                type="text"
                value={confirmation}
                aria-label="确认文本"
                onChange={(event) => setConfirmation(event.target.value)}
              />
            </label>
          </div>
        </section>
      ) : null}

      <section className="panel" aria-label="预检明细">
        <div className="button-row">
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={onlyProblems}
              aria-label="只显示阻断和警告"
              onChange={(event) => setOnlyProblems(event.target.checked)}
            />
            <span>只显示阻断 / 警告</span>
          </label>
          {needsProbe.length > 0 ? (
            <button
              type="button"
              className="button button-secondary"
              disabled={busy !== null}
              aria-busy={busy === "probe"}
              onClick={() => void probeAll()}
            >
              {busy === "probe" ? <Spinner label="探测中" /> : <Search size={15} aria-hidden />}
              探测目标状态（{needsProbe.length}）
            </button>
          ) : null}
        </div>

        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th scope="col">仓库</th>
                <th scope="col">计划动作</th>
                <th scope="col">权限</th>
                <th scope="col">目标状态</th>
                <th scope="col" className="col-secondary">
                  模块保真度
                </th>
                <th scope="col">阻断原因 / 建议</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => (
                <tr key={row.repository_id}>
                  <td data-label="仓库">
                    <button
                      type="button"
                      className="button button-link"
                      onClick={() => setDetail(row)}
                    >
                      {row.target_name}
                    </button>
                    <br />
                    <UrlCell url={row.source_url} />
                  </td>
                  <td data-label="计划动作">
                    <Badge
                      tone={ACTION_LABELS[row.action].tone}
                      label={ACTION_LABELS[row.action].label}
                    />
                  </td>
                  <td data-label="权限">
                    <PermissionBadge level={row.permission} />
                  </td>
                  <td data-label="目标状态">
                    <TargetStateBadge state={row.target_state} />
                  </td>
                  <td data-label="模块保真度" className="col-secondary">
                    {row.module_fidelity.length === 0
                      ? "仅 Git 数据"
                      : row.module_fidelity.map((module) => (
                          <FidelityBadge
                            key={module.module}
                            fidelity={module.fidelity}
                            reason={`${module.module}：${module.reason ?? ""}`}
                          />
                        ))}
                  </td>
                  <td data-label="阻断原因 / 建议">
                    {row.blocking_reason ? <strong>{row.blocking_reason}</strong> : null}
                    {row.suggested_action ? (
                      <span className="step-note">{row.suggested_action}</span>
                    ) : null}
                    {!row.blocking_reason && !row.suggested_action ? "—" : null}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>

      <div className="button-row">
        <button
          type="button"
          className="button button-primary"
          disabled={!canStart || busy !== null}
          aria-busy={busy === "freeze" || busy === "start"}
          title={
            preview.metrics.blocked > 0
              ? `仍有 ${preview.metrics.blocked} 项阻断`
              : unconfirmed.length > 0
                ? `请先确认 ${unconfirmed.length} 个降级模块`
                : !confirmationSatisfied
                  ? "请输入后端提供的确认文本"
                  : undefined
          }
          onClick={() => void startMigration()}
        >
          {busy ? <Spinner label="启动中" /> : <PlayCircle size={15} aria-hidden />}
          冻结计划并开始迁移
        </button>
        <button
          type="button"
          className="button button-secondary"
          disabled={busy !== null}
          onClick={() => void store.refresh()}
        >
          <RefreshCw size={15} aria-hidden /> 重新读取预检
        </button>
      </div>

      <Drawer
        open={detail !== null}
        title={`字段映射：${detail?.target_name ?? ""}`}
        onClose={() => setDetail(null)}
      >
        {detail ? (
          <>
            <dl className="definition-list">
              <dt>源地址</dt>
              <dd className="mono">{detail.source_url}</dd>
              <dt>目标地址</dt>
              <dd className="mono">{detail.target_url}</dd>
              <dt>计划动作</dt>
              <dd>{ACTION_LABELS[detail.action].label}</dd>
              <dt>磁盘预估</dt>
              <dd>
                {detail.disk_estimate_bytes > 0
                  ? `${Math.ceil(detail.disk_estimate_bytes / 1_048_576)} MiB`
                  : "未知（将在克隆阶段实测）"}
              </dd>
            </dl>
            <h3>字段映射</h3>
            <div className="table-scroll">
              <table>
                <thead>
                  <tr>
                    <th scope="col">字段</th>
                    <th scope="col">源值</th>
                    <th scope="col">映射结果</th>
                  </tr>
                </thead>
                <tbody>
                  {detail.field_mapping.map((field) => (
                    <tr key={field.field}>
                      <td data-label="字段">{field.field}</td>
                      <td data-label="源值">{field.source_value ?? "—"}</td>
                      <td data-label="映射结果">{field.result}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </>
        ) : null}
      </Drawer>
    </>
  );
}
