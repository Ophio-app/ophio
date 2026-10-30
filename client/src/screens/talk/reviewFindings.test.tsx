// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { workIds } from "../../dev/sim/work";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { Composer } from "./Composer";
import { Transcript } from "./Transcript";
import { TalkHeader } from "./TalkHeader";
import { TalkConversations } from "./Conversations";
import Review from "./Review";
import Talk from "./Talk";
import Receipt from "../work/Receipt";
import SafetyStrip from "../control/SafetyStrip";
import Agents from "../work/Agents";
import { AgentSheet } from "./AgentSheet";

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
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
function view(child: React.ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{child}</RouteProvider></AppProvider>);
}
function freeWriter() {
  const host = window.__sim!.host;
  const project = host.projects.find((p) => p.project_id === ids.pTide)!;
  const free = { ...project, active_writer: null };
  host.projects = host.projects.map((p) => p.project_id === free.project_id ? free : p);
  host.emit({ type: "project_upserted", project: free });
}
it.each([
  ["locked", false, true], ["unknown", false, true], ["unknown", true, false],
] as const)("Chat and Review fence computer-use Continue (lock: %s, input available: %s)", async (lock, inputAvailable, blocked) => {
  // A lock this device can undo is undone right there; anything else is for Control.
  const unlockHere = lock === "locked";
  const host = window.__sim!.host;
  const original = host.tasks.find(t => t.kind === "computer_use")!;
  const task = { ...original, state: "interrupted" as const };
  const project = { ...host.projects.find(p => p.project_id === task.project_id)!, active_writer: null };
  const run = { ...host.runs.find(r => r.run_id === task.current_run)!, state: "interrupted" as const };
  host.emit({ type: "task_upserted", task });
  host.emit({ type: "project_upserted", project });
  host.emit({ type: "run_upserted", run });
  const control = host.control.find(c => c.target_id === task.target_id)!;
  host.emit({ type: "control_changed", view: { ...control, desktop_lock: lock, input_state: "paused" } });
  host.emit({ type: "capabilities_changed", capabilities: { ...host.host.capabilities,
    input: inputAvailable ? { state: "available" } : { state: "temporarily_unavailable", reason: "Desktop input unavailable while locked or unknown" } } });
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Continue checking the desktop" });
  const harness = app.session.state.get().harnesses.find(h => h.kind === task.harness)!;
  const composer = view(<Composer project={project} task={task} run={run} harness={harness} agent={null} />);
  expect(screen.getByRole<HTMLButtonElement>("button", { name: /^Continue$/ }).disabled).toBe(blocked);
  if (unlockHere) {
    expect(screen.getByText(/Unlock it here with its password/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Unlock with password" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Open Control" })).toBeNull();
  } else if (blocked) {
    fireEvent.click(screen.getByRole("button", { name: "Open Control" }));
    expect(app.ui.route()).toEqual({ screen: "control" });
  }
  composer.unmount();
  const route = { screen: "review", projectId: task.project_id, taskId: task.task_id, intent: "continue" } as const;
  view(<Review route={route} />, route);
  expect(screen.getByRole<HTMLButtonElement>("button", { name: /^Continue$/ }).disabled).toBe(blocked);
  if (blocked) expect(screen.getByRole("button", { name: unlockHere ? "Unlock with password" : "Open Control" })).toBeTruthy();
});
it("keeps sheet edits across parent updates and explains an unsupported effort", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const project = app.session.state.get().projects[ids.pTide]!;
  const sheet = (open = true) => <AppProvider app={app}><AgentSheet open={open} onClose={() => {}} project={project}
    value={{ harness: "claude_code", model: "claude-opus-5-5", effort: "high" }} onChange={() => {}} /></AppProvider>;
  const mounted = render(sheet());
  await tick();
  fireEvent.click(screen.getByRole("button", { name: /^Codex/ }));
  mounted.rerender(sheet());
  expect(screen.getByRole("button", { name: "GPT-6-Sol" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "High" }));
  mounted.rerender(sheet());
  expect(screen.getByRole("button", { name: "High" }).getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(screen.getByRole("button", { name: "GPT-6-Sol" }));
  expect(screen.getByText(/GPT-6-Sol doesn’t offer High effort/)).toBeTruthy();
  mounted.rerender(sheet(false));
  mounted.rerender(sheet());
  expect(screen.getByRole("button", { name: /^Claude Opus 5.5$/ }).getAttribute("aria-pressed")).toBe("true");
});
it.each(["completed", "working"] as const)("annotation Review derives delivery for a %s task", async (state) => {
  freeWriter();
  const task = app.session.state.get().tasks[state === "completed" ? workIds.receipt : ids.tDash]!;
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Keep the chart visible" });
  const command = vi.spyOn(app.session, "command");
  const route = { screen: "review", projectId: task.project_id, taskId: task.task_id } as const;
  view(<Review route={route} />, route);
  const action = screen.getByRole<HTMLButtonElement>("button", { name: state === "completed" ? "Continue" : "Send" });
  expect(action.disabled).toBe(false);
  fireEvent.click(action); await tick();
  if (state === "completed") expect(command).toHaveBeenCalledWith(expect.objectContaining({ command: "resume_task", task_id: task.task_id }), expect.objectContaining({ expectedRevision: task.revision }));
  else expect(command).toHaveBeenCalledWith(expect.objectContaining({ command: "send_message", task_id: task.task_id, delivery: "next_turn" }), expect.anything());
});
it("a removed queue item emits the archived task, invalidates its permission, and never starts", async () => {
  const host = window.__sim!.host;
  const item = host.queue.items.find((i) => i.item_id === workIds.qReady)!;
  const revision = host.queue.revision;
  const pending = app.session.command({ command: "queue_remove", item_id: item.item_id }, { label: "Remove", expectedRevision: revision });
  await tick(); await pending;
  expect(host.queue.items.some((i) => i.item_id === item.item_id)).toBe(false);
  expect(app.session.state.get().tasks[item.task_id]?.state).toBe("archived");
  expect(host.journal.some(({ event }) => event.type === "task_upserted" && event.task.task_id === item.task_id && event.task.state === "archived")).toBe(true);
  const task = app.session.state.get().tasks[item.task_id]!;
  const project = app.session.state.get().projects[task.project_id]!;
  view(<Composer project={project} task={task} run={null} harness={null} agent={null} />);
  expect(screen.queryByText(/starts when/)).toBeNull();
  expect(screen.getByText("Archived")).toBeTruthy();
});
it("an accepted task without a queue item does not promise a future start", () => {
  const task = app.session.state.get().tasks[workIds.ready]!;
  const project = app.session.state.get().projects[task.project_id]!;
  app.session.state.set((s) => ({ ...s, queues: { ...s.queues, [task.project_id]: { ...window.__sim!.host.queue, items: [] } } }));
  view(<Composer project={project} task={task} run={null} harness={null} agent={null} />);
  expect(screen.getByText("Not queued")).toBeTruthy();
  expect(screen.queryByText(/starts when/)).toBeNull();
});
it("offline Chat labels last reported activity and removes both streaming carets and spinners", async () => {
  const task = app.session.state.get().tasks[ids.tDash]!;
  const project = app.session.state.get().projects[task.project_id]!;
  const loading = app.session.loadTimeline(task.task_id); await tick(); await loading;
  app.session.state.set((s) => ({ ...s, liveText: { stream: { key: "stream", taskId: task.task_id, runId: task.current_run!, text: "A streamed reply", at: Date.now() } },
    timelines: { ...s.timelines, [task.task_id]: { loaded: true, hasMore: false, items: [{ seq: 50, at: Date.now(), run_id: task.current_run, item: { kind: "agent_message", text: "An interrupted partial reply", partial: true } }] } } }));
  const { container } = view(<><TalkHeader project={project} task={task} run={undefined} /><Transcript task={task} agent="Claude Code" /></>);
  expect(container.querySelector(".talk-caret")).toBeNull();
  expect(container.querySelector(".talk-working .stateline__spinner")).toBeTruthy();
  await act(async () => window.__sim!.offline());
  expect(container.querySelector(".talk-working .stateline__spinner")).toBeNull();
  expect(container.querySelector(".chat-head__sub")?.textContent).toContain("Last reported");
  expect(container.querySelector(".talk-working")?.textContent).toContain("last reported");
});

it("offline, Chat's conversations say each state is only what the computer last reported", async () => {
  const task = app.session.state.get().tasks[ids.tDash]!;
  const project = app.session.state.get().projects[task.project_id]!;
  act(() => app.session.state.set((s) => ({ ...s, tasks: Object.fromEntries(Object.entries(s.tasks).map(([id, t]) => [id, t.project_id === project.project_id ? { ...t, conversation: true } : t])) })));
  view(<TalkConversations project={project} />);
  const pills = () => [...document.querySelectorAll(".talk-tasks .row__pills .pill")].map((p) => p.textContent).filter((p) => p !== "New reply" && p !== "Draft");
  expect(pills().length).toBeGreaterThan(0);
  expect(pills().some((p) => p?.startsWith("Last reported"))).toBe(false);
  await act(async () => window.__sim!.offline());
  expect(pills().every((p) => p?.startsWith("Last reported · "))).toBe(true);
});
it("a Continue blocked by another task says so, waits in Next up, and opens the task", async () => {
  const state = app.session.state.get();
  const task = state.tasks[ids.tHeaders]!;
  const harness = state.harnesses.find((h) => h.kind === task.harness)!;
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Also cover /v1/refunds" });
  view(<Composer project={state.projects[task.project_id]!} task={task} run={null} harness={harness} agent={null} />);
  expect(screen.getByRole("status").textContent).toBe("Waits in Next up: “Fix the flaky retry test in…” is waiting for your approvalOpen");
  // It can't continue now; the words go in Next up and continue it after.
  expect(screen.getByRole<HTMLTextAreaElement>("textbox").value).toBe("Also cover /v1/refunds");
  expect(screen.queryByRole("button", { name: /^Continue$/ })).toBeNull();
  expect(screen.getByRole("button", { name: "Put in Next up" })).toBeTruthy();
  expect(app.ui.draft(task.project_id, task.task_id)?.text).toBe("Also cover /v1/refunds");
  const open = screen.getByRole("button", { name: "Open the blocking task" });
  expect(open.textContent).toBe("Open");
  fireEvent.click(open);
  expect(app.ui.get().taskByProject[task.project_id]).toBe(ids.tFlaky);
  expect(app.ui.get().tab).toBe("talk");
});
it("offline Agents qualifies activity and desktop input", async () => {
  app.ui.selectProject(ids.pTide);
  await act(async () => window.__sim!.offline());
  const { container } = view(<Agents route={{ screen: "agents" }} />);
  expect(container.textContent).toContain("last reported");
  expect(screen.getByText("Agent last reported with desktop input")).toBeTruthy();
});
it("a receipt opens its current Markdown file on the same stack and uses the reusable route", async () => {
  const route = { screen: "receipt", taskId: workIds.receipt } as const;
  app.ui.push(route, "work");
  const { container } = view(<Receipt route={route} />, route);
  await tick(800);
  fireEvent.click(screen.getByRole("button", { name: /README.md/ }));
  expect(app.ui.route()).toEqual({ screen: "file", rootId: ids.rootInstr, path: "README.md" });
  expect(app.ui.get().tab).toBe("work");
  app.ui.back(); expect(app.ui.route()).toEqual(route);
  fireEvent.click(screen.getByRole("button", { name: "Make reusable…" }));
  expect(app.ui.route()).toEqual({ screen: "reusable", taskId: workIds.receipt });
  expect(container.querySelector('input[maxlength="120"]')).toBeNull();
});

it("the collapsed safety rail selects the displayed task, never a null-target match", () => {
  app.ui.selectTask(ids.pTide, ids.tDash);
  view(<SafetyStrip dismiss={() => {}} />);
  const stop = screen.getAllByRole("button", { name: /^Stop/ });
  expect(stop).toHaveLength(1);
  // The rail's Stop is the symbol alone, named for its task and display; the
  // quiet caption beside it puts the display's state on a line of its own
  // under the task name, each cut short on its own.
  expect(stop[0]?.textContent).toBe("");
  expect(stop[0]?.getAttribute("aria-label")).toContain("Check the staging dashboard");
  const caption = stop[0]?.closest(".named-stop")?.querySelector(".named-stop__caption");
  expect(caption?.querySelector(".safety-rail__title")?.textContent).toContain("Check the staging dashboard");
  expect(caption?.querySelector(".safety-rail__line")?.textContent).toContain("DP-1");
  fireEvent.click(screen.getByRole("button", { name: /Safety/ }));
  expect(screen.getAllByRole("button", { name: /^Stop/ })).toHaveLength(2);
});
it("a task this device just created waits for its record instead of reading as removed", async () => {
  const project = Object.values(app.session.state.get().projects)[0]!;
  app.ui.selectCreated(project.project_id, "task-on-its-way");
  view(<Talk route={{ screen: "talk" }} />);
  expect(screen.getByLabelText("Loading the task")).toBeTruthy();
  expect(screen.queryByText("That task isn’t on this computer any more")).toBeNull();
  await tick(10_000);
  expect(screen.getByText("That task isn’t on this computer any more")).toBeTruthy();
  app.ui.selectTask(project.project_id, "removed-elsewhere");
  await tick();
  expect(screen.getByText("That task isn’t on this computer any more")).toBeTruthy();
});

it("new-task review offers Ask and Plan while Work explains its missing permission bridge", async () => {
  const project = app.session.state.get().projects[ids.pTide]!;
  const harness = app.session.state.get().harnesses.find((h) => h.kind === "claude_code")!;
  const remedy = "Work needs the permission bridge. Install computer-mcp beside Øphio on the computer to enable Claude permission prompts.";
  const modes = (["ask", "plan", "work"] as const).map((mode) => ({ mode, enforcement: "Read-only fixture", availability: mode === "work" ? { state: "permission_missing" as const, remedy } : { state: "available" as const } }));
  app.session.state.set((s) => ({ ...s, harnesses: [{ ...harness, modes, capabilities: { ...harness.capabilities, start_run: { state: "available" }, native_approvals: { state: "permission_missing", remedy } } }] }));
  app.ui.saveDraft(project.project_id, null, { text: "Explain the code", agent: { harness: "claude_code", model: null, effort: null } });
  const route = { screen: "review", projectId: project.project_id, taskId: null, intent: "new_task" } as const;
  view(<Review route={route} />, route);
  await tick();
  expect(screen.getByText(remedy)).toBeTruthy();
  expect(screen.getByRole("radio", { name: /^Work/ })).toHaveProperty("disabled", true);
  for (const mode of ["Ask", "Plan"]) {
    const radio = screen.getByRole("radio", { name: mode });
    expect(radio).toHaveProperty("disabled", false);
    fireEvent.click(radio);
    expect(screen.getByRole("button", { name: "Start task" })).toHaveProperty("disabled", false);
  }
});

it("names when a queued task starts on screen, not only to a screen reader", () => {
  const route: Route = { screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" };
  app.ui.saveDraft(ids.pTide, null, { text: "Tidy the retry notes", mode: "work" });
  view(<Review route={route} />, route);
  fireEvent.click(screen.getByRole("radio", { name: "Add to Next up" }));
  const choice = screen.getByRole("radiogroup", { name: "When its turn comes" });
  const field = choice.closest(".field")!;
  expect(field.querySelector(".field__label")?.textContent).toBe("When its turn comes");
  expect(within(choice).getAllByRole("radio").map((r) => r.textContent)).toEqual(["Ask me first", "Start by itself"]);
});
