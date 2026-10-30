// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import { companionDownload, companionSteps, shareCompanionSteps } from "./companionDownload";
afterEach(() => { vi.unstubAllEnvs(); vi.restoreAllMocks(); });
it("uses Web Share when available and copies only on platforms without it", async () => {
  const share = vi.fn(async () => {}), writeText = vi.fn(async () => {});
  Object.defineProperty(navigator, "share", { configurable: true, get: () => share });
  Object.defineProperty(navigator, "clipboard", { configurable: true, get: () => ({ writeText }) });
  expect(await shareCompanionSteps({})).toBe("shared");
  expect(share).toHaveBeenCalledWith({ text: companionSteps() });
  expect(writeText).not.toHaveBeenCalled();
  Object.defineProperty(navigator, "share", { configurable: true, get: () => undefined });
  expect(await shareCompanionSteps({})).toBe("copied");
  expect(writeText).toHaveBeenCalledWith(companionSteps());
});
it.each(["javascript:alert(1)", "https://user:secret@example.test", "invalid"])("does not expose an invalid configured URL: %s", url => {
  vi.stubEnv("VITE_OPHIO_DOWNLOAD_URL", url);
  expect(companionDownload().url).toBeNull();
});
