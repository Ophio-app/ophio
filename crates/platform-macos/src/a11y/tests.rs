// SPDX-License-Identifier: Apache-2.0
use super::*;
use protocol::voice::VoiceBounds;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn button() -> VoiceElement {
    VoiceElement {
        id: "native-control-42".into(),
        role: "button".into(),
        name: "Fixture".into(),
        bounds: VoiceBounds {
            x: -30.,
            y: 10.,
            width: 80.,
            height: 24.,
        },
        visible: true,
        enabled: true,
        focused: false,
        sensitive: false,
        destructive: false,
    }
}
struct State {
    element: VoiceElement,
    context: String,
    permission: bool,
    unlocked: bool,
    actions: usize,
    threads: Vec<std::thread::ThreadId>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            element: button(),
            context: "native-window-7".into(),
            permission: true,
            unlocked: true,
            actions: 0,
            threads: vec![],
        }
    }
}
#[derive(Default)]
struct Gate {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}
struct Fixture {
    state: Arc<Mutex<State>>,
    gate: Arc<Gate>,
}
impl Backend for Fixture {
    fn frame(&mut self) -> Result<Frame, Error> {
        let mut s = self.state.lock().unwrap();
        s.threads.push(std::thread::current().id());
        if !s.permission {
            return Err(Error::Unavailable(PERMISSION.into()));
        }
        if !s.unlocked {
            return Err(Error::Unavailable(SESSION.into()));
        }
        Ok(Frame {
            context: s.context.clone(),
            elements: vec![s.element.clone()],
            window: Some(s.element.bounds),
        })
    }
    fn resolve(&mut self, expected: &VoiceElement, context: &str) -> Result<(), Error> {
        if let Some(entered) = self.gate.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        if let Some(release) = self.gate.release.lock().unwrap().take() {
            release.recv().unwrap();
        }
        let mut s = self.state.lock().unwrap();
        s.threads.push(std::thread::current().id());
        if !s.permission {
            return Err(Error::Unavailable(PERMISSION.into()));
        }
        if !s.unlocked {
            return Err(Error::Unavailable(SESSION.into()));
        }
        if s.element != *expected || s.context != context {
            return Err(Error::StaleTarget);
        }
        Ok(())
    }
    fn perform(&mut self, _: &str, _: bool, authority: &dyn Authority) -> Result<bool, Error> {
        let mut s = self.state.lock().unwrap();
        s.threads.push(std::thread::current().id());
        authority.check()?;
        s.actions += 1;
        Ok(true)
    }
}
#[derive(Default)]
struct Lease {
    stopped: AtomicBool,
    checks: AtomicUsize,
}
impl Authority for Lease {
    fn check(&self) -> Result<(), Error> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        if self.stopped.load(Ordering::SeqCst) {
            Err(Error::Stopped)
        } else {
            Ok(())
        }
    }
}
fn fixture() -> (Arc<Ax>, Arc<Mutex<State>>, Arc<Gate>) {
    let state = Arc::new(Mutex::new(State::default()));
    let gate = Arc::new(Gate::default());
    let s = state.clone();
    let g = gate.clone();
    (
        Arc::new(Ax::start(move || {
            Ok(Box::new(Fixture { state: s, gate: g }))
        })),
        state,
        gate,
    )
}
async fn target(ax: &Ax) -> VoiceElement {
    ax.snapshot().await.unwrap().elements.remove(0)
}

#[tokio::test]
async fn synthetic_identity_stays_stable_and_native_dispatch_is_serial_authorized() {
    let (ax, state, _) = fixture();
    let element = target(&ax).await;
    assert_eq!(element, target(&ax).await);
    assert!(!ax.activate(&element, false).await.unwrap());
    assert_eq!(state.lock().unwrap().actions, 0);
    let authority = Arc::new(Lease::default());
    assert!(
        ax.activate_authorized(&element, false, authority.clone())
            .await
            .unwrap()
    );
    assert!(
        ax.activate_authorized(&element, true, authority.clone())
            .await
            .unwrap()
    );
    let s = state.lock().unwrap();
    assert_eq!(s.actions, 2);
    assert!(s.threads.iter().all(|thread| *thread == s.threads[0]));
    assert_ne!(s.threads[0], std::thread::current().id());
    assert_eq!(authority.checks.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn synthetic_replaced_moved_relabelled_and_refocused_controls_are_stale() {
    for change in [
        "identity",
        "position",
        "size",
        "label",
        "role",
        "focus",
        "sensitive",
        "destructive",
        "hidden",
        "disabled",
        "window",
    ] {
        let (ax, state, _) = fixture();
        let element = target(&ax).await;
        {
            let mut s = state.lock().unwrap();
            match change {
                // Same tree position, label and bounds; a different native object.
                "identity" => s.element.id = "native-control-43".into(),
                "position" => s.element.bounds.x += 1.,
                "size" => s.element.bounds.width += 1.,
                "label" => s.element.name = "Different".into(),
                "role" => s.element.role = "textbox".into(),
                "focus" => s.element.focused = true,
                "sensitive" => s.element.sensitive = true,
                "destructive" => s.element.destructive = true,
                "hidden" => s.element.visible = false,
                "disabled" => s.element.enabled = false,
                "window" => s.context = "native-window-8".into(),
                _ => unreachable!(),
            }
        }
        assert!(
            matches!(
                ax.activate_authorized(&element, false, Arc::new(Lease::default()))
                    .await,
                Err(Error::StaleTarget)
            ),
            "{change}"
        );
        assert_eq!(state.lock().unwrap().actions, 0);
    }
}

#[tokio::test]
async fn synthetic_permission_missing_revoked_and_recovered_and_lock_fail_closed() {
    let (ax, state, _) = fixture();
    state.lock().unwrap().permission = false;
    assert!(matches!(ax.snapshot().await, Err(Error::Unavailable(reason)) if reason == PERMISSION));
    state.lock().unwrap().permission = true;
    let old = target(&ax).await;
    state.lock().unwrap().permission = false;
    assert!(
        matches!(ax.activate_authorized(&old, false, Arc::new(Lease::default())).await, Err(Error::Unavailable(reason)) if reason == PERMISSION)
    );
    state.lock().unwrap().permission = true;
    // Permission recovery alone does not resurrect a previously observed target.
    assert!(matches!(
        ax.activate_authorized(&old, false, Arc::new(Lease::default()))
            .await,
        Err(Error::StaleTarget)
    ));
    let recovered = target(&ax).await;
    assert!(
        ax.activate_authorized(&recovered, false, Arc::new(Lease::default()))
            .await
            .unwrap()
    );
    state.lock().unwrap().unlocked = false;
    assert!(matches!(ax.snapshot().await, Err(Error::Unavailable(reason)) if reason == SESSION));
    state.lock().unwrap().unlocked = true;
    assert!(matches!(
        ax.activate_authorized(&recovered, false, Arc::new(Lease::default()))
            .await,
        Err(Error::StaleTarget)
    ));
    assert_eq!(state.lock().unwrap().actions, 1);
}

#[tokio::test]
async fn synthetic_stop_during_delayed_native_resolution_never_dispatches() {
    let (ax, state, gate) = fixture();
    let element = target(&ax).await;
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, released) = mpsc::channel();
    *gate.entered.lock().unwrap() = Some(entered);
    *gate.release.lock().unwrap() = Some(released);
    let lease = Arc::new(Lease::default());
    let lease_for_action = lease.clone();
    let action = tokio::spawn(async move {
        ax.activate_authorized(&element, false, lease_for_action)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap();
    lease.stopped.store(true, Ordering::SeqCst);
    release.send(()).unwrap();
    assert!(matches!(action.await.unwrap(), Err(Error::Stopped)));
    assert_eq!(state.lock().unwrap().actions, 0);
    assert_eq!(lease.checks.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn synthetic_permission_and_context_changes_during_resolution_never_dispatch() {
    for permission in [false, true] {
        let (ax, state, gate) = fixture();
        let element = target(&ax).await;
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::channel();
        *gate.entered.lock().unwrap() = Some(entered);
        *gate.release.lock().unwrap() = Some(released);
        let action = tokio::spawn(async move {
            ax.activate_authorized(&element, false, Arc::new(Lease::default()))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        {
            let mut s = state.lock().unwrap();
            if permission {
                s.permission = false;
            } else {
                s.context = "native-window-replacement".into();
            }
        }
        release.send(()).unwrap();
        assert!(action.await.unwrap().is_err());
        assert_eq!(state.lock().unwrap().actions, 0);
    }
}

#[test]
fn metadata_safety_labels_are_conservative() {
    assert!(private_label("Password"));
    assert!(private_label("Payment card"));
    assert!(!private_label("Search"));
    assert!(destructive_label("Delete file"));
    assert!(destructive_label("Send message"));
    assert!(!destructive_label("Open"));
}

#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn non_mac_default_never_reads_a_desktop() {
    assert!(
        matches!(Ax::default().snapshot().await, Err(Error::Unavailable(reason)) if reason == UNSUPPORTED)
    );
}
