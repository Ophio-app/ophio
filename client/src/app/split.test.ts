// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { browserWindowLayout, SplitStore, splitGeometry } from "./split";
import { UiStore } from "./ui";
import { profile } from "../platform/lifecycle.fixtures";
import { secondProfile } from "../dev/sim/lifecycle";
import type { WindowFold, WindowLayout } from "../platform/windowLayout";

const spare = secondProfile(profile);
function fold(bounds: WindowFold["bounds"], orientation: WindowFold["orientation"] = "vertical"): WindowLayout {
  return { availability: "available", width: 690, height: 830,
    folds: [{ bounds, orientation, state: "flat", separating: false, occlusion: "none" }] };
}
beforeEach(() => localStorage.clear());
afterEach(() => vi.unstubAllGlobals());

describe("split eligibility and physical geometry", () => {
  it.each([
    [1280, 800, true], [890, 626, true], [915, 412, false],
    [800, 1280, false], [690, 830, false], [0, 830, false], [1280, 0, false],
  ])("uses the app window, not tablet or phone identity, at %i by %i", (width, height, eligible) => {
    expect(splitGeometry(width, height).eligible).toBe(eligible);
  });
  it.each(["flat", "half-opened"] as const)("keeps a %s non-occluding portrait crease eligible without segmenting a single view", state => {
    const layout = fold({ x: 345, y: 0, width: 0, height: 830 });
    layout.folds[0]!.state = state;
    layout.folds[0]!.separating = state === "half-opened";
    expect(splitGeometry(690, 830, layout))
      .toMatchObject({ eligible: true, left: 345, right: 345, gap: 0, fold: false });
  });
  it("does not segment a single view for a zero-width crease marked fully occluding", () => {
    const layout = fold({ x: 345, y: 0, width: 0, height: 830 });
    layout.folds[0]!.occlusion = "full";
    expect(splitGeometry(690, 830, layout))
      .toMatchObject({ eligible: true, left: 345, right: 345, gap: 0, fold: false });
  });
  it("preserves positive crease bounds without segmenting a non-occluding single view", () => {
    expect(splitGeometry(1000, 830, fold({ x: 340, y: 0, width: 24, height: 830 })))
      .toMatchObject({ eligible: true, left: 340, right: 636, gap: 24, fold: false });
  });
  it("keeps the actual off-centre crease and its occluded gap", () => {
    const layout = fold({ x: 340, y: 0, width: 24, height: 830 });
    layout.folds[0]!.occlusion = "full";
    expect(splitGeometry(1000, 830, layout))
      .toMatchObject({ eligible: true, left: 340, right: 636, gap: 24, fold: true });
  });
  it("does not squeeze a side below its usable width even on a physical fold", () => {
    expect(splitGeometry(690, 830, fold({ x: 319, y: 0, width: 0, height: 830 })).eligible).toBe(false);
  });
  it("does not turn a horizontal hinge into side-by-side panes on a wide window", () => {
    const layout = fold({ x: 0, y: 390, width: 1280, height: 20 }, "horizontal");
    expect(splitGeometry(1280, 800, layout)).toMatchObject({ eligible: false, fold: false, horizontal: layout.folds[0] });
  });
  it("keeps a horizontal physical hinge gap outside the larger usable segment", () => {
    const layout = fold({ x: 0, y: 300, width: 1280, height: 24 }, "horizontal");
    layout.folds[0]!.occlusion = "full";
    expect(splitGeometry(1280, 800, layout).safeHorizontal).toEqual({ top: 324, height: 476 });
    layout.folds[0]!.bounds.y = 500;
    expect(splitGeometry(1280, 800, layout).safeHorizontal).toEqual({ top: 0, height: 500 });
  });
  it.each([true, false])("uses the whole screen for a flat horizontal crease, separating=%s", separating => {
    const layout = fold({ x: 0, y: 400, width: 1280, height: 0 }, "horizontal");
    layout.folds[0]!.separating = separating;
    expect(splitGeometry(1280, 800, layout).safeHorizontal).toBeUndefined();
  });
  it("uses the whole screen for a non-occluding half-opened tabletop fold", () => {
    const layout = fold({ x: 0, y: 400, width: 1280, height: 0 }, "horizontal");
    layout.folds[0]!.state = "half-opened";
    layout.folds[0]!.separating = true;
    expect(splitGeometry(1280, 800, layout).safeHorizontal).toBeUndefined();
  });
  it("uses a vertical crease that extends beyond the window but intersects it meaningfully", () => {
    expect(splitGeometry(690, 830, fold({ x: 345, y: -100, width: 0, height: 1030 })))
      .toMatchObject({ eligible: true, left: 345, right: 345, fold: false });
  });
  it.each([
    { x: 345, y: 830, width: 0, height: 50 },
    { x: 345, y: -100, width: 0, height: 100 },
    { x: -20, y: 0, width: 10, height: 830 },
    { x: 700, y: 0, width: 0, height: 830 },
  ])("ignores a crease outside the current window: %o", bounds => {
    expect(splitGeometry(690, 830, fold(bounds))).toMatchObject({ eligible: false, fold: false });
  });
  it("does not let an out-of-window horizontal hinge veto a normal wide split", () => {
    expect(splitGeometry(1280, 800, fold({ x: 1280, y: 400, width: 100, height: 0 }, "horizontal")).eligible).toBe(true);
  });
});

describe("browser fold fallback", () => {
  it("returns no geometry when the browser supplies no segments or throws", () => {
    expect(browserWindowLayout()).toBeNull();
    vi.stubGlobal("getWindowSegments", () => { throw new Error("unavailable"); });
    expect(browserWindowLayout()).toBeNull();
  });
  it("orders segments and preserves an off-centre gap rather than assuming equal halves", () => {
    vi.stubGlobal("getWindowSegments", () => [new DOMRect(364, 0, 636, 830), new DOMRect(0, 0, 340, 830)]);
    expect(browserWindowLayout()?.folds[0]).toMatchObject({ orientation: "vertical", bounds: { x: 340, y: 0, width: 24, height: 830 } });
  });
  it("accepts a zero-width browser crease and identifies horizontal segments", () => {
    vi.stubGlobal("viewport", { segments: [new DOMRect(0, 0, 345, 830), new DOMRect(345, 0, 345, 830)] });
    expect(browserWindowLayout()?.folds[0]).toMatchObject({ orientation: "vertical", bounds: { x: 345, width: 0 } });
    vi.stubGlobal("viewport", { segments: [new DOMRect(0, 420, 690, 410), new DOMRect(0, 0, 690, 400)] });
    expect(browserWindowLayout()?.folds[0]).toMatchObject({ orientation: "horizontal", bounds: { x: 0, y: 400, width: 690, height: 20 } });
  });
  it("does not invent a fold for one segment", () => {
    vi.stubGlobal("viewport", { segments: [new DOMRect(0, 0, 690, 830)] });
    expect(browserWindowLayout()).toBeNull();
  });
});

describe("split choices across restart", () => {
  it("starts with the remembered computer and copies only the active choice when opened", () => {
    const store = new SplitStore([profile, spare], spare.profileId, true);
    expect(store.get()).toMatchObject({ open: false, active: "left", sides: { left: { profileId: spare.profileId, all: true } } });
    store.open(); store.choose("right", profile.profileId); store.showAll("right", false); store.activate("right");
    const restarted = new SplitStore([profile, spare], null, false);
    expect(restarted.get()).toEqual(store.get());
    expect(restarted.get().sides.left).toEqual({ profileId: spare.profileId, all: true });
    expect(restarted.get().sides.right).toEqual({ profileId: profile.profileId, all: false });
  });
  it.each(["left", "right"] as const)("closing %s preserves the other side as active on restart", side => {
    const store = new SplitStore([profile, spare], null, false);
    store.open(); store.choose("right", spare.profileId); store.close(side);
    const restarted = new SplitStore([profile, spare], null, false);
    expect(restarted.get()).toEqual(store.get());
    expect(restarted.get()).toMatchObject({ open: false, active: side === "left" ? "right" : "left" });
  });
  it("reconciles a forgotten computer without changing the other side or active side", () => {
    const store = new SplitStore([profile, spare], null, false);
    store.open(); store.choose("right", spare.profileId); store.activate("right"); store.reconcile([profile]);
    expect(store.get()).toMatchObject({ open: true, active: "right", sides: { left: { profileId: profile.profileId }, right: { profileId: profile.profileId } } });
    store.reconcile([]);
    expect(store.get().sides).toEqual({ left: { profileId: null, all: false }, right: { profileId: null, all: false } });
  });
  it("recovers stale saved computer choices without discarding the valid side", () => {
    const store = new SplitStore([profile, spare], null, false);
    store.open(); store.choose("right", spare.profileId); store.activate("right");
    expect(new SplitStore([profile], null, false).get()).toMatchObject({ open: true, active: "right", sides: { right: { profileId: profile.profileId } } });
  });
});

it("shares one chat draft in both directions without sharing project, tab, or route history", () => {
  const left = new UiStore("split-draft"), right = left.fork("right");
  try {
    left.selectProject("project-a"); left.selectTab("work");
    right.selectProject("project-b"); right.selectTab("control"); right.push({ screen: "connections" });
    left.saveDraft("project-a", null, { text: "first words" });
    expect(right.draft("project-a", null)?.text).toBe("first words");
    right.saveDraft("project-a", null, { text: "edited on the other side" });
    expect(left.draft("project-a", null)?.text).toBe("edited on the other side");
    expect(left.get()).toMatchObject({ projectId: "project-a", tab: "work" });
    expect(left.route()).toEqual({ screen: "work" });
    expect(right.get()).toMatchObject({ projectId: "project-b", tab: "control" });
    expect(right.route()).toEqual({ screen: "connections" });
    expect(left.fork("right")).toBe(right);
    left.persistNow(); right.persistNow();
    const restarted = new UiStore("split-draft"), restartedRight = restarted.fork("right");
    try {
      expect(restartedRight.route()).toEqual({ screen: "connections" });
      expect(restartedRight.draft("project-a", null)?.text).toBe("edited on the other side");
      expect(restarted.get().projectId).toBe("project-a");
    } finally { restartedRight.retire(); restarted.retire(); }
  } finally { right.retire(); left.retire(); }
});
