// SPDX-License-Identifier: Apache-2.0
mod support;
use protocol::{
    ClientFrame, Command, CommandEnvelope, ServerFrame, Subscribe, auth::ConnectionKind,
};
use services::api::Outbound;
use std::{sync::atomic::Ordering, time::Duration};
use support::*;

async fn interventions_during_slow_request(download: bool) {
    let fixture = Fixture::new().await;
    let gate = std::sync::Arc::new(RequestGate::default());
    *fixture.hub.slow_request.lock().unwrap() = Some(gate.clone());
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    let slow = if download {
        ClientFrame::Command(Box::new(CommandEnvelope::new(Command::CreateDownload {
            source: protocol::command::DownloadSource::ProjectFile {
                project_id: protocol::ProjectId::new(),
                path: "large.bin".into(),
            },
            shown_in_task: None,
        })))
    } else {
        ClientFrame::Query(protocol::QueryEnvelope {
            query_id: protocol::QueryId::new(),
            query: protocol::Query::LibraryRead {
                root_id: protocol::LibraryRootId::new(),
                path: "slow.md".into(),
            },
        })
    };
    client.send(&slow).await;
    tokio::time::timeout(Duration::from_secs(2), gate.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        for body in [
            Command::StopTask {
                task_id: protocol::TaskId::new(),
                force: false,
            },
            Command::StopTask {
                task_id: protocol::TaskId::new(),
                force: true,
            },
            Command::PauseInput {
                target_id: protocol::TargetId::new(),
            },
        ] {
            let command = CommandEnvelope::new(body);
            client
                .send(&ClientFrame::Command(Box::new(command.clone())))
                .await;
            assert!(matches!(client.receive::<ServerFrame>().await,
                ServerFrame::Receipt(receipt) if receipt.command_id == command.command_id
                    && receipt.status == protocol::ReceiptStatus::Accepted));
            assert!(fixture.hub.commands.lock().unwrap().contains(&command));
        }
        client.send(&ClientFrame::Ping { nonce: 15 }).await;
        assert_eq!(
            client.receive::<ServerFrame>().await,
            ServerFrame::Pong { nonce: 15 }
        );
    })
    .await;
    // Release even on failure, so the reproduction does not leave work behind.
    gate.release.close();
    drop(client);
    fixture.stop().await;
    assert!(
        result.is_ok(),
        "intervention waited behind the gated request"
    );
}

#[tokio::test]
async fn slow_download_preparation_does_not_block_interventions() {
    interventions_during_slow_request(true).await;
}

#[tokio::test]
async fn slow_library_query_does_not_block_interventions() {
    interventions_during_slow_request(false).await;
}

fn library_query() -> ClientFrame {
    ClientFrame::Query(protocol::QueryEnvelope {
        query_id: protocol::QueryId::new(),
        query: protocol::Query::LibraryRead {
            root_id: protocol::LibraryRootId::new(),
            path: "slow.md".into(),
        },
    })
}

#[tokio::test]
async fn a_full_request_queue_refuses_excess_work_and_keeps_stop_available() {
    let fixture = Fixture::new().await;
    let gate = std::sync::Arc::new(RequestGate::default());
    *fixture.hub.slow_request.lock().unwrap() = Some(gate.clone());
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client.send(&library_query()).await;
    tokio::time::timeout(Duration::from_secs(2), gate.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let mut queued = Vec::new();
    for _ in 0..128 {
        let command = CommandEnvelope::new(Command::RunReadinessCheck);
        queued.push(command.command_id);
        client.send(&ClientFrame::Command(Box::new(command))).await;
    }
    let excess = CommandEnvelope::new(Command::RunReadinessCheck);
    client
        .send(&ClientFrame::Command(Box::new(excess.clone())))
        .await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Receipt(receipt)
        if receipt.command_id == excess.command_id
            && receipt.status == protocol::ReceiptStatus::Rejected
            && receipt.error.as_ref().unwrap().code == protocol::ErrorCode::LimitExceeded)
    );
    let query = library_query();
    client.send(&query).await;
    let ClientFrame::Query(query) = query else {
        unreachable!()
    };
    assert!(matches!(client.receive::<ServerFrame>().await,
        ServerFrame::QueryResult(result) if result.query_id == query.query_id
            && result.result.as_ref().unwrap_err().code == protocol::ErrorCode::LimitExceeded));
    let stop = CommandEnvelope::new(Command::StopTask {
        task_id: protocol::TaskId::new(),
        force: false,
    });
    client
        .send(&ClientFrame::Command(Box::new(stop.clone())))
        .await;
    assert!(matches!(client.receive::<ServerFrame>().await,
        ServerFrame::Receipt(receipt) if receipt.command_id == stop.command_id));
    // Heartbeats still refresh last-seen while file work remains held.
    for nonce in 0..3 {
        fixture.clock.advance(Duration::from_secs(29));
        client.send(&ClientFrame::Ping { nonce }).await;
        assert_eq!(
            client.receive::<ServerFrame>().await,
            ServerFrame::Pong { nonce }
        );
    }
    assert_eq!(fixture.hub.commands.lock().unwrap().len(), 1);
    gate.release.close();
    assert!(matches!(
        client.receive::<ServerFrame>().await,
        ServerFrame::QueryResult(_)
    ));
    for id in queued {
        assert!(matches!(client.receive::<ServerFrame>().await,
            ServerFrame::Receipt(receipt) if receipt.command_id == id
                && receipt.status == protocol::ReceiptStatus::Accepted));
    }
    drop(client);
    fixture.stop().await;
}

#[tokio::test]
async fn revocation_during_slow_work_discards_queued_commands() {
    let fixture = Fixture::new().await;
    let gate = std::sync::Arc::new(RequestGate::default());
    *fixture.hub.slow_request.lock().unwrap() = Some(gate.clone());
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client.send(&library_query()).await;
    tokio::time::timeout(Duration::from_secs(2), gate.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    client
        .send(&ClientFrame::Command(Box::new(CommandEnvelope::new(
            Command::RunReadinessCheck,
        ))))
        .await;
    client.send(&ClientFrame::Ping { nonce: 1 }).await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::Pong { nonce: 1 }
    );
    fixture.hub.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(client.receive::<ServerFrame>().await,
        ServerFrame::Fatal(error) if error.code == protocol::ErrorCode::Unauthenticated));
    let hub = fixture.hub.clone();
    drop(client);
    fixture.stop().await;
    gate.release.close();
    assert!(hub.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn backlog_caught_up_live_filter_and_resubscribe() {
    let fixture = Fixture::new().await;
    for seq in 1..=3 {
        fixture.hub.emit(seq);
    }
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client
        .send(&ClientFrame::Subscribe(Subscribe { after_seq: Some(0) }))
        .await;
    for seq in 1..=3 {
        assert!(
            matches!(client.receive::<ServerFrame>().await, ServerFrame::Event(event) if event.seq == seq)
        );
    }
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::CaughtUp { head: 3 }
    );
    fixture.hub.live.send(Outbound::Event(event(3))).unwrap();
    fixture.hub.emit(4);
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Event(event) if event.seq == 4)
    );
    client
        .send(&ClientFrame::Subscribe(Subscribe { after_seq: Some(3) }))
        .await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Event(event) if event.seq == 4)
    );
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::CaughtUp { head: 4 }
    );
    drop(client);
    fixture.stop().await;
}

#[tokio::test]
async fn queued_event_frames_leave_together_each_its_own_message() {
    let fixture = Fixture::new().await;
    let tcp = tokio::net::TcpStream::connect(fixture.address)
        .await
        .unwrap();
    let records = TlsRecords::new(tcp);
    let count = records.count.clone();
    let client = fixture.connect_over(ConnectionKind::Control, records).await;
    let mut client = fixture.authenticate(client).await;
    client
        .send(&ClientFrame::Subscribe(Subscribe { after_seq: None }))
        .await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::CaughtUp { head: 0 }
    );
    // Fewer than the hub's live buffer holds, so none is replayed.
    let before = count.load(Ordering::SeqCst);
    for seq in 1..=12 {
        fixture.hub.emit(seq);
    }
    for seq in 1..=12 {
        assert!(
            matches!(client.receive::<ServerFrame>().await, ServerFrame::Event(event) if event.seq == seq)
        );
    }
    let records = count.load(Ordering::SeqCst) - before;
    assert!(records < 4, "12 queued events took {records} TLS records");
    drop(client);
    fixture.stop().await;
}

#[tokio::test]
async fn one_hundred_thousand_events_do_not_block_receipts() {
    let fixture = Fixture::new().await;
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client
        .send(&ClientFrame::Subscribe(Subscribe { after_seq: None }))
        .await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::CaughtUp { head: 0 }
    );
    let hub = fixture.hub.clone();
    let flood = tokio::spawn(async move {
        for seq in 1..=100_000 {
            hub.emit(seq);
            if seq % 32 == 0 {
                tokio::task::yield_now().await;
            }
        }
    });
    while fixture.hub.history.lock().unwrap().len() < 128 {
        tokio::task::yield_now().await;
    }
    let command = CommandEnvelope::new(Command::StopTask {
        force: false,
        task_id: protocol::TaskId::new(),
    });
    let command_id = command.command_id;
    client.send(&ClientFrame::Command(Box::new(command))).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match client.receive::<ServerFrame>().await {
                ServerFrame::Receipt(receipt) => {
                    assert_eq!(receipt.command_id, command_id);
                    break;
                }
                ServerFrame::Fatal(error) => panic!("event flood closed command channel: {error}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    flood.await.unwrap();
    assert_eq!(fixture.hub.history.lock().unwrap().len(), 100_000);
    drop(client);
    fixture.stop().await;
}

#[tokio::test]
async fn overflowing_event_queue_resyncs_without_closing_control() {
    let fixture = Fixture::with_limits(companion::server::Limits {
        event_queue: 1,
        ..Default::default()
    })
    .await;
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client
        .send(&ClientFrame::Subscribe(Subscribe { after_seq: Some(0) }))
        .await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::CaughtUp { head: 0 }
    );
    // Deliver a burst on the single-thread runtime before the socket writer
    // can drain this deliberately one-element subscription queue.
    for seq in 1..=8 {
        fixture.hub.emit(seq);
    }
    let after = loop {
        match client.receive::<ServerFrame>().await {
            ServerFrame::SubscriptionClosed { after_seq, reason } => {
                assert_eq!(reason.code, protocol::ErrorCode::LimitExceeded);
                break after_seq;
            }
            ServerFrame::Event(_) => {}
            other => panic!("expected explicit resync, got {other:?}"),
        }
    };
    assert!(after < 8);
    client.send(&ClientFrame::Ping { nonce: 5 }).await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::Pong { nonce: 5 }
    );
    client
        .send(&ClientFrame::Subscribe(Subscribe { after_seq: Some(8) }))
        .await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::CaughtUp { head: 8 }
    );
    drop(client);
    fixture.stop().await;
}

#[tokio::test]
async fn a_long_backlog_replays_in_order_a_page_at_a_time() {
    let fixture = Fixture::with_limits(companion::server::Limits {
        event_queue: 4096,
        ..Default::default()
    })
    .await;
    for seq in 1..=1000 {
        fixture.hub.emit(seq);
    }
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client
        .send(&ClientFrame::Subscribe(Subscribe {
            after_seq: Some(10),
        }))
        .await;
    let mut seen = vec![];
    loop {
        match client.receive::<ServerFrame>().await {
            ServerFrame::Event(event) => seen.push(event.seq),
            ServerFrame::CaughtUp { head } => {
                assert_eq!(head, 1000);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seen, (11..=1000).collect::<Vec<_>>());
    assert!(fixture.hub.backlog_pages.load(Ordering::SeqCst) >= 4);
    drop(client);
    fixture.stop().await;
}

#[tokio::test]
async fn broadcast_lag_replays_retained_events() {
    let fixture = Fixture::with_limits(companion::server::Limits {
        event_queue: 512,
        ..Default::default()
    })
    .await;
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client
        .send(&ClientFrame::Subscribe(Subscribe { after_seq: Some(0) }))
        .await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::CaughtUp { head: 0 }
    );
    // Current-thread runtime: producer fills the hub ring before pump polls.
    for seq in 1..=100 {
        fixture.hub.emit(seq);
    }
    let mut seen = vec![];
    while seen.len() < 100 {
        match client.receive::<ServerFrame>().await {
            ServerFrame::Event(event) => seen.push(event.seq),
            ServerFrame::CaughtUp { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seen, (1..=100).collect::<Vec<_>>());
    assert!(fixture.hub.subscriptions.load(Ordering::SeqCst) >= 2);
    drop(client);
    fixture.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn owner_socket_uses_owner_principal_and_pairing_offer() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    // Binding creates an owner-only directory; the temporary one follows the umask.
    let path = dir.path().join("run/owner.sock");
    let local =
        companion::local::LocalServer::bind(&path, companion::started::Started::claim().unwrap())
            .await
            .unwrap();
    let task = tokio::spawn(local.run(fixture.deps.clone(), fixture.shutdown.clone()));
    let mut client = companion::local::OwnerClient::connect(&path).await.unwrap();
    let offer: companion::local::OwnerResponse = client
        .request(&companion::local::OwnerRequest::OpenPairing)
        .await
        .unwrap();
    let companion::local::OwnerResponse::PairingOffer(offer) = offer else {
        panic!("expected pairing offer");
    };
    assert_eq!(offer.host_id, fixture.hub.host);
    assert_eq!(offer.cert_sha256, fixture.deps.identity.fingerprint());
    let result: ServerFrame = client
        .request(&ClientFrame::Command(Box::new(CommandEnvelope::new(
            Command::RunReadinessCheck,
        ))))
        .await
        .unwrap();
    assert!(matches!(result, ServerFrame::Receipt(_)));
    assert_eq!(
        fixture.hub.callers.lock().unwrap()[0].principal,
        protocol::auth::Principal::LocalOwner
    );
    drop(client);
    fixture.stop().await;
    task.await.unwrap().unwrap();
    assert!(!path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn the_owner_can_ask_the_companion_to_quit() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run/owner.sock");
    let local =
        companion::local::LocalServer::bind(&path, companion::started::Started::claim().unwrap())
            .await
            .unwrap();
    let task = tokio::spawn(local.run(fixture.deps.clone(), fixture.shutdown.clone()));
    let mut client = companion::local::OwnerClient::connect(&path).await.unwrap();
    let answer: companion::local::OwnerResponse = client
        .request(&companion::local::OwnerRequest::Quit)
        .await
        .unwrap();
    assert!(matches!(answer, companion::local::OwnerResponse::Quitting));
    // Everything that shares the companion's shutdown stops, without the
    // test asking it to.
    tokio::time::timeout(Duration::from_secs(5), fixture.shutdown.cancelled())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!path.exists());
    drop(client);
    fixture.stop().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_program_the_companion_started_uses_the_owners_controls_but_never_consents() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run/owner.sock");
    let local =
        companion::local::LocalServer::bind(&path, companion::started::Started::claim().unwrap())
            .await
            .unwrap();
    let task = tokio::spawn(local.run(fixture.deps.clone(), fixture.shutdown.clone()));
    let command = |command| {
        serde_json::to_string(&ClientFrame::Command(Box::new(CommandEnvelope::new(
            command,
        ))))
        .unwrap()
    };
    // This test process stands in for the companion, so its child stands in
    // for a coding agent running a setup skill. Each request is asked on a
    // connection of its own.
    let requests = [
        r#"{"op":"open_pairing"}"#.to_owned(),
        command(Command::RunReadinessCheck),
        command(Command::RevokeDevice {
            device_id: protocol::DeviceId::new(),
        }),
        command(Command::DeleteOneOff {
            project_id: protocol::ProjectId::new(),
        }),
        command(loosened_login()),
        r#"{"op":"answer_pairing","code":"123456","confirm":true}"#.to_owned(),
        r#"{"op":"quit"}"#.to_owned(),
    ];
    let output = tokio::process::Command::new("python3")
        .args([
            "-c",
            "import socket, sys\n\
             for request in sys.argv[2:]:\n\
             \x20   s = socket.socket(socket.AF_UNIX)\n\
             \x20   s.connect(sys.argv[1])\n\
             \x20   s.sendall(request.encode() + b'\\n')\n\
             \x20   print(s.makefile().readline(), end='')\n\
             \x20   s.close()",
        ])
        .arg(&path)
        .args(&requests)
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let replies: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(replies.len(), requests.len());
    // It may use the owner's controls, as a setup skill does.
    assert!(matches!(
        serde_json::from_str(replies[0]).unwrap(),
        companion::local::OwnerResponse::PairingOffer(_)
    ));
    let ServerFrame::Receipt(receipt) = serde_json::from_str(replies[1]).unwrap() else {
        panic!("expected a receipt, not {}", replies[1]);
    };
    assert_eq!(receipt.status, protocol::ReceiptStatus::Accepted);
    // It never consents for the person, loosens a login, deletes their files,
    // nor ends Øphio.
    for reply in &replies[2..5] {
        let ServerFrame::Receipt(receipt) = serde_json::from_str(reply).unwrap() else {
            panic!("expected a receipt, not {reply}");
        };
        assert_eq!(receipt.status, protocol::ReceiptStatus::Rejected);
        assert_eq!(
            receipt.error.unwrap().code,
            protocol::ErrorCode::PermissionDenied
        );
    }
    for reply in &replies[5..] {
        let ServerFrame::Fatal(error) = serde_json::from_str(reply).unwrap() else {
            panic!("expected a refusal, not {reply}");
        };
        assert_eq!(error.code, protocol::ErrorCode::PermissionDenied);
    }
    assert!(!fixture.shutdown.is_cancelled());
    // Only what it was allowed reached Øphio, as the owner.
    let callers = fixture.hub.callers.lock().unwrap().clone();
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].principal, protocol::auth::Principal::LocalOwner);
    // The owner's own program still may.
    let mut client = companion::local::OwnerClient::connect(&path).await.unwrap();
    for command in [
        Command::RevokeDevice {
            device_id: protocol::DeviceId::new(),
        },
        loosened_login(),
    ] {
        let result: ServerFrame = client
            .request(&ClientFrame::Command(Box::new(CommandEnvelope::new(
                command,
            ))))
            .await
            .unwrap();
        assert!(
            matches!(result, ServerFrame::Receipt(receipt) if receipt.status == protocol::ReceiptStatus::Accepted)
        );
    }
    drop(client);
    fixture.stop().await;
    task.await.unwrap().unwrap();
}

/// An edit that keeps a login's site and password but adds a site of the
/// editor's choosing and lets any task use it without asking.
#[cfg(target_os = "linux")]
fn loosened_login() -> Command {
    use protocol::agent_logins::{AgentLoginAction, LoginDetails, LoginHow, LoginWho};
    Command::AgentLogin {
        action: AgentLoginAction::Edit {
            login_id: "login-1".into(),
            details: LoginDetails {
                name: "Example".into(),
                sites: vec!["example.com".into(), "elsewhere.example".into()],
                username: "owner".into(),
                notes: String::new(),
                who: LoginWho::default(),
                how: LoginHow::Whenever,
                notify: false,
            },
        },
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_program_detached_from_a_devices_terminal_cannot_quit_the_companion() {
    use ports::terminal::{TerminalHost, TerminalSpec};
    if let Err(reason) = containment::TerminalGroup::new() {
        println!("skipped: {reason}");
        return;
    }
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run/owner.sock");
    // Started from a test runner, the companion shares its group with the
    // runner's, as a companion whose group is not its own alone does.
    let local =
        companion::local::LocalServer::bind(&path, companion::started::Started::claim().unwrap())
            .await
            .unwrap();
    let task = tokio::spawn(local.run(fixture.deps.clone(), fixture.shutdown.clone()));
    // As the terminal's program is loaded, before any of the program runs,
    // something the environment has the loader run leaves a program behind in
    // a session of its own, orphaned, so nothing the companion started is its
    // parent any more. That program asks the companion to quit, which only the
    // person may, then asks again of the companion that has replaced this one.
    std::fs::write(
        dir.path().join("preload.c"),
        "#include <stdlib.h>\n\
         #include <unistd.h>\n\
         __attribute__((constructor)) static void leave(void) {\n\
           const char *detach = getenv(\"DETACH\");\n\
           if (!detach) return;\n\
           unsetenv(\"LD_PRELOAD\");\n\
           if (fork() == 0) {\n\
             execlp(\"python3\", \"python3\", detach, getenv(\"SOCKET\"), getenv(\"REPLY\"), (char *)0);\n\
             _exit(127);\n\
           }\n\
         }\n",
    )
    .unwrap();
    let compiled = std::process::Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(dir.path().join("preload.so"))
        .arg(dir.path().join("preload.c"))
        .status()
        .unwrap();
    assert!(compiled.success());
    std::fs::write(
        dir.path().join("detach.py"),
        "import os, socket, sys, time\n\
         if os.fork(): sys.exit(0)\n\
         os.setsid()\n\
         middle = os.getpid()\n\
         if os.fork(): sys.exit(0)\n\
         null = os.open(os.devnull, os.O_RDWR)\n\
         for fd in (0, 1, 2): os.dup2(null, fd)\n\
         while os.getppid() == middle: time.sleep(0.01)\n\
         def ask(path, reply):\n\
         \x20   s = socket.socket(socket.AF_UNIX)\n\
         \x20   s.connect(path)\n\
         \x20   s.sendall(b'{\"op\":\"quit\"}\\n')\n\
         \x20   open(reply + '.part', 'w').write(s.makefile().readline())\n\
         \x20   os.rename(reply + '.part', reply)\n\
         ask(sys.argv[1], sys.argv[2])\n\
         again = sys.argv[2] + '-again'\n\
         deadline = time.time() + 30\n\
         while not os.path.exists(again + '-at') and time.time() < deadline: time.sleep(0.01)\n\
         ask(open(again + '-at').read(), again)\n",
    )
    .unwrap();
    let reply = dir.path().join("reply");
    let terminals = terminal::PtyHost::default();
    terminals
        .open(TerminalSpec {
            title: "Device terminal".into(),
            cwd: dir.path().to_owned(),
            program: Some(("/bin/sh".into(), vec!["-c".into(), "sleep 30".into()])),
            env: [
                ("LD_PRELOAD", dir.path().join("preload.so")),
                ("DETACH", dir.path().join("detach.py")),
                ("SOCKET", path.clone()),
                ("REPLY", reply.clone()),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value.to_string_lossy().into_owned()))
            .collect(),
            project_id: None,
            harness: None,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();
    let refused = |reply: &std::path::Path, when: &str| {
        let reply = std::fs::read_to_string(reply).unwrap();
        let Ok(ServerFrame::Fatal(error)) = serde_json::from_str(&reply) else {
            panic!("{when}: expected a refusal, not {reply}");
        };
        assert_eq!(error.code, protocol::ErrorCode::PermissionDenied);
    };
    let arrived = |reply: std::path::PathBuf| async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !reply.exists() {
            assert!(tokio::time::Instant::now() < deadline, "no reply");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        reply
    };
    refused(&arrived(reply.clone()).await, "while its companion runs");
    assert!(fixture.hub.callers.lock().unwrap().is_empty());
    // The terminal closes and its companion is replaced by another, while
    // what the terminal left running carries on. The first still holds its
    // folder, so the other takes one of its own.
    drop(terminals);
    let replaced = dir.path().join("again/owner.sock");
    let mut stand_in = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let again = companion::local::LocalServer::bind(
        &replaced,
        companion::started::Started::of(stand_in.id()).unwrap(),
    )
    .await
    .unwrap();
    let again = tokio::spawn(again.run(fixture.deps.clone(), fixture.shutdown.clone()));
    std::fs::write(
        dir.path().join("reply-again-at"),
        replaced.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    refused(
        &arrived(dir.path().join("reply-again")).await,
        "once its companion is replaced",
    );
    assert!(fixture.hub.callers.lock().unwrap().is_empty());
    // The owner's own program still connects to the companion that replaced it.
    let mut client = companion::local::OwnerClient::connect(&replaced)
        .await
        .unwrap();
    let offer: companion::local::OwnerResponse = client
        .request(&companion::local::OwnerRequest::OpenPairing)
        .await
        .unwrap();
    assert!(matches!(
        offer,
        companion::local::OwnerResponse::PairingOffer(_)
    ));
    drop(client);
    stand_in.kill().unwrap();
    stand_in.wait().unwrap();
    fixture.stop().await;
    task.await.unwrap().unwrap();
    again.await.unwrap().unwrap();
}

#[tokio::test]
async fn unsafe_cursor_is_rejected_before_hub_and_maximum_is_exact() {
    let fixture = Fixture::new().await;
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client
        .send(&ClientFrame::Subscribe(Subscribe {
            after_seq: Some(protocol::MAX_SAFE_INTEGER + 1),
        }))
        .await;
    assert!(matches!(client.receive::<ServerFrame>().await,
        ServerFrame::Fatal(error) if error.code == protocol::ErrorCode::InvalidRequest));
    assert_eq!(fixture.hub.subscriptions.load(Ordering::SeqCst), 0);
    drop(client);
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client
        .send(&ClientFrame::Ping {
            nonce: protocol::MAX_SAFE_INTEGER,
        })
        .await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::Pong {
            nonce: protocol::MAX_SAFE_INTEGER
        }
    );
    client
        .send(&ClientFrame::Subscribe(Subscribe {
            after_seq: Some(protocol::MAX_SAFE_INTEGER),
        }))
        .await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::CaughtUp { head: 0 }
    );
    assert_eq!(fixture.hub.subscriptions.load(Ordering::SeqCst), 1);
    drop(client);
    fixture.stop().await;
}

#[tokio::test]
async fn an_answer_too_large_for_a_frame_is_refused_and_the_connection_stays() {
    let fixture = Fixture::new().await;
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    let query_id = protocol::QueryId::new();
    client
        .send(&ClientFrame::Query(protocol::QueryEnvelope {
            query_id,
            query: protocol::Query::ReusableDraft {
                task_id: Default::default(),
            },
        }))
        .await;
    assert!(matches!(client.receive::<ServerFrame>().await,
        ServerFrame::QueryResult(protocol::QueryResult { query_id: id, result: Err(error) })
            if id == query_id && error.code == protocol::ErrorCode::LimitExceeded));
    client.send(&ClientFrame::Ping { nonce: 7 }).await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::Pong { nonce: 7 }
    );
    drop(client);
    fixture.stop().await;
}

/// A phone that asks many things at once while it reads slowly, as it does
/// reloading its tasks after an update, is answered in turn, not cut off.
#[tokio::test]
async fn many_questions_at_once_are_answered_while_the_phone_reads_slowly() {
    let fixture = Fixture::new().await;
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    let asked: Vec<_> = (0..64).map(|_| protocol::QueryId::new()).collect();
    for query_id in &asked {
        client
            .send(&ClientFrame::Query(protocol::QueryEnvelope {
                query_id: *query_id,
                query: protocol::Query::ReusableDraft {
                    task_id: FITS.parse().unwrap(),
                },
            }))
            .await;
    }
    // The answers fill the socket, so the computer's writer waits for the phone.
    tokio::time::sleep(Duration::from_millis(500)).await;
    for query_id in asked {
        match client.receive::<ServerFrame>().await {
            ServerFrame::QueryResult(protocol::QueryResult {
                query_id: id,
                result,
            }) => {
                assert_eq!(id, query_id);
                assert!(result.is_ok());
            }
            other => panic!("expected an answer, got {other:?}"),
        }
    }
    drop(client);
    fixture.stop().await;
}

/// A journal event larger than a frame, as journals written before agent text
/// was bounded can hold.
fn oversized(seq: u64) -> std::sync::Arc<protocol::EventEnvelope> {
    let mut event = (*event(seq)).clone();
    event.event = protocol::Event::CommandProgress {
        command_id: protocol::CommandId::new(),
        stage: protocol::command::CommandStage::Observed,
        result: None,
        error: None,
        detail: Some("\"".repeat(600 * 1024)),
    };
    std::sync::Arc::new(event)
}

#[tokio::test]
async fn an_event_too_large_to_send_is_a_gap_and_the_connection_stays() {
    let fixture = Fixture::new().await;
    {
        let mut history = fixture.hub.history.lock().unwrap();
        history.extend([event(1), oversized(2), event(3)]);
    }
    fixture.hub.head.store(3, Ordering::SeqCst);
    let mut client = fixture.authenticated(ConnectionKind::Control).await;
    client
        .send(&ClientFrame::Subscribe(Subscribe { after_seq: Some(0) }))
        .await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Event(event) if event.seq == 1)
    );
    // The phone reloads a snapshot instead of meeting the same event at
    // every reconnect.
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::JournalGap {
            requested_from: 1,
            oldest_retained: 3
        }
    );
    client.send(&ClientFrame::Ping { nonce: 9 }).await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::Pong { nonce: 9 }
    );
    // Resubscribed from the snapshot's place, the journal carries on.
    client
        .send(&ClientFrame::Subscribe(Subscribe { after_seq: Some(3) }))
        .await;
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::CaughtUp { head: 3 }
    );
    let live = oversized(4);
    fixture.hub.history.lock().unwrap().push(live.clone());
    fixture.hub.head.store(4, Ordering::SeqCst);
    fixture.hub.live.send(Outbound::Event(live)).unwrap();
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::JournalGap {
            requested_from: 3,
            oldest_retained: 5
        }
    );
    fixture.hub.emit(5);
    client
        .send(&ClientFrame::Subscribe(Subscribe { after_seq: Some(4) }))
        .await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Event(event) if event.seq == 5)
    );
    assert_eq!(
        client.receive::<ServerFrame>().await,
        ServerFrame::CaughtUp { head: 5 }
    );
    drop(client);
    fixture.stop().await;
}
