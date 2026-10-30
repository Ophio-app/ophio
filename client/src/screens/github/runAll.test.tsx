// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { prefs } from "../../app/prefs";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { Toasts } from "../../design/Toast";
import { HostSession } from "../../host/session";
import type { Command, GithubAction, GithubItem, LaunchChoice, Task } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Github from "./Github";

let app: App;
const initial = prefs.get();
const opus = { harness: "claude_code" as const, model: "claude-opus-5-5", effort: null };
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
const wait = async (ms: number) => { for (let t = 0; t < ms; t += 900) await tick(); };
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  prefs.set({ ...initial, warningsOff: false, hiddenWarnings: [] });
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
  const host = window.__sim!.host;
  const project = { ...session.state.get().projects[ids.pTide]!, active_writer: null };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project });
  // Two more nobody has sorted, from people who can write to the repository.
  const more = (number: number, title: string): GithubItem => ({
    number, pr: false, title, url: `https://github.com/samrivera/tidewater-api/issues/${number}`, author: "samrivera",
    author_writes: true, mine: true, draft: false, labels: [], updated_at: Date.now() - number * 60_000,
  });
  host.github.items.push(more(230, "Orders search ignores the date filter"), more(231, "Webhook retries log the secret header"));
  localStorage.setItem(`github.roles.${profile.profileId}`, JSON.stringify({ [ids.pTide]: { fix: opus, find: opus } }));
  await tick();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); prefs.set(initial); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

const hub = async () => {
  const route = { screen: "github", projectId: ids.pTide, view: "issues" } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Github route={route} /><Toasts /></RouteProvider></AppProvider>);
  await tick(); await tick();
};
const sim = () => window.__sim!.host;
const sent = (): Command[] => Object.values(app.session.commands.get()).map(c => c.body);
const actions = (): GithubAction[] => sent().flatMap((c) => (c.command === "github" ? [c.action] : []));
const card = () => within(document.querySelector<HTMLElement>(".run-all")!);
const section = (name: string) => within(screen.getByRole("heading", { name }).closest("section")!);
const listed = (name: string) => section(name).queryAllByRole("button").map((b) => b.textContent?.match(/^#(\d+)/)?.[1]).filter(Boolean).map(Number);
const dialog = (name: string) => within(screen.getByRole("dialog", { name }));
const tasks = () => Object.values(app.session.state.get().tasks);
const fixFor = (number: number): Task | undefined => tasks().find((t) => t.github?.link.type === "issue" && t.github.link.number === number && t.state !== "archived");
const clearQueue = async () => {
  const host = sim();
  host.queue = { ...host.queue, items: [], revision: host.queue.revision + 1 };
  host.emit({ type: "queue_changed", queue: host.queue }); await tick();
};
const defaults = (fix_with: LaunchChoice, review_with?: LaunchChoice) => {
  const s = sim().github.settings.get(ids.pTide)!;
  sim().github.settings.set(ids.pTide, { ...s, workflow: { ...s.workflow, fix_with, ...(review_with ? { review_with } : {}) } });
};

it("asks once who fixes them, then fixes every issue the agent can take, sorting the rest first, until none is left", async () => {
  await clearQueue(); await hub();
  expect(card().getByRole("status").textContent).toBe("1 issue to fix · 3 to sort first");
  expect(card().getByText("Choose who fixes them once; then one tap runs them all.")).toBeTruthy();
  expect(listed("Agent can take it")).toEqual([219]);
  expect(listed("Not sorted")).toEqual([223, 230, 231]);

  fireEvent.click(card().getByRole("button", { name: "Run all" })); await tick();
  const sheet = dialog("Run all defaults");
  expect(sheet.getByRole("button", { name: /Fixed by/ }).textContent).toMatch(/Claude Code · Opus 5\.5/);
  expect((sheet.getByRole("checkbox", { name: "Have another agent review each fix" }) as HTMLInputElement).checked).toBe(true);
  expect(sheet.getByText(/It also sorts new issues/)).toBeTruthy();
  fireEvent.click(sheet.getByRole("button", { name: "Save and run all" })); await tick(); await tick();
  expect(actions().map((a) => a.action)).toEqual(["set_settings", "run_issues"]);
  const saved = sim().github.settings.get(ids.pTide)!.workflow;
  expect(saved.fix_with).toMatchObject({ harness: "claude_code", model: "claude-opus-5-5" });
  expect(saved.review_with).toBeTruthy();
  expect(screen.queryByRole("dialog")).toBeNull();

  // #219 starts at once; the Sort runs beside it, read-only, with the reviewer.
  expect(fixFor(219)?.state).toMatch(/dispatching|working/);
  const sort = tasks().find((t) => t.github?.link.type === "triage")!;
  expect([sort.mode, sort.harness, sort.model]).toEqual(["plan", saved.review_with!.harness, saved.review_with!.model]);
  expect(card().getByRole("heading").textContent).toBe("Fixing #219");
  expect(card().getByRole("button", { name: "Pause" })).toBeTruthy();
  expect(card().getByRole("button", { name: "Stop…" })).toBeTruthy();
  expect(listed("Agent can take it")).toEqual([219]);

  // Once sorted, the two that say what to change join the run; the stranger's waits for a read.
  await wait(120_000);
  expect(listed("Needs you")).toEqual([221, 223]);
  expect(section("Needs you").getByText("Read it first: it’s from someone who can’t write to the repository.")).toBeTruthy();
  expect(screen.queryByRole("heading", { name: "Not sorted" })).toBeNull();
  // What waits on the person comes right under New issue, before what the agent can take.
  expect(screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent).filter((t) => t === "Needs you" || t === "Agent can take it"))
    .toEqual(["Needs you", "Agent can take it"]);
  expect(screen.getByRole("button", { name: /New issue/ }).compareDocumentPosition(screen.getByRole("heading", { name: "Needs you" })) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect([219, 230, 231].map((n) => fixFor(n)?.state)).toEqual(["completed", "completed", "completed"]);
  expect(fixFor(223)).toBeUndefined();
  expect(sim().github.runs.get(ids.pTide)!.view.ended_at).toBeTruthy();
  expect(card().getByRole("heading").textContent).toBe("Run all");
  expect(card().getByText("Last run: 3 done")).toBeTruthy();
  expect(card().getByText(/^Fixed by Claude Code · Opus 5\.5 · reviewed by/)).toBeTruthy();
});

it("Sort under way says how many it sorts and opens its task; it is done when Sort comes back", async () => {
  defaults(opus);
  await clearQueue(); await hub();
  fireEvent.click(card().getByRole("button", { name: "Sort" })); await tick(); await tick();
  const sort = tasks().find((t) => t.github?.link.type === "triage")!;
  expect(sort.mode).toBe("plan");
  expect(card().getByRole("status").textContent).toBe("1 issue to fix · sorting 3");
  fireEvent.click(card().getByRole("button", { name: "Sorting 3 issues: open it" })); await tick();
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(sort.task_id);

  await wait(120_000);
  expect(card().queryByRole("button", { name: /^Sorting/ })).toBeNull();
  expect(card().queryByRole("button", { name: "Sort" })).toBeNull();
  expect(card().getByRole("status").textContent).not.toMatch(/sort/i);
});

it("with defaults saved, Run all asks nothing, and keeps the go-ahead of a fix already waiting in Next up", async () => {
  defaults(opus);
  await clearQueue(); await hub();
  // An earlier fix for #219 waits for a go-ahead, as #6's did: it isn't anyone's question.
  const host = sim();
  host.projects = host.projects.map((p) => p.project_id === ids.pTide ? { ...p, active_writer: "elsewhere" } : p);
  fireEvent.click(section("Agent can take it").getByRole("button", { name: /#219/ })); await tick();
  fireEvent.click(dialog("#219 Retry flake in the orders webhook test").getByRole("button", { name: "Add to Next up" })); await tick(); await tick();
  const queue = host.talk.queue(ids.pTide);
  queue.items[0]!.authorization = "ask_before_start"; host.talk.refresh(queue); host.talk.put(queue); await tick();
  host.projects = host.projects.map((p) => p.project_id === ids.pTide ? { ...p, active_writer: null } : p);
  await tick();
  expect(listed("Agent can take it")).toEqual([219]);
  expect(section("Agent can take it").getByText("In Next up")).toBeTruthy();
  expect(listed("Needs you")).toEqual([221]);

  const before = actions().length;
  fireEvent.click(card().getByRole("button", { name: "Run all" })); await tick(); await tick();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(actions().slice(before)).toEqual([{ action: "run_issues", project_id: ids.pTide, step: "start" }]);
  expect(sim().github.runs.get(ids.pTide)!.view.fixes.map((f) => f.number)).toContain(219);
  expect(fixFor(219)?.state).toMatch(/dispatching|working/);
});

it("pause stops the working fix where it is and holds the rest; resume carries it on; stop takes out what waits", async () => {
  defaults(opus);
  // Sorted already: the stranger's waits for a read, and the agent can take the rest.
  for (const item of sim().github.items) if (!item.group && !item.pr) item.group = item.author_writes ? "agent_can_take" : "needs_you";
  await clearQueue(); await hub();
  expect(card().getByRole("status").textContent).toBe("3 issues to fix");
  fireEvent.click(card().getByRole("button", { name: "Run all" })); await tick(); await tick();
  expect(card().getByRole("heading").textContent).toBe("Fixing #219");
  expect(card().getByRole("status").textContent).toBe("0 of 3 done · 2 waiting");
  expect(card().getAllByRole("listitem").map((li) => li.textContent)).toEqual(["#219", "#230", "#231"]);

  fireEvent.click(card().getByRole("button", { name: "Pause" })); await tick(); await tick(); await tick();
  expect(fixFor(219)?.state).toBe("interrupted");
  expect(card().getByRole("heading").textContent).toBe("Paused");
  expect(card().getByRole("status").textContent).toBe("#219 carries on at Resume · 2 waiting");
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(true);
  // Paused, nothing more starts, however long it waits.
  await wait(30_000);
  expect([230, 231].map((n) => fixFor(n)?.state)).toEqual(["accepted", "accepted"]);

  fireEvent.click(card().getByRole("button", { name: "Resume" })); await tick(); await tick();
  expect(fixFor(219)?.state).toMatch(/dispatching|working/);
  expect(app.session.state.get().queues[ids.pTide]!.paused).toBe(false);
  expect(card().getByRole("heading").textContent).toBe("Fixing #219");

  fireEvent.click(card().getByRole("button", { name: /^Stop/ })); await tick();
  const sheet = dialog("Stop Run all?");
  expect(sheet.getByText("#219 stops now, where it is. The 2 issues waiting are taken out of Next up. Run all takes them up again.")).toBeTruthy();
  fireEvent.click(sheet.getByRole("button", { name: "Stop" })); await tick(); await tick(); await tick();
  expect(fixFor(219)?.state).toBe("interrupted");
  expect([230, 231].map((n) => fixFor(n))).toEqual([undefined, undefined]);
  expect(app.session.state.get().queues[ids.pTide]!.items.filter((i) => i.state.state !== "started")).toEqual([]);
  expect(card().getByRole("heading").textContent).toBe("Run all");
  expect(card().getByText("Last run, stopped: 1 stopped")).toBeTruthy();
  // Run all takes them up again.
  expect(card().getByRole("status").textContent).toBe("2 issues to fix");
});

it("says what holds the fixes up when a task ahead of them in Next up waits for a go-ahead", async () => {
  defaults(opus);
  // Next up holds only the README task, waiting for a go-ahead, as #6's fix did.
  const host = sim();
  const readme = host.queue.items.find((i) => i.task_id === ids.tReadme)!;
  host.queue = { ...host.queue, writer: null, items: [{ ...readme, position: 0, authorization: "ask_before_start" }], revision: host.queue.revision + 1 };
  host.talk.refresh(host.queue); host.emit({ type: "queue_changed", queue: host.queue });
  await hub();
  fireEvent.click(card().getByRole("button", { name: "Run all" })); await tick(); await tick();
  expect(card().getByText(/^“.+” is first in Next up and waits for your go-ahead\. The fixes start after it\.$/)).toBeTruthy();
  expect(fixFor(219)?.state).toBe("accepted");
  fireEvent.click(card().getByRole("button", { name: "Review and start" })); await tick();
  expect(app.ui.get().stacks.talk.at(-1)).toMatchObject({ screen: "review", projectId: ids.pTide, taskId: ids.tReadme, queueItemId: readme.item_id });
});

it("takes the person to the task holding the project when it waits on them", async () => {
  defaults(opus);
  await clearQueue();
  // The flaky-test task is changing files and waits for an approval.
  const host = sim();
  host.projects = host.projects.map((p) => p.project_id === ids.pTide ? { ...p, active_writer: ids.rFlaky } : p);
  host.emit({ type: "project_upserted", project: host.projects.find((p) => p.project_id === ids.pTide)! });
  await hub();
  fireEvent.click(card().getByRole("button", { name: "Run all" })); await tick(); await tick();
  expect(card().getByText("Held up by another task")).toBeTruthy();
  expect(card().getByText(/It is changing files in tidewater-api, so this can’t start until it finishes\.$/)).toBeTruthy();
  fireEvent.click(card().getByRole("button", { name: "Open it" })); await tick();
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(ids.tFlaky);
});

it("moves an issue between groups; Needs you takes its waiting fix out of the run, and Not sorted gives it back to the next Sort", async () => {
  defaults(opus);
  await hub();
  fireEvent.click(card().getByRole("button", { name: "Run all" })); await tick(); await tick();
  expect(fixFor(219)?.state).toBe("accepted");
  fireEvent.click(section("Agent can take it").getByRole("button", { name: /#219/ })); await tick();
  const sheet = () => dialog("#219 Retry flake in the orders webhook test");
  expect(sheet().getByText("Waiting in Next up")).toBeTruthy();
  expect(sheet().getByRole("button", { name: "Open its task" })).toBeTruthy();
  expect(sheet().queryByRole("button", { name: "Start" })).toBeNull();
  const listedUnder = () => sheet().getByRole("radiogroup", { name: "Listed under" });
  expect(within(listedUnder()).getByRole("radio", { name: "Agent can take it" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(within(listedUnder()).getByRole("radio", { name: "Needs you" })); await tick(); await tick();
  expect(actions().at(-1)).toEqual({ action: "set_group", project_id: ids.pTide, number: 219, pr: false, group: "needs_you" });
  expect(fixFor(219)).toBeUndefined();
  expect(sim().github.runs.get(ids.pTide)!.view.fixes.some((f) => f.number === 219)).toBe(false);
  fireEvent.click(sheet().getByRole("button", { name: "Close" })); await tick();
  expect(listed("Needs you")).toContain(219);
  expect(section("Needs you").getByText("You asked it to wait for you.")).toBeTruthy();

  // Not sorted puts it back for the next Sort.
  fireEvent.click(section("Needs you").getByRole("button", { name: /#219/ })); await tick();
  fireEvent.click(within(listedUnder()).getByRole("radio", { name: "Not sorted" })); await tick(); await tick();
  expect(actions().at(-1)).toEqual({ action: "set_group", project_id: ids.pTide, number: 219, pr: false });
  fireEvent.click(sheet().getByRole("button", { name: "Close" })); await tick();
  expect(listed("Not sorted")).toContain(219);
});

it("Stop asks first unless its warning is off, and an old computer says to update", async () => {
  defaults(opus);
  await clearQueue(); await hub();
  fireEvent.click(card().getByRole("button", { name: "Run all" })); await tick(); await tick();
  fireEvent.click(card().getByRole("button", { name: "Stop…" })); await tick();
  fireEvent.click(dialog("Stop Run all?").getByRole("checkbox", { name: "Don’t show me again" }));
  fireEvent.click(dialog("Stop Run all?").getByRole("button", { name: "Stop" })); await tick(); await tick();
  expect(prefs.get().hiddenWarnings).toEqual(["issues.stop"]);
  fireEvent.click(card().getByRole("button", { name: "Run all" })); await tick(); await tick();
  fireEvent.click(card().getByRole("button", { name: "Stop" })); await tick(); await tick();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(actions().filter((a) => a.action === "run_issues").map((a) => (a as { step: string }).step)).toEqual(["start", "stop", "start", "stop"]);
  cleanup();

  const caps = app.session.state.get().capabilities!;
  app.session.state.set((s) => ({ ...s, capabilities: { ...caps, features: caps.features.filter((f) => f !== "github_run_v1") } }));
  await hub();
  expect(screen.getByText(/Update Øphio on this computer to run every issue/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Run all" })).toBeNull();
});
