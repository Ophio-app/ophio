// SPDX-License-Identifier: Apache-2.0
// Summarize asks a stopped or finished task's own agent, read-only, what
// happened: one tap, offered even while another task holds the project, and
// the task is left as it was.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession, NotConnectedError } from "../../host/session";
import type { Run, Task, TimelineBody } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { planProposed } from "../work/logic";
import { findCarryPlan } from "./carryPlan";
import { READ_ONLY_TURN_FEATURE, SUMMARY_PROMPT, summarizable } from "./summarize";
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

/** The finished Codex task, without its GitHub link, on its kept session. */
function finished(writer: string | null, patch: Partial<Task> = {}) {
  const host = window.__sim!.host;
  const task: Task = { ...host.tasks.find(t => t.task_id === ids.tHeaders)!, github: undefined, mode: "work", current_run: ids.rHeaders, ...patch };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? task : t);
  const project = { ...host.projects.find(p => p.project_id === task.project_id)!, active_writer: writer };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project });
  host.emit({ type: "task_upserted", task });
  // Already read, so opening it sends nothing but the Summarize.
  for (const a of host.attention.filter(a => a.task_id === task.task_id)) host.emit({ type: "attention_resolved", attention_id: a.attention_id });
  host.attention = host.attention.filter(a => a.task_id !== task.task_id);
  app.ui.selectTask(task.project_id, task.task_id);
  return { host, task };
}
const talk = () => render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);

it("asks the task's own agent read-only while another task holds the project, and leaves the task and draft as they were", async () => {
  const { host, task } = finished(ids.rFlaky);
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Keep this for later" });
  const command = vi.spyOn(app.session, "command");
  talk();
  await tick(900);
  expect(screen.getByRole("button", { name: "Open the blocking task" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Summarize" }));
  await tick();
  expect(command.mock.calls[0]![0]).toEqual({ command: "resume_task", task_id: task.task_id, text: SUMMARY_PROMPT, read_only: true });
  expect(command.mock.calls[0]![1]).toMatchObject({ label: "Summarize", expectedRevision: task.revision });
  const asked = host.runs.at(-1)!;
  expect(asked).toMatchObject({ task_id: task.task_id, mode: "ask", holds_writer: false, requested_model: task.model, requested_effort: task.effort });
  expect(app.ui.draft(task.project_id, task.task_id)).toMatchObject({ text: "Keep this for later", attempt: null });

  await tick(8_000);
  const state = app.session.state.get();
  expect(state.runs[asked.run_id]!.state).toBe("completed");
  expect(state.tasks[task.task_id]).toMatchObject({ state: "completed", mode: "work" });
  expect(state.projects[task.project_id]!.active_writer).toBe(ids.rFlaky);
});

it("tries a Summarize that never left again as Summarize, not as what is typed", async () => {
  const { task } = finished(null);
  const command = vi.spyOn(app.session, "command").mockRejectedValueOnce(new NotConnectedError());
  talk();
  await tick(900);
  fireEvent.click(screen.getByRole("button", { name: "Summarize" }));
  await tick();
  expect(app.ui.draft(task.project_id, task.task_id)?.attempt).toMatchObject({ phase: "not_delivered", readOnly: true });
  act(() => app.ui.saveDraft(task.project_id, task.task_id, { text: "Typed since" }));
  fireEvent.click(screen.getByRole("button", { name: "Send again" }));
  await tick();
  expect(command).toHaveBeenCalledTimes(2);
  expect(command.mock.calls[1]![0]).toMatchObject({ command: "resume_task", text: SUMMARY_PROMPT, read_only: true });
  expect(app.ui.draft(task.project_id, task.task_id)).toMatchObject({ text: "Typed since", attempt: null });
});

it("isn't offered by a computer that can't ask read-only, for a running task, or without a kept session", async () => {
  const { host, task } = finished(null);
  const codex = app.session.state.get().harnesses.find(h => h.kind === "codex")!;
  const kept = host.runs.find(r => r.run_id === ids.rHeaders)!;
  expect(summarizable(task, kept, codex)).toBe(true);
  // The computer resumes the current run's session, not an older one's.
  expect(summarizable({ ...task, current_run: null }, kept, codex)).toBe(false);
  act(() => app.session.state.set(s => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter(f => f !== READ_ONLY_TURN_FEATURE) } })));
  talk();
  await tick(900);
  expect(screen.queryByRole("button", { name: "Summarize" })).toBeNull();
  cleanup();

  act(() => app.session.state.set(s => ({ ...s, capabilities: { ...s.capabilities!, features: [...s.capabilities!.features, READ_ONLY_TURN_FEATURE] } })));
  const run = host.runs.find(r => r.run_id === ids.rHeaders)!;
  act(() => host.emit({ type: "run_upserted", run: { ...run, native_session: null } }));
  talk();
  await tick();
  expect(screen.queryByRole("button", { name: "Summarize" })).toBeNull();
  cleanup();

  act(() => host.emit({ type: "run_upserted", run }));
  act(() => host.emit({ type: "task_upserted", task: { ...task, state: "working" } }));
  talk();
  await tick();
  expect(screen.queryByRole("button", { name: "Summarize" })).toBeNull();
});

it("leaves a proposed plan proposed and carried from its own run after a summary", () => {
  const host = window.__sim!.host;
  const plan: Run = { ...host.runs.find(r => r.run_id === ids.rHeaders)!, mode: "plan", outcome: null };
  const asked: Run = { ...plan, run_id: "asked", mode: "ask", admitted_at: plan.admitted_at + 10_000 };
  const task: Task = { ...host.tasks.find(t => t.task_id === ids.tHeaders)!, github: undefined, mode: "plan", state: "completed", current_run: asked.run_id };
  const item = (run: Run, body: TimelineBody, seq: number) => ({ seq, at: seq, run_id: run.run_id, item: body });
  const timelines = { [task.task_id]: { loaded: true, hasMore: false, items: [
    item(plan, { kind: "activity", tool: "ExitPlanMode", summary: "Plan proposed", status: "succeeded", detail: JSON.stringify({ plan: "1. Add the headers." }) }, 1),
    item(asked, { kind: "agent_message", text: "Here’s what happened: I proposed a plan.", partial: false }, 2),
  ] } } as never;
  expect(findCarryPlan(task, { runs: { [plan.run_id]: plan, [asked.run_id]: asked }, timelines })).toEqual({ text: "1. Add the headers.", runId: plan.run_id, number: 1, latest: true });
  expect(planProposed(task, asked)).toBe(true);
  expect(planProposed({ ...task, state: "interrupted" }, asked)).toBe(false);
});
