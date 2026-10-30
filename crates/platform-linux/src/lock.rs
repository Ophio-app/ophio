// SPDX-License-Identifier: Apache-2.0
//! Observe only explicitly supplied session endpoints. No lock requests occur
//! here, and the only discovery is asking logind for the user's one graphical
//! session. Any affirmative lock wins over an unlocked source.
use crate::native::LockObserver;
use async_trait::async_trait;
use futures_util::StreamExt;
use protocol::control::DesktopLockState;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

#[derive(Clone, Debug)]
enum Endpoint {
    Logind {
        connection: zbus::Connection,
        path: OwnedObjectPath,
    },
    Saver {
        connection: zbus::Connection,
        name: &'static str,
        path: &'static str,
    },
    Hyprland {
        directory: PathBuf,
    },
}
impl Endpoint {
    async fn read(&self) -> Option<bool> {
        tokio::time::timeout(Duration::from_millis(300), async {
            match self {
                Self::Logind { connection, path } => {
                    let reply = connection
                        .call_method(
                            Some("org.freedesktop.login1"),
                            path,
                            Some("org.freedesktop.DBus.Properties"),
                            "Get",
                            &("org.freedesktop.login1.Session", "LockedHint"),
                        )
                        .await
                        .ok()?;
                    bool::try_from(reply.body().deserialize::<OwnedValue>().ok()?).ok()
                }
                Self::Saver {
                    connection,
                    name,
                    path,
                } => connection
                    .call_method(Some(*name), *path, Some(*name), "GetActive", &())
                    .await
                    .ok()?
                    .body()
                    .deserialize()
                    .ok(),
                Self::Hyprland { directory } => {
                    let mut socket =
                        tokio::net::UnixStream::connect(directory.join(".socket.sock"))
                            .await
                            .ok()?;
                    socket.write_all(b"j/locked").await.ok()?;
                    let mut bytes = Vec::new();
                    socket.take(4097).read_to_end(&mut bytes).await.ok()?;
                    if bytes.len() > 4096 {
                        return None;
                    }
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                        .ok()?
                        .get("locked")?
                        .as_bool()
                }
            }
        })
        .await
        .ok()
        .flatten()
    }
}

#[derive(Debug)]
struct Source {
    endpoint: Endpoint,
    state: watch::Sender<DesktopLockState>,
    serial: std::sync::Mutex<u64>,
    subscribed: std::sync::atomic::AtomicBool,
}
impl Source {
    fn update(&self, value: DesktopLockState) {
        let mut serial = self.serial.lock().unwrap_or_else(|e| e.into_inner());
        *serial = serial.wrapping_add(1);
        self.state.send_if_modified(|state| {
            if *state == value {
                false
            } else {
                *state = value;
                true
            }
        });
    }
    async fn read(&self) {
        let serial = *self.serial.lock().unwrap_or_else(|e| e.into_inner());
        let value = match self.endpoint.read().await {
            Some(true) => DesktopLockState::Locked,
            Some(false) => DesktopLockState::Unlocked,
            None => DesktopLockState::Unknown,
        };
        // A signal received during a round trip takes precedence over its reply.
        let mut current_serial = self.serial.lock().unwrap_or_else(|e| e.into_inner());
        if value == DesktopLockState::Locked || *current_serial == serial {
            *current_serial = current_serial.wrapping_add(1);
            self.state.send_if_modified(|state| {
                if *state == value {
                    false
                } else {
                    *state = value;
                    true
                }
            });
        }
    }
}

/// The user's graphical login session, as logind records it. A user service
/// the desktop started isn't told which session it belongs to, so this names
/// the one to lock and watch, but only when the user has exactly one live
/// graphical session of the supplied type (any, when that is empty). With
/// two, nothing ties this service's desktop to either, and locking the other
/// would leave this one open. None as well when logind doesn't answer promptly.
pub async fn display_session(
    system: &zbus::Connection,
    uid: u32,
    session_type: &str,
) -> Option<String> {
    let property = |path: OwnedObjectPath, interface: &'static str, name: &'static str| async move {
        system
            .call_method(
                Some("org.freedesktop.login1"),
                path.as_str(),
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(interface, name),
            )
            .await
            .ok()?
            .body()
            .deserialize::<OwnedValue>()
            .ok()
    };
    let ask = async {
        let user: OwnedObjectPath = system
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "GetUser",
                &(uid,),
            )
            .await
            .ok()?
            .body()
            .deserialize()
            .ok()?;
        let sessions: Vec<(String, OwnedObjectPath)> =
            property(user, "org.freedesktop.login1.User", "Sessions")
                .await?
                .try_into()
                .ok()?;
        let mut graphical = None;
        for (id, path) in sessions {
            let kind: String = property(path.clone(), "org.freedesktop.login1.Session", "Type")
                .await?
                .try_into()
                .ok()?;
            if !matches!(kind.as_str(), "wayland" | "x11" | "mir")
                || (!session_type.is_empty() && kind != session_type)
            {
                continue;
            }
            let state: String = property(path, "org.freedesktop.login1.Session", "State")
                .await?
                .try_into()
                .ok()?;
            if state != "closing" && graphical.replace(id).is_some() {
                return None;
            }
        }
        graphical.filter(|id: &String| !id.is_empty())
    };
    tokio::time::timeout(Duration::from_millis(300), ask)
        .await
        .ok()
        .flatten()
}

#[derive(Debug)]
pub struct SessionLockObserver {
    sources: Vec<Arc<Source>>,
    state: watch::Sender<DesktopLockState>,
    workers: Vec<tokio::task::JoinHandle<()>>,
}
impl SessionLockObserver {
    /// Missing or broken sources are never interpreted as an unlocked session.
    /// Hyprland requires its compositor source because hyprlock need not update
    /// logind's hint. D-Bus endpoints come from the selected login session.
    pub async fn connect(
        session: Option<zbus::Connection>,
        system: Option<zbus::Connection>,
        session_id: Option<&str>,
        hyprland: Option<PathBuf>,
    ) -> Self {
        let mut endpoints = Vec::new();
        if let (Some(connection), Some(id)) = (system, session_id) {
            let path = tokio::time::timeout(
                Duration::from_millis(300),
                connection.call_method(
                    Some("org.freedesktop.login1"),
                    "/org/freedesktop/login1",
                    Some("org.freedesktop.login1.Manager"),
                    "GetSession",
                    &(id,),
                ),
            )
            .await;
            if let Ok(Ok(reply)) = path
                && let Ok(path) = reply.body().deserialize()
            {
                endpoints.push(Endpoint::Logind { connection, path });
            }
        }
        if let Some(connection) = session {
            for (name, path) in [
                ("org.freedesktop.ScreenSaver", "/ScreenSaver"),
                ("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver"),
            ] {
                let endpoint = Endpoint::Saver {
                    connection: connection.clone(),
                    name,
                    path,
                };
                if endpoint.read().await.is_some() {
                    endpoints.push(endpoint);
                }
            }
        }
        if let Some(directory) = hyprland {
            endpoints.push(Endpoint::Hyprland { directory });
        }
        Self::start(endpoints).await
    }

    async fn start(endpoints: Vec<Endpoint>) -> Self {
        let sources: Vec<_> = endpoints
            .into_iter()
            .map(|endpoint| {
                Arc::new(Source {
                    endpoint,
                    state: watch::channel(DesktopLockState::Unknown).0,
                    serial: std::sync::Mutex::new(0),
                    subscribed: std::sync::atomic::AtomicBool::new(false),
                })
            })
            .collect();
        let state = watch::channel(DesktopLockState::Unknown).0;
        let mut workers = Vec::new();
        for source in &sources {
            let source = source.clone();
            let all = sources.clone();
            let notify = state.clone();
            workers.push(tokio::spawn(async move {
                // Subscribe before reading, closing the initial snapshot/signal gap.
                let endpoint = source.endpoint.clone();
                match endpoint {
                    Endpoint::Logind { connection, path } => {
                        let Ok(proxy) = zbus::Proxy::new(
                            &connection,
                            "org.freedesktop.login1",
                            path,
                            "org.freedesktop.DBus.Properties",
                        )
                        .await
                        else {
                            return;
                        };
                        let Ok(mut signals) = proxy.receive_signal("PropertiesChanged").await
                        else {
                            return;
                        };
                        source
                            .subscribed
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        source.read().await;
                        publish(&all, &notify);
                        while let Some(signal) = signals.next().await {
                            type Change = (
                                String,
                                std::collections::HashMap<String, OwnedValue>,
                                Vec<String>,
                            );
                            if let Ok((interface, changed, invalid)) =
                                signal.body().deserialize::<Change>()
                                && interface == "org.freedesktop.login1.Session"
                            {
                                if let Some(value) = changed
                                    .get("LockedHint")
                                    .and_then(|value| bool::try_from(value).ok())
                                {
                                    source.update(if value {
                                        DesktopLockState::Locked
                                    } else {
                                        DesktopLockState::Unlocked
                                    });
                                } else if invalid.iter().any(|name| name == "LockedHint") {
                                    source.update(DesktopLockState::Unknown);
                                    publish(&all, &notify);
                                    source.read().await;
                                }
                                publish(&all, &notify);
                            }
                        }
                    }
                    Endpoint::Saver {
                        connection, name, ..
                    } => {
                        // Heard from any of the service's objects, not only the
                        // one asked: hypridle answers at /ScreenSaver but
                        // signals only from /org/freedesktop/ScreenSaver, and
                        // an unlock missed there would read as locked until
                        // something asked again.
                        let Ok(rule) = zbus::MatchRule::builder()
                            .msg_type(zbus::message::Type::Signal)
                            .sender(name)
                            .and_then(|rule| rule.interface(name))
                            .and_then(|rule| rule.member("ActiveChanged"))
                            .map(|rule| rule.build())
                        else {
                            return;
                        };
                        let Ok(mut signals) =
                            zbus::MessageStream::for_match_rule(rule, &connection, None).await
                        else {
                            return;
                        };
                        source
                            .subscribed
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        source.read().await;
                        publish(&all, &notify);
                        while let Some(signal) = signals.next().await {
                            if let Ok(signal) = signal
                                && let Ok(active) = signal.body().deserialize::<bool>()
                            {
                                source.update(if active {
                                    DesktopLockState::Locked
                                } else {
                                    DesktopLockState::Unlocked
                                });
                                publish(&all, &notify);
                            }
                        }
                    }
                    Endpoint::Hyprland { directory } => {
                        // IPC has no documented session-lock event in the tested
                        // version. Any compositor event triggers a fresh query;
                        // a bounded timer and injection-time reads cover silence.
                        if let Ok(mut socket) =
                            tokio::net::UnixStream::connect(directory.join(".socket2.sock")).await
                        {
                            let mut buffer = [0; 4096];
                            loop {
                                match socket.read(&mut buffer).await {
                                    Ok(0) | Err(_) => break,
                                    Ok(_) => {
                                        source.read().await;
                                        publish(&all, &notify);
                                    }
                                }
                            }
                        }
                    }
                }
                source
                    .subscribed
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                source.update(DesktopLockState::Unknown);
                publish(&all, &notify);
            }));
        }
        let all = sources.clone();
        let notify = state.clone();
        workers.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                futures_util::future::join_all(
                    all.iter()
                        .filter(|source| {
                            !source.subscribed.load(std::sync::atomic::Ordering::SeqCst)
                        })
                        .map(|source| source.read()),
                )
                .await;
                publish(&all, &notify);
            }
        }));
        let observer = Self {
            sources,
            state,
            workers,
        };
        observer.observe().await;
        observer
    }
}
fn aggregate(states: impl IntoIterator<Item = DesktopLockState>) -> DesktopLockState {
    let mut any = false;
    let mut unknown = false;
    for state in states {
        any = true;
        match state {
            DesktopLockState::Locked => return DesktopLockState::Locked,
            DesktopLockState::Unknown => unknown = true,
            DesktopLockState::Unlocked => {}
        }
    }
    if !any || unknown {
        DesktopLockState::Unknown
    } else {
        DesktopLockState::Unlocked
    }
}
fn publish(sources: &[Arc<Source>], state: &watch::Sender<DesktopLockState>) {
    let next = aggregate(sources.iter().map(|source| *source.state.borrow()));
    state.send_if_modified(|current| {
        if *current == next {
            false
        } else {
            *current = next;
            true
        }
    });
}
#[async_trait]
impl LockObserver for SessionLockObserver {
    async fn observe(&self) -> DesktopLockState {
        futures_util::future::join_all(self.sources.iter().map(|source| source.read())).await;
        publish(&self.sources, &self.state);
        self.current()
    }
    fn current(&self) -> DesktopLockState {
        *self.state.borrow()
    }
    fn changes(&self) -> watch::Receiver<DesktopLockState> {
        self.state.subscribe()
    }
}
impl Drop for SessionLockObserver {
    fn drop(&mut self) {
        for worker in &self.workers {
            worker.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_lock_wins_and_missing_evidence_never_means_unlocked() {
        use DesktopLockState::*;
        assert_eq!(aggregate([]), Unknown);
        assert_eq!(aggregate([Unlocked, Unknown]), Unknown);
        assert_eq!(aggregate([Unlocked, Locked, Unknown]), Locked);
        assert_eq!(aggregate([Unknown, Locked]), Locked);
        assert_eq!(aggregate([Locked, Unknown]), Locked);
        assert_eq!(aggregate([Unknown, Unknown]), Unknown);
        assert_eq!(aggregate([Unlocked]), Unlocked);
        assert_eq!(aggregate([Unknown, Unlocked]), Unknown);
        assert_eq!(aggregate([Unlocked, Unlocked]), Unlocked);
    }
}

#[cfg(test)]
mod signals {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
        sync::atomic::{AtomicBool, Ordering},
    };
    struct Bus(Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    struct Saver(Arc<AtomicBool>);
    #[zbus::interface(name = "org.freedesktop.ScreenSaver")]
    impl Saver {
        fn get_active(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
    struct GnomeSaver(Arc<AtomicBool>);
    #[zbus::interface(name = "org.gnome.ScreenSaver")]
    impl GnomeSaver {
        fn get_active(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
    struct Login(Arc<AtomicBool>);
    #[zbus::interface(name = "org.freedesktop.login1.Session")]
    impl Login {
        #[zbus(property)]
        fn locked_hint(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
    /// A private bus for fake desktop services; it ends when dropped.
    fn private_bus() -> (tempfile::TempDir, Bus, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                dir.path().join("bus").display()
            ))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        (dir, Bus(child), address.trim().to_owned())
    }
    struct Manager;
    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl Manager {
        fn get_user(&self, uid: u32) -> zbus::fdo::Result<OwnedObjectPath> {
            if uid == 1000 {
                Ok(OwnedObjectPath::try_from("/org/freedesktop/login1/user/_1000").unwrap())
            } else {
                Err(zbus::fdo::Error::Failed("No such user".into()))
            }
        }
    }
    /// A login user and every session it holds.
    struct User(Vec<&'static str>);
    #[zbus::interface(name = "org.freedesktop.login1.User")]
    impl User {
        #[zbus(property)]
        fn sessions(&self) -> Vec<(String, OwnedObjectPath)> {
            self.0
                .iter()
                .map(|id| {
                    let path = format!("/org/freedesktop/login1/session/_3{id}");
                    ((*id).into(), OwnedObjectPath::try_from(path).unwrap())
                })
                .collect()
        }
    }
    /// A login session's kind and state, as logind reports them.
    struct Kind(&'static str, &'static str);
    #[zbus::interface(name = "org.freedesktop.login1.Session")]
    impl Kind {
        #[zbus(property, name = "Type")]
        fn kind(&self) -> String {
            self.0.into()
        }
        #[zbus(property)]
        fn state(&self) -> String {
            self.1.into()
        }
    }
    /// Serves a fake logind for user 1000 with the given sessions: id, type
    /// and state.
    async fn logind(
        address: &str,
        sessions: &[(&'static str, &'static str, &'static str)],
    ) -> zbus::Connection {
        let mut builder = zbus::connection::Builder::address(address)
            .unwrap()
            .name("org.freedesktop.login1")
            .unwrap()
            .serve_at("/org/freedesktop/login1", Manager)
            .unwrap()
            .serve_at(
                "/org/freedesktop/login1/user/_1000",
                User(sessions.iter().map(|(id, _, _)| *id).collect()),
            )
            .unwrap();
        for (id, kind, state) in sessions {
            builder = builder
                .serve_at(
                    format!("/org/freedesktop/login1/session/_3{id}"),
                    Kind(kind, state),
                )
                .unwrap();
        }
        builder.build().await.unwrap()
    }
    #[tokio::test]
    async fn logind_names_the_graphical_session_a_user_service_was_not_told() {
        for (sessions, supplied, expected) in [
            // The one graphical session, beside a terminal login.
            (
                vec![("c2", "wayland", "active"), ("5", "tty", "online")],
                "wayland",
                Some("c2"),
            ),
            (vec![("c2", "x11", "active")], "", Some("c2")),
            // One that is ending doesn't count.
            (
                vec![("c2", "wayland", "closing"), ("c3", "wayland", "active")],
                "wayland",
                Some("c3"),
            ),
            // Two desktops: nothing says which this service was given, so
            // neither is named, unless only one is of the supplied type.
            (
                vec![("c2", "wayland", "active"), ("c3", "wayland", "online")],
                "wayland",
                None,
            ),
            (
                vec![("c2", "wayland", "active"), ("c3", "x11", "online")],
                "",
                None,
            ),
            (
                vec![("c2", "wayland", "active"), ("c3", "x11", "online")],
                "x11",
                Some("c3"),
            ),
            // The only desktop isn't the kind this service was given.
            (vec![("c2", "x11", "active")], "wayland", None),
            (vec![("5", "tty", "active")], "", None),
            (vec![], "", None),
        ] {
            let (_dir, _bus, address) = private_bus();
            let _logind = logind(&address, &sessions).await;
            let system = zbus::connection::Builder::address(address.as_str())
                .unwrap()
                .build()
                .await
                .unwrap();
            assert_eq!(
                display_session(&system, 1000, supplied).await.as_deref(),
                expected,
                "{sessions:?} {supplied}"
            );
            // Another user's session is never named.
            assert_eq!(display_session(&system, 1001, supplied).await, None);
        }
        // Without logind on the bus there is no session to name.
        let (_dir, _bus, address) = private_bus();
        let system = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        assert_eq!(display_session(&system, 1000, "").await, None);
    }
    #[tokio::test]
    async fn dbus_signals_fence_without_waiting_for_a_poll() {
        let (_dir, _bus, address) = private_bus();
        let active = Arc::new(AtomicBool::new(false));
        let server = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name("org.freedesktop.ScreenSaver")
            .unwrap()
            .name("org.gnome.ScreenSaver")
            .unwrap()
            .name("org.freedesktop.login1")
            .unwrap()
            .serve_at("/ScreenSaver", Saver(active.clone()))
            .unwrap()
            .serve_at("/org/gnome/ScreenSaver", GnomeSaver(active.clone()))
            .unwrap()
            .serve_at("/session", Login(active.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        for endpoint in [
            Endpoint::Saver {
                connection: client.clone(),
                name: "org.freedesktop.ScreenSaver",
                path: "/ScreenSaver",
            },
            Endpoint::Saver {
                connection: client.clone(),
                name: "org.gnome.ScreenSaver",
                path: "/org/gnome/ScreenSaver",
            },
            Endpoint::Logind {
                connection: client.clone(),
                path: OwnedObjectPath::try_from("/session").unwrap(),
            },
        ] {
            let observer = SessionLockObserver::start(vec![endpoint.clone()]).await;
            let mut changed = observer.changes();
            // Wait for the watcher to have subscribed and read the initial state.
            tokio::time::timeout(Duration::from_secs(3), async {
                while !observer.sources[0].subscribed.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            // GetActive/LockedHint deliberately continues returning false.
            // A live signal subscription must never be periodically overwritten.
            match endpoint {
                Endpoint::Saver { name, path, .. } => server
                    .emit_signal(None::<&str>, path, name, "ActiveChanged", &true)
                    .await
                    .unwrap(),
                Endpoint::Logind { .. } => {
                    let values =
                        std::collections::HashMap::from([("LockedHint", OwnedValue::from(true))]);
                    server
                        .emit_signal(
                            None::<&str>,
                            "/session",
                            "org.freedesktop.DBus.Properties",
                            "PropertiesChanged",
                            &(
                                "org.freedesktop.login1.Session",
                                values,
                                Vec::<String>::new(),
                            ),
                        )
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                while *changed.borrow_and_update() != DesktopLockState::Locked {
                    changed.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(observer.current(), DesktopLockState::Locked);
            tokio::time::resume();
            let fence = crate::native::InputFence {
                cancelled: Arc::new(AtomicBool::new(false)),
                lock: Some(Arc::new(observer)),
            };
            assert!(matches!(
                fence.check(false),
                Err(ports::PlatformError::Locked)
            ));
            assert!(fence.check(true).is_ok());
        }
    }
    /// hypridle answers GetActive at /ScreenSaver but signals only from
    /// /org/freedesktop/ScreenSaver. Its unlock must still be heard without
    /// anyone asking, or a released lock screen reads as locked.
    #[tokio::test]
    async fn a_screensaver_is_heard_from_whichever_object_signals() {
        let (_dir, _bus, address) = private_bus();
        let active = Arc::new(AtomicBool::new(false));
        let server = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name("org.freedesktop.ScreenSaver")
            .unwrap()
            .serve_at("/ScreenSaver", Saver(active.clone()))
            .unwrap()
            .serve_at("/org/freedesktop/ScreenSaver", Saver(active.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let observer = SessionLockObserver::start(vec![Endpoint::Saver {
            connection: client,
            name: "org.freedesktop.ScreenSaver",
            path: "/ScreenSaver",
        }])
        .await;
        let mut changed = observer.changes();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !observer.sources[0].subscribed.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut signal = async |locked: bool| {
            active.store(locked, Ordering::SeqCst);
            server
                .emit_signal(
                    None::<&str>,
                    "/org/freedesktop/ScreenSaver",
                    "org.freedesktop.ScreenSaver",
                    "ActiveChanged",
                    &locked,
                )
                .await
                .unwrap();
            let wanted = if locked {
                DesktopLockState::Locked
            } else {
                DesktopLockState::Unlocked
            };
            tokio::time::timeout(
                Duration::from_secs(3),
                changed.wait_for(|lock| *lock == wanted),
            )
            .await
            .unwrap()
            .unwrap();
        };
        signal(true).await;
        // Another connection saying the same thing is no screensaver.
        let impostor = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        impostor
            .emit_signal(
                None::<&str>,
                "/org/freedesktop/ScreenSaver",
                "org.freedesktop.ScreenSaver",
                "ActiveChanged",
                &false,
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(observer.current(), DesktopLockState::Locked);
        signal(false).await;
        assert_eq!(observer.current(), DesktopLockState::Unlocked);
    }
    #[tokio::test]
    async fn hyprland_query_fences_a_lock_even_without_logind_or_an_event() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::UnixListener::bind(dir.path().join(".socket.sock")).unwrap();
        let locked = Arc::new(AtomicBool::new(false));
        let state = locked.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut query = [0; 8];
                socket.read_exact(&mut query).await.unwrap();
                assert_eq!(&query, b"j/locked");
                socket
                    .write_all(if state.load(Ordering::SeqCst) {
                        b"{\"locked\":true}"
                    } else {
                        b"{\"locked\":false}"
                    })
                    .await
                    .unwrap();
            }
        });
        let observer =
            SessionLockObserver::connect(None, None, None, Some(dir.path().into())).await;
        assert_eq!(observer.observe().await, DesktopLockState::Unlocked);
        locked.store(true, Ordering::SeqCst);
        assert_eq!(observer.observe().await, DesktopLockState::Locked);
        server.abort();
        assert_eq!(observer.observe().await, DesktopLockState::Unknown);
    }
}
