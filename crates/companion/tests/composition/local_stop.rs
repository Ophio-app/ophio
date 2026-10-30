// SPDX-License-Identifier: Apache-2.0
//! The notification service may start after the companion or restart under it.
use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

struct Bus(std::process::Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Notifications;
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    // Matches mako: actions without persistence.
    fn get_capabilities(&self) -> Vec<&str> {
        vec!["body", "body-markup", "actions", "icon-static"]
    }
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app: &str,
        _replaces: u32,
        _icon: &str,
        _summary: &str,
        _body: &str,
        _actions: Vec<String>,
        _hints: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
        _expire: i32,
    ) -> u32 {
        7
    }
    fn close_notification(&self, _id: u32) {}
}
async fn serve(address: &str) -> zbus::Connection {
    zbus::connection::Builder::address(address)
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at("/org/freedesktop/Notifications", Notifications)
        .unwrap()
        .build()
        .await
        .unwrap()
}
async fn input_available(rig: &Rig, want: bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let input = rig.runtime.hub.capabilities().await.input;
            if matches!(input, Availability::Available) == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("input availability never became {want}"));
}

#[tokio::test]
async fn local_stop_connects_late_and_reconnects_after_service_restart() {
    let dir = tempfile::tempdir().unwrap();
    // No service directories: a desktop's notification daemon must not be
    // auto-activated between our synthetic service releasing/reclaiming its name.
    let config_path = dir.path().join("bus.conf");
    std::fs::write(&config_path, r#"<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth><policy context="default"><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/></policy></busconfig>"#).unwrap();
    let mut child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config_path.display()))
        .args(["--nofork", "--print-address=1"])
        .arg(format!(
            "--address=unix:path={}",
            dir.path().join("bus").display()
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut address = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let _bus = Bus(child);
    let address = address.trim().to_owned();
    let config = Config {
        desktop_enabled: true,
        ..Config::default()
    };
    let rig = Rig::build(
        false,
        config,
        Arc::new(companion::frontdoors::LocalSpeech),
        Some(address.clone()),
    )
    .await;
    // No notification service yet: control stays closed and says why.
    let Availability::TemporarilyUnavailable { reason } =
        rig.runtime.hub.capabilities().await.input
    else {
        panic!("input must wait for the local Stop indicator")
    };
    assert!(reason.contains("notification service"), "{reason}");
    // Let the first connection attempt fail so the retry path is exercised.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let server = serve(&address).await;
    input_available(&rig, true).await;
    // A restarted service is picked up again without restarting the companion.
    server
        .release_name("org.freedesktop.Notifications")
        .await
        .unwrap();
    input_available(&rig, false).await;
    let _server = serve(&address).await;
    input_available(&rig, true).await;
    rig.shutdown().await;
}
