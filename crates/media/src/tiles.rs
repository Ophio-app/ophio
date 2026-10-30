// SPDX-License-Identifier: Apache-2.0
use crate::image::{dimensions, invalid, jpeg, rgb_part};
use bytes::Bytes;
use ports::desktop::{CapturedFrame, DamageRect, PlatformError};
use protocol::media::{SourceRegion, StreamParams, TileFrameHeader, build_tile_frame};

/// Tile size is fixed for a stream. See BENCHMARKS.md for the measured tradeoff.
pub const TILE_EDGE: u16 = 128;

#[derive(Debug, Default)]
pub struct TileState {
    size: (u32, u32),
    hashes: Vec<u64>,
    previous: Vec<u8>,
    cursor: Option<(u16, u16)>,
    region: Option<SourceRegion>,
}

impl TileState {
    /// Produce a frame relative to the last successfully emitted image.
    /// Call only when output capacity and a send-window slot have been reserved.
    pub fn encode(
        &mut self,
        frame: &CapturedFrame,
        params: StreamParams,
        seq: u64,
        keyframe: bool,
    ) -> Result<Option<Bytes>, PlatformError> {
        validate(params)?;
        let region = params.region.map(|r| r.pixels(frame.width, frame.height));
        let area = region.unwrap_or(SourceRegion {
            x: 0,
            y: 0,
            width: frame.width,
            height: frame.height,
        });
        // A frame the source marks as wholly unchanged needs no pixel work.
        if !keyframe
            && !self.previous.is_empty()
            && self.region == region
            && frame.damage.as_ref().is_some_and(Vec::is_empty)
            && self.cursor == scale_cursor(frame, area, self.size)
            && dimensions(
                area.width,
                area.height,
                params.max_width.into(),
                params.max_height.into(),
            )? == self.size
        {
            return Ok(None);
        }
        let (w, h, image) = rgb_part(
            frame,
            area,
            params.max_width.into(),
            params.max_height.into(),
        )?;
        let keyframe =
            keyframe || self.size != (w, h) || self.region != region || self.previous.is_empty();
        let edge = TILE_EDGE as u32;
        let cols = w.div_ceil(edge);
        let rows = h.div_ceil(edge);
        if cols * rows > u16::MAX as u32 {
            return Err(invalid("too many tiles"));
        }
        let mut hashes = if keyframe {
            vec![0; (cols * rows) as usize]
        } else {
            self.hashes.clone()
        };
        let mut tiles = Vec::new();
        let cursor = scale_cursor(frame, area, (w, h));
        for row in 0..rows {
            for col in 0..cols {
                let x = col * edge;
                let y = row * edge;
                let tw = edge.min(w - x);
                let th = edge.min(h - y);
                if !keyframe
                    && frame.damage.as_ref().is_some_and(|rects| {
                        !rects
                            .iter()
                            .any(|rect| intersects(*rect, (x, y, tw, th), area, (w, h)))
                    })
                {
                    continue;
                }
                let mut tile = Vec::with_capacity((tw * th * 3) as usize);
                let mut equal = !keyframe;
                for ty in y..y + th {
                    let start = ((ty * w + x) * 3) as usize;
                    let end = start + tw as usize * 3;
                    tile.extend_from_slice(&image[start..end]);
                    equal &= !keyframe && self.previous[start..end] == image[start..end];
                }
                let index = (row * cols + col) as usize;
                let hash = xxhash_rust::xxh3::xxh3_64(&tile);
                if !keyframe && hashes[index] == hash && equal {
                    continue;
                }
                hashes[index] = hash;
                tiles.push((
                    col as u16,
                    row as u16,
                    jpeg(&tile, tw as u16, th as u16, params.quality)?,
                ));
            }
        }
        if tiles.is_empty() && cursor == self.cursor && !keyframe {
            return Ok(None);
        }
        let refs: Vec<_> = tiles
            .iter()
            .map(|(col, row, bytes)| (*col, *row, bytes.as_slice()))
            .collect();
        let output = build_tile_frame(
            TileFrameHeader {
                keyframe,
                cursor,
                tile: TILE_EDGE,
                seq,
                geometry_revision: frame.geometry.revision,
                enc_width: w as u16,
                enc_height: h as u16,
                src_width: frame.width,
                src_height: frame.height,
                region,
                count: 0,
            },
            &refs,
        );
        self.size = (w, h);
        self.hashes = hashes;
        self.previous = image;
        self.cursor = cursor;
        self.region = region;
        Ok(Some(output.into()))
    }

    /// A frame with no tiles, saying the last picture sent still stands.
    /// Nothing until a picture has been sent.
    pub fn unchanged(&self, frame: &CapturedFrame, seq: u64) -> Option<Bytes> {
        if self.previous.is_empty() {
            return None;
        }
        let output = build_tile_frame(
            TileFrameHeader {
                keyframe: false,
                cursor: self.cursor,
                tile: TILE_EDGE,
                seq,
                geometry_revision: frame.geometry.revision,
                enc_width: self.size.0 as u16,
                enc_height: self.size.1 as u16,
                src_width: frame.width,
                src_height: frame.height,
                region: self.region,
                count: 0,
            },
            &[],
        );
        Some(output.into())
    }
}

/// The cursor in an encoding of `area`; a cursor outside it is not shown.
fn scale_cursor(
    frame: &CapturedFrame,
    area: SourceRegion,
    (w, h): (u32, u32),
) -> Option<(u16, u16)> {
    frame.cursor.and_then(|(x, y)| {
        let x = x.checked_sub(area.x).filter(|x| *x < area.width)?;
        let y = y.checked_sub(area.y).filter(|y| *y < area.height)?;
        Some((
            ((u64::from(x) * u64::from(w) / u64::from(area.width)).min(u64::from(w - 1))) as u16,
            ((u64::from(y) * u64::from(h) / u64::from(area.height)).min(u64::from(h - 1))) as u16,
        ))
    })
}

/// Whether a damaged source rectangle touches an encoded tile of `area`.
fn intersects(
    rect: DamageRect,
    tile: (u32, u32, u32, u32),
    area: SourceRegion,
    encoded: (u32, u32),
) -> bool {
    // Clip the damage to the area, then measure it from the area's corner.
    let clip = |start: u32, size: u32, from: u32, span: u32| {
        let a = start.max(from);
        let b = start.saturating_add(size).min(from + span);
        (a < b).then(|| (u64::from(a - from), u64::from(b - from)))
    };
    let (Some((rx0, rx1)), Some((ry0, ry1))) = (
        clip(rect.x, rect.width, area.x, area.width),
        clip(rect.y, rect.height, area.y, area.height),
    ) else {
        return false;
    };
    let (x, y, w, h) = tile;
    let left = rx0 * u64::from(encoded.0) / u64::from(area.width);
    let top = ry0 * u64::from(encoded.1) / u64::from(area.height);
    let right = (rx1 * u64::from(encoded.0)).div_ceil(u64::from(area.width));
    let bottom = (ry1 * u64::from(encoded.1)).div_ceil(u64::from(area.height));
    u64::from(x) < right
        && u64::from(x + w) > left
        && u64::from(y) < bottom
        && u64::from(y + h) > top
}

pub(crate) fn validate(p: StreamParams) -> Result<(), PlatformError> {
    if p.max_width == 0
        || p.max_height == 0
        || !(1..=100).contains(&p.quality)
        || !(1..=120).contains(&p.max_fps)
        || !(1..=16).contains(&p.window)
        || p.region.is_some_and(|r| !r.valid())
    {
        Err(invalid(
            "invalid stream parameters (quality 1..100, fps 1..120, window 1..16, nonzero dimensions and a region inside the target required)",
        ))
    } else {
        Ok(())
    }
}
