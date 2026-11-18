// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import { companionDownload, companionSteps, ONE_LINE_INSTALL, oneLineInstall, shareCompanionSteps } from "./companionDownload";
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
it("describes the configured download without assuming the computer runs Linux", () => {
  const steps = companionSteps({ url: "https://example.test/downloads", checksum: null });
  expect(steps).toContain("Download the archive for your computer: https://example.test/downloads");
  expect(steps).not.toContain("Linux archive");
});
it.each(["javascript:alert(1)", "https://user:secret@example.test", "invalid"])("does not expose an invalid configured URL: %s", url => {
  vi.stubEnv("VITE_OPHIO_DOWNLOAD_URL", url);
  expect(companionDownload().url).toBeNull();
});
it("offers the one-line installers only to builds that link to the official download page", () => {
  const official = companionSteps({ url: "https://ophio.app/download/", checksum: "https://ophio.app/download/#checksums" });
  expect(official).toContain(`Quickest: one command`);
  expect(official).toContain(ONE_LINE_INSTALL.unix);
  expect(official).toContain(ONE_LINE_INSTALL.windows);
  expect(official.indexOf("Or by hand:")).toBeLessThan(official.indexOf("Download the archive"));
  expect(oneLineInstall({ url: "https://ophio.app/download/", checksum: null })).toBe(ONE_LINE_INSTALL);
  for (const url of ["https://example.test/downloads", "https://ophio.app.example.test/", null]) {
    expect(oneLineInstall({ url, checksum: null })).toBeNull();
    expect(companionSteps({ url, checksum: null })).not.toContain("get-ophio");
  }
});
it("names where each computer installs Ophio, not the extracted folder", () => {
  const steps = companionSteps({ url: "https://example.test/downloads", checksum: null });
  expect(steps).toContain("Linux and Mac:");
  expect(steps).toContain(String.raw`& "$env:LOCALAPPDATA\Programs\Ophio\ophio.exe" setup`);
  expect(steps).not.toContain("extracted folder");
  expect(steps).not.toContain(String.raw`.\ophio.exe`);
});
