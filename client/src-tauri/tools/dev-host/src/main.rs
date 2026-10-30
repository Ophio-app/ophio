// SPDX-License-Identifier: Apache-2.0
//! Loopback-only development companion with synthetic ports and real services.
//! The printed one-use link is sensitive; redirect it to a private temporary file.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use companion::{
    identity::{FileSecretStore, Identity},
    server::{Server, ServerDeps},
};
use ports::{SpeechEngine, SpeechSession, TerminalHost};
use protocol::{Command, CommandEnvelope, Query, QueryData, capability::Availability};
use services::{
    Caller, CompanionHub, Hub,
    testing::{FakeDesktop, FakeHarness, FakeLibrary, FakeSpeech, FakeTerminal},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct CountedSpeech(Arc<AtomicU64>);
struct CountedSession {
    inner: Box<dyn SpeechSession>,
    samples: Arc<AtomicU64>,
}
#[async_trait::async_trait]
impl SpeechEngine for CountedSpeech {
    fn name(&self) -> String {
        "Synthetic speech (sample counter)".into()
    }
    async fn availability(&self) -> Availability {
        Availability::Available
    }
    async fn start(
        &self,
        language: Option<String>,
    ) -> Result<Box<dyn SpeechSession>, ports::SpeechError> {
        Ok(Box::new(CountedSession {
            inner: FakeSpeech.start(language).await?,
            samples: self.0.clone(),
        }))
    }
}
#[async_trait::async_trait]
impl SpeechSession for CountedSession {
    async fn push_pcm(
        &mut self,
        samples: &[i16],
    ) -> Result<Vec<protocol::annotation::SpeechSegment>, ports::SpeechError> {
        self.samples
            .fetch_add(samples.len() as u64, Ordering::Relaxed);
        self.inner.push_pcm(samples).await
    }
    async fn finish(
        &mut self,
    ) -> Result<Vec<protocol::annotation::SpeechSegment>, ports::SpeechError> {
        self.inner.finish().await
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let data = PathBuf::from(args.get(1).map(String::as_str).unwrap_or(".dev-host"));
    let port: u16 = args.get(2).map(String::as_str).unwrap_or("47101").parse()?;
    companion::identity::private_dir(&data)?;
    let identity = Arc::new(Identity::load_or_create(
        &FileSecretStore::new(data.join("test-identity.json")),
        vec!["10.0.2.2".into()],
    )?);
    {
        let mut db =
            store::Store::open(data.join("companion.sqlite"), store::Retention::default())?;
        let previous: Option<protocol::HostId> = db.read(|r| r.get("meta", "host_id"))?;
        if previous.is_some_and(|id| id != identity.host_id) {
            return Err("test database and identity do not match".into());
        }
        db.write(protocol::time::Timestamp::now(), |tx| {
            tx.put("meta", "host_id", &identity.host_id)
        })?;
    }
    let terminal = Arc::new(FakeTerminal::default());
    let samples = Arc::new(AtomicU64::new(0));
    let desktop = Arc::new(FakeDesktop::default());
    let harness = Arc::new(FakeHarness::default());
    let hub = CompanionHub::builder()
        .data_dir(&data)
        .host_name("Development computer")
        .desktop(desktop.clone())
        .media(Arc::new(media::TileEncoder))
        .terminal(terminal.clone())
        .speech(Arc::new(CountedSpeech(samples.clone())))
        .harness(harness.clone())
        .library(Arc::new(FakeLibrary::default()))
        .build()
        .await?;
    let owner = Caller::local_owner();
    let QueryData::Snapshot(snapshot) = hub
        .query(
            &owner,
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await?
    else {
        return Err("snapshot missing".into());
    };
    if snapshot.projects.is_empty() {
        let project = data.join("project");
        std::fs::create_dir_all(&project)?;
        let receipt = hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::AddProject {
                    host_path: std::fs::canonicalize(project)?
                        .to_string_lossy()
                        .into_owned(),
                    name: Some("Native connection checks".into()),
                }),
            )
            .await;
        if receipt.error.is_some() {
            return Err("could not create development project".into());
        }
    }
    let term = terminal
        .open(ports::TerminalSpec {
            title: "Synthetic terminal".into(),
            cwd: data.clone(),
            program: None,
            env: vec![],
            project_id: None,
            harness: None,
            cols: 80,
            rows: 24,
        })
        .await?;
    terminal.output(
        term.terminal_id,
        b"Synthetic terminal: no commands are executed.\r\n",
    );
    let mut deps = ServerDeps::new(Arc::new(hub.clone()), identity)?;
    deps.addresses = vec![format!("10.0.2.2:{port}"), format!("127.0.0.1:{port}")].into();
    let server = Server::bind(deps.clone(), &[([127, 0, 0, 1], port).into()]).await?;
    let offer = deps.pairing_offer().await?;
    println!(
        "{}://pair?d={}",
        companion::config::PAIR_SCHEME,
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&offer)?)
    );
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(server.run(shutdown.clone()));
    let metrics = data.join("metrics.json");
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut seen = std::collections::HashSet::new();
    // A phone pairs once the code it shows is typed here, as the owner
    // confirms at a real computer.
    let (typed, mut codes) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(std::result::Result::ok) {
            if typed.send(line).is_err() {
                break;
            }
        }
    });
    let mut asking = None;
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => { result?; break; }
            Some(code) = codes.recv() => match hub.answer_pairing(&owner, &code, true).await {
                Ok(_) => eprintln!("Paired."),
                Err(error) => eprintln!("{}", error.message),
            },
            _ = tick.tick() => {
                let data = serde_json::json!({"speechSamples":samples.load(Ordering::Relaxed),"journalHead":hub.journal_head(),"terminalId":term.terminal_id,"targetId":desktop.target.lock().map_err(|_| "target lock")?.target_id});
                std::fs::write(&metrics, serde_json::to_vec(&data)?)?;
                let waiting = hub.waiting_pairing(&owner).await?.map(|p| p.device_name);
                if let Some(name) = waiting.as_ref().filter(|name| asking.as_ref() != Some(*name)) {
                    eprintln!("{name} is asking to pair. Type the code it shows and press Enter.");
                }
                asking = waiting;
                terminal.output(term.terminal_id, b"Connection is alive.\r\n");
                for run in harness.runs() {
                    if seen.insert(run) {
                        harness.emit(run, ports::HarnessEvent::SessionStarted { native_session: format!("synthetic-{run}"), actual_model: Some("synthetic".into()), actual_effort: None, account_label: None }).await;
                        harness.emit(run, ports::HarnessEvent::ApprovalRequested(ports::NativeApproval { native_request_id: "test-approval".into(), kind: protocol::model::ApprovalKind::Question, title: "Review the connection check".into(), detail: Some("Synthetic request; no native agent was launched.".into()), detail_lines: Vec::new(), questions: Vec::new(), raw_request: None, scope: "Development fixture only".into(), shortened: false, tool_call: None })).await;
                    }
                }
            }
        }
    }
    shutdown.cancel();
    task.await??;
    hub.shutdown().await?;
    Ok(())
}
