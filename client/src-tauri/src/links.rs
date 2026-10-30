// SPDX-License-Identifier: Apache-2.0
//! Bounded, independently authenticated links. No frame survives a reconnect.
use crate::{
    backoff::{self, Backoff},
    device::Audio,
    error::{Error, Result},
    keys::KeyVault,
    network::{self, Socket},
    profiles::Record,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use protocol::{
    ServerFrame,
    auth::{Authenticated, Challenge, ConnectionKind},
    media::SpeechClientFrame,
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Notify, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LinkState {
    Idle,
    Connecting {
        attempt: u32,
    },
    Authenticating,
    Offline {
        since: i64,
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<protocol::ErrorCode>,
        #[serde(rename = "retryAt")]
        retry_at: Option<i64>,
        /// No address presented the pinned certificate: another device
        /// answered at the computer's address. The link keeps trying, since
        /// the computer may answer again once the phone is back on its network.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        mismatch: bool,
    },
    Failed {
        reason: &'static str,
        detail: String,
    },
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LinkEvent {
    State {
        state: LinkState,
    },
    Authenticated {
        challenge: Challenge,
        auth: Box<Authenticated>,
    },
    Text {
        text: String,
    },
    Level {
        rms: f64,
    },
    /// The phone's microphone started: a speech session's times count from
    /// here, not from the tap that asked for it.
    Capturing,
}
pub trait Events: Send + Sync {
    fn event(&self, event: LinkEvent) -> Result<()>;
    fn binary(&self, bytes: Vec<u8>) -> Result<()>;
}
/// Counts what a link hands the page, a failed handover too: the page's
/// channel numbered that message all the same, and holds back every later
/// one until it arrives. See [`Links::sent`].
struct Counted {
    events: Arc<dyn Events>,
    sent: Arc<AtomicU64>,
}
impl Events for Counted {
    fn event(&self, event: LinkEvent) -> Result<()> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        self.events.event(event)
    }
    fn binary(&self, bytes: Vec<u8>) -> Result<()> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        self.events.binary(bytes)
    }
}
struct Outgoing {
    generation: u64,
    message: Message,
    reply: oneshot::Sender<bool>,
}
struct Handle {
    id: u32,
    /// The page that opened it: see [`Links::page`].
    page: u64,
    kind: ConnectionKind,
    record: Record,
    generation: AtomicU64,
    start: AtomicBool,
    wake: Notify,
    /// The person asked to try again: ends a wait before the next attempt,
    /// or restarts a link that failed.
    retry: Notify,
    reset: Notify,
    stop: CancellationToken,
    queue: mpsc::Sender<Outgoing>,
    events: Arc<dyn Events>,
    sent: Arc<AtomicU64>,
    preview_owner: std::sync::Mutex<Option<(String, CancellationToken)>>,
}
pub struct Links {
    handles: Mutex<BTreeMap<u32, Arc<Handle>>>,
    next: AtomicU32,
    page: AtomicU64,
    /// Held while an earlier page's links end, so whoever waits for them
    /// waits until all have.
    ending: Mutex<()>,
    vault: Arc<dyn KeyVault>,
    audio: Arc<dyn Audio>,
    clock: Arc<dyn ports::Clock>,
    pub foreground: Arc<AtomicBool>,
}
impl Links {
    pub fn new(
        vault: Arc<dyn KeyVault>,
        audio: Arc<dyn Audio>,
        clock: Arc<dyn ports::Clock>,
    ) -> Self {
        Self {
            handles: Mutex::new(BTreeMap::new()),
            next: AtomicU32::new(1),
            page: AtomicU64::new(0),
            ending: Mutex::new(()),
            vault,
            audio,
            clock,
            foreground: Arc::new(AtomicBool::new(true)),
        }
    }
    /// Opens a link for `page`, the page that asked for it. A page that has
    /// since been left gets [`Error::Closed`], even when it asked before
    /// the next began loading: nothing could reach the link. The earlier
    /// pages' links have ended before the new page's opens.
    pub async fn open(
        &self,
        page: u64,
        record: Record,
        kind: ConnectionKind,
        events: Arc<dyn Events>,
    ) -> Result<u32> {
        self.end_earlier_pages().await;
        let mut handles = self.handles.lock().await;
        if page != self.page() {
            return Err(Error::Closed);
        }
        if handles.len() >= 64
            || handles
                .values()
                .any(|h| h.kind == kind && h.record.profile.profile_id == record.profile.profile_id)
        {
            return Err(Error::Busy);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        if id == u32::MAX {
            return Err(Error::Busy);
        }
        let (queue, receiver) = mpsc::channel(16);
        let sent = Arc::new(AtomicU64::new(0));
        let events = Arc::new(Counted {
            events,
            sent: sent.clone(),
        });
        let handle = Arc::new(Handle {
            id,
            page,
            kind,
            record,
            generation: AtomicU64::new(0),
            start: AtomicBool::new(kind != ConnectionKind::Control),
            wake: Notify::new(),
            retry: Notify::new(),
            reset: Notify::new(),
            stop: CancellationToken::new(),
            queue,
            events,
            sent,
            preview_owner: std::sync::Mutex::new(None),
        });
        handles.insert(id, handle.clone());
        let vault = self.vault.clone();
        let audio = self.audio.clone();
        let clock = self.clock.clone();
        let foreground = self.foreground.clone();
        tauri::async_runtime::spawn(run(handle, receiver, vault, audio, clock, foreground));
        Ok(id)
    }
    pub async fn connect(&self, id: u32) -> Result<()> {
        let handle = self
            .handles
            .lock()
            .await
            .get(&id)
            .cloned()
            .ok_or(Error::Closed)?;
        if !handle.start.swap(true, Ordering::SeqCst) {
            handle.wake.notify_one();
        } else if handle.kind == ConnectionKind::Control {
            handle.retry.notify_one();
        }
        Ok(())
    }
    /// How many messages link `id` has handed the page. A page that stays
    /// behind this lost one on the way, and with it every later one, so it
    /// opens the link again.
    pub async fn sent(&self, id: u32) -> Result<u64> {
        let handles = self.handles.lock().await;
        let handle = handles.get(&id).ok_or(Error::Closed)?;
        Ok(handle.sent.load(Ordering::SeqCst))
    }
    pub async fn send(&self, id: u32, message: Message) -> Result<bool> {
        let handle = self
            .handles
            .lock()
            .await
            .get(&id)
            .cloned()
            .ok_or(Error::Closed)?;
        if let Err(error) = validate_outgoing(handle.kind, &message) {
            handle.reset.notify_one();
            return Err(error);
        }
        if handle.kind == ConnectionKind::Control
            && !self.foreground.load(Ordering::SeqCst)
            && is_input(&message)
        {
            handle.reset.notify_one();
            return Ok(false);
        }
        let generation = handle.generation.load(Ordering::SeqCst);
        if generation == 0 || handle.stop.is_cancelled() {
            return Ok(false);
        }
        let (reply, done) = oneshot::channel();
        if handle
            .queue
            .try_send(Outgoing {
                generation,
                message,
                reply,
            })
            .is_err()
        {
            // The JS send interface is synchronous. Once it has observed a
            // refused asynchronous write, reset the transport so its offline
            // state is followed by an honest new authentication, never a
            // permanently stranded native-online/JS-offline split.
            handle.reset.notify_one();
            return Ok(false);
        }
        Ok(done.await.unwrap_or(false))
    }
    pub async fn close(&self, id: u32) {
        let handle = self.handles.lock().await.remove(&id);
        if let Some(handle) = handle {
            handle.generation.store(0, Ordering::SeqCst);
            handle.stop.cancel();
            end_previews(&handle);
            if handle.kind == ConnectionKind::Speech {
                let _ = self.audio.stop(id, false).await;
            }
        }
    }
    pub async fn forget(&self, profile: &str) {
        let ids: Vec<_> = self
            .handles
            .lock()
            .await
            .values()
            .filter(|h| h.record.profile.profile_id == profile)
            .map(|h| h.id)
            .collect();
        for id in ids {
            self.close(id).await;
        }
    }
    /// The page loaded now. A page asks once, and opens its links with it.
    pub fn page(&self) -> u64 {
        self.page.load(Ordering::SeqCst)
    }
    /// A new page began loading. The earlier page can no longer reach its
    /// links, and would otherwise find them holding its computers: they
    /// end with [`Links::end_earlier_pages`].
    pub fn page_started(&self) {
        self.page.fetch_add(1, Ordering::SeqCst);
    }
    /// Ends every link an earlier page opened, and returns once all have
    /// ended, however many ask at once.
    pub async fn end_earlier_pages(&self) {
        let _ending = self.ending.lock().await;
        let page = self.page();
        let ids: Vec<_> = self
            .handles
            .lock()
            .await
            .values()
            .filter(|h| h.page < page)
            .map(|h| h.id)
            .collect();
        for id in ids {
            self.close(id).await;
        }
    }
    pub async fn preview_owner(
        &self,
        profile: &str,
    ) -> Result<(Record, String, CancellationToken)> {
        let handles = self.handles.lock().await;
        let handle = handles
            .values()
            .find(|h| {
                h.kind == ConnectionKind::Control
                    && h.record.profile.profile_id == profile
                    && h.generation.load(Ordering::SeqCst) != 0
            })
            .ok_or(Error::Closed)?;
        let owner = handle.preview_owner.lock().map_err(|_| Error::Closed)?;
        let (id, stop) = owner
            .as_ref()
            .filter(|(_, stop)| !stop.is_cancelled())
            .ok_or(Error::Closed)?;
        Ok((handle.record.clone(), id.clone(), stop.clone()))
    }
    /// Whether the app's screen has a live connection to this computer.
    pub async fn control_online(&self, profile_id: &str) -> bool {
        self.handles.lock().await.values().any(|h| {
            h.kind == ConnectionKind::Control
                && h.record.profile.profile_id == profile_id
                && h.generation.load(Ordering::SeqCst) != 0
        })
    }
}
fn end_previews(handle: &Handle) {
    if let Ok(mut owner) = handle.preview_owner.lock()
        && let Some((_, stop)) = owner.take()
    {
        stop.cancel();
    }
}
fn validate_outgoing(kind: ConnectionKind, message: &Message) -> Result<()> {
    match message {
        Message::Text(s) if s.len() <= network::MAX_TEXT => {
            let v: serde_json::Value =
                serde_json::from_str(s).map_err(|_| Error::Invalid("Invalid JSON frame."))?;
            if !v.is_object()
                || !v.get("type").is_some_and(serde_json::Value::is_string)
                || matches!(v["type"].as_str(), Some("hello" | "authenticate"))
            {
                return Err(Error::Invalid("Invalid application frame."));
            }
            Ok(())
        }
        Message::Binary(bytes)
            if bytes.len() <= protocol::transfer::MAX_CHUNK_BYTES
                && matches!(kind, ConnectionKind::Transfer | ConnectionKind::Terminal) =>
        {
            Ok(())
        }
        _ => Err(Error::Invalid(
            "This frame is too large or is not allowed on this connection.",
        )),
    }
}
fn is_input(message: &Message) -> bool {
    matches!(message, Message::Text(s) if serde_json::from_str::<serde_json::Value>(s).ok().is_some_and(|v| matches!(v["type"].as_str(), Some("input" | "renew_lease"))))
}
fn state(handle: &Handle, s: LinkState) -> Result<()> {
    handle.events.event(LinkEvent::State { state: s })
}
async fn run(
    handle: Arc<Handle>,
    mut queue: mpsc::Receiver<Outgoing>,
    vault: Arc<dyn KeyVault>,
    audio: Arc<dyn Audio>,
    clock: Arc<dyn ports::Clock>,
    foreground: Arc<AtomicBool>,
) {
    let mut backoff = Backoff::default();
    let mut reachability = Reachability::default();
    let mut attempt = 0_u32;
    let mut generation = 0_u64;
    while !handle.start.load(Ordering::SeqCst) {
        tokio::select! { _ = handle.stop.cancelled() => return, _ = handle.wake.notified() => {} }
    }
    loop {
        attempt = attempt.saturating_add(1);
        if state(&handle, LinkState::Connecting { attempt }).is_err() {
            break;
        }
        let connection = tokio::select! {
            _ = handle.stop.cancelled() => break,
            result = network::authenticate(&handle.record, handle.kind, vault.as_ref(), || { let _ = state(&handle, LinkState::Authenticating); }) => result,
        };
        let error = match connection {
            Ok((socket, challenge, auth)) => {
                reachability.heard();
                if handle.kind == ConnectionKind::Control
                    && let Ok(mut owner) = handle.preview_owner.lock()
                {
                    *owner = auth
                        .control_connection_id
                        .as_ref()
                        .map(|id| (id.clone(), handle.stop.child_token()));
                }
                backoff.reset();
                attempt = 0;
                generation = generation.saturating_add(1).max(1);
                handle.generation.store(generation, Ordering::SeqCst);
                if handle
                    .events
                    .event(LinkEvent::Authenticated {
                        challenge,
                        auth: Box::new(auth),
                    })
                    .is_err()
                {
                    break;
                }
                tokio::select! {
                    _ = handle.stop.cancelled() => Error::Closed,
                    _ = handle.reset.notified() => Error::Busy,
                    result = session(&handle, socket, &mut queue, audio.as_ref(), &foreground, generation) => result.err().unwrap_or(Error::Closed),
                }
            }
            Err(e) => e,
        };
        let error = reachability.failed(error);
        handle.generation.store(0, Ordering::SeqCst);
        end_previews(&handle);
        while let Ok(out) = queue.try_recv() {
            let _ = out.reply.send(false);
        }
        if handle.kind == ConnectionKind::Speech {
            let _ = audio.stop(handle.id, false).await;
        }
        if handle.stop.is_cancelled() {
            break;
        }
        if let Some(reason) = error.failure() {
            let _ = state(
                &handle,
                LinkState::Failed {
                    reason,
                    detail: error.connection_message(),
                },
            );
            // Only the person asking tries again, as after updating the
            // computer or returning to the network it was paired on.
            tokio::select! { _ = handle.stop.cancelled() => break, _ = handle.retry.notified() => continue }
        }
        let entropy = crate::keys::random::<2>()
            .map(|r| u16::from_ne_bytes(*r))
            .unwrap_or(32768);
        let retry =
            (handle.kind == ConnectionKind::Control).then(|| backoff.next(clock.as_ref(), entropy));
        if state(
            &handle,
            LinkState::Offline {
                since: clock.now().0,
                reason: error.connection_message(),
                code: error.remote_code(),
                retry_at: retry.as_ref().map(|r| r.wall_ms),
                mismatch: matches!(error, Error::CertificateMismatch),
            },
        )
        .is_err()
        {
            break;
        }
        let Some(retry) = retry else { return };
        tokio::select! { _ = handle.stop.cancelled() => break, _ = handle.retry.notified() => {}, _ = backoff::wait(clock.as_ref(), retry.at) => {} }
    }
    handle.generation.store(0, Ordering::SeqCst);
    let _ = state(&handle, LinkState::Idle);
}

async fn session(
    handle: &Handle,
    mut socket: Socket,
    queue: &mut mpsc::Receiver<Outgoing>,
    audio: &dyn Audio,
    foreground: &AtomicBool,
    generation: u64,
) -> Result<()> {
    let mut recording = false;
    let mut silence = Silence::new(handle.kind, network::heard(&socket));
    let mut tick = tokio::time::interval(Duration::from_millis(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            // Handle cancellation and application commands before another PCM batch.
            biased;
            Some(out) = queue.recv() => {
                if out.generation != generation || (handle.kind == ConnectionKind::Control && !foreground.load(Ordering::SeqCst) && is_input(&out.message)) { let _ = out.reply.send(false); handle.reset.notify_one(); continue; }
                let sent = if handle.kind == ConnectionKind::Speech {
                    speech_command(handle, &mut socket, audio, &mut recording, out.message).await
                } else { network::send(&mut socket, out.message).await };
                let _ = out.reply.send(sent.is_ok()); sent?;
            }
            message = silence.next(&mut socket) => match message? {
                Message::Text(text) => {
                    if text.len() > network::MAX_TEXT { return Err(Error::Invalid("The computer sent an oversized text frame.")); }
                    if let Ok(ServerFrame::Fatal(error)) = serde_json::from_str(&text) { return Err(Error::Remote(error)); }
                    if handle.kind == ConnectionKind::Speech && serde_json::from_str::<serde_json::Value>(&text).ok().is_some_and(|v| matches!(v["type"].as_str(), Some("error" | "finished"))) {
                        let _ = audio.stop(handle.id, false).await; recording = false;
                    }
                    handle.events.event(LinkEvent::Text { text: text.to_string() })?;
                }
                Message::Binary(bytes) => handle.events.binary(validate_incoming(handle.kind, &bytes)?)?,
                Message::Ping(_) => { tokio::time::timeout(network::WRITE_TIMEOUT, socket.flush()).await.map_err(|_| Error::Timeout)?.map_err(|_| Error::Closed)?; }
                Message::Pong(_) => {},
                Message::Close(_) => return Err(Error::Closed),
                Message::Frame(_) => return Err(Error::Invalid("Unexpected WebSocket frame.")),
            },
            _ = tick.tick(), if recording => {
                let chunk = audio.read(handle.id).await?;
                if let Some(error) = chunk.error {
                    let _ = audio.stop(handle.id, false).await; recording = false;
                    network::send_json(&mut socket, &SpeechClientFrame::Cancel).await?;
                    speech_error(handle, &error)?;
                } else if let Some(pcm) = chunk.pcm {
                    let bytes = URL_SAFE_NO_PAD.decode(pcm).map_err(|_| Error::Invalid("Invalid microphone audio."))?;
                    if bytes.len() > 32 * 1024 || bytes.len() % 2 != 0 { return Err(Error::Invalid("Invalid microphone audio.")); }
                    if !bytes.is_empty() { network::send(&mut socket, Message::Binary(bytes.into())).await?; }
                    handle.events.event(LinkEvent::Level { rms: if chunk.rms.is_finite() { chunk.rms.clamp(0.0, 1.0) } else { 0.0 } })?;
                }
            }
        }
    }
}
/// What the link says about a computer between attempts. A retry that times
/// out while the computer is silent meets the same silence, so once the link
/// has said the computer is unreachable it keeps saying so until the computer
/// answers again, instead of each retry giving a new reason.
#[derive(Default)]
struct Reachability {
    unreachable: bool,
}
impl Reachability {
    fn heard(&mut self) {
        self.unreachable = false;
    }
    fn failed(&mut self, error: Error) -> Error {
        match error {
            Error::Timeout if self.unreachable => return Error::Unreachable,
            Error::Unreachable => self.unreachable = true,
            // The computer answered, if only to refuse.
            Error::Remote(_) => self.heard(),
            _ => {}
        }
        error
    }
}
/// The app pings the computer every 15 s on the control connection and the
/// companion answers each ping at once. Hearing nothing for this long means the
/// computer is unreachable even while the connection looks open, as when it is
/// asleep or frozen: at least two pings have gone 15 s without an answer.
const SILENCE: Duration = Duration::from_secs(45);
/// The timer below runs on the monotonic clock, which stops while the phone
/// sleeps, so silence is looked at again this often: on waking, a computer
/// that went quiet meanwhile is found within this time, not a bound later.
const LOOK_AGAIN: Duration = Duration::from_secs(5);
/// Reads what the computer sends. Only the control connection is pinged, so
/// only there does silence end the session; the others may rightly go quiet.
/// Silence is counted from the last byte, not the last whole message, so a
/// large message arriving slowly keeps a working link.
struct Silence {
    limit: Option<Duration>,
    heard: network::Heard,
}
impl Silence {
    fn new(kind: ConnectionKind, heard: network::Heard) -> Self {
        Self {
            limit: (kind == ConnectionKind::Control).then_some(SILENCE),
            heard,
        }
    }
    async fn next<S, E>(&mut self, socket: &mut S) -> Result<Message>
    where
        S: futures_util::Stream<Item = std::result::Result<Message, E>> + Unpin,
    {
        let message = loop {
            let Some(limit) = self.limit else {
                break socket.next().await;
            };
            let wait = limit
                .saturating_sub(self.heard.silent_for())
                .min(LOOK_AGAIN);
            match tokio::time::timeout(wait, socket.next()).await {
                Ok(message) => break message,
                // Bytes arrived in the meantime, or not yet for long enough.
                Err(_) if self.heard.silent_for() < limit => {}
                Err(_) => return Err(Error::Unreachable),
            }
        };
        message.ok_or(Error::Closed)?.map_err(|_| Error::Closed)
    }
}
/// Check a binary frame from the computer and pass it on whole: the app reads
/// the terminal output offset itself to replay after a reattach.
fn validate_incoming(kind: ConnectionKind, bytes: &[u8]) -> Result<Vec<u8>> {
    match kind {
        ConnectionKind::Terminal => {
            let (_, output) = protocol::terminal::split_output_frame(bytes).ok_or(
                Error::Invalid("The computer sent an invalid terminal frame."),
            )?;
            if output.len() > protocol::transfer::MAX_CHUNK_BYTES {
                return Err(Error::Invalid("The terminal frame is too large."));
            }
        }
        ConnectionKind::Transfer if bytes.len() <= protocol::transfer::MAX_CHUNK_BYTES => {}
        ConnectionKind::Media if bytes.len() <= network::MAX_MEDIA => {}
        _ => {
            return Err(Error::Invalid(
                "The computer sent an unexpected binary frame.",
            ));
        }
    }
    Ok(bytes.to_vec())
}
fn speech_error(handle: &Handle, message: &str) -> Result<()> {
    speech_error_to(handle.events.as_ref(), message)
}
fn speech_error_to(events: &dyn Events, message: &str) -> Result<()> {
    events.event(LinkEvent::Text {
        text: serde_json::json!({"type":"error","message":message}).to_string(),
    })
}
/// Starts the phone's microphone, before the computer is asked to listen:
/// a delayed Android prompt must not consume the companion's speech-session
/// timeout. Whether it captures; capture is stopped on failure.
async fn start_capture(id: u32, events: &dyn Events, audio: &dyn Audio) -> Result<bool> {
    if let Err(error) = audio.start(id).await {
        speech_error_to(events, &error.to_string())?;
        return Ok(false);
    }
    events.event(LinkEvent::Capturing)?;
    Ok(true)
}
async fn speech_command(
    handle: &Handle,
    socket: &mut Socket,
    audio: &dyn Audio,
    recording: &mut bool,
    message: Message,
) -> Result<()> {
    let Message::Text(text) = &message else {
        return Err(Error::Invalid("Speech audio is captured by the phone."));
    };
    let frame: SpeechClientFrame =
        serde_json::from_str(text).map_err(|_| Error::Invalid("Invalid speech request."))?;
    match frame {
        SpeechClientFrame::Start { .. } => {
            if *recording {
                return Err(Error::Busy);
            }
            if !start_capture(handle.id, handle.events.as_ref(), audio).await? {
                return Ok(());
            }
            *recording = true;
            network::send(socket, message).await
        }
        SpeechClientFrame::Finish => {
            let remaining = audio.stop(handle.id, true).await?;
            *recording = false;
            if !remaining.is_empty() {
                network::send(socket, Message::Binary(remaining.into())).await?;
            }
            network::send(socket, message).await
        }
        SpeechClientFrame::Cancel => {
            let _ = audio.stop(handle.id, false).await;
            *recording = false;
            network::send(socket, message).await
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncWriteExt, DuplexStream},
        time::Instant,
    };
    use tokio_tungstenite::{
        WebSocketStream,
        tungstenite::protocol::{
            Role,
            frame::{
                Frame,
                coding::{Data, OpCode},
            },
        },
    };
    #[test]
    fn binary_limits_and_auth_cannot_be_bypassed_from_webview() {
        assert!(
            validate_outgoing(
                ConnectionKind::Transfer,
                &Message::Binary(vec![0; protocol::transfer::MAX_CHUNK_BYTES].into())
            )
            .is_ok()
        );
        assert!(
            validate_outgoing(
                ConnectionKind::Transfer,
                &Message::Binary(vec![0; protocol::transfer::MAX_CHUNK_BYTES + 1].into())
            )
            .is_err()
        );
        assert!(
            validate_outgoing(
                ConnectionKind::Control,
                &Message::Text("{\"type\":\"authenticate\"}".into())
            )
            .is_err()
        );
        assert!(
            validate_outgoing(ConnectionKind::Speech, &Message::Binary(vec![0; 10].into()))
                .is_err()
        );
    }
    /// A control socket over an in-memory pipe, and the far end, where the
    /// test writes what the computer sends.
    async fn pipe() -> (
        WebSocketStream<network::HeardStream<DuplexStream>>,
        network::Heard,
        DuplexStream,
    ) {
        let (near, far) = tokio::io::duplex(64 * 1024);
        let near = network::HeardStream::new(near);
        let heard = near.heard();
        let socket = WebSocketStream::from_raw_socket(near, Role::Client, None).await;
        (socket, heard, far)
    }
    fn text_frame(len: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        Frame::message(vec![b'a'; len], OpCode::Data(Data::Text), true)
            .format(&mut bytes)
            .unwrap();
        bytes
    }
    #[tokio::test(start_paused = true)]
    async fn a_silent_computer_is_unreachable_after_the_bound() {
        let start = Instant::now();
        // One message 30 s in, then nothing: each byte restarts the count.
        let (mut socket, heard, mut computer) = pipe().await;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            computer.write_all(&text_frame(2)).await.unwrap();
            std::future::pending::<()>().await;
        });
        let mut control = Silence::new(ConnectionKind::Control, heard);
        assert!(matches!(
            control.next(&mut socket).await,
            Ok(Message::Text(_))
        ));
        assert!(matches!(
            control.next(&mut socket).await,
            Err(Error::Unreachable)
        ));
        assert_eq!(start.elapsed(), Duration::from_secs(30) + SILENCE);
        let (mut quiet, heard, _computer) = pipe().await;
        let mut terminal = Silence::new(ConnectionKind::Terminal, heard);
        assert!(
            tokio::time::timeout(SILENCE * 4, terminal.next(&mut quiet))
                .await
                .is_err()
        );
    }
    #[tokio::test(start_paused = true)]
    async fn a_computer_that_went_quiet_while_the_phone_slept_is_found_on_waking() {
        let (mut socket, heard, _computer) = pipe().await;
        let mut control = Silence::new(ConnectionKind::Control, heard.clone());
        let next = control.next(&mut socket);
        tokio::pin!(next);
        assert!(
            tokio::time::timeout(Duration::from_secs(10), &mut next)
                .await
                .is_err()
        );
        // Two hours asleep: the wall clock moved and the monotonic one did not.
        heard.slept(Duration::from_secs(2 * 3600));
        let woke = Instant::now();
        assert!(matches!(next.await, Err(Error::Unreachable)));
        assert!(woke.elapsed() <= LOOK_AGAIN);
    }
    #[tokio::test(start_paused = true)]
    async fn a_large_message_arriving_slowly_keeps_the_link() {
        let start = Instant::now();
        let (mut socket, heard, mut computer) = pipe().await;
        tokio::spawn(async move {
            // The largest message at 16 KiB/s takes 64 s, more than the bound.
            for chunk in text_frame(network::MAX_TEXT).chunks(16 * 1024) {
                computer.write_all(chunk).await.unwrap();
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            std::future::pending::<()>().await;
        });
        let mut control = Silence::new(ConnectionKind::Control, heard);
        let Ok(Message::Text(text)) = control.next(&mut socket).await else {
            panic!("the message did not arrive whole");
        };
        assert_eq!(text.len(), network::MAX_TEXT);
        let arrived = start.elapsed();
        assert!(arrived > SILENCE, "{arrived:?}");
        // After it, silence ends the link, counted from its last byte.
        assert!(matches!(
            control.next(&mut socket).await,
            Err(Error::Unreachable)
        ));
        assert_eq!(start.elapsed(), arrived + SILENCE);
    }
    #[test]
    fn a_silent_computer_stays_unreachable_through_retries_until_it_answers() {
        let mut link = Reachability::default();
        // Silence ends the session, then retries reach the computer but time
        // out in the handshake, or fail to reach it at all.
        for error in [
            Error::Unreachable,
            Error::Timeout,
            Error::Timeout,
            Error::Unreachable,
            Error::Timeout,
        ] {
            assert!(matches!(link.failed(error), Error::Unreachable));
        }
        // A connection closing mid-handshake is not the computer answering.
        assert!(matches!(link.failed(Error::Closed), Error::Closed));
        assert!(matches!(link.failed(Error::Timeout), Error::Unreachable));
        // It answers: a later timeout is a new, different failure.
        link.heard();
        assert!(matches!(link.failed(Error::Timeout), Error::Timeout));
        // A refusal is an answer too.
        link.failed(Error::Unreachable);
        let refusal =
            protocol::ProtocolError::new(protocol::ErrorCode::TemporarilyUnavailable, "Busy");
        assert!(matches!(
            link.failed(Error::Remote(refusal)),
            Error::Remote(_)
        ));
        assert!(matches!(link.failed(Error::Timeout), Error::Timeout));
        // A computer that was never silent times out as it always did.
        let mut fresh = Reachability::default();
        assert!(matches!(fresh.failed(Error::Timeout), Error::Timeout));
        assert!(matches!(fresh.failed(Error::Closed), Error::Closed));
        assert!(matches!(fresh.failed(Error::Timeout), Error::Timeout));
    }
    struct States(mpsc::UnboundedSender<LinkState>);
    impl Events for States {
        fn event(&self, event: LinkEvent) -> Result<()> {
            if let LinkEvent::State { state } = event {
                let _ = self.0.send(state);
            }
            Ok(())
        }
        fn binary(&self, _: Vec<u8>) -> Result<()> {
            Ok(())
        }
    }
    struct NoKeys;
    #[async_trait::async_trait]
    impl KeyVault for NoKeys {
        async fn generate(&self, _: &str) -> Result<Option<[u8; 32]>> {
            Err(Error::Key)
        }
        async fn sign(&self, _: &str, _: &[u8]) -> Result<[u8; 64]> {
            Err(Error::Key)
        }
        async fn wrap(&self, _: &str, _: &[u8; 32]) -> Result<String> {
            Err(Error::Key)
        }
        async fn unwrap(&self, _: &str, _: &str) -> Result<zeroize::Zeroizing<Vec<u8>>> {
            Err(Error::Key)
        }
        async fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }
    struct NoAudio;
    #[async_trait::async_trait]
    impl Audio for NoAudio {
        async fn start(&self, _: u32) -> Result<()> {
            Ok(())
        }
        async fn read(&self, _: u32) -> Result<crate::device::AudioChunk> {
            Ok(crate::device::AudioChunk::default())
        }
        async fn stop(&self, _: u32, _: bool) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }
    /// A microphone behind a permission prompt answered after five seconds.
    struct Prompted(bool);
    #[async_trait::async_trait]
    impl Audio for Prompted {
        async fn start(&self, _: u32) -> Result<()> {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if self.0 {
                Ok(())
            } else {
                Err(Error::Invalid("Microphone permission was denied."))
            }
        }
        async fn read(&self, _: u32) -> Result<crate::device::AudioChunk> {
            Ok(crate::device::AudioChunk::default())
        }
        async fn stop(&self, _: u32, _: bool) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }
    /// Events as they arrive, with when.
    #[derive(Default)]
    struct Heard(
        std::sync::Mutex<Vec<(Duration, serde_json::Value)>>,
        Option<Instant>,
    );
    impl Events for Heard {
        fn event(&self, event: LinkEvent) -> Result<()> {
            let at = self.1.map(|s| s.elapsed()).unwrap_or_default();
            self.0
                .lock()
                .unwrap()
                .push((at, serde_json::to_value(event).unwrap()));
            Ok(())
        }
        fn binary(&self, _: Vec<u8>) -> Result<()> {
            Ok(())
        }
    }
    #[tokio::test(start_paused = true)]
    async fn speech_is_timed_from_when_the_microphone_captures() {
        let heard = Heard(Default::default(), Some(Instant::now()));
        assert!(start_capture(1, &heard, &Prompted(true)).await.unwrap());
        assert_eq!(
            *heard.0.lock().unwrap(),
            [(
                Duration::from_secs(5),
                serde_json::json!({"type": "capturing"})
            )]
        );
        // Refused: an error, and nothing to count from.
        let heard = Heard::default();
        assert!(!start_capture(1, &heard, &Prompted(false)).await.unwrap());
        let events = heard.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        let text: serde_json::Value =
            serde_json::from_str(events[0].1["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["type"], "error");
    }
    /// A clock that never reaches the time of the next attempt.
    #[derive(Debug)]
    struct Stopped(std::time::Instant);
    impl ports::Clock for Stopped {
        fn now(&self) -> protocol::time::Timestamp {
            protocol::time::Timestamp(1_000)
        }
        fn monotonic(&self) -> std::time::Instant {
            self.0
        }
    }
    fn studio(address: String, cert_sha256: String) -> Record {
        Record {
            profile: crate::profiles::HostProfile {
                profile_id: "p".into(),
                host_id: protocol::HostId::new(),
                name: "studio".into(),
                addresses: vec![address],
                cert_sha256,
                paired_at: 1,
                device_id: protocol::DeviceId::new(),
                key_protection: "keystore".into(),
            },
            key: crate::keys::ProtectedKey::Keystore,
        }
    }
    /// A control link to the given address and pinned certificate, with
    /// states reported in order.
    async fn control_link(
        address: String,
        cert_sha256: String,
    ) -> (Links, u32, mpsc::UnboundedReceiver<LinkState>) {
        let record = studio(address, cert_sha256);
        let links = Links::new(
            Arc::new(NoKeys),
            Arc::new(NoAudio),
            Arc::new(Stopped(std::time::Instant::now())),
        );
        let (seen, states) = mpsc::unbounded_channel();
        let id = links
            .open(
                links.page(),
                record,
                ConnectionKind::Control,
                Arc::new(States(seen)),
            )
            .await
            .unwrap();
        (links, id, states)
    }
    #[tokio::test]
    async fn trying_again_now_ends_the_wait_before_the_next_attempt() {
        // Nothing listens at this address, so each attempt fails at once.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (links, id, mut states) =
            control_link(format!("127.0.0.1:{port}"), "ab".repeat(32)).await;
        links.connect(id).await.unwrap();
        assert!(matches!(
            states.recv().await,
            Some(LinkState::Connecting { attempt: 1 })
        ));
        assert!(matches!(
            states.recv().await,
            Some(LinkState::Offline {
                retry_at: Some(_),
                ..
            })
        ));
        // The person asks to try again while the link waits.
        links.connect(id).await.unwrap();
        let next = tokio::time::timeout(Duration::from_secs(5), states.recv()).await;
        assert!(
            matches!(next, Ok(Some(LinkState::Connecting { attempt: 2 }))),
            "{next:?}"
        );
        links.close(id).await;
    }
    /// Hands the page each state but an offline one, as a webview that
    /// failed to take it.
    struct Dropping(mpsc::UnboundedSender<LinkState>);
    impl Events for Dropping {
        fn event(&self, event: LinkEvent) -> Result<()> {
            let LinkEvent::State { state } = event else {
                return Ok(());
            };
            if matches!(state, LinkState::Offline { .. }) {
                return Err(Error::Closed);
            }
            let _ = self.0.send(state);
            Ok(())
        }
        fn binary(&self, _: Vec<u8>) -> Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn the_page_can_tell_it_missed_a_message() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let links = Links::new(
            Arc::new(NoKeys),
            Arc::new(NoAudio),
            Arc::new(Stopped(std::time::Instant::now())),
        );
        let (seen, mut states) = mpsc::unbounded_channel();
        let record = studio(format!("127.0.0.1:{port}"), "ab".repeat(32));
        let id = links
            .open(
                links.page(),
                record,
                ConnectionKind::Control,
                Arc::new(Dropping(seen)),
            )
            .await
            .unwrap();
        assert_eq!(links.sent(id).await.unwrap(), 0);
        links.connect(id).await.unwrap();
        assert!(matches!(
            states.recv().await,
            Some(LinkState::Connecting { attempt: 1 })
        ));
        assert!(matches!(states.recv().await, Some(LinkState::Idle)));
        // The page received two; the offline state between them counts too.
        assert_eq!(links.sent(id).await.unwrap(), 3);
        links.close(id).await;
        assert!(matches!(links.sent(id).await, Err(Error::Closed)));
    }
    #[tokio::test]
    async fn a_failed_link_tries_again_when_the_person_asks() {
        // The paired computer answers but has removed this phone, so the
        // link stops. A wrong certificate would not do: that keeps trying.
        let computer = rcgen::generate_simple_self_signed(vec!["computer".into()]).unwrap();
        let pin = crate::network::fingerprint(computer.cert.der());
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![computer.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(computer.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    continue;
                };
                let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
                    continue;
                };
                let _ = ws.next().await;
                let removed = protocol::ServerFrame::Fatal(protocol::ProtocolError::new(
                    protocol::ErrorCode::Unauthenticated,
                    "This device was removed.",
                ));
                let text = serde_json::to_string(&removed).unwrap();
                let _ = ws.send(Message::Text(text.into())).await;
            }
        });
        let (links, id, mut states) = control_link(address, pin).await;
        links.connect(id).await.unwrap();
        assert!(matches!(
            states.recv().await,
            Some(LinkState::Connecting { .. })
        ));
        assert!(matches!(
            states.recv().await,
            Some(LinkState::Authenticating)
        ));
        let failed = states.recv().await;
        assert!(
            matches!(
                failed,
                Some(LinkState::Failed {
                    reason: "revoked",
                    ..
                })
            ),
            "{failed:?}"
        );
        // Nothing happens by itself; Retry connection is the person acting.
        assert!(
            tokio::time::timeout(Duration::from_millis(300), states.recv())
                .await
                .is_err()
        );
        links.connect(id).await.unwrap();
        let next = tokio::time::timeout(Duration::from_secs(5), states.recv()).await;
        assert!(
            matches!(next, Ok(Some(LinkState::Connecting { .. }))),
            "{next:?}"
        );
        links.close(id).await;
    }
    #[tokio::test]
    async fn a_reloaded_page_opens_its_links_again() {
        let address = "127.0.0.1:9".to_owned();
        let pin = "ab".repeat(32);
        let (links, earlier, _old) = control_link(address.clone(), pin.clone()).await;
        // The page reloads. Its script is gone, and it never closed its link.
        // The new page opens its own before the earlier one's have ended.
        links.page_started();
        let (seen, _new) = mpsc::unbounded_channel();
        let id = links
            .open(
                links.page(),
                studio(address, pin),
                ConnectionKind::Control,
                Arc::new(States(seen)),
            )
            .await
            .unwrap();
        assert!(matches!(links.connect(earlier).await, Err(Error::Closed)));
        // A link the new page opened is never the earlier page's to end.
        links.end_earlier_pages().await;
        assert!(links.connect(id).await.is_ok());
        links.close(id).await;
    }
    #[tokio::test]
    async fn an_open_the_earlier_page_asked_for_is_refused_after_a_reload() {
        let address = "127.0.0.1:9".to_owned();
        let pin = "ab".repeat(32);
        let links = Arc::new(Links::new(
            Arc::new(NoKeys),
            Arc::new(NoAudio),
            Arc::new(Stopped(std::time::Instant::now())),
        ));
        let (seen, _states) = mpsc::unbounded_channel();
        let events: Arc<dyn Events> = Arc::new(States(seen));
        // The earlier page asked for a link, which waits behind another
        // operation while the page reloads and the earlier page's links end.
        let earlier = links.page();
        links.page_started();
        let ending = tokio::spawn({
            let links = links.clone();
            async move { links.end_earlier_pages().await }
        });
        let stale = links
            .open(
                earlier,
                studio(address.clone(), pin.clone()),
                ConnectionKind::Control,
                events.clone(),
            )
            .await;
        assert!(matches!(stale, Err(Error::Closed)), "{stale:?}");
        ending.await.unwrap();
        // Nothing holds the computer, so the new page's own open is not busy.
        let id = links
            .open(
                links.page(),
                studio(address, pin),
                ConnectionKind::Control,
                events,
            )
            .await
            .unwrap();
        assert!(links.connect(id).await.is_ok());
        links.close(id).await;
    }
    #[test]
    fn terminal_output_keeps_its_offset_for_the_app() {
        let frame = protocol::terminal::output_frame(42, b"ok\r\n");
        assert_eq!(
            validate_incoming(ConnectionKind::Terminal, &frame).unwrap(),
            frame
        );
        assert!(validate_incoming(ConnectionKind::Terminal, &[0; 3]).is_err());
        assert!(validate_incoming(ConnectionKind::Control, &frame).is_err());
    }
}
