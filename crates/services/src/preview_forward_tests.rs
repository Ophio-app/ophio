// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{Hub, testing::FakeClock};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use protocol::{
    auth::{Grants, PairRequest},
    command::{Command, CommandEnvelope, CommandResult},
    time::Timestamp,
};
use tokio::net::TcpListener;

struct Fixture {
    hub: CompanionHub,
    path: std::path::PathBuf,
    clock: Arc<FakeClock>,
    caller: Caller,
}
impl Fixture {
    async fn new() -> Self {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/preview-tests")
            .join(uuid::Uuid::now_v7().to_string());
        let clock = Arc::new(FakeClock::default());
        let hub = CompanionHub::builder()
            .data_dir(&path)
            .clock(clock.clone())
            .build()
            .await
            .unwrap();
        let ticket = hub.open_pairing(&Caller::local_owner()).await.unwrap();
        let paired = crate::testing::pair_confirmed(
            &hub,
            PairRequest {
                pairing_secret: ticket.secret,
                device_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
                device_name: "Test phone".into(),
                device_platform: "test".into(),
                client_version: "test".into(),
                protocol: protocol::ProtocolVersion::CURRENT,
                shows_code: true,
                replaces: None,
            },
        )
        .await
        .unwrap();
        let caller = Caller {
            principal: Principal::Device {
                device_id: paired.device_id,
            },
            grants: paired.grants,
        };
        Self {
            hub,
            path,
            clock,
            caller,
        }
    }
    async fn preview(&self, url: String, checked: bool) -> PreviewId {
        let existing: Vec<protocol::model::Project> = self.hub.read(|r| r.list("project")).unwrap();
        let project_id = if let Some(project) = existing.first() {
            project.project_id
        } else {
            let folder = self.path.with_extension("project");
            std::fs::create_dir_all(&folder).unwrap();
            let result = self
                .hub
                .submit(
                    &Caller::local_owner(),
                    CommandEnvelope::new(Command::AddProject {
                        host_path: folder.to_string_lossy().into(),
                        name: Some("Preview test".into()),
                    }),
                )
                .await
                .result
                .unwrap();
            let CommandResult::ProjectAdded { project_id } = result else {
                panic!("project")
            };
            project_id
        };
        let result = self
            .hub
            .submit(
                &self.caller,
                CommandEnvelope::new(Command::RegisterPreview {
                    project_id,
                    label: "Preview".into(),
                    url,
                }),
            )
            .await
            .result
            .unwrap();
        let CommandResult::PreviewRegistered { preview_id } = result else {
            panic!("preview")
        };
        if checked {
            self.hub
                .write(|tx| {
                    let mut p: PreviewEndpoint = tx.read().require("preview", preview_id)?;
                    p.reachable = Some(true);
                    p.checked_at = Some(Timestamp(1));
                    tx.put("preview", preview_id, &p)
                })
                .unwrap();
        }
        preview_id
    }
    async fn finish(self) {
        self.hub.shutdown().await.unwrap();
        drop(self.hub);
        let _ = std::fs::remove_dir_all(self.path.with_extension("project"));
        std::fs::remove_dir_all(self.path).unwrap();
    }
}
async fn eof(stream: &mut DuplexStream) {
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn only_checked_registered_loopback_is_accepted() {
    let f = Fixture::new().await;
    let unchecked = f.preview("http://localhost:12345".into(), false).await;
    assert_eq!(
        f.hub
            .open_preview(&f.caller, unchecked)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let lan = f.preview("http://192.168.1.2:12345".into(), true).await;
    assert_eq!(
        f.hub.open_preview(&f.caller, lan).await.unwrap_err().code,
        ErrorCode::PathNotAllowed
    );
    assert_eq!(
        f.hub
            .open_preview(&f.caller, PreviewId::new())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let stranger = Caller {
        principal: Principal::Device {
            device_id: protocol::DeviceId::new(),
        },
        grants: Grants::owner(),
    };
    assert!(f.hub.open_preview(&stranger, lan).await.is_err());
    assert!(loopback_socket("https://localhost:443").is_err());
    assert!(loopback_socket("http://localhost.evil:12345").is_err());
    assert_eq!(
        loopback_socket("http://localhost:12345/path").unwrap(),
        SocketAddr::from(([127, 0, 0, 1], 12345))
    );
    f.finish().await;
}
#[tokio::test]
async fn http_and_websocket_upgrade_are_bidirectional() {
    let f = Fixture::new().await;
    for upgrade in [false, true] {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let id = f
            .preview(format!("http://{}", listener.local_addr().unwrap()), true)
            .await;
        let mut stream = f.hub.open_preview(&f.caller, id).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        let request = if upgrade {
            b"GET /live HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n".as_slice()
        } else {
            b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n".as_slice()
        };
        stream.write_all(request).await.unwrap();
        let mut received = vec![0; request.len()];
        server.read_exact(&mut received).await.unwrap();
        assert_eq!(received, request);
        let response = if upgrade {
            b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n"
                .as_slice()
        } else {
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".as_slice()
        };
        server.write_all(response).await.unwrap();
        let mut received = vec![0; response.len()];
        stream.read_exact(&mut received).await.unwrap();
        assert_eq!(received, response);
        if upgrade {
            let masked_frame = [0x81, 0x82, 1, 2, 3, 4, b'h' ^ 1, b'i' ^ 2];
            stream.write_all(&masked_frame).await.unwrap();
            let mut received = [0; 8];
            server.read_exact(&mut received).await.unwrap();
            assert_eq!(received, masked_frame);
            server.write_all(&[0x81, 2, b'h', b'i']).await.unwrap();
            let mut received = [0; 4];
            stream.read_exact(&mut received).await.unwrap();
            assert_eq!(received, [0x81, 2, b'h', b'i']);
        }
        drop(stream);
        drop(server);
        drop(listener);
    }
    f.finish().await;
}
#[tokio::test]
async fn closes_on_unregister_revoke_disconnect_and_idle() {
    for reason in ["unregister", "revoke", "disconnect", "idle", "close"] {
        let f = Fixture::new().await;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let id = f
            .preview(format!("http://{}", listener.local_addr().unwrap()), true)
            .await;
        let mut stream = f.hub.open_preview(&f.caller, id).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        match reason {
            "unregister" => {
                f.hub
                    .submit(
                        &f.caller,
                        CommandEnvelope::new(Command::RemovePreview { preview_id: id }),
                    )
                    .await;
            }
            "revoke" => {
                let Principal::Device { device_id } = f.caller.principal else {
                    panic!("device")
                };
                f.hub
                    .submit(
                        &Caller::local_owner(),
                        CommandEnvelope::new(Command::RevokeDevice { device_id }),
                    )
                    .await;
            }
            "disconnect" => f.hub.control_disconnected(&f.caller).await,
            "idle" => {
                tokio::task::yield_now().await;
                f.clock.advance(Duration::from_secs(301));
            }
            "close" => {
                stream.shutdown().await.unwrap();
            }
            _ => unreachable!(),
        }
        eof(&mut stream).await;
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0,
            "{reason}"
        );
        drop(stream);
        drop(server);
        drop(listener);
        f.finish().await;
    }
}
#[tokio::test]
async fn connection_limit_is_bounded() {
    let f = Fixture::new().await;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let id = f
        .preview(format!("http://{}", listener.local_addr().unwrap()), true)
        .await;
    let mut streams = Vec::new();
    let mut servers = Vec::new();
    for _ in 0..MAX_STREAMS_PER_PRINCIPAL {
        streams.push(f.hub.open_preview(&f.caller, id).await.unwrap());
        servers.push(listener.accept().await.unwrap().0);
    }
    assert_eq!(
        f.hub.open_preview(&f.caller, id).await.unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    f.hub.control_disconnected(&f.caller).await;
    for stream in &mut streams {
        eof(stream).await;
    }
    drop(streams);
    drop(servers);
    drop(listener);
    f.finish().await;
}

#[tokio::test]
async fn localhost_tries_ipv4_then_ipv6_and_reports_the_answer() {
    let f = Fixture::new().await;
    // Hold the same IPv4 port without listening, so the IPv4 attempt is refused
    // rather than reaching another test's listener under parallel load.
    let (listener, port, _ipv4) = loop {
        let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let ipv4 = tokio::net::TcpSocket::new_v4().unwrap();
        if ipv4
            .bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
            .is_ok()
        {
            break (listener, port, ipv4);
        }
    };
    let url = format!("http://localhost:{port}/");
    assert_eq!(
        loopback_sockets(&url).unwrap(),
        vec![
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, port))
        ]
    );
    let id = f.preview(url, true).await;
    let mut connection = f.hub.open_preview_connection(&f.caller, id).await.unwrap();
    assert_eq!(connection.connected_address, Some(format!("[::1]:{port}")));
    let (mut server, _) = listener.accept().await.unwrap();
    server.write_all(b"ipv6").await.unwrap();
    let mut received = [0; 4];
    connection.stream.read_exact(&mut received).await.unwrap();
    assert_eq!(&received, b"ipv6");
    f.clock.advance(Duration::from_secs(301));
    connection.closed.changed().await.unwrap();
    assert_eq!(
        *connection.closed.borrow(),
        Some(PreviewCloseReason::IdleTimeout)
    );
    eof(&mut connection.stream).await;
    f.finish().await;
}
