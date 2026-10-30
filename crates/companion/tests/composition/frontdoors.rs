// SPDX-License-Identifier: Apache-2.0
use super::*;
use companion::frontdoors::{
    ChannelCommand, Connector, FrontdoorStatus, SpeechFactory, TokenInput, VoiceCommand,
    VoiceProgress,
};
use protocol::media::{SpeechClientFrame, SpeechServerFrame};
use serde_json::{Value, json};
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, Default)]
struct FixtureSpeech {
    opens: AtomicUsize,
    stall: bool,
}
#[async_trait::async_trait]
impl SpeechFactory for FixtureSpeech {
    fn open(
        &self,
        directory: &std::path::Path,
        name: &str,
    ) -> companion::Result<Arc<dyn ports::SpeechEngine>> {
        assert_eq!(
            std::fs::read(directory.join(name).join("config.json"))?,
            b"fixture"
        );
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(services::testing::FakeSpeech))
    }
    async fn install(
        &self,
        directory: &std::path::Path,
        name: &str,
        progress: tokio::sync::mpsc::Sender<OwnerResponse>,
    ) -> companion::Result<()> {
        let stage = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(directory)?;
        progress
            .send(OwnerResponse::VoiceProgress(VoiceProgress {
                model: name.into(),
                file: "config.json".into(),
                downloaded_bytes: 1,
                total_bytes: 3,
            }))
            .await
            .map_err(|_| companion::Error::Closed)?;
        if self.stall {
            std::future::pending::<()>().await;
        }
        for file in companion::frontdoors::model(name)?.files {
            std::fs::write(stage.path().join(file.name), b"fixture")?;
        }
        std::fs::rename(stage.path(), directory.join(name))?;
        Ok(())
    }
}
async fn owner(rig: &Rig, request: OwnerRequest) -> FrontdoorStatus {
    let mut client = OwnerClient::connect(&rig.paths.socket()).await.unwrap();
    let OwnerResponse::Frontdoors(status) = client.request(&request).await.unwrap() else {
        panic!()
    };
    status
}
async fn voice(rig: &Rig, action: VoiceCommand) -> FrontdoorStatus {
    owner(rig, OwnerRequest::Voice { action }).await
}
async fn channel(rig: &Rig, action: ChannelCommand) -> FrontdoorStatus {
    owner(rig, OwnerRequest::Channels { action }).await
}
async fn speech_start(rig: &Rig) -> Client {
    let mut speech = rig.client(ConnectionKind::Speech).await;
    speech
        .send(&SpeechClientFrame::Start {
            session_id: protocol::SpeechSessionId::new(),
            language: Some("en".into()),
            keep_audio: false,
        })
        .await;
    speech
}
#[tokio::test]
async fn voice_install_use_remove_live_over_owner_socket_and_tls() {
    let factory = Arc::new(FixtureSpeech::default());
    let mut rig = Rig::configured(false, Config::default(), factory.clone()).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let initial = voice(&rig, VoiceCommand::Status).await;
    assert_eq!(
        initial.voice.remedy.as_deref(),
        Some("ophio voice install tiny.en")
    );
    assert_eq!(factory.opens.load(Ordering::SeqCst), 0);
    let mut unavailable = speech_start(&rig).await;
    // The transport sends its typed fatal refusal when stream admission fails.
    let frame: Value = unavailable.receive().await;
    assert!(
        frame.to_string().contains("ophio voice install tiny.en"),
        "{frame}"
    );
    let mut client = OwnerClient::connect(&rig.paths.socket()).await.unwrap();
    let mut progress = Vec::new();
    let response: OwnerResponse = client
        .request_with_progress(
            &OwnerRequest::Voice {
                action: VoiceCommand::Install {
                    model: "tiny.en".into(),
                },
            },
            |p| progress.push(p),
        )
        .await
        .unwrap();
    assert!(
        matches!(response, OwnerResponse::Frontdoors(s) if s.voice.availability.is_available())
    );
    assert!(!progress.is_empty());
    assert_eq!(factory.opens.load(Ordering::SeqCst), 1);
    phone.event(|e| matches!(e, Event::CapabilitiesChanged { capabilities } if capabilities.speech.is_available())).await;
    let mut running = speech_start(&rig).await;
    assert!(matches!(
        running.receive::<SpeechServerFrame>().await,
        SpeechServerFrame::Ready { .. }
    ));
    assert!(
        !voice(
            &rig,
            VoiceCommand::Remove {
                model: "tiny.en".into()
            }
        )
        .await
        .voice
        .availability
        .is_available()
    );
    phone.events.clear();
    phone.event(|e| matches!(e, Event::CapabilitiesChanged { capabilities } if matches!(&capabilities.speech, Availability::PermissionMissing { remedy } if remedy == "ophio voice install tiny.en"))).await;
    running.send(&SpeechClientFrame::Finish).await;
    loop {
        if matches!(
            running.receive::<SpeechServerFrame>().await,
            SpeechServerFrame::Finished { .. }
        ) {
            break;
        }
    }
    assert!(!rig.paths.data.join("voice/tiny.en").exists());
    voice(
        &rig,
        VoiceCommand::Install {
            model: "base.en".into(),
        },
    )
    .await;
    assert_eq!(
        Config::load(&rig.paths.config_file()).unwrap().voice_model,
        "base.en"
    );
    voice(
        &rig,
        VoiceCommand::Use {
            model: "tiny.en".into(),
        },
    )
    .await;
    assert!(!rig.runtime.hub.capabilities().await.speech.is_available());
    drop(running);
    drop(client);
    drop(phone);
    drop(unavailable);
    rig.shutdown().await;
}
#[tokio::test]
async fn owner_disconnect_cancels_voice_install_and_removes_staging() {
    let rig = Rig::configured(
        false,
        Config::default(),
        Arc::new(FixtureSpeech {
            stall: true,
            ..Default::default()
        }),
    )
    .await;
    let path = rig.paths.socket();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut started = Some(started);
        let mut client = OwnerClient::connect(&path).await.unwrap();
        client
            .request_with_progress::<_, OwnerResponse>(
                &OwnerRequest::Voice {
                    action: VoiceCommand::Install {
                        model: "tiny.en".into(),
                    },
                },
                |_| {
                    if let Some(started) = started.take() {
                        let _ = started.send(());
                    }
                },
            )
            .await
    });
    waiting.await.unwrap();
    task.abort();
    let _ = task.await;
    // The next owner request serializes behind cancellation cleanup.
    voice(&rig, VoiceCommand::Status).await;
    assert_eq!(
        std::fs::read_dir(rig.paths.data.join("voice"))
            .unwrap()
            .count(),
        0
    );
    rig.shutdown().await;
}

struct Bot {
    updates: StdMutex<Vec<Value>>,
    sent: StdMutex<Vec<Value>>,
    connected: tokio::sync::Semaphore,
    messages: AtomicUsize,
}
impl Default for Bot {
    fn default() -> Self {
        Self {
            updates: StdMutex::new(vec![]),
            sent: StdMutex::new(vec![]),
            connected: tokio::sync::Semaphore::new(0),
            messages: AtomicUsize::new(0),
        }
    }
}
async fn bot_api(
    axum::extract::State(bot): axum::extract::State<Arc<Bot>>,
    axum::extract::Path(path): axum::extract::Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> axum::Json<Value> {
    let method = path.rsplit('/').next().unwrap();
    let result = match method {
        "getMe" => {
            bot.connected.acquire().await.unwrap().forget();
            json!({"id":123, "is_bot":true, "username":"fixture_bot"})
        }
        "getWebhookInfo" => json!({"url":""}),
        "getUpdates" => json!(std::mem::take(&mut *bot.updates.lock().unwrap())),
        "sendMessage" | "editMessageText" => {
            bot.sent.lock().unwrap().push(body);
            json!({"message_id":bot.messages.fetch_add(1, Ordering::SeqCst) + 1})
        }
        _ => panic!("unexpected fake API method"),
    };
    axum::Json(json!({"ok":true,"result":result}))
}
impl Bot {
    /// A fake Telegram API, and companion settings that point at it.
    async fn serve() -> (
        Arc<Self>,
        Config,
        CancellationToken,
        tokio::task::JoinHandle<()>,
    ) {
        let bot = Arc::new(Self::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = Config::default();
        config.channels.telegram.endpoints.telegram_api =
            format!("http://{}", listener.local_addr().unwrap());
        let api_stop = CancellationToken::new();
        let stop = api_stop.clone();
        let router = axum::Router::new()
            .route("/{*path}", axum::routing::post(bot_api))
            .with_state(bot.clone());
        let api = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .unwrap();
        });
        (bot, config, api_stop, api)
    }
    /// The code the bot showed for approving this chat in the app.
    async fn link_code(&self) -> String {
        let notice = self
            .notice(|v| {
                v["text"]
                    .as_str()
                    .is_some_and(|s| s.contains("Approve this code"))
            })
            .await;
        notice["text"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .find(|word| {
                word.len() == 8
                    && word
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            })
            .unwrap()
            .to_owned()
    }
    fn message(&self, id: usize, text: &str) {
        self.updates.lock().unwrap().push(json!({"update_id":id,"message":{"message_id":id,"chat":{"id":42,"type":"private"},"from":{"id":7,"is_bot":false,"first_name":"Fixture"},"text":text}}));
    }
    async fn notice(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(value) = self
                    .sent
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|v| predicate(v))
                    .cloned()
                {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
}
#[tokio::test]
async fn telegram_composition_link_task_minimal_approval_capabilities_and_shutdown() {
    let (bot, config, api_stop, api) = Bot::serve().await;
    let mut rig = Rig::configured(true, config, Arc::new(FixtureSpeech::default())).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let missing = channel(
        &rig,
        ChannelCommand::Enable {
            connector: Connector::Telegram,
        },
    )
    .await;
    assert_eq!(
        missing.channels[0].state,
        channels::ChannelState::TokenMissing
    );
    assert_eq!(
        rig.runtime.hub.capabilities().await.telegram,
        Availability::PermissionMissing {
            remedy: "ophio channels token set telegram".into()
        }
    );
    let project = phone.project(&rig.project_dir("chat-project")).await;
    phone
        .effect(Command::UpdateProjectDefaults {
            project_id: project,
            harness: Some(HarnessKind::ClaudeCode),
            model: None,
            effort: None,
        })
        .await;
    channel(
        &rig,
        ChannelCommand::Map {
            connector: Connector::Telegram,
            chat_id: "42".into(),
            project_id: project,
        },
    )
    .await;
    let connecting = owner(
        &rig,
        OwnerRequest::TokenSet {
            connector: Connector::Telegram,
            token: TokenInput::new("123:fixture-only".into()).unwrap(),
        },
    )
    .await;
    assert_eq!(
        connecting.channels[0].state,
        channels::ChannelState::Connecting
    );
    assert!(matches!(
        rig.runtime.hub.capabilities().await.telegram,
        Availability::Unknown { .. }
    ));
    bot.connected.add_permits(1);
    phone.event(|e| matches!(e, Event::CapabilitiesChanged { capabilities } if capabilities.telegram.is_available())).await;
    bot.message(1, "/status");
    let code = bot.link_code().await;
    // Enter the code displayed by the bot into the paired phone's approval.
    phone
        .effect(Command::ApproveChannelLink { link_code: code })
        .await;
    bot.message(2, "/new bridge-permission");
    let Event::ApprovalUpserted { approval } = phone.event(|e| matches!(e, Event::ApprovalUpserted { approval } if approval.project_id == project && approval.state == ApprovalState::Pending)).await else { panic!() };
    let notice = bot
        .notice(|v| {
            v["text"]
                .as_str()
                .is_some_and(|s| s.contains("approval") && s.contains("app"))
        })
        .await;
    assert!(
        notice["reply_markup"]["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row.as_array().unwrap().is_empty())
    );
    assert!(
        !notice
            .to_string()
            .contains(approval.raw_request.as_deref().unwrap_or("PRIVATE_UNSET"))
    );
    assert_eq!(
        owner(&rig, OwnerRequest::FrontdoorStatus).await.channels[0].state,
        channels::ChannelState::Connected
    );
    channel(
        &rig,
        ChannelCommand::Disable {
            connector: Connector::Telegram,
        },
    )
    .await;
    assert_eq!(
        rig.runtime.hub.capabilities().await.telegram,
        Availability::PermissionMissing {
            remedy: "ophio channels enable telegram".into()
        }
    );
    bot.connected.add_permits(1);
    channel(
        &rig,
        ChannelCommand::Enable {
            connector: Connector::Telegram,
        },
    )
    .await;
    voice(
        &rig,
        VoiceCommand::Install {
            model: "tiny.en".into(),
        },
    )
    .await;
    let mut speech = speech_start(&rig).await;
    assert!(matches!(
        speech.receive::<SpeechServerFrame>().await,
        SpeechServerFrame::Ready { .. }
    ));
    drop(phone);
    let (_temp, paths, _, _) = rig.shutdown().await;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(paths.data.join("channels/channels.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    drop(speech);
    api_stop.cancel();
    api.await.unwrap();
}

#[tokio::test]
async fn telegram_chat_project_is_chosen_changed_and_cleared_from_the_app_live() {
    let (bot, config, api_stop, api) = Bot::serve().await;
    let mut rig = Rig::configured(true, config, Arc::new(FixtureSpeech::default())).await;
    rig.pair().await;
    let mut phone = Phone::new(&rig).await;
    let first = phone.project(&rig.project_dir("first")).await;
    let second = phone.project(&rig.project_dir("second")).await;
    for project_id in [first, second] {
        phone
            .effect(Command::UpdateProjectDefaults {
                project_id,
                harness: Some(HarnessKind::ClaudeCode),
                model: None,
                effort: None,
            })
            .await;
    }
    channel(
        &rig,
        ChannelCommand::Enable {
            connector: Connector::Telegram,
        },
    )
    .await;
    // Each change to the chat settings reconnects the bot.
    bot.connected.add_permits(5);
    owner(
        &rig,
        OwnerRequest::TokenSet {
            connector: Connector::Telegram,
            token: TokenInput::new("123:fixture-only".into()).unwrap(),
        },
    )
    .await;
    phone.event(|e| matches!(e, Event::CapabilitiesChanged { capabilities } if capabilities.telegram.is_available())).await;
    bot.message(1, "/status");
    let link_code = bot.link_code().await;
    phone
        .effect(Command::ApproveChannelLink { link_code })
        .await;
    let Event::ChannelIdentitiesChanged { identities } = phone
        .event(|e| matches!(e, Event::ChannelIdentitiesChanged { identities } if !identities.is_empty()))
        .await
    else {
        panic!()
    };
    let identity_id = identities[0].id;
    assert_eq!(identities[0].project_id, None);
    let unmapped = "Choose a project for this chat in the app: Connections › Chat";
    let saved = |rig: &Rig| {
        Config::load(&rig.paths.config_file())
            .unwrap()
            .channels
            .telegram
            .projects
    };
    bot.message(2, "/new before-a-project");
    bot.notice(|v| v["text"] == unmapped).await;
    for (update, project) in [(3, Some(first)), (4, Some(second)), (5, None)] {
        phone
            .effect(Command::SetChannelProject {
                identity_id,
                project_id: project,
            })
            .await;
        // Saved where the CLI saves it, listed for the app, and live in the chat.
        assert_eq!(saved(&rig).get("42").copied(), project);
        let QueryData::Devices { identities, .. } = phone.query(Query::Devices).await else {
            panic!()
        };
        assert_eq!(identities[0].project_id, project);
        bot.sent.lock().unwrap().clear();
        bot.message(update, "/new after-choosing");
        match project {
            Some(project) => {
                phone
                    .event(
                        |e| matches!(e, Event::TaskUpserted { task } if task.project_id == project),
                    )
                    .await;
            }
            None => {
                bot.notice(|v| v["text"] == unmapped).await;
            }
        }
    }
    // A change made with the CLI reaches the app too.
    phone.events.clear();
    channel(
        &rig,
        ChannelCommand::Map {
            connector: Connector::Telegram,
            chat_id: "42".into(),
            project_id: first,
        },
    )
    .await;
    phone.event(|e| matches!(e, Event::ChannelIdentitiesChanged { identities } if identities[0].project_id == Some(first))).await;
    drop(phone);
    rig.shutdown().await;
    api_stop.cancel();
    api.await.unwrap();
}

#[tokio::test]
async fn token_stdin_permissions_redaction_status_doctor_and_offline_cli() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    let paths = Paths {
        config: temp.path().join("config/ophio"),
        data: temp.path().join("data/ophio"),
        runtime: temp.path().join("runtime/ophio/run"),
    };
    paths.initialize().unwrap();
    std::fs::create_dir(&bin).unwrap();
    for name in ["claude", "codex", "tailscale"] {
        let path = bin.join(name);
        test_support::executable(&path, "#!/bin/sh\nexit 1\n").unwrap();
    }
    Config {
        port: 0,
        ..Default::default()
    }
    .save(&paths.config_file())
    .unwrap();
    let process = || {
        let mut p = tokio::process::Command::new(env!("CARGO_BIN_EXE_ophio"));
        p.env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", temp.path())
            .env("SHELL", "/bin/sh")
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("XDG_DATA_HOME", temp.path().join("data"))
            .env("XDG_RUNTIME_DIR", temp.path().join("runtime"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        p
    };
    let mut running = process().arg("run").spawn().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let output = process().args(["status", "--json"]).output().await.unwrap();
            if output.status.success() {
                let value = serde_json::from_slice::<Value>(&output.stdout).unwrap();
                if value["running"] != false {
                    break value;
                }
            }
            assert!(running.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    println!(
        "Status JSON excerpt: {}",
        json!({"voice":status["voice"],"channels":status["channels"]})
    );
    const TOKEN: &str = "987654:FIXTURE-SYNTHETIC-SECRET";
    let mut setting = process()
        .args(["channels", "token", "set", "telegram"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    setting
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{TOKEN}\n").as_bytes())
        .await
        .unwrap();
    let output = setting.wait_with_output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
    let secret_path = paths.data.join("secrets/telegram.token");
    assert_eq!(std::fs::read_to_string(&secret_path).unwrap(), TOKEN);
    assert_eq!(
        std::fs::metadata(&secret_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(secret_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(
        !std::fs::read_to_string(paths.config_file())
            .unwrap()
            .contains(TOKEN)
    );
    for args in [
        vec!["status"],
        vec!["status", "--json"],
        vec!["doctor"],
        vec!["doctor", "--json"],
        vec!["channels", "status"],
        vec!["channels", "status", "--json"],
        vec!["voice", "status"],
        vec!["voice", "status", "--json"],
    ] {
        let output = process().args(args).output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
    }
    let invalid_map = process()
        .args([
            "channels",
            "map",
            "telegram",
            "42",
            &ProjectId::new().to_string(),
        ])
        .output()
        .await
        .unwrap();
    assert!(!invalid_map.status.success());
    for args in [
        vec!["channels", "token", "clear", "telegram"],
        vec!["channels", "enable", "telegram"],
        vec!["channels", "exposure", "telegram", "titles"],
        vec!["channels", "disable", "telegram"],
    ] {
        let output = process().args(args).output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(!secret_path.exists());
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(running.id().unwrap() as i32).unwrap(),
        rustix::process::Signal::TERM,
    )
    .unwrap();
    let stopped = running.wait_with_output().await.unwrap();
    assert!(stopped.status.success());
    assert!(!String::from_utf8_lossy(&stopped.stderr).contains(TOKEN));
    // Capture the companion's tracing output from this throwaway data directory.
    let logs = std::fs::read_dir(paths.data.join("logs"))
        .unwrap()
        .map(|p| std::fs::read_to_string(p.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(logs.contains("Øphio listening"));
    assert!(!logs.contains(TOKEN));
    for args in [
        vec!["voice", "use", "base.en"],
        vec!["voice", "remove", "tiny.en"],
        vec!["voice", "status"],
        vec!["channels", "enable", "discord"],
        vec!["channels", "unmap", "discord", "42"],
    ] {
        let output = process().args(args).output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("the next time it starts"));
    }
    let config = Config::load(&paths.config_file()).unwrap();
    assert_eq!(config.voice_model, "base.en");
    assert!(config.channels.discord.enabled);
    assert!(matches!(
        config.channels.telegram.exposure,
        channels::Exposure::Titles
    ));
}
