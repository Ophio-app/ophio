// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, useUi, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Receipt from "./Receipt";

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
  // Nothing else holds the project, so the finished task can continue.
  const host = window.__sim!.host;
  const task = host.tasks.find((t) => t.task_id === ids.tHeaders)!;
  const free = { ...host.projects.find((p) => p.project_id === task.project_id)!, active_writer: null };
  host.projects = host.projects.map((p) => p.project_id === free.project_id ? free : p);
  host.emit({ type: "project_upserted", project: free });
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});
function Front() {
  const s = useUi((s) => s);
  const route = s.stacks[s.tab].at(-1)!;
  return <RouteProvider value={{ tab: s.tab, route, depth: s.stacks[s.tab].length - 1, active: true }}>
    {route.screen === "receipt" ? <Receipt route={route} /> : <p>{route.screen}</p>}
  </RouteProvider>;
}

it("retests a finished task from its receipt as it's tapped, keeping the person's words, and follows it in Chat", async () => {
  app.ui.reset("work");
  app.ui.push({ screen: "receipt", taskId: ids.tHeaders });
  const task = app.session.state.get().tasks[ids.tHeaders]!;
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Half a thought" });
  render(<AppProvider app={app}><Front /></AppProvider>);
  await advance();
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(screen.getByRole("button", { name: "Retest" }));
  await advance();
  // Sent as tapped: nothing is added to the message the person is writing.
  expect(command).toHaveBeenCalledTimes(1);
  expect(command.mock.calls[0]![0]).toEqual({ command: "resume_task", task_id: task.task_id, text: "Please retest the changes in this task and report the checks you ran." });
  expect(app.ui.draft(task.project_id, task.task_id)?.text).toBe("Half a thought");
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.get().taskByProject[task.project_id]).toBe(task.task_id);
});

it("opens the message box from Continue this task, and Back closes it keeping the draft", async () => {
  app.ui.reset("work");
  app.ui.push({ screen: "receipt", taskId: ids.tHeaders });
  render(<AppProvider app={app}><Front /></AppProvider>);
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  await advance();
  const field = screen.getByRole("textbox") as HTMLTextAreaElement;
  fireEvent.change(field, { target: { value: "Also check the footer" } });
  await advance();
  act(() => { window.systemBack?.(); });
  await advance();
  expect(screen.queryByRole("textbox")).toBeNull();
  expect(app.ui.route()).toMatchObject({ screen: "receipt", taskId: ids.tHeaders });
  fireEvent.click(screen.getByRole("button", { name: "Continue your draft" }));
  await advance();
  expect((screen.getByRole("textbox") as HTMLTextAreaElement).value).toBe("Also check the footer");
});

it("the bar's Back folds the message box first, as system Back does, when it would not close the screen", async () => {
  app.ui.reset("work");
  app.ui.push({ screen: "review", projectId: app.session.state.get().tasks[ids.tHeaders]!.project_id, taskId: ids.tHeaders });
  app.ui.push({ screen: "receipt", taskId: ids.tHeaders });
  render(<AppProvider app={app}><Front /></AppProvider>);
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  await advance();
  expect(screen.getByRole("textbox")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Back/ }));
  await advance();
  expect(screen.queryByRole("textbox")).toBeNull();
  expect(app.ui.route()).toMatchObject({ screen: "receipt", taskId: ids.tHeaders });
  fireEvent.click(screen.getByRole("button", { name: /^Back/ }));
  expect(app.ui.route()).toMatchObject({ screen: "review" });
});

it("offers no composer on an archived task", async () => {
  const host = window.__sim!.host;
  const archived = { ...host.tasks.find((t) => t.task_id === ids.tHeaders)!, state: "archived" as const };
  host.tasks = host.tasks.map((t) => t.task_id === archived.task_id ? archived : t);
  host.emit({ type: "task_upserted", task: archived });
  await advance();
  app.ui.push({ screen: "receipt", taskId: ids.tHeaders });
  render(<AppProvider app={app}><Front /></AppProvider>);
  await advance();
  expect(screen.queryByRole("textbox")).toBeNull();
  expect(screen.queryByRole("button", { name: "Continue this task" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Retest" })).toBeNull();
});
