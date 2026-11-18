// SPDX-License-Identifier: Apache-2.0
//
// An ordered set starts as a whole, in its order, from Start all; a card for
// fixing every issue starts the project's own Run all; and a card beside a
// set opened in Review is still the suggestion it shows.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { CommandError, HostSession } from "../../host/session";
import { Toasts } from "../../design/Toast";
import { RouteProvider } from "../../shell/Screen";
import Talk from "./Talk";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  window.history.replaceState(null, "", "/?cards=1");
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  // One more suggestion after the set, outside it, for its own card.
  window.__sim!.host.runs.find((r) => r.run_id === ids.rSet)!.outcome!.report!.new_work.push({
    title: "Note the move in the changelog", mode: "work", brief: "Add a line to the changelog about the new settings store." });
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});
const tick = async (ms = 300) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const host = () => window.__sim!.host;
const openChat = async (chat: string) => {
  app.ui.selectProject(ids.pTide);
  app.ui.selectTask(ids.pTide, chat);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>
    <Talk route={{ screen: "talk" }} /></RouteProvider><Toasts /></AppProvider>);
  await tick(600);
};
const fromSet = () => Object.values(app.session.state.get().tasks).filter((t) => t.origin?.chat === ids.tSet);

it("starts a set in its order from Start all, each waiting in Next up for the one before", async () => {
  await openChat(ids.tSet);
  const set = screen.getByRole("button", { name: /^4 tasks, in order/ }).closest<HTMLElement>(".work-cards")!;
  const command = vi.spyOn(app.session, "command");
  const start = within(set).getByRole("button", { name: "Start all" });
  act(() => { fireEvent.click(start); fireEvent.click(start); });
  await tick(3000);
  expect(command.mock.calls.filter(([b]) => b.command === "create_task")).toHaveLength(4);
  expect(command.mock.calls.some(([b]) => b.command === "resume_task" || b.command === "send_message")).toBe(false);
  const started = fromSet().sort((a, b) => a.origin!.index - b.origin!.index);
  expect(started.map((t) => t.origin!.index)).toEqual([0, 1, 2, 3]);
  const items = app.session.state.get().queues[ids.pTide]!.items;
  const item = (task: string) => items.find((i) => i.task_id === task)!;
  for (const k of [1, 2, 3]) expect(item(started[k]!.task_id).depends_on).toEqual([item(started[k - 1]!.task_id).item_id]);
  // Nothing left to start on the set; the card beside it is untouched.
  expect(within(set).queryByRole("button", { name: /^Start (all|the rest)$/ })).toBeNull();
  expect(started.some((t) => t.origin!.index === 4)).toBe(false);
});

it("stops at a refused creation and retries only the uncreated rest in order", async () => {
  await openChat(ids.tSet);
  const set = screen.getByRole("button", { name: /^4 tasks, in order/ }).closest<HTMLElement>(".work-cards")!;
  const send = app.session.command.bind(app.session);
  let refused = false;
  const command = vi.spyOn(app.session, "command").mockImplementation((body, options) => {
    if (body.command === "create_task" && body.origin?.index === 1 && !refused) {
      refused = true;
      return Promise.reject(new CommandError({ code: "conflict", message: "This step needs another try." }, null));
    }
    return send(body, options);
  });
  fireEvent.click(within(set).getByRole("button", { name: "Start all" })); await tick(1500);
  expect(fromSet().map((t) => t.origin!.index)).toEqual([0]);
  expect(set.querySelectorAll(".is-starting")).toHaveLength(0);
  fireEvent.click(within(set).getByRole("button", { name: "Start the rest" })); await tick(3000);
  expect(fromSet().map((t) => t.origin!.index).sort()).toEqual([0, 1, 2, 3]);
  const creates = command.mock.calls.map(([b]) => b).filter((b) => b.command === "create_task");
  expect(creates.map((b) => b.origin!.index)).toEqual([0, 1, 1, 2, 3]);
  const tasks = fromSet().sort((a, b) => a.origin!.index - b.origin!.index);
  const items = app.session.state.get().queues[ids.pTide]!.items;
  for (const k of [1, 2, 3]) expect(items.find((i) => i.task_id === tasks[k]!.task_id)?.depends_on)
    .toEqual([items.find((i) => i.task_id === tasks[k - 1]!.task_id)!.item_id]);
});

it("opens the card beside a set in Review as the suggestion it shows", async () => {
  await openChat(ids.tSet);
  fireEvent.click(screen.getByRole("button", { name: /^Agent for this task:/ }));
  fireEvent.click(within(screen.getByRole("dialog", { name: "Agent for this task" })).getByRole("button", { name: /More options/ }));
  await tick();
  expect(app.ui.draft(ids.pTide, null)?.review?.origin).toEqual({ chat: ids.tSet, run: ids.rSet, index: 4, reviewed: true });
});

it("starts the project's own Run all from its card, once, and not from an archived chat", async () => {
  const s = host().github.settings.get(ids.pTide)!;
  host().github.settings.set(ids.pTide, { ...s, warning_answered: true,
    workflow: { ...s.workflow, fix_with: { harness: "claude_code", model: null, effort: null } } });
  await openChat(ids.tRunAll);
  const card = screen.getByRole("region", { name: "Fix every open issue" });
  const act = vi.spyOn(app.session, "command");
  fireEvent.click(within(card).getByRole("button", { name: "Run all" }));
  await tick(1500);
  const sent = act.mock.calls.map((c) => c[0]).find((b) => b.command === "github");
  expect(sent).toMatchObject({ action: { action: "run_issues", step: "start", origin: { chat: ids.tRunAll, run: ids.rRunAll, index: 0 } } });
  expect(within(card).getByRole("button", { name: /^Running\. Open Issues/ })).toBeTruthy();
  // Started once: its card can't start another.
  const again = await host().command({ command_id: "again", issued_at: Date.now(), expires_at: null, expected_revision: null,
    body: { command: "github", action: { action: "run_issues", project_id: ids.pTide, step: "start", origin: { chat: ids.tRunAll, run: ids.rRunAll, index: 0 } } } });
  expect(again.error?.message).toBe("This was already started.");
});

it("refuses Run all from a chat archived since its card was drawn", async () => {
  const s = host().github.settings.get(ids.pTide)!;
  host().github.settings.set(ids.pTide, { ...s, warning_answered: true,
    workflow: { ...s.workflow, fix_with: { harness: "claude_code", model: null, effort: null } } });
  host().tasks.find((t) => t.task_id === ids.tRunAll)!.state = "archived";
  const r = await host().command({ command_id: "archived", issued_at: Date.now(), expires_at: null, expected_revision: null,
    body: { command: "github", action: { action: "run_issues", project_id: ids.pTide, step: "start", origin: { chat: ids.tRunAll, run: ids.rRunAll, index: 0 } } } });
  expect(r.error?.message).toBe("This chat is archived. Bring it back first, or start the work from Tasks.");
});

it("says once for a set what its steps share, and keeps each step to its title", async () => {
  await openChat(ids.tSet);
  const set = screen.getByRole("button", { name: /^4 tasks, in order/ }).closest<HTMLElement>(".work-cards")!;
  fireEvent.click(within(set).getByRole("button", { name: /^4 tasks, in order/ }));
  await tick();
  const line = set.querySelector(".work-cards__line")!;
  expect(line.textContent).toMatch(/^Change · .+ · each starts after the one before$/);
  // The agent is said once, on the set's own chip; the steps say only their titles.
  expect(within(set).getAllByRole("button", { name: /^Agent for these tasks:/ })).toHaveLength(1);
  expect(within(set).queryByRole("button", { name: /^Agent for this task:/ })).toBeNull();
  expect(set.querySelectorAll(".work-card-item__line")).toHaveLength(0);
  expect(within(set).getAllByRole("heading", { level: 4 }).map((h) => h.textContent)).toEqual([
    "Add the new settings store", "Copy existing settings across", "Read settings from the new store", "Remove the old settings store"]);
});

it("stops a set at a step that didn't succeed and lets the person start a later one on its own", async () => {
  await openChat(ids.tSet);
  const set = screen.getByRole("button", { name: /^4 tasks, in order/ }).closest<HTMLElement>(".work-cards")!;
  fireEvent.click(within(set).getByRole("button", { name: "Start all" }));
  await tick(3000);
  const made = fromSet().sort((a, b) => a.origin!.index - b.origin!.index);
  // The first fails; the computer puts the rest away before they start.
  act(() => {
    host().touchTask(made[0]!.task_id, { state: "failed" });
    // Its run ended, so it no longer holds the project.
    const p = { ...host().projects.find((p) => p.project_id === ids.pTide)!, active_writer: null };
    host().projects = host().projects.map((x) => x.project_id === ids.pTide ? p : x);
    host().emit({ type: "project_upserted", project: p });
    for (const t of made.slice(1)) host().touchTask(t.task_id, { state: "archived", current_run: null, status_line: "Not started: “Add the new settings store” didn't succeed" });
  });
  await tick();
  expect(within(set).queryByRole("button", { name: /^Start (all|the rest)$/ })).toBeNull();
  expect(set.textContent).toContain("“Add the new settings store” didn’t succeed, so the rest don’t start after it.");
  fireEvent.click(within(set).getByRole("button", { name: /^4 tasks, in order/ }));
  await tick();
  // The step that failed shows as it stands; each after it has a Start of its own.
  const steps = within(set).getAllByRole("listitem");
  expect(within(steps[0]!).getByRole("button", { name: /Add the new settings store/ }).textContent).toContain("Failed");
  // The set says why once, and what they share; each step is its title and its Start.
  expect(set.querySelector(".work-cards__line")!.textContent).toMatch(/^Change · own branch and pull request · [^·]+$/);
  expect(steps[1]!.textContent).toBe("Copy existing settings acrossStart");
  expect(within(set).queryByRole("button", { name: /^Agent for this task:/ })).toBeNull();
  const act2 = vi.spyOn(app.session, "command");
  fireEvent.click(within(steps[2]!).getByRole("button", { name: "Start" }));
  await tick(1500);
  const sent = act2.mock.calls.map((c) => c[0]).find((b) => b.command === "create_task");
  expect(sent).toMatchObject({ placement: { mode: "start_now" }, origin: { index: 2 } });
  const again = fromSet().filter((t) => t.origin!.index === 2 && t.state !== "archived");
  expect(again).toHaveLength(1);
  // Only that one: the others stay put away until the person starts them.
  expect(fromSet().filter((t) => t.state !== "archived" && t.origin!.index !== 0)).toHaveLength(1);
});

it("says the shared agent once on an opened set of tasks that stand alone", async () => {
  await openChat(ids.tCards);
  const opener = screen.getByRole("button", { name: /^3 suggested tasks/ });
  const card = opener.closest<HTMLElement>(".work-cards")!;
  // Folded, the set's chip changes all three at once.
  expect(within(card).getAllByRole("button", { name: /^Agent for these tasks:/ })).toHaveLength(1);
  fireEvent.click(opener);
  await tick();
  // Opened, each task's own chip says it, with its own options; the set's would repeat it.
  expect(within(card).queryByRole("button", { name: /^Agent for these tasks:/ })).toBeNull();
  expect(within(card).getAllByRole("button", { name: /^Agent for this task:/ })).toHaveLength(3);
  fireEvent.click(opener);
  await tick();
  expect(within(card).getAllByRole("button", { name: /^Agent for these tasks:/ })).toHaveLength(1);
});
