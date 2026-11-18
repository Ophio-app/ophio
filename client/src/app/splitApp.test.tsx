// SPDX-License-Identifier: Apache-2.0
// Render the exported App and the real Shell, screens, split controls, Back
// registration, connections and leave-control flow. The wrapper only exposes
// the real per-pane context so tests can choose exact projects and routes.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { useApp, type App as AppContext } from "./context";
import { prefs } from "./prefs";
import { PARK_AFTER_MS } from "./computers";
import { UiStore } from "./ui";
import { annotationStore, blobs } from "../state/annotations";
import { forgetRemovedMarkup, removeMarkup } from "../screens/talk/removedMarkup";
import { usePane } from "../shell/pane";
import { FakeLink, platform, profile } from "../platform/lifecycle.fixtures";
import { secondProfile } from "../dev/sim/lifecycle";
import * as fx from "../dev/sim/scenario";
import type { WindowLayout } from "../platform/windowLayout";

const spare = secondProfile(profile);
let fake: ReturnType<typeof platform>;
let links: Map<string, FakeLink[]>;
const contexts = new Map<string, AppContext>();
vi.mock("../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../shell/Shell", async original => {
  const actual = await original<typeof import("../shell/Shell")>();
  return { ...actual, Shell: function ObservedShell() {
    const app = useApp(), pane = usePane();
    if (pane) contexts.set(pane.id, app);
    return <actual.Shell />;
  } };
});

function resize(width: number, height: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, value: height });
  fireEvent(window, new Event("resize"));
}
// A lazy route resolves in a separate turn from its mount effects. Always do
// both turns; otherwise a passing assertion can precede the real screen.
async function tick() {
  for (let i = 0; i < 2; i++) await act(async () => {
    await vi.dynamicImportSettled();
    await vi.advanceTimersByTimeAsync(100);
  });
}
async function start() {
  render(<App />);
  await tick();
  expect(screen.getByRole("region", { name: "Left side" })).toBeTruthy();
  await act(async () => { contexts.get("left")!.ui.selectTab("work"); });
  await tick();
}
const side = (id: "left" | "right") => within(screen.getByRole("region", { name: id === "left" ? "Left side" : "Right side" }));
async function open() {
  fireEvent.click(side("left").getAllByRole("button", { name: "Open split view" })[0]!);
  await tick();
  expect(screen.getByRole("region", { name: "Right side" })).toBeTruthy();
}
function lease() {
  fake.value.control = id => {
    const link = makeLink(id);
    if (id === profile.profileId) link.snap.control = [{ target_id: fx.ids.tgDp1, input_state: "human", lease: {
      lease_id: fx.ids.lease1, target_id: fx.ids.tgDp1, owner: { kind: "human", device_id: profile.deviceId },
      state: "active", generation: 1, boot_epoch: fx.ids.boot, granted_at: 1, renewal_window_ms: 15000,
    }, model_observation: null, private_step: null }];
    return link;
  };
}
function makeLink(id: string) {
  const link = new FakeLink();
  if (id === spare.profileId) {
    link.snap.host = { ...link.snap.host, host_id: spare.hostId, name: spare.name };
    link.snap.tasks = []; link.snap.runs = []; link.snap.approvals = []; link.snap.attention = [];
  }
  links.set(id, [...(links.get(id) ?? []), link]);
  return link;
}
beforeEach(() => {
  localStorage.clear(); contexts.clear(); links = new Map();
  vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  Element.prototype.scrollTo ??= () => {};
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  prefs.set(p => ({ ...p, theme: "system", reduceMotion: false, lastProfileId: null,
    allComputers: false, allProjects: false, warningsOff: false }));
  resize(1280, 800);
  fake = platform();
  fake.value.profiles = async () => [profile, spare];
  fake.value.control = makeLink;
});
afterEach(() => {
  for (const dismiss of screen.queryAllByRole("button", { name: "Dismiss" })) fireEvent.click(dismiss);
  cleanup(); contexts.clear();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});

it("labels the real root and pushed-screen split controls without changing their accessible name", async () => {
  await start();
  for (const button of side("left").getAllByRole("button", { name: "Open split view" })) {
    expect(button.textContent).toBe("Split view");
  }
  act(() => contexts.get("left")!.ui.push({ screen: "settings" }));
  await tick();
  const header = document.querySelector(".bar .split-control--focused button");
  expect(header?.textContent).toBe("Split view");
  expect(header?.getAttribute("aria-label")).toBe("Open split view");
});

it.each([{ width: 390, height: 844 }, { width: 915, height: 412 }])("offers no split control on an ordinary phone at $width by $height", async ({ width, height }) => {
  resize(width, height);
  await start();
  expect(screen.queryByRole("button", { name: "Open split view" })).toBeNull();
  expect(screen.queryByText("Split view", { exact: true })).toBeNull();
});

it.each(["work", "talk"] as const)("opens the complementary destination beside %s without duplicating computer connections", async tab => {
  await start();
  act(() => contexts.get("left")!.ui.selectTab(tab));
  await tick(); await open();
  expect(contexts.get("left")!.ui.get().tab).toBe(tab);
  expect(contexts.get("right")!.ui.get().tab).toBe(tab === "work" ? "talk" : "work");
  expect(contexts.get("left")!.session).toBe(contexts.get("right")!.session);
  expect(contexts.get("left")!.ui).not.toBe(contexts.get("right")!.ui);
  for (const opened of links.values()) expect(opened).toHaveLength(1);
  expect(side("left").getByRole("button", { name: "Close left side" })).toBeTruthy();
  expect(side("right").getByRole("button", { name: "Close right side" })).toBeTruthy();
});

it("keeps two computers and their projects independent, including switching away and back", async () => {
  await start(); await open();
  act(() => {
    contexts.get("left")!.ui.selectProject(fx.ids.pTide);
    contexts.get("right")!.switchTo(spare.profileId);
  });
  await tick();
  act(() => contexts.get("right")!.ui.selectProject(fx.ids.pNotes));
  await tick();
  expect(contexts.get("left")!.profile.profileId).toBe(profile.profileId);
  expect(contexts.get("left")!.ui.get().projectId).toBe(fx.ids.pTide);
  expect(contexts.get("right")!.profile.profileId).toBe(spare.profileId);
  expect(contexts.get("right")!.ui.get().projectId).toBe(fx.ids.pNotes);
  act(() => contexts.get("right")!.switchTo(profile.profileId)); await tick();
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  expect(contexts.get("right")!.ui.get().projectId).toBe(fx.ids.pNotes);
  expect(contexts.get("left")!.ui.get().projectId).toBe(fx.ids.pTide);
  for (const opened of links.values()) expect(opened).toHaveLength(1);
});

it("updates the same unsent chat draft on both sides without moving either route", async () => {
  await start(); await open();
  const left = contexts.get("left")!.ui, right = contexts.get("right")!.ui;
  act(() => left.saveDraft(fx.ids.pTide, null, { text: "still writing" }));
  expect(right.draft(fx.ids.pTide, null)?.text).toBe("still writing");
  act(() => right.saveDraft(fx.ids.pTide, null, { text: "continued here" }));
  expect(left.draft(fx.ids.pTide, null)?.text).toBe("continued here");
  expect(left.get().tab).toBe("work"); expect(right.get().tab).toBe("talk");
});

it("routes system Back to the side activated by a pointer or keyboard focus, not the other history", async () => {
  await start(); await open();
  act(() => {
    for (const app of contexts.values()) { app.ui.selectTab("work"); app.ui.push({ screen: "inbox" }); }
  });
  await tick();
  fireEvent.pointerDown(screen.getByRole("region", { name: "Right side" }));
  act(() => { expect(window.systemBack?.()).toBe(true); });
  expect(contexts.get("right")!.ui.route()).toEqual({ screen: "work" });
  expect(contexts.get("left")!.ui.route()).toEqual({ screen: "inbox" });
  fireEvent.focus(side("left").getByRole("button", { name: "Close left side" }));
  act(() => { expect(window.systemBack?.()).toBe(true); });
  expect(contexts.get("left")!.ui.route()).toEqual({ screen: "work" });
});

it("hides only the inactive side when narrowed and restores its untouched route when widened", async () => {
  await start(); await open();
  act(() => { contexts.get("left")!.ui.selectTab("work"); contexts.get("left")!.ui.push({ screen: "inbox" }); });
  await tick();
  const leftUi = contexts.get("left")!.ui;
  fireEvent.pointerDown(screen.getByRole("region", { name: "Right side" }));
  resize(915, 412); await tick();
  expect(screen.queryByRole("region", { name: "Left side" })).toBeNull();
  expect(screen.getByRole("region", { name: "Right side" })).toBeTruthy();
  expect(leftUi.route()).toEqual({ screen: "inbox" });
  resize(1280, 800); await tick();
  expect(screen.getByRole("region", { name: "Left side" })).toBeTruthy();
  expect(contexts.get("left")!.ui).toBe(leftUi);
  expect(leftUi.route()).toEqual({ screen: "inbox" });
});

it.each(["left", "right"] as const)("closing %s keeps the other side and its chosen computer across an App restart", async closing => {
  await start(); await open();
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  act(() => {
    for (const app of contexts.values()) {
      app.ui.selectTab("work"); app.ui.push({ screen: "inbox" }); app.ui.persistNow();
    }
  });
  await tick();
  fireEvent.click(side(closing).getByRole("button", { name: `Close ${closing} side` }));
  await tick();
  const remaining = closing === "left" ? "right" : "left";
  const expected = remaining === "right" ? spare.profileId : profile.profileId;
  expect(screen.queryByRole("region", { name: closing === "left" ? "Left side" : "Right side" })).toBeNull();
  expect(contexts.get(remaining)!.profile.profileId).toBe(expected);
  cleanup(); contexts.clear(); render(<App />); await tick();
  expect(screen.getByRole("region", { name: remaining === "left" ? "Left side" : "Right side" })).toBeTruthy();
  expect(screen.queryByRole("region", { name: closing === "left" ? "Left side" : "Right side" })).toBeNull();
  expect(contexts.get(remaining)!.profile.profileId).toBe(expected);
  expect(contexts.get(remaining)!.ui.route()).toEqual({ screen: "inbox" });
});

it("does not ask to leave control while the same computer is still visible on the other side", async () => {
  lease(); await start(); await open();
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  expect(contexts.get("right")!.profile.profileId).toBe(spare.profileId);
  expect(screen.queryByRole("dialog", { name: "Leave studio?" })).toBeNull();
  expect(links.get(profile.profileId)![0]!.sent.filter(c => c.command === "release_lease")).toHaveLength(0);
  act(() => contexts.get("left")!.switchTo(spare.profileId)); await tick();
  expect(screen.getByRole("dialog", { name: "Leave studio?" })).toBeTruthy();
  expect(contexts.get("left")!.profile.profileId).toBe(profile.profileId);
  fireEvent.click(screen.getByRole("button", { name: "Leave control and switch" })); await tick();
  expect(contexts.get("left")!.profile.profileId).toBe(spare.profileId);
  expect(links.get(profile.profileId)![0]!.sent.filter(c => c.command === "release_lease")).toHaveLength(1);
});

it("closing a duplicate view is safe, but closing the last visible view keeps the leave-control question", async () => {
  lease(); await start(); await open();
  fireEvent.click(side("right").getByRole("button", { name: "Close right side" })); await tick();
  expect(screen.queryByRole("dialog", { name: "Leave studio?" })).toBeNull();
  expect(links.get(profile.profileId)![0]!.sent.filter(c => c.command === "release_lease")).toHaveLength(0);
  await open();
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  fireEvent.click(side("left").getByRole("button", { name: "Close left side" })); await tick();
  expect(screen.getByRole("dialog", { name: "Leave studio?" })).toBeTruthy();
  expect(screen.getByRole("region", { name: "Left side" })).toBeTruthy();
});

it("a hidden duplicate does not bypass leave-control protection in a narrow window", async () => {
  lease(); await start(); await open();
  fireEvent.pointerDown(screen.getByRole("region", { name: "Right side" }));
  resize(915, 412); await tick();
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  expect(screen.getByRole("dialog", { name: "Leave studio?" })).toBeTruthy();
  expect(contexts.get("right")!.profile.profileId).toBe(profile.profileId);
});

it("pairing from duplicate controlled views still asks once and releases the computer once", async () => {
  lease(); await start(); await open();
  act(() => contexts.get("right")!.pairAnother()); await tick();
  const question = screen.getByRole("dialog", { name: "Leave control before pairing?" });
  expect(screen.getAllByRole("dialog", { name: "Leave control before pairing?" })).toHaveLength(1);
  expect(within(question).getByRole("button", { name: "Stop on studio" })).toBeTruthy();
  expect(links.get(profile.profileId)![0]!.sent.filter(c => c.command === "release_lease")).toHaveLength(0);
  fireEvent.click(within(question).getByRole("button", { name: "Leave control and pair" })); await tick();
  expect(links.get(profile.profileId)![0]!.sent.filter(c => c.command === "release_lease")).toHaveLength(1);
  expect(screen.queryByRole("region", { name: "Left side" })).toBeNull();
  expect(screen.queryByRole("region", { name: "Right side" })).toBeNull();
});

it("pairing from a safe side still protects the other visible computer's control", async () => {
  lease(); await start(); await open();
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  expect(contexts.get("right")!.profile.profileId).toBe(spare.profileId);
  act(() => contexts.get("right")!.pairAnother()); await tick();
  const question = screen.getByRole("dialog", { name: "Leave control before pairing?" });
  expect(within(question).getByRole("button", { name: "Stop on studio" })).toBeTruthy();
  expect(within(question).queryByRole("button", { name: `Stop on ${spare.name}` })).toBeNull();
  fireEvent.click(within(question).getByRole("button", { name: "Leave control and pair" })); await tick();
  expect(links.get(profile.profileId)![0]!.sent.filter(c => c.command === "release_lease")).toHaveLength(1);
  expect(links.get(spare.profileId)![0]!.sent.filter(c => c.command === "release_lease")).toHaveLength(0);
});

it.each(["left", "right"] as const)("one shared-computer login notice opens its task on the currently active %s side", async targetSide => {
  await start(); await open();
  act(() => contexts.get("right")!.ui.selectTab("work")); await tick();
  fireEvent.pointerDown(screen.getByRole("region", { name: "Right side" }));
  const session = contexts.get("left")!.session;
  const task = { ...session.state.get().tasks[fx.ids.tFlaky]!, state: "working" as const };
  act(() => session.state.set(s => ({ ...s, tasks: { ...s.tasks, [task.task_id]: task }, agentLoginUse: {
    seq: 100, loginId: "split-login", name: "Shared login", site: "example.test", taskId: task.task_id,
  } })));
  await tick();
  const notices = screen.getByRole("region", { name: "Notifications" });
  expect(within(notices).getAllByText(/Shared login was used on example.test/)).toHaveLength(1);
  expect(screen.getAllByRole("button", { name: "Open task" })).toHaveLength(1);
  // The toast already exists before activation changes, so its callback must
  // consult the current side at click time rather than retain its birth side.
  fireEvent.pointerDown(screen.getByRole("region", { name: targetSide === "left" ? "Left side" : "Right side" }));
  fireEvent.click(screen.getByRole("button", { name: "Open task" })); await tick();
  const untouched = targetSide === "left" ? "right" : "left";
  expect(contexts.get(targetSide)!.ui.get().tab).toBe("talk");
  expect(contexts.get(targetSide)!.ui.get().taskByProject[task.project_id]).toBe(task.task_id);
  expect(contexts.get(untouched)!.ui.get().tab).toBe("work");
  expect(contexts.get(untouched)!.ui.get().taskByProject[task.project_id]).not.toBe(task.task_id);
});

it("keeps one global alert setup and its dismissal across active-side changes", async () => {
  const unasked = { settings: null, statuses: {}, android: { running: true, unrestricted: true } };
  fake.value.alerts = { state: async () => unasked, configure: async () => unasked,
    battery: async () => {}, channel: async () => {} };
  await start();
  expect(screen.getAllByRole("dialog", { name: "Alerts" })).toHaveLength(1);
  fireEvent.click(within(screen.getByRole("dialog", { name: "Alerts" })).getByRole("button", { name: "Close" }));
  await tick(); await open();
  for (const label of ["Right side", "Left side", "Right side"]) {
    fireEvent.pointerDown(screen.getByRole("region", { name: label })); await tick();
    expect(screen.queryByRole("dialog", { name: "Alerts" })).toBeNull();
  }
});

it("does not create a hidden duplicate editor on an ordinary phone or retain its removed markup", async () => {
  resize(390, 844);
  const store = annotationStore(profile.profileId);
  const { id } = store.create(fx.ids.pTide, null);
  const source = `${id}-image`;
  store.update(id, draft => ({ ...draft, sources: [{ key: "S1", blobKey: source, artifactId: null,
    sha256: "a".repeat(64), mediaType: "image/png", width: 600, height: 400, durationMs: null,
    origin: { kind: "phone_import", original_name: null }, capturedAt: 100, derivedFrom: null, derivation: null }], nextSource: 2 }));
  const seed = new UiStore(profile.profileId);
  seed.push({ screen: "annotate", annotationId: id });
  seed.saveDraft(fx.ids.pTide, null, { annotationId: id });
  seed.retire();
  const removeImage = vi.spyOn(blobs, "remove").mockResolvedValue(undefined);
  await start();
  const ui = contexts.get("left")!.ui;
  expect(ui.navigationStores()).toEqual([ui]);
  expect(ui.get().stacks.talk.at(-1)).toMatchObject({ screen: "annotate", annotationId: id });
  act(() => {
    ui.reset("talk");
    removeMarkup(ui, store, fx.ids.pTide, null, id);
  });
  await act(async () => { await vi.advanceTimersByTimeAsync(7100); });
  expect(store.get()[id]).toBeUndefined();
  expect(removeImage).toHaveBeenCalledExactlyOnceWith(source);
  expect(localStorage.getItem(`ui.${profile.profileId}.side.right`)).toBeNull();
});

it("retains a narrowed side's history for cleanup, but stops counting it when the side closes", async () => {
  await start(); await open();
  const left = contexts.get("left")!.ui, right = contexts.get("right")!.ui;
  expect(left.retainedNavigationStores()).toEqual([left, right]);
  fireEvent.pointerDown(screen.getByRole("region", { name: "Right side" }));
  resize(915, 412); await tick();
  expect(left.retainedNavigationStores()).toEqual([left, right]);
  resize(1280, 800); await tick();
  fireEvent.click(side("left").getByRole("button", { name: "Close left side" })); await tick();
  expect(left.retainedNavigationStores()).toEqual([right]);
  expect(left.navigationStores()).toEqual([left, right]);
});

it("restores an open side's editor on another computer before resuming removed-image cleanup", async () => {
  await start(); await open();
  const left = contexts.get("left")!.ui, right = contexts.get("right")!.ui;
  const store = annotationStore(profile.profileId), { id } = store.create(fx.ids.pTide, null);
  const source = `${id}-image`;
  store.update(id, draft => ({ ...draft, generalNote: "Keep this pending drawing", sources: [{
    key: "S1", blobKey: source, artifactId: null, sha256: "b".repeat(64), mediaType: "image/png",
    width: 600, height: 400, durationMs: null, origin: { kind: "phone_import", original_name: null },
    capturedAt: 100, derivedFrom: null, derivation: null,
  }], nextSource: 2 }));
  const removeImage = vi.spyOn(blobs, "remove").mockResolvedValue(undefined);
  act(() => {
    left.saveDraft(fx.ids.pTide, null, { annotationId: id });
    right.push({ screen: "annotate", annotationId: id });
    removeMarkup(left, store, fx.ids.pTide, null, id);
  });
  await act(async () => { await vi.advanceTimersByTimeAsync(7100); });
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  expect(right.navigationRetained.get()).toBe(true);
  expect(store.get()[id]).toBeDefined();
  left.persistNow(); right.persistNow();
  cleanup(); contexts.clear();
  forgetRemovedMarkup(left);
  for (const view of left.navigationStores()) view.retire();
  render(<App />); await tick();
  const restoredLeft = contexts.get("left")!.ui;
  const restoredRight = restoredLeft.navigationStores().find(view => view.navigationSide() === "right");
  expect(contexts.get("right")!.profile.profileId).toBe(spare.profileId);
  expect(restoredRight?.route()).toMatchObject({ screen: "annotate", annotationId: id });
  expect(restoredRight?.navigationRetained.get()).toBe(true);
  expect(store.get()[id]).toBeDefined();
  expect(removeImage).not.toHaveBeenCalled();
  act(() => contexts.get("right")!.switchTo(profile.profileId)); await tick();
  expect(contexts.get("right")!.ui).toBe(restoredRight);
  expect(contexts.get("right")!.ui.route()).toMatchObject({ screen: "annotate", annotationId: id });
  act(() => contexts.get("right")!.ui.back()); await tick();
  expect(store.get()[id]).toBeUndefined();
  expect(removeImage).toHaveBeenCalledExactlyOnceWith(source);
});

it.each([false, true])("pairing stops viewing every computer, raises new attention, and allows idle disconnect (split %s)", async split => {
  const notify = vi.fn(async () => {});
  fake.value.notify = notify;
  fake.value.notificationPermission = async () => "granted";
  await start();
  if (split) await open();
  const app = contexts.get(split ? "right" : "left")!;
  const computers = app.computers!;
  expect(computers.viewing.get()).toBe(profile.profileId);
  expect(computers.visible.get().has(profile.profileId)).toBe(true);
  act(() => app.pairAnother()); await tick();
  expect(screen.queryByRole("region", { name: "Left side" })).toBeNull();
  expect(screen.queryByRole("region", { name: "Right side" })).toBeNull();
  expect(computers.viewing.get()).toBeNull();
  expect(computers.visible.get().size).toBe(0);
  expect(computers.showingAll.get()).toBe(false);
  notify.mockClear();
  const attention = app.session.state.get().attention[fx.ids.atApproval]!;
  act(() => app.session.state.set(s => ({ ...s, attention: { ...s.attention,
    "while-pairing": { ...attention, attention_id: "while-pairing" } } })));
  await tick();
  expect(notify).toHaveBeenCalledWith(expect.objectContaining({ id: `${profile.profileId}/while-pairing` }));
  const descriptor = Object.getOwnPropertyDescriptor(document, "visibilityState");
  try {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    fireEvent(document, new Event("visibilitychange"));
    await act(async () => { await vi.advanceTimersByTimeAsync(PARK_AFTER_MS + 1); });
    expect(links.get(profile.profileId)![0]!.stopped).toBe(true);
    expect(computers.peek(profile.profileId)).toBeUndefined();
    // A background layout update must not let shared-work rendering open the
    // parked connection again or publish a registry change during render.
    resize(1278, 800); await tick();
    expect(computers.peek(profile.profileId)).toBeUndefined();
    expect(links.get(profile.profileId)).toHaveLength(1);
  } finally {
    if (descriptor) Object.defineProperty(document, "visibilityState", descriptor);
    else Reflect.deleteProperty(document, "visibilityState");
    fireEvent(document, new Event("visibilitychange"));
  }
});

it.each([
  { state: "flat", separating: false, occlusion: "none", width: 0 },
  { state: "half-opened", separating: true, occlusion: "none", width: 0 },
  { state: "flat", separating: true, occlusion: "full", width: 0 },
  { state: "half-opened", separating: true, occlusion: "none", width: 24 },
] as const)("uses a full-width single view for a $state crease with $occlusion occlusion and width $width", async ({ state, separating, occlusion, width }) => {
  resize(690, 830);
  fake.value.windowLayout = async () => ({ availability: "available", width: 690, height: 830, folds: [{
    bounds: { x: 333, y: 0, width, height: 830 }, orientation: "vertical", state, separating, occlusion,
  }] });
  await start();
  const space = document.querySelector<HTMLElement>(".split-space")!;
  expect(space.style.gridTemplateColumns).toBe("");
  expect(document.querySelector<HTMLElement>(".split-slot")!.style.gridColumn).toBe("1");
  expect(document.querySelector(".split-invitation")).toBeNull();
  expect(document.querySelector(".split-divider")).toBeNull();
  expect(side("left").getAllByRole("button", { name: "Open split view" }).length).toBeGreaterThan(0);
  expect(screen.queryByRole("region", { name: "Right side" })).toBeNull();
  await open();
  expect(space.style.gridTemplateColumns).toBe(`333px ${width}px minmax(0, 1fr)`);
  expect(document.querySelector(".split-divider")).not.toBeNull();
  fireEvent.click(side("left").getByRole("button", { name: "Close left side" })); await tick();
  expect(screen.getByRole("region", { name: "Right side" })).toBeTruthy();
  expect(space.style.gridTemplateColumns).toBe("");
  expect(document.querySelector<HTMLElement>(".split-slot")!.style.gridColumn).toBe("1");
  expect(document.querySelector(".split-invitation")).toBeNull();
  expect(document.querySelector(".split-divider")).toBeNull();
  expect(side("right").getAllByRole("button", { name: "Open split view" }).length).toBeGreaterThan(0);
});

it("keeps a single view and its invitation on opposite sides of a physical occluding gap", async () => {
  resize(690, 830);
  fake.value.windowLayout = async () => ({ availability: "available", width: 690, height: 830, folds: [{
    bounds: { x: 333, y: 0, width: 24, height: 830 }, orientation: "vertical", state: "flat", separating: true, occlusion: "full",
  }] });
  await start();
  const space = document.querySelector<HTMLElement>(".split-space")!;
  expect(space.style.gridTemplateColumns).toBe("333px 24px minmax(0, 1fr)");
  expect(document.querySelector<HTMLElement>(".split-slot")!.style.gridColumn).toBe("1");
  const invitation = document.querySelector<HTMLElement>(".split-invitation")!;
  expect(invitation.style.gridColumn).toBe("3");
  expect(within(invitation).getByRole("button", { name: "Open split view" })).toBeTruthy();
  expect(document.querySelector<HTMLElement>(".split-divider")!.style.gridColumn).toBe("2");
  await open();
  expect(space.style.gridTemplateColumns).toBe("333px 24px minmax(0, 1fr)");
  expect(document.querySelector(".split-invitation")).toBeNull();
  fireEvent.click(side("left").getByRole("button", { name: "Close left side" })); await tick();
  expect(document.querySelector<HTMLElement>(".split-slot")!.style.gridColumn).toBe("3");
  expect(document.querySelector<HTMLElement>(".split-invitation")!.style.gridColumn).toBe("1");
  expect(document.querySelector(".split-divider")).not.toBeNull();
});

it("reacts to native portrait fold updates and unsubscribes when the App unmounts", async () => {
  resize(690, 830);
  let receive: ((layout: WindowLayout) => void) | undefined;
  const off = vi.fn();
  fake.value.onWindowLayout = listener => { receive = listener; return off; };
  fake.value.windowLayout = async () => ({ availability: "available", width: 690, height: 830, folds: [] });
  await start();
  expect(side("left").queryByRole("button", { name: "Open split view" })).toBeNull();
  act(() => receive!({ availability: "available", width: 690, height: 830, folds: [{
    bounds: { x: 345, y: 0, width: 0, height: 830 }, orientation: "vertical", state: "flat", separating: false, occlusion: "none",
  }] }));
  await tick(); await open();
  expect(document.querySelector<HTMLElement>(".split-space")!.style.gridTemplateColumns).toBe("345px 0px minmax(0, 1fr)");
  cleanup(); expect(off).toHaveBeenCalledTimes(1);
});

it("restores an open split with both histories and the active right side after restarting", async () => {
  await start(); await open();
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  act(() => {
    const left = contexts.get("left")!.ui, right = contexts.get("right")!.ui;
    left.selectProject(fx.ids.pTide); left.selectTab("work"); left.push({ screen: "inbox" });
    right.selectProject(fx.ids.pNotes); right.selectTab("work");
    left.persistNow(); right.persistNow();
  });
  await tick();
  fireEvent.pointerDown(screen.getByRole("region", { name: "Right side" }));
  cleanup(); contexts.clear(); render(<App />); await tick();
  expect(screen.getByRole("region", { name: "Left side" })).toBeTruthy();
  expect(screen.getByRole("region", { name: "Right side" }).classList.contains("pane-active")).toBe(true);
  expect(contexts.get("left")!.ui.route()).toEqual({ screen: "inbox" });
  expect(contexts.get("left")!.ui.get().projectId).toBe(fx.ids.pTide);
  expect(contexts.get("right")!.profile.profileId).toBe(spare.profileId);
  expect(contexts.get("right")!.ui.get().projectId).toBe(fx.ids.pNotes);
  resize(915, 412); await tick();
  expect(screen.queryByRole("region", { name: "Left side" })).toBeNull();
  expect(screen.getByRole("region", { name: "Right side" })).toBeTruthy();
});

it("can reopen from the surviving right side and keeps that side's project", async () => {
  await start(); await open();
  act(() => { contexts.get("right")!.ui.selectProject(fx.ids.pNotes); contexts.get("right")!.ui.selectTab("work"); });
  await tick();
  fireEvent.click(side("left").getByRole("button", { name: "Close left side" })); await tick();
  fireEvent.click(side("right").getAllByRole("button", { name: "Open split view" })[0]!); await tick();
  expect(contexts.get("left")!.ui.get()).toMatchObject({ tab: "talk", projectId: fx.ids.pNotes });
  expect(contexts.get("right")!.ui.get()).toMatchObject({ tab: "work", projectId: fx.ids.pNotes });
});

it("does not use stale native fold geometry after the app window changes width", async () => {
  resize(690, 830);
  fake.value.windowLayout = async () => ({ availability: "available", width: 690, height: 830, folds: [{
    bounds: { x: 345, y: 0, width: 0, height: 830 }, orientation: "vertical", state: "flat", separating: false, occlusion: "none",
  }] });
  await start(); await open();
  resize(800, 1280); await tick();
  expect(screen.queryByRole("region", { name: "Right side" })).toBeNull();
  expect(screen.getByRole("region", { name: "Left side" })).toBeTruthy();
  resize(690, 830); await tick();
  expect(screen.getByRole("region", { name: "Right side" })).toBeTruthy();
});

it("keeps a hidden computer parked through background layout updates without remounting its screens", async () => {
  await start(); await open();
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  const hidden = contexts.get("left")!, previousLink = links.get(profile.profileId)![0]!;
  act(() => { hidden.ui.selectTab("work"); hidden.ui.push({ screen: "inbox" }); });
  fireEvent.pointerDown(screen.getByRole("region", { name: "Right side" }));
  resize(915, 412); await tick();
  const pane = document.querySelector('[aria-label="Left side"]');
  const descriptor = Object.getOwnPropertyDescriptor(document, "visibilityState");
  const errors = vi.spyOn(console, "error");
  try {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    fireEvent(document, new Event("visibilitychange"));
    await act(async () => { await vi.advanceTimersByTimeAsync(PARK_AFTER_MS + 1); });
    expect(previousLink.stopped).toBe(true);
    expect(hidden.computers!.peek(profile.profileId)).toBeUndefined();
    resize(914, 412); await tick();
    expect(hidden.computers!.peek(profile.profileId)).toBeUndefined();
    expect(links.get(profile.profileId)).toHaveLength(1);
    expect(document.querySelector('[aria-label="Left side"]')).toBe(pane);
    expect(contexts.get("left")!.ui).toBe(hidden.ui);
    expect(hidden.ui.route()).toEqual({ screen: "inbox" });
    expect(errors.mock.calls.filter(args => String(args[0]).includes("Cannot update a component"))).toEqual([]);
  } finally {
    if (descriptor) Object.defineProperty(document, "visibilityState", descriptor);
    else Reflect.deleteProperty(document, "visibilityState");
    fireEvent(document, new Event("visibilitychange"));
  }
});

it("restores the current live session and fresh state after a hidden side's computer was parked in the background", async () => {
  await start(); await open();
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  const previous = contexts.get("left")!.session;
  const previousLink = links.get(profile.profileId)![0]!;
  const computers = contexts.get("left")!.computers!;
  fireEvent.pointerDown(screen.getByRole("region", { name: "Right side" }));
  resize(915, 412); await tick();
  expect(screen.queryByRole("region", { name: "Left side" })).toBeNull();
  expect(computers.viewing.get()).toBe(spare.profileId);
  const descriptor = Object.getOwnPropertyDescriptor(document, "visibilityState");
  try {
    // The next connection must contain an observation the retired session
    // never saw, so identity alone cannot make this regression pass.
    fake.value.control = id => {
      const link = makeLink(id);
      if (id === profile.profileId) link.snap.host = { ...link.snap.host, name: "Freshly returned studio" };
      return link;
    };
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    fireEvent(document, new Event("visibilitychange"));
    await act(async () => { await vi.advanceTimersByTimeAsync(PARK_AFTER_MS + 1); });
    expect(previousLink.stopped).toBe(true);
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    fireEvent(document, new Event("visibilitychange"));
    await tick();
    resize(1280, 800); await tick();
    expect(screen.getByRole("region", { name: "Left side" })).toBeTruthy();
    const restored = contexts.get("left")!.session;
    expect(restored).toBe(computers.peek(profile.profileId));
    expect(restored).not.toBe(previous);
    expect(restored.link.get().state).toBe("online");
    expect(restored.state.get().host?.name).toBe("Freshly returned studio");
    expect(previous.state.get().host?.name).not.toBe("Freshly returned studio");
    expect(links.get(profile.profileId)!.at(-1)!.stopped).toBe(false);
  } finally {
    if (descriptor) Object.defineProperty(document, "visibilityState", descriptor);
    else Reflect.deleteProperty(document, "visibilityState");
    fireEvent(document, new Event("visibilitychange"));
  }
});
