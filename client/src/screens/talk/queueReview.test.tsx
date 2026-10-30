// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route, type Tab } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { workIds } from "../../dev/sim/work";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import * as notifications from "../../design/Toast";
import { useHost } from "../../app/context";
import { annotationStore } from "../../state/annotations";
import NextUp from "../work/NextUp";
import { AttentionList } from "../work/shared";
import { pendingQueueItems } from "../work/useQueue";
import { Composer } from "./Composer";
import { HandBack } from "./HandBack";
import Review from "./Review";
import Talk from "./Talk";
import { taskRowBadge } from "./TaskBar";
import { Transcript } from "./Transcript";

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
vi.mock("../control/useMedia", () => ({ useMedia: (target: { geometry: { revision: number } }) => ({
  picture: { at: Date.now(), revision: target?.geometry.revision, connected: true, error: null }, canvas: { current: null }, retry() {},
}) }));
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
function view(child: React.ReactNode, route: Route, tab: Tab = "talk") {
  return render(<AppProvider app={app}><RouteProvider value={{ tab, depth: app.ui.get().stacks[tab].indexOf(route), active: true, route }}>{child}</RouteProvider></AppProvider>);
}

/** Next up is paused with one task at its front, reviewed from Work. */
function queuedReview() {
  const host = window.__sim!.host;
  const item = host.queue.items.find(i => i.state.state !== "started")!;
  host.queue = { ...host.queue, paused: true, pause_reason: "Paused for review.", writer: null };
  host.projects = host.projects.map(p => ({ ...p, active_writer: null }));
  for (const project of host.projects) host.emit({ type: "project_upserted", project });
  host.emit({ type: "queue_changed", queue: host.queue });
  const task = host.tasks.find(t => t.task_id === item.task_id)!;
  const route: Route = { screen: "review", projectId: task.project_id, taskId: task.task_id, queueItemId: item.item_id };
  app.ui.push({ screen: "next_up" }, "work"); app.ui.push(route, "work");
  return { host, item, task, route };
}

it("closes a Control Review when its unknown send is later confirmed, without moving the person", async () => {
  const show = vi.spyOn(notifications, "toast");
  // Talk shows another project's task, not the one this Review starts.
  app.ui.selectTask(ids.pTide, ids.tDash);
  const route: Route = { screen: "review", projectId: ids.pGarden, taskId: null, intent: "new_task" };
  app.ui.saveDraft(ids.pGarden, null, { text: "Tidy the planting notes", mode: "ask" });
  app.ui.push({ screen: "annotate", annotationId: "markup" }, "control"); app.ui.push(route, "control");
  view(<Review route={route} />, route, "control");
  window.__sim!.dropNextReceipt();
  fireEvent.click(screen.getByRole("button", { name: "Start task" }));
  await tick(500);
  expect(app.ui.draft(ids.pGarden, null)?.attempt?.phase).toBe("unknown");
  act(() => app.ui.selectTab("talk"));
  const talk = app.ui.get().stacks.talk;
  await tick(6000);
  expect(app.ui.draft(ids.pGarden, null)).toBeNull();
  expect(app.ui.get().stacks.control).toEqual([{ screen: "control" }]);
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.get().stacks.talk).toBe(talk);
  expect(show).toHaveBeenCalledTimes(1);
  expect(show).toHaveBeenCalledWith("Task started in garden-planner", expect.objectContaining({ action: expect.objectContaining({ label: "Open task" }) }));
});

it("finishes a slow queued start in Tasks while the person stays in Library", async () => {
  const show = vi.spyOn(notifications, "toast");
  const { task, route } = queuedReview();
  view(<Review route={route} />, route, "work");
  window.__sim!.latency(1500);
  fireEvent.click(screen.getByRole("button", { name: "Start only this task" }));
  act(() => app.ui.selectTab("library"));
  await tick(2500);
  expect(app.ui.get().tab).toBe("library");
  expect(app.ui.get().stacks.work).toEqual([{ screen: "work" }, { screen: "next_up" }]);
  expect(app.ui.get().taskByProject[task.project_id]).toBeUndefined();
  expect(show).toHaveBeenCalledWith(`Started “${task.title}”`, expect.objectContaining({ action: expect.objectContaining({ label: "Open task" }) }));
  act(() => show.mock.calls[0]![1]!.action!.run());
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.get().taskByProject[task.project_id]).toBe(task.task_id);
});

it("replaces 'will check when it reconnects' once the computer answers", async () => {
  const { host, item, route } = queuedReview();
  view(<Review route={route} />, route, "work");
  window.__sim!.dropNextReceipt();
  fireEvent.click(screen.getByRole("button", { name: "Start only this task" }));
  await tick(500);
  expect(screen.getByText(/this device will check when it reconnects/)).toBeTruthy();
  await tick(6000);
  expect(host.queue.items.find(i => i.item_id === item.item_id)?.state.state).toBe("started");
  expect(screen.queryByText(/will check when it reconnects/)).toBeNull();
  expect(screen.queryByText(/already left Next up/)).toBeNull();
  expect(app.ui.get().stacks.work).toEqual([{ screen: "work" }, { screen: "next_up" }]);
});

it("offers the task and a way back when a queued request left Next up elsewhere", async () => {
  const { host, task, item, route } = queuedReview();
  view(<Review route={route} />, route, "work");
  void app.session.command({ command: "queue_start", item_id: item.item_id, task_revision: task.revision }, { label: "Start", expectedRevision: host.queue.revision });
  await tick(700);
  expect(screen.getByText("This task has left Next up")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Close review" }));
  expect(app.ui.get().stacks.work).toEqual([{ screen: "work" }, { screen: "next_up" }]);
  expect(app.ui.get().tab).toBe("work");
});

it("says the queued task's permission setting where it is reviewed and beside Start, warning when it skips asking", async () => {
  const { host, task, route } = queuedReview();
  const risky = { ...task, harness: "claude_code" as const, launch_policy: "bypass_permissions" as const };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? risky : t);
  host.emit({ type: "task_upserted", task: risky });
  view(<Review route={route} />, route, "work"); await tick();
  const foot = document.querySelector(".review-foot .review-foot__summary")!;
  expect(foot.textContent).toMatch(/^In tidewater-api · Claude Code · Work, “Skip permissions” · Starts now$/);
  expect(foot.classList.contains("launch-line__risk")).toBe(true);
  expect(document.querySelector(".review")?.textContent).toContain("Permission prompts: Skip permissions");
});

it("pins the queued Review's action when the screen is short for its text", async () => {
  const { route } = queuedReview();
  const size = { innerWidth: window.innerWidth, innerHeight: window.innerHeight };
  Object.defineProperties(window, { innerWidth: { value: 360, configurable: true }, innerHeight: { value: 390, configurable: true } });
  try {
    view(<Review route={route} />, route, "work");
    act(() => { window.dispatchEvent(new Event("resize")); });
    expect(document.documentElement.classList.contains("app-tight")).toBe(true);
    const start = screen.getByRole("button", { name: "Start only this task" });
    expect(start.closest(".screen__footer")).toBeTruthy();
    expect(start.closest(".screen__body")).toBeNull();
  } finally {
    Object.defineProperties(window, { innerWidth: { value: size.innerWidth, configurable: true }, innerHeight: { value: size.innerHeight, configurable: true } });
    act(() => { window.dispatchEvent(new Event("resize")); });
  }
});

it("offers the return-to-agent review beside the waiting line, and Needs you opens it directly", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const pause = app.session.command({ command: "pause_input", target_id: ids.tgDp1 }, { label: "Pause requested" });
  await tick(900); await pause;
  expect(app.session.state.get().tasks[ids.tDash]?.state).toBe("awaiting_decision");
  function Conversation() {
    const task = useHost(s => s.tasks[ids.tDash]!);
    return <><Transcript task={task} agent="Claude Code" /><HandBack task={task} /></>;
  }
  const talk = view(<Conversation />, { screen: "talk" });
  await tick();
  // Said once, by the bar with its Resume, not again by the working line.
  expect(screen.queryByText("Agent paused · resume it when you’re ready")).toBeNull();
  expect(screen.getByText("Agent paused · it can’t use Screen 1 (DP-1) until you resume it")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Resume agent" }));
  expect(screen.getByRole("dialog", { name: "Resume the agent on Screen 1 (DP-1)" })).toBeTruthy();
  talk.unmount();

  view(<AttentionList projectId={ids.pTide} />, { screen: "work" }, "work");
  fireEvent.click(screen.getByRole("button", { name: /Check the staging dashboard.*Agent paused on Screen 1 \(DP-1\) · Resume agent/ }));
  expect(screen.getByRole("dialog", { name: "Resume the agent on Screen 1 (DP-1)" })).toBeTruthy();
  await tick(1100);
  fireEvent.click(screen.getByRole("button", { name: "Resume now" }));
  await tick(900);
  expect(app.session.state.get().control[ids.tgDp1]?.input_state).toBe("agent");
  // The agent acts only while someone watches its screen, which opens next.
  expect(app.ui.route()).toEqual({ screen: "computer_use", taskId: ids.tDash });
});

it("gives one queue truth on Chat after the computer's records restart", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const { host, item, task } = queuedReview();
  void app.session.command({ command: "queue_start", item_id: item.item_id, task_revision: task.revision }, { label: "Start", expectedRevision: host.queue.revision });
  await tick(6000);
  expect(app.session.state.get().queues[task.project_id]?.items.some(i => i.item_id === item.item_id && i.state.state !== "started")).toBe(false);
  window.__sim!.restart();
  await tick(6000);
  expect(app.session.state.get().tasks[task.task_id]?.state).toBe("accepted");
  act(() => { app.ui.selectTask(task.project_id, task.task_id); app.ui.reset("talk"); });
  const { container } = view(<Talk route={{ screen: "talk" }} />, app.ui.get().stacks.talk[0]!);
  await tick(500);
  const chip = () => container.querySelector(".chat-head__state")?.textContent;
  expect(chip()).toBe("In Next up");
  expect(screen.getByText("Waiting in Next up")).toBeTruthy();
  expect(container.querySelector(".composer__queue summary")?.textContent).not.toBe("Not queued");
  expect(screen.queryByText(/not yet confirmed/)).toBeNull();

  // Waiting to start, but the queue this device holds has no item for it.
  act(() => app.session.state.set(s => ({ ...s, queues: { ...s.queues, [task.project_id]: { ...s.queues[task.project_id]!, items: s.queues[task.project_id]!.items.filter(i => i.task_id !== task.task_id) } } })));
  expect(chip()).toBe("Waiting to start · not in Next up");
  expect(screen.getByText("Sent to studio")).toBeTruthy();
  expect(container.querySelector(".composer__queue summary")?.textContent).toBe("Not queued");
  expect(screen.queryByText(/not yet confirmed|In Next up|Waiting in Next up/)).toBeNull();
});

it("names a retry that opens Review, and only Review's Try again sends", async () => {
  const state = app.session.state.get();
  const task = state.tasks[ids.tDash]!;
  const composer = () => <Composer project={state.projects[task.project_id]!} task={task} run={state.runs[task.current_run!]!} harness={state.harnesses.find(h => h.kind === task.harness)!} agent={null} />;
  const failed = (text: string, annotationId: string | null) => ({ phase: "failed" as const, at: Date.now(), commandId: null, detail: "The connection dropped before the computer answered", text, annotationId });
  const sends = () => Object.values(app.session.commands.get()).filter(c => c.body.command === "send_message").length;

  // Text alone was never reviewed, so Try again sends it from Talk.
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Check the legend", attempt: failed("Check the legend", null) });
  view(composer(), { screen: "talk" });
  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  await tick(900);
  expect(sends()).toBe(1);
  expect(app.ui.route()).toEqual({ screen: "talk" });
  cleanup();

  // Markup is shown again before it leaves, so the retry says it opens Review.
  const store = annotationStore(app.profile.profileId);
  const markup = store.create(task.project_id, task.task_id);
  store.update(markup.id, d => ({ ...d, sources: [{ key: "S1", blobKey: null, artifactId: "artifact", sha256: "abc", mediaType: "image/png", width: 640, height: 360, durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 123, derivedFrom: null, derivation: null }] }));
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Move the title", annotationId: markup.id, attempt: failed("Move the title", markup.id) });
  view(composer(), { screen: "talk" });
  await tick();
  expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Review and send again" }));
  const route = app.ui.route();
  expect(route).toMatchObject({ screen: "review", projectId: task.project_id, taskId: task.task_id });
  expect(sends()).toBe(1);
  cleanup();

  view(<Review route={route as Route & { screen: "review" }} />, route);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  await tick(900);
  expect(sends()).toBe(2);
  expect(app.ui.draft(task.project_id, task.task_id)?.attempt ?? null).toBeNull();
});

it("names both runs when a task with a ready result was continued and stopped", async () => {
  const host = window.__sim!.host;
  host.projects = host.projects.map(p => ({ ...p, active_writer: null }));
  const project = app.session.state.get().tasks[ids.tHeaders]!.project_id;
  view(<AttentionList projectId={project} review />, { screen: "work" }, "work");
  const row = () => screen.getByRole("button", { name: /^Add rate-limit headers to \/v1\/orders/ });
  expect(row().textContent).toMatch(/orders\s*Finished · 5\d min ago$/);
  void app.session.command({ command: "resume_task", task_id: ids.tHeaders, text: "Also cover Retry-After" }, { label: "Continue", expectedRevision: app.session.state.get().tasks[ids.tHeaders]!.revision });
  await tick(900);
  expect(row().textContent).toMatch(/orders\s*Run 1 finished · run 2 working · 5\d min ago$/);
  void app.session.command({ command: "stop_task", task_id: ids.tHeaders }, { label: "Stop" });
  await tick(3000);
  expect(app.session.state.get().tasks[ids.tHeaders]?.state).toBe("interrupted");
  expect(row().textContent).toMatch(/orders\s*Run 1 finished · run 2 stopped · 5\d min ago$/);
});

it("asks for Next up's go-ahead once the task can start, and its header says so instead of Running", async () => {
  const host = window.__sim!.host;
  act(() => app.ui.selectTask(ids.pTide, null));
  view(<><AttentionList projectId={ids.pTide} /><NextUp route={{ screen: "next_up" }} /></>, { screen: "next_up" }, "work");
  await tick();
  const state = () => screen.getByText(/^(Running|Paused|Blocked|Needs your go-ahead)$/).textContent;
  expect(state()).toBe("Running");
  void app.session.command({ command: "queue_move", item_id: workIds.qReady, position: 1 }, { label: "Move", expectedRevision: host.queue.revision });
  await tick(500);
  expect(state()).toBe("Needs your go-ahead");
  const goAhead = () => screen.queryAllByRole("button").find(b => /^Review keyboard navigation\s*Next up · Review and start/.test(b.textContent ?? ""));
  // "Fix the flaky retry test" is changing files, so the start is not yet the person's to give.
  expect(host.queue.writer).toBe(ids.rFlaky);
  expect(goAhead()).toBeUndefined();
  await tick(5 * 60_000);
  const approval = Object.values(app.session.state.get().approvals)[0]!;
  void app.session.command({ command: "resolve_approval", approval_id: approval.approval_id, decision: { decision: "allow" } }, { label: "Allow", expectedRevision: approval.revision });
  await tick(15_000);
  expect(host.queue.writer).toBeNull();
  expect(goAhead()?.textContent).toMatch(/Review and start · \d+ s ago$/);
  expect(state()).toBe("Needs your go-ahead");
});

it("says a message to a task carries its markup comments instead of leaving them unmentioned", async () => {
  const task = app.session.state.get().tasks[ids.tDash]!;
  const store = annotationStore(app.profile.profileId);
  const markup = store.create(task.project_id, task.task_id);
  const comment = (id: string, text: string) => ({ comment_id: id, text, mark_ids: [], origin: { kind: "typed" as const }, link_origins: [] });
  store.update(markup.id, d => ({ ...d, comments: [comment("a", "Move the chart title above the legend"), comment("b", " ")] }));
  app.ui.saveDraft(task.project_id, task.task_id, { text: "", annotationId: markup.id });
  const route = { screen: "review", projectId: task.project_id, taskId: task.task_id } as const;
  view(<Review route={route} />, route);
  await tick();
  // The agent reads the comment from the markup, so it is not copied into the message as well.
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Your message" }).value).toBe("");
  expect(screen.getByText("Your comment travels with the markup.")).toBeTruthy();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Send" }).disabled).toBe(false);
  act(() => store.update(markup.id, d => ({ ...d, comments: [...d.comments, comment("c", "Keep the legend colours")] })));
  await tick();
  expect(screen.getByText("Your 2 comments travel with the markup.")).toBeTruthy();
});

it("shows a task Next up holds for the go-ahead as needing the person, and its composer leads to Review and start", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const host = window.__sim!.host;
  const item = pendingQueueItems(host.queue)[0]!;
  host.queue = { ...host.queue, paused: false, writer: null, items: host.queue.items.map(i => i.item_id === item.item_id ? { ...i, authorization: "ask_before_start", state: { state: "awaiting_start" } } : i) };
  host.emit({ type: "queue_changed", queue: host.queue });
  await tick();
  const state = app.session.state.get();
  const task = state.tasks[item.task_id]!;
  const queue = state.queues[task.project_id];
  expect(taskRowBadge(task, undefined, queue, null).label).toBe("In Next up");
  expect(taskRowBadge(task, undefined, queue, "Next up needs your go-ahead · Review and start").label).toBe("Needs you");
  view(<Composer project={state.projects[task.project_id]!} task={task} run={null} harness={state.harnesses.find(h => h.kind === task.harness)!} agent={null} />, app.ui.get().stacks.talk[0]!);
  expect(screen.getByText("Ready — needs your go-ahead")).toBeTruthy();
  expect(screen.getByPlaceholderText("Describe a separate task")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Review and start" }));
  expect(app.ui.get().stacks.talk.at(-1)).toMatchObject({ screen: "review", taskId: task.task_id, queueItemId: item.item_id });
});
