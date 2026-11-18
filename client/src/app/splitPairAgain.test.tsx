// SPDX-License-Identifier: Apache-2.0
// Real App, Shell and split controls. Welcome alone replaces its native
// pairing ceremony with the same onPaired callback the actual screen uses.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { useApp, type App as AppContext } from "./context";
import { prefs } from "./prefs";
import { usePane } from "../shell/pane";
import { FakeLink, platform, profile } from "../platform/lifecycle.fixtures";
import { secondProfile } from "../dev/sim/lifecycle";
import { ids } from "../dev/sim/scenario";
import type { HostProfile } from "../host/link";

let fake: ReturnType<typeof platform>;
let first: HostProfile, again: HostProfile, spare: HostProfile;
let links: Map<string, FakeLink[]>;
let run = 0;
const contexts = new Map<string, AppContext>();
vi.mock("../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../screens/welcome/Welcome", () => ({ Welcome: ({ onPaired }: { onPaired(p: HostProfile): void }) =>
  <button onClick={() => onPaired(again)}>Complete replacement pairing</button> }));
vi.mock("../shell/Shell", async original => {
  const actual = await original<typeof import("../shell/Shell")>();
  return { ...actual, Shell: function ObservedShell() {
    const app = useApp(), pane = usePane();
    if (pane) contexts.set(pane.id, app);
    return <actual.Shell />;
  } };
});
async function tick() {
  // Lazy screens settle separately from the effects that run after mounting.
  for (let i = 0; i < 2; i++) await act(async () => {
    await vi.dynamicImportSettled(); await vi.advanceTimersByTimeAsync(100);
  });
}
const region = (side: "left" | "right") => screen.getByRole("region", { name: side === "left" ? "Left side" : "Right side" });
async function open() {
  render(<App />); await tick();
  act(() => contexts.get("left")!.ui.selectTab("work")); await tick();
  fireEvent.click(within(region("left")).getAllByRole("button", { name: "Open split view" })[0]!);
  await tick();
  expect(region("left")).toBeTruthy(); expect(region("right")).toBeTruthy();
}
async function pairFrom(side: "left" | "right") {
  fireEvent.pointerDown(region(side));
  act(() => contexts.get(side)!.pairAnother()); await tick();
  expect(screen.queryByRole("region", { name: "Left side" })).toBeNull();
  expect(screen.queryByRole("region", { name: "Right side" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Complete replacement pairing" })); await tick();
}
beforeEach(() => {
  vi.useFakeTimers(); localStorage.clear(); contexts.clear(); links = new Map(); run++;
  first = { ...profile, profileId: `split-pair-first-${run}` };
  again = { ...first, profileId: `split-pair-again-${run}`, deviceId: "replacement-device", pairedAt: 2 };
  spare = { ...secondProfile(first), profileId: `split-pair-spare-${run}` };
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  Element.prototype.scrollTo ??= () => {};
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  Object.defineProperty(window, "innerWidth", { configurable: true, value: 1280 });
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
  prefs.set(p => ({ ...p, theme: "system", reduceMotion: false, lastProfileId: null,
    allComputers: false, allProjects: false, warningsOff: false }));
  fake = platform();
  fake.value.profiles = async () => [first, spare];
  fake.value.control = id => {
    const link = new FakeLink();
    const paired = [first, again, spare].find(p => p.profileId === id)!;
    link.snap.host = { ...link.snap.host, host_id: paired.hostId, name: paired.name };
    if (id === spare.profileId) { link.snap.tasks = []; link.snap.runs = []; link.snap.approvals = []; link.snap.attention = []; }
    links.set(id, [...(links.get(id) ?? []), link]);
    return link;
  };
});
afterEach(() => {
  cleanup(); contexts.clear();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});

it.each(["left", "right"] as const)("re-pairing from %s replaces the old pairing on both sides and keeps one shared draft", async side => {
  await open();
  const oldSession = contexts.get("left")!.session;
  act(() => contexts.get("left")!.ui.saveDraft(ids.pTide, null, { text: "Started on the left" }));
  expect(contexts.get("right")!.ui.draft(ids.pTide, null)?.text).toBe("Started on the left");
  act(() => contexts.get("right")!.ui.saveDraft(ids.pTide, null, { text: "Continued on the right before pairing again" }));
  await pairFrom(side);
  expect(region("left")).toBeTruthy(); expect(region("right")).toBeTruthy();
  const left = contexts.get("left")!, right = contexts.get("right")!;
  for (const app of [left, right]) {
    expect(app.profile.profileId).toBe(again.profileId);
    expect(app.profiles.some(p => p.profileId === first.profileId)).toBe(false);
    expect(app.session.link.get().state).toBe("online");
    expect(app.ui.draft(ids.pTide, null)?.text).toBe("Continued on the right before pairing again");
  }
  expect(left.session).toBe(right.session);
  expect(left.session).not.toBe(oldSession);
  expect(left.ui).not.toBe(right.ui);
  expect(left.ui.sharedStore()).toBe(right.ui.sharedStore());
  expect(links.get(first.profileId)![0]!.stopped).toBe(true);
  expect(links.get(again.profileId)).toHaveLength(1);
  act(() => right.ui.saveDraft(ids.pTide, null, { text: "Still shared after re-pairing" }));
  expect(left.ui.draft(ids.pTide, null)?.text).toBe("Still shared after re-pairing");
});

it("replaces one computer without moving the other side's computer, project, history, draft or session", async () => {
  await open();
  act(() => contexts.get("right")!.switchTo(spare.profileId)); await tick();
  const other = contexts.get("right")!;
  act(() => {
    contexts.get("left")!.ui.saveDraft(ids.pTide, null, { text: "Draft on the computer being re-paired" });
    other.ui.selectProject(ids.pNotes); other.ui.selectTab("work"); other.ui.push({ screen: "inbox" });
    other.ui.saveDraft(ids.pNotes, null, { text: "Unrelated computer's draft" });
  });
  await tick(); await pairFrom("left");
  expect(region("left")).toBeTruthy(); expect(region("right")).toBeTruthy();
  expect(contexts.get("left")!.profile.profileId).toBe(again.profileId);
  expect(contexts.get("left")!.ui.draft(ids.pTide, null)?.text).toBe("Draft on the computer being re-paired");
  const restored = contexts.get("right")!;
  expect(restored.profile.profileId).toBe(spare.profileId);
  expect(restored.session).toBe(other.session);
  expect(restored.ui).toBe(other.ui);
  expect(restored.ui.get().projectId).toBe(ids.pNotes);
  expect(restored.ui.route()).toEqual({ screen: "inbox" });
  expect(restored.ui.draft(ids.pNotes, null)?.text).toBe("Unrelated computer's draft");
  expect(links.get(spare.profileId)).toHaveLength(1);
  expect(links.get(spare.profileId)![0]!.stopped).toBe(false);
});
