// SPDX-License-Identifier: Apache-2.0
// Settings leads with the app update when the computer has one this phone can
// take, and is unchanged when it has none.
import { act, cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import type { AppUpdater } from "../../host/link";
import { HostSession } from "../../host/session";
import type { AppOffer } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Settings from "./Settings";

const offer: AppOffer = { sha256: "a".repeat(64), size: 3 * 1024 * 1024, signers: ["b".repeat(64)], version: "0.2.0" };
let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
async function open(newer: AppOffer | null) {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const check = vi.fn<AppUpdater["check"]>().mockResolvedValue({ offer: newer, version: "0.1.0", asks: false });
  platform.appUpdate = { check, install: vi.fn(), onResult: () => () => undefined };
  window.__sim!.host.host.capabilities.app_updates = [offer];
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "settings" }, depth: 1, active: true }}><Settings route={{ screen: "settings" }} /></RouteProvider></AppProvider>);
  await advance();
  return check;
}
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("puts a newer app at the top of Settings, with Update now", async () => {
  const check = await open(offer);
  expect(check).toHaveBeenCalledWith([offer]);
  const sections = screen.getAllByRole("region").map((r) => r.getAttribute("aria-labelledby"));
  expect(sections[0]).toBe("settings-update");
  const card = within(screen.getByRole("region", { name: "Update available" }));
  expect(card.getByText(/has Øphio 0\.2\.0 for this phone/)).toBeTruthy();
  expect(card.getByRole("button", { name: "Update now" })).toBeTruthy();
});

it("leaves Settings as it is when there is nothing newer", async () => {
  await open(null);
  expect(screen.queryByRole("region", { name: "Update available" })).toBeNull();
  expect(screen.getAllByRole("region")[0]!.getAttribute("aria-labelledby")).toBe("settings-agents-group");
  expect(screen.queryByText(/up to date/)).toBeNull();
});
