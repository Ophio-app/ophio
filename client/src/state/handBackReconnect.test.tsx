// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { HostSession } from "../host/session";
import type { Command } from "../protocol/types";
import { HandBack } from "../screens/talk/HandBack";
import { RouteProvider } from "../shell/Screen";
import { handBackTarget } from "./handBack";
import { openAttention } from "./projection";

let app: App;
const sessions: HostSession[] = [];
async function connect(): Promise<HostSession> {
  const profile = (await app.platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, app.platform.control(profile.profileId));
  sessions.push(session);
  session.start();
  await vi.advanceTimersByTimeAsync(900);
  return session;
}
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  app = { platform, profile, session: null!, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  app.session = await connect();
});
afterEach(() => { cleanup(); for (const s of sessions.splice(0)) s.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 500) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const command = (body: Command) => window.__sim!.host.command({ command_id: crypto.randomUUID(), issued_at: Date.now(), expires_at: null, expected_revision: null, body });

/** Talk, Needs you and the Work badge all offer the hand-back, from state the session rebuilt. */
async function offersHandBack(session: HostSession) {
  const s = session.state.get();
  const task = s.tasks[ids.tDash]!;
  expect(handBackTarget(s, task)?.label).toBe("Screen 1 (DP-1)");
  // Needs you and the Work badge both count open attention.
  expect(openAttention(s).filter(a => a.task_id === ids.tDash).map(a => a.body)).toEqual(["Agent paused on Screen 1 (DP-1) · Resume agent"]);
  render(<AppProvider app={{ ...app, session }}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><HandBack task={task} /></RouteProvider></AppProvider>);
  expect(screen.getByRole("button", { name: "Resume agent" })).toBeTruthy();
}

it("an agent paused on its own display stays resumable after the link drops and returns", async () => {
  await command({ command: "pause_input", target_id: ids.tgDp1 });
  await tick(900);
  window.__sim!.offline();
  await tick(1500);
  window.__sim!.online();
  await tick(3000);
  expect(app.session.link.get().state).toBe("online");
  await offersHandBack(app.session);
});

it("a pause made while the phone was away is offered once it reconnects", async () => {
  window.__sim!.offline();
  await tick(500);
  await command({ command: "pause_input", target_id: ids.tgDp1 });
  await tick(900);
  window.__sim!.online();
  await tick(3000);
  await offersHandBack(app.session);
});

it("a phone that starts after the pause offers it from its first snapshot", async () => {
  await command({ command: "pause_input", target_id: ids.tgDp1 });
  await tick(900);
  app.session.stop();
  await offersHandBack(await connect());
});
