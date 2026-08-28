import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { DiscoveryView } from "./DiscoveryView";
import { MigrationStore, useMigrationState } from "../../state/migrationStore";
import {
  FakeBridge,
  connectedSnapshot,
  ipcError,
  repositories,
  repository,
} from "../../state/testBridge";
import {
  ROW_WINDOW,
  applyFilters,
  emptyFilters,
  emptySelection,
  resolveSelection,
  selectAllFiltered,
  toggle,
  windowRows,
} from "./selectionModel";

function Harness({ store, onContinue }: { store: MigrationStore; onContinue?: () => void }) {
  const state = useMigrationState(store);
  return <DiscoveryView store={store} state={state} onContinue={onContinue ?? (() => {})} />;
}

async function mount(bridge: FakeBridge, onContinue?: () => void) {
  const store = new MigrationStore(bridge);
  render(<Harness store={store} onContinue={onContinue} />);
  await act(async () => {
    await store.refresh();
  });
  return store;
}

const NOW = 1_760_000_000;

describe("选择模型", () => {
  it("全选覆盖整个筛选结果，而不是渲染出来的行", () => {
    const items = repositories(1000);
    const filtered = applyFilters(items, emptyFilters, NOW);
    const resolved = resolveSelection(filtered, selectAllFiltered(emptySelection));

    const { rows, hidden } = windowRows(filtered, ROW_WINDOW);
    expect(rows).toHaveLength(ROW_WINDOW);
    expect(hidden).toBe(900);
    // Windowing bounds rendering only; the selection is still the full set.
    expect(resolved.selectedIds).toHaveLength(1000);
    expect(resolved.matchingCount).toBe(1000);
  });

  it("排除规则与手动排除都带可见原因", () => {
    const items = [
      repository({ name: "alpha" }),
      repository({ name: "beta-archive" }),
      repository({ name: "gamma" }),
    ];
    let selection = selectAllFiltered(emptySelection);
    selection = {
      ...selection,
      rules: [{ id: "r1", kind: "name_glob", pattern: "*-archive", enabled: true }],
    };
    selection = toggle(selection, items[2].id);

    const resolved = resolveSelection(items, selection);
    expect(resolved.selectedIds).toEqual([items[0].id]);
    expect(resolved.exclusions.map((item) => item.reason)).toEqual([
      "排除规则：名称匹配 *-archive",
      "手动排除",
    ]);
  });

  it("权限不足的仓库永远不会进入选择集合", () => {
    const items = [
      repository({ name: "alpha" }),
      repository({
        name: "locked",
        permission: "insufficient",
        selectable: false,
        unselectable_reason: "当前凭据没有推送权限",
      }),
    ];
    const resolved = resolveSelection(items, selectAllFiltered(emptySelection));
    expect(resolved.selectedIds).toHaveLength(1);
    expect(resolved.blockedCount).toBe(1);
    expect(resolved.exclusions[0].reason).toBe("当前凭据没有推送权限");
  });

  it("筛选条件改变会更新符合条件数，但不清空选择方式", () => {
    const items = [
      repository({ name: "alpha", namespace: "ops" }),
      repository({ name: "beta", namespace: "infra" }),
    ];
    const selection = selectAllFiltered(emptySelection);
    const filtered = applyFilters(items, { ...emptyFilters, namespace: "ops" }, NOW);
    const resolved = resolveSelection(filtered, selection);
    expect(resolved.matchingCount).toBe(1);
    expect(resolved.selectedIds).toHaveLength(1);
  });

  it("按更新时间筛选会排除时间未知的仓库", () => {
    const items = [
      repository({ name: "fresh", updated_at_epoch_seconds: NOW - 86_400 }),
      repository({ name: "old", updated_at_epoch_seconds: NOW - 86_400 * 400 }),
      repository({ name: "unknown", updated_at_epoch_seconds: null }),
    ];
    const filtered = applyFilters(items, { ...emptyFilters, updatedWithinDays: 30 }, NOW);
    expect(filtered.map((item) => item.name)).toEqual(["fresh"]);
  });
});

describe("仓库选择页", () => {
  it("100 个仓库全部渲染，计数为全量", async () => {
    await mount(new FakeBridge(connectedSnapshot({ repositories: repositories(100) })));
    fireEvent.click(screen.getByRole("button", { name: /选择全部筛选结果（100）/ }));

    expect(screen.getByRole("table").getAttribute("aria-rowcount")).toBe("100");
    expect(screen.getByText(/已选择全部 100 个筛选结果/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /继续设置映射（100）/ })).toBeTruthy();
  });

  it("1000 个仓库只渲染窗口内的行，但选择与计数仍是全量", async () => {
    await mount(new FakeBridge(connectedSnapshot({ repositories: repositories(1000) })));
    fireEvent.click(screen.getByRole("button", { name: /选择全部筛选结果（1000）/ }));

    const table = screen.getByRole("table");
    expect(table.getAttribute("aria-rowcount")).toBe("1000");
    expect(within(table).getAllByRole("row")).toHaveLength(ROW_WINDOW + 1); // + header
    expect(screen.getByText(/还有 900 行未渲染/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /继续设置映射（1000）/ })).toBeTruthy();
  });

  it("全选后取消一行，横幅与最终计划数同时更新", async () => {
    const items = repositories(5);
    await mount(new FakeBridge(connectedSnapshot({ repositories: items })));
    fireEvent.click(screen.getByRole("button", { name: /选择全部筛选结果（5）/ }));
    fireEvent.click(screen.getByLabelText("选择 repo2"));

    expect(screen.getByText(/已选择全部 5 个筛选结果，当前排除 1 个/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /继续设置映射（4）/ })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /查看排除项（1）/ }));
    const drawer = within(screen.getByRole("dialog", { name: "排除项" }));
    expect(drawer.getByText("repo2")).toBeTruthy();
    expect(drawer.getByText("手动排除")).toBeTruthy();
  });

  it("权限不足的行禁用选择框并说明原因", async () => {
    await mount(
      new FakeBridge(
        connectedSnapshot({
          repositories: [
            repository({ name: "alpha" }),
            repository({
              name: "locked",
              permission: "insufficient",
              selectable: false,
              unselectable_reason: "当前凭据没有推送权限",
            }),
          ],
        }),
      ),
    );
    const checkbox = screen.getByLabelText("选择 locked") as HTMLInputElement;
    expect(checkbox.disabled).toBe(true);
    expect(checkbox.getAttribute("title")).toBe("当前凭据没有推送权限");
    expect(within(screen.getByRole("table")).getByText("权限不足")).toBeTruthy();
  });

  it("部分获取失败保留已加载结果并给出警告", async () => {
    const bridge = new FakeBridge(
      connectedSnapshot({ repositories: repositories(3) }),
    ).on("repository_discover", () => ({
      items: [],
      next_cursor: null,
      total_count: null,
      loaded: 3,
      warnings: ["保留已加载结果；第 2 页读取失败"],
    }));
    await mount(bridge);

    fireEvent.click(screen.getByRole("button", { name: /自动发现仓库/ }));
    await waitFor(() => {
      expect(screen.getByText(/第 2 页读取失败/)).toBeTruthy();
    });
    // The three repositories loaded earlier are still on screen.
    expect(screen.getByRole("table").getAttribute("aria-rowcount")).toBe("3");
  });

  it("没有 API 时发现命令返回不支持，并指向手动导入", async () => {
    const bridge = new FakeBridge(connectedSnapshot()).failWith(
      "repository_discover",
      ipcError({
        code: "ipc.unsupported",
        category: "unsupported",
        stage: "discovery",
        safe_message: "通用 Git 服务没有仓库发现 API",
        action: "请改用「手动 URL 导入」",
      }),
    );
    const store = await mount(bridge);
    fireEvent.click(screen.getByRole("button", { name: /自动发现仓库/ }));

    await waitFor(() => {
      expect(store.getState().error?.action).toContain("手动 URL 导入");
    });
  });

  it("导入报告逐行说明未导入的地址", async () => {
    const bridge = new FakeBridge(connectedSnapshot()).on("repository_import", () => ({
      imported: 1,
      duplicate_count: 1,
      issues: [{ line: 3, value: "javascript:alert(1)", message: "不支持的协议: javascript" }],
    }));
    await mount(bridge);

    fireEvent.change(screen.getByLabelText(/手动 URL 导入/), {
      target: { value: "https://git.example.test/a.git" },
    });
    fireEvent.click(screen.getByRole("button", { name: "导入这些地址" }));

    await waitFor(() => {
      expect(screen.getByText(/已导入 1 个地址，去重 1 个/)).toBeTruthy();
    });
    expect(screen.getByText(/第 3 行：不支持的协议: javascript/)).toBeTruthy();
  });

  it("空结果给出下一步动作而不是空白表格", async () => {
    await mount(new FakeBridge(connectedSnapshot({ repositories: repositories(2) })));
    fireEvent.change(screen.getByLabelText("仓库名称"), { target: { value: "找不到" } });

    expect(screen.getByText("当前筛选条件没有结果")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "清除全部筛选" }));
    expect(screen.getByRole("table")).toBeTruthy();
  });

  it("继续时把全量选择与排除写入计划草稿", async () => {
    let advanced = false;
    const items = repositories(4);
    const store = await mount(
      new FakeBridge(connectedSnapshot({ repositories: items })),
      () => {
        advanced = true;
      },
    );
    fireEvent.click(screen.getByRole("button", { name: /选择全部筛选结果（4）/ }));
    fireEvent.click(screen.getByLabelText("选择 repo0"));
    fireEvent.click(screen.getByRole("button", { name: /继续设置映射（3）/ }));

    expect(advanced).toBe(true);
    const draft = store.getState().draft;
    expect(draft.selectedRepositoryIds).toHaveLength(3);
    expect(draft.excludedRepositoryIds).toEqual([items[0].id]);
  });

  it("长 URL 完整值保留在 title 中，表格里省略显示", async () => {
    const long = repository({
      name: "very-long",
    });
    await mount(new FakeBridge(connectedSnapshot({ repositories: [long] })));
    const cell = screen.getAllByTitle(long.source_url)[0];
    expect(cell.className).toContain("cell-url");
  });
});
