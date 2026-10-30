// SPDX-License-Identifier: Apache-2.0
//
// Binary frame formats shared with crates/protocol (media.rs, terminal.rs).

export const TILE_MAGIC = "TDV1";
export const TILE_HEADER_LEN = 44;
const FLAG_KEYFRAME = 0b01;
const FLAG_CURSOR = 0b10;
const FLAG_REGION = 0b100;
const TILE_REGION_LEN = 16;

/** Part of the source, in source pixels. */
export interface SourceRegion {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface TileFrameHeader {
  keyframe: boolean;
  cursor: { x: number; y: number } | null;
  tile: number;
  seq: number;
  geometryRevision: number;
  encWidth: number;
  encHeight: number;
  srcWidth: number;
  srcHeight: number;
  /** The part of the source this frame shows; `null` is all of it. */
  region: SourceRegion | null;
  count: number;
}

export interface Tile {
  col: number;
  row: number;
  jpeg: Uint8Array;
}

export class FrameError extends Error {}

/** Parse one TDV1 tile-delta frame. Tile byte views alias `buf`. */
export function parseTileFrame(buf: Uint8Array): { header: TileFrameHeader; tiles: Tile[] } {
  if (buf.length < TILE_HEADER_LEN) throw new FrameError("short frame");
  if (buf[0] !== 0x54 || buf[1] !== 0x44 || buf[2] !== 0x56 || buf[3] !== 0x31) {
    throw new FrameError("bad magic");
  }
  const v = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const flags = v.getUint8(4);
  const header: TileFrameHeader = {
    keyframe: (flags & FLAG_KEYFRAME) !== 0,
    tile: v.getUint16(6),
    // Sequence and revision are u64 on the wire; they stay well inside 2^53.
    seq: Number(v.getBigUint64(8)),
    geometryRevision: Number(v.getBigUint64(16)),
    encWidth: v.getUint16(24),
    encHeight: v.getUint16(26),
    srcWidth: v.getUint32(28),
    srcHeight: v.getUint32(32),
    cursor: (flags & FLAG_CURSOR) !== 0 ? { x: v.getUint16(36), y: v.getUint16(38) } : null,
    region: null,
    count: v.getUint16(40),
  };
  const tiles: Tile[] = [];
  let pos = TILE_HEADER_LEN;
  if ((flags & FLAG_REGION) !== 0) {
    if (buf.length < pos + TILE_REGION_LEN) throw new FrameError("short frame");
    const region = {
      x: v.getUint32(pos),
      y: v.getUint32(pos + 4),
      width: v.getUint32(pos + 8),
      height: v.getUint32(pos + 12),
    };
    if (
      region.width === 0 ||
      region.height === 0 ||
      region.x + region.width > header.srcWidth ||
      region.y + region.height > header.srcHeight
    )
      throw new FrameError("region empty or outside the source");
    header.region = region;
    pos += TILE_REGION_LEN;
  }
  for (let i = 0; i < header.count; i++) {
    if (buf.length < pos + 8) throw new FrameError(`truncated at tile ${i}`);
    const col = v.getUint16(pos);
    const row = v.getUint16(pos + 2);
    const len = v.getUint32(pos + 4);
    pos += 8;
    if (buf.length < pos + len) throw new FrameError(`truncated at tile ${i}`);
    tiles.push({ col, row, jpeg: buf.subarray(pos, pos + len) });
    pos += len;
  }
  if (pos !== buf.length) throw new FrameError("trailing bytes");
  return { header, tiles };
}

/** Build a TDV1 frame (used by tests and the development simulator). */
export function buildTileFrame(
  header: Omit<TileFrameHeader, "count" | "region"> & { region?: SourceRegion | null },
  tiles: { col: number; row: number; jpeg: Uint8Array }[],
): Uint8Array {
  const region = header.region ?? null;
  const size =
    TILE_HEADER_LEN + (region ? TILE_REGION_LEN : 0) + tiles.reduce((n, t) => n + 8 + t.jpeg.length, 0);
  const out = new Uint8Array(size);
  const v = new DataView(out.buffer);
  out.set([0x54, 0x44, 0x56, 0x31], 0);
  v.setUint8(
    4,
    (header.keyframe ? FLAG_KEYFRAME : 0) | (header.cursor ? FLAG_CURSOR : 0) | (region ? FLAG_REGION : 0),
  );
  v.setUint16(6, header.tile);
  v.setBigUint64(8, BigInt(header.seq));
  v.setBigUint64(16, BigInt(header.geometryRevision));
  v.setUint16(24, header.encWidth);
  v.setUint16(26, header.encHeight);
  v.setUint32(28, header.srcWidth);
  v.setUint32(32, header.srcHeight);
  v.setUint16(36, header.cursor?.x ?? 0);
  v.setUint16(38, header.cursor?.y ?? 0);
  v.setUint16(40, tiles.length);
  let pos = TILE_HEADER_LEN;
  if (region) {
    v.setUint32(pos, region.x);
    v.setUint32(pos + 4, region.y);
    v.setUint32(pos + 8, region.width);
    v.setUint32(pos + 12, region.height);
    pos += TILE_REGION_LEN;
  }
  for (const t of tiles) {
    v.setUint16(pos, t.col);
    v.setUint16(pos + 2, t.row);
    v.setUint32(pos + 4, t.jpeg.length);
    out.set(t.jpeg, pos + 8);
    pos += 8 + t.jpeg.length;
  }
  return out;
}

export const OUTPUT_OFFSET_PREFIX = 8;

/** Split a server→client binary terminal frame into offset and bytes. */
export function splitTerminalFrame(frame: Uint8Array): { offset: number; bytes: Uint8Array } | null {
  if (frame.length < OUTPUT_OFFSET_PREFIX) return null;
  const v = new DataView(frame.buffer, frame.byteOffset, OUTPUT_OFFSET_PREFIX);
  return { offset: Number(v.getBigUint64(0)), bytes: frame.subarray(OUTPUT_OFFSET_PREFIX) };
}

export function terminalFrame(offset: number, bytes: Uint8Array): Uint8Array {
  const out = new Uint8Array(OUTPUT_OFFSET_PREFIX + bytes.length);
  new DataView(out.buffer).setBigUint64(0, BigInt(offset));
  out.set(bytes, OUTPUT_OFFSET_PREFIX);
  return out;
}
