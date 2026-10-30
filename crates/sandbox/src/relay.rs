// SPDX-License-Identifier: Apache-2.0
//! Model-only HTTP egress. Authentication and audit state stay on the host.
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use ports::sandbox::{RelayProvider, RelaySignal, RelaySpec, RelayTotals};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    fs::{File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::UnixListener,
    sync::{mpsc, watch},
    task::JoinSet,
};

pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 1024 * 1024;
type Body = BoxBody<Bytes, io::Error>;

#[derive(Debug, Default)]
struct Accounting {
    totals: RelayTotals,
    closed: bool,
    signalled: bool,
}
struct Shared {
    spec: RelaySpec,
    key: HeaderValue,
    upstream: String,
    client: reqwest::Client,
    accounting: Mutex<Accounting>,
    admission: Arc<tokio::sync::Mutex<()>>,
    log: Mutex<File>,
}
impl Shared {
    fn signal(&self, signal: RelaySignal) {
        if let Some(callback) = &self.spec.on_signal {
            callback(signal);
        }
    }
    fn spent(&self, a: &Accounting) -> bool {
        a.closed
            || a.totals.requests >= self.spec.budget.max_requests
            || a.totals.input_tokens >= self.spec.budget.max_input_tokens
            || a.totals.output_tokens >= self.spec.budget.max_output_tokens
            || self
                .spec
                .budget
                .deadline
                .is_some_and(|d| SystemTime::now() >= d)
    }
    fn budget_signal(&self) {
        let signal = {
            let mut a = self.accounting.lock().unwrap();
            let signal = self.spent(&a) && !a.signalled;
            a.signalled |= signal;
            signal
        };
        if signal {
            self.signal(RelaySignal::BudgetSpent);
        }
    }
    fn close(&self) {
        self.accounting.lock().unwrap().closed = true;
        self.budget_signal();
    }
}

/// One host relay per run. Drop stops its connections and joins its worker.
pub struct Relay {
    directory: tempfile::TempDir,
    stop: watch::Sender<bool>,
    worker: Option<thread::JoinHandle<()>>,
    shared: Arc<Shared>,
}
impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay")
            .field("totals", &self.totals())
            .finish_non_exhaustive()
    }
}
impl Relay {
    pub fn start(state: &Path, spec: RelaySpec) -> io::Result<Self> {
        if let Some(address) = spec.test_upstream {
            if !address.ip().is_loopback() {
                return Err(io::Error::other("test provider must be loopback"));
            }
            return Self::start_inner(state, spec, &format!("http://{address}"));
        }
        let upstream = match spec.provider {
            RelayProvider::Anthropic => "https://api.anthropic.com",
            RelayProvider::OpenAi => "https://api.openai.com",
        };
        Self::start_inner(state, spec, upstream)
    }
    #[cfg(test)]
    fn test_upstream(state: &Path, spec: RelaySpec, upstream: &str) -> io::Result<Self> {
        Self::start_inner(state, spec, upstream)
    }
    fn start_inner(state: &Path, spec: RelaySpec, upstream: &str) -> io::Result<Self> {
        let raw = std::fs::read_to_string(&spec.key_file)?;
        let mut key = HeaderValue::from_str(raw.trim())
            .map_err(|_| io::Error::other("invalid relay credential"))?;
        if key.is_empty() || raw.len() > 16384 {
            return Err(io::Error::other("invalid relay credential"));
        }
        if spec.provider == RelayProvider::OpenAi {
            key = HeaderValue::from_str(&format!("Bearer {}", raw.trim()))
                .map_err(|_| io::Error::other("invalid relay credential"))?;
        }
        key.set_sensitive(true);
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&spec.log)?;
        let key_metadata = std::fs::metadata(&spec.key_file)?;
        let log_metadata = log.metadata()?;
        if !key_metadata.is_file()
            || !log_metadata.is_file()
            || (key_metadata.dev() == log_metadata.dev()
                && key_metadata.ino() == log_metadata.ino())
        {
            return Err(io::Error::other(
                "relay credential and log must be separate regular files",
            ));
        }
        let directory = tempfile::Builder::new()
            .prefix("relay-")
            .tempdir_in(state)?;
        let listener = crate::sys::bind_unix(directory.path(), "model.sock")?;
        listener.set_nonblocking(true)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let shared = Arc::new(Shared {
            spec,
            key,
            upstream: upstream.to_owned(),
            client: reqwest::Client::builder()
                .use_rustls_tls()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .http1_only()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .map_err(io::Error::other)?,
            accounting: Mutex::new(Accounting::default()),
            admission: Arc::new(tokio::sync::Mutex::new(())),
            log: Mutex::new(log),
        });
        let (stop, mut stopped) = watch::channel(false);
        let server = shared.clone();
        let worker = thread::Builder::new().name("sandbox-relay".into()).spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = UnixListener::from_std(listener) else { return };
                let mut tasks = JoinSet::new();
                loop {
                    tokio::select! {
                        biased;
                        _ = stopped.changed() => break,
                        accepted = listener.accept(), if tasks.len() < 128 => {
                            let Ok((stream, _)) = accepted else { break };
                            let shared = server.clone();
                            tasks.spawn(async move {
                                let service = service_fn(move |request| handle(request, shared.clone()));
                                let _ = hyper::server::conn::http1::Builder::new()
                                    .keep_alive(false).max_buf_size(32 * 1024)
                                    .serve_connection(TokioIo::new(stream), service).await;
                            });
                        }
                        _ = tasks.join_next(), if !tasks.is_empty() => {}
                    }
                }
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
            });
            // Detached response readers are cancelled too; their audit guards run.
            runtime.shutdown_timeout(Duration::from_secs(1));
        })?;
        Ok(Self {
            directory,
            stop,
            worker: Some(worker),
            shared,
        })
    }
    pub fn socket(&self) -> PathBuf {
        self.directory.path().join("model.sock")
    }
    pub fn totals(&self) -> RelayTotals {
        self.shared.accounting.lock().unwrap().totals
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Audit {
    shared: Arc<Shared>,
    start: Instant,
    method: &'static str,
    path: &'static str,
    model: Option<String>,
    status: u16,
    request_bytes: u64,
    response_bytes: u64,
    input: u64,
    output: u64,
    charged_input: u64,
    charged_output: u64,
    verdict: &'static str,
    admitted: bool,
    complete: bool,
    _admission: Option<tokio::sync::OwnedMutexGuard<()>>,
}
impl Audit {
    fn admit(&mut self, value: &mut Value, token_key: &str) -> Option<Vec<u8>> {
        let mut a = self.shared.accounting.lock().unwrap();
        if self.shared.spent(&a) {
            return None;
        }
        let output = if self.path == "/v1/messages/count_tokens" {
            0
        } else {
            let remaining = self.shared.spec.budget.max_output_tokens - a.totals.output_tokens;
            let output = value[token_key]
                .as_u64()
                .unwrap_or(remaining)
                .min(remaining);
            value[token_key] = output.into();
            output
        };
        let bytes = serde_json::to_vec(value).expect("parsed request is JSON");
        // Charge one token per body byte, including JSON overhead. This is
        // deliberately more conservative than a text tokenization estimate.
        let input = self.request_bytes.max(bytes.len() as u64);
        if input > self.shared.spec.budget.max_input_tokens - a.totals.input_tokens {
            a.closed = true;
            return None;
        }
        a.totals.requests += 1;
        a.totals.input_tokens += input;
        a.totals.output_tokens += output;
        self.charged_input = input;
        self.charged_output = output;
        self.admitted = true;
        self.complete = false;
        Some(bytes)
    }
    fn usage(&mut self, input: u64, output: u64) {
        self.input = self.input.max(input);
        self.output = self.output.max(output);
        let mut a = self.shared.accounting.lock().unwrap();
        let overrun = self.input > self.charged_input || self.output > self.charged_output;
        a.totals.input_tokens = a
            .totals
            .input_tokens
            .saturating_add(self.input.saturating_sub(self.charged_input));
        a.totals.output_tokens = a
            .totals
            .output_tokens
            .saturating_add(self.output.saturating_sub(self.charged_output));
        self.charged_input = self.charged_input.max(self.input);
        self.charged_output = self.charged_output.max(self.output);
        a.closed |= overrun;
        drop(a);
        if overrun {
            self.shared.budget_signal();
        }
    }
    fn settle(&mut self) {
        let mut a = self.shared.accounting.lock().unwrap();
        a.totals.input_tokens -= self.charged_input - self.input;
        a.totals.output_tokens -= self.charged_output - self.output;
        self.charged_input = self.input;
        self.charged_output = self.output;
        drop(a);
        self.shared.budget_signal();
    }
    fn refuse(&mut self, budget: bool) -> Response<Body> {
        self.status = if budget { 429 } else { 403 };
        self.verdict = if budget {
            "budget_spent"
        } else {
            "hijack_attempt"
        };
        self.complete = true;
        if budget {
            self.shared.budget_signal();
        } else {
            self.shared.signal(RelaySignal::HijackAttempt);
        }
        empty(self.status)
    }
}
impl Drop for Audit {
    fn drop(&mut self) {
        if self.admitted && !self.complete {
            self.verdict = "incomplete";
            self.shared.close();
        }
        let line = json!({
            "time_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "method": self.method, "path": self.path, "model": self.model,
            "status": self.status, "request_bytes": self.request_bytes,
            "response_bytes": self.response_bytes, "input_tokens": self.charged_input,
            "output_tokens": self.charged_output, "duration_ms": self.start.elapsed().as_millis(),
            "verdict": self.verdict,
        });
        let mut bytes = serde_json::to_vec(&line).expect("audit metadata is JSON");
        bytes.push(b'\n');
        let result = self.shared.log.lock().unwrap().write_all(&bytes);
        if result.is_err() {
            self.shared.close();
        }
        self.shared.budget_signal();
    }
}
fn empty(status: u16) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(
            Full::new(Bytes::new())
                .map_err(|e: Infallible| match e {})
                .boxed(),
        )
        .unwrap()
}
fn endpoint(provider: RelayProvider, path: &str) -> Option<&'static str> {
    match (provider, path) {
        (RelayProvider::Anthropic, "/v1/messages") => Some("/v1/messages"),
        (RelayProvider::Anthropic, "/v1/messages/count_tokens") => {
            Some("/v1/messages/count_tokens")
        }
        (RelayProvider::OpenAi, "/v1/responses") => Some("/v1/responses"),
        _ => None,
    }
}
fn headers(inbound: &HeaderMap) -> HeaderMap {
    let connection: Vec<_> = inbound
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|v| v.trim().to_ascii_lowercase())
        .collect();
    let mut headers = HeaderMap::new();
    for name in [
        "content-type",
        "accept",
        "anthropic-version",
        "anthropic-beta",
        "user-agent",
        "x-request-id",
        "x-client-request-id",
    ] {
        if !connection.iter().any(|n| n == name) {
            for value in inbound.get_all(name) {
                headers.append(name, value.clone());
            }
        }
    }
    headers
}
// Refuse ambiguous JSON rather than depending on the upstream parser's choice
// of first or last value for a repeated model, token limit or tool definition.
struct Unique(Value);
impl<'de> serde::Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Unique, M::Error> {
                let mut object = serde_json::Map::new();
                while let Some((key, Unique(value))) = map.next_entry::<String, Unique>()? {
                    if object.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(Unique(Value::Object(object)))
            }
            fn visit_seq<S: serde::de::SeqAccess<'de>>(
                self,
                mut seq: S,
            ) -> Result<Unique, S::Error> {
                let mut values = vec![];
                while let Some(Unique(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Unique, E> {
                Ok(Unique(value.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Unique, E> {
                Ok(Unique(value.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Unique, E> {
                Ok(Unique(value.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Unique, E> {
                Ok(Unique(value.into()))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Unique, E> {
                Ok(Unique(value.into()))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Unique, E> {
                Ok(Unique(value.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

// Hosted tools and URL inputs would let an allowed model endpoint ask a
// provider to contact a third party, outside the sandbox's network policy.
fn local_inference(value: &Value) -> bool {
    if value.get("background").is_some_and(|v| v != false)
        || value
            .get("mcp_servers")
            .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
    {
        return false;
    }
    if let Some(tools) = value.get("tools") {
        let Some(tools) = tools.as_array() else {
            return false;
        };
        if tools.iter().any(|tool| {
            tool.get("type")
                .is_some_and(|t| !matches!(t.as_str(), Some("function" | "custom")))
        }) {
            return false;
        }
    }
    fn remote(value: &Value) -> bool {
        match value {
            Value::Object(object) => object.iter().any(|(key, value)| {
                (matches!(
                    key.as_str(),
                    "file_url" | "image_url" | "video_url" | "audio_url"
                ) && !value.as_str().is_some_and(|s| s.starts_with("data:")))
                    || (key == "source" && value["type"] == "url")
                    || remote(value)
            }),
            Value::Array(values) => values.iter().any(remote),
            _ => false,
        }
    }
    !remote(&value["input"]) && !remote(&value["messages"])
}

async fn handle(
    request: Request<Incoming>,
    shared: Arc<Shared>,
) -> Result<Response<Body>, Infallible> {
    let method = match request.method().as_str() {
        "POST" => "POST",
        "GET" => "GET",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "CONNECT" => "CONNECT",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "other",
    };
    // Untrusted paths and model strings must not become an alternate content log.
    let path = endpoint(
        shared.spec.provider,
        request.uri().path_and_query().map_or("", |p| p.as_str()),
    );
    let mut audit = Audit {
        shared: shared.clone(),
        start: Instant::now(),
        method,
        path: path.unwrap_or("<blocked>"),
        model: None,
        status: 502,
        request_bytes: 0,
        response_bytes: 0,
        input: 0,
        output: 0,
        charged_input: 0,
        charged_output: 0,
        verdict: "upstream_error",
        admitted: false,
        complete: true,
        _admission: None,
    };
    let invalid_route = request.version() != hyper::Version::HTTP_11
        || method != "POST"
        || path.is_none()
        || request.uri().scheme().is_some()
        || request.uri().authority().is_some()
        || request.headers().contains_key("upgrade")
        || request.headers().get_all("connection").iter().any(|v| {
            v.to_str().is_ok_and(|s| {
                s.split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
            })
        })
        || request.headers().contains_key("content-encoding")
        || request.headers().contains_key("expect");
    if request.headers().get("content-length").is_some_and(|v| {
        v.to_str()
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .is_none_or(|n| n > MAX_BODY_BYTES as u64)
    }) {
        return Ok(audit.refuse(false));
    }
    let inbound_headers = headers(request.headers());
    let mut body = request.into_body();
    let read = async {
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| ())?;
            if let Ok(data) = frame.into_data() {
                audit.request_bytes = audit.request_bytes.saturating_add(data.len() as u64);
                if bytes.len() + data.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                bytes.extend_from_slice(&data);
            }
        }
        Ok(bytes)
    };
    let bytes = match tokio::time::timeout(Duration::from_secs(10), read).await {
        Ok(Ok(bytes)) => bytes,
        _ => return Ok(audit.refuse(false)),
    };
    if shared
        .spec
        .inspect
        .as_ref()
        .is_some_and(|inspect| inspect(&bytes))
    {
        return Ok(audit.refuse(false));
    }
    let Ok(Unique(mut value)) = serde_json::from_slice::<Unique>(&bytes) else {
        return Ok(audit.refuse(false));
    };
    // Inspect decoded strings too: JSON escapes must not conceal a canary.
    fn inspect_strings(value: &Value, inspect: &ports::sandbox::InspectRelayBody) -> bool {
        match value {
            Value::String(s) => inspect(s.as_bytes()),
            Value::Array(a) => a.iter().any(|v| inspect_strings(v, inspect)),
            Value::Object(o) => o
                .iter()
                .any(|(k, v)| inspect(k.as_bytes()) || inspect_strings(v, inspect)),
            _ => false,
        }
    }
    if shared
        .spec
        .inspect
        .as_ref()
        .is_some_and(|inspect| inspect_strings(&value, inspect))
    {
        return Ok(audit.refuse(false));
    }
    if invalid_route {
        return Ok(audit.refuse(false));
    }
    let Some(model) = value.get("model").and_then(Value::as_str).filter(|s| {
        !s.is_empty()
            && s.len() <= 128
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._:/".contains(&b))
    }) else {
        return Ok(audit.refuse(false));
    };
    if shared
        .spec
        .policy
        .allowed_models
        .as_ref()
        .is_some_and(|models| !models.iter().any(|m| m == model))
    {
        return Ok(audit.refuse(false));
    }
    audit.model = Some(model.to_owned());
    if !local_inference(&value) {
        return Ok(audit.refuse(false));
    }
    let token_key = match shared.spec.provider {
        RelayProvider::Anthropic => "max_tokens",
        RelayProvider::OpenAi => "max_output_tokens",
    };
    if audit.path != "/v1/messages/count_tokens"
        && value
            .get(token_key)
            .is_some_and(|v| v.as_u64().is_none_or(|n| n == 0))
    {
        return Ok(audit.refuse(false));
    }
    if let Some(ceiling) = shared.spec.policy.max_output_tokens {
        let count = value.get(token_key).and_then(Value::as_u64);
        if audit.path != "/v1/messages/count_tokens" && count.is_none_or(|n| n == 0 || n > ceiling)
        {
            return Ok(audit.refuse(false));
        }
    }
    // Keep serial admission so queued requests can use refunded reservations.
    let remaining = shared
        .spec
        .budget
        .deadline
        .map(|d| d.duration_since(SystemTime::now()).unwrap_or_default())
        .unwrap_or(Duration::from_secs(600))
        .min(Duration::from_secs(600));
    let deadline = tokio::time::Instant::now() + remaining;
    audit._admission =
        match tokio::time::timeout_at(deadline, shared.admission.clone().lock_owned()).await {
            Ok(guard) => Some(guard),
            Err(_) => {
                shared.close();
                return Ok(audit.refuse(true));
            }
        };
    let Some(bytes) = audit.admit(&mut value, token_key) else {
        return Ok(audit.refuse(true));
    };
    let auth = match shared.spec.provider {
        RelayProvider::Anthropic => "x-api-key",
        RelayProvider::OpenAi => "authorization",
    };
    let upstream = shared
        .client
        .post(format!("{}{}", shared.upstream, audit.path))
        .headers(inbound_headers)
        .header(auth, shared.key.clone())
        .body(bytes)
        .send();
    let mut upstream = match tokio::time::timeout_at(deadline, upstream).await {
        Ok(Ok(upstream)) => upstream,
        _ => return Ok(empty(502)),
    };
    audit.status = upstream.status().as_u16();
    audit.verdict = "allowed";
    let sse = upstream
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|s| s.trim() == "text/event-stream")
        });
    let mut response = Response::builder().status(upstream.status());
    for name in ["content-type", "request-id", "x-request-id", "retry-after"] {
        if let Some(value) = upstream.headers().get(name) {
            response = response.header(name, value);
        }
    }
    let (sender, receiver) = mpsc::channel(2);
    tokio::spawn(async move {
        let mut observer = Observer::new(sse);
        let stream = async {
            while let Some(chunk) = upstream.chunk().await.map_err(io::Error::other)? {
                audit.response_bytes = audit.response_bytes.saturating_add(chunk.len() as u64);
                observer.feed(&chunk, &mut audit)?;
                // Keep observing overruns even if the sandbox disconnects.
                let _ = sender.send(Ok(hyper::body::Frame::data(chunk))).await;
            }
            observer.finish(&mut audit)?;
            Ok::<_, io::Error>(())
        };
        match tokio::time::timeout_at(deadline, stream).await {
            Ok(Ok(())) => {
                audit.complete = !sender.is_closed();
                if audit.complete && observer.accounted(audit.path) {
                    audit.settle();
                }
            }
            _ => {
                let _ = sender.try_send(Err(io::Error::other("relay response interrupted")));
            }
        }
        if audit.status < 300 && !observer.accounted(audit.path) {
            audit.complete = false;
        }
        drop(audit);
    });
    Ok(response.body(Frames(receiver).boxed()).unwrap())
}

struct Frames(mpsc::Receiver<Result<hyper::body::Frame<Bytes>, io::Error>>);
impl hyper::body::Body for Frames {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, io::Error>>> {
        self.0.poll_recv(cx)
    }
}
struct Observer {
    sse: bool,
    buffer: Vec<u8>,
    data: Vec<u8>,
    input_seen: bool,
    output_seen: bool,
    terminal: bool,
}
impl Observer {
    fn new(sse: bool) -> Self {
        Self {
            sse,
            buffer: vec![],
            data: vec![],
            input_seen: false,
            output_seen: false,
            terminal: false,
        }
    }
    fn feed(&mut self, bytes: &[u8], audit: &mut Audit) -> io::Result<()> {
        if !self.sse {
            if self.buffer.len() + bytes.len() > MAX_BODY_BYTES {
                return Err(io::Error::other("response accounting limit"));
            }
            self.buffer.extend_from_slice(bytes);
            return Ok(());
        }
        for byte in bytes {
            if *byte == b'\n' {
                let line = std::mem::take(&mut self.buffer);
                let line = line.strip_suffix(b"\r").unwrap_or(&line);
                if line.is_empty() {
                    let data = std::mem::take(&mut self.data);
                    self.observe(&data, audit);
                } else if let Some(data) = line.strip_prefix(b"data:") {
                    self.data
                        .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
                    self.data.push(b'\n');
                }
            } else {
                self.buffer.push(*byte);
            }
            if self.buffer.len() + self.data.len() > MAX_EVENT_BYTES {
                return Err(io::Error::other("event accounting limit"));
            }
        }
        Ok(())
    }
    fn finish(&mut self, audit: &mut Audit) -> io::Result<()> {
        if !self.sse {
            let data = std::mem::take(&mut self.buffer);
            self.observe(&data, audit);
        }
        Ok(())
    }
    fn accounted(&self, path: &str) -> bool {
        self.input_seen
            && (path == "/v1/messages/count_tokens" || (self.output_seen && self.terminal))
    }
    fn observe(&mut self, bytes: &[u8], audit: &mut Audit) {
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return;
        };
        let usage = match value["type"].as_str() {
            Some("message_start") => &value["message"]["usage"],
            Some("message_delta") => &value["usage"],
            Some("response.completed") => &value["response"]["usage"],
            Some(_) if self.sse => return,
            _ => value.get("usage").unwrap_or(&value),
        };
        let input = usage["input_tokens"].as_u64();
        let output = usage["output_tokens"].as_u64();
        if input.is_some() || output.is_some() {
            self.input_seen |= input.is_some();
            self.output_seen |= output.is_some();
            self.terminal |= !self.sse
                || matches!(
                    value["type"].as_str(),
                    Some("message_delta" | "response.completed")
                );
            let input = input
                .unwrap_or(audit.input)
                .saturating_add(usage["cache_creation_input_tokens"].as_u64().unwrap_or(0))
                .saturating_add(usage["cache_read_input_tokens"].as_u64().unwrap_or(0));
            audit.usage(input, output.unwrap_or(audit.output));
        }
    }
}

#[cfg(test)]
mod tests;
