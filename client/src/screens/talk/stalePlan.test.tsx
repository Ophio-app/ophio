// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Review from "./Review";
import { reviewPlan } from "./carryPlan";
import type { Command, Run, Task, TimelineBody } from "../../protocol/types";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 500) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const proposal = (plan: string): TimelineBody => ({ kind: "activity", tool: "ExitPlanMode", summary: "Plan proposed", status: "succeeded", detail: JSON.stringify({ plan }) });

it("does not carry out a plan that a newer Plan run replaced while Review was open", async () => {
  const host = window.__sim!.host;
  const task: Task = { ...host.tasks.find(t => t.task_id === ids.tHeaders)!, mode: "plan", current_run: ids.rHeaders };
  const run: Run = { ...host.runs.find(r => r.run_id === ids.rHeaders)!, outcome: null, mode: "plan" };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? task : t);
  host.runs = host.runs.filter(r => r.task_id !== task.task_id).concat([run]);
  for (let i = host.journal.length - 1; i >= 0; i--) {
    const e = host.journal[i]!.event;
    if (e.type === "timeline_appended" && e.task_id === task.task_id) host.journal.splice(i, 1);
  }
  const project = { ...host.projects.find(p => p.project_id === task.project_id)!, active_writer: null };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project });
  host.emit({ type: "task_upserted", task });
  host.emit({ type: "run_upserted", run });
  const append = (item: TimelineBody, runId: string) => host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: runId, item } });
  append(proposal("1. Rewrite the header middleware."), run.run_id);
  const pending = reviewPlan(app, task);
  await tick(); await pending;
  const route = app.ui.route();
  if (route.screen !== "review") throw Error("Review missing");
  expect(route.plan).toMatchObject({ number: 1, text: "1. Rewrite the header middleware." });
  const command = vi.spyOn(app.session, "command");
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
  await tick();

  // From another device, the person asked for changes; the next Plan run proposed a different plan.
  const second: Run = { ...run, run_id: "second-plan-run", admitted_at: run.admitted_at + 60_000, state: "completed", ended_at: Date.now() };
  host.runs.push(second);
  act(() => {
    host.emit({ type: "run_upserted", run: second });
    append(proposal("1. Keep the middleware; add headers in the handler instead."), second.run_id);
    host.emit({ type: "task_upserted", task: { ...task, current_run: second.run_id, revision: task.revision + 2, updated_at: Date.now() } });
  });
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "Review current task" }));
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start Work" }));
  await tick();
  const sent = command.mock.calls.map(([body]) => body as Command).filter(body => body.command === "resume_task");
  // Either nothing is sent until the newer plan is reviewed, or the newer plan is what is carried.
  for (const body of sent) expect(body.command === "resume_task" && body.carried_plan?.run_id).toBe(second.run_id);
  if (!sent.length) expect(screen.getByText(/newer plan/)).toBeTruthy();
});
