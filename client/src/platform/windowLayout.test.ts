// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { parseWindowLayout, unavailableWindowLayout, type WindowLayout } from "./windowLayout";

const crease: WindowLayout = {
  availability: "available", width: 820, height: 900,
  folds: [{ bounds: { x: 410, y: -24, width: 0, height: 924 },
    orientation: "vertical", state: "flat", separating: false, occlusion: "none" }],
};

it("retains zero-width, flat, non-separating creases and native WebView offsets", () => {
  expect(parseWindowLayout(crease)).toEqual(crease);
});
it("retains horizontal occluding half-open folds in fractional CSS pixels", () => {
  const fold: WindowLayout = { availability: "available", width: 900, height: 820,
    folds: [{ bounds: { x: 0, y: 400.25, width: 900, height: 19.5 }, orientation: "horizontal",
      state: "half-opened", separating: true, occlusion: "full" }] };
  expect(parseWindowLayout(fold)).toEqual(fold);
});
it("distinguishes available no-fold geometry from unsupported systems", () => {
  expect(parseWindowLayout({ ...crease, folds: [] })?.availability).toBe("available");
  expect(parseWindowLayout(unavailableWindowLayout(390, 844))).toEqual(unavailableWindowLayout(390, 844));
});
it("accepts Apple posture without inventing bounds", () => {
  const value: WindowLayout = { ...unavailableWindowLayout(700, 900), posture: { status: "partially-open", angle: Math.PI / 2 } };
  expect(parseWindowLayout(value)).toEqual(value);
  expect(parseWindowLayout({ ...value, folds: crease.folds })).toBeNull();
});
it("rejects malformed and nonfinite geometry instead of guessing", () => {
  for (const value of [null, {}, { ...crease, width: NaN }, { ...crease, height: -1 },
    { ...crease, availability: true }, { ...crease, folds: [null] },
    { ...crease, folds: [{ ...crease.folds[0], bounds: { x: Infinity, y: 0, width: 0, height: 900 } }] },
    { ...crease, folds: [{ ...crease.folds[0], bounds: { x: 0, y: 0, width: -1, height: 900 } }] },
    { ...crease, posture: { status: "open", angle: 3 } },
    { ...crease, posture: { status: "unknown", angle: Infinity } }]) {
    expect(parseWindowLayout(value)).toBeNull();
  }
});
