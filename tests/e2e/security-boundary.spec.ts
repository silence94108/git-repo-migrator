/**
 * Security boundary (T-031, CM-004).
 *
 * The claim under test is narrow and checkable: **nothing the renderer can do
 * moves a secret, reaches the file system, or talks to a network.** So this spec
 * scans the actual traffic and the actual command payloads instead of reading
 * the source.
 *
 * It also guards the one thing that would quietly invalidate the rest of this
 * suite: the E2E bridge seam must not survive into a production build.
 */

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

import { expect, test } from "@playwright/test";

import {
  COMMAND_WHITELIST,
  connectedSnapshot,
  installBridge,
  recordedCalls,
} from "./fixtures/platform-fixtures";

/** Substrings that must never appear in an outbound command payload. */
const FORBIDDEN_KEYS = [
  "token",
  "access_token",
  "private_token",
  "password",
  "secret",
  "private_key",
  "cookie",
  "authorization",
  "response_body",
];

const DIST = join(__dirname, "..", "..", "apps", "desktop", "dist");

test.describe("安全边界", () => {
  test("渲染进程只调用白名单命令", async ({ page }) => {
    await installBridge(page, { snapshot: connectedSnapshot(4) });
    await page.goto("/");

    // Walk the whole flow so every command the UI can reach gets exercised.
    await page.getByRole("button", { name: /选择仓库/ }).click();
    await page.getByRole("button", { name: /选择全部筛选结果/ }).click();
    await page.getByRole("button", { name: /继续设置映射/ }).click();
    await page.getByRole("button", { name: /保存映射并运行预检/ }).click();
    const ack = page.getByRole("region", { name: "保真度确认" }).getByRole("checkbox");
    if (await ack.count()) await ack.first().check();
    await page.getByRole("button", { name: /冻结计划并开始迁移/ }).click();
    const toolbar = page.getByRole("region", { name: "批次工具栏" });
    await toolbar.getByRole("button", { name: "暂停", exact: true }).click();
    await toolbar.getByRole("button", { name: "继续", exact: true }).click();

    const calls = await recordedCalls(page);
    expect(calls.length).toBeGreaterThan(5);
    for (const call of calls) {
      expect(
        COMMAND_WHITELIST as readonly string[],
        `renderer called an unlisted command: ${call.command}`,
      ).toContain(call.command);
    }
  });

  test("没有任何命令载荷包含 secret 字段", async ({ page }) => {
    await installBridge(page, { snapshot: connectedSnapshot(4) });
    await page.goto("/");

    await page.getByRole("region", { name: "源平台" }).getByRole("button", { name: "录入令牌" }).click();
    await page.getByRole("button", { name: /选择仓库/ }).click();
    await page.getByRole("button", { name: /选择全部筛选结果/ }).click();
    await page.getByRole("button", { name: /继续设置映射/ }).click();
    await page.getByRole("button", { name: /保存映射并运行预检/ }).click();

    const calls = await recordedCalls(page);
    for (const call of calls) {
      const payload = JSON.stringify(call.input ?? {}).toLowerCase();
      for (const key of FORBIDDEN_KEYS) {
        expect(payload, `${call.command} payload contains ${key}`).not.toContain(`"${key}"`);
      }
    }

    // The credential entry command in particular: a name in, nothing else.
    const authorize = calls.find((call) => call.command === "connection_authorize");
    expect(authorize).toBeTruthy();
    expect(Object.keys((authorize?.input as any)?.input ?? {})).toEqual(["name"]);
  });

  test("界面不提供令牌输入框，凭据只以引用出现", async ({ page }) => {
    await installBridge(page, { snapshot: connectedSnapshot(3) });
    await page.goto("/");

    expect(await page.locator('input[type="password"]').count()).toBe(0);
    const body = await page.locator("body").innerText();
    // A reference is fine; a token shape is not.
    expect(body).toContain("credential/windows/");
    for (const shape of ["ghp_", "glpat-", "-----BEGIN", "Bearer "]) {
      expect(body, `a token-shaped string reached the DOM: ${shape}`).not.toContain(shape);
    }
  });

  test("整个流程不发起任何外部网络请求", async ({ page }) => {
    const external: string[] = [];
    page.on("request", (request) => {
      const url = request.url();
      if (!url.startsWith("http://127.0.0.1") && !url.startsWith("data:") && !url.startsWith("blob:")) {
        external.push(url);
      }
    });

    await installBridge(page, { snapshot: connectedSnapshot(4) });
    await page.goto("/");
    await page.getByRole("button", { name: /选择仓库/ }).click();
    await page.getByRole("button", { name: /选择全部筛选结果/ }).click();
    await page.getByRole("button", { name: /继续设置映射/ }).click();
    await page.getByRole("button", { name: /保存映射并运行预检/ }).click();

    // The renderer talks to the backend, never to a platform directly.
    expect(external, "the renderer must not reach the network itself").toEqual([]);
  });

  test("导出前明确说明文件内容，并且路径由用户提供", async ({ page }) => {
    await installBridge(page, {
      snapshot: connectedSnapshot(3, {
        active_plan: {
          plan_id: "plan-e2e",
          plan_hash: "e".repeat(64),
          status: "frozen",
          repository_count: 1,
          capability_snapshot_hash: "cap-e2e",
          dangerous_confirmed: false,
          created_at_ms: 1_760_000_000_000,
        },
        active_batch: {
          batch_id: "batch-e2e",
          plan_id: "plan-e2e",
          plan_hash: "e".repeat(64),
          control: "completed",
          concurrency: 1,
          total: 1,
          completed: 1,
          failed: 0,
          started_at_ms: 1_760_000_000_500,
          ended_at_ms: 1_760_000_009_000,
          logs: [],
          tasks: [],
        },
        report: {
          batch_id: "batch-e2e",
          metrics: {
            complete_success: 1,
            git_success_platform_partial: 0,
            retryable_failure: 0,
            permission_or_conflict_skip: 0,
          },
          cleanup: { type: "cleaned" },
          rows: [
            {
              task_id: "task-0",
              source_url: "https://git.source.test/ops/repo-000.git",
              target_url: "https://git.target.test/ops/repo-000",
              status: "succeeded",
              completed_at_ms: 1_760_000_003_000,
              git_verified: true,
              lfs_verified: true,
              metadata_verified: true,
              modules: [],
              error_code: null,
              evidence: {
                refs_checked: 7,
                refs_missing: 0,
                lfs_checked: 2,
                lfs_missing: 0,
                metadata_checked: true,
                excluded_refs: ["refs/pull/1/head"],
              },
              unmapped_fields: [],
              archive_path: null,
              source_links: [],
              next_action: null,
            },
          ],
        },
      }),
    });
    await page.goto("/#/report");

    const exportPanel = page.getByRole("region", { name: "导出" });
    await expect(exportPanel).toContainText("导出文件不含令牌、密码或认证头");
    // The renderer never picks a path on its own; there is an explicit field.
    await expect(exportPanel.getByLabel(/导出目录/)).toBeVisible();
  });

  /**
   * The webview project only works because the production store accepts an
   * injected bridge in non-production builds. If that branch ever survived a
   * release build, a page in the shipped app could replace the backend.
   */
  test("生产构建里没有 E2E bridge 注入点", async () => {
    const assets = join(DIST, "assets");
    const bundles = readdirSync(assets).filter((name) => name.endsWith(".js"));
    expect(bundles.length, "run `npm run build` before this spec").toBeGreaterThan(0);

    for (const name of bundles) {
      const source = readFileSync(join(assets, name), "utf8");
      expect(source, `${name} still contains the E2E bridge seam`).not.toContain(
        "__migrationBridge",
      );
    }
  });
});
