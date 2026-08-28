/**
 * Selection model for the repository step.
 *
 * The rule this file exists to enforce: **"select all" means the whole filtered
 * result set, not the rows currently rendered.** Selection is therefore stored as
 * a filter snapshot plus exclusion rules, and the count is always derived from the
 * full set. Windowing the table can never change what is selected.
 */

import type { PermissionLevel, RepositorySnapshot, RepositoryVisibility } from "../../state/ipcTypes";

/** How many rows are rendered at once; the rest stay out of the DOM. */
export const ROW_WINDOW = 100;

export interface Filters {
  search: string;
  namespace: string;
  visibility: RepositoryVisibility | "any";
  permission: PermissionLevel | "any";
  updatedWithinDays: number | null;
  onlyFullMigration: boolean;
}

export const emptyFilters: Filters = {
  search: "",
  namespace: "",
  visibility: "any",
  permission: "any",
  updatedWithinDays: null,
  onlyFullMigration: false,
};

export type ExclusionKind = "name_glob" | "namespace" | "repository";

export interface ExclusionRule {
  id: string;
  kind: ExclusionKind;
  pattern: string;
  enabled: boolean;
}

export interface SelectionState {
  /** True once the operator chose "select all filtered results". */
  selectAll: boolean;
  /** Individually ticked ids, used only when `selectAll` is false. */
  included: string[];
  /** Individually unticked ids, used only when `selectAll` is true. */
  excluded: string[];
  rules: ExclusionRule[];
}

export const emptySelection: SelectionState = {
  selectAll: false,
  included: [],
  excluded: [],
  rules: [],
};

export interface FilterChip {
  key: keyof Filters;
  label: string;
}

export function activeChips(filters: Filters): FilterChip[] {
  const chips: FilterChip[] = [];
  if (filters.search.trim()) chips.push({ key: "search", label: `名称含「${filters.search.trim()}」` });
  if (filters.namespace.trim())
    chips.push({ key: "namespace", label: `组织：${filters.namespace.trim()}` });
  if (filters.visibility !== "any")
    chips.push({ key: "visibility", label: `可见性：${filters.visibility}` });
  if (filters.permission !== "any")
    chips.push({ key: "permission", label: `权限：${filters.permission}` });
  if (filters.updatedWithinDays !== null)
    chips.push({ key: "updatedWithinDays", label: `${filters.updatedWithinDays} 天内更新` });
  if (filters.onlyFullMigration)
    chips.push({ key: "onlyFullMigration", label: "只显示可完整迁移" });
  return chips;
}

export function clearChip(filters: Filters, key: keyof Filters): Filters {
  return { ...filters, [key]: emptyFilters[key] } as Filters;
}

function globToRegExp(pattern: string): RegExp {
  const escaped = pattern.replace(/[.+^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`^${escaped.replace(/\*/g, ".*").replace(/\?/g, ".")}$`, "i");
}

export function applyFilters(
  repositories: RepositorySnapshot[],
  filters: Filters,
  nowSeconds: number,
): RepositorySnapshot[] {
  const search = filters.search.trim().toLowerCase();
  const namespace = filters.namespace.trim().toLowerCase();
  return repositories.filter((repository) => {
    if (search && !repository.name.toLowerCase().includes(search)) return false;
    if (namespace && repository.namespace.toLowerCase() !== namespace) return false;
    if (filters.visibility !== "any" && repository.visibility !== filters.visibility) return false;
    if (filters.permission !== "any" && repository.permission !== filters.permission) return false;
    if (filters.onlyFullMigration && repository.permission !== "full_migration") return false;
    if (filters.updatedWithinDays !== null) {
      const updated = repository.updated_at_epoch_seconds;
      if (updated === null) return false;
      if (nowSeconds - updated > filters.updatedWithinDays * 86_400) return false;
    }
    return true;
  });
}

export function matchesRule(rule: ExclusionRule, repository: RepositorySnapshot): boolean {
  if (!rule.enabled || !rule.pattern.trim()) return false;
  switch (rule.kind) {
    case "name_glob":
      return globToRegExp(rule.pattern.trim()).test(repository.name);
    case "namespace":
      return repository.namespace.toLowerCase() === rule.pattern.trim().toLowerCase();
    case "repository":
      return repository.id === rule.pattern.trim() || repository.source_url === rule.pattern.trim();
    default:
      return false;
  }
}

export interface ResolvedSelection {
  /** Ids that will be planned. Always derived from the whole filtered set. */
  selectedIds: string[];
  /** Ids inside the filtered set that are deliberately left out, with a reason. */
  exclusions: Array<{ id: string; name: string; reason: string }>;
  matchingCount: number;
  fullMigrationCount: number;
  gitOnlyCount: number;
  blockedCount: number;
}

export function resolveSelection(
  filtered: RepositorySnapshot[],
  selection: SelectionState,
): ResolvedSelection {
  const included = new Set(selection.included);
  const excluded = new Set(selection.excluded);
  const selectedIds: string[] = [];
  const exclusions: ResolvedSelection["exclusions"] = [];
  let fullMigration = 0;
  let gitOnly = 0;
  let blocked = 0;

  for (const repository of filtered) {
    // Insufficient permission is never selectable, whatever the rules say.
    if (!repository.selectable) {
      blocked += 1;
      exclusions.push({
        id: repository.id,
        name: repository.name,
        reason: repository.unselectable_reason ?? "权限不足，无法选择",
      });
      continue;
    }
    const rule = selection.rules.find((candidate) => matchesRule(candidate, repository));
    if (rule) {
      exclusions.push({
        id: repository.id,
        name: repository.name,
        reason: `排除规则：${describeRule(rule)}`,
      });
      continue;
    }
    const chosen = selection.selectAll ? !excluded.has(repository.id) : included.has(repository.id);
    if (!chosen) {
      if (selection.selectAll) {
        exclusions.push({ id: repository.id, name: repository.name, reason: "手动排除" });
      }
      continue;
    }
    selectedIds.push(repository.id);
    if (repository.permission === "full_migration" && repository.platform_capable) {
      fullMigration += 1;
    } else {
      gitOnly += 1;
    }
  }

  return {
    selectedIds,
    exclusions,
    matchingCount: filtered.length,
    fullMigrationCount: fullMigration,
    gitOnlyCount: gitOnly,
    blockedCount: blocked,
  };
}

export function describeRule(rule: ExclusionRule): string {
  switch (rule.kind) {
    case "name_glob":
      return `名称匹配 ${rule.pattern}`;
    case "namespace":
      return `组织为 ${rule.pattern}`;
    case "repository":
      return `指定仓库 ${rule.pattern}`;
    default:
      return rule.pattern;
  }
}

export function isSelected(selection: SelectionState, id: string): boolean {
  return selection.selectAll ? !selection.excluded.includes(id) : selection.included.includes(id);
}

export function toggle(selection: SelectionState, id: string): SelectionState {
  if (selection.selectAll) {
    const excluded = selection.excluded.includes(id)
      ? selection.excluded.filter((value) => value !== id)
      : [...selection.excluded, id];
    return { ...selection, excluded };
  }
  const included = selection.included.includes(id)
    ? selection.included.filter((value) => value !== id)
    : [...selection.included, id];
  return { ...selection, included };
}

/** Switches to whole-result-set selection and drops per-row ticks. */
export function selectAllFiltered(selection: SelectionState): SelectionState {
  return { ...selection, selectAll: true, included: [], excluded: [] };
}

export function clearSelection(selection: SelectionState): SelectionState {
  return { ...selection, selectAll: false, included: [], excluded: [] };
}

/**
 * Rows actually put in the DOM. The window bounds rendering only — never the
 * selection, whose count comes from `resolveSelection`.
 */
export function windowRows<T>(rows: T[], visible: number): { rows: T[]; hidden: number } {
  if (rows.length <= visible) return { rows, hidden: 0 };
  return { rows: rows.slice(0, visible), hidden: rows.length - visible };
}

export function formatUpdated(epochSeconds: number | null): string {
  if (epochSeconds === null) return "未知";
  return new Date(epochSeconds * 1000).toISOString().slice(0, 10);
}
