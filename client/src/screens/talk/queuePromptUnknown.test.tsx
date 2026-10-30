// SPDX-License-Identifier: Apache-2.0
// A queued task's Review saves prompt edits and starts the task. When the
// answer to "Save prompt changes" is lost and the computer later confirms it,
// the phone must not say the task started: it is still waiting in Next up.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import * as notifications from "../../design/Toast";
import * as announcer from "../../design/Announcer";
import Review from "./Review";

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

it("a prompt edit confirmed after a lost answer is not reported as a start", async () => {
  const toast = vi.spyOn(notifications, "toast");
  const announce = vi.spyOn(announcer, "announce");
  const host = window.__sim!.host;
  const item = host.queue.items.find(i => i.state.state !== "started")!;
  const task = host.tasks.find(t => t.task_id === item.task_id)!;
  const route: Route = { screen: "review", projectId: task.project_id, taskId: task.task_id, queueItemId: item.item_id };
  app.ui.push({ screen: "next_up" }, "work"); app.ui.push(route, "work");
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 2, active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
  await tick(900);

  fireEvent.change(screen.getByLabelText("Queued prompt"), { target: { value: "Rewrite the README intro only" } });
  // The computer saves the edit, but its answer is lost with the connection.
  window.__sim!.dropNextReceipt();
  fireEvent.click(screen.getByRole("button", { name: "Save prompt changes" }));
  await tick(500);
  // Reconnected, the phone learns the edit was accepted.
  await tick(6000);
  expect(host.queue.items.find(i => i.item_id === item.item_id)?.request?.prompt).toBe("Rewrite the README intro only");

  // The task has not started; nothing may say it did, and its Review stays.
  expect(host.queue.items.find(i => i.item_id === item.item_id)?.state.state).not.toBe("started");
  const said = [...toast.mock.calls, ...announce.mock.calls].map(([text]) => text);
  expect(said.filter(text => /Started/.test(text))).toEqual([]);
  expect(app.ui.get().stacks.work).toContainEqual(route);
});
