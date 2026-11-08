// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { HostSession } from "../host/session";
import { backgroundAlerts } from "../platform/alerts";
import { ConnectionNote } from "../screens/work/shared";
import Connections from "../screens/settings/Connections";
import { ColdStartOffline } from "./ColdStartOffline";
import { RouteProvider } from "./Screen";
import { StatusSheet } from "./StatusSheet";

const HOUR = 3_600_000, DAY = 24 * HOUR;
let app: App;
const advance = async (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}>{children}</AppProvider>;
const device = () => { const d = window.__sim?.device; if (!d) throw new Error("Missing simulated device"); return d; };

beforeEach(async () => {
  vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
  // As after the app was off screen for two days: its own connection last
  // heard from the computer then, and now cannot reach it.
  session.stop();
  act(() => {
    session.state.set((s) => ({ ...s, lastHeardAt: Date.now() - 2.5 * DAY }));
    session.link.set({ state: "offline", since: Date.now(), reason: "Cannot reach the computer.", retryAt: null });
  });
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals();
});

it("says when the computer was last heard by the connection kept for alerts while the app was closed", async () => {
  device().setAlerts({ heard: { [app.profile.profileId]: Date.now() - 5.5 * HOUR } });
  render(<StatusSheet open onClose={vi.fn()} />, { wrapper });
  await advance();
  expect(screen.getByText(/Last heard/).textContent).toContain("Last heard 5 h ago");
});

it("says the newer of the two times in the offline notes on the work screens", async () => {
  device().setAlerts({ heard: { [app.profile.profileId]: Date.now() - 5.5 * HOUR } });
  // Before the computer has sent its details, the work screens show a card instead.
  act(() => { app.session.state.set((s) => ({ ...s, host: null })); });
  render(<>
    <ConnectionNote />
    <RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>
      <ColdStartOffline route={{ screen: "talk" }}><p>Talk</p></ColdStartOffline>
    </RouteProvider>
  </>, { wrapper });
  await advance();
  expect(screen.getByText(/Last heard/).textContent).toContain("Last heard 5 h ago");
  expect(screen.getByText(/Last reached/).textContent).toContain("Last reached 5 h ago");
  expect(screen.queryByText(/2 d ago/)).toBeNull();
});

it("tells the work screens the phone is off Tailscale, not only that the computer is offline", async () => {
  const reason = "This phone isn’t connected to Tailscale. Turn Tailscale on to reach the computer.";
  act(() => { app.session.link.set({ state: "offline", since: Date.now(), reason, retryAt: null, noTailscale: true }); });
  render(<ConnectionNote />, { wrapper });
  await advance();
  expect(screen.getByText("Tailscale off")).toBeTruthy();
  expect(screen.getByText(/isn’t connected to Tailscale/).textContent).toContain(`${reason} Last heard 2 d ago.`);
  expect(screen.queryByText(/hasn’t confirmed any newer changes/)).toBeNull();
});

it("says the newer of the two times in Settings", async () => {
  const other = { ...app.profile, profileId: "other-pairing", addresses: ["100.64.0.9:7443"] };
  app = { ...app, profiles: [app.profile, other] };
  device().setAlerts({ heard: { [app.profile.profileId]: Date.now() - 5.5 * HOUR, [other.profileId]: Date.now() - 7.5 * HOUR } });
  const route = { screen: "connections" } as const;
  // Settings lists the paired computers on this screen.
  render(<RouteProvider value={{ tab: "work", depth: 1, active: true, route }}><Connections route={route} /></RouteProvider>, { wrapper });
  await advance();
  expect(screen.getByText("Computer disconnected")).toBeTruthy();
  // Once in the disconnected notice, once for the computer on screen among
  // two paired computers of one name, and the other one's own time.
  expect(screen.getAllByText("5 h ago")).toHaveLength(2);
  expect(screen.getAllByText("7 h ago")).toHaveLength(1);
  expect(screen.queryByText(/2 d ago/)).toBeNull();
});

it("keeps the app's own time when it is the newer one", async () => {
  device().setAlerts({ heard: { [app.profile.profileId]: Date.now() - 3.5 * DAY } });
  render(<StatusSheet open onClose={vi.fn()} />, { wrapper });
  await advance();
  expect(screen.getByText(/Last heard/).textContent).toContain("Last heard 2 d ago");
});

it("picks up a newer time from the alert connection when the app returns to the screen", async () => {
  render(<StatusSheet open onClose={vi.fn()} />, { wrapper });
  await advance();
  expect(screen.getByText(/Last heard/).textContent).toContain("Last heard 2 d ago");
  device().setAlerts({ heard: { [app.profile.profileId]: Date.now() - 20.5 * 60_000 } });
  act(() => { document.dispatchEvent(new Event("visibilitychange")); });
  await advance();
  expect(screen.getByText(/Last heard/).textContent).toContain("Last heard 20 min ago");
});

it("keeps contact times current while the screen stays visible and stops following on unmount", async () => {
  // The app's notification subscription already keeps the shared watcher started.
  const watcher = backgroundAlerts(app.platform)!;
  const stop = watcher.start();
  const read = vi.spyOn(app.platform.alerts!, "state");
  try {
    const view = render(<StatusSheet open onClose={vi.fn()} />, { wrapper });
    await advance();
    expect(screen.getByText(/Last heard/).textContent).toContain("Last heard 2 d ago");
    device().setAlerts({ heard: { [app.profile.profileId]: Date.now() - 20.5 * 60_000 } });
    // No foreground or visibility event: the watcher heard the computer while
    // this screen was already open.
    await advance(2000);
    expect(screen.getByText(/Last heard/).textContent).toContain("Last heard 20 min ago");
    view.unmount();
    read.mockClear();
    await advance(4000);
    expect(read).not.toHaveBeenCalled();
  } finally {
    read.mockRestore();
    stop();
  }
});

it("reads nothing in the background for screens that show no contact time, and once per two seconds for several that do", async () => {
  const read = vi.spyOn(app.platform.alerts!, "state");
  try {
    // Online with the computer's details in: the notes and the card are hidden.
    act(() => { app.session.link.set({ state: "online", since: Date.now() }); });
    const screens = <>
      <ConnectionNote />
      <ConnectionNote />
      <RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>
        <ColdStartOffline route={{ screen: "talk" }}><p>Talk</p></ColdStartOffline>
      </RouteProvider>
    </>;
    render(screens, { wrapper });
    await advance(6000);
    expect(read).not.toHaveBeenCalled();
    // Offline: the two notes on screens kept mounted share one timer.
    act(() => { app.session.link.set({ state: "offline", since: Date.now(), reason: "Cannot reach the computer.", retryAt: null }); });
    await advance();
    read.mockClear();
    await advance(6000);
    expect(read).toHaveBeenCalledTimes(3);
  } finally {
    read.mockRestore();
  }
});

it("updates another paired computer's contact time without leaving Settings", async () => {
  const other = { ...app.profile, profileId: "other-pairing", addresses: ["100.64.0.9:7443"] };
  app = { ...app, profiles: [app.profile, other] };
  device().setAlerts({ heard: { [other.profileId]: Date.now() - 7.5 * HOUR } });
  const route = { screen: "connections" } as const;
  render(<RouteProvider value={{ tab: "work", depth: 1, active: true, route }}><Connections route={route} /></RouteProvider>, { wrapper });
  await advance();
  expect(screen.getByText("7 h ago")).toBeTruthy();
  device().setAlerts({ heard: { [other.profileId]: Date.now() - 2.5 * 60_000 } });
  await advance(2000);
  expect(screen.getByText("2 min ago")).toBeTruthy();
  expect(screen.queryByText("7 h ago")).toBeNull();
});

it("uses the app's own time where the phone keeps no connection for alerts", async () => {
  app = { ...app, platform: { ...app.platform, alerts: undefined } };
  render(<StatusSheet open onClose={vi.fn()} />, { wrapper });
  await advance();
  expect(screen.getByText(/Last heard/).textContent).toContain("Last heard 2 d ago");
});

it.each(["note", "cold start", "connections"])("does not poll a hidden mounted %s screen, and resumes when shown", async (kind) => {
  if (kind === "cold start") act(() => { app.session.state.set((s) => ({ ...s, host: null })); });
  const read = vi.spyOn(app.platform.alerts!, "state");
  const content = kind === "note" ? <ConnectionNote /> : kind === "cold start"
    ? <ColdStartOffline route={{ screen: "talk" }}><p>Talk content</p></ColdStartOffline>
    : <Connections route={{ screen: "connections" }} />;
  const route = (active: boolean) => <RouteProvider value={{ tab: "work", depth: 1, active, route: { screen: "connections" } }}>{content}</RouteProvider>;
  const view = render(route(false), { wrapper });
  try {
    await advance(4000);
    expect(read).not.toHaveBeenCalled();
    view.rerender(route(true));
    await advance();
    read.mockClear();
    device().setAlerts({ heard: { [app.profile.profileId]: Date.now() - 5.5 * HOUR } });
    await advance(2000);
    expect(read).toHaveBeenCalledTimes(1);
    expect(screen.getAllByText(/5 h ago/).length).toBeGreaterThan(0);
    view.rerender(route(false));
    read.mockClear();
    await advance(4000);
    expect(read).not.toHaveBeenCalled();
    view.rerender(route(true));
    await advance();
    read.mockClear();
    await advance(4000);
    expect(read).toHaveBeenCalledTimes(2);
    view.unmount();
    read.mockClear();
    await advance(4000);
    expect(read).not.toHaveBeenCalled();
  } finally { read.mockRestore(); }
});

it("keeps one shared timer until the final follower leaves, then can start a fresh timer", async () => {
  const watcher = backgroundAlerts(app.platform)!;
  const stopStarted = watcher.start();
  await advance();
  const read = vi.spyOn(app.platform.alerts!, "state");
  const clear = vi.spyOn(globalThis, "clearInterval");
  const interval = vi.spyOn(globalThis, "setInterval");
  const first = watcher.follow(), second = watcher.follow();
  const timer = interval.mock.results[0]!.value;
  expect(interval).toHaveBeenCalledTimes(1);
  await advance(2000);
  expect(read).toHaveBeenCalledTimes(1);
  first();
  expect(clear.mock.calls.filter(([id]) => id === timer)).toHaveLength(0);
  await advance(2000);
  expect(read).toHaveBeenCalledTimes(2);
  second();
  expect(clear.mock.calls.filter(([id]) => id === timer)).toHaveLength(1);
  await advance(4000);
  expect(read).toHaveBeenCalledTimes(2);
  const third = watcher.follow();
  expect(interval).toHaveBeenCalledTimes(2);
  await advance(2000);
  expect(read).toHaveBeenCalledTimes(3);
  third();
  stopStarted();
  await advance(4000);
  expect(read).toHaveBeenCalledTimes(3);
  expect(clear.mock.calls.filter(([id]) => id === timer)).toHaveLength(1);
  expect(clear.mock.calls.filter(([id]) => id === interval.mock.results[1]!.value)).toHaveLength(1);
  read.mockRestore(); clear.mockRestore(); interval.mockRestore();
});

it("does not poll online Connections when no paired names need contact times", async () => {
  act(() => { app.session.link.set({ state: "online", since: Date.now() }); });
  const read = vi.spyOn(app.platform.alerts!, "state");
  try {
    const route = { screen: "connections" } as const;
    render(<RouteProvider value={{ tab: "work", depth: 1, active: true, route }}><Connections route={route} /></RouteProvider>, { wrapper });
    await advance(6000);
    expect(screen.queryByText("Last reached")).toBeNull();
    expect(screen.queryByText("Computer disconnected")).toBeNull();
    expect(read).not.toHaveBeenCalled();
    act(() => { app.session.link.set({ state: "offline", since: Date.now(), reason: "Cannot reach the computer.", retryAt: null }); });
    await advance();
    read.mockClear();
    device().setAlerts({ heard: { [app.profile.profileId]: Date.now() - 5.5 * HOUR } });
    await advance(2000);
    expect(read).toHaveBeenCalledTimes(1);
    expect(screen.getByText("5 h ago")).toBeTruthy();
    act(() => { app.session.link.set({ state: "online", since: Date.now() }); });
    read.mockClear();
    await advance(4000);
    expect(read).not.toHaveBeenCalled();
  } finally { read.mockRestore(); }
});
