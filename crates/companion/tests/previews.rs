// SPDX-License-Identifier: Apache-2.0
mod support;
use protocol::{auth::*, preview::*, *};
use std::{sync::atomic::Ordering, time::Duration};
use support::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn control(f: &Fixture) -> (Client, String) {
    let mut client = f.connect(ConnectionKind::Control).await;
    let challenge = client.challenge(f.hub.device).await;
    client
        .send(&ClientFrame::Authenticate(client.proof(
            &challenge,
            &f.hub.key,
            f.hub.device,
        )))
        .await;
    let ServerFrame::Authenticated(auth) = client.receive().await else {
        panic!("authentication")
    };
    (client, auth.control_connection_id.unwrap())
}
async fn open(f: &Fixture, owner: &str) -> Client {
    let mut side = f.authenticated(ConnectionKind::Preview).await;
    side.send(&PreviewClientFrame::Open {
        preview_id: PreviewId::new(),
        control_connection_id: Some(owner.into()),
    })
    .await;
    side
}
#[tokio::test]
async fn authentication_grants_and_exact_live_owner_are_required() {
    let f = Fixture::new().await;
    let mut side = open(&f, "missing").await;
    assert!(
        matches!(side.receive::<ServerFrame>().await, ServerFrame::Fatal(e) if e.code == ErrorCode::PermissionDenied)
    );
    assert_eq!(f.hub.preview_opens.load(Ordering::SeqCst), 0);
    let (control, id) = control(&f).await;
    f.hub
        .credentials
        .lock()
        .unwrap()
        .get_mut(&f.hub.device)
        .unwrap()
        .grants
        .tasks = false;
    let mut side = open(&f, &id).await;
    assert!(
        matches!(side.receive::<ServerFrame>().await, ServerFrame::Fatal(e) if e.code == ErrorCode::PermissionDenied)
    );
    assert_eq!(f.hub.preview_opens.load(Ordering::SeqCst), 0);
    drop(control);
    f.stop().await;
}
#[tokio::test]
async fn control_loss_closes_the_socket_and_reopening_never_replays_bytes() {
    let f = Fixture::new().await;
    let (control_link, id) = control(&f).await;
    let (stream, mut server) = tokio::io::duplex(MAX_PREVIEW_CHUNK);
    *f.hub.preview.lock().unwrap() = Some(stream);
    let mut side = open(&f, &id).await;
    assert!(matches!(
        side.receive::<PreviewServerFrame>().await,
        PreviewServerFrame::Opened { .. }
    ));
    side.binary(b"GET / HTTP/1.1\r\n\r\n").await;
    let mut bytes = [0; 18];
    server.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"GET / HTTP/1.1\r\n\r\n");
    server
        .write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")
        .await
        .unwrap();
    assert!(side.message().await.is_binary());
    side.binary(&[0x81, 2, 1, 2]).await;
    let mut frame = [0; 4];
    server.read_exact(&mut frame).await.unwrap();
    assert_eq!(frame, [0x81, 2, 1, 2]);
    drop(control_link);
    assert!(matches!(
        side.receive::<PreviewServerFrame>().await,
        PreviewServerFrame::Closed {
            reason: Some(PreviewCloseReason::ControlLost),
            ..
        }
    ));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), server.read(&mut bytes))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let mut old = open(&f, &id).await;
    assert!(matches!(
        old.receive::<ServerFrame>().await,
        ServerFrame::Fatal(_)
    ));
    let (new_control, fresh_id) = control(&f).await;
    assert_ne!(id, fresh_id);
    let (stream, mut fresh_server) = tokio::io::duplex(MAX_PREVIEW_CHUNK);
    *f.hub.preview.lock().unwrap() = Some(stream);
    let mut fresh = open(&f, &fresh_id).await;
    assert!(matches!(
        fresh.receive::<PreviewServerFrame>().await,
        PreviewServerFrame::Opened { .. }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(40), fresh_server.read(&mut bytes))
            .await
            .is_err()
    );
    fresh.send(&PreviewClientFrame::Close).await;
    assert!(matches!(
        fresh.receive::<PreviewServerFrame>().await,
        PreviewServerFrame::Closed {
            reason: Some(PreviewCloseReason::Closed),
            ..
        }
    ));
    drop(new_control);
    f.stop().await;
}
#[tokio::test]
async fn revoke_and_idle_close_blocked_streams() {
    for revoked in [true, false] {
        let f = Fixture::new().await;
        let (control, id) = control(&f).await;
        let (stream, mut server) = tokio::io::duplex(1);
        *f.hub.preview.lock().unwrap() = Some(stream);
        let mut side = open(&f, &id).await;
        assert!(matches!(
            side.receive::<PreviewServerFrame>().await,
            PreviewServerFrame::Opened { .. }
        ));
        if revoked {
            f.hub.revoked.store(true, Ordering::SeqCst);
        } else {
            f.clock
                .advance(Duration::from_millis(PREVIEW_IDLE_TIMEOUT_MS.into()));
        }
        assert!(matches!(
            side.receive::<PreviewServerFrame>().await,
            PreviewServerFrame::Closed { .. }
        ));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server.read(&mut [0]))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        drop(control);
        f.stop().await;
    }
}
