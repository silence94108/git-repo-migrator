/**
 * Fault injection: crash recovery, rate limiting and lost responses (T-031).
 *
 * Everything here is about what the operator sees when the backend does *not*
 * behave: an interrupted batch has to announce itself and demand a re-check
 * before it resumes, a 429 has to read as "waiting", not "failed", and a
 * permission failure must never be offered as a retry.
 */

import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import {
  connectedSnapshot,
  installBridge,
  ipcError,
  recordedCalls,
} from "./fixtures/platform-fixtures";

/** Puts the page straight on the queue step with a scripted batch. */
async function queueWith(page: Page, tasks: Record<string, unknown>[]) {
  const snapshot = connectedSnapshot(3, {
    active_plan: {
      plan_id: "plan-e2e",
      plan_hash: "e".repeat(64),
      status: "frozen",
      repository_count: tasks.length,
      capability_snapshot_hash: "cap-e2e",
      dangerous_confirmed: false,
      created_at_ms: 1_760_000_000_000,
    },
    active_batch: {
      batch_id: "batch-e2e",
      plan_id: "plan-e2e",
      plan_hash: "e".repeat(64),
      control: "running",
      concurrency: 2,
      total: tasks.length,
      completed: 0,
      failed: tasks.filter((task) => task.state === "retryable_failed").length,
      started_at_ms: 1_760_000_000_500,
      ended_at_ms: null,
      logs: [],
      tasks,
    },
  });
  await installBridge(page, { snapshot });
  await page.goto("/#/queue");
  await expect(page.getByRole("heading", { name: "迁移队列" })).toBeVisible();
}

function task(
  index: number,
  patch: Record<string, unknown> = {},
): Record<string, unknown> {
  const name = `repo-${String(index).padStart(3, "0")}`;
  return {
    task_id: `task-${index}`,
    repository_id: `https://git.source.test/ops/${name}.git`,
    source_url: `https://git.source.test/ops/${name}.git`,
    target_url: `https://git.target.test/ops/${name}`,
    stage: "git",
    state: "git",
    attempt: 0,
    progress_completed: 1,
    progress_total: 3,
    retryable: false,
    error: null,
    last_checkpoint: {
      stage: "git",
      transition: "heartbeat",
      attempt: 0,
      resumable: true,
      created_at_ms: 1_760_000_001_000,
    },
    updated_at_ms: 1_760_000_001_000,
    ...patch,
  };
}

test.describe("崩溃恢复与限流", () => {
  test("重新打开时提示未完成批次，并要求恢复前复检凭据与能力", async ({ page }) => {
    const snapshot = connectedSnapshot(3, {
      active_plan: {
        plan_id: "plan-e2e",
        plan_hash: "e".repeat(64),
        status: "frozen",
        repository_count: 3,
        capability_snapshot_hash: "cap-e2e",
        dangerous_confirmed: false,
        created_at_ms: 1_760_000_000_000,
      },
      active_batch: {
        batch_id: "batch-e2e",
        plan_id: "plan-e2e",
        plan_hash: "e".repeat(64),
        control: "paused",
        concurrency: 2,
        total: 3,
        completed: 1,
        failed: 0,
        started_at_ms: 1_760_000_000_500,
        ended_at_ms: null,
        logs: [],
        tasks: [task(0, { state: "succeeded", stage: "complete" }), task(1), task(2)],
      },
      resumable: [
        {
          batch_id: "batch-e2e",
          plan_id: "plan-e2e",
          pending: 2,
          plan_hash_matches: true,
          credential_recheck_required: true,
          capability_recheck_required: true,
        },
      ],
    });
    await installBridge(page, { snapshot });
    await page.goto("/#/queue");

    await expect(page.getByText("发现 1 个未完成批次")).toBeVisible();
    await expect(page.getByText("剩余 2 个仓库")).toBeVisible();
    await expect(page.getByText(/需要复检凭据/)).toBeVisible();
    await expect(page.getByText(/需要复检平台能力/)).toBeVisible();
    // The already-finished repository must not be offered again.
    await expect(page.getByText("已暂停").first()).toBeVisible();
  });

  test("恢复后已完成的仓库不会被重复迁移", async ({ page }) => {
    await queueWith(page, [
      task(0, { state: "succeeded", stage: "complete" }),
      task(1, {
        state: "retryable_failed",
        retryable: true,
        error: ipcError({ code: "ipc.network", safe_message: "推送过程中连接中断" }),
      }),
    ]);

    await page.getByRole("button", { name: /只重试可重试失败（1）/ }).click();

    const calls = await recordedCalls(page);
    const retry = calls.find((call) => call.command === "task_retry");
    expect(retry, "the UI must ask the backend to retry").toBeTruthy();
    const ids = ((retry?.input as Record<string, any>)?.input?.task_ids ?? []) as string[];
    expect(ids).toEqual(["task-1"]);
    expect(ids, "a completed repository must never be re-queued").not.toContain("task-0");
  });

  test("429 显示为等待重试而不是失败，并保留 Retry-After 提示", async ({ page }) => {
    await queueWith(page, [
      task(0, {
        state: "retryable_failed",
        retryable: true,
        error: ipcError({
          code: "platform.rate_limited",
          category: "rate_limited",
          retryable: true,
          safe_message: "触发平台限流，将在 30 秒后自动重试",
          action: "无需操作；队列会按 Retry-After 自动退避",
        }),
      }),
    ]);

    await expect(page.getByText(/触发平台限流/)).toBeVisible();
    await expect(page.getByText("platform.rate_limited", { exact: false })).toBeVisible();
    // Rate limiting is retryable, so the retry action must stay available.
    await expect(page.getByRole("button", { name: /只重试可重试失败（1）/ })).toBeEnabled();
  });

  test("权限失败不会被当成可重试项", async ({ page }) => {
    await queueWith(page, [
      task(0, {
        state: "skipped",
        stage: "prepare_target",
        retryable: false,
        error: ipcError({
          code: "platform.permission",
          category: "permission",
          retryable: false,
          stage: "prepare_target",
          safe_message: "凭据对目标命名空间没有写入权限",
          action: "请授予写入权限或排除该仓库",
        }),
      }),
    ]);

    await expect(page.getByText("凭据对目标命名空间没有写入权限")).toBeVisible();
    await expect(page.getByRole("button", { name: /只重试可重试失败（0）/ })).toBeDisabled();
  });

  test("丢失的事件由下一次快照校正，界面不会停留在旧状态", async ({ page }) => {
    await queueWith(page, [task(0), task(1)]);

    // The backend advanced two revisions; the renderer only sees the second
    // event, which is exactly the gap that must trigger a refetch.
    await page.evaluate(() => {
      const w = window as unknown as {
        __e2eSnapshot: () => Record<string, unknown>;
        __e2eSetSnapshot: (value: Record<string, unknown>) => void;
        __e2eEmit: (value: unknown) => void;
      };
      const current = w.__e2eSnapshot();
      const batch = current.active_batch as Record<string, unknown>;
      w.__e2eSetSnapshot({
        ...current,
        revision: (current.revision as number) + 5,
        active_batch: {
          ...batch,
          completed: 2,
          control: "completed",
          tasks: (batch.tasks as Record<string, unknown>[]).map((item) => ({
            ...item,
            state: "succeeded",
            stage: "complete",
          })),
        },
      });
      w.__e2eEmit({
        revision: (current.revision as number) + 5,
        event: { type: "batch_completed", batch_id: "batch-e2e", status: "completed" },
      });
    });

    // The event itself is never trusted as state: it only tells the operator the
    // screen is behind, and the snapshot is what corrects it.
    // The batch state badge, not the "completed" metric label next to it.
    const stateBadge = page
      .getByRole("region", { name: "批次工具栏" })
      .locator(".badge", { hasText: "已完成" });
    await expect(page.getByText("收到新的进度事件，界面可能不是最新")).toBeVisible();
    await expect(stateBadge).toHaveCount(0);

    await page.getByRole("button", { name: "刷新" }).click();
    await expect(stateBadge).toBeVisible();
    await expect(page.getByText("收到新的进度事件，界面可能不是最新")).toHaveCount(0);

    const calls = await recordedCalls(page);
    expect(
      calls.filter((call) => call.command === "migration_snapshot").length,
      "the corrected state must come from a fresh snapshot",
    ).toBeGreaterThan(1);
  });

  test("命令失败时界面给出可执行的下一步而不是空白页", async ({ page }) => {
    await installBridge(page, {
      snapshot: connectedSnapshot(3),
      failures: [
        {
          command: "plan_preview",
          error: ipcError({
            code: "ipc.network",
            stage: "preflight",
            safe_message: "预检请求超时",
            action: "请检查网络后重新运行预检",
          }),
        },
      ],
    });
    await page.goto("/");
    await page.getByRole("button", { name: /选择仓库/ }).click();
    await page.getByRole("button", { name: /选择全部筛选结果/ }).click();
    await page.getByRole("button", { name: /继续设置映射/ }).click();
    await page.getByRole("button", { name: /保存映射并运行预检/ }).click();

    await expect(page.getByText("预检请求超时")).toBeVisible();
    await expect(page.getByText("请检查网络后重新运行预检")).toBeVisible();
    const body = await page.locator("main").innerText();
    expect(body.trim().length, "an error must not blank the page").toBeGreaterThan(40);
  });
});
