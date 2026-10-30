// SPDX-License-Identifier: Apache-2.0
//! Host-local speech recognition.

use async_trait::async_trait;

use protocol::annotation::SpeechSegment;
use protocol::capability::Availability;

#[async_trait]
pub trait SpeechEngine: Send + Sync + std::fmt::Debug + 'static {
    fn name(&self) -> String;
    async fn availability(&self) -> Availability;
    async fn start(&self, language: Option<String>) -> Result<Box<dyn SpeechSession>, SpeechError>;
}

#[async_trait]
pub trait SpeechSession: Send {
    /// Feed 16 kHz mono PCM16 samples; returns new or revised segments.
    async fn push_pcm(&mut self, samples: &[i16]) -> Result<Vec<SpeechSegment>, SpeechError>;
    /// Finalize; returns final segments.
    async fn finish(&mut self) -> Result<Vec<SpeechSegment>, SpeechError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    #[error("speech unavailable: {0}")]
    Unavailable(String),
    #[error("recognition failed: {0}")]
    Failed(String),
}
