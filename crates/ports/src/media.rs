// SPDX-License-Identifier: Apache-2.0
//! Encoding captured frames for the phone (live `TDV1` tile stream) and for
//! model observation (still images).

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use protocol::control::TargetGeometry;
use protocol::media::StreamParams;

use crate::desktop::{CapturedFrame, FrameStream, PlatformError};

#[derive(Debug)]
pub enum EncodedOut {
    /// A complete `TDV1` tile frame.
    Frame(bytes::Bytes),
    GeometryChanged(TargetGeometry),
    /// Capture stalled or was suspended (screen locked, permission lost…).
    Paused {
        reason: String,
    },
    Resumed,
    /// Terminal; nothing follows.
    Ended {
        reason: String,
    },
}

#[async_trait]
pub trait EncodedStreamControl: Send + Sync {
    /// Client rendered frame `seq`; opens the send window.
    async fn ack(&self, seq: u64);
    async fn adjust(&self, params: StreamParams);
    async fn request_keyframe(&self);
    async fn close(&self);
}

pub struct EncodedStream {
    pub output: mpsc::Receiver<EncodedOut>,
    pub control: Arc<dyn EncodedStreamControl>,
}

impl std::fmt::Debug for EncodedStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncodedStream").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StillFormat {
    Png,
    Jpeg { quality: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedStill {
    pub bytes: bytes::Bytes,
    pub media_type: &'static str,
    pub width: u32,
    pub height: u32,
}

pub trait MediaEncoder: Send + Sync + std::fmt::Debug + 'static {
    /// Start encoding a capture stream. The encoder owns pacing: at most
    /// `params.window` unacknowledged frames in flight, latest frame wins.
    fn start(&self, stream: Box<dyn FrameStream>, params: StreamParams) -> EncodedStream;

    /// Encode one frame, downscaled to fit `max_dim` (longest side) if set.
    fn encode_still(
        &self,
        frame: &CapturedFrame,
        format: StillFormat,
        max_dim: Option<u32>,
    ) -> Result<EncodedStill, PlatformError>;
}
