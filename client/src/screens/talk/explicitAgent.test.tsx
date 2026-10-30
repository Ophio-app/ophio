// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { createTaskCommand, resolveAgent } from "./intent";
import { uuidv7 } from "../../protocol/ids";
import Review from "./Review";
import Talk from "./Talk";

let app: App;
const astra = { harness: "codex" as const, model: "gpt-6-astra", effort: "high" };
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
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
  session.start(); await tick();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
function view(route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{route.screen === "review" ? <Review route={route} /> : <Talk route={{ screen: "talk" }} />}</RouteProvider></AppProvider>);
}
function unavailableCodex() {
  const host = window.__sim!.host;
  host.emit({ type: "harnesses_changed", harnesses: app.session.state.get().harnesses.map(h => h.kind === "codex" ? { ...h, capabilities: { ...h.capabilities, start_run: { state: "temporarily_unavailable", reason: "Command helper is missing" } } } : h) });
}
function oldTask(chosen = true) {
  const host = window.__sim!.host;
  const task = { ...app.session.state.get().tasks[ids.tHeaders]!, harness: "claude_code" as const, model: "claude-opus-5-5", effort: "high" };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? task : t);
  host.emit({ type: "task_upserted", task });
  const run = { ...app.session.state.get().runs[ids.rHeaders]!, harness: task.harness, requested_model: task.model, actual_model: task.model };
  host.runs = host.runs.map(r => r.run_id === run.run_id ? run : r);
  host.emit({ type: "run_upserted", run });
  const project = { ...app.session.state.get().projects[task.project_id]!, active_writer: null };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project });
  app.ui.selectTask(project.project_id, task.task_id);
  if (chosen) app.ui.saveDraft(project.project_id, null, { agent: astra });
  app.ui.saveDraft(project.project_id, task.task_id, { text: "Read the git history" });
  return task;
}

it.each(["standard", "bypass_permissions", "sandboxed"] as const)("reviews and persists the exact launch policy %s", async launchPolicy => {
  oldTask(false); app.ui.selectTask(ids.pTide, null);
  app.ui.saveDraft(ids.pTide, null, { text: "Check the parser", agent: { ...astra, launchPolicy }, mode: "work" });
  view({ screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" }); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  const body = Object.values(app.session.commands.get()).find(c => c.body.command === "create_task")?.body;
  expect(body).toMatchObject({ command: "create_task", ...astra, launch_policy: launchPolicy });
  const task = Object.values(app.session.state.get().tasks).find(t => t.title === "Check the parser")!;
  expect(task.launch_policy).toBe(launchPolicy);
  expect(app.session.state.get().runs[task.current_run!]?.launch_policy).toBe(launchPolicy);
});
it("requires confirmation for a flags-only continuation mismatch without changing the saved task", async () => {
  const task = oldTask(false);
  app.ui.saveDraft(task.project_id, null, { agent: { harness: task.harness, model: task.model, effort: task.effort, launchPolicy: "bypass_permissions" } });
  view(); await tick();
  expect(screen.getByText("This task uses a different permission setting")).toBeTruthy();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Continue" }).disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Keep this task’s agent" }));
  fireEvent.click(screen.getByRole("button", { name: "Continue" })); await tick();
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "resume_task")).toBe(true);
  expect(app.session.state.get().tasks[task.task_id]?.launch_policy).toBeUndefined();
});
it("starts a Next up from a task with no saved flags with the person's default flags", async () => {
  const task = oldTask(false);
  view({ screen: "review", projectId: task.project_id, taskId: task.task_id, intent: "next_up" }); await tick();
  expect(screen.getByText("Permission prompts: Skip permissions")).toBeTruthy();
  // The flags are folded away; that it won't ask is said in words.
  const fold = screen.getByText("What it’s started with").closest("details")!;
  expect(fold.open).toBe(false);
  expect(fold.textContent).toContain("--dangerously-skip-permissions");
  expect(screen.getAllByRole("note").some((n) => /won’t ask before running commands/.test(n.textContent ?? ""))).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Add to Next up" })); await tick();
  const body = Object.values(app.session.commands.get()).find(c => c.body.command === "create_task")?.body;
  expect(body).toMatchObject({ command: "create_task", harness: task.harness, placement: { mode: "queue" }, launch_policy: "bypass_permissions" });
});
/** The old task, made a Plan that is still running. */
function runningPlan(github?: { from_stranger: boolean }) {
  const old = oldTask(false);
  const host = window.__sim!.host;
  const task = { ...old, mode: "plan" as const, state: "working" as const, launch_policy: undefined,
    ...(github ? { github: { ...old.github!, ...github } } : {}) };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? task : t);
  host.emit({ type: "task_upserted", task });
  return task;
}
it("starts a new task from a running Plan in Work, with the person's flags, and says it is switching", async () => {
  const task = runningPlan();
  view({ screen: "review", projectId: task.project_id, taskId: task.task_id, intent: "next_up" }); await tick();
  expect(screen.getByRole<HTMLInputElement>("radio", { name: /Work/ }).checked).toBe(true);
  // Said right above the button that makes the switch.
  const note = document.querySelector<HTMLElement>(".mode-switch")!;
  expect(note.textContent).toBe("Switching from Plan to Work. This new task can change files, and starts with “Skip permissions”.");
  expect(note.nextElementSibling).toBe(screen.getByRole("button", { name: "Add to Next up" }));
  // Work without prompts is warned of in words.
  expect(screen.getAllByRole("note").map((n) => n.textContent)).toContain("It won’t ask before running commands or changing files. Only use it for work you trust.");
  // Plan can still be chosen, and then nothing says it is switching, or warns.
  fireEvent.click(screen.getByRole("radio", { name: /Plan/ }));
  expect(screen.queryByRole("note")).toBeNull();
  fireEvent.click(screen.getByRole("radio", { name: /Work/ }));
  fireEvent.click(screen.getByRole("button", { name: "Add to Next up" })); await tick();
  const body = Object.values(app.session.commands.get()).find(c => c.body.command === "create_task")?.body;
  expect(body).toMatchObject({ command: "create_task", mode: "work", launch_policy: "bypass_permissions", placement: { mode: "queue" } });
});
it("keeps a Next up from a stranger's work read-only", async () => {
  const task = runningPlan({ from_stranger: true });
  view({ screen: "review", projectId: task.project_id, taskId: task.task_id, intent: "next_up" }); await tick();
  expect(screen.getByRole<HTMLInputElement>("radio", { name: /Plan/ }).checked).toBe(true);
  expect(screen.queryByRole("note")).toBeNull();
});
it("retries the original flags, not edited draft flags, only after confirmation", async () => {
  oldTask(false); app.ui.selectTask(ids.pTide, null);
  const body = createTaskCommand({ projectId: ids.pTide, prompt: "Inspect parser", title: null, agent: { ...astra, launchPolicy: "standard" }, mode: "work", placement: { mode: "start_now" }, mission: null, bundle: null });
  const commandId = uuidv7();
  app.ui.saveDraft(ids.pTide, null, { text: "Inspect parser", agent: { ...astra, launchPolicy: "bypass_permissions" }, attempt: {
    phase: "unknown", at: Date.now(), commandId, historyId: app.session.commandHistory, text: "Inspect parser", detail: "Answer lost", body,
  } });
  window.__sim!.latency(5000); view();
  expect(screen.getByText("The earlier request uses a different permission setting")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Send again" })); await tick();
  expect(app.session.commands.get()[commandId]).toBeUndefined();
  fireEvent.click(screen.getByRole("button", { name: "Keep the original request’s agent" }));
  fireEvent.click(screen.getByRole("button", { name: "Send again" })); await tick();
  expect(app.session.commands.get()[commandId]?.body).toEqual(body);
});
it("refuses sending or replaying launch flags to an old companion before creating a pending request", async () => {
  const caps = app.session.state.get().capabilities!;
  app.session.state.set(s => ({ ...s, capabilities: { ...caps, features: caps.features.filter(f => f !== "launch_policy_v1") } }));
  const body = createTaskCommand({ projectId: ids.pTide, prompt: "Inspect parser", title: null, agent: { ...astra, launchPolicy: "bypass_permissions" }, mode: "work", placement: { mode: "start_now" }, mission: null, bundle: null });
  const before = app.session.commands.get();
  await expect(app.session.command(body)).rejects.toThrow(/on this computer to choose permission prompts/);
  expect(app.session.commands.get()).toEqual(before);
});

it("never replaces an unavailable explicit agent with Claude", () => {
  unavailableCodex();
  const s = app.session.state.get();
  expect(resolveAgent(s.projects[ids.pTide], s.harnesses, astra)).toEqual({ ...astra, launchPolicy: "bypass_permissions" });
});
it("never replaces a missing explicit model or effort with the native default", () => {
  const s = app.session.state.get();
  const chosen = { ...astra, model: "model-not-in-current-catalog", effort: "max" };
  expect(resolveAgent(s.projects[ids.pTide], s.harnesses, chosen)).toEqual({ ...chosen, launchPolicy: "bypass_permissions" });
});
it("never replaces an unavailable saved project agent with another provider", () => {
  unavailableCodex();
  const s = app.session.state.get();
  const project = { ...s.projects[ids.pTide]!, default_harness: astra.harness, default_model: astra.model, default_effort: astra.effort };
  expect(resolveAgent(project, s.harnesses)).toEqual({ ...astra, launchPolicy: "bypass_permissions" });
});
it("blocks Start rather than rewriting a selected agent after readiness changes", async () => {
  app.ui.selectTask(ids.pTide, null);
  app.ui.saveDraft(ids.pTide, null, { text: "Read the git history", agent: astra, mode: "ask" });
  view({ screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" });
  await tick();
  act(unavailableCodex);
  expect(screen.getByRole("button", { name: /Codex · GPT-6-Astra/ })).toBeTruthy();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Start task" }).disabled).toBe(true);
  expect(screen.getByText(/Command helper is missing/)).toBeTruthy();
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "create_task")).toBe(false);
});
it("requires an explicit recipient decision before resuming an old Claude task with a Codex draft", async () => {
  oldTask();
  view(); await tick();
  expect(screen.getByText("This task uses a different agent or model")).toBeTruthy();
  const button = screen.getByRole<HTMLButtonElement>("button", { name: "Continue" });
  expect(button.disabled).toBe(true);
  fireEvent.keyDown(screen.getByRole("textbox"), { key: "Enter", ctrlKey: true });
  await tick();
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "resume_task")).toBe(false);
});
it("carries the message to a reviewed Codex task, never resumes Claude, and keeps the old conversation", async () => {
  const task = oldTask();
  const mounted = view(); await tick();
  fireEvent.click(screen.getByRole("button", { name: "New task with this selection" }));
  await tick();
  expect(app.ui.draft(task.project_id, null)).toMatchObject({ text: "Read the git history", agent: astra });
  const route = app.ui.route();
  expect(route).toMatchObject({ screen: "review", taskId: null });
  mounted.unmount(); view(route); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  const commands = Object.values(app.session.commands.get()).map(c => c.body);
  expect(commands.some(c => c.command === "resume_task")).toBe(false);
  expect(commands.find(c => c.command === "create_task")).toMatchObject({ harness: "codex", model: "gpt-6-astra", effort: "high", prompt: "Read the git history" });
  expect(app.session.state.get().tasks[task.task_id]?.harness).toBe("claude_code");
});
it("honors Codex/Astra chosen through the real picker when returning to an older Claude conversation", async () => {
  const task = oldTask(false);
  app.ui.selectTask(task.project_id, null);
  const mounted = view(); await tick();
  fireEvent.click(screen.getByRole("button", { name: /Claude Code ·/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "Agent for this task" }));
  fireEvent.click(within(sheet.getByRole("group", { name: "Agent" })).getByRole("button", { name: /Codex/ }));
  fireEvent.click(within(sheet.getByRole("group", { name: "Model" })).getByRole("button", { name: /GPT-6-Astra/ }));
  fireEvent.click(within(sheet.getByRole("group", { name: "Effort" })).getByRole("button", { name: "High" }));
  fireEvent.click(sheet.getByRole("button", { name: "Use this agent" })); await tick();
  expect(app.ui.draft(task.project_id, null)?.agent).toEqual({ ...astra, launchPolicy: "bypass_permissions" });
  // Persisted selection also survives reconstructing the UI store.
  expect(new UiStore(app.profile.profileId).draft(task.project_id, null)?.agent).toEqual({ ...astra, launchPolicy: "bypass_permissions" });
  act(() => app.ui.selectTask(task.project_id, task.task_id)); await tick();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Continue" }).disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "New task with this selection" })); await tick();
  const route = app.ui.route(); mounted.unmount(); view(route); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  const commands = Object.values(app.session.commands.get()).map(c => c.body);
  expect(commands.some(c => c.command === "resume_task")).toBe(false);
  expect(commands.find(c => c.command === "create_task")).toMatchObject({ ...astra, agent_chosen: true, prompt: "Read the git history" });
});
it("requires a new acknowledgement when the explicit selection changes", async () => {
  oldTask(); view(); await tick();
  fireEvent.click(screen.getByRole("button", { name: /Keep this task’s agent/ }));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Continue" }).disabled).toBe(false);
  act(() => app.ui.saveDraft(ids.pTide, null, { agent: { ...astra, effort: "medium" } }));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Continue" }).disabled).toBe(true);
});
it.each([
  ["missing harness", "selected agent is no longer reported", "harness"],
  ["missing model", "selected model", "model"],
  ["missing effort", "selected effort", "effort"],
  ["signed-out account", "Sign in again", "account"],
] as const)("blocks an explicit %s in Review without changing the selection", async (_, message, change) => {
  app.ui.selectTask(ids.pTide, null);
  app.ui.saveDraft(ids.pTide, null, { text: "Read the git history", agent: astra, mode: "ask" });
  view({ screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" }); await tick();
  act(() => window.__sim!.host.emit({ type: "harnesses_changed", harnesses: app.session.state.get().harnesses
    .filter(h => change !== "harness" || h.kind !== "codex")
    .map(h => h.kind !== "codex" ? h : change === "model" ? { ...h, models: [] }
      : change === "effort" ? { ...h, capabilities: { ...h.capabilities, effort_selection: { state: "unsupported", reason: "Effort selection unavailable" } } }
      : change === "account" ? { ...h, account: { state: "signed_out", remedy: "Sign in again" } } : h) }));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Start task" }).disabled).toBe(true);
  expect(screen.getAllByText(new RegExp(message)).length).toBeGreaterThan(0);
  expect(app.ui.draft(ids.pTide, null)?.agent).toEqual(astra);
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "create_task")).toBe(false);
});
it.each(["talk", "review"] as const)("does not replay an earlier Claude start under a new Codex label in %s", async place => {
  oldTask(false); app.ui.selectTask(ids.pTide, null);
  const body = createTaskCommand({ projectId: ids.pTide, prompt: "Read the git history", title: null,
    agent: { harness: "claude_code", model: "claude-opus-5-5", effort: "high" }, mode: "ask", placement: { mode: "start_now" }, mission: null, bundle: null });
  const commandId = uuidv7();
  app.ui.saveDraft(ids.pTide, null, { text: "Read the git history", agent: astra, mode: "ask", attempt: {
    phase: "unknown", at: Date.now(), commandId, historyId: app.session.commandHistory, text: "Read the git history", detail: "The connection closed before the answer.", body,
  } });
  window.__sim!.latency(5000);
  view(place === "talk" ? { screen: "talk" } : { screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" });
  // Deliberately unresolved, but safe to replay under the same id; only the
  // recipient changed. The receipt checker must not manufacture a new send.
  fireEvent.click(screen.getByRole("button", { name: "Send again" })); await tick();
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "create_task")).toBe(false);
  expect(screen.getByText("The earlier request uses a different agent or model")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /Keep the original request’s agent/ }));
  fireEvent.click(screen.getByRole("button", { name: "Send again" })); await tick();
  expect(app.session.commands.get()[commandId]?.body).toEqual(body);
});
it.each(["talk", "review"] as const)("uses the saved command’s recipient, not the current intent, for retries in %s", async place => {
  const task = oldTask();
  const body = createTaskCommand({ projectId: ids.pTide, prompt: "Read the git history", title: null,
    agent: astra, mode: "ask", placement: { mode: "queue", position: null, authorization: "ask_before_start" }, mission: null, bundle: null });
  const commandId = uuidv7();
  app.ui.saveDraft(ids.pTide, task.task_id, { agent: astra, attempt: {
    phase: "unknown", at: Date.now(), commandId, historyId: app.session.commandHistory, text: "Read the git history", detail: "The answer was lost.", body,
  } });
  window.__sim!.latency(5000);
  // Talk/Continue now names the old Claude conversation, but the pending
  // request really creates a separate Codex task. It must not ask to keep Claude.
  view(place === "talk" ? { screen: "talk" } : { screen: "review", projectId: ids.pTide, taskId: task.task_id, intent: "continue" });
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Send again" }).disabled).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Send again" })); await tick();
  expect(app.session.commands.get()[commandId]?.body).toEqual(body);
});
it("does not replay a Claude continuation hidden behind a new Codex task review", async () => {
  const task = oldTask();
  const body = { command: "resume_task" as const, task_id: task.task_id, text: "Read the git history" };
  const commandId = uuidv7();
  app.ui.saveDraft(ids.pTide, task.task_id, { agent: astra, attempt: {
    phase: "unknown", at: Date.now(), commandId, historyId: app.session.commandHistory, text: body.text, detail: "The answer was lost.", body, expectedRevision: task.revision,
  } });
  window.__sim!.latency(5000);
  view({ screen: "review", projectId: ids.pTide, taskId: task.task_id, intent: "next_up" });
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Send again" }).disabled).toBe(true);
  expect(screen.getByText("The earlier request uses a different agent or model")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Add to Next up" })); await tick();
  expect(app.session.commands.get()[commandId]).toBeUndefined();
  fireEvent.click(screen.getByRole("button", { name: "Keep the original request’s agent" }));
  fireEvent.click(screen.getByRole("button", { name: "Send again" })); await tick();
  expect(app.session.commands.get()[commandId]?.body).toEqual(body);
});
it("also fences a continuation opened directly in Review", async () => {
  const task = oldTask();
  view({ screen: "review", projectId: task.project_id, taskId: task.task_id, intent: "continue" });
  await tick();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Continue" }).disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: /Keep this task’s agent/ }));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Continue" }).disabled).toBe(false);
});
