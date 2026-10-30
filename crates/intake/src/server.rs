// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result, Store,
    config::{self, Config},
    maintainer, validate,
};
use axum::{
    Router,
    body::to_bytes,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use protocol::{ReportId, reports::*, time::Timestamp};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;

const SMALL_BODY: usize = 32 * 1024;
const ENVELOPE_MARGIN: usize = 4096;

#[derive(Clone)]
pub struct Intake {
    store: Arc<Mutex<Store>>,
    config: Arc<Config>,
    token_hash: [u8; 32],
}
impl std::fmt::Debug for Intake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Intake").finish_non_exhaustive()
    }
}
impl Intake {
    pub fn open(data: &Path) -> Result<Self> {
        Self::new(
            Store::open(data.join("intake.sqlite"))?,
            Config::load(data)?,
            &config::read_token(&data.join("maintainer.token"))?,
        )
    }
    pub fn new(store: Store, config: Config, token: &str) -> Result<Self> {
        config.validate()?;
        if token.len() < 32 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            config: Arc::new(config),
            token_hash: Sha256::digest(token.as_bytes()).into(),
        })
    }
    pub fn reporter_router(&self) -> Router {
        Router::new()
            .route(PATH_SUBMIT, post(reporter).options(preflight))
            .route(PATH_STATUS, post(reporter).options(preflight))
            .route(PATH_ANSWER, post(reporter).options(preflight))
            .route(PATH_WITHDRAW, post(reporter).options(preflight))
            .route(PATH_RELEASES, get(reporter).options(preflight))
            .layer(middleware::from_fn(cors))
            .with_state(self.clone())
    }
    pub fn maintainer_router(&self) -> Router {
        Router::new()
            .route(maintainer::PATH_QUERY, post(maintain))
            .route(maintainer::PATH_COMMAND, post(maintain))
            .route(maintainer::PATH_STATE, get(maintain))
            .route(maintainer::PATH_COUNTERS, get(maintain))
            .route("/maintainer/v1/known", get(maintain))
            .route("/maintainer/v1/releases", get(maintain))
            .route("/maintainer/v1/reports/{id}", get(maintain))
            .with_state(self.clone())
    }
    /// Listeners are supplied by the caller, so tests bind only ephemeral loopback ports.
    pub async fn serve(self, reporter: TcpListener, maintainer: TcpListener) -> Result<()> {
        if !maintainer.local_addr()?.ip().is_loopback() {
            return Err(Error::Invalid);
        }
        self.sweep().await?;
        let cleanup = self.clone();
        let reporter_router = self.reporter_router();
        let maintainer_router = self.maintainer_router();
        let reporter = axum::serve(
            reporter,
            reporter_router.into_make_service_with_connect_info::<SocketAddr>(),
        );
        let maintainer = axum::serve(maintainer, maintainer_router);
        let maintenance = async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3600));
            loop {
                interval.tick().await;
                if cleanup.sweep().await.is_err() {
                    tracing::error!(verdict = "retention_failed");
                }
            }
        };
        tokio::select! {
            result = reporter => result.map_err(Error::Io),
            result = maintainer => result.map_err(Error::Io),
            _ = maintenance => Ok(()),
            result = tokio::signal::ctrl_c() => result.map_err(Error::Io),
        }
    }
    pub async fn sweep(&self) -> Result<usize> {
        let days = self.config.retention_days;
        self.run(move |store| store.retention_sweep(Timestamp::now(), days))
            .await
    }
    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            operation(&mut *store.lock().map_err(|_| Error::Worker)?)
        })
        .await
        .map_err(|_| Error::Worker)?
    }
    async fn rejection(&self, error: Error, reason: &'static str) -> Response {
        let _ = self.run(move |store| store.reject(reason)).await;
        tracing::info!(verdict = reason);
        error_response(error)
    }
}

async fn preflight() -> StatusCode {
    StatusCode::NO_CONTENT
}
async fn cors(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    if let Ok(allowed) = HeaderValue::from_str(&format!(
        "content-type, {KEY_HEADER}, {TIME_HEADER}, {SIGNATURE_HEADER}"
    )) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, allowed);
    }
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Retry-After"),
    );
    response
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    if headers.get_all(name).iter().count() != 1 {
        return Err(Error::Unauthorized);
    }
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or(Error::Unauthorized)
}
fn identity(
    headers: &HeaderMap,
    method: &Method,
    path: &str,
    body: &[u8],
    now: Timestamp,
) -> Result<(String, String, i64)> {
    let time: i64 = header_text(headers, TIME_HEADER)?
        .parse()
        .map_err(|_| Error::Unauthorized)?;
    if time.abs_diff(now.0) > MAX_CLOCK_SKEW_MS as u64 {
        return Err(Error::Unauthorized);
    }
    let key = URL_SAFE_NO_PAD
        .decode(header_text(headers, KEY_HEADER)?)
        .map_err(|_| Error::Unauthorized)?;
    let key: [u8; 32] = key.try_into().map_err(|_| Error::Unauthorized)?;
    let verifier = VerifyingKey::from_bytes(&key).map_err(|_| Error::Unauthorized)?;
    let signature = URL_SAFE_NO_PAD
        .decode(header_text(headers, SIGNATURE_HEADER)?)
        .map_err(|_| Error::Unauthorized)?;
    let signature = Signature::from_slice(&signature).map_err(|_| Error::Unauthorized)?;
    verifier
        .verify_strict(
            &signing_payload(method.as_str(), path, time, body),
            &signature,
        )
        .map_err(|_| Error::Unauthorized)?;
    // Store canonical signature bytes, so alternate header encodings cannot evade replay checks.
    Ok((
        fingerprint(&key),
        URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        time,
    ))
}
fn fingerprint(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn client_ip(peer: IpAddr, headers: &HeaderMap, config: &Config) -> Result<IpAddr> {
    if config.trusted_proxy == Some(peer) {
        let forwarded = header_text(headers, "x-forwarded-for").map_err(|_| Error::Invalid)?;
        forwarded.trim().parse().map_err(|_| Error::Invalid)
    } else {
        Ok(peer)
    }
}

async fn reporter(
    State(intake): State<Intake>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path().to_owned();
    let limit = if path == PATH_SUBMIT {
        MAX_REPORT_BYTES + ENVELOPE_MARGIN
    } else {
        SMALL_BODY
    };
    let body = match to_bytes(body, limit).await {
        Ok(body) => body,
        Err(_) => {
            return intake
                .rejection(Error::Invalid, "body_limit")
                .await
                .with_status(StatusCode::PAYLOAD_TOO_LARGE);
        }
    };
    let now = Timestamp::now();
    let (key, signature, signed_at) =
        match identity(&parts.headers, &parts.method, &path, &body, now) {
            Ok(identity) => identity,
            Err(error) => return intake.rejection(error, "authentication").await,
        };
    let ip = match client_ip(peer.ip(), &parts.headers, &intake.config) {
        Ok(ip) => fingerprint(ip.to_string().as_bytes()),
        Err(error) => return intake.rejection(error, "proxy_header").await,
    };
    let auth_key = key.clone();
    let auth_ip = ip.clone();
    let config = intake.config.clone();
    if let Err(error) = intake
        .run(move |store| {
            store.authorize(&auth_key, &auth_ip, &signature, signed_at, now.0, &config)
        })
        .await
    {
        let reason = match &error {
            Error::Limited(_) => "request_rate",
            Error::Refused => "banned",
            _ => "replay",
        };
        return intake.rejection(error, reason).await;
    }
    let config = intake.config.clone();
    let size = body.len();
    let result = intake
        .run(move |store| match path.as_str() {
            PATH_SUBMIT => {
                let request: IntakeSubmission = parse(&body)?;
                validate::bundle(&request.bundle)?;
                encode(store.submit(request.bundle, &key, &ip, &config, now)?)
            }
            PATH_STATUS => {
                let request: StatusRequest = parse(&body)?;
                if request.report_ids.len() > MAX_STATUS_IDS {
                    return Err(Error::Invalid);
                }
                encode(store.status(request, &key, now)?)
            }
            PATH_ANSWER => encode(store.answer(parse(&body)?, &key, now)?),
            PATH_WITHDRAW => {
                let request: WithdrawRequest = parse(&body)?;
                encode(store.withdraw(request.report_id, &key, now)?)
            }
            PATH_RELEASES => {
                if !body.is_empty() {
                    return Err(Error::Invalid);
                }
                encode(store.releases(now)?)
            }
            _ => Err(Error::NotFound),
        })
        .await;
    tracing::info!(
        bytes = size,
        verdict = if result.is_ok() {
            "accepted"
        } else {
            "refused"
        }
    );
    match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(error) => {
            let reason = match &error {
                Error::Limited(_) => "submit_rate",
                Error::Closed => "intake_closed",
                Error::Refused | Error::NotFound => "ownership_or_state",
                _ => "validation",
            };
            intake.rejection(error, reason).await
        }
    }
}

async fn maintain(State(intake): State<Intake>, request: Request) -> Response {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    let hash: [u8; 32] = Sha256::digest(supplied.as_bytes()).into();
    if !bool::from(hash.ct_eq(&intake.token_hash)) {
        return error_response(Error::Unauthorized);
    }
    let path = request.uri().path().to_owned();
    let body = match to_bytes(request.into_body(), 64 * 1024).await {
        Ok(body) => body,
        Err(_) => return error_response(Error::Invalid),
    };
    let result = intake
        .run(move |store| match path.as_str() {
            maintainer::PATH_QUERY => encode(store.list(parse(&body)?)?),
            maintainer::PATH_COMMAND => encode(store.command(parse(&body)?, Timestamp::now())?),
            maintainer::PATH_STATE => encode(store.switches()?),
            maintainer::PATH_COUNTERS => encode(store.counters()?),
            "/maintainer/v1/known" => encode(store.known_issues()?),
            "/maintainer/v1/releases" => encode(store.releases(Timestamp::now())?),
            path => {
                let id: ReportId = path
                    .strip_prefix("/maintainer/v1/reports/")
                    .ok_or(Error::NotFound)?
                    .parse()
                    .map_err(|_| Error::NotFound)?;
                encode(store.get(id)?)
            }
        })
        .await;
    match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(error) => error_response(error),
    }
}
fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|_| Error::Invalid)
}
fn encode(value: impl Serialize) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(value)?)
}
fn error_response(error: Error) -> Response {
    if matches!(error, Error::Closed) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(StatusResponse {
                reports: vec![],
                now: Timestamp::now(),
                paused: true,
            }),
        )
            .into_response();
    }
    let status = match error {
        Error::Unauthorized => StatusCode::UNAUTHORIZED,
        Error::Refused => StatusCode::FORBIDDEN,
        Error::NotFound => StatusCode::NOT_FOUND,
        Error::Closed => StatusCode::SERVICE_UNAVAILABLE,
        Error::Limited(_) => StatusCode::TOO_MANY_REQUESTS,
        Error::Invalid | Error::Json(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let mut response = (status, "Request refused").into_response();
    if let Error::Limited(seconds) = error {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&seconds.to_string()).unwrap_or(HeaderValue::from_static("1")),
        );
    }
    response
}
trait WithStatus {
    fn with_status(self, status: StatusCode) -> Self;
}
impl WithStatus for Response {
    fn with_status(mut self, status: StatusCode) -> Self {
        *self.status_mut() = status;
        self
    }
}
