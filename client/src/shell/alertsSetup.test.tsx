// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { HostSession } from "../host/session";
import { alertsPreset, backgroundAlerts } from "../platform/alerts";
import { appLock } from "../platform/lock";
import { notificationAccess, notificationDetail, watchAttention } from "../platform/notifications";
import { Notifications } from "../screens/settings/Notifications";
import { AlertsSetup } from "./AlertsSetup";
import { RouteProvider } from "./Screen";

let app: App;
const advance = async (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const device = () => { const d = window.__sim?.device; if (!d) throw new Error("Missing simulated device"); return d; };
const watcher = () => { const w = backgroundAlerts(app.platform); if (!w) throw new Error("Missing watcher"); return w; };
function visibility(value: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, value });
  document.dispatchEvent(new Event("visibilitychange"));
}
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "talk", route: { screen: "talk" }, depth: 0, active: true }}>{children}</RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  localStorage.clear();
  visibility("visible");
  notificationDetail.set("minimal");
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing fixture profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  // As after the app's start checked the lock, which is off.
  appLock(platform).set((s) => ({ ...s, locked: false, checking: false }));
  session.start();
  await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); visibility("visible");
});
function raise(id: string) {
  const original = app.session.state.get().attention[ids.atReady];
  if (!original) throw new Error("Missing attention fixture");
  act(() => app.session.state.set((s) => ({ ...s, attention: { ...s.attention, [id]: { ...original, attention_id: id } } })));
}

it("leaves a computer's alerts to the watcher only while it is watching that computer", async () => {
  const stop = watchAttention(app.platform, app.session, app.profile.name);
  await advance();
  visibility("hidden");
  device().setAlerts({ statuses: { [app.profile.profileId]: "watching" } });
  await act(() => watcher().refresh());
  raise("watched");
  await advance();
  expect(device().notifications).toHaveLength(0);

  device().setAlerts({ statuses: { [app.profile.profileId]: "unreachable" } });
  await act(() => watcher().refresh());
  raise("unwatched");
  await advance();
  expect(device().notifications.map((n) => n.id)).toEqual([`${app.profile.profileId}/unwatched`]);
  // The watcher records it as raised, not as shown on the screen, so a
  // restart of the phone raises it again and turning alerts off clears it.
  expect(device().raised).toEqual([`${app.profile.profileId}/unwatched`]);
  expect(device().seen.flatMap((s) => s.attentionIds)).not.toContain("unwatched");

  // Android's shade has no room: the alert is missed, not recorded.
  device().shadeFull(true);
  raise("crowded");
  await advance();
  expect(device().raised).toEqual([`${app.profile.profileId}/unwatched`]);
  expect(notificationAccess(app.platform).get().missed[app.profile.profileId]?.count).toBe(1);
  device().shadeFull(false);

  device().setAlerts({ statuses: { [app.profile.profileId]: "watching" }, android: { running: false } });
  await act(() => watcher().refresh());
  raise("stopped");
  await advance();
  expect(device().notifications).toHaveLength(2);
  stop();
});

it("tells the watcher which computer's alerts the app shows, and when it stops", async () => {
  const stop = watchAttention(app.platform, app.session, app.profile.name);
  await advance();
  expect(device().viewing).toBe(app.profile.profileId);
  stop();
  await advance();
  expect(device().viewing).toBeNull();
});

it("clears an answered alert's notification, and tells the watcher what the app showed", async () => {
  const stop = watchAttention(app.platform, app.session, app.profile.name);
  await advance();
  // The open list on screen at start is reported too.
  expect(device().seen).toHaveLength(1);
  raise("onscreen");
  await advance();
  expect(device().seen.at(-1)).toEqual({ profileId: app.profile.profileId, attentionIds: ["onscreen"] });
  expect(device().notifications).toHaveLength(0);

  visibility("hidden");
  const before = device().cancelled.length;
  raise("answered");
  await advance();
  expect(device().notifications.map((n) => n.id)).toEqual([`${app.profile.profileId}/answered`]);
  act(() => app.session.state.set((s) => {
    const item = s.attention.answered;
    if (!item) throw new Error("Missing raised item");
    return { ...s, attention: { ...s.attention, answered: { ...item, resolved_at: item.created_at } } };
  }));
  await advance();
  act(() => app.session.state.set((s) => ({ ...s, resolvedAttention: { ...s.resolvedAttention, elsewhere: 1 } })));
  await advance();
  expect(device().cancelled.slice(before)).toEqual([`${app.profile.profileId}/answered`, `${app.profile.profileId}/elsewhere`]);
  // Each is cleared once.
  act(() => app.session.state.set((s) => ({ ...s, attention: { ...s.attention } })));
  await advance();
  expect(device().cancelled.slice(before)).toHaveLength(2);
  stop();
});

it("tells the watcher only what the unlocked screen shows", async () => {
  const lock = appLock(app.platform);
  lock.set((s) => ({ ...s, enabled: true, locked: true }));
  const stop = watchAttention(app.platform, app.session, app.profile.name);
  await advance();
  expect(device().seen).toHaveLength(0);
  raise("behind-lock");
  await advance();
  // Locked, the app shows nothing of it, so the watcher still raises it.
  expect(device().seen).toHaveLength(0);
  lock.set((s) => ({ ...s, locked: false }));
  await advance();
  const reported = device().seen.flatMap((s) => s.attentionIds);
  expect(reported).toContain("behind-lock");
  // Each is reported once.
  visibility("hidden");
  visibility("visible");
  await advance();
  expect(device().seen.flatMap((s) => s.attentionIds)).toEqual(reported);
  stop();
});

it("asks once, then walks only the Android steps still needed", async () => {
  device().setAlerts({ settings: null });
  device().notificationPermission("promptable");
  render(<AlertsSetup />, { wrapper });
  await advance();
  expect(screen.getByText("Get alerts when an agent asks something or finishes?")).toBeTruthy();
  expect((screen.getByRole("radio", { name: "Turn on" }) as HTMLInputElement).checked).toBe(true);

  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await advance();
  expect(device().alerts.settings).toEqual({ enabled: true, detail: "minimal" });
  expect(screen.getByText(/Android asks next whether Øphio may show notifications/)).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await advance();
  expect(device().notificationRequests).toBe(1);
  expect(screen.getByText(/run without battery limits/)).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await advance();
  expect(device().batteryRequests).toBe(1);
  // That was the last: the watcher shows no notice to hide.
  expect(screen.queryByText(/run without battery limits/)).toBeNull();
  expect(screen.queryByRole("button", { name: /Skip|Continue/ })).toBeNull();
  expect(device().channels).toEqual([]);
});

it("skips steps Android already allows", async () => {
  device().setAlerts({ settings: null, android: { unrestricted: true } });
  render(<AlertsSetup />, { wrapper });
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await advance();
  expect(screen.queryByText(/may show notifications/)).toBeNull();
  expect(screen.queryByText(/battery limits/)).toBeNull();
  expect(screen.queryByRole("button", { name: /Skip|Continue/ })).toBeNull();
  expect(device().alerts.settings).toEqual({ enabled: true, detail: "minimal" });
});

it("saves Not now, and asks again next launch when closed without a choice", async () => {
  device().setAlerts({ settings: null });
  const first = render(<AlertsSetup />, { wrapper });
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  await advance();
  expect(screen.queryByText("Get alerts when an agent asks something or finishes?")).toBeNull();
  expect(device().alerts.settings).toBeNull();
  first.unmount();

  render(<AlertsSetup />, { wrapper });
  await advance();
  fireEvent.click(screen.getByRole("radio", { name: "Not now" }));
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await advance();
  expect(device().alerts.settings).toEqual({ enabled: false, detail: "minimal" });
  expect(device().alerts.android.running).toBe(false);
  expect(screen.queryByText("Get alerts when an agent asks something or finishes?")).toBeNull();
});

it("starts on the answer setup carried, and forgets it once saved", async () => {
  device().setAlerts({ settings: null });
  alertsPreset.set(false);
  render(<AlertsSetup />, { wrapper });
  await advance();
  expect((screen.getByRole("radio", { name: "Not now" }) as HTMLInputElement).checked).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await advance();
  expect(device().alerts.settings).toEqual({ enabled: false, detail: "minimal" });
  expect(alertsPreset.get()).toBeNull();
});

it("shows the watcher's state in Settings and opens Android's own controls", async () => {
  render(<Notifications />, { wrapper });
  await advance();
  const toggle = screen.getByRole("checkbox", { name: "Alerts when the app is closed" }) as HTMLInputElement;
  expect(toggle.checked).toBe(true);
  // The app's own connection is live, so the watcher's is said as its own:
  // the screen never reads as disagreeing with the header.
  expect(screen.getByText(`Alerts for when the app is closed are still starting. Until then, alerts from ${app.profile.name} arrive while the app is open.`)).toBeTruthy();
  expect(screen.queryByText(`Connecting to ${app.profile.name}…`)).toBeNull();
  device().setAlerts({ statuses: { [app.profile.profileId]: "unreachable" } });
  await advance(2000);
  expect(screen.getByText(`Alerts for when the app is closed can’t reach ${app.profile.name} yet and keep trying. Until then they arrive while the app is open.`)).toBeTruthy();

  // Followed while shown, as Android starts the watcher after it is turned on.
  device().setAlerts({ statuses: { [app.profile.profileId]: "update_needed" } });
  await advance(2000);
  expect(screen.getByText(`Update Øphio on ${app.profile.name} to get alerts while the app is closed.`)).toBeTruthy();

  // Without Unrestricted battery use, Android stops the watcher once the app closes.
  expect(screen.getByText("Alerts stop when the app closes")).toBeTruthy();
  expect(screen.getByText(/^Keeps a light connection to each paired computer while the app is open\./)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Allow" }));
  fireEvent.click(screen.getAllByRole("button", { name: "Change" })[1]!);
  await advance();
  expect(device().batteryRequests).toBe(1);
  expect(device().channels).toEqual(["finished"]);
  device().setAlerts({ android: { unrestricted: true } });
  await advance(2000);
  expect(screen.queryByText("Alerts stop when the app closes")).toBeNull();
  expect(screen.getByText(/^Keeps a light connection to each paired computer, so alerts arrive after you swipe the app away/)).toBeTruthy();

  fireEvent.change(screen.getByLabelText("Notification detail"), { target: { value: "task_title" } });
  await advance();
  expect(device().alerts.settings).toEqual({ enabled: true, detail: "task_title" });

  fireEvent.click(toggle);
  await advance();
  expect(device().alerts.settings?.enabled).toBe(false);
  expect(screen.getByText("Off: alerts arrive only while the app is open.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Allow" })).toBeNull();

  fireEvent.click(toggle);
  await advance();
  device().setAlerts({ android: { running: false } });
  await act(() => watcher().refresh());
  expect(screen.getByText("Android stopped alerts")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start again" }));
  await advance();
  expect(device().alerts.android.running).toBe(true);
});
