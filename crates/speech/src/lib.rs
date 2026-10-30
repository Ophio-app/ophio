// SPDX-License-Identifier: Apache-2.0
//! Optional host-local speech recognition. No microphone capture, automatic
//! downloads, audio persistence or network access occurs during recognition.
mod catalog;
mod decoder;
mod install;
mod stream;

pub use catalog::{CatalogEntry, DEFAULT_MODEL, ModelFile, catalog};
pub use install::{InstallError, InstallProgress, install, remove};
pub use stream::{MAX_PUSH_SAMPLES, MAX_SESSION_SAMPLES, SAMPLE_RATE};

use async_trait::async_trait;
use decoder::{Decoder, LocalDecoder};
use ports::{PROGRAM_NAME, SpeechEngine, SpeechError, SpeechSession};
use protocol::{annotation::SpeechSegment, capability::Availability};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use stream::StreamState;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const INFERENCE_THREADS: usize = 4;
pub const MAX_SESSIONS: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("could not create the speech CPU pool")]
    ThreadPool,
    #[error("unknown speech model")]
    UnknownModel,
}

pub struct Installed {
    model_name: &'static str,
    availability: Availability,
    shared: Option<Arc<Shared>>,
}
impl std::fmt::Debug for Installed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Installed")
            .field("model", &self.model_name)
            .field("availability", &self.availability)
            .finish_non_exhaustive()
    }
}
struct Shared {
    decoder: Mutex<Box<dyn Decoder>>,
    pool: rayon::ThreadPool,
    sessions: Arc<Semaphore>,
    inference: Arc<Semaphore>,
}

impl Installed {
    /// Verify and load the default installed model. This performs local disk I/O
    /// and CPU work: call at startup or from `spawn_blocking`, never per session.
    /// Missing, corrupt or unreadable weights return an explanatory engine.
    pub fn open(model_dir: &Path) -> Result<Arc<dyn SpeechEngine>, OpenError> {
        Self::open_model(model_dir, DEFAULT_MODEL)
    }

    /// Select an explicitly installed catalog model; never searches remote hubs.
    pub fn open_model(
        model_dir: &Path,
        model_name: &str,
    ) -> Result<Arc<dyn SpeechEngine>, OpenError> {
        let model = catalog()
            .iter()
            .find(|m| m.name == model_name)
            .ok_or(OpenError::UnknownModel)?;
        let remedy = format!(
            "On the computer, run `{PROGRAM_NAME} voice install {}`. If present but damaged, run `{PROGRAM_NAME} voice remove {}` first, then install again.",
            model.name, model.name
        );
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(INFERENCE_THREADS)
            .thread_name(|i| format!("speech-cpu-{i}"))
            .build()
            .map_err(|_| OpenError::ThreadPool)?;
        let dir = model_dir.join(model.name);
        let loaded = (|| {
            let weights = install::verified_read(&dir, &model.files[0])?;
            let config = install::verified_read(&dir, &model.files[1])?;
            let tokenizer = install::verified_read(&dir, &model.files[2])?;
            Ok::<_, InstallError>((weights, config, tokenizer))
        })();
        let (availability, shared) = match loaded {
            Ok((weights, config, tokenizer)) => {
                match pool.install(|| LocalDecoder::load(weights, &config, &tokenizer)) {
                    Ok(decoder) => (
                        Availability::Available,
                        Some(Arc::new(Shared {
                            decoder: Mutex::new(Box::new(decoder)),
                            pool,
                            sessions: Arc::new(Semaphore::new(MAX_SESSIONS)),
                            inference: Arc::new(Semaphore::new(1)),
                        })),
                    ),
                    Err(_) => (
                        Availability::Unsupported {
                            reason: format!("Local speech model could not load. {remedy}"),
                        },
                        None,
                    ),
                }
            }
            Err(InstallError::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied => (
                Availability::PermissionMissing {
                    remedy: format!(
                        "Allow the user Øphio runs as to read the model directory. {remedy}"
                    ),
                },
                None,
            ),
            Err(_) => (
                Availability::Unsupported {
                    reason: format!("No verified local speech model is installed. {remedy}"),
                },
                None,
            ),
        };
        Ok(Arc::new(Self {
            model_name: model.name,
            availability,
            shared,
        }))
    }
}

#[async_trait]
impl SpeechEngine for Installed {
    fn name(&self) -> String {
        format!("Local Whisper {} (CPU)", self.model_name)
    }
    async fn availability(&self) -> Availability {
        self.availability.clone()
    }
    async fn start(&self, language: Option<String>) -> Result<Box<dyn SpeechSession>, SpeechError> {
        let Some(shared) = &self.shared else {
            let message = match &self.availability {
                Availability::PermissionMissing { remedy } => remedy.clone(),
                Availability::Unsupported { reason } => reason.clone(),
                _ => "local speech is unavailable".to_owned(),
            };
            return Err(SpeechError::Unavailable(message));
        };
        if language.as_ref().is_some_and(|s| {
            !s.eq_ignore_ascii_case("en")
                && !s.eq_ignore_ascii_case("en-us")
                && !s.eq_ignore_ascii_case("en-gb")
                && !s.eq_ignore_ascii_case("english")
        }) {
            return Err(SpeechError::Unavailable(
                "installed model supports English only".into(),
            ));
        }
        let permit = shared.sessions.clone().try_acquire_owned().map_err(|_| {
            SpeechError::Unavailable("busy: two speech sessions are already active".into())
        })?;
        Ok(Box::new(Session {
            shared: shared.clone(),
            permit: Arc::new(permit),
            state: Some(StreamState::new(
                protocol::ids::SpeechSessionId::new().to_string(),
            )),
            cancel: Arc::new(AtomicBool::new(false)),
            finished: false,
        }))
    }
}

struct Session {
    shared: Arc<Shared>,
    permit: Arc<OwnedSemaphorePermit>,
    state: Option<StreamState>,
    cancel: Arc<AtomicBool>,
    finished: bool,
}

/// Also cancels a dropped *operation future*. Its state cannot be resumed after
/// an unobserved blocking result, so retrying cannot silently skip input/output.
struct CancelGuard {
    token: Arc<AtomicBool>,
    armed: bool,
}
impl Drop for CancelGuard {
    fn drop(&mut self) {
        if self.armed {
            self.token.store(true, Ordering::Release);
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Session {
    async fn run(
        &mut self,
        samples: Vec<i16>,
        finish: bool,
    ) -> Result<Vec<SpeechSegment>, SpeechError> {
        let mut state = self.state.take().ok_or_else(|| {
            SpeechError::Failed("speech session ended or an operation was cancelled".into())
        })?;
        let mut cancelled = CancelGuard {
            token: self.cancel.clone(),
            armed: true,
        };
        let gate = self
            .shared
            .inference
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| SpeechError::Unavailable("speech worker closed".into()))?;
        let shared = self.shared.clone();
        let cancel = self.cancel.clone();
        let session_permit = self.permit.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _gate = gate;
            let _session_permit = session_permit;
            shared.pool.install(|| {
                decoder::check_cancel(&cancel)?;
                let mut decoder = shared
                    .decoder
                    .lock()
                    .map_err(|_| decoder::DecodeError::Configuration)?;
                let segments = if finish {
                    state.finish(decoder.as_mut(), &cancel)?
                } else {
                    state.push(&samples, decoder.as_mut(), &cancel)?
                };
                Ok::<_, decoder::DecodeError>((state, segments))
            })
        })
        .await
        .map_err(|_| SpeechError::Failed("speech worker stopped unexpectedly".into()))?
        .map_err(|e| SpeechError::Failed(e.to_string()))?;
        self.state = (!finish).then_some(result.0);
        // Normal completion disarms the cancellation guard without changing the
        // token that a blocking decoder might still be observing on error.
        cancelled.armed = false;
        Ok(result.1)
    }
}

#[async_trait]
impl SpeechSession for Session {
    async fn push_pcm(&mut self, samples: &[i16]) -> Result<Vec<SpeechSegment>, SpeechError> {
        if self.finished {
            return Err(SpeechError::Failed("speech session finished".into()));
        }
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| SpeechError::Failed("speech session cancelled".into()))?;
        if samples.len() > MAX_PUSH_SAMPLES {
            return Err(SpeechError::Failed(
                "audio chunk exceeds one second; split it before pushing".into(),
            ));
        }
        if state.accepted.saturating_add(samples.len()) > MAX_SESSION_SAMPLES {
            return Err(SpeechError::Failed(
                "speech session exceeds ten minutes; finish and start a new session".into(),
            ));
        }
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        self.run(samples.to_vec(), false).await
    }
    async fn finish(&mut self) -> Result<Vec<SpeechSegment>, SpeechError> {
        if self.finished {
            return Ok(Vec::new());
        }
        let result = self.run(Vec::new(), true).await;
        self.finished = true;
        result
    }
}

#[cfg(test)]
mod tests;
