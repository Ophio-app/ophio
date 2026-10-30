// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { webm, type VideoPacket } from "./webm";
import { frameTimes, validateClipRange } from "./clipEncoding";
const packet = (
  timestamp: number,
  duration: number,
  key = false,
): VideoPacket => ({
  timestamp,
  duration,
  key,
  bytes: new Uint8Array([1, 2, 3]),
});
it("refuses missing, reordered and incomplete encoded frame sequences", () => {
  for (const packets of [
    [],
    [packet(0, 67)],
    [packet(67, 67, true)],
    [packet(0, 67, true), packet(0, 67)],
    [packet(0, 67, true), packet(80, 20)],
    [packet(0, 67, true)],
  ])
    expect(() => webm(packets, 480, 270, 100)).toThrow();
  expect(webm([packet(0, 67, true), packet(67, 33)], 480, 270, 100).type).toBe(
    "video/webm",
  );
});
it("bounds every key frame inside even a short selected range, never at the exclusive end", () => {
  for (const range of [
    { start: 2, end: 5 },
    { start: 2.01, end: 2.02 },
    { start: 0, end: 60 },
  ]) {
    const frames = frameTimes(range);
    expect(frames).toHaveLength(3);
    expect(
      frames.every((time) => time >= range.start && time < range.end),
    ).toBe(true);
    expect(new Set(frames).size).toBe(3);
  }
});
it("refuses empty or unbounded exports before opening a decoder", () => {
  const clip = new Blob(["fixture"]);
  for (const range of [
    { start: -1, end: 2 },
    { start: 5, end: 2 },
    { start: 0, end: 61 },
    { start: 0, end: NaN },
  ])
    expect(() => validateClipRange(clip, range)).toThrow();
  expect(() => validateClipRange(new Blob(), { start: 2, end: 5 })).toThrow();
  expect(() => validateClipRange(clip, { start: 2, end: 5 })).not.toThrow();
});
