// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { Toasts } from "../design/Toast";
import { HostSession, type PendingCommand } from "../host/session";
import type { LocalNotification, NotificationPermission } from "../host/link";
import { recommendedTiers } from "./alertTypes";
import { AlertsOffer, TaskAlerts } from "../shell/AlertsOffer";
import { RouteProvider } from "../shell/Screen";
import { Notifications } from "../screens/settings/Notifications";
import { attentionDestination } from "../screens/work/AttentionScreen";
import { openAttention, replyAttention } from "../state/projection";
import { notificationSettings } from "./notificationSettings";
import { Store } from "../state/store";
import { appLock } from "./lock";
import { deviceTiers } from "./deviceTiers";
import { backgroundAlerts } from "./alerts";
import { choose, notificationFor, parseOpenRoute, notificationAccess, notificationDetail, taskAlertsOffer, TaskAlertsOffer, viewingTask, watchAttention } from "./notifications";

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
  // A finished task stays quiet by default; most of these tests are about how an alert is raised.
  const device = window.__sim!.device;
  device.setAlerts({ tiers: { ...device.alerts.tiers, finished: "alert" } });
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
/** A computer that doesn't know the types, as before: the old permission offer and topic switches hold. */
function older() {
  const caps = app.session.state.get().capabilities!;
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...caps, features: caps.features.filter((f) => f !== "alert_types_v1") } })));
}
function offer() { render(<><TaskAlerts /><AlertsOffer projectId={ids.pTide} taskId={ids.tHeaders} /></>, { wrapper }); }

it("routes a quiet agent update to agent settings only on capable computers", () => {
  const state = app.session.state.get();
  const original = state.attention[ids.atReady];
  if (!original || !state.capabilities) throw new Error("Missing fixture alert");
  const item = { ...original, alert: undefined, task_id: null, run_id: null, agent_update: { harness: "codex" as const, kind: "new_models_available" as const, version: null, models: ["model-x"] } };
  const capable = { ...state, capabilities: { ...state.capabilities, features: [...state.capabilities.features, "agent_update_policy_v1"] } };
  const older = { ...state, capabilities: { ...state.capabilities, features: state.capabilities.features.filter(f => f !== "agent_update_policy_v1") } };
  const settings = notificationSettings(app.session).current();
  const device = { tier: () => "silent" as const };
  expect(choose(item, older, settings, device)).toBeNull();
  expect(choose(item, capable, settings, device)).toEqual({ type: "agent_update", tier: "silent" });
  const note = notificationFor(item, capable, app.profile, "minimal", { type: "agent_update", tier: "silent" });
  expect(note).toMatchObject({ category: "finished", silent: true, route: { kind: "agent_updates", profileId: app.profile.profileId } });
  expect(note?.title).toContain("new model options");
  expect(note?.body).not.toContain("model-x");
  expect(parseOpenRoute(note?.route)).toEqual(note?.route);
  expect(parseOpenRoute({ kind: "agent_updates", profileId: "" })).toBeNull();
  expect(notificationFor(item, older, app.profile, "minimal")).toBeNull();
});

it("asks only after an accepted task action, remembers Not now across reload, and keeps profiles separate", async () => {
  older();
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

it("after an accepted task action, offers to tell you when the task is done, and the bell goes on the task", async () => {
  offer(); await advance();
  expect(screen.queryByText(/Tell you when this/)).toBeNull();
  command("continue"); await advance();
  expect(screen.getByText("Tell you when this task is done?")).toBeTruthy();
  expect(screen.queryByText(/Get an alert/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Tell me" }));
  await advance(900);
  expect(window.__sim!.host.tasks.find((t) => t.task_id === ids.tHeaders)?.notify_done).toBe(true);
  expect(screen.getByRole("status").textContent).toContain("We’ll tell you when it’s done");
  expect(screen.queryByText("Tell you when this task is done?")).toBeNull();
});

it("turning a chat into a task, or starting a chat's suggestion, brings no offer", async () => {
  offer(); await advance();
  act(() => app.session.state.set((s) => ({ ...s, tasks: { ...s.tasks, [ids.tHeaders]: { ...s.tasks[ids.tHeaders]!, conversation: true } } })));
  const chat = app.session.state.get().tasks[ids.tHeaders]!;
  const into: PendingCommand = { id: "into", body: { command: "resume_task", task_id: chat.task_id, mode: "work", text: "Go ahead" }, label: "Turn into a task", issuedAt: Date.now(), phase: "accepted", error: null, receipt: { command_id: "into", status: "accepted", seq: 1, result: null, error: null } };
  act(() => app.session.commands.set((s) => ({ ...s, into })));
  await advance();
  expect(screen.queryByText(/^Tell you when this/)).toBeNull();
  const started: PendingCommand = {
    id: "card",
    body: { command: "create_task", project_id: chat.project_id, prompt: "Look", title: "Look", harness: "claude_code", model: null, effort: null, mode: "ask", placement: { mode: "start_now" }, mission: null, bundle: null, branch: null, start: null, place: null, browser: null, github: null, origin: { chat: chat.task_id, run: "r", index: 0 } } as unknown as PendingCommand["body"],
    label: "Start", issuedAt: Date.now(), phase: "accepted", error: null,
    receipt: { command_id: "card", status: "accepted", seq: 1, result: { kind: "task_created", task_id: ids.tHeaders } as never, error: null },
  };
  act(() => app.session.commands.set((s) => ({ ...s, card: started })));
  await advance();
  expect(screen.queryByText(/^Tell you when this/)).toBeNull();
  // An ordinary action still brings it.
  command("continue"); await advance();
  expect(screen.getByText(/^Tell you when this/)).toBeTruthy();
});

it("someone who answered the older Get alerts offer is still offered the bell once", async () => {
  localStorage.setItem(`task-alerts-offer.${app.profile.profileId}`, "seen");
  offer(); await advance();
  command("continue"); await advance();
  expect(screen.getByText("Tell you when this task is done?")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Not now" }));
  expect(screen.queryByText("Tell you when this task is done?")).toBeNull();
  // Once, across a reload; the older offer's answer is kept as it was.
  const reloaded = new TaskAlertsOffer(app.profile.profileId);
  const off = reloaded.watch(app.session, notificationAccess(app.platform));
  command("after-reload"); await advance();
  expect(reloaded.get()).toBeNull(); off();
  expect(localStorage.getItem(`task-bell-offer.${app.profile.profileId}`)).toBe("seen");
  expect(localStorage.getItem(`task-alerts-offer.${app.profile.profileId}`)).toBe("seen");
});

it("answering the bell offer leaves the older offer for an older computer", async () => {
  localStorage.setItem(`task-bell-offer.${app.profile.profileId}`, "seen");
  older(); permission("promptable"); offer(); await advance();
  command("continue"); await advance();
  expect(screen.getByText(/Get an alert when this/)).toBeTruthy();
});

it("Allow alerts calls the foreground request and disappears only on confirmed grant", async () => {
  older();
  permission("promptable"); offer(); command("start"); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Allow alerts" })); await advance();
  expect(window.__sim?.device.notificationRequests).toBe(1);
  expect(notificationAccess(app.platform).get().permission).toBe("granted");
  expect(screen.queryByText(/Get an alert/)).toBeNull();
});

it.each(["granted", "blocked"] as const)("%s does not offer or invoke permission on a task action", async (value) => {
  older();
  permission(value); offer(); command("start"); await advance();
  expect(screen.queryByText(/Get an alert/)).toBeNull();
  expect(window.__sim?.device.notificationRequests).toBe(0);
});

it("accepted new task offers in its exact task context, queued work does not", async () => {
  older();
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
  const summary = screen.getByText("Background delivery and phone settings").closest("summary")!;
  expect(within(summary).getByText("Alerts are off")).toBeTruthy();
  expect(summary.closest("details")?.open).toBe(false);
  fireEvent.click(summary);
  expect(screen.getByRole("button", { name: "Allow alerts" })).toBeTruthy();
  act(() => visibility("hidden")); permission("blocked"); act(() => visibility("visible")); await advance();
  expect(within(summary).getByText("Alerts are blocked in the phone’s settings")).toBeTruthy();
  fireEvent.click(screen.getAllByRole("button", { name: "Open Android settings" })[0]!); await advance();
  expect(window.__sim?.device.notificationSettingsOpened).toBe(1);
  act(() => visibility("hidden")); permission("granted"); act(() => visibility("visible")); await advance();
  expect(within(summary).getByText("Alerts are on")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Allow alerts" })).toBeNull();
});

it("denial is not reported as permission granted and query failure stays unknown", async () => {
  permission("promptable"); window.__sim?.device.notificationRequestResult("blocked");
  render(<Notifications />, { wrapper }); await advance();
  const summary = screen.getByText("Background delivery and phone settings").closest("summary")!;
  fireEvent.click(summary);
  fireEvent.click(screen.getByRole("button", { name: "Allow alerts" })); await advance();
  expect(screen.queryByText("Alerts are on")).toBeNull();
  expect(within(summary).getByText("Alerts are blocked in the phone’s settings")).toBeTruthy();
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
  older();
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
  older();
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

it("a cut-off reply uses reply settings, opens Chat, and leaves the shade when seen without a Home decision", async () => {
  const device = window.__sim!.device;
  const original = app.session.state.get().attention[ids.atReady]!;
  const taskId = original.task_id!;
  const item = { ...original, attention_id: "cut-off-reply", kind: "completed" as const, topic: "finished" as const, alert: undefined, title: "Reply cut off. Write to carry on.", quiet: false };
  visibility("hidden");
  act(() => app.session.state.set((s) => ({ ...s,
    tasks: { ...s.tasks, [taskId]: { ...s.tasks[taskId]!, conversation: true, state: "reconciliation_required", attention_count: 0 } },
    attention: { ...s.attention, [item.attention_id]: item },
  })));
  await advance();
  const state = app.session.state.get();
  const settings = notificationSettings(app.session).current();
  expect(choose(item, state, settings, { tier: () => "off" })).toBeNull();
  expect(choose(item, state, settings, { tier: () => "silent" })).toEqual({ type: "reply", tier: "silent" });
  const note = device.notifications.find(n => n.id.endsWith("/cut-off-reply"));
  expect(note).toMatchObject({ title: item.title, category: "finished", alertType: "reply", want: "alert",
    route: { kind: "attention", profileId: app.profile.profileId, attentionId: item.attention_id } });
  expect(attentionDestination(item, state, Date.now())).toMatchObject({ kind: "talk", task: { task_id: taskId } });
  expect(openAttention(state).some(a => a.attention_id === item.attention_id)).toBe(false);
  expect(replyAttention(state).some(a => a.attention_id === item.attention_id)).toBe(true);
  // Ordinary replies still use their ordinary heading, not the task's title.
  expect(notificationFor({ ...item, topic: undefined, title: "A chat title" }, state, app.profile, "minimal", { type: "reply", tier: "silent" })?.title).toBe("New reply");
  const offLock = appLock(app.platform).start();
  visibility("visible"); await advance();
  const close = viewingTask(app.profile.profileId, taskId);
  await advance();
  expect(device.cancelled).toContain(note!.id);
  close();
  const count = device.notifications.length;
  visibility("hidden"); await advance();
  expect(device.notifications).toHaveLength(count);
  offLock();
});

function chatReply(id: string) {
  const original = app.session.state.get().attention[ids.atReady]!;
  const taskId = original.task_id!;
  act(() => app.session.state.set(s => ({ ...s,
    tasks: { ...s.tasks, [taskId]: { ...s.tasks[taskId]!, conversation: true, notify_done: false } },
    attention: { ...s.attention, [id]: { ...original, attention_id: id, kind: "completed", alert: "reply", quiet: false } },
  })));
  return taskId;
}

it.each(["home", "same chat", "different chat", "background"] as const)("only a reply outside the exact visible chat alerts: %s", async where => {
  const offLock = appLock(app.platform).start();
  await advance();
  const taskId = app.session.state.get().attention[ids.atReady]!.task_id!;
  const close = where === "same chat" || where === "background" ? viewingTask(app.profile.profileId, taskId)
    : where === "different chat" ? viewingTask(app.profile.profileId, "another-chat") : () => {};
  if (where === "background") visibility("hidden");
  const notify = vi.spyOn(app.platform, "notify");
  chatReply("reply");
  await advance();
  if (where === "same chat") {
    expect(notify).not.toHaveBeenCalled();
    close();
    visibility("hidden");
    await advance();
    expect(notify).not.toHaveBeenCalled();
  } else {
    expect(notify).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ alertType: "reply", want: "alert" }));
    expect(notify.mock.calls[0]![0].silent).not.toBe(true);
    close();
  }
  offLock();
});

it("the foreground app handles unseen replies even with a watcher and never marks them seen just for showing Home", async () => {
  const offLock = appLock(app.platform).start();
  await advance();
  vi.spyOn(backgroundAlerts(app.platform)!, "covers").mockReturnValue(true);
  const reported = vi.spyOn(app.platform.alerts!, "seen");
  const notify = vi.spyOn(app.platform, "notify");
  chatReply("unseen-reply");
  await advance();
  expect(notify).toHaveBeenCalledTimes(1);
  expect(reported.mock.calls.flatMap(([, ids]) => ids)).not.toContain("unseen-reply");
  // A caught-up snapshot and replay carry the same identity, not new news.
  act(() => app.session.state.set(s => ({ ...s, caughtUp: false })));
  act(() => app.session.state.set(s => ({ ...s, caughtUp: true, attention: { ...s.attention } })));
  chatReply("unseen-reply");
  visibility("hidden"); visibility("visible");
  await advance();
  expect(notify).toHaveBeenCalledTimes(1);
  stop();
  stop = watchAttention(app.platform, app.session, app.profile.name);
  await advance();
  expect(notify).toHaveBeenCalledTimes(1);
  offLock();
});

it.each(["silent", "off"] as const)("explicit %s reply settings survive the new default", async tier => {
  await advance();
  await deviceTiers(app.platform).setTier("reply", tier);
  const notify = vi.spyOn(app.platform, "notify");
  chatReply("chosen-reply");
  await advance();
  if (tier === "off") expect(notify).not.toHaveBeenCalled();
  else expect(notify).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ alertType: "reply", want: "silent", silent: true }));
});

it("a muted project and a blocked notification permission still hold back reply alerts", async () => {
  await advance();
  const settings = notificationSettings(app.session);
  settings.set({ view: { off: [], bundle: false, revision: 1, projects: [{ project_id: ids.pTide, mute: true, on: [], off: [] }] }, error: null });
  chatReply("muted-reply");
  await advance();
  expect(window.__sim!.device.notifications).toHaveLength(0);
  settings.set({ view: { off: [], bundle: false, revision: 2, projects: [] }, error: null });
  permission("blocked");
  chatReply("blocked-reply");
  await advance();
  expect(window.__sim!.device.notifications).toHaveLength(0);
  expect(notificationAccess(app.platform).get().missed[app.profile.profileId]?.off).toBe(true);
});

/** Android routing at its real typed defaults, including Finished = Off.
 * Run the actual watchAttention producer against a shade, not just a notify spy. */
function typedShade() {
  const delivered = new Map<string, LocalNotification>();
  const sounded: string[] = [];
  const tiers = deviceTiers(app.platform);
  const cancel = app.platform.cancelNotifications!.bind(app.platform);
  vi.spyOn(app.platform, "cancelNotifications").mockImplementation(async ids => {
    for (const id of ids) delivered.delete(id);
    await cancel(ids);
  });
  vi.spyOn(app.platform, "notify").mockImplementation(async note => {
    const type = note.alertType ?? (note.category === "finished" ? "finished" : "decision");
    const wanted = note.want ?? tiers.tier(type);
    if (wanted === "off") return;
    const replacing = delivered.has(note.id);
    delivered.set(note.id, note);
    if (!note.silent && wanted === "alert" && !replacing) sounded.push(note.id);
  });
  return { delivered, sounded };
}

it.each(["alert", "silent"] as const)("folded foreground replies survive default Android routing and silent recount: %s", async tier => {
  const offLock = appLock(app.platform).start();
  await advance();
  // Undo this file's historical Finished=Alert fixture override.
  await deviceTiers(app.platform).setTier("finished", recommendedTiers().finished);
  await deviceTiers(app.platform).setTier("reply", tier);
  const settings = notificationSettings(app.session);
  act(() => settings.set({ view: { off: [], projects: [], bundle: true, revision: 1 }, error: null }));
  const shade = typedShade();
  chatReply("fold-one"); await advance();
  expect(shade.delivered.size).toBe(1);
  chatReply("fold-two"); await advance();
  const tag = `${app.profile.profileId}/project:${ids.pTide}`;
  expect([...shade.delivered.keys()]).toEqual([tag]);
  expect(shade.delivered.get(tag)).toMatchObject({ alertType: "reply", want: tier, handles: [`${app.profile.profileId}/fold-one`, `${app.profile.profileId}/fold-two`], title: "tidewater-api has 2 notifications" });
  // A later replacement and a recount must not make another sound.
  const before = shade.sounded.length;
  chatReply("fold-three"); await advance();
  act(() => app.session.state.set(s => ({ ...s, resolvedAttention: { ...s.resolvedAttention, "fold-one": Date.now() } })));
  await advance();
  expect(shade.delivered.get(tag)).toMatchObject({ alertType: "reply", want: tier, silent: true, handles: [`${app.profile.profileId}/fold-two`, `${app.profile.profileId}/fold-three`], title: "tidewater-api has 2 notifications" });
  expect(shade.sounded).toHaveLength(before);
  if (tier === "silent") expect(shade.sounded).toHaveLength(0);
  // An off choice must remove all folded membership, not resurrect it on recount.
  await deviceTiers(app.platform).setTier("reply", "off");
  await advance();
  expect(shade.delivered.size).toBe(0);
  chatReply("fold-off"); await advance();
  expect(shade.delivered.size).toBe(0);
  offLock();
});

it("folded mixed needs-you membership keeps the loudest remaining type and honors project mute", async () => {
  visibility("hidden");
  await advance();
  await deviceTiers(app.platform).setTier("finished", "off");
  const settings = notificationSettings(app.session);
  act(() => settings.set({ view: { off: [], projects: [], bundle: true, revision: 1 }, error: null }));
  const shade = typedShade();
  chatReply("mixed-reply"); await advance();
  const sample = app.session.state.get().attention[ids.atReady]!;
  act(() => app.session.state.set(s => ({ ...s, attention: { ...s.attention, "mixed-question": { ...sample, attention_id: "mixed-question", kind: "question", alert: "question", quiet: false } } })));
  await advance();
  const tag = `${app.profile.profileId}/project:${ids.pTide}`;
  expect(shade.delivered.get(tag)).toMatchObject({ category: "needs_you", alertType: "question", want: "alert" });
  act(() => app.session.state.set(s => ({ ...s, resolvedAttention: { ...s.resolvedAttention, "mixed-question": Date.now() } })));
  await advance();
  expect(shade.delivered.get(tag)).toMatchObject({ category: "finished", alertType: "reply", want: "alert", silent: true });
  act(() => settings.set({ view: { off: [], bundle: true, revision: 2, projects: [{ project_id: ids.pTide, mute: true, on: [], off: [] }] }, error: null }));
  await advance();
  expect(shade.delivered.size).toBe(0);
});

it("round2 leaves Android membership authoritative across foreground A/B, hidden C, and opening A", async () => {
  stop();
  Object.assign(app.platform, { nativeNotificationFolding: true });
  stop = watchAttention(app.platform, app.session, app.profile.name);
  const offLock = appLock(app.platform).start();
  await advance();
  vi.spyOn(backgroundAlerts(app.platform)!, "covers").mockReturnValue(true);
  act(() => notificationSettings(app.session).set({ view: { off: [], projects: [], bundle: true, revision: 1 }, error: null }));
  const notify = vi.spyOn(app.platform, "notify");
  const cancel = vi.spyOn(app.platform, "cancelNotifications");
  const aTask = chatReply("round2-a"); await advance();
  // Give B and C different chats, so opening A only clears A.
  const original = app.session.state.get().attention[ids.atReady]!;
  const add = (id: string) => act(() => app.session.state.set(s => ({ ...s,
    tasks: { ...s.tasks, [id]: { ...s.tasks[aTask]!, task_id: id, conversation: true } },
    attention: { ...s.attention, [id]: { ...original, attention_id: id, task_id: id, kind: "completed", alert: "reply", quiet: false } },
  })));
  add("round2-b"); await advance();
  visibility("hidden");
  add("round2-c"); await advance();
  expect(notify).toHaveBeenCalledTimes(2);
  visibility("visible"); await advance();
  const close = viewingTask(app.profile.profileId, aTask); await advance();
  expect(cancel.mock.calls.flatMap(([ids]) => ids)).toContain(`${app.profile.profileId}/round2-a`);
  // No JavaScript recount may replace Android's B/C summary with only B.
  expect(notify).toHaveBeenCalledTimes(2);
  for (const [note] of notify.mock.calls) {
    expect(note.id).not.toContain("/project:");
    expect(note).toMatchObject({ fold: { projectId: ids.pTide, project: "tidewater-api", computer: app.profile.name } });
  }
  close(); offLock();
});

it.each(["off", "mute"] as const)("round2 native members update silently with choices and clear for %s", async ending => {
  stop();
  Object.assign(app.platform, { nativeNotificationFolding: true });
  stop = watchAttention(app.platform, app.session, app.profile.name);
  const offLock = appLock(app.platform).start();
  await advance();
  const settings = notificationSettings(app.session);
  act(() => settings.set({ view: { off: [], projects: [], bundle: true, revision: 1 }, error: null }));
  const notify = vi.spyOn(app.platform, "notify");
  const cancel = vi.spyOn(app.platform, "cancelNotifications");
  chatReply("choice-one"); await advance();
  chatReply("choice-two"); await advance();
  expect(notify).toHaveBeenCalledTimes(2);
  await deviceTiers(app.platform).setTier("reply", "silent"); await advance();
  expect(notify).toHaveBeenCalledTimes(4);
  for (const [note] of notify.mock.calls.slice(2)) {
    expect(note).toMatchObject({ silent: true, alertType: "reply", want: "silent", fold: { projectId: ids.pTide } });
    expect(note.id).not.toContain("/project:");
    expect(note.handles).toBeUndefined();
  }
  if (ending === "off") await deviceTiers(app.platform).setTier("reply", "off");
  else act(() => settings.set({ view: { off: [], bundle: true, revision: 2, projects: [{ project_id: ids.pTide, mute: true, on: [], off: [] }] }, error: null }));
  await advance();
  expect(cancel.mock.calls.flatMap(([ids]) => ids)).toEqual(expect.arrayContaining([`${app.profile.profileId}/choice-one`, `${app.profile.profileId}/choice-two`]));
  chatReply("choice-after"); await advance();
  expect(notify).toHaveBeenCalledTimes(4);
  offLock();
});

it("round3 ungrouped Android routing refresh keeps the individual identity and leaves suppression to native accounting", async () => {
  stop();
  Object.assign(app.platform, { nativeNotificationFolding: true });
  stop = watchAttention(app.platform, app.session, app.profile.name);
  const offLock = appLock(app.platform).start();
  await advance();
  act(() => notificationSettings(app.session).set({ view: { off: [], projects: [], bundle: false, revision: 1 }, error: null }));
  const notify = vi.spyOn(app.platform, "notify");
  chatReply("round3-existing"); await advance();
  expect(notify).toHaveBeenCalledTimes(1);
  notify.mockClear();
  await deviceTiers(app.platform).setTier("reply", "silent"); await advance();
  // Native eviction is not a page-level handling event. The native tests
  // exercise suppression after eviction, free space, and this exact refresh.
  expect(notify).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
    id: `${app.profile.profileId}/round3-existing`, alertType: "reply", want: "silent", silent: true,
  }));
  expect(notify.mock.calls[0]![0].fold).toBeUndefined();
  expect(notify.mock.calls[0]![0].handles).toBeUndefined();
  chatReply("round3-new"); await advance();
  expect(notify.mock.calls.at(-1)![0]).toMatchObject({ id: `${app.profile.profileId}/round3-new`, want: "silent", silent: true });
  offLock();
});

it("a choice in Settings shows on the tap, and goes back with a toast when Android refuses", async () => {
  const device = window.__sim!.device;
  render(<><Notifications /><Toasts /></>, { wrapper });
  await advance(3000);
  const questions = () => within(screen.getByRole("radiogroup", { name: "Questions" }));
  expect(questions().getByRole("radio", { name: "Alert, recommended" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(questions().getByRole("radio", { name: "Off" }));
  await advance(100);
  expect(questions().getByRole("radio", { name: "Off" }).getAttribute("aria-checked")).toBe("true");
  await advance(3000);
  expect(device.tierChanges).toContainEqual({ alertType: "question", tier: "off" });
  expect(questions().getByRole("radio", { name: "Off" }).getAttribute("aria-checked")).toBe("true");

  device.refuseTier("Android said no.");
  fireEvent.click(questions().getByRole("radio", { name: "Silent" }));
  await advance(3000);
  expect(questions().getByRole("radio", { name: "Off" }).getAttribute("aria-checked")).toBe("true");
  expect(screen.getByText("Couldn’t change how questions notify you. Android said no.")).toBeTruthy();
});

it("raises report alerts without a task and respects the Reports topic", async () => {
  older();
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

it("raises another computer's alerts while the app shows a different one, and stops once it shows that computer", async () => {
  stop();
  const shown = new Store(false);
  stop = watchAttention(app.platform, app.session, app.profile.name, { get: shown.get, subscribe: shown.subscribe });
  // Unlocked on the screen, as the app is once opened.
  const offLock = appLock(app.platform).start();
  await advance();
  const notify = vi.spyOn(app.platform, "notify");
  // The app is on screen, but showing another computer: this one's new
  // item isn't seen there, so it alerts.
  miss("elsewhere"); await advance();
  expect(notify.mock.calls.map((c) => c[0].id)).toEqual([`${app.profile.profileId}/elsewhere`]);
  // Once the screen shows this computer, its new items are seen there.
  act(() => shown.set(true));
  miss("here"); await advance();
  expect(notify).toHaveBeenCalledTimes(1);
  offLock();
});

it("posts a new-version notice in the visible app despite the watcher, without reporting it as shown", async () => {
  const offLock = appLock(app.platform).start();
  await advance();
  const caps = app.session.state.get().capabilities!;
  const native = app.platform.alerts!;
  const covered = vi.spyOn(native, "seen");
  // A running watcher would otherwise hold this computer's alerts for the app.
  const watcher = (await import("./alerts")).backgroundAlerts(app.platform)!;
  vi.spyOn(watcher, "covers").mockReturnValue(true);
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...caps, features: [...caps.features, "release_check_v1"] }, attention: { ...s.attention, release: { ...s.attention[ids.atReady]!, attention_id: "release", task_id: null, kind: "completed", new_version: "0.3.0", alert_handled_at: undefined, quiet: false } } })));
  await advance();
  expect(window.__sim!.device.notifications.some((n) => n.id.endsWith("/release"))).toBe(true);
  expect(covered.mock.calls.flatMap(([, ids]) => ids)).not.toContain("release");
  offLock();
});
