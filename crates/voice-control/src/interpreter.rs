// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Snapshot, thresholds::*};
use jev::questions::{VoiceAnswer, VoiceAssessor};
use std::sync::{Arc, Mutex};
use tokio::sync::{Semaphore, watch};
#[derive(Debug)]
pub struct Interpreter {
    assessor: Arc<dyn VoiceAssessor>,
    limit: Semaphore,
    current: Mutex<Option<watch::Sender<bool>>>,
}
impl Interpreter {
    pub fn new(assessor: Arc<dyn VoiceAssessor>) -> Self {
        Self {
            assessor,
            limit: Semaphore::new(MAX_IN_FLIGHT),
            current: Mutex::new(None),
        }
    }
    pub fn cancel(&self) {
        if let Ok(mut current) = self.current.lock()
            && let Some(old) = current.take()
        {
            let _ = old.send(true);
        }
    }
    pub async fn assess(&self, text: String, snapshot: &Snapshot) -> Result<VoiceAnswer, Error> {
        let (cancel, mut cancelled) = watch::channel(false);
        {
            let mut slot = self.current.lock().map_err(|_| Error::Stopped)?;
            if let Some(old) = slot.replace(cancel) {
                let _ = old.send(true);
            }
        }
        let request = snapshot.request(text);
        tokio::select! {
            biased;
            _ = cancelled.changed() => Err(Error::Superseded),
            result = tokio::time::timeout(ASSESS_TIMEOUT, async {
                let _permit = self.limit.acquire().await.map_err(|_| Error::Stopped)?;
                self.assessor.assess_voice(&request).await.map_err(|e| if e == jev::Error::Timeout { Error::Timeout } else { Error::Assessment })
            }) => result.map_err(|_| Error::Timeout)?,
        }
    }
}
