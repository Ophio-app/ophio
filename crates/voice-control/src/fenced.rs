// SPDX-License-Identifier: Apache-2.0
//! Native effects remain owned when a request future is cancelled. Stop waits for
//! their completion; timing out a barrier never discards an unfinished effect.
use crate::{Driver, Error, Snapshot};
use async_trait::async_trait;
use protocol::voice::VoiceAction;
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
type Job = Arc<AsyncMutex<Option<tokio::task::JoinHandle<()>>>>;
#[derive(Default)]
struct Jobs {
    closed: bool,
    pending: Vec<Job>,
}
pub struct FencedDriver {
    inner: Arc<dyn Driver>,
    jobs: Mutex<Jobs>,
}
impl std::fmt::Debug for FencedDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FencedVoiceDriver")
    }
}
impl FencedDriver {
    pub fn new(inner: Arc<dyn Driver>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            jobs: Mutex::default(),
        })
    }
}
#[async_trait]
impl Driver for FencedDriver {
    async fn snapshot(&self) -> Result<Snapshot, Error> {
        self.inner.snapshot().await
    }
    async fn act(&self, action: &VoiceAction, snapshot: &Snapshot) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        {
            let mut jobs = self.jobs.lock().map_err(|_| Error::Stopped)?;
            if jobs.closed {
                return Err(Error::Stopped);
            }
            jobs.pending.retain(|job| {
                job.try_lock().map_or(true, |guard| {
                    guard.as_ref().is_some_and(|j| !j.is_finished())
                })
            });
            let inner = self.inner.clone();
            let action = action.clone();
            let snapshot = snapshot.clone();
            let job = tokio::spawn(async move {
                let result = inner.act(&action, &snapshot).await;
                let _ = tx.send(result);
            });
            jobs.pending.push(Arc::new(AsyncMutex::new(Some(job))));
        }
        rx.await.map_err(|_| Error::OutcomeUnknown)?
    }
    async fn picture(&self) -> Result<Option<String>, Error> {
        self.inner.picture().await
    }
    async fn barrier(&self) -> Result<(), Error> {
        let pending = {
            let mut jobs = self.jobs.lock().map_err(|_| Error::OutcomeUnknown)?;
            jobs.closed = true;
            jobs.pending.clone()
        };
        for job in pending {
            let mut guard = job.lock().await;
            if let Some(handle) = guard.as_mut() {
                handle.await.map_err(|_| Error::OutcomeUnknown)?;
                guard.take();
            }
        }
        self.inner.barrier().await
    }
    async fn detach(&self) -> Result<(), Error> {
        self.barrier().await?;
        self.inner.detach().await
    }
}
