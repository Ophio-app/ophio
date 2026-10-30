// SPDX-License-Identifier: Apache-2.0
//! A private session bus carrying just enough of the desktop portal to reach
//! its sharing dialog, which it shows and never answers.
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
};
use tokio::sync::mpsc::UnboundedSender;
use zbus::{
    message::Header,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

pub(crate) struct Bus {
    child: Child,
    _dir: tempfile::TempDir,
    pub(crate) address: String,
}
impl Bus {
    pub(crate) fn start() -> Self {
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
        Self {
            child,
            _dir: dir,
            address: address.trim().to_owned(),
        }
    }
    pub(crate) async fn connect(&self) -> zbus::Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What the portal was asked to do.
#[derive(Debug, PartialEq)]
pub(crate) enum Seen {
    Dialog,
    Closed(String),
}

/// Serves the portal until the returned connection is dropped.
pub(crate) async fn serve(bus: &Bus, seen: UnboundedSender<Seen>) -> zbus::Connection {
    const DESKTOP: &str = "/org/freedesktop/portal/desktop";
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at(DESKTOP, Remote(seen))
        .unwrap()
        .serve_at(DESKTOP, Cast)
        .unwrap()
        .serve_at(DESKTOP, Clipboard)
        .unwrap()
        .build()
        .await
        .unwrap()
}

fn handle(
    header: &Header<'_>,
    kind: &str,
    options: &HashMap<String, OwnedValue>,
    key: &str,
) -> OwnedObjectPath {
    let sender = header
        .sender()
        .unwrap()
        .trim_start_matches(':')
        .replace('.', "_");
    let token = options[key].downcast_ref::<&str>().unwrap();
    OwnedObjectPath::try_from(format!(
        "/org/freedesktop/portal/desktop/{kind}/{sender}/{token}"
    ))
    .unwrap()
}

/// Answers a request at once, as a portal does when no dialog is involved.
async fn answer(
    connection: &zbus::Connection,
    header: &Header<'_>,
    options: &HashMap<String, OwnedValue>,
    results: HashMap<&str, Value<'_>>,
) -> zbus::fdo::Result<OwnedObjectPath> {
    let request = handle(header, "request", options, "handle_token");
    connection
        .emit_signal(
            None::<zbus::names::BusName<'_>>,
            &request,
            "org.freedesktop.portal.Request",
            "Response",
            &(0u32, results),
        )
        .await?;
    Ok(request)
}

struct Remote(UnboundedSender<Seen>);
#[zbus::interface(name = "org.freedesktop.portal.RemoteDesktop")]
impl Remote {
    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let session = handle(&header, "session", &options, "session_handle_token");
        connection
            .object_server()
            .at(&session, Session(session.to_string(), self.0.clone()))
            .await?;
        let results = HashMap::from([("session_handle", Value::from(session.as_str()))]);
        answer(connection, &header, &options, results).await
    }
    async fn select_devices(
        &self,
        _session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        answer(connection, &header, &options, HashMap::new()).await
    }
    fn start(
        &self,
        _session: OwnedObjectPath,
        _parent: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
    ) -> OwnedObjectPath {
        let _ = self.0.send(Seen::Dialog);
        handle(&header, "request", &options, "handle_token")
    }
    #[zbus(property)]
    fn version(&self) -> u32 {
        2
    }
}

struct Cast;
#[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
impl Cast {
    async fn select_sources(
        &self,
        _session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        answer(connection, &header, &options, HashMap::new()).await
    }
    #[zbus(property)]
    fn version(&self) -> u32 {
        4
    }
}

struct Clipboard;
#[zbus::interface(name = "org.freedesktop.portal.Clipboard")]
impl Clipboard {
    fn request_clipboard(&self, _session: OwnedObjectPath, _options: HashMap<String, OwnedValue>) {}
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }
}

struct Session(String, UnboundedSender<Seen>);
#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl Session {
    fn close(&self) {
        let _ = self.1.send(Seen::Closed(self.0.clone()));
    }
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }
}
