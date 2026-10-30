// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, draftKey, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { tasks as fixtureTasks } from "../../dev/sim/scenario";
import { workIds } from "../../dev/sim/work";
import { Toasts } from "../../design/Toast";
import { HostSession } from "../../host/session";
import type { Run, RunReport, Shown, Task, TimelineBody } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Review from "../talk/Review";
import Talk from "../talk/Talk";
import { taskBadge } from "../talk/TaskBar";
import { TalkConversations } from "../talk/Conversations";
import { receiptHeadline, reportLabel } from "./logic";
import { reachesRunStart, runOutcome, runShown } from "./outcome";
import Receipt from "./Receipt";

const report = (r: Partial<RunReport>): RunReport => ({ status: "done", questions: [], remaining: [], new_work: [], ...r });

describe("runOutcome", () => {
  const task = { ...fixtureTasks(1_000_000)[0]!, state: "completed" as const, github: undefined } as Task;
  const run = { run_id: "r", task_id: task.task_id, state: "completed", outcome: { summary: "s", changed_files: [], checks: [], still_active: [], unknowns: [], usage: null }, children: [] } as unknown as Run;
  const withReport = (r: Partial<RunReport>, over: Partial<Run> = {}) => ({ ...run, ...over, outcome: { ...run.outcome!, report: report(r) } });

  it("reads the agent's report of a finished turn", () => {
    expect(runOutcome(task, withReport({ status: "needs_you", questions: ["Which port?"] }))).toMatchObject({ kind: "needs_you", items: ["Which port?"], reported: true });
    expect(runOutcome(task, withReport({ status: "unfinished", remaining: ["Write the tests"] }))).toMatchObject({ kind: "unfinished", detail: "Left to do:", items: ["Write the tests"] });
    expect(runOutcome(task, run)).toMatchObject({ kind: "done", reported: false, newWork: [] });
  });

  it("calls a turn the agent said was done finished, needing a look, while a check failed or work is left", () => {
    const check = { name: "unit tests", command: "npm test", status: "failed" as const, detail: null, provenance: "observed" as const };
    const failed = { ...run, outcome: { ...run.outcome!, checks: [check], report: report({ status: "done" }) } };
    expect(runOutcome(task, failed)).toMatchObject({ kind: "done", title: "Finished, needs a look", detail: "1 check failed.", items: ["unit tests failed"], doubts: ["1 check failed"] });
    expect(receiptHeadline(task, failed)).toBe("Finished — 1 check failed");
    expect(taskBadge({ ...task, current_run: "r" }, failed)).toEqual({ label: "Finished, needs a look", tone: "warn" });
    expect(taskBadge({ ...task, current_run: "r" }, run)).toEqual({ label: "Finished", tone: "ok" });
    const helping = { ...run, children: [{ state: "active" }], outcome: { ...run.outcome!, unknowns: ["Whether the deploy took"] } } as unknown as Run;
    expect(runOutcome(task, helping)).toMatchObject({ title: "Finished, needs a look", detail: "1 helper still running · unknowns remain." });
    expect(runOutcome(task, run)).toMatchObject({ title: "Done", doubts: [] });
  });

  it("lists each failed check no later run of it replaced", () => {
    const check = (command: string, status: "failed" | "passed", after_last_change = true) =>
      ({ name: "Check", command, status, after_last_change, detail: null, provenance: "native" as const });
    const denied = { ...check("never rerun", "failed"), status: "not_run" as const };
    const checks = [check("never rerun", "failed", false), check("fixed", "failed"), check("fixed", "passed"), check("still failing", "failed"), denied];
    const tested = { ...run, outcome: { ...run.outcome!, checks, verification: { status: "failed" as const, passed: 1, failed: 1, unknown: 0, failed_not_rerun: 1 } } };
    // A failure never run again stays one, and a denied rerun replaces nothing.
    expect(runOutcome(task, tested)).toMatchObject({ title: "Finished, needs a look",
      doubts: ["1 check failed", "1 check failed and wasn’t run again"], items: ["never rerun failed", "still failing failed"] });
  });

  it("goes by the run where the run says more than the report", () => {
    expect(runOutcome(task, { ...run, state: "working" })).toBeNull();
    expect(runOutcome(task, { ...run, state: "awaiting_input" })?.kind).toBe("waiting");
    expect(runOutcome(task, withReport({ status: "done", remaining: ["Deploy"] }, { state: "interrupted" }))).toMatchObject({ kind: "stopped", items: ["Deploy"] });
    const marked = withReport({ status: "done" }, { human_resolution: { resolution: "mark_failed", at: 1, by: { kind: "device", device_id: "d" } } });
    expect(runOutcome(task, marked)?.kind).toBe("failed");
  });

  it("offers the new work it found, except a review's, which files its own issues", () => {
    const found = withReport({ new_work: [{ title: "Fix the parser", brief: "Make the parser accept tabs.", mode: "work", issue: 12 }] });
    expect(runOutcome(task, found)?.newWork).toHaveLength(1);
    const review = { ...task, github: { link: { type: "find", query: "q" } } } as unknown as Task;
    expect(runOutcome(review, found)?.newWork).toEqual([]);
  });

  it("keeps what a run showed, each once where it was last shown", () => {
    const pic: Shown = { what: "media", artifact_id: "a1", media_type: "image/png", path: "shot.png", caption: null, invite: null };
    const page: Shown = { what: "preview", preview_id: "p1", label: "Site", url: "http://127.0.0.1:5173", note: null };
    const item = (seq: number, run_id: string | null, item: TimelineBody) => ({ seq, at: seq * 1000, run_id, item });
    const items = [
      item(1, "other", { kind: "shown", shown: pic }),
      item(2, "r", { kind: "run_started", run_id: "r" }),
      item(3, "r", { kind: "shown", shown: pic }),
      item(4, "r", { kind: "shown", shown: page }),
      item(5, "r", { kind: "note", text: "n" }),
      item(6, "r", { kind: "shown", shown: { ...pic, caption: "Again" } }),
    ];
    expect(runShown(items, "r")).toEqual([
      { key: "s4", at: 4000, shown: page },
      { key: "s6", at: 6000, shown: { ...pic, caption: "Again" } },
    ]);
    expect(reachesRunStart(items, "r")).toBe(true);
    expect(reachesRunStart(items.slice(2), "r")).toBe(false);
  });

  it("badges a turn that needs the person or left work, while it is the task's latest word", () => {
    const asks = withReport({ status: "needs_you", questions: ["Which port?"] }, { run_id: task.current_run! });
    expect(reportLabel(task, asks)).toBe("Needs you");
    expect(taskBadge(task, asks)).toEqual({ label: "Needs you", tone: "warn" });
    expect(receiptHeadline(task, asks)).toBe("Needs you");
    expect(reportLabel({ ...task, state: "working" }, asks)).toBeNull();
    expect(reportLabel(task, withReport({ status: "unfinished" }))).toBe("Not finished");
    expect(reportLabel(task, withReport({ status: "done" }))).toBeNull();
  });

  it("reads a turn that asked Ophio to wake it as watching, with nothing left to the person", () => {
    const watching = { ...task, wake: { at: 2_000_000, reason: "the checks on PR #12" } } as Task;
    const left = withReport({ status: "unfinished", remaining: ["Report once CI passes"] }, { run_id: task.current_run! });
    expect(runOutcome(watching, left)).toMatchObject({ kind: "watching", title: "Watching", detail: "the checks on PR #12", items: [] });
    expect(reportLabel(watching, left)).toBeNull();
    expect(taskBadge(watching, left).label).toBe("Watching");
    // An earlier run's report, or one that asks something, still says so.
    expect(runOutcome(watching, withReport({ status: "needs_you", questions: ["Which port?"] }, { run_id: task.current_run! }))?.kind).toBe("needs_you");
    expect(runOutcome(watching, withReport({ status: "unfinished" }))?.kind).toBe("unfinished");
  });

  it("reads a turn that asked another agent as waiting for its answer", () => {
    const waiting = { ...task, wake: { at: 2_000_000, reason: "Codex’s answer to “Review my change”", answer: true } } as Task;
    const left = withReport({ status: "unfinished", remaining: ["Act on the review"] }, { run_id: task.current_run! });
    expect(runOutcome(waiting, left)).toMatchObject({ kind: "watching", title: "Waiting for an answer", detail: "Codex’s answer to “Review my change”", items: [] });
    expect(taskBadge(waiting, left).label).toBe("Waiting");
  });
});

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
function view(child: React.ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{child}<Toasts /></RouteProvider></AppProvider>);
}
/** The receipt task's latest run, ended with `r` as the agent's report;
 * shown on its receipt, or with `talk` in its conversation. */
async function reported(r: Partial<RunReport>, talk = false) {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.receipt)!;
  const run = host.runs.find(x => x.run_id === task.current_run)!;
  const next = { ...run, human_resolution: undefined, outcome: { ...run.outcome!, report: report(r) } };
  host.runs.splice(host.runs.indexOf(run), 1, next);
  host.emit({ type: "run_upserted", run: next });
  await tick(600);
  if (talk) {
    app.ui.selectTask(task.project_id, task.task_id);
    view(<Talk route={{ screen: "talk" }} />);
  } else {
    const route = { screen: "receipt", taskId: task.task_id } as const;
    view(<Receipt route={route} />, route);
  }
  await tick(600);
  return task;
}

it("puts what the agent asks first, with the way to answer", async () => {
  await reported({ status: "needs_you", questions: ["Which port should it listen on?"] });
  const outcome = screen.getByRole("region", { name: "It needs you" });
  expect(within(outcome).getByText("Which port should it listen on?")).toBeTruthy();
  expect(within(outcome).getByRole("button", { name: "Answer" }).className).toContain("btn--primary");
  expect(within(outcome).queryByRole("button", { name: "Archive and start new" })).toBeNull();
  expect(document.querySelector(".work-receipt-state")?.textContent).toMatch(/^Needs you/);
});

it("an answer while another task holds the project waits in Next up", async () => {
  const task = await reported({ status: "needs_you", questions: ["Which port should it listen on?"] });
  const host = window.__sim!.host;
  const holder = host.runs.find(r => r.state === "working" && r.task_id !== task.task_id)!;
  host.projects = host.projects.map(p => p.project_id === task.project_id ? { ...p, active_writer: holder.run_id } : p);
  act(() => host.emit({ type: "project_upserted", project: host.projects.find(p => p.project_id === task.project_id)! }));
  await tick();
  fireEvent.click(within(screen.getByRole("region", { name: "It needs you" })).getByRole("button", { name: "Answer" }));
  await tick();
  // The answer box stays open and its words wait their turn, not a bare "Blocked".
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Port 8080" } });
  expect(document.querySelector(".composer__blocker")?.textContent).toMatch(/^Waits in Next up/);
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(screen.getByRole("button", { name: "Put in Next up" }));
  await tick();
  expect(command.mock.calls[0]![0]).toMatchObject({ command: "queue_continuation", task_id: task.task_id, text: "Port 8080" });
});

/** Adds `body` to the receipt task's conversation, as its latest run's. */
function append(body: TimelineBody, run: boolean | null = true) {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.receipt)!;
  host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: run ? task.current_run : null, item: body } });
}

it("shows what the agent showed with what it asks, where the answer is given", async () => {
  append({ kind: "shown", shown: { what: "media", artifact_id: "sim-missing", media_type: "image/png", path: "review/comparison.png", caption: "Top row is the reference.", invite: null } });
  append({ kind: "shown", shown: { what: "invitation", action: "capture", note: "Capture it at your window size." } });
  await reported({ status: "needs_you", questions: ["Does the glow match?"] });
  const outcome = screen.getByRole("region", { name: "It needs you" });
  expect(within(outcome).getByRole("heading", { name: "What it showed you · 2" })).toBeTruthy();
  expect(within(outcome).getByText("Top row is the reference.")).toBeTruthy();
  expect(within(outcome).getByRole("button", { name: "Capture the screen" })).toBeTruthy();
});

it("reads back to the run's start for what it showed, however far up the conversation", async () => {
  for (let i = 0; i < 90; i++) append({ kind: "note", text: `Before ${i}` }, null);
  const run = window.__sim!.host.tasks.find(t => t.task_id === workIds.receipt)!.current_run!;
  append({ kind: "run_started", run_id: run });
  append({ kind: "shown", shown: { what: "invitation", action: "send_files", note: "Send me the logo files." } });
  for (let i = 0; i < 100; i++) append({ kind: "note", text: `After ${i}` });
  const task = await reported({ status: "needs_you", questions: ["Which logo?"] }, true);
  await tick(600);
  const outcome = document.querySelector(".talk-outcome") as HTMLElement;
  expect(within(outcome).getByText("Send me the logo files.")).toBeTruthy();
  expect(within(outcome).getByRole("button", { name: "Send files" })).toBeTruthy();
  // It stopped at the run's start rather than reading the whole conversation.
  expect(app.session.state.get().timelines[task.task_id]?.hasMore).toBe(true);
});

it("carries on with what's left as soon as Carry on is tapped, through Next up while another task holds the project", async () => {
  const task = await reported({ status: "unfinished", remaining: ["Write the migration"] });
  app.ui.saveDraft(task.project_id, task.task_id, { text: "My own words" });
  const command = vi.spyOn(app.session, "command");
  const outcome = screen.getByRole("region", { name: "Not finished" });
  expect(within(outcome).getByText("Write the migration")).toBeTruthy();
  expect(within(outcome).getByText(/^Carry on waits in Next up\. Waiting for your approval on “Fix the flaky retry test/)).toBeTruthy();
  fireEvent.click(within(outcome).getByRole("button", { name: "Carry on" }));
  await tick();
  expect(command).toHaveBeenCalledTimes(1);
  expect(command.mock.calls[0]![0]).toEqual({ command: "queue_continuation", task_id: task.task_id, text: "Carry on with what’s left.", authorization: "auto_start" });
  expect(app.session.state.get().queues[task.project_id]?.items.some((i) => i.task_id === task.task_id && i.state.state !== "started")).toBe(true);
  // What the person was writing is theirs, and stays.
  expect(app.ui.draft(task.project_id, task.task_id)?.text).toBe("My own words");
  expect(app.ui.get().tab).toBe("talk");
});

it("starts new work it found as a task of its own, its brief replacing the new-task draft, and stays in Review when there's a choice to make", async () => {
  const task = await reported({ new_work: [{ title: "Fix the flaky upload test", brief: "Make the upload test wait for the server.", mode: "work" }] });
  app.ui.saveDraft(task.project_id, null, { text: "Half a thought" });
  const card = screen.getByRole("heading", { name: "Fix the flaky upload test" }).closest(".receipt-new-work") as HTMLElement;
  expect(within(card).getByText("Work")).toBeTruthy();
  fireEvent.click(within(card).getByRole("button", { name: "Start" }));
  const route = app.ui.route();
  expect(route).toMatchObject({ screen: "review", taskId: null, intent: "new_task" });
  const draft = app.ui.draft(task.project_id, null)!;
  // The brief replaces what was there; the person's words are set aside, never added to.
  expect(draft.text).toBe("Make the upload test wait for the server.");
  expect(draft.review?.title).toBe("Fix the flaky upload test");
  expect(draft.mode).toBe("work");
  expect(app.ui.get().savedDrafts[draftKey(task.project_id, null)]?.map(d => d.text)).toEqual(["Half a thought"]);
  // Review sends it at once unless something needs choosing: here the
  // run's model is gone, so it waits for one with the brief in place.
  cleanup();
  if (route.screen !== "review") throw Error("Review missing");
  const command = vi.spyOn(app.session, "command");
  view(<Review route={route} />, route);
  await tick(1500);
  expect(command).not.toHaveBeenCalled();
  expect(document.querySelector(".review-foot")?.textContent).toMatch(/^The selected model \(claude-sonnet-5\) is unavailable\. Choose a model/);
  expect(screen.getByRole("textbox", { name: "What you’re asking" })).toHaveProperty("value", "Make the upload test wait for the server.");
});

it("ends the conversation in Chat with the same way on, sent as it is tapped", async () => {
  const task = await reported({ status: "unfinished", remaining: ["Write the migration"], new_work: [{ title: "Fix the flaky upload test", brief: "", mode: "work" }] }, true);
  const outcome = document.querySelector(".talk-outcome") as HTMLElement;
  expect(within(outcome).getByRole("heading", { name: "Not finished" })).toBeTruthy();
  expect(within(outcome).getByText("Write the migration")).toBeTruthy();
  expect(within(outcome).getByRole("button", { name: "Start" })).toBeTruthy();
  expect(within(outcome).getByRole("button", { name: "Archive" })).toBeTruthy();
  // With the project free, it goes to the task at once.
  const host = window.__sim!.host;
  const project = host.projects.find((p) => p.project_id === task.project_id)!;
  host.projects = host.projects.map((p) => p === project ? { ...p, active_writer: null } : p);
  host.emit({ type: "project_upserted", project: { ...project, active_writer: null } });
  await tick();
  expect(within(outcome).queryByText(/waits in Next up/)).toBeNull();
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(within(outcome).getByRole("button", { name: "Carry on" }));
  await tick();
  expect(command.mock.calls[0]![0]).toEqual({ command: "resume_task", task_id: task.task_id, text: "Carry on with what’s left." });
  expect(app.ui.draft(task.project_id, task.task_id)).toBeNull();
});

it("leads a finished task with its changes, and Archive follows", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.receipt)!;
  const run = host.runs.find(x => x.run_id === task.current_run)!;
  const files = run.outcome!.changed_files.length;
  expect(files).toBeGreaterThan(0);
  await reported({ status: "done" }, true);
  const outcome = screen.getByRole("region", { name: "Finished, needs a look" });
  const see = within(outcome).getByRole("button", { name: `See changes · ${files} ${files === 1 ? "file" : "files"}` });
  expect(see.className).toContain("btn--primary");
  expect(within(outcome).getByRole("button", { name: "Archive" }).className).not.toContain("btn--primary");
  fireEvent.click(see);
  expect(app.ui.get().stacks[app.ui.get().tab].at(-1)).toEqual({ screen: "receipt", taskId: task.task_id, runId: run.run_id, section: "changes" });
});

it("keeps one lime button on a finished task: its changes, not the new work's Start", async () => {
  await reported({ status: "done", new_work: [{ title: "Fix the flaky upload test", brief: "Make the upload test wait for the server.", mode: "work" }] }, true);
  const outcome = screen.getByRole("region", { name: "Finished, needs a look" });
  expect([...outcome.querySelectorAll(".btn--primary")].map((b) => b.textContent)).toEqual([expect.stringMatching(/^See changes/)]);
  expect(within(outcome).getByRole("button", { name: "Start" }).className).not.toContain("btn--primary");
});

it("archives a finished task, and archives then starts a new one", async () => {
  const task = await reported({ status: "done" });
  const outcome = screen.getByRole("region", { name: "Finished, needs a look" });
  fireEvent.click(within(outcome).getByRole("button", { name: "Archive and start new" }));
  await tick();
  // At once, with Undo, in place of asking first.
  expect(screen.queryByRole("dialog", { name: /Archive task/ })).toBeNull();
  expect(screen.getByText("Archived task, now in Tasks › Archived")).toBeTruthy();
  await tick(1200);
  expect(app.session.state.get().tasks[task.task_id]?.state).toBe("archived");
  // A new task in Talk.
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.get().taskByProject[task.project_id] ?? null).toBeNull();
});

it("archives a conversation from Chat's list", async () => {
  const host = window.__sim!.host;
  const project = app.session.state.get().projects[host.tasks.find(t => t.task_id === workIds.receipt)!.project_id]!;
  for (const t of host.tasks) if (t.project_id === project.project_id) t.conversation = true;
  act(() => app.session.state.set((s) => ({ ...s, tasks: Object.fromEntries(Object.entries(s.tasks).map(([id, t]) => [id, t.project_id === project.project_id ? { ...t, conversation: true } : t])) })));
  view(<TalkConversations project={project} />);
  await tick();
  const list = screen.getByRole("heading", { name: "Conversations" }).closest("section")!;
  const rows = list.querySelectorAll(".talk-task-row");
  expect(rows.length).toBeGreaterThan(1);
  const ended = [...rows].find((r) => within(r as HTMLElement).queryByRole("button", { name: /^Archive / }))!;
  fireEvent.click(within(ended as HTMLElement).getByRole("button", { name: /^Archive / }));
  await tick();
  expect(screen.getByText("Archived conversation, now in Chat › Archived")).toBeTruthy();
  await tick(1200);
  expect(list.querySelectorAll(".talk-task-row").length).toBe(rows.length - 1);
});
