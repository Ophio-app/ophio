// SPDX-License-Identifier: Apache-2.0
/** A native fold in layout-viewport CSS pixels, relative to the app WebView.
 * A crease can have zero thickness. Flat, non-separating folds are retained:
 * layout eligibility belongs to the app, not to the native bridge. */
export interface WindowFold {
  bounds: { x: number; y: number; width: number; height: number };
  orientation: "horizontal" | "vertical";
  state: "flat" | "half-opened";
  separating: boolean;
  occlusion: "none" | "full";
}

export interface WindowLayout {
  /** Available means native fold geometry is observable, even with no folds.
   * Apple's posture-only API does not make geometry available. */
  availability: "available" | "unavailable";
  width: number;
  height: number;
  folds: WindowFold[];
  /** Apple supplies hinge posture, but no position. Angle is in radians. */
  posture?: {
    status: "unknown" | "closed" | "partially-open" | "fully-open";
    angle: number;
  };
}

export function unavailableWindowLayout(width = 0, height = 0): WindowLayout {
  return { availability: "unavailable", width, height, folds: [] };
}

/** Reject malformed snapshots without inventing a crease or using screen size. */
export function parseWindowLayout(value: unknown): WindowLayout | null {
  if (!value || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  const length = (n: unknown): n is number => typeof n === "number" && Number.isFinite(n) && n >= 0;
  const number = (n: unknown): n is number => typeof n === "number" && Number.isFinite(n);
  if ((v.availability !== "available" && v.availability !== "unavailable") ||
      !length(v.width) || !length(v.height) || !Array.isArray(v.folds)) return null;
  const folds: WindowFold[] = [];
  for (const item of v.folds) {
    if (!item || typeof item !== "object") return null;
    const f = item as Record<string, unknown>;
    const b = f.bounds as Record<string, unknown> | null;
    if (!b || !number(b.x) || !number(b.y) || !length(b.width) || !length(b.height) ||
        (f.orientation !== "horizontal" && f.orientation !== "vertical") ||
        (f.state !== "flat" && f.state !== "half-opened") ||
        typeof f.separating !== "boolean" || (f.occlusion !== "none" && f.occlusion !== "full")) return null;
    folds.push({ bounds: { x: b.x, y: b.y, width: b.width, height: b.height },
      orientation: f.orientation, state: f.state, separating: f.separating, occlusion: f.occlusion });
  }
  // A posture-only source must not smuggle guessed geometry into the layout.
  if (v.availability === "unavailable" && folds.length) return null;
  const result: WindowLayout = { availability: v.availability, width: v.width, height: v.height, folds };
  if (v.posture !== undefined) {
    if (!v.posture || typeof v.posture !== "object") return null;
    const p = v.posture as Record<string, unknown>;
    if ((p.status !== "unknown" && p.status !== "closed" && p.status !== "partially-open" && p.status !== "fully-open") ||
        !number(p.angle)) return null;
    result.posture = { status: p.status, angle: p.angle };
  }
  return result;
}
