// SPDX-License-Identifier: Apache-2.0
//! Notification-area protection, with a message boundary testable without Windows.
#[cfg(any(windows, test))]
use async_trait::async_trait;
#[cfg(any(windows, test))]
use ports::desktop::LocalIndicator;
use ports::desktop::{IndicatorSession, PlatformError};
#[cfg(any(windows, test))]
use std::sync::Arc;
#[cfg(any(windows, test))]
use tokio::sync::{mpsc, oneshot};

#[cfg(any(windows, test))]
pub(crate) enum Request {
    Show(String, oneshot::Sender<Result<(), PlatformError>>),
    Hide(oneshot::Sender<Result<(), PlatformError>>),
    Close,
}
#[cfg(any(windows, test))]
struct Indicator(mpsc::UnboundedSender<Request>);
#[cfg(any(windows, test))]
impl Drop for Indicator {
    fn drop(&mut self) {
        let _ = self.0.send(Request::Close);
    }
}
#[cfg(any(windows, test))]
#[async_trait]
impl LocalIndicator for Indicator {
    async fn show(&self, holders: &str) -> Result<(), PlatformError> {
        let (reply, answer) = oneshot::channel();
        self.0
            .send(Request::Show(holders.into(), reply))
            .map_err(|_| unavailable())?;
        answer.await.map_err(|_| unavailable())?
    }
    async fn hide(&self) -> Result<(), PlatformError> {
        let (reply, answer) = oneshot::channel();
        self.0
            .send(Request::Hide(reply))
            .map_err(|_| unavailable())?;
        answer.await.map_err(|_| unavailable())?
    }
}
#[cfg(any(windows, test))]
fn unavailable() -> PlatformError {
    PlatformError::Native("Local Stop indicator is unavailable".into())
}

#[cfg(windows)]
pub async fn connect() -> Result<IndicatorSession, PlatformError> {
    let (requests, receiver) = mpsc::unbounded_channel();
    let (stop, events) = mpsc::channel(4);
    let (ready, started) = oneshot::channel();
    std::thread::Builder::new()
        .name("local-stop".into())
        .spawn(move || crate::win::protection::run(receiver, stop, ready))
        .map_err(|_| unavailable())?;
    started.await.map_err(|_| unavailable())??;
    Ok(IndicatorSession {
        indicator: Arc::new(Indicator(requests)),
        stop: events,
    })
}
#[cfg(not(windows))]
pub async fn connect() -> Result<IndicatorSession, PlatformError> {
    Err(PlatformError::Unsupported(
        "The Windows local indicator requires Windows".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fake_tray_receives_holder_and_hide_and_reports_lost_ack() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let indicator = Arc::new(Indicator(sender));
        let call = indicator.clone();
        let show = tokio::spawn(async move { call.show("Phone input").await });
        let Request::Show(holder, reply) = receiver.recv().await.unwrap() else {
            panic!()
        };
        assert_eq!(holder, "Phone input");
        reply.send(Ok(())).unwrap();
        show.await.unwrap().unwrap();
        let call = indicator.clone();
        let hide = tokio::spawn(async move { call.hide().await });
        let Request::Hide(reply) = receiver.recv().await.unwrap() else {
            panic!()
        };
        drop(reply);
        assert!(hide.await.unwrap().is_err());
        drop(indicator);
        assert!(matches!(receiver.recv().await, Some(Request::Close)));
    }
}
