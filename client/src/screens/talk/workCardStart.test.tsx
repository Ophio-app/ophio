// SPDX-License-Identifier: Apache-2.0
//
// A suggestion card's Start: one tap flips it at once and starts exactly one
// task, however fast a second tap follows.
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Talk from "./Talk";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  window.history.replaceState(null, "", "/?cards=1");
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});
const tick = async (ms = 300) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

it("starts one task for two fast taps on Start, and says Starting at once", async () => {
  app.ui.selectProject(ids.pTide);
  app.ui.selectTask(ids.pTide, ids.tOneCard);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);
  await tick(600);
  const command = vi.spyOn(app.session, "command");
  const start = screen.getByRole<HTMLButtonElement>("button", { name: "Start" });
  // Both taps reach the same rendered handler before React re-renders the card.
  act(() => { start.click(); start.click(); });
  expect(screen.getByText("Starting")).toBeTruthy();
  await tick(3000);
  const starts = command.mock.calls.map((c) => c[0]).filter((b) => b.command === "create_task");
  expect(starts).toHaveLength(1);
  expect(starts[0]).toMatchObject({ project_id: ids.pTide, origin: { chat: ids.tOneCard, run: ids.rOneCard, index: 0 } });
  expect(screen.queryByText("This was already started.")).toBeNull();
});
