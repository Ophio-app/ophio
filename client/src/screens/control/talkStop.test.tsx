// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import { UrgentSlot } from "../../design/urgent";
import Talk from "../talk/Talk";
import SafetyStrip from "./SafetyStrip";
import { stopStore } from "./stops";

let app: App;
const tick = (ms = 1500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  app.ui.selectProject(ids.pTide);
  const host = window.__sim!.host;
  const dash = { ...host.tasks.find(t => t.task_id === ids.tDash)!, updated_at: Date.now() + 1000 };
  host.tasks = host.tasks.map(t => t.task_id === dash.task_id ? dash : t);
  host.emit({ type: "task_upserted", task: dash });
  host.stopSafety.ignore(ids.tDash);
  await tick(100);
});
afterEach(() => { cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

// Talk picks up a conversation still going, as if the dashboard check were one.
const conversing = () => {
  const host = window.__sim!.host;
  const dash = { ...host.tasks.find(t => t.task_id === ids.tDash)!, conversation: true };
  host.tasks = host.tasks.map(t => t.task_id === dash.task_id ? dash : t);
  host.emit({ type: "task_upserted", task: dash });
};

it("keeps the stopped conversation and receipt after Force stop with another task awaiting approval", async () => {
  conversing(); await tick(100);
  expect(app.ui.get().taskByProject[ids.pTide]).toBeUndefined();
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><UrgentSlot.Provider value={SafetyStrip}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /><SafetyStrip except={{ taskId: ids.tDash, keepDisplay: true }} /></RouteProvider></UrgentSlot.Provider></AppProvider>);
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(ids.tDash);
  // As in the shell, Talk's own Stop stands in for the rail's while it shows the task.
  fireEvent.click(screen.getByRole("button", { name: /^Stop · Check the staging/ })); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^Stop again · Check the staging/ })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Force stop" }));
  fireEvent.click(screen.getByRole("button", { name: `Force stop on ${app.profile.name}` })); await tick();
  expect(app.session.state.get().tasks[ids.tFlaky]?.state).toBe("awaiting_decision");
  // A stopped conversation is at rest between messages, not broken: no pill.
  expect(screen.getByRole("heading", { level: 1, name: /Check the staging/ })).toBeTruthy();
  expect(document.querySelector(".chat-head__state")).toBeNull();
  expect(screen.getAllByRole("button", { name: "What happened" }).length).toBeGreaterThan(0);
  expect(screen.queryByRole("button", { name: "Allow" })).toBeNull();
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(ids.tDash);
});

it("Stop in the rail pins an implicit Chat selection before sending", async () => {
  conversing(); await tick(100);
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  expect(app.ui.get().taskByProject[ids.pTide]).toBeUndefined();
  fireEvent.click(screen.getByRole("button", { name: /^Stop · Check the staging/ }));
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(ids.tDash);
  await tick();
});
