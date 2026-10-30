// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { openAttention, pendingApprovals, runningTasks } from "../../state/projection";
import { RouteProvider } from "../../shell/Screen";
import { Composer } from "./Composer";
import { Transcript } from "./Transcript";
import { ApprovalQuickActions } from "./ApprovalQuickActions";
import Review from "./Review";
import { missionFrom } from "./intent";
import { takeSendAsk } from "./sendOnOpen";
import Receipt from "../work/Receipt";
import Annotate from "../annotate/Annotate";
import { QUEUE_MARKUP_FEATURE, QUEUE_MARKUP_UPDATE } from "../../app/queueMarkupFeature";

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
const tick = async (ms = 180) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
function view(child: React.ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{child}</RouteProvider></AppProvider>);
}
function endedTask() {
  const host = window.__sim!.host;
  const task = host.tasks.find((t) => t.task_id === ids.tHeaders)!;
  const project = host.projects.find((p) => p.project_id === task.project_id)!;
  const free = { ...project, active_writer: null };
  host.projects = host.projects.map((p) => p.project_id === free.project_id ? free : p);
  host.emit({ type: "project_upserted", project: free });
  return { host, task, project: free, harness: app.session.state.get().harnesses.find((h) => h.kind === task.harness)! };
}
it("real Composer Continue carries the displayed revision and survives a pace change", async () => {
  const { host, task, project, harness } = endedTask();
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Check the updated tests" });
  const command = vi.spyOn(app.session, "command");
  window.__sim!.pace(4);
  expect(window.__sim!.host).toBe(host);
  view(<Composer project={project} task={task} run={app.session.state.get().runs[task.current_run!]!} harness={harness} agent={null} />);
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await tick();
  expect(command).toHaveBeenCalledWith({ command: "resume_task", task_id: task.task_id, text: "Check the updated tests" }, expect.objectContaining({ expectedRevision: task.revision }));
  expect(app.ui.draft(task.project_id, task.task_id)).toBeNull();
  expect(host.tasks.find((t) => t.task_id === task.task_id)?.state).toBe("dispatching");
});
it("stale Continue keeps text, refreshes the task, explains changes and never retries itself", async () => {
  const { host, task, project, harness } = endedTask();
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Keep my correction" });
  view(<Composer project={project} task={task} run={null} harness={harness} agent={null} />);
  host.tasks = host.tasks.map((t) => t.task_id === task.task_id ? { ...t, revision: t.revision + 1, title: "Updated parser task", state: "reconciliation_required" } : t);
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await tick(400);
  expect(app.ui.draft(task.project_id, task.task_id)?.text).toBe("Keep my correction");
  expect(app.session.state.get().tasks[task.task_id]?.title).toBe("Updated parser task");
  expect(screen.getByText(/Title: Updated parser task/)).toBeTruthy();
  expect(screen.getByText(/Now Outcome unknown/)).toBeTruthy();
  expect(command).toHaveBeenCalledTimes(1);
});
it("Review binds Continue to the reviewed task and asks to review a changed version", async () => {
  const { host, task } = endedTask();
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Check it again" });
  const route = { screen: "review", projectId: task.project_id, taskId: task.task_id, intent: "continue" } as const;
  view(<Review route={route} />, route);
  await act(async () => host.emit({ type: "task_upserted", task: { ...task, revision: task.revision + 1, title: "Changed title", state: "reconciliation_required" } }));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Continue" }).disabled).toBe(true);
  expect(screen.getByRole("button", { name: "Review current task" })).toBeTruthy();
  expect(screen.getByText(/Was Finished; now Outcome unknown/)).toBeTruthy();
  expect(screen.queryByText(/reconciliation required/)).toBeNull();
});
it("answering from the list resolves the card, task attention and all shared selectors", async () => {
  view(<ApprovalQuickActions approvalId={ids.aTest} />);
  fireEvent.click(screen.getByRole("button", { name: "Allow" }));
  await tick();
  const state = app.session.state.get();
  expect(pendingApprovals(state)).toHaveLength(0);
  expect(openAttention(state).some((a) => a.approval_id === ids.aTest)).toBe(false);
  expect(state.tasks[ids.tFlaky]?.state).toBe("working");
  expect(state.tasks[ids.tFlaky]?.attention_count).toBe(0);
  expect(screen.queryByRole("button", { name: "Allow" })).toBeNull();
  expect(screen.getByText(/^Allowed ·/)).toBeTruthy();
  await tick(12_000);
  // Unwatched, DP-1's supervised agent lease lapses meanwhile; that is another task's hand-back.
  expect(openAttention(app.session.state.get()).filter((a) => a.task_id === ids.tFlaky)).toEqual([]);
  expect(runningTasks(app.session.state.get()).some((t) => t.task_id === ids.tFlaky)).toBe(false);
});
it("keeps a held message bubble and confirms Cancel before native delivery", async () => {
  const task = app.session.state.get().tasks[ids.tDash]!;
  const loading = app.session.loadTimeline(task.task_id); await vi.advanceTimersByTimeAsync(100); await loading;
  view(<Transcript task={task} agent="Claude Code" />);
  let sending: ReturnType<HostSession["command"]>;
  await act(async () => { sending = app.session.command({ command: "send_message", task_id: task.task_id, text: "Held correction", bundle: null, delivery: "next_turn" }, { label: "After this turn" }); });
  await tick(); await sending!;
  expect(screen.getByText("Held correction")).toBeTruthy();
  expect(screen.getByText("Held until this turn ends")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await tick(900);
  expect(screen.getByText("Cancelled before delivery")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();
});
it("names no agent in the message placeholder where the text is large for the screen", () => {
  const state = app.session.state.get();
  const task = state.tasks[ids.tDash]!;
  const harness = state.harnesses.find((h) => h.kind === task.harness)!;
  view(<Composer project={state.projects[task.project_id]!} task={task} run={state.runs[task.current_run!]!} harness={harness} agent={null} />);
  const placeholder = () => screen.getByRole("textbox").getAttribute("placeholder");
  expect(placeholder()).toBe(`Message ${harness.display_name}`);
  const size = { innerWidth: window.innerWidth, innerHeight: window.innerHeight };
  Object.defineProperties(window, { innerWidth: { value: 360, configurable: true }, innerHeight: { value: 390, configurable: true } });
  try {
    act(() => { window.dispatchEvent(new Event("resize")); });
    expect(placeholder()).toBe("Message");
  } finally {
    Object.defineProperties(window, { innerWidth: { value: size.innerWidth, configurable: true }, innerHeight: { value: size.innerHeight, configurable: true } });
    act(() => { window.dispatchEvent(new Event("resize")); });
  }
});
it("names how the arrow sends in the chip above it, and starts the next message from Steer now", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const state = app.session.state.get();
  const task = state.tasks[ids.tDash]!;
  const harness = state.harnesses.find((h) => h.kind === task.harness)!;
  view(<Composer project={state.projects[task.project_id]!} task={task} run={state.runs[task.current_run!]!} harness={harness} agent={null} />);
  const pick = (way: RegExp) => {
    fireEvent.click(screen.getAllByRole("button", { name: /^How to send:/ })[0]!);
    fireEvent.click(screen.getByRole("radio", { name: way }));
  };
  expect(screen.getByRole("button", { name: "How to send: Steer now" })).toBeTruthy();
  pick(/^Stop the turn and send/);
  expect(screen.getByRole("button", { name: /^How to send: Stop the turn and send/ })).toBeTruthy();
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Also add the p95 line" } });
  expect(screen.getByRole("button", { name: "Stop and send" })).toBeTruthy();
  pick(/^After this turn/);
  expect(screen.getByRole("button", { name: "How to send: After this turn" })).toBeTruthy();
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await tick(700);
  expect(command).toHaveBeenCalledWith(expect.objectContaining({ command: "send_message", text: "Also add the p95 line", delivery: "next_turn" }), expect.anything());
  expect(screen.getByRole("button", { name: "How to send: Steer now" })).toBeTruthy();
});
it("reviews and edits the persisted queue prompt before explicitly starting", async () => {
  const { host, project } = endedTask();
  const item = host.queue.items.find((i) => i.state.state !== "started")!;
  host.queue = { ...host.queue, writer: null, items: host.queue.items.map((i) => i.item_id === item.item_id ? { ...i, authorization: "ask_before_start", state: { state: "awaiting_start" } } : i) };
  host.emit({ type: "queue_changed", queue: host.queue });
  const route = { screen: "review", projectId: project.project_id, taskId: item.task_id, queueItemId: item.item_id } as const;
  view(<Review route={route} />, route);
  expect(screen.getByLabelText<HTMLTextAreaElement>("Queued prompt").value).toBe(item.request?.prompt);
  fireEvent.change(screen.getByLabelText("Queued prompt"), { target: { value: "Inspect empty state keyboard focus" } });
  fireEvent.click(screen.getByRole("button", { name: "Save prompt changes" }));
  await tick(400);
  expect(host.queue.items.find((i) => i.item_id === item.item_id)?.request?.prompt).toBe("Inspect empty state keyboard focus");
  fireEvent.click(screen.getByRole("button", { name: "Review current request" }));
  fireEvent.click(screen.getByRole("button", { name: "Start queued task" }));
  await tick();
  expect(host.queue.items.find((i) => i.item_id === item.item_id)?.state.state).toBe("started");
});
it("mission bounds and checks persist as requested instructions", () => {
  const mission = missionFrom("Fix focus", "Before changing navigation", "Keep public APIs\nPreserve labels", "Run keyboard checks\nCheck narrow screens");
  expect(mission?.bounds).toEqual([{ text: "Keep public APIs", enforcement: "requested_only" }, { text: "Preserve labels", enforcement: "requested_only" }]);
  expect(mission?.completion_checks).toEqual(["Run keyboard checks", "Check narrow screens"]);
});
it("excludes stale task badges when the current run has ended", () => {
  const state = app.session.state.get();
  const task = state.tasks[ids.tDash]!;
  const run = state.runs[task.current_run!]!;
  expect(runningTasks(state).some((t) => t.task_id === task.task_id)).toBe(true);
  expect(runningTasks({ ...state, runs: { ...state.runs, [run.run_id]: { ...run, state: "completed", ended_at: Date.now() } } }).some((t) => t.task_id === task.task_id)).toBe(false);
});

it("keeps offline continuation drafts editable and disables sending", async () => {
  const { task, project, harness } = endedTask();
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Keep this until connected" });
  view(<Composer project={project} task={task} run={null} harness={harness} agent={null} />);
  await act(async () => window.__sim!.offline());
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Continue" }).disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("What should it do next?"), { target: { value: "Still editable" } });
  expect(app.ui.draft(task.project_id, task.task_id)?.text).toBe("Still editable");
});

it("keeps a blocked continuation's words and puts them in Next up when sent", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === ids.tHeaders)!;
  const project = host.projects.find(p => p.project_id === task.project_id)!;
  app.ui.saveDraft(task.project_id, task.task_id, { text: "My own words" });
  const command = vi.spyOn(app.session, "command");
  view(<Composer project={project} task={task} run={null} harness={app.session.state.get().harnesses.find(h => h.kind === task.harness)!} agent={null} />);
  expect(screen.getByText(/^Waits in Next up: “Fix the flaky retry test/)).toBeTruthy();
  // It can't continue now, so the words wait in Next up for the project.
  expect(screen.queryByRole("button", { name: /^Continue$|Start a new task/ })).toBeNull();
  expect(screen.getByRole<HTMLTextAreaElement>("textbox").value).toBe("My own words");
  expect(command).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Put in Next up" }));
  await tick();
  expect(command).toHaveBeenCalledWith(
    { command: "queue_continuation", task_id: task.task_id, text: "My own words", authorization: "auto_start" },
    expect.anything(),
  );
  expect(app.ui.draft(task.project_id, task.task_id)).toBeNull();
});

it("Review of a continuation another task holds back puts it in Next up", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === ids.tHeaders)!;
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Also cover /v1/refunds" });
  const command = vi.spyOn(app.session, "command");
  const route = { screen: "review", projectId: task.project_id, taskId: task.task_id, intent: "continue" } as const;
  view(<Review route={route} />, route);
  expect(screen.getByText(/^Waits in Next up, then continues this task with your message\. Waiting for your approval on “Fix the flaky retry test/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Put in Next up" }));
  await tick();
  expect(command.mock.calls[0]![0]).toEqual({ command: "queue_continuation", task_id: task.task_id, text: "Also cover /v1/refunds", authorization: "auto_start" });
  expect(app.session.state.get().queues[task.project_id]?.items.some((i) => i.task_id === task.task_id && i.state.state !== "started")).toBe(true);
});

/** A held-back continuation's draft holding markup over one uploaded picture. */
async function markedContinuation(text: string) {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === ids.tHeaders)!;
  const project = host.projects.find(p => p.project_id === task.project_id)!;
  const { annotationStore } = await import("../../state/annotations");
  const store = annotationStore(app.profile.profileId);
  const annotation = store.create(task.project_id, task.task_id);
  const source = await host.addArtifact(new Uint8Array([1, 2, 3]), "image/png", "0".repeat(64), "Preview picture", task.task_id);
  store.update(annotation.id, d => ({ ...d, generalNote: "Only spacing", sources: [{
    key: "S1", blobKey: null, artifactId: source.artifact_id, sha256: source.sha256, mediaType: "image/png", width: 390, height: 260,
    durationMs: null, capturedAt: Date.now(), derivedFrom: null, derivation: null, origin: { kind: "phone_import", original_name: "preview.png" },
  }] }));
  app.ui.saveDraft(task.project_id, task.task_id, { text, annotationId: annotation.id });
  return { task, project, harness: app.session.state.get().harnesses.find(h => h.kind === task.harness)!, source };
}

it("markup on a held-back continuation goes through Review and waits in Next up with its words", async () => {
  const { task, project, harness, source } = await markedContinuation("Tighten this gap");
  const command = vi.spyOn(app.session, "command");
  const composer = view(<Composer project={project} task={task} run={null} harness={harness} agent={null} />);
  expect(screen.queryByText(QUEUE_MARKUP_UPDATE)).toBeNull();
  expect(screen.queryByRole("button", { name: "Put in Next up" })).toBeNull();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Review" }).disabled).toBe(false);
  composer.unmount();
  const route = { screen: "review", projectId: task.project_id, taskId: task.task_id, intent: "continue" } as const;
  view(<Review route={route} />, route);
  fireEvent.click(screen.getByRole("button", { name: "Put in Next up" }));
  await tick(700);
  const sent = command.mock.calls.map(c => c[0]).find(c => c.command === "queue_continuation");
  expect(sent).toMatchObject({ task_id: task.task_id, text: "Tighten this gap", authorization: "auto_start" });
  expect(sent && "bundle" in sent && sent.bundle?.sources.map(s => s.artifact_id)).toEqual([source.artifact_id]);
  expect(app.ui.draft(task.project_id, task.task_id)).toBeNull();
  expect(app.session.state.get().queues[task.project_id]?.items.find(i => i.task_id === task.task_id && i.state.state !== "started")?.request?.bundle).toBeTruthy();
});

it("an older computer keeps markup out of Next up and says to update it", async () => {
  const { task, project, harness } = await markedContinuation("Tighten this gap");
  app.session.state.set(s => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter(f => f !== QUEUE_MARKUP_FEATURE) } }));
  const command = vi.spyOn(app.session, "command");
  view(<Composer project={project} task={task} run={null} harness={harness} agent={null} />);
  expect(screen.getByText(QUEUE_MARKUP_UPDATE)).toBeTruthy();
  expect(screen.queryByRole<HTMLButtonElement>("button", { name: "Review" })?.disabled ?? true).toBe(true);
  expect(command).not.toHaveBeenCalled();
  expect(app.ui.draft(task.project_id, task.task_id)?.annotationId).toBeTruthy();
});

it("receipt Continue replaces a previous new-task choice without adding scaffold text", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const { task, project, harness } = endedTask();
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Keep my words" });
  view(<Composer project={project} task={task} run={null} harness={harness} agent={null} />);
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  fireEvent.click(screen.getByRole("radio", { name: /Start a new task/ }));
  expect(screen.queryByLabelText("What should it do next?")).toBeNull();
  const receipt = view(<Receipt route={{ screen: "receipt", taskId: task.task_id }} />);
  fireEvent.click([...receipt.container.querySelectorAll("button")].find(b => b.textContent === "Continue your draft")!);
  fireEvent.click(within(receipt.container).getByRole("button", { name: "Continue" }));
  expect(within(receipt.container).getByLabelText<HTMLTextAreaElement>("What should it do next?").value).toBe("Keep my words");
  expect(app.ui.draft(task.project_id, task.task_id)?.text).toBe("Keep my words");
  await tick();
});

it("Back from an empty Annotate leaves no attachment chip and keeps Continue", async () => {
  const { task, project, harness } = endedTask();
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Continue checking" });
  const composer = () => <Composer project={project} task={task} run={null} harness={harness} agent={null} />;
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const mounted = view(composer());
  // With words in the field, Attach steps aside for Send and is reached from the tools.
  expect(screen.queryByRole("button", { name: "Attach" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Message tools" }));
  fireEvent.click(screen.getByRole("button", { name: /^Attach/ }));
  fireEvent.click(within(screen.getByRole("dialog", { name: "Attach" })).getByRole("button", { name: /^Mark up/ }));
  const route = app.ui.route();
  expect(route.screen).toBe("annotate");
  if (route.screen !== "annotate") throw Error("Expected Annotate");
  mounted.unmount();
  const annotate = view(<Annotate route={route} />, route);
  fireEvent.click(screen.getByRole("button", { name: "Back and keep draft" }));
  annotate.unmount();
  const back = view(composer());
  expect(back.container.querySelector(".composer__attachment")).toBeNull();
  expect(screen.queryByText("Empty")).toBeNull();
  expect(screen.queryByRole("button", { name: "Review" })).toBeNull();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Continue" }).disabled).toBe(false);
});

it.each([
  ["host_accepted", "Handed to studio · not yet confirmed by the agent"],
  ["native_delivered", "Delivered to the agent"],
  ["queued_for_boundary", "Held until this turn ends"],
  ["cancelled", "Cancelled before delivery"],
  ["unknown", "Outcome unknown"],
  ["failed", "Not delivered"],
] as const)("Chat delivery caption for %s", async (delivery, caption) => {
  const { task } = endedTask();
  vi.spyOn(app.session, "loadTimeline").mockResolvedValue(undefined);
  app.session.state.set(s => ({ ...s, timelines: { ...s.timelines, [task.task_id]: { loaded: true, hasMore: false, items: [{ seq: 99999, at: Date.now(), run_id: task.current_run, item: { kind: "user_message", text: "Review this prompt", bundle: null, delivery } }] } } }));
  view(<Transcript task={task} agent="Claude Code" />);
  await tick();
  expect(screen.getByText(caption)).toBeTruthy();
});

it("explains the stopped queue in Chat and starts one reviewed task without resuming the rest", async () => {
  const { host, project, harness } = endedTask();
  const item = host.queue.items.find(i => i.state.state !== "started")!;
  const task = host.tasks.find(t => t.task_id === item.task_id)!;
  const reason = 'Next up paused when you stopped “Parser check”, so nothing starts by itself.';
  host.queue = { ...host.queue, paused: true, pause_reason: reason, writer: null };
  host.emit({ type: "queue_changed", queue: host.queue });
  view(<Composer project={project} task={task} run={null} harness={harness} agent={null} />);
  expect(screen.getByText("Next up is paused")).toBeTruthy();
  expect(screen.getByText(reason)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Review queued task" }));
  const route = app.ui.get().stacks.talk.at(-1)!;
  expect(route).toMatchObject({ screen: "review", queueItemId: item.item_id });
  cleanup();
  if (route.screen !== "review") throw Error("Missing review");
  view(<Review route={route} />, route);
  expect(screen.queryByRole("button", { name: "Resume Next up" })).toBeNull();
  expect(screen.getByText("Only this task will start. The rest of Next up stays paused.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start only this task" }));
  await tick(700);
  expect(host.queue.paused).toBe(true);
  expect(host.queue.items.find(i => i.item_id === item.item_id)?.state.state).toBe("started");
  expect(Object.values(app.session.commands.get()).filter(c => c.body.command === "queue_start")).toHaveLength(1);
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "queue_resume")).toBe(false);
});

it("retains a denial reason in the decision line and the selected run receipt", async () => {
  view(<ApprovalQuickActions approvalId={ids.aTest} />);
  fireEvent.click(screen.getByRole("button", { name: "Deny…" }));
  fireEvent.change(screen.getByLabelText("Reason (optional)"), { target: { value: "Please wait for the maintainer." } });
  fireEvent.click(screen.getByRole("button", { name: "Deny request" }));
  await tick(500);
  expect(screen.getByText("Reason: Please wait for the maintainer.")).toBeTruthy();
  const approval = app.session.state.get().approvals[ids.aTest]!;
  expect(approval.delivery?.decision).toEqual({ decision: "deny", reason: "Please wait for the maintainer." });
  cleanup();
  view(<Receipt route={{ screen: "receipt", taskId: approval.task_id, runId: approval.run_id }} />);
  await tick(600);
  expect(screen.getByText("Approval decisions")).toBeTruthy();
  expect(screen.getByText("Reason: Please wait for the maintainer.")).toBeTruthy();
});

it.each(["composer", "receipt"])("%s opens Review in Work with the plan; only Start changes mode", async source => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const { host, project, harness } = endedTask();
  const original = host.tasks.find(t => t.task_id === ids.tHeaders)!;
  const task = { ...original, mode: "plan" as const, current_run: ids.rHeaders };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? task : t);
  host.emit({ type: "task_upserted", task });
  const plan = "1. Inspect the parser.\n2. Add regression coverage.\n3. Implement and verify.";
  host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: task.current_run, item: { kind: "activity", tool: "ExitPlanMode", summary: "Plan proposed", status: "succeeded", detail: plan } } });
  if (source === "composer") {
    view(<Composer project={project} task={task} run={app.session.state.get().runs[task.current_run!]!} harness={harness} agent={null} />);
    await tick(500);
    fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  } else {
    view(<Receipt route={{ screen: "receipt", taskId: task.task_id }} />);
    await tick(500);
  }
  fireEvent.click(screen.getByRole("button", { name: /^Carry out this plan/ }));
  await tick(400);
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "resume_task")).toBe(false);
  expect(host.tasks.find(t => t.task_id === task.task_id)?.mode).toBe("plan");
  const route = app.ui.get().stacks.talk.at(-1)!;
  expect(route).toMatchObject({ screen: "review", intent: "carry_out_plan", taskId: task.task_id });
  expect(route).toMatchObject({ plan: { text: plan } });
  cleanup();
  if (route.screen !== "review") throw Error("Review missing");
  const command = vi.spyOn(app.session, "command");
  view(<Review route={route} />, route);
  expect(screen.getByText("Mode: Work.")).toBeTruthy();
  expect(screen.getByText(/before actions that need more permission/)).toBeTruthy();
  // The receipt's button already said to carry it out, so Review starts it;
  // the composer's choice is reviewed first.
  if (source === "composer") {
    const startWork = screen.getByRole<HTMLButtonElement>("button", { name: "Start Work" });
    expect(startWork.previousElementSibling?.textContent).toBe("Switching from Plan to Work. It can change files.");
    expect(startWork.disabled, document.querySelector(".review-foot")?.textContent ?? "footer").toBe(false);
    fireEvent.click(startWork);
  }
  await tick(700);
  expect(command).toHaveBeenCalledWith(expect.objectContaining({ command: "resume_task", task_id: task.task_id, mode: "work", text: expect.stringContaining(plan) }), expect.objectContaining({ expectedRevision: task.revision }));
  expect(host.tasks.find(t => t.task_id === task.task_id)?.mode).toBe("work");
});
it("says right above each Start when it switches mode; an agent that only reads starts nothing", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  const { host } = endedTask();
  const task = { ...host.tasks.find(t => t.task_id === ids.tHeaders)!, mode: "plan" as const, current_run: ids.rHeaders };
  const base = host.runs.find(r => r.run_id === ids.rHeaders)!;
  const run = { ...base, mode: "plan" as const, outcome: { ...base.outcome!, report: { status: "done" as const, questions: [], remaining: [], new_work: [
    { title: "Pass the headers through the gateway", brief: "Change the gateway config to pass the RateLimit headers.", mode: "work" as const },
    { title: "Check the staging logs", brief: "Read the staging logs for stripped headers.", mode: "plan" as const },
  ] } } };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? task : t);
  host.runs = host.runs.map(r => r.run_id === run.run_id ? run : r);
  host.emit({ type: "task_upserted", task });
  host.emit({ type: "run_upserted", run });
  const command = vi.spyOn(app.session, "command");
  view(<Receipt route={{ screen: "receipt", taskId: task.task_id }} />);
  await tick(500);
  // Nothing it proposed has started.
  expect(command.mock.calls.some(([c]) => c.command === "create_task" || c.command === "resume_task")).toBe(false);
  const carry = screen.getByRole("button", { name: /^Carry out this plan/ });
  expect(carry.closest(".work-actions")!.previousElementSibling!.textContent).toBe("Switching from Plan to Work. It can change files.");
  const [work, plan] = [...document.querySelectorAll<HTMLElement>(".receipt-new-work")];
  const note = within(work!).getByRole("note");
  expect(note.textContent).toBe("Switching from Plan to Work. This new task can change files, and starts with “Skip permissions”.");
  expect(note.nextElementSibling).toBe(within(work!).getByRole("button", { name: "Start" }));
  // Work it proposes in its own mode switches nothing, so nothing is said.
  expect(within(plan!).queryByRole("note")).toBeNull();
  fireEvent.click(within(work!).getByRole("button", { name: "Start" }));
  await tick(400);
  expect(app.ui.draft(task.project_id, null)).toMatchObject({ mode: "work", review: { from: "plan", title: "Pass the headers through the gateway" } });
  const route = app.ui.get().stacks.talk.at(-1)!;
  expect(route).toMatchObject({ screen: "review", intent: "new_task", taskId: null });
  cleanup();
  if (route.screen !== "review") throw Error("Review missing");
  // Where Review stays open, the note is right above its Start too.
  takeSendAsk(task.project_id, null);
  view(<Review route={route} />, route);
  await tick();
  const footNote = document.querySelector(".review-foot")!.querySelector<HTMLElement>("[role=note]")!;
  expect(footNote.textContent).toBe("Switching from Plan to Work. This new task can change files, and starts with “Skip permissions”.");
  expect(footNote.nextElementSibling?.tagName).toBe("BUTTON");
});
it.each([false, true])("slow Review send finishes Control after a tab switch (annotated message %s)", async message => {
  const taskId = message ? ids.tDash : null;
  const projectId = message ? ids.pTide : ids.pGarden;
  const { annotationStore } = await import("../../state/annotations");
  const annotation = annotationStore(app.profile.profileId).create(projectId, taskId);
  const source = await window.__sim!.host.addArtifact(new Uint8Array([1, 2, 3]), "image/png", "0".repeat(64), "Preview picture", taskId);
  annotationStore(app.profile.profileId).update(annotation.id, d => ({ ...d, generalNote: "Only spacing", sources: [{
    key: "S1", blobKey: null, artifactId: source.artifact_id, sha256: source.sha256, mediaType: "image/png", width: 390, height: 260,
    durationMs: null, capturedAt: Date.now(), derivedFrom: null, derivation: null, origin: { kind: "phone_import", original_name: "preview.png" },
  }] }));
  app.ui.saveDraft(projectId, taskId, { text: "Keep the typography", mode: "ask", annotationId: annotation.id });
  const route = { screen: "review", projectId, taskId, intent: message ? "next_turn" : "new_task" } as const;
  app.ui.push({ screen: "annotate", annotationId: annotation.id }, "control"); app.ui.push(route);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "control", depth: 2, active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
  window.__sim!.latency(1500); window.__sim!.pace(50);
  fireEvent.click(screen.getByRole("button", { name: message ? "Send" : "Start task" }));
  act(() => app.ui.push({ screen: "receipt", taskId: ids.tHeaders }, "talk"));
  const talk = app.ui.get().stacks.talk;
  const selection = app.ui.get().taskByProject;
  await tick(2000);
  expect(app.ui.draft(projectId, taskId)).toBeNull();
  expect(app.ui.get().stacks.control).toEqual([{ screen: "control" }]);
  expect(app.ui.get().stacks.talk).toBe(talk);
  expect(app.ui.get().taskByProject).toBe(selection);
  expect(app.ui.get().tab).toBe("talk");
});
it("a failed send after leaving Review shows a visible notice and keeps the draft", async () => {
  const notifications = await import("../../design/Toast");
  const show = vi.spyOn(notifications, "toast");
  const route = { screen: "review", projectId: ids.pGarden, taskId: null, intent: "new_task" } as const;
  app.ui.saveDraft(ids.pGarden, null, { text: "Keep this request", mode: "ask" }); app.ui.push(route, "control");
  vi.spyOn(app.session, "command").mockImplementation(() => new Promise((_, reject) => setTimeout(() => reject(Error("Connection lost")), 1500)));
  render(<AppProvider app={app}><RouteProvider value={{ tab: "control", depth: 1, active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Start task" }));
  act(() => app.ui.selectTab("talk")); await tick(1800);
  expect(app.ui.draft(ids.pGarden, null)?.text).toBe("Keep this request");
  expect(app.ui.get().stacks.control.at(-1)).toBe(route);
  expect(show).toHaveBeenCalledWith(expect.stringContaining("Your draft is kept in Review"), expect.objectContaining({ tone: "stop", action: expect.objectContaining({ label: "Review" }) }));
});

it.each(["ask_before_start", "auto_start"] as const)("queued first message waits in Next up (%s)", async authorization => {
  const host = window.__sim!.host;
  const task = { ...host.tasks.find(t => t.task_id === ids.tNotes)!, state: "accepted" as const, current_run: null };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? task : t);
  host.emit({ type: "task_upserted", task });
  for (let i = host.journal.length - 1; i >= 0; i--) {
    const event = host.journal[i]!.event;
    if (event.type === "timeline_appended" && event.task_id === task.task_id) host.journal.splice(i, 1);
  }
  host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: null, item: { kind: "user_message", text: "Review tomorrow", bundle: null, delivery: "host_accepted" } } });
  host.emit({ type: "queue_changed", queue: { ...host.queue, project_id: task.project_id, paused: true, items: [{ ...host.queue.items[0]!, task_id: task.task_id, authorization, authorization_expires_at: Date.now() + 60000, state: { state: "waiting" } }] } });
  const mounted = view(<Transcript task={task} agent="Codex" />);
  await tick(500);
  expect(screen.getByText("Waiting in Next up")).toBeTruthy();
  expect(screen.getByText(authorization === "ask_before_start" ? /Will ask before starting/ : /Can start without asking again/)).toBeTruthy();
  expect(screen.queryByText(/Handed to/)).toBeNull();
  mounted.rerender(<AppProvider app={app}><Transcript task={{ ...task, state: "dispatching", current_run: "started" }} agent="Codex" /></AppProvider>);
  expect(screen.queryByText("Waiting in Next up")).toBeNull();
  expect(screen.getByText(/Handed to/)).toBeTruthy();
});
