// SPDX-License-Identifier: Apache-2.0
//! Shared by the benchmarks: a hub with synthetic ports, and a paired phone
//! that connects to the real TLS server and authenticates.
#![allow(dead_code)]
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use protocol::{auth::*, capability::HarnessKind, command::*, model::*, *};
use services::{CompanionHub, Hub, api::Caller, testing::FakeHarness};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_tungstenite::tungstenite::Message;

pub type Phone =
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

#[derive(Debug)]
struct Pin(String);
impl rustls::client::danger::ServerCertVerifier for Pin {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if companion::identity::fingerprint(end_entity) != self.0 {
            return Err(rustls::Error::General("certificate pin mismatch".into()));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 prohibited".into()))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Peak resident memory of this process so far, in megabytes.
pub fn peak_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
    line.split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<f64>()
        .unwrap()
        / 1024.0
}

/// User plus system CPU of this process, in seconds.
pub fn cpu() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<&str> = stat.rsplit_once(") ").unwrap().1.split(' ').collect();
    (fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap()) / 100.0
}

pub async fn until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(600);
    while !done() {
        assert!(Instant::now() < deadline, "workload did not settle");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// A hub with one project and one started coding task, and the task's run.
/// The hub's data and the project are separate folders under `path`.
pub async fn hub_with_run(path: &Path, harness: &Arc<FakeHarness>) -> (CompanionHub, RunId) {
    let project_dir = path.join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let hub = CompanionHub::builder()
        .data_dir(path.join("data"))
        .harness(harness.clone())
        .bridge(services::bridge::BridgeConfig::new(
            "synthetic-bridge",
            "synthetic-socket",
        ))
        .build()
        .await
        .unwrap();
    let owner = Caller::local_owner();
    let Some(CommandResult::ProjectAdded { project_id }) = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::AddProject {
                host_path: project_dir.to_string_lossy().into_owned(),
                name: Some("Bench".into()),
            }),
        )
        .await
        .result
    else {
        panic!("project");
    };
    let Some(CommandResult::TaskCreated { .. }) = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateTask {
                github: None,
                agent_chosen: None,
                mode: None,
                project_id,
                kind: TaskKind::Coding,
                title: Some("Bench task".into()),
                prompt: "bench".into(),
                bundle: None,
                harness: HarnessKind::ClaudeCode,
                model: None,
                effort: None,
                launch_policy: None,
                branch: None,
                mission: None,
                target_id: None,
                placement: TaskPlacement::StartNow,
                start: None,
                place: None,
                browser: None,
                conversation: None,
                access: None,
                review: None,
            }),
        )
        .await
        .result
    else {
        panic!("task");
    };
    until(|| !harness.runs().is_empty()).await;
    let run = harness.runs()[0];
    (hub, run)
}

pub fn activity(i: u64) -> ports::HarnessEvent {
    ports::HarnessEvent::Activity {
        key: format!("tool-{i}"),
        tool: "Bash".into(),
        summary: format!("Ran step {i} of the build"),
        status: ActivityStatus::Succeeded,
        detail: Some("cargo test -p services --lib -- --nocapture".into()),
    }
}

/// Pairs a phone whose device key is derived from `seed`.
pub async fn pair(hub: &CompanionHub, seed: u8) -> (SigningKey, DeviceId) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let ticket = hub.open_pairing(&Caller::local_owner()).await.unwrap();
    let paired = services::testing::pair_confirmed(
        hub,
        PairRequest {
            pairing_secret: ticket.secret,
            device_public_key: URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes()),
            device_name: format!("Phone {seed}"),
            device_platform: "android".into(),
            client_version: "bench".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        },
    )
    .await
    .unwrap();
    (key, paired.device_id)
}

pub fn text(value: &ClientFrame) -> Message {
    Message::Text(serde_json::to_string(value).unwrap().into())
}

pub fn frame(message: Message) -> ServerFrame {
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}

/// Connects a paired phone's control channel and authenticates it. A small
/// `receive_window` stands in for a phone's slower network path.
pub async fn connect(
    address: std::net::SocketAddr,
    pin: &str,
    key: &SigningKey,
    device_id: DeviceId,
    receive_window: Option<u32>,
) -> Phone {
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(Pin(pin.to_owned())))
    .with_no_client_auth();
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    if let Some(window) = receive_window {
        socket.set_recv_buffer_size(window).unwrap();
    }
    let tcp = socket.connect(address).await.unwrap();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(
            rustls::pki_types::ServerName::try_from("localhost").unwrap(),
            tcp,
        )
        .await
        .unwrap();
    let exporter = tls
        .get_ref()
        .1
        .export_keying_material([0u8; 32], TLS_EXPORTER_LABEL, None)
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async("wss://localhost/v1/control", tls)
        .await
        .unwrap();
    ws.send(text(&ClientFrame::Hello(Hello {
        supported: vec![ProtocolVersion::CURRENT],
        device_id,
        client_nonce: URL_SAFE_NO_PAD.encode([3; 32]),
        client_version: "bench".into(),
        channel: ConnectionKind::Control,
    })))
    .await
    .unwrap();
    let ServerFrame::Challenge(challenge) = frame(ws.next().await.unwrap().unwrap()) else {
        panic!("challenge");
    };
    let transcript = auth_transcript(
        &challenge.host_id,
        &device_id,
        ConnectionKind::Control,
        &[3; 32],
        &URL_SAFE_NO_PAD.decode(&challenge.server_nonce).unwrap(),
        &exporter,
    );
    ws.send(text(&ClientFrame::Authenticate(Authenticate {
        signature: URL_SAFE_NO_PAD.encode(key.sign(&transcript).to_bytes()),
    })))
    .await
    .unwrap();
    let ServerFrame::Authenticated(_) = frame(ws.next().await.unwrap().unwrap()) else {
        panic!("authenticated");
    };
    ws
}
