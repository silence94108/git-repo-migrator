// `vitest/config` re-exports Vite's `defineConfig` and adds the `test` block,
// so one file configures both the app build and the test runner.
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  envPrefix: ["VITE_", "TAURI_"],
  test: {
    environment: "jsdom",
    globals: false,
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["./src/state/testSetup.ts"],
    restoreMocks: true,
  },
});
