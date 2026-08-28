/**
 * Mapping and strategy step.
 *
 * Everything dangerous is off by default and stays off until the operator turns
 * it on explicitly: overwrite is a switch that expands a warning, and archived
 * refs are opt-in. The renderer only records intent — the backend re-derives the
 * action for every repository when the preview runs.
 */

import { useMemo, useState } from "react";
import { ArrowRight, ShieldAlert } from "lucide-react";

import {
  Alert,
  Badge,
  EmptyState,
  FidelityBadge,
  Spinner,
  TargetStateBadge,
  UrlCell,
} from "../../components/primitives";
import type { MigrationState, MigrationStore } from "../../state/migrationStore";
import { connectionFor } from "../../state/migrationStore";
import { applyNameTemplate, buildTargetUrl, validateTargetName } from "../../state/planDraft";
import type { ModuleDraft } from "../../state/planDraft";
import type { CapabilitySummary, Fidelity, RepositoryMappingInput } from "../../state/ipcTypes";

type StrategyId = "reuse_empty_skip_non_empty" | "auto_rename" | "continue_sync" | "overwrite";

interface Strategy {
  id: StrategyId;
  label: string;
  risk: string;
  available: boolean;
  unavailableReason?: string;
}

const STRATEGIES: Strategy[] = [
  {
    id: "reuse_empty_skip_non_empty",
    label: "空仓库复用，非空跳过（默认）",
    risk: "风险最低：不会覆盖目标已有内容",
    available: true,
  },
  {
    id: "auto_rename",
    label: "冲突时自动改名",
    risk: "中等：目标名称可能与源不一致",
    available: true,
  },
  {
    id: "continue_sync",
    label: "继续同步到非空目标",
    risk: "需要目标 refs 差异比对",
    available: false,
    unavailableReason: "本版本尚未实现增量同步；请选择跳过、改名或显式覆盖",
  },
  {
    id: "overwrite",
    label: "覆盖迁移",
    risk: "最高：会替换目标已有的分支和 Tag",
    available: true,
  },
];

const OPTIONAL_MODULES: Array<{ key: keyof ModuleDraft; module: string; label: string }> = [
  { key: "lfs", module: "lfs", label: "Git LFS 对象" },
  { key: "metadata", module: "metadata", label: "基础元数据（简介、主页、Topics）" },
  { key: "issues", module: "issues", label: "Issues" },
  { key: "pullRequests", module: "pull_requests", label: "Pull Request / Merge Request" },
  { key: "wiki", module: "wiki", label: "Wiki" },
  { key: "releases", module: "releases", label: "Release 与附件" },
];

function strategyFor(draft: {
  autoRename: boolean;
  allowOverwrite: boolean;
  skipNonEmpty: boolean;
}): StrategyId {
  if (draft.allowOverwrite) return "overwrite";
  if (draft.autoRename && !draft.skipNonEmpty) return "auto_rename";
  return "reuse_empty_skip_non_empty";
}

function fidelityOf(capabilities: CapabilitySummary[], module: string): Fidelity {
  const capability = capabilities.find((item) => item.module === module);
  if (!capability) return "unsupported";
  return capability.supported && capability.permitted ? capability.fidelity : "unsupported";
}

function reasonOf(capabilities: CapabilitySummary[], module: string): string | null {
  return capabilities.find((item) => item.module === module)?.reason ?? null;
}

export function MappingView({
  store,
  state,
  onPreflight,
}: {
  store: MigrationStore;
  state: MigrationState;
  onPreflight: () => void;
}) {
  const draft = state.draft;
  const target = connectionFor(state.snapshot, "target");
  const capabilities = target?.capabilities ?? [];
  const [busy, setBusy] = useState(false);
  const [overwriteAcknowledged, setOverwriteAcknowledged] = useState(false);

  const selected = useMemo(
    () =>
      (state.snapshot?.repositories ?? []).filter((repository) =>
        draft.selectedRepositoryIds.includes(repository.id),
      ),
    [state.snapshot, draft.selectedRepositoryIds],
  );

  const mappings: Array<RepositoryMappingInput & { name: string; error?: string }> = useMemo(
    () =>
      selected.map((repository) => {
        const name = applyNameTemplate(draft.nameTemplate, {
          name: repository.name,
          namespace: repository.namespace,
          visibility: repository.visibility,
        });
        return {
          repository_id: repository.id,
          target_url: buildTargetUrl(
            target?.endpoint ?? "https://target.invalid",
            draft.targetNamespace,
            name,
          ),
          target_name: name,
          name: repository.name,
          error: validateTargetName(name),
        };
      }),
    [selected, draft.nameTemplate, draft.targetNamespace, target],
  );

  const invalid = mappings.filter((mapping) => mapping.error);
  const strategy = strategyFor(draft);
  const degraded = OPTIONAL_MODULES.filter(
    (module) =>
      draft.modules[module.key] && fidelityOf(capabilities, module.module) !== "native_rebuild",
  );
  const blockedByConfirmation = strategy === "overwrite" && !overwriteAcknowledged;

  const applyStrategy = (id: StrategyId) => {
    switch (id) {
      case "overwrite":
        store.updateDraft({ reuseEmpty: true, skipNonEmpty: false, autoRename: false, allowOverwrite: true });
        break;
      case "auto_rename":
        store.updateDraft({ reuseEmpty: true, skipNonEmpty: false, autoRename: true, allowOverwrite: false });
        setOverwriteAcknowledged(false);
        break;
      default:
        store.updateDraft({ reuseEmpty: true, skipNonEmpty: true, autoRename: true, allowOverwrite: false });
        setOverwriteAcknowledged(false);
        break;
    }
  };

  const runPreflight = async () => {
    setBusy(true);
    // Persist each mapping first: `batch_start` reads the target from the
    // repository row, so a preview-only mapping would not be executable.
    for (const mapping of mappings) {
      const result = await store.setMapping({
        repository_id: mapping.repository_id,
        target_url: mapping.target_url,
        target_name: mapping.target_name,
      });
      if (!result.ok) {
        setBusy(false);
        return;
      }
    }
    const preview = await store.previewPlan({
      selected_repository_ids: draft.selectedRepositoryIds,
      excluded_repository_ids: draft.excludedRepositoryIds,
      mappings: mappings.map((mapping) => ({
        repository_id: mapping.repository_id,
        target_url: mapping.target_url,
        target_name: mapping.target_name,
      })),
      reuse_empty: draft.reuseEmpty,
      skip_non_empty: draft.skipNonEmpty,
      auto_rename: draft.autoRename,
      allow_overwrite: draft.allowOverwrite,
      include_archived_refs: draft.includeArchivedRefs,
      module_lfs: draft.modules.lfs,
      module_metadata: draft.modules.metadata,
      module_issues: draft.modules.issues,
      module_pull_requests: draft.modules.pullRequests,
      module_wiki: draft.modules.wiki,
      module_releases: draft.modules.releases,
    });
    setBusy(false);
    if (preview.ok) onPreflight();
  };

  if (selected.length === 0) {
    return (
      <EmptyState
        title="尚未选择任何仓库"
        description="请返回「选择仓库」步骤，筛选并选择要迁移的仓库。"
      />
    );
  }

  return (
    <>
      <section className="panel" aria-label="目标命名">
        <h2>目标位置与命名</h2>
        <div className="two-column">
          <label className="field">
            <span className="field-label">目标组织 / 命名空间</span>
            <input
              type="text"
              value={draft.targetNamespace}
              placeholder="ops"
              onChange={(event) => store.updateDraft({ targetNamespace: event.target.value })}
            />
          </label>
          <label className="field">
            <span className="field-label">仓库名模板</span>
            <input
              type="text"
              value={draft.nameTemplate}
              onChange={(event) => store.updateDraft({ nameTemplate: event.target.value })}
            />
          </label>
        </div>
        <p className="caption">可用变量：{"{name}"}、{"{namespace}"}、{"{visibility}"}</p>
        <ul className="log-list" aria-label="命名预览">
          {mappings.slice(0, 3).map((mapping) => (
            <li className="log-entry" key={mapping.repository_id}>
              <span>
                {mapping.name} → <strong>{mapping.target_name}</strong>
              </span>
              <UrlCell url={mapping.target_url} />
              {mapping.error ? <span className="field-error">{mapping.error}</span> : null}
            </li>
          ))}
        </ul>
        {invalid.length > 0 ? (
          <Alert
            tone="error"
            title={`${invalid.length} 个目标名称不符合平台命名规则`}
            action="请修改命名模板；平台会拒绝这些名称。"
          />
        ) : null}
      </section>

      <section className="panel" aria-label="迁移模块">
        <h2>迁移模块</h2>
        <label className="checkbox-row">
          <input type="checkbox" checked disabled aria-label="Git 历史、分支与 Tag" />
          <span>
            Git 历史、分支与 Tag<span className="step-note">核心数据，始终迁移，不可取消。</span>
          </span>
        </label>
        {OPTIONAL_MODULES.map((module) => {
          const fidelity = fidelityOf(capabilities, module.module);
          return (
            <label className="checkbox-row" key={module.key}>
              <input
                type="checkbox"
                checked={draft.modules[module.key]}
                aria-label={module.label}
                onChange={(event) =>
                  store.updateDraft({
                    modules: { ...draft.modules, [module.key]: event.target.checked },
                  })
                }
              />
              <span>
                {module.label}{" "}
                <FidelityBadge
                  fidelity={fidelity}
                  reason={reasonOf(capabilities, module.module)}
                />
                <span className="step-note">
                  {fidelity === "native_rebuild"
                    ? "目标支持原生重建，逐项校验。"
                    : fidelity === "read_only_archive"
                      ? "目标不支持写入：只在本地生成只读归档，不会呈现为可交互条目。"
                      : (reasonOf(capabilities, module.module) ??
                        "目标不支持该模块，将标记为未迁移。")}
                </span>
              </span>
            </label>
          );
        })}
        {degraded.length > 0 ? (
          <Alert
            tone="warning"
            title={`${degraded.length} 个模块只能归档或不支持迁移`}
            action="预检页会要求逐项确认后才允许冻结计划。"
          />
        ) : null}
      </section>

      <section className="panel" aria-label="引用策略">
        <h2>引用（refs）策略</h2>
        <p className="caption">
          默认只迁移 <code>refs/heads/*</code> 与 <code>refs/tags/*</code>；
          平台私有 refs（Pull Request、Merge Request、Gerrit changes）和远程跟踪 refs 不会写入目标。
        </p>
        <label className="checkbox-row">
          <input
            type="checkbox"
            checked={draft.includeArchivedRefs}
            aria-label="把平台私有 refs 归档到本地报告"
            onChange={(event) => store.updateDraft({ includeArchivedRefs: event.target.checked })}
          />
          <span>
            把平台私有 refs 归档到本地报告
            <span className="step-note">仅本地归档，仍然不会推送到目标。</span>
          </span>
        </label>
      </section>

      <section className="panel" aria-label="冲突策略">
        <h2>目标冲突策略</h2>
        <div role="radiogroup" aria-label="目标冲突策略">
          {STRATEGIES.map((option) => (
            <label
              className="radio-row"
              key={option.id}
              data-selected={strategy === option.id}
              title={option.unavailableReason}
            >
              <input
                type="radio"
                name="conflict-strategy"
                value={option.id}
                checked={strategy === option.id}
                disabled={!option.available}
                onChange={() => applyStrategy(option.id)}
              />
              <span>
                {option.label}
                <span className="step-note">
                  {option.available ? option.risk : option.unavailableReason}
                </span>
              </span>
            </label>
          ))}
        </div>

        {strategy === "overwrite" ? (
          <div className="danger-switch">
            <p className="alert-title">
              <ShieldAlert size={15} aria-hidden /> 覆盖迁移会替换目标已有的分支和 Tag
            </p>
            <p>
              影响范围：{mappings.length} 个目标仓库。已完成的覆盖无法自动回滚，
              请先在目标平台建立备份。
            </p>
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={overwriteAcknowledged}
                aria-label="我已了解覆盖迁移的影响并准备了备份"
                onChange={(event) => setOverwriteAcknowledged(event.target.checked)}
              />
              <span>我已了解覆盖迁移的影响并准备了备份</span>
            </label>
            <p className="caption">
              预检页仍会要求输入目标仓库名再次确认；确认由后端校验，界面无法跳过。
            </p>
          </div>
        ) : null}
      </section>

      <section className="panel" aria-label="映射表">
        <h2>映射预览（{mappings.length}）</h2>
        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th scope="col">源仓库</th>
                <th scope="col">目标地址</th>
                <th scope="col">目标名称</th>
                <th scope="col">目标状态</th>
                <th scope="col" className="col-secondary">
                  平台数据
                </th>
              </tr>
            </thead>
            <tbody>
              {mappings.slice(0, 100).map((mapping, index) => {
                const repository = selected[index];
                return (
                  <tr key={mapping.repository_id}>
                    <td data-label="源仓库">
                      <UrlCell url={repository.source_url} />
                    </td>
                    <td data-label="目标地址">
                      <UrlCell url={mapping.target_url} />
                    </td>
                    <td data-label="目标名称">{mapping.target_name}</td>
                    <td data-label="目标状态">
                      <TargetStateBadge state={repository.target_state} />
                    </td>
                    <td data-label="平台数据" className="col-secondary">
                      <Badge
                        tone={repository.platform_capable ? "success" : "neutral"}
                        label={repository.platform_capable ? "可迁移" : "不支持"}
                      />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </section>

      <div className="button-row">
        <button
          type="button"
          className="button button-primary"
          disabled={busy || invalid.length > 0 || blockedByConfirmation}
          aria-busy={busy}
          title={
            blockedByConfirmation
              ? "请先确认覆盖迁移的影响范围"
              : invalid.length > 0
                ? "请先修正不合法的目标名称"
                : undefined
          }
          onClick={() => void runPreflight()}
        >
          {busy ? <Spinner label="生成预检" /> : "保存映射并运行预检"}
          <ArrowRight size={15} aria-hidden />
        </button>
      </div>
    </>
  );
}
