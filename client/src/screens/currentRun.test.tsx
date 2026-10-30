// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { allGrants } from "../dev/sim/host";
import * as fx from "../dev/sim/scenario";
import type { Platform } from "../host/link";
import { HostSession } from "../host/session";
import type { Approval, Project, Run, Task } from "../protocol/types";
import { applyEvent } from "../state/projection";
import { RouteProvider } from "../shell/Screen";
import { pictureTaskLabel } from "./control/Previews";
import Talk from "./talk/Talk";
import { TalkHeader } from "./talk/TalkHeader";
import { Transcript } from "./talk/Transcript";
import { latestRun } from "./work/logic";
import { TaskRow } from "./work/shared";
import Work from "./work/Work";
import { hasEndedTask } from "./work/logic";

const now = 1_800_000_000_000;
let app: App;
let task: Task;
let run: Run;
beforeEach(() => {
  localStorage.clear();
  task = { ...fx.tasks(now).find(t => t.task_id === fx.ids.tDash)!, state: "working", status_line: null };
  run = { ...fx.runs(now).find(r => r.run_id === task.current_run)!, state: "working", ended_at: null, children: [] };
  const session = new HostSession("current-run", { connect() {}, disconnect() {}, send: () => false, listen: () => () => {} });
  session.link.set({ state: "online", since: now });
  vi.spyOn(session, "loadTimeline").mockResolvedValue(undefined);
  app = {
    session, ui: new UiStore("current-run"), profiles: [], switchTo() {}, pairAnother() {}, async rename() {}, async forget() {},
    profile: { profileId: "current-run", hostId: "host", name: "Computer", deviceId: "phone", addresses: [], certSha256: "", pairedAt: 0 },
    platform: { kind: "tauri", openExternal: async () => {} } as unknown as Platform,
  };
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => { cleanup(); app.session.stop(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

function seed(approvals: Approval[] = []) {
  app.session.state.set(s => ({ ...s, caughtUp: true, grants: allGrants,
    tasks: { [task.task_id]: task }, runs: { [run.run_id]: run },
    approvals: Object.fromEntries(approvals.map(a => [a.approval_id, a])),
    timelines: { [task.task_id]: { loaded: true, hasMore: false, items: [] } },
  }));
}
async function transcript() {
  let mounted!: ReturnType<typeof render>;
  await act(async () => { mounted = render(<AppProvider app={app}><Transcript task={task} agent="Claude Code" /></AppProvider>); });
  return mounted.container;
}
/** The chat header's back arrow leaves a task for Work, where the project's tasks are listed. */
async function backToTasks(container: HTMLElement, project: Project) {
  // As when the task was opened in Talk, which chooses its project.
  act(() => app.ui.selectProject(project.project_id));
  app.session.state.set((s) => ({ ...s, projects: { ...s.projects, [project.project_id]: project } }));
  await act(async () => { fireEvent.click(container.querySelector(".chat-head__back")!); });
  expect(app.ui.get().tab).toBe("work");
  let list!: ReturnType<typeof render>;
  await act(async () => {
    list = render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 0, active: true, route: { screen: "work" } }}>
      <Work route={{ screen: "work" }} />
    </RouteProvider></AppProvider>);
  });
  // The tab the task is in: what has ended, what is archived, or what is open.
  const tab = task.state === "archived" ? /^Archived/ : hasEndedTask(task) ? /^Finished/ : /^Open/;
  await act(async () => { fireEvent.click(within(list.container).getByRole("radio", { name: tab })); });
  return list.container.querySelector<HTMLElement>(".work-part")!;
}
function decision(resolution: "mark_completed" | "mark_failed"): Run["human_resolution"] {
  return { resolution, at: now, by: { kind: "local_owner" } };
}

async function expectTaskBadges(label: string) {
  expect(pictureTaskLabel(task, app.session.state.get().runs)).toContain(` · ${label} · `);
  const project = fx.projects(now).find(p => p.project_id === task.project_id)!;
  app.session.state.set(s => ({ ...s, projects: { [project.project_id]: project } }));
  app.ui.selectTask(project.project_id, task.task_id);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  let mounted!: ReturnType<typeof render>;
  await act(async () => {
    mounted = render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>
      <Talk route={{ screen: "talk" }} /><TaskRow task={task} />
    </RouteProvider></AppProvider>);
  });
  const { container } = mounted;
  expect(container.querySelector(".chat-head__state")?.textContent).toBe(label);
  expect(container.querySelector(".work-row-state")?.textContent).toContain(`${label} ·`);
  await act(async () => { fireEvent.click(container.querySelector(".chat-head__back")!); });
  expect(app.ui.get().tab).toBe("work");
}

it.each([
  ["accepted", undefined, "Waiting to start"],
  ["dispatching", "dispatching", "Starting"],
  ["working", "working", "Working"],
  ["awaiting_decision", "awaiting_approval", "Needs you"],
  ["awaiting_decision", "awaiting_input", "Needs you"],
  ["interrupted", "interrupted", "Stopped"],
  ["completed", "completed", "Finished"],
  ["failed", "failed", "Failed"],
  ["reconciliation_required", "reconciliation_required", "Outcome unknown"],
  ["archived", "completed", "Archived"],
] as const)("picture badge agrees with Chat, Tasks and switcher: %s / %s", async (state, runState, label) => {
  task = { ...task, state, current_run: runState ? run.run_id : null };
  run = { ...run, state: runState ?? "admitted", ended_at: ["interrupted", "completed", "failed", "reconciliation_required"].includes(runState ?? "") ? now : null };
  seed();
  if (!runState) app.session.state.set(s => ({ ...s, runs: {} }));
  const runs = app.session.state.get().runs;
  expect(pictureTaskLabel(task, runs)).toContain(` · ${label} · `);
  const project = fx.projects(now).find(p => p.project_id === task.project_id)!;
  const { container } = render(<AppProvider app={app}><TalkHeader project={project} task={task} run={runState ? run : undefined} /><TaskRow task={task} /></AppProvider>);
  expect(container.querySelector(".chat-head__sub")?.textContent).toContain(label);
  expect(container.querySelector(".work-row-state")?.textContent).toContain(`${label} ·`);
  expect((await backToTasks(container, project)).textContent).toContain(label);
});

it.each(["mark_completed", "mark_failed"] as const)("picture keeps a human decision distinct from a verified outcome: %s", resolution => {
  task = { ...task, state: resolution === "mark_completed" ? "completed" : "failed" };
  run = { ...run, state: task.state as Run["state"], ended_at: now, human_resolution: decision(resolution) };
  expect(pictureTaskLabel(task, { [run.run_id]: run })).toContain(resolution === "mark_completed" ? " · Marked completed · " : " · Marked failed · ");
});

it("picture force-stop label follows the interrupted task", () => {
  task = { ...task, state: "interrupted" };
  run = { ...run, state: "interrupted", ended_at: now, force_stoppable: false };
  expect(pictureTaskLabel(task, { [run.run_id]: run })).toContain(" · Stopped · ");
});

it("picture chooses the new current run without carrying an older human decision forward", () => {
  const old: Run = { ...run, run_id: "old-run", state: "completed", admitted_at: run.admitted_at - 1, ended_at: now - 10, human_resolution: decision("mark_completed") };
  expect(pictureTaskLabel(task, { [old.run_id]: old, [run.run_id]: run })).toContain(" · Working · ");
});

it("picture fallback chooses the latest run of this task when current_run is absent", () => {
  task = { ...task, state: "failed", current_run: null };
  const old: Run = { ...run, run_id: "old-run", admitted_at: run.admitted_at - 1, human_resolution: decision("mark_completed") };
  const latest: Run = { ...run, state: "failed", ended_at: now, human_resolution: decision("mark_failed") };
  const foreign: Run = { ...old, run_id: "foreign-run", task_id: "other-task", admitted_at: now + 1 };
  expect(pictureTaskLabel(task, { [foreign.run_id]: foreign, [old.run_id]: old, [latest.run_id]: latest })).toContain(" · Marked failed · ");
});

it.each(["mark_completed", "mark_failed"] as const)("an archived picture target remains visibly archived after %s", async resolution => {
  // ArchiveTask changes task.state but deliberately retains the run and decision.
  task = { ...task, state: "archived" };
  run = { ...run, state: resolution === "mark_completed" ? "completed" : "failed", ended_at: now, human_resolution: decision(resolution) };
  expect(pictureTaskLabel(task, { [run.run_id]: run })).toContain(" · Archived · ");
  seed();
  const project = fx.projects(now).find(p => p.project_id === task.project_id)!;
  const { container } = render(<AppProvider app={app}><TalkHeader project={project} task={task} run={run} /><TaskRow task={task} /></AppProvider>);
  expect(container.querySelector(".chat-head__sub")?.textContent).toContain("Archived");
  expect(container.querySelector(".work-row-state")?.textContent).toContain("Archived ·");
  expect((await backToTasks(container, project)).textContent).not.toMatch(/Marked completed|Marked failed/);
});

it.each([
  ["admitted", "Starting"],
  ["working", "Working"],
  ["awaiting_input", "Needs you"],
] as const)("a new %s run's journal event supersedes the old current_run decision across task badges", async (state, label) => {
  task = { ...task, state: "completed" };
  run = { ...run, state: "completed", ended_at: now - 10, human_resolution: decision("mark_completed") };
  seed();
  const next: Run = { ...run, run_id: "next-run", state, admitted_at: now + 1, ended_at: null, human_resolution: undefined };
  // new_run emits RunUpserted before ResumeTask emits TaskUpserted. A disconnect
  // between those events can leave precisely this projection on the phone.
  app.session.state.set(s => applyEvent(s, { seq: 1, at: now + 1, source: { kind: "companion" }, provenance: "native", event: { type: "run_upserted", run: next } }, now + 1));
  const projected = app.session.state.get();
  expect(projected.tasks[task.task_id]!.current_run).toBe(run.run_id);
  expect(latestRun(task, projected.runs)?.run_id).toBe(next.run_id);
  await expectTaskBadges(label);
});

it("another task's current_run record cannot supply task badges with its decision", async () => {
  // Defensive cross-task check; normal host admission does not deliberately
  // create this inconsistent pointer.
  const foreign: Run = { ...run, task_id: "other-task", human_resolution: decision("mark_failed") };
  const own: Run = { ...run, run_id: "own-run" };
  const runs = { [foreign.run_id]: foreign, [own.run_id]: own };
  expect(latestRun(task, runs)?.task_id).toBe(task.task_id);
  expect(latestRun(task, { [foreign.run_id]: foreign })).toBeUndefined();
  seed();
  app.session.state.set(s => ({ ...s, runs }));
  await expectTaskBadges("Working");
});

it("an unavailable new current run does not make starting task badges look completed", async () => {
  // An explicit current_run must not borrow an earlier run's human decision.
  const old: Run = { ...run, state: "completed", ended_at: now - 10, human_resolution: decision("mark_completed") };
  task = { ...task, state: "dispatching", current_run: "new-run-not-loaded" };
  seed();
  app.session.state.set(s => ({ ...s, runs: { [old.run_id]: old } }));
  expect(latestRun(task, app.session.state.get().runs)).toBeUndefined();
  await expectTaskBadges("Starting");
});

it.each([
  ["current", "working", "Working on the file", "Waiting for your approval", false],
  ["earlier", "working", "Working on the file", "Working on the file", true],
  ["other-task", "working", undefined, "Working", true],
  ["none", "awaiting_decision", "Working on the file", "Waiting for you", false],
  ["none", "dispatching", undefined, "Starting", true],
  ["none", "working", "Working on the file", "Working on the file", true],
  ["none", "working", undefined, "Working", true],
] as const)("Transcript state line: approval=%s task=%s live=%s", async (approvalRun, state, activity, text, spinner) => {
  task = { ...task, state };
  const approval = { ...fx.approvals(now)[0]!, task_id: approvalRun === "other-task" ? "other-task" : task.task_id, run_id: approvalRun === "earlier" ? "old-run" : run.run_id };
  seed(approvalRun === "none" ? [] : [approval]);
  if (activity) app.session.state.set(s => ({ ...s, activity: { [task.task_id]: { runId: run.run_id, text: activity, at: now } } }));
  const container = await transcript();
  expect(container.querySelector(".talk-working")?.textContent).toBe(text);
  expect(!!container.querySelector(".talk-working .stateline__spinner")).toBe(spinner);
});

it.each([
  [["question"], "Waiting for your answer"],
  [["question", "command"], "Waiting for your approval"],
] as const)("Transcript names a question as waiting for an answer: %j", async (kinds, text) => {
  const base = fx.approvals(now)[0]!;
  seed(kinds.map((kind, i) => ({ ...base, approval_id: `waiting-${i}`, kind, task_id: task.task_id, run_id: run.run_id })));
  const container = await transcript();
  expect(container.querySelector(".talk-working")?.textContent).toBe(text);
});

it.each(["completed", "failed", "interrupted", "reconciliation_required"] as const)("Transcript suppresses activity after the current run is %s, even with a stale working task", async state => {
  run = { ...run, state, ended_at: now };
  seed();
  app.session.state.set(s => ({ ...s, activity: { [task.task_id]: { runId: run.run_id, text: "Late activity after the run ended", at: now + 1 } } }));
  const container = await transcript();
  expect(container.querySelector(".talk-working")).toBeNull();
  expect(container.textContent).not.toContain("Late activity");
});

it.each([false, true])("Transcript offline waiting=%s shows last-reported text without a spinner", async waiting => {
  task = { ...task, state: waiting ? "awaiting_decision" : "working" };
  seed();
  app.session.link.set({ state: "offline", since: now, reason: "lost", retryAt: null });
  const container = await transcript();
  expect(container.querySelector(".talk-working")?.textContent).toContain(waiting ? "Waiting for you · last reported" : "Working · last reported");
  expect(container.querySelector(".talk-working .stateline__spinner")).toBeNull();
});

it.each(["awaiting_input", "awaiting_approval"] as const)("an %s run stops the spinner before its task summary catches up", async state => {
  task = { ...task, state: "dispatching" };
  run = { ...run, state: "admitted" };
  seed();
  // A run can start waiting before its task does. Its RunUpserted precedes
  // TaskUpserted, just as ended runs precede the terminal task summary.
  const waitingRun: Run = { ...run, state, holds_writer: false };
  app.session.state.set(s => applyEvent(s, { seq: 1, at: now + 1, source: { kind: "companion" }, provenance: "native", event: { type: "run_upserted", run: waitingRun } }, now + 1));
  app.session.state.set(s => ({ ...s, activity: { [task.task_id]: { runId: run.run_id, text: "Working on the file", at: now } } }));
  const container = await transcript();
  expect(container.querySelector(".talk-working .stateline__spinner")).toBeNull();
  expect(container.querySelector(".talk-working")?.textContent).toBe("Waiting for you");
});

it.each(["earlier", "other-task"])("an %s run waiting for input does not stop the current run's spinner", async owner => {
  task = { ...task, state: "dispatching" };
  seed();
  const waitingRun: Run = { ...run, run_id: "another-run", task_id: owner === "earlier" ? task.task_id : "other-task", admitted_at: run.admitted_at - 1, state: "awaiting_input" };
  app.session.state.set(s => ({ ...s, runs: { ...s.runs, [waitingRun.run_id]: waitingRun } }));
  const container = await transcript();
  expect(container.querySelector(".talk-working .stateline__spinner")).not.toBeNull();
  expect(container.querySelector(".talk-working")?.textContent).toBe("Starting");
});

it.each([true, false])("the waiting Transcript preserves the released-control explanation with online=%s", async (online) => {
  task = { ...task, state: "awaiting_decision", status_line: "You took over" };
  run = { ...run, state: "awaiting_input" };
  seed();
  const [dp1] = fx.control(now);
  app.session.state.set(s => ({ ...s,
    control: { [task.target_id!]: { ...dp1!, input_state: "paused", lease: { ...dp1!.lease!, owner: { kind: "human", device_id: "phone" }, state: "revoked" } } },
    activity: { [task.task_id]: { runId: run.run_id, text: "Working on the file", at: now } },
  }));
  if (!online) app.session.link.set({ state: "offline", since: now, reason: "lost", retryAt: null });
  const container = await transcript();
  expect(container.querySelector(".talk-working .stateline__spinner")).toBeNull();
  const text = container.querySelector(".talk-working")?.textContent;
  expect(text).toContain("Your control ended · resume it when you’re ready");
  expect(text).not.toMatch(/Working on the file|Waiting for you|You took over/);
  if (!online) expect(text).toContain(" · last reported");
});
