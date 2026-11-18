// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Debug, Clone, Copy)]
enum Outcome {
    Accepted,
    Rejected,
    SpawnFailure,
    Hung,
    WaitFailure,
    Unkillable,
}
#[derive(Debug)]
struct FixtureDriver {
    outcome: Outcome,
    requests: Arc<Mutex<Vec<Request>>>,
    prepared: AtomicUsize,
    killed: Arc<AtomicBool>,
    revoked: Option<Arc<AtomicBool>>,
    delay: Duration,
}
impl FixtureDriver {
    fn new(outcome: Outcome) -> Self {
        Self {
            outcome,
            requests: Arc::default(),
            prepared: AtomicUsize::new(0),
            killed: Arc::default(),
            revoked: None,
            delay: Duration::ZERO,
        }
    }
}
impl NativeDriver for FixtureDriver {
    fn info(&self, bytes: &[u8]) -> Option<Info> {
        std::thread::sleep(self.delay);
        let fields: BTreeMap<_, _> = std::str::from_utf8(bytes)
            .ok()?
            .lines()
            .filter_map(|s| s.split_once('='))
            .collect();
        let field = |key| fields.get(key).copied().unwrap_or("").to_owned();
        Some(Info {
            identifier: field("id"),
            name: field("name"),
            display_name: field("display"),
            executable: field("exe"),
            package: field("package"),
            category: field("category"),
            schemes: field("schemes").split(',').map(str::to_owned).collect(),
            content_types: field("content").split(',').map(str::to_owned).collect(),
            background: field("background") == "true",
        })
    }
    fn default_browser(&self) -> Option<String> {
        Some("test.browser".into())
    }
    fn prepare(&self, request: Request, _: Option<&Path>) -> Result<Box<dyn PreparedOpen>, Error> {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        if let Some(revoked) = &self.revoked {
            revoked.store(true, Ordering::SeqCst);
        }
        Ok(Box::new(FixturePrepared {
            outcome: self.outcome,
            request,
            requests: self.requests.clone(),
            killed: self.killed.clone(),
        }))
    }
}
struct FixturePrepared {
    outcome: Outcome,
    request: Request,
    requests: Arc<Mutex<Vec<Request>>>,
    killed: Arc<AtomicBool>,
}
impl PreparedOpen for FixturePrepared {
    fn start(self: Box<Self>) -> io::Result<Box<dyn PendingOpen>> {
        if matches!(self.outcome, Outcome::SpawnFailure) {
            return Err(io::Error::other("fixture spawn failure"));
        }
        self.requests.lock().unwrap().push(self.request.clone());
        Ok(Box::new(FixturePending {
            outcome: self.outcome,
            killed: self.killed.clone(),
        }))
    }
}
struct FixturePending {
    outcome: Outcome,
    killed: Arc<AtomicBool>,
}
#[async_trait]
impl PendingOpen for FixturePending {
    async fn wait(&mut self) -> io::Result<bool> {
        match self.outcome {
            Outcome::Accepted => Ok(true),
            Outcome::Rejected => Ok(false),
            Outcome::WaitFailure => Err(io::Error::other("fixture wait failure")),
            Outcome::Hung if self.killed.load(Ordering::SeqCst) => Ok(false),
            _ => std::future::pending().await,
        }
    }
    fn kill(&mut self) -> io::Result<()> {
        self.killed.store(true, Ordering::SeqCst);
        if matches!(self.outcome, Outcome::Unkillable) {
            Err(io::Error::other("fixture kill failure"))
        } else {
            Ok(())
        }
    }
}
struct Allow;
impl Authority for Allow {
    fn check(&self) -> Result<(), Error> {
        Ok(())
    }
}
struct Revocable(Arc<AtomicBool>);
impl Authority for Revocable {
    fn check(&self) -> Result<(), Error> {
        if self.0.load(Ordering::SeqCst) {
            Err(Error::Stopped)
        } else {
            Ok(())
        }
    }
}
fn launcher(root: &Path, driver: Arc<FixtureDriver>) -> MacLauncher {
    MacLauncher {
        roots: vec![root.into()],
        home: Some(root.into()),
        driver,
    }
}
fn fixture(root: &Path, folder: &str, extra: &str) -> PathBuf {
    let path = root.join(folder);
    fs::create_dir_all(path.join("Contents/MacOS")).unwrap();
    fs::write(
        path.join("Contents/Info.plist"),
        format!("package=APPL\nid=test.app\nname=Fixture\nexe=fixture\n{extra}"),
    )
    .unwrap();
    let executable = path.join("Contents/MacOS/fixture");
    fs::write(&executable, b"synthetic executable; never executed").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    }
    path
}
fn voice(app: &LaunchApp) -> VoiceApp {
    VoiceApp {
        id: app.id.clone(),
        name: app.name.clone(),
    }
}

#[test]
fn mac_launch_roots_include_system_and_home_without_environment_commands() {
    let environment = BTreeMap::from([
        ("HOME".into(), "/home/you".into()),
        ("BROWSER".into(), "sh -c forbidden".into()),
    ]);
    let launcher = MacLauncher::from_environment(&environment);
    assert!(launcher.roots.contains(&PathBuf::from("/Applications")));
    assert!(
        launcher
            .roots
            .contains(&PathBuf::from("/System/Applications"))
    );
    assert!(
        launcher
            .roots
            .contains(&PathBuf::from("/System/Library/CoreServices"))
    );
    assert!(
        launcher
            .roots
            .contains(&PathBuf::from("/home/you/Applications"))
    );
    let invalid =
        MacLauncher::from_environment(&BTreeMap::from([("HOME".into(), "relative".into())]));
    assert!(invalid.home.is_none());
}

#[tokio::test]
async fn mac_launch_catalog_names_roles_defaults_and_nested_utilities() {
    let root = tempfile::tempdir().unwrap();
    fixture(
        root.path(),
        "Utilities/Browser.app",
        "id=test.browser\ndisplay=Fixture Browser\nschemes=https,http\n",
    );
    fixture(
        root.path(),
        "Editor.app",
        "name=Editor\ncontent=public.plain-text,com.adobe.pdf\n",
    );
    fixture(root.path(), "Hidden.app", "background=true\n");
    fixture(root.path(), "Outer.app", "name=Outer\n");
    fixture(
        &root.path().join("Outer.app/Contents"),
        "Helper.app",
        "name=Embedded helper\n",
    );
    let driver = Arc::new(FixtureDriver::new(Outcome::Accepted));
    let apps = launcher(root.path(), driver.clone()).apps().await.unwrap();
    assert_eq!(apps.len(), 3);
    let browser = apps.iter().find(|a| a.name == "Fixture Browser").unwrap();
    assert!(browser.roles.contains(&AppRole::Browser));
    assert_eq!(browser.defaults, vec![AppRole::Browser]);
    assert!(browser.aliases.contains(&"test.browser".into()));
    assert!(browser.id.starts_with("mac-app:"));
    assert!(!browser.id.contains(root.path().to_str().unwrap()));
    let editor = apps.iter().find(|a| a.name == "Editor").unwrap();
    assert!(editor.roles.contains(&AppRole::TextEditor));
    assert!(editor.roles.contains(&AppRole::Documents));
    assert!(driver.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mac_launch_duplicate_browser_ids_require_choice_and_fake_finder_is_not_default() {
    let root = tempfile::tempdir().unwrap();
    fixture(
        root.path(),
        "One.app",
        "id=test.browser\nname=Browser\nschemes=https\n",
    );
    fixture(
        root.path(),
        "Two.app",
        "id=test.browser\nname=Browser\nschemes=https\n",
    );
    fixture(
        root.path(),
        "Finder.app",
        "id=com.apple.finder\nname=Finder\n",
    );
    let driver = Arc::new(FixtureDriver::new(Outcome::Accepted));
    let apps = launcher(root.path(), driver).apps().await.unwrap();
    assert_eq!(apps.len(), 3);
    assert!(apps.iter().all(|app| app.defaults.is_empty()));
    assert_ne!(apps[0].id, apps[1].id);
    assert!(matches!(
        voice_control::apps::resolve("the browser", &apps),
        voice_control::apps::Resolution::Several(_)
    ));
}

#[tokio::test]
async fn mac_launch_uses_exact_bundle_path_not_name_id_or_shell() {
    let root = tempfile::tempdir().unwrap();
    let path = fixture(
        root.path(),
        "Fixture ; touch ignored.app",
        "name=Fixture ; not a command\n",
    );
    let driver = Arc::new(FixtureDriver::new(Outcome::Accepted));
    let launcher = launcher(root.path(), driver.clone());
    let app = launcher.apps().await.unwrap().remove(0);
    launcher.launch(&voice(&app), &Allow).await.unwrap();
    assert_eq!(
        *driver.requests.lock().unwrap(),
        vec![Request::App(fs::canonicalize(path).unwrap())]
    );
}

#[tokio::test]
async fn mac_launch_rejects_forged_command_and_name_without_dispatch() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), "Fixture.app", "");
    let driver = Arc::new(FixtureDriver::new(Outcome::Accepted));
    let launcher = launcher(root.path(), driver.clone());
    let app = launcher.apps().await.unwrap().remove(0);
    for forged in [
        VoiceApp {
            id: "sh -c forbidden".into(),
            name: app.name.clone(),
        },
        VoiceApp {
            id: app.id,
            name: "Changed".into(),
        },
    ] {
        assert!(matches!(
            launcher.launch(&forged, &Allow).await,
            Err(Error::StaleTarget)
        ));
    }
    assert!(driver.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mac_launch_stale_plist_executable_and_moved_bundle_are_refused_then_refresh_recovers() {
    for change in ["plist", "executable", "move"] {
        let root = tempfile::tempdir().unwrap();
        let path = fixture(root.path(), "Fixture.app", "");
        let driver = Arc::new(FixtureDriver::new(Outcome::Accepted));
        let launcher = launcher(root.path(), driver.clone());
        let old = launcher.apps().await.unwrap().remove(0);
        match change {
            "plist" => {
                let plist = path.join("Contents/Info.plist");
                let mut bytes = fs::read(&plist).unwrap();
                bytes.extend(b"category=changed\n");
                fs::write(plist, bytes).unwrap();
            }
            "executable" => {
                let executable = path.join("Contents/MacOS/fixture");
                let replacement = path.join("Contents/MacOS/replacement");
                fs::copy(&executable, &replacement).unwrap();
                fs::rename(replacement, executable).unwrap();
            }
            _ => {
                fs::rename(&path, root.path().join("Moved.app")).unwrap();
            }
        }
        assert!(
            matches!(
                launcher.launch(&voice(&old), &Allow).await,
                Err(Error::StaleTarget)
            ),
            "{change}"
        );
        assert!(driver.requests.lock().unwrap().is_empty());
        let fresh = launcher.apps().await.unwrap().remove(0);
        assert_ne!(old.id, fresh.id, "{change}");
        launcher.launch(&voice(&fresh), &Allow).await.unwrap();
    }
}

#[tokio::test]
async fn mac_launch_authority_is_checked_after_preparation_and_blocks_dispatch() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), "Fixture.app", "");
    let revoked = Arc::new(AtomicBool::new(false));
    let mut driver = FixtureDriver::new(Outcome::Accepted);
    driver.revoked = Some(revoked.clone());
    let driver = Arc::new(driver);
    let launcher = launcher(root.path(), driver.clone());
    let app = launcher.apps().await.unwrap().remove(0);
    assert!(matches!(
        launcher
            .launch(&voice(&app), &Revocable(revoked.clone()))
            .await,
        Err(Error::Stopped)
    ));
    revoked.store(false, Ordering::SeqCst);
    assert!(matches!(
        launcher
            .open_url("https://example.invalid/", &Revocable(revoked))
            .await,
        Err(Error::Stopped)
    ));
    assert_eq!(driver.prepared.load(Ordering::SeqCst), 2);
    assert!(driver.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mac_launch_urls_are_literal_http_https_and_unsafe_urls_never_prepare() {
    let root = tempfile::tempdir().unwrap();
    let driver = Arc::new(FixtureDriver::new(Outcome::Accepted));
    let launcher = launcher(root.path(), driver.clone());
    for url in [
        "https://example.invalid/a?b=c#d",
        "HTTP://localhost:8080/",
        "https://[::1]:443/a%20b",
    ] {
        launcher.open_url(url, &Allow).await.unwrap();
    }
    assert_eq!(driver.requests.lock().unwrap().len(), 3);
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "-a Calculator",
        "https:///no-host",
        "https://",
        "https://user:secret@example.invalid",
        "https://example.invalid/\n",
        "https://example.invalid/%0a",
        "https://example.invalid/%00",
        "https://example.invalid/\\evil",
        "https://example.invalid/%5c",
        "https://example.invalid/%xx",
        "https://example.invalid:abc",
        "https://example.invalid:0",
        "https://[bad]/",
    ] {
        assert!(
            matches!(
                launcher.open_url(url, &Allow).await,
                Err(Error::StaleTarget)
            ),
            "{url:?}"
        );
    }
    assert_eq!(driver.prepared.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn mac_launch_bad_bundles_are_not_exposed_as_apps() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), "NoPackage.app", "package=BNDL\n");
    fixture(root.path(), "Traversal.app", "exe=../fixture\n");
    fixture(root.path(), "Absolute.app", "exe=/bin/sh\n");
    let missing = fixture(root.path(), "Missing.app", "");
    fs::remove_file(missing.join("Contents/MacOS/fixture")).unwrap();
    let huge = fixture(root.path(), "Huge.app", "");
    fs::write(
        huge.join("Contents/Info.plist"),
        vec![b'x'; MAX_PLIST as usize + 1],
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let external = tempfile::tempdir().unwrap();
        let target = fixture(external.path(), "Outside.app", "");
        symlink(target, root.path().join("Alias.app")).unwrap();
        let escape = fixture(root.path(), "Escape.app", "");
        fs::remove_file(escape.join("Contents/MacOS/fixture")).unwrap();
        symlink("/bin/sh", escape.join("Contents/MacOS/fixture")).unwrap();
        let noexec = fixture(root.path(), "NoExec.app", "");
        fs::set_permissions(
            noexec.join("Contents/MacOS/fixture"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let driver = Arc::new(FixtureDriver::new(Outcome::Accepted));
    assert!(
        launcher(root.path(), driver.clone())
            .apps()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(driver.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mac_launch_unreadable_roots_report_failure_without_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let driver = Arc::new(FixtureDriver::new(Outcome::Accepted));
    let launcher = launcher(&root.path().join("absent"), driver.clone());
    assert!(matches!(launcher.apps().await, Err(Error::Unavailable(_))));
    assert!(driver.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mac_launch_slow_catalog_times_out_without_later_dispatch() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), "Fixture.app", "");
    let mut driver = FixtureDriver::new(Outcome::Accepted);
    driver.delay = CATALOG + Duration::from_millis(100);
    let driver = Arc::new(driver);
    let launcher = launcher(root.path(), driver.clone());
    let start = Instant::now();
    assert!(matches!(
        launcher
            .launch(
                &VoiceApp {
                    id: "unused".into(),
                    name: "unused".into()
                },
                &Allow
            )
            .await,
        Err(Error::Unavailable(_))
    ));
    assert!(start.elapsed() < voice_control::LAUNCH_LIMIT);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(driver.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mac_launch_spawn_failure_and_native_rejection_are_visible_and_retryable() {
    for outcome in [Outcome::SpawnFailure, Outcome::Rejected] {
        let root = tempfile::tempdir().unwrap();
        let driver = Arc::new(FixtureDriver::new(outcome));
        let launcher = launcher(root.path(), driver);
        assert!(matches!(
            launcher.open_url("https://example.invalid/", &Allow).await,
            Err(Error::Unavailable(_))
        ));
    }
    let root = tempfile::tempdir().unwrap();
    let driver = Arc::new(FixtureDriver::new(Outcome::Accepted));
    launcher(root.path(), driver)
        .open_url("https://example.invalid/", &Allow)
        .await
        .unwrap();
}

#[tokio::test]
async fn mac_launch_killed_helper_still_reports_uncertain_queued_native_request() {
    for outcome in [Outcome::Hung, Outcome::Unkillable, Outcome::WaitFailure] {
        let root = tempfile::tempdir().unwrap();
        let driver = Arc::new(FixtureDriver::new(outcome));
        let launcher = launcher(root.path(), driver.clone());
        let start = Instant::now();
        assert!(
            matches!(
                launcher.open_url("https://example.invalid/", &Allow).await,
                Err(Error::LaunchUnstopped)
            ),
            "{outcome:?}"
        );
        assert!(driver.killed.load(Ordering::SeqCst));
        assert!(
            start.elapsed() < voice_control::LAUNCH_AFTER_CHECK,
            "{outcome:?}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn mac_launch_native_plist_parser_reads_xml_and_rejects_non_dictionary() {
    let bytes = br#"<?xml version="1.0"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>test.fixture</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleExecutable</key><string>fixture</string>
<key>CFBundleDisplayName</key><string>Fixture &amp; Test</string>
<key>LSUIElement</key><true/>
<key>CFBundleURLTypes</key><array><dict><key>CFBundleURLSchemes</key><array><string>https</string></array></dict></array>
<key>CFBundleDocumentTypes</key><array><dict><key>LSItemContentTypes</key><array><string>public.text</string></array></dict></array>
</dict></plist>"#;
    let info = native::info(bytes).unwrap();
    assert_eq!(info.identifier, "test.fixture");
    assert_eq!(info.display_name, "Fixture & Test");
    assert!(info.background);
    assert_eq!(info.schemes, ["https"]);
    assert_eq!(info.content_types, ["public.text"]);
    assert!(native::info(b"not a plist").is_none());
    assert!(native::info(br#"<plist version="1.0"><array/></plist>"#).is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn mac_launch_native_plist_parser_reads_binary() {
    let bytes = b"bplist00\xd5\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x5f\x10\x12CFBundleExecutable\x5f\x10\x12CFBundleIdentifier\x5cCFBundleName\x5f\x10\x13CFBundlePackageType\x5f\x10\x10LSBackgroundOnlyWfixture[test.binary\x5eBinary FixtureTAPPL\x09\x08\x13\x28\x3d\x4a\x60\x73\x7b\x87\x96\x9b\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\x00\x00\x00\x00\x0b\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x9c";
    let info = native::info(bytes).unwrap();
    assert_eq!(info.identifier, "test.binary");
    assert_eq!(info.name, "Binary Fixture");
    assert_eq!(info.executable, "fixture");
    assert_eq!(info.package, "APPL");
    assert!(info.background);
}

#[cfg(target_os = "macos")]
#[test]
fn mac_launch_native_plist_parser_absent_fields_and_wrong_types_are_safe() {
    for bytes in [
        br#"<plist version="1.0"><dict/></plist>"#.as_slice(),
        br#"<plist version="1.0"><dict>
<key>CFBundleName</key><array/>
<key>LSUIElement</key><dict/>
<key>CFBundleURLTypes</key><array><string>not a dictionary</string><dict/></array>
<key>CFBundleDocumentTypes</key><string>not an array</string>
</dict></plist>"#
            .as_slice(),
    ] {
        let info = native::info(bytes).expect("a valid dictionary may omit app fields");
        assert!(info.identifier.is_empty());
        assert!(info.name.is_empty());
        assert!(info.display_name.is_empty());
        assert!(info.executable.is_empty());
        assert!(info.package.is_empty());
        assert!(info.category.is_empty());
        assert!(info.schemes.is_empty());
        assert!(info.content_types.is_empty());
        assert!(!info.background);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn mac_launch_native_plist_parser_malformed_inputs_return_none_without_crash() {
    for bytes in [
        b"".as_slice(),
        b"not a property list".as_slice(),
        b"bplist00".as_slice(),
        br#"<plist version="1.0"><dict><key>unterminated"#.as_slice(),
        br#"<plist version="1.0"><dict><key>missing value</key></dict></plist>"#.as_slice(),
        br#"<plist version="1.0"><array><string>not a dictionary</string></array></plist>"#
            .as_slice(),
    ] {
        assert!(
            native::info(bytes).is_none(),
            "unexpected successful parse: {bytes:?}"
        );
    }
}

/// Explicitly opt-in: opens only a disposable app created by this test. It does
/// not activate an installed app, use accounts, inspect clipboard contents, or
/// request Accessibility permission. Run only in an approved native test slot.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires OPHIO_MAC_LAUNCH_FIXTURE_ROOT and explicit permission to launch a disposable native app"]
async fn mac_launch_native_disposable_bundle_delivers_marker() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::var_os("OPHIO_MAC_LAUNCH_FIXTURE_ROOT")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .expect("set an approved absolute temporary fixture root");
    let temp = tempfile::tempdir_in(root).unwrap();
    let bundle = temp.path().join("Fixture.app");
    fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    let marker = temp.path().join("accepted");
    let escaped = marker.to_str().unwrap().replace('\'', "'\\''");
    let executable = bundle.join("Contents/MacOS/fixture");
    fs::write(&executable, format!("#!/bin/sh\n: > '{escaped}'\n")).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        bundle.join("Contents/Info.plist"),
        br#"<?xml version="1.0"?><plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>org.example.launch-fixture</string>
<key>CFBundleName</key><string>Disposable Launch Fixture</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleExecutable</key><string>fixture</string>
</dict></plist>"#,
    )
    .unwrap();
    let launcher = MacLauncher {
        roots: vec![temp.path().into()],
        home: None,
        driver: Arc::new(SystemDriver),
    };
    let apps = launcher.apps().await.unwrap();
    assert_eq!(apps.len(), 1);
    // A denied launch must not start even this fixture.
    assert!(matches!(
        launcher
            .launch(
                &voice(&apps[0]),
                &Revocable(Arc::new(AtomicBool::new(true)))
            )
            .await,
        Err(Error::Stopped)
    ));
    assert!(!marker.exists());
    launcher.launch(&voice(&apps[0]), &Allow).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Launch Services did not execute the disposable fixture");
}
