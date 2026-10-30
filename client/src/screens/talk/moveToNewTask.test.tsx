// SPDX-License-Identifier: Apache-2.0
// Writing on a task whose composer can only start a new task carries the
// words over to the project's new-task draft. That draft may still hold a
// start whose answer was lost: carrying words into it must not forget that
// start, or the next Start sends the same request again as a second task.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { createTaskCommand, resolveAgent } from "./intent";
import Review from "./Review";
import { send } from "./send";
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
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
const tick = async (ms = 180) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

const first = "Tidy the retry notes";
const later = "Also update the changelog";

it("carrying words to the new-task draft keeps a start whose answer was lost", async () => {
  const host = window.__sim!.host;
  const received = vi.spyOn(host, "command");
  const started = () => new Set(received.mock.calls
    .filter(([env]) => env.body.command === "create_task" && env.body.prompt.includes(first))
    .map(([env]) => env.command_id)).size;

  // A new task is started in Tide; the computer creates it, but the answer
  // is lost with the connection.
  const s = app.session.state.get();
  const agent = resolveAgent(s.projects[ids.pTide], s.harnesses, null)!;
  app.ui.saveDraft(ids.pTide, null, { text: first });
  window.__sim!.dropNextReceipt();
  const out = send(app, {
    projectId: ids.pTide, draftTaskId: null, text: first, annotationId: null, label: "Add to Next up",
    build: (bundle) => createTaskCommand({ projectId: ids.pTide, prompt: first, title: null, agent, mode: "work",
      placement: { mode: "queue", position: null, authorization: "ask_before_start" }, mission: null, bundle }),
  }, () => {});
  await tick(600);
  expect((await out).ok).toBe(false);
  expect(app.ui.draft(ids.pTide, null)?.attempt?.phase).toBe("unknown");
  expect(started()).toBe(1);
  while (app.session.link.get().state !== "online") await tick(500);
  await tick(1000);

  // Later, on a task still waiting in Next up, the person writes more. Its
  // composer can only start a new task, so Review carries the words over.
  app.ui.selectTask(ids.pTide, ids.tReadme);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);
  await tick(900);
  fireEvent.change(screen.getByRole("textbox"), { target: { value: later } });
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  await tick();
  // The new-task page now in Talk learns that the first start arrived, so
  // only the later words are left to send.
  expect(app.ui.draft(ids.pTide, null)).toMatchObject({ text: later, attempt: null });

  // Starting from that Review never makes the computer create the first
  // request a second time.
  cleanup();
  const route = app.ui.route() as Extract<Route, { screen: "review" }>;
  expect(route).toMatchObject({ screen: "review", projectId: ids.pTide, taskId: null });
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
  await tick(900);
  const primary = document.querySelector<HTMLButtonElement>(".review-foot .btn--primary")!;
  if (!primary.disabled) fireEvent.click(primary);
  await tick(3000);
  expect(started()).toBe(1);
  expect(received.mock.calls.some(([env]) => env.body.command === "create_task" && env.body.prompt === later)).toBe(true);
});
