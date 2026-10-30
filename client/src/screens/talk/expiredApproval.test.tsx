// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { Transcript } from "./Transcript";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

it("says a request's deadline passed instead of asking a connected phone to connect", async () => {
  const state = app.session.state.get();
  expect(app.session.online && state.caughtUp).toBe(true);
  const task = state.tasks[ids.tFlaky]!;
  const approval = state.approvals[ids.aTest]!;
  // A computer-use request carries a deadline; this phone's clock has just passed it
  // and the computer's Expired update hasn't arrived yet.
  act(() => app.session.state.set(s => ({ ...s, approvals: { ...s.approvals, [ids.aTest]: { ...approval, expires_at: Date.now() + 5_000 } } })));
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}>
    <Transcript task={task} agent="Claude Code" />
  </RouteProvider></AppProvider>);
  await tick();
  expect(screen.getByRole("button", { name: "Allow" })).toHaveProperty("disabled", false);
  await tick(6_000);
  expect(app.session.online && app.session.state.get().caughtUp).toBe(true);
  expect(screen.getByRole("button", { name: "Allow" })).toHaveProperty("disabled", true);
  expect(screen.queryByText("Connect to the computer to answer this request.")).toBeNull();
  expect(screen.getByText(/deadline has passed/)).toBeTruthy();
});
