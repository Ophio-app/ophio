// SPDX-License-Identifier: Apache-2.0
//! Production composition; browser launch happens only after an explicit voice Start.
use async_trait::async_trait;
use protocol::{capability::Availability, voice::VoiceTarget};
use std::{path::PathBuf, sync::Arc};
use voice_control::{Accessibility, Driver, DriverFactory, Error, Snapshot};
#[derive(Debug)]
struct Browsers {
    data: PathBuf,
    retained: Windows<browser::Browser>,
}
/// A session's window stays open after the session ends, so each is kept
/// here until it closes. At most `OPEN_WINDOWS` are open at once.
#[derive(Debug)]
struct Windows<W>(tokio::sync::Mutex<Vec<Arc<W>>>);
const OPEN_WINDOWS: usize = 16;
trait Window {
    fn is_closed(&self) -> bool;
}
impl Window for browser::Browser {
    fn is_closed(&self) -> bool {
        browser::Browser::is_closed(self)
    }
}
impl<W> Default for Windows<W> {
    fn default() -> Self {
        Self(Default::default())
    }
}
impl<W: Window> Windows<W> {
    async fn open(
        &self,
        launch: impl std::future::Future<Output = Result<Arc<W>, Error>>,
    ) -> Result<Arc<W>, Error> {
        let mut open = self.0.lock().await;
        open.retain(|window| !window.is_closed());
        if open.len() >= OPEN_WINDOWS {
            return Err(Error::Unavailable(
                "Close one of the separate browser windows on the computer before starting another browser session".into(),
            ));
        }
        let window = launch.await?;
        open.push(window.clone());
        Ok(window)
    }
}
#[async_trait]
impl DriverFactory for Browsers {
    async fn availability(&self, _: VoiceTarget) -> Availability {
        if browser::discover().is_some() {
            Availability::Available
        } else {
            Availability::PermissionMissing {
                remedy: "Install Chromium, Chrome, Edge or Brave on the computer".into(),
            }
        }
    }
    async fn open(&self, target: VoiceTarget) -> Result<Arc<dyn Driver>, Error> {
        if target != VoiceTarget::Browser {
            return Err(Error::Unavailable(
                "Use the desktop accessibility driver".into(),
            ));
        }
        let executable = browser::discover().ok_or_else(|| {
            Error::Unavailable("Install a Chromium-family browser on the computer".into())
        })?;
        let browser = self
            .retained
            .open(browser::Browser::launch(&executable, &self.data, false))
            .await?;
        Ok(browser)
    }
}
#[derive(Debug)]
struct DesktopAccessibility;
#[async_trait]
impl Accessibility for DesktopAccessibility {
    async fn snapshot(&self) -> Result<Snapshot, Error> {
        #[cfg(target_os = "linux")]
        {
            platform_linux::a11y::AtSpi::connect()
                .await?
                .snapshot()
                .await
        }
        #[cfg(windows)]
        {
            platform_windows::a11y::Uia.snapshot().await
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            Err(Error::Unavailable(
                "Desktop accessibility is unavailable".into(),
            ))
        }
    }
    async fn activate(
        &self,
        element: &protocol::voice::VoiceElement,
        focus: bool,
    ) -> Result<bool, Error> {
        #[cfg(target_os = "linux")]
        {
            platform_linux::a11y::AtSpi::connect()
                .await?
                .activate(element, focus)
                .await
        }
        #[cfg(windows)]
        {
            platform_windows::a11y::Uia.activate(element, focus).await
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = (element, focus);
            Err(Error::Unavailable(
                "Desktop accessibility is unavailable".into(),
            ))
        }
    }
}
pub(crate) async fn configure(
    hub: &services::CompanionHub,
    data: PathBuf,
    assessor: Arc<::jev::Jev>,
) -> Result<(), protocol::ProtocolError> {
    // Windows closed while Ophio was not running leave their profiles behind.
    browser::sweep(&data).await;
    hub.configure_voice(services::voice::VoiceConfig {
        browser: Arc::new(Browsers {
            data,
            retained: Default::default(),
        }),
        desktop: Arc::new(DesktopAccessibility),
        assessor,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct FakeWindow(AtomicBool);
    impl Window for FakeWindow {
        fn is_closed(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
    }
    async fn launch() -> Result<Arc<FakeWindow>, Error> {
        Ok(Arc::default())
    }

    #[tokio::test]
    async fn a_session_starts_once_an_earlier_window_has_closed() {
        let windows = Windows::default();
        let mut open = Vec::new();
        for _ in 0..OPEN_WINDOWS {
            open.push(windows.open(launch()).await.unwrap());
        }
        assert!(matches!(
            windows.open(launch()).await,
            Err(Error::Unavailable(_))
        ));

        open[3].0.store(true, Ordering::Relaxed);
        let seventeenth = windows.open(launch()).await.unwrap();
        assert!(matches!(
            windows.open(launch()).await,
            Err(Error::Unavailable(_))
        ));
        assert_eq!(windows.0.lock().await.len(), OPEN_WINDOWS);
        assert!(!seventeenth.is_closed());

        for window in &open {
            window.0.store(true, Ordering::Relaxed);
        }
        for _ in 0..OPEN_WINDOWS - 1 {
            windows.open(launch()).await.unwrap();
        }
    }
}
