/**
 * Repository step: discover or import, filter, then select the whole result set
 * and carve out exceptions.
 *
 * The table renders a bounded window of rows, but every count on screen — and
 * everything handed to the next step — comes from the full filtered set.
 */

import { useMemo, useState } from "react";
import { FileUp, Filter, RefreshCw, X } from "lucide-react";

import {
  Alert,
  Badge,
  Drawer,
  EmptyState,
  MetricStrip,
  PermissionBadge,
  Spinner,
  TargetStateBadge,
  UrlCell,
} from "../../components/primitives";
import type { MigrationState, MigrationStore } from "../../state/migrationStore";
import { connectionFor } from "../../state/migrationStore";
import type {
  PermissionLevel,
  RepositoryImportReport,
  RepositoryVisibility,
} from "../../state/ipcTypes";
import {
  ROW_WINDOW,
  activeChips,
  applyFilters,
  clearChip,
  clearSelection,
  emptyFilters,
  emptySelection,
  formatUpdated,
  isSelected,
  resolveSelection,
  selectAllFiltered,
  toggle,
  windowRows,
} from "./selectionModel";
import type { ExclusionKind, Filters, SelectionState } from "./selectionModel";

const VISIBILITY_OPTIONS: Array<{ value: RepositoryVisibility | "any"; label: string }> = [
  { value: "any", label: "全部可见性" },
  { value: "public", label: "公开" },
  { value: "internal", label: "内部" },
  { value: "private", label: "私有" },
  { value: "unknown", label: "未知" },
];

const PERMISSION_OPTIONS: Array<{ value: PermissionLevel | "any"; label: string }> = [
  { value: "any", label: "全部权限" },
  { value: "full_migration", label: "完整迁移" },
  { value: "git_only", label: "仅 Git 数据" },
  { value: "insufficient", label: "权限不足" },
];

export function DiscoveryView({
  store,
  state,
  onContinue,
}: {
  store: MigrationStore;
  state: MigrationState;
  onContinue: () => void;
}) {
  const [filters, setFilters] = useState<Filters>(emptyFilters);
  const [selection, setSelection] = useState<SelectionState>(emptySelection);
  const [urls, setUrls] = useState("");
  const [importReport, setImportReport] = useState<RepositoryImportReport | null>(null);
  const [busy, setBusy] = useState<"discover" | "import" | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [showExcluded, setShowExcluded] = useState(false);
  const [rulePattern, setRulePattern] = useState("");
  const [ruleKind, setRuleKind] = useState<ExclusionKind>("name_glob");

  const repositories = state.snapshot?.repositories ?? [];
  const source = connectionFor(state.snapshot, "source");
  const nowSeconds = Math.floor(Date.now() / 1000);

  const filtered = useMemo(
    () => applyFilters(repositories, filters, nowSeconds),
    [repositories, filters, nowSeconds],
  );
  const resolved = useMemo(() => resolveSelection(filtered, selection), [filtered, selection]);
  const { rows, hidden } = windowRows(filtered, ROW_WINDOW);
  const chips = activeChips(filters);

  const discover = async () => {
    if (!source) return;
    setBusy("discover");
    const result = await store.discoverRepositories(source.id, {
      scope: "all_accessible",
      search: filters.search.trim() || null,
      visibility: filters.visibility === "any" ? null : filters.visibility,
      include_archived: false,
      cursor: null,
      page_size: 100,
    });
    // A partial failure keeps whatever was already loaded and explains itself.
    setWarnings(result.ok ? result.value.warnings : []);
    setBusy(null);
  };

  const runImport = async () => {
    if (!source) return;
    setBusy("import");
    const result = await store.importRepositories({ connection_id: source.id, urls });
    if (result.ok) {
      setImportReport(result.value);
      setUrls("");
    }
    setBusy(null);
  };

  const addRule = () => {
    if (!rulePattern.trim()) return;
    setSelection((current) => ({
      ...current,
      rules: [
        ...current.rules,
        {
          id: `${ruleKind}:${rulePattern.trim()}`,
          kind: ruleKind,
          pattern: rulePattern.trim(),
          enabled: true,
        },
      ],
    }));
    setRulePattern("");
  };

  const proceed = () => {
    store.updateDraft({
      selectedRepositoryIds: resolved.selectedIds,
      excludedRepositoryIds: resolved.exclusions.map((exclusion) => exclusion.id),
    });
    onContinue();
  };

  return (
    <>
      <section className="panel" aria-label="仓库范围">
        <div className="button-row">
          <span className="caption">源连接：{source?.endpoint ?? "未配置"}</span>
          <button
            type="button"
            className="button button-secondary"
            disabled={!source || busy !== null}
            aria-busy={busy === "discover"}
            onClick={() => void discover()}
          >
            {busy === "discover" ? <Spinner label="发现中" /> : <RefreshCw size={15} aria-hidden />}
            自动发现仓库
          </button>
        </div>

        <label className="field">
          <span className="field-label">
            <FileUp size={13} aria-hidden /> 手动 URL 导入（每行一个，支持 HTTPS / SSH）
          </span>
          <textarea
            value={urls}
            disabled={busy !== null}
            placeholder={"https://git.example.com/team/repo.git\ngit@git.example.com:team/other.git"}
            onChange={(event) => setUrls(event.target.value)}
          />
        </label>
        <div className="button-row">
          <button
            type="button"
            className="button button-secondary"
            disabled={!source || busy !== null || !urls.trim()}
            aria-busy={busy === "import"}
            onClick={() => void runImport()}
          >
            {busy === "import" ? <Spinner label="导入中" /> : "导入这些地址"}
          </button>
        </div>

        {importReport ? (
          <Alert
            tone={importReport.issues.length > 0 ? "warning" : "success"}
            title={`已导入 ${importReport.imported} 个地址，去重 ${importReport.duplicate_count} 个`}
            action={
              importReport.issues.length > 0
                ? "下列行未导入，请修正后重新粘贴。"
                : undefined
            }
          >
            {importReport.issues.length > 0 ? (
              <ul className="log-list">
                {importReport.issues.map((issue) => (
                  <li className="log-entry" key={`${issue.line}:${issue.value}`}>
                    第 {issue.line} 行：{issue.message}
                    <span className="mono">{issue.value}</span>
                  </li>
                ))}
              </ul>
            ) : null}
          </Alert>
        ) : null}

        {warnings.map((warning) => (
          <Alert key={warning} tone="warning" title={warning} action="已保留成功加载的结果。" />
        ))}
      </section>

      <section className="panel" aria-label="筛选条件">
        <div className="two-column">
          <label className="field">
            <span className="field-label">
              <Filter size={13} aria-hidden /> 仓库名称
            </span>
            <input
              type="search"
              value={filters.search}
              onChange={(event) => setFilters({ ...filters, search: event.target.value })}
            />
          </label>
          <label className="field">
            <span className="field-label">组织 / 命名空间</span>
            <input
              type="text"
              value={filters.namespace}
              onChange={(event) => setFilters({ ...filters, namespace: event.target.value })}
            />
          </label>
          <label className="field">
            <span className="field-label">可见性</span>
            <select
              value={filters.visibility}
              onChange={(event) =>
                setFilters({
                  ...filters,
                  visibility: event.target.value as RepositoryVisibility | "any",
                })
              }
            >
              {VISIBILITY_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span className="field-label">权限级别</span>
            <select
              value={filters.permission}
              onChange={(event) =>
                setFilters({
                  ...filters,
                  permission: event.target.value as PermissionLevel | "any",
                })
              }
            >
              {PERMISSION_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
        </div>
        <label className="checkbox-row">
          <input
            type="checkbox"
            checked={filters.onlyFullMigration}
            onChange={(event) =>
              setFilters({ ...filters, onlyFullMigration: event.target.checked })
            }
          />
          <span>只显示可完整迁移的仓库</span>
        </label>
        {chips.length > 0 ? (
          <div className="button-row" aria-label="已应用的筛选条件">
            {chips.map((chip) => (
              <button
                type="button"
                key={chip.key}
                className="badge"
                data-tone="info"
                onClick={() => setFilters(clearChip(filters, chip.key))}
              >
                {chip.label}
                <X size={12} aria-hidden />
                <span className="visually-hidden">移除该筛选条件</span>
              </button>
            ))}
          </div>
        ) : null}
      </section>

      <section className="panel" aria-label="选择集合">
        <div className="button-row">
          <button
            type="button"
            className="button button-primary"
            disabled={filtered.length === 0}
            onClick={() => setSelection(selectAllFiltered(selection))}
          >
            选择全部筛选结果（{filtered.length}）
          </button>
          <button
            type="button"
            className="button button-secondary"
            onClick={() => setSelection(clearSelection(selection))}
          >
            清空选择
          </button>
          <button
            type="button"
            className="button button-secondary"
            onClick={() => setShowExcluded(true)}
          >
            查看排除项（{resolved.exclusions.length}）
          </button>
        </div>

        {selection.selectAll ? (
          <Alert
            tone="info"
            title={`已选择全部 ${resolved.matchingCount} 个筛选结果，当前排除 ${resolved.exclusions.length} 个`}
            action="选择的是全部筛选结果，不是当前显示的行。"
          />
        ) : null}

        <div className="button-row">
          <label className="field">
            <span className="field-label">排除规则类型</span>
            <select
              value={ruleKind}
              onChange={(event) => setRuleKind(event.target.value as ExclusionKind)}
            >
              <option value="name_glob">名称通配符</option>
              <option value="namespace">组织</option>
              <option value="repository">单个仓库</option>
            </select>
          </label>
          <label className="field">
            <span className="field-label">规则内容</span>
            <input
              type="text"
              value={rulePattern}
              placeholder="例如 *-archive"
              onChange={(event) => setRulePattern(event.target.value)}
            />
          </label>
          <button type="button" className="button button-secondary" onClick={addRule}>
            添加排除规则
          </button>
        </div>

        <MetricStrip
          label="选择摘要"
          metrics={[
            { label: "最终计划数", value: resolved.selectedIds.length },
            { label: "可完整迁移", value: resolved.fullMigrationCount },
            { label: "仅 Git 数据", value: resolved.gitOnlyCount },
            { label: "权限不足（阻断）", value: resolved.blockedCount },
          ]}
        />
      </section>

      <section className="panel" aria-label="仓库列表">
        {filtered.length === 0 ? (
          <EmptyState
            title={repositories.length === 0 ? "尚未发现任何仓库" : "当前筛选条件没有结果"}
            description={
              repositories.length === 0
                ? "使用「自动发现仓库」，或在上方粘贴仓库地址后点击导入。"
                : "请放宽筛选条件，或清除已应用的条件标签。"
            }
            action={
              repositories.length > 0 ? (
                <button
                  type="button"
                  className="button button-secondary"
                  onClick={() => setFilters(emptyFilters)}
                >
                  清除全部筛选
                </button>
              ) : null
            }
          />
        ) : (
          <>
            <div className="table-scroll">
              <table aria-rowcount={filtered.length}>
                <caption className="visually-hidden">
                  仓库列表，共 {filtered.length} 行，已渲染 {rows.length} 行
                </caption>
                <thead>
                  <tr>
                    <th scope="col">选择</th>
                    <th scope="col">仓库</th>
                    <th scope="col">权限</th>
                    <th scope="col" className="col-secondary">
                      可见性
                    </th>
                    <th scope="col" className="col-secondary">
                      更新时间
                    </th>
                    <th scope="col">Git 能力</th>
                    <th scope="col" className="col-secondary">
                      平台数据能力
                    </th>
                    <th scope="col">目标状态</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((repository) => {
                    const selected = isSelected(selection, repository.id);
                    return (
                      <tr
                        key={repository.id}
                        aria-selected={selected}
                        data-disabled={!repository.selectable}
                      >
                        <td data-label="选择">
                          <input
                            type="checkbox"
                            checked={selected && repository.selectable}
                            disabled={!repository.selectable}
                            aria-label={`选择 ${repository.name}`}
                            title={repository.unselectable_reason ?? undefined}
                            onChange={() => setSelection(toggle(selection, repository.id))}
                          />
                        </td>
                        <td data-label="仓库">
                          <strong>{repository.name}</strong>
                          <br />
                          <UrlCell url={repository.source_url} />
                        </td>
                        <td data-label="权限">
                          <PermissionBadge level={repository.permission} />
                        </td>
                        <td data-label="可见性" className="col-secondary">
                          {repository.visibility}
                        </td>
                        <td data-label="更新时间" className="col-secondary">
                          {formatUpdated(repository.updated_at_epoch_seconds)}
                        </td>
                        <td data-label="Git 能力">
                          <Badge
                            tone={repository.git_capable ? "success" : "error"}
                            label={repository.git_capable ? "可迁移" : "不可读取"}
                          />
                        </td>
                        <td data-label="平台数据能力" className="col-secondary">
                          <Badge
                            tone={repository.platform_capable ? "success" : "neutral"}
                            label={repository.platform_capable ? "可迁移" : "不支持"}
                          />
                        </td>
                        <td data-label="目标状态">
                          <TargetStateBadge state={repository.target_state} />
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
            {hidden > 0 ? (
              <p className="caption" role="status">
                为保持响应速度，表格只渲染前 {rows.length} 行；还有 {hidden}{" "}
                行未渲染，但计数与选择始终覆盖全部 {filtered.length} 个结果。
              </p>
            ) : null}
          </>
        )}
      </section>

      <div className="button-row">
        <button
          type="button"
          className="button button-primary"
          disabled={resolved.selectedIds.length === 0}
          title={
            resolved.selectedIds.length === 0 ? "请先选择至少一个可迁移的仓库" : undefined
          }
          onClick={proceed}
        >
          继续设置映射（{resolved.selectedIds.length}）
        </button>
      </div>

      <Drawer open={showExcluded} title="排除项" onClose={() => setShowExcluded(false)}>
        {resolved.exclusions.length === 0 ? (
          <p className="caption">当前筛选结果中没有被排除的仓库。</p>
        ) : (
          <ul className="log-list">
            {resolved.exclusions.map((exclusion) => (
              <li className="log-entry" key={exclusion.id}>
                <strong>{exclusion.name}</strong>
                <span>{exclusion.reason}</span>
                <UrlCell url={exclusion.id} />
              </li>
            ))}
          </ul>
        )}
      </Drawer>
    </>
  );
}
