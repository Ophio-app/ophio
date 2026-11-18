// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { HostSession } from "../host/session";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { Toasts } from "../design/Toast";
import { notificationDetail } from "../platform/notifications";
import type { Task } from "../protocol/types";
import { AlertsSetup } from "./AlertsSetup";
import { LoginUseNotices } from "./LoginUseNotices";

let app: App;
const advance = (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const device = () => window.__sim!.device;
const ask = () => screen.getByText("Notify me when Øphio is closed?");
function setup(lifetime: object, context = app) {
  return render(<AppProvider app={context}><AlertsSetup lifetime={lifetime} /></AppProvider>);
}
function notice(onOpenTask?: (task: Task) => void) {
  return <AppProvider app={app}><LoginUseNotices onOpenTask={onOpenTask} /><Toasts /></AppProvider>;
}
function used(state: Task["state"] = "working") {
  const task = { ...app.session.state.get().tasks[ids.tFlaky]!, state };
  act(() => app.session.state.set(s => ({ ...s, tasks: { ...s.tasks, [task.task_id]: task }, agentLoginUse: {
    seq: 100, loginId: "notice-login", name: "Test login", site: "example.test", taskId: task.task_id,
  } })));
  return task;
}
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  notificationDetail.set("minimal");
  const platform = createSimPlatform(), profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId),
    switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
  device().setAlerts({ settings: null });
});
afterEach(() => {
  for (const dismiss of screen.queryAllByRole("button", { name: "Dismiss" })) fireEvent.click(dismiss);
  cleanup(); app.session.stop();
  for (const ui of app.ui.navigationStores()) ui.retire();
  window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("an already-visible login notice uses the latest task-opening callback after the active side changes", async () => {
  const rightUi = app.ui.fork("right");
  const left = vi.fn((task: Task) => app.ui.openTask(task.project_id, task.task_id));
  const right = vi.fn((task: Task) => rightUi.openTask(task.project_id, task.task_id));
  const openDefault = vi.spyOn(app.ui, "openTask"), pushDefault = vi.spyOn(app.ui, "push");
  const mounted = render(notice(left));
  const task = used(); await advance();
  expect(screen.getAllByText(/Test login was used on example.test/)).toHaveLength(1);
  mounted.rerender(notice(right)); await advance();
  expect(screen.getAllByText(/Test login was used on example.test/)).toHaveLength(1);
  fireEvent.click(screen.getByRole("button", { name: "Open task" }));
  expect(right).toHaveBeenCalledExactlyOnceWith(task);
  expect(left).not.toHaveBeenCalled();
  expect(openDefault).not.toHaveBeenCalled(); expect(pushDefault).not.toHaveBeenCalled();
  expect(rightUi.get().taskByProject[task.project_id]).toBe(task.task_id);
  expect(app.ui.get().taskByProject[task.project_id]).not.toBe(task.task_id);
});

it.each(["working", "completed"] as const)("keeps default single-view navigation for a %s task when no callback is supplied", async state => {
  const open = vi.spyOn(app.ui, "openTask"), push = vi.spyOn(app.ui, "push");
  render(notice());
  const task = used(state); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Open task" }));
  if (state === "working") expect(open).toHaveBeenCalledWith(task.project_id, task.task_id);
  else {
    expect(open).not.toHaveBeenCalled();
    expect(push).toHaveBeenCalledWith({ screen: "receipt", taskId: task.task_id });
  }
});

it("does not repeat a login notice already observed when the owner remounts on another side", async () => {
  const mounted = render(notice()); used(); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  mounted.unmount(); render(notice()); await advance();
  expect(screen.queryByText(/was used on/)).toBeNull();
});

it("keeps Later dismissed when the global portal remounts during the same app lifetime", async () => {
  const lifetime = {};
  const first = setup(lifetime); await advance(); ask();
  fireEvent.click(screen.getByRole("button", { name: "Close" })); await advance();
  first.unmount(); setup(lifetime); await advance();
  expect(screen.queryByText("Notify me when Øphio is closed?")).toBeNull();
  expect(device().alerts.settings).toBeNull();
});

it("asks again after the actual app lifetime changes, even with the same platform watcher", async () => {
  const first = setup({}); await advance(); ask();
  fireEvent.click(screen.getByRole("button", { name: "Close" })); await advance();
  first.unmount(); setup({}); await advance();
  expect(ask()).toBeTruthy();
});

it("does not carry a dismissal to a different watcher in the same app lifetime", async () => {
  const lifetime = {};
  const first = setup(lifetime); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Close" })); await advance();
  first.unmount();
  setup(lifetime, { ...app, platform: { ...app.platform } }); await advance();
  expect(ask()).toBeTruthy();
});

it("keeps the unsaved Not now radio choice across a side's portal remount", async () => {
  const lifetime = {};
  const first = setup(lifetime); await advance();
  fireEvent.click(screen.getByRole("radio", { name: "Not now" }));
  first.unmount(); setup(lifetime); await advance();
  expect((screen.getByRole("radio", { name: "Not now" }) as HTMLInputElement).checked).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Continue" })); await advance();
  expect(device().alerts.settings).toEqual({ enabled: false, detail: "minimal" });
});

it("retains the current Android permission step and then the battery step across portal remounts", async () => {
  const lifetime = {};
  device().notificationPermission("promptable");
  const first = setup(lifetime); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Continue" })); await advance();
  expect(screen.getByText(/Android asks next whether Øphio may show notifications/)).toBeTruthy();
  first.unmount();
  const second = setup(lifetime); await advance();
  expect(screen.queryByText("Notify me when Øphio is closed?")).toBeNull();
  expect(screen.getByText(/Android asks next whether Øphio may show notifications/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Continue" })); await advance();
  expect(device().notificationRequests).toBe(1);
  expect(screen.getByText(/run without battery limits/)).toBeTruthy();
  second.unmount(); setup(lifetime); await advance();
  expect(screen.getByText(/run without battery limits/)).toBeTruthy();
  expect(device().notificationRequests).toBe(1);
  fireEvent.click(screen.getByRole("button", { name: "Continue" })); await advance();
  expect(device().batteryRequests).toBe(1);
  expect(screen.queryByRole("dialog", { name: "Alerts" })).toBeNull();
});
