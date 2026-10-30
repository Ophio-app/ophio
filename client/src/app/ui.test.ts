// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it, vi } from "vitest";
import { homeTab, TABS, UiStore } from "./ui";

beforeEach(() => localStorage.clear());

it("recovers issue drafts after a temporary storage read failure", () => {
  const seed = new UiStore("issues");
  seed.saveIssueDraft("p", { title: "Keep this report", body: "Steps to reproduce" });
  seed.retire();
  const read = vi.spyOn(Storage.prototype, "getItem").mockImplementationOnce(() => { throw new Error("Unavailable"); });
  const current = new UiStore("issues");
  read.mockRestore();
  current.selectTab("work");
  expect(current.persistNow()).toBe(true);
  const reopened = new UiStore("issues");
  expect(reopened.get().issueDrafts.p).toMatchObject({ title: "Keep this report", body: "Steps to reproduce" });
  current.retire(); reopened.retire();
});

it("preserves a competing issue draft before replacing it after storage recovers", () => {
  const seed = new UiStore("issues");
  seed.saveIssueDraft("p", { title: "Earlier report", body: "Earlier details" });
  seed.retire();
  const original = localStorage.getItem("ui.issues");
  const read = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("Unavailable"); });
  const current = new UiStore("issues");
  current.saveIssueDraft("p", { title: "New report" });
  expect(current.persistNow()).toBe(false);
  read.mockRestore();
  const setItem = Storage.prototype.setItem;
  const write = vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (this: Storage, key, value) {
    if (key === "ui.issues.issue-drafts.recovered") throw new Error("Full");
    setItem.call(this, key, value);
  });
  expect(current.persistNow()).toBe(false);
  expect(localStorage.getItem("ui.issues")).toBe(original);
  expect(current.get().issueDrafts.p?.title).toBe("New report");
  write.mockRestore();
  expect(current.persistNow()).toBe(true);
  expect(JSON.parse(localStorage.getItem("ui.issues.issue-drafts.recovered")!).p).toMatchObject({ title: "Earlier report", body: "Earlier details" });
  expect(current.get().issueDrafts.p?.title).toBe("New report");
  expect(current.recovery.get()).toContain("earlier issue draft");
  const reopened = new UiStore("issues");
  expect(reopened.get().issueDrafts.p?.title).toBe("New report");
  current.retire(); reopened.retire();
});

it("recovers saved chat drafts before flushing after an initial read failure", () => {
  const original = new UiStore("read-failure");
  original.saveDraft("p", "t", { text: "unsent chat" });
  original.retire();
  const read = vi.spyOn(Storage.prototype, "getItem").mockImplementationOnce(() => {
    throw new Error("Storage temporarily unavailable");
  });
  const ui = new UiStore("read-failure");
  read.mockRestore();
  ui.selectTab("work");
  ui.persistNow();
  const reopened = new UiStore("read-failure");
  expect(reopened.draft("p", "t")?.text).toBe("unsent chat");
  ui.retire();
  reopened.retire();
});

it("a sent review closes with the annotation it carried, landing on the conversation", () => {
  const ui = new UiStore("finish");
  ui.push({ screen: "annotate", annotationId: "a" });
  ui.push({ screen: "review", projectId: "p", taskId: "t" });
  ui.finish(["review", "annotate"]);
  expect(ui.get().stacks.talk).toEqual([{ screen: "talk" }]);
});

it("finishing leaves the routes beneath the flow in place", () => {
  const ui = new UiStore("finish");
  ui.push({ screen: "desktop", targetId: "d" }, "control");
  ui.push({ screen: "annotate", annotationId: "a" });
  ui.push({ screen: "review", projectId: "p", taskId: "t" });
  ui.finish(["review", "annotate"]);
  expect(ui.get().stacks.control).toEqual([{ screen: "control" }, { screen: "desktop", targetId: "d" }]);
  ui.finish(["review", "annotate"]);
  expect(ui.get().stacks.control).toHaveLength(2);
});

it("a takeover asked for elsewhere is claimed once, by its display, and never survives a reload", () => {
  const ui = new UiStore("takeover");
  ui.requestTakeover("d");
  expect(ui.takeoverRequested("other")).toBe(false);
  expect(ui.claimTakeover("other")).toBe(false);
  expect(ui.takeoverRequested("d")).toBe(true);
  window.dispatchEvent(new Event("pagehide"));
  expect(new UiStore("takeover").takeoverRequested("d")).toBe(false);
  expect(ui.claimTakeover("d")).toBe(true);
  expect(ui.claimTakeover("d")).toBe(false);
});

it("Back from a conversation in Chat goes to its project's conversations, choosing one leaves them, and the list outlasts a reload (D-065)", () => {
  const ui = new UiStore("tasks");
  ui.selectTask("p", "t");
  ui.push({ screen: "instructions", taskId: "t" });
  ui.back("conversation");
  expect(ui.get()).toMatchObject({ taskList: false, stacks: { talk: [{ screen: "talk" }] } });
  expect(ui.canGoBack("conversation")).toBe(true);
  ui.back("conversation");
  expect(ui.get()).toMatchObject({ tab: "talk", taskList: true, taskByProject: { p: "t" } });
  expect(ui.canGoBack("list")).toBe(false);
  ui.selectProject("q");
  expect(ui.get().taskList).toBe(true);
  ui.selectTask("q", null);
  expect(ui.get().taskList).toBe(false);
  ui.push({ screen: "settings" }, "work");
  ui.showTaskList();
  expect(ui.get()).toMatchObject({ tab: "talk", taskList: true, stacks: { talk: [{ screen: "talk" }], work: [{ screen: "work" }, { screen: "settings" }] } });
  window.dispatchEvent(new Event("pagehide"));
  expect(new UiStore("tasks").get().taskList).toBe(true);
});

it("Back from a task, or a new task, in Chat goes to Tasks's tasks; Chat showing its conversations is the root (D-065)", () => {
  const ui = new UiStore("tasks");
  ui.selectTask("p", "t");
  ui.back("task");
  expect(ui.get()).toMatchObject({ tab: "work", taskList: true });
  ui.back("list");
  expect(ui.get().tab).toBe("talk");
  expect(ui.canGoBack("list")).toBe(false);
  ui.newConversation("p");
  expect(ui.canGoBack("conversation")).toBe(true);
  // A conversation just sent is still one while it arrives.
  ui.selectCreated("p", "new");
  expect(ui.get().conversing).toBe(true);
});

it("Back from a task opened on Tasks goes back to Tasks, as it was, and leaves the task shown in Chat", () => {
  const ui = new UiStore("returns");
  ui.selectTask("p", "a");
  ui.push({ screen: "github", projectId: "p" }, "work");
  ui.openTask("p", "b");
  expect(ui.get()).toMatchObject({ tab: "talk", taskList: false, taskByProject: { p: "b" } });
  ui.push({ screen: "instructions", taskId: "b" });
  ui.back();
  expect(ui.get().tab).toBe("talk");
  ui.leaveTask();
  expect(ui.get()).toMatchObject({ tab: "work", taskByProject: { p: "b" }, stacks: { work: [{ screen: "work" }, { screen: "github", projectId: "p" }] } });
  // Leaving clears the way back: Talk's back arrow now goes to the task list.
  ui.selectTab("talk");
  ui.leaveTask();
  expect(ui.get()).toMatchObject({ tab: "talk", taskList: true });
});

it("a task opened from another task goes back to that task; one opened from the task list goes back to the list", () => {
  const ui = new UiStore("returns");
  ui.selectTask("p", "a");
  ui.openTask("q", "b");
  ui.back();
  expect(ui.get()).toMatchObject({ tab: "talk", projectId: "p", taskList: false, taskByProject: { p: "a", q: "b" } });
  ui.back();
  expect(ui.get().taskList).toBe(true);
  ui.openTask("q", "b");
  ui.leaveTask();
  expect(ui.get()).toMatchObject({ tab: "talk", taskList: true });
});

it("a screen pushed onto another tab goes back to the tab it came from, after its own steps back", () => {
  const ui = new UiStore("returns");
  ui.push({ screen: "github", projectId: "p" }, "work");
  ui.push({ screen: "review", projectId: "p", taskId: null, intent: "new_task" }, "talk");
  ui.push({ screen: "annotate", annotationId: "x" });
  expect(ui.canGoBack()).toBe(true);
  ui.back();
  expect(ui.route()).toEqual({ screen: "review", projectId: "p", taskId: null, intent: "new_task" });
  ui.back();
  expect(ui.get().tab).toBe("work");
  expect(ui.route()).toEqual({ screen: "github", projectId: "p" });
  expect(ui.get().stacks.talk).toEqual([{ screen: "talk" }]);
});

it("a task started from a Review opened elsewhere still goes back there; choosing a tab by hand forgets it", () => {
  const ui = new UiStore("returns");
  ui.push({ screen: "github", projectId: "p" }, "work");
  ui.selectTask("p", null);
  ui.push({ screen: "review", projectId: "p", taskId: null, intent: "new_task" }, "talk");
  ui.finish(["review"], "talk");
  ui.selectCreated("p", "t");
  ui.leaveTask();
  expect(ui.get()).toMatchObject({ tab: "work", taskByProject: { p: "t" } });
  ui.openTask("p", "t");
  ui.selectTab("library");
  ui.selectTab("talk");
  ui.back();
  expect(ui.get()).toMatchObject({ tab: "talk", taskList: true });
});

it("the way back outlasts a reload", () => {
  const ui = new UiStore("returns");
  ui.push({ screen: "next_up" }, "work");
  ui.openTask("p", "t");
  window.dispatchEvent(new Event("pagehide"));
  const again = new UiStore("returns");
  again.back();
  expect(again.get()).toMatchObject({ tab: "work", stacks: { work: [{ screen: "work" }, { screen: "next_up" }] } });
});

it("a deep stack keeps its tab's home screen at the bottom, so Back always reaches it", () => {
  const ui = new UiStore("deep");
  ui.selectTab("library");
  for (let i = 0; i < 20; i++) ui.push({ screen: "folder", rootId: "r", path: `d${i}` });
  const stack = ui.get().stacks.library;
  expect(stack[0]).toEqual({ screen: "library" });
  expect(stack.at(-1)).toEqual({ screen: "folder", rootId: "r", path: "d19" });
  expect(stack).toHaveLength(12);
  for (let i = 1; i < stack.length; i++) ui.back();
  expect(ui.route()).toEqual({ screen: "library" });
  expect(ui.get().tab).toBe("library");
});

it("a saved stack that lost its home screen gets it back on restart", () => {
  const settings = Array.from({ length: 12 }, (_, i) => ({ screen: "settings", section: `s${i}` }));
  localStorage.setItem("ui.saved", JSON.stringify({ version: 2, state: { tab: "talk", stacks: { talk: settings } } }));
  const ui = new UiStore("saved");
  const stack = ui.get().stacks.talk;
  expect(stack[0]).toEqual({ screen: "talk" });
  expect(stack.slice(1)).toEqual(settings.slice(-11));
  expect(ui.get().stacks.library).toEqual([{ screen: "library" }]);
});

it("a saved screen this version doesn't have is dropped on restart", () => {
  // An earlier version, rolled back to, doesn't know a newer version's screens.
  localStorage.setItem("ui.unknown-screen", JSON.stringify({ version: 2, state: {
    tab: "control", stacks: { control: [{ screen: "control" }, { screen: "terminal", terminalId: "term" }, { screen: "versions" }],
      work: [{ screen: "work" }, { screen: "not_yet_made" }, { screen: "settings" }] },
    returns: { control: { tab: "work", depth: 3 } },
  } }));
  const ui = new UiStore("unknown-screen");
  expect(ui.get().stacks.control).toEqual([{ screen: "control" }, { screen: "terminal", terminalId: "term" }]);
  expect(ui.get().stacks.work).toEqual([{ screen: "work" }, { screen: "settings" }]);
  expect(ui.route()).toEqual({ screen: "terminal", terminalId: "term" });
  expect(ui.get().returns.control).toBeUndefined();
  ui.retire();
});

it("a deep stack preserves the return to the tab that opened it", () => {
  const ui = new UiStore("deep-return");
  ui.push({ screen: "settings" }, "library");
  for (let i = 0; i < 20; i++) ui.push({ screen: "folder", rootId: "r", path: `d${i}` });
  for (let i = 0; i < 11; i++) ui.back();
  expect(ui.route()).toEqual({ screen: "library" });
  ui.back();
  expect(ui.get().tab).toBe("talk");
});

it("going up returns to a destination already open in the tab, and opens it otherwise", () => {
  const ui = new UiStore("up");
  const folder = { screen: "folder", rootId: "r", path: "a" } as const;
  ui.selectTab("library");
  ui.up(folder);
  expect(ui.get().stacks.library).toEqual([{ screen: "library" }, folder]);
  const open = ui.get().stacks.library[1];
  ui.push({ screen: "folder", rootId: "r", path: "a/b" });
  ui.push({ screen: "file", rootId: "r", path: "a/b/c.md" });
  ui.up({ screen: "folder", rootId: "r", path: "a" });
  expect(ui.get().stacks.library).toEqual([{ screen: "library" }, folder]);
  // The screen returned to is the one that was open, so it keeps its place.
  expect(ui.get().stacks.library[1]).toBe(open);
  const state = ui.get();
  ui.up(folder);
  expect(ui.get()).toBe(state);
});

it("going up matches the root and path in the active tab and keeps the nearest occurrence", () => {
  const ui = new UiStore("up-routes");
  const folder = { screen: "folder", rootId: "r", path: "a" } as const;
  ui.push(folder);
  const talk = ui.get().stacks.talk;
  ui.selectTab("library");
  ui.push({ ...folder, rootId: "other" });
  ui.up(folder);
  expect(ui.get().stacks.library).toHaveLength(3);
  ui.push({ ...folder, path: "a/b" });
  ui.push(folder);
  const nearest = ui.get().stacks.library;
  ui.push({ screen: "file", rootId: "r", path: "a/file.md" });
  ui.up(folder);
  expect(ui.get().stacks.library).toEqual(nearest);
  expect(ui.get().stacks.talk).toBe(talk);
});

it("going above a cross-tab destination clears its return, including after reload", () => {
  const ui = new UiStore("up-return");
  const folder = { screen: "folder", rootId: "r", path: "a" } as const;
  ui.selectTab("library");
  ui.push(folder);
  ui.selectTab("work");
  ui.push({ screen: "file", rootId: "r", path: "a/file.md" }, "library");
  ui.up(folder);
  expect(ui.get().returns.library).toBeUndefined();
  ui.persistNow();
  const again = new UiStore("up-return");
  again.push({ ...folder, path: "a/b" });
  again.back();
  expect(again.get().tab).toBe("library");
  expect(again.route()).toEqual(folder);
  again.back();
  expect(again.route()).toEqual({ screen: "library" });
});

it("going up within a cross-tab visit preserves its way back", () => {
  const ui = new UiStore("up-within-return");
  const folder = { screen: "folder", rootId: "r", path: "a" } as const;
  ui.selectTab("work");
  ui.push(folder, "library");
  ui.push({ ...folder, path: "a/b" });
  ui.push({ screen: "file", rootId: "r", path: "a/b/file.md" });
  ui.up(folder);
  ui.back();
  expect(ui.get().tab).toBe("work");
  expect(ui.get().stacks.library).toEqual([{ screen: "library" }]);
});

it("keeps the fifth To-Do tab and All selection across a restart, with older four-tab state repaired", () => {
  localStorage.setItem("ui.todo-migrate", JSON.stringify({ version: 2, state: {
    tab: "work", stacks: { talk: [{ screen: "talk" }], control: [{ screen: "control" }],
      work: [{ screen: "work" }], library: [{ screen: "library" }] }, projectId: "p"
  } }));
  const ui = new UiStore("todo-migrate");
  expect(TABS).toEqual(["talk", "control", "work", "library", "todo"]);
  expect(ui.get().stacks.todo).toEqual([{ screen: "todo" }]);
  ui.selectTab("todo");
  ui.setTodoAll(true);
  ui.persistNow(); ui.retire();
  const reopened = new UiStore("todo-migrate");
  expect(reopened.get()).toMatchObject({ tab: "todo", todoAll: true, projectId: "p" });
  expect(reopened.route()).toEqual({ screen: "todo" });
  reopened.selectProject("q");
  expect(reopened.get()).toMatchObject({ projectId: "q", todoAll: false });
  reopened.retire();
});

it("opens linked checklist items in To-Do, selecting their project and exiting All", () => {
  const ui = new UiStore("todo-link");
  ui.selectTab("work");
  ui.setTodoAll(true);
  ui.openChecklist("p", "item-a");
  expect(homeTab({ screen: "checklist", projectId: "p" })).toBe("todo");
  expect(ui.get()).toMatchObject({ tab: "todo", projectId: "p", todoAll: false, todoItemId: "item-a" });
  expect(ui.route()).toEqual({ screen: "todo" });
  ui.setTodoItem(null);
  ui.push({ screen: "checklist", projectId: "q", itemId: "item-b" }, "work");
  expect(ui.get()).toMatchObject({ tab: "todo", projectId: "q", todoItemId: "item-b" });
  ui.openChecklist("q");
  expect(ui.get()).toMatchObject({ tab: "todo", projectId: "q", todoItemId: null });
  ui.retire();
});

it("moves an active legacy checklist overlay to To-Do without losing its focused item", () => {
  localStorage.setItem("ui.legacy-checklist", JSON.stringify({ version: 2, state: {
    tab: "work", projectId: "before", stacks: { talk: [{ screen: "talk" }], control: [{ screen: "control" }],
      work: [{ screen: "work" }, { screen: "checklist", projectId: "p", itemId: "item-a" }], library: [{ screen: "library" }] }
  } }));
  const ui = new UiStore("legacy-checklist");
  expect(ui.get()).toMatchObject({ tab: "todo", projectId: "p", todoItemId: "item-a", todoAll: false });
  expect(ui.get().stacks.work).toEqual([{ screen: "work" }]);
  ui.retire();
});

it("migrates a checklist opened from Chat and removes dormant legacy routes in every tab", () => {
  localStorage.setItem("ui.legacy-chat-checklist", JSON.stringify({ version: 2, state: {
    tab: "talk", projectId: "before", todoAll: true, drafts: { "before/new": { projectId: "before", taskId: null, text: "Keep my words" } },
    stacks: { talk: [{ screen: "talk" }, { screen: "checklist", projectId: "p", itemId: "focus" }],
      control: [{ screen: "control" }, { screen: "checklist", projectId: "other" }, { screen: "terminal", terminalId: "term" }],
      work: [{ screen: "work" }, { screen: "settings" }, { screen: "checklist", projectId: "old" }],
      library: [{ screen: "library" }, { screen: "folder", rootId: "r", path: "kept" }] }
  } }));
  const ui = new UiStore("legacy-chat-checklist");
  expect(ui.get()).toMatchObject({ tab: "todo", projectId: "p", todoItemId: "focus", todoAll: false });
  expect(ui.get().stacks.talk).toEqual([{ screen: "talk" }]);
  expect(ui.get().stacks.control).toEqual([{ screen: "control" }, { screen: "terminal", terminalId: "term" }]);
  expect(ui.get().stacks.work).toEqual([{ screen: "work" }, { screen: "settings" }]);
  expect(ui.get().stacks.library).toHaveLength(2);
  expect(ui.get().drafts["before/new"]?.text).toBe("Keep my words");
  ui.retire();
});

it("Back from a To-Do opened in Tasks returns to the exact Tasks destination", () => {
  const ui = new UiStore("todo-return");
  ui.selectTab("work");
  ui.push({ screen: "next_up" });
  ui.openChecklist("p", "blocked");
  expect(ui.get().returns.todo).toEqual({ tab: "work", depth: 1 });
  ui.back();
  expect(ui.get().tab).toBe("work");
  expect(ui.route()).toEqual({ screen: "next_up" });
  ui.selectTab("talk");
  ui.openChecklist("q");
  ui.back();
  expect(ui.get().tab).toBe("talk");
  ui.retire();
});

it("following work in another project retains All, but choosing its project explicitly exits All", () => {
  const ui = new UiStore("todo-context");
  ui.selectProject("p");
  ui.selectTab("todo");
  ui.setTodoAll(true);
  ui.contextProject("q");
  ui.push({ screen: "next_up" }, "work");
  expect(ui.get()).toMatchObject({ tab: "work", projectId: "q", todoAll: true });
  ui.back();
  expect(ui.get()).toMatchObject({ tab: "todo", todoAll: true });
  ui.selectProject("q");
  expect(ui.get().todoAll).toBe(false);
  ui.retire();
});
