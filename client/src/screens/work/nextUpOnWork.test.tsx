// SPDX-License-Identifier: Apache-2.0
// Next up on Work: its own section under Running, so it can be found before
// anything is queued; with work queued it gives the front task's state and one
// action, since a paused queue starts nothing on its own.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import type { QueueView } from "../../protocol/types";
import Work from "./Work";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
  app.ui.selectProject(ids.pTide); app.ui.selectTab("work");
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

function queueChange(patch: Partial<QueueView>) {
  const host = window.__sim!.host;
  act(() => { host.queue = { ...host.queue, ...patch }; host.emit({ type: "queue_changed", queue: host.queue }); });
}
/** Work, showing Running. */
async function work() {
  const route = { screen: "work" } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><Work route={route} /></RouteProvider></AppProvider>);
  await advance();
  fireEvent.click(screen.getByRole("radio", { name: /^Open/ }));
}
const sections = () => [...document.querySelectorAll(".section__title")].map((h) => h.textContent);
const nextUp = () => within(screen.getByRole("heading", { name: "Next up" }).closest("section")!);

it("shows a paused queue in Open, after Running, with why it waits and Resume queue", async () => {
  queueChange({ paused: true, pause_reason: "Next up starts paused. Resume it to let tasks start." });
  await work();
  expect(sections()).toContain("Running");
  expect(sections().slice(-2)).toEqual(["Next up", "Project"]);
  const waiting = app.session.state.get().queues[ids.pTide]!.items.filter((i) => i.state.state !== "started").sort((a, b) => a.position - b.position);
  const front = waiting[0]!;
  const title = app.session.state.get().tasks[front.task_id]!.title;
  expect(nextUp().getByRole("button", { name: `Open Next up: ${title}`, description: new RegExp(`^Paused · ${waiting.length} waiting · `) })).toBeTruthy();
  expect(nextUp().getByText("Next up starts paused. Resume it to let tasks start.")).toBeTruthy();
  // The project's doors don't repeat the way in.
  expect(screen.queryByRole("button", { name: "Open Next up" })).toBeNull();

  fireEvent.click(nextUp().getByRole("button", { name: "Resume queue" }));
  await advance(900);
  expect(app.session.state.get().queues[ids.pTide]?.paused).toBe(false);
  expect(nextUp().queryByRole("button", { name: "Resume queue" })).toBeNull();
  expect(nextUp().getByRole("button", { name: `Open Next up: ${title}` }).textContent).not.toContain("Paused");

  fireEvent.click(nextUp().getByRole("button", { name: `Open Next up: ${title}` }));
  expect(app.ui.get().stacks.work.at(-1)).toEqual({ screen: "next_up" });
});

it("keeps Next up in Open, after Running, saying how tasks get there, when nothing is queued", async () => {
  queueChange({ items: [], paused: true });
  await work();
  expect(sections()).toContain("Running");
  expect(sections().slice(-2)).toEqual(["Next up", "Project"]);
  // What the row shows is what a screen reader says.
  const open = nextUp().getByRole("button", {
    name: "Open Next up: Nothing waiting",
    description: "Paused · When you review a task, Add to Next up lines it up here",
  });
  expect(within(screen.getByRole("heading", { name: "Project" }).closest("section")!).queryByText(/Next up/)).toBeNull();
  fireEvent.click(open);
  expect(app.ui.get().stacks.work.at(-1)).toEqual({ screen: "next_up" });
});

it("does not say nothing is waiting before the computer has sent the queue", async () => {
  const query = app.session.query.bind(app.session);
  vi.spyOn(app.session, "query").mockImplementation((q) => q.kind === "queue" ? new Promise(() => undefined) : query(q));
  act(() => app.session.state.set((s) => ({ ...s, queues: {} })));
  await work();
  const open = nextUp().getByRole("button", {
    name: "Open Next up: Queue not loaded",
    description: "What is waiting shows here once the computer sends it",
  });
  expect(open.textContent).not.toContain("Nothing waiting");
  // Once it arrives empty, the row says so.
  queueChange({ items: [], paused: false });
  await advance();
  expect(nextUp().getByRole("button", { name: "Open Next up: Nothing waiting" })).toBeTruthy();
});

it("leaves a queued task waiting on the person to Needs you, and leads with the next one", async () => {
  const host = window.__sim!.host;
  const waiting = host.queue.items.filter((i) => i.state.state !== "started").sort((a, b) => a.position - b.position);
  const [front, second] = [waiting[0]!, waiting[1]!];
  const title = (id: string) => host.tasks.find((t) => t.task_id === id)!.title;
  const item = {
    attention_id: "0192f7a1-0000-7000-8000-00000000a7e1", kind: "waiting_for_you" as const, title: "Which branch should it use?",
    body: null, task_id: front.task_id, run_id: null, approval_id: null, created_at: Date.now(), resolved_at: null, priority: 5,
  };
  act(() => { host.attention = [...host.attention, item]; host.emit({ type: "attention_upserted", item }); });
  await work();
  expect(within(screen.getByRole("heading", { name: "Needs you" }).closest("section")!).getByText(title(front.task_id))).toBeTruthy();
  expect(nextUp().getByRole("button", { name: `Open Next up: ${title(second.task_id)}` })).toBeTruthy();
  expect(nextUp().queryByText(title(front.task_id))).toBeNull();
});
