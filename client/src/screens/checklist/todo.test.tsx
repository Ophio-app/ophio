// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useTodoBadge } from "../../app/checklist";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { Toasts } from "../../design/Toast";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { Checklist, ChecklistItem } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Todo from "./Todo";
import { checklistOrder, compareTodo, type TodoOrder } from "./todoOrder";

let app: App;
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
const sim = () => window.__sim!.host;
const route = { screen: "todo" } as const;
const headerName = () => document.querySelector(".ctx__project-name")!.textContent;
// Switches project, or to All projects, through the header's project picker.
function pick(name: RegExp) {
  fireEvent.click(document.querySelector(".ctx__context")!);
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name }));
}
function Badge() { return <output data-testid="todo-badge">{useTodoBadge()}</output>; }
function rejectNextChecklistRead(projectId: string) {
  const original = app.session.query.bind(app.session);
  let rejected = 0;
  vi.spyOn(app.session, "query").mockImplementation(((request: { kind: string; project_id?: string }) => {
    if (request.kind === "checklist" && request.project_id === projectId) {
      rejected++;
      return Promise.reject(new Error("Checklist refresh unavailable"));
    }
    return original(request as never);
  }) as typeof app.session.query);
  return () => rejected;
}

beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers(); vi.setSystemTime(new Date("2026-10-18T09:00:00"));
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.stubGlobal("crypto", { getRandomValues: crypto.getRandomValues.bind(crypto), randomUUID: crypto.randomUUID.bind(crypto) });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
});
afterEach(() => { cleanup(); app.session.stop(); sim().dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); });

async function view(withBadge = false) {
  app.ui.selectTab("todo");
  const result = render(<AppProvider app={app}><RouteProvider value={{ tab: "todo", depth: 0, active: true, route }}><Todo route={route} />{withBadge && <Badge />}<Toasts /></RouteProvider></AppProvider>);
  await tick(); await tick();
  return result;
}

it("switches projects and All through the header picker, without a chip row", async () => {
  app.ui.selectProject(ids.pNotes);
  await view();
  expect(screen.queryByRole("group", { name: "To-Do projects" })).toBeNull();
  expect(headerName()).toBe("field-notes");
  pick(/^tidewater-api/);
  expect(app.ui.get().projectId).toBe(ids.pTide);
  expect(headerName()).toBe("tidewater-api");
  pick(/^All projects/);
  expect(app.ui.get().todoAll).toBe(true);
  expect(headerName()).toBe("All projects");
  expect(screen.getByRole("heading", { name: "All to-dos" })).toBeTruthy();
});

it("persists All locally and returns to a project when the header project changes", async () => {
  app.ui.selectProject(ids.pTide);
  await view();
  pick(/^All projects/);
  expect(screen.getByRole("heading", { name: "All to-dos" })).toBeTruthy();
  expect(app.ui.get().todoAll).toBe(true);
  app.ui.persistNow();
  const restored = new UiStore(app.profile.profileId);
  expect(restored.get().todoAll).toBe(true);
  act(() => app.ui.selectProject(ids.pNotes));
  expect(app.ui.get().todoAll).toBe(false);
  expect(headerName()).toBe("field-notes");
  expect(screen.queryByRole("heading", { name: "All to-dos" })).toBeNull();
});

it("sorts cross-project All by blocker, due date, priority, then stable project and item ids", () => {
  const item = (item_id: string, fields: Partial<ChecklistItem> = {}): ChecklistItem => ({ item_id, title: item_id, priority: "medium", steps: [], added_at: 1, changed_at: 1, ...fields });
  const list = (project_id: string, items: ChecklistItem[]): Checklist => ({ project_id, title: project_id, sections: [{ section_id: "s", title: "s", items }], revision: 1, created_at: 1, updated_at: 1 });
  const a = checklistOrder(list("a", [item("later", { priority: "high" }), item("due", { due: "2026-10-18", priority: "low" }), item("block", { priority: "blocker" })]), "2026-10-18");
  const b = checklistOrder(list("b", [item("first", { priority: "high" }), item("done", { priority: "blocker", done_at: 2 }), item("early", { due: "2026-10-17" })]), "2026-10-18");
  const order: TodoOrder[] = [...b, ...a, { id: "agent", projectId: "b", tier: 0 }];
  expect(order.sort(compareTodo).map(({ projectId, id }) => `${projectId}/${id}`)).toEqual(["a/block", "b/agent", "b/early", "a/due", "a/later", "b/first", "b/done"]);
  expect(compareTodo({ id: "a", projectId: "a", tier: 2 }, { id: "a", projectId: "b", tier: 2 })).toBeLessThan(0);
});

it("drops an agent waiting row and badge immediately on an optimistic completed prerequisite", async () => {
  app.ui.selectProject(ids.pNotes);
  await view(true);
  act(() => app.session.state.set((s) => ({ ...s, attention: Object.fromEntries(Object.entries(s.attention).filter(([, a]) => a.task_id !== ids.tNotes)), tasks: { ...s.tasks, [ids.tNotes]: { ...s.tasks[ids.tNotes]!, state: "working" } } })));
  const badgeBefore = Number(screen.getByTestId("todo-badge").textContent);
  expect(badgeBefore).toBeGreaterThan(0);
  expect(screen.getByText(/Waiting on you to finish “Review the proposed topic tags”/)).toBeTruthy();
  const person = [...document.querySelectorAll(".cl-item__title")].find((node) => node.textContent === "Review the proposed topic tags")!.closest("li")!;
  fireEvent.click(within(person).getByRole("button", { name: "Done" }));
  expect(screen.queryByText(/Waiting on you to finish “Review the proposed topic tags”/)).toBeNull();
  expect(Number(screen.getByTestId("todo-badge").textContent)).toBe(badgeBefore - 1);
});

it("restores a rejected optimistic tick and its badge, and explains the refusal", async () => {
  app.ui.selectProject(ids.pNotes);
  await view(true);
  act(() => app.session.state.set((s) => ({ ...s, attention: Object.fromEntries(Object.entries(s.attention).filter(([, a]) => a.task_id !== ids.tNotes)), tasks: { ...s.tasks, [ids.tNotes]: { ...s.tasks[ids.tNotes]!, state: "working" } } })));
  const before = Number(screen.getByTestId("todo-badge").textContent);
  const person = [...document.querySelectorAll(".cl-item__title")].find((node) => node.textContent === "Review the proposed topic tags")!.closest("li")!;
  vi.spyOn(app.session, "command").mockRejectedValueOnce(new Error("The computer refused this tick"));
  fireEvent.click(within(person).getByRole("button", { name: "Done" }));
  expect(Number(screen.getByTestId("todo-badge").textContent)).toBe(before - 1);
  await tick();
  expect(Number(screen.getByTestId("todo-badge").textContent)).toBe(before);
  expect(screen.getByText("The computer refused this tick")).toBeTruthy();
});

it("keeps a confirmed tick optimistic when the follow-up checklist read fails", async () => {
  app.ui.selectProject(ids.pNotes);
  await view(true);
  const before = Number(screen.getByTestId("todo-badge").textContent);
  const person = [...document.querySelectorAll(".cl-item__title")].find((node) => node.textContent === "Review the proposed topic tags")!.closest("li")!;
  const rejected = rejectNextChecklistRead(ids.pNotes);
  fireEvent.click(within(person).getByRole("button", { name: "Done" }));
  await tick(); await tick();
  expect(rejected()).toBeGreaterThan(0);
  expect(Number(screen.getByTestId("todo-badge").textContent)).toBe(before - 1);
  expect(within(person).getByRole("checkbox", { name: /Open “Review the proposed topic tags” again/ }).getAttribute("aria-checked")).toBe("true");
  expect(screen.queryByText("Checklist refresh unavailable")).toBeTruthy(); // A read notice, not a failed command toast.
  expect(document.querySelector(".toast--stop")?.textContent ?? "").not.toContain("Checklist refresh unavailable");
});

it("clears sent feedback despite a failed checklist refresh, preventing duplicate resend", async () => {
  app.ui.selectProject(ids.pNotes);
  await view();
  const agent = [...document.querySelectorAll(".cl-item__title")].find((node) => node.textContent === "Apply the approved tags")!.closest("li")!;
  const toggle = within(agent).getByRole("button", { name: /Apply the approved tags/ });
  fireEvent.click(toggle);
  fireEvent.click(within(agent).getByRole("button", { name: "Send feedback" }));
  fireEvent.change(within(agent).getByLabelText("Send feedback"), { target: { value: "Keep the tags short" } });
  const rejected = rejectNextChecklistRead(ids.pNotes);
  fireEvent.click(within(agent).getByRole("button", { name: "Send feedback" }));
  await tick(); await tick();
  expect(rejected()).toBeGreaterThan(0);
  expect(within(agent).queryByLabelText("Send feedback")).toBeNull();
  expect(document.querySelector(".toast--stop")?.textContent ?? "").not.toContain("Checklist refresh unavailable");
  expect(sim().checklists.lists.get(ids.pNotes)!.sections[0]!.items.find((item) => item.item_id === "apply-tags")!.notes?.filter((note) => note.text === "Keep the tags short")).toHaveLength(1);
});

it("does not let a stale read remove a newer tick while two refreshes overlap", async () => {
  const notes = sim().checklists.lists.get(ids.pNotes)!;
  const second: ChecklistItem = { ...notes.sections[0]!.items[0]!, item_id: "second-review", title: "Independent review", steps: [], task_id: undefined, waiting: "Second agent handoff" };
  sim().checklists.lists.set(ids.pNotes, { ...notes, sections: [{ ...notes.sections[0]!, items: [...notes.sections[0]!.items, second] }] });
  app.ui.selectProject(ids.pNotes);
  await view(true);
  const before = Number(screen.getByTestId("todo-badge").textContent);
  const reads: { resolve: (value: { kind: "checklist"; checklist: Checklist }) => void; snapshot: Checklist }[] = [];
  const originalQuery = app.session.query.bind(app.session);
  vi.spyOn(app.session, "query").mockImplementation(((request: { kind: string; project_id?: string }) => {
    if (request.kind !== "checklist" || request.project_id !== ids.pNotes) return originalQuery(request as never);
    return new Promise<{ kind: "checklist"; checklist: Checklist }>((resolve) => reads.push({ resolve, snapshot: structuredClone(sim().checklists.lists.get(ids.pNotes)!) }));
  }) as typeof app.session.query);
  const person = [...document.querySelectorAll(".cl-item__title")].find((node) => node.textContent === "Review the proposed topic tags")!.closest("li")!;
  fireEvent.click(within(person).getByRole("button", { name: "Done" }));
  await tick();
  expect(reads.length).toBeGreaterThan(0);
  const other = [...document.querySelectorAll(".cl-item__title")].find((node) => node.textContent === "Independent review")!.closest("li")!;
  fireEvent.click(within(other).getByRole("button", { name: /Independent review/ }));
  fireEvent.click(within(other).getByRole("button", { name: "Done" }));
  await tick();
  expect(reads.length).toBeGreaterThan(1);
  const wasDone = (list: Checklist, id: string) => list.sections.some((section) => section.items.some((item) => item.item_id === id && item.done_at !== undefined));
  expect(wasDone(reads[0]!.snapshot, "second-review")).toBe(false);
  expect(wasDone(reads.at(-1)!.snapshot, "second-review")).toBe(true);
  expect(Number(screen.getByTestId("todo-badge").textContent)).toBe(before - 2);
  // Resolve every older response with the snapshot from before the second tick.
  for (const read of reads.slice(0, -1)) await act(async () => read.resolve({ kind: "checklist", checklist: read.snapshot }));
  expect(Number(screen.getByTestId("todo-badge").textContent)).toBe(before - 2);
  const latest = reads.at(-1)!;
  await act(async () => latest.resolve({ kind: "checklist", checklist: latest.snapshot }));
  expect(Number(screen.getByTestId("todo-badge").textContent)).toBe(before - 2);
});

it("keeps pending ticks scoped to their project when item ids coincide in All", async () => {
  const tide = sim().checklists.lists.get(ids.pTide)!;
  const notes = sim().checklists.lists.get(ids.pNotes)!;
  const sameId = notes.sections[0]!.items[0]!;
  sim().checklists.lists.set(ids.pTide, { ...tide, sections: tide.sections.map((section, index) => index === 0
    ? { ...section, items: [...section.items, { ...sameId, title: "Other project's review", task_id: undefined, waiting: undefined, steps: [] }] }
    : section) });
  app.ui.selectProject(ids.pNotes);
  await view();
  pick(/^All projects/);
  const row = (title: string) => [...document.querySelectorAll(".todo-all .cl-item__title")].find((node) => node.textContent === title)?.closest("li");
  const other = row("Other project's review");
  expect(other).toBeTruthy();
  // Leave the command in flight to assert against the shared optimistic cache.
  vi.spyOn(app.session, "command").mockImplementationOnce(() => new Promise(() => {}));
  const review = row("Review the proposed topic tags")!;
  fireEvent.click(within(review).getByRole("checkbox", { name: /Tick off “Review the proposed topic tags”/ }));
  expect(within(other!).getByRole("checkbox", { name: /Tick off “Other project's review”/ }).getAttribute("aria-checked")).toBe("false");
  expect(app.ui.get().todoAll).toBe(true);
});

it("responds to host checklist revisions and agent task completion while still mounted", async () => {
  app.ui.selectProject(ids.pNotes);
  await view();
  act(() => app.session.state.set((s) => ({ ...s, attention: Object.fromEntries(Object.entries(s.attention).filter(([, a]) => a.task_id !== ids.tNotes)), tasks: { ...s.tasks, [ids.tNotes]: { ...s.tasks[ids.tNotes]!, state: "working" } } })));
  expect(screen.getByText(/Waiting on you to finish “Review the proposed topic tags”/)).toBeTruthy();
  const original = sim().checklists.lists.get(ids.pNotes)!;
  sim().checklists.lists.set(ids.pNotes, { ...original, revision: original.revision + 1, sections: original.sections.map((section) => ({ ...section, items: section.items.map((item) => item.item_id === "review-tags" ? { ...item, done_at: Date.now() } : item) })) });
  act(() => app.session.state.set((s) => ({ ...s, checklistsRevision: s.checklistsRevision + 1, checklistProjects: { ...s.checklistProjects, [ids.pNotes]: (s.checklistProjects[ids.pNotes] ?? 0) + 1 } })));
  await tick(); await tick();
  expect(screen.queryByText(/Waiting on you to finish “Review the proposed topic tags”/)).toBeNull();
  act(() => app.session.state.set((s) => ({ ...s, tasks: { ...s.tasks, [ids.tNotes]: { ...s.tasks[ids.tNotes]!, state: "completed", revision: s.tasks[ids.tNotes]!.revision + 1 } } })));
  expect(screen.queryByText("Tag last week's notes by topic")).toBeNull();
});

it("does not read or show To-Do entries without the tasks grant", async () => {
  window.__sim!.permissions({ tasks: false }); await tick(); await tick();
  await view(true);
  expect(screen.getByTestId("todo-badge").textContent).toBe("0");
  expect(screen.getByText(/see and tick off checklists/i)).toBeTruthy();
  expect(screen.queryByRole("group", { name: "To-Do projects" })).toBeNull();
});

it("shows a connection notice instead of empty success while All cannot read offline", async () => {
  app.ui.setTodoAll(true);
  window.__sim!.offline("Computer disconnected");
  await tick();
  await view();
  expect(screen.getByText(`Connect to ${app.profile.name} to read the checklists.`)).toBeTruthy();
  expect(screen.queryByText("Nothing to do here")).toBeNull();
  expect(screen.queryByText("Reading the checklists…")).toBeNull();
});

it("shows the named project and Retry when a Next up query fails", async () => {
  app.ui.selectProject(ids.pTide);
  const original = app.session.query.bind(app.session);
  vi.spyOn(app.session, "query").mockImplementation(((request: { kind: string; project_id?: string }) =>
    request.kind === "queue" && request.project_id === ids.pTide
      ? Promise.reject(new Error("Queue temporarily unavailable")) : original(request as never)
  ) as typeof app.session.query);
  await view();
  expect(screen.getByText("Next up · tidewater-api")).toBeTruthy();
  expect(screen.getByText("Queue temporarily unavailable")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
});

it("shows legacy host agent entries without checklist_v1, without asking for checklist data", async () => {
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== "checklist_v1") } })));
  app.ui.selectProject(ids.pTide);
  const query = vi.spyOn(app.session, "query");
  await view();
  expect(query.mock.calls.some(([request]) => request.kind === "checklist" || request.kind === "checklists")).toBe(false);
  expect(screen.getByText("Update the computer")).toBeTruthy();
  expect(screen.getByText("Fix the flaky retry test in the payments client")).toBeTruthy();
  expect(screen.getByText("Update README install steps for the new CLI")).toBeTruthy();
});

it("shows failed first reads in All even without a checklist or task for that project", async () => {
  app.ui.selectProject(ids.pTide);
  app.ui.setTodoAll(true);
  act(() => app.session.state.set(s => ({ ...s, tasks: Object.fromEntries(Object.entries(s.tasks).filter(([, task]) => task.project_id !== ids.pNotes)), queues: Object.fromEntries(Object.entries(s.queues).filter(([id]) => id !== ids.pNotes)) })));
  rejectNextChecklistRead(ids.pNotes);
  await view();
  expect(screen.getByText("Checklist refresh unavailable")).toBeTruthy();
  expect(screen.getByText("field-notes")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
});

it("refreshes only the changed project's list, and all lists after resync", async () => {
  await view(true);
  const query = vi.spyOn(app.session, "query");
  act(() => app.session.state.set(s => ({ ...s, checklistsRevision: s.checklistsRevision + 1,
    checklistProjects: { ...s.checklistProjects, [ids.pNotes]: (s.checklistProjects[ids.pNotes] ?? 0) + 1 } })));
  await tick();
  expect(query.mock.calls.filter(([r]) => r.kind === "checklist").map(([r]) => (r as { project_id: string }).project_id)).toEqual([ids.pNotes]);
  query.mockClear();
  act(() => app.session.state.set(s => ({ ...s, checklistEpoch: s.checklistEpoch + 1 })));
  await tick();
  expect(query.mock.calls.filter(([r]) => r.kind === "checklist")).toHaveLength(Object.keys(app.session.state.get().projects).length);
});

it("shows All's done items latest first, and not once they're over three days done", async () => {
  const tide = sim().checklists.lists.get(ids.pTide)!;
  const now = Date.now(), day = 24 * 3_600_000;
  const done = (item_id: string, title: string, done_at: number): ChecklistItem => ({ item_id, title, priority: "medium", steps: [], added_at: 1, changed_at: 1, done_at });
  sim().checklists.lists.set(ids.pTide, { ...tide, sections: tide.sections.map((section, index) => ({ ...section,
    items: [...section.items.filter((item) => item.item_id !== "dns"), ...(index === 0 ? [done("older", "Done two days ago", now - 2 * day), done("newer", "Done an hour ago", now - day / 24), done("aged", "Done four days ago", now - 4 * day)] : [])],
  })) });
  app.ui.setTodoAll(true);
  await view();
  fireEvent.click(screen.getByRole("button", { name: /^Done \(/ }));
  const titles = [...document.querySelectorAll(".todo-done .cl-item__title")].map((node) => node.textContent);
  expect(titles.indexOf("Done an hour ago")).toBeLessThan(titles.indexOf("Done two days ago"));
  expect(titles).not.toContain("Done four days ago");
});
