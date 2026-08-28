/**
 * Renderer-local plan draft.
 *
 * This is the only state the renderer owns, and it is deliberately just the
 * operator's *intent*: which repositories, which policy, which modules. Nothing
 * here is authoritative — every value is re-validated by the backend when
 * `plan_preview` and `plan_freeze` run, and the frozen plan lives in SQLite.
 */

export interface ModuleDraft {
  lfs: boolean;
  metadata: boolean;
  issues: boolean;
  pullRequests: boolean;
  wiki: boolean;
  releases: boolean;
}

export interface PlanDraft {
  selectedRepositoryIds: string[];
  excludedRepositoryIds: string[];
  targetNamespace: string;
  /** Supports `{name}`, `{namespace}` and `{visibility}`. */
  nameTemplate: string;
  reuseEmpty: boolean;
  skipNonEmpty: boolean;
  autoRename: boolean;
  allowOverwrite: boolean;
  includeArchivedRefs: boolean;
  modules: ModuleDraft;
  acknowledgedFidelity: string[];
  concurrency: number;
  workspacePolicy: "reuse" | "clean";
}

export const emptyDraft: PlanDraft = {
  selectedRepositoryIds: [],
  excludedRepositoryIds: [],
  targetNamespace: "",
  nameTemplate: "{name}",
  // Defaults are the non-destructive ones from the PRD: reuse an empty target,
  // skip a non-empty one, never overwrite.
  reuseEmpty: true,
  skipNonEmpty: true,
  autoRename: true,
  allowOverwrite: false,
  includeArchivedRefs: false,
  modules: {
    lfs: true,
    metadata: true,
    issues: false,
    pullRequests: false,
    wiki: false,
    releases: false,
  },
  acknowledgedFidelity: [],
  concurrency: 2,
  workspacePolicy: "reuse",
};

export function applyNameTemplate(
  template: string,
  values: { name: string; namespace: string; visibility: string },
): string {
  return template
    .replace(/\{name\}/g, values.name)
    .replace(/\{namespace\}/g, values.namespace)
    .replace(/\{visibility\}/g, values.visibility)
    .trim();
}

/** Platform repository names: letters, digits, dot, dash, underscore. */
export function validateTargetName(name: string): string | undefined {
  if (!name) return "目标名称不能为空";
  if (name.length > 100) return "目标名称超过 100 个字符，多数平台会拒绝";
  if (!/^[A-Za-z0-9._-]+$/.test(name)) {
    return "目标名称只能包含字母、数字、点、短横线和下划线";
  }
  return undefined;
}

export function buildTargetUrl(
  endpoint: string,
  namespace: string,
  name: string,
): string {
  const base = endpoint.replace(/\/+$/, "");
  const scope = namespace.trim().replace(/^\/+|\/+$/g, "");
  return scope ? `${base}/${scope}/${name}` : `${base}/${name}`;
}
