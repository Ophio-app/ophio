// SPDX-License-Identifier: Apache-2.0
use crate::{
    alerts,
    device::AndroidDevice,
    error::{Error, Result},
    keys::{self, KeyVault},
    links::{Events, LinkEvent, Links},
    network,
    outbox::Outbox,
    profiles::{HostProfile, Profiles, Record},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use protocol::{
    ProtocolVersion,
    auth::{ConnectionKind, PairRequest, PairResponse, PairingOffer, pairing_code},
    capability::AppOffer,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering},
};
use tauri::{
    State,
    ipc::{Channel, InvokeBody, InvokeResponseBody, Request, Response},
};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

pub struct AppCore {
    pub profiles: Mutex<Result<Profiles>>,
    pub operations: Mutex<()>,
    /// Ends the pairing request waiting for the computer's owner.
    pub pairing: std::sync::Mutex<Option<CancellationToken>>,
    pub outbox: Outbox,
    pub links: Arc<Links>,
    pub device: AndroidDevice,
    pub previews: Arc<crate::preview::Previews>,
}
impl AppCore {
    /// A page that starts loading cannot reach the earlier page's links or
    /// pairing request. They end, so the new page can open its own.
    pub fn page_started(&self) {
        if let Ok(pairing) = self.pairing.lock()
            && let Some(cancel) = pairing.as_ref()
        {
            cancel.cancel();
        }
        self.links.page_started();
        let links = self.links.clone();
        tauri::async_runtime::spawn(async move { links.end_earlier_pages().await });
    }
}
#[derive(Serialize)]
pub struct PairingResult {
    profile: HostProfile,
    response: PairResponse,
}

#[tauri::command]
pub async fn profiles(core: State<'_, AppCore>) -> Result<Vec<HostProfile>> {
    Ok(core
        .profiles
        .lock()
        .await
        .as_ref()
        .map_err(|_| Error::Storage)?
        .list())
}
/// Pairs with a new key and waits while the computer's owner confirms it.
/// `waiting` receives the code both screens show once the request is made.
/// A computer already paired here is paired again only with `replace`, which
/// removes its earlier profile once the new one is kept.
#[tauri::command]
pub async fn pair(
    core: State<'_, AppCore>,
    offer: PairingOffer,
    device_name: String,
    replace: bool,
    waiting: Channel<String>,
) -> Result<PairingResult> {
    network::validate_offer(&offer, &device_name, protocol::time::Timestamp::now().0)?;
    let cancel = CancellationToken::new();
    {
        let mut pairing = core.pairing.lock().map_err(|_| Error::Busy)?;
        if pairing.is_some() {
            return Err(Error::Busy);
        }
        *pairing = Some(cancel.clone());
    }
    let profile_id = protocol::DeviceId::new().to_string();
    let result = async {
        let (key, public, replaces) = {
            let _operation = core.operations.lock().await;
            // Refuse to enroll a key whose profile this device could not keep.
            let replaces = core
                .profiles
                .lock()
                .await
                .as_ref()
                .map_err(|_| Error::Storage)?
                .admits(&offer.host_id, replace)?;
            let (key, public) = keys::create(&core.device, &profile_id).await?;
            (key, public, replaces)
        };
        let code = pairing_code(&offer.cert_sha256, &offer.pairing_secret, &public);
        let request = PairRequest {
            pairing_secret: offer.pairing_secret.clone(),
            device_public_key: public,
            device_name,
            device_platform: "android".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            // The owner confirms at the computer whether the new pairing takes
            // over from the earlier one.
            replaces,
        };
        // Ending the request withdraws it; the computer enrolls nothing. The
        // owner may take minutes, so other computers' links open meanwhile.
        let response = tokio::select! {
            () = cancel.cancelled() => Err(Error::Closed),
            response = network::pair(&offer, request, || waiting.send(code).map_err(|_| Error::Closed)) => response,
        }?;
        let profile = HostProfile {
            profile_id: profile_id.clone(),
            host_id: response.host_id,
            name: response.host_name.clone(),
            addresses: offer.addresses,
            cert_sha256: offer.cert_sha256,
            paired_at: protocol::time::Timestamp::now().0,
            device_id: response.device_id,
            key_protection: key.protection().into(),
        };
        let _operation = core.operations.lock().await;
        let replaced = core
            .profiles
            .lock()
            .await
            .as_mut()
            .map_err(|_| Error::Storage)?
            .insert(
                Record {
                    profile: profile.clone(),
                    key,
                },
                replace,
            )?;
        // The store no longer names these, so a key left behind is never used.
        for id in replaced {
            core.links.forget(&id).await;
            let _ = core.device.delete(&id).await;
            let _ = core.outbox.forget(&id);
        }
        alerts::reload();
        Ok(PairingResult { profile, response })
    }
    .await;
    core.pairing.lock().map_err(|_| Error::Busy)?.take();
    if result.is_err() {
        let _ = core.device.delete(&profile_id).await;
    }
    result
}
#[tauri::command]
pub fn pair_cancel(core: State<'_, AppCore>) {
    // The request clears its own slot when it ends, so another cannot start
    // before this one has.
    if let Some(cancel) = core.pairing.lock().ok().and_then(|p| p.clone()) {
        cancel.cancel();
    }
}
#[tauri::command]
pub async fn rename_profile(
    core: State<'_, AppCore>,
    profile_id: String,
    name: String,
) -> Result<HostProfile> {
    let _operation = core.operations.lock().await;
    let renamed = core
        .profiles
        .lock()
        .await
        .as_mut()
        .map_err(|_| Error::Storage)?
        .rename(&profile_id, &name);
    alerts::reload();
    renamed
}
#[tauri::command]
pub async fn forget(core: State<'_, AppCore>, profile_id: String) -> Result<()> {
    let _operation = core.operations.lock().await;
    core.links.forget(&profile_id).await;
    // Delete the key first: failure to remove metadata must never leave a
    // usable forgotten key behind. Repeating forget is safe.
    core.device.delete(&profile_id).await?;
    core.outbox.forget(&profile_id)?;
    let removed = core
        .profiles
        .lock()
        .await
        .as_mut()
        .map_err(|_| Error::Storage)?
        .remove(&profile_id);
    alerts::reload();
    removed
}
/// Recorded (and synced to disk) before the command it describes is sent.
#[tauri::command]
pub async fn outbox_put(
    core: State<'_, AppCore>,
    profile_id: String,
    command_id: String,
    record: String,
) -> Result<()> {
    core.outbox.put(&profile_id, &command_id, &record)
}
#[tauri::command]
pub async fn outbox_list(core: State<'_, AppCore>, profile_id: String) -> Result<Vec<String>> {
    core.outbox.list(&profile_id)
}
#[tauri::command]
pub async fn outbox_remove(
    core: State<'_, AppCore>,
    profile_id: String,
    command_id: String,
) -> Result<()> {
    core.outbox.remove(&profile_id, &command_id)
}
struct ChannelEvents(Channel<InvokeResponseBody>);
impl Events for ChannelEvents {
    fn event(&self, event: LinkEvent) -> Result<()> {
        self.0
            .send(InvokeResponseBody::Json(
                serde_json::to_string(&event).map_err(|_| Error::Closed)?,
            ))
            .map_err(|_| Error::Closed)
    }
    fn binary(&self, bytes: Vec<u8>) -> Result<()> {
        self.0
            .send(InvokeResponseBody::Raw(bytes))
            .map_err(|_| Error::Closed)
    }
}
/// The page loaded now, which a page opens its links with: a link another
/// page asked for, even just before this one began loading, is refused.
#[tauri::command]
pub fn link_page(core: State<'_, AppCore>) -> u64 {
    core.links.page()
}
#[tauri::command]
pub async fn link_open(
    core: State<'_, AppCore>,
    page: u64,
    profile_id: String,
    kind: ConnectionKind,
    events: Channel<InvokeResponseBody>,
) -> Result<u32> {
    let _operation = core.operations.lock().await;
    let record = core
        .profiles
        .lock()
        .await
        .as_ref()
        .map_err(|_| Error::Storage)?
        .get(&profile_id)?;
    core.links
        .open(page, record, kind, Arc::new(ChannelEvents(events)))
        .await
}
#[tauri::command]
pub async fn link_connect(core: State<'_, AppCore>, link_id: u32) -> Result<()> {
    core.links.connect(link_id).await
}
#[tauri::command]
pub async fn link_send(core: State<'_, AppCore>, link_id: u32, text: String) -> Result<bool> {
    core.links.send(link_id, Message::Text(text.into())).await
}
#[tauri::command]
pub async fn link_send_binary(core: State<'_, AppCore>, request: Request<'_>) -> Result<bool> {
    let id = request
        .headers()
        .get("x-link-id")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse().ok())
        .ok_or(Error::Invalid("Missing link id."))?;
    let bytes = binary_body(request.body())?;
    core.links.send(id, Message::Binary(bytes.into())).await
}
#[tauri::command]
pub async fn link_sent(core: State<'_, AppCore>, link_id: u32) -> Result<u64> {
    core.links.sent(link_id).await
}
#[tauri::command]
pub async fn link_close(core: State<'_, AppCore>, link_id: u32) -> Result<()> {
    core.links.close(link_id).await;
    Ok(())
}
#[tauri::command]
pub async fn device_events(core: State<'_, AppCore>, events: Channel<Value>) -> Result<()> {
    let links = core.links.clone();
    let channel = Channel::<Value>::new(move |body| {
        if let InvokeResponseBody::Json(text) = body {
            let value: Value = serde_json::from_str(&text)?;
            match value["type"].as_str() {
                Some("background") => links.foreground.store(false, Ordering::SeqCst),
                Some("foreground") => links.foreground.store(true, Ordering::SeqCst),
                _ => {}
            }
            events.send(value)?;
        }
        Ok(())
    });
    core.device.call("events", json!({"events":channel})).await
}
#[derive(Deserialize)]
struct EncodedBytes {
    bytes: String,
}
#[tauri::command]
pub async fn shared_read(core: State<'_, AppCore>, id: String, offset: u64) -> Result<Response> {
    let data: EncodedBytes = core
        .device
        .call("sharedRead", json!({"id":id,"offset":offset}))
        .await?;
    let bytes = URL_SAFE_NO_PAD
        .decode(data.bytes)
        .map_err(|_| Error::Invalid("The shared file could not be read."))?;
    Ok(Response::new(bytes))
}
#[tauri::command]
pub async fn shared_ack(core: State<'_, AppCore>, id: String) -> Result<()> {
    core.device.call("sharedAck", json!({"id":id})).await
}
#[tauri::command]
pub async fn save_begin(
    core: State<'_, AppCore>,
    name: String,
    mime: String,
    size: u64,
) -> Result<Value> {
    core.device
        .call("saveBegin", json!({"name":name,"mime":mime,"size":size}))
        .await
}
#[tauri::command]
pub async fn save_chunk(core: State<'_, AppCore>, request: Request<'_>) -> Result<()> {
    let id = request
        .headers()
        .get("x-save-id")
        .and_then(|h| h.to_str().ok())
        .ok_or(Error::Invalid("Missing download id."))?;
    let bytes = binary_body(request.body())?;
    core.device
        .call(
            "saveChunk",
            json!({"id":id,"bytes":URL_SAFE_NO_PAD.encode(bytes)}),
        )
        .await
}
#[tauri::command]
pub async fn save_finish(core: State<'_, AppCore>, id: String) -> Result<Value> {
    core.device.call("saveFinish", json!({"id":id})).await
}
#[tauri::command]
pub async fn save_cancel(core: State<'_, AppCore>, id: String) -> Result<()> {
    core.device.call("saveCancel", json!({"id":id})).await
}
/// Which of the apps a computer offers this app can update to, if any.
#[tauri::command]
pub async fn app_update_check(core: State<'_, AppCore>, offers: Vec<AppOffer>) -> Result<Value> {
    if offers.len() > 16 {
        return Err(Error::Invalid("Too many apps offered."));
    }
    core.device
        .call("appUpdateCheck", json!({ "offers": offers }))
        .await
}
#[tauri::command]
pub async fn app_update_begin(
    core: State<'_, AppCore>,
    profile_id: String,
    size: u64,
    sha256: String,
) -> Result<()> {
    core.device
        .call(
            "appUpdateBegin",
            json!({"profileId":profile_id,"size":size,"sha256":sha256}),
        )
        .await
}
#[tauri::command]
pub async fn app_update_chunk(core: State<'_, AppCore>, request: Request<'_>) -> Result<()> {
    let bytes = binary_body(request.body())?;
    core.device
        .call(
            "appUpdateChunk",
            json!({"bytes":URL_SAFE_NO_PAD.encode(bytes)}),
        )
        .await
}
/// Hands the app to Android, which installs it or asks the person first.
#[tauri::command]
pub async fn app_update_commit(core: State<'_, AppCore>) -> Result<()> {
    core.device.call("appUpdateCommit", json!({})).await
}
#[tauri::command]
pub async fn app_update_cancel(core: State<'_, AppCore>) -> Result<()> {
    core.device.call("appUpdateCancel", json!({})).await
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    id: String,
    title: String,
    body: String,
    /// `needs_you` or `finished`; older pages send none.
    category: Option<String>,
    /// Finished tasks from one computer stack together under its name.
    group: Option<String>,
    group_title: Option<String>,
    route: Option<Value>,
}
/// Android refuses an alert once the shade holds as many as the watcher
/// keeps there, rather than dropping it unseen.
#[tauri::command]
pub async fn notify(core: State<'_, AppCore>, notification: Notification) -> Result<()> {
    /// Android's answer when the alert was answered while its post waited.
    #[derive(Deserialize)]
    struct Cleared {
        cleared: bool,
    }
    let tag = notification.id.clone();
    let posted = core
        .device
        .call::<Option<Cleared>>("notify", notification)
        .await?;
    if posted.is_some_and(|p| p.cleared) {
        return Ok(());
    }
    if let Err(error) = alerts::raised_by_app(&tag) {
        tracing::warn!(%error, "alerts: could not record an alert the app raised");
    }
    Ok(())
}
/// Clears alerts answered elsewhere, by the tag they were raised with.
#[tauri::command]
pub async fn cancel_notifications(core: State<'_, AppCore>, ids: Vec<String>) -> Result<()> {
    if ids.len() > 256 || ids.iter().any(|id| id.len() > 256) {
        return Err(Error::Invalid("Too many alerts."));
    }
    core.device
        .call("cancelNotifications", json!({ "ids": ids }))
        .await
}
#[tauri::command]
pub async fn request_notifications(
    core: State<'_, AppCore>,
    open_settings: Option<bool>,
) -> Result<()> {
    core.device
        .call(
            if open_settings.unwrap_or(false) {
                "notificationSettings"
            } else {
                "requestNotifications"
            },
            json!({}),
        )
        .await
}
#[tauri::command]
pub async fn app_info(core: State<'_, AppCore>) -> Result<Value> {
    let mut info: Value = core.device.call("info", json!({})).await?;
    let profiles = core.profiles.lock().await;
    let modes: std::collections::BTreeSet<_> = profiles
        .as_ref()
        .map_err(|_| Error::Storage)?
        .list()
        .into_iter()
        .map(|p| p.key_protection)
        .collect();
    info["protocol"] =
        serde_json::to_value(protocol::ProtocolVersion::CURRENT).map_err(|_| Error::Storage)?;
    info["keyProtection"] = Value::String(if modes.is_empty() {
        "unpaired".into()
    } else {
        modes.into_iter().collect::<Vec<_>>().join(", ")
    });
    Ok(info)
}
#[tauri::command]
pub async fn keep_awake(core: State<'_, AppCore>, on: bool) -> Result<()> {
    core.device.call("keepAwake", json!({"on":on})).await
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertsState {
    /// None until the person has been asked.
    settings: Option<alerts::Settings>,
    /// Each paired computer's connection, while the watcher runs.
    statuses: BTreeMap<String, alerts::Status>,
    /// Android's side: whether the service runs, and whether Android lets
    /// it run in the background, as it does only with Unrestricted battery use.
    android: Value,
}
#[tauri::command]
pub async fn alerts_state(core: State<'_, AppCore>) -> Result<AlertsState> {
    Ok(AlertsState {
        settings: alerts::settings()?,
        statuses: alerts::statuses(),
        android: core.device.call("alertsInfo", json!({})).await?,
    })
}
#[tauri::command]
pub async fn alerts_configure(
    core: State<'_, AppCore>,
    settings: alerts::Settings,
) -> Result<AlertsState> {
    let raised = alerts::configure(settings)?;
    if !raised.is_empty()
        && let Err(error) = core
            .device
            .call::<()>("cancelNotifications", json!({ "ids": raised }))
            .await
    {
        tracing::warn!(%error, "alerts: could not clear what the watcher raised");
    }
    core.device
        .call::<()>("alertsService", json!({"run": settings.enabled}))
        .await?;
    alerts_state(core).await
}
/// The alerts from a computer the app showed on its screen, which the
/// watcher then does not raise.
#[tauri::command]
pub async fn alerts_seen(
    profile_id: String,
    attention_ids: Vec<protocol::AttentionId>,
) -> Result<()> {
    if profile_id.len() > 256 || attention_ids.len() > protocol::alerts::MAX_OPEN_ALERTS {
        return Err(Error::Invalid("Too many alerts."));
    }
    alerts::seen(&profile_id, &attention_ids)
}
/// The app's screen began, `on`, or stopped showing a computer's alerts.
#[tauri::command]
pub async fn alerts_viewing(profile_id: String, on: bool) -> Result<()> {
    if profile_id.len() > 256 {
        return Err(Error::Invalid("Invalid computer."));
    }
    alerts::viewing(&profile_id, on);
    Ok(())
}
/// Asks Android to let alerts run without battery limits.
#[tauri::command]
pub async fn alerts_battery(core: State<'_, AppCore>) -> Result<()> {
    core.device.call("alertsBattery", json!({})).await
}
/// Opens Android's settings for one notification category: `needs-you` or
/// `finished`.
#[tauri::command]
pub async fn alerts_channel(core: State<'_, AppCore>, channel: String) -> Result<()> {
    if !["needs-you", "finished"].contains(&channel.as_str()) {
        return Err(Error::Invalid("Unknown notification category."));
    }
    core.device
        .call("alertsChannel", json!({ "channel": channel }))
        .await
}
#[tauri::command]
pub async fn scan_pairing_code(core: State<'_, AppCore>) -> Result<Option<String>> {
    core.device.call("scan", json!({})).await
}
#[tauri::command]
pub async fn haptic(core: State<'_, AppCore>) -> Result<()> {
    core.device.call("haptic", json!({})).await
}
#[tauri::command]
pub async fn shell_theme(core: State<'_, AppCore>, dark: bool) -> Result<()> {
    core.device.call("theme", json!({"dark":dark})).await
}
#[tauri::command]
pub async fn shell_immersive(core: State<'_, AppCore>, on: bool) -> Result<()> {
    core.device.call("immersive", json!({"on":on})).await
}

// Android's Tauri bridge represents an incoming ArrayBuffer as a JSON byte
// array. Keep the public raw-body IPC identical across platforms and enforce
// the limit before allocating a second buffer.
pub(crate) fn binary_body(body: &InvokeBody) -> Result<Vec<u8>> {
    let limit = protocol::transfer::MAX_CHUNK_BYTES;
    match body {
        InvokeBody::Raw(bytes) if bytes.len() <= limit => Ok(bytes.clone()),
        InvokeBody::Json(Value::Array(values)) if values.len() <= limit => values
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or(Error::Invalid("Invalid binary byte."))
            })
            .collect(),
        _ => Err(Error::Invalid("Expected a bounded binary chunk.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn android_binary_bridge_enforces_types_and_bounds() {
        assert_eq!(
            binary_body(&InvokeBody::Raw(vec![0, 255])).unwrap(),
            [0, 255]
        );
        assert_eq!(
            binary_body(&InvokeBody::Json(json!([0, 255]))).unwrap(),
            [0, 255]
        );
        for value in [
            json!([-1]),
            json!([256]),
            json!([1.5]),
            json!(["1"]),
            json!({}),
        ] {
            assert!(binary_body(&InvokeBody::Json(value)).is_err());
        }
        assert!(
            binary_body(&InvokeBody::Raw(vec![
                0;
                protocol::transfer::MAX_CHUNK_BYTES
                    + 1
            ]))
            .is_err()
        );
    }
}

#[tauri::command]
pub async fn app_lock_state(core: State<'_, AppCore>) -> Result<Value> {
    core.device.call("lockState", json!({})).await
}
#[tauri::command]
pub async fn app_lock_configure(
    core: State<'_, AppCore>,
    enabled: bool,
    relock_minutes: u8,
) -> Result<Value> {
    if ![0, 1, 5, 15].contains(&relock_minutes) {
        return Err(Error::Invalid("Invalid relock delay."));
    }
    core.device
        .call(
            "lockConfigure",
            json!({"enabled": enabled, "relockMinutes": relock_minutes}),
        )
        .await
}
#[tauri::command]
pub async fn app_unlock(core: State<'_, AppCore>) -> Result<Value> {
    core.device.call("unlock", json!({})).await
}
/// The phone's own unlock before one account action; `title` names it.
#[tauri::command]
pub async fn app_confirm(core: State<'_, AppCore>, title: String) -> Result<Value> {
    if title.trim().is_empty() || title.chars().count() > 80 {
        return Err(Error::Invalid("Invalid confirmation."));
    }
    core.device.call("confirm", json!({ "title": title })).await
}
#[tauri::command]
pub async fn app_lock(core: State<'_, AppCore>) -> Result<Value> {
    core.device.call("lock", json!({})).await
}
#[tauri::command]
pub async fn app_lock_screen_ready(core: State<'_, AppCore>) -> Result<Value> {
    core.device.call("lockScreenReady", json!({})).await
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationPermission {
    Granted,
    Promptable,
    Blocked,
}

#[tauri::command]
pub async fn notification_permission(core: State<'_, AppCore>) -> Result<NotificationPermission> {
    core.device.call("notificationState", json!({})).await
}

/// The platform chooser receives only the plain text supplied by this device.
#[tauri::command]
pub async fn share_text(core: State<'_, AppCore>, text: String) -> Result<bool> {
    #[cfg(target_os = "android")]
    {
        core.device
            .call::<()>("shareText", json!({"text":text}))
            .await?;
        Ok(true)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (core, text);
        Ok(false)
    }
}
