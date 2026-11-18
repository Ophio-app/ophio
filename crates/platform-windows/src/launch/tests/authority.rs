// SPDX-License-Identifier: Apache-2.0
//! Synthetic native-worker handoff tests. No app, URL or native API is invoked.
use super::*;

#[derive(Clone, Copy, Debug)]
enum Route {
    Shortcut,
    Packaged,
    Url,
}

fn route_setup(route: Route) -> (Arc<Fake>, WindowsLauncher) {
    let (fake, launcher) = setup();
    if matches!(route, Route::Packaged) {
        fake.entries.lock().unwrap().clear();
        fake.packaged.lock().unwrap().push(PackagedApp {
            aumid: "Fixture.Editor_123456789abcd!App".into(),
            package: "Fixture.Editor_1.0.0.0_x64__123456789abcd".into(),
            name: "Fixture Editor".into(),
        });
    }
    (fake, launcher)
}

struct PausedAuthority {
    checks: AtomicUsize,
    pause_check: usize,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
    stopped: AtomicBool,
    expires: Mutex<Instant>,
}
impl Authority for PausedAuthority {
    fn check(&self) -> Result<(), Error> {
        if self.checks.fetch_add(1, Ordering::SeqCst) + 1 == self.pause_check {
            // All async caller checks and the delivery gate have completed. Hold
            // the native worker just before it observes live Stop/lease state.
            self.entered
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(())
                .unwrap();
            self.resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        if self.stopped.load(Ordering::SeqCst) || Instant::now() >= *self.expires.lock().unwrap() {
            Err(Error::Stopped)
        } else {
            Ok(())
        }
    }
}

async fn withdraw_after_gate(route: Route, expire: bool) {
    let (fake, launcher) = route_setup(route);
    let selected = selected(&launcher).await;
    let launcher = Arc::new(launcher);
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    let authority = Arc::new(PausedAuthority {
        checks: AtomicUsize::new(0),
        // App selection adds one check before fresh catalog membership lookup.
        pause_check: if matches!(route, Route::Url) { 3 } else { 4 },
        entered: Mutex::new(Some(entered_tx)),
        resume: Mutex::new(resume_rx),
        stopped: AtomicBool::new(false),
        expires: Mutex::new(Instant::now() + Duration::from_secs(60)),
    });
    let task = tokio::spawn({
        let authority = authority.clone();
        let launcher = launcher.clone();
        let selected = selected.clone();
        async move {
            match route {
                Route::Url => {
                    launcher
                        .open_url_authorized("https://example.test", authority)
                        .await
                }
                _ => launcher.launch_authorized(&selected, authority).await,
            }
        }
    });
    tokio::time::timeout(Duration::from_secs(1), entered_rx)
        .await
        .expect("worker must check authority after the gate")
        .unwrap();
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        0,
        "{route:?} dispatched before final check"
    );
    if expire {
        *authority.expires.lock().unwrap() = Instant::now() - Duration::from_secs(1);
    } else {
        authority.stopped.store(true, Ordering::SeqCst);
    }
    resume_tx.send(()).unwrap();
    assert!(
        matches!(task.await.unwrap(), Err(Error::Stopped)),
        "{route:?}"
    );
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        0,
        "{route:?} escaped withdrawal"
    );
    // A fresh grant recovers without carrying the withdrawn authority forward.
    match route {
        Route::Url => launcher
            .open_url_authorized("https://example.test", Arc::new(Allow))
            .await
            .unwrap(),
        _ => launcher
            .launch_authorized(&selected, Arc::new(Allow))
            .await
            .unwrap(),
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stop_after_delivery_gate_blocks_shortcut_packaged_and_url_dispatch() {
    for route in [Route::Shortcut, Route::Packaged, Route::Url] {
        withdraw_after_gate(route, false).await;
    }
}

#[tokio::test]
async fn lease_expiry_after_delivery_gate_blocks_shortcut_packaged_and_url_dispatch() {
    for route in [Route::Shortcut, Route::Packaged, Route::Url] {
        withdraw_after_gate(route, true).await;
    }
}

#[tokio::test]
async fn borrowed_windows_entries_fail_closed_without_any_dispatch() {
    for route in [Route::Shortcut, Route::Packaged] {
        let (fake, launcher) = route_setup(route);
        let selected = selected(&launcher).await;
        for result in [
            launcher.launch(&selected, &Allow).await,
            launcher.open_url("https://example.test", &Allow).await,
        ] {
            let Err(Error::Unavailable(reason)) = result else {
                panic!("borrowed authority must be refused");
            };
            assert!(reason.contains("owned execution authority"));
        }
        assert!(matches!(
            launcher.launch(&selected, &Stop).await,
            Err(Error::Stopped)
        ));
        assert!(matches!(
            launcher.open_url("https://example.test", &Stop).await,
            Err(Error::Stopped)
        ));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    }
}
