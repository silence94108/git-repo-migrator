import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ReportView } from "./ReportView";
import { MigrationStore, useMigrationState } from "../../state/migrationStore";
import {
  FakeBridge,
  batch,
  connectedSnapshot,
  ipcError,
  report,
  reportRow,
} from "../../state/testBridge";
import type { MigrationSnapshot, ReportSnapshot } from "../../state/ipcTypes";

function Harness({ store, onRetry }: { store: MigrationStore; onRetry?: () => void }) {
  const state = useMigrationState(store);
  return <ReportView store={store} state={state} onRetry={onRetry ?? (() => {})} />;
}

async function mount(bridge: FakeBridge, onRetry?: () => void) {
  const store = new MigrationStore(bridge);
  render(<Harness store={store} onRetry={onRetry} />);
  await act(async () => {
    await store.refresh();
  });
  return store;
}

function withReport(value: ReportSnapshot): MigrationSnapshot {
  return connectedSnapshot({ active_batch: batch(), report: value });
}

const partialRow = reportRow({
  task_id: "task-partial",
  status: "partial",
  git_verified: true,
  lfs_verified: true,
  metadata_verified: false,
  archive_path: "D:\\workspace\\archive\\alpha.jsonl",
  unmapped_fields: ["assignee", "milestone"],
  modules: [
    {
      module: "issues",
      fidelity: "read_only_archive",
      reason: "目标平台不支持写入 Issues",
      confirmation_required: true,
    },
  ],
  source_links: ["https://git.source.test/ops/alpha/issues/1"],
  next_action: "如需在目标平台重建 Issues，请改用支持 Issues API 的目标连接",
});

describe("报告页", () => {
  it("没有结果时不显示成功，而是解释原因", async () => {
    await mount(new FakeBridge(connectedSnapshot({ active_batch: batch(), report: report({ rows: [] }) })));
    expect(screen.getByText("批次尚未产生结果")).toBeTruthy();
    expect(screen.getByText(/进行中的任务不会被计为成功/)).toBeTruthy();
  });

  it("Git 成功但平台数据部分失败不会显示为完整成功", async () => {
    await mount(new FakeBridge(withReport(report({ rows: [partialRow] }))));

    const table = within(screen.getByRole("table"));
    expect(table.getByText("Git 成功 · 平台数据部分失败")).toBeTruthy();
    expect(table.queryByText("完整成功")).toBeNull();
    expect(screen.getByText(/只做了归档或未迁移/)).toBeTruthy();
    expect(screen.getByText(/这些仓库不算完整成功/)).toBeTruthy();
  });

  it("四类结果分开统计并可点击筛选", async () => {
    const rows = [
      reportRow({ task_id: "a", status: "succeeded" }),
      partialRow,
      reportRow({ task_id: "c", status: "retryable_failed", error_code: "ipc.network" }),
      reportRow({ task_id: "d", status: "skipped", error_code: "platform.permission" }),
    ];
    await mount(new FakeBridge(withReport(report({ rows }))));

    expect(screen.getByRole("table").getAttribute("aria-rowcount")).toBe("4");
    fireEvent.click(screen.getByRole("button", { name: /完整成功/ }));
    expect(screen.getByRole("table").getAttribute("aria-rowcount")).toBe("1");

    fireEvent.click(screen.getByRole("button", { name: "清除筛选" }));
    expect(screen.getByRole("table").getAttribute("aria-rowcount")).toBe("4");
  });

  it("证据抽屉区分归档与原生，并显示未映射字段", async () => {
    await mount(new FakeBridge(withReport(report({ rows: [partialRow] }))));
    fireEvent.click(screen.getByRole("button", { name: "查看证据" }));

    const drawer = within(screen.getByRole("dialog", { name: /证据详情/ }));
    expect(drawer.getByText("只读归档")).toBeTruthy();
    expect(drawer.queryByText("原生重建")).toBeNull();
    expect(drawer.getByText(/不会在目标平台呈现为可交互的 Issue 或 PR/)).toBeTruthy();
    expect(drawer.getByText("D:\\workspace\\archive\\alpha.jsonl")).toBeTruthy();
    expect(drawer.getByText(/2 个字段无法映射/)).toBeTruthy();
    expect(drawer.getByText("assignee · milestone")).toBeTruthy();
  });

  it("证据抽屉列出被策略排除的引用和引用比对结果", async () => {
    await mount(new FakeBridge(withReport(report({ rows: [reportRow()] }))));
    fireEvent.click(screen.getByRole("button", { name: "查看证据" }));

    const drawer = within(screen.getByRole("dialog", { name: /证据详情/ }));
    expect(drawer.getByText("refs/pull/1/head")).toBeTruthy();
    expect(drawer.getByText(/已校验 7 个，缺失 0 个/)).toBeTruthy();
  });

  it("可重试失败的行在抽屉里提供回到队列重试", async () => {
    let retried = false;
    await mount(
      new FakeBridge(
        withReport(
          report({
            rows: [
              reportRow({ task_id: "c", status: "retryable_failed", error_code: "ipc.network" }),
            ],
          }),
        ),
      ),
      () => {
        retried = true;
      },
    );
    fireEvent.click(screen.getByRole("button", { name: "查看证据" }));
    fireEvent.click(screen.getByRole("button", { name: "回到队列重试该仓库" }));
    expect(retried).toBe(true);
  });

  it("导出前必须选择目录，导出路径由后端校验", async () => {
    const bridge = new FakeBridge(withReport(report())).on("report_export", () => ({
      path: "D:\\reports\\batch-1-csv.csv",
      bytes_written: 256,
      row_count: 1,
    }));
    await mount(bridge);

    const csv = screen.getByRole("button", { name: /导出 CSV/ });
    expect(csv.hasAttribute("disabled")).toBe(true);
    expect(csv.getAttribute("title")).toBe("请先填写导出目录");

    fireEvent.change(screen.getByLabelText(/导出目录/), {
      target: { value: "D:\\reports" },
    });
    fireEvent.click(screen.getByRole("button", { name: /导出 CSV/ }));

    await waitFor(() => {
      expect(bridge.countOf("report_export")).toBe(1);
    });
    const sent = bridge.inputFor("report_export") as {
      input: { format: string; path: string };
    };
    expect(sent.input.format).toBe("csv");
    expect(sent.input.path).toBe("D:\\reports\\batch-1-csv.csv");
    expect(await screen.findByText(/已导出 1 行到本地文件/)).toBeTruthy();
  });

  it("导出失败显示可重试的原因，不丢失报告", async () => {
    const bridge = new FakeBridge(withReport(report())).failWith(
      "report_export",
      ipcError({
        code: "ipc.export",
        category: "disk",
        retryable: true,
        stage: "report",
        safe_message: "导出目录不存在或不可访问",
        action: "请选择已存在的目录后重试",
      }),
    );
    const store = await mount(bridge);

    fireEvent.change(screen.getByLabelText(/导出目录/), { target: { value: "D:\\missing" } });
    fireEvent.click(screen.getByRole("button", { name: /导出 JSON/ }));

    await waitFor(() => {
      expect(store.getState().error?.safe_message).toBe("导出目录不存在或不可访问");
    });
    // The report itself is still on screen.
    expect(screen.getByRole("table")).toBeTruthy();
  });

  it("导出说明文件不含令牌但含仓库 URL", async () => {
    await mount(new FakeBridge(withReport(report())));
    expect(screen.getByText(/不含令牌、密码或认证头/)).toBeTruthy();
    expect(screen.getByText(/会包含仓库 URL、错误信息和本地临时目录位置/)).toBeTruthy();
  });

  it("清理失败时给出路径和手动删除指引", async () => {
    await mount(
      new FakeBridge(
        withReport(
          report({
            cleanup: {
              type: "cleanup_failed",
              path: "D:\\workspace\\tmp\\batch-1",
              reason: "目录被其他进程占用",
            },
          }),
        ),
      ),
    );
    expect(screen.getByText("临时工作目录清理失败")).toBeTruthy();
    expect(screen.getByText("D:\\workspace\\tmp\\batch-1")).toBeTruthy();
    expect(screen.getByText(/不会删除该目录以外的任何内容/)).toBeTruthy();
  });

  it("保留临时目录时说明风险", async () => {
    await mount(
      new FakeBridge(
        withReport(
          report({
            cleanup: { type: "retained_temp_directory", path: "D:\\workspace\\tmp\\batch-1" },
          }),
        ),
      ),
    );
    expect(screen.getByText(/按设置保留了临时工作目录/)).toBeTruthy();
    expect(screen.getByText(/请在确认迁移无误后手动删除/)).toBeTruthy();
  });
});
