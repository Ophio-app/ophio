// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import type { Run, Task } from "../../protocol/types";
import { RunPicker } from "../work/RunPicker";
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
const tick = async (ms = 500) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

// A send whose answer was lost is confirmed while the person is already
// writing a different request in the same Review. The Review closes under
// them; their new words survive only as a Talk draft.
it("a confirmed send does not close a Review the person has since changed", async () => {
  const route: Route = { screen: "review", projectId: ids.pGarden, taskId: null, intent: "new_task" };
  app.ui.saveDraft(ids.pGarden, null, { text: "Tidy the planting notes", mode: "ask" });
  app.ui.push(route, "talk");
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
  window.__sim!.dropNextReceipt();
  fireEvent.click(screen.getByRole("button", { name: "Start task" }));
  await tick(500);
  expect(app.ui.draft(ids.pGarden, null)?.attempt?.phase).toBe("unknown");
  // The person moves on to a new request in the same Review.
  fireEvent.change(screen.getByRole("textbox", { name: "What you’re asking" }), { target: { value: "Water the tomatoes" } });
  await tick(6000);
  expect(app.ui.draft(ids.pGarden, null)?.text).toBe("Water the tomatoes");
  expect(app.ui.get().stacks.talk.at(-1)).toBe(route);
});

// A run recorded before the host kept Run.mode takes the task's current
// mode. A Plan task carried out to Work, whose plan run predates Run.mode,
// then lists that plan run as Work that finished.
it("a Plan run recorded without its mode is not listed as Work after the task is carried out", () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const host = window.__sim!.host;
  const task: Task = { ...host.tasks[0]!, mode: "work", state: "working" };
  const { mode: _, ...base } = host.runs[0]!;
  const plan: Run = { ...base, run_id: "plan", task_id: task.task_id, state: "completed", admitted_at: Date.UTC(2026, 8, 20, 9) };
  const work: Run = { ...base, run_id: "work", task_id: task.task_id, mode: "work", state: "working", admitted_at: Date.UTC(2026, 8, 28, 9) };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}>
    <RunPicker task={{ ...task, current_run: work.run_id }} runs={[work, plan]} latest={work} selectedId={plan.run_id} onChange={() => {}} />
  </RouteProvider></AppProvider>);
  const shown = screen.getByRole("button", { name: /^Choose a run: showing Run 1/ });
  expect(shown.textContent).not.toMatch(/Work/);
});
