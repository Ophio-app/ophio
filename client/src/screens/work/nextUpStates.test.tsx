// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, useHost, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids, queue as simQueue } from "../../dev/sim/scenario";
import { workIds } from "../../dev/sim/work";
import { HostSession } from "../../host/session";
import type { Command, QueueItem } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Review from "../talk/Review";
import Talk from "../talk/Talk";
import NextUp from "./NextUp";
import Receipt from "./Receipt";
import Work from "./Work";
import { TaskRow } from "./shared";
import { queueItemState } from "./useQueue";

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
function view(child: React.ReactNode, route: Route) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: app.ui.get().stacks.work.indexOf(route), active: true, route }}>{child}</RouteProvider></AppProvider>);
}
const states = () => [...document.querySelectorAll(".work-queue li")].map(li => [li.querySelector("h2")?.textContent, li.querySelector(".work-queue-state")?.textContent, li.querySelector(".btn--primary")?.textContent]);

it("calls only the front item ready and says how many tasks are ahead of the rest", () => {
  const base = simQueue(Date.now()).items[0]!;
  const item = (authorization: QueueItem["authorization"], state: QueueItem["state"]): QueueItem => ({ ...base, authorization, state });
  const askFirst = item("ask_before_start", { state: "awaiting_start" });
  const byItself = item("auto_start", { state: "waiting" });
  expect(queueItemState(askFirst, 0)).toBe("Ready — needs your go-ahead");
  expect(queueItemState(byItself, 0)).toBe("Waiting");
  expect(queueItemState(askFirst, 1)).toBe("After 1 earlier task");
  expect(queueItemState(byItself, 1)).toBe("After 1 earlier task");
  expect(queueItemState(askFirst, 3)).toBe("After 3 earlier tasks");
  expect(queueItemState(byItself, 2)).toBe("After 2 earlier tasks");
  for (const index of [0, 2]) expect(queueItemState(item("ask_before_start", { state: "blocked", reason: "Waiting for an earlier task" }), index)).toBe("Blocked — Waiting for an earlier task");
});

it("Next up offers Review and start only on the item that can start next", async () => {
  const host = window.__sim!.host;
  // No writer holds the project, so only the queue decides what starts.
  host.queue = { ...host.queue, writer: null };
  host.emit({ type: "queue_changed", queue: host.queue });
  app.ui.selectTask(ids.pTide, null);
  view(<NextUp route={{ screen: "next_up" }} />, { screen: "next_up" });
  await tick(600);
  // The front starts by itself; two ask-first items wait behind it.
  expect(states()).toEqual([
    ["Polish empty states", "Waiting", "Review and start"],
    ["Document the new empty states", "Blocked — Waiting for an earlier task", "Review"],
    ["Review keyboard navigation", "After 2 earlier tasks", "Review"],
    ["Update README install steps for the new CLI", "After 3 earlier tasks", "Review"],
  ]);
  void app.session.command({ command: "queue_move", item_id: workIds.qReady, position: 1 }, { label: "Move", expectedRevision: host.queue.revision });
  await tick(600);
  expect(states()).toEqual([
    ["Review keyboard navigation", "Ready — needs your go-ahead", "Review and start"],
    ["Polish empty states", "After 1 earlier task", "Review"],
    ["Document the new empty states", "Blocked — Waiting for an earlier task", "Review"],
    ["Update README install steps for the new CLI", "After 3 earlier tasks", "Review"],
  ]);
});

it("a slow start says Starting… until its Review closes, never that an earlier task waits", async () => {
  const host = window.__sim!.host;
  const item = host.queue.items.find(i => i.state.state !== "started")!;
  host.queue = { ...host.queue, paused: true, pause_reason: "Paused for review.", writer: null };
  host.projects = host.projects.map(p => ({ ...p, active_writer: null }));
  for (const project of host.projects) host.emit({ type: "project_upserted", project });
  host.emit({ type: "queue_changed", queue: host.queue });
  const route: Route = { screen: "review", projectId: host.queue.project_id, taskId: item.task_id, queueItemId: item.item_id };
  app.ui.push({ screen: "next_up" }, "work"); app.ui.push(route, "work");
  view(<Review route={route} />, route);
  await tick(600);
  window.__sim!.latency(1500);
  fireEvent.click(screen.getByRole("button", { name: "Start only this task" }));
  let seenStarted = false;
  const seen = new Set<string>();
  while (app.ui.get().stacks.work.includes(route)) {
    await tick(50);
    if (!app.ui.get().stacks.work.includes(route)) break;
    seenStarted ||= app.session.state.get().queues[route.projectId]?.items.find(i => i.item_id === item.item_id)?.state.state === "started";
    seen.add(document.querySelector(".review-foot")?.textContent ?? "");
    if (screen.queryByText("The task or queue changed")) seen.add("changed");
  }
  // The phone heard the item start while its own start was still in flight.
  expect(seenStarted).toBe(true);
  expect([...seen]).toEqual(["Only this task will start. The rest of Next up stays paused."
    + "In tidewater-api · Claude Code · Work, the agent’s own permission settings · Starts nowStarting…"]);
  expect(app.ui.get().stacks.work).toEqual([{ screen: "work" }, { screen: "next_up" }]);
});

const send = (body: Command) => window.__sim!.host.command({ command_id: crypto.randomUUID(), issued_at: Date.now(), expires_at: null, expected_revision: null, body });

it("the Stop receipt counts the held messages Stop withdrew and lists them as your messages", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.state === "working" && t.harness === "claude_code")!;
  await send({ command: "send_message", task_id: task.task_id, text: "Also check the p99 line.", bundle: null, delivery: "next_turn" });
  await send({ command: "send_message", task_id: task.task_id, text: "Then screenshot the legend.", bundle: null, delivery: "next_turn" });
  await send({ command: "stop_task", task_id: task.task_id });
  await tick(1500);
  const route = { screen: "receipt", taskId: task.task_id } as const;
  app.ui.push(route, "work");
  view(<Receipt route={route} />, route);
  await tick(900);
  const outcomes = within(screen.getByRole("list", { name: "Stop outcomes" })).getAllByRole("listitem").map(li => li.textContent);
  expect(outcomes).toContain("Held messages withdrawn · 2 · Confirmed");
  expect(screen.getByRole("heading", { name: "Your messages in this run" })).toBeTruthy();
  expect(screen.queryByText("Annotated requests")).toBeNull();
  expect(screen.getAllByText("Withdrawn by Stop · not sent")).toHaveLength(2);
});

/** A row's state word, the first line beneath its title, and whether its icon asks for the person. */
function row(scope: ParentNode, title: string) {
  const found = [...scope.querySelectorAll(".row")].find(r => r.querySelector(".row__title")?.textContent === title);
  if (!found) throw new Error(`No row for ${title}`);
  const work = found.querySelector(".work-row-state");
  return {
    state: work ? work.textContent?.split(" · ")[0] : [...found.querySelectorAll(".pill")].map(p => p.textContent).join(" · "),
    line: (work ? found.querySelector(".work-row-line") : found.querySelector(".row__subtitle"))?.textContent,
    asks: !!found.querySelector(".row__icon [class*=message-circle-question]"),
  };
}
function DashRow() {
  const task = useHost(s => s.tasks[ids.tDash]!);
  return <TaskRow task={task} />;
}

it("a task waiting on the person says Needs you and why, not Working, on Tasks, under Needs you alone and opened in Chat", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const take = app.session.command({ command: "take_over", geometry_revision: 3, target_id: ids.tgDp1 }, { label: "Take over" });
  await tick(1600); await take;
  const leave = app.session.command({ command: "release_lease", lease_id: app.session.state.get().control[ids.tgDp1]!.lease!.lease_id }, { label: "Leave" });
  await tick(500); await leave;
  // As on the computer, the task is still reported working while its input waits to be given back.
  const host = window.__sim!.host, before = host.tasks.find(t => t.task_id === ids.tDash)!;
  const reported = { ...before, state: "working" as const, status_line: "Looked at DP-1", revision: before.revision + 1 };
  host.tasks = host.tasks.map(t => t.task_id === ids.tDash ? reported : t);
  host.emit({ type: "task_upserted", task: reported });
  await tick();
  expect(app.session.state.get().tasks[ids.tDash]?.state).toBe("working");
  const dash = "Check the staging dashboard renders the new latency chart", flaky = "Fix the flaky retry test in the payments client";
  const waiting = "Agent paused on Screen 1 (DP-1) · Resume agent", approval = "Approval needed";

  const work = view(<DashRow />, { screen: "work" });
  expect(row(document, dash)).toEqual({ state: "Needs you", line: waiting, asks: true });
  work.unmount();

  app.ui.selectTask(ids.pTide, ids.tDash);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);
  await tick(900);
  expect(document.querySelector(".chat-head__state")?.textContent).toBe("Needs you");
  cleanup();

  const route: Route = { screen: "work" };
  view(<Work route={route} />, route);
  await tick();
  fireEvent.click(screen.getByRole("radio", { name: /^Open/ }));
  // Under Needs you with why, and only there: Running says where they went.
  const section = (name: string) => screen.getByRole("heading", { name }).closest("section")!;
  const needs = section("Needs you").textContent!;
  expect(needs).toContain(dash);
  expect(needs).toContain(waiting);
  expect(needs).toContain(flaky);
  expect(needs).toContain(approval);
  expect(section("Running").textContent).toBe("Running2 tasks running here need you, so they’re under Needs you.Live agentsEvery agent working on this computer");
});

it("moves tapped over a slow link show at once, keep the buttons usable, and reach the computer one after another", async () => {
  const host = window.__sim!.host;
  host.queue = { ...host.queue, writer: null };
  host.emit({ type: "queue_changed", queue: host.queue });
  app.ui.selectTask(ids.pTide, null);
  view(<NextUp route={{ screen: "next_up" }} />, { screen: "next_up" });
  await tick(600);
  const revision = host.queue.revision;
  const readme = "Update README install steps for the new CLI";
  const up = () => within([...document.querySelectorAll<HTMLElement>(".work-queue li")].find((li) => li.querySelector("h2")?.textContent === readme)!)
    .getByRole<HTMLButtonElement>("button", { name: "Move up" });
  const order = () => states().map(([title]) => title);
  window.__sim!.latency(1500);
  fireEvent.click(up());
  await tick(100);
  expect(order()).toEqual(["Polish empty states", "Document the new empty states", readme, "Review keyboard navigation"]);
  expect(up().disabled).toBe(false);
  fireEvent.click(up());
  await tick(100);
  const moved = ["Polish empty states", readme, "Document the new empty states", "Review keyboard navigation"];
  expect(order()).toEqual(moved);
  await tick(8000);
  expect(order()).toEqual(moved);
  expect(host.queue.revision).toBe(revision + 2);
  expect(Object.values(app.session.commands.get()).filter((c) => c.phase === "rejected")).toEqual([]);
});
