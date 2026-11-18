// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { taskOnScreen, talkShows } from "../../app/taskOnScreen";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import { stopStore } from "../control/stops";
import { appLock, unavailableLock } from "../../platform/lock";
import type { Task } from "../../protocol/types";
import Talk from "./Talk";

let app: App;
const tick = (ms = 100) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const otherId = "0192f7a1-0000-7000-8000-00000000aaaa";
const oldId = "0192f7a1-0000-7000-8000-00000000aaab";
const archiveId = "0192f7a1-0000-7000-8000-00000000aaac";
let otherProject: string;

beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  appLock(platform).accept(unavailableLock);
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  app.ui.selectProject(ids.pTide);
  app.ui.showTaskList();
  const host = window.__sim!.host;
  otherProject = host.projects.find(p => p.project_id !== ids.pTide && !p.home)!.project_id;
  const sample = host.tasks[0]!;
  const tasks: Task[] = [
    { ...sample, task_id: oldId, project_id: ids.pTide, title: "Earlier conversation", conversation: true, mode: "ask", state: "completed", current_run: null, updated_at: 1000 },
    { ...sample, task_id: otherId, project_id: otherProject, title: "Newest conversation", conversation: true, mode: "ask", state: "completed", current_run: null, updated_at: 3000 },
    { ...sample, task_id: archiveId, project_id: otherProject, title: "Kept conversation", conversation: true, mode: "ask", state: "archived", current_run: null, updated_at: 2000 },
  ];
  act(() => { for (const task of tasks) { host.tasks.push(task); host.emit({ type: "task_upserted", task }); } });
  await tick();
});
afterEach(() => { cleanup(); app.ui.retire(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

async function view() {
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
  await tick();
}
const rows = () => [...document.querySelectorAll(".talk-task-row")];

it("defaults to all conversations once loaded and sorts their project names together", async () => {
  await view();
  expect(screen.getByRole("heading", { level: 1, name: "All projects" })).toBeTruthy();
  expect(rows().map(r => r.querySelector(".row__title")?.textContent)).toEqual(["Newest conversation", "Earlier conversation"]);
  expect(rows().map(r => r.querySelector(".talk-conversation-project")?.textContent)).toEqual([
    app.session.state.get().projects[otherProject]!.name, app.session.state.get().projects[ids.pTide]!.name,
  ]);
  expect(screen.queryByRole("button", { name: /Project to-dos/ })).toBeNull();
});

it("keeps All metadata to project and agent while preserving the full accessible name", async () => {
  await view();
  const row = rows()[0]!;
  const line = row.querySelector(".talk-conversation-line")!;
  expect(line.textContent).toBe(`${app.session.state.get().projects[otherProject]!.name}·Claude Code`);
  expect(line.textContent).not.toContain("Opus");
  expect(screen.getByRole("button", { name: /Newest conversation.*Claude Code.*Opus 5.5.*High/ })).toBeTruthy();
  act(() => app.ui.selectTalkProject(otherProject));
  await tick();
  expect(rows()[0]!.querySelector(".row__subtitle")?.textContent).toBe("Claude Code · Opus 5.5 · High");
});

it("opens the row's own project and returns to All at its prior scroll position", async () => {
  await view();
  const body = document.querySelector(".talk-tasks .screen__body")!;
  body.scrollTop = 120;
  fireEvent.click(screen.getByRole("button", { name: /^Newest conversation/ }));
  await tick();
  expect(app.ui.get()).toMatchObject({ projectId: otherProject, taskByProject: { [otherProject]: otherId }, talkAll: null });
  expect(screen.getByRole("heading", { level: 1, name: /Newest conversation/ })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Conversations/ }));
  await tick();
  expect(screen.getByRole("heading", { level: 1, name: "All projects" })).toBeTruthy();
  expect(document.querySelector(".talk-tasks .screen__body")!.scrollTop).toBe(120);
});

it("keeps Archived selected on returning from another project's conversation", async () => {
  await view();
  fireEvent.click(screen.getByRole("radio", { name: "Archived 1" }));
  await tick();
  expect(rows().map(r => r.querySelector(".row__title")?.textContent)).toEqual(["Kept conversation"]);
  fireEvent.click(screen.getByRole("button", { name: /^Kept conversation/ }));
  await tick();
  fireEvent.click(screen.getByRole("button", { name: /^Conversations/ }));
  await tick();
  expect(screen.getByRole("radio", { name: "Archived 1" }).getAttribute("aria-checked")).toBe("true");
  expect(rows()).toHaveLength(1);
});

it("starts in the selected project, names it beforehand, and keeps All", async () => {
  app.ui.setTalkAll(true);
  await view();
  const name = app.session.state.get().projects[ids.pTide]!.name;
  const start = screen.getByRole("button", { name: /^New conversation/ });
  expect(start.textContent).toContain(`In ${name}`);
  fireEvent.click(start);
  await tick();
  expect(app.ui.get()).toMatchObject({ conversing: true, projectId: ids.pTide, talkAll: true });
  expect(screen.getByRole("heading", { name })).toBeTruthy();
  expect(screen.queryByText("All projects")).toBeNull();
});

it("uses Home without a selected project, or asks for a project when there is no Home", async () => {
  app.ui.set(s => ({ ...s, projectId: null, talkAll: true }));
  const home = Object.values(app.session.state.get().projects).find(p => p.home);
  await view();
  fireEvent.click(screen.getByRole("button", { name: /^New conversation/ }));
  await tick();
  if (home) expect(app.ui.get()).toMatchObject({ conversing: true, projectId: home.project_id, talkAll: true });
  else {
    const sheet = screen.getByRole("dialog", { name: "Projects" });
    fireEvent.click(within(sheet).getByRole("button", { name: new RegExp(app.session.state.get().projects[ids.pTide]!.name) }));
    await tick();
    expect(app.ui.get()).toMatchObject({ conversing: true, projectId: ids.pTide, talkAll: true });
  }
});

it("does not treat a replying conversation as open while the default All list is shown", async () => {
  app.ui.set(s => ({ ...s, taskList: false }));
  const host = window.__sim!.host;
  const task = { ...host.tasks.find(t => t.task_id === oldId)!, state: "working" as const };
  act(() => host.emit({ type: "task_upserted", task }));
  await tick();
  await view();
  expect(screen.getByRole("heading", { level: 1, name: "All projects" })).toBeTruthy();
  expect(talkShows(app.session.state.get(), app.ui.get())).toBe("list");
  expect(taskOnScreen(app.session.state.get(), app.ui.get())).toBeNull();
});

it("keeps the list and its scope on screen while the phone reconnects", async () => {
  await view();
  expect(screen.getByRole("heading", { level: 1, name: "All projects" })).toBeTruthy();
  // A new connection is not caught up until the computer's fresh copy lands.
  act(() => app.session.state.set(s => ({ ...s, caughtUp: false })));
  await tick();
  expect(screen.queryByLabelText("Loading projects")).toBeNull();
  expect(screen.getByRole("heading", { level: 1, name: "All projects" })).toBeTruthy();
  act(() => app.session.state.set(s => ({ ...s, caughtUp: true })));
  await tick();
  expect(screen.getByRole("heading", { level: 1, name: "All projects" })).toBeTruthy();
});

it("keeps the one-project default and an explicit single-project choice", async () => {
  const original = app.session.state.get().tasks;
  app.session.state.set(s => ({ ...s, tasks: Object.fromEntries(Object.entries(s.tasks).filter(([,t]) => !t.conversation || t.project_id === ids.pTide)) }));
  await view();
  expect(screen.getByRole("heading", { level: 1, name: "tidewater-api" })).toBeTruthy();
  act(() => { app.ui.selectTalkProject(ids.pTide); app.session.state.set(s => ({ ...s, tasks: original })); });
  await tick();
  expect(rows().map(r => r.querySelector(".row__title")?.textContent)).toEqual(["Earlier conversation"]);
});

it("choosing a project stays on its list even when it has a remembered conversation", async () => {
  app.ui.set(s => ({ ...s, taskList: false, taskByProject: { [otherProject]: otherId } }));
  await view();
  fireEvent.click(screen.getByRole("button", { name: "All projects" }));
  await tick();
  const name = app.session.state.get().projects[otherProject]!.name;
  fireEvent.click(within(screen.getByRole("dialog", { name: "Projects" })).getByRole("button", { name: new RegExp(`^${name}`) }));
  await tick();
  expect(screen.getByRole("heading", { level: 1, name })).toBeTruthy();
  expect(rows().map(r => r.querySelector(".row__title")?.textContent)).toEqual(["Newest conversation"]);
  expect(app.ui.get().talkAll).toBe(false);
});

it("asks for a project without Home and starts there without leaving All", async () => {
  app.ui.set(s => ({ ...s, projectId: null, talkAll: true }));
  app.session.state.set(s => ({ ...s, projects: Object.fromEntries(Object.entries(s.projects).filter(([,p]) => !p.home)) }));
  await view();
  fireEvent.click(screen.getByRole("button", { name: /^New conversation/ }));
  await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: "Projects" })).getByRole("button", { name: /^tidewater-api/ }));
  await tick();
  expect(app.ui.get()).toMatchObject({ conversing: true, projectId: ids.pTide, talkAll: true });
  expect(screen.getByRole("heading", { name: "tidewater-api" })).toBeTruthy();
});

it("reads drafts in each conversation's own project", async () => {
  app.ui.saveDraft(otherProject, otherId, { text: "Keep this unsent" });
  await view();
  expect(rows()[0]!.textContent).toContain("Draft");
  expect(rows()[1]!.textContent).not.toContain("Draft");
});

it("marks only an opened conversation's unread reply read, not the whole All list", async () => {
  const item = { attention_id: "reply-other", kind: "completed" as const, title: "New reply", body: null, task_id: otherId, run_id: null, approval_id: null, created_at: 3000, resolved_at: null, priority: 0 };
  app.session.state.set(s => ({ ...s, attention: { ...s.attention, [item.attention_id]: item } }));
  const command = vi.spyOn(app.session, "command");
  await view();
  expect(rows()[0]!.textContent).toContain("New reply");
  const read = () => command.mock.calls.filter(([c]) => c.command === "acknowledge_attention");
  expect(read()).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: /^Newest conversation/ }));
  await tick();
  expect(read().map(([c]) => c)).toEqual([{ command: "acknowledge_attention", attention_id: item.attention_id }]);
});

it("still opens a linked conversation from another tab while All is chosen", async () => {
  app.ui.setTalkAll(true);
  app.ui.selectTab("work");
  app.ui.openTask(otherProject, otherId);
  await view();
  expect(screen.getByRole("heading", { level: 1, name: /Newest conversation/ })).toBeTruthy();
  expect(app.ui.get().talkAll).toBe(true);
  act(() => app.ui.leaveTask());
  expect(app.ui.get()).toMatchObject({ tab: "work", talkAll: true });
});
