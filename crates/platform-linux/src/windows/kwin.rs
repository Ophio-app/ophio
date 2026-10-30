// SPDX-License-Identifier: Apache-2.0
//! Temporary KWin scripts; no persistent extension or desktop configuration changes.
use ports::PlatformError;
use std::{io::Write, sync::Mutex, time::Duration};
use tokio::sync::oneshot;
struct Reply {
    owner: String,
    tx: Mutex<Option<oneshot::Sender<String>>>,
}
#[zbus::interface(name = "app.ophio.WindowReply")]
impl Reply {
    fn publish(
        &self,
        body: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if header.sender().map(|s| s.as_str()) != Some(&self.owner) || body.len() > 4 * 1024 * 1024
        {
            return Err(zbus::fdo::Error::AccessDenied(
                "Unexpected window reply".into(),
            ));
        }
        if let Some(tx) = self
            .tx
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("Window reply unavailable".into()))?
            .take()
        {
            let _ = tx.send(body);
        }
        Ok(())
    }
}
fn failed() -> PlatformError {
    PlatformError::TemporarilyUnavailable(
        "KWin’s D-Bus scripting interface did not answer. App launching is still available.".into(),
    )
}
pub(super) async fn call(
    bus: &str,
    action: Option<(&str, bool)>,
    authority: Option<&dyn ports::windows::WindowAuthority>,
) -> Result<String, PlatformError> {
    let connection = tokio::time::timeout(
        Duration::from_secs(2),
        zbus::connection::Builder::address(bus)
            .map_err(|_| failed())?
            .build(),
    )
    .await
    .map_err(|_| failed())?
    .map_err(|_| failed())?;
    let dbus = zbus::fdo::DBusProxy::new(&connection)
        .await
        .map_err(|_| failed())?;
    let owner = dbus
        .get_name_owner(zbus::names::BusName::try_from("org.kde.KWin").map_err(|_| failed())?)
        .await
        .map_err(|_| failed())?
        .to_string();
    let name = connection.unique_name().ok_or_else(failed)?.as_str();
    let (tx, rx) = oneshot::channel();
    connection
        .object_server()
        .at(
            "/windows",
            Reply {
                owner,
                tx: Mutex::new(Some(tx)),
            },
        )
        .await
        .map_err(|_| failed())?;
    let action = action
        .map(|(id, close)| serde_json::json!({"id":id,"close":close}))
        .unwrap_or(serde_json::Value::Null);
    // API identifiers only; all variable values are JSON encoded.
    let script = format!(
        r#"(() => {{
const windows = workspace.windowList ? workspace.windowList() : workspace.clientList();
const action = {action};
let result;
const identity = w => String(w.internalId || w.windowId);
if (action) {{
 const window = windows.find(w => identity(w) === action.id);
 if (!window) result = {{error: 'missing'}};
 else {{ if (action.close) window.closeWindow(); else {{ if ('activeWindow' in workspace) workspace.activeWindow = window; else workspace.activeClient = window; }} result = {{requested: true}}; }}
}} else result = windows.filter(w => w.normalWindow || w.dialog).map(w => ({{
 id: identity(w), title: w.caption, app: String(w.resourceClass), pid: w.pid,
 workspace: w.desktops ? w.desktops.map(d => d.name).join(', ') : String(w.desktop),
 monitor: w.output ? w.output.name : null, focused: w === (workspace.activeWindow || workspace.activeClient)
}}));
callDBus({name}, '/windows', 'app.ophio.WindowReply', 'publish', JSON.stringify(result));
}})();"#,
        name = serde_json::to_string(name).map_err(|_| failed())?
    );
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(script.as_bytes())?;
    file.flush()?;
    let scripting = zbus::Proxy::new(
        &connection,
        "org.kde.KWin",
        "/Scripting",
        "org.kde.kwin.Scripting",
    )
    .await
    .map_err(|_| failed())?;
    let plugin = format!(
        "{}-window-{}",
        ports::PROGRAM_NAME,
        protocol::CommandId::new()
    );
    let id: i32 = tokio::time::timeout(
        Duration::from_secs(2),
        scripting.call(
            "loadScript",
            &(file.path().to_string_lossy().as_ref(), &plugin),
        ),
    )
    .await
    .map_err(|_| failed())?
    .map_err(|_| failed())?;
    if id < 0 {
        return Err(failed());
    }
    let path = format!("/Scripting/Script{id}");
    let result = async {
        let script = zbus::Proxy::new(
            &connection,
            "org.kde.KWin",
            path.as_str(),
            "org.kde.kwin.Script",
        )
        .await
        .map_err(|_| failed())?;
        if let Some(authority) = authority {
            authority.check()?;
        }
        script
            .call::<_, _, ()>("run", &())
            .await
            .map_err(|_| failed())?;
        rx.await.map_err(|_| failed())
    };
    let result = tokio::time::timeout(Duration::from_secs(2), result).await;
    let unloaded = tokio::time::timeout(
        Duration::from_secs(2),
        scripting.call::<_, _, bool>("unloadScript", &(plugin,)),
    )
    .await;
    if action != serde_json::Value::Null
        && (!matches!(unloaded, Ok(Ok(true))) || !matches!(result, Ok(Ok(_))))
    {
        return Err(PlatformError::OutcomeUnknown("KWin did not confirm this action or script cleanup. Check the window before trying again.".into()));
    }
    let body = result.map_err(|_| failed())??;
    if action != serde_json::Value::Null
        && serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .is_none_or(|v| v["requested"] != true)
    {
        return Err(PlatformError::TargetNotFound);
    }
    Ok(body)
}
