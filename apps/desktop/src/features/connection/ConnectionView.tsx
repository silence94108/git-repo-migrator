/**
 * Connection page: source and target panels plus the capability matrix.
 *
 * The panels never render a token input. Feedback comes from the backend
 * (`connection_test` / `connection_save`), including its timeout, so the ten
 * second feedback budget is enforced where the work happens rather than by a
 * renderer timer.
 */

import { useState } from "react";
import { HardDrive, KeyRound, Server } from "lucide-react";

import {
  Alert,
  Badge,
  ErrorAlert,
  Field,
  FidelityBadge,
  Spinner,
} from "../../components/primitives";
import type { MigrationState, MigrationStore } from "../../state/migrationStore";
import { connectionFor } from "../../state/migrationStore";
import type {
  CapabilitySummary,
  ConnectionRole,
  ConnectionSnapshot,
  IpcError,
  PlatformKind,
} from "../../state/ipcTypes";
import {
  AUTH_METHOD_OPTIONS,
  PLATFORM_OPTIONS,
  capabilityLabel,
  emptyForm,
  hasErrors,
  requiresManualImport,
  toSaveInput,
  validateForm,
} from "./connectionModel";
import type { AuthMethod, ConnectionForm } from "./connectionModel";

type PanelStatus = "idle" | "testing" | "saving" | "tested" | "saved";

function CapabilityMatrix({ capabilities }: { capabilities: CapabilitySummary[] }) {
  if (capabilities.length === 0) return null;
  return (
    <div className="table-scroll">
      <table>
        <caption className="visually-hidden">权限与能力摘要</caption>
        <thead>
          <tr>
            <th scope="col">能力</th>
            <th scope="col">状态</th>
            <th scope="col">保真度</th>
            <th scope="col">所需权限</th>
          </tr>
        </thead>
        <tbody>
          {capabilities.map((capability) => (
            <tr key={capability.module}>
              <td data-label="能力">{capabilityLabel(capability.module)}</td>
              <td data-label="状态">
                {capability.supported && capability.permitted ? (
                  <Badge tone="success" label="可用" />
                ) : (
                  <Badge
                    tone={capability.supported ? "warning" : "neutral"}
                    label={capability.supported ? "权限不足" : "不支持"}
                    title={capability.reason ?? undefined}
                  />
                )}
              </td>
              <td data-label="保真度">
                <FidelityBadge fidelity={capability.fidelity} reason={capability.reason} />
              </td>
              <td data-label="所需权限">
                {capability.required_scopes.length > 0
                  ? capability.required_scopes.join(", ")
                  : capability.degradation ?? "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ConnectionPanel({
  role,
  saved,
  onTest,
  onSave,
}: {
  role: ConnectionRole;
  saved: ConnectionSnapshot | null;
  onTest: (form: ConnectionForm) => Promise<CapabilitySummary[] | IpcError>;
  onSave: (form: ConnectionForm) => Promise<ConnectionSnapshot | IpcError>;
}) {
  const isSource = role === "source";
  const [form, setForm] = useState<ConnectionForm>(() => ({
    ...emptyForm(role),
    platform: (saved?.platform ?? "generic_git") as PlatformKind,
    endpoint: saved?.endpoint ?? "",
    credentialRef: saved?.credential_ref ?? "",
  }));
  const [status, setStatus] = useState<PanelStatus>(saved ? "saved" : "idle");
  const [touched, setTouched] = useState(false);
  const [error, setError] = useState<IpcError | null>(null);
  const [probed, setProbed] = useState<CapabilitySummary[]>(saved?.capabilities ?? []);

  const errors = validateForm(form);
  const showErrors = touched;
  const busy = status === "testing" || status === "saving";

  const patch = (next: Partial<ConnectionForm>) => {
    setForm((current) => ({ ...current, ...next }));
    setStatus("idle");
  };

  const run = async (
    kind: "test" | "save",
    action: () => Promise<CapabilitySummary[] | ConnectionSnapshot | IpcError>,
  ) => {
    setTouched(true);
    if (hasErrors(errors)) return;
    setStatus(kind === "test" ? "testing" : "saving");
    setError(null);
    const outcome = await action();
    if (Array.isArray(outcome)) {
      setProbed(outcome);
      setStatus("tested");
      return;
    }
    if ("capabilities" in outcome) {
      setProbed(outcome.capabilities);
      setStatus("saved");
      return;
    }
    setError(outcome);
    setStatus("idle");
  };

  const capabilities = probed.length > 0 ? probed : saved?.capabilities ?? [];

  return (
    <section className="panel" aria-labelledby={`${role}-heading`}>
      <div className="app-brand">
        {isSource ? <Server size={18} aria-hidden /> : <HardDrive size={18} aria-hidden />}
        <h2 id={`${role}-heading`}>{isSource ? "源平台" : "目标平台"}</h2>
      </div>
      <p className="caption">
        {isSource
          ? "先连接需要迁出的 Git 服务；只读取仓库，不修改源数据。"
          : "目标仓库默认不覆盖：非空目标会跳过，覆盖需要单独确认。"}
      </p>

      <Field label="平台类型">
        {({ inputId }) => (
          <select
            id={inputId}
            value={form.platform}
            disabled={busy}
            onChange={(event) => patch({ platform: event.target.value as PlatformKind })}
          >
            {PLATFORM_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        )}
      </Field>

      <Field
        label="服务地址"
        hint="自建平台请填写完整实例地址，例如 https://gitlab.internal.test"
        error={showErrors ? errors.endpoint : undefined}
      >
        {({ inputId, describedBy }) => (
          <input
            id={inputId}
            type="url"
            inputMode="url"
            value={form.endpoint}
            disabled={busy}
            aria-invalid={showErrors && Boolean(errors.endpoint)}
            aria-describedby={describedBy}
            placeholder="https://git.example.com"
            onChange={(event) => patch({ endpoint: event.target.value })}
            onBlur={() => setTouched(true)}
          />
        )}
      </Field>

      <fieldset className="field" style={{ border: 0, margin: 0, padding: 0 }}>
        <legend className="field-label">认证方式</legend>
        <div className="button-row" role="radiogroup" aria-label="认证方式">
          {AUTH_METHOD_OPTIONS.map((option) => (
            <label
              className="radio-row"
              key={option.value}
              data-selected={form.authMethod === option.value}
            >
              <input
                type="radio"
                name={`${role}-auth`}
                value={option.value}
                checked={form.authMethod === option.value}
                disabled={busy}
                onChange={() => patch({ authMethod: option.value as AuthMethod })}
              />
              <span>{option.label}</span>
            </label>
          ))}
        </div>
      </fieldset>

      <Field
        label="凭据引用（Windows 凭据管理器名称）"
        hint="界面不接收令牌。请先在 Windows 凭据管理器中保存令牌，再在此填写它的名称。"
        error={showErrors ? errors.credentialRef : undefined}
      >
        {({ inputId, describedBy }) => (
          <input
            id={inputId}
            type="text"
            autoComplete="off"
            value={form.credentialRef}
            disabled={busy}
            aria-invalid={showErrors && Boolean(errors.credentialRef)}
            aria-describedby={describedBy}
            placeholder="git-repo-migrator/source"
            onChange={(event) => patch({ credentialRef: event.target.value })}
            onBlur={() => setTouched(true)}
          />
        )}
      </Field>

      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={form.trustSelfSigned}
          disabled={busy}
          onChange={(event) => patch({ trustSelfSigned: event.target.checked })}
        />
        <span>
          该实例使用自签名证书，我要固定它的 SHA-256 指纹
          <span className="step-note">固定指纹仍然会校验证书，不等于跳过验证。</span>
        </span>
      </label>

      {form.trustSelfSigned ? (
        <Field
          label="证书 SHA-256 指纹"
          error={showErrors ? errors.fingerprint : undefined}
        >
          {({ inputId, describedBy }) => (
            <input
              id={inputId}
              type="text"
              className="mono"
              value={form.fingerprint}
              disabled={busy}
              aria-invalid={showErrors && Boolean(errors.fingerprint)}
              aria-describedby={describedBy}
              onChange={(event) => patch({ fingerprint: event.target.value })}
              onBlur={() => setTouched(true)}
            />
          )}
        </Field>
      ) : null}

      {requiresManualImport(form.platform) ? (
        <Alert
          tone="info"
          title="通用 Git 服务没有 API"
          action="下一步请使用「手动 URL 导入」；平台数据模块（Issues、PR、Wiki、Release）不可保证。"
        />
      ) : null}

      <div className="button-row">
        <button
          type="button"
          className="button button-secondary"
          disabled={busy}
          aria-busy={busy}
          onClick={() => void run("test", () => onTest(form))}
        >
          {status === "testing" ? <Spinner label="测试中" /> : "测试连接"}
        </button>
        <button
          type="button"
          className="button button-primary"
          disabled={busy}
          aria-busy={busy}
          onClick={() => void run("save", () => onSave(form))}
        >
          {status === "saving" ? <Spinner label="保存中" /> : "保存连接"}
        </button>
      </div>

      {error ? <ErrorAlert error={error} /> : null}

      {saved ? (
        <dl className="definition-list">
          <dt>连接身份</dt>
          <dd>{saved.account_name ?? "未提供账号名"}</dd>
          <dt>实例版本</dt>
          <dd>{saved.instance_version ?? "未知"}</dd>
          <dt>凭据引用</dt>
          {/* Only the reference is ever displayed; there is no token to reveal. */}
          <dd className="mono">
            <KeyRound size={12} aria-hidden /> {saved.credential_ref ?? "未绑定"}
          </dd>
          <dt>TLS</dt>
          <dd>{saved.tls_trusted ? "已校验" : "未校验"}</dd>
        </dl>
      ) : null}

      {status === "tested" ? (
        <Alert tone="success" title="连接测试完成，下面是探测到的能力" />
      ) : null}

      <CapabilityMatrix capabilities={capabilities} />
    </section>
  );
}

function panelKey(role: ConnectionRole, saved: ConnectionSnapshot | null): string {
  return [role, saved?.platform ?? "", saved?.endpoint ?? "", saved?.credential_ref ?? ""].join(
    "|",
  );
}

export function ConnectionView({
  store,
  state,
}: {
  store: MigrationStore;
  state: MigrationState;
}) {  const source = connectionFor(state.snapshot, "source");
  const target = connectionFor(state.snapshot, "target");

  const test = async (role: ConnectionRole, form: ConnectionForm) => {
    const input = toSaveInput(role, form);
    const result = await store.testConnection({
      endpoint: input.endpoint,
      platform_hint: input.platform_hint,
      credential_ref: input.credential_ref,
    });
    return result.ok ? result.value : result.error;
  };

  const save = async (role: ConnectionRole, form: ConnectionForm) => {
    const result = await store.saveConnection(toSaveInput(role, form));
    return result.ok ? result.value : result.error;
  };

  return (
    <>
      <Alert
        tone="info"
        title="数据仅在本机处理"
        action="仓库内容、令牌和临时目录都不会离开本机；日志与导出自动脱敏。"
      />
      <div className="two-column">
        <ConnectionPanel
          // Re-keying on the persisted values makes the form adopt the saved
          // connection once the first snapshot arrives, and after every save.
          key={panelKey("source", source)}
          role="source"
          saved={source}
          onTest={(form) => test("source", form)}
          onSave={(form) => save("source", form)}
        />
        <ConnectionPanel
          key={panelKey("target", target)}
          role="target"
          saved={target}
          onTest={(form) => test("target", form)}
          onSave={(form) => save("target", form)}
        />
      </div>
      {source && target ? (
        <Alert
          tone="success"
          title="源与目标均已保存，可以进入下一步选择仓库"
          action="左侧步骤导航中的「选择仓库」已解锁。"
        />
      ) : (
        <Alert
          tone="info"
          title="请先保存源平台和目标平台连接"
          action="两个连接都保存后，「选择仓库」步骤会自动解锁。"
        />
      )}
    </>
  );
}
