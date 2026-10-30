// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { TalkPins } from "./TalkHeader";
import { taskBadge, taskRowBadge } from "./TaskBar";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

/** A task whose turn ended after its agent asked to be woken in 20 minutes,
 * or, with `answer`, asked another agent and waits for its answer. */
function watching(answer = false) {
  const host = window.__sim!.host;
  const run = host.runs.find(r => r.run_id === ids.rHeaders)!;
  Object.assign(run, { state: "completed", ended_at: Date.now() - 1000, holds_writer: false });
  const task = host.tasks.find(t => t.task_id === ids.tHeaders)!;
  Object.assign(task, { state: "completed", current_run: run.run_id, wake: answer
    ? { at: Date.now() + 24 * 3_600_000, reason: "Codex’s answer to “Review the API”", answer: true }
    : { at: Date.now() + 20 * 60_000, reason: "the checks on PR #214" } });
  host.emit({ type: "run_upserted", run: { ...run } });
  host.emit({ type: "task_upserted", task: { ...task } });
  return task.task_id;
}

it("shows a task whose agent is waiting between turns as Watching, and Needs you while a question is open", async () => {
  const id = watching();
  await tick();
  const task = app.session.state.get().tasks[id]!;
  expect(taskBadge(task)).toEqual({ label: "Watching", tone: "neutral" });
  expect(taskRowBadge(task, undefined, undefined, "Question waiting · Which region?").label).toBe("Needs you");
  expect(taskBadge({ ...task, state: "working" }).label).toBe("Working");
  expect(taskBadge({ ...task, wake: undefined }).label).toBe("Finished");
});

it("pins what the task is watching, and Stop watching drops the wake without starting anything", async () => {
  const id = watching();
  await tick();
  const view = () => render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}>
    <TalkPins task={app.session.state.get().tasks[id]!} />
  </RouteProvider></AppProvider>);
  const pins = view();
  const pin = screen.getByRole("group", { name: "Watching" });
  expect(pin.textContent).toMatch(/^Watching · the checks on PR #214 · wakes it in 20 min/);
  const runs = Object.keys(app.session.state.get().runs).length;
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Stop watching" })));
  await tick(1500);
  const task = app.session.state.get().tasks[id]!;
  expect(task.wake).toBeUndefined();
  expect(task.state).toBe("completed");
  expect(Object.keys(app.session.state.get().runs)).toHaveLength(runs);
  pins.unmount();
  view();
  expect(screen.queryByRole("group", { name: "Watching" })).toBeNull();
});

it("pins the other agent's answer a task waits for, and on the task asked, the task that asked it", async () => {
  const id = watching(true);
  await tick();
  const host = window.__sim!.host;
  const asked = host.tasks.find((t) => t.task_id !== id && t.project_id === host.tasks.find((u) => u.task_id === id)!.project_id)!;
  Object.assign(asked, { asked_by: id });
  host.emit({ type: "task_upserted", task: { ...asked } });
  await tick();
  const view = (taskId: string) => render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}>
    <TalkPins task={app.session.state.get().tasks[taskId]!} />
  </RouteProvider></AppProvider>);
  expect(taskBadge(app.session.state.get().tasks[id]!)).toEqual({ label: "Waiting", tone: "neutral" });
  const pins = view(id);
  expect(screen.getByRole("group", { name: "Waiting" }).textContent).toMatch(/^Waiting for Codex’s answer to “Review the API” · wakes it when it comes/);
  expect(screen.queryByRole("group", { name: "Watching" })).toBeNull();
  pins.unmount();

  view(asked.task_id);
  const pin = screen.getByRole("group", { name: "Asked by another agent" });
  const title = app.session.state.get().tasks[id]!.title;
  expect(pin.textContent).toContain(`Asked by “${title}” · its answer goes back there`);
  fireEvent.click(screen.getByRole("button", { name: "Open" }));
  const project = app.session.state.get().tasks[id]!.project_id;
  expect(app.ui.get()).toMatchObject({ tab: "talk", taskList: false, taskByProject: { [project]: id } });
});
