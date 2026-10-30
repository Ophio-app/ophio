// SPDX-License-Identifier: Apache-2.0
//! Clipboard ownership exists only inside an explicitly consented portal session.
use ashpd::desktop::{Session, clipboard::Clipboard, remote_desktop::RemoteDesktop};
use futures_util::StreamExt;
use ports::desktop::PlatformError;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;

pub(crate) struct PortalClipboard {
    portal: Arc<Clipboard>,
    text: Arc<Mutex<String>>,
    worker: tokio::task::JoinHandle<()>,
}
impl PortalClipboard {
    pub(crate) async fn start(
        portal: Arc<Clipboard>,
        session: Arc<Session<RemoteDesktop>>,
    ) -> Result<Self, PlatformError> {
        let text = Arc::new(Mutex::new(String::new()));
        let supplied = text.clone();
        let writer = portal.clone();
        let (ready, received) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let Ok(transfers) = writer.receive_selection_transfer::<RemoteDesktop>().await else {
                let _ = ready.send(false);
                return;
            };
            let mut transfers = std::pin::pin!(transfers);
            let _ = ready.send(true);
            while let Some((owner, mime, serial)) = transfers.next().await {
                if serde_json::to_value(&owner).ok() != serde_json::to_value(&*session).ok() {
                    continue;
                }
                let value = if matches!(mime.as_str(), "text/plain;charset=utf-8" | "text/plain") {
                    supplied.lock().ok().map(|text| text.clone())
                } else {
                    None
                };
                let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    let value = value.ok_or(())?;
                    let fd = writer
                        .selection_write(&session, serial)
                        .await
                        .map_err(|_| ())?;
                    let fd: std::os::fd::OwnedFd = fd.into();
                    let mut file = tokio::fs::File::from_std(std::fs::File::from(fd));
                    file.write_all(value.as_bytes()).await.map_err(|_| ())?;
                    file.flush().await.map_err(|_| ())?;
                    drop(file);
                    Ok::<_, ()>(())
                })
                .await;
                let _ = writer
                    .selection_write_done(&session, serial, matches!(result, Ok(Ok(()))))
                    .await;
            }
        });
        // Owned before the first wait, so a start given up there ends the
        // worker and with it the worker's hold on the session.
        let clipboard = Self {
            portal,
            text,
            worker,
        };
        if received.await != Ok(true) {
            return Err(PlatformError::Unsupported(
                "Clipboard transfer notifications are unavailable".into(),
            ));
        }
        Ok(clipboard)
    }
    pub(crate) async fn set(
        &self,
        session: &Session<RemoteDesktop>,
        text: &str,
    ) -> Result<(), PlatformError> {
        *self.text.lock().map_err(crate::native::failure)? = text.to_owned();
        self.portal
            .set_selection(
                session,
                ashpd::desktop::clipboard::SetSelectionOptions::default()
                    .set_mime_types(&["text/plain;charset=utf-8", "text/plain"]),
            )
            .await
            .map_err(super::portal::portal_input_error)
    }
}
impl Drop for PortalClipboard {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[tokio::test]
    async fn a_start_given_up_before_it_is_ready_lets_go_of_the_session() {
        let bus = crate::fake_portal::Bus::start();
        let (seen, _saw) = tokio::sync::mpsc::unbounded_channel();
        let _portal = crate::fake_portal::serve(&bus, seen).await;
        let client = bus.connect().await;
        let remote = RemoteDesktop::with_connection(client.clone())
            .await
            .unwrap();
        let session = Arc::new(remote.create_session(Default::default()).await.unwrap());
        let portal = Arc::new(Clipboard::with_connection(client).await.unwrap());
        // One poll spawns the transfer worker; dropping the start then gives it up.
        assert!(
            PortalClipboard::start(portal, session.clone())
                .now_or_never()
                .is_none()
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while Arc::strong_count(&session) > 1 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("nothing keeps the session once its clipboard start is given up");
    }
}
