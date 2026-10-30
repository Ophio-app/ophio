// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Debug)]
struct Fake {
    windows: Mutex<Vec<NativeWindow>>,
    effects: AtomicUsize,
    refuse_focus: bool,
}
impl Driver for Fake {
    fn list(&self) -> Result<Vec<NativeWindow>, PlatformError> {
        Ok(self.windows.lock().unwrap().clone())
    }
    fn focus(&self, _: &NativeWindow) -> Result<(), PlatformError> {
        if self.refuse_focus {
            return Err(PlatformError::PermissionMissing(
                "Windows refused focus".into(),
            ));
        }
        self.effects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn close(&self, _: &NativeWindow) -> Result<(), PlatformError> {
        self.effects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn launch(&self, _: &Path) -> Result<(), PlatformError> {
        self.effects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Permit(bool);
impl WindowAuthority for Permit {
    fn check(&self) -> Result<(), PlatformError> {
        if self.0 {
            Ok(())
        } else {
            Err(PlatformError::PermissionMissing("expired".into()))
        }
    }
}
fn driver(refuse_focus: bool) -> Arc<Fake> {
    Arc::new(Fake {
        windows: Mutex::new(vec![NativeWindow {
            handle: 12,
            pid: 20,
            title: "Editor".into(),
            app: "editor.exe".into(),
            monitor: Some("Display 1".into()),
            focused: false,
        }]),
        effects: AtomicUsize::new(0),
        refuse_focus,
    })
}
#[tokio::test]
async fn focus_refusal_close_request_and_stale_handles_remain_distinct() {
    let driver = driver(true);
    let backend = WindowsActions::new(driver.clone(), vec![], Arc::new(ports::SystemClock));
    let catalog = backend.catalog("").await.unwrap();
    let id = catalog.windows[0].window_id.clone();
    assert!(matches!(
        backend
            .act(
                &WindowAction::Focus {
                    window_id: id.clone()
                },
                Arc::new(Permit(true))
            )
            .await,
        Err(PlatformError::PermissionMissing(_))
    ));
    assert_eq!(
        backend
            .act(
                &WindowAction::Close {
                    window_id: id.clone()
                },
                Arc::new(Permit(true))
            )
            .await
            .unwrap()
            .0,
        WindowOutcome::CloseRequested
    );
    driver.windows.lock().unwrap()[0].pid = 30;
    assert!(matches!(
        backend
            .act(
                &WindowAction::Close { window_id: id },
                Arc::new(Permit(true))
            )
            .await,
        Err(PlatformError::TargetNotFound)
    ));
    assert_eq!(driver.effects.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn start_menu_only_known_unchanged_shortcuts_can_launch_with_authority() {
    let root = std::env::temp_dir().join(format!("window-app-test-{}", protocol::CommandId::new()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("Editor.lnk"), b"fixture shortcut").unwrap();
    std::fs::write(root.join("Not an app.txt"), b"fixture").unwrap();
    let driver = driver(false);
    let backend = WindowsActions::new(
        driver.clone(),
        vec![root.clone()],
        Arc::new(ports::SystemClock),
    );
    let catalog = backend.catalog("editor").await.unwrap();
    assert_eq!(catalog.apps.len(), 1);
    let id = catalog.apps[0].app_id.clone();
    let action = WindowAction::OpenApp { app_id: id };
    assert!(backend.act(&action, Arc::new(Permit(false))).await.is_err());
    assert_eq!(driver.effects.load(Ordering::SeqCst), 0);
    assert_eq!(
        backend
            .act(&action, Arc::new(Permit(true)))
            .await
            .unwrap()
            .0,
        WindowOutcome::AppLaunchRequested
    );
    std::fs::write(root.join("Editor.lnk"), b"changed shortcut").unwrap();
    assert!(backend.act(&action, Arc::new(Permit(true))).await.is_err());
    std::fs::remove_dir_all(root).unwrap();
}
