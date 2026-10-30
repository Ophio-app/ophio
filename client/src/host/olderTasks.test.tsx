// SPDX-License-Identifier: Apache-2.0
//
// A snapshot carries what is live and the most recent finished tasks. The
// phone reads older ones a page at a time when the person reaches the end of
// a list, and a task it does not hold when its receipt opens.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, expect, it } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore, type Route } from "../app/ui";
import { allGrants } from "../dev/sim/host";
import * as fx from "../dev/sim/scenario";
import { platform, profile, snapshot } from "../platform/lifecycle.fixtures";
import type { ClientFrame, ProtocolError, Query, QueryData, Run, Snapshot, Task, TaskCursor, TimelineItem } from "../protocol/types";
import { Transcript } from "../screens/talk/Transcript";
import Receipt from "../screens/work/Receipt";
import Work from "../screens/work/Work";
import { RouteProvider } from "../shell/Screen";
import { applyOlderTasks, applyTaskRecords, refreshFromSnapshot } from "../state/projection";
import type { ControlLink, LinkEvent } from "./link";
import { HostSession } from "./session";

class HeldLink implements ControlLink {
  sent: ClientFrame[] = [];
  #listener: ((e: LinkEvent) => void) | undefined;
  #answered = new Set<string>();
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
    this.sent.push(frame);
    return true;
  }
  listen(listener: (e: LinkEvent) => void) {
    this.#listener = listener;
    return () => { this.#listener = undefined; };
  }
  emit(e: LinkEvent) {
    this.#listener?.(e);
  }
  /** The latest unanswered query of this kind. */
  asked<K extends QueryData["kind"]>(kind: K) {
    const frame = this.sent.findLast((f) => f.type === "query" && f.query.kind === kind && !this.#answered.has(f.query_id));
    if (frame?.type !== "query") throw new Error(`No ${kind} query`);
    return frame;
  }
  answer(data: QueryData, which = (_: Query) => true) {
    const frame = this.sent.findLast((f) => f.type === "query" && f.query.kind === data.kind && which(f.query) && !this.#answered.has(f.query_id));
    if (frame?.type !== "query") throw new Error(`No ${data.kind} query`);
    this.#answered.add(frame.query_id);
    this.emit({ type: "frame", frame: { type: "query_result", query_id: frame.query_id, result: { Ok: data } } });
  }
  refuse(kind: QueryData["kind"], error: ProtocolError) {
    const { query_id } = this.asked(kind);
    this.#answered.add(query_id);
    this.emit({ type: "frame", frame: { type: "query_result", query_id, result: { Err: error } } });
  }
  queried(kind: QueryData["kind"]): Query[] {
    return this.sent.flatMap((f) => (f.type === "query" && f.query.kind === kind ? [f.query] : []));
  }
}

const project = fx.ids.pGarden;
const template = snapshot();
const finished = template.tasks.find((t) => t.task_id === fx.ids.tHeaders)!;
const finishedRun = template.runs.find((r) => r.run_id === fx.ids.rHeaders)!;
function older(n: number): { task: Task; run: Run } {
  const task_id = `0192f7a1-0000-7000-8000-00000000f${String(n).padStart(3, "0")}`;
  const run_id = `0192f7a1-0000-7000-8000-00000000e${String(n).padStart(3, "0")}`;
  return {
    task: { ...finished, task_id, project_id: project, title: `Older task ${n}`, updated_at: 100 - n, current_run: run_id },
    run: { ...finishedRun, run_id, task_id },
  };
}
const cursor = (task: Task): TaskCursor => ({ updated_at: task.updated_at, task_id: task.task_id });
const records = (...items: { task: Task; run: Run }[]) => ({
  tasks: items.map((i) => i.task), runs: items.map((i) => i.run), approvals: [], attention: [],
});
const [one, two] = [older(1), older(2)];
const flush = () => act(async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); });

let link: HeldLink;
let app: App;
let snap: Snapshot;
beforeEach(async () => {
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  link = new HeldLink();
  const session = new HostSession(profile.profileId, link);
  app = { platform: platform().value, profile, profiles: [profile], session, ui: new UiStore(profile.profileId),
    switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  app.ui.selectProject(project);
  // The snapshot left out this project's finished tasks.
  snap = { ...template, journal_head: 10, tasks: template.tasks.filter((t) => t.project_id !== project),
    older_tasks: { from: cursor(one.task), projects: [project] } };
  session.start();
  link.emit({ type: "authenticated", challenge: { selected: { major: 1, minor: 0 }, host_id: snap.host.host_id, boot_epoch: fx.ids.boot, server_nonce: "fixture" },
    auth: { host_id: snap.host.host_id, device_id: profile.deviceId, grants: allGrants, capabilities: fx.capabilities(), companion_version: "test", journal_head: 10 } });
  link.answer({ kind: "snapshot", ...snap });
  await flush();
  link.emit({ type: "frame", frame: { type: "caught_up", head: 10 } });
});
afterEach(() => {
  cleanup();
  app.session.stop();
});
function view(child: ReactNode, route: Route) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 1, active: true, route }}>{child}</RouteProvider></AppProvider>);
}

it("Tasks › Finished reads older tasks a page at a time", async () => {
  view(<Work route={{ screen: "work" }} />, { screen: "work" });
  fireEvent.click(screen.getByRole("radio", { name: /^Finished/ }));
  expect(screen.queryByText("Older task 1")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Show earlier tasks" }));
  expect(link.asked("task_history").query).toEqual({ kind: "task_history", project_id: project, from: cursor(one.task), limit: 50 });
  act(() => link.answer({ kind: "task_history", records: records(one), next: cursor(two.task) }));
  await flush();
  expect(screen.getByText("Older task 1")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Show earlier tasks" }));
  expect(link.asked("task_history").query).toMatchObject({ from: cursor(two.task) });
  act(() => link.answer({ kind: "task_history", records: records(two), next: null }));
  await flush();
  expect(screen.getByText("Older task 2")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Show earlier tasks" })).toBeNull();
});

it("Tasks › Archived keeps older archived tasks to read, and search finds them", async () => {
  view(<Work route={{ screen: "work" }} />, { screen: "work" });
  fireEvent.click(screen.getByRole("radio", { name: /^Archived/ }));
  fireEvent.click(screen.getByRole("button", { name: "Show earlier tasks" }));
  const shelved = [1, 2, 3, 4, 5, 6].map((n) => { const o = older(n); return { ...o, task: { ...o.task, state: "archived" as const } }; });
  act(() => link.answer({ kind: "task_history", records: records(...shelved), next: null }));
  await flush();
  expect(screen.getByText("Older task 1")).toBeTruthy();
  // Archived is read: nothing there archives again.
  expect(screen.queryByRole("button", { name: /^Archive/ })).toBeNull();
  fireEvent.change(screen.getByLabelText("Find an archived task"), { target: { value: "task 2" } });
  expect(screen.queryByText("Older task 1")).toBeNull();
  expect(screen.getByText("Older task 2")).toBeTruthy();
});

it("a receipt reads a task the phone does not hold, and its run whole for the reported diffs", async () => {
  view(<Receipt route={{ screen: "receipt", taskId: one.task.task_id }} />, { screen: "receipt", taskId: one.task.task_id });
  expect(screen.getByText("Reading this task from the computer…")).toBeTruthy();
  expect(link.asked("task").query).toEqual({ kind: "task", task_id: one.task.task_id });
  // Lists carry runs without the diffs their agents reported.
  const file = { path: "src/orders.rs", change: "modified" as const, added: 3, removed: 1, provenance: "native" as const };
  const listed = { ...one.run, outcome: { ...one.run.outcome!, changed_files: [file] } };
  act(() => link.answer({ kind: "task", records: { ...records(one), runs: [listed] } }));
  await flush();
  expect(screen.getByText("Older task 1")).toBeTruthy();
  expect(screen.getByText("No diff was recorded by the agent.", { exact: false })).toBeTruthy();
  expect(link.asked("run").query).toEqual({ kind: "run", run_id: one.run.run_id });
  const whole = { ...listed, outcome: { ...listed.outcome, changed_files: [{ ...file, diff: { text: "+ rate limit", truncated: false } }] } };
  act(() => link.answer({ kind: "run", run: whole, approvals: [], attention: [] }));
  await flush();
  expect(screen.getByText("Open the recorded diff", { exact: false })).toBeTruthy();
});

it("older tasks read stay through a refreshed snapshot, and a stale page does not move the list", () => {
  const read = applyOlderTasks(app.session.state.get(), project, cursor(one.task), records(one), cursor(two.task));
  expect(read.olderTasks[project]).toEqual(cursor(two.task));
  // A page asked for from a place the list has since left adds its tasks only.
  const stale = applyOlderTasks(read, project, cursor(one.task), records(two), null);
  expect(stale.olderTasks[project]).toEqual(cursor(two.task));
  expect(stale.tasks[two.task.task_id]).toBeTruthy();
  // What the phone holds is newer than a page: the journal keeps it current.
  const renamed = { ...one.task, title: "Renamed on the computer" };
  expect(applyTaskRecords({ ...read, tasks: { ...read.tasks, [one.task.task_id]: renamed } }, records(one)).tasks[one.task.task_id]).toBe(renamed);
  const refreshed = refreshFromSnapshot(read, { ...snap, journal_head: 11 }, 5);
  expect(refreshed.tasks[one.task.task_id]).toEqual(one.task);
  expect(refreshed.runs[one.run.run_id]).toEqual(one.run);
  expect(refreshed.olderTasks[project]).toEqual(cursor(two.task));
});

it("a receipt says a task is not available once the computer has no record of it", async () => {
  view(<Receipt route={{ screen: "receipt", taskId: two.task.task_id }} />, { screen: "receipt", taskId: two.task.task_id });
  act(() => link.refuse("task", { code: "not_found", message: "Record not found" }));
  await flush();
  expect(screen.queryByText("Reading this task from the computer…")).toBeNull();
  expect(screen.getByText("Task not available")).toBeTruthy();
  expect(screen.getByText("The computer hasn’t provided a record for this task.")).toBeTruthy();
});

it("a computer from before task history is never asked for it", async () => {
  // It would close the connection on a query it does not know.
  app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: [] } }));
  const held = template.tasks.find((t) => t.task_id === fx.ids.tHeaders)!;
  view(<Receipt route={{ screen: "receipt", taskId: one.task.task_id }} />, { screen: "receipt", taskId: one.task.task_id });
  await flush();
  expect(screen.getByText("Task not available")).toBeTruthy();
  cleanup();
  view(<Receipt route={{ screen: "receipt", taskId: held.task_id }} />, { screen: "receipt", taskId: held.task_id });
  await flush();
  expect(link.queried("task")).toEqual([]);
  expect(link.queried("run")).toEqual([]);
});

const earlier = { ...one.run, run_id: "0192f7a1-0000-7000-8000-00000000e999", admitted_at: 1 };
const note = (seq: number, run_id: string): TimelineItem => ({ seq, at: seq, run_id, item: { kind: "note", text: `Step ${seq}` } });

it("a receipt reads the earlier runs its conversation names", async () => {
  app.session.state.set((s) => applyTaskRecords(s, records(one)));
  view(<Receipt route={{ screen: "receipt", taskId: one.task.task_id }} />, { screen: "receipt", taskId: one.task.task_id });
  act(() => link.answer({ kind: "task_timeline", items: [note(1, earlier.run_id), note(2, one.run.run_id)], has_more: false }));
  await flush();
  // Whole runs are asked for one at a time: the receipt's own run first.
  expect(link.queried("run")).toEqual([{ kind: "run", run_id: one.run.run_id }]);
  act(() => link.answer({ kind: "run", run: one.run, approvals: [], attention: [] }));
  await flush();
  expect(link.asked("run").query).toEqual({ kind: "run", run_id: earlier.run_id });
  const decided = { ...template.approvals[0]!, approval_id: "0192f7a1-0000-7000-8000-00000000c001", run_id: earlier.run_id, task_id: one.task.task_id, state: "allowed" as const };
  act(() => link.answer({ kind: "run", run: earlier, approvals: [decided], attention: [] }, (q) => q.kind === "run" && q.run_id === earlier.run_id));
  await flush();
  expect(app.session.state.get().runs[earlier.run_id]).toEqual(earlier);
  expect(app.session.state.get().approvals[decided.approval_id]).toEqual(decided);
});

it("a conversation reads a decision on an earlier run with that run", async () => {
  app.session.state.set((s) => applyTaskRecords(s, records(one)));
  const decided = { ...template.approvals[0]!, approval_id: "0192f7a1-0000-7000-8000-00000000c002", run_id: earlier.run_id, task_id: one.task.task_id, state: "allowed" as const, title: "Run the migration" };
  const asked: TimelineItem = { seq: 3, at: 3, run_id: earlier.run_id, item: { kind: "approval", approval_id: decided.approval_id } };
  app.session.state.set((s) => ({ ...s, timelines: { ...s.timelines, [one.task.task_id]: { items: [asked], hasMore: false, loaded: true } } }));
  view(<Transcript task={one.task} agent="Codex" />, { screen: "talk" });
  await flush();
  expect(link.queried("run")).toEqual([{ kind: "run", run_id: earlier.run_id }]);
  act(() => link.answer({ kind: "run", run: earlier, approvals: [decided], attention: [] }));
  await flush();
  expect(screen.queryByText("An approval request that this device can’t see.")).toBeNull();
  expect(screen.getAllByText("Run the migration", { exact: false }).length).toBeGreaterThan(0);
});
