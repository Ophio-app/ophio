// SPDX-License-Identifier: Apache-2.0
// Settings and its Versions page lead with the app update when the computer
// has one this phone can take, and are unchanged when it has none.
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
import { Versions } from "./Versions";

const offer: AppOffer = { sha256: "a".repeat(64), size: 3 * 1024 * 1024, signers: ["b".repeat(64)], version: "0.2.0" };
let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
async function open(newer: AppOffer | null, page: "settings" | "versions" = "settings", setup?: (platform: ReturnType<typeof createSimPlatform>) => void) {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  // The computer's own newer-version card, which used to lead Versions.
  if (page === "versions") window.__sim!.host.host.capabilities.features.push("release_check_v1");
  const check = vi.fn<AppUpdater["check"]>().mockResolvedValue({ offer: newer, version: "0.1.0", allowed: true, asks: false });
  platform.appUpdate = { check, install: vi.fn(), onResult: () => () => undefined };
  window.__sim!.host.host.capabilities.app_updates = [offer];
  setup?.(platform);
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
  const route = page === "versions" ? { screen: "settings" as const, section: "settings-versions" } : { screen: "settings" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 1, active: true }}>{page === "versions" ? <Versions /> : <Settings route={route} />}</RouteProvider></AppProvider>);
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

/** Whether `a` comes before `b` on the page. */
const before = (a: Element, b: Element) => !!(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);

it("puts Update now and Open-source notices above everything else on Versions when there is a newer app", async () => {
  const check = await open(offer, "versions");
  // One check for the page, not one for the page and one for the card.
  expect(check).toHaveBeenCalledTimes(1);
  const update = screen.getByRole("button", { name: "Update now" });
  const notices = screen.getByRole("button", { name: "Open-source notices" });
  const computer = screen.getByRole("checkbox", { name: /Check for new versions/ });
  const facts = screen.getByText("App protocol");
  expect(before(update, notices)).toBe(true);
  expect(before(notices, computer)).toBe(true);
  expect(before(notices, facts)).toBe(true);
  // Nothing but the page's heading sits above the update.
  const lead = update.closest(".settings-card")!;
  expect(lead.previousElementSibling?.querySelector("h2")?.textContent).toBe("Versions");
  expect(within(lead as HTMLElement).getByText(/has Øphio 0\.2\.0 for this phone/)).toBeTruthy();
  expect(screen.getAllByRole("button", { name: "Open-source notices" })).toHaveLength(1);
});

it("keeps Versions in its usual order when there is nothing newer", async () => {
  const check = await open(null, "versions");
  expect(check).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("button", { name: "Update now" })).toBeNull();
  const computer = screen.getByRole("checkbox", { name: /Check for new versions/ });
  const facts = screen.getByText("App protocol");
  const notices = screen.getByRole("button", { name: "Open-source notices" });
  expect(before(computer, facts)).toBe(true);
  expect(before(facts, notices)).toBe(true);
  expect(screen.getByText(/This app is up to date with/)).toBeTruthy();
});

it("folds the protocol numbers and their note under Details for support", async () => {
  await open(null, "versions");
  const support = screen.getByText("Details for support").closest("details")!;
  expect(support.open).toBe(false);
  for (const words of ["App protocol", "Negotiated protocol", /agreed on this protocol/]) {
    expect(screen.getByText(words).closest("details")).toBe(support);
  }
  // The facts a person reads stay in view.
  expect(screen.getByText("App").closest("details")).toBeNull();
  expect(screen.getByText("Computer").closest("details")).toBeNull();
});

it("keeps the protocol note in view when it asks for an update", async () => {
  await open(null, "versions", (platform) => {
    const appInfo = platform.appInfo!.bind(platform);
    platform.appInfo = async () => ({ ...(await appInfo()), protocol: { major: 1, minor: 9 } });
  });
  const note = screen.getByText(/older protocol supported by both sides/);
  expect(note.closest("details")).toBeNull();
  expect(screen.getByText("App protocol").closest("details")).not.toBeNull();
});
