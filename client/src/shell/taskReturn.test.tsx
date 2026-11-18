// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { homeTab, UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { HostSession } from "../host/session";
import { resolveObject } from "../platform/links";
import { Shell } from "./Shell";

let app: App;
let taskId: string;
let taskTitle: string;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  app.ui.selectProject(ids.pTide);
  app.ui.openFirst(false);
  session.start(); await vi.advanceTimersByTimeAsync(900);
  const task = Object.values(session.state.get().tasks).find(t => t.project_id === ids.pTide && t.state === "completed" && !t.conversation)!;
  taskId = task.task_id; taskTitle = task.title;
  render(<AppProvider app={app}><Shell /></AppProvider>);
  await ready();
});
afterEach(() => { cleanup(); app.session.stop(); app.ui.retire(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const ready = async () => {
  await act(async () => { await vi.dynamicImportSettled(); await vi.advanceTimersByTimeAsync(500); });
  await act(async () => { await vi.dynamicImportSettled(); });
};
const back = (path: string) => {
  if (path === "header") fireEvent.click(document.querySelector<HTMLElement>(".chat-head__back")!);
  else act(() => { expect(window.systemBack!()).toBe(true); });
};
const openConversation = async () => {
  await ready();
  expect(app.ui.route().screen).toBe("receipt");
  // Receipt's onSent/onFollow callback, also used after continuing or retesting.
  act(() => app.ui.openTask(ids.pTide, taskId));
  await ready();
  expect(app.ui.route().screen).toBe("talk");
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(taskId);
};

it.each(["header", "Android Back"])("%s closes a finished task to the Tasks list, not its entry receipt", async path => {
  act(() => app.ui.selectTab("work")); await ready();
  fireEvent.click(screen.getByRole("radio", { name: /^Finished/ }));
  fireEvent.click(screen.getByRole("button", { name: new RegExp(`^${taskTitle}`) }));
  await openConversation();
  back(path);
  expect(app.ui.get().tab).toBe("work");
  expect(app.ui.route()).toEqual({ screen: "work" });
  expect(screen.getByRole("button", { name: new RegExp(`^${taskTitle}`) })).toBeTruthy();
});

it.each(["header", "Android Back"])("%s closes a finished task to Live agents", async path => {
  act(() => { app.ui.selectTab("work"); app.ui.push({ screen: "agents" }); }); await ready();
  const card = screen.getByRole("heading", { name: taskTitle }).closest("article")!;
  fireEvent.click(within(card).getByRole("button", { name: "Open receipt" }));
  await openConversation();
  back(path);
  expect(app.ui.get().tab).toBe("work");
  expect(app.ui.route()).toEqual({ screen: "agents" });
});

it.each(["header", "Android Back"])("%s closes a notification's finished task to the screen behind the notification", async path => {
  const host = window.__sim!.host;
  const task = app.session.state.get().tasks[taskId]!;
  const alert = { ...host.attention[0]!, attention_id: "finished-task-alert", task_id: taskId, run_id: task.current_run, approval_id: null, kind: "completed" as const, resolved_at: null };
  act(() => {
    host.emit({ type: "attention_upserted", item: alert });
    app.ui.selectTab("control");
    app.ui.push({ screen: "attention", attentionId: alert.attention_id }, "work");
  });
  await openConversation();
  back(path);
  expect(app.ui.get().tab).toBe("control");
  expect(app.ui.route()).toEqual({ screen: "control" });
});

it.each(["header", "Android Back"])("%s closes a chat's task link back to that chat", async path => {
  act(() => app.ui.openTask(ids.pTide, ids.tDash)); await ready();
  const resolution = resolveObject({ hostId: app.profile.hostId, kind: "task", id: taskId }, app.session);
  await act(async () => { await vi.advanceTimersByTimeAsync(500); });
  const resolved = await resolution;
  expect("route" in resolved && resolved.route).toBeTruthy();
  if (!("route" in resolved) || !resolved.route) throw new Error("Missing task destination");
  act(() => app.ui.push(resolved.route!, homeTab(resolved.route!)));
  await openConversation();
  back(path);
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(ids.tDash);
});

it.each(["header", "Android Back"])("%s keeps a receipt explicitly opened before its conversation", async path => {
  act(() => { app.ui.selectTab("work"); app.ui.push({ screen: "receipt", taskId }); });
  await openConversation();
  back(path);
  expect(app.ui.get().tab).toBe("work");
  expect(app.ui.route()).toEqual({ screen: "receipt", taskId });
});
