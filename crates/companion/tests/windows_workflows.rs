// SPDX-License-Identifier: Apache-2.0
//! Connected Windows workflows, using only temporary files and local listeners.
//! No real agents, accounts, desktop input, service installation or phone needed.
#![cfg(windows)]

mod support;

use companion::{
    Server, ServerDeps,
    compose::{ComposeOptions, Runtime},
    config::{Config, Paths},
    identity::{FileSecretStore, Identity},
    local::{LocalServer, OwnerClient, OwnerRequest, OwnerResponse, OwnerRuntime},
};
use ports::terminal::{TerminalEvent, TerminalHost, TerminalSpec};
use protocol::{
    Command, CommandEnvelope, Query, QueryData,
    command::{CommandResult, CommandStage, DownloadSource, ReceiptStatus},
    library::SaveOutcome,
    transfer::{TransferDestination, UploadCollision},
};
use services::{Caller, Hub};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

async fn effect(hub: &dyn Hub, command: Command) -> CommandResult {
    perform(hub, command)
        .await
        .expect("command must return its result")
}

async fn perform(hub: &dyn Hub, command: Command) -> Option<CommandResult> {
    let caller = Caller::local_owner();
    let receipt = hub.submit(&caller, CommandEnvelope::new(command)).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    if let Some(result) = receipt.result {
        return Some(result);
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let QueryData::CommandStatus {
                stage,
                result,
                error,
                ..
            } = hub
                .query(
                    &caller,
                    Query::CommandStatus {
                        command_id: receipt.command_id,
                    },
                )
                .await
                .unwrap()
            else {
                panic!("command status missing")
            };
            assert!(error.is_none(), "{error:?}");
            if stage == Some(CommandStage::Observed) {
                return result.map(|result| *result);
            }
            assert!(
                matches!(
                    stage,
                    Some(CommandStage::HostAccepted | CommandStage::NativeAcknowledged)
                ),
                "command did not complete successfully: {stage:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("command did not finish")
}

#[tokio::test]
async fn isolated_runtime_library_terminal_transfer_preview_and_restart() {
    tokio::time::timeout(Duration::from_secs(90), workflow(false))
        .await
        .expect("Windows workflow timed out");
}

// Windows reserves one owner pipe per user, regardless of Paths. Never connect
// to an existing instance: binding must succeed before creating the client.
#[tokio::test]
#[ignore = "requires a disposable Windows user with no running Ophio instance"]
async fn exclusive_user_named_pipe_workflow() {
    tokio::time::timeout(Duration::from_secs(90), workflow(true))
        .await
        .expect("Windows named-pipe workflow timed out");
}

async fn workflow(use_pipe: bool) {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths {
        config: temp.path().join("config"),
        data: temp.path().join("data"),
        runtime: temp.path().join("runtime"),
    };
    paths.initialize().unwrap();
    let lock = paths.lock().unwrap();
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("notes.md"), "original\r\n").unwrap();
    for (folder, name) in [(".claude", "CLAUDE.md"), (".codex", "AGENTS.md")] {
        std::fs::create_dir_all(home.join(folder)).unwrap();
        std::fs::write(home.join(folder).join(name), "User instruction fixture\n").unwrap();
    }
    std::fs::create_dir_all(home.join(".claude/skills/windows-fixture")).unwrap();
    std::fs::write(
        home.join(".claude/skills/windows-fixture/SKILL.md"),
        "---\nname: windows-fixture\ndescription: Isolated Windows fixture\n---\nFixture skill\n",
    )
    .unwrap();
    let mut environment = BTreeMap::from([
        ("USERPROFILE".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), String::new()),
    ]);
    // ConPTY needs Windows' own shell, not any installed agent or user profile.
    for key in ["SystemRoot", "WINDIR", "COMSPEC", "TEMP", "TMP"] {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.into(), value);
        }
    }
    let options = ComposeOptions {
        environment,
        executable_dir: temp.path().join("no-programs"),
        clock: Arc::new(services::testing::FakeClock::default()),
        claude_binary: Some(temp.path().join("no-claude.exe")),
        codex_binary: Some(temp.path().join("no-codex.exe")),
        desktop_factory: None,
        speech_factory: Arc::new(companion::frontdoors::LocalSpeech),
        accounts: false,
        release_check: false,
    };
    let identity = Arc::new(
        Identity::load_or_create(
            &FileSecretStore::new(paths.identity()),
            vec!["localhost".into()],
        )
        .unwrap(),
    );
    let config = Config::default();
    config.save(&paths.config_file()).unwrap();
    let runtime = Runtime::build(paths.clone(), config, &identity, options.clone())
        .await
        .unwrap();
    let caller = Caller::local_owner();
    let QueryData::AgentFiles { files, .. } =
        runtime.hub.query(&caller, Query::AgentFiles).await.unwrap()
    else {
        panic!("Library agent files missing")
    };
    for (folder, name) in [
        (".claude", "CLAUDE.md"),
        (".codex", "AGENTS.md"),
        (".claude/skills/windows-fixture", "SKILL.md"),
    ] {
        let expected = home.join(folder).join(name);
        let file = files
            .iter()
            .find(|file| std::path::Path::new(&file.host_path) == expected)
            .unwrap_or_else(|| {
                panic!(
                    "USERPROFILE-only Library missed {}: {files:?}",
                    expected.display()
                )
            });
        assert!(file.everywhere);
        let QueryData::LibraryRead { file } = runtime
            .hub
            .query(
                &caller,
                Query::LibraryRead {
                    root_id: file.root_id,
                    path: file.path.clone(),
                },
            )
            .await
            .unwrap()
        else {
            panic!("discovered user instruction or skill cannot be read")
        };
        assert!(file.text.contains("fixture"));
    }
    println!(
        "PASS: USERPROFILE-only runtime discovers and reads both agents' user instructions and a skill"
    );

    let stop = CancellationToken::new();
    let mut serving = None;
    let mut owner = None;
    if use_pipe {
        let mut deps = ServerDeps::new(runtime.hub.clone(), identity.clone()).unwrap();
        deps.owner = Some(runtime.clone());
        let local = LocalServer::bind(
            &paths.socket(),
            companion::started::Started::claim().unwrap(),
        )
        .await
        .expect("exclusive owner pipe unavailable; do not connect to an installed instance");
        serving = Some(tokio::spawn(local.run(deps, stop.clone())));
        let client = OwnerClient::connect(&paths.socket()).await.unwrap();
        assert_eq!(client.server(), std::process::id());
        owner = Some(client);
    }
    let request = OwnerRequest::RootsAdd {
        path: project.clone(),
        label: Some("Windows fixture".into()),
        writable: true,
    };
    let response: OwnerResponse = match owner.as_mut() {
        Some(owner) => owner.request(&request).await.unwrap(),
        None => runtime.request(request).await.unwrap(),
    };
    let OwnerResponse::Settings {
        config,
        applied_live,
        ..
    } = response
    else {
        panic!("named-pipe settings response missing")
    };
    assert!(applied_live);
    let root = config.roots[0].root_id;
    assert_eq!(
        Config::load(&paths.config_file()).unwrap().roots[0].root_id,
        root
    );
    let QueryData::LibraryRead { file } = runtime
        .hub
        .query(
            &caller,
            Query::LibraryRead {
                root_id: root,
                path: "notes.md".into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("Library read missing")
    };
    let save = |text: &str| Command::LibrarySave {
        root_id: root,
        path: "notes.md".into(),
        text: text.into(),
        base_revision: file.revision.clone(),
        newline: file.newline,
        bom: file.has_bom,
    };
    assert!(matches!(
        effect(runtime.hub.as_ref(), save("changed\n")).await,
        CommandResult::Saved {
            outcome: SaveOutcome::Saved { .. }
        }
    ));
    assert!(matches!(
        effect(runtime.hub.as_ref(), save("stale\n")).await,
        CommandResult::Saved {
            outcome: SaveOutcome::Conflict { .. }
        }
    ));
    assert_eq!(
        std::fs::read(project.join("notes.md")).unwrap(),
        b"changed\r\n"
    );
    println!(
        "PASS: owner settings, Windows Library save, CRLF preservation and stale-save conflict"
    );

    let CommandResult::ProjectAdded { project_id } = effect(
        runtime.hub.as_ref(),
        Command::AddProject {
            host_path: project.to_string_lossy().into_owned(),
            name: Some("Windows fixture".into()),
        },
    )
    .await
    else {
        panic!("project missing")
    };
    for (harness, folder, name) in [
        (
            protocol::capability::HarnessKind::ClaudeCode,
            ".claude",
            "CLAUDE.md",
        ),
        (
            protocol::capability::HarnessKind::Codex,
            ".codex",
            "AGENTS.md",
        ),
    ] {
        perform(
            runtime.hub.as_ref(),
            Command::UpdateProjectDefaults {
                project_id,
                harness: Some(harness),
                model: None,
                effort: None,
            },
        )
        .await;
        let QueryData::Instructions { sources } = runtime
            .hub
            .query(
                &caller,
                Query::Instructions {
                    project_id,
                    task_id: None,
                },
            )
            .await
            .unwrap()
        else {
            panic!("resolved instructions missing")
        };
        let expected = home.join(folder).join(name);
        assert!(
            sources.iter().any(|source| {
                source.host_path.as_deref().map(std::path::Path::new) == Some(expected.as_path())
                    && source.scope == protocol::library::InstructionScope::User
                    && source.state == protocol::library::InstructionState::Discovered
            }),
            "USERPROFILE-only instruction resolver missed {}: {sources:?}",
            expected.display()
        );
        if harness == protocol::capability::HarnessKind::ClaudeCode {
            let expected = home.join(".claude/skills/windows-fixture/SKILL.md");
            assert!(
                sources.iter().any(|source| {
                    source.host_path.as_deref().map(std::path::Path::new)
                        == Some(expected.as_path())
                        && source.scope == protocol::library::InstructionScope::Skill
                        && source.state == protocol::library::InstructionState::Discovered
                }),
                "USERPROFILE-only instruction resolver missed skill: {sources:?}"
            );
        }
    }
    println!(
        "PASS: USERPROFILE-only project instruction resolver discovers both user files and skill"
    );
    // This tests the native adapter, not Hub terminal transport. Default Hub
    // terminals use the real user's shell/profile, so never open one in CI.
    let pty = terminal::PtyHost::default();
    let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    assert!(system.is_absolute());
    let shell = system.join("System32/WindowsPowerShell/v1.0/powershell.exe");
    assert!(shell.is_file(), "Windows PowerShell missing");
    let mut shell_env: Vec<_> = options.environment.clone().into_iter().collect();
    let modules = shell.parent().unwrap().join("Modules");
    assert!(
        modules.is_dir(),
        "Windows PowerShell system modules missing"
    );
    shell_env.retain(|(key, _)| !key.eq_ignore_ascii_case("PSModulePath"));
    shell_env.push((
        "PSModulePath".into(),
        modules.to_string_lossy().into_owned(),
    ));
    for key in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
    ] {
        shell_env.retain(|(name, _)| !name.eq_ignore_ascii_case(key));
        shell_env.push((key.into(), home.to_string_lossy().into_owned()));
    }
    let terminal = pty
        .open(TerminalSpec {
            title: "Profile-free Windows fixture".into(),
            cwd: project.clone(),
            program: Some((shell, vec!["-NoLogo".into(), "-NoProfile".into()])),
            env: shell_env,
            project_id: Some(project_id),
            harness: None,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();
    let (replay, mut terminal_output) = pty.attach(terminal.terminal_id, Some(0)).await.unwrap();
    pty.write(
        terminal.terminal_id,
        b"Write-Output ('WINDOWS_' + 'WORKFLOW_OK')\r\n",
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut output = replay.bytes.to_vec();
        loop {
            if String::from_utf8_lossy(&output).contains("WINDOWS_WORKFLOW_OK") {
                return;
            }
            match terminal_output.recv().await.unwrap() {
                TerminalEvent::Output { bytes, .. } => output.extend_from_slice(&bytes),
                TerminalEvent::Exited { .. } => panic!("terminal ended before command output"),
                TerminalEvent::Resized { .. } => {}
            }
        }
    })
    .await
    .expect("PowerShell output never arrived");
    pty.close(terminal.terminal_id).await.unwrap();
    assert!(matches!(
        pty.info(terminal.terminal_id).await.unwrap().state,
        protocol::terminal::TerminalState::Exited { .. }
    ));
    println!("PASS: native ConPTY adapter runs profile-free PowerShell and confirms exited state");

    let bytes = b"Windows transfer fixture\n";
    let sha = companion::identity::fingerprint(bytes);
    let CommandResult::Transfer { transfer } = effect(
        runtime.hub.as_ref(),
        Command::CreateUploadReviewed {
            task_id: None,
            name: "notes.md".into(),
            collision: UploadCollision::KeepBoth {
                name: "received.md".into(),
            },
            size: bytes.len() as u64,
            sha256: sha.clone(),
            destination: TransferDestination::ProjectDir {
                project_id,
                dir: "".into(),
            },
        },
    )
    .await
    else {
        panic!("upload missing")
    };
    let mut upload = runtime
        .hub
        .upload(&caller, transfer.transfer_id)
        .await
        .unwrap();
    assert_eq!(upload.resume_at(), 0);
    upload.write(bytes).await.unwrap();
    assert_eq!(upload.finish().await.unwrap().sha256, sha);
    assert_eq!(std::fs::read(project.join("received.md")).unwrap(), bytes);
    let CommandResult::Transfer { transfer } = effect(
        runtime.hub.as_ref(),
        Command::CreateDownload {
            shown_in_task: None,
            source: DownloadSource::ProjectFile {
                project_id,
                path: "received.md".into(),
            },
        },
    )
    .await
    else {
        panic!("download missing")
    };
    let mut download = runtime
        .hub
        .download(&caller, transfer.transfer_id, 0)
        .await
        .unwrap();
    let mut received = Vec::new();
    download.reader.read_to_end(&mut received).await.unwrap();
    assert_eq!(received, bytes);
    println!("PASS: verified upload and download through real Windows filesystem");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\npreview",
                )
                .await
                .unwrap();
        }
    });
    let CommandResult::PreviewRegistered { preview_id } = effect(
        runtime.hub.as_ref(),
        Command::RegisterPreview {
            project_id,
            label: "Windows preview".into(),
            url,
        },
    )
    .await
    else {
        panic!("preview missing")
    };
    perform(
        runtime.hub.as_ref(),
        Command::CheckPreview {
            preview_id,
            start: false,
        },
    )
    .await;
    let mut preview = runtime.hub.open_preview(&caller, preview_id).await.unwrap();
    preview
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    preview.read_to_string(&mut response).await.unwrap();
    assert!(response.ends_with("preview"), "{response}");
    server.await.unwrap();
    println!("PASS: manually registered preview checked and privately forwarded");

    drop((owner, terminal_output, download, preview));
    stop.cancel();
    if let Some(serving) = serving {
        serving.await.unwrap().unwrap();
    }
    runtime.shutdown().await.unwrap();
    let hub = runtime.hub.as_ref().clone();
    drop(runtime);
    services::testing::drop_hub_and_wait(hub, Duration::from_secs(10)).await;
    // The fixture owns the instance guard; Runtime::shutdown does not drop it.
    assert!(
        paths.lock().is_err(),
        "fixture instance guard stopped holding its lock"
    );
    drop(lock);
    let _lock = paths
        .lock()
        .expect("fixture instance lock was not released after explicit drop");
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
    let QueryData::LibraryRead { file } = runtime
        .hub
        .query(
            &caller,
            Query::LibraryRead {
                root_id: root,
                path: "notes.md".into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("Library file missing after restart")
    };
    assert_eq!(file.text, "changed\r\n");
    runtime.shutdown().await.unwrap();
    println!(
        "PASS: fixture releases and reacquires instance guard; identity, Library settings and edits survive restart"
    );
}

/// A phone's real pinned-TLS control connection, without a phone, owner pipe,
/// installed application, native agent, desktop, account or fixed TCP port.
#[tokio::test]
async fn phone_protocol_browses_adds_and_removes_a_windows_project_without_changing_files() {
    tokio::time::timeout(Duration::from_secs(60), project_phone_workflow())
        .await
        .expect("Windows project phone workflow timed out");
}

async fn phone_query(client: &mut support::Client, query: Query) -> QueryData {
    let query_id = protocol::QueryId::new();
    client
        .send(&protocol::ClientFrame::Query(protocol::QueryEnvelope {
            query_id,
            query,
        }))
        .await;
    loop {
        match client.receive::<protocol::ServerFrame>().await {
            protocol::ServerFrame::QueryResult(answer) if answer.query_id == query_id => {
                return answer.result.unwrap();
            }
            protocol::ServerFrame::Fatal(error) => panic!("phone query failed: {error:?}"),
            _ => {}
        }
    }
}

async fn phone_command(client: &mut support::Client, command: Command) -> Option<CommandResult> {
    let command = CommandEnvelope::new(command);
    let command_id = command.command_id;
    client
        .send(&protocol::ClientFrame::Command(Box::new(command)))
        .await;
    loop {
        match client.receive::<protocol::ServerFrame>().await {
            protocol::ServerFrame::Receipt(receipt) if receipt.command_id == command_id => {
                assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
                return receipt.result;
            }
            protocol::ServerFrame::Fatal(error) => panic!("phone command failed: {error:?}"),
            _ => {}
        }
    }
}

async fn phone_folders(
    client: &mut support::Client,
    path: Option<&std::path::Path>,
) -> protocol::project_folders::ProjectFolderListing {
    let QueryData::ProjectFolders { listing } = phone_query(
        client,
        Query::ProjectFolders {
            path: path.map(|path| path.to_str().unwrap().to_owned()),
            after: None,
            show_hidden: true,
            search: String::new(),
        },
    )
    .await
    else {
        panic!("folder listing missing")
    };
    listing
}

async fn authenticated_phone(
    runtime: &Runtime,
    identity: &Identity,
    address: std::net::SocketAddr,
) -> support::Client {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use protocol::auth::{ConnectionKind, PairRequest, TLS_EXPORTER_LABEL};

    let ticket = runtime
        .hub
        .open_pairing(&Caller::local_owner())
        .await
        .unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[17; 32]);
    let device = services::testing::pair_confirmed(
        runtime.hub.as_ref(),
        PairRequest {
            pairing_secret: ticket.secret,
            device_public_key: URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
            device_name: "Isolated project phone".into(),
            device_platform: "android".into(),
            client_version: "fixture".into(),
            protocol: protocol::ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        },
    )
    .await
    .unwrap();
    let tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(support::PinVerifier(identity.fingerprint())))
    .with_no_client_auth();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(tls_config))
        .connect(
            rustls::pki_types::ServerName::try_from("localhost").unwrap(),
            tokio::net::TcpStream::connect(address).await.unwrap(),
        )
        .await
        .unwrap();
    let exporter = tls
        .get_ref()
        .1
        .export_keying_material([0; 32], TLS_EXPORTER_LABEL, None)
        .unwrap();
    let (socket, _) = tokio_tungstenite::client_async("wss://localhost/v1/control", tls)
        .await
        .unwrap();
    let mut client = support::Client {
        socket,
        exporter,
        kind: ConnectionKind::Control,
        nonce: [11; 32],
    };
    let challenge = client.challenge(device.device_id).await;
    let proof = client.proof(&challenge, &key, device.device_id);
    client
        .send(&protocol::ClientFrame::Authenticate(proof))
        .await;
    assert!(matches!(
        client.receive::<protocol::ServerFrame>().await,
        protocol::ServerFrame::Authenticated(_)
    ));

    client
}

async fn project_phone_workflow() {
    use std::os::windows::process::CommandExt;

    let temp = tempfile::tempdir().unwrap();
    let base = ports::library::canonical(temp.path()).unwrap();
    let home = base.join("home");
    let project = home.join("Projects").join("Café notes");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("notes.txt"), b"unchanged\r\n").unwrap();
    let paths = Paths {
        config: base.join("config"),
        data: base.join("data"),
        runtime: base.join("runtime"),
    };
    paths.initialize().unwrap();
    let options = ComposeOptions {
        environment: BTreeMap::from([
            ("USERPROFILE".into(), home.to_string_lossy().into_owned()),
            ("PATH".into(), String::new()),
        ]),
        executable_dir: base.join("no-programs"),
        clock: Arc::new(services::testing::FakeClock::default()),
        claude_binary: Some(base.join("no-claude.exe")),
        codex_binary: Some(base.join("no-codex.exe")),
        desktop_factory: None,
        speech_factory: Arc::new(companion::frontdoors::LocalSpeech),
        accounts: false,
        release_check: false,
    };
    let identity =
        Arc::new(Identity::generate(protocol::HostId::new(), vec!["localhost".into()]).unwrap());
    let runtime = Runtime::build(paths, Config::default(), &identity, options)
        .await
        .unwrap();
    let deps = ServerDeps::new(runtime.hub.clone(), identity.clone()).unwrap();
    let server = Server::bind(deps, &["127.0.0.1:0".parse().unwrap()])
        .await
        .unwrap();
    let address = server.local_addresses().unwrap()[0];
    assert_ne!(address.port(), 47100);
    let stop = CancellationToken::new();
    let serving = tokio::spawn(server.run(stop.clone()));
    let mut client = authenticated_phone(&runtime, &identity, address).await;

    let places = phone_folders(&mut client, None).await;
    assert!(
        places.folders.iter().any(|folder| folder.path == "C:\\"),
        "C: drive missing: {places:?}"
    );
    assert!(
        places
            .folders
            .iter()
            .any(|folder| folder.name == "Home" && std::path::Path::new(&folder.path) == home)
    );
    // Only the disposable-VM runner creates/removes a SUBST mapping. This test
    // never changes drive mappings when run on an ordinary development host.
    if let Ok(drive) = std::env::var("OPHIO_TEST_PROJECT_DRIVE") {
        let drive = drive.to_ascii_uppercase();
        assert!(
            drive.len() == 3
                && drive.as_bytes()[0].is_ascii_alphabetic()
                && drive.ends_with(":\\")
                && drive != "C:\\",
            "OPHIO_TEST_PROJECT_DRIVE must name a fixture drive root, such as Z:\\"
        );
        let expected = ports::library::canonical(std::path::Path::new(&drive))
            .expect("the VM runner must create the fixture drive before starting this test");
        assert!(
            places.folders.iter().any(
                |folder| folder.name == drive && std::path::Path::new(&folder.path) == expected
            ),
            "fixture drive is missing from starting places: {places:?}"
        );
        let opened = phone_folders(&mut client, Some(std::path::Path::new(&drive))).await;
        assert_eq!(
            std::path::Path::new(&opened.current.expect("fixture drive listing").path),
            expected
        );
        let authored = tempfile::tempdir_in(&drive).unwrap();
        let drive_project = authored.path().join("Café second drive");
        std::fs::create_dir(&drive_project).unwrap();
        std::fs::write(drive_project.join("notes.txt"), b"unchanged\r\n").unwrap();
        let listing = phone_folders(&mut client, Some(authored.path())).await;
        assert_eq!(listing.folders.len(), 1);
        assert_eq!(listing.folders[0].name, "Café second drive");
        phone_project_roundtrip(&mut client, &drive_project).await;
        println!(
            "PASS: phone discovers {drive}, opens an authored child and adds/lists/removes its project without changing bytes"
        );
    } else {
        println!("NOT EXERCISED: VM runner did not supply OPHIO_TEST_PROJECT_DRIVE");
    }
    let listing = phone_folders(&mut client, Some(&home.join("Projects"))).await;
    assert_eq!(listing.folders.len(), 1);
    assert_eq!(listing.folders[0].name, "Café notes");
    assert!(listing.folders[0].project_id.is_none());
    let opened = phone_folders(&mut client, Some(&project)).await;
    assert_eq!(std::path::Path::new(&opened.current.unwrap().path), project);
    println!(
        "PASS: pinned TLS phone browses C:, Home and a folder with a space and non-ASCII letter on {address}"
    );

    let junction = home.join("Projects").join("project junction");
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .arg(&junction)
        .arg(&project)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let listing = phone_folders(&mut client, Some(&home.join("Projects"))).await;
    assert_eq!(
        listing.folders.len(),
        1,
        "junction must not be followed during discovery"
    );
    let opened = phone_folders(&mut client, Some(&junction)).await;
    assert_eq!(std::path::Path::new(&opened.current.unwrap().path), project);
    std::fs::remove_dir(&junction).unwrap();
    println!(
        "PASS: junction is hidden during discovery and explicit navigation resolves its real target"
    );

    phone_project_roundtrip(&mut client, &project).await;
    println!(
        "PASS: phone adds, lists and removes the project; its directory and file bytes stay unchanged"
    );

    drop(client);
    stop.cancel();
    serving.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}

async fn phone_project_roundtrip(client: &mut support::Client, project: &std::path::Path) {
    let canonical = ports::library::canonical(project).unwrap();
    let Some(CommandResult::ProjectAdded { project_id }) = phone_command(
        client,
        Command::AddProject {
            host_path: project.to_str().unwrap().into(),
            name: None,
        },
    )
    .await
    else {
        panic!("project addition missing")
    };
    let QueryData::Snapshot(snapshot) = phone_query(
        client,
        Query::Snapshot {
            reads_withheld: Some(true),
            account_items: true,
        },
    )
    .await
    else {
        panic!("snapshot missing")
    };
    assert!(
        snapshot
            .projects
            .iter()
            .any(|entry| entry.project_id == project_id
                && std::path::Path::new(&entry.host_path) == canonical)
    );
    let current = phone_folders(client, Some(project)).await.current.unwrap();
    assert_eq!(current.project_id, Some(project_id));
    assert_eq!(std::path::Path::new(&current.path), canonical);
    phone_command(client, Command::RemoveProject { project_id }).await;
    let QueryData::Snapshot(snapshot) = phone_query(
        client,
        Query::Snapshot {
            reads_withheld: Some(true),
            account_items: true,
        },
    )
    .await
    else {
        panic!("snapshot missing")
    };
    assert!(
        !snapshot
            .projects
            .iter()
            .any(|entry| entry.project_id == project_id)
    );
    assert!(
        phone_folders(client, Some(project))
            .await
            .current
            .unwrap()
            .project_id
            .is_none()
    );
    assert_eq!(
        std::fs::read(project.join("notes.txt")).unwrap(),
        b"unchanged\r\n"
    );
    assert_eq!(std::fs::read_dir(project).unwrap().count(), 1);
}

/// The VM driver supplies a real, already formatted volume, never a SUBST
/// mapping or a mocked capability. Only the unique child belongs to this test.
#[tokio::test]
#[ignore = "requires a real FAT32 volume root in OPHIO_LIBRARY_TEST_FAT32 and NTFS temporary storage"]
async fn real_fat32_root_starts_and_reads_over_tls_but_refuses_saving() {
    tokio::time::timeout(Duration::from_secs(60), fat32_phone_workflow())
        .await
        .expect("Windows FAT32 companion workflow timed out");
}

fn assert_volume_format(path: &std::path::Path, expected: &str) {
    use std::os::windows::process::CommandExt;

    // DriveInfo queries the actual filesystem, without formatting, mounting,
    // changing drive mappings, loading a user profile or opening a window.
    let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let output = std::process::Command::new(
        system.join("System32/WindowsPowerShell/v1.0/powershell.exe"),
    )
    .args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "([System.IO.DriveInfo]::new([System.IO.Path]::GetPathRoot($env:OPHIO_VOLUME_FIXTURE))).DriveFormat",
    ])
    .env_clear()
    .env("SystemRoot", &system)
    .env("OPHIO_VOLUME_FIXTURE", path)
    .creation_flags(0x08000000) // CREATE_NO_WINDOW
    .output()
    .unwrap();
    assert!(output.status.success(), "volume query failed: {output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
}

async fn fat32_phone_workflow() {
    use companion::config::LibraryRootConfig;
    use protocol::library::RootKind;

    let volume = std::env::var("OPHIO_LIBRARY_TEST_FAT32")
        .expect("the VM driver must supply an existing real FAT32 volume root");
    assert!(
        volume.len() == 3 && volume.as_bytes()[0].is_ascii_alphabetic() && volume.ends_with(":\\"),
        "OPHIO_LIBRARY_TEST_FAT32 must name a real drive root, such as F:\\"
    );
    let volume = std::path::Path::new(&volume);
    assert!(volume.is_dir(), "the VM driver must prepare the volume");
    assert_volume_format(volume, "FAT32");
    let fat = tempfile::Builder::new()
        .prefix("ophio-companion-fat32-")
        .tempdir_in(volume)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    assert_volume_format(temp.path(), "NTFS");
    let home = temp.path().join("home");
    let ntfs = temp.path().join("ntfs-root");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&ntfs).unwrap();
    std::fs::write(fat.path().join("note.md"), b"FAT32 original\r\n").unwrap();
    std::fs::write(ntfs.join("note.md"), b"NTFS original\r\n").unwrap();
    let paths = Paths {
        config: temp.path().join("config"),
        data: temp.path().join("data"),
        runtime: temp.path().join("runtime"),
    };
    paths.initialize().unwrap();
    let _lock = paths.lock().unwrap();
    let fat_id = protocol::LibraryRootId::new();
    let ntfs_id = protocol::LibraryRootId::new();
    let config = Config {
        port: 0,
        roots: [(fat_id, fat.path()), (ntfs_id, ntfs.as_path())]
            .into_iter()
            .map(|(root_id, path)| LibraryRootConfig {
                root_id,
                label: "Isolated volume fixture".into(),
                path: path.to_owned(),
                writable: true,
                kind: RootKind::Notes,
                project_id: None,
                harness: None,
            })
            .collect(),
        ..Config::default()
    };
    config.save(&paths.config_file()).unwrap();
    let options = ComposeOptions {
        environment: BTreeMap::from([
            ("USERPROFILE".into(), home.to_string_lossy().into_owned()),
            ("PATH".into(), String::new()),
        ]),
        executable_dir: temp.path().join("no-programs"),
        clock: Arc::new(services::testing::FakeClock::default()),
        claude_binary: Some(temp.path().join("no-claude.exe")),
        codex_binary: Some(temp.path().join("no-codex.exe")),
        desktop_factory: None,
        speech_factory: Arc::new(companion::frontdoors::LocalSpeech),
        accounts: false,
        release_check: false,
    };
    let identity =
        Arc::new(Identity::generate(protocol::HostId::new(), vec!["localhost".into()]).unwrap());
    let runtime = Runtime::build(paths, config, &identity, options)
        .await
        .expect("the full companion must start with FAT32 and NTFS roots already configured");
    let deps = ServerDeps::new(runtime.hub.clone(), identity.clone()).unwrap();
    let server = Server::bind(deps, &["127.0.0.1:0".parse().unwrap()])
        .await
        .unwrap();
    let address = server.local_addresses().unwrap()[0];
    assert_ne!(address.port(), 47100);
    let stop = CancellationToken::new();
    let serving = tokio::spawn(server.run(stop.clone()));
    let mut client = authenticated_phone(&runtime, &identity, address).await;
    let QueryData::Snapshot(snapshot) = phone_query(
        &mut client,
        Query::Snapshot {
            reads_withheld: Some(true),
            account_items: true,
        },
    )
    .await
    else {
        panic!("ready companion snapshot missing")
    };
    for root_id in [fat_id, ntfs_id] {
        assert!(
            snapshot
                .library_roots
                .iter()
                .any(|root| root.root_id == root_id && root.writable),
            "startup dropped or disabled a configured root: {snapshot:?}"
        );
    }
    let QueryData::LibraryList { listing } = phone_query(
        &mut client,
        Query::LibraryList {
            root_id: fat_id,
            dir: String::new(),
        },
    )
    .await
    else {
        panic!("FAT32 listing missing")
    };
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].path, "note.md");
    let QueryData::LibraryRead { file } = phone_query(
        &mut client,
        Query::LibraryRead {
            root_id: fat_id,
            path: "note.md".into(),
        },
    )
    .await
    else {
        panic!("FAT32 file missing")
    };
    assert_eq!(file.text, "FAT32 original\r\n");
    let command = CommandEnvelope::new(Command::LibrarySave {
        root_id: fat_id,
        path: file.path,
        text: "must not save\n".into(),
        base_revision: file.revision,
        newline: file.newline,
        bom: file.has_bom,
    });
    let command_id = command.command_id;
    client
        .send(&protocol::ClientFrame::Command(Box::new(command)))
        .await;
    loop {
        match client.receive::<protocol::ServerFrame>().await {
            protocol::ServerFrame::Receipt(receipt) if receipt.command_id == command_id => {
                assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
                assert!(receipt.error.is_none() && receipt.result.is_none());
                break;
            }
            protocol::ServerFrame::Fatal(error) => panic!("save connection failed: {error:?}"),
            _ => {}
        }
    }
    let refusal = loop {
        let QueryData::CommandStatus {
            stage,
            error,
            result,
            ..
        } = phone_query(&mut client, Query::CommandStatus { command_id }).await
        else {
            panic!("FAT32 save status missing")
        };
        assert!(result.is_none(), "FAT32 must never report a saved file");
        if let Some(error) = error {
            assert_eq!(stage, Some(CommandStage::Failed));
            break error;
        }
        assert!(matches!(
            stage,
            Some(CommandStage::HostAccepted | CommandStage::NativeAcknowledged)
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(refusal.code, protocol::ErrorCode::Unsupported);
    assert_eq!(
        refusal.message,
        "This drive's format does not support the safe saving Ophio needs. Reading still works."
    );
    for (root_id, expected) in [
        (fat_id, "FAT32 original\r\n"),
        (ntfs_id, "NTFS original\r\n"),
    ] {
        let QueryData::LibraryRead { file } = phone_query(
            &mut client,
            Query::LibraryRead {
                root_id,
                path: "note.md".into(),
            },
        )
        .await
        else {
            panic!("configured root cannot be read after refused save")
        };
        assert_eq!(file.text, expected);
    }
    assert_eq!(
        std::fs::read(fat.path().join("note.md")).unwrap(),
        b"FAT32 original\r\n"
    );
    assert_eq!(std::fs::read_dir(fat.path()).unwrap().count(), 1);
    drop(client);
    stop.cancel();
    serving.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
    drop(runtime);
    fat.close().unwrap();
    println!(
        "PASS: full companion starts with real FAT32 and NTFS roots; authenticated pinned TLS lists and reads FAT32, refuses saving with the plain format message, and keeps both roots readable"
    );
}
