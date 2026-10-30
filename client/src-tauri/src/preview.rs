// SPDX-License-Identifier: Apache-2.0
//! Private loopback HTTP origin. Each request gets a fresh, non-replaying side stream.
use crate::{
    error::{Error, Result},
    keys::KeyVault,
    network,
    profiles::Record,
};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{Request, Response, StatusCode, body::Incoming, header, service::service_fn};
use hyper_util::rt::TokioIo;
use protocol::{PreviewId, ServerFrame, auth::ConnectionKind, preview::*};
use serde::Serialize;
use std::{
    collections::HashMap,
    convert::Infallible,
    net::Ipv4Addr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    net::TcpListener,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

type Body = UnsyncBoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;
/// Connections a preview holds open at once, WebSocket relays included.
const LIMIT: usize = 16;
/// A WebSocket relay, handed from the request that upgraded a connection to
/// the task that owns the connection.
type Relay = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

#[async_trait]
pub trait Connector: Send + Sync {
    async fn open(&self) -> Result<(DuplexStream, String)>;
}
pub struct Remote {
    pub record: Record,
    pub vault: Arc<dyn KeyVault>,
    pub owner: String,
    pub preview: PreviewId,
    pub stop: CancellationToken,
}
#[async_trait]
impl Connector for Remote {
    async fn open(&self) -> Result<(DuplexStream, String)> {
        let (mut ws, _, _) = tokio::select! {
            biased;
            _ = self.stop.cancelled() => return Err(Error::Closed),
            result = network::authenticate(&self.record, ConnectionKind::Preview, self.vault.as_ref(), || {}) => result?,
        };
        network::send_json(
            &mut ws,
            &PreviewClientFrame::Open {
                preview_id: self.preview,
                control_connection_id: Some(self.owner.clone()),
            },
        )
        .await?;
        let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .map_err(|_| Error::Timeout)?
            .ok_or(Error::Closed)?
            .map_err(|_| Error::Closed)?;
        let tokio_tungstenite::tungstenite::Message::Text(text) = frame else {
            return Err(Error::Closed);
        };
        let address = match serde_json::from_str::<PreviewServerFrame>(&text) {
            Ok(PreviewServerFrame::Opened {
                connected_address: Some(address),
                ..
            }) => address,
            Ok(PreviewServerFrame::Closed {
                error: Some(error), ..
            }) => return Err(Error::Remote(error)),
            _ => {
                if let Ok(ServerFrame::Fatal(error)) = serde_json::from_str(&text) {
                    return Err(Error::Remote(error));
                }
                return Err(Error::Closed);
            }
        };
        loopback_socket(&address)?;
        let (client, relay) = tokio::io::duplex(MAX_PREVIEW_CHUNK);
        let stop = self.stop.clone();
        tokio::spawn(async move {
            use tokio_tungstenite::tungstenite::Message;
            let (mut to_host, mut from_host) = ws.split();
            let (mut reader, mut writer) = tokio::io::split(relay);
            let up = async {
                let mut buffer = vec![0; MAX_PREVIEW_CHUNK];
                loop {
                    let count = reader.read(&mut buffer).await.map_err(|_| Error::Closed)?;
                    if count == 0 {
                        return Ok::<_, Error>(());
                    }
                    tokio::time::timeout(
                        network::WRITE_TIMEOUT,
                        to_host.send(Message::Binary(Bytes::copy_from_slice(&buffer[..count]))),
                    )
                    .await
                    .map_err(|_| Error::Timeout)?
                    .map_err(|_| Error::Closed)?;
                }
            };
            let down = async {
                while let Some(Ok(message)) = from_host.next().await {
                    match message {
                        Message::Binary(bytes) if bytes.len() <= MAX_PREVIEW_CHUNK => {
                            writer.write_all(&bytes).await.map_err(|_| Error::Closed)?
                        }
                        Message::Ping(_) | Message::Pong(_) => {}
                        _ => break,
                    }
                }
                Ok::<_, Error>(())
            };
            tokio::select! { biased; _ = stop.cancelled() => {}, _ = up => {}, _ = down => {} }
        });
        Ok((client, address))
    }
}

/// The host can only report a loopback socket, never inject a Host header.
/// IPv4 loopback written as IPv6 (`[::ffff:127.0.0.1]`) is loopback too: the
/// companion connects that way to a preview registered at that address.
fn loopback_socket(address: &str) -> Result<()> {
    let socket: std::net::SocketAddr = address.parse().map_err(|_| Error::Closed)?;
    if !socket.ip().to_canonical().is_loopback() {
        return Err(Error::Closed);
    }
    Ok(())
}

/// Cookies have no browser-level port scope. A unique per-listener cookie name,
/// exact Host/Origin checks, and the native view's origin restriction bind it here.
struct Gate {
    secret: zeroize::Zeroizing<String>,
    authority: String,
    cookie: String,
}
impl Gate {
    fn new(port: u16) -> Result<Self> {
        let secret = URL_SAFE_NO_PAD.encode(*crate::keys::random::<32>()?);
        Ok(Self {
            secret: zeroize::Zeroizing::new(secret),
            authority: format!("127.0.0.1:{port}"),
            cookie: format!(
                "preview_{port}_{}",
                URL_SAFE_NO_PAD.encode(*crate::keys::random::<12>()?)
            ),
        })
    }
    fn url(&self) -> String {
        format!("http://{}/{}/", self.authority, self.secret.as_str())
    }
    fn same_origin<B>(&self, request: &Request<B>) -> bool {
        if request
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            != Some(&self.authority)
        {
            return false;
        }
        if let Some(origin) = request.headers().get(header::ORIGIN)
            && origin.to_str().ok() != Some(format!("http://{}", self.authority).as_str())
        {
            return false;
        }
        !request
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|v| v != "same-origin" && v != "none")
    }
    fn cookie_ok(&self, headers: &header::HeaderMap) -> bool {
        let mut values = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|v| v.trim().split_once('='))
            .filter(|(k, _)| *k == self.cookie);
        let found = values.next().map(|(_, value)| value);
        values.next().is_none()
            && found.is_some_and(|v| constant_equal(v.as_bytes(), self.secret.as_bytes()))
    }
    fn bootstrap(&self, path: &str) -> Option<String> {
        let rest = path.strip_prefix('/')?;
        let (secret, path) = rest.split_once('/')?;
        constant_equal(secret.as_bytes(), self.secret.as_bytes()).then(|| format!("/{path}"))
    }
}
fn constant_equal(a: &[u8], b: &[u8]) -> bool {
    // Length is public; perform the full equal-length comparison without early exit.
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}
fn response(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(text.as_bytes()))
            .map_err(|never: Infallible| match never {})
            .boxed_unsync(),
    );
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "referrer-policy",
        header::HeaderValue::from_static("no-referrer"),
    );
    response
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PreviewState {
    Open { connected_address: String },
    Closed { reason: String },
}
pub trait Events: Send + Sync {
    fn emit(&self, state: PreviewState);
    fn dismiss(&self, _id: &str) {}
}
struct Entry {
    url: String,
    stop: CancellationToken,
}
#[derive(Default)]
pub struct Previews {
    entries: Mutex<HashMap<String, Entry>>,
}
impl Previews {
    pub fn url(&self, id: &str) -> Result<String> {
        self.entries
            .lock()
            .map_err(|_| Error::Closed)?
            .get(id)
            .filter(|e| !e.stop.is_cancelled())
            .map(|e| e.url.clone())
            .ok_or(Error::Closed)
    }
    fn initial_path(&self, id: &str, path: &str) -> Result<()> {
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains(['\r', '\n', '\\', '#'])
            || path.len() > 8192
            || path.parse::<hyper::http::uri::PathAndQuery>().is_err()
        {
            return Err(Error::Invalid("Invalid preview path."));
        }
        let mut entries = self.entries.lock().map_err(|_| Error::Closed)?;
        let entry = entries.get_mut(id).ok_or(Error::Closed)?;
        entry.url.push_str(&path[1..]);
        Ok(())
    }
    pub fn close(&self, id: &str) {
        if let Ok(mut entries) = self.entries.lock()
            && let Some(entry) = entries.remove(id)
        {
            entry.stop.cancel();
        }
    }
    pub async fn open(
        self: &Arc<Self>,
        connector: Arc<dyn Connector>,
        control: CancellationToken,
        foreground: Arc<AtomicBool>,
        clock: Arc<dyn ports::Clock>,
        events: Arc<dyn Events>,
    ) -> Result<String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| Error::Unreachable)?;
        let gate = Arc::new(Gate::new(
            listener.local_addr().map_err(|_| Error::Closed)?.port(),
        )?);
        let (probe, address) = connector.open().await?;
        drop(probe);
        if control.is_cancelled() {
            return Err(Error::Closed);
        }
        let id = protocol::CommandId::new().to_string();
        let stop = CancellationToken::new();
        {
            let mut entries = self.entries.lock().map_err(|_| Error::Closed)?;
            entries.retain(|_, e| !e.stop.is_cancelled());
            if entries.len() >= 4 {
                return Err(Error::Busy);
            }
            entries.insert(
                id.clone(),
                Entry {
                    url: gate.url(),
                    stop: stop.clone(),
                },
            );
        }
        events.emit(PreviewState::Open {
            connected_address: address,
        });
        let manager = Arc::downgrade(self);
        let key = id.clone();
        tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            let activity = Arc::new(Mutex::new(clock.monotonic()));
            let mut background_at: Option<Instant> = None;
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let mut check = tokio::time::interval(Duration::from_secs(5));
            check.tick().await;
            let reason = loop {
                tokio::select! {
                    biased;
                    _ = control.cancelled() => break "lost_connection".to_owned(),
                    _ = stop.cancelled() => break "closed".to_owned(),
                    _ = tick.tick() => {
                        let now = clock.monotonic();
                        if foreground.load(Ordering::SeqCst) { background_at = None; }
                        else { background_at.get_or_insert(now); }
                        let expired = |at| now.saturating_duration_since(at) >= Duration::from_millis(PREVIEW_IDLE_TIMEOUT_MS.into());
                        if background_at.is_some_and(expired) || activity.lock().map_or(true, |at| expired(*at)) { break "idle_timeout".to_owned(); }
                    }
                    _ = check.tick() => {
                        let result = tokio::select! { biased; _ = control.cancelled() => break "lost_connection".to_owned(), _ = stop.cancelled() => break "closed".to_owned(), r = connector.open() => r };
                        if let Err(error) = result { break error.to_string(); }
                    }
                    Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                    accepted = listener.accept() => {
                        let Ok((tcp, _)) = accepted else { break "lost_connection".to_owned() };
                        if tasks.len() >= LIMIT { drop(tcp); continue; }
                        let gate = gate.clone(); let connector = connector.clone(); let activity = activity.clone(); let clock = clock.clone(); let stop = stop.clone();
                        tasks.spawn(async move {
                            let upgraded: Arc<Mutex<Option<Relay>>> = Arc::default();
                            let relay = upgraded.clone();
                            let service = service_fn(move |request| serve(request, gate.clone(), connector.clone(), activity.clone(), clock.clone(), stop.clone(), relay.clone()));
                            let mut http = hyper::server::conn::http1::Builder::new();
                            http.keep_alive(true).max_buf_size(32 * 1024).timer(hyper_util::rt::TokioTimer::new()).header_read_timeout(Duration::from_secs(10));
                            let _ = http.serve_connection(TokioIo::new(tcp), service).with_upgrades().await;
                            // An upgraded connection goes on as a relay in this task, so
                            // it counts against the limit until it closes.
                            let relay = upgraded.lock().ok().and_then(|mut relay| relay.take());
                            if let Some(relay) = relay { relay.await; }
                        });
                    }
                }
            };
            stop.cancel();
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            if let Some(manager) = manager.upgrade() {
                manager.close(&key);
            }
            events.dismiss(&key);
            events.emit(PreviewState::Closed { reason });
        });
        Ok(id)
    }
}
async fn serve(
    mut request: Request<Incoming>,
    gate: Arc<Gate>,
    connector: Arc<dyn Connector>,
    activity: Arc<Mutex<Instant>>,
    clock: Arc<dyn ports::Clock>,
    stop: CancellationToken,
    upgraded: Arc<Mutex<Option<Relay>>>,
) -> std::result::Result<Response<Body>, Infallible> {
    if !gate.same_origin(&request) {
        return Ok(response(StatusCode::FORBIDDEN, "Preview access refused"));
    }
    if let Some(path) = gate.bootstrap(request.uri().path_and_query().map_or("/", |p| p.as_str())) {
        if request.method() != hyper::Method::GET
            || path.starts_with("//")
            || path.contains(['\r', '\n', '\\'])
        {
            return Ok(response(StatusCode::FORBIDDEN, "Preview access refused"));
        }
        let mut answer = response(StatusCode::SEE_OTHER, "");
        if let (Ok(location), Ok(cookie)) = (
            header::HeaderValue::from_str(&path),
            header::HeaderValue::from_str(&format!(
                "{}={}; HttpOnly; SameSite=Strict; Path=/",
                gate.cookie,
                gate.secret.as_str()
            )),
        ) {
            answer.headers_mut().insert(header::LOCATION, location);
            answer.headers_mut().insert(header::SET_COOKIE, cookie);
        } else {
            return Ok(response(StatusCode::FORBIDDEN, "Preview access refused"));
        }
        return Ok(answer);
    }
    if !gate.cookie_ok(request.headers()) {
        return Ok(response(StatusCode::FORBIDDEN, "Preview access refused"));
    }
    touch(&activity, clock.as_ref());
    let (stream, address) = match connector.open().await {
        Ok(remote) => remote,
        Err(_) => {
            return Ok(response(
                StatusCode::BAD_GATEWAY,
                "The computer could not open this preview",
            ));
        }
    };
    let upgrade = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|h| h.as_bytes().eq_ignore_ascii_case(b"websocket"));
    if request.uri().scheme().is_some() || request.method() == hyper::Method::CONNECT {
        return Ok(response(StatusCode::FORBIDDEN, "Preview access refused"));
    }
    let local_upgrade = upgrade.then(|| hyper::upgrade::on(&mut request));
    let cookies = request
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter(|v| !v.trim_start().starts_with("preview_"))
        .collect::<Vec<_>>()
        .join(";");
    request.headers_mut().remove(header::COOKIE);
    if !cookies.is_empty()
        && let Ok(cookies) = header::HeaderValue::from_str(&cookies)
    {
        request.headers_mut().insert(header::COOKIE, cookies);
    }
    request.headers_mut().remove(header::REFERER);
    let Ok(host) = header::HeaderValue::from_str(&address) else {
        return Ok(response(StatusCode::BAD_GATEWAY, "Invalid preview address"));
    };
    request.headers_mut().insert(header::HOST, host);
    if request.headers().contains_key(header::ORIGIN)
        && let Ok(origin) = header::HeaderValue::from_str(&format!("http://{address}"))
    {
        request.headers_mut().insert(header::ORIGIN, origin);
    }
    let Ok((mut sender, connection)) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await
    else {
        return Ok(response(StatusCode::BAD_GATEWAY, "Preview connection lost"));
    };
    let connection_stop = stop.clone();
    tokio::spawn(async move {
        tokio::select! { biased; _ = connection_stop.cancelled() => {}, _ = connection.with_upgrades() => {} }
    });
    let request = request.map(|body| Active {
        body,
        activity: activity.clone(),
        clock: clock.clone(),
    });
    let answer = tokio::select! { biased; _ = stop.cancelled() => return Ok(response(StatusCode::BAD_GATEWAY, "Preview closed")), answer = sender.send_request(request) => answer };
    let Ok(mut answer) = answer else {
        return Ok(response(StatusCode::BAD_GATEWAY, "Preview connection lost"));
    };
    if answer.status() == StatusCode::SWITCHING_PROTOCOLS {
        let (Some(local), Ok(mut relay)) = (local_upgrade, upgraded.lock()) else {
            return Ok(response(
                StatusCode::BAD_GATEWAY,
                "Unexpected preview upgrade",
            ));
        };
        let remote = hyper::upgrade::on(&mut answer);
        let (activity, clock) = (activity.clone(), clock.clone());
        *relay = Some(Box::pin(async move {
            tokio::select! { biased; _ = stop.cancelled() => {}, _ = async {
                if let (Ok(local), Ok(remote)) = tokio::join!(local, remote) {
                    let mut local = TokioIo::new(local); let mut remote = TokioIo::new(remote);
                    let (mut lr, mut lw) = tokio::io::split(&mut local); let (mut rr, mut rw) = tokio::io::split(&mut remote);
                    let _ = tokio::try_join!(copy_active(&mut lr, &mut rw, &activity, clock.as_ref()), copy_active(&mut rr, &mut lw, &activity, clock.as_ref()));
                }
            } => {} }
        }));
    }
    // Upstream cannot overwrite the gate cookie or grant cross-origin access.
    let cookies: Vec<_> = answer
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter(|v| {
            v.to_str()
                .ok()
                .is_some_and(|v| !v.trim_start().starts_with("preview_"))
        })
        .cloned()
        .collect();
    answer.headers_mut().remove(header::SET_COOKIE);
    for cookie in cookies {
        answer.headers_mut().append(header::SET_COOKIE, cookie);
    }
    answer.headers_mut().remove(header::HeaderName::from_static(
        "access-control-allow-origin",
    ));
    answer.headers_mut().insert(
        "referrer-policy",
        header::HeaderValue::from_static("no-referrer"),
    );
    answer.headers_mut().append(header::CONTENT_SECURITY_POLICY, header::HeaderValue::from_static("default-src 'self' data: blob:; script-src 'self' 'unsafe-inline' 'unsafe-eval' blob:; style-src 'self' 'unsafe-inline'; connect-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'"));
    Ok(answer.map(|body| {
        Active {
            body,
            activity,
            clock,
        }
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
        .boxed_unsync()
    }))
}
/// Marks the preview in use. Bytes moving either way count, so a long
/// streamed response or upload keeps it open.
fn touch(activity: &Mutex<Instant>, clock: &dyn ports::Clock) {
    if let Ok(mut at) = activity.lock() {
        *at = clock.monotonic();
    }
}
/// A request or response body whose bytes count as activity.
struct Active<B> {
    body: B,
    activity: Arc<Mutex<Instant>>,
    clock: Arc<dyn ports::Clock>,
}
impl<B: hyper::body::Body + Unpin> hyper::body::Body for Active<B> {
    type Data = B::Data;
    type Error = B::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<std::result::Result<hyper::body::Frame<B::Data>, B::Error>>> {
        let frame = std::pin::Pin::new(&mut self.body).poll_frame(cx);
        if matches!(frame, std::task::Poll::Ready(Some(Ok(_)))) {
            touch(&self.activity, self.clock.as_ref());
        }
        frame
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_are_unique_and_cookies_require_exact_single_value() {
        let gate = Gate::new(34567).unwrap();
        let other = Gate::new(34567).unwrap();
        assert_ne!(*gate.secret, *other.secret);
        assert_ne!(gate.cookie, other.cookie);
        assert_eq!(
            gate.bootstrap(&format!("/{}/path?x=1", gate.secret.as_str())),
            Some("/path?x=1".into())
        );
        assert!(gate.bootstrap("/guess/").is_none());
        let mut headers = header::HeaderMap::new();
        assert!(!gate.cookie_ok(&headers));
        headers.insert(
            header::COOKIE,
            format!("{}={}", gate.cookie, gate.secret.as_str())
                .parse()
                .unwrap(),
        );
        assert!(gate.cookie_ok(&headers));
        assert!(!other.cookie_ok(&headers));
        headers.append(
            header::COOKIE,
            format!("{}={}", gate.cookie, gate.secret.as_str())
                .parse()
                .unwrap(),
        );
        assert!(!gate.cookie_ok(&headers));
    }
}

struct ChannelEvents(
    tauri::ipc::Channel<PreviewState>,
    crate::device::AndroidDevice,
);
impl Events for ChannelEvents {
    fn emit(&self, state: PreviewState) {
        let _ = self.0.send(state);
    }
    fn dismiss(&self, id: &str) {
        let device = self.1.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let _ = device
                .call::<()>("previewClose", serde_json::json!({"id":id}))
                .await;
        });
    }
}
#[tauri::command]
pub async fn preview_open(
    core: tauri::State<'_, crate::ipc::AppCore>,
    profile_id: String,
    preview_id: PreviewId,
    path: String,
    events: tauri::ipc::Channel<PreviewState>,
) -> Result<String> {
    let (record, owner, stop) = core.links.preview_owner(&profile_id).await?;
    let connector = Arc::new(Remote {
        record,
        vault: Arc::new(core.device.clone()),
        owner,
        preview: preview_id,
        stop: stop.clone(),
    });
    let id = core
        .previews
        .open(
            connector,
            stop,
            core.links.foreground.clone(),
            Arc::new(ports::SystemClock),
            Arc::new(ChannelEvents(events, core.device.clone())),
        )
        .await?;
    if let Err(error) = core.previews.initial_path(&id, &path) {
        core.previews.close(&id);
        return Err(error);
    }
    Ok(id)
}
#[tauri::command]
pub async fn preview_close(core: tauri::State<'_, crate::ipc::AppCore>, id: String) -> Result<()> {
    core.previews.close(&id);
    core.device
        .call("previewClose", serde_json::json!({"id":id}))
        .await
}
#[tauri::command]
pub async fn preview_show(
    core: tauri::State<'_, crate::ipc::AppCore>,
    id: String,
    profile_id: String,
    preview_id: String,
    task_id: Option<String>,
    // What the agent asked about the page and the answers so far, shown
    // beside it; the preview hands back the answers as it closes.
    questions: Option<Vec<serde_json::Value>>,
    answers: Option<Vec<serde_json::Value>>,
) -> Result<serde_json::Value> {
    let url = core.previews.url(&id)?;
    let questions = questions.unwrap_or_default();
    let answers = answers.unwrap_or_default();
    if questions.len() > 6 || answers.len() != questions.len() {
        return Err(Error::Closed);
    }
    let result = core
        .device
        .call("previewShow", serde_json::json!({"id":id,"url":url,"profileId":profile_id,"previewId":preview_id,"taskId":task_id.unwrap_or_default(),"questions":questions,"answers":answers}))
        .await;
    core.previews.close(&id);
    result
}
#[tauri::command]
pub async fn preview_picture_read(
    core: tauri::State<'_, crate::ipc::AppCore>,
    id: String,
    offset: u64,
) -> Result<tauri::ipc::Response> {
    let value: serde_json::Value = core
        .device
        .call(
            "previewPictureRead",
            serde_json::json!({"id":id,"offset":offset}),
        )
        .await?;
    let bytes = URL_SAFE_NO_PAD
        .decode(value["bytes"].as_str().ok_or(Error::Closed)?)
        .map_err(|_| Error::Closed)?;
    if bytes.len() > 256 * 1024 {
        return Err(Error::Closed);
    }
    Ok(tauri::ipc::Response::new(bytes))
}
#[tauri::command]
pub async fn preview_picture_ack(
    core: tauri::State<'_, crate::ipc::AppCore>,
    id: String,
) -> Result<()> {
    core.device
        .call("previewPictureAck", serde_json::json!({"id":id}))
        .await
}

async fn copy_active<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    activity: &Mutex<Instant>,
    clock: &dyn ports::Clock,
) -> std::io::Result<()> {
    let mut bytes = vec![0; MAX_PREVIEW_CHUNK];
    loop {
        let count = reader.read(&mut bytes).await?;
        if count == 0 {
            writer.shutdown().await?;
            return Ok(());
        }
        touch(activity, clock);
        writer.write_all(&bytes[..count]).await?;
    }
}

#[cfg(test)]
#[path = "preview/tests.rs"]
mod transport_tests;

#[tauri::command]
pub async fn preview_pending(
    core: tauri::State<'_, crate::ipc::AppCore>,
    profile_id: String,
    preview_id: String,
) -> Result<serde_json::Value> {
    core.device
        .call(
            "previewPending",
            serde_json::json!({"profileId":profile_id,"previewId":preview_id}),
        )
        .await
}
