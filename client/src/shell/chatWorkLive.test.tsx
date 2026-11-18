// SPDX-License-Identifier: Apache-2.0
//
// Work started from a chat stays with that chat: its card shows how it
// stands, in Tasks' own words and in place; tapping it opens the task and
// Back comes back to the chat. The task, in Tasks and on its own heading,
// says which chat it came from, and that leads back there too.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { HostSession } from "../host/session";
import type { Run, Task } from "../protocol/types";
import { Shell } from "./Shell";
import { draftNewTask } from "../app/github";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  window.history.replaceState(null, "", "/?cards=1");
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  app.ui.selectProject(ids.pTide);
  app.ui.selectTask(ids.pTide, ids.tOneCard);
  session.start(); await vi.advanceTimersByTimeAsync(900);
  act(() => app.ui.selectTab("talk"));
  render(<AppProvider app={app}><Shell /></AppProvider>);
  await ready(); await ready(600);
});
afterEach(() => {
  cleanup(); app.session.stop(); app.ui.retire(); window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});
const ready = async (ms = 500) => {
  await act(async () => { await vi.dynamicImportSettled(); await vi.advanceTimersByTimeAsync(ms); });
  await act(async () => { await vi.dynamicImportSettled(); });
};
const host = () => window.__sim!.host;
const task = (id: string): Task => app.session.state.get().tasks[id]!;
const shown = () => app.ui.get().taskByProject[ids.pTide];
const card = () => document.querySelector<HTMLElement>(".work-cards")!;
const chatTitle = () => task(ids.tOneCard).title;

/** Starts the chat's one suggestion from its card; the new task's id. */
async function startFromCard(): Promise<string> {
  const before = new Set(Object.keys(app.session.state.get().tasks));
  fireEvent.click(within(card()).getByRole("button", { name: "Start" }));
  await ready(3000);
  const made = Object.values(app.session.state.get().tasks).find((t) => !before.has(t.task_id));
  expect(made?.origin?.chat).toBe(ids.tOneCard);
  return made!.task_id;
}
const live = (title: string) => within(card()).getByRole("button", { name: new RegExp(`^${title}`) });

it("shows how a card's task stands in Tasks' words, in place, as it changes", async () => {
  const id = await startFromCard();
  const { title } = task(id);
  expect(live(title).querySelector(".pill")?.textContent).toMatch(/^(In Next up|Waiting to start|Working|Starting)$/);
  const run: Run = { ...Object.values(app.session.state.get().runs).find((r) => r.task_id === id)
    ?? Object.values(app.session.state.get().runs)[0]!, run_id: "0192f7a1-0000-7000-8000-0000000a11ce", task_id: id, state: "completed", ended_at: Date.now() };
  host().emit({ type: "run_upserted", run });
  const pr = { number: 412, url: "https://example.invalid/pr/412", state: "open" as const, draft: false, head: "abc", passed: 3, failed: 0, pending: 0, unresolved: 0, conflict: false };
  host().emit({ type: "task_upserted", task: { ...task(id), state: "completed", current_run: run.run_id, github: { link: { type: "new_pr" }, pr } as Task["github"] } });
  await ready();
  expect(live(title).querySelector(".pill")?.textContent).toBe("Pull request open");
  host().emit({ type: "task_upserted", task: { ...task(id), github: { ...task(id).github!, pr: { ...pr, state: "merged" } } } });
  await ready();
  expect(live(title).querySelector(".pill")?.textContent).toBe("Merged");
  host().emit({ type: "run_upserted", run: { ...run, state: "failed" } });
  host().emit({ type: "task_upserted", task: { ...task(id), state: "failed", github: undefined } });
  await ready();
  expect(live(title).querySelector(".pill")?.textContent).toBe("Failed");
  // It stays the same card in the same chat.
  expect(shown()).toBe(ids.tOneCard);
});

it.each(["header", "Android Back"])("opens a card's task from the card, and %s comes back to the chat", async (path) => {
  const id = await startFromCard();
  fireEvent.click(live(task(id).title));
  await ready();
  expect(shown()).toBe(id);
  // The task says where it came from.
  expect(screen.getByRole("button", { name: `From chat “${chatTitle()}”. Open the chat` })).toBeTruthy();
  if (path === "header") fireEvent.click(document.querySelector<HTMLElement>(".chat-head__back")!);
  else act(() => { expect(window.systemBack!()).toBe(true); });
  await ready();
  expect(app.ui.get().tab).toBe("talk");
  expect(shown()).toBe(ids.tOneCard);
});

it("marks a chat's work in Tasks with its chat, and its heading leads back to the chat", async () => {
  const id = await startFromCard();
  act(() => app.ui.selectTab("work")); await ready();
  const tasks = () => within(document.querySelector<HTMLElement>("section.tab:not(.is-hidden)")!);
  // Waiting in Next up, its place there says where it came from.
  act(() => app.ui.push({ screen: "next_up" })); await ready();
  const waiting = tasks().getByRole("heading", { name: task(id).title }).closest("li")!;
  expect(within(waiting).getByRole("img", { name: `From chat “${chatTitle()}”` })).toBeTruthy();
  act(() => { app.ui.back(); }); await ready();
  // Once it ends, its row under Finished does.
  host().emit({ type: "task_upserted", task: { ...task(id), state: "completed" } });
  await ready();
  fireEvent.click(tasks().getByRole("radio", { name: /^Finished/ })); await ready(100);
  const row = tasks().getByRole("button", { name: new RegExp(`^${task(id).title}`) });
  expect(within(row).getByRole("img", { name: `From chat “${chatTitle()}”` })).toBeTruthy();
  fireEvent.click(row); await ready();
  fireEvent.click(tasks().getByRole("button", { name: `From chat “${chatTitle()}”. Open the chat` }));
  await ready();
  expect(app.ui.get().tab).toBe("talk");
  expect(shown()).toBe(ids.tOneCard);
});

it("keeps the chat a task is turned from when it goes through Review first", async () => {
  const command = vi.spyOn(app.session, "command");
  act(() => { draftNewTask(app.ui, ids.pTide, { text: "Show upload progress beside the file name.", mode: "work", fromChat: ids.tOneCard }); });
  await ready();
  const go = screen.getAllByRole("button").find((b) => /^(Start task|Add to Next up)$/.test(b.textContent ?? "") && !b.closest(".is-hidden"))!;
  fireEvent.click(go);
  await ready(3000);
  const starts = command.mock.calls.map(([c]) => c).filter((c) => c.command === "create_task");
  expect(starts).toHaveLength(1);
  expect(starts[0]).toMatchObject({ project_id: ids.pTide, from_chat: ids.tOneCard });
  expect(starts[0]).not.toHaveProperty("origin");
  expect(task(ids.tOneCard)).toMatchObject({ conversation: true });
});
