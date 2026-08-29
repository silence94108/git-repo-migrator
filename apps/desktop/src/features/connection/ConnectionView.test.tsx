import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ConnectionView } from "./ConnectionView";
import { MigrationStore, useMigrationState } from "../../state/migrationStore";
import {
  FakeBridge,
  connectedSnapshot,
  connection,
  ipcError,
  snapshot,
} from "../../state/testBridge";
import {
  toSaveInput,
  validateCredentialRef,
  validateEndpoint,
  validateFingerprint,
} from "./connectionModel";

function Harness({ store }: { store: MigrationStore }) {
  const state = useMigrationState(store);
  return <ConnectionView store={store} state={state} />;
}

async function mount(bridge: FakeBridge) {
  const store = new MigrationStore(bridge);
  render(<Harness store={store} />);
  await act(async () => {
    await store.refresh();
  });
  return store;
}

function sourcePanel() {
  return within(screen.getByRole("region", { name: "源平台" }));
}

function credentialRefInput(): HTMLInputElement {
  return sourcePanel().getByLabelText(/凭据引用/) as HTMLInputElement;
}

describe("连接页表单模型", () => {
  it("拒绝带有内联凭据或不支持协议的地址", () => {
    expect(validateEndpoint("https://user:pass@git.example.test")).toContain("不得包含");
    expect(validateEndpoint("ftp://git.example.test")).toContain("仅支持");
    expect(validateEndpoint("not a url")).toContain("格式不正确");
    expect(validateEndpoint("")).toBe("请输入服务地址");
    expect(validateEndpoint("https://git.example.test")).toBeUndefined();
  });

  it("把明文令牌识别为错误而不是凭据引用", () => {
    for (const secret of [
      "ghp_0123456789abcdefghij",
      "glpat-abcdefghij",
      "Bearer abc",
      "-----BEGIN OPENSSH PRIVATE KEY-----",
    ]) {
      expect(validateCredentialRef(secret, true)).toContain("明文令牌");
    }
    expect(validateCredentialRef("git-repo-migrator/source", true)).toBeUndefined();
  });

  it("SSH 认证下凭据引用可选，其他方式必填", () => {
    expect(validateCredentialRef("", true)).toContain("请填写");
    expect(validateCredentialRef("", false)).toBeUndefined();
  });

  it("固定自签名指纹要求完整 SHA-256", () => {
    expect(validateFingerprint("aa:bb", true)).toContain("64");
    expect(validateFingerprint("a".repeat(64), true)).toBeUndefined();
    expect(validateFingerprint("", false)).toBeUndefined();
  });

  it("命令载荷里没有任何 secret 字段", () => {
    const input = toSaveInput("source", {
      platform: "github",
      endpoint: " https://github.com ",
      authMethod: "access_token",
      credentialRef: " git-repo-migrator/source ",
      trustSelfSigned: false,
      fingerprint: "",
    });
    expect(Object.keys(input).sort()).toEqual([
      "credential_ref",
      "endpoint",
      "platform_hint",
      "role",
      "trust_fingerprint_sha256",
    ]);
    expect(input.endpoint).toBe("https://github.com");
    expect(input.credential_ref).toBe("git-repo-migrator/source");
  });
});

describe("连接页交互", () => {
  it("不渲染任何令牌输入框", async () => {
    await mount(new FakeBridge(snapshot()));
    expect(document.querySelectorAll('input[type="password"]')).toHaveLength(0);
    // "Access Token" is an auth-method label, not a secret field; what must not
    // exist is an input asking for the secret itself.
    for (const label of [/令牌/, /密码/, /私钥/]) {
      expect(screen.queryByLabelText(label)).toBeNull();
    }
    expect(sourcePanel().getByLabelText(/凭据引用/)).toBeTruthy();
  });

  it("地址无效时就地报错，且不调用后端", async () => {
    const bridge = new FakeBridge(snapshot());
    await mount(bridge);
    const panel = sourcePanel();

    fireEvent.change(panel.getByLabelText("服务地址"), {
      target: { value: "https://user:secret@git.example.test" },
    });
    fireEvent.click(panel.getByRole("button", { name: "保存连接" }));

    await waitFor(() => {
      expect(panel.getByText(/不得包含用户名或密码/)).toBeTruthy();
    });
    expect(panel.getByLabelText("服务地址").getAttribute("aria-invalid")).toBe("true");
    expect(bridge.countOf("connection_save")).toBe(0);
  });

  it("凭据引用里粘贴令牌会被拦在渲染进程内", async () => {
    const bridge = new FakeBridge(snapshot());
    await mount(bridge);
    const panel = sourcePanel();

    fireEvent.change(panel.getByLabelText("服务地址"), {
      target: { value: "https://git.example.test" },
    });
    fireEvent.change(panel.getByLabelText(/凭据引用/), {
      target: { value: "ghp_0123456789abcdefghij" },
    });
    fireEvent.click(panel.getByRole("button", { name: "测试连接" }));

    await waitFor(() => {
      expect(panel.getByText(/明文令牌/)).toBeTruthy();
    });
    expect(bridge.countOf("connection_test")).toBe(0);
  });

  it("后端错误显示原因、动作和错误代码", async () => {
    const bridge = new FakeBridge(snapshot()).failWith(
      "connection_save",
      ipcError({
        code: "platform.auth",
        category: "auth",
        stage: "connection",
        safe_message: "令牌无效或已过期",
        action: "请在 Windows 凭据管理器中更新该凭据",
      }),
    );
    await mount(bridge);
    const panel = sourcePanel();

    fireEvent.change(panel.getByLabelText("服务地址"), {
      target: { value: "https://git.example.test" },
    });
    fireEvent.change(panel.getByLabelText(/凭据引用/), {
      target: { value: "git-repo-migrator/source" },
    });
    fireEvent.click(panel.getByRole("button", { name: "保存连接" }));

    await waitFor(() => {
      expect(panel.getByText("令牌无效或已过期")).toBeTruthy();
    });
    expect(panel.getByText(/请在 Windows 凭据管理器中更新该凭据/)).toBeTruthy();
    expect(panel.getByText(/platform\.auth/)).toBeTruthy();
  });

  it("勾选自签名后要求指纹，并说明固定指纹不等于跳过校验", async () => {
    const bridge = new FakeBridge(snapshot());
    await mount(bridge);
    const panel = sourcePanel();

    expect(panel.getByText(/不等于跳过验证/)).toBeTruthy();
    fireEvent.click(panel.getByLabelText(/自签名证书/));
    fireEvent.change(panel.getByLabelText("服务地址"), {
      target: { value: "https://git.internal.test" },
    });
    fireEvent.change(panel.getByLabelText(/凭据引用/), {
      target: { value: "git-repo-migrator/source" },
    });
    fireEvent.change(panel.getByLabelText("证书 SHA-256 指纹"), { target: { value: "aa:bb" } });
    fireEvent.click(panel.getByRole("button", { name: "保存连接" }));

    await waitFor(() => {
      expect(panel.getByText(/64 位十六进制/)).toBeTruthy();
    });
    expect(bridge.countOf("connection_save")).toBe(0);
  });

  it("保存成功后只显示凭据引用，绝不回显 secret", async () => {
    const saved = connection({
      role: "source",
      id: "source",
      credential_ref: "git-repo-migrator/source",
    });
    const bridge = new FakeBridge(snapshot()).on("connection_save", () => saved);
    bridge.setSnapshot(snapshot({ connections: [saved] }));
    await mount(bridge);

    const panel = sourcePanel();
    expect(panel.getByText("git-repo-migrator/source")).toBeTruthy();
    expect(document.body.textContent).not.toContain("ghp_");
    // The form adopts the persisted values, so the payload the renderer sends
    // carries a reference — never a secret.
    expect(panel.getByLabelText(/凭据引用/)).toHaveProperty(
      "value",
      "git-repo-migrator/source",
    );
    fireEvent.click(panel.getByRole("button", { name: "保存连接" }));
    await waitFor(() => {
      expect(bridge.countOf("connection_save")).toBe(1);
    });
    const sent = bridge.inputFor("connection_save") as { input: Record<string, unknown> };
    expect(Object.keys(sent.input)).not.toContain("token");
    expect(Object.keys(sent.input)).not.toContain("password");
    expect(sent.input.credential_ref).toBe("git-repo-migrator/source");
  });

  it("通用 Git 服务提示改用手动 URL 导入", async () => {
    await mount(new FakeBridge(snapshot()));
    const panel = sourcePanel();
    expect(panel.getByText(/通用 Git 服务没有 API/)).toBeTruthy();
    expect(panel.getByText(/手动 URL 导入/)).toBeTruthy();
  });

  it("两个连接都保存后提示下一步已解锁", async () => {
    await mount(new FakeBridge(connectedSnapshot()));
    expect(screen.getByText(/源与目标均已保存/)).toBeTruthy();
  });

  it("能力矩阵把不支持的模块显示为文字而不仅是颜色", async () => {
    await mount(new FakeBridge(connectedSnapshot()));
    const panel = sourcePanel();
    expect(panel.getByText("可迁移 Issues")).toBeTruthy();
    expect(panel.getAllByText("不支持").length).toBeGreaterThan(0);
    expect(panel.getByText("可写入 Git 数据")).toBeTruthy();
  });
});

describe("本机凭据录入", () => {
  it("只把凭据名称发给后端，并用返回的引用回填表单", async () => {
    const bridge = new FakeBridge(snapshot()).on("connection_authorize", () => ({
      credential_ref: "credential/windows/1a2b3c4d",
      instructions: "已打开凭据录入窗口，请在该窗口中粘贴令牌两次",
    }));
    await mount(bridge);

    fireEvent.click(sourcePanel().getByRole("button", { name: "录入令牌" }));

    await waitFor(() => {
      expect(bridge.countOf("connection_authorize")).toBe(1);
    });
    // The payload must be a name and nothing else: a token here would put a
    // secret on the IPC boundary, which CM-004 forbids.
    expect(bridge.inputFor("connection_authorize")).toEqual({ input: { name: "source" } });

    await waitFor(() => {
      expect(credentialRefInput().value).toBe("credential/windows/1a2b3c4d");
    });
    expect(screen.getAllByText("已打开本机凭据录入窗口").length).toBeGreaterThan(0);
  });

  it("录入窗口无法打开时给出可执行的下一步", async () => {
    const bridge = new FakeBridge(snapshot()).failWith(
      "connection_authorize",
      ipcError({
        code: "credential.companion_missing",
        category: "validation",
        retryable: false,
        stage: "connection",
        safe_message: "找不到凭据录入程序 git-repo-migrator-credential.exe",
        action: "请重新安装应用；或在命令行中直接运行该程序录入凭据",
      }),
    );
    await mount(bridge);

    fireEvent.click(sourcePanel().getByRole("button", { name: "录入令牌" }));

    await waitFor(() => {
      expect(screen.getByText(/找不到凭据录入程序/)).toBeTruthy();
    });
    expect(screen.getByText(/请重新安装应用/)).toBeTruthy();
    expect(credentialRefInput().value).toBe("");
  });

  it("界面上没有任何令牌输入框", async () => {
    await mount(new FakeBridge(connectedSnapshot()));
    // The page may *mention* tokens in its guidance, but must never offer a
    // field that accepts one.
    for (const input of Array.from(document.querySelectorAll("input"))) {
      expect(input.type).not.toBe("password");
      const label = input.labels?.[0]?.textContent ?? "";
      expect(label).not.toMatch(/^令牌|访问令牌|密码/);
    }
    expect(connection().credential_ref).not.toMatch(/ghp_|glpat-/);
  });
});
