import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { QueueView } from "./QueueView";
import { filterLogs } from "./LogDrawer";
import { MigrationStore, useMigrationState } from "../../state/migrationStore";
import {
  FakeBridge,
  batch,
  connectedSnapshot,
  ipcError,
  logEntry,
  task,
} from "../../state/testBridge";
import type { BatchSnapshot, MigrationSnapshot } from "../../state/ipcTypes";

function Harness({ store, onViewReport }: { store: MigrationStore; onViewReport?: () => void }) {
  const state = useMigrationState(store);
  return <QueueView store={store} state={state} onViewReport={onViewReport ?? (() => {})} />;
}

async function mount(bridge: FakeBridge, onViewReport?: () => void) {
  const store = new MigrationStore(bridge);
  render(<Harness store={store} onViewReport={onViewReport} />);
  await act(async () => {
    await store.refresh();
  });
  return store;
}

function withBatch(patch: Partial<BatchSnapshot> = {}): MigrationSnapshot {
  return connectedSnapshot({ active_batch: batch(patch) });
}

const networkFailure = ipcError({
  code: "ipc.network",
  category: "network",
  retryable: true,
  stage: "git",
  safe_message: "推送过程中连接中断",
  action: "请检查网络后重试该仓库",
});

describe("日志筛选", () => {
  it("按任务、阶段、级别和错误代码过滤", () => {
    const logs = [
      logEntry({ task_id: "task-1", stage: "git", level: "error", code: "ipc.network" }),
      logEntry({ task_id: "task-2", stage: "lfs", level: "info", code: "lfs.ok" }),
    ];
    expect(filterLogs(logs, { taskId: "task-1", stage: "", level: "", code: "" })).toHaveLength(1);
    expect(filterLogs(logs, { taskId: "", stage: "lfs", level: "", code: "" })).toHaveLength(1);
    expect(filterLogs(logs, { taskId: "", stage: "", level: "error", code: "" })).toHaveLength(1);
    expect(filterLogs(logs, { taskId: "", stage: "", level: "", code: "network" })).toHaveLength(1);
  });
});

describe("迁移队列页", () => {
  it("没有批次时说明前置条件", async () => {
    await mount(new FakeBridge(connectedSnapshot()));
    expect(screen.getByText("尚未启动任何批次")).toBeTruthy();
  });

  it("暂停后显示已暂停，并说明不会启动新阶段", async () => {
    const bridge = new FakeBridge(withBatch()).on("batch_pause", () => batch({ control: "paused" }));
    const store = await mount(bridge);

    fireEvent.click(screen.getByRole("button", { name: /暂停/ }));
    await waitFor(() => {
      expect(bridge.countOf("batch_pause")).toBe(1);
    });

    // The snapshot is authoritative: the UI only shows "paused" once SQLite says so.
    bridge.setSnapshot(withBatch({ control: "paused" }));
    await act(async () => {
      await store.refresh();
    });
    expect(screen.getByText("已暂停")).toBeTruthy();
    expect(screen.getByText(/不会启动新的阶段/)).toBeTruthy();
    expect(
      screen.getByRole("button", { name: /继续/ }).hasAttribute("disabled"),
    ).toBe(false);
  });

  it("取消需要确认，并说明已完成的仓库不会回滚", async () => {
    const bridge = new FakeBridge(
      withBatch({
        completed: 2,
        tasks: [task({ task_id: "t1", state: "succeeded" }), task({ task_id: "t2" })],
      }),
    ).on("batch_cancel", () => batch({ control: "cancelled" }));
    await mount(bridge);

    fireEvent.click(screen.getByRole("button", { name: /取消批次/ }));
    const dialog = screen.getByRole("dialog", { name: "取消迁移批次" });
    expect(within(dialog).getByText(/不会回滚/)).toBeTruthy();
    expect(within(dialog).getByText(/引用也不会被删除/)).toBeTruthy();

    fireEvent.click(within(dialog).getByRole("button", { name: "确认取消" }));
    await waitFor(() => {
      expect(bridge.countOf("batch_cancel")).toBe(1);
    });
  });

  it("只重试后端标记为可重试的任务", async () => {
    const tasks = [
      task({ task_id: "t-network", state: "retryable_failed", retryable: true, error: networkFailure }),
      task({
        task_id: "t-permission",
        state: "skipped",
        retryable: false,
        error: ipcError({ code: "platform.permission" }),
      }),
    ];
    const bridge = new FakeBridge(withBatch({ tasks })).on("task_retry", () => ({
      retried: ["t-network"],
      rejected: [{ task_id: "t-permission", reason: "platform.permission：请授予写入权限" }],
      batch: batch({ tasks }),
    }));
    await mount(bridge);

    const retryButton = screen.getByRole("button", { name: /只重试可重试失败（1）/ });
    fireEvent.click(retryButton);

    await waitFor(() => {
      expect(bridge.countOf("task_retry")).toBe(1);
    });
    const sent = bridge.inputFor("task_retry") as { input: { task_ids: string[] } };
    expect(sent.input.task_ids).toEqual(["t-network"]);
    expect(await screen.findByText(/1 个任务未被重试/)).toBeTruthy();
    expect(screen.getByText(/请授予写入权限/)).toBeTruthy();
  });

  it("没有可重试项时重试按钮禁用并说明原因", async () => {
    await mount(new FakeBridge(withBatch({ tasks: [task({ state: "succeeded" })] })));
    const button = screen.getByRole("button", { name: /只重试可重试失败（0）/ });
    expect(button.hasAttribute("disabled")).toBe(true);
    expect(button.getAttribute("title")).toBe("当前没有可重试的失败项");
  });

  it("事件丢失后靠快照校正进度，而不是靠事件累加", async () => {
    const bridge = new FakeBridge(
      withBatch({ tasks: [task({ task_id: "t1", progress_completed: 3, progress_total: 10 })] }),
    );
    const store = await mount(bridge);
    expect(screen.getByText(/30% · 3\/10/)).toBeTruthy();

    // An event announces revision 9 but never delivers the numbers.
    act(() => {
      bridge.emit({ revision: 9, event: { type: "batch_started", batch_id: "batch-1" } });
    });
    expect(store.isStale()).toBe(true);

    bridge.setSnapshot({
      ...withBatch({
        tasks: [task({ task_id: "t1", progress_completed: 9, progress_total: 10 })],
      }),
      revision: 9,
    });
    await act(async () => {
      await store.refresh();
    });
    expect(screen.getByText(/90% · 9\/10/)).toBeTruthy();
    expect(store.isStale()).toBe(false);
  });

  it("进度区域使用 aria-live=polite，阻断错误使用 assertive", async () => {
    const bridge = new FakeBridge(withBatch()).failWith("batch_pause", ipcError({ retryable: false }));
    await mount(bridge);

    const progress = screen.getAllByRole("progressbar")[0];
    expect(progress.parentElement?.getAttribute("aria-live")).toBe("polite");

    fireEvent.click(screen.getByRole("button", { name: /暂停/ }));
    const alert = await screen.findByRole("alert");
    expect(alert.getAttribute("aria-live")).toBe("assertive");
  });

  it("按状态和错误代码筛选队列", async () => {
    const tasks = [
      task({ task_id: "t1", state: "succeeded" }),
      task({ task_id: "t2", state: "retryable_failed", retryable: true, error: networkFailure }),
    ];
    await mount(new FakeBridge(withBatch({ tasks })));

    fireEvent.change(screen.getByLabelText("状态"), { target: { value: "retryable_failed" } });
    expect(screen.getByRole("table").getAttribute("aria-rowcount")).toBe("1");

    fireEvent.change(screen.getByLabelText("状态"), { target: { value: "all" } });
    fireEvent.change(screen.getByLabelText("错误代码"), { target: { value: "ipc.network" } });
    expect(screen.getByRole("table").getAttribute("aria-rowcount")).toBe("1");
  });

  it("日志抽屉说明已脱敏，并可复制错误代码", async () => {
    const logs = [logEntry({ task_id: "task-1" })];
    await mount(new FakeBridge(withBatch({ tasks: [task({ task_id: "task-1" })], logs })));

    fireEvent.click(screen.getByRole("button", { name: "查看日志" }));
    const drawer = screen.getByRole("dialog", { name: /日志：task-1/ });
    expect(within(drawer).getByText(/不包含令牌、密码、私钥或完整响应体/)).toBeTruthy();
    expect(within(drawer).getByText("推送过程中连接中断")).toBeTruthy();
    expect(within(drawer).getByRole("button", { name: /复制错误代码 ipc.network/ })).toBeTruthy();
  });

  it("发现未完成批次时提示恢复前会复检凭据与能力", async () => {
    await mount(
      new FakeBridge(
        connectedSnapshot({
          active_batch: batch({ control: "paused" }),
          resumable: [
            {
              batch_id: "batch-1",
              plan_id: "plan-1",
              pending: 4,
              plan_hash_matches: true,
              credential_recheck_required: true,
              capability_recheck_required: true,
            },
          ],
        }),
      ),
    );
    expect(screen.getByText(/发现 1 个未完成批次/)).toBeTruthy();
    expect(screen.getByText(/重新检查凭据、目标可达性和平台能力/)).toBeTruthy();
    expect(screen.getByText(/需要复检凭据 · 需要复检平台能力/)).toBeTruthy();
  });

  it("状态同时用文字和徽章表达，不只依赖颜色", async () => {
    await mount(
      new FakeBridge(
        withBatch({
          tasks: [
            task({ task_id: "t1", state: "partial" }),
            task({ task_id: "t2", state: "skipped" }),
          ],
        }),
      ),
    );
    const table = within(screen.getByRole("table"));
    expect(table.getByText("平台数据部分失败")).toBeTruthy();
    expect(table.getByText("权限/冲突跳过")).toBeTruthy();
  });
});
