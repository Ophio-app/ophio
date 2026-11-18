// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
};
#[derive(Debug)]
struct Fake {
    windows: Mutex<Vec<NativeWindow>>,
    permitted: AtomicBool,
    calls: AtomicUsize,
    revoke: Mutex<Option<Arc<Allowed>>>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            windows: Mutex::new(vec![row()]),
            permitted: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
            revoke: Mutex::new(None),
        }
    }
}
impl Driver for Fake {
    fn availability(&self) -> Availability {
        if self.permitted.load(SeqCst) {
            Availability::Available
        } else {
            Availability::PermissionMissing {
                remedy: "Enable Accessibility".into(),
            }
        }
    }
    fn list(&self) -> Result<Vec<NativeWindow>, PlatformError> {
        if !self.permitted.load(SeqCst) {
            return Err(PlatformError::PermissionMissing(
                "Enable Accessibility".into(),
            ));
        }
        Ok(self.windows.lock().unwrap().clone())
    }
    fn act(
        &self,
        expected: &NativeWindow,
        _: bool,
        authority: &dyn WindowAuthority,
    ) -> Result<(), PlatformError> {
        let current = self.list()?;
        if !current
            .iter()
            .any(|w| w.identity == expected.identity && w.app == expected.app)
        {
            return Err(PlatformError::TargetNotFound);
        }
        if let Some(revoke) = self.revoke.lock().unwrap().take() {
            revoke.0.store(false, SeqCst);
        }
        authority.check()?;
        self.calls.fetch_add(1, SeqCst);
        Ok(())
    }
}
#[derive(Debug, Default)]
struct Allowed(AtomicBool);
impl WindowAuthority for Allowed {
    fn check(&self) -> Result<(), PlatformError> {
        if self.0.load(SeqCst) {
            Ok(())
        } else {
            Err(PlatformError::PermissionMissing("Control stopped".into()))
        }
    }
}
fn allowed() -> Arc<Allowed> {
    Arc::new(Allowed(AtomicBool::new(true)))
}
fn row() -> NativeWindow {
    NativeWindow {
        identity: "retained-native-window-a".into(),
        pid: 42,
        app: "Fixture".into(),
        title: "Document".into(),
        focused: true,
        bounds: protocol::voice::VoiceBounds {
            x: -20.,
            y: 10.,
            width: 800.,
            height: 600.,
        },
    }
}
#[derive(Debug, Default)]
struct Apps(AtomicUsize);
#[async_trait]
impl Launcher for Apps {
    async fn apps(&self) -> Result<Vec<LaunchApp>, voice_control::Error> {
        Ok(vec![LaunchApp {
            id: "bundle-fingerprint".into(),
            name: "Fixture".into(),
            aliases: vec![],
            roles: vec![],
            defaults: vec![],
            listed: true,
            detail: None,
        }])
    }
    async fn launch(
        &self,
        app: &VoiceApp,
        authority: &dyn voice_control::Authority,
    ) -> Result<(), voice_control::Error> {
        if app.id != "bundle-fingerprint" || app.name != "Fixture" {
            return Err(voice_control::Error::StaleTarget);
        }
        authority.check()?;
        self.0.fetch_add(1, SeqCst);
        Ok(())
    }
    async fn open_url(
        &self,
        _: &str,
        _: &dyn voice_control::Authority,
    ) -> Result<(), voice_control::Error> {
        unreachable!()
    }
}
fn adapter(driver: Arc<Fake>, apps: Arc<Apps>) -> MacWindows {
    MacWindows::new(driver, apps, Arc::new(ports::SystemClock))
}
#[tokio::test]
async fn mac_windows_list_search_focus_close_and_separate_app_launch() {
    let driver = Arc::new(Fake::default());
    let apps = Arc::new(Apps::default());
    let adapter = adapter(driver.clone(), apps.clone());
    let catalog = adapter.catalog("fix").await.unwrap();
    assert_eq!(catalog.windows.len(), 1);
    assert_eq!(catalog.apps.len(), 1);
    let id = catalog.windows[0].window_id.clone();
    assert_eq!(
        adapter
            .act(
                &WindowAction::Focus {
                    window_id: id.clone()
                },
                allowed()
            )
            .await
            .unwrap()
            .0,
        WindowOutcome::Focused
    );
    assert_eq!(
        adapter
            .act(&WindowAction::Close { window_id: id }, allowed())
            .await
            .unwrap()
            .0,
        WindowOutcome::CloseRequested
    );
    assert_eq!(driver.calls.load(SeqCst), 2);
    assert_eq!(
        adapter
            .act(
                &WindowAction::OpenApp {
                    app_id: catalog.apps[0].app_id.clone()
                },
                allowed()
            )
            .await
            .unwrap()
            .0,
        WindowOutcome::AppLaunchRequested
    );
    assert_eq!(apps.0.load(SeqCst), 1);
    assert_eq!(adapter.focused().await.unwrap().x, -20.);
    let empty = adapter.catalog("no matching name").await.unwrap();
    assert!(empty.windows.is_empty() && empty.apps.is_empty());
}
#[tokio::test]
async fn mac_windows_stale_reference_and_forged_command_never_dispatch() {
    let driver = Arc::new(Fake::default());
    let apps = Arc::new(Apps::default());
    let adapter = adapter(driver.clone(), apps.clone());
    let catalog = adapter.catalog("").await.unwrap();
    // Same title/app/position; only native identity changed.
    driver.windows.lock().unwrap()[0].identity = "replacement-native-window".into();
    assert!(matches!(
        adapter
            .act(
                &WindowAction::Close {
                    window_id: catalog.windows[0].window_id.clone()
                },
                allowed()
            )
            .await,
        Err(PlatformError::TargetNotFound)
    ));
    assert!(matches!(
        adapter
            .act(
                &WindowAction::OpenApp {
                    app_id: "/bin/anything --argument".into()
                },
                allowed()
            )
            .await,
        Err(PlatformError::TargetNotFound)
    ));
    assert_eq!(driver.calls.load(SeqCst), 0);
    assert_eq!(apps.0.load(SeqCst), 0);
    let fresh = adapter.catalog("").await.unwrap();
    adapter
        .act(
            &WindowAction::Focus {
                window_id: fresh.windows[0].window_id.clone(),
            },
            allowed(),
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn mac_windows_permission_revoke_and_recovery_do_not_hide_app_catalog() {
    let driver = Arc::new(Fake::default());
    let adapter = adapter(driver.clone(), Arc::default());
    let catalog = adapter.catalog("").await.unwrap();
    driver.permitted.store(false, SeqCst);
    assert!(matches!(
        adapter
            .act(
                &WindowAction::Focus {
                    window_id: catalog.windows[0].window_id.clone()
                },
                allowed()
            )
            .await,
        Err(PlatformError::PermissionMissing(_))
    ));
    let refused = adapter.catalog("").await.unwrap();
    assert!(matches!(
        refused.capabilities.windows,
        Availability::PermissionMissing { .. }
    ));
    assert!(refused.windows.is_empty());
    assert_eq!(refused.apps.len(), 1);
    assert!(refused.capabilities.open_apps.is_available());
    driver.permitted.store(true, SeqCst);
    let fresh = adapter.catalog("").await.unwrap();
    assert_eq!(fresh.windows.len(), 1);
    adapter
        .act(
            &WindowAction::Focus {
                window_id: fresh.windows[0].window_id.clone(),
            },
            allowed(),
        )
        .await
        .unwrap();
    assert_eq!(driver.calls.load(SeqCst), 1);
}
#[tokio::test]
async fn mac_windows_stop_during_native_revalidation_blocks_focus_close_and_launch() {
    let driver = Arc::new(Fake::default());
    let apps = Arc::new(Apps::default());
    let adapter = adapter(driver.clone(), apps.clone());
    let catalog = adapter.catalog("").await.unwrap();
    for close in [false, true] {
        let authority = allowed();
        *driver.revoke.lock().unwrap() = Some(authority.clone());
        let id = catalog.windows[0].window_id.clone();
        let action = if close {
            WindowAction::Close { window_id: id }
        } else {
            WindowAction::Focus { window_id: id }
        };
        assert!(matches!(
            adapter.act(&action, authority).await,
            Err(PlatformError::PermissionMissing(_))
        ));
    }
    assert!(matches!(
        adapter
            .act(
                &WindowAction::OpenApp {
                    app_id: catalog.apps[0].app_id.clone()
                },
                Arc::new(Allowed::default())
            )
            .await,
        Err(PlatformError::PermissionMissing(_))
    ));
    assert_eq!(driver.calls.load(SeqCst), 0);
    assert_eq!(apps.0.load(SeqCst), 0);
}
#[tokio::test]
async fn mac_windows_catalog_marks_truncation_and_launch_failure_honestly() {
    let driver = Arc::new(Fake::default());
    *driver.windows.lock().unwrap() = (0..201)
        .map(|i| NativeWindow {
            identity: format!("window-{i}"),
            ..row()
        })
        .collect();
    let adapter = adapter(driver, Arc::default());
    let catalog = adapter.catalog("").await.unwrap();
    assert!(catalog.truncated);
    assert_eq!(catalog.windows.len(), 200);
    assert!(matches!(
        unavailable(voice_control::Error::LaunchUnstopped),
        PlatformError::LaunchUnstopped
    ));
    assert!(matches!(
        unavailable(voice_control::Error::OutcomeUnknown),
        PlatformError::OutcomeUnknown(_)
    ));
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "Requires explicit permission and a running disposable AppKit fixture; focuses and closes only that fixture"]
async fn mac_windows_native_disposable_fixture_focus_close_and_stale_identity() {
    assert_eq!(
        std::env::var("OPHIO_MAC_ACTIONS_FIXTURE").as_deref(),
        Ok("1"),
        "Run only in an approved disposable native desktop fixture slot"
    );
    let markers = std::path::PathBuf::from(
        std::env::var_os("OPHIO_MAC_NATIVE_FIXTURE_MARKERS")
            .expect("Set the disposable fixture marker directory"),
    );
    assert!(markers.is_absolute());
    let pid: i32 = std::fs::read_to_string(markers.join("pid.txt"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(pid > 0);
    let driver = native::Native::default();
    let fixture = driver
        .list()
        .unwrap()
        .into_iter()
        .find(|w| w.pid == pid && w.title == "Ophio native fixture")
        .expect("Start the disposable native fixture first");
    assert_eq!(fixture.app, "Ophio Native Fixture");
    let authority = allowed();
    authority.0.store(false, SeqCst);
    assert!(matches!(
        driver.act(&fixture, false, &*authority),
        Err(PlatformError::PermissionMissing(_))
    ));
    authority.0.store(true, SeqCst);
    driver.act(&fixture, false, &*authority).unwrap();
    assert!(
        driver
            .list()
            .unwrap()
            .iter()
            .any(|w| w.identity == fixture.identity && w.focused)
    );
    driver.act(&fixture, true, &*authority).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while driver
        .list()
        .unwrap()
        .iter()
        .any(|w| w.identity == fixture.identity)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "fixture did not close"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(matches!(
        driver.act(&fixture, false, &*authority),
        Err(PlatformError::TargetNotFound)
    ));
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "Requires an approved native slot with Accessibility deliberately absent; reads trust state but never prompts"]
async fn mac_windows_native_missing_permission_is_reported_without_prompt() {
    assert_eq!(
        std::env::var("OPHIO_MAC_ACTIONS_PERMISSION_MISSING").as_deref(),
        Ok("1")
    );
    let driver = native::Native::default();
    assert!(matches!(
        driver.availability(),
        Availability::PermissionMissing { .. }
    ));
    assert!(matches!(
        driver.list(),
        Err(PlatformError::PermissionMissing(_))
    ));
}
