import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { AppRouter, parseRoute, routeFor } from "./router";
import { MigrationStore, connectionFor, stepUnlocked } from "./state/migrationStore";
import {
  FakeBridge,
  batch,
  connectedSnapshot,
  plan,
  preview,
  report,
  repositories,
  snapshot,
} from "./state/testBridge";

async function mount(bridge: FakeBridge) {
  const store = new MigrationStore(bridge);
  render(<AppRouter store={store} />);
  // The router refreshes on mount; wait for the first snapshot to land.
  await waitFor(() => {
    expect(store.getState().status).toBe("ready");
  });
  return store;
}

describe("路由解析", () => {
  it("把哈希映射到步骤，未知路径回落到连接页", () => {
    expect(parseRoute("#/queue")).toBe("queue");
    expect(parseRoute("")).toBe("connections");
    expect(parseRoute("#/nope")).toBe("connections");
    expect(routeFor("report")).toBe("/report");
  });
});

describe("步骤解锁规则", () => {
  it("按后端快照而不是导航历史决定可达性", () => {
    const empty = snapshot();
    expect(stepUnlocked(empty, "connections")).toBe(true);
    expect(stepUnlocked(empty, "repositories")).toBe(false);

    const connected = connectedSnapshot();
    expect(stepUnlocked(connected, "repositories")).toBe(true);
    expect(stepUnlocked(connected, "mapping")).toBe(false);

    const withRepos = connectedSnapshot({ repositories: repositories(2) });
    expect(stepUnlocked(withRepos, "mapping")).toBe(true);
    expect(stepUnlocked(withRepos, "queue")).toBe(false);

    const withPlan = connectedSnapshot({ repositories: repositories(2), active_plan: plan() });
    expect(stepUnlocked(withPlan, "queue")).toBe(true);
    expect(stepUnlocked(withPlan, "report")).toBe(false);

    const withBatch = connectedSnapshot({ active_batch: batch() });
    expect(stepUnlocked(withBatch, "report")).toBe(true);
  });

  it("按角色取连接", () => {
    const connected = connectedSnapshot();
    expect(connectionFor(connected, "source")?.id).toBe("source");
    expect(connectionFor(connected, "target")?.id).toBe("target");
    expect(connectionFor(snapshot(), "source")).toBeNull();
  });
});

describe("应用外壳", () => {
  it("未满足前置条件的步骤禁用并说明原因", async () => {
    await mount(new FakeBridge(snapshot()));
    const nav = within(screen.getByRole("navigation", { name: "迁移步骤" }));

    const repositoriesStep = nav.getByRole("button", { name: /选择仓库/ });
    expect(repositoriesStep.hasAttribute("disabled")).toBe(true);
    expect(repositoriesStep.getAttribute("title")).toBe("请先完成源平台和目标平台连接");

    const queueStep = nav.getByRole("button", { name: /迁移队列/ });
    expect(queueStep.getAttribute("title")).toBe("请先冻结迁移计划");
  });

  it("直接请求被锁定的路由时回落到可达的步骤", async () => {
    window.location.hash = "#/report";
    await mount(new FakeBridge(snapshot()));
    // Only the connection step is reachable, so that is what renders.
    expect(screen.getByRole("heading", { level: 1 }).textContent).toBe("连接 Git 平台");
  });

  it("前置条件满足后可以进入对应步骤", async () => {
    await mount(new FakeBridge(connectedSnapshot({ repositories: repositories(3) })));
    const nav = within(screen.getByRole("navigation", { name: "迁移步骤" }));

    fireEvent.click(nav.getByRole("button", { name: /选择仓库/ }));
    expect(screen.getByRole("heading", { level: 1 }).textContent).toBe("选择要迁移的仓库");
    expect(window.location.hash).toBe("#/repositories");
  });

  it("事件领先于快照时提示界面可能不是最新", async () => {
    const bridge = new FakeBridge(connectedSnapshot());
    const store = await mount(bridge);

    act(() => {
      bridge.emit({ revision: 42, event: { type: "batch_started", batch_id: "batch-1" } });
    });
    expect(await screen.findByText(/收到新的进度事件/)).toBeTruthy();
    expect(store.isStale()).toBe(true);

    bridge.setSnapshot({ ...connectedSnapshot(), revision: 42 });
    await act(async () => {
      await store.refresh();
    });
    await waitFor(() => {
      expect(screen.queryByText(/收到新的进度事件/)).toBeNull();
    });
  });

  it("快照 schema 版本不一致时拒绝按旧语义渲染", async () => {
    await mount(new FakeBridge({ ...connectedSnapshot(), schema_version: 99 }));
    expect(screen.getByText("本地状态库版本与界面不一致")).toBeTruthy();
    expect(screen.getByText(/不要在版本不一致时启动迁移/)).toBeTruthy();
  });

  it("刷新按钮重新读取本地状态库", async () => {
    const bridge = new FakeBridge(connectedSnapshot());
    await mount(bridge);
    const before = bridge.countOf("migration_snapshot");

    fireEvent.click(screen.getByRole("button", { name: /刷新/ }));
    await waitFor(() => {
      expect(bridge.countOf("migration_snapshot")).toBe(before + 1);
    });
  });

  it("常驻显示本地处理提示", async () => {
    await mount(new FakeBridge(snapshot()));
    expect(screen.getByText(/代码与令牌仅在本机处理/)).toBeTruthy();
    expect(screen.getByText("本地模式")).toBeTruthy();
  });

  it("有批次和报告时六步全部可达", async () => {
    await mount(
      new FakeBridge(
        connectedSnapshot({
          repositories: repositories(2),
          active_preview: preview(),
          active_plan: plan(),
          active_batch: batch(),
          report: report(),
        }),
      ),
    );
    const nav = within(screen.getByRole("navigation", { name: "迁移步骤" }));
    for (const label of [
      /连接/,
      /选择仓库/,
      /映射与策略/,
      /预检/,
      /迁移队列/,
      /报告/,
    ]) {
      expect(nav.getByRole("button", { name: label }).hasAttribute("disabled")).toBe(false);
    }
  });
});
