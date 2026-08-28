/**
 * Connection form model.
 *
 * Pure functions only, so the validation rules that mirror the backend can be
 * tested directly. The single hard rule: **no secret ever enters this form.** The
 * operator supplies a Windows Credential Manager *reference*; the token itself is
 * stored outside the renderer and never crosses the IPC boundary.
 */

import type { ConnectionSaveInput, ConnectionRole, PlatformKind } from "../../state/ipcTypes";

export type AuthMethod = "access_token" | "ssh_key" | "https_credential";

export interface ConnectionForm {
  platform: PlatformKind;
  endpoint: string;
  authMethod: AuthMethod;
  credentialRef: string;
  trustSelfSigned: boolean;
  fingerprint: string;
}

export interface ConnectionErrors {
  endpoint?: string;
  credentialRef?: string;
  fingerprint?: string;
}

export const PLATFORM_OPTIONS: ReadonlyArray<{ value: PlatformKind; label: string }> = [
  { value: "github", label: "GitHub" },
  { value: "gitlab", label: "GitLab / Self-Managed" },
  { value: "gitee", label: "Gitee" },
  { value: "gitea", label: "Gitea" },
  { value: "forgejo", label: "Forgejo" },
  { value: "generic_git", label: "通用 Git 服务" },
];

export const AUTH_METHOD_OPTIONS: ReadonlyArray<{ value: AuthMethod; label: string }> = [
  { value: "access_token", label: "Access Token" },
  { value: "ssh_key", label: "SSH Key" },
  { value: "https_credential", label: "HTTPS 凭据" },
];

/** Markers that mean the operator pasted a real secret instead of a reference. */
const SECRET_MARKERS = [
  "ghp_",
  "github_pat_",
  "glpat-",
  "gho_",
  "bearer ",
  "token=",
  "password=",
  "-----begin",
];

export function emptyForm(role: ConnectionRole): ConnectionForm {
  return {
    // The generic adapter needs no API, so it is the safe default for an
    // unknown host: it degrades to manual URL import instead of failing.
    platform: "generic_git",
    endpoint: "",
    authMethod: role === "source" ? "access_token" : "access_token",
    credentialRef: "",
    trustSelfSigned: false,
    fingerprint: "",
  };
}

export function validateEndpoint(value: string): string | undefined {
  const endpoint = value.trim();
  if (!endpoint) return "请输入服务地址";
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch {
    return "地址格式不正确，例如 https://gitlab.example.com";
  }
  if (!["https:", "http:", "ssh:"].includes(url.protocol)) {
    return "仅支持 HTTPS、HTTP 或 SSH 地址";
  }
  if (!url.hostname) return "地址缺少主机名";
  if (url.username || url.password) {
    return "地址不得包含用户名或密码；令牌只保存在 Windows 凭据管理器";
  }
  return undefined;
}

export function validateCredentialRef(value: string, required: boolean): string | undefined {
  const reference = value.trim();
  if (!reference) {
    return required ? "请填写 Windows 凭据管理器中的凭据名称" : undefined;
  }
  if (reference.length > 256) return "凭据引用过长";
  const lowered = reference.toLowerCase();
  if (SECRET_MARKERS.some((marker) => lowered.includes(marker))) {
    return "这看起来是一个明文令牌。请只填写凭据名称，令牌需保存在 Windows 凭据管理器";
  }
  return undefined;
}

export function validateFingerprint(
  value: string,
  enabled: boolean,
): string | undefined {
  if (!enabled) return undefined;
  const normalized = value.replace(/:/g, "").trim();
  if (!/^[0-9a-fA-F]{64}$/.test(normalized)) {
    return "请填写完整的 64 位十六进制 SHA-256 指纹";
  }
  return undefined;
}

export function validateForm(form: ConnectionForm): ConnectionErrors {
  const errors: ConnectionErrors = {};
  const endpoint = validateEndpoint(form.endpoint);
  if (endpoint) errors.endpoint = endpoint;
  // SSH key auth is resolved by the SSH agent, so a reference is optional there.
  const credentialRef = validateCredentialRef(
    form.credentialRef,
    form.authMethod !== "ssh_key",
  );
  if (credentialRef) errors.credentialRef = credentialRef;
  const fingerprint = validateFingerprint(form.fingerprint, form.trustSelfSigned);
  if (fingerprint) errors.fingerprint = fingerprint;
  return errors;
}

export function hasErrors(errors: ConnectionErrors): boolean {
  return Object.values(errors).some(Boolean);
}

/**
 * Maps the form to the command payload. Note what is absent: there is no token,
 * password or key field to map, by construction.
 */
export function toSaveInput(role: ConnectionRole, form: ConnectionForm): ConnectionSaveInput {
  const reference = form.credentialRef.trim();
  return {
    role,
    endpoint: form.endpoint.trim(),
    platform_hint: form.platform,
    credential_ref: reference ? reference : null,
    trust_fingerprint_sha256: form.trustSelfSigned
      ? form.fingerprint.replace(/:/g, "").trim()
      : null,
  };
}

/** Generic Git has no discovery API; the UI must say so before the user tries. */
export function requiresManualImport(platform: PlatformKind): boolean {
  return platform === "generic_git";
}

export const CAPABILITY_LABELS: Record<string, string> = {
  discovery: "可发现仓库",
  repository_inspection: "可读取仓库状态",
  repository_creation: "可创建目标仓库",
  git_read: "可读取 Git 数据",
  git_write: "可写入 Git 数据",
  lfs: "可迁移 Git LFS",
  metadata: "可写入基础元数据",
  issues: "可迁移 Issues",
  pull_requests: "可迁移 Pull Request",
  merge_requests: "可迁移 Merge Request",
  wiki: "可迁移 Wiki",
  releases: "可迁移 Release",
  release_assets: "可迁移 Release 附件",
};

export function capabilityLabel(module: string): string {
  return CAPABILITY_LABELS[module] ?? module;
}
