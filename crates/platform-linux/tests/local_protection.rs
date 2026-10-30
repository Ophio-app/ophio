// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use zbus::zvariant::OwnedValue;

struct Bus(Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[derive(Default)]
struct State {
    bodies: Vec<String>,
    closed: Vec<u32>,
}
struct Notifications(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    fn get_capabilities(&self) -> Vec<&str> {
        vec!["actions", "body", "body-markup"]
    }
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app: &str,
        replaces: u32,
        _icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire: i32,
    ) -> u32 {
        assert_eq!(app, "Øphio");
        assert_eq!(summary, "Computer control is active");
        assert!(replaces == 0 || replaces == 7);
        assert_eq!(expire, 0);
        assert_eq!(
            actions,
            [
                "default",
                "Stop all computer control",
                "stop",
                "Stop all computer control"
            ]
        );
        assert!(bool::try_from(hints.get("resident").unwrap()).unwrap());
        self.0.lock().unwrap().bodies.push(body.into());
        7
    }
    fn close_notification(&self, id: u32) {
        self.0.lock().unwrap().closed.push(id);
    }
}

#[tokio::test]
async fn private_bus_indicator_names_holders_stops_on_action_and_service_loss() {
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
    let _bus = Bus(child);
    let state = Arc::new(Mutex::new(State::default()));
    let server = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at(
            "/org/freedesktop/Notifications",
            Notifications(state.clone()),
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .build()
        .await
        .unwrap();
    let mut indicator = platform_linux::protection::connect(client).await.unwrap();
    indicator
        .indicator
        .show("Phone · Pat’s phone")
        .await
        .unwrap();
    indicator
        .indicator
        .show("Agent · Review <page>")
        .await
        .unwrap();
    assert!(state.lock().unwrap().bodies[1].contains("Review &lt;page&gt;"));
    server
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "ActionInvoked",
            &(7_u32, "stop"),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), indicator.stop.recv())
        .await
        .unwrap()
        .unwrap();
    // Servers without buttons, such as mako, invoke "default" on click.
    server
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "ActionInvoked",
            &(7_u32, "default"),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), indicator.stop.recv())
        .await
        .unwrap()
        .unwrap();
    // A burst of local clicks must not hold the notification mutex while the
    // Stop consumer is waiting to hide it. A full queue already carries Stop.
    for _ in 0..32 {
        server
            .emit_signal(
                None::<&str>,
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "ActionInvoked",
                &(7_u32, "stop"),
            )
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while indicator.stop.len() < 4 {
            tokio::task::yield_now().await;
        }
        indicator.indicator.hide().await.unwrap();
    })
    .await
    .unwrap();
    while indicator.stop.try_recv().is_ok() {}
    assert_eq!(state.lock().unwrap().closed, [7]);
    indicator
        .indicator
        .show("Agent · Review page")
        .await
        .unwrap();
    server
        .release_name("org.freedesktop.Notifications")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), indicator.stop.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(indicator.indicator.show("Phone").await.is_err());
}
