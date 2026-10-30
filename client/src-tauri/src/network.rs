// SPDX-License-Identifier: Apache-2.0
//! Pinned TLS 1.3 and protocol authentication. No CA or hostname fallback.
use crate::{
    error::{Error, Result},
    keys::{self, KeyVault},
    profiles::Record,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::{Request, client::conn::http1};
use hyper_util::rt::TokioIo;
use protocol::{ClientFrame, ProtocolVersion, ServerFrame, auth::*};
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    time::Instant,
};
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};

pub const MAX_TEXT: usize = 1024 * 1024;
// TDV1 can contain several JPEG tiles; all other binary channels are smaller.
pub const MAX_MEDIA: usize = 16 * 1024 * 1024;
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
pub type Socket = WebSocketStream<TlsStream<HeardStream<TcpStream>>>;

/// When bytes last arrived from the computer. A message can be 1 MiB and a
/// network slow, so a link that is still receiving one is not silent even
/// though no whole message has arrived for a while.
#[derive(Clone)]
pub struct Heard(Arc<Marks>);
struct Marks {
    start: Instant,
    after_ms: AtomicU64,
    wall_ms: AtomicU64,
}
impl Heard {
    fn new() -> Self {
        Self(Arc::new(Marks {
            start: Instant::now(),
            after_ms: AtomicU64::new(0),
            wall_ms: AtomicU64::new(wall_ms()),
        }))
    }
    /// How long nothing has arrived. The monotonic clock stops while the phone
    /// sleeps and the wall clock does not, so this is the longer of the two: a
    /// computer that went quiet while the phone slept is not taken for one
    /// heard moments ago.
    pub fn silent_for(&self) -> Duration {
        let awake = self.0.start.elapsed().saturating_sub(Duration::from_millis(
            self.0.after_ms.load(Ordering::Relaxed),
        ));
        let wall = wall_ms().saturating_sub(self.0.wall_ms.load(Ordering::Relaxed));
        awake.max(Duration::from_millis(wall))
    }
    fn now(&self) {
        self.0
            .after_ms
            .store(millis(self.0.start.elapsed()), Ordering::Relaxed);
        self.0.wall_ms.store(wall_ms(), Ordering::Relaxed);
    }
    /// As if the phone had slept this long since bytes last arrived.
    #[cfg(test)]
    pub fn slept(&self, time: Duration) {
        self.0.wall_ms.fetch_sub(millis(time), Ordering::Relaxed);
    }
}
fn millis(time: Duration) -> u64 {
    u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
}
fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, millis)
}
/// A connection that marks `Heard` whenever bytes arrive on it.
pub struct HeardStream<S> {
    inner: S,
    heard: Heard,
}
impl<S> HeardStream<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            heard: Heard::new(),
        }
    }
    pub fn heard(&self) -> Heard {
        self.heard.clone()
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for HeardStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        if buf.filled().len() > before {
            self.heard.now();
        }
        poll
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for HeardStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
/// When the computer at the other end of this socket was last heard.
pub fn heard(socket: &Socket) -> Heard {
    socket.get_ref().get_ref().0.heard()
}

#[derive(Debug)]
pub struct PinVerifier {
    pin: [u8; 32],
    mismatch: Arc<AtomicBool>,
}
impl PinVerifier {
    pub fn new(hex: &str) -> Result<Self> {
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::Invalid(
                "The pairing certificate fingerprint is invalid.",
            ));
        }
        let mut pin = [0; 32];
        for (out, pair) in pin.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
            *out = (digit(pair[0]) << 4) | digit(pair[1]);
        }
        Ok(Self {
            pin,
            mismatch: Arc::new(AtomicBool::new(false)),
        })
    }
}
fn digit(c: u8) -> u8 {
    if c <= b'9' { c - b'0' } else { c - b'a' + 10 }
}
pub fn fingerprint(der: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, der)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if ring::digest::digest(&ring::digest::SHA256, cert).as_ref() != self.pin {
            self.mismatch.store(true, Ordering::Relaxed);
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.3 is required".into()))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug, Clone)]
pub struct Address {
    /// `host:port` as the connection's URL and Host header carry it, with a
    /// Unicode host name in its ASCII (punycode) form.
    pub authority: String,
    host: String,
    port: u16,
}
impl Address {
    pub fn parse(value: &str) -> Result<Self> {
        let bad = || {
            Error::Invalid(
                "The computer address must be a host name or IP address followed by a port.",
            )
        };
        if value.len() > 300
            || value.chars().any(|c| c.is_whitespace() || c.is_control())
            || value.contains(['/', '?', '#', '@', '\\'])
        {
            return Err(bad());
        }
        let url = url::Url::parse(&format!("https://{value}")).map_err(|_| bad())?;
        let port: u16 = value
            .rsplit(':')
            .next()
            .and_then(|s| s.parse().ok())
            .filter(|p| *p != 0)
            .ok_or_else(bad)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(bad());
        }
        let host = url.host_str().ok_or_else(bad)?;
        Ok(Self {
            authority: format!("{host}:{port}"),
            host: host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned(),
            port,
        })
    }
}

pub async fn dial(
    addresses: &[String],
    pin: &str,
) -> Result<(TlsStream<HeardStream<TcpStream>>, Address)> {
    if addresses.is_empty() || addresses.len() > 16 {
        return Err(Error::Invalid(
            "The pairing code has no usable computer addresses.",
        ));
    }
    let parsed = addresses
        .iter()
        .map(|s| Address::parse(s))
        .collect::<Result<Vec<_>>>()?;
    let verifier = Arc::new(PinVerifier::new(pin)?);
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|_| Error::Key)?
    .dangerous()
    .with_custom_certificate_verifier(verifier.clone())
    .with_no_client_auth();
    config.enable_early_data = false;
    config.resumption = rustls::client::Resumption::disabled();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    for addr in parsed {
        let result = tokio::time::timeout(Duration::from_secs(4), async {
            let tcp = TcpStream::connect((addr.host.as_str(), addr.port))
                .await
                .map_err(|_| Error::Unreachable)?;
            tcp.set_nodelay(true).map_err(|_| Error::Unreachable)?;
            let name = ServerName::try_from(addr.host.clone())
                .map_err(|_| Error::Invalid("Invalid computer address."))?;
            connector
                .connect(name, HeardStream::new(tcp))
                .await
                .map_err(|_| Error::Unreachable)
        })
        .await;
        if let Ok(Ok(tls)) = result {
            return Ok((tls, addr));
        }
    }
    // Whoever answers at one address with another certificate must not stop
    // the paired computer being reached at the next. The mismatch is reported
    // only when no address reaches it.
    Err(if verifier.mismatch.load(Ordering::Relaxed) {
        Error::CertificateMismatch
    } else {
        Error::Unreachable
    })
}

pub fn validate_offer(offer: &PairingOffer, device_name: &str, now: i64) -> Result<()> {
    if !ProtocolVersion::CURRENT.compatible_with(&offer.protocol) {
        return Err(Error::Incompatible);
    }
    if offer.expires_at.0 <= now {
        return Err(Error::Invalid(
            "The pairing code expired. Get a new code on the computer.",
        ));
    }
    if device_name.is_empty()
        || device_name.len() > 256
        || offer.host_name.len() > 256
        || offer.addresses.is_empty()
        || offer.addresses.len() > 16
    {
        return Err(Error::Invalid("The pairing code or phone name is invalid."));
    }
    decode::<32>(&offer.pairing_secret)?;
    PinVerifier::new(&offer.cert_sha256)?;
    for addr in &offer.addresses {
        Address::parse(addr)?;
    }
    Ok(())
}

/// The computer holds a pairing request until its owner answers or the
/// five-minute window closes, and then answers it; this bounds the wait if it
/// never does.
const PAIRING_WAIT: Duration = Duration::from_secs(5 * 60 + 30);

/// Asks the computer to enroll this device. `ready` runs once the pinned
/// connection is up, just before the request is written: a code shown
/// earlier could be matched by someone racing a request while this device is
/// still dialling. An error from it withdraws the request unsent.
pub async fn pair(
    offer: &PairingOffer,
    request: PairRequest,
    ready: impl FnOnce() -> Result<()>,
) -> Result<PairResponse> {
    let (tls, addr) = dial(&offer.addresses, &offer.cert_sha256).await?;
    tokio::time::timeout(PAIRING_WAIT, async {
        let (mut sender, connection) = http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|_| Error::Closed)?;
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        let result = async {
            let body = serde_json::to_vec(&request)
                .map_err(|_| Error::Invalid("Invalid pairing request."))?;
            let request = Request::post("/v1/pair")
                .header("host", addr.authority)
                .header("content-type", "application/json")
                .header("connection", "close")
                .body(Full::new(Bytes::from(body)))
                .map_err(|_| Error::Invalid("Invalid pairing request."))?;
            ready()?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|_| Error::Closed)?;
            let status = response.status();
            let mut body = response.into_body();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.map_err(|_| Error::Closed)?.into_data() {
                    if bytes.len() + data.len() > 64 * 1024 {
                        return Err(Error::Invalid(
                            "The computer sent an oversized pairing response.",
                        ));
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            if !status.is_success() {
                return Err(Error::Remote(
                    serde_json::from_slice(&bytes).map_err(|_| Error::Unauthenticated)?,
                ));
            }
            let paired: PairResponse = serde_json::from_slice(&bytes)
                .map_err(|_| Error::Invalid("The computer sent an invalid pairing response."))?;
            if paired.host_id != offer.host_id {
                return Err(Error::Unauthenticated);
            }
            Ok(paired)
        }
        .await;
        task.abort();
        result
    })
    .await
    .map_err(|_| Error::Timeout)?
}

pub fn channel_name(kind: ConnectionKind) -> &'static str {
    match kind {
        ConnectionKind::Control => "control",
        ConnectionKind::Media => "media",
        ConnectionKind::Terminal => "terminal",
        ConnectionKind::Speech => "speech",
        ConnectionKind::Preview => "preview",
        ConnectionKind::Transfer => "transfer",
        ConnectionKind::Alerts => "alerts",
    }
}

pub async fn authenticate(
    record: &Record,
    kind: ConnectionKind,
    vault: &dyn KeyVault,
    connected: impl FnOnce(),
) -> Result<(Socket, Challenge, Authenticated)> {
    let (tls, addr) = dial(&record.profile.addresses, &record.profile.cert_sha256).await?;
    connected();
    let exporter = zeroize::Zeroizing::new(
        tls.get_ref()
            .1
            .export_keying_material([0; 32], TLS_EXPORTER_LABEL, None)
            .map_err(|_| Error::Unauthenticated)?,
    );
    tokio::time::timeout(Duration::from_secs(9), async {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MEDIA))
            .max_frame_size(Some(MAX_MEDIA))
            .write_buffer_size(0)
            .max_write_buffer_size(MAX_MEDIA + 1024);
        let (mut ws, _) = tokio_tungstenite::client_async_with_config(
            format!("wss://{}/v1/{}", addr.authority, channel_name(kind)),
            tls,
            Some(config),
        )
        .await
        .map_err(|e| match e {
            // A computer whose Øphio predates this channel has no such route.
            tokio_tungstenite::tungstenite::Error::Http(response)
                if response.status()
                    == tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND =>
            {
                Error::Incompatible
            }
            _ => Error::Closed,
        })?;
        let nonce = keys::random::<32>()?;
        send_json(
            &mut ws,
            &ClientFrame::Hello(Hello {
                supported: vec![ProtocolVersion::CURRENT],
                device_id: record.profile.device_id,
                client_nonce: URL_SAFE_NO_PAD.encode(*nonce),
                client_version: env!("CARGO_PKG_VERSION").into(),
                channel: kind,
            }),
        )
        .await?;
        let challenge = match receive_auth(&mut ws).await? {
            ServerFrame::Challenge(c) => c,
            _ => return Err(Error::Unauthenticated),
        };
        if challenge.host_id != record.profile.host_id {
            return Err(Error::Unauthenticated);
        }
        if !ProtocolVersion::CURRENT.compatible_with(&challenge.selected)
            || challenge.selected.minor > ProtocolVersion::CURRENT.minor
        {
            return Err(Error::Incompatible);
        }
        let server_nonce = decode::<32>(&challenge.server_nonce)?;
        let transcript = zeroize::Zeroizing::new(auth_transcript(
            &record.profile.host_id,
            &record.profile.device_id,
            kind,
            &*nonce,
            &server_nonce,
            &*exporter,
        ));
        let signature =
            keys::sign(vault, &record.profile.profile_id, &record.key, &transcript).await?;
        send_json(
            &mut ws,
            &ClientFrame::Authenticate(Authenticate {
                signature: URL_SAFE_NO_PAD.encode(signature),
            }),
        )
        .await?;
        let auth = match receive_auth(&mut ws).await? {
            ServerFrame::Authenticated(a) => a,
            _ => return Err(Error::Unauthenticated),
        };
        if auth.host_id != record.profile.host_id || auth.device_id != record.profile.device_id {
            return Err(Error::Unauthenticated);
        }
        Ok((ws, challenge, auth))
    })
    .await
    .map_err(|_| Error::Timeout)?
}
async fn receive_auth(ws: &mut Socket) -> Result<ServerFrame> {
    match ws
        .next()
        .await
        .ok_or(Error::Closed)?
        .map_err(|_| Error::Closed)?
    {
        Message::Text(t) if t.len() <= MAX_TEXT => {
            match serde_json::from_str(&t).map_err(|_| Error::Unauthenticated)? {
                ServerFrame::Fatal(e) => Err(Error::Remote(e)),
                other => Ok(other),
            }
        }
        _ => Err(Error::Unauthenticated),
    }
}
pub async fn send_json(ws: &mut Socket, value: &impl serde::Serialize) -> Result<()> {
    let text =
        serde_json::to_string(value).map_err(|_| Error::Invalid("Invalid protocol frame."))?;
    send(ws, Message::Text(text.into())).await
}
pub async fn send(ws: &mut Socket, message: Message) -> Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, ws.send(message))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::Closed)
}
fn decode<const N: usize>(s: &str) -> Result<[u8; N]> {
    if s.len() > N * 2 {
        return Err(Error::Unauthenticated);
    }
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::Unauthenticated)?
        .try_into()
        .map_err(|_| Error::Unauthenticated)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pin_accepts_exact_der_and_rejects_rotation() {
        let cert = rcgen::generate_simple_self_signed(vec!["computer".into()]).unwrap();
        let rotated = rcgen::generate_simple_self_signed(vec!["computer".into()]).unwrap();
        let verifier = PinVerifier::new(&fingerprint(cert.cert.der())).unwrap();
        let name = ServerName::try_from("10.0.2.2").unwrap();
        assert!(
            verifier
                .verify_server_cert(cert.cert.der(), &[], &name, &[], UnixTime::now())
                .is_ok()
        );
        assert!(
            verifier
                .verify_server_cert(rotated.cert.der(), &[], &name, &[], UnixTime::now())
                .is_err()
        );
        let wrong = PinVerifier::new(&"00".repeat(32)).unwrap();
        assert!(
            wrong
                .verify_server_cert(cert.cert.der(), &[], &name, &[], UnixTime::now())
                .is_err()
        );
        assert!(PinVerifier::new(&"AB".repeat(32)).is_err());
    }
    async fn tls_server(certified: &rcgen::CertifiedKey<rcgen::KeyPair>) -> String {
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certified.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let _ = acceptor.accept(tcp).await;
            }
        });
        address
    }
    #[tokio::test]
    async fn a_wrong_certificate_at_one_address_does_not_hide_the_computer_at_another() {
        let computer = rcgen::generate_simple_self_signed(vec!["computer".into()]).unwrap();
        let other = rcgen::generate_simple_self_signed(vec!["computer".into()]).unwrap();
        let pin = fingerprint(computer.cert.der());
        let impostor = tls_server(&other).await;
        let paired = tls_server(&computer).await;
        let closed = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().to_string()
        };
        let (_, reached) = dial(&[impostor.clone(), paired.clone()], &pin)
            .await
            .unwrap();
        assert_eq!(reached.authority, paired);
        // With no address reaching the paired computer, the mismatch is reported.
        assert!(matches!(
            dial(&[impostor.clone(), closed.clone()], &pin).await,
            Err(Error::CertificateMismatch)
        ));
        assert!(matches!(
            dial(&[closed, impostor], &pin).await,
            Err(Error::CertificateMismatch)
        ));
    }
    /// A computer that completes the pinned handshake and reports how many
    /// bytes of request reached it before the phone wrote or hung up.
    async fn listening_server(
        certified: &rcgen::CertifiedKey<rcgen::KeyPair>,
    ) -> (String, tokio::sync::mpsc::UnboundedReceiver<usize>) {
        use tokio::io::AsyncReadExt;
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certified.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (received, report) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    continue;
                };
                let mut buffer = [0; 4096];
                let _ = received.send(tls.read(&mut buffer).await.unwrap_or(0));
            }
        });
        (address, report)
    }
    #[tokio::test]
    async fn the_pairing_code_is_shown_only_once_the_pinned_computer_is_reached() {
        let computer = rcgen::generate_simple_self_signed(vec!["computer".into()]).unwrap();
        let other = rcgen::generate_simple_self_signed(vec!["computer".into()]).unwrap();
        let offer = |address: String| PairingOffer {
            protocol: ProtocolVersion::CURRENT,
            host_id: protocol::HostId::new(),
            host_name: "Workstation".into(),
            addresses: vec![address],
            cert_sha256: fingerprint(computer.cert.der()),
            pairing_secret: URL_SAFE_NO_PAD.encode([7; 32]),
            expires_at: protocol::time::Timestamp(i64::MAX),
        };
        let request = || PairRequest {
            pairing_secret: URL_SAFE_NO_PAD.encode([7; 32]),
            device_public_key: URL_SAFE_NO_PAD.encode([9; 32]),
            device_name: "Phone".into(),
            device_platform: "android".into(),
            client_version: "1.0.0".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        };
        let shown = std::cell::Cell::new(0);
        let show = || {
            shown.set(shown.get() + 1);
            Ok(())
        };
        // No code while only another certificate answers.
        let impostor = tls_server(&other).await;
        assert!(matches!(
            pair(&offer(impostor), request(), show).await,
            Err(Error::CertificateMismatch)
        ));
        assert_eq!(shown.get(), 0);
        // Reached, the code comes first; a code that could not be shown
        // withdraws the request before any of it is written.
        let (address, mut received) = listening_server(&computer).await;
        assert!(matches!(
            pair(&offer(address.clone()), request(), || Err(Error::Closed)).await,
            Err(Error::Closed)
        ));
        assert_eq!(received.recv().await, Some(0));
        // Shown, the request follows.
        let _ = pair(&offer(address), request(), show).await;
        assert_eq!(shown.get(), 1);
        assert!(received.recv().await.unwrap() > 0);
    }
    #[test]
    fn addresses_reject_ambiguous_or_injected_authorities() {
        for addr in [
            "localhost:47100",
            "10.0.2.2:47100",
            "[::1]:47100",
            "host.example:443",
        ] {
            assert!(Address::parse(addr).is_ok(), "{addr}");
        }
        for addr in [
            "localhost",
            "host:0",
            "https://host:443",
            "user@host:12",
            "host:12/path",
            "host:12\r\nX: evil",
            "host:12?x",
            "host:12#x",
        ] {
            assert!(Address::parse(addr).is_err(), "{addr}");
        }
    }
    #[test]
    fn a_unicode_host_name_is_carried_in_its_ascii_form() {
        let offer = |address: &str| PairingOffer {
            protocol: ProtocolVersion::CURRENT,
            host_id: protocol::HostId::new(),
            host_name: "Büro".into(),
            addresses: vec![address.into()],
            cert_sha256: "ab".repeat(32),
            pairing_secret: URL_SAFE_NO_PAD.encode([7; 32]),
            expires_at: protocol::time::Timestamp(i64::MAX),
        };
        for (typed, ascii) in [
            ("bücher.example:47100", "xn--bcher-kva.example:47100"),
            ("工作站.local:443", "xn--2qq276am0v.local:443"),
            ("[::1]:47100", "[::1]:47100"),
        ] {
            // A pairing link's addresses are checked before pairing starts.
            validate_offer(&offer(typed), "Phone", 0).unwrap();
            let address = Address::parse(typed).unwrap();
            assert_eq!(address.authority, ascii);
            // Pairing sends it as the Host header; later connections put it
            // in the WebSocket URL.
            hyper::header::HeaderValue::from_str(&address.authority).unwrap();
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
                format!(
                    "wss://{}/v1/{}",
                    address.authority,
                    channel_name(ConnectionKind::Control)
                ),
            )
            .unwrap();
        }
    }
    #[test]
    fn transcript_is_protocol_length_prefixed_and_channel_bound() {
        let host: protocol::HostId = "01234567-89ab-4def-8012-3456789abcde".parse().unwrap();
        let device: protocol::DeviceId = "12345678-9abc-4def-8123-456789abcdef".parse().unwrap();
        let transcript = auth_transcript(
            &host,
            &device,
            ConnectionKind::Control,
            &[1; 32],
            &[2; 32],
            &[3; 32],
        );
        let mut expected = Vec::new();
        for field in [
            b"auth-v1".as_slice(),
            host.0.as_bytes(),
            device.0.as_bytes(),
            b"control",
            &[1; 32],
            &[2; 32],
            &[3; 32],
        ] {
            expected.extend_from_slice(&(field.len() as u32).to_be_bytes());
            expected.extend_from_slice(field);
        }
        assert_eq!(transcript, expected);
        assert_ne!(
            transcript,
            auth_transcript(
                &host,
                &device,
                ConnectionKind::Transfer,
                &[1; 32],
                &[2; 32],
                &[3; 32]
            )
        );
    }
}
