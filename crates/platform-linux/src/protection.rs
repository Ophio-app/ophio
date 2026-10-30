// SPDX-License-Identifier: Apache-2.0
//! Persistent local control notification on an explicitly supplied session bus.
use async_trait::async_trait;
use futures_util::StreamExt;
use ports::desktop::{IndicatorSession, LocalIndicator, PlatformError};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};
use tokio::sync::Mutex;
use zbus::{Connection, Proxy, zvariant::Value};

pub async fn connect(connection: Connection) -> Result<IndicatorSession, PlatformError> {
    let proxy = proxy(&connection).await?;
    let capabilities: Vec<String> = proxy.call("GetCapabilities", &()).await.map_err(failure)?;
    // Persistence is not required: the notification never expires, and a
    // server that drops or dismisses it reports NotificationClosed, which stops
    // control. Common Wayland servers such as mako do not advertise it.
    if !capabilities.iter().any(|s| s == "actions") {
        return Err(PlatformError::Unsupported(
            "The desktop notification service must support notification actions".into(),
        ));
    }
    let markup = capabilities.iter().any(|s| s == "body-markup");
    let mut actions = proxy
        .receive_signal("ActionInvoked")
        .await
        .map_err(failure)?;
    let mut closed = proxy
        .receive_signal("NotificationClosed")
        .await
        .map_err(failure)?;
    let bus = Proxy::new_owned(
        connection.clone(),
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await
    .map_err(failure)?;
    let mut owners = bus
        .receive_signal("NameOwnerChanged")
        .await
        .map_err(failure)?;
    let operation = Arc::new(Mutex::new(()));
    let events_operation = operation.clone();
    let id = Arc::new(AtomicU32::new(0));
    let current = id.clone();
    let (stop, receiver) = tokio::sync::mpsc::channel(4);
    // Stop clicks coalesce. Waiting for queue space while holding the indicator
    // mutex would prevent the Stop consumer from hiding that same indicator.
    let watcher = tokio::spawn(async move {
        loop {
            tokio::select! {
                event = owners.next() => {
                    let Some(event) = event else { let _ = stop.try_send(()); break };
                    if let Ok((name, _, _)) = event.body().deserialize::<(String, String, String)>()
                        && name == "org.freedesktop.Notifications" {
                        let _ = stop.try_send(()); break;
                    }
                }
                event = actions.next() => {
                    let Some(event) = event else { let _ = stop.try_send(()); break };
                    let _operation = events_operation.lock().await;
                    if let Ok((id, action)) = event.body().deserialize::<(u32, String)>()
                        && id == current.load(Ordering::SeqCst) && (action == "stop" || action == "default") {
                        let _ = stop.try_send(());
                    }
                }
                event = closed.next() => {
                    let Some(event) = event else { let _ = stop.try_send(()); break };
                    let _operation = events_operation.lock().await;
                    if let Ok((id, _)) = event.body().deserialize::<(u32, u32)>()
                        && current.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                        let _ = stop.try_send(());
                    }
                }
            }
        }
    });
    Ok(IndicatorSession {
        indicator: Arc::new(Indicator {
            connection,
            markup,
            icon: icon(),
            id,
            operation,
            watcher,
        }),
        stop: receiver,
    })
}
/// Øphio's own icon once install.sh has put it in the icon theme; until then
/// a keyboard, which every theme has.
fn icon() -> &'static str {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    let shared = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let shared = if shared.is_empty() {
        "/usr/local/share:/usr/share"
    } else {
        &shared
    };
    let installed = home
        .into_iter()
        .chain(shared.split(':').map(PathBuf::from))
        .filter(|dir| dir.is_absolute())
        .any(|dir| dir.join("icons/hicolor/scalable/apps/ophio.svg").is_file());
    if installed { "ophio" } else { "input-keyboard" }
}
struct Indicator {
    connection: Connection,
    markup: bool,
    icon: &'static str,
    id: Arc<AtomicU32>,
    operation: Arc<Mutex<()>>,
    watcher: tokio::task::JoinHandle<()>,
}
impl Drop for Indicator {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}
async fn proxy(connection: &Connection) -> Result<Proxy<'static>, PlatformError> {
    Proxy::new_owned(
        connection.clone(),
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    .map_err(failure)
}
fn failure(_: impl std::fmt::Display) -> PlatformError {
    PlatformError::Native("Local control notification is unavailable".into())
}
#[async_trait]
impl LocalIndicator for Indicator {
    async fn show(&self, holders: &str) -> Result<(), PlatformError> {
        let _operation = self.operation.lock().await;
        if self.watcher.is_finished() {
            return Err(failure("notification service ended"));
        }
        let proxy = proxy(&self.connection).await?;
        let hints: HashMap<&str, Value<'_>> = HashMap::from([
            ("resident", Value::Bool(true)),
            ("transient", Value::Bool(false)),
            ("urgency", Value::U8(2)),
        ]);
        // Servers without buttons (mako) invoke "default" on click; dismissing
        // the notification also stops control.
        let actions = [
            "default",
            "Stop all computer control",
            "stop",
            "Stop all computer control",
        ];
        let holders = if self.markup {
            escape(holders)
        } else {
            holders.to_owned()
        };
        let body = format!(
            "{holders}\nClick or dismiss to stop phone input and computer-use tasks. Earlier effects remain."
        );
        let id: u32 = proxy
            .call(
                "Notify",
                &(
                    ports::PRODUCT_NAME,
                    self.id.load(Ordering::SeqCst),
                    self.icon,
                    "Computer control is active",
                    body,
                    actions.as_slice(),
                    hints,
                    0_i32,
                ),
            )
            .await
            .map_err(failure)?;
        if id == 0 {
            return Err(failure("invalid notification id"));
        }
        self.id.store(id, Ordering::SeqCst);
        Ok(())
    }
    async fn hide(&self) -> Result<(), PlatformError> {
        let _operation = self.operation.lock().await;
        let id = self.id.swap(0, Ordering::SeqCst);
        if id != 0 {
            proxy(&self.connection)
                .await?
                .call::<_, _, ()>("CloseNotification", &(id,))
                .await
                .map_err(failure)?;
        }
        Ok(())
    }
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
