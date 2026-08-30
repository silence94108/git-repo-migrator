/**
 * Windows GUI E2E harness (T-030 owner file).
 *
 * Two projects, because "the GUI works" has two independent failure modes:
 *
 * * **webview** — the real production bundle in a real Chromium, driven through
 *   the scripted backend in `tests/e2e/fixtures/platform-fixtures.ts`. This is
 *   what catches layout overlap, text overflow, blank canvases and the
 *   selection/queue responsiveness budget, none of which jsdom can see. It runs
 *   everywhere, including CI, with no Tauri toolchain.
 * * **desktop** — the packaged Tauri application, attached over the WebView2
 *   CDP endpoint, exercising the real Rust backend, real SQLite and real Git.
 *   It only runs when `E2E_TAURI_BINARY` points at a built executable, so a
 *   developer without a Windows build still gets the webview gate.
 *
 * The bundle under test is built with `--mode e2e`, which is the only mode where
 * the renderer will accept an injected bridge; the production `npm run build`
 * output has that branch compiled out and the security spec asserts it.
 */

import { defineConfig, devices } from "@playwright/test";

const PORT = Number(process.env.E2E_PORT ?? 4173);
const BASE_URL = process.env.E2E_BASE_URL ?? `http://127.0.0.1:${PORT}`;

export default defineConfig({
  testDir: "../../tests/e2e",
  // Windows 10 and 11 agents are slower than a developer machine, and a real
  // browser render of 100+ rows is genuinely slow the first time.
  timeout: 90_000,
  expect: { timeout: 15_000 },
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  workers: process.env.CI ? 2 : undefined,
  reporter: process.env.CI
    ? [
        ["list"],
        ["html", { outputFolder: "playwright-report", open: "never" }],
        // Consumed by the CI "surface failures" step: GitHub annotations let
        // the failure reason be read without downloading an artifact.
        ["json", { outputFile: "playwright-report/results.json" }],
      ]
    : [["list"]],
  outputDir: "test-results",

  use: {
    baseURL: BASE_URL,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "off",
    // Windows 10 laptop default. The layout assertions are written against this
    // viewport, so changing it changes what "no overlap" means.
    viewport: { width: 1366, height: 768 },
    locale: "zh-CN",
  },

  projects: [
    {
      name: "webview",
      testIgnore: /\.desktop\.spec\.ts$/,
      use: { ...devices["Desktop Chrome"] },
    },
    {
      name: "desktop",
      testMatch: /\.desktop\.spec\.ts$/,
      // The spec launches the packaged application itself and attaches over the
      // WebView2 debugging endpoint, so there is no connectOptions here: a
      // config-level endpoint would have to exist before the app is started.
      // All tests share one application instance (one fixed CDP port), so the
      // file must never be split across parallel workers.
      fullyParallel: false,
      use: { ...devices["Desktop Chrome"] },
    },
  ],

  webServer: {
    // Serves the `--mode e2e` bundle. `reuseExistingServer` keeps a local
    // `npx playwright test --ui` session from fighting the config.
    command: "npm run build:e2e && npm run preview:e2e",
    url: BASE_URL,
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
    cwd: ".",
  },
});
