// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { appLock, unavailableLock } from "../../platform/lock";
import { forgetHandled, wasHandled } from "../../platform/handled";
import { notificationFor, parseOpenRoute } from "../../platform/notifications";
import { RouteProvider } from "../../shell/Screen";
import { emptyHostState } from "../../state/projection";
import type { AttentionItem } from "../../protocol/types";
import { Versions } from "./Versions";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const item = { attention_id: "release-note", task_id: null, kind: "completed", resolved_at: null, new_version: "0.3.0" } as AttentionItem;
async function open(supported: boolean, admin = true) {
  vi.useFakeTimers(); localStorage.clear(); forgetHandled();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  if (supported) window.__sim!.host.host.capabilities.features.push("release_check_v1");
  const host = window.__sim!.host;
  host.releaseCheck.view.newer = { version: "0.3.0", url: "https://github.com/example/releases/tag/v0.3.0" };
  host.releaseCheck.view.how_to_update = "Run ophio update on this computer.";
  host.host.release_check = structuredClone(host.releaseCheck.view);
  if (!admin) platform.openExternal = vi.fn();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  appLock(platform).accept(unavailableLock);
  session.start(); await advance(900);
  if (!admin) {
    window.__sim!.permissions({ admin: false }); await advance();
  }
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "settings", section: "settings-versions" }, depth: 1, active: true }}><Versions /></RouteProvider></AppProvider>);
  await advance();
  return host;
}
afterEach(() => { cleanup(); app?.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

it("hides version checking without the feature", async () => {
  await open(false);
  expect(screen.queryByRole("checkbox", { name: /Check for new versions/ })).toBeNull();
});
it("shows the release, opens its page, explains updating and saves the expected revision", async () => {
  const host = await open(true);
  const external = vi.spyOn(app.platform, "openExternal").mockResolvedValue();
  expect(screen.getByText("Øphio 0.3.0 is available for studio.")).toBeTruthy();
  expect(screen.getByText("Run ophio update on this computer.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "What’s new" }));
  expect(external).toHaveBeenCalledWith("https://github.com/example/releases/tag/v0.3.0");
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(screen.getByRole("checkbox", { name: /Check for new versions/ }));
  await advance();
  expect(command).toHaveBeenCalledWith({ command: "set_release_check", enabled: false }, expect.objectContaining({ expectedRevision: 1 }));
  expect(host.releaseCheck.view.revision).toBe(2);
  expect(host.releaseCheck.view.enabled).toBe(false);
});
it("shows when an enabled computer has the newest version", async () => {
  const host = await open(true);
  act(() => { host.releaseCheck.view.newer = null; host.host.release_check = structuredClone(host.releaseCheck.view); host.emit({ type: "host_updated", host: structuredClone(host.host) }); host.emit({ type: "attention_upserted", item: { ...item } }); });
  await advance();
  expect(screen.getByText(/studio has the newest Øphio. Checked/)).toBeTruthy();
});
it("disables the switch without Manage access", async () => {
  await open(true, false);
  expect((screen.getByRole("checkbox", { name: /Check for new versions/ }) as HTMLInputElement).disabled).toBe(true);
  expect(screen.getByText("This device needs the Manage access permission to change this.")).toBeTruthy();
});
it("recognizes the new route and notifies only with a supporting computer", () => {
  expect(parseOpenRoute({ kind: "new_version", profileId: "studio" })).toEqual({ kind: "new_version", profileId: "studio" });
  expect(parseOpenRoute({ kind: "new_version", profileId: "" })).toBeNull();
  const state = { ...emptyHostState, capabilities: { features: ["release_check_v1"] } } as Parameters<typeof notificationFor>[1];
  const computer = { profileId: "studio", name: "studio" };
  expect(notificationFor(item, state, computer, "minimal")).toMatchObject({ id: "studio/release-note", title: "Øphio 0.3.0 is available", body: "studio · Tap to see what’s new and how to update.", category: "finished", route: { kind: "new_version", profileId: "studio" } });
  expect(notificationFor(item, { ...state, capabilities: null }, computer, "minimal")).toBeNull();
});
it("handles new-version attention while Versions is on screen", async () => {
  const host = await open(true);
  act(() => { host.attention.push({ ...item }); host.emit({ type: "attention_upserted", item: { ...item } }); });
  await advance();
  expect(app.session.state.get().attention[item.attention_id]).toBeDefined();
  expect(document.visibilityState).not.toBe("hidden");
  expect(wasHandled(app.profile.profileId, item.attention_id)).toBe(true);
});

it("does not acknowledge a notice until matching release details are rendered", async () => {
  const host = await open(true);
  act(() => { (host.host as { release_check?: typeof host.releaseCheck.view }).release_check = undefined; host.releaseCheck.view.newer = null; host.emit({ type: "host_updated", host: structuredClone(host.host) }); host.attention.push({ ...item }); host.emit({ type: "attention_upserted", item: { ...item } }); });
  await advance();
  expect(wasHandled(app.profile.profileId, item.attention_id)).toBe(false);
  act(() => { host.host.release_check = { ...host.releaseCheck.view, newer: { version: "0.3.0", url: "https://github.com/example/releases/tag/v0.3.0" } }; host.emit({ type: "host_updated", host: structuredClone(host.host) }); });
  await advance();
  expect(wasHandled(app.profile.profileId, item.attention_id)).toBe(true);
});

it("acknowledges a matching notice through the command when no background watcher exists", async () => {
  const host = await open(true);
  delete (app.platform as { alerts?: unknown }).alerts;
  const command = vi.spyOn(app.session, "command");
  act(() => { host.attention.push({ ...item }); host.emit({ type: "attention_upserted", item: { ...item } }); });
  await advance();
  expect(command).toHaveBeenCalledWith({ command: "acknowledge_attention", attention_id: item.attention_id });
  expect(command.mock.calls.filter(([body]) => body.command === "acknowledge_attention")).toHaveLength(1);
});

it("shows checking status and follows host updates without reloading", async () => {
  const host = await open(true);
  act(() => { host.host.release_check = { ...host.releaseCheck.view, newer: null, checking: true }; host.emit({ type: "host_updated", host: structuredClone(host.host) }); });
  await advance();
  expect(screen.getByText("Checking for new versions…")).toBeTruthy();
  expect(screen.queryByText(/has the newest Øphio/)).toBeNull();
  act(() => { host.host.release_check = { ...host.host.release_check!, checking: false }; host.emit({ type: "host_updated", host: structuredClone(host.host) }); });
  await advance();
  expect(screen.queryByText("Checking for new versions…")).toBeNull();
  expect(screen.getByText(/has the newest Øphio/)).toBeTruthy();
});

it("retries an offline version acknowledgement on reconnect and records only acceptance", async () => {
  const host = await open(true);
  delete (app.platform as { alerts?: unknown }).alerts;
  const command = vi.spyOn(app.session, "command");
  act(() => app.session.link.set({ state: "offline", since: Date.now(), reason: "Disconnected", retryAt: null }));
  act(() => { host.attention.push({ ...item }); host.emit({ type: "attention_upserted", item: { ...item } }); });
  await advance();
  expect(wasHandled(app.profile.profileId, item.attention_id)).toBe(false);
  expect(command.mock.calls.filter(([body]) => body.command === "acknowledge_attention")).toHaveLength(0);
  act(() => app.session.link.set({ state: "online", since: Date.now() }));
  await advance();
  expect(command.mock.calls.filter(([body]) => body.command === "acknowledge_attention")).toHaveLength(1);
  expect(wasHandled(app.profile.profileId, item.attention_id)).toBe(true);
});

it("retries a refused version acknowledgement on the next visit without concurrent duplicates", async () => {
  const host = await open(true);
  delete (app.platform as { alerts?: unknown }).alerts;
  const original = app.session.command.bind(app.session);
  let rejectFirst: ((reason: Error) => void) | undefined;
  const command = vi.spyOn(app.session, "command").mockImplementation((...args) =>
    args[0].command === "acknowledge_attention" && !rejectFirst
      ? new Promise((_, reject) => { rejectFirst = reject; })
      : original(...args));
  act(() => { host.attention.push({ ...item }); host.emit({ type: "attention_upserted", item: { ...item } }); });
  await advance();
  expect(wasHandled(app.profile.profileId, item.attention_id)).toBe(false);
  expect(command.mock.calls.filter(([body]) => body.command === "acknowledge_attention")).toHaveLength(1);
  await act(async () => { rejectFirst?.(new Error("Computer refused")); await Promise.resolve(); });
  expect(wasHandled(app.profile.profileId, item.attention_id)).toBe(false);
  cleanup();
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "settings", section: "settings-versions" }, depth: 1, active: true }}><Versions /></RouteProvider></AppProvider>);
  await advance();
  expect(command.mock.calls.filter(([body]) => body.command === "acknowledge_attention")).toHaveLength(2);
  expect(wasHandled(app.profile.profileId, item.attention_id)).toBe(true);
});

it("clears a query error when valid release details arrive from the host", async () => {
  const host = await open(true);
  vi.spyOn(app.session, "query").mockRejectedValue(new Error("Release query failed"));
  act(() => app.session.state.set((s) => ({ ...s, host: { ...s.host!, release_check: undefined } })));
  await advance();
  expect(screen.getByText(/Release query failed/)).toBeTruthy();
  act(() => { host.host.release_check = structuredClone(host.releaseCheck.view); host.emit({ type: "host_updated", host: structuredClone(host.host) }); });
  await advance();
  expect(screen.getByText("Øphio 0.3.0 is available for studio.")).toBeTruthy();
  expect(screen.queryByText(/Release query failed/)).toBeNull();
});
