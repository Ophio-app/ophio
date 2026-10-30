// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { Toasts } from "../design/Toast";
import { CommandError, HostSession, type PendingCommand } from "../host/session";
import type { NotificationPermission } from "../host/link";
import { AlertsOffer, TaskAlerts } from "../shell/AlertsOffer";
import { RouteProvider } from "../shell/Screen";
import { Notifications } from "../screens/settings/Notifications";
import { notificationSettings } from "./notificationSettings";
import { appLock } from "./lock";
import { notificationAccess, notificationDetail, taskAlertsOffer, TaskAlertsOffer, viewingTask, watchAttention } from "./notifications";

let app: App;
let stop = () => {};
const advance = async (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
function visibility(value: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, value });
  document.dispatchEvent(new Event("visibilitychange"));
}
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "talk", route: { screen: "talk" }, depth: 0, active: true }}>{children}</RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  visibility("visible");
  notificationDetail.set("minimal");
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing fixture profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start();
  await advance(900);
  stop = watchAttention(platform, session, profile.name);
});
afterEach(() => {
  cleanup(); stop(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); visibility("visible");
});
function permission(value: NotificationPermission) { window.__sim?.device.notificationPermission(value); }
function command(id: string, phase: PendingCommand["phase"] = "accepted", kind: "resume_task" | "send_message" = "resume_task") {
  const c: PendingCommand = { id, body: kind === "resume_task" ? { command: kind, task_id: ids.tHeaders, text: "Continue" } : { command: kind, task_id: ids.tHeaders, text: "Follow up", bundle: null, delivery: "next_turn" }, label: "Continue", issuedAt: Date.now(), phase, error: null, receipt: { command_id: id, status: "accepted", seq: 1, result: null, error: null } };
  act(() => app.session.commands.set((s) => ({ ...s, [id]: c })));
}
function miss(id: string) {
  const original = app.session.state.get().attention[ids.atReady];
  if (!original) throw new Error("Missing attention fixture");
  act(() => app.session.state.set((s) => ({ ...s, attention: { ...s.attention, [id]: { ...original, attention_id: id } } })));
}
function offer() { render(<><TaskAlerts /><AlertsOffer projectId={ids.pTide} taskId={ids.tHeaders} /></>, { wrapper }); }

it("asks only after an accepted task action, remembers Not now across reload, and keeps profiles separate", async () => {
  permission("promptable"); offer(); await advance();
  expect(screen.queryByText(/Get an alert/)).toBeNull();
  command("pending", "sending"); await advance();
  expect(screen.queryByText(/Get an alert/)).toBeNull();
  command("pending", "rejected"); await advance();
  expect(screen.queryByText(/Get an alert/)).toBeNull();
  command("continue"); await advance();
  expect(screen.getByText(/Get an alert when this/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Not now" }));
  command("next", "accepted", "send_message"); await advance();
  expect(screen.queryByText(/Get an alert/)).toBeNull();
  const reloaded = new TaskAlertsOffer(app.profile.profileId);
  const off = reloaded.watch(app.session, notificationAccess(app.platform));
  command("after-reload"); await advance();
  expect(reloaded.get()).toBeNull(); off();
  const other = new TaskAlertsOffer("another-computer");
  const offOther = other.watch(app.session, notificationAccess(app.platform));
  command("another"); await advance();
  expect(other.get()?.taskId).toBe(ids.tHeaders); offOther();
});

it("Allow alerts calls the foreground request and disappears only on confirmed grant", async () => {
  permission("promptable"); offer(); command("start"); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Allow alerts" })); await advance();
  expect(window.__sim?.device.notificationRequests).toBe(1);
  expect(notificationAccess(app.platform).get().permission).toBe("granted");
  expect(screen.queryByText(/Get an alert/)).toBeNull();
});

it.each(["granted", "blocked"] as const)("%s does not offer or invoke permission on a task action", async (value) => {
  permission(value); offer(); command("start"); await advance();
  expect(screen.queryByText(/Get an alert/)).toBeNull();
  expect(window.__sim?.device.notificationRequests).toBe(0);
});

it("accepted new task offers in its exact task context, queued work does not", async () => {
  permission("promptable"); offer(); await advance();
  const send = (mode: "queue" | "start_now") => {
    const id = mode;
    act(() => app.session.commands.set((s) => ({ ...s, [id]: { id, label: "Start", issuedAt: Date.now(), phase: "accepted", error: null,
      body: { command: "create_task", project_id: ids.pTide, kind: "coding", title: null, prompt: "Inspect", harness: "codex", model: null, effort: null, mission: null, target_id: null, bundle: null, placement: mode === "queue" ? { mode, position: null, authorization: "ask_before_start" } : { mode } },
      receipt: { command_id: id, status: "accepted", seq: 1, error: null, result: { kind: "task_created", task_id: ids.tHeaders, queue_item_id: null } },
    } })));
  };
  send("queue"); await advance(); expect(screen.queryByText(/Get an alert/)).toBeNull();
  send("start_now"); await advance(); expect(taskAlertsOffer(app.session).get()).toEqual({ projectId: ids.pTide, taskId: ids.tHeaders });
});

it("settings displays all three real states and refreshes on foreground return", async () => {
  permission("promptable"); render(<Notifications />, { wrapper }); await advance();
  expect(screen.getByText("Alerts are off")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Allow alerts" })).toBeTruthy();
  act(() => visibility("hidden")); permission("blocked"); act(() => visibility("visible")); await advance();
  expect(screen.getByText("Alerts are blocked in Android settings")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Open Android settings" })); await advance();
  expect(window.__sim?.device.notificationSettingsOpened).toBe(1);
  act(() => visibility("hidden")); permission("granted"); act(() => visibility("visible")); await advance();
  expect(screen.getByText("Alerts are on")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Allow alerts" })).toBeNull();
});

it("denial is not reported as permission granted and query failure stays unknown", async () => {
  permission("promptable"); window.__sim?.device.notificationRequestResult("blocked");
  render(<Notifications />, { wrapper }); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Allow alerts" })); await advance();
  expect(screen.queryByText("Alerts are on")).toBeNull();
  expect(screen.getByText("Alerts are blocked in Android settings")).toBeTruthy();
  vi.spyOn(app.platform, "notificationPermission").mockRejectedValue(new Error("Unavailable"));
  act(() => visibility("visible")); await advance();
  expect(screen.getByText(/couldn’t check whether task alerts/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Check alerts again" })).toBeTruthy();
});

it.each(["promptable", "blocked"] as const)("two rejected background alerts become one %s recovery banner", async (state) => {
  permission(state); render(<TaskAlerts />, { wrapper }); await advance();
  act(() => visibility("hidden")); miss("one"); miss("two"); await advance();
  expect(screen.queryByLabelText("Missed task alerts")).toBeNull();
  act(() => visibility("visible")); await advance();
  expect(screen.getAllByLabelText("Missed task alerts")).toHaveLength(1);
  expect(screen.getByText(/2 alerts, including “tidewater-api”, couldn’t be shown because alerts are off/)).toBeTruthy();
  expect(screen.getByRole("button", { name: state === "blocked" ? "Open Android settings" : "Allow alerts" })).toBeTruthy();
  expect(window.__sim?.device.notifications).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(screen.queryByLabelText("Missed task alerts")).toBeNull();
});

it("respects task-title detail, and reports other delivery failures without blaming permission", async () => {
  notificationDetail.set("task_title");
  vi.spyOn(app.platform, "notify").mockRejectedValue(new Error("Unavailable"));
  render(<TaskAlerts />, { wrapper }); await advance();
  act(() => visibility("hidden")); miss("failure"); await advance(); act(() => visibility("visible")); await advance();
  const task = app.session.state.get().tasks[ids.tHeaders];
  expect(screen.getByLabelText("Missed task alerts").textContent).toContain(task?.title);
  expect(screen.getByLabelText("Missed task alerts").textContent).not.toContain("because alerts are off");
});

it("enabling alerts keeps the missed event visible without claiming alerts are still off", async () => {
  permission("promptable"); render(<TaskAlerts />, { wrapper }); await advance();
  act(() => visibility("hidden")); miss("missed"); await advance();
  act(() => visibility("visible")); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Allow alerts" })); await advance();
  expect(screen.getByLabelText("Missed task alerts").textContent).toContain("because alerts were off");
  expect(screen.queryByText("Alerts are off")).toBeNull();
  expect(screen.queryByRole("button", { name: "Allow alerts" })).toBeNull();
});

it("looks for new attention only when the attention record changes, and still alerts for a new item", async () => {
  let reads = 0;
  act(() => app.session.state.set((s) => ({ ...s, attention: new Proxy(s.attention, { ownKeys: (target) => { reads++; return Reflect.ownKeys(target); } }) })));
  reads = 0;
  for (let i = 1; i <= 3; i++) act(() => app.session.state.set((s) => ({ ...s, lastHeardAt: Date.now() + i })));
  expect(reads).toBe(0);
  const notify = vi.spyOn(app.platform, "notify");
  visibility("hidden");
  miss("new-attention");
  await advance();
  expect(notify).toHaveBeenCalledTimes(1);
});

it("never raises what the computer raised quietly, as part of something that alerts once", async () => {
  const notify = vi.spyOn(app.platform, "notify");
  visibility("hidden");
  const original = app.session.state.get().attention[ids.atReady]!;
  act(() => app.session.state.set((s) => ({ ...s, attention: { ...s.attention, held: { ...original, attention_id: "held", quiet: true } } })));
  await advance();
  expect(notify).not.toHaveBeenCalled();
  miss("told");
  await advance();
  expect(notify).toHaveBeenCalledTimes(1);
});

it("holds back what the computer's settings turn off, and folds a project's alerts into one note that leads to Needs you", async () => {
  const device = window.__sim!.device;
  const profileId = app.profile.profileId;
  const settle = async (body: Parameters<HostSession["command"]>[0], expectedRevision: number) => {
    const done = app.session.command(body, { label: "Notifications", expectedRevision });
    await advance(900);
    await act(async () => { await done; });
    await advance(900);
  };
  const raise = (id: string, kind: "completed" | "question" = "completed") => {
    const original = app.session.state.get().attention[ids.atReady]!;
    act(() => app.session.state.set((s) => ({ ...s, attention: { ...s.attention, [id]: { ...original, attention_id: id, kind } } })));
  };
  const answer = (id: string) => act(() => app.session.state.set((s) => ({ ...s, attention: { ...s.attention, [id]: { ...s.attention[id]!, resolved_at: Date.now() } } })));
  await settle({ command: "set_notification_settings", off: ["finished"], bundle: false, projects: [] }, 0);
  visibility("hidden");
  raise("held"); await advance();
  expect(device.notifications).toHaveLength(0);

  // The project turns finished tasks back on for itself, as one note.
  await settle({ command: "set_notification_settings", off: ["finished"], bundle: false, projects: [{ project_id: ids.pTide, mute: false, on: ["finished"], off: [], bundle: true }] }, 1);
  raise("one"); await advance();
  expect(device.notifications.map((n) => n.id)).toEqual([`${profileId}/one`]);
  raise("two"); await advance();
  const note = `${profileId}/project:${ids.pTide}`;
  expect(device.cancelled).toContain(`${profileId}/one`);
  expect(device.notifications.at(-1)).toEqual(expect.objectContaining({
    id: note, title: "tidewater-api has 2 notifications", body: `${app.profile.name} · 2 to look at`,
    route: { kind: "needs_you", profileId, projectId: ids.pTide },
  }));
  // A question alerts again; the note leads with it.
  const before = device.cancelled.filter((id) => id === note).length;
  raise("ask", "question"); await advance();
  expect(device.cancelled.filter((id) => id === note).length).toBe(before + 1);
  expect(device.notifications.at(-1)).toEqual(expect.objectContaining({ id: note, title: "tidewater-api has 3 notifications", body: `${app.profile.name} · 1 need you, 2 to look at`, category: "needs_you" }));
  // Answered, the note counts again, and goes with the last.
  answer("ask"); answer("one"); await advance();
  expect(device.notifications.at(-1)).toEqual(expect.objectContaining({ id: note, title: "tidewater-api has 1 notification" }));
  answer("two"); await advance();
  expect(device.cancelled.at(-1)).toBe(note);
});

it("opening a task takes its alerts out of the shade, answered or not, and they don't come back", async () => {
  const device = window.__sim!.device;
  const profileId = app.profile.profileId;
  const original = app.session.state.get().attention[ids.atReady]!;
  const taskId = original.task_id!;
  visibility("hidden");
  act(() => app.session.state.set((s) => ({ ...s, attention: { ...s.attention, ask: { ...original, attention_id: "ask", kind: "question" } } })));
  await advance();
  expect(device.notifications.map((n) => n.id)).toContain(`${profileId}/ask`);
  // Unlocked on the screen, as the app is once opened.
  const offLock = appLock(app.platform).start();
  visibility("visible"); await advance();
  const close = viewingTask(profileId, taskId);
  await advance();
  expect(device.cancelled).toContain(`${profileId}/ask`);
  // Still unanswered, it stays under Needs you but never alerts again.
  expect(app.session.state.get().attention.ask?.resolved_at).toBeNull();
  close();
  const shown = device.notifications.length;
  visibility("hidden"); await advance();
  expect(device.notifications).toHaveLength(shown);
  offLock();
});

it("a choice in Settings shows on the tap over a slow link, stays usable, and goes back with a toast when refused", async () => {
  window.__sim!.latency(1500);
  render(<><Notifications /><Toasts /></>, { wrapper });
  await advance(3000);
  const box = (name: string) => screen.getByRole("checkbox", { name }) as HTMLInputElement;
  expect(box("Questions").checked).toBe(true);
  fireEvent.click(box("Questions"));
  await advance(100);
  expect(box("Questions").checked).toBe(false);
  // Usable while the first is on its way; the second goes once it is answered.
  expect(box("Approvals").disabled).toBe(false);
  fireEvent.click(box("Approvals"));
  await advance(100);
  expect(box("Approvals").checked).toBe(false);
  await advance(8000);
  const read = app.session.query({ kind: "notification_settings" });
  await advance(2000);
  expect((await read).view).toMatchObject({ off: expect.arrayContaining(["question", "approval"]), revision: 2 });
  expect(box("Questions").checked).toBe(false);
  expect(box("Approvals").checked).toBe(false);

  const refused = vi.spyOn(app.session, "command").mockRejectedValueOnce(new CommandError({ code: "stale_revision", message: "" }, null));
  fireEvent.click(box("Finished tasks"));
  expect(box("Finished tasks").checked).toBe(false);
  await advance();
  expect(refused).toHaveBeenCalledOnce();
  expect(box("Finished tasks").checked).toBe(true);
  expect(screen.getByText("Couldn’t change finished tasks alerts. It changed on the computer. Review the current version first.")).toBeTruthy();
});

it("raises report alerts without a task and respects the Reports topic", async () => {
  await advance(200);
  visibility("hidden");
  const sample = app.session.state.get().attention[ids.atReady]!;
  const raise = (id: string) => act(() => app.session.state.set(s => ({ ...s, attention: { ...s.attention, [id]: { ...sample, attention_id: id, task_id: null, report_id: "report", kind: "question", topic: "reports", quiet: false } } })));
  raise("report-alert"); await advance(100);
  expect(window.__sim!.device.notifications.find(n => n.id.endsWith("/report-alert"))?.route).toEqual({ kind: "report", profileId: app.profile.profileId, reportId: "report" });
  act(() => notificationSettings(app.session).set({ view: { off: ["reports"], projects: [], bundle: false, revision: 1 }, error: null }));
  raise("quiet-report-alert"); await advance(100);
  expect(window.__sim!.device.notifications.some(n => n.id.endsWith("/quiet-report-alert"))).toBe(false);
});
