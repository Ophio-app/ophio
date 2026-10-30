// SPDX-License-Identifier: Apache-2.0
// A message for a running task that never left the phone, whose task then
// finished. Sending it now would start a new run, so the card says so and
// only Review, where the person confirms, may start one.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession, NotConnectedError } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Talk from "./Talk";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  // Nothing scripted changes the task behind the test's back.
  window.__sim!.host.dispose();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
const tick = async (ms = 180) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

it("an unsent follow-up whose task has finished opens Review for a new run instead of starting one", async () => {
  app.ui.selectTask(ids.pTide, ids.tDash);
  app.ui.saveDraft(ids.pTide, ids.tDash, { text: "Also check the legend." });
  const command = vi.spyOn(app.session, "command").mockRejectedValueOnce(new NotConnectedError());
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);
  await tick(900);
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await tick();
  expect(command.mock.calls[0]![0]).toMatchObject({ command: "send_message", task_id: ids.tDash });
  expect(screen.getByRole("button", { name: "Send again" })).toBeTruthy();
  expect(screen.queryByText(/This task has finished/)).toBeNull();

  act(() => app.session.state.set((s) => ({ ...s, tasks: { ...s.tasks, [ids.tDash]: { ...s.tasks[ids.tDash]!, state: "completed" } } })));
  await tick();
  expect(screen.getByText(/This task has finished; sending starts a new run\./)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Review and send again" }));
  await tick();
  expect(command).toHaveBeenCalledTimes(1);
  expect(app.ui.route()).toEqual({ screen: "review", projectId: ids.pTide, taskId: ids.tDash, intent: "continue" });
  expect(app.ui.draft(ids.pTide, ids.tDash)).toMatchObject({ text: "Also check the legend.", attempt: null });
});
