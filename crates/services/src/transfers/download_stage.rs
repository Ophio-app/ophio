// SPDX-License-Identifier: Apache-2.0
use crate::internal;
use protocol::ProtocolError;
use std::{
    fs::File,
    io::{self, Write},
    path::PathBuf,
};
use tokio::sync::OwnedRwLockReadGuard;

/// An unpublished download owns its staging name until it is removed. Move
/// that ownership into each filesystem worker: dropping an async caller must
/// not unlink before a queued create runs, or leave an in-flight write behind.
#[derive(Debug)]
pub(super) struct DownloadStage {
    path: PathBuf,
    file: Option<File>,
    removed: bool,
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Result<T, ProtocolError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| internal("download staging worker failed"))?
        .map_err(internal)
}

impl DownloadStage {
    fn create_file(path: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| io::Error::other("stage directory missing"))?,
        )?;
        let file = File::options().write(true).create_new(true).open(&path)?;
        Ok(Self {
            path,
            file: Some(file),
            removed: false,
        })
    }

    pub(super) async fn create(path: PathBuf) -> Result<Self, ProtocolError> {
        blocking(move || Self::create_file(path)).await
    }

    async fn with_file<T: Send + 'static>(
        mut self,
        operation: impl FnOnce(&mut File) -> io::Result<T> + Send + 'static,
    ) -> Result<(Self, T), ProtocolError> {
        blocking(move || {
            let value = operation(self.file.as_mut().expect("stage owns its file"))?;
            Ok((self, value))
        })
        .await
    }

    pub(super) async fn write(
        self,
        bytes: Vec<u8>,
        len: usize,
    ) -> Result<(Self, Vec<u8>), ProtocolError> {
        self.with_file(move |file| {
            file.write_all(&bytes[..len])?;
            Ok(bytes)
        })
        .await
    }

    pub(super) async fn publish(
        mut self,
        destination: PathBuf,
        publishing: OwnedRwLockReadGuard<()>,
    ) -> Result<OwnedRwLockReadGuard<()>, ProtocolError> {
        blocking(move || {
            self.file
                .as_ref()
                .expect("stage owns its file")
                .sync_all()?;
            let parent = destination
                .parent()
                .ok_or_else(|| io::Error::other("artifact directory missing"))?;
            std::fs::create_dir_all(parent)?;
            match std::fs::hard_link(&self.path, destination.as_path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            #[cfg(unix)]
            File::open(parent)?.sync_all()?;
            self.remove()?;
            // The collection lock must outlive even a cancelled hard-link or
            // directory sync. Return it only after the worker has finished.
            Ok(publishing)
        })
        .await
    }

    fn remove(&mut self) -> io::Result<()> {
        // Close first for platforms that refuse unlinking an open file.
        drop(self.file.take());
        if !self.removed {
            std::fs::remove_file(&self.path)?;
            self.removed = true;
        }
        Ok(())
    }
}

impl Drop for DownloadStage {
    fn drop(&mut self) {
        // Every operation owns this guard; no create or write can recreate
        // staging after cleanup. Normal publication reports cleanup errors.
        let _ = self.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn removed(path: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while path.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled filesystem operation left staging behind");
    }

    #[tokio::test]
    async fn cancelled_creation_cleans_up_even_when_the_file_is_created_later() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stage");
        let target = path.clone();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let (finished, done) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(blocking(move || {
            let _ = entered.send(());
            gate.recv().unwrap();
            let stage = DownloadStage::create_file(target)?;
            let _ = finished.send(());
            Ok(stage)
        }));
        started.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        done.await.unwrap();
        removed(&path).await;
    }

    #[tokio::test]
    async fn cancelled_write_keeps_cleanup_owned_until_the_worker_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stage");
        let stage = DownloadStage::create(path.clone()).await.unwrap();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let task = tokio::spawn(stage.with_file(move |file| {
            file.write_all(b"before cancellation")?;
            let _ = entered.send(());
            gate.recv().unwrap();
            file.write_all(b"after cancellation")
        }));
        started.await.unwrap();
        let written = std::fs::metadata(&path).unwrap().len();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        removed(&path).await;
        assert!(written > 0);
    }
}
