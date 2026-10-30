// SPDX-License-Identifier: Apache-2.0
//! Reproducible hub workloads with synthetic ports; never starts a native tool.
//!
//! `cargo run --release -p services --features testing --example hub_bench -- [history] [events] [phones]`
//!
//! Builds `history` finished tasks, then drives one long run with `phones`
//! subscribed devices and reports wall time per workload. The synthetic agent
//! tool reports a catalog the size of a real one.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ports::{HarnessAdapter, HarnessError, HarnessEvent, RunSpec, RunStarted};
use protocol::{auth::*, capability::*, command::*, model::*, *};
use services::{
    CompanionHub, Hub,
    api::{Backlog, Caller, Outbound},
    testing::FakeHarness,
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// The synthetic agent tool, with eight models of four effort levels each,
/// three modes and a catalog observation, as a signed-in agent tool reports.
#[derive(Debug)]
struct Catalogued(Arc<FakeHarness>);
#[async_trait::async_trait]
impl HarnessAdapter for Catalogued {
    fn kind(&self) -> HarnessKind {
        self.0.kind()
    }
    async fn probe(&self) -> HarnessInfo {
        let mut info = self.0.probe().await;
        let efforts = ["low", "medium", "high", "max"];
        info.models = (0..8)
            .map(|i| ModelOption {
                id: format!("synthetic-model-{i}-2026"),
                label: format!("Synthetic model {i}"),
                suggested_for_computer_use: i == 0,
                is_default: i == 1,
                details: Some(ModelDetails {
                    alias: false,
                    efforts: efforts
                        .iter()
                        .map(|e| EffortOption {
                            id: e.to_string(),
                            label: e.to_string(),
                        })
                        .collect(),
                    default_effort: Some("medium".into()),
                }),
            })
            .collect();
        info.efforts = efforts.iter().map(|e| e.to_string()).collect();
        info.modes = [TaskMode::Ask, TaskMode::Plan, TaskMode::Work]
            .into_iter()
            .map(|mode| ModeSupport {
                mode,
                availability: Availability::Available,
                enforcement: "The agent tool enforces this mode with its own permission settings on every tool call.".into(),
            })
            .collect();
        info.catalog = Some(CatalogObservation {
            checked_at: time::Timestamp(0),
            source: "synthetic --help".into(),
            availability: Availability::Available,
            default_model: Some("synthetic-model-1-2026".into()),
            default_effort: Some("medium".into()),
        });
        info
    }
    async fn start_run(&self, spec: RunSpec) -> Result<RunStarted, HarnessError> {
        self.0.start_run(spec).await
    }
}

async fn build(path: &Path, harness: &Arc<FakeHarness>) -> CompanionHub {
    CompanionHub::builder()
        .data_dir(path)
        .harness(Arc::new(Catalogued(harness.clone())))
        .bridge(services::bridge::BridgeConfig::new(
            "synthetic-bridge",
            "synthetic-socket",
        ))
        .build()
        .await
        .unwrap()
}
async fn pair(hub: &CompanionHub, key: u8) -> Caller {
    let ticket = hub.open_pairing(&Caller::local_owner()).await.unwrap();
    let response = services::testing::pair_confirmed(
        hub,
        PairRequest {
            pairing_secret: ticket.secret,
            device_public_key: URL_SAFE_NO_PAD.encode([key; 32]),
            device_name: format!("Phone {key}"),
            device_platform: "android".into(),
            client_version: "bench".into(),
            protocol: ProtocolVersion::CURRENT,
            shows_code: true,
            replaces: None,
        },
    )
    .await
    .unwrap();
    Caller {
        principal: Principal::Device {
            device_id: response.device_id,
        },
        grants: response.grants,
    }
}
async fn until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(600);
    while !done() {
        assert!(Instant::now() < deadline, "workload did not settle");
        tokio::time::sleep(Duration::from_micros(200)).await;
    }
}
async fn start_task(
    hub: &CompanionHub,
    harness: &FakeHarness,
    project: ProjectId,
) -> (TaskId, RunId) {
    let known = harness.runs();
    let receipt = hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CreateTask {
                github: None,
                agent_chosen: None,
                mode: None,
                project_id: project,
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
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("task not created: {receipt:?}");
    };
    until(|| harness.runs().len() > known.len()).await;
    let run = harness
        .runs()
        .into_iter()
        .find(|r| !known.contains(r))
        .unwrap();
    harness
        .emit(
            run,
            HarnessEvent::SessionStarted {
                native_session: format!("session-{run}"),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    (task_id, run)
}
/// The latest journaled state of every run, as a phone would see it.
#[derive(Clone, Default)]
struct RunStates(Arc<std::sync::Mutex<std::collections::HashMap<RunId, RunState>>>);
impl RunStates {
    async fn watch(hub: &CompanionHub) -> Self {
        let states = Self::default();
        let mut subscription = hub.subscribe(&Caller::local_owner(), None).await.unwrap();
        let writer = states.clone();
        tokio::spawn(async move {
            while let Ok(out) = subscription.live.recv().await {
                if let Outbound::Event(event) = out
                    && let Event::RunUpserted { run } = &event.event
                {
                    writer.0.lock().unwrap().insert(run.run_id, run.state);
                }
            }
        });
        states
    }
    fn state(&self, run: RunId) -> Option<RunState> {
        self.0.lock().unwrap().get(&run).copied()
    }
}
fn activity(i: usize) -> HarnessEvent {
    HarnessEvent::Activity {
        key: format!("tool-{i}"),
        tool: "Bash".into(),
        summary: format!("Ran step {i} of the build"),
        status: protocol::model::ActivityStatus::Succeeded,
        detail: Some("cargo test -p services --lib -- --nocapture".into()),
    }
}
fn report(name: &str, count: usize, elapsed: Duration) {
    println!(
        "{name},{count},{:.1} ms,{:.1} us/op",
        elapsed.as_secs_f64() * 1e3,
        elapsed.as_secs_f64() * 1e6 / count.max(1) as f64
    );
}

#[tokio::main]
async fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("numeric argument"))
        .collect();
    let history = args.first().copied().unwrap_or(300);
    let events = args.get(1).copied().unwrap_or(2000);
    let phones = args.get(2).copied().unwrap_or(3);
    let path = std::env::temp_dir().join(format!("hub-bench-{}", uuid::Uuid::now_v7()));
    let harness = Arc::new(FakeHarness::default());
    let hub = build(&path, &harness).await;
    let owner = Caller::local_owner();
    // A project may not contain the companion's own data folder.
    let project_dir =
        std::env::temp_dir().join(format!("hub-bench-project-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&project_dir).unwrap();
    let project = match hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::AddProject {
                host_path: project_dir.to_string_lossy().into_owned(),
                name: Some("Bench".into()),
            }),
        )
        .await
        .result
    {
        Some(CommandResult::ProjectAdded { project_id }) => project_id,
        other => panic!("project: {other:?}"),
    };
    let runs = RunStates::watch(&hub).await;
    println!("workload,count,total,per op");

    // Finished work accumulates: every task leaves a result and a timeline.
    let started = Instant::now();
    for _ in 0..history {
        let (_, run) = start_task(&hub, &harness, project).await;
        harness.emit(run, activity(0)).await;
        harness
            .emit(
                run,
                HarnessEvent::Exited {
                    exit: ports::RunExit::Completed,
                    outcome: None,
                },
            )
            .await;
        until(|| runs.state(run).is_some_and(|s| s.is_terminal())).await;
    }
    report("history_tasks", history, started.elapsed());

    let callers: Vec<Caller> = {
        let mut callers = Vec::new();
        for key in 0..phones {
            callers.push(pair(&hub, 10 + key as u8).await);
        }
        callers
    };
    let received = Arc::new(AtomicU64::new(0));
    let live = Arc::new(AtomicU64::new(0));
    for caller in &callers {
        let mut subscription = hub.subscribe(caller, None).await.unwrap();
        let received = received.clone();
        let live = live.clone();
        tokio::spawn(async move {
            loop {
                match subscription.live.recv().await {
                    Ok(Outbound::Event(_)) => {
                        received.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(Outbound::Live(_)) => {
                        live.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        panic!("subscriber lagged by {n}");
                    }
                    Err(_) => break,
                }
            }
        });
    }
    let (task, run) = start_task(&hub, &harness, project).await;
    until(|| runs.state(run) == Some(RunState::Working)).await;

    // A long agent run: each native activity is one durable timeline row.
    let head = hub.journal_head();
    let before = received.load(Ordering::Relaxed);
    let started = Instant::now();
    for i in 0..events {
        harness.emit(run, activity(i)).await;
    }
    let journaled = events as u64 * 3;
    until(|| hub.journal_head() >= head + journaled).await;
    let expected = before + journaled * phones as u64;
    until(|| received.load(Ordering::Relaxed) >= expected).await;
    report("activity_events", events, started.elapsed());

    // Streaming text is live-only and fans out to every subscribed phone.
    let started = Instant::now();
    for i in 0..events {
        harness
            .emit(
                run,
                HarnessEvent::AgentTextDelta {
                    message_key: "m1".into(),
                    text: format!("token {i} "),
                },
            )
            .await;
    }
    until(|| live.load(Ordering::Relaxed) >= (events * phones) as u64).await;
    report("text_deltas", events, started.elapsed());

    // Receipts: admission, dedupe record and progress events per command.
    let commands = events / 4;
    let QueryData::Snapshot(snapshot) = hub
        .query(
            &owner,
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("snapshot");
    };
    let first = snapshot
        .tasks
        .iter()
        .find(|t| t.task_id == task)
        .unwrap()
        .revision;
    let started = Instant::now();
    for (i, revision) in (first..).take(commands).enumerate() {
        let mut command = CommandEnvelope::new(Command::RenameTask {
            task_id: task,
            title: format!("Bench task {i}"),
            expected_title: None,
        });
        command.expected_revision = Some(revision);
        let receipt = hub.submit(&callers[0], command).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    }
    report("receipts", commands, started.elapsed());

    // A phone scrolls back through the whole conversation.
    let started = Instant::now();
    let mut before_seq = None;
    let mut pages = 0;
    let mut first_page = Duration::ZERO;
    loop {
        let page_started = Instant::now();
        let QueryData::TaskTimeline { items, has_more } = hub
            .query(
                &callers[0],
                Query::TaskTimeline {
                    task_id: task,
                    before_seq,
                    limit: 80,
                },
            )
            .await
            .unwrap()
        else {
            panic!("timeline");
        };
        if pages == 0 {
            first_page = page_started.elapsed();
        }
        pages += 1;
        if !has_more {
            break;
        }
        before_seq = items.first().map(|i| i.seq);
    }
    report("timeline_first_page", 1, first_page);
    report("timeline_all_pages", pages, started.elapsed());

    // A phone reconnects after missing the whole run.
    let started = Instant::now();
    let subscription = hub.subscribe(&callers[0], Some(head)).await.unwrap();
    let Backlog::Events(backlog) = subscription.backlog else {
        panic!("gap");
    };
    let count = backlog.len();
    drop(backlog);
    report("catch_up_subscribe", count, started.elapsed());

    let started = Instant::now();
    let _: QueryData = hub
        .query(
            &callers[0],
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap();
    report("snapshot", 1, started.elapsed());

    hub.shutdown().await.unwrap();
    drop(hub);
    let started = Instant::now();
    let hub = build(&path, &harness).await;
    report("restart", 1, started.elapsed());
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(&path).unwrap();
    std::fs::remove_dir_all(&project_dir).unwrap();
}
