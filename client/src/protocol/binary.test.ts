// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import frames from "../../../crates/protocol/tests/fixtures/binary-frames.json";
import { FrameError, buildTileFrame, parseTileFrame, splitTerminalFrame, terminalFrame } from "./binary";

describe("TDV1", () => {
  it("round-trips a frame with cursor and tiles", () => {
    const frame = buildTileFrame(
      {
        keyframe: true,
        cursor: { x: 10, y: 20 },
        tile: 64,
        seq: 7,
        geometryRevision: 3,
        encWidth: 1280,
        encHeight: 720,
        srcWidth: 2560,
        srcHeight: 1440,
      },
      [
        { col: 0, row: 0, jpeg: new Uint8Array([1, 2, 3]) },
        { col: 5, row: 2, jpeg: new Uint8Array([9]) },
      ],
    );
    const { header, tiles } = parseTileFrame(frame);
    expect(header).toMatchObject({ keyframe: true, cursor: { x: 10, y: 20 }, seq: 7, count: 2, srcWidth: 2560 });
    expect(tiles.map((t) => [t.col, t.row, [...t.jpeg]])).toEqual([
      [0, 0, [1, 2, 3]],
      [5, 2, [9]],
    ]);
  });

  it("rejects truncation and trailing bytes", () => {
    const f = buildTileFrame(
      { keyframe: false, cursor: null, tile: 64, seq: 1, geometryRevision: 1, encWidth: 1, encHeight: 1, srcWidth: 1, srcHeight: 1 },
      [{ col: 0, row: 0, jpeg: new Uint8Array([1, 2]) }],
    );
    expect(() => parseTileFrame(f.subarray(0, f.length - 1))).toThrow(/truncated/);
    const extra = new Uint8Array(f.length + 1);
    extra.set(f);
    expect(() => parseTileFrame(extra)).toThrow(/trailing/);
  });

  // Byte layout pinned against crates/protocol/src/media.rs.
  it("matches the Rust header layout", () => {
    const f = buildTileFrame(
      { keyframe: true, cursor: null, tile: 0x40, seq: 0x0102, geometryRevision: 5, encWidth: 2, encHeight: 3, srcWidth: 4, srcHeight: 6 },
      [],
    );
    expect([...f.subarray(0, 8)]).toEqual([0x54, 0x44, 0x56, 0x31, 1, 0, 0, 0x40]);
    expect(f.length).toBe(44);
  });

  it("round-trips a region", () => {
    const region = { x: 640, y: 360, width: 1280, height: 720 };
    const f = buildTileFrame(
      { keyframe: true, cursor: null, tile: 64, seq: 1, geometryRevision: 1, encWidth: 1280, encHeight: 720, srcWidth: 2560, srcHeight: 1440, region },
      [],
    );
    expect(f[4]).toBe(0b101);
    expect(f.length).toBe(60);
    expect(parseTileFrame(f).header.region).toEqual(region);
  });
});

describe("terminal frames", () => {
  it("round-trips offset and bytes", () => {
    const f = terminalFrame(42, new TextEncoder().encode("hello"));
    const s = splitTerminalFrame(f);
    expect(s?.offset).toBe(42);
    expect(new TextDecoder().decode(s!.bytes)).toBe("hello");
    expect(splitTerminalFrame(f.subarray(0, 3))).toBeNull();
  });
});

// The companion reads the same frames in Rust (crates/protocol/tests/binary_frames.rs).
// Both readers are held to these cases, well-formed and not.
describe("frames shared with the Rust reader", () => {
  const bytes = (hex: string) => Uint8Array.from(hex.match(/../g) ?? [], (b) => parseInt(b, 16));
  const hex = (data: Uint8Array) => [...data].map((b) => b.toString(16).padStart(2, "0")).join("");

  it.each(frames.tile_frames.map((c) => [c.name, c] as const))("reads a tile frame: %s", (_name, c) => {
    const data = bytes(c.hex);
    if (c.error !== undefined) {
      expect(() => parseTileFrame(data)).toThrow(FrameError);
      expect(() => parseTileFrame(data)).toThrow(new RegExp(c.error));
      return;
    }
    const { header, tiles } = parseTileFrame(data);
    expect({
      keyframe: header.keyframe,
      cursor: header.cursor ? [header.cursor.x, header.cursor.y] : null,
      tile: header.tile,
      seq: header.seq,
      geometry_revision: header.geometryRevision,
      enc_width: header.encWidth,
      enc_height: header.encHeight,
      src_width: header.srcWidth,
      src_height: header.srcHeight,
      count: header.count,
      tiles: tiles.map((t) => [t.col, t.row, hex(t.jpeg)]),
      ...(header.region && {
        region: [header.region.x, header.region.y, header.region.width, header.region.height],
      }),
    }).toEqual(c.frame);
  });

  it.each(frames.terminal_frames.map((c) => [c.name, c] as const))("reads a terminal frame: %s", (_name, c) => {
    const part = splitTerminalFrame(bytes(c.hex));
    expect(part && { offset: part.offset, bytes: hex(part.bytes) }).toEqual(
      c.offset === null ? null : { offset: c.offset, bytes: c.bytes },
    );
  });
});
