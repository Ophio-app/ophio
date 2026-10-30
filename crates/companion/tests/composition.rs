// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]
mod support;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use companion::{
    Server, ServerDeps,
    compose::{ComposeOptions, DesktopFactory, DesktopSession, Runtime},
    config::{Config, Paths},
    identity::{FileSecretStore, Identity},
    local::{LocalServer, OwnerClient, OwnerRequest, OwnerResponse, OwnerRuntime},
};
use protocol::{
    auth::*, capability::*, command::*, library::*, model::*, terminal::*, transfer::*, *,
};
use services::Hub;
use std::{path::PathBuf, sync::Arc, time::Duration};
use support::{Client, PinVerifier};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

struct Rig {
    temp: tempfile::TempDir,
    paths: Paths,
    runtime: Arc<Runtime>,
    identity: Arc<Identity>,
    options: ComposeOptions,
    address: std::net::SocketAddr,
    stop: CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<companion::Result<()>>>,
    lock: store::InstanceLock,
    key: ed25519_dalek::SigningKey,
    device: Option<DeviceId>,
}
#[derive(Debug)]
struct SyntheticDesktop;
#[async_trait::async_trait]
impl DesktopFactory for SyntheticDesktop {
    async fn open(&self, _: &Paths, _: &ComposeOptions) -> companion::Result<DesktopSession> {
        Ok(synthetic_session(Arc::new(SyntheticDriver)))
    }
}
fn synthetic_session(driver: Arc<dyn platform_linux::native::Driver>) -> DesktopSession {
    let env = platform_linux::Environment {
        session_type: "wayland".into(),
        wayland_globals: [
            "wl_shm",
            "wl_seat",
            "zwlr_screencopy_manager_v1",
            "zwlr_virtual_pointer_manager_v1",
            "zwp_virtual_keyboard_manager_v1",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        ..Default::default()
    };
    DesktopSession {
        backend: Arc::new(platform_linux::LinuxDesktop::new(
            driver,
            platform_linux::LinuxDesktop::detect(&env).capabilities,
            None,
        )),
        portal: None,
    }
}
/// A synthetic desktop whose input stops answering once `stuck` is set, so
/// letting go of it can never be confirmed.
#[derive(Debug)]
struct StuckDesktop(Arc<std::sync::atomic::AtomicBool>);
#[async_trait::async_trait]
impl DesktopFactory for StuckDesktop {
    async fn open(&self, _: &Paths, _: &ComposeOptions) -> companion::Result<DesktopSession> {
        Ok(synthetic_session(Arc::new(StuckDriver(self.0.clone()))))
    }
}
#[derive(Debug)]
struct StuckDriver(Arc<std::sync::atomic::AtomicBool>);
#[async_trait::async_trait]
impl platform_linux::native::Driver for StuckDriver {
    async fn outputs(&self) -> Result<Vec<platform_linux::native::Output>, ports::PlatformError> {
        SyntheticDriver.outputs().await
    }
    async fn capture(
        &self,
        output: &platform_linux::native::Output,
    ) -> Result<ports::CapturedFrame, ports::PlatformError> {
        SyntheticDriver.capture(output).await
    }
    async fn input(
        &self,
    ) -> Result<Box<dyn platform_linux::native::NativeInput>, ports::PlatformError> {
        Ok(Box::new(StuckDriver(self.0.clone())))
    }
}
#[async_trait::async_trait]
impl platform_linux::native::NativeInput for StuckDriver {
    async fn barrier(&mut self) -> Result<(), ports::PlatformError> {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ports::PlatformError::Native(
                "the input connection stopped answering".into(),
            ));
        }
        Ok(())
    }
    async fn event(
        &mut self,
        _: platform_linux::native::Event,
    ) -> Result<(), ports::PlatformError> {
        Ok(())
    }
}
#[derive(Debug)]
struct SyntheticDriver;
#[async_trait::async_trait]
impl platform_linux::native::Driver for SyntheticDriver {
    async fn outputs(&self) -> Result<Vec<platform_linux::native::Output>, ports::PlatformError> {
        Ok(vec![platform_linux::native::Output {
            name: "Synthetic output".into(),
            x: 0.0,
            y: 0.0,
            width: 32,
            height: 24,
            scale: 1.0,
            rotation: 0,
        }])
    }
    async fn capture(
        &self,
        output: &platform_linux::native::Output,
    ) -> Result<ports::CapturedFrame, ports::PlatformError> {
        Ok(ports::CapturedFrame {
            width: output.width,
            height: output.height,
            stride: output.width * 4,
            format: ports::PixelFormat::Bgra8,
            data: vec![0; (output.width * output.height * 4) as usize],
            geometry: protocol::control::TargetGeometry {
                width: output.width,
                height: output.height,
                scale: 1.0,
                rotation_degrees: 0,
                revision: 1,
            },
            cursor: None,
            damage: None,
        })
    }
    async fn input(
        &self,
    ) -> Result<Box<dyn platform_linux::native::NativeInput>, ports::PlatformError> {
        Ok(Box::new(SyntheticDriver))
    }
}
#[async_trait::async_trait]
impl platform_linux::native::NativeInput for SyntheticDriver {
    async fn barrier(&mut self) -> Result<(), ports::PlatformError> {
        // Synthetic injection is synchronous and has no native queue.
        Ok(())
    }
    async fn event(
        &mut self,
        _: platform_linux::native::Event,
    ) -> Result<(), ports::PlatformError> {
        Ok(())
    }
}
impl Rig {
    async fn new(bridge: bool) -> Self {
        Self::configured(
            bridge,
            Config::default(),
            Arc::new(companion::frontdoors::LocalSpeech),
        )
        .await
    }
    async fn configured(
        bridge: bool,
        config: Config,
        speech_factory: Arc<dyn companion::frontdoors::SpeechFactory>,
    ) -> Self {
        Self::build(bridge, config, speech_factory, None).await
    }
    /// With `bus`, the runtime connects its real local Stop indicator to that
    /// session bus instead of receiving the test double.
    async fn build(
        bridge: bool,
        config: Config,
        speech_factory: Arc<dyn companion::frontdoors::SpeechFactory>,
        bus: Option<String>,
    ) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        let lock = paths.lock().unwrap();
        config.save(&paths.config_file()).unwrap();
        let identity = Arc::new(
            Identity::load_or_create(
                &FileSecretStore::new(paths.identity()),
                vec!["localhost".into()],
            )
            .unwrap(),
        );
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_owned();
        let fake = |crate_name: &str, filename: &str| {
            let dest = temp.path().join(filename);
            test_support::executable(
                &dest,
                std::fs::read(fixtures.join(crate_name).join("tests").join(filename)).unwrap(),
            )
            .unwrap();
            dest
        };
        let claude = fake("harness-claude", "fake_claude.py");
        let codex = fake("harness-codex", "fake_codex.py");
        let executable_dir = if bridge {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_owned()
        } else {
            temp.path().join("no-bridge")
        };
        if bridge {
            assert!(
                executable_dir.join(computer_mcp::BINARY_NAME).exists(),
                "Build computer-mcp before this test: cargo build -p computer-mcp"
            );
        }
        let options = ComposeOptions {
            environment: [(
                "HOME".into(),
                temp.path().join("home").to_string_lossy().into_owned(),
            )]
            .into_iter()
            .chain(
                bus.clone()
                    .map(|address| ("DBUS_SESSION_BUS_ADDRESS".into(), address)),
            )
            .collect(),
            executable_dir,
            clock: Arc::new(services::testing::FakeClock::default()),
            claude_binary: Some(claude),
            codex_binary: Some(codex),
            desktop_factory: Some(Arc::new(SyntheticDesktop)),
            speech_factory,
            accounts: false,
        };
        let runtime = Runtime::build(paths.clone(), config, &identity, options.clone())
            .await
            .unwrap();
        if bus.is_none() {
            runtime
                .hub
                .set_local_indicator(Some(Arc::new(FakeIndicator)))
                .await;
        }
        let mut deps = ServerDeps::new(runtime.hub.clone(), identity.clone()).unwrap();
        deps.clock = options.clock.clone();
        deps.owner = Some(runtime.clone());
        let server = Server::bind(deps.clone(), &["127.0.0.1:0".parse().unwrap()])
            .await
            .unwrap();
        let address = server.local_addresses().unwrap()[0];
        let local = LocalServer::bind(
            &paths.socket(),
            companion::started::Started::claim().unwrap(),
        )
        .await
        .unwrap();
        let stop = CancellationToken::new();
        let tasks = vec![
            tokio::spawn(server.run(stop.clone())),
            tokio::spawn(local.run(deps, stop.clone())),
        ];
        Self {
            temp,
            paths,
            runtime,
            identity,
            options,
            address,
            stop,
            tasks,
            lock,
            key: ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
            device: None,
        }
    }
    async fn tls(&self) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier(self.identity.fingerprint())))
        .with_no_client_auth();
        tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(
                rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                tokio::net::TcpStream::connect(self.address).await.unwrap(),
            )
            .await
            .unwrap()
    }
    /// Pairs as the owner does: the phone asks, and the owner confirms the
    /// code at the computer through the local socket.
    async fn pair(&mut self) {
        let mut owner = OwnerClient::connect(&self.paths.socket()).await.unwrap();
        let OwnerResponse::PairingOffer(offer) =
            owner.request(&OwnerRequest::OpenPairing).await.unwrap()
        else {
            panic!()
        };
        let request = PairRequest {
            pairing_secret: offer.pairing_secret,
            device_public_key: URL_SAFE_NO_PAD.encode(self.key.verifying_key().as_bytes()),
            device_name: "Fixture phone".into(),
            device_platform: "android".into(),
            client_version: "fixture".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        };
        let code = pairing_code(
            &self.identity.fingerprint(),
            &request.pairing_secret,
            &request.device_public_key,
        );
        let body = serde_json::to_string(&request).unwrap();
        let mut tls = self.tls().await;
        tls.write_all(format!("POST /v1/pair HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        let prompt = loop {
            if let OwnerResponse::PairingWaiting(Some(prompt)) =
                owner.request(&OwnerRequest::WaitingPairing).await.unwrap()
            {
                break prompt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(prompt.code, code);
        let OwnerResponse::PairingAnswered(Some(device)) = owner
            .request(&OwnerRequest::AnswerPairing {
                code,
                confirm: true,
            })
            .await
            .unwrap()
        else {
            panic!()
        };
        let mut response = String::new();
        tls.read_to_string(&mut response).await.unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        let response: PairResponse = serde_json::from_str(body).unwrap();
        assert_eq!(response.device_id, device.device_id);
        self.device = Some(response.device_id);
    }
    async fn client(&self, kind: ConnectionKind) -> Client {
        let tls = self.tls().await;
        let exporter = tls
            .get_ref()
            .1
            .export_keying_material([0; 32], TLS_EXPORTER_LABEL, None)
            .unwrap();
        let channel = match kind {
            ConnectionKind::Control => "control",
            ConnectionKind::Terminal => "terminal",
            ConnectionKind::Transfer => "transfer",
            ConnectionKind::Media => "media",
            ConnectionKind::Speech => "speech",
            ConnectionKind::Preview => "preview",
            ConnectionKind::Alerts => "alerts",
        };
        let (socket, _) =
            tokio_tungstenite::client_async(format!("wss://localhost/v1/{channel}"), tls)
                .await
                .unwrap();
        let mut client = Client {
            socket,
            exporter,
            kind,
            nonce: [3; 32],
        };
        let device = self.device.unwrap();
        let challenge = client.challenge(device).await;
        let proof = client.proof(&challenge, &self.key, device);
        client.send(&ClientFrame::Authenticate(proof)).await;
        assert!(matches!(
            client.receive::<ServerFrame>().await,
            ServerFrame::Authenticated(_)
        ));
        client
    }
    async fn owner(&self, request: OwnerRequest) -> Config {
        let mut owner = OwnerClient::connect(&self.paths.socket()).await.unwrap();
        let response: OwnerResponse = owner.request(&request).await.unwrap();
        let OwnerResponse::Settings {
            config,
            applied_live,
            ..
        } = response
        else {
            panic!()
        };
        assert!(applied_live);
        config
    }
    fn project_dir(&self, name: &str) -> PathBuf {
        let dir = self.temp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    async fn shutdown(self) -> (tempfile::TempDir, Paths, Arc<Identity>, ComposeOptions) {
        self.stop.cancel();
        for task in self.tasks {
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        self.runtime.shutdown().await.unwrap();
        drop(self.runtime);
        drop(self.lock);
        assert!(!self.paths.socket().exists());
        assert!(
            !std::fs::read_dir(&self.paths.runtime)
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "sock"))
        );
        self.paths
            .lock()
            .expect("instance lock remains held after shutdown");
        (self.temp, self.paths, self.identity, self.options)
    }
}
struct Phone {
    client: Client,
    events: Vec<Event>,
}
impl Phone {
    async fn new(rig: &Rig) -> Self {
        let mut client = rig.client(ConnectionKind::Control).await;
        client
            .send(&ClientFrame::Subscribe(protocol::Subscribe {
                after_seq: None,
            }))
            .await;
        Self {
            client,
            events: vec![],
        }
    }
    async fn frame(&mut self) -> ServerFrame {
        let frame = self.client.receive::<ServerFrame>().await;
        if let ServerFrame::Event(e) = &frame {
            self.events.push(e.event.clone());
        }
        if let ServerFrame::Fatal(e) = &frame {
            panic!("unexpected fatal {e:?}");
        }
        frame
    }
    async fn query(&mut self, query: Query) -> QueryData {
        let id = QueryId::new();
        self.client
            .send(&ClientFrame::Query(QueryEnvelope {
                query_id: id,
                query,
            }))
            .await;
        loop {
            if let ServerFrame::QueryResult(r) = self.frame().await
                && r.query_id == id
            {
                return r.result.unwrap();
            }
        }
    }
    async fn command(&mut self, body: Command, revision: Option<u64>) -> CommandReceipt {
        let mut envelope = CommandEnvelope::new(body);
        envelope.expected_revision = revision;
        let id = envelope.command_id;
        self.client
            .send(&ClientFrame::Command(Box::new(envelope)))
            .await;
        loop {
            if let ServerFrame::Receipt(r) = self.frame().await
                && r.command_id == id
            {
                assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
                return r;
            }
        }
    }
    async fn effect(&mut self, body: Command) -> Option<CommandResult> {
        let r = self.command(body, None).await;
        if r.result.is_some() {
            return r.result;
        }
        loop {
            if let QueryData::CommandStatus {
                stage,
                result,
                error,
                ..
            } = self
                .query(Query::CommandStatus {
                    command_id: r.command_id,
                })
                .await
            {
                assert!(error.is_none(), "{error:?}");
                if stage != Some(CommandStage::HostAccepted) {
                    return result.map(|r| *r);
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    async fn event(&mut self, predicate: impl Fn(&Event) -> bool) -> Event {
        loop {
            if let Some(e) = self.events.iter().find(|e| predicate(e)) {
                return e.clone();
            }
            self.frame().await;
        }
    }
    async fn project(&mut self, path: &std::path::Path) -> ProjectId {
        match self
            .effect(Command::AddProject {
                host_path: path.to_string_lossy().into_owned(),
                name: None,
            })
            .await
            .unwrap()
        {
            CommandResult::ProjectAdded { project_id } => project_id,
            _ => panic!(),
        }
    }
    async fn task(&mut self, project_id: ProjectId, harness: HarnessKind, prompt: &str) -> TaskId {
        match self
            .effect(Command::CreateTask {
                github: None,
                agent_chosen: None,
                mode: None,
                project_id,
                harness,
                prompt: prompt.into(),
                kind: TaskKind::Coding,
                title: None,
                bundle: None,
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
            })
            .await
            .unwrap()
        {
            CommandResult::TaskCreated { task_id, .. } => task_id,
            _ => panic!(),
        }
    }
}

#[tokio::test]
async fn real_composition_tls_claude_allow_deny_codex_terminal_library_transfer() {
    let mut rig = Rig::new(true).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let QueryData::Snapshot(snapshot) = phone
        .query(Query::Snapshot {
            reads_withheld: None,
        })
        .await
    else {
        panic!()
    };
    assert_eq!(snapshot.host.host_id, rig.identity.host_id);
    assert!(
        matches!(&snapshot.host.capabilities.capture, Availability::PermissionMissing { remedy } if remedy.contains("desktop enable"))
    );
    assert!(
        matches!(&snapshot.host.capabilities.speech, Availability::PermissionMissing { remedy } if remedy == &companion::frontdoors::install_remedy())
    );
    let readiness = phone.query(Query::Readiness).await;
    println!(
        "status excerpt: {}",
        serde_json::json!({"host_id": snapshot.host.host_id, "identity_host_id": rig.identity.host_id, "capabilities": snapshot.host.capabilities, "readiness": readiness})
    );
    for allow in [true, false] {
        let dir = rig.project_dir(if allow { "claude-allow" } else { "claude-deny" });
        let project = phone.project(&dir).await;
        let task = phone
            .task(project, HarnessKind::ClaudeCode, "bridge-permission")
            .await;
        let event = phone.event(|e| matches!(e, Event::ApprovalUpserted { approval } if approval.task_id == task && approval.state == ApprovalState::Pending)).await;
        let Event::ApprovalUpserted { approval } = event else {
            panic!()
        };
        phone
            .command(
                Command::ResolveApproval {
                    approval_id: approval.approval_id,
                    decision: if allow {
                        ApprovalDecision::Allow
                    } else {
                        ApprovalDecision::Deny {
                            reason: Some("Fixture denial".into()),
                        }
                    },
                },
                Some(u64::from(approval.revision)),
            )
            .await;
        phone.event(|e| matches!(e, Event::RunUpserted { run } if run.task_id == task && run.state == RunState::Completed)).await;
        let decision: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("permission-result.json")).unwrap())
                .unwrap();
        assert_eq!(decision["behavior"], if allow { "allow" } else { "deny" });
        let QueryData::TaskTimeline { items, .. } = phone
            .query(Query::TaskTimeline {
                task_id: task,
                before_seq: None,
                limit: 100,
            })
            .await
        else {
            panic!()
        };
        assert!(
            items
                .iter()
                .any(|i| matches!(i.item, TimelineBody::RunEnded { .. }))
        );
        let QueryData::Snapshot(snapshot) = phone
            .query(Query::Snapshot {
                reads_withheld: None,
            })
            .await
        else {
            panic!()
        };
        let approvals: Vec<_> = snapshot
            .approvals
            .iter()
            .filter(|a| a.task_id == task)
            .collect();
        assert_eq!(approvals.len(), 1);
        assert_eq!(
            approvals[0].state,
            if allow {
                ApprovalState::Allowed
            } else {
                ApprovalState::Denied
            }
        );
        println!(
            "Claude {}: approval {} revision {}, timeline {} items, run completed",
            if allow { "allow" } else { "deny" },
            approval.approval_id,
            approval.revision,
            items.len()
        );
    }
    let dir = rig.project_dir("codex");
    let project = phone.project(&dir).await;
    let task = phone.task(project, HarnessKind::Codex, "fixture").await;
    phone.event(|e| matches!(e, Event::RunUpserted { run } if run.task_id == task && run.native_session.is_some())).await;
    phone
        .command(
            Command::StopTask {
                task_id: task,
                force: false,
            },
            None,
        )
        .await;
    phone
        .event(
            |e| matches!(e, Event::InterventionRecorded { task_id: Some(id), .. } if *id == task),
        )
        .await;

    let terminal = match phone
        .effect(Command::OpenTerminal {
            cwd: None,
            project_id: Some(project),
            harness: None,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap()
    {
        CommandResult::TerminalOpened { terminal } => terminal,
        _ => panic!(),
    };
    let mut term = rig.client(ConnectionKind::Terminal).await;
    term.send(&TerminalClientFrame::Attach {
        terminal_id: terminal.terminal_id,
        from_offset: Some(0),
    })
    .await;
    assert!(matches!(
        term.receive::<TerminalServerFrame>().await,
        TerminalServerFrame::Attached { .. }
    ));
    term.binary(b"printf '\\125\\116\\111\\121\\125\\105\\137\\105\\103\\110\\117\\n'\n")
        .await;
    let mut output = Vec::new();
    loop {
        if let tokio_tungstenite::tungstenite::Message::Binary(bytes) = term.message().await {
            output.extend_from_slice(split_output_frame(&bytes).unwrap().1);
            if String::from_utf8_lossy(&output).contains("UNIQUE_ECHO") {
                break;
            }
        }
    }
    phone
        .effect(Command::CloseTerminal {
            terminal_id: terminal.terminal_id,
        })
        .await;
    drop(term);

    let root = rig.project_dir("notes");
    std::fs::write(root.join("notes.md"), "original\n").unwrap();
    let config = rig
        .owner(OwnerRequest::RootsAdd {
            path: root.clone(),
            label: Some("Notes".into()),
            writable: true,
        })
        .await;
    let id = config.roots[0].root_id;
    phone.event(|e| matches!(e, Event::LibraryRootsChanged { roots } if roots.iter().any(|r| r.root_id == id))).await;
    assert!(matches!(
        phone
            .query(Query::LibraryList {
                root_id: id,
                dir: "".into()
            })
            .await,
        QueryData::LibraryList { .. }
    ));
    let QueryData::LibraryRead { file } = phone
        .query(Query::LibraryRead {
            root_id: id,
            path: "notes.md".into(),
        })
        .await
    else {
        panic!()
    };
    let save = |text: &str| Command::LibrarySave {
        root_id: id,
        path: "notes.md".into(),
        text: text.into(),
        base_revision: file.revision.clone(),
        newline: file.newline,
        bom: file.has_bom,
    };
    assert!(matches!(
        phone.effect(save("changed\n")).await,
        Some(CommandResult::Saved {
            outcome: SaveOutcome::Saved { .. }
        })
    ));
    assert!(matches!(
        phone.effect(save("stale\n")).await,
        Some(CommandResult::Saved {
            outcome: SaveOutcome::Conflict { .. }
        })
    ));
    rig.owner(OwnerRequest::RootsRemove { root_id: id }).await;
    assert!(
        rig.runtime
            .hub
            .query(
                &services::Caller::local_owner(),
                Query::LibraryRead {
                    root_id: id,
                    path: "notes.md".into()
                }
            )
            .await
            .is_err()
    );
    let root_project = phone.project(&root).await;
    let bytes = b"uploaded\n";
    let transfer = match phone
        .effect(Command::CreateUploadReviewed {
            task_id: None,
            name: "notes.md".into(),
            collision: UploadCollision::KeepBoth {
                name: "notes (1).md".into(),
            },
            size: bytes.len() as u64,
            sha256: companion::identity::fingerprint(bytes),
            destination: TransferDestination::ProjectDir {
                project_id: root_project,
                dir: "".into(),
            },
        })
        .await
        .unwrap()
    {
        CommandResult::Transfer { transfer } => transfer,
        _ => panic!(),
    };
    let mut upload = rig.client(ConnectionKind::Transfer).await;
    upload
        .send(&TransferClientFrame::Upload {
            transfer_id: transfer.transfer_id,
        })
        .await;
    assert!(matches!(
        upload.receive::<TransferServerFrame>().await,
        TransferServerFrame::ResumeAt { offset: 0, .. }
    ));
    upload.binary(bytes).await;
    upload
        .send(&TransferClientFrame::Finish {
            transfer_id: transfer.transfer_id,
        })
        .await;
    assert!(matches!(
        upload.receive::<TransferServerFrame>().await,
        TransferServerFrame::Completed { .. }
    ));
    assert_eq!(std::fs::read(root.join("notes (1).md")).unwrap(), bytes);
    assert_eq!(std::fs::read(root.join("notes.md")).unwrap(), b"changed\n");
    let download = match phone
        .effect(Command::CreateDownload {
            shown_in_task: None,
            source: DownloadSource::ProjectFile {
                project_id: root_project,
                path: "notes (1).md".into(),
            },
        })
        .await
        .unwrap()
    {
        CommandResult::Transfer { transfer } => transfer,
        _ => panic!(),
    };
    let mut receiving = rig.client(ConnectionKind::Transfer).await;
    receiving
        .send(&TransferClientFrame::Download {
            transfer_id: download.transfer_id,
            offset: 0,
        })
        .await;
    assert!(matches!(
        receiving.receive::<TransferServerFrame>().await,
        TransferServerFrame::Sending { .. }
    ));
    let mut received = Vec::new();
    loop {
        match receiving.message().await {
            tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                received.extend_from_slice(&bytes)
            }
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                assert!(matches!(
                    serde_json::from_str::<TransferServerFrame>(&text).unwrap(),
                    TransferServerFrame::Completed { .. }
                ));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(received, bytes);
    drop(receiving);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let preview_server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 1024];
        let count = stream.read(&mut request).await.unwrap();
        assert!(count > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let preview = match phone
        .effect(Command::RegisterPreview {
            project_id: project,
            label: "Fixture preview".into(),
            url,
        })
        .await
        .unwrap()
    {
        CommandResult::PreviewRegistered { preview_id } => preview_id,
        _ => panic!(),
    };
    phone
        .effect(Command::CheckPreview {
            preview_id: preview,
            start: false,
        })
        .await;
    preview_server.await.unwrap();
    let QueryData::Previews { previews } = phone
        .query(Query::Previews {
            project_id: project,
        })
        .await
    else {
        panic!()
    };
    assert!(!previews.is_empty());
    rig.owner(OwnerRequest::RetentionSet { days: 7 }).await;
    assert_eq!(
        Config::load(&rig.paths.config_file())
            .unwrap()
            .retention_days,
        7
    );
    drop(upload);
    drop(phone);
    let (_temp, paths, identity, options) = rig.shutdown().await;
    let restored = Identity::load_or_create(
        &FileSecretStore::new(paths.identity()),
        vec!["localhost".into()],
    )
    .unwrap();
    assert_eq!(restored.host_id, identity.host_id);
    let runtime = Runtime::build(
        paths.clone(),
        Config::load(&paths.config_file()).unwrap(),
        &restored,
        options,
    )
    .await
    .unwrap();
    assert_eq!(runtime.hub.host_id(), identity.host_id);
    runtime.shutdown().await.unwrap();
}

/// A conversation gets Ophio's tools while its access bar governs native steps
/// instead of asking before each one. Whole-computer access needs no sandbox,
/// so this checks the launch contract without requiring a sandbox backend.
#[tokio::test]
async fn a_claude_conversation_gets_ophio_tools_while_bypassing_native_prompts() {
    let mut rig = Rig::new(true).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let dir = rig.project_dir("conversation");
    let project_id = phone.project(&dir).await;
    let CommandResult::TaskCreated { task_id, .. } = phone
        .effect(Command::CreateTask {
            github: None,
            agent_chosen: None,
            mode: None,
            project_id,
            harness: HarnessKind::ClaudeCode,
            prompt: "complete".into(),
            kind: TaskKind::Coding,
            title: None,
            bundle: None,
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
            conversation: Some(true),
            access: Some(protocol::access::ConversationAccess {
                whole_write: true,
                ..Default::default()
            }),
            review: None,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let Event::RunUpserted { run } = phone
        .event(|e| matches!(e, Event::RunUpserted { run } if run.task_id == task_id && matches!(run.state, RunState::Completed | RunState::Failed)))
        .await
    else {
        panic!()
    };
    assert_eq!(run.state, RunState::Completed, "{:?}", run.outcome);
    let argv: Vec<String> =
        serde_json::from_slice(&std::fs::read(dir.join("argv.json")).unwrap()).unwrap();
    assert!(
        argv.windows(2)
            .any(|w| w == ["--permission-mode", "bypassPermissions"])
    );
    assert!(argv.iter().any(|a| a == "--dangerously-skip-permissions"));
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"]["computer"]["command"],
        serde_json::to_value(rig.options.executable_dir.join(computer_mcp::BINARY_NAME)).unwrap()
    );
    let allowed = argv
        .windows(2)
        .find(|w| w[0] == "--allowedTools")
        .expect("the conversation gets Ophio's tools")[1]
        .split(',')
        .collect::<Vec<_>>();
    assert!(allowed.contains(&"mcp__computer__show_person"));
    assert!(allowed.contains(&"mcp__computer__show_preview"));
    // Claude can resolve its prompt tool, while it stays hidden from the model.
    for flag in ["--permission-prompt-tool", "--disallowedTools"] {
        assert!(
            argv.windows(2)
                .any(|w| w == [flag, "mcp__computer__permission_prompt"])
        );
    }
    assert!(!allowed.contains(&"mcp__computer__permission_prompt"));
}

#[tokio::test]
async fn desktop_disable_forgets_consent_before_the_next_enable() {
    use platform_linux::portal::{FileTokenStore, TokenStore};

    struct ConsentCheckingDesktop(Arc<std::sync::Mutex<Vec<bool>>>);
    #[async_trait::async_trait]
    impl DesktopFactory for ConsentCheckingDesktop {
        async fn open(
            &self,
            paths: &Paths,
            options: &ComposeOptions,
        ) -> companion::Result<DesktopSession> {
            let restored = FileTokenStore::new(paths.data.join("desktop-consent"))
                .load()
                .await
                .unwrap()
                .is_some();
            self.0.lock().unwrap().push(restored);
            SyntheticDesktop.open(paths, options).await
        }
    }

    let rig = Rig::new(false).await;
    let (_temp, paths, identity, mut options) = rig.shutdown().await;
    let directory = paths.data.join("desktop-consent");
    std::fs::create_dir(&directory).unwrap();
    let tokens = FileTokenStore::new(directory.clone());
    tokens.save("synthetic-restore-token").await.unwrap();
    std::fs::write(directory.join("unrelated"), "keep").unwrap();
    std::fs::write(paths.data.join("unrelated"), "keep").unwrap();
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    options.desktop_factory = Some(Arc::new(ConsentCheckingDesktop(observed.clone())));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(true),
        })
        .await
        .unwrap();
    assert!(tokens.load().await.unwrap().is_some());
    runtime
        .request(OwnerRequest::Desktop { enabled: None })
        .await
        .unwrap();
    assert!(tokens.load().await.unwrap().is_some());
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(false),
        })
        .await
        .unwrap();
    assert!(tokens.load().await.unwrap().is_none());
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(true),
        })
        .await
        .unwrap();
    assert_eq!(*observed.lock().unwrap(), vec![true, false]);
    for _ in 0..2 {
        runtime
            .request(OwnerRequest::Desktop {
                enabled: Some(false),
            })
            .await
            .unwrap();
        assert!(tokens.load().await.unwrap().is_none());
    }
    assert_eq!(
        std::fs::read_to_string(directory.join("unrelated")).unwrap(),
        "keep"
    );
    assert_eq!(
        std::fs::read_to_string(paths.data.join("unrelated")).unwrap(),
        "keep"
    );
    assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
    runtime.shutdown().await.unwrap();
}

/// The desktop's sharing dialog, open until the test answers it.
struct WaitingDesktop {
    shown: Arc<tokio::sync::Notify>,
    answered: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl DesktopFactory for WaitingDesktop {
    async fn open(
        &self,
        paths: &Paths,
        options: &ComposeOptions,
    ) -> companion::Result<DesktopSession> {
        self.shown.notify_one();
        self.answered.notified().await;
        SyntheticDesktop.open(paths, options).await
    }
}

#[tokio::test]
async fn settings_reads_answer_while_desktop_sharing_waits_for_the_person() {
    use companion::{
        frontdoors::{ChannelCommand, VoiceCommand},
        jev::JevOperation,
    };
    use tokio::sync::Notify;

    let rig = Rig::new(false).await;
    let (_temp, paths, identity, mut options) = rig.shutdown().await;
    let (shown, answered) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    options.desktop_factory = Some(Arc::new(WaitingDesktop {
        shown: shown.clone(),
        answered: answered.clone(),
    }));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    let enabling = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .request(OwnerRequest::Desktop {
                    enabled: Some(true),
                })
                .await
        }
    });
    shown.notified().await;
    let reads: [fn() -> OwnerRequest; 6] = [
        || OwnerRequest::FrontdoorStatus,
        || OwnerRequest::Voice {
            action: VoiceCommand::Status,
        },
        || OwnerRequest::Channels {
            action: ChannelCommand::Status,
        },
        || OwnerRequest::RootsList,
        || OwnerRequest::Desktop { enabled: None },
        || OwnerRequest::Jev {
            operation: JevOperation::Status,
        },
    ];
    for read in reads {
        let request = read();
        let label = format!("{request:?}");
        let response = tokio::time::timeout(Duration::from_secs(5), runtime.request(request))
            .await
            .unwrap_or_else(|_| panic!("{label} waited for the sharing dialog"))
            .unwrap();
        if let OwnerResponse::Settings { config, .. } = response {
            assert!(
                !config.desktop_enabled,
                "{label} reported an unapplied change"
            );
        }
    }
    assert!(!enabling.is_finished());
    answered.notify_one();
    enabling.await.unwrap().unwrap();
    let OwnerResponse::Settings { config, .. } = runtime
        .request(OwnerRequest::Desktop { enabled: None })
        .await
        .unwrap()
    else {
        panic!("Desktop status")
    };
    assert!(config.desktop_enabled);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn stopping_does_not_wait_for_an_unanswered_desktop_sharing_dialog() {
    let rig = Rig::new(false).await;
    let (_temp, paths, identity, mut options) = rig.shutdown().await;
    let shown = Arc::new(tokio::sync::Notify::new());
    options.desktop_factory = Some(Arc::new(WaitingDesktop {
        shown: shown.clone(),
        answered: Arc::new(tokio::sync::Notify::new()),
    }));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    // The control connection cancels its operation when Ophio stops or the
    // command that asked goes away.
    let cancel = CancellationToken::new();
    let enabling = tokio::spawn({
        let (runtime, cancel) = (runtime.clone(), cancel.clone());
        async move {
            runtime
                .request_progress(
                    OwnerRequest::Desktop {
                        enabled: Some(true),
                    },
                    tokio::sync::mpsc::channel(1).0,
                    cancel,
                )
                .await
        }
    });
    shown.notified().await;
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), enabling)
        .await
        .expect("the change waited for the sharing dialog")
        .unwrap();
    assert!(result.is_err(), "an unconfirmed change reported success");
    assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
    let OwnerResponse::Settings { config, .. } = runtime
        .request(OwnerRequest::Desktop { enabled: None })
        .await
        .unwrap()
    else {
        panic!("Desktop status")
    };
    assert!(!config.desktop_enabled);
    tokio::time::timeout(Duration::from_secs(5), runtime.shutdown())
        .await
        .expect("stopping waited for the sharing dialog")
        .unwrap();
}

#[tokio::test]
async fn an_interrupted_enable_records_sharing_off_even_when_it_was_on() {
    let rig = Rig::new(false).await;
    let (_temp, paths, identity, mut options) = rig.shutdown().await;
    let (shown, answered) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    options.desktop_factory = Some(Arc::new(WaitingDesktop {
        shown: shown.clone(),
        answered: answered.clone(),
    }));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    answered.notify_one();
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(true),
        })
        .await
        .unwrap();
    shown.notified().await;
    assert!(Config::load(&paths.config_file()).unwrap().desktop_enabled);
    // Sharing ended on the desktop; the person enables it again and the
    // request ends before they answer.
    let cancel = CancellationToken::new();
    let enabling = tokio::spawn({
        let (runtime, cancel) = (runtime.clone(), cancel.clone());
        async move {
            runtime
                .request_progress(
                    OwnerRequest::Desktop {
                        enabled: Some(true),
                    },
                    tokio::sync::mpsc::channel(1).0,
                    cancel,
                )
                .await
        }
    });
    shown.notified().await;
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), enabling)
        .await
        .expect("the change waited for the sharing dialog")
        .unwrap();
    assert!(result.is_err(), "an unconfirmed change reported success");
    // Sharing is closed, so neither the status nor the next start may say on.
    assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
    let OwnerResponse::Settings {
        config, desktop, ..
    } = runtime
        .request(OwnerRequest::Desktop { enabled: None })
        .await
        .unwrap()
    else {
        panic!("Desktop status")
    };
    assert!(!config.desktop_enabled);
    assert!(!matches!(desktop, Availability::Available));
    runtime.shutdown().await.unwrap();
}

/// A sharing dialog the person refuses, or a desktop that fails to open.
struct RefusingDesktop {
    code: ErrorCode,
    asked: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait::async_trait]
impl DesktopFactory for RefusingDesktop {
    async fn open(&self, _: &Paths, _: &ComposeOptions) -> companion::Result<DesktopSession> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(ProtocolError::new(self.code, "Desktop sharing was denied or cancelled").into())
    }
}

#[tokio::test]
async fn an_enable_that_is_refused_or_fails_leaves_sharing_off() {
    use std::sync::atomic::Ordering::SeqCst;
    for code in [ErrorCode::OsPermissionMissing, ErrorCode::NativeFailure] {
        let rig = Rig::new(false).await;
        let (_temp, paths, identity, mut options) = rig.shutdown().await;
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        options.desktop_factory = Some(Arc::new(RefusingDesktop {
            code,
            asked: asked.clone(),
        }));
        let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options.clone())
            .await
            .unwrap();
        let refused = runtime
            .request(OwnerRequest::Desktop {
                enabled: Some(true),
            })
            .await
            .expect_err("sharing nobody confirmed was reported on");
        assert!(
            refused.to_string().contains("denied or cancelled"),
            "{code:?}: {refused}"
        );
        assert_eq!(asked.load(SeqCst), 1);
        assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
        let OwnerResponse::Settings {
            config, desktop, ..
        } = runtime
            .request(OwnerRequest::Desktop { enabled: None })
            .await
            .unwrap()
        else {
            panic!("Desktop status")
        };
        assert!(!config.desktop_enabled, "{code:?}: status says on");
        assert!(
            format!("{desktop:?}").contains("denied or cancelled"),
            "{code:?}: the phone is not told why: {desktop:?}"
        );
        runtime.shutdown().await.unwrap();
        drop(runtime);
        // The next start does not ask again.
        let runtime = Runtime::build(
            paths.clone(),
            Config::load(&paths.config_file()).unwrap(),
            &identity,
            options,
        )
        .await
        .unwrap();
        assert_eq!(
            asked.load(SeqCst),
            1,
            "{code:?}: the next start asked again"
        );
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn owner_desktop_toggle_applies_live_and_private_roots_are_rejected() {
    let mut rig = Rig::new(false).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    rig.owner(OwnerRequest::Desktop {
        enabled: Some(true),
    })
    .await;
    let QueryData::ControlTargets { targets } = phone.query(Query::ControlTargets).await else {
        panic!()
    };
    assert!(!targets.is_empty());
    phone
        .effect(Command::RequestHumanLease {
            target_id: targets[0].target_id,
            geometry_revision: targets[0].geometry.revision,
        })
        .await;
    let mut local = OwnerClient::connect(&rig.paths.socket()).await.unwrap();
    let stopped: OwnerResponse = local.request(&OwnerRequest::Stop).await.unwrap();
    assert!(matches!(
        stopped,
        OwnerResponse::ControlStopped { tasks_requested: 0 }
    ));
    phone
        .effect(Command::RequestHumanLease {
            target_id: targets[0].target_id,
            geometry_revision: targets[0].geometry.revision,
        })
        .await;
    let mut media = rig.client(ConnectionKind::Media).await;
    media
        .send(&protocol::media::MediaClientFrame::Open {
            target_id: targets[0].target_id,
            params: protocol::media::StreamParams::default(),
        })
        .await;
    assert!(matches!(
        media.receive::<protocol::media::MediaServerFrame>().await,
        protocol::media::MediaServerFrame::Opened { .. }
    ));
    assert!(matches!(
        media.message().await,
        tokio_tungstenite::tungstenite::Message::Binary(_)
    ));
    rig.owner(OwnerRequest::Desktop {
        enabled: Some(false),
    })
    .await;
    phone.event(|e| matches!(e, Event::InterventionRecorded { receipt, .. } if receipt.summary.contains("Desktop sharing ended"))).await;
    loop {
        if let tokio_tungstenite::tungstenite::Message::Text(text) = media.message().await
            && let protocol::media::MediaServerFrame::Ended { reason } =
                serde_json::from_str(&text).unwrap()
        {
            assert!(reason.contains("Desktop sharing ended"));
            break;
        }
    }
    drop(media);
    assert!(matches!(
        rig.runtime.hub.capabilities().await.capture,
        Availability::PermissionMissing { .. }
    ));
    for path in [&rig.paths.data, rig.temp.path()] {
        let mut owner = OwnerClient::connect(&rig.paths.socket()).await.unwrap();
        assert!(
            owner
                .request::<_, OwnerResponse>(&OwnerRequest::RootsAdd {
                    path: path.into(),
                    label: None,
                    writable: true
                })
                .await
                .is_err()
        );
    }
    let QueryData::Harnesses { harnesses } = phone.query(Query::Harnesses).await else {
        panic!()
    };
    let claude = harnesses
        .iter()
        .find(|h| h.kind == HarnessKind::ClaudeCode)
        .unwrap();
    assert!(!claude.capabilities.native_approvals.is_available());
    assert!(claude.capabilities.start_run.is_available());
    for mode in [
        protocol::model::TaskMode::Ask,
        protocol::model::TaskMode::Plan,
    ] {
        assert!(
            claude
                .modes
                .iter()
                .find(|m| m.mode == mode)
                .unwrap()
                .availability
                .is_available()
        );
    }
    assert!(
        !claude
            .modes
            .iter()
            .find(|m| m.mode == protocol::model::TaskMode::Work)
            .unwrap()
            .availability
            .is_available()
    );
    drop(phone);
    rig.shutdown().await;
}

/// Whether a process has ended: gone, or exited and not yet reaped. Shutdown
/// confirms the agent's exit without reaping it, and its parent reaps it just
/// after, so for a moment it is still listed.
#[cfg(target_os = "linux")]
async fn ended(pid: &str) -> bool {
    let started = tokio::time::Instant::now();
    loop {
        let ended = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        };
        if ended || started.elapsed() > Duration::from_secs(5) {
            return ended;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn shutdown_drains_active_run_terminal_transfer_and_releases_sockets_and_locks() {
    let mut rig = Rig::new(true).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let dir = rig.project_dir("active");
    let project = phone.project(&dir).await;
    let task = phone
        .task(project, HarnessKind::ClaudeCode, "bridge-permission")
        .await;
    phone
        .event(|e| matches!(e, Event::ApprovalUpserted { approval } if approval.task_id == task))
        .await;
    let terminal = phone
        .effect(Command::OpenTerminal {
            cwd: None,
            project_id: Some(project),
            harness: None,
            cols: 80,
            rows: 24,
        })
        .await;
    let Some(CommandResult::TerminalOpened { terminal }) = terminal else {
        panic!()
    };
    let mut terminal_client = rig.client(ConnectionKind::Terminal).await;
    terminal_client
        .send(&TerminalClientFrame::Attach {
            terminal_id: terminal.terminal_id,
            from_offset: Some(0),
        })
        .await;
    let _: TerminalServerFrame = terminal_client.receive().await;
    terminal_client
        .binary(b"printf '\\120\\111\\104:%s\\n' \"$$\"\n")
        .await;
    let mut terminal_output = Vec::new();
    let terminal_pid: u32 = loop {
        if let tokio_tungstenite::tungstenite::Message::Binary(bytes) =
            terminal_client.message().await
        {
            terminal_output.extend_from_slice(split_output_frame(&bytes).unwrap().1);
            if let Some(pid) = String::from_utf8_lossy(&terminal_output)
                .lines()
                .find_map(|line| {
                    line.split_once("PID:")
                        .and_then(|(_, tail)| tail.trim().parse().ok())
                })
            {
                break pid;
            }
        }
    };
    let transfer = match phone
        .effect(Command::CreateUpload {
            evidence: None,
            name: "unfinished.md".into(),
            size: 10,
            sha256: companion::identity::fingerprint(b"0123456789"),
            destination: TransferDestination::ProjectDir {
                project_id: project,
                dir: "".into(),
            },
        })
        .await
        .unwrap()
    {
        CommandResult::Transfer { transfer } => transfer,
        _ => panic!(),
    };
    let mut upload = rig.client(ConnectionKind::Transfer).await;
    upload
        .send(&TransferClientFrame::Upload {
            transfer_id: transfer.transfer_id,
        })
        .await;
    let _: TransferServerFrame = upload.receive().await;
    upload.binary(b"012").await;
    let pid = std::fs::read_to_string(dir.join("fixture.pid")).unwrap();
    let (_temp, paths, identity, options) = rig.shutdown().await;
    #[cfg(target_os = "linux")]
    assert!(ended(pid.trim()).await, "fake harness survived shutdown");
    #[cfg(target_os = "linux")]
    assert!(
        ended(&terminal_pid.to_string()).await,
        "terminal survived shutdown"
    );
    drop(terminal_client);
    drop(phone);
    drop(upload);
    assert!(!dir.join("unfinished.md").exists());
    let restarted = Runtime::build(
        paths.clone(),
        Config::load(&paths.config_file()).unwrap(),
        &identity,
        options,
    )
    .await
    .unwrap();
    let QueryData::Terminals { terminals } = restarted
        .hub
        .query(&services::Caller::local_owner(), Query::Terminals)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(terminals.is_empty());
    let QueryData::Transfers { transfers } = restarted
        .hub
        .query(&services::Caller::local_owner(), Query::Transfers)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(
        transfers
            .iter()
            .any(|t| t.transfer_id == transfer.transfer_id && t.state == TransferState::Paused)
    );
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn binary_run_status_live_config_and_offline_config_use_isolated_directories() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    let runtime_dir = temp.path().join("runtime");
    let config_home = temp.path().join("config");
    let data_home = temp.path().join("data");
    let home = temp.path().join("home");
    // HOME deliberately names a missing directory; private state uses XDG paths.
    for dir in [&bin, &runtime_dir, &config_home.join("ophio")] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned();
    for (name, source) in [
        ("claude", "harness-claude/tests/fake_claude.py"),
        ("codex", "harness-codex/tests/fake_codex.py"),
    ] {
        let path = bin.join(name);
        test_support::executable(&path, std::fs::read(fixtures.join(source)).unwrap()).unwrap();
    }
    // Discovery is isolated too: no real network service or account is queried.
    test_support::executable(bin.join("tailscale"), "#!/bin/sh\nexit 1\n").unwrap();
    let config_path = config_home.join("ophio/config.toml");
    Config {
        port: 0,
        ..Default::default()
    }
    .save(&config_path)
    .unwrap();
    let process = || {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ophio"));
        command
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", &home)
            .env("SHELL", "/bin/sh")
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_DATA_HOME", &data_home)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        command
    };
    let mut running = process()
        .arg("run")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = process().args(["status", "--json"]).output().await.unwrap();
            if status.status.success() {
                let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
                if value.get("snapshot").is_some() {
                    break value;
                }
            }
            assert!(
                running.try_wait().unwrap().is_none(),
                "companion exited before status became available"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let host = &status["snapshot"]["host"]["host_id"];
    assert!(host.is_string(), "{status}");
    assert_eq!(
        status["snapshot"]["host"]["capabilities"]["capture"]["state"],
        "permission_missing"
    );
    println!(
        "ophio status JSON excerpt: {}",
        serde_json::json!({"host_id": host, "capture": status["snapshot"]["host"]["capabilities"]["capture"], "voice": status["voice"], "channels": status["channels"]})
    );
    assert!(!home.exists());
    assert!(status["inbox"]["folder"].is_null());
    assert!(status["snapshot"]["host"]["inbox_path"].is_null());
    assert_eq!(
        status["inbox"]["remedy"],
        "Set one with `ophio inbox set <PATH>`"
    );
    let doctor = process().args(["doctor", "--json"]).output().await.unwrap();
    assert!(doctor.status.success());
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(
        doctor["inbox"]["remedy"],
        "Set one with `ophio inbox set <PATH>`"
    );
    let settings = process()
        .args(["retention", "set", "14"])
        .output()
        .await
        .unwrap();
    assert!(
        settings.status.success(),
        "{}",
        String::from_utf8_lossy(&settings.stderr)
    );
    assert!(Config::load(&config_path).unwrap().inbox_path.is_none());
    let inbox = temp.path().join("Received");
    let changed = process()
        .args(["inbox", "set", "--json"])
        .arg(&inbox)
        .output()
        .await
        .unwrap();
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    assert!(String::from_utf8_lossy(&changed.stdout).contains("\"applied_live\": true"));
    for action in ["status", "doctor"] {
        let output = process().arg(action).output().await.unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(&*inbox.to_string_lossy()));
        let output = process().args([action, "--json"]).output().await.unwrap();
        assert!(output.status.success());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["inbox"]["folder"], inbox.to_string_lossy().as_ref());
        if action == "status" {
            assert_eq!(
                value["snapshot"]["host"]["inbox_path"],
                inbox.to_string_lossy().as_ref()
            );
        }
    }
    let root = temp.path().join("notes");
    std::fs::create_dir(&root).unwrap();
    let add = process()
        .args(["roots", "add", "--json"])
        .arg(&root)
        .arg("--writable")
        .output()
        .await
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(String::from_utf8_lossy(&add.stdout).contains("\"applied_live\": true"));
    let listed = process().args(["roots", "list"]).output().await.unwrap();
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains(&format!(
        "notes: {}\n  Writable, kind notes.\n  ID ",
        root.canonicalize().unwrap().display()
    )));
    assert!(
        process()
            .args(["retention", "set", "14"])
            .output()
            .await
            .unwrap()
            .status
            .success()
    );
    // With display and D-Bus absent, enable reports the exact remedy and
    // leaves sharing off, so the next start does not try it unasked.
    let enabled = process()
        .args(["desktop", "enable"])
        .output()
        .await
        .unwrap();
    assert!(!enabled.status.success());
    assert!(
        String::from_utf8_lossy(&enabled.stderr).contains("No supported graphical session"),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert!(!Config::load(&config_path).unwrap().desktop_enabled);
    assert!(
        process()
            .args(["desktop", "disable"])
            .output()
            .await
            .unwrap()
            .status
            .success()
    );
    assert!(
        process()
            .arg("doctor")
            .output()
            .await
            .unwrap()
            .status
            .success()
    );
    let stopped = process().arg("stop").output().await.unwrap();
    assert!(stopped.status.success());
    assert!(
        String::from_utf8_lossy(&stopped.stdout)
            .contains("Stop requested for 0 computer-use tasks")
    );
    let pid = running.id().unwrap();
    assert!(
        tokio::process::Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(10), running.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    let offline = process()
        .args(["retention", "set", "21", "--json"])
        .output()
        .await
        .unwrap();
    assert!(offline.status.success());
    assert!(String::from_utf8_lossy(&offline.stderr).contains("the next time it starts"));
    assert!(String::from_utf8_lossy(&offline.stdout).contains("\"applied_live\": false"));
    assert_eq!(Config::load(&config_path).unwrap().retention_days, 21);
}

#[path = "composition/frontdoors.rs"]
mod frontdoor_tests;

#[path = "composition/boundary.rs"]
mod boundary_tests;

#[path = "composition/library_workflows.rs"]
mod library_workflow_tests;
#[path = "composition/live.rs"]
mod live_tests;
#[path = "composition/local_stop.rs"]
mod local_stop_tests;
#[derive(Debug)]
struct FakeIndicator;
#[async_trait::async_trait]
impl ports::desktop::LocalIndicator for FakeIndicator {
    async fn show(&self, _: &str) -> Result<(), ports::PlatformError> {
        Ok(())
    }
    async fn hide(&self) -> Result<(), ports::PlatformError> {
        Ok(())
    }
}

#[tokio::test]
async fn terminal_shell_exit_precedes_close_and_allows_repeated_replay() {
    let mut rig = Rig::new(true).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let project = phone.project(&rig.project_dir("terminal-exit")).await;
    let Some(CommandResult::TerminalOpened { terminal }) = phone
        .effect(Command::OpenTerminal {
            cwd: None,
            project_id: Some(project),
            harness: None,
            cols: 80,
            rows: 24,
        })
        .await
    else {
        panic!("terminal not opened")
    };
    for attempt in 0..3 {
        let mut term = rig.client(ConnectionKind::Terminal).await;
        term.send(&TerminalClientFrame::Attach {
            terminal_id: terminal.terminal_id,
            from_offset: Some(0),
        })
        .await;
        if attempt > 0 {
            // A resize already queued by the UI can race the retained exit.
            term.send(&TerminalClientFrame::Resize {
                cols: 100,
                rows: 30,
            })
            .await;
        }
        assert!(matches!(
            term.receive::<TerminalServerFrame>().await,
            TerminalServerFrame::Attached { .. }
        ));
        if attempt == 0 {
            term.binary(b"printf 'final output\n'; exit 7\n").await;
        }
        let mut output = Vec::new();
        loop {
            match term.message().await {
                tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                    output.extend_from_slice(split_output_frame(&bytes).unwrap().1)
                }
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    assert!(matches!(
                        serde_json::from_str::<TerminalServerFrame>(&text).unwrap(),
                        TerminalServerFrame::Exited { code: Some(7) }
                    ));
                    break;
                }
                other => panic!("stream closed before exit: {other:?}"),
            }
        }
        assert!(String::from_utf8_lossy(&output).contains("final output"));
        assert!(matches!(
            term.message().await,
            tokio_tungstenite::tungstenite::Message::Close(_)
        ));
    }
    let mut attachment = rig
        .runtime
        .hub
        .attach_terminal(
            &services::api::Caller::local_owner(),
            terminal.terminal_id,
            Some(0),
        )
        .await
        .unwrap();
    let error = attachment.input.resize(100, 30).await.unwrap_err();
    assert_eq!(error.detail.unwrap()["terminal_state"], "exited");
    loop {
        if matches!(
            attachment.output.recv().await,
            Some(services::api::TerminalOut::Exited { code: Some(7) })
        ) {
            break;
        }
    }
    assert!(
        attachment.output.recv().await.is_none(),
        "service stream must end after exit"
    );
    attachment.input.detach().await;
    drop(phone);
    rig.shutdown().await;
}

#[tokio::test]
async fn turning_sharing_off_does_not_wait_for_an_unanswered_enable() {
    use platform_linux::portal::{FileTokenStore, TokenStore};

    let rig = Rig::new(false).await;
    let (_temp, paths, identity, mut options) = rig.shutdown().await;
    let directory = paths.data.join("desktop-consent");
    std::fs::create_dir(&directory).unwrap();
    let tokens = FileTokenStore::new(directory);
    tokens.save("synthetic-restore-token").await.unwrap();
    let shown = Arc::new(tokio::sync::Notify::new());
    options.desktop_factory = Some(Arc::new(WaitingDesktop {
        shown: shown.clone(),
        answered: Arc::new(tokio::sync::Notify::new()),
    }));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    // The command that asked stays connected, so nothing else ends the wait.
    let enabling = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .request(OwnerRequest::Desktop {
                    enabled: Some(true),
                })
                .await
        }
    });
    shown.notified().await;
    let OwnerResponse::Settings { config, .. } = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.request(OwnerRequest::Desktop {
            enabled: Some(false),
        }),
    )
    .await
    .expect("turning sharing off waited for the sharing dialog")
    .unwrap() else {
        panic!("Desktop disable")
    };
    assert!(!config.desktop_enabled);
    let result = tokio::time::timeout(Duration::from_secs(5), enabling)
        .await
        .expect("the enable outlived the disable")
        .unwrap();
    assert!(result.is_err(), "a withdrawn change reported success");
    assert!(tokens.load().await.unwrap().is_none());
    assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
    let OwnerResponse::Settings {
        config, desktop, ..
    } = runtime
        .request(OwnerRequest::Desktop { enabled: None })
        .await
        .unwrap()
    else {
        panic!("Desktop status")
    };
    assert!(!config.desktop_enabled);
    assert!(!matches!(desktop, Availability::Available));
    runtime.shutdown().await.unwrap();
}

/// Answers a desktop status request with the settings and sharing state.
async fn desktop_status(runtime: &Runtime) -> (bool, Availability) {
    let OwnerResponse::Settings {
        config, desktop, ..
    } = runtime
        .request(OwnerRequest::Desktop { enabled: None })
        .await
        .unwrap()
    else {
        panic!("Desktop status")
    };
    (config.desktop_enabled, desktop)
}

#[tokio::test]
async fn sharing_is_saved_on_only_once_the_person_confirms_it() {
    let rig = Rig::new(false).await;
    let (_temp, paths, identity, mut options) = rig.shutdown().await;
    let (shown, answered) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    options.desktop_factory = Some(Arc::new(WaitingDesktop {
        shown: shown.clone(),
        answered: answered.clone(),
    }));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    answered.notify_one();
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(true),
        })
        .await
        .unwrap();
    shown.notified().await;
    // Sharing ended on the desktop and the person asks for it again. The old
    // session closes first, so until they answer nothing is shared.
    let enabling = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .request(OwnerRequest::Desktop {
                    enabled: Some(true),
                })
                .await
        }
    });
    shown.notified().await;
    assert!(
        !Config::load(&paths.config_file()).unwrap().desktop_enabled,
        "an unanswered enable was saved as on"
    );
    assert!(!desktop_status(&runtime).await.0);
    answered.notify_one();
    tokio::time::timeout(Duration::from_secs(5), enabling)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(Config::load(&paths.config_file()).unwrap().desktop_enabled);
    assert!(desktop_status(&runtime).await.0);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_ended_enable_is_reported_off_even_when_its_settings_cannot_be_saved() {
    use std::os::unix::fs::PermissionsExt;

    let rig = Rig::new(false).await;
    let (_temp, paths, identity, mut options) = rig.shutdown().await;
    let (shown, answered) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    options.desktop_factory = Some(Arc::new(WaitingDesktop {
        shown: shown.clone(),
        answered: answered.clone(),
    }));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    answered.notify_one();
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(true),
        })
        .await
        .unwrap();
    shown.notified().await;
    let cancel = CancellationToken::new();
    let enabling = tokio::spawn({
        let (runtime, cancel) = (runtime.clone(), cancel.clone());
        async move {
            runtime
                .request_progress(
                    OwnerRequest::Desktop {
                        enabled: Some(true),
                    },
                    tokio::sync::mpsc::channel(1).0,
                    cancel,
                )
                .await
        }
    });
    shown.notified().await;
    let mode = std::fs::metadata(&paths.config).unwrap().permissions();
    std::fs::set_permissions(&paths.config, std::fs::Permissions::from_mode(0o500)).unwrap();
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), enabling)
        .await
        .expect("the change waited for the sharing dialog")
        .unwrap();
    std::fs::set_permissions(&paths.config, mode).unwrap();
    let error = result.expect_err("an unconfirmed change reported success");
    assert!(
        error.to_string().contains("not turned on"),
        "the reason the change ended was lost: {error}"
    );
    let (enabled, desktop) = desktop_status(&runtime).await;
    assert!(!enabled, "status claimed sharing that had ended");
    assert!(!matches!(desktop, Availability::Available));
    assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn turning_sharing_off_closes_it_even_when_the_setting_cannot_be_saved() {
    use platform_linux::portal::{FileTokenStore, TokenStore};
    use std::os::unix::fs::PermissionsExt;

    let rig = Rig::new(false).await;
    let (_temp, paths, identity, mut options) = rig.shutdown().await;
    options.desktop_factory = Some(Arc::new(SyntheticDesktop));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(true),
        })
        .await
        .unwrap();
    let consent = paths.data.join("desktop-consent");
    std::fs::create_dir_all(&consent).unwrap();
    FileTokenStore::new(consent.clone())
        .save("synthetic-restore-token")
        .await
        .unwrap();
    assert!(desktop_status(&runtime).await.0);
    let mode = std::fs::metadata(&paths.config).unwrap().permissions();
    std::fs::set_permissions(&paths.config, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(false),
        })
        .await;
    std::fs::set_permissions(&paths.config, mode).unwrap();
    let error = result.expect_err("an unsaved change reported success");
    let (enabled, desktop) = desktop_status(&runtime).await;
    assert!(!enabled, "status says sharing is still on");
    assert!(
        !matches!(desktop, Availability::Available),
        "sharing is still live: {desktop:?}"
    );
    assert!(
        !consent.join("desktop-restore-token").exists(),
        "consent outlived the turn-off"
    );
    assert!(
        error.to_string().contains("could not be saved"),
        "the person is not told the setting is unsaved: {error}"
    );
    // Turning it off again once settings can be saved records it off.
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(false),
        })
        .await
        .unwrap();
    assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
    runtime.shutdown().await.unwrap();
}

/// A paired phone takes hold of the screen, and the owner sees it held.
async fn phone_holds_the_screen(runtime: &Runtime) -> TargetId {
    let ticket = runtime
        .hub
        .open_pairing(&services::Caller::local_owner())
        .await
        .unwrap();
    let paired = services::testing::pair_confirmed(
        runtime.hub.as_ref(),
        PairRequest {
            pairing_secret: ticket.secret,
            device_public_key: URL_SAFE_NO_PAD.encode([9; 32]),
            device_name: "Phone".into(),
            device_platform: "android".into(),
            client_version: "test".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        },
    )
    .await
    .unwrap();
    let phone = services::Caller {
        principal: Principal::Device {
            device_id: paired.device_id,
        },
        grants: paired.grants,
    };
    let Ok(QueryData::ControlTargets { targets }) =
        runtime.hub.query(&phone, Query::ControlTargets).await
    else {
        panic!("no screens to hold")
    };
    let target = targets[0].target_id;
    let lease = runtime
        .hub
        .submit(
            &phone,
            CommandEnvelope::new(Command::RequestHumanLease {
                target_id: target,
                geometry_revision: targets[0].geometry.revision,
            }),
        )
        .await;
    assert!(lease.error.is_none(), "{lease:?}");
    let input = || async {
        let Ok(QueryData::Snapshot(snapshot)) = runtime
            .hub
            .query(
                &services::Caller::local_owner(),
                Query::Snapshot {
                    reads_withheld: None,
                },
            )
            .await
        else {
            panic!("no snapshot")
        };
        snapshot
            .control
            .iter()
            .find(|view| view.target_id == target)
            .map(|view| view.input_state)
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        while input().await != Some(control::InputState::Human) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the phone never held the screen");
    target
}

#[tokio::test]
async fn a_failed_settings_change_says_first_that_input_may_still_be_held() {
    let rig = Rig::new(false).await;
    let (temp, paths, identity, mut options) = rig.shutdown().await;
    let stuck = Arc::new(std::sync::atomic::AtomicBool::new(false));
    options.desktop_factory = Some(Arc::new(StuckDesktop(stuck.clone())));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    runtime
        .hub
        .set_local_indicator(Some(Arc::new(FakeIndicator)))
        .await;
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(true),
        })
        .await
        .unwrap();
    phone_holds_the_screen(&runtime).await;
    // Another process holds the database, so adding a Library folder fails,
    // and the screen cannot confirm it has let go as the change is undone.
    let folder = temp.path().join("notes");
    std::fs::create_dir(&folder).unwrap();
    let database = paths.database().join("companion.sqlite");
    let (locked, held) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let mut store = store::Store::open(&database, store::Retention::default()).unwrap();
        store
            .write(protocol::time::Timestamp(0), |_| {
                locked.send(()).unwrap();
                let _ = released.recv();
                Ok(())
            })
            .unwrap();
    });
    held.recv().unwrap();
    stuck.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = runtime
        .request(OwnerRequest::RootsAdd {
            path: folder,
            label: None,
            writable: false,
        })
        .await;
    release.send(()).unwrap();
    holder.join().unwrap();
    let error = result.expect_err("a change that could not be applied reported success");
    let companion::Error::Protocol(error) = error else {
        panic!("a release nobody confirmed is not said first: {error}")
    };
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    assert!(
        error
            .message
            .starts_with("input to the desktop may not have been released"),
        "{error:?}"
    );
    let (enabled, desktop) = desktop_status(&runtime).await;
    assert!(!enabled, "status says sharing is still on");
    assert!(
        !matches!(desktop, Availability::Available),
        "sharing is still live: {desktop:?}"
    );
    // Once the screen answers again, turning sharing off finishes the job.
    stuck.store(false, std::sync::atomic::Ordering::SeqCst);
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(false),
        })
        .await
        .unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_program_ophio_started_reaches_only_its_tasks_tokens_and_the_rest_of_the_owners_controls()
{
    let rig = Rig::new(false).await;
    // `ophio token` finds the socket where it looks under XDG_RUNTIME_DIR.
    let xdg = rig.temp.path().join("xdg");
    std::fs::create_dir(&xdg).unwrap();
    std::os::unix::fs::symlink(rig.temp.path(), xdg.join(ports::PROGRAM_NAME)).unwrap();
    // A child of this process is one the companion started.
    let ophio = |args: &[&str], credential: Option<&str>| {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ophio"));
        command
            .args(args)
            .env_clear()
            .env("HOME", rig.temp.path())
            .env("XDG_CONFIG_HOME", rig.temp.path().join("xdg-config"))
            .env("XDG_DATA_HOME", rig.temp.path().join("xdg-data"))
            .env("XDG_RUNTIME_DIR", &xdg);
        if let Some(credential) = credential {
            command.env(ports::accounts::RENEW_ENV, credential);
        }
        command.output()
    };
    let token = |credential| ophio(&["token", "github"], credential);
    let refused = token(None).await.unwrap();
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(said.contains("started this program"), "{said}");
    // With a credential the task's request reaches the hub, which knows no
    // running task by it.
    let ended = token(Some("not-a-running-task")).await.unwrap();
    assert!(!ended.status.success());
    let said = String::from_utf8_lossy(&ended.stderr);
    assert!(said.contains("This task has ended"), "{said}");
    assert!(ended.stdout.is_empty());
    // A setup skill running in a task uses the rest of the owner's controls.
    let voice = ophio(&["voice", "status"], None).await.unwrap();
    assert!(
        voice.status.success(),
        "{}",
        String::from_utf8_lossy(&voice.stderr)
    );
    rig.shutdown().await;
}

#[tokio::test]
async fn turning_sharing_off_says_over_the_owner_socket_that_input_may_still_be_held() {
    let rig = Rig::new(false).await;
    let (_temp, paths, identity, mut options) = rig.shutdown().await;
    let stuck = Arc::new(std::sync::atomic::AtomicBool::new(false));
    options.desktop_factory = Some(Arc::new(StuckDesktop(stuck.clone())));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options.clone())
        .await
        .unwrap();
    runtime
        .hub
        .set_local_indicator(Some(Arc::new(FakeIndicator)))
        .await;
    let mut deps = ServerDeps::new(runtime.hub.clone(), identity.clone()).unwrap();
    deps.clock = options.clock.clone();
    deps.owner = Some(runtime.clone());
    let local = LocalServer::bind(
        &paths.socket(),
        companion::started::Started::claim().unwrap(),
    )
    .await
    .unwrap();
    let stop = CancellationToken::new();
    let served = tokio::spawn(local.run(deps, stop.clone()));
    let socket = paths.socket();
    let desktop = |enabled| {
        let socket = socket.clone();
        async move {
            let mut owner = OwnerClient::connect(&socket).await.unwrap();
            owner
                .request::<_, OwnerResponse>(&OwnerRequest::Desktop {
                    enabled: Some(enabled),
                })
                .await
        }
    };
    desktop(true).await.unwrap();
    phone_holds_the_screen(&runtime).await;
    // The screen stops answering, so letting go of it cannot be confirmed.
    stuck.store(true, std::sync::atomic::Ordering::SeqCst);
    let error = desktop(false)
        .await
        .expect_err("a release nobody confirmed was reported as done");
    let companion::Error::Protocol(error) = error else {
        panic!("the owner socket did not carry the error's code: {error}")
    };
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    assert!(
        error.message.starts_with(
            "Desktop sharing is off in Øphio, but input to the desktop may not have been released"
        ),
        "{error:?}"
    );
    assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
    // Once the screen answers again, turning sharing off finishes the job.
    stuck.store(false, std::sync::atomic::Ordering::SeqCst);
    desktop(false).await.unwrap();
    stop.cancel();
    served.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn turning_sharing_off_does_not_wait_on_a_library_folder_that_is_gone() {
    use platform_linux::portal::{FileTokenStore, TokenStore};

    let rig = Rig::new(false).await;
    let (temp, paths, identity, mut options) = rig.shutdown().await;
    options.desktop_factory = Some(Arc::new(SyntheticDesktop));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    let folder = temp.path().join("notes");
    std::fs::create_dir(&folder).unwrap();
    runtime
        .request(OwnerRequest::RootsAdd {
            path: folder.clone(),
            label: None,
            writable: false,
        })
        .await
        .unwrap();
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(true),
        })
        .await
        .unwrap();
    let consent = paths.data.join("desktop-consent");
    std::fs::create_dir_all(&consent).unwrap();
    FileTokenStore::new(consent.clone())
        .save("synthetic-restore-token")
        .await
        .unwrap();
    std::fs::remove_dir(&folder).unwrap();
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(false),
        })
        .await
        .expect("a removed Library folder kept sharing on");
    let (enabled, desktop) = desktop_status(&runtime).await;
    assert!(!enabled, "status says sharing is still on");
    assert!(
        !matches!(desktop, Availability::Available),
        "sharing is still live: {desktop:?}"
    );
    assert!(
        !consent.join("desktop-restore-token").exists(),
        "consent outlived the turn-off"
    );
    let saved = Config::load(&paths.config_file()).unwrap();
    assert!(!saved.desktop_enabled);
    assert_eq!(saved.roots.len(), 1, "the Library folder was dropped");
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn turning_sharing_off_forgets_consent_even_when_nothing_can_be_recorded() {
    use platform_linux::portal::{FileTokenStore, TokenStore};
    use std::os::unix::fs::PermissionsExt;

    let rig = Rig::new(false).await;
    let (_temp, paths, identity, mut options) = rig.shutdown().await;
    options.desktop_factory = Some(Arc::new(SyntheticDesktop));
    let runtime = Runtime::build(paths.clone(), Config::default(), &identity, options)
        .await
        .unwrap();
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(true),
        })
        .await
        .unwrap();
    let consent = paths.data.join("desktop-consent");
    std::fs::create_dir_all(&consent).unwrap();
    FileTokenStore::new(consent.clone())
        .save("synthetic-restore-token")
        .await
        .unwrap();
    assert!(desktop_status(&runtime).await.0);
    // Another process holds the database, so the change cannot be recorded
    // for the phone, and the settings cannot be saved either.
    let database = paths.database().join("companion.sqlite");
    let (locked, held) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let mut store = store::Store::open(&database, store::Retention::default()).unwrap();
        store
            .write(protocol::time::Timestamp(0), |_| {
                locked.send(()).unwrap();
                let _ = released.recv();
                Ok(())
            })
            .unwrap();
    });
    held.recv().unwrap();
    let mode = std::fs::metadata(&paths.config).unwrap().permissions();
    std::fs::set_permissions(&paths.config, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(false),
        })
        .await;
    std::fs::set_permissions(&paths.config, mode).unwrap();
    release.send(()).unwrap();
    holder.join().unwrap();
    let error = result.expect_err("an unrecorded change reported success");
    assert!(
        !consent.join("desktop-restore-token").exists(),
        "consent outlived the turn-off: {error}"
    );
    assert!(
        error.to_string().contains("could not be saved"),
        "the person is not told the setting is unsaved: {error}"
    );
    let (enabled, desktop) = desktop_status(&runtime).await;
    assert!(!enabled, "status says sharing is still on");
    assert!(
        !matches!(desktop, Availability::Available),
        "sharing is still live: {desktop:?}"
    );
    runtime
        .request(OwnerRequest::Desktop {
            enabled: Some(false),
        })
        .await
        .unwrap();
    assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
    runtime.shutdown().await.unwrap();
}

/// Setup's agent pane is admitted as the phone is: a request is decided at
/// the revision it was shown at, a stale one is refused, and a reply after a
/// task's turn has ended resumes it at the task's revision.
#[tokio::test]
async fn setup_decides_requests_and_resumes_ended_tasks_through_real_admission() {
    use companion::setup::probe;
    let mut rig = Rig::new(true).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let dir = rig.project_dir("setup-pane");
    let project = phone.project(&dir).await;

    // Allowing a command.
    let task = phone
        .task(project, HarnessKind::ClaudeCode, "bridge-permission")
        .await;
    phone.event(|e| matches!(e, Event::ApprovalUpserted { approval } if approval.task_id == task && approval.state == ApprovalState::Pending)).await;
    let (approval, revision) = probe::shown(&rig.paths, task).await.unwrap().unwrap();
    let stale = probe::decide(&rig.paths, approval, revision + 1, ApprovalDecision::Allow).await;
    assert!(
        matches!(&stale, Err(companion::Error::Protocol(e)) if e.code == ErrorCode::StaleRevision),
        "{stale:?}"
    );
    probe::decide(&rig.paths, approval, revision, ApprovalDecision::Allow)
        .await
        .unwrap();
    let mut at = reached(&mut phone, task, TaskState::Completed, 0).await;
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("permission-result.json")).unwrap())
            .unwrap();
    assert_eq!(result["behavior"], "allow");

    // Replies after completion, after a Stop and after a failure each resume
    // the task.
    probe::tell(&rig.paths, task, "wait").await.unwrap();
    reached(&mut phone, task, TaskState::Working, at).await;
    phone
        .command(
            Command::StopTask {
                task_id: task,
                force: false,
            },
            None,
        )
        .await;
    at = reached(&mut phone, task, TaskState::Interrupted, at).await;
    probe::tell(&rig.paths, task, "crash").await.unwrap();
    at = reached(&mut phone, task, TaskState::Failed, at).await;
    probe::tell(&rig.paths, task, "complete").await.unwrap();
    reached(&mut phone, task, TaskState::Completed, at).await;

    // Answering a question.
    std::fs::write(
        dir.join("permission-request.json"),
        serde_json::json!({
            "tool_name": "AskUserQuestion",
            "input": {"questions": [{
                "question": "Which folder?",
                "header": "Folder",
                "options": [{"label": "~/work"}, {"label": "~/notes"}],
                "multiSelect": false,
            }]},
        })
        .to_string(),
    )
    .unwrap();
    let asking = phone
        .task(project, HarnessKind::ClaudeCode, "bridge-permission")
        .await;
    phone.event(|e| matches!(e, Event::ApprovalUpserted { approval } if approval.task_id == asking && approval.kind == ApprovalKind::Question && approval.state == ApprovalState::Pending)).await;
    let (approval, revision) = probe::shown(&rig.paths, asking).await.unwrap().unwrap();
    let answer = ApprovalDecision::Answer {
        text: "~/notes".into(),
        answers: vec![QuestionAnswer {
            id: None,
            question: "Which folder?".into(),
            choices: vec!["~/notes".into()],
        }],
    };
    probe::decide(&rig.paths, approval, revision, answer)
        .await
        .unwrap();
    reached(&mut phone, asking, TaskState::Completed, 0).await;
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("permission-result.json")).unwrap())
            .unwrap();
    assert_eq!(result["behavior"], "allow");
    assert_eq!(
        result["updatedInput"]["answers"]["Which folder?"],
        "~/notes"
    );
    rig.shutdown().await;
}

/// A question an agent asks reaches the phone's alert with its words, and the
/// phone answers it from the notification as the app would: one command on a
/// control connection of its own, at the revision the alert carried.
#[tokio::test]
async fn a_question_alert_carries_the_question_and_takes_its_answer() {
    use protocol::alerts::{AlertClientFrame, AlertServerFrame};
    let mut rig = Rig::new(true).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let dir = rig.project_dir("answer-alert");
    let project = phone.project(&dir).await;
    let mut watcher = rig.client(ConnectionKind::Alerts).await;
    watcher
        .send(&AlertClientFrame::Watch {
            showing: Vec::new(),
            quiet: true,
            reports: false,
        })
        .await;
    let AlertServerFrame::Open { alerts, .. } = watcher.receive().await else {
        panic!("open list first")
    };
    assert!(alerts.is_empty(), "{alerts:?}");

    std::fs::write(
        dir.join("permission-request.json"),
        serde_json::json!({
            "tool_name": "AskUserQuestion",
            "input": {"questions": [{
                "question": "Create a private repo?",
                "header": "Repo",
                "options": [{"label": "Yes"}, {"label": "No"}],
                "multiSelect": false,
            }]},
        })
        .to_string(),
    )
    .unwrap();
    let task = phone
        .task(project, HarnessKind::ClaudeCode, "bridge-permission")
        .await;
    let alert = loop {
        if let AlertServerFrame::Raised { alert } = watcher.receive().await
            && alert.task_id == task
        {
            break alert;
        }
    };
    assert_eq!(alert.kind, AttentionKind::Question);
    let question = alert.question.expect("the alert carries its question");
    assert_eq!(question.text, "Create a private repo?");
    assert_eq!(question.asked.as_deref(), Some("Create a private repo?"));
    assert_eq!(question.choices, ["Yes", "No"]);

    let mut answering = rig.client(ConnectionKind::Control).await;
    let answer = |revision: u32| {
        let mut envelope = CommandEnvelope::new(Command::ResolveApproval {
            approval_id: question.approval_id,
            decision: ApprovalDecision::Answer {
                text: "Yes".into(),
                answers: vec![QuestionAnswer {
                    id: None,
                    question: "Create a private repo?".into(),
                    choices: vec!["Yes".into()],
                }],
            },
        });
        envelope.expected_revision = Some(revision.into());
        envelope
    };
    async fn receipt(client: &mut Client, envelope: CommandEnvelope) -> CommandReceipt {
        let id = envelope.command_id;
        client.send(&ClientFrame::Command(Box::new(envelope))).await;
        loop {
            if let ServerFrame::Receipt(r) = client.receive().await
                && r.command_id == id
            {
                return r;
            }
        }
    }
    // An answer to a question that changed since is refused.
    let stale = receipt(&mut answering, answer(question.revision + 1)).await;
    assert_eq!(stale.status, ReceiptStatus::Rejected);
    assert_eq!(stale.error.unwrap().code, ErrorCode::StaleRevision);
    let taken = receipt(&mut answering, answer(question.revision)).await;
    assert_eq!(taken.status, ReceiptStatus::Accepted, "{taken:?}");

    assert_eq!(
        watcher.receive::<AlertServerFrame>().await,
        AlertServerFrame::Resolved {
            attention_id: alert.attention_id
        }
    );
    reached(&mut phone, task, TaskState::Completed, 0).await;
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("permission-result.json")).unwrap())
            .unwrap();
    assert_eq!(result["behavior"], "allow");
    assert_eq!(
        result["updatedInput"]["answers"]["Create a private repo?"],
        "Yes"
    );
    rig.shutdown().await;
}

/// The revision `task` had when it next reached `state` after revision `after`.
async fn reached(phone: &mut Phone, task: TaskId, state: TaskState, after: u64) -> u64 {
    let event = phone.event(|e| matches!(e, Event::TaskUpserted { task: t } if t.task_id == task && t.revision > after && t.state == state)).await;
    let Event::TaskUpserted { task } = event else {
        unreachable!()
    };
    task.revision
}
