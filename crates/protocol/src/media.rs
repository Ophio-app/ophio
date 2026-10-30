// SPDX-License-Identifier: Apache-2.0
//! Live view media and speech connections.
//!
//! ## Tile-delta video
//!
//! The host captures a target, divides it into square tiles, and sends only
//! tiles that changed, JPEG-encoded, over an authenticated media connection.
//! Delivery is latest-frame-wins: the host keeps at most `window` unacked
//! frames in flight and coalesces changes while waiting, so a slow network
//! lowers the frame rate instead of building latency. A keyframe carries every
//! tile and is sent on open, on geometry change and on request.
//!
//! Binary frame layout (all integers big-endian):
//!
//! ```text
//! magic     4  b"TDV1"
//! flags     1  bit0 = keyframe, bit1 = cursor present
//! reserved  1
//! tile      2  tile edge in encoded pixels
//! seq       8  frame sequence
//! georev    8  target geometry revision
//! enc_w     2  encoded frame width  (after scaling)
//! enc_h     2  encoded frame height
//! src_w     4  source width in target pixels
//! src_h     4  source height in target pixels
//! cursor_x  2  encoded-space cursor x (valid if bit1)
//! cursor_y  2
//! count     2  number of tiles that follow
//! pad       2  zero
//! then, if bit2 (region): x u32, y u32, w u32, h u32
//! then `count` × { col u16, row u16, len u32, jpeg[len] }
//! ```
//!
//! ## Regions
//!
//! A client that zooms in may ask for only the part of the target it shows
//! (`StreamParams::region`). The host crops to that part before scaling, so a
//! zoomed view gets the target's own pixels within the same size limit. Each
//! frame of a region carries the region it shows, in source pixels, and a
//! change of region is a keyframe. The host sets bit2 only for a client that
//! asked for a region, so readers older than regions never see it.
//!
//! ## Speech
//!
//! The speech connection streams 16 kHz mono PCM16LE audio in binary frames
//! after a `start` text frame; the host runs local recognition and returns
//! revisable segments.

use serde::{Deserialize, Serialize};

use crate::annotation::SpeechSegment;
use crate::control::TargetGeometry;
use crate::ids::{ArtifactId, MediaSessionId, SpeechSessionId, TargetId};

pub const TILE_MAGIC: [u8; 4] = *b"TDV1";
pub const TILE_HEADER_LEN: usize = 44;
pub const FLAG_KEYFRAME: u8 = 0b01;
pub const FLAG_CURSOR: u8 = 0b10;
pub const FLAG_REGION: u8 = 0b100;
pub const TILE_REGION_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum MediaClientFrame {
    Open {
        target_id: TargetId,
        params: StreamParams,
    },
    /// Change quality/size without reopening; triggers a keyframe.
    Adjust {
        params: StreamParams,
    },
    /// Frame displayed; opens the flow-control window.
    Ack {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        seq: u64,
    },
    RequestKeyframe,
    Close,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct StreamParams {
    /// Upper bound on encoded size; the host keeps aspect ratio.
    pub max_width: u16,
    pub max_height: u16,
    /// JPEG quality 1..=100.
    pub quality: u8,
    pub max_fps: u8,
    /// Frames in flight before the host waits for an ack.
    pub window: u8,
    /// A person explicitly chose to reopen a lost desktop sharing session.
    /// Only opening a stream consumes this flag; quality adjustments do not.
    #[serde(default)]
    #[ts(optional)]
    pub reopen: Option<bool>,
    /// Send only this part of the target. Omitted: all of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub region: Option<StreamRegion>,
}

/// Part of a target, in fractions of its width and height.
#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct StreamRegion {
    #[schemars(range(min = 0.0, max = 1.0))]
    pub x: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub y: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub width: f64,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub height: f64,
}

impl StreamRegion {
    /// Inside the target and not empty. Rounding may overshoot the far edge
    /// slightly; `pixels` clamps it.
    pub fn valid(&self) -> bool {
        const SLACK: f64 = 1e-6;
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.x >= 0.0
            && self.y >= 0.0
            && self.width > 0.0
            && self.height > 0.0
            && self.x + self.width <= 1.0 + SLACK
            && self.y + self.height <= 1.0 + SLACK
    }

    /// The source pixels this region covers: outward to whole pixels, at
    /// least one pixel, never past the source.
    pub fn pixels(&self, width: u32, height: u32) -> SourceRegion {
        let span = |start: f64, size: f64, total: u32| {
            let last = total.saturating_sub(1);
            let first = ((start * f64::from(total)).floor().max(0.0) as u32).min(last);
            let end =
                (((start + size) * f64::from(total)).ceil() as u32).clamp(first + 1, total.max(1));
            (first, end - first)
        };
        let (x, w) = span(self.x, self.width, width);
        let (y, h) = span(self.y, self.height, height);
        SourceRegion {
            x,
            y,
            width: w,
            height: h,
        }
    }
}

/// Part of a source frame, in source pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Default for StreamParams {
    fn default() -> Self {
        Self {
            max_width: 1600,
            max_height: 1600,
            quality: 70,
            max_fps: 20,
            window: 2,
            reopen: None,
            region: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum MediaServerFrame {
    Opened {
        session_id: MediaSessionId,
        target_id: TargetId,
        geometry: TargetGeometry,
        tile: u16,
    },
    GeometryChanged {
        geometry: TargetGeometry,
    },
    /// Capture is paused (e.g. session locked, permission revoked).
    Paused {
        reason: String,
    },
    Resumed,
    Ended {
        reason: String,
    },
}

/// Parsed tile-delta frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileFrameHeader {
    pub keyframe: bool,
    pub cursor: Option<(u16, u16)>,
    pub tile: u16,
    pub seq: u64,
    pub geometry_revision: u64,
    pub enc_width: u16,
    pub enc_height: u16,
    pub src_width: u32,
    pub src_height: u32,
    /// The part of the source this frame shows; `None` is all of it.
    pub region: Option<SourceRegion>,
    pub count: u16,
}

/// One encoded tile inside a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile<'a> {
    pub col: u16,
    pub row: u16,
    pub jpeg: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TileFrameError {
    #[error("frame shorter than header")]
    Short,
    #[error("bad magic")]
    Magic,
    #[error("tile {0} truncated")]
    Truncated(u16),
    #[error("trailing bytes after last tile")]
    Trailing,
    #[error("region empty or outside the source")]
    Region,
}

impl TileFrameHeader {
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&TILE_MAGIC);
        let mut flags = 0u8;
        if self.keyframe {
            flags |= FLAG_KEYFRAME;
        }
        if self.cursor.is_some() {
            flags |= FLAG_CURSOR;
        }
        if self.region.is_some() {
            flags |= FLAG_REGION;
        }
        out.push(flags);
        out.push(0);
        out.extend_from_slice(&self.tile.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.geometry_revision.to_be_bytes());
        out.extend_from_slice(&self.enc_width.to_be_bytes());
        out.extend_from_slice(&self.enc_height.to_be_bytes());
        out.extend_from_slice(&self.src_width.to_be_bytes());
        out.extend_from_slice(&self.src_height.to_be_bytes());
        let (cx, cy) = self.cursor.unwrap_or((0, 0));
        out.extend_from_slice(&cx.to_be_bytes());
        out.extend_from_slice(&cy.to_be_bytes());
        out.extend_from_slice(&self.count.to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        if let Some(r) = self.region {
            for v in [r.x, r.y, r.width, r.height] {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
    }
}

/// Append one tile record to a frame being built.
pub fn push_tile(out: &mut Vec<u8>, col: u16, row: u16, jpeg: &[u8]) {
    out.extend_from_slice(&col.to_be_bytes());
    out.extend_from_slice(&row.to_be_bytes());
    out.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
    out.extend_from_slice(jpeg);
}

/// Parse a complete tile-delta frame.
pub fn parse_tile_frame(buf: &[u8]) -> Result<(TileFrameHeader, Vec<Tile<'_>>), TileFrameError> {
    if buf.len() < TILE_HEADER_LEN {
        return Err(TileFrameError::Short);
    }
    if buf[0..4] != TILE_MAGIC {
        return Err(TileFrameError::Magic);
    }
    let be16 = |i: usize| u16::from_be_bytes([buf[i], buf[i + 1]]);
    let be32 = |i: usize| u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
    let be64 = |i: usize| {
        let mut b = [0u8; 8];
        b.copy_from_slice(&buf[i..i + 8]);
        u64::from_be_bytes(b)
    };
    let flags = buf[4];
    let mut header = TileFrameHeader {
        keyframe: flags & FLAG_KEYFRAME != 0,
        tile: be16(6),
        seq: be64(8),
        geometry_revision: be64(16),
        enc_width: be16(24),
        enc_height: be16(26),
        src_width: be32(28),
        src_height: be32(32),
        cursor: (flags & FLAG_CURSOR != 0).then(|| (be16(36), be16(38))),
        region: None,
        count: be16(40),
    };
    let mut pos = TILE_HEADER_LEN;
    if flags & FLAG_REGION != 0 {
        if buf.len() < pos + TILE_REGION_LEN {
            return Err(TileFrameError::Short);
        }
        let region = SourceRegion {
            x: be32(pos),
            y: be32(pos + 4),
            width: be32(pos + 8),
            height: be32(pos + 12),
        };
        if region.width == 0
            || region.height == 0
            || u64::from(region.x) + u64::from(region.width) > u64::from(header.src_width)
            || u64::from(region.y) + u64::from(region.height) > u64::from(header.src_height)
        {
            return Err(TileFrameError::Region);
        }
        header.region = Some(region);
        pos += TILE_REGION_LEN;
    }
    let mut tiles = Vec::with_capacity(header.count as usize);
    for i in 0..header.count {
        if buf.len() < pos + 8 {
            return Err(TileFrameError::Truncated(i));
        }
        let col = be16(pos);
        let row = be16(pos + 2);
        let len = be32(pos + 4) as usize;
        pos += 8;
        if buf.len() < pos + len {
            return Err(TileFrameError::Truncated(i));
        }
        tiles.push(Tile {
            col,
            row,
            jpeg: &buf[pos..pos + len],
        });
        pos += len;
    }
    if pos != buf.len() {
        return Err(TileFrameError::Trailing);
    }
    Ok((header, tiles))
}

/// Build a full frame from a header and tiles. `header.count` is overwritten.
pub fn build_tile_frame(mut header: TileFrameHeader, tiles: &[(u16, u16, &[u8])]) -> Vec<u8> {
    header.count = tiles.len() as u16;
    let body: usize = tiles.iter().map(|t| 8 + t.2.len()).sum();
    let mut out = Vec::with_capacity(TILE_HEADER_LEN + TILE_REGION_LEN + body);
    header.encode_into(&mut out);
    for (col, row, jpeg) in tiles {
        push_tile(&mut out, *col, *row, jpeg);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum SpeechClientFrame {
    Start {
        session_id: SpeechSessionId,
        language: Option<String>,
        keep_audio: bool,
    },
    /// No more audio; finalize remaining segments.
    Finish,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum SpeechServerFrame {
    Ready {
        session_id: SpeechSessionId,
        engine: String,
    },
    /// New or revised segment (same `segment_id`, higher `revision`).
    Segment {
        segment: SpeechSegment,
    },
    Finished {
        session_id: SpeechSessionId,
        audio_artifact: Option<ArtifactId>,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> TileFrameHeader {
        TileFrameHeader {
            keyframe: true,
            cursor: Some((10, 20)),
            tile: 128,
            seq: 7,
            geometry_revision: 3,
            enc_width: 1280,
            enc_height: 720,
            src_width: 2560,
            src_height: 1440,
            region: None,
            count: 0,
        }
    }

    #[test]
    fn tile_frame_round_trip() {
        let a = [1u8, 2, 3];
        let b = [9u8; 10];
        let frame = build_tile_frame(header(), &[(0, 0, &a), (3, 2, &b)]);
        let (h, tiles) = parse_tile_frame(&frame).unwrap();
        assert_eq!(
            h,
            TileFrameHeader {
                count: 2,
                ..header()
            }
        );
        assert_eq!(tiles.len(), 2);
        assert_eq!(
            tiles[1],
            Tile {
                col: 3,
                row: 2,
                jpeg: &b
            }
        );
    }

    #[test]
    fn rejects_truncation_and_garbage() {
        let frame = build_tile_frame(header(), &[(0, 0, &[1, 2, 3])]);
        assert_eq!(
            parse_tile_frame(&frame[..frame.len() - 1]),
            Err(TileFrameError::Truncated(0))
        );
        assert_eq!(parse_tile_frame(&frame[..10]), Err(TileFrameError::Short));
        let mut bad = frame.clone();
        bad[0] = b'X';
        assert_eq!(parse_tile_frame(&bad), Err(TileFrameError::Magic));
        let mut extra = frame;
        extra.push(0);
        assert_eq!(parse_tile_frame(&extra), Err(TileFrameError::Trailing));
    }
}
