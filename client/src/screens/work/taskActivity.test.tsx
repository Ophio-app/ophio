// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids, runs, tasks } from "../../dev/sim/scenario";
import { emptyHostState, isTaskRunning } from "../../state/projection";
import { TaskRow } from "./shared";
import { taskActivity, taskStatus } from "./taskActivity";
let app: App;
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.useRealTimers(); vi.restoreAllMocks(); });
it("force-stopped rows discard last live activity and stale status, including a late event", async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  const host = window.__sim!.host;
  host.stopSafety.ignore(ids.tDash);
  const stop = session.command({ command: "stop_task", task_id: ids.tDash }, { label: "Stop" });
  await vi.advanceTimersByTimeAsync(1600); await stop;
  expect(session.state.get().activity[ids.tDash]?.text).toBe("Still running after Stop");
  const force = session.command({ command: "stop_task", task_id: ids.tDash, force: true }, { label: "Force stop" });
  await vi.advanceTimersByTimeAsync(1600); await force;
  const task = session.state.get().tasks[ids.tDash]!;
  expect(session.state.get().runs[task.current_run!]!.ended_at).not.toBeNull();
  render(<AppProvider app={app}><TaskRow task={task} /></AppProvider>);
  expect(screen.getByText(/Stopped ·/)).toBeTruthy();
  expect(screen.queryByText(/Still running after Stop/)).toBeNull();
  act(() => host.live({ kind: "activity_status", task_id: ids.tDash, run_id: task.current_run!, text: "Late live activity" }));
  expect(screen.queryByText(/Late live activity/)).toBeNull();
  const state = session.state.get();
  const stale = { ...task, state: "working" as const, status_line: "Still running after Stop" };
  expect(taskActivity(state, stale)).toBeUndefined();
  expect(taskStatus(stale, state.runs[task.current_run!])).toBeNull();
});

it("keeps a finished turn running, and stoppable, while helpers it left still finish", () => {
  const now = Date.now();
  const run = { ...runs(now)[0]!, run_id: "r-helpers", state: "completed" as const, ended_at: now, holds_writer: true };
  const task = { ...tasks(now)[0]!, task_id: run.task_id, state: "completed" as const, current_run: run.run_id };
  const state = { ...emptyHostState, tasks: { [task.task_id]: task }, runs: { [run.run_id]: run } };
  expect(isTaskRunning(state, task)).toBe(true);
  expect(taskStatus(task, run)).toBe("Helpers are still finishing");
  // The computer frees the writer once they end.
  const settled = { ...run, holds_writer: false };
  expect(isTaskRunning({ ...state, runs: { [run.run_id]: settled } }, task)).toBe(false);
  expect(taskStatus(task, settled)).toBeNull();
});

it.each(["ask", "plan"] as const)("keeps a finished %s turn running, and stoppable, until the computer confirms what it started ended", (mode) => {
  const now = Date.now();
  const run = { ...runs(now)[0]!, run_id: "r-unconfirmed", state: "completed" as const, ended_at: now, holds_writer: false, cleanup_pending: true };
  const task = { ...tasks(now)[0]!, task_id: run.task_id, mode, state: "completed" as const, current_run: run.run_id };
  const state = { ...emptyHostState, tasks: { [task.task_id]: task }, runs: { [run.run_id]: run } };
  expect(isTaskRunning(state, task)).toBe(true);
  expect(taskStatus(task, run)).toBe("Waiting for what it started to end");
  const confirmed = { ...run, cleanup_pending: false };
  expect(isTaskRunning({ ...state, runs: { [run.run_id]: confirmed } }, task)).toBe(false);
  expect(taskStatus(task, confirmed)).toBeNull();
});
