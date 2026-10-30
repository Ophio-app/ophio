// SPDX-License-Identifier: Apache-2.0
mod support;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use protocol::{
    ClientFrame, Command, CommandEnvelope, ErrorCode, ProtocolVersion, ServerFrame, auth::*,
};
use services::Hub;
use std::{sync::atomic::Ordering, time::Duration};
use support::*;

#[tokio::test]
async fn pinned_tls_authentication_and_attributed_command() {
    let fixture = Fixture::new().await;
    assert!(fixture.tls(&"00".repeat(32)).await.is_err());
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    let command = CommandEnvelope::new(Command::RunReadinessCheck);
    let command_id = command.command_id;
    client.send(&ClientFrame::Command(Box::new(command))).await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Receipt(receipt) if receipt.command_id == command_id)
    );
    assert_eq!(
        fixture.hub.callers.lock().unwrap()[0].principal,
        Principal::Device {
            device_id: fixture.hub.device
        }
    );
    assert_eq!(fixture.hub.seen.load(Ordering::SeqCst), 1);
    drop(client);
    fixture.stop().await;
}

#[tokio::test]
async fn an_address_that_appears_while_running_is_served() {
    // As when Tailscale comes up after Øphio has started.
    let fixture = Fixture::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.2:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    fixture.later.send(listener).unwrap();
    let tcp = tokio::net::TcpStream::connect(address).await.unwrap();
    let client = fixture.connect_over(ConnectionKind::Control, tcp).await;
    let mut client = fixture.authenticate(client).await;
    let command = CommandEnvelope::new(Command::RunReadinessCheck);
    let command_id = command.command_id;
    client.send(&ClientFrame::Command(Box::new(command))).await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Receipt(receipt) if receipt.command_id == command_id)
    );
    drop(client);
    fixture.stop().await;
}

#[tokio::test]
async fn rejected_unknown_revoked_and_wrong_channel_hello() {
    let fixture = Fixture::new().await;
    for (unknown, revoked, wrong_channel) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        fixture.hub.revoked.store(revoked, Ordering::SeqCst);
        let mut client = fixture.connect(ConnectionKind::Control).await;
        client
            .send(&ClientFrame::Hello(Hello {
                device_id: if unknown {
                    protocol::DeviceId::new()
                } else {
                    fixture.hub.device
                },
                supported: vec![ProtocolVersion::CURRENT],
                client_nonce: URL_SAFE_NO_PAD.encode([3u8; 32]),
                client_version: "test".into(),
                channel: if wrong_channel {
                    ConnectionKind::Media
                } else {
                    ConnectionKind::Control
                },
            }))
            .await;
        assert!(
            matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::Unauthenticated)
        );
    }
    assert_eq!(fixture.hub.seen.load(Ordering::SeqCst), 0);
    fixture.stop().await;
}

#[tokio::test]
async fn proofs_bind_key_channel_tls_session_and_fresh_challenge() {
    let fixture = Fixture::new().await;
    let mut first = fixture.connect(ConnectionKind::Control).await;
    let challenge = first.challenge(fixture.hub.device).await;
    let original = first.proof(&challenge, &fixture.hub.key, fixture.hub.device);
    let previous_exporter = first.exporter;
    first
        .send(&ClientFrame::Authenticate(original.clone()))
        .await;
    assert!(matches!(
        first.receive::<ServerFrame>().await,
        ServerFrame::Authenticated(_)
    ));
    for mode in 0..4 {
        let mut client = fixture.connect(ConnectionKind::Control).await;
        let challenge = client.challenge(fixture.hub.device).await;
        let proof = match mode {
            0 => client.proof(
                &challenge,
                &ed25519_dalek::SigningKey::from_bytes(&[8; 32]),
                fixture.hub.device,
            ),
            1 => {
                client.kind = ConnectionKind::Media;
                client.proof(&challenge, &fixture.hub.key, fixture.hub.device)
            }
            2 => {
                client.exporter = previous_exporter;
                client.proof(&challenge, &fixture.hub.key, fixture.hub.device)
            }
            _ => original.clone(),
        };
        client.send(&ClientFrame::Authenticate(proof)).await;
        assert!(
            matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::Unauthenticated)
        );
    }
    drop(first);
    fixture.stop().await;
}

#[tokio::test]
async fn auth_deadline_heartbeat_and_active_revocation() {
    let fixture = Fixture::new().await;
    let mut pending = fixture.connect(ConnectionKind::Control).await;
    // Send Hello so the server has established the absolute deadline.
    let _ = pending.challenge(fixture.hub.device).await;
    fixture.clock.advance(Duration::from_secs(11));
    assert!(
        matches!(pending.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::Expired)
    );
    let mut active = fixture.authenticated(ConnectionKind::Control).await;
    fixture.clock.advance(Duration::from_secs(31));
    assert!(
        matches!(active.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::Expired)
    );
    let mut active = fixture.authenticated(ConnectionKind::Control).await;
    fixture.hub.revoked.store(true, Ordering::SeqCst);
    assert!(
        matches!(active.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::Unauthenticated)
    );
    assert_eq!(fixture.hub.disconnected.load(Ordering::SeqCst), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn a_long_connection_keeps_its_device_last_seen_current() {
    let fixture = Fixture::new().await;
    let mut active = fixture.authenticated(ConnectionKind::Control).await;
    assert_eq!(fixture.hub.seen.load(Ordering::SeqCst), 1);
    // 75 seconds connected, never silent long enough to hit the heartbeat.
    for nonce in 0..3 {
        fixture.clock.advance(Duration::from_secs(25));
        active.send(&ClientFrame::Ping { nonce }).await;
        assert_eq!(
            active.receive::<ServerFrame>().await,
            ServerFrame::Pong { nonce }
        );
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while fixture.hub.seen.load(Ordering::SeqCst) < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "last seen was not refreshed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(fixture.hub.seen.load(Ordering::SeqCst), 2);
    drop(active);
    fixture.stop().await;
}

fn pair_request(secret: String, key: u8) -> PairRequest {
    let key = ed25519_dalek::SigningKey::from_bytes(&[key; 32]);
    PairRequest {
        pairing_secret: secret,
        device_public_key: URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
        device_name: "test phone".into(),
        device_platform: "android".into(),
        client_version: "test".into(),
        protocol: ProtocolVersion::CURRENT,
        shows_code: true,
        replaces: None,
    }
}

#[tokio::test]
async fn pairing_refuses_weak_keys_and_incompatible_majors_before_waiting() {
    let fixture = Fixture::new().await;
    let ticket = fixture
        .hub
        .open_pairing(&services::Caller::local_owner())
        .await
        .unwrap();
    let mut request = pair_request(ticket.secret, 5);
    request.protocol.major += 1;
    let (status, result) = fixture.pair(&request).await;
    assert_eq!(status, 426);
    assert_eq!(result["code"], "incompatible_version");
    request.protocol = ProtocolVersion::CURRENT;
    let mut weak_key = request.clone();
    weak_key.device_public_key = URL_SAFE_NO_PAD.encode([0u8; 32]);
    assert_eq!(fixture.pair(&weak_key).await.0, 400);
    request.pairing_secret = URL_SAFE_NO_PAD.encode([1u8; 32]);
    assert_eq!(fixture.pair(&request).await.0, 401);
    fixture.stop().await;
}

#[tokio::test]
async fn a_pairing_request_waits_past_the_sign_in_deadline_until_the_owner_confirms() {
    let fixture = Fixture::with_limits(companion::server::Limits {
        authentication: Duration::from_millis(300),
        ..Default::default()
    })
    .await;
    let owner = services::Caller::local_owner();
    let ticket = fixture.hub.open_pairing(&owner).await.unwrap();
    let request = pair_request(ticket.secret, 5);
    let pending = fixture.send_pair(&request).await;
    let prompt = fixture.waiting().await;
    // The computer shows the code the phone computes for itself.
    assert_eq!(
        prompt.code,
        pairing_code(
            &fixture.deps.identity.fingerprint(),
            &request.pairing_secret,
            &request.device_public_key
        )
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    // A second device is told another one is waiting; the owner sees one.
    let (status, result) = fixture
        .pair(&pair_request(request.pairing_secret.clone(), 6))
        .await;
    assert_eq!((status, result["code"].as_str()), (409, Some("conflict")));
    assert_eq!(fixture.waiting().await, prompt);
    fixture
        .hub
        .answer_pairing(&owner, &prompt.code, true)
        .await
        .unwrap()
        .unwrap();
    let (status, result) = pair_response(pending).await;
    assert_eq!(status, 200);
    let paired: PairResponse = serde_json::from_value(result).unwrap();
    assert!(
        fixture
            .hub
            .credentials
            .lock()
            .unwrap()
            .contains_key(&paired.device_id)
    );
    fixture.stop().await;
}

#[tokio::test]
async fn a_refused_request_is_told_so_and_enrolls_nothing() {
    let fixture = Fixture::new().await;
    let owner = services::Caller::local_owner();
    let ticket = fixture.hub.open_pairing(&owner).await.unwrap();
    let pending = fixture.send_pair(&pair_request(ticket.secret, 5)).await;
    let prompt = fixture.waiting().await;
    assert!(
        fixture
            .hub
            .answer_pairing(&owner, &prompt.code, false)
            .await
            .unwrap()
            .is_none()
    );
    let (status, result) = pair_response(pending).await;
    assert_eq!(
        (status, result["code"].as_str()),
        (403, Some("permission_denied"))
    );
    assert_eq!(fixture.hub.credentials.lock().unwrap().len(), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn an_unanswered_request_ends_when_its_window_closes() {
    let fixture = Fixture::new().await;
    let owner = services::Caller::local_owner();
    let ticket = fixture.hub.open_pairing(&owner).await.unwrap();
    let pending = fixture.send_pair(&pair_request(ticket.secret, 5)).await;
    fixture.waiting().await;
    fixture.clock.advance(Duration::from_secs(61));
    let (status, result) = pair_response(pending).await;
    assert_eq!((status, result["code"].as_str()), (401, Some("expired")));
    assert!(fixture.hub.waiting_pairing(&owner).await.unwrap().is_none());
    assert_eq!(fixture.hub.credentials.lock().unwrap().len(), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn a_phone_that_stops_waiting_withdraws_its_request() {
    let fixture = Fixture::new().await;
    let owner = services::Caller::local_owner();
    let ticket = fixture.hub.open_pairing(&owner).await.unwrap();
    let pending = fixture.send_pair(&pair_request(ticket.secret, 5)).await;
    let prompt = fixture.waiting().await;
    drop(pending);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while fixture.hub.waiting_pairing(&owner).await.unwrap().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "request not withdrawn"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        fixture
            .hub
            .answer_pairing(&owner, &prompt.code, true)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(fixture.hub.credentials.lock().unwrap().len(), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn a_phone_from_before_pairing_codes_reaches_the_owner_marked_as_showing_none() {
    let fixture = Fixture::new().await;
    let owner = services::Caller::local_owner();
    let ticket = fixture.hub.open_pairing(&owner).await.unwrap();
    let request = PairRequest {
        shows_code: false,
        replaces: None,
        ..pair_request(ticket.secret, 5)
    };
    // Exactly what such a phone sends: no field about codes.
    let body = serde_json::to_value(&request).unwrap();
    assert!(body.get("shows_code").is_none());
    let pending = fixture.send_pair(&request).await;
    let prompt = fixture.waiting().await;
    assert!(!prompt.shows_code);
    // The computer refuses it, and the phone hears the refusal.
    fixture
        .hub
        .answer_pairing(&owner, &prompt.code, false)
        .await
        .unwrap();
    assert_eq!(pair_response(pending).await.0, 403);
    fixture.stop().await;
}

#[tokio::test]
async fn incompatible_hello_and_browser_origin_are_rejected() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};
    let fixture = Fixture::new().await;
    let mut client = fixture.connect(ConnectionKind::Control).await;
    client
        .send(&ClientFrame::Hello(Hello {
            supported: vec![ProtocolVersion {
                major: ProtocolVersion::CURRENT.major + 1,
                minor: 0,
            }],
            device_id: fixture.hub.device,
            client_nonce: URL_SAFE_NO_PAD.encode([3u8; 32]),
            client_version: "test".into(),
            channel: ConnectionKind::Control,
        }))
        .await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::IncompatibleVersion)
    );
    let tls = fixture
        .tls(&fixture.deps.identity.fingerprint())
        .await
        .unwrap();
    let mut request = "wss://localhost/v1/control".into_client_request().unwrap();
    request.headers_mut().insert(
        "origin",
        HeaderValue::from_static("https://untrusted.invalid"),
    );
    let result = tokio_tungstenite::client_async(request, tls).await;
    assert!(
        matches!(result, Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status().as_u16() == 403)
    );
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client
        .socket
        .send(tokio_tungstenite::tungstenite::Message::Text("{".into()))
        .await
        .unwrap();
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::InvalidRequest)
    );
    fixture.stop().await;
}

#[tokio::test]
async fn a_stalled_request_releases_its_connection_at_the_authentication_deadline() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::with_limits(companion::server::Limits {
        authentication: Duration::from_millis(500),
        connections: 1,
        ..Default::default()
    })
    .await;
    let mut stalled = fixture
        .tls(&fixture.deps.identity.fingerprint())
        .await
        .unwrap();
    // A complete request head whose body never arrives.
    stalled
        .write_all(b"POST /v1/pair HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n")
        .await
        .unwrap();
    let closed =
        tokio::time::timeout(Duration::from_secs(5), stalled.read_to_end(&mut vec![])).await;
    assert!(closed.is_ok(), "a stalled request kept its connection");
    // The only slot is free again, and an authenticated connection outlives
    // the deadline that bounds everything before it.
    let mut active = fixture.authenticated(ConnectionKind::Control).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    active.send(&ClientFrame::Ping { nonce: 1 }).await;
    assert_eq!(
        active.receive::<ServerFrame>().await,
        ServerFrame::Pong { nonce: 1 }
    );
    drop(active);
    fixture.stop().await;
}

#[tokio::test]
async fn one_peer_cannot_hold_every_connection_before_authentication() {
    use tokio::io::AsyncReadExt;
    let fixture = Fixture::with_limits(companion::server::Limits {
        connections: 5,
        unauthenticated_per_peer: 2,
        ..Default::default()
    })
    .await;
    // Another host on the network opens connections and never speaks.
    let mut held = vec![];
    for _ in 0..4 {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.2:0".parse().unwrap()).unwrap();
        held.push(socket.connect(fixture.address).await.unwrap());
    }
    // Its connections beyond the limit are closed at once.
    let refused = tokio::time::timeout(Duration::from_secs(5), held[3].read(&mut [0; 1])).await;
    assert!(matches!(refused, Ok(Ok(0)) | Ok(Err(_))));
    // The paired phone, at its own address, still gets in, and authenticated
    // connections no longer count against that address's limit.
    let mut clients = vec![];
    for _ in 0..3 {
        clients.push(fixture.authenticated(ConnectionKind::Control).await);
    }
    drop((held, clients));
    fixture.stop().await;
}

#[tokio::test]
async fn one_device_cannot_hold_every_authenticated_connection() {
    let fixture = Fixture::with_limits(companion::server::Limits {
        connections: 8,
        authenticated_per_device: 2,
        ..Default::default()
    })
    .await;
    let mut held = vec![];
    for _ in 0..2 {
        held.push(fixture.authenticated(ConnectionKind::Control).await);
    }
    let mut refused = fixture.connect(ConnectionKind::Control).await;
    let challenge = refused.challenge(fixture.hub.device).await;
    let proof = refused.proof(&challenge, &fixture.hub.key, fixture.hub.device);
    refused.send(&ClientFrame::Authenticate(proof)).await;
    assert!(
        matches!(refused.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::LimitExceeded)
    );
    drop(refused);
    // Another paired device still connects.
    let other = protocol::DeviceId::new();
    let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    fixture.hub.credentials.lock().unwrap().insert(
        other,
        services::api::DeviceCredential {
            device_id: other,
            public_key: key.verifying_key().to_bytes(),
            grants: Grants::owner(),
            revoked: false,
        },
    );
    let mut client = fixture.connect(ConnectionKind::Control).await;
    let challenge = client.challenge(other).await;
    let proof = client.proof(&challenge, &key, other);
    client.send(&ClientFrame::Authenticate(proof)).await;
    assert!(matches!(
        client.receive::<ServerFrame>().await,
        ServerFrame::Authenticated(_)
    ));
    // A closed connection gives its device's slot back.
    drop(held.pop());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let mut retry = fixture.connect(ConnectionKind::Control).await;
        let challenge = retry.challenge(fixture.hub.device).await;
        let proof = retry.proof(&challenge, &fixture.hub.key, fixture.hub.device);
        retry.send(&ClientFrame::Authenticate(proof)).await;
        if matches!(
            retry.receive::<ServerFrame>().await,
            ServerFrame::Authenticated(_)
        ) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the slot was not released"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop((held, client));
    fixture.stop().await;
}

#[tokio::test]
async fn sign_in_frames_are_refused_by_size_before_they_are_parsed() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let fixture = Fixture::new().await;
    // Not JSON at all: a parser would call it invalid, so a size refusal
    // shows nothing was parsed.
    let oversized = format!("{{{}", "a".repeat(8 * 1024));
    let mut client = fixture.connect(ConnectionKind::Control).await;
    client
        .socket
        .send(Message::Text(oversized.clone().into()))
        .await
        .unwrap();
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::LimitExceeded)
    );
    let mut client = fixture.connect(ConnectionKind::Control).await;
    client.challenge(fixture.hub.device).await;
    client
        .socket
        .send(Message::Text(oversized.into()))
        .await
        .unwrap();
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::LimitExceeded)
    );
    // A small frame that is not JSON is still parsed, and refused as such.
    let mut client = fixture.connect(ConnectionKind::Control).await;
    client.socket.send(Message::Text("{".into())).await.unwrap();
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(error) if error.code == ErrorCode::InvalidRequest)
    );
    assert_eq!(fixture.hub.seen.load(Ordering::SeqCst), 0);
    fixture.stop().await;
}

#[tokio::test]
async fn a_message_of_a_whole_mebibyte_still_arrives_after_sign_in() {
    let fixture = Fixture::new().await;
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    let mut command = CommandEnvelope::new(Command::AddProject {
        host_path: String::new(),
        name: None,
    });
    let frame = |command: &CommandEnvelope| {
        serde_json::to_string(&ClientFrame::Command(Box::new(command.clone()))).unwrap()
    };
    let room = 1024 * 1024 - frame(&command).len();
    command.body = Command::AddProject {
        host_path: "a".repeat(room),
        name: None,
    };
    assert_eq!(frame(&command).len(), 1024 * 1024);
    let command_id = command.command_id;
    client.send(&ClientFrame::Command(Box::new(command))).await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Receipt(receipt) if receipt.command_id == command_id)
    );
    fixture.stop().await;
}
