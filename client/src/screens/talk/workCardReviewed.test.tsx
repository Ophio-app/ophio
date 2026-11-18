// SPDX-License-Identifier: Apache-2.0
//
// A card opened in Review and started there is still that card's task, and a
// fix the computer started as a plan says so on its card.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { CommandError, HostSession, OutcomeUnknownError } from "../../host/session";
import type { Task } from "../../protocol/types";
import { Toasts } from "../../design/Toast";
import { RouteProvider } from "../../shell/Screen";
import Review from "./Review";
import Talk from "./Talk";
import { planned } from "./WorkCard";
import { sendOnOpen } from "./sendOnOpen";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  window.history.replaceState(null, "", "/?cards=1");
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => {
  cleanup(); app.ui.retire(); app.session.stop(); window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});
const tick = async (ms = 300) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const host = () => window.__sim!.host;
const view = (route: Route = { screen: "talk" }) => render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: route.screen === "talk" ? 0 : 1, active: true, route }}>
  {route.screen === "review" ? <Review route={route} /> : <Talk route={{ screen: "talk" }} />}</RouteProvider><Toasts /></AppProvider>);
const openChat = async (chat: string) => {
  app.ui.selectProject(ids.pTide);
  app.ui.selectTask(ids.pTide, chat);
  view(); await tick(600);
};

it("keeps a card started from Review as that card's task: started once, linked to its chat", async () => {
  await openChat(ids.tOneCard);
  fireEvent.click(screen.getByRole("button", { name: /^Agent for this task:/ }));
  fireEvent.click(within(screen.getByRole("dialog", { name: "Agent for this task" })).getByRole("button", { name: /More options/ }));
  await tick();
  const origin = { chat: ids.tOneCard, run: ids.rOneCard, index: 0, reviewed: true };
  expect(app.ui.draft(ids.pTide, null)?.review?.origin).toEqual(origin);
  cleanup();
  view(app.ui.route()); await tick(600);
  const command = vi.spyOn(app.session, "command");
  // Its primary action, as the person sees it: the card's work goes to Next up.
  fireEvent.click(screen.getAllByRole("button", { name: "Add to Next up" })[0]!); await tick(1500);
  const made = command.mock.calls.map((c) => c[0]).find((b) => b.command === "create_task");
  expect(made).toMatchObject({ origin });
  const task = Object.values(app.session.state.get().tasks).find((t) => t.origin?.chat === ids.tOneCard);
  expect(task?.origin).toEqual(origin);
  // Back in the chat the card is that task, with nothing left to start.
  cleanup();
  await openChat(ids.tOneCard);
  const card = document.querySelector<HTMLElement>(".work-cards")!;
  expect(within(card).queryByRole("button", { name: "Start" })).toBeNull();
  expect(within(card).getByText(task!.title)).toBeTruthy();
});

const setCard = () => screen.getByRole("button", { name: /^2 tasks, in order/ }).closest<HTMLElement>(".work-cards")!;
const fromSet = () => Object.values(app.session.state.get().tasks).filter((t) => t.origin?.chat === ids.tSet).sort((a, b) => a.origin!.index - b.origin!.index);
const orderedMerge = async () => {
  host().runs.find((r) => r.run_id === ids.rSet)!.outcome!.report!.new_work = host().runs.find((r) => r.run_id === ids.rSet)!.outcome!.report!.new_work.slice(0, 2);
  const settings = host().github.settings.get(ids.pTide)!;
  host().github.settings.set(ids.pTide, { ...settings, mode: "through_merge", warning_answered: false });
  await openChat(ids.tSet);
};
const openSetReview = async () => {
  fireEvent.click(within(setCard()).getByRole("button", { name: "Review tasks" }));
  await tick();
  cleanup(); view(app.ui.route()); await tick(600);
};
const createButton = () => within(document.querySelector<HTMLElement>(".review-foot")!).getByRole<HTMLButtonElement>("button", { name: "Add to Next up" });
const backToCard = async () => { cleanup(); view(); await tick(600); };

it("offers Review tasks on an ordered proposal that requires merge confirmation", async () => {
  await orderedMerge();
  const card = screen.getByRole("button", { name: /^2 tasks, in order/ }).closest<HTMLElement>(".work-cards")!;
  expect(within(card).getByText("This project merges by itself. Confirm that in Review first.")).toBeTruthy();
  fireEvent.click(within(card).getByRole("button", { name: "Review tasks" }));
  await tick();
  expect(app.ui.route()).toMatchObject({ screen: "review", projectId: ids.pTide, taskId: null });
  expect(app.ui.draft(ids.pTide, null)?.review).toMatchObject({
    origin: { chat: ids.tSet, run: ids.rSet, index: 0, reviewed: true },
    placement: "queue",
  });
});

it("confirms merging in Review, creates the first real Task, then starts the dependent rest without a chat turn", async () => {
  await orderedMerge();
  const command = vi.spyOn(app.session, "command");
  await openSetReview();
  const agent = app.ui.draft(ids.pTide, null)!.agent!;
  expect(screen.getByRole("heading", { name: "Review task 1 of 2" })).toBeTruthy();
  expect(screen.getByText("Before it merges by itself")).toBeTruthy();
  expect(createButton().disabled).toBe(true);
  fireEvent.click(createButton()); await tick();
  expect(fromSet()).toHaveLength(0);
  expect(command.mock.calls).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Merge once checks pass" })); await tick(900);
  expect(createButton().disabled).toBe(false);
  act(() => { fireEvent.click(createButton()); fireEvent.click(createButton()); });
  await tick(1500);
  expect(fromSet()).toHaveLength(1);
  expect(app.ui.route()).toEqual({ screen: "talk" });
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(ids.tSet);
  const first = fromSet()[0]!;
  expect(first.origin).toEqual({ chat: ids.tSet, run: ids.rSet, index: 0, reviewed: true });
  await backToCard();
  const card = setCard();
  fireEvent.click(within(card).getByRole("button", { name: /^2 tasks, in order/ })); await tick();
  const live = within(card).getByRole("button", { name: new RegExp(`^${first.title}`) });
  expect(live.textContent).toMatch(/Next up/);
  expect(live.textContent).not.toMatch(/Started|Working/);
  const rest = within(card).getByRole("button", { name: "Start the rest" });
  act(() => { fireEvent.click(rest); fireEvent.click(rest); });
  await tick(1500);
  const tasks = fromSet();
  expect(tasks).toHaveLength(2);
  const items = app.session.state.get().queues[ids.pTide]!.items;
  const firstItem = items.find((i) => i.task_id === first.task_id)!;
  expect(items.find((i) => i.task_id === tasks[1]!.task_id)?.depends_on).toEqual([firstItem.item_id]);
  const creates = command.mock.calls.map((c) => c[0]).filter((b) => b.command === "create_task");
  expect(creates).toHaveLength(2);
  expect(creates[0]).toMatchObject({ harness: agent.harness, model: agent.model, effort: agent.effort,
    launch_policy: agent.launchPolicy, github: { type: "new_pr" }, placement: { mode: "queue" } });
  expect(creates[1]).toMatchObject({ origin: { chat: ids.tSet, run: ids.rSet, index: 1 } });
  expect(command.mock.calls.some(([b]) => b.command === "resume_task" || b.command === "send_message")).toBe(false);
  fireEvent.click(live); await tick();
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(first.task_id);
});

it("cancels Review without creating anything, and keeps edited text and agent settings when reopened", async () => {
  await orderedMerge(); await openSetReview();
  const command = vi.spyOn(app.session, "command");
  const text = "Keep these explicit review edits.";
  const agent = { harness: "codex" as const, model: null, effort: null, launchPolicy: "accept_edits" as const };
  act(() => app.ui.saveDraft(ids.pTide, null, { text, agent }));
  act(() => app.ui.back());
  await backToCard();
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(ids.tSet);
  expect(fromSet()).toHaveLength(0);
  fireEvent.click(within(setCard()).getByRole("button", { name: "Review tasks" })); await tick();
  expect(app.ui.draft(ids.pTide, null)).toMatchObject({ text, agent,
    review: { origin: { index: 0 }, ordered: { step: 1, total: 2 } } });
  expect(command.mock.calls).toHaveLength(0);
});

it("reviews the next task when a later step still needs its own changes, retaining order and reviewed origins", async () => {
  await orderedMerge();
  act(() => {
    const run = host().runs.find((r) => r.run_id === ids.rSet)!;
    run.outcome!.report!.new_work[1]!.refused = "This needs your corrected instructions.";
    host().emit({ type: "run_upserted", run: { ...run } });
  });
  await openSetReview();
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  fireEvent.click(createButton()); await tick(1500);
  await backToCard();
  fireEvent.click(within(setCard()).getByRole("button", { name: "Review next task" })); await tick();
  expect(app.ui.draft(ids.pTide, null)?.review).toMatchObject({ origin: { index: 1, reviewed: true },
    ordered: { step: 2, total: 2, after: fromSet()[0]!.title }, placement: "queue" });
  cleanup(); view(app.ui.route()); await tick(600);
  const text = "Use the corrected instructions for the second task.";
  fireEvent.change(screen.getByRole("textbox", { name: "What you’re asking" }), { target: { value: text } });
  fireEvent.click(createButton()); await tick(1500);
  const tasks = fromSet();
  expect(tasks.map((t) => t.origin)).toEqual([0, 1].map((index) => ({ chat: ids.tSet, run: ids.rSet, index, reviewed: true })));
  const items = app.session.state.get().queues[ids.pTide]!.items;
  expect(items.find((i) => i.task_id === tasks[1]!.task_id)?.depends_on).toEqual([items.find((i) => i.task_id === tasks[0]!.task_id)!.item_id]);
  expect(app.ui.route()).toEqual({ screen: "talk" });
});

it("never resumes paused Next up when creating a reviewed task or starting the rest", async () => {
  await orderedMerge();
  act(() => {
    const q = { ...app.session.state.get().queues[ids.pTide]!, paused: true };
    host().talk.put(q);
  });
  await openSetReview();
  expect(app.ui.draft(ids.pTide, null)?.review?.authorization).toBe("ask_before_start");
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  const command = vi.spyOn(app.session, "command");
  expect(screen.getByRole<HTMLButtonElement>("radio", { name: "Start by itself" }).disabled).toBe(true);
  fireEvent.click(createButton()); await tick(1500);
  await backToCard();
  fireEvent.click(within(setCard()).getByRole("button", { name: "Start the rest" })); await tick(1500);
  const creates = command.mock.calls.map((c) => c[0]).filter((b) => b.command === "create_task");
  expect(creates).toHaveLength(2);
  for (const body of creates) expect(body.placement).toMatchObject({ mode: "queue", authorization: "ask_before_start" });
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(true);
});

it("keeps a refused reviewed step ready to retry, without starting the later step", async () => {
  await orderedMerge(); await openSetReview();
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  const command = vi.spyOn(app.session, "command").mockRejectedValueOnce(new CommandError({ code: "permission_denied", message: "Task creation was refused." }, null));
  fireEvent.click(createButton()); await tick(600);
  expect(fromSet()).toHaveLength(0);
  expect(app.ui.draft(ids.pTide, null)?.attempt?.phase).toBe("rejected");
  expect(app.ui.route()).toMatchObject({ screen: "review" });
  fireEvent.click(createButton()); await tick(1500);
  expect(fromSet()).toHaveLength(1);
  expect(command.mock.calls.filter(([b]) => b.command === "create_task").map(([b]) => b.command === "create_task" ? b.origin?.index : null)).toEqual([0, 0]);
  await backToCard();
  expect(within(setCard()).getByRole("button", { name: "Start the rest" })).toBeTruthy();
});

it("recovers a lost reviewed creation receipt after reconnect and reload without creating the task twice", async () => {
  await orderedMerge(); await openSetReview();
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  const command = vi.spyOn(app.session, "command");
  window.__sim!.dropNextReceipt();
  fireEvent.click(createButton()); await tick(300);
  expect(app.ui.draft(ids.pTide, null)?.attempt?.phase).toBe("unknown");
  expect(fromSet()).toHaveLength(0);
  expect(host().tasks.filter((t) => t.origin?.chat === ids.tSet)).toHaveLength(1);
  act(() => window.__sim!.offline());
  app.ui.persistNow();
  cleanup(); app.ui.retire(); app.ui = new UiStore(app.profile.profileId);
  view(app.ui.route()); await tick(300);
  expect(app.ui.draft(ids.pTide, null)?.review?.ordered).toMatchObject({ step: 1, total: 2 });
  act(() => window.__sim!.online()); await tick(1800);
  expect(fromSet()).toHaveLength(1);
  expect(app.ui.draft(ids.pTide, null)).toBeNull();
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(ids.tSet);
  expect(app.ui.route()).toEqual({ screen: "talk" });
  expect(command.mock.calls.filter(([b]) => b.command === "create_task")).toHaveLength(1);
  await backToCard();
  expect(within(setCard()).getByRole("button", { name: "Start the rest" })).toBeTruthy();
});

it("checks an unresolved ordered creation before repeating an earlier auto-start choice into paused Next up", async () => {
  await orderedMerge(); await openSetReview();
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  // Keep the first request unresolved: the computer has not answered its status query yet.
  vi.spyOn(app.session, "query").mockImplementationOnce(() => new Promise<never>(() => {}));
  const command = vi.spyOn(app.session, "command").mockRejectedValueOnce(new OutcomeUnknownError("unanswered"));
  fireEvent.click(createButton()); await tick(300);
  const before = app.ui.draft(ids.pTide, null)!.attempt!;
  expect(before.phase).toBe("unknown");
  expect(before.body).toMatchObject({ placement: { mode: "queue", authorization: "auto_start" } });
  act(() => host().talk.put({ ...app.session.state.get().queues[ids.pTide]!, paused: true }));
  await tick();
  expect(createButton().disabled).toBe(true);
  const repeat = screen.getByRole<HTMLButtonElement>("button", { name: "Send again" });
  expect(repeat.disabled).toBe(true);
  fireEvent.click(createButton()); fireEvent.click(repeat); await tick();
  expect(command.mock.calls).toHaveLength(1);
  expect(app.ui.draft(ids.pTide, null)?.attempt?.commandId).toBe(before.commandId);
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(true);
  expect(fromSet()).toHaveLength(0);
  const check = screen.getByRole<HTMLButtonElement>("button", { name: "Check" });
  expect(check.disabled).toBe(false);
  fireEvent.click(check); await tick(600);
  expect(app.ui.draft(ids.pTide, null)?.attempt?.phase).toBe("not_delivered");
  fireEvent.click(createButton()); await tick(1500);
  expect(fromSet()).toHaveLength(1);
  expect(command.mock.calls[1]![0]).toMatchObject({ placement: { mode: "queue", authorization: "ask_before_start" } });
  expect(command.mock.calls[1]![1]?.commandId).not.toBe(before.commandId);
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(true);
});

const openOtherProposal = async () => {
  cleanup();
  act(() => app.ui.selectTask(ids.pTide, ids.tOneCard));
  view(); await tick(600);
  fireEvent.click(screen.getByRole("button", { name: /^Agent for this task:/ })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: "Agent for this task" })).getByRole("button", { name: /More options/ })); await tick();
};

it("preserves edited linked proposal drafts in Saved drafts when another proposal is opened", async () => {
  await orderedMerge(); await openSetReview();
  const text = "My own revised instructions that must not be lost.";
  fireEvent.change(screen.getByRole("textbox", { name: "What you’re asking" }), { target: { value: text } });
  act(() => app.ui.back());
  await openOtherProposal();
  expect(app.ui.draft(ids.pTide, null)?.text).not.toBe(text);
  expect(Object.values(app.ui.get().savedDrafts).flat()).toEqual(expect.arrayContaining([
    expect.objectContaining({ text, review: expect.objectContaining({ origin: expect.objectContaining({ chat: ids.tSet, index: 0 }) }) }),
  ]));
});

it.each([false, true])("never bypasses a cancelled edited Review after merge confirmation (saved elsewhere: %s)", async (saved) => {
  await orderedMerge(); await openSetReview();
  const text = "Only do the narrower reviewed work, not the original request.";
  fireEvent.change(screen.getByRole("textbox", { name: "What you’re asking" }), { target: { value: text } });
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  act(() => app.ui.back());
  let displaced: string | undefined;
  if (saved) {
    await openOtherProposal();
    displaced = app.ui.draft(ids.pTide, null)!.text;
    app.ui.persistNow();
    cleanup(); app.ui.retire(); app.ui = new UiStore(app.profile.profileId);
    act(() => { app.ui.back(); app.ui.selectTask(ids.pTide, ids.tSet); });
  }
  await backToCard();
  expect(within(setCard()).queryByRole("button", { name: "Start all" })).toBeNull();
  const command = vi.spyOn(app.session, "command");
  await openSetReview();
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "What you’re asking" }).value).toBe(text);
  if (saved) expect(Object.values(app.ui.get().savedDrafts).flat().some((d) => d.text === displaced)).toBe(true);
  fireEvent.click(createButton()); await tick(1500);
  expect(fromSet()).toHaveLength(1);
  const first = command.mock.calls.map(([b]) => b).find((b) => b.command === "create_task");
  expect(first).toMatchObject({ prompt: text, origin: { chat: ids.tSet, index: 0, reviewed: true } });
  await backToCard();
  fireEvent.click(within(setCard()).getByRole("button", { name: "Start the rest" })); await tick(1500);
  expect(fromSet()).toHaveLength(2);
});

it.each(["issue", "new_pr"] as const)("keeps a %s proposal linked through offline Review and reconnect, including merge confirmation", async (kind) => {
  const run = host().runs.find((r) => r.run_id === ids.rSet)!;
  run.outcome!.report!.new_work = run.outcome!.report!.new_work.slice(0, 2);
  if (kind === "issue") run.outcome!.report!.new_work[0]!.issue = 219;
  const settings = host().github.settings.get(ids.pTide)!;
  const reviewer = { harness: "codex" as const, model: null, effort: null };
  host().github.settings.set(ids.pTide, { ...settings, mode: "through_merge", warning_answered: false,
    workflow: { ...settings.workflow, review_with: reviewer } });
  const query = app.session.query.bind(app.session);
  let waiting = true;
  vi.spyOn(app.session, "query").mockImplementation((q) => {
    if (q.kind === "github_project" && waiting) { waiting = false; return new Promise<never>(() => {}); }
    return query(q);
  });
  await openChat(ids.tSet);
  act(() => window.__sim!.offline()); await tick();
  await openSetReview();
  expect(createButton().disabled).toBe(true);
  app.ui.persistNow();
  cleanup(); app.ui.retire(); app.ui = new UiStore(app.profile.profileId);
  view(app.ui.route()); await tick();
  act(() => window.__sim!.online()); await tick(1500);
  expect(screen.getByText("Before it merges by itself")).toBeTruthy();
  expect(createButton().disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(createButton()); await tick(1500);
  expect(fromSet()).toHaveLength(1);
  const link = kind === "issue" ? { type: "issue", number: 219, review_with: reviewer } : { type: "new_pr" };
  expect(command.mock.calls.map(([b]) => b).find((b) => b.command === "create_task")).toMatchObject({ github: link });
  expect(fromSet()[0]!.github).toMatchObject(kind === "issue" ? { issue: 219, check_with: reviewer, link } : { link });
});

it("keeps an explicit Not linked choice instead of restoring the proposal's GitHub default", async () => {
  await orderedMerge(); await openSetReview();
  fireEvent.click(screen.getByRole("button", { name: "GitHub: New pull request" })); await tick();
  fireEvent.click(within(screen.getByRole("radiogroup", { name: "What it’s for on GitHub" })).getByRole("radio", { name: /^Not linked/ })); await tick();
  expect(app.ui.draft(ids.pTide, null)?.review?.github).toBeNull();
  act(() => app.ui.back()); await backToCard();
  await openSetReview();
  app.ui.persistNow();
  cleanup(); app.ui.retire(); app.ui = new UiStore(app.profile.profileId);
  view(app.ui.route()); await tick(600);
  expect(createButton().disabled).toBe(false);
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(createButton()); await tick(1500);
  expect(fromSet()).toHaveLength(1);
  expect(command.mock.calls.map(([b]) => b).find((b) => b.command === "create_task")?.github).toBeFalsy();
  expect(fromSet()[0]!.github).toBeFalsy();
});

it("rejects a stale reviewed auto-start at host admission if Next up pauses during durable outbox storage", async () => {
  await orderedMerge(); await openSetReview();
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  let release!: () => void;
  const pending = new Promise<void>((resolve) => { release = resolve; });
  app.platform.outbox = { put: vi.fn(() => pending), list: vi.fn(async () => []), remove: vi.fn(async () => {}) } as NonNullable<App["platform"]["outbox"]>;
  const command = vi.spyOn(app.session, "command");
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(false);
  fireEvent.click(createButton()); await tick(100);
  const original = app.ui.draft(ids.pTide, null)!.attempt!;
  expect(command.mock.calls).toHaveLength(0);
  act(() => host().talk.put({ ...app.session.state.get().queues[ids.pTide]!, paused: true })); await tick(100);
  await act(async () => { release(); }); await tick(1600);
  expect(command.mock.calls[0]![0]).toMatchObject({ placement: { mode: "queue", authorization: "auto_start" }, origin: { reviewed: true } });
  expect(fromSet()).toHaveLength(0);
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(true);
  expect(command.mock.calls[0]![1]?.commandId).toBe(original.commandId);
  expect(app.ui.draft(ids.pTide, null)?.attempt).toMatchObject({ phase: "rejected", commandId: null, detail: expect.stringContaining("Next up is paused") });
  fireEvent.click(createButton()); await tick(1500);
  expect(fromSet()).toHaveLength(1);
  expect(command.mock.calls[1]![0]).toMatchObject({ placement: { mode: "queue", authorization: "ask_before_start" } });
  expect(command.mock.calls[1]![1]?.commandId).not.toBe(original.commandId);
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(true);
});

it("retains edited second-step Review but offers an independent start after its predecessor fails", async () => {
  await orderedMerge();
  act(() => {
    const run = host().runs.find((r) => r.run_id === ids.rSet)!;
    run.outcome!.report!.new_work[1]!.refused = "Correct these instructions before starting.";
    host().emit({ type: "run_upserted", run: { ...run } });
  });
  await openSetReview();
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  fireEvent.click(createButton()); await tick(1500);
  await backToCard();
  fireEvent.click(within(setCard()).getByRole("button", { name: "Review next task" })); await tick();
  cleanup(); view(app.ui.route()); await tick(600);
  const text = "The corrected second step";
  fireEvent.change(screen.getByRole("textbox", { name: "What you’re asking" }), { target: { value: text } });
  act(() => app.ui.back()); await backToCard();
  act(() => {
    host().touchTask(fromSet()[0]!.task_id, { state: "failed" });
    const p = { ...host().projects.find((p) => p.project_id === ids.pTide)!, active_writer: null };
    host().projects = host().projects.map((x) => x.project_id === ids.pTide ? p : x);
    host().emit({ type: "project_upserted", project: p });
  }); await tick();
  fireEvent.click(within(setCard()).getByRole("button", { name: /^2 tasks, in order/ })); await tick();
  fireEvent.click(within(setCard()).getByRole("button", { name: "Open in Review" })); await tick();
  cleanup(); view(app.ui.route()); await tick(600);
  expect(screen.queryByRole("heading", { name: "Review task 2 of 2" })).toBeNull();
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "What you’re asking" }).value).toBe(text);
  const start = screen.getByRole("radio", { name: "Start now" });
  fireEvent.click(start); await tick();
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(within(document.querySelector<HTMLElement>(".review-foot")!).getByRole("button", { name: "Start task" })); await tick(1500);
  expect(fromSet()).toHaveLength(2);
  expect(command.mock.calls.map(([b]) => b).find((b) => b.command === "create_task")).toMatchObject({ prompt: text, placement: { mode: "start_now" }, origin: { index: 1, reviewed: true } });
  expect(app.ui.draft(ids.pTide, null)).toBeNull();
});

const secondStepReview = async () => {
  await orderedMerge();
  act(() => {
    const run = host().runs.find((r) => r.run_id === ids.rSet)!;
    run.outcome!.report!.new_work[1]!.refused = "Correct these instructions before starting.";
    host().emit({ type: "run_upserted", run: { ...run } });
  });
  await openSetReview();
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  fireEvent.click(createButton()); await tick(1500);
  await backToCard();
  fireEvent.click(within(setCard()).getByRole("button", { name: "Review next task" })); await tick();
  cleanup(); view(app.ui.route()); await tick(600);
  fireEvent.change(screen.getByRole("textbox", { name: "What you’re asking" }), { target: { value: "My corrected second step" } });
  act(() => {
    const draft = app.ui.draft(ids.pTide, null)!;
    app.ui.saveDraft(ids.pTide, null, { review: { ...draft.review!, browser: false,
      bounds: "Keep the storage API unchanged.", completionChecks: "Run the storage tests.", stopWhen: "The tests pass." } });
  }); await tick();
};
const failPrevious = async () => {
  act(() => {
    host().touchTask(fromSet()[0]!.task_id, { state: "failed" });
    const p = { ...host().projects.find((p) => p.project_id === ids.pTide)!, active_writer: null };
    host().projects = host().projects.map((x) => x.project_id === ids.pTide ? p : x);
    host().emit({ type: "project_upserted", project: p });
  }); await tick();
};
const startIndependent = async () => {
  const before = app.ui.draft(ids.pTide, null)!;
  const start = screen.getByRole<HTMLButtonElement>("radio", { name: "Start now" });
  expect(start.disabled).toBe(false);
  fireEvent.click(start); await tick();
  expect(app.ui.draft(ids.pTide, null)).toMatchObject({ text: before.text, agent: before.agent,
    review: { ...before.review, ordered: undefined, placement: "start_now" } });
  fireEvent.click(within(document.querySelector<HTMLElement>(".review-foot")!).getByRole("button", { name: "Start task" })); await tick(1500);
};

it("offers independent work live when the predecessor fails while second-step Review stays open", async () => {
  await secondStepReview();
  const before = app.ui.draft(ids.pTide, null)!;
  const command = vi.spyOn(app.session, "command");
  await failPrevious();
  expect(screen.getByRole("radio", { name: "Start now" })).toBeTruthy();
  expect(screen.getByText(/no longer follows/i)).toBeTruthy();
  expect(within(document.querySelector<HTMLElement>(".review-foot")!).getByRole<HTMLButtonElement>("button", { name: "Start task" }).disabled).toBe(true);
  expect(command.mock.calls).toHaveLength(0);
  await startIndependent();
  expect(fromSet()).toHaveLength(2);
  expect(command.mock.calls.map(([b]) => b).find((b) => b.command === "create_task")).toMatchObject({ prompt: before.text,
    harness: before.agent!.harness, model: before.agent!.model, effort: before.agent!.effort,
    placement: { mode: "start_now" }, origin: { index: 1, reviewed: true } });
});

it.each(["not_received", "rejected"] as const)("keeps an unknown second-step request intact, then offers independent work after Check reports %s", async (result) => {
  await secondStepReview();
  const query = app.session.query.bind(app.session);
  let holdStatus = true;
  vi.spyOn(app.session, "query").mockImplementation((q) => q.kind === "command_status" && holdStatus ? new Promise<never>(() => {}) : query(q));
  const command = vi.spyOn(app.session, "command").mockRejectedValueOnce(new OutcomeUnknownError("unanswered"));
  fireEvent.click(createButton()); await tick(300);
  const original = app.ui.draft(ids.pTide, null)!.attempt!;
  expect(original.phase).toBe("unknown");
  await failPrevious();
  act(() => app.ui.back()); await backToCard();
  fireEvent.click(within(setCard()).getByRole("button", { name: /^2 tasks, in order/ })); await tick();
  fireEvent.click(within(setCard()).getByRole("button", { name: "Open in Review" })); await tick();
  sendOnOpen(ids.pTide, null);
  cleanup(); view(app.ui.route()); await tick(600);
  expect(app.ui.draft(ids.pTide, null)?.attempt).toMatchObject({ phase: "unknown", commandId: original.commandId, body: original.body });
  const retryWasBlocked = screen.getByRole<HTMLButtonElement>("button", { name: "Send again" }).disabled;
  expect(command.mock.calls).toHaveLength(1);
  if (result === "rejected") host().receipts.set(original.commandId!, { command_id: original.commandId!, status: "rejected", seq: null, result: null,
    error: { code: "conflict", message: "The predecessor did not succeed." } });
  holdStatus = false;
  fireEvent.click(screen.getByRole("button", { name: "Check" })); await tick(600);
  expect(app.ui.draft(ids.pTide, null)?.attempt?.phase).toBe(result === "rejected" ? "rejected" : "not_delivered");
  expect(screen.getByRole("radio", { name: "Start now" })).toBeTruthy();
  expect(retryWasBlocked).toBe(true);
  expect(command.mock.calls).toHaveLength(1);
  await startIndependent();
  expect(command.mock.calls[1]![1]?.commandId).not.toBe(original.commandId);
  expect(command.mock.calls[1]![0]).toMatchObject({ prompt: original.text, placement: { mode: "start_now" }, origin: { index: 1, reviewed: true } });
  expect(fromSet()).toHaveLength(2);
});

it("preserves an in-flight ordered request through failure, then requires a fresh independent-work choice after refusal", async () => {
  await secondStepReview();
  const realCommand = app.session.command.bind(app.session);
  let release!: () => void;
  const held = new Promise<void>((resolve) => { release = resolve; });
  const command = vi.spyOn(app.session, "command").mockImplementationOnce(async (body, options) => {
    await held;
    return realCommand(body, options);
  });
  fireEvent.click(createButton()); await tick(300);
  const original = app.ui.draft(ids.pTide, null)!.attempt!;
  expect(original.phase).toBe("sending");
  await failPrevious();
  const start = screen.getByRole<HTMLButtonElement>("radio", { name: "Start now" });
  expect(start.disabled).toBe(true);
  fireEvent.click(start); await tick();
  expect(app.ui.draft(ids.pTide, null)?.attempt).toEqual(original);
  expect(app.ui.draft(ids.pTide, null)?.review?.ordered?.step).toBe(2);
  expect(command.mock.calls).toHaveLength(1);
  await act(async () => { release(); }); await tick(600);
  expect(app.ui.draft(ids.pTide, null)?.attempt?.phase).toBe("rejected");
  expect(fromSet()).toHaveLength(1);
  expect(screen.getByRole<HTMLInputElement>("radio", { name: "Start now" }).checked).toBe(false);
  await startIndependent();
  expect(fromSet()).toHaveLength(2);
  expect(command.mock.calls[1]![1]?.commandId).not.toBe(original.commandId);
});

it("settles an accepted second-step receipt after predecessor failure without offering a duplicate independent start", async () => {
  await secondStepReview();
  const query = app.session.query.bind(app.session);
  let holdStatus = true;
  vi.spyOn(app.session, "query").mockImplementation((q) => q.kind === "command_status" && holdStatus ? new Promise<never>(() => {}) : query(q));
  const command = vi.spyOn(app.session, "command");
  window.__sim!.dropNextReceipt();
  fireEvent.click(createButton()); await tick(300);
  const original = app.ui.draft(ids.pTide, null)!.attempt!;
  expect(original.phase).toBe("unknown");
  expect(host().tasks.filter((t) => t.origin?.chat === ids.tSet)).toHaveLength(2);
  await failPrevious();
  expect(app.ui.draft(ids.pTide, null)?.attempt).toMatchObject({ commandId: original.commandId, body: original.body });
  act(() => window.__sim!.offline());
  holdStatus = false;
  act(() => window.__sim!.online()); await tick(1800);
  expect(app.ui.draft(ids.pTide, null)).toBeNull();
  expect(host().tasks.filter((t) => t.origin?.chat === ids.tSet)).toHaveLength(2);
  expect(command.mock.calls).toHaveLength(1);
  expect(app.ui.route()).toEqual({ screen: "talk" });
});

it("never carries automatic-send approval across a failed predecessor or the independent-work choice", async () => {
  await secondStepReview(); await failPrevious();
  cleanup();
  sendOnOpen(ids.pTide, null);
  const command = vi.spyOn(app.session, "command");
  view(app.ui.route()); await tick(900);
  expect(command.mock.calls).toHaveLength(0);
  fireEvent.click(screen.getByRole("radio", { name: "Start now" })); await tick(900);
  expect(command.mock.calls).toHaveLength(0);
  expect(fromSet()).toHaveLength(1);
  expect(app.ui.draft(ids.pTide, null)?.text).toBe("My corrected second step");
  fireEvent.click(within(document.querySelector<HTMLElement>(".review-foot")!).getByRole("button", { name: "Start task" })); await tick(1500);
  expect(fromSet()).toHaveLength(2);
});

const singleCardReview = async (paused = false) => {
  await openChat(ids.tOneCard);
  if (paused) {
    act(() => host().talk.put({ ...app.session.state.get().queues[ids.pTide]!, paused: true })); await tick();
  }
  fireEvent.click(screen.getByRole("button", { name: /^Agent for this task:/ })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: "Agent for this task" })).getByRole("button", { name: /More options/ })); await tick();
  cleanup(); view(app.ui.route()); await tick(600);
};

it("adds a single-card reviewed task safely to paused Next up without promising or attempting resumption", async () => {
  await singleCardReview(true);
  expect(app.ui.draft(ids.pTide, null)?.review?.ordered).toBeUndefined();
  expect(screen.getByRole<HTMLButtonElement>("radio", { name: "Start by itself" }).disabled).toBe(true);
  expect(screen.queryByText(/adding this resumes it/)).toBeNull();
  expect(createButton().disabled).toBe(false);
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(createButton()); await tick(1500);
  expect(command.mock.calls[0]![0]).toMatchObject({ origin: { chat: ids.tOneCard, reviewed: true }, placement: { mode: "queue", authorization: "ask_before_start" } });
  expect(Object.values(app.session.state.get().tasks).filter((t) => t.origin?.chat === ids.tOneCard)).toHaveLength(1);
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(true);
});

it("keeps an ordinary non-card Review’s deliberate option to resume paused Next up", async () => {
  await singleCardReview(true);
  // Reuse the populated Review fields, but make this an ordinary new task.
  act(() => {
    const draft = app.ui.draft(ids.pTide, null)!;
    app.ui.saveDraft(ids.pTide, null, { review: { ...draft.review!, origin: undefined } });
  }); await tick();
  expect(screen.getByRole<HTMLButtonElement>("radio", { name: "Start by itself" }).disabled).toBe(false);
  expect(screen.getByText(/adding this resumes it/)).toBeTruthy();
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(createButton()); await tick(1500);
  expect(command.mock.calls[0]![0]).toMatchObject({ placement: { mode: "queue", authorization: "auto_start" } });
  expect(command.mock.calls[0]![0]).not.toHaveProperty("origin");
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(false);
});

it("checks an unknown single-card auto-start before sending a fresh safe choice into paused Next up", async () => {
  await singleCardReview();
  const query = app.session.query.bind(app.session);
  let holdStatus = true;
  vi.spyOn(app.session, "query").mockImplementation((q) => q.kind === "command_status" && holdStatus ? new Promise<never>(() => {}) : query(q));
  const command = vi.spyOn(app.session, "command").mockRejectedValueOnce(new OutcomeUnknownError("unanswered"));
  fireEvent.click(createButton()); await tick(300);
  const original = app.ui.draft(ids.pTide, null)!.attempt!;
  act(() => host().talk.put({ ...app.session.state.get().queues[ids.pTide]!, paused: true })); await tick();
  expect(original.body).toMatchObject({ placement: { mode: "queue", authorization: "auto_start" } });
  expect(app.ui.draft(ids.pTide, null)?.attempt).toMatchObject({ phase: "unknown", commandId: original.commandId, body: original.body });
  expect(createButton().disabled).toBe(true);
  const repeat = screen.getByRole<HTMLButtonElement>("button", { name: "Send again" });
  expect(repeat.disabled).toBe(true);
  fireEvent.click(createButton()); fireEvent.click(repeat); await tick();
  expect(command.mock.calls).toHaveLength(1);
  holdStatus = false;
  fireEvent.click(screen.getByRole("button", { name: "Check" })); await tick(600);
  expect(app.ui.draft(ids.pTide, null)?.attempt?.phase).toBe("not_delivered");
  fireEvent.click(createButton()); await tick(1500);
  expect(command.mock.calls[1]![1]?.commandId).not.toBe(original.commandId);
  expect(command.mock.calls[1]![0]).toMatchObject({ placement: { mode: "queue", authorization: "ask_before_start" } });
  expect(Object.values(app.session.state.get().tasks).filter((t) => t.origin?.chat === ids.tOneCard)).toHaveLength(1);
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(true);
});

it.each(["chat_work_sets_v1", "chat_work_reviewed_v1"])("does not detach ordered tasks from their origins when %s is missing", async (feature) => {
  await orderedMerge();
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== feature) } })));
  await tick();
  const command = vi.spyOn(app.session, "command");
  const review = within(setCard()).getByRole<HTMLButtonElement>("button", { name: "Review tasks" });
  expect(review.disabled).toBe(true);
  fireEvent.click(review); await tick();
  expect(app.ui.route()).toEqual({ screen: "talk" });
  expect(fromSet()).toHaveLength(0);
  expect(command.mock.calls).toHaveLength(0);
});

it.each(["offline", "tasks", "capability"])("keeps the reviewed draft without sending when %s blocks creation", async (guard) => {
  await orderedMerge(); await openSetReview();
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick(900);
  if (guard === "offline") act(() => window.__sim!.offline());
  else act(() => app.session.state.set((s) => guard === "tasks"
    ? { ...s, grants: { ...s.grants!, tasks: false } }
    : { ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== "chat_work_reviewed_v1") } }));
  await tick();
  const command = vi.spyOn(app.session, "command");
  expect(createButton().disabled).toBe(true);
  fireEvent.click(createButton()); await tick();
  expect(fromSet()).toHaveLength(0);
  expect(command.mock.calls).toHaveLength(0);
  expect(app.ui.draft(ids.pTide, null)?.review?.origin).toMatchObject({ index: 0, reviewed: true });
});

it("says on its card when a fix started as a plan because the issue comes from outside the project", async () => {
  host().github.items.find((i) => i.number === 219)!.author_writes = false;
  await openChat(ids.tCards);
  fireEvent.click(screen.getByRole("button", { name: /^3 suggested tasks/ })); await tick();
  const row = screen.getByRole("heading", { name: "Fix the orders webhook retry test" }).closest<HTMLElement>(".work-card-item")!;
  fireEvent.click(within(row).getByRole("button", { name: /^(Start|Review)$/ }));
  await tick(1500);
  const task = Object.values(app.session.state.get().tasks).find((t) => t.origin?.chat === ids.tCards && t.origin.index === 2);
  expect(task).toMatchObject({ mode: "plan", github: { from_stranger: true } });
  expect(screen.getByText(/^Started as a plan, because the issue comes from outside the project\./)).toBeTruthy();
});

it("says nothing about a plan for a fix that starts as one, or a plan that was asked for", () => {
  const issue = { link: { type: "issue", number: 219 }, rounds: 0 } as NonNullable<Task["github"]>;
  const task = (mode: Task["mode"], from_stranger: boolean) => ({ mode, github: { ...issue, from_stranger } }) as Task;
  expect(planned(task("plan", true))).toBe("Started as a plan, because the issue comes from outside the project.");
  expect(planned(task("work", false))).toBeNull();
  expect(planned(task("plan", false))).toBeNull();
  expect(planned({ mode: "plan", github: { link: { type: "triage" }, rounds: 0, from_stranger: true } } as unknown as Task)).toBeNull();
});
