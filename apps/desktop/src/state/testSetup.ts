/** Vitest setup: unmount React trees between tests so DOM queries stay scoped. */
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(() => {
  cleanup();
  window.location.hash = "";
});
