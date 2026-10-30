// SPDX-License-Identifier: Apache-2.0
// Work's root (D-056): Open, Finished and Archived as tabs, so what is still
// going is never mixed with what has ended. Open leads with Needs you, the
// first thing outlined on its own; Finished archives; Archived is read.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { attentionGroups } from "./attention";
import { hasEndedTask } from "./logic";
import { listedAttention } from "./shared";
import { uuidv7 } from "../../protocol/ids";
import type { AttentionItem } from "../../protocol/types";
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

const tide = () => Object.values(app.session.state.get().tasks).filter((t) => t.project_id === ids.pTide && !t.conversation);
function show() {
  const route = { screen: "work" } as const;
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><Work route={route} /></RouteProvider></AppProvider>);
}

it("opens on Open, with what needs the person first, the first thing outlined alone", async () => {
  const groups = attentionGroups(listedAttention(app.session.state.get(), ids.pTide));
  expect(groups.length).toBeGreaterThan(1);
  const { container } = show();
  await advance();
  const open = tide().filter((t) => t.state !== "archived" && !hasEndedTask(t)).length;
  expect(screen.getByRole("radio", { name: `Open ${open}` }).getAttribute("aria-checked")).toBe("true");
  // The project's name is the screen's heading, and the body doesn't repeat it.
  expect(screen.getByRole("heading", { level: 1 }).textContent).toContain("tidewater-api");
  const lead = container.querySelectorAll(".work-lead");
  expect(lead).toHaveLength(1);
  expect(lead[0]!.textContent).toContain(groups[0]![0]!.title);
  const rest = lead[0]!.nextElementSibling!;
  for (const group of groups.slice(1)) expect(rest.textContent).toContain(group[0]!.title);
  expect(rest.textContent).not.toContain(groups[0]![0]!.title);
  // Nothing that has ended is here: results are Finished's.
  const part = container.querySelector<HTMLElement>(".work-part")!;
  for (const t of tide().filter(hasEndedTask)) expect(part.textContent).not.toContain(t.title);
  expect(screen.queryByRole("button", { name: /^Archive/ })).toBeNull();
});

/** More finished tasks in pTide: two the person opened, and one a pull
 * request held for them waits on, which is unread. */
function moreFinished() {
  const host = window.__sim!.host;
  const base = host.tasks.find((t) => t.task_id === ids.tHeaders)!;
  const read = { ...base, task_id: uuidv7(Date.now()), title: "Trim the changelog", current_run: null, updated_at: Date.now() - 9000 };
  const opened = { ...base, task_id: uuidv7(Date.now() + 3), title: "Pin the linter version", current_run: null, updated_at: Date.now() - 9500 };
  const held = { ...base, task_id: uuidv7(Date.now() + 1), title: "Retry webhooks on 503", current_run: null, updated_at: Date.now() - 8000 };
  const item: AttentionItem = { attention_id: uuidv7(Date.now() + 2), kind: "waiting_for_you", title: "PR #7 needs you", body: "Checks pass; it’s yours to merge",
    task_id: held.task_id, run_id: null, approval_id: null, created_at: Date.now(), resolved_at: null, priority: 60 };
  act(() => {
    host.tasks.push(read, opened, held); host.attention.push(item);
    for (const task of [read, opened, held]) host.emit({ type: "task_upserted", task });
    host.emit({ type: "attention_upserted", item });
  });
  return { read, held };
}

it("lists a finished task under Finished, never Needs you, unread until opened, each part with its own Archive all", async () => {
  const { read, held } = moreFinished();
  const { container } = show();
  await advance();
  // What waits on a finished task is part of its result.
  expect(container.querySelector(".work-part")!.textContent).not.toContain(held.title);
  const finished = tide().filter(hasEndedTask);
  const unread = new Set(listedAttention(app.session.state.get(), ids.pTide, true).flatMap((a) => a.task_id ?? []));
  expect(unread.has(held.task_id)).toBe(true);
  expect(unread.has(read.task_id)).toBe(false);
  fireEvent.click(screen.getByRole("radio", { name: `Finished ${finished.length}` }));
  expect(container.querySelector(".work-lead")).toBeNull();
  const part = (title: string) => screen.getByRole("heading", { name: title }).closest("section")!;
  const [unreadTasks, readTasks] = [finished.filter((t) => unread.has(t.task_id)), finished.filter((t) => !unread.has(t.task_id))];
  expect(unreadTasks.length).toBeGreaterThan(1);
  expect(readTasks.length).toBeGreaterThan(1);
  for (const t of unreadTasks) expect(part("Unread").textContent).toContain(t.title);
  for (const t of readTasks) expect(part("Read").textContent).toContain(t.title);
  for (const t of finished) expect(screen.getByRole("button", { name: `Archive ${t.title}` })).toBeTruthy();

  // Archiving goes ahead at once, with Undo in place of asking first.
  fireEvent.click(screen.getByRole("button", { name: `Archive ${readTasks[0]!.title}` }));
  expect(screen.queryByRole("dialog", { name: /^Archive task/ })).toBeNull();
  await advance(1500);
  expect(app.session.state.get().tasks[readTasks[0]!.task_id]?.state).toBe("archived");
  expect(screen.queryByRole("button", { name: `Archive ${readTasks[0]!.title}` })).toBeNull();

  // Archive all in Read leaves Unread alone.
  const rest = readTasks.slice(1);
  if (rest.length > 1) {
    fireEvent.click(within(part("Read")).getByRole("button", { name: `Archive all ${rest.length} read` }));
    await advance(3000);
  } else {
    fireEvent.click(screen.getByRole("button", { name: `Archive ${rest[0]!.title}` }));
    await advance(1500);
  }
  for (const t of readTasks) expect(app.session.state.get().tasks[t.task_id]?.state).toBe("archived");
  for (const t of unreadTasks) expect(app.session.state.get().tasks[t.task_id]?.state).not.toBe("archived");
  expect(screen.queryByRole("heading", { name: "Read" })).toBeNull();

  fireEvent.click(within(part("Unread")).getByRole("button", { name: `Archive all ${unreadTasks.length} unread` }));
  await advance(3000);
  for (const t of finished) expect(app.session.state.get().tasks[t.task_id]?.state).toBe("archived");
  expect(screen.getByText("Tasks move here when they finish, fail or are stopped.")).toBeTruthy();

  fireEvent.click(screen.getByRole("radio", { name: "Archived" }));
  const archived = container.querySelector<HTMLElement>(".work-part")!;
  for (const t of finished) expect(archived.textContent).toContain(t.title);
  // Archived is read: Undo was the way back, and nothing is archived again.
  expect(within(archived).queryByRole("button", { name: /^Archive|Restore|Unarchive/ })).toBeNull();
});

it("shows a report question inline on Open even though it has no task", async () => {
  window.__sim!.host.reports.view.reports = [{
    report_id: "report", reference: "A7K3QX", created_at: Date.now(), origin: { type: "settings" },
    signature: { code: "none", screen: "/work", build: "sim" }, history: [{ at: Date.now(), status: { state: "question" } }],
    questions: [{ question_id: "report-question", asked_at: Date.now(), template: { template: "happened_again" } }],
  }];
  show(); await advance();
  expect(screen.getByText("Has it happened again since you reported it?")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Yes" })).toBeTruthy();
});

it("puts the checklist under New, and after what needs the person on a screen short for its text", async () => {
  const order = (c: HTMLElement) => {
    const row = c.querySelector(".checklist-row")!, lead = c.querySelector(".work-lead")!;
    return row.compareDocumentPosition(lead) & Node.DOCUMENT_POSITION_FOLLOWING ? "checklist first" : "needs you first";
  };
  const tall = show();
  await advance();
  expect(order(tall.container)).toBe("checklist first");
  expect(tall.container.querySelector(".work-new .checklist-row")).not.toBeNull();
  cleanup();
  const height = window.innerHeight;
  Object.defineProperty(window, "innerHeight", { value: 390, configurable: true });
  try {
    const short = show();
    await advance();
    expect(order(short.container)).toBe("needs you first");
    expect(short.container.querySelector(".work-new .checklist-row")).toBeNull();
  } finally {
    Object.defineProperty(window, "innerHeight", { value: height, configurable: true });
  }
});
