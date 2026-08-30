/**
 * The packaged Windows application (T-030 / T-031, `desktop` project).
 *
 * The `webview` specs prove the GUI; this one proves the *application* — the
 * real Rust backend, the real SQLite file and the real command surface, driven
 * through the WebView2 debugging endpoint.
 *
 * It needs an executable built with the E2E window config, because the
 * debugging port cannot be injected at runtime: wry passes its own browser
 * arguments, so `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` is ignored by the
 * WebView2 loader. Build it like this:
 *
 * ```
 * npx tauri build --prefix apps/desktop --no-bundle --config src-tauri/tauri.e2e.conf.json
 * set E2E_TAURI_BINARY=target\release\git-repo-migrator-desktop.exe
 * npx playwright test --project=desktop
 * ```
 *
 * A skip here is not a pass. `docs/release-checklist.md` requires a recorded run
 * of this project on Windows 10 and Windows 11 before a release is signed.
 */

import { spawn } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { chromium, expect, test } from "@playwright/test";
import type { Browser, Page } from "@playwright/test";

const BINARY = process.env.E2E_TAURI_BINARY;
const DEBUG_PORT = Number(process.env.E2E_CDP_PORT ?? 9222);
const CDP_ENDPOINT = `http://127.0.0.1:${DEBUG_PORT}`;

test.skip(
  !BINARY,
  "set E2E_TAURI_BINARY to a built git-repo-migrator.exe to run the packaged-application project",
);

let app: ChildProcess | undefined;
let browser: Browser | undefined;
let dataDir: string | undefined;

/** Set when the application process itself dies — the most common CI failure. */
let appExit: { code: number | null; signal: string | null } | null = null;
/** Tail of the application's own output; a panicked backend reports itself here. */
let appOutput = "";

/** Renders everything we know about the application process for error messages. */
function appVitals(): string {
  const exit = appExit
    ? `the application exited (code ${appExit.code ?? "?"}, signal ${appExit.signal ?? "?"})`
    : "the application process is still running";
  const output = appOutput.trim()
    ? `\n--- application output (tail) ---\n${appOutput.trim()}`
    : "\nthe application produced no output";
  return `${exit}.${output}`;
}

/** Launches the packaged application isolated in `dataDir`. */
function launchApp(): ChildProcess {
  if (!dataDir) throw new Error("dataDir not initialised");
  appExit = null;
  appOutput = "";
  const child = spawn(BINARY as string, [], {
    env: {
      ...process.env,
      // The WebView2 user-data folder decides which *browser process* hosts the
      // window. Tauri resolves it through the Windows known-folder API, which
      // ignores LOCALAPPDATA/APPDATA, so it has to be overridden explicitly —
      // otherwise the run attaches to a developer's already-running browser
      // process and the debugging port never appears.
      WEBVIEW2_USER_DATA_FOLDER: join(dataDir, "webview"),
      LOCALAPPDATA: dataDir,
      APPDATA: dataDir,
    },
    // A GUI application rarely writes here, but a panic on startup does. The
    // tail is reported when the debugging endpoint never appears — without it
    // a crashed backend is indistinguishable from a missing port.
    stdio: ["ignore", "pipe", "pipe"],
  });
  const collect = (chunk: Buffer) => {
    appOutput = (appOutput + chunk.toString("utf8")).slice(-4000);
  };
  child.stdout?.on("data", collect);
  child.stderr?.on("data", collect);
  // Assign before wiring the exit handler: a process that dies immediately
  // (missing loader, bad architecture) must still record its exit.
  app = child;
  // Only the *current* instance updates the vitals: the restart test kills an
  // old child whose exit event may land after the replacement has launched.
  child.on("exit", (code, signal) => {
    if (app === child) appExit = { code, signal };
  });
  return child;
}

/** Waits for WebView2 to publish its debugging endpoint. */
async function connect(): Promise<Browser> {
  const deadline = Date.now() + 60_000;
  let lastError: unknown;
  while (Date.now() < deadline) {
    if (appExit) {
      // The host is gone — no amount of retrying will open the port.
      throw new Error(
        `the packaged application died before WebView2 published its debugging endpoint. ` +
          `${appVitals()}`,
      );
    }
    try {
      return await chromium.connectOverCDP(CDP_ENDPOINT);
    } catch (cause) {
      lastError = cause;
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
  }
  throw new Error(
    `could not attach to WebView2 at ${CDP_ENDPOINT}: ${String(lastError)}. ${appVitals()}`,
  );
}

async function appPage(): Promise<Page> {
  const contexts = browser?.contexts() ?? [];
  for (const context of contexts) {
    const [page] = context.pages();
    if (page) return page;
  }
  throw new Error("the application window did not expose a page");
}

test.beforeAll(async () => {
  if (!BINARY) return;
  // A dedicated app-data directory keeps the run from touching a developer's
  // real migration state, and lets the restart test start from a known state.
  dataDir = mkdtempSync(join(tmpdir(), "git-repo-migrator-e2e-"));
  app = launchApp();
  browser = await connect();
});

test.afterAll(async () => {
  await browser?.close().catch(() => undefined);
  app?.kill();
  if (dataDir) {
    // WebView2's crashpad handler outlives its host for a moment and keeps
    // metrics files locked; retry the removal instead of failing the run.
    for (let attempt = 0; attempt < 10; attempt += 1) {
      try {
        rmSync(dataDir, { recursive: true, force: true });
        break;
      } catch {
        await new Promise((resolve) => setTimeout(resolve, 500));
      }
    }
  }
});

test.describe("打包后的 Windows 应用", () => {
  test("窗口启动后显示连接页，并从本地状态库读取快照", async () => {
    const page = await appPage();
    await expect(page.getByRole("heading", { name: "连接 Git 平台" })).toBeVisible();
    await expect(page.getByText("数据仅在本机处理")).toBeVisible();

    // The real backend answered: an empty store still produces a valid snapshot.
    await expect(page.getByText("请先保存源平台和目标平台连接")).toBeVisible();
  });

  test("渲染进程拿不到 shell、文件系统或凭据能力", async () => {
    const page = await appPage();
    const exposed = await page.evaluate(() => {
      const internals = (window as Record<string, any>).__TAURI_INTERNALS__;
      return {
        hasShell: Boolean((window as Record<string, any>).__TAURI__?.shell),
        hasFs: Boolean((window as Record<string, any>).__TAURI__?.fs),
        hasInvoke: typeof internals?.invoke === "function",
      };
    });
    expect(exposed.hasShell, "the renderer must not reach a shell plugin").toBe(false);
    expect(exposed.hasFs, "the renderer must not reach a file system plugin").toBe(false);
    expect(exposed.hasInvoke, "the whitelisted command channel must exist").toBe(true);
  });

  test("未在白名单中的命令会被后端拒绝", async () => {
    const page = await appPage();
    const outcome = await page.evaluate(async () => {
      const internals = (window as Record<string, any>).__TAURI_INTERNALS__;
      try {
        await internals.invoke("credential_read", { input: { name: "source" } });
        return "accepted";
      } catch (cause) {
        return `rejected: ${String(cause)}`;
      }
    });
    expect(outcome, "an unlisted command must never be accepted").toContain("rejected");
  });

  test("保存的连接在重启后仍然存在", async () => {
    const page = await appPage();
    const source = page.getByRole("region", { name: "源平台" });
    await source.getByLabel("服务地址").fill("https://git.source.test");
    await source.getByLabel(/凭据引用/).fill("credential/windows/1a2b3c4d");
    await source.getByRole("button", { name: "保存连接" }).click();
    await expect(source.getByText("credential/windows/1a2b3c4d")).toBeVisible();

    // Restart against the same app-data directory: SQLite is the source of truth.
    await browser?.close().catch(() => undefined);
    app?.kill();
    await new Promise((resolve) => setTimeout(resolve, 2000));
    app = launchApp();
    browser = await connect();

    const restarted = await appPage();
    await expect(
      restarted.getByRole("region", { name: "源平台" }).getByText("credential/windows/1a2b3c4d"),
    ).toBeVisible();
  });
});
