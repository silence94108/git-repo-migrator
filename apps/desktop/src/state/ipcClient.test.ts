/**
 * Guards the shape of what the production bridge hands to Tauri.
 *
 * The in-memory test double receives the store's argument object directly, so
 * nothing else catches an extra wrapping layer here — and one such layer
 * (`{ input: { input: {...} } }`) broke *every* payload-carrying command in the
 * packaged application while the whole jsdom suite stayed green. The commands
 * are `deny_unknown_fields` structs, so the object must arrive exactly as the
 * Rust parameter names expect.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn<(command: string, args?: unknown) => Promise<unknown>>();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string, args?: unknown) => invoke(command, args),
}));

import { createTauriBridge } from "./ipcClient";

describe("生产 Tauri bridge", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue({});
  });

  it("把载荷对象原样作为命令参数转发，不做二次包裹", async () => {
    const bridge = createTauriBridge();
    await bridge.invoke("connection_save", {
      input: { role: "source", endpoint: "https://git.example" },
    });

    expect(invoke).toHaveBeenCalledExactlyOnceWith("connection_save", {
      input: { role: "source", endpoint: "https://git.example" },
    });
  });

  it("无载荷命令不传参数", async () => {
    const bridge = createTauriBridge();
    await bridge.invoke("migration_snapshot");

    expect(invoke).toHaveBeenCalledExactlyOnceWith("migration_snapshot", undefined);
  });
});
