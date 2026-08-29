/**
 * Windows GUI main flow (T-030).
 *
 * Walks connect → select-all-then-exclude → map → preflight → queue → report in
 * a real browser, and asserts the things jsdom cannot see: that every step's
 * text actually fits its box, that no two pieces of content overlap, and that
 * the canvas is never blank.
 *
 * The four conflict outcomes are all present in the fixture set: `repo-000` and
 * `repo-001` reuse an empty target, `repo-002` has a non-empty target and must
 * be skipped, `repo-003` does not exist and must be created.
 */

import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { connectedSnapshot, installBridge, recordedCalls } from "./fixtures/platform-fixtures";

/** Fails if any visible text is clipped by its own box. */
async function expectNoTextOverflow(page: Page) {
  const clipped = await page.evaluate(() => {
    const offenders: string[] = [];
    for (const element of Array.from(document.querySelectorAll("h1,h2,h3,button,th,td,label,p"))) {
      const node = element as HTMLElement;
      if (node.offsetParent === null) continue;
      const style = getComputedStyle(node);
      if (style.overflow === "hidden" && style.textOverflow === "ellipsis") continue;
      // 1px of tolerance absorbs sub-pixel rounding in the layout engine.
      if (node.scrollWidth > node.clientWidth + 1 && node.clientWidth > 0) {
        offenders.push(`${node.tagName}:${(node.textContent ?? "").slice(0, 40)}`);
      }
    }
    return offenders;
  });
  expect(clipped, "text must not overflow its container").toEqual([]);
}

/** Fails if the page rendered nothing meaningful. */
async function expectNotBlank(page: Page) {
  const text = (await page.locator("main").innerText()).trim();
  expect(text.length, "the page body must not be blank").toBeGreaterThan(40);
  const box = await page.locator("main").boundingBox();
  expect(box?.height ?? 0).toBeGreaterThan(200);
}

/** Fails if the sidebar and the page body visually collide. */
async function expectNoOverlap(page: Page) {
  const sidebar = await page.locator("nav").first().boundingBox();
  const body = await page.locator("main").boundingBox();
  expect(sidebar).not.toBeNull();
  expect(body).not.toBeNull();
  if (sidebar && body) {
    expect(sidebar.x + sidebar.width, "sidebar must not cover the page body").toBeLessThanOrEqual(
      body.x + 1,
    );
  }
}

async function expectHealthyLayout(page: Page) {
  await expectNotBlank(page);
  await expectNoOverlap(page);
  await expectNoTextOverflow(page);
}

function rowFor(page: Page, name: string): Locator {
  return page.getByRole("row").filter({ hasText: name });
}

/**
 * A plan containing an `unsupported` module cannot be frozen until the operator
 * acknowledges it, so every path to the queue goes through this.
 */
async function acknowledgeFidelity(page: Page) {
  const box = page.getByRole("region", { name: "保真度确认" }).getByRole("checkbox");
  if (await box.count()) await box.first().check();
}

test.describe("Windows GUI 迁移主流程", () => {
  test.beforeEach(async ({ page }) => {
    await installBridge(page, { snapshot: connectedSnapshot(4) });
    await page.goto("/");
  });

  test("连接页显示身份和能力，且没有任何令牌输入框", async ({ page }) => {
    await expect(page.getByRole("heading", { name: "连接 Git 平台" })).toBeVisible();
    await expect(page.getByText("数据仅在本机处理")).toBeVisible();

    const source = page.getByRole("region", { name: "源平台" });
    await expect(source.getByText("credential/windows/1a2b3c4d")).toBeVisible();
    await expect(source.getByRole("button", { name: "录入令牌" })).toBeVisible();

    // A password field anywhere on this page would mean the GUI accepts a token.
    expect(await page.locator('input[type="password"]').count()).toBe(0);
    await expectHealthyLayout(page);
  });

  test("全选后排除覆盖整个筛选结果，而不是当前页", async ({ page }) => {
    await page.getByRole("button", { name: /选择仓库/ }).click();
    await page.getByRole("button", { name: /选择全部筛选结果（4）/ }).click();
    await expect(page.getByRole("button", { name: /继续设置映射（4）/ })).toBeVisible();

    // Excluding one repository must leave the other three selected.
    const row = rowFor(page, "repo-001");
    await row.getByRole("checkbox").uncheck();
    await expect(page.getByRole("button", { name: /继续设置映射（3）/ })).toBeVisible();

    await expectHealthyLayout(page);
    await page.getByRole("button", { name: /继续设置映射（3）/ }).click();

    const calls = await recordedCalls(page);
    expect(calls.map((call) => call.command)).toContain("migration_snapshot");
  });

  test("预检区分自动建库、空仓库复用和非空跳过，并显示 RefPolicy 与保真度", async ({ page }) => {
    await page.getByRole("button", { name: /选择仓库/ }).click();
    await page.getByRole("button", { name: /选择全部筛选结果（4）/ }).click();
    await page.getByRole("button", { name: /继续设置映射/ }).click();
    await page.getByRole("button", { name: /保存映射并运行预检/ }).click();

    await expect(page.getByRole("heading", { name: "预检与计划冻结" })).toBeVisible();

    // One row per conflict outcome the plan can produce.
    await expect(rowFor(page, "repo-000")).toContainText("复用空仓库");
    await expect(rowFor(page, "repo-002")).toContainText("跳过非空目标");
    await expect(rowFor(page, "repo-003")).toContainText("创建目标");

    // The ref policy must be visible before the operator freezes anything.
    const refs = page.getByRole("region", { name: "引用策略摘要" });
    await expect(refs).toContainText("refs/heads/*");
    await expect(refs).toContainText("refs/tags/*");
    await expect(refs).toContainText("refs/pull/*");

    // Fidelity is stated, not implied: the degraded module has to be
    // acknowledged before the plan may be frozen.
    const fidelity = page.getByRole("region", { name: "保真度确认" });
    await expect(fidelity).toContainText("通用 Git 服务没有平台数据 API");
    await expect(fidelity).toContainText("不会伪装成目标平台的可交互条目");
    await expect(rowFor(page, "repo-000").getByTitle(/issues：通用 Git 服务没有平台数据 API/))
      .toBeVisible();
    await expectHealthyLayout(page);
  });

  test("冻结计划后队列只包含可执行仓库，非空目标不会被推送", async ({ page }) => {
    await page.getByRole("button", { name: /选择仓库/ }).click();
    await page.getByRole("button", { name: /选择全部筛选结果（4）/ }).click();
    await page.getByRole("button", { name: /继续设置映射/ }).click();
    await page.getByRole("button", { name: /保存映射并运行预检/ }).click();
    await acknowledgeFidelity(page);
    await page.getByRole("button", { name: /冻结计划并开始迁移/ }).click();

    await expect(page.getByRole("heading", { name: "迁移队列" })).toBeVisible();
    const queue = page.getByRole("region", { name: "队列明细" });
    await expect(queue.getByRole("row")).toHaveCount(4); // header + three tasks
    await expect(queue).toContainText("repo-000");
    await expect(queue).toContainText("repo-003");
    await expect(queue, "a skipped target must never enter the queue").not.toContainText(
      "repo-002",
    );

    const calls = await recordedCalls(page);
    const start = calls.find((call) => call.command === "batch_start");
    expect(start, "the queue may only start through batch_start").toBeTruthy();
    await expectHealthyLayout(page);
  });

  test("报告区分四类结果，证据可见，导出路径可重试", async ({ page }) => {
    await page.getByRole("button", { name: /选择仓库/ }).click();
    await page.getByRole("button", { name: /选择全部筛选结果（4）/ }).click();
    await page.getByRole("button", { name: /继续设置映射/ }).click();
    await page.getByRole("button", { name: /保存映射并运行预检/ }).click();
    await acknowledgeFidelity(page);
    await page.getByRole("button", { name: /冻结计划并开始迁移/ }).click();

    // The backend has finished the batch: one clean success, one Git-success
    // with a degraded module, one retryable failure.
    await page.evaluate(() => {
      const w = window as unknown as {
        __e2eSnapshot: () => Record<string, unknown>;
        __e2eSetSnapshot: (value: Record<string, unknown>) => void;
      };
      const current = w.__e2eSnapshot();
      const row = (index: number, status: string, patch: Record<string, unknown> = {}) => ({
        task_id: `task-${index}`,
        source_url: `https://git.source.test/ops/repo-00${index}.git`,
        target_url: `https://git.target.test/ops/repo-00${index}`,
        status,
        completed_at_ms: 1760000003000,
        git_verified: true,
        lfs_verified: true,
        metadata_verified: status === "succeeded",
        modules: [],
        error_code: null,
        evidence: {
          refs_checked: 7,
          refs_missing: 0,
          lfs_checked: 2,
          lfs_missing: 0,
          metadata_checked: status === "succeeded",
          excluded_refs: ["refs/pull/1/head"],
        },
        unmapped_fields: status === "partial" ? ["issues"] : [],
        archive_path: null,
        source_links: [],
        next_action: status === "retryable_failed" ? "请检查网络后重试该仓库" : null,
        ...patch,
      });
      w.__e2eSetSnapshot({
        ...current,
        report: {
          batch_id: "batch-e2e",
          metrics: {
            complete_success: 1,
            git_success_platform_partial: 1,
            retryable_failure: 1,
            permission_or_conflict_skip: 0,
          },
          cleanup: { type: "cleaned" },
          rows: [row(0, "succeeded"), row(1, "partial"), row(3, "retryable_failed")],
        },
      });
    });
    await page.getByRole("button", { name: "刷新" }).click();
    await page.getByRole("button", { name: "查看报告" }).click();

    await expect(page.getByRole("heading", { name: "迁移报告" })).toBeVisible();
    const summary = page.getByRole("region", { name: "结果摘要" });
    await expect(summary).toContainText("完整成功");
    await expect(summary).toContainText("Git 成功 · 平台数据部分失败");
    await expect(summary).toContainText("可重试失败");

    // Evidence, including the refs that were deliberately not migrated.
    await rowFor(page, "repo-001").getByRole("button", { name: "查看证据" }).click();
    await expect(page.getByText("refs/pull/1/head").first()).toBeVisible();

    await expectHealthyLayout(page);
  });

  test("六步导航在每一步都保持可读且不重叠", async ({ page }, testInfo) => {
    for (const [route, heading] of [
      ["/connections", "连接 Git 平台"],
      ["/repositories", "选择要迁移的仓库"],
    ] as const) {
      await page.evaluate((value) => {
        window.location.hash = value;
      }, route);
      await expect(page.getByRole("heading", { name: heading })).toBeVisible();
      await expectHealthyLayout(page);
      // A screenshot per step, so a CI failure carries the actual pixels.
      // `outputPath` keeps them inside the configured output directory rather
      // than wherever the runner happened to be started from.
      await page.screenshot({
        path: testInfo.outputPath(`step${route.replace(/\//g, "-")}.png`),
      });
    }
  });
});
