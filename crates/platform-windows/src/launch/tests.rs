// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::sync::{Mutex, atomic::AtomicUsize};

mod authority;
mod packaged;

#[test]
fn start_menu_environment_names_are_case_insensitive() {
    let environment = BTreeMap::from([
        ("AppData".into(), "/fixture/user".into()),
        ("ProgramData".into(), "/fixture/common".into()),
    ]);
    assert_eq!(
        start_menu_roots(&environment),
        vec![
            PathBuf::from("/fixture/user").join("Microsoft/Windows/Start Menu/Programs"),
            PathBuf::from("/fixture/common").join("Microsoft/Windows/Start Menu/Programs"),
        ]
    );
}

#[derive(Debug, Default)]
struct Fake {
    entries: Mutex<Vec<Entry>>,
    packaged: Mutex<Vec<PackagedApp>>,
    defaults: Mutex<BTreeMap<AppRole, String>>,
    calls: Arc<AtomicUsize>,
    fail_prepare: AtomicBool,
    delay_prepare: Mutex<Duration>,
    delay_open: Mutex<Duration>,
}
struct FakeReady {
    calls: Arc<AtomicUsize>,
    delay: Duration,
}
impl Prepared for FakeReady {
    fn open(self: Box<Self>) -> Result<(), Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        Ok(())
    }
}
impl Platform for Fake {
    fn entries(&self) -> Result<Vec<Entry>, Error> {
        Ok(self.entries.lock().unwrap().clone())
    }
    fn prepare(&self, entry: &Entry) -> Result<Box<dyn Prepared>, Error> {
        std::thread::sleep(*self.delay_prepare.lock().unwrap());
        if self.fail_prepare.load(Ordering::SeqCst) || !self.entries.lock().unwrap().contains(entry)
        {
            return Err(Error::StaleTarget);
        }
        Ok(Box::new(FakeReady {
            calls: self.calls.clone(),
            delay: *self.delay_open.lock().unwrap(),
        }))
    }
    fn packaged_entries(&self) -> Result<Vec<PackagedApp>, Error> {
        Ok(self.packaged.lock().unwrap().clone())
    }
    fn prepare_packaged(&self, app: &PackagedApp) -> Result<Box<dyn Prepared>, Error> {
        std::thread::sleep(*self.delay_prepare.lock().unwrap());
        if self.fail_prepare.load(Ordering::SeqCst) || !self.packaged.lock().unwrap().contains(app)
        {
            return Err(Error::StaleTarget);
        }
        Ok(Box::new(FakeReady {
            calls: self.calls.clone(),
            delay: *self.delay_open.lock().unwrap(),
        }))
    }
    fn packaged_defaults(&self) -> Result<BTreeMap<AppRole, String>, Error> {
        Ok(self.defaults.lock().unwrap().clone())
    }
    fn prepare_url(&self, _: &str) -> Result<Box<dyn Prepared>, Error> {
        let entry = self.entries.lock().unwrap()[0].clone();
        self.prepare(&entry)
    }
}
struct Allow;
impl Authority for Allow {
    fn check(&self) -> Result<(), Error> {
        Ok(())
    }
}
struct Stop;
impl Authority for Stop {
    fn check(&self) -> Result<(), Error> {
        Err(Error::Stopped)
    }
}
struct StopLast(AtomicUsize);
impl Authority for StopLast {
    fn check(&self) -> Result<(), Error> {
        if self.0.fetch_add(1, Ordering::SeqCst) >= 2 {
            Err(Error::Stopped)
        } else {
            Ok(())
        }
    }
}
fn setup() -> (Arc<Fake>, WindowsLauncher) {
    let platform = Arc::new(Fake::default());
    platform.entries.lock().unwrap().push(Entry {
        path: PathBuf::from("/Start Menu/Editor.lnk"),
        name: "Editor".into(),
        bytes: b"shortcut".to_vec(),
    });
    let launcher = WindowsLauncher::new(platform.clone());
    (platform, launcher)
}
async fn selected(launcher: &WindowsLauncher) -> VoiceApp {
    let app = launcher.apps().await.unwrap().remove(0);
    VoiceApp {
        id: app.id,
        name: app.name,
    }
}
#[tokio::test]
async fn stable_opaque_ids_and_approved_launch() {
    let (fake, launcher) = setup();
    let app = selected(&launcher).await;
    assert_eq!(selected(&launcher).await, app);
    assert!(!app.id.contains("Editor"));
    launcher
        .launch_authorized(&app, Arc::new(Allow))
        .await
        .unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn stale_tampered_unknown_and_renamed_entries_are_refused() {
    let (fake, launcher) = setup();
    let original = selected(&launcher).await;
    for app in [
        VoiceApp {
            id: "cmd.exe /c arbitrary".into(),
            ..original.clone()
        },
        VoiceApp {
            name: "Other".into(),
            ..original.clone()
        },
    ] {
        assert!(matches!(
            launcher.launch_authorized(&app, Arc::new(Allow)).await,
            Err(Error::StaleTarget)
        ));
    }
    fake.entries.lock().unwrap()[0].bytes.push(1);
    assert!(matches!(
        launcher.launch_authorized(&original, Arc::new(Allow)).await,
        Err(Error::StaleTarget)
    ));
    let replacement = selected(&launcher).await;
    assert_ne!(replacement.id, original.id);
    fake.entries.lock().unwrap().clear();
    assert!(matches!(
        launcher
            .launch_authorized(&replacement, Arc::new(Allow))
            .await,
        Err(Error::StaleTarget)
    ));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn stop_and_final_permission_check_prevent_delivery_and_recover() {
    let (fake, launcher) = setup();
    let app = selected(&launcher).await;
    assert!(matches!(
        launcher.launch_authorized(&app, Arc::new(Stop)).await,
        Err(Error::Stopped)
    ));
    assert!(matches!(
        launcher
            .launch_authorized(&app, Arc::new(StopLast(AtomicUsize::new(0))))
            .await,
        Err(Error::Stopped)
    ));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    fake.fail_prepare.store(true, Ordering::SeqCst);
    assert!(matches!(
        launcher.launch_authorized(&app, Arc::new(Allow)).await,
        Err(Error::StaleTarget)
    ));
    fake.fail_prepare.store(false, Ordering::SeqCst);
    launcher
        .launch_authorized(&app, Arc::new(Allow))
        .await
        .unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn urls_are_data_and_only_http_or_https() {
    let (fake, launcher) = setup();
    for url in [
        "file:///test",
        "shell:AppsFolder",
        "javascript:alert(1)",
        "https://",
        "https:///test",
        "https://example.test/\ncmd",
        "https://example.test/a b",
    ] {
        assert!(matches!(
            launcher.open_url_authorized(url, Arc::new(Allow)).await,
            Err(Error::StaleTarget)
        ));
    }
    assert!(matches!(
        launcher
            .open_url_authorized("https://example.test", Arc::new(Stop))
            .await,
        Err(Error::Stopped)
    ));
    launcher
        .open_url_authorized("https://example.test/?q=%22;%20cmd", Arc::new(Allow))
        .await
        .unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn timed_out_preparation_cannot_deliver_later() {
    let (fake, launcher) = setup();
    let app = selected(&launcher).await;
    *fake.delay_prepare.lock().unwrap() = PREPARE + Duration::from_millis(100);
    assert!(matches!(
        launcher.launch_authorized(&app, Arc::new(Allow)).await,
        Err(Error::Unavailable(_))
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn aborted_preparation_cannot_deliver_later() {
    let (fake, launcher) = setup();
    let app = selected(&launcher).await;
    *fake.delay_prepare.lock().unwrap() = Duration::from_millis(100);
    let launcher = Arc::new(launcher);
    let task = tokio::spawn(async move { launcher.launch_authorized(&app, Arc::new(Allow)).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    task.abort();
    let _ = task.await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn dispatched_timeout_is_never_reported_as_canceled() {
    let (fake, launcher) = setup();
    let app = selected(&launcher).await;
    *fake.delay_open.lock().unwrap() = OPEN + Duration::from_millis(100);
    assert!(matches!(
        launcher.launch_authorized(&app, Arc::new(Allow)).await,
        Err(Error::LaunchUnstopped)
    ));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("voice-launch-{}", protocol::CommandId::new()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn discovery_is_bounded_and_ignores_non_shortcuts() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.0.join("Tools")).unwrap();
    std::fs::write(fixture.0.join("Tools/Editor.lnk"), b"entry").unwrap();
    std::fs::write(fixture.0.join("command.cmd"), b"not an entry").unwrap();
    std::fs::write(fixture.0.join("oversized.lnk"), vec![0; 1024 * 1024 + 1]).unwrap();
    let entries = discover(std::slice::from_ref(&fixture.0)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "Editor");
    assert_eq!(entries[0].bytes, b"entry");
}
#[cfg(unix)]
#[test]
fn discovery_does_not_follow_links_or_linked_roots() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    std::fs::write(outside.0.join("Outside.lnk"), b"entry").unwrap();
    std::os::unix::fs::symlink(&outside.0, fixture.0.join("linked")).unwrap();
    std::os::unix::fs::symlink(outside.0.join("Outside.lnk"), fixture.0.join("Alias.lnk")).unwrap();
    assert!(
        discover(std::slice::from_ref(&fixture.0))
            .unwrap()
            .is_empty()
    );
    assert!(discover(&[fixture.0.join("linked")]).unwrap().is_empty());
}
/// Native filesystem fixture only: never calls ShellExecute or accesses a desktop.
#[cfg(windows)]
#[test]
fn native_preparation_locks_identity_and_rejects_replacement() {
    let fixture = Fixture::new();
    let root = fixture.0.join("Programs");
    std::fs::create_dir(&root).unwrap();
    let path = root.join("Fixture.lnk");
    std::fs::write(&path, b"synthetic shortcut; never executed").unwrap();
    let platform = native::Native {
        roots: vec![root.clone()],
    };
    let entry = platform.entries().unwrap().remove(0);
    let prepared = platform.prepare(&entry).unwrap();
    assert!(std::fs::write(&path, b"replacement").is_err());
    assert!(std::fs::rename(&path, root.join("Moved.lnk")).is_err());
    assert!(std::fs::rename(&root, fixture.0.join("Moved")).is_err());
    drop(prepared);
    std::fs::write(&path, b"replacement").unwrap();
    assert!(matches!(platform.prepare(&entry), Err(Error::StaleTarget)));
}
