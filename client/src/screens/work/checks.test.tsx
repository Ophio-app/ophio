// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { workIds } from "../../dev/sim/work";
import { Toasts } from "../../design/Toast";
import { HostSession } from "../../host/session";
import type { Check } from "../../protocol/generated/Check";
import type { CheckJob } from "../../protocol/generated/CheckJob";
import type { ChecksDraftView } from "../../protocol/generated/ChecksDraftView";
import type { OphioCheckResult } from "../../protocol/generated/OphioCheckResult";
import type { ProjectChecksView } from "../../protocol/generated/ProjectChecksView";
import type { Query, Run, Task } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { taskBadge, taskRowBadge } from "../talk/TaskBar";
import Talk from "../talk/Talk";
import { writerActivity, writerBlockLine, writerHold, writerHoldState, writerQueueLine } from "../talk/intent";
import { taskStatus } from "./taskActivity";
import { StopTask } from "../control/StopTask";
import { checksSummary, OphioCheckResults } from "./OphioCheckResults";
import { ProjectChecksRow } from "./ProjectChecksRow";
import Receipt from "./Receipt";
import NextUp from "./NextUp";
import { SetupChecksOffer, latestSetupTask } from "./SetupChecksOffer";
import { TaskRow } from "./shared";

let app: App;
let projectView: ProjectChecksView;
let jobs: CheckJob[];
let drafts: ChecksDraftView[];
const tick = (ms = 500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const task = () => app.session.state.get().tasks[workIds.receipt]!;
const definition = (id = "build"): Check => ({ check_id: id, name: "The app builds", about: "Makes sure the app can be built.", run: "npm run build", setup: null, paths: [], when: "every_task", timeout_secs: 600, network: false, inputs: [], enabled: true, typical_secs: null });
const result = (kind: OphioCheckResult["outcome"]["kind"] = "passed", id = "build"): OphioCheckResult => ({ check_id: id, name: id === "build" ? "The app builds" : "The buttons work", outcome: kind === "could_not_run" ? { kind, reason: "The required program is not installed." } : { kind }, duration_secs: 2, exit_code: kind === "passed" ? 0 : null, output: `Recorded output for ${id}`, inputs_changed: [] });
const job = (over: Partial<CheckJob> = {}): CheckJob => ({ id: "job", project_id: task().project_id, task_id: task().task_id, run_id: task().current_run, purpose: { kind: "after_run" }, checks_version: 1, checks: [definition()], state: "finished", process: null, created_at: Date.now() - 3000, started_at: Date.now() - 2000, finished_at: Date.now(), sandbox: { kind: "contained" }, results: [result()], message: null, ...over });
const suggestion = (state: ChecksDraftView["draft"]["state"] = "ready"): ChecksDraftView => ({ draft: { id: "draft", project_id: task().project_id, base_version: 0, task_id: task().task_id, request: null, items: [{ kind: "add", check: definition(), note: "Make sure the app builds." }], proofs: [result()], state, created_at: Date.now(), updated_at: Date.now() }, differences: [["Adds a build check"]], facts: [{ when: "After every task that changes files", internet: "Does not use the internet", time_limit: "Stops after 10 minutes", typical_time: null }] });
function view(child: ReactNode, route: Route = { screen: "work" }, active = true) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: route.screen === "talk" ? "talk" : "work", depth: route.screen === "talk" ? 0 : 1, active, route }}>{child}<Toasts /></RouteProvider></AppProvider>);
}
function capability(enabled: boolean) {
  app.session.state.set(s => ({ ...s, capabilities: { ...s.capabilities!, features: enabled ? [...s.capabilities!.features, "checks_v1"] : s.capabilities!.features.filter(f => f !== "checks_v1") } }));
}
function finishedTasks() {
  // Isolate a single ordinary Work task from other finished fixture tasks.
  app.session.state.set(s => ({ ...s, tasks: { [task().task_id]: { ...task(), mode: "work", conversation: false, review: undefined, asked_by: undefined, state: "completed", checking: undefined } } }));
}
function sentChecks() {
  return vi.mocked(app.session.query).mock.calls.map(([q]) => q).filter(q => ["checks_list", "task_checks", "check_job"].includes(q.kind));
}

beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  capability(true);
  projectView = { project_id: task().project_id, project_name: "Project", approved: { project_id: task().project_id, version: 0, checks: [], input_fingerprints: [], changed_at: Date.now(), changed_by: { kind: "person", device_id: null } }, checks: [], drafts: [] };
  jobs = []; drafts = [];
  const query = session.query.bind(session);
  vi.spyOn(session, "query").mockImplementation(((q: Query) => {
    if (q.kind === "checks_list") return Promise.resolve({ kind: "checks_list", projects: [projectView], projects_without_checks: 1 });
    if (q.kind === "task_checks") return Promise.resolve({ kind: "task_checks", jobs: structuredClone(jobs), drafts: structuredClone(drafts) });
    return query(q);
  }) as typeof session.query);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});

describe("checking task badges", () => {
  it("keeps Checking and counts ahead of the agent's completed run and old attention", () => {
    const checking = { ...task(), state: "working" as const, checking: { job_id: "job", started_at: Date.now(), done: 2, total: 4 } };
    const run = app.session.state.get().runs[checking.current_run!]!;
    expect(taskBadge(checking, run)).toEqual({ label: "Checking… · 2 of 4", tone: "accent" });
    expect(taskRowBadge(checking, run, undefined, "Old agent question").label).toBe("Checking… · 2 of 4");
    expect(taskBadge({ ...checking, checking: { ...checking.checking, total: 0 } }).label).toBe("Checking…");
    view(<TaskRow task={checking} />);
    expect(screen.getByText(/Checking… · 2 of 4/)).toBeTruthy();
    expect(screen.getByText("Ophio is checking the agent’s work.")).toBeTruthy();
  });

  it("puts checking before helper cleanup on an ended run and keeps ordinary Stop", async () => {
    const checking = { ...task(), state: "working" as const, checking: { job_id: "job", started_at: Date.now(), done: 2, total: 4 } };
    const run = { ...app.session.state.get().runs[checking.current_run!]!, state: "completed" as const, ended_at: Date.now() - 1000, cleanup_pending: true, holds_writer: true };
    app.session.state.set(s => ({ ...s, tasks: { ...s.tasks, [checking.task_id]: checking }, runs: { ...s.runs, [run.run_id]: run } }));
    expect(taskStatus(checking, run)).toBe("Checking… · 2 of 4");
    expect(taskStatus(checking, { ...run, holds_writer: false })).toBe("Checking… · 2 of 4");
    expect(taskBadge(checking, run).label).toBe("Checking… · 2 of 4");
    const hold = writerHold(run.run_id, app.session.state.get())!;
    expect(hold.helpers).toBe(false);
    expect(writerActivity(hold, "Project")).toContain("Checking… · 2 of 4");
    expect(writerHoldState(hold)).toContain("Checking… · 2 of 4");
    expect(writerBlockLine(hold)).toContain("finish checking");
    expect(writerQueueLine(hold)).toContain("finishes checking");
    // A job may have been identified before its task arrived. The task's
    // progress still takes precedence over that older taskless-holder cache.
    const identified = { ...hold, checkJob: job({ state: "running" }) };
    expect(writerActivity(identified, "Project")).toContain("Checking… · 2 of 4");
    expect(writerHoldState(identified)).toContain("Checking… · 2 of 4");
    expect(writerBlockLine(identified)).toContain("finish checking");
    expect(writerQueueLine(identified)).toContain("finishes checking");
    const command = vi.spyOn(app.session, "command").mockResolvedValue({ command_id: "stop-cleanup-checks", status: "accepted", seq: null, result: null, error: null });
    view(<><TaskRow task={checking} /><StopTask task={checking} /></>);
    expect(screen.queryByText(/helpers.*finishing|Waiting for what it started/i)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /^Stop/ }));
    await tick();
    expect(command.mock.calls[0]?.[0]).toMatchObject({ command: "stop_task", task_id: checking.task_id });
  });

  it("keeps Chat checking instead of showing Done and Archive after the agent ends", async () => {
    vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
    const checking = { ...task(), state: "working" as const, checking: { job_id: "job", started_at: Date.now(), done: 2, total: 4 } };
    const run = { ...app.session.state.get().runs[checking.current_run!]!, state: "completed" as const, ended_at: Date.now() - 1000, cleanup_pending: true, holds_writer: true };
    app.session.state.set(s => ({ ...s, tasks: { ...s.tasks, [checking.task_id]: checking }, runs: { ...s.runs, [run.run_id]: run } }));
    app.ui.selectTask(checking.project_id, checking.task_id);
    view(<Talk route={{ screen: "talk" }} />, { screen: "talk" });
    await tick();
    expect(screen.getAllByText(/Checking… · 2 of 4/).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: /^Stop/ })).toBeTruthy();
    expect(document.querySelector(".talk-outcome")).toBeNull();
    expect(screen.queryByRole("button", { name: /^Archive/ })).toBeNull();
  });

  it("falls back for both a future task state and a future run state", () => {
    const unknown = { ...task(), state: "future_state", wake: undefined } as unknown as Task;
    expect(taskBadge(unknown)).toEqual({ label: "Status unknown", tone: "neutral" });
    const run = { ...app.session.state.get().runs[task().current_run!]!, run_id: "newer", state: "future_state", human_resolution: undefined, outcome: null } as unknown as Run;
    expect(taskBadge({ ...task(), state: "working" }, run)).toEqual({ label: "Status unknown", tone: "neutral" });
  });

  it("uses the ordinary Stop command while checks are running", async () => {
    const checking = { ...task(), state: "working" as const, checking: { job_id: "job", started_at: Date.now(), done: 0, total: 4 } };
    app.session.state.set(s => ({ ...s, tasks: { ...s.tasks, [checking.task_id]: checking } }));
    const command = vi.spyOn(app.session, "command").mockResolvedValue({ command_id: "stop-checking", status: "accepted", seq: null, result: null, error: null });
    view(<StopTask task={checking} />);
    fireEvent.click(screen.getByRole("button", { name: /^Stop/ }));
    await tick();
    expect(command.mock.calls[0]?.[0]).toMatchObject({ command: "stop_task", task_id: checking.task_id });
    expect(command.mock.calls.some(([c]) => c.command === "checks_stop_job")).toBe(false);
  });
});

it("names a taskless check job in Next up and the project's holder line", async () => {
  const projectId = task().project_id;
  const alone = job({ id: "taskless-check-job", task_id: null, run_id: null, state: "running", purpose: { kind: "run_now", check_ids: ["build"] }, results: [] });
  const queue = { project_id: projectId, revision: 99, paused: false, writer: alone.id, items: [] };
  app.ui.selectProject(projectId);
  app.session.state.set(s => ({ ...s, projects: { ...s.projects, [projectId]: { ...s.projects[projectId]!, active_writer: alone.id } }, queues: { ...s.queues, [projectId]: queue } }));
  const query = vi.mocked(app.session.query).getMockImplementation()!;
  vi.mocked(app.session.query).mockImplementation(((q: Query) => q.kind === "check_job" && q.job_id === alone.id
    ? Promise.resolve({ kind: "check_job", job: alone })
    : q.kind === "queue" ? Promise.resolve({ kind: "queue", queue }) : query(q)) as typeof app.session.query);
  const route = { screen: "next_up" } as const;
  view(<NextUp route={route} />, route);
  await tick(1000);
  expect(screen.getByText("Ophio checks")).toBeTruthy();
  expect(screen.getByText(/Ophio is running this project’s checks/)).toBeTruthy();
  expect(screen.queryByText(/hasn’t reported what holds/)).toBeNull();
  const held = writerHold(alone.id, app.session.state.get())!;
  expect(held.task).toBeNull();
  expect(writerActivity(held, "Project")).toBe("Ophio is running this project’s checks");
  expect(writerHoldState(held)).toBe("After Ophio finishes this project’s checks");
});

it("keeps an unmatched project holder honest and safe instead of inventing a check job", async () => {
  const projectId = task().project_id;
  const queue = { project_id: projectId, revision: 99, paused: false, writer: "unknown-holder", items: [] };
  app.ui.selectProject(projectId);
  app.session.state.set(s => ({ ...s, projects: { ...s.projects, [projectId]: { ...s.projects[projectId]!, active_writer: queue.writer } }, queues: { ...s.queues, [projectId]: queue } }));
  const query = vi.mocked(app.session.query).getMockImplementation()!;
  vi.mocked(app.session.query).mockImplementation(((q: Query) => q.kind === "queue" ? Promise.resolve({ kind: "queue", queue }) : query(q)) as typeof app.session.query);
  const route = { screen: "next_up" } as const;
  view(<NextUp route={route} />, route);
  await tick(1000);
  expect(screen.getByText("The computer hasn’t reported what holds this project")).toBeTruthy();
  expect(screen.queryByText(/Ophio is running this project’s checks/)).toBeNull();
  expect(writerHold(queue.writer, app.session.state.get())?.task).toBeNull();
});

it("shows Ophio's count and individual output before agent commands and the agent's summary", async () => {
  jobs = [job({ checks: [definition(), definition("buttons")], results: [result(), result("failed", "buttons")] })];
  drafts = [suggestion()];
  const route = { screen: "receipt", taskId: task().task_id } as const;
  view(<Receipt route={route} />, route);
  await tick();
  const checks = screen.getByRole("heading", { name: "Checks" }).closest("section")!;
  expect(within(checks).getByText("Ophio ran 2 checks: 1 passed, 1 failed.")).toBeTruthy();
  const own = within(checks).getByRole("heading", { name: "Ophio’s results" });
  const agent = within(checks).getByRole("heading", { name: "Agent-run commands" });
  expect(own.compareDocumentPosition(agent) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(checks.compareDocumentPosition(screen.getByRole("heading", { name: "Result" })) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  const output = within(checks).getByText("The app builds", { selector: ".receipt-checks .row__title" }).closest("details")!;
  expect(output.open).toBe(false);
  fireEvent.click(output.querySelector("summary")!);
  expect(output.open).toBe(true);
  expect(output.querySelector("pre")?.textContent).toBe("Recorded output for build");
  expect(screen.getByText("Suggested checks")).toBeTruthy();
});

it("keeps check evidence with its own run, not an earlier receipt", async () => {
  jobs = [job({ run_id: "another-run" }), job({ id: "proof-job", purpose: { kind: "draft_proof", draft_id: "draft" } })];
  const route = { screen: "receipt", taskId: task().task_id } as const;
  view(<Receipt route={route} />, route);
  await tick();
  expect(screen.queryByRole("heading", { name: "Ophio’s results" })).toBeNull();
});

it("does not announce a finished outcome while Ophio checks an ended agent run", async () => {
  const checking = { ...task(), state: "working" as const, checking: { job_id: "job", started_at: Date.now(), done: 1, total: 2 } };
  const ended = { ...app.session.state.get().runs[checking.current_run!]!, state: "completed" as const, ended_at: Date.now() - 1000, holds_writer: true, cleanup_pending: true };
  app.session.state.set(s => ({ ...s, tasks: { ...s.tasks, [checking.task_id]: checking }, runs: { ...s.runs, [ended.run_id]: ended } }));
  jobs = [job({ state: "running", checks: [definition(), definition("buttons")], results: [result()] })];
  const route = { screen: "receipt", taskId: checking.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick();
  expect(document.querySelector(".work-receipt-state")?.textContent).toBe("Checking… · 1 of 2");
  expect(document.querySelector(".receipt-outcome")).toBeNull();
  expect(screen.getByText("Ophio is checking the agent’s work. Stop still stops this task.")).toBeTruthy();
  jobs = [job({ checks: [definition(), definition("buttons")], results: [result(), result("failed", "buttons")] })];
  await tick(2200);
  expect(screen.getByText("Ophio ran 2 checks: 1 passed, 1 failed.")).toBeTruthy();
});

it("does not send checks requests on an old computer or an inactive receipt", async () => {
  capability(false);
  const route = { screen: "receipt", taskId: task().task_id } as const;
  const first = view(<Receipt route={route} />, route);
  await tick();
  expect(sentChecks()).toEqual([]);
  first.unmount();
  capability(true);
  view(<Receipt route={route} />, route, false);
  await tick();
  expect(sentChecks()).toEqual([]);
});

it("reports sandbox access, uncertain endings, changed inputs and stopped checks honestly", () => {
  view(<OphioCheckResults jobs={[job({ state: "uncertain", sandbox: { kind: "unsandboxed", reason: "Not available on this computer" }, message: "Stop was not confirmed", results: [{ ...result("stopped"), inputs_changed: ["scripts/build.sh"] }] })]} />);
  expect(screen.getByText(/These checks ran without a sandbox/)).toBeTruthy();
  expect(screen.getByText(/could not confirm that these checks finished or stopped/)).toBeTruthy();
  expect(screen.getByText(/This task changed files a check depends on/)).toBeTruthy();
  const stopped = screen.getByText("The app builds", { selector: ".receipt-checks .row__title" }).closest("summary")!;
  expect(within(stopped).getByText("Not checked: stopped")).toBeTruthy();
  expect(screen.queryByText(/Ophio ran 1 check/)).toBeNull();
  expect(checksSummary([result(), result("off", "disabled"), result("skipped_paths", "unchanged")])).toBe("Ophio ran 1 check: 1 passed, 1 not needed for these changes, 1 off.");
});

it("opens the project's Checks part from the count and does not pretend an old computer has no checks", async () => {
  const close = vi.fn();
  projectView.approved.checks = [definition(), definition("buttons")];
  const first = view(<ProjectChecksRow projectId={task().project_id} onClose={close} />);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "Checks: 2" }));
  expect(close).toHaveBeenCalledOnce();
  expect(app.ui.get().projectId).toBe(task().project_id);
  expect(app.ui.route()).toEqual({ screen: "library", part: "checks" });
  first.unmount();
  capability(false);
  vi.mocked(app.session.query).mockClear();
  view(<ProjectChecksRow projectId={task().project_id} onClose={close} />);
  await tick();
  expect(screen.queryByText("Checks: not set up")).toBeNull();
  expect(screen.getByText("Update this computer to use checks in the Library.")).toBeTruthy();
  expect(sentChecks()).toEqual([]);
});

it("offers setup only on the latest finished Work card and remembers dismissal on this phone", async () => {
  finishedTasks();
  const old = { ...task(), task_id: "old", updated_at: task().updated_at - 1000, current_run: null };
  app.session.state.set(s => ({ ...s, tasks: { ...s.tasks, old } }));
  expect(latestSetupTask(app.session.state.get(), task().project_id)).toBe(task().task_id);
  const first = view(<><SetupChecksOffer task={old} /><SetupChecksOffer task={task()} /></>);
  await tick();
  expect(screen.getAllByRole("button", { name: "Set up checks" })).toHaveLength(1);
  fireEvent.click(within(screen.getByRole("button", { name: "Set up checks" }).closest(".checks-setup-offer") as HTMLElement).getByRole("button", { name: "Dismiss" }));
  expect(screen.queryByRole("button", { name: "Set up checks" })).toBeNull();
  first.unmount();
  const next = { ...task(), task_id: "next", current_run: null, updated_at: Date.now() + 1000 };
  app.session.state.set(s => ({ ...s, tasks: { ...s.tasks, next } }));
  view(<SetupChecksOffer task={next} />);
  await tick();
  expect(screen.queryByRole("button", { name: "Set up checks" })).toBeNull();
  expect(Object.keys(localStorage).some(k => k.includes(`checks-offer.${next.project_id}`))).toBe(true);
});

it.each(["approved", "ready", "proving", "stale", "ask", "plan", "conversation", "old-computer"])("does not offer setup for %s", async reason => {
  finishedTasks();
  let candidate = task();
  if (reason === "approved") projectView.approved.checks = [definition()];
  else if (["ready", "proving", "stale"].includes(reason)) projectView.drafts = [suggestion(reason as "ready" | "proving" | "stale")];
  else if (reason === "old-computer") capability(false);
  else {
    candidate = { ...candidate, ...(reason === "conversation" ? { conversation: true } : { mode: reason as "ask" | "plan" }) };
    app.session.state.set(s => ({ ...s, tasks: { [candidate.task_id]: candidate } }));
  }
  view(<SetupChecksOffer task={candidate} />);
  await tick();
  expect(screen.queryByRole("button", { name: "Set up checks" })).toBeNull();
  if (reason === "old-computer") expect(sentChecks()).toEqual([]);
});

it("starts setup directly and keeps the offer available if the computer refuses", async () => {
  finishedTasks();
  const command = vi.spyOn(app.session, "command").mockResolvedValue({ command_id: "refused", status: "rejected", seq: null, result: null, error: { code: "permission_denied", message: "This project cannot start a task right now." } });
  view(<SetupChecksOffer task={task()} />);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "Set up checks" }));
  await tick();
  expect(command).toHaveBeenCalledWith({ command: "checks_ask", project_id: task().project_id, check_id: null, request: "" }, expect.anything());
  expect(screen.getByRole("button", { name: "Set up checks" })).toHaveProperty("disabled", false);
  expect(screen.getByText(/This project cannot start a task right now/)).toBeTruthy();
  expect(Object.keys(localStorage).some(k => k.includes("checks-offer"))).toBe(false);
  command.mockResolvedValue({ command_id: "created", status: "accepted", seq: null, result: { kind: "task_created", task_id: "setup-task", queue_item_id: null }, error: null });
  fireEvent.click(screen.getByRole("button", { name: "Set up checks" }));
  await tick();
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.get().taskByProject[task().project_id]).toBe("setup-task");
  expect(app.ui.route().screen).not.toBe("review");
  expect(screen.queryByRole("button", { name: "Set up checks" })).toBeNull();
});

it.each(["permission", "offline"])("does not request check evidence or offer setup without %s", async reason => {
  finishedTasks();
  if (reason === "permission") app.session.state.set(s => ({ ...s, grants: { ...s.grants!, tasks: false } }));
  else app.session.link.set({ state: "offline", since: Date.now(), reason: "Disconnected", retryAt: null });
  const route = { screen: "receipt", taskId: task().task_id } as const;
  view(<><Receipt route={route} /><SetupChecksOffer task={task()} /></>, route);
  await tick();
  expect(sentChecks()).toEqual([]);
  expect(screen.queryByRole("button", { name: "Set up checks" })).toBeNull();
});

it("warns when this phone cannot remember dismissing the setup offer", async () => {
  finishedTasks();
  view(<SetupChecksOffer task={task()} />);
  await tick();
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("Storage full"); });
  const offer = screen.getByRole("button", { name: "Set up checks" }).closest(".checks-setup-offer") as HTMLElement;
  fireEvent.click(within(offer).getByRole("button", { name: "Dismiss" }));
  expect(screen.queryByRole("button", { name: "Set up checks" })).toBeNull();
  expect(screen.getByText("This phone could not save your choice. The offer may appear again.")).toBeTruthy();
});

it("stops polling check evidence as soon as the receipt goes offline", async () => {
  jobs = [job({ state: "running", results: [] })];
  const route = { screen: "receipt", taskId: task().task_id } as const;
  view(<Receipt route={route} />, route);
  await tick();
  const reads = sentChecks().filter(q => q.kind === "task_checks").length;
  expect(reads).toBeGreaterThan(0);
  act(() => app.session.link.set({ state: "offline", since: Date.now(), reason: "Disconnected", retryAt: null }));
  await tick(2500);
  expect(sentChecks().filter(q => q.kind === "task_checks")).toHaveLength(reads);
});
