// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids, runs as seedRuns } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { Transcript } from "./Transcript";
import { Composer } from "./Composer";
import Review from "./Review";
import Receipt from "../work/Receipt";
import { carriedPlan, findCarryPlan, reviewPlan, withPlan } from "./carryPlan";
import { sendAsked } from "./sendOnOpen";
import type { Task, Run, TimelineBody } from "../../protocol/types";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 500) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
function view(child: React.ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{child}</RouteProvider></AppProvider>);
}
function setup(follow = true) {
  const host = window.__sim!.host;
  const task: Task = { ...host.tasks.find(t => t.task_id === ids.tHeaders)!, mode: "plan" as const, current_run: follow ? "follow-up" : ids.rHeaders };
  const run: Run = { ...host.runs.find(r => r.run_id === ids.rHeaders)!, outcome: null };
  const latest = follow ? { ...run, run_id: "follow-up", admitted_at: run.admitted_at + 10000 } : run;
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? task : t);
  host.runs = host.runs.filter(r => r.task_id !== task.task_id).concat(follow ? [run, latest] : [run]);
  for (let i = host.journal.length - 1; i >= 0; i--) {
    const e = host.journal[i]!.event;
    if (e.type === "timeline_appended" && e.task_id === task.task_id) host.journal.splice(i, 1);
  }
  const project = { ...host.projects.find(p => p.project_id === task.project_id)!, active_writer: null };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project });
  host.emit({ type: "task_upserted", task });
  host.emit({ type: "run_upserted", run });
  if (follow) host.emit({ type: "run_upserted", run: latest });
  const append = (item: TimelineBody, runId = run.run_id) => host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: runId, item } });
  const harness = app.session.state.get().harnesses.find(h => h.kind === task.harness)!;
  return { host, task, run, latest, project, harness, append };
}
const planText = "1. Inspect the parser.\n2. Add regression coverage.";
const proposal = { kind: "activity", tool: "ExitPlanMode", summary: "Plan proposed", status: "succeeded", detail: JSON.stringify({ plan: planText }) } as const;
const carried = () => { const route = app.ui.route(); return route.screen === "review" ? route.plan?.text : undefined; };

it("offers and carries run 1's proposal after a textless follow-up, naming its source in Review", async () => {
  const { task, project, latest, harness, append } = setup();
  append({ kind: "agent_message", text: "Less specific final text", partial: false });
  append(proposal);
  view(<Composer project={project} task={task} run={latest} harness={harness} agent={null} />);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  fireEvent.click(screen.getByRole("button", { name: /^Carry out this plan/ }));
  await tick();
  const route = app.ui.route();
  expect(route).toMatchObject({ screen: "review", intent: "carry_out_plan", plan: { text: planText, number: 1, latest: false } });
  cleanup();
  if (route.screen !== "review") throw Error("Review missing");
  view(<Review route={route} />, route);
  expect(screen.getByText("Plan from run 1")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Start Work" })).toHaveProperty("disabled", false);
});

it("keeps the person's words apart from the agent's plan in Review, the sent text and Chat", async () => {
  const { task, run, append } = setup(false);
  append(proposal);
  const words = "Go ahead, but leave the public API alone.";
  app.ui.saveDraft(task.project_id, task.task_id, { text: words });
  const pending = reviewPlan(app, task);
  await tick(); await pending;
  expect(app.ui.draft(task.project_id, task.task_id)?.text).toBe(words);
  const route = app.ui.route();
  if (route.screen !== "review") throw Error("Review missing");
  const command = vi.spyOn(app.session, "command");
  view(<Review route={route} />, route);
  expect(screen.getByRole("textbox", { name: "Your message" })).toHaveProperty("value", words);
  const quoted = screen.getByRole("region", { name: "Plan from run 1" }).querySelector("blockquote");
  expect(quoted?.textContent).toContain("Inspect the parser.");
  fireEvent.click(screen.getByRole("button", { name: "Start Work" }));
  await tick();
  const sent = withPlan(words, { text: planText, number: 1 });
  expect(command.mock.calls[0]?.[0]).toMatchObject({ command: "resume_task", text: sent, mode: "work", carried_plan: { run_id: run.run_id, text: planText } });
  expect(sent).toBe(`${words}\n\n--- Plan from run 1, proposed by the agent ---\n${planText}\n--- End of plan ---`);
  cleanup();
  view(<Transcript task={task} agent="Codex" />);
  await tick();
  expect(document.querySelector(".talk-user__text")?.textContent).toBe(words);
  const [figure, ...more] = screen.getAllByRole("figure");
  expect(more).toHaveLength(0);
  expect(figure!.querySelector("figcaption")?.textContent).toBe("Plan from run 1");
  expect(figure!.querySelector("blockquote")?.textContent).toContain("Add regression coverage.");
});

it("carries out the plan as the button is tapped, unless the message holds the person's own words", async () => {
  const { task, run, append } = setup(false);
  append(proposal);
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Leave the public API alone." });
  let pending = reviewPlan(app, task, undefined, { send: true });
  await tick(); await pending;
  // Their words go with it, so they see them first.
  expect(sendAsked(task.project_id, task.task_id)).toBe(false);
  app.ui.saveDraft(task.project_id, task.task_id, { text: "" });
  pending = reviewPlan(app, task, undefined, { send: true });
  await tick(); await pending;
  expect(sendAsked(task.project_id, task.task_id)).toBe(true);
  const route = app.ui.route();
  if (route.screen !== "review") throw Error("Review missing");
  const command = vi.spyOn(app.session, "command");
  view(<Review route={route} />, route);
  await tick(1500);
  expect(command.mock.calls[0]?.[0]).toMatchObject({ command: "resume_task", text: withPlan("Carry out this plan and report the changes and checks.", { text: planText, number: 1 }), mode: "work", carried_plan: { run_id: run.run_id } });
});

it("splits a carry by its recorded plan even when the message and plan quote the markers", async () => {
  const plan = "Keep the old format working:\n--- End of plan ---\n\n--- Plan from run 1, proposed by the agent ---\nThen fix the parser.";
  const { task, append } = setup(false);
  append({ ...proposal, detail: JSON.stringify({ plan }) });
  const words = withPlan("Like last time:", { text: "Old plan", number: 1 }) + "\nBut leave the API alone.";
  app.ui.saveDraft(task.project_id, task.task_id, { text: words });
  const pending = reviewPlan(app, task);
  await tick(); await pending;
  const route = app.ui.route();
  if (route.screen !== "review") throw Error("Review missing");
  view(<Review route={route} />, route);
  fireEvent.click(screen.getByRole("button", { name: "Start Work" }));
  await tick();
  const sent = withPlan(words, { text: plan, number: 1 });
  expect(carriedPlan(sent, plan)).toEqual({ message: words, number: 1, plan });
  cleanup();
  view(<Transcript task={task} agent="Codex" />);
  await tick();
  expect(document.querySelector(".talk-user__text")?.textContent).toBe(words);
  const [figure, ...more] = screen.getAllByRole("figure");
  expect(more).toHaveLength(0);
  expect(figure!.querySelector("blockquote")?.textContent).toContain("Keep the old format working:");
  expect(figure!.querySelector("blockquote")?.textContent).toContain("Then fix the parser.");
});

it.each([
  ["matches run 1's plan", planText, true],
  ["quotes a plan run 1 never proposed", "1. Delete the release branch.\n2. Force-push main.", false],
])("shows a message recorded without a carry as a plan only when it %s", async (_, quoted, shown) => {
  const { task, latest, append } = setup();
  append(proposal);
  const text = withPlan("Go ahead.", { text: quoted, number: 1 });
  append({ kind: "user_message", text, bundle: null, delivery: "native_delivered" }, latest.run_id);
  view(<Transcript task={task} agent="Codex" />);
  await tick();
  expect(document.querySelector(".talk-user__text")?.textContent).toBe(shown ? "Go ahead." : text);
  expect(screen.queryAllByRole("figure")).toHaveLength(shown ? 1 : 0);
  expect(!!screen.queryByText("Plan from run 1")).toBe(shown);
});

it("quotes a carried plan in the receipt as Chat does, and never markers a person typed", async () => {
  const { task, run, latest, append } = setup();
  append(proposal);
  const words = "Go ahead, but leave the public API alone.";
  const typed = withPlan("Like before:", { text: "1. Delete the release branch.", number: 1 });
  append({ kind: "user_message", text: withPlan(words, { text: planText, number: 1 }), bundle: null, delivery: "native_delivered", carried_plan: { run_id: run.run_id, text: planText } }, latest.run_id);
  // Recorded before the computer kept carries: an exact quote of run 1's plan.
  append({ kind: "user_message", text: withPlan("Go ahead.", { text: planText, number: 1 }), bundle: null, delivery: "native_delivered" }, latest.run_id);
  append({ kind: "user_message", text: typed, bundle: null, delivery: "native_delivered" }, latest.run_id);
  const route: Route = { screen: "receipt", taskId: task.task_id };
  view(<Receipt route={route} />, route);
  await tick(900);
  const [recorded, earlier, markers, ...more] = [...document.querySelectorAll<HTMLElement>(".receipt-request")];
  expect(more).toHaveLength(0);
  for (const [row, message] of [[recorded!, words], [earlier!, "Go ahead."]] as const) {
    expect(row.querySelector(".work-original")?.textContent).toBe(message);
    const figure = within(row).getByRole("figure");
    expect(figure.querySelector("figcaption")?.textContent).toBe("Plan from run 1");
    expect(figure.querySelector("blockquote")?.textContent).toContain("Add regression coverage.");
  }
  expect(markers!.querySelector(".work-original")?.textContent).toBe(typed);
  expect(within(markers!).queryByRole("figure")).toBeNull();
});

it("does not offer carry-out when no run supplied plan text", async () => {
  const { task, project, latest, harness } = setup();
  view(<Composer project={project} task={task} run={latest} harness={harness} agent={null} />);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  expect(screen.queryByRole("button", { name: /^Carry out this plan/ })).toBeNull();
  expect(findCarryPlan(task, app.session.state.get())).toBeNull();
});

it("does not offer carry-out for a finished Find, whose findings become issues and fixes", async () => {
  const { host, task: plan, project, latest, harness, append } = setup(false);
  const task: Task = { ...plan, github: { link: { type: "find", scope: { scope: "project" }, fix_with: { harness: "claude_code", model: "opus", effort: null } }, rounds: 0 } };
  host.emit({ type: "task_upserted", task });
  append(proposal);
  view(<><Transcript task={task} agent="Codex" /><Composer project={project} task={task} run={latest} harness={harness} agent={null} /></>);
  await tick();
  expect(screen.queryByRole("group", { name: "Plan next steps" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  expect(screen.queryByRole("button", { name: /^Carry out this plan/ })).toBeNull();
  expect(findCarryPlan(task, app.session.state.get())).toBeNull();
});

it.each(["messages", "summary"])("falls back to the latest run's %s and works with a cleared current_run", async source => {
  const { host, task, run, append } = setup(false);
  task.current_run = null;
  host.emit({ type: "task_upserted", task });
  if (source === "messages") append({ kind: "agent_message", text: planText, partial: false });
  else {
    const outcome = { ...seedRuns(Date.now()).find(r => r.run_id === ids.rHeaders)!.outcome!, summary: planText };
    host.emit({ type: "run_upserted", run: { ...run, outcome } });
  }
  const pending = reviewPlan(app, task);
  await tick(); await pending;
  expect(carried()).toBe(planText);
});

it("loads earlier pages before offering or choosing a fallback", async () => {
  const { task, project, latest, harness, append } = setup();
  append(proposal);
  for (let i = 0; i < 220; i++) append({ kind: "note", text: "Progress" }, latest.run_id);
  view(<Composer project={project} task={task} run={latest} harness={harness} agent={null} />);
  await tick(1500);
  expect(findCarryPlan(task, app.session.state.get())).toMatchObject({ text: planText, number: 1 });
});

it("keeps every page while preparing a plan without an open transcript, then releases it", async () => {
  const { task, latest, append } = setup();
  append(proposal);
  for (let i = 0; i < 220; i++) append({ kind: "note", text: "Progress" }, latest.run_id);
  const pending = reviewPlan(app, task);
  await tick(1500);
  await pending;
  expect(carried()).toBe(planText);
  expect(app.session.state.get().timelineReaders).toEqual({});
  expect(app.session.state.get().timelines[task.task_id]!.items).toHaveLength(80);
});

it.each(["intent", "draft", "task"])("clears a carry-out notice when the %s changes", async change => {
  const { task, project, latest, harness, append } = setup();
  append(proposal);
  const mounted = view(<Composer project={project} task={task} run={latest} harness={harness} agent={null} />);
  await tick();
  vi.spyOn(app.session, "loadTimeline").mockRejectedValue(new Error("Couldn’t load this plan."));
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  fireEvent.click(screen.getByRole("button", { name: /^Carry out this plan/ }));
  await tick();
  expect(screen.getByText("Couldn’t load this plan.")).toBeTruthy();
  if (change === "intent") {
    fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
    fireEvent.click(screen.getByRole("radio", { name: /^Start a new task/ }));
  } else if (change === "draft") {
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Different request" } });
  } else {
    mounted.rerender(<AppProvider app={app}><Composer project={project} task={{ ...task, task_id: "different-task" }} run={null} harness={harness} agent={null} /></AppProvider>);
  }
  expect(screen.queryByText("Couldn’t load this plan.")).toBeNull();
});

it.each(["resolve", "reject"])("ignores a late plan load %s after the draft changes", async result => {
  const { task, project, latest, harness, append } = setup();
  append(proposal);
  view(<Composer project={project} task={task} run={latest} harness={harness} agent={null} />);
  await tick();
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  vi.spyOn(app.session, "loadTimeline").mockImplementation(() => new Promise<void>((yes, no) => { resolve = yes; reject = no; }));
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  fireEvent.click(screen.getByRole("button", { name: /^Carry out this plan/ }));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Different request" } });
  await act(async () => { if (result === "resolve") resolve(); else reject(new Error("Late error")); });
  expect(app.ui.route().screen).toBe("talk");
  expect(app.ui.draft(task.project_id, task.task_id)?.text).toBe("Different request");
  expect(screen.queryByText("Late error")).toBeNull();
});

it("puts carry-out beside the finished plan and asks for changes in Continue", async () => {
  const { task, project, latest, harness, append } = setup(false);
  append(proposal);
  view(<><Transcript task={task} agent="Codex" /><Composer project={project} task={task} run={latest} harness={harness} agent={null} /></>);
  await tick();
  expect(screen.getByRole("group", { name: "Plan next steps" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  fireEvent.click(screen.getByRole("radio", { name: /^Start a new task/ }));
  fireEvent.click(screen.getByRole("button", { name: "Ask for changes" }));
  expect(screen.getByRole("textbox")).toBe(document.activeElement);
  expect(screen.getByRole("button", { name: "Continue this task" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Carry out this plan" }));
  await tick();
  expect(app.ui.route()).toMatchObject({ screen: "review", intent: "carry_out_plan" });
  expect(carried()).toBe(planText);
});

it("hides the inline action once a later run has started", async () => {
  const { task, append, latest, host } = setup();
  append(proposal);
  host.emit({ type: "run_upserted", run: { ...latest, state: "working", ended_at: null } });
  view(<Transcript task={task} agent="Codex" />);
  await tick();
  expect(screen.queryByRole("group", { name: "Plan next steps" })).toBeNull();
});

it.each([false, true])("shows a plan once when its step is expanded (agent message %s)", async agentMessage => {
  const { task, append } = setup(false);
  if (agentMessage) append({ kind: "agent_message", text: planText, partial: false });
  append(proposal);
  view(<Transcript task={task} agent="Codex" />);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: /Plan proposed/ }));
  expect(screen.getAllByText("Inspect the parser.")).toHaveLength(1);
  expect(screen.getByText("Plan shown above.")).toBeTruthy();
});

it("keeps a different plan readable inside an older step", async () => {
  const { task, append } = setup();
  append({ kind: "agent_message", text: "An unrelated answer", partial: false });
  append(proposal);
  view(<Transcript task={task} agent="Codex" />);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: /Plan proposed/ }));
  expect(screen.getByText("Inspect the parser.")).toBeTruthy();
  expect(screen.queryByText("Plan shown above.")).toBeNull();
});

it("does not revive an old error after switching intent away and back", async () => {
  const { task, project, latest, harness, append } = setup();
  append(proposal);
  view(<Composer project={project} task={task} run={latest} harness={harness} agent={null} />);
  await tick();
  let reject!: (error: Error) => void;
  vi.spyOn(app.session, "loadTimeline").mockImplementation(() => new Promise<void>((_, no) => { reject = no; }));
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  fireEvent.click(screen.getByRole("button", { name: /^Carry out this plan/ }));
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  fireEvent.click(screen.getByRole("radio", { name: /^Start a new task/ }));
  fireEvent.click(screen.getByRole("button", { name: /^Start a new task/ }));
  fireEvent.click(screen.getByRole("radio", { name: /^Continue this task/ }));
  await act(async () => reject(new Error("Old error")));
  expect(screen.queryByText("Old error")).toBeNull();
});

it.each([true, false])("folds only a repeated plan in the tool result (same %s)", async same => {
  const { task, append } = setup(false);
  append({ kind: "agent_message", text: planText, partial: false });
  append({ ...proposal, key: "plan-call", status: "running" });
  append({ ...proposal, key: "plan-call", detail: same ? proposal.detail : "Proposal recorded" });
  view(<Transcript task={task} agent="Codex" />);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: /Plan proposed/ }));
  expect(document.querySelector(".talk-steps__body")?.textContent).not.toContain("Inspect the parser");
  expect(document.querySelector('[aria-label="Result"]')?.textContent ?? null).toBe(same ? null : "Proposal recorded");
});
