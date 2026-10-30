// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use companion::{
    identity::Identity,
    server::{Server, ServerDeps},
};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use phone_shell::{
    device::{Audio, AudioChunk},
    error::{Error, Result},
    keys::{KeyVault, ProtectedKey},
    links::{Events, LinkEvent, LinkState, Links},
    network::{self, Socket},
    profiles::{HostProfile, Record},
};
use protocol::{auth::*, media::*, terminal::*, transfer::*, *};
use services::{Caller, CompanionHub, Hub, testing::*};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

struct Vault(SigningKey);
#[async_trait]
impl KeyVault for Vault {
    async fn generate(&self, _: &str) -> Result<Option<[u8; 32]>> {
        Ok(Some(self.0.verifying_key().to_bytes()))
    }
    async fn sign(&self, _: &str, transcript: &[u8]) -> Result<[u8; 64]> {
        Ok(self.0.sign(transcript).to_bytes())
    }
    async fn wrap(&self, _: &str, _: &[u8; 32]) -> Result<String> {
        Err(Error::Key)
    }
    async fn unwrap(&self, _: &str, _: &str) -> Result<Zeroizing<Vec<u8>>> {
        Err(Error::Key)
    }
    async fn delete(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    hub: CompanionHub,
    deps: ServerDeps,
    terminal: Arc<FakeTerminal>,
    record: Record,
    vault: Vault,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<companion::Result<()>>,
}
impl Fixture {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let terminal = Arc::new(FakeTerminal::default());
        let clock = Arc::new(FakeClock::default());
        let hub = CompanionHub::builder()
            .data_dir(dir.path())
            .clock(clock.clone())
            .desktop(Arc::new(FakeDesktop::default()))
            .media(Arc::new(FakeMedia))
            .terminal(terminal.clone())
            .speech(Arc::new(FakeSpeech))
            .build()
            .await
            .unwrap();
        let identity =
            Arc::new(Identity::generate(hub.host_id(), vec!["localhost".into()]).unwrap());
        let mut deps = ServerDeps::new(Arc::new(hub.clone()), identity).unwrap();
        deps.clock = clock;
        let server = Server::bind(deps.clone(), &["127.0.0.1:0".parse().unwrap()])
            .await
            .unwrap();
        deps.addresses = vec![server.local_addresses().unwrap()[0].to_string()].into();
        let stop = CancellationToken::new();
        let server = tokio::spawn(server.run(stop.clone()));
        let vault = Vault(SigningKey::from_bytes(&[77; 32]));
        let offer = deps.pairing_offer().await.unwrap();
        let request = PairRequest {
            pairing_secret: offer.pairing_secret.clone(),
            device_public_key: URL_SAFE_NO_PAD.encode(vault.0.verifying_key().to_bytes()),
            device_name: "Fixture phone".into(),
            device_platform: "android".into(),
            client_version: "test".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        };
        // The phone and the computer show the same code; the owner confirms it.
        let code = pairing_code(
            &offer.cert_sha256,
            &request.pairing_secret,
            &request.device_public_key,
        );
        let pairing = tokio::spawn({
            let offer = offer.clone();
            async move { network::pair(&offer, request, || Ok(())).await }
        });
        let owner = Caller::local_owner();
        while hub.waiting_pairing(&owner).await.unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        hub.answer_pairing(&owner, &code, true).await.unwrap();
        let response = pairing.await.unwrap().unwrap();
        let record = Record {
            profile: HostProfile {
                profile_id: "fixture".into(),
                host_id: response.host_id,
                name: response.host_name,
                addresses: offer.addresses,
                cert_sha256: offer.cert_sha256,
                paired_at: 0,
                device_id: response.device_id,
                key_protection: "keystore".into(),
            },
            key: ProtectedKey::Keystore,
        };
        Self {
            _dir: dir,
            hub,
            deps,
            terminal,
            record,
            vault,
            stop,
            server,
        }
    }
    async fn socket(&self, kind: ConnectionKind) -> Socket {
        network::authenticate(&self.record, kind, &self.vault, || {})
            .await
            .unwrap()
            .0
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
        self.hub.shutdown().await.unwrap();
    }
}
async fn next(socket: &mut Socket) -> Message {
    tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
async fn json<T: serde::de::DeserializeOwned>(socket: &mut Socket) -> T {
    match next(socket).await {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        _ => panic!("expected JSON"),
    }
}

#[tokio::test]
async fn authenticates_all_five_channels_pin_and_device_are_required() {
    let f = Fixture::start().await;
    for kind in [
        ConnectionKind::Control,
        ConnectionKind::Media,
        ConnectionKind::Terminal,
        ConnectionKind::Speech,
        ConnectionKind::Transfer,
    ] {
        let mut socket = f.socket(kind).await;
        socket.close(None).await.unwrap();
    }
    let mut wrong = f.record.clone();
    wrong.profile.cert_sha256 = "00".repeat(32);
    assert!(matches!(
        network::authenticate(&wrong, ConnectionKind::Control, &f.vault, || {}).await,
        Err(Error::CertificateMismatch)
    ));
    let impostor = Vault(SigningKey::from_bytes(&[78; 32]));
    assert!(
        network::authenticate(&f.record, ConnectionKind::Control, &impostor, || {})
            .await
            .is_err()
    );
    let mut control = f.socket(ConnectionKind::Control).await;
    network::send_json(
        &mut control,
        &ClientFrame::Query(QueryEnvelope {
            query_id: QueryId::new(),
            query: Query::Snapshot {
                reads_withheld: None,
            },
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        json::<ServerFrame>(&mut control).await,
        ServerFrame::QueryResult(QueryResult {
            result: Ok(QueryData::Snapshot(_)),
            ..
        })
    ));
    drop(control);
    f.close().await;
}

#[tokio::test]
async fn three_megabytes_verified_upload_download_round_trip() {
    let f = Fixture::start().await;
    let bytes: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let hash = network::fingerprint(&bytes);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CreateUpload {
                name: "fixture.bin".into(),
                size: bytes.len() as u64,
                sha256: hash.clone(),
                destination: TransferDestination::Artifact,
                evidence: None,
            }),
        )
        .await;
    let Some(protocol::command::CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("upload receipt")
    };
    let mut socket = f.socket(ConnectionKind::Transfer).await;
    network::send_json(
        &mut socket,
        &TransferClientFrame::Upload {
            transfer_id: transfer.transfer_id,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        json::<TransferServerFrame>(&mut socket).await,
        TransferServerFrame::ResumeAt { offset: 0, .. }
    ));
    for chunk in bytes.chunks(MAX_CHUNK_BYTES) {
        socket
            .send(Message::Binary(chunk.to_vec().into()))
            .await
            .unwrap();
    }
    network::send_json(
        &mut socket,
        &TransferClientFrame::Finish {
            transfer_id: transfer.transfer_id,
        },
    )
    .await
    .unwrap();
    let TransferServerFrame::Completed {
        sha256,
        artifact_id: Some(artifact_id),
        ..
    } = json(&mut socket).await
    else {
        panic!("verified upload")
    };
    assert_eq!(sha256, hash);
    drop(socket);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CreateDownload {
                source: protocol::command::DownloadSource::Artifact { artifact_id },
                shown_in_task: None,
            }),
        )
        .await;
    let Some(protocol::command::CommandResult::Transfer { transfer }) = receipt.result else {
        panic!("download receipt")
    };
    let mut socket = f.socket(ConnectionKind::Transfer).await;
    network::send_json(
        &mut socket,
        &TransferClientFrame::Download {
            transfer_id: transfer.transfer_id,
            offset: 0,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(json::<TransferServerFrame>(&mut socket).await, TransferServerFrame::Sending { size, .. } if size == bytes.len() as u64)
    );
    let mut received = Vec::new();
    loop {
        match next(&mut socket).await {
            Message::Binary(b) => {
                assert!(b.len() <= MAX_CHUNK_BYTES);
                received.extend_from_slice(&b);
            }
            Message::Text(t) => {
                assert!(
                    matches!(serde_json::from_str::<TransferServerFrame>(&t).unwrap(), TransferServerFrame::Completed { sha256, .. } if sha256 == hash)
                );
                break;
            }
            _ => panic!("unexpected transfer frame"),
        }
    }
    assert_eq!(received, bytes);
    assert_eq!(network::fingerprint(&received), hash);
    drop(socket);
    f.close().await;
}

#[tokio::test]
async fn media_terminal_speech_stream_real_companion_frames() {
    let f = Fixture::start().await;
    let QueryData::ControlTargets { targets } = f
        .hub
        .query(&Caller::local_owner(), Query::ControlTargets)
        .await
        .unwrap()
    else {
        panic!("targets")
    };
    let mut media = f.socket(ConnectionKind::Media).await;
    network::send_json(
        &mut media,
        &MediaClientFrame::Open {
            target_id: targets[0].target_id,
            params: StreamParams::default(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        json::<MediaServerFrame>(&mut media).await,
        MediaServerFrame::Opened { .. }
    ));
    let Message::Binary(frame) = next(&mut media).await else {
        panic!("media bytes")
    };
    protocol::media::parse_tile_frame(&frame).unwrap();
    drop(media);
    let credential = f
        .hub
        .device_credential(&f.record.profile.device_id)
        .await
        .unwrap();
    let caller = Caller {
        principal: Principal::Device {
            device_id: credential.device_id,
        },
        grants: credential.grants,
    };
    // Opening through the hub records the paired device as the terminal owner.
    let command = CommandEnvelope::new(Command::OpenTerminal {
        cwd: None,
        project_id: None,
        harness: None,
        cols: 80,
        rows: 24,
    });
    let command_id = command.command_id;
    let receipt = f.hub.submit(&caller, command).await;
    assert!(receipt.error.is_none(), "{:?}", receipt.error);
    let info = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let QueryData::CommandStatus { result, error, .. } = f
                .hub
                .query(&caller, Query::CommandStatus { command_id })
                .await
                .unwrap()
            else {
                panic!("command status")
            };
            assert!(error.is_none(), "{error:?}");
            if let Some(result) = result {
                let protocol::command::CommandResult::TerminalOpened { terminal } = *result else {
                    panic!("terminal result")
                };
                break terminal;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    f.terminal.output(info.terminal_id, b"sample\r\n");
    let mut terminal = f.socket(ConnectionKind::Terminal).await;
    network::send_json(
        &mut terminal,
        &TerminalClientFrame::Attach {
            terminal_id: info.terminal_id,
            from_offset: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        json::<TerminalServerFrame>(&mut terminal).await,
        TerminalServerFrame::Attached { .. }
    ));
    let Message::Binary(frame) = next(&mut terminal).await else {
        panic!("terminal bytes")
    };
    assert_eq!(
        protocol::terminal::split_output_frame(&frame),
        Some((0, b"sample\r\n".as_slice()))
    );
    drop(terminal);
    let mut speech = f.socket(ConnectionKind::Speech).await;
    network::send_json(
        &mut speech,
        &SpeechClientFrame::Start {
            session_id: SpeechSessionId::new(),
            language: None,
            keep_audio: false,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        json::<SpeechServerFrame>(&mut speech).await,
        SpeechServerFrame::Ready { .. }
    ));
    speech
        .send(Message::Binary(vec![0; 3200].into()))
        .await
        .unwrap();
    network::send_json(&mut speech, &SpeechClientFrame::Finish)
        .await
        .unwrap();
    let SpeechServerFrame::Segment { segment } = json(&mut speech).await else {
        panic!("speech segment")
    };
    assert_eq!(segment.end_ms, 100);
    assert!(matches!(
        json::<SpeechServerFrame>(&mut speech).await,
        SpeechServerFrame::Finished { .. }
    ));
    drop(speech);
    f.close().await;
}

#[tokio::test]
async fn revoked_device_is_terminal_and_certificate_rotation_is_refused() {
    let f = Fixture::start().await;
    let result = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::RevokeDevice {
                device_id: f.record.profile.device_id,
            }),
        )
        .await;
    assert!(result.error.is_none());
    assert!(
        network::authenticate(&f.record, ConnectionKind::Control, &f.vault, || {})
            .await
            .is_err()
    );
    let rotated = Arc::new(Identity::generate(f.hub.host_id(), vec!["localhost".into()]).unwrap());
    let mut deps = f.deps.clone();
    deps.identity = rotated;
    let server = Server::bind(deps, &["127.0.0.1:0".parse().unwrap()])
        .await
        .unwrap();
    let mut record = f.record.clone();
    record.profile.addresses = vec![server.local_addresses().unwrap()[0].to_string()];
    let stop = CancellationToken::new();
    let task = tokio::spawn(server.run(stop.clone()));
    assert!(matches!(
        network::authenticate(&record, ConnectionKind::Control, &f.vault, || {}).await,
        Err(Error::CertificateMismatch)
    ));
    stop.cancel();
    task.await.unwrap().unwrap();
    f.close().await;
}

/// Another device, with a certificate of its own, in front of the fixture's
/// computer and its devices.
async fn impostor(f: &Fixture) -> (SocketAddr, CancellationToken) {
    let mut deps = f.deps.clone();
    deps.identity =
        Arc::new(Identity::generate(f.hub.host_id(), vec!["localhost".into()]).unwrap());
    let server = Server::bind(deps, &["127.0.0.1:0".parse().unwrap()])
        .await
        .unwrap();
    let address = server.local_addresses().unwrap()[0];
    let stop = CancellationToken::new();
    tokio::spawn(server.run(stop.clone()));
    (address, stop)
}
/// The computer's address, answered by whichever server the test points it at.
async fn address(target: Arc<std::sync::Mutex<SocketAddr>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            let to = *target.lock().unwrap();
            tokio::spawn(async move {
                if let Ok(mut outbound) = tokio::net::TcpStream::connect(to).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    address
}
struct Seen(mpsc::UnboundedSender<LinkEvent>);
impl Events for Seen {
    fn event(&self, event: LinkEvent) -> Result<()> {
        let _ = self.0.send(event);
        Ok(())
    }
    fn binary(&self, _: Vec<u8>) -> Result<()> {
        Ok(())
    }
}
struct Silent;
#[async_trait]
impl Audio for Silent {
    async fn start(&self, _: u32) -> Result<()> {
        Ok(())
    }
    async fn read(&self, _: u32) -> Result<AudioChunk> {
        Err(Error::Closed)
    }
    async fn stop(&self, _: u32, _: bool) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
}
/// Wall time as it is, and monotonic time a minute further on at each look,
/// so each back-off the link waits passes at once while keeping its length.
#[derive(Debug, Default)]
struct Hurried(AtomicU64);
impl ports::Clock for Hurried {
    fn now(&self) -> protocol::time::Timestamp {
        protocol::time::Timestamp::now()
    }
    fn monotonic(&self) -> Instant {
        Instant::now() + Duration::from_secs(60 * self.0.fetch_add(1, Ordering::Relaxed))
    }
}
/// Opens the phone's control link to the computer at `address` and returns
/// what it reports.
async fn control_link(
    f: &Fixture,
    address: String,
) -> (Links, u32, mpsc::UnboundedReceiver<LinkEvent>) {
    let links = Links::new(
        Arc::new(Vault(f.vault.0.clone())),
        Arc::new(Silent),
        Arc::new(Hurried::default()),
    );
    let mut record = f.record.clone();
    record.profile.addresses = vec![address];
    let (events, seen) = mpsc::unbounded_channel();
    let id = links
        .open(
            links.page(),
            record,
            ConnectionKind::Control,
            Arc::new(Seen(events)),
        )
        .await
        .unwrap();
    links.connect(id).await.unwrap();
    (links, id, seen)
}
/// The next attempt's outcome: how long the link waits before the one after
/// it when another device answered, or `None` once the computer accepted it.
async fn attempt(seen: &mut mpsc::UnboundedReceiver<LinkEvent>) -> Option<i64> {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), seen.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            LinkEvent::Authenticated { .. } => return None,
            LinkEvent::State {
                state:
                    LinkState::Offline {
                        since,
                        retry_at,
                        mismatch,
                        ref reason,
                        ..
                    },
            } => {
                assert!(mismatch, "{event:?}");
                assert!(reason.starts_with("Another device answered"), "{reason}");
                return Some(retry_at.expect("the link keeps trying") - since);
            }
            LinkEvent::State {
                state: LinkState::Failed { .. },
            } => panic!("the link gave up: {event:?}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_link_that_meets_another_certificate_keeps_trying_and_recovers() {
    let f = Fixture::start().await;
    let (other, stop_other) = impostor(&f).await;
    let target = Arc::new(std::sync::Mutex::new(other));
    let (links, id, mut seen) = control_link(&f, address(target.clone()).await).await;
    assert!(attempt(&mut seen).await.is_some());
    assert!(attempt(&mut seen).await.is_some());
    // The computer answers at its address again, as when the phone is back
    // on its private network, and the link returns to normal by itself.
    *target.lock().unwrap() = f.deps.addresses.get()[0].parse().unwrap();
    while attempt(&mut seen).await.is_some() {}
    links.close(id).await;
    stop_other.cancel();
    f.close().await;
}

#[tokio::test]
async fn a_link_that_keeps_meeting_another_certificate_keeps_its_back_off() {
    let f = Fixture::start().await;
    let (other, stop_other) = impostor(&f).await;
    let (links, id, mut seen) =
        control_link(&f, address(Arc::new(std::sync::Mutex::new(other))).await).await;
    let mut waits = Vec::new();
    for _ in 0..6 {
        waits.push(attempt(&mut seen).await.expect("another device answered"));
    }
    // The usual back-off: longer each time, with jitter, up to 30 s.
    assert!(
        waits.windows(2).all(|w| w[0] < w[1] || w[1] >= 24_000),
        "{waits:?}"
    );
    assert!(
        waits.iter().all(|&w| (3_000..=30_000).contains(&w)),
        "{waits:?}"
    );
    links.close(id).await;
    stop_other.cancel();
    f.close().await;
}
