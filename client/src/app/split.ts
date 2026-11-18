// SPDX-License-Identifier: Apache-2.0
// Split navigation survives a fold or resize; computer work lives elsewhere.
import { Store } from "../state/store";
import type { HostProfile } from "../host/link";
import type { WindowLayout, WindowFold } from "../platform/windowLayout";
import { readSavedJson } from "../state/savedJson";

export type Side = "left" | "right";
export const SIDES: readonly Side[] = ["left", "right"];
export const otherSide = (side: Side): Side => side === "left" ? "right" : "left";
export interface SideState { profileId: string | null; all: boolean }
export interface SplitState { open: boolean; active: Side; sides: Record<Side, SideState> }
export class SplitStore extends Store<SplitState> {
  constructor(profiles: HostProfile[], last: string | null, all: boolean) {
    const first = profiles.find(p => p.profileId === last)?.profileId ?? profiles[0]?.profileId ?? null;
    let state: SplitState = { open: false, active: "left", sides: { left: { profileId: first, all }, right: { profileId: first, all } } };
    try {
      const { value } = readSavedJson(localStorage, "split-view.v1");
      const saved = value as Partial<SplitState> | null;
      if (saved && typeof saved.open === "boolean" && (saved.active === "left" || saved.active === "right")) {
        state = { ...state, open: saved.open, active: saved.active };
        for (const side of SIDES) {
          const choice = saved.sides?.[side];
          if (choice) state.sides[side] = { profileId: profiles.find(p => p.profileId === choice.profileId)?.profileId ?? first, all: choice.all === true };
        }
      }
    } catch { /* Existing damaged data remains untouched until a user changes the split. */ }
    super(state);
    this.subscribe(() => { try { localStorage.setItem("split-view.v1", JSON.stringify(this.get())); } catch { /* Navigation still works without local storage. */ } });
  }
  activate(side: Side) { if (this.get().active !== side) this.set(s => ({ ...s, active: side })); }
  choose(side: Side, profileId: string | null) { this.set(s => ({ ...s, sides: { ...s.sides, [side]: { ...s.sides[side], profileId } } })); }
  showAll(side: Side, all: boolean) { this.set(s => ({ ...s, sides: { ...s.sides, [side]: { ...s.sides[side], all } } })); }
  open() { this.set(s => ({ ...s, open: true, sides: { ...s.sides, [otherSide(s.active)]: { ...s.sides[s.active] } } })); }
  close(side: Side) { this.set(s => ({ ...s, open: false, active: otherSide(side) })); }
  reconcile(profiles: HostProfile[]) {
    const ids = new Set(profiles.map(p => p.profileId));
    if (SIDES.every(side => !this.get().sides[side].profileId || ids.has(this.get().sides[side].profileId!))) return;
    this.set(s => ({ ...s, sides: Object.fromEntries(SIDES.map(side => [side, { ...s.sides[side], profileId: ids.has(s.sides[side].profileId ?? "") ? s.sides[side].profileId : profiles[0]?.profileId ?? null }])) as SplitState["sides"] }));
  }
}
export interface SplitGeometry { eligible: boolean; left: number; right: number; gap: number; fold: boolean; horizontal?: WindowFold; safeHorizontal?: { top: number; height: number } }
export const MIN_SIDE_WIDTH = 320;
export const MIN_SPLIT_HEIGHT = 560;
/** A physical crease is meaningful only inside this app's current window. */
export function splitGeometry(width: number, height: number, layout?: WindowLayout | null): SplitGeometry {
  const folds = layout?.folds ?? [];
  const vertical = folds.find(f => f.orientation === "vertical" && f.bounds.y < height && f.bounds.y + f.bounds.height > 0 && f.bounds.x > 0 && f.bounds.x + f.bounds.width < width);
  const horizontal = folds.find(f => f.orientation === "horizontal" && f.bounds.x < width && f.bounds.x + f.bounds.width > 0 && f.bounds.y > 0 && f.bounds.y + f.bounds.height < height);
  const gap = vertical ? Math.max(0, vertical.bounds.width) : 1;
  const left = vertical ? vertical.bounds.x : (width - gap) / 2;
  const right = width - left - gap;
  // A crease, including tabletop posture, does not hide pixels. Reserve a
  // segment only for an actual horizontal hinge gap, never for its posture.
  const safeHorizontal = horizontal?.occlusion === "full" && horizontal.bounds.height > 0
    ? horizontal.bounds.y >= height - horizontal.bounds.y - horizontal.bounds.height
      ? { top: 0, height: horizontal.bounds.y }
      : { top: horizontal.bounds.y + horizontal.bounds.height, height: height - horizontal.bounds.y - horizontal.bounds.height }
    : undefined;
  // An unopened single view only needs segmentation around a physical gap.
  const fold = vertical?.occlusion === "full" && vertical.bounds.width > 0;
  return { eligible: Math.min(left, right) >= MIN_SIDE_WIDTH && height >= MIN_SPLIT_HEIGHT && (!!vertical || (!horizontal && width > height)), left, right, gap, fold, horizontal, safeHorizontal };
}
/** Browser segments are supplemental; native Android fold bounds take priority. */
export function browserWindowLayout(): WindowLayout | null {
  const browser = window as Window & { viewport?: { segments?: readonly DOMRect[] }; getWindowSegments?: () => readonly DOMRect[] };
  let segments: readonly DOMRect[] | undefined;
  try { segments = browser.viewport?.segments ?? browser.getWindowSegments?.(); } catch { return null; }
  if (!segments || segments.length !== 2) return null;
  const ordered = [...segments].sort((x, y) => x.left - y.left || x.top - y.top);
  const a = ordered[0]!, b = ordered[1]!;
  const vertical = b.left >= a.right;
  return { availability: "available", width: window.innerWidth, height: window.innerHeight, folds: [{ bounds: vertical
    ? { x: a.right, y: Math.max(a.top, b.top), width: b.left - a.right, height: Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) }
    : { x: Math.max(a.left, b.left), y: a.bottom, width: Math.min(a.right, b.right) - Math.max(a.left, b.left), height: b.top - a.bottom }, orientation: vertical ? "vertical" : "horizontal", state: "flat", separating: true, occlusion: "full" }] };
}
