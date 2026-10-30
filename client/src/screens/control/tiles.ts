// SPDX-License-Identifier: Apache-2.0
import {
  FrameError,
  type Tile,
  type TileFrameHeader,
} from "../../protocol/binary";

export interface TileRect {
  x: number;
  y: number;
  width: number;
  height: number;
}
export function tileRects(header: TileFrameHeader, tiles: Tile[]): TileRect[] {
  const { encWidth: w, encHeight: h, tile } = header;
  if (
    ![
      w,
      h,
      tile,
      header.seq,
      header.geometryRevision,
      header.srcWidth,
      header.srcHeight,
    ].every(Number.isSafeInteger) ||
    w < 1 ||
    h < 1 ||
    w > 8192 ||
    h > 8192 ||
    w * h > 16_777_216 ||
    tile < 1 ||
    header.srcWidth < 1 ||
    header.srcHeight < 1 ||
    header.count !== tiles.length
  )
    throw new FrameError("Invalid picture dimensions.");
  const seen = new Set<string>();
  const rects = tiles.map((t) => {
    const x = t.col * tile,
      y = t.row * tile,
      key = `${t.col}:${t.row}`;
    if (x >= w || y >= h || x < 0 || y < 0 || seen.has(key))
      throw new FrameError("Invalid picture tile.");
    seen.add(key);
    return {
      x,
      y,
      width: Math.min(tile, w - x),
      height: Math.min(tile, h - y),
    };
  });
  if (
    header.keyframe &&
    rects.length !== Math.ceil(w / tile) * Math.ceil(h / tile)
  )
    throw new FrameError("Incomplete picture.");
  return rects;
}

/** Canvas-independent composition model; a delta requires a complete base. */
export class TileModel {
  revision = -1;
  seq = -1;
  width = 0;
  height = 0;
  /** The part of the source the base shows; a delta must show the same. */
  region = "";
  ready = false;
  reset(): void {
    this.ready = false;
    this.seq = -1;
  }
  accept(header: TileFrameHeader, tiles: Tile[]): TileRect[] | null {
    const rects = tileRects(header, tiles);
    if (header.seq <= this.seq) return null;
    const region = JSON.stringify(header.region);
    if (
      !header.keyframe &&
      (!this.ready ||
        header.geometryRevision !== this.revision ||
        header.encWidth !== this.width ||
        header.encHeight !== this.height ||
        region !== this.region ||
        header.seq !== this.seq + 1)
    ) {
      this.ready = false;
      return null;
    }
    this.revision = header.geometryRevision;
    this.seq = header.seq;
    this.width = header.encWidth;
    this.height = header.encHeight;
    this.region = region;
    this.ready = true;
    return rects;
  }
}

export function blitRgba(
  buffer: Uint8ClampedArray,
  width: number,
  rect: TileRect,
  pixels: Uint8ClampedArray,
): void {
  if (
    pixels.length !== rect.width * rect.height * 4 ||
    (rect.y + rect.height) * width * 4 > buffer.length ||
    rect.x + rect.width > width
  )
    throw new FrameError("Invalid decoded tile size.");
  for (let row = 0; row < rect.height; row++) {
    buffer.set(
      pixels.subarray(row * rect.width * 4, (row + 1) * rect.width * 4),
      ((rect.y + row) * width + rect.x) * 4,
    );
  }
}
