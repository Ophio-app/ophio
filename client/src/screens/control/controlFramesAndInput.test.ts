// SPDX-License-Identifier: Apache-2.0
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  buildTileFrame,
  parseTileFrame,
  terminalFrame,
  type TileFrameHeader,
} from "../../protocol/binary";
import type { InputEnvelope, LeaseView } from "../../protocol/types";
import {
  isDoubleTap,
  panWithin,
  targetPoint,
  recognizeGesture,
  pinchView,
  visibleRegion,
} from "./gestures";
import { keyChord, terminalKey } from "./keys";
import { blitRgba, TileModel } from "./tiles";
import {
  FRAME_STALE_MS,
  frameFresh,
  InputSender,
  renewalDelay,
  scheduleRenewal,
} from "./lease";
import { cleanPaste, pasteLines, TerminalOffset } from "./terminalBuffer";

const header: TileFrameHeader = {
  keyframe: true,
  cursor: null,
  tile: 2,
  seq: 1,
  geometryRevision: 3,
  encWidth: 3,
  encHeight: 2,
  srcWidth: 6,
  srcHeight: 4,
  region: null,
  count: 2,
};
const tiles = [
  { col: 0, row: 0, jpeg: new Uint8Array([1]) },
  { col: 1, row: 0, jpeg: new Uint8Array([2]) },
];
const lease: LeaseView = {
  lease_id: "lease",
  target_id: "display",
  owner: { kind: "human", device_id: "phone" },
  generation: 4,
  boot_epoch: "boot",
  state: "active",
  granted_at: 0,
  renewal_window_ms: 3000,
};
afterEach(() => vi.useRealTimers());

describe("TDV1 composition", () => {
  it("composes a complete keyframe and a delta without disturbing unchanged pixels", () => {
    const model = new TileModel(),
      buffer = new Uint8ClampedArray(3 * 2 * 4);
    const parsed = parseTileFrame(buildTileFrame(header, tiles));
    const rects = model.accept(parsed.header, parsed.tiles);
    expect(rects).toEqual([
      { x: 0, y: 0, width: 2, height: 2 },
      { x: 2, y: 0, width: 1, height: 2 },
    ]);
    for (const [i, rect] of (rects ?? []).entries())
      blitRgba(
        buffer,
        3,
        rect,
        new Uint8ClampedArray(rect.width * rect.height * 4).fill(i + 1),
      );
    expect([...buffer]).toEqual([
      1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2,
    ]);
    const changed = model.accept(
      { ...header, keyframe: false, seq: 2, count: 1 },
      [tiles[1]!],
    );
    const rect = changed?.[0];
    if (!rect) throw new Error("Delta was rejected");
    blitRgba(buffer, 3, rect, new Uint8ClampedArray(8).fill(9));
    expect(buffer[0]).toBe(1);
    expect(buffer[8]).toBe(9);
    expect(buffer[20]).toBe(9);
  });
  it("requires a keyframe after a gap, geometry change, reset or incomplete base", () => {
    const model = new TileModel();
    expect(model.accept({ ...header, keyframe: false }, tiles)).toBeNull();
    model.accept(header, tiles);
    expect(
      model.accept({ ...header, keyframe: false, seq: 3 }, tiles),
    ).toBeNull();
    expect(model.ready).toBe(false);
    model.accept({ ...header, seq: 4 }, tiles);
    expect(
      model.accept(
        { ...header, keyframe: false, seq: 5, geometryRevision: 4 },
        tiles,
      ),
    ).toBeNull();
    model.reset();
    expect(model.ready).toBe(false);
    expect(() => model.accept({ ...header, count: 1 }, [tiles[0]!])).toThrow(
      "Incomplete picture",
    );
  });
  it("requires a keyframe when the delta shows another part of the source", () => {
    const model = new TileModel();
    const region = { x: 2, y: 0, width: 3, height: 2 };
    expect(model.accept({ ...header, region }, tiles)).not.toBeNull();
    expect(
      model.accept({ ...header, keyframe: false, seq: 2, region }, tiles),
    ).not.toBeNull();
    expect(
      model.accept(
        { ...header, keyframe: false, seq: 3, region: { ...region, x: 1 } },
        tiles,
      ),
    ).toBeNull();
    expect(model.ready).toBe(false);
    model.accept({ ...header, seq: 4, region }, tiles);
    expect(
      model.accept({ ...header, keyframe: false, seq: 5 }, tiles),
    ).toBeNull();
  });
  it("rejects oversized, duplicate and out-of-bounds tiles and stale sequences", () => {
    const model = new TileModel();
    model.accept(header, tiles);
    expect(model.accept(header, tiles)).toBeNull();
    expect(() =>
      model.accept({ ...header, seq: 2, encWidth: 9000 }, tiles),
    ).toThrow();
    expect(() =>
      model.accept({ ...header, seq: 2 }, [tiles[0]!, tiles[0]!]),
    ).toThrow();
    expect(() =>
      model.accept({ ...header, seq: 2 }, [
        tiles[0]!,
        { ...tiles[1]!, col: 5 },
      ]),
    ).toThrow();
  });
});

describe("the zoomed view's region", () => {
  const box = { width: 400, height: 200 };
  it("is the whole target at 1×", () => {
    expect(visibleRegion({ zoom: 1, pan: { x: 0, y: 0 } }, box)).toBeNull();
  });
  it("is what the view shows and an eighth more on each side", () => {
    // At 2× panned to the middle, the view shows a quarter to three quarters.
    expect(
      visibleRegion({ zoom: 2, pan: { x: -200, y: -100 } }, box),
    ).toEqual({ x: 0.1875, y: 0.1875, width: 0.625, height: 0.625 });
  });
  it("stays inside the target at its edges", () => {
    expect(visibleRegion({ zoom: 4, pan: { x: 0, y: 0 } }, box)).toEqual({
      x: 0,
      y: 0,
      width: 0.28125,
      height: 0.28125,
    });
    const far = visibleRegion({ zoom: 2, pan: { x: -400, y: -200 } }, box);
    expect(far && far.x + far.width).toBe(1);
    expect(far && far.y + far.height).toBe(1);
  });
  it("asks again only after a pan crosses a step", () => {
    const at = (x: number) =>
      visibleRegion({ zoom: 2, pan: { x, y: 0 } }, box);
    expect(at(-201)).toEqual(at(-201.2));
    expect(at(-201)).not.toEqual(at(-210));
  });
});

describe("pointer transforms and gestures", () => {
  it.each([1, 1.25, 2, 3])(
    "maps CSS coordinates at DPR %s to normalized target space",
    (dpr) => {
      const view = {
        left: 20,
        top: 30,
        width: 320,
        height: 180,
        zoom: 2,
        pan: { x: -80, y: -40 },
        dpr,
      };
      expect(targetPoint({ x: 260, y: 170 }, view)).toEqual({ x: 0.5, y: 0.5 });
      expect(targetPoint({ x: -61, y: 170 }, view)).toBeNull();
      expect(targetPoint({ x: 260, y: 170 }, { ...view, width: 0 })).toBeNull();
    },
  );
  it("distinguishes tap, drift, long press, pan and multi-touch", () => {
    const start = { x: 10, y: 20 };
    expect(recognizeGesture(start, start, 50, 1)).toBe("tap");
    expect(recognizeGesture(start, { x: 14, y: 24 }, 80, 1)).toBe("tap");
    expect(recognizeGesture(start, start, 600, 1)).toBe("long_press");
    expect(recognizeGesture(start, start, 80, 1, 30)).toBe("pan");
    expect(recognizeGesture(start, start, 50, 2)).toBe("pinch");
  });
  it("zooms around the local focal point and clamps magnification", () => {
    expect(
      pinchView({ zoom: 1, pan: { x: 0, y: 0 } }, { x: 100, y: 50 }, 2, {
        x: 0,
        y: 0,
      }),
    ).toEqual({ zoom: 2, pan: { x: -100, y: -50 } });
    expect(
      pinchView({ zoom: 3, pan: { x: 0, y: 0 } }, { x: 0, y: 0 }, 5, {
        x: 0,
        y: 0,
      }).zoom,
    ).toBe(4);
  });
  it("counts a second tap close in time and place as a double tap", () => {
    const first = { at: 1000, point: { x: 100, y: 100 } };
    expect(isDoubleTap(null, 1100, { x: 100, y: 100 })).toBe(false);
    expect(isDoubleTap(first, 1250, { x: 130, y: 120 })).toBe(true);
    expect(isDoubleTap(first, 1400, { x: 100, y: 100 })).toBe(false);
    expect(isDoubleTap(first, 1100, { x: 200, y: 100 })).toBe(false);
  });
  it("moves a zoomed picture to its edges and leaves the rest to scroll", () => {
    const box = { width: 400, height: 200 };
    // At 1× the picture cannot move: everything scrolls.
    expect(panWithin({ zoom: 1, pan: { x: 0, y: 0 } }, { x: 10, y: -20 }, box)).toEqual({
      pan: { x: 0, y: 0 },
      rest: { x: 10, y: -20 },
    });
    // At 2× it can move 400 by 200 before its edges show.
    expect(panWithin({ zoom: 2, pan: { x: -10, y: -190 } }, { x: 30, y: -30 }, box)).toEqual({
      pan: { x: 0, y: -200 },
      rest: { x: 20, y: -20 },
    });
    // A pan already past an edge stays where it is rather than jumping back.
    expect(panWithin({ zoom: 2, pan: { x: 64, y: 0 } }, { x: 5, y: 0 }, box).pan.x).toBe(64);
  });
});

describe("lease input and renewal", () => {
  it("rejects input immediately after Leave control, before the host receipt", () => {
    const transmit = vi.fn(() => true);
    const sender = new InputSender(transmit);
    sender.send(lease, 3, { action: "key_press", key: "ctrl", pressed: true });
    sender.retire(lease, 3);
    expect(sender.retired(lease)).toBe(true);
    expect(sender.send(lease, 3, { action: "text", text: "late input" })).toBe(
      false,
    );
    expect(transmit).toHaveBeenCalledTimes(3);
  });
  it("releases held keys and buttons before release_all, preserving sequence", () => {
    const actions: InputEnvelope[] = [];
    const sender = new InputSender((env) => {
      actions.push(env);
      return true;
    });
    sender.send(lease, 3, { action: "key_press", key: "ctrl", pressed: true });
    sender.send(lease, 3, {
      action: "pointer_button",
      button: "left",
      pressed: true,
      at: { x: 0.2, y: 0.3 },
    });
    sender.release(lease, 3);
    expect(actions.map((a) => a.seq)).toEqual([1, 2, 3, 4, 5]);
    expect(actions.slice(2).map((a) => a.action)).toEqual([
      { action: "key_press", key: "ctrl", pressed: false },
      {
        action: "pointer_button",
        button: "left",
        pressed: false,
        at: { x: 0.2, y: 0.3 },
      },
      { action: "release_all" },
    ]);
    expect(
      actions.every((a) => a.generation === 4 && a.geometry_revision === 3),
    ).toBe(true);
  });
  it("renews at one third of the window and stops on hidden/stale/disposal", () => {
    vi.useFakeTimers();
    let visible = true,
      fresh = true;
    const renew = vi.fn();
    const stop = scheduleRenewal(3000, () => visible && fresh, renew);
    vi.advanceTimersByTime(999);
    expect(renew).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(renew).toHaveBeenCalledTimes(1);
    visible = false;
    vi.advanceTimersByTime(4000);
    expect(renew).toHaveBeenCalledTimes(1);
    stop();
    visible = true;
    const again = scheduleRenewal(3000, () => visible && fresh, renew);
    fresh = false;
    vi.advanceTimersByTime(2000);
    expect(renew).toHaveBeenCalledTimes(1);
    again();
    fresh = true;
    const last = scheduleRenewal(3000, () => visible && fresh, renew);
    last();
    vi.advanceTimersByTime(3000);
    expect(renew).toHaveBeenCalledTimes(1);
  });
  it("never declares a stale frame fresh or invents a deadline for invalid windows", () => {
    expect(frameFresh(null, 0)).toBe(false);
    expect(frameFresh(0, FRAME_STALE_MS + 1)).toBe(false);
    expect(renewalDelay(3000, false, true)).toBeNull();
    expect(renewalDelay(3000, true, false)).toBeNull();
    expect(renewalDelay(0, true, true)).toBeNull();
  });
});

describe("key and terminal encoding", () => {
  it("uses the native input key vocabulary and distinct terminal bytes", () => {
    expect(keyChord("Ctrl+C")).toEqual({
      action: "key_chord",
      keys: ["ctrl", "c"],
    });
    expect(keyChord("Alt+Tab")).toEqual({
      action: "key_chord",
      keys: ["alt", "Tab"],
    });
    expect(keyChord("Esc")).toEqual({ action: "key_chord", keys: ["Escape"] });
    expect(keyChord("F12")).toEqual({ action: "key_chord", keys: ["F12"] });
    expect(terminalKey("Ctrl+C")).toBe("\x03");
    expect(terminalKey("↑")).toBe("\x1b[A");
  });
  it("resumes output without duplicate bytes, including overlapping replay", () => {
    const offset = new TerminalOffset();
    offset.attach(0, false);
    expect([
      ...offset.accept(terminalFrame(0, new Uint8Array([1, 2, 3]))),
    ]).toEqual([1, 2, 3]);
    offset.attach(1, false);
    expect([
      ...offset.accept(terminalFrame(1, new Uint8Array([2, 3, 4]))),
    ]).toEqual([4]);
    expect(offset.next).toBe(4);
    expect(offset.accept(terminalFrame(0, new Uint8Array([1, 2]))).length).toBe(
      0,
    );
    expect(() => offset.accept(terminalFrame(5, new Uint8Array([6])))).toThrow(
      "gap",
    );
  });
  it("discloses retention loss, refuses malformed offsets and cleans bracket terminators", () => {
    const offset = new TerminalOffset();
    offset.attach(80, true);
    expect(offset.next).toBe(80);
    expect(() => offset.accept(new Uint8Array(7))).toThrow();
    expect(() => offset.attach(NaN, false)).toThrow();
    expect(cleanPaste("one\r\ntwo\x1b[201~\x03")).toBe("one\ntwo[201~");
  });
  it("counts a pasted command and its Enter as one line", () => {
    expect(pasteLines("cargo test\n")).toBe("1 line");
    expect(pasteLines("cd client\r\nnpm test\r\n")).toBe("2 lines");
    expect(pasteLines("one\ntwo\nthree")).toBe("3 lines");
  });
});
