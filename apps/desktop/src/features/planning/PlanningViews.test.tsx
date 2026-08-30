import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { MappingView } from "./MappingView";
import { PreflightView } from "./PreflightView";
import { MigrationStore, useMigrationState } from "../../state/migrationStore";
import {
  FakeBridge,
  batch,
  connectedSnapshot,
  connection,
  ipcError,
  preflightRow,
  preview,
  repositories,
  repository,
} from "../../state/testBridge";
import type { MigrationSnapshot } from "../../state/ipcTypes";
import { applyNameTemplate, buildTargetUrl, validateTargetName } from "../../state/planDraft";

function MappingHarness({ store, onPreflight }: { store: MigrationStore; onPreflight?: () => void }) {
  const state = useMigrationState(store);
  return <MappingView store={store} state={state} onPreflight={onPreflight ?? (() => {})} />;
}

function PreflightHarness({ store, onStarted }: { store: MigrationStore; onStarted?: () => void }) {
  const state = useMigrationState(store);
  return <PreflightView store={store} state={state} onStarted={onStarted ?? (() => {})} />;
}

async function mountMapping(
  bridge: FakeBridge,
  selectedIds: string[],
  onPreflight?: () => void,
) {
  const store = new MigrationStore(bridge);
  render(<MappingHarness store={store} onPreflight={onPreflight} />);
  await act(async () => {
    await store.refresh();
    store.updateDraft({ selectedRepositoryIds: selectedIds });
  });
  return store;
}

async function mountPreflight(bridge: FakeBridge, onStarted?: () => void) {
  const store = new MigrationStore(bridge);
  render(<PreflightHarness store={store} onStarted={onStarted} />);
  await act(async () => {
    await store.refresh();
  });
  return store;
}

/** Target that can rebuild every selected module natively. */
function nativeTarget() {
  const native = (module: string) => ({
    module,
    supported: true,
    permitted: true,
    required_scopes: ["repo"],
    fidelity: "native_rebuild" as const,
    reason: null,
    degradation: null,
  });
  return connection({
    role: "target",
    id: "target",
    platform: "github",
    endpoint: "https://git.target.test",
    capabilities: ["lfs", "metadata", "issues", "pull_requests", "wiki", "releases"].map(native),
  });
}

describe("命名模板", () => {
  it("展开变量并拒绝平台会拒绝的名称", () => {
    expect(
      applyNameTemplate("{namespace}-{name}", {
        name: "alpha",
        namespace: "ops",
        visibility: "private",
      }),
    ).toBe("ops-alpha");
    expect(validateTargetName("ops-alpha")).toBeUndefined();
    expect(validateTargetName("ops/alpha")).toContain("只能包含");
    expect(validateTargetName("")).toContain("不能为空");
    expect(validateTargetName("a".repeat(101))).toContain("100");
  });

  it("拼接目标地址时不产生重复斜杠", () => {
    expect(buildTargetUrl("https://git.test/", "ops", "alpha")).toBe(
      "https://git.test/ops/alpha",
    );
    expect(buildTargetUrl("https://git.test", "", "alpha")).toBe("https://git.test/alpha");
  });
});

describe("映射与策略页", () => {
  const items = repositories(3);
  const snapshotWithRepos = (): MigrationSnapshot =>
    connectedSnapshot({ repositories: items });

  it("默认策略是空仓复用、非空跳过，覆盖开关默认关闭", async () => {
    const store = await mountMapping(
      new FakeBridge(snapshotWithRepos()),
      items.map((item) => item.id),
    );
    const selected = screen.getByLabelText(/空仓库复用，非空跳过/) as HTMLInputElement;
    expect(selected.checked).toBe(true);
    expect(store.getState().draft.allowOverwrite).toBe(false);
    expect(screen.queryByLabelText(/我已了解覆盖迁移的影响/)).toBeNull();
  });

  it("未实现的「继续同步」显示为禁用并说明原因", async () => {
    await mountMapping(new FakeBridge(snapshotWithRepos()), items.map((item) => item.id));
    const option = screen.getByLabelText(/继续同步到非空目标/) as HTMLInputElement;
    expect(option.disabled).toBe(true);
    expect(screen.getByText(/本版本尚未实现增量同步/)).toBeTruthy();
  });

  it("打开覆盖迁移会展开影响范围，且未确认前不能运行预检", async () => {
    const bridge = new FakeBridge(snapshotWithRepos());
    await mountMapping(bridge, items.map((item) => item.id));

    fireEvent.click(screen.getByLabelText(/覆盖迁移/));
    const danger = within(screen.getByText(/影响范围：3 个目标仓库/).parentElement!);
    expect(danger.getByText(/会替换目标已有的分支和 Tag/)).toBeTruthy();

    const run = screen.getByRole("button", { name: /保存映射并运行预检/ });
    expect(run.hasAttribute("disabled")).toBe(true);

    fireEvent.click(screen.getByLabelText(/我已了解覆盖迁移的影响/));
    expect(run.hasAttribute("disabled")).toBe(false);
  });

  it("三档保真度按目标能力显示，不支持时说明原因", async () => {
    await mountMapping(new FakeBridge(snapshotWithRepos()), items.map((item) => item.id));
    // The generic-git fixture cannot rebuild issues.
    expect(screen.getByText(/通用 Git 服务没有平台数据 API/)).toBeTruthy();

    fireEvent.click(screen.getByLabelText("Issues"));
    expect(screen.getByText(/个模块只能归档或不支持迁移/)).toBeTruthy();
  });

  it("目标平台支持时显示原生重建", async () => {
    const snapshot = connectedSnapshot({ repositories: items });
    snapshot.connections = [snapshot.connections[0], nativeTarget()];
    await mountMapping(new FakeBridge(snapshot), items.map((item) => item.id));
    fireEvent.click(screen.getByLabelText("Issues"));
    expect(screen.getAllByText("原生重建").length).toBeGreaterThan(0);
    expect(screen.queryByText(/个模块只能归档或不支持迁移/)).toBeNull();
  });

  it("私有 refs 默认排除，并说明归档只在本地", async () => {
    await mountMapping(new FakeBridge(snapshotWithRepos()), items.map((item) => item.id));
    const archive = screen.getByLabelText(/把平台私有 refs 归档到本地报告/) as HTMLInputElement;
    expect(archive.checked).toBe(false);
    expect(screen.getByText(/仅本地归档，仍然不会推送到目标/)).toBeTruthy();
  });

  it("临时工作区策略默认复用镜像，可切换为重试前清理", async () => {
    const store = await mountMapping(
      new FakeBridge(snapshotWithRepos()),
      items.map((item) => item.id),
    );
    const reuse = screen.getByLabelText(/复用残留镜像（默认）/ ) as HTMLInputElement;
    const clean = screen.getByLabelText(/重试前清理工作区/) as HTMLInputElement;
    expect(reuse.checked).toBe(true);
    expect(clean.checked).toBe(false);
    expect(store.getState().draft.workspacePolicy).toBe("reuse");

    fireEvent.click(clean);
    expect(store.getState().draft.workspacePolicy).toBe("clean");
    expect((screen.getByLabelText(/复用残留镜像（默认）/) as HTMLInputElement).checked).toBe(
      false,
    );
  });

  it("非法目标名称阻止预检", async () => {
    const bridge = new FakeBridge(snapshotWithRepos());
    const store = await mountMapping(bridge, items.map((item) => item.id));
    await act(async () => {
      store.updateDraft({ nameTemplate: "{namespace}/{name}" });
    });
    expect(screen.getByText(/3 个目标名称不符合平台命名规则/)).toBeTruthy();
    expect(
      screen.getByRole("button", { name: /保存映射并运行预检/ }).hasAttribute("disabled"),
    ).toBe(true);
  });

  it("运行预检会先持久化映射，再请求预览", async () => {
    let advanced = false;
    const bridge = new FakeBridge(snapshotWithRepos())
      .on("repository_set_mapping", () => repository())
      .on("plan_preview", () => preview());
    await mountMapping(bridge, items.map((item) => item.id), () => {
      advanced = true;
    });

    fireEvent.click(screen.getByRole("button", { name: /保存映射并运行预检/ }));
    await waitFor(() => {
      expect(advanced).toBe(true);
    });
    expect(bridge.countOf("repository_set_mapping")).toBe(3);
    const sent = bridge.inputFor("plan_preview") as {
      input: { allow_overwrite: boolean; include_archived_refs: boolean };
    };
    expect(sent.input.allow_overwrite).toBe(false);
    expect(sent.input.include_archived_refs).toBe(false);
  });
});

describe("预检页", () => {
  it("没有预检结果时引导返回映射页", async () => {
    await mountPreflight(new FakeBridge(connectedSnapshot()));
    expect(screen.getByText("尚未生成预检")).toBeTruthy();
  });

  it("阻断项不为零时禁用开始，并解释原因", async () => {
    const blocked = preview({
      rows: [
        preflightRow({
          action: "blocked",
          target_state: "unknown",
          blocking_reason: "目标状态待复检：https://git.target.test/ops/alpha",
          suggested_action: "请点击「探测目标」确认目标是否存在",
        }),
      ],
      blocking: ["目标状态待复检：https://git.target.test/ops/alpha"],
    });
    await mountPreflight(
      new FakeBridge(connectedSnapshot({ active_preview: blocked })),
    );

    const start = screen.getByRole("button", { name: /冻结计划并开始迁移/ });
    expect(start.hasAttribute("disabled")).toBe(true);
    expect(start.getAttribute("title")).toContain("1 项阻断");
    expect(screen.getByText(/阻断项必须修正或排除/)).toBeTruthy();
    expect(screen.getByText(/请点击「探测目标」确认目标是否存在/)).toBeTruthy();
  });

  it("目标状态未知时提供探测按钮", async () => {
    const bridge = new FakeBridge(
      connectedSnapshot({
        active_preview: preview({
          rows: [preflightRow({ action: "blocked", target_state: "unknown" })],
        }),
      }),
    ).on("repository_probe_target", () => repository());
    await mountPreflight(bridge);

    fireEvent.click(screen.getByRole("button", { name: /探测目标状态（1）/ }));
    await waitFor(() => {
      expect(bridge.countOf("repository_probe_target")).toBe(1);
    });
  });

  it("三档保真度全部显式展示，降级模块必须逐项确认", async () => {
    const withFidelity = preview({
      rows: [
        preflightRow({
          module_fidelity: [
            { module: "metadata", fidelity: "native_rebuild", reason: null, confirmation_required: false },
            {
              module: "issues",
              fidelity: "read_only_archive",
              reason: "目标不支持写入 Issues",
              confirmation_required: true,
            },
            {
              module: "wiki",
              fidelity: "unsupported",
              reason: "目标没有 Wiki API",
              confirmation_required: true,
            },
          ],
        }),
      ],
    });
    await mountPreflight(new FakeBridge(connectedSnapshot({ active_preview: withFidelity })));

    expect(screen.getAllByText("原生重建").length).toBeGreaterThan(0);
    expect(screen.getAllByText("只读归档").length).toBeGreaterThan(0);
    expect(screen.getAllByText("不支持").length).toBeGreaterThan(0);

    const start = screen.getByRole("button", { name: /冻结计划并开始迁移/ });
    expect(start.hasAttribute("disabled")).toBe(true);
    expect(start.getAttribute("title")).toContain("2 个降级模块");

    fireEvent.click(screen.getByLabelText("确认模块 issues 的降级处理"));
    fireEvent.click(screen.getByLabelText("确认模块 wiki 的降级处理"));
    expect(start.hasAttribute("disabled")).toBe(false);
  });

  it("覆盖计划必须输入后端下发的确认文本", async () => {
    const dangerous = preview({
      rows: [preflightRow({ action: "overwrite", target_state: "non_empty" })],
      requires_confirmation: true,
      confirmation_phrase: "alpha",
    });
    const bridge = new FakeBridge(connectedSnapshot({ active_preview: dangerous }));
    await mountPreflight(bridge);

    const start = screen.getByRole("button", { name: /冻结计划并开始迁移/ });
    expect(start.hasAttribute("disabled")).toBe(true);

    fireEvent.change(screen.getByLabelText("确认文本"), { target: { value: "alph" } });
    expect(start.hasAttribute("disabled")).toBe(true);

    fireEvent.change(screen.getByLabelText("确认文本"), { target: { value: "alpha" } });
    expect(start.hasAttribute("disabled")).toBe(false);
    expect(screen.getByText(/确认由后端校验/)).toBeTruthy();
  });

  it("即使界面允许，后端仍会因过期快照拒绝启动", async () => {
    const bridge = new FakeBridge(connectedSnapshot({ active_preview: preview() }))
      .on("plan_freeze", () => ({
        plan_id: "plan-1",
        plan_hash: "b".repeat(64),
        status: "frozen",
        repository_count: 1,
        capability_snapshot_hash: "cap-hash",
        dangerous_confirmed: false,
        created_at_ms: 0,
      }))
      .failWith(
        "batch_start",
        ipcError({
          code: "ipc.conflict",
          category: "conflict",
          stage: "queue",
          safe_message: "目标平台能力快照已变化",
          action: "请重新运行预检以刷新能力矩阵",
        }),
      );
    let started = false;
    const store = await mountPreflight(bridge, () => {
      started = true;
    });

    fireEvent.click(screen.getByRole("button", { name: /冻结计划并开始迁移/ }));
    await waitFor(() => {
      expect(store.getState().error?.safe_message).toBe("目标平台能力快照已变化");
    });
    expect(started).toBe(false);
  });

  it("非空目标默认显示为跳过而不是覆盖", async () => {
    await mountPreflight(
      new FakeBridge(
        connectedSnapshot({
          active_preview: preview({
            rows: [preflightRow({ action: "skip_non_empty", target_state: "non_empty" })],
          }),
        }),
      ),
    );
    const table = within(screen.getByRole("table"));
    expect(table.getByText("跳过非空目标")).toBeTruthy();
    expect(table.queryByText("覆盖目标")).toBeNull();
  });

  it("字段映射抽屉显示不支持的字段并可用 Esc 关闭", async () => {
    await mountPreflight(new FakeBridge(connectedSnapshot({ active_preview: preview() })));
    fireEvent.click(screen.getByRole("button", { name: "alpha" }));

    const drawer = screen.getByRole("dialog", { name: /字段映射：alpha/ });
    expect(within(drawer).getByText(/不支持写入可见性/)).toBeTruthy();

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: /字段映射：alpha/ })).toBeNull();
    });
  });

  it("引用策略摘要列出白名单与被排除的私有 refs", async () => {
    await mountPreflight(new FakeBridge(connectedSnapshot({ active_preview: preview() })));
    expect(screen.getByText(/refs\/heads\/\*:refs\/heads\/\*/)).toBeTruthy();
    expect(screen.getByText(/refs\/merge-requests\/\*/)).toBeTruthy();
  });

  it("摘要显示工作区策略，并把该策略原样传给 batch_start", async () => {
    const bridge = new FakeBridge(connectedSnapshot({ active_preview: preview() }))
      .on("plan_freeze", () => ({
        plan_id: "plan-1",
        plan_hash: "b".repeat(64),
        status: "frozen",
        repository_count: 1,
        capability_snapshot_hash: "cap-hash",
        dangerous_confirmed: false,
        created_at_ms: 0,
      }))
      .on("batch_start", () => batch());
    let started = false;
    const store = await mountPreflight(bridge, () => {
      started = true;
    });

    await act(async () => {
      store.updateDraft({ workspacePolicy: "clean" });
    });
    expect(screen.getByText(/重试前清理工作区（每次重新克隆）/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /冻结计划并开始迁移/ }));
    await waitFor(() => {
      expect(started).toBe(true);
    });
    const sent = bridge.inputFor("batch_start") as {
      input: { workspace_policy: string; concurrency: number };
    };
    expect(sent.input.workspace_policy).toBe("clean");
  });
});
