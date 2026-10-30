// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { QueueReview } from "../talk/QueueReview";
import NextUp from "./NextUp";
import { workIds } from "../../dev/sim/work";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "work" }, depth: 1, active: true }}>{children}</RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
  app.ui.selectTask(ids.pTide, null);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});


function queueChange(patch: Partial<import("../../protocol/types").QueueView>) {
  const host = window.__sim!.host;
  act(() => { host.queue = { ...host.queue, ...patch }; host.emit({ type: "queue_changed", queue: host.queue }); });
}
it("explains an initial pause and a Stop pause, shows only the latest queue receipt", async () => {
  queueChange({ paused: true, pause_reason: "Next up starts paused. Resume it to let tasks start." });
  render(<NextUp route={{ screen: "next_up" }} />, { wrapper }); await advance();
  expect(screen.getByText("Next up starts paused. Resume it to let tasks start.")).toBeTruthy();
  for (let i = 0; i < 3; i++) {
    fireEvent.click(screen.getByRole("button", { name: "Resume queue" })); await advance();
    queueChange({ paused: true });
  }
  expect(screen.getAllByText("Sent to studio")).toHaveLength(1);
  queueChange({ pause_reason: "Next up paused when you stopped “Writer”, so nothing starts by itself. Resume when you’re ready." });
  expect(screen.getByText(/Next up paused when you stopped/)).toBeTruthy();
});
it("describes running, stopping, unresolved and absent writers accurately", async () => {
  const host = window.__sim!.host;
  const run = host.runs.find(r => r.run_id === host.queue.writer)!;
  render(<NextUp route={{ screen: "next_up" }} />, { wrapper }); await advance();
  expect(screen.getByText("Waiting for approval. This task holds the project’s writer slot.")).toBeTruthy();
  act(() => host.emit({ type: "run_upserted", run: { ...run, state: "working" } }));
  expect(screen.getByText("Running. This task is changing files.")).toBeTruthy();
  act(() => host.emit({ type: "run_upserted", run: { ...run, state: "stopping" } }));
  expect(screen.getByText("Stopping. The computer is checking what ended.")).toBeTruthy();
  act(() => host.emit({ type: "run_upserted", run: { ...run, state: "reconciliation_required" } }));
  expect(screen.getByText(/Stopped and waiting for your decision/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Decide what happened" }));
  expect(app.ui.route()).toMatchObject({ screen: "receipt", taskId: run.task_id });
  act(() => host.emit({ type: "run_upserted", run: { ...run, state: "completed", holds_writer: true } }));
  expect(screen.getByText("This task has ended. The computer is updating the writer slot.")).toBeTruthy();
  expect(screen.queryByText("Running. This task is changing files.")).toBeNull();
  act(() => host.emit({ type: "run_upserted", run: { ...run, state: "completed", holds_writer: false } }));
  expect(screen.getByText("No task is changing files")).toBeTruthy();
  queueChange({ writer: "unreported-writer" });
  expect(screen.getByText("Waiting for the computer to report the writer’s state.")).toBeTruthy();
  queueChange({ writer: null });
  expect(screen.getByText("No task is changing files")).toBeTruthy();
  expect(screen.queryByText(/current task/)).toBeNull();
});
it("opens the task that is changing files from its row", async () => {
  const host = window.__sim!.host;
  const run = host.runs.find(r => r.run_id === host.queue.writer)!;
  const task = app.session.state.get().tasks[run.task_id]!;
  render(<NextUp route={{ screen: "next_up" }} />, { wrapper }); await advance();
  fireEvent.click(screen.getByRole("button", { name: new RegExp(`^${task.title}`) }));
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.get().taskByProject[task.project_id]).toBe(task.task_id);
});
it("ignores unrelated queue revisions and offers one paused start; protects changed requests and unsaved edits", async () => {
  const host = window.__sim!.host;
  const item = host.queue.items.find(i => i.item_id === workIds.qReady)!;
  render(<QueueReview route={{ screen: "review", projectId: ids.pTide, taskId: item.task_id }} itemId={item.item_id} />, { wrapper }); await advance();
  queueChange({ revision: host.queue.revision + 1, paused: true });
  expect(screen.queryByText("The task or queue changed")).toBeNull();
  expect(screen.queryByText(/Your edits are kept/)).toBeNull();
  // An earlier task still waits, so this one can't start yet and the paused
  // note waits until it could.
  expect(screen.getByText("An earlier task is still waiting.")).toBeTruthy();
  expect(screen.queryByText("Only this task will start. The rest of Next up stays paused.")).toBeNull();
  expect(screen.queryByRole("button", { name: "Resume Next up" })).toBeNull();
  expect(screen.getByRole("button", { name: "Start only this task" })).toBeTruthy();
  expect(host.queue.paused).toBe(true);
  fireEvent.change(screen.getByLabelText("Queued prompt"), { target: { value: "My unsaved changes" } });
  queueChange({ revision: host.queue.revision + 1 });
  expect(screen.getByText("The task or queue changed")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Review current request" }));
  fireEvent.change(screen.getByLabelText("Queued prompt"), { target: { value: item.request?.prompt } });
  queueChange({ revision: host.queue.revision + 1, items: host.queue.items.map(i => i.item_id === item.item_id ? { ...i, request: { prompt: "Changed elsewhere", bundle: null } } : i) });
  expect(screen.getByText("The task or queue changed")).toBeTruthy();
});

it("removes Stop receipt ready after the person decides the outcome", async () => {
  const host = window.__sim!.host;
  const run = host.runs.find(r => r.run_id === host.queue.writer)!;
  const task = host.tasks.find(t => t.task_id === run.task_id)!;
  const promise = app.session.command({ command: "stop_task", task_id: task.task_id, force: false }, { label: "Stop task" });
  await advance(1300); await promise;
  act(() => host.emit({ type: "task_upserted", task: { ...task, state: "reconciliation_required" } }));
  render(<NextUp route={{ screen: "next_up" }} />, { wrapper }); await advance();
  expect(screen.getByText("Stop receipt ready")).toBeTruthy();
  act(() => host.emit({ type: "task_upserted", task: { ...task, state: "completed" } }));
  expect(screen.queryByText("Stop receipt ready")).toBeNull();
});
