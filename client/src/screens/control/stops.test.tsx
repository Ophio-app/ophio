// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { taskOnScreen, talkTask } from "../../app/taskOnScreen";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import type { Task } from "../../protocol/types";
import { StopStore, stopStore, STOP_WAIT_MS, stopFrequency } from "./stops";
import { UrgentSlot } from "../../design/urgent";
import { StopTask } from "./StopTask";
import SafetyStrip from "./SafetyStrip";

let app: App;
let store: StopStore;
let task: Task;
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
// The rail's Stop is the symbol alone; which task and display it ends is the
// quiet text beside it.
const stopText = (stop: HTMLElement) => stop.closest(".named-stop")?.querySelector(".named-stop__caption")?.textContent ?? stop.textContent ?? "";
beforeEach(async ({ task: test }) => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start();
  await advance(900);
  const current = session.state.get().tasks[ids.tFlaky];
  if (!current) throw new Error("Missing task");
  task = current;
  // Silence scripted activity; these tests advance the admission bound exactly.
  if (!test.name.startsWith("offers confirmed Force stop")) window.__sim?.host.dispose();
  vi.spyOn(session, "command").mockImplementation(async (_body, options) => ({ command_id: options?.commandId ?? "unused", status: "accepted", seq: null, result: null, error: null }));
  store = stopStore(session);
});
afterEach(() => {
  cleanup(); store.dispose(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("an accepted Stop becomes Stop again at the bound and uses a new admission and current revision", async () => {
  render(<AppProvider app={app}><StopTask task={task} /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /^Stop ·/ }));
  await advance(0);
  const first = store.get()[task.task_id]?.id;
  await advance(STOP_WAIT_MS - 1);
  expect((screen.getByRole("button", { name: /^Stop requested/ }) as HTMLButtonElement).disabled).toBe(true);
  await advance(1);
  const again = screen.getByRole("button", { name: /^Stop again/ }) as HTMLButtonElement;
  expect(again.disabled).toBe(false);
  expect(screen.getByRole("status").textContent).toContain("does not force quit");
  act(() => store.request({ ...task, revision: task.revision + 1 }));
  expect(store.get()[task.task_id]?.id).not.toBe(first);
  expect(app.session.command).toHaveBeenLastCalledWith({ command: "stop_task", task_id: task.task_id }, expect.objectContaining({ expectedRevision: task.revision + 1 }));
  await advance(0);
});

it("new activity on the same run reopens Stop before the time bound", async () => {
  store.request(task); await advance(1);
  app.session.state.set((s) => ({ ...s, activity: { [task.task_id]: { runId: task.current_run ?? "", text: "Still working", at: Date.now() } } }));
  expect(store.get()[task.task_id]?.phase).toBe("still_running");
});

it("reload reconciles persisted acceptance against a running run immediately", async () => {
  store.request(task); await advance(0);
  expect(store.get()[task.task_id]?.phase).toBe("accepted");
  store.dispose();
  store = new StopStore(app.session);
  await advance(0);
  expect(store.get()[task.task_id]?.phase).toBe("still_running");
  store.request(task);
  expect(app.session.command).toHaveBeenCalledTimes(2);
  await advance(0);
});

it("a restart on the computer with the same running run cannot preserve a disabled acceptance", async () => {
  store.request(task); await advance(0);
  app.session.state.set((s) => ({ ...s, bootEpoch: "next-boot" }));
  expect(store.get()[task.task_id]?.phase).toBe("still_running");
});

it("drops missing runs only after host catch-up", async () => {
  store.request(task); await advance(0);
  app.session.state.set((s) => ({ ...s, caughtUp: false, tasks: {}, runs: {} }));
  expect(store.get()[task.task_id]).toBeDefined();
  app.session.state.set((s) => ({ ...s, caughtUp: true }));
  expect(store.get()).toEqual({});
  expect(JSON.parse(localStorage.getItem(`control-stops.${app.profile.profileId}`) ?? "null")).toEqual({});
});

it.each(["Agent confirmed Stop", "Agent acknowledged Stop", "Agent exited"])("only confirmed native facts clear the intent: %s", async (label) => {
  store.request(task); await advance(0);
  const seq = app.session.state.get().journalHead + 1;
  const record = { seq, at: Date.now(), taskId: task.task_id, targetId: null, receipt: { kind: "stop_task" as const, requested_at: Date.now(), summary: "Stop receipt", facts: [{ label, status: "unknown" as const, detail: null }] } };
  app.session.state.set((s) => ({ ...s, interventions: [record] }));
  expect(store.get()[task.task_id]?.phase).toBe("accepted");
  app.session.state.set((s) => ({ ...s, interventions: [{ ...record, receipt: { ...record.receipt, facts: [{ label, status: "done", detail: null }] } }] }));
  expect(store.get()[task.task_id]).toBeUndefined();
});

it("an acknowledgement arriving before the admission response is never resurrected", async () => {
  let admit: (() => void) | undefined;
  vi.mocked(app.session.command).mockImplementation((_body, opts) => new Promise((resolve) => {
    admit = () => resolve({ command_id: opts?.commandId ?? "", status: "accepted", seq: null, error: null, result: null });
  }));
  store.request(task);
  app.session.state.set((s) => ({ ...s, interventions: [{ seq: s.journalHead + 1, at: Date.now(), taskId: task.task_id, targetId: null,
    receipt: { kind: "stop_task", requested_at: Date.now(), summary: "Confirmed", facts: [{ label: "Agent confirmed Stop", status: "done", detail: null }] } }] }));
  admit?.(); await advance(0);
  expect(store.get()[task.task_id]).toBeUndefined();
});

it("unchosen Chat fallback and collapsed Stop both name the newest conversation still going", () => {
  // Talk picks up only a conversation; a task opens there when chosen.
  expect(taskOnScreen(app.session.state.get(), app.ui.get())).toBeNull();
  act(() => app.session.state.set((s) => ({ ...s, tasks: { ...s.tasks, [ids.tDash]: { ...s.tasks[ids.tDash]!, conversation: true } } })));
  const state = app.session.state.get();
  const shown = taskOnScreen(state, app.ui.get());
  expect(shown?.task_id).toBe(ids.tDash);
  expect(talkTask(state, ids.pTide, undefined)).toBe(shown);
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  expect(stopText(screen.getByRole("button", { name: /^Stop ·/ }))).toContain(shown?.title);
});

it("an explicit new draft or a different tab does not borrow Chat’s task", () => {
  app.ui.selectTask(ids.pTide, null);
  expect(taskOnScreen(app.session.state.get(), app.ui.get())).toBeNull();
  app.ui.selectTask(ids.pTide, ids.tFlaky);
  app.ui.selectTab("work");
  expect(taskOnScreen(app.session.state.get(), app.ui.get())).toBeNull();
  app.ui.push({ screen: "receipt", taskId: ids.tDash });
  expect(taskOnScreen(app.session.state.get(), app.ui.get())?.task_id).toBe(ids.tDash);
});

it("an inline native acknowledgement clears admission without waiting for another event", async () => {
  vi.mocked(app.session.command).mockImplementation(async (_body, opts) => ({
    command_id: opts?.commandId ?? "", status: "accepted", seq: null, error: null,
    result: { kind: "intervention", receipt: { kind: "stop_task", requested_at: Date.now(), summary: "Confirmed",
      facts: [{ label: "Agent confirmed Stop", status: "done", detail: null }] } },
  }));
  store.request(task);
  await advance(0);
  expect(store.get()[task.task_id]).toBeUndefined();
});

it("offers confirmed Force stop after two ignored Stops and records both", async () => {
  vi.mocked(app.session.command).mockRestore();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  window.__sim!.host.stopSafety.ignore(task.task_id);
  render(<AppProvider app={app}><StopTask task={task} /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /^Stop ·/ }));
  await advance(1_500);
  fireEvent.click(screen.getByRole("button", { name: /^Stop again ·/ }));
  await advance(1_500);
  expect(screen.getByText(/Stop sent twice · not confirmed/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Force stop" }));
  expect(screen.getByText(/Files it already changed stay changed/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: `Force stop on ${app.profile.name}` }));
  await advance(1_500);
  expect(app.session.state.get().tasks[task.task_id]?.state).toBe("interrupted");
  const receipt = app.session.state.get().timelines[task.task_id]?.items.find(i => i.item.kind === "receipt" && i.item.receipt.facts.some(f => f.label === "Agent process ended on the computer"));
  expect(receipt?.item.kind === "receipt" && receipt.item.receipt.facts).toEqual(expect.arrayContaining([
    expect.objectContaining({ label: "Agent process ended on the computer", status: "done" }),
    expect.objectContaining({ label: "Agent confirmed Stop", status: "unknown" }),
    expect.objectContaining({ label: "Stop requests", detail: "Stop sent twice · then Force stop" }),
  ]));
});

it.each([[1, "once"], [2, "twice"], [3, "3 times"]] as const)("Stop count %s uses plain words", (count, words) => {
  expect(stopFrequency(count)).toBe(words);
});

it.each([false, true])("prioritizes the caption's second task by target or lease (lease only: %s)", (leaseOnly) => {
  app.ui.selectTab("work");
  if (leaseOnly) app.session.state.set(s => ({ ...s, tasks: { ...s.tasks, [ids.tDash]: { ...s.tasks[ids.tDash]!, target_id: null } } }));
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  const stop = screen.getByRole("button", { name: /^Stop ·/ });
  expect(stopText(stop)).toContain("Check the staging");
  expect(stopText(stop)).toContain("DP-1");
});
it("explicitly labels a Stop when no running task holds the captioned display", () => {
  app.ui.selectTab("work");
  app.session.state.set(s => ({ ...s, tasks: { [ids.tFlaky]: s.tasks[ids.tFlaky]! } }));
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  expect(stopText(screen.getByRole("button", { name: /^Stop ·/ }))).toContain("not using a display");
});
it("the on-screen task wins and its visible label disambiguates the other display", () => {
  app.ui.selectTask(ids.pTide, ids.tFlaky);
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  const stop = screen.getByRole("button", { name: /^Stop ·/ });
  expect(stopText(stop)).toContain(task.title);
  expect(stopText(stop)).toContain("not using a display");
});

const pauseDp1 = () => app.session.state.set(s => ({ ...s, control: { ...s.control, [ids.tgDp1]: { ...s.control[ids.tgDp1]!, lease: null, input_state: "paused" } } }));
it("a display's state is captioned only beside the Stop of the task holding it", () => {
  app.ui.selectTask(ids.pTide, ids.tFlaky);
  pauseDp1();
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  const strip = screen.getByRole("complementary", { name: "Safety controls" });
  // The first Stop is the on-screen task's, so the toggle names who holds
  // DP-1, and its count is marked rather than the Stop's line.
  const held = "Screen 1 (DP-1) held by Check the staging dashboard renders the new latency chart · Agent paused";
  const toggle = within(strip).getByRole("button", { name: `Safety · ${held} · 2 running tasks` });
  expect(toggle.querySelector(".safety-rail__count--warn")?.textContent).toBe("2");
  const stop = within(strip).getByRole("button", { name: /^Stop ·/ });
  expect(stopText(stop)).toContain(task.title);
  expect(stopText(stop)).toContain("not using a display");
  expect(strip.querySelector(".safety-rail__line--warn")).toBeNull();
  fireEvent.click(toggle);
  expect(within(strip).getAllByRole("button", { name: /^Stop ·/ }).map(b => b.getAttribute("aria-label"))).toEqual([
    `Stop · ${task.title} · not using a display`,
    "Stop · Check the staging dashboard renders the new latency chart · Screen 1 (DP-1) · agent paused",
  ]);
});
it("the on-screen task's own display state is the line under its name, beside its Stop", () => {
  app.ui.selectTask(ids.pTide, ids.tDash);
  pauseDp1();
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  const strip = screen.getByRole("complementary", { name: "Safety controls" });
  const toggle = within(strip).getByRole("button", { name: "Safety · Screen 1 (DP-1) · Agent paused · 2 running tasks" });
  expect(toggle.querySelector(".safety-rail__count--warn")).toBeNull();
  const stop = within(strip).getByRole("button", { name: /^Stop ·/ });
  expect(stop.getAttribute("aria-label")).toBe("Stop · Check the staging dashboard renders the new latency chart · Screen 1 (DP-1) · agent paused");
  expect(strip.querySelector(".safety-rail__line--warn")?.textContent).toBe("Agent paused · Screen 1 (DP-1)");
  expect(strip.querySelector(".safety-rail__summary")).toBeNull();
});

it("Force stop sheet excludes its own task from the urgent rail and keeps the other Stop", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  render(<AppProvider app={app}><UrgentSlot.Provider value={SafetyStrip}><StopTask task={task} /></UrgentSlot.Provider></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /^Stop ·/ }));
  await advance(STOP_WAIT_MS + 1);
  fireEvent.click(screen.getByRole("button", { name: "Force stop" }));
  const dialog = within(screen.getByRole("dialog"));
  expect(dialog.queryByRole("button", { name: /^Stop again/ })).toBeNull();
  expect(dialog.queryByRole("button", { name: "Force stop" })).toBeNull();
  expect(dialog.getByRole("button", { name: /^Stop · Check the staging/ })).toBeTruthy();
  expect(dialog.getByRole("button", { name: /^Force stop on/ })).toBeTruthy();
});
it("an empty display caption names Safety just once", () => {
  app.session.state.set(s => ({ ...s, control: {} }));
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  expect(screen.getByRole("button", { name: "Safety · 2 running tasks" })).toBeTruthy();
});

it("an offline rail does not infer that a task holds no display", () => {
  app.session.link.set({ state: "offline", since: Date.now(), reason: "Disconnected", retryAt: null });
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  expect(stopText(screen.getByRole("button", { name: /^Stop ·/ }))).toContain("display unknown until reconnected");
});

it("names the on-screen display even when its controls are excluded from the rail", () => {
  app.ui.push({ screen: "desktop", targetId: ids.tgDp1 }, "control");
  render(<AppProvider app={app}><SafetyStrip except={{ targetId: ids.tgDp1 }} /></AppProvider>);
  expect(stopText(screen.getByRole("button", { name: /^Stop · Check the staging/ }))).toContain("this display");
});

it.each(["talk", "work", "library"] as const)("keeps the display task first after its lease lapses on %s", (tab) => {
  app.ui.selectTask(ids.pTide, ids.tDash);
  app.ui.selectTab(tab);
  app.session.state.set(s => ({ ...s, control: { ...s.control, [ids.tgDp1]: { ...s.control[ids.tgDp1]!, lease: null, input_state: "paused" } } }));
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  const stop = screen.getByRole("button", { name: /^Stop ·/ });
  expect(stopText(stop)).toContain("Check the staging");
  expect(stopText(stop)).toContain("Agent paused · Screen 1 (DP-1)");
});

it("resolves a display from the latest observation without a lease or target hint", () => {
  app.ui.selectTab("library");
  app.session.state.set(s => ({ ...s, tasks: { ...s.tasks, [ids.tDash]: { ...s.tasks[ids.tDash]!, target_id: null } },
    control: { ...s.control, [ids.tgDp1]: { ...s.control[ids.tgDp1]!, lease: null, input_state: "ready" } } }));
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  expect(stopText(screen.getByRole("button", { name: /^Stop · Check the staging/ }))).toContain("No one has input · Screen 1 (DP-1)");
});

it("a target without a control observation is still a display", () => {
  app.ui.selectTab("library");
  app.session.state.set(s => ({ ...s, control: {} }));
  render(<AppProvider app={app}><SafetyStrip /></AppProvider>);
  expect(stopText(screen.getByRole("button", { name: /^Stop · Check the staging/ }))).toContain("Screen 1 (DP-1) · input unconfirmed");
});

it("omits a sheet rail when its own task is the only running task", () => {
  app.session.state.set(s => ({ ...s, tasks: { [ids.tDash]: s.tasks[ids.tDash]! } }));
  render(<AppProvider app={app}><SafetyStrip except={{ taskId: ids.tDash }} /></AppProvider>);
  expect(screen.queryByRole("complementary")).toBeNull();
});

it("does not caption another task's Stop with the excluded display", () => {
  render(<AppProvider app={app}><SafetyStrip except={{ taskId: ids.tDash }} /></AppProvider>);
  expect(screen.getByRole("button", { name: "Safety · 1 running task" })).toBeTruthy();
  expect(stopText(screen.getByRole("button", { name: /^Stop · Fix the flaky/ }))).toContain("not using a display");
  expect(screen.queryByText(/DP-1/)).toBeNull();
});

it("standalone and rail Stops use the same display suffix", () => {
  const dash = app.session.state.get().tasks[ids.tDash]!;
  app.ui.selectTask(ids.pTide, ids.tDash);
  render(<AppProvider app={app}><StopTask task={dash} /><SafetyStrip /></AppProvider>);
  const buttons = screen.getAllByRole("button", { name: /^Stop · Check the staging/ });
  expect(buttons).toHaveLength(2);
  expect(buttons[0]!.getAttribute("aria-label")).toBe(buttons[1]!.getAttribute("aria-label"));
  expect(stopText(buttons[0]!)).toContain("DP-1");
});
it.each([ids.tDash, ids.tFlaky])("a sheet Stop derives its own display for %s", async taskId => {
  const { Sheet } = await import("../../design/Sheet");
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const target = app.session.state.get().tasks[taskId]!;
  render(<AppProvider app={app}><Sheet open onClose={() => {}} title="Archive task" footer={<StopTask task={target} />}>Review this task before archiving.</Sheet></AppProvider>);
  const dialog = screen.getByRole("dialog", { name: "Archive task" });
  expect(within(dialog).getByRole("button", { name: `Stop · ${target.title} · ${taskId === ids.tDash ? "Screen 1 (DP-1)" : "not using a display"}` })).toBeTruthy();
});
