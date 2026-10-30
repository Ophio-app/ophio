// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::{
    Clock,
    bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse},
};

#[tokio::test]
async fn in_run_subscription_hides_inbox_attention_in_live_and_paged_history() {
    let f = Fixture::new().await;
    let owner = Caller::local_owner();
    let mut agent = f.hub.agent_subscribe(&owner, None).await.unwrap().live;
    let mut person = f.hub.subscribe(&owner, None).await.unwrap().live;
    let item = AttentionItem {
        child_id: None,
        attention_id: AttentionId::new(),
        kind: AttentionKind::WaitingForYou,
        title: "Private report check".into(),
        body: Some("Only for the person".into()),
        task_id: None,
        run_id: None,
        approval_id: None,
        created_at: f.clock.now(),
        resolved_at: None,
        priority: 50,
        topic: None,
        quiet: false,
        report_id: Some(ReportId::new()),
    };
    let ordinary = AttentionItem {
        attention_id: AttentionId::new(),
        kind: AttentionKind::Failure,
        task_id: Some(TaskId::new()),
        report_id: None,
        ..item.clone()
    };
    let start = f.hub.journal_head();
    f.hub
        .write(|tx| {
            tx.emit(Event::AttentionUpserted { item: item.clone() })?;
            tx.emit(Event::AttentionUpserted {
                item: ordinary.clone(),
            })?;
            Ok(())
        })
        .unwrap();
    assert!(
        matches!(person.try_recv().unwrap(), Outbound::Event(event) if matches!(&event.event, Event::AttentionUpserted { item: seen } if seen == &item))
    );
    assert!(
        matches!(agent.try_recv().unwrap(), Outbound::Event(event) if matches!(&event.event, Event::AttentionUpserted { item: seen } if seen == &ordinary))
    );
    assert!(matches!(
        agent.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let agent_backlog = f
        .hub
        .agent_subscribe(&owner, Some(start))
        .await
        .unwrap()
        .backlog;
    let person_backlog = f.hub.subscribe(&owner, Some(start)).await.unwrap().backlog;
    assert!(
        matches!(agent_backlog, Backlog::Events(events) if events.len() == 1 && matches!(&events[0].event, Event::AttentionUpserted { item: seen } if seen == &ordinary))
    );
    assert!(matches!(person_backlog, Backlog::Events(events) if events.len() == 2));
    let head = f.hub.journal_head();
    let (agent_page, through) = f.hub.agent_backlog(&owner, start, head, 10).await.unwrap();
    assert_eq!(through, head);
    assert!(
        matches!(agent_page, Backlog::Events(events) if events.len() == 1 && matches!(&events[0].event, Event::AttentionUpserted { item: seen } if seen == &ordinary))
    );
    assert!(
        matches!(f.hub.backlog(&owner, start, head, 10).await.unwrap().0, Backlog::Events(events) if events.len() == 2)
    );
    f.close().await;
}

#[tokio::test]
async fn discovery_publishes_control_before_the_first_command() {
    let f = Fixture::new().await;
    let caller = f.device().await;
    let QueryData::Snapshot(snapshot) = f
        .hub
        .query(
            &caller,
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("snapshot expected");
    };
    let target = &snapshot.targets[0];
    assert_eq!(snapshot.control.len(), snapshot.targets.len());
    assert_eq!(snapshot.control[0].input_state, InputState::Ready);
    assert!(snapshot.control[0].lease.is_none());
    let subscription = f.hub.subscribe(&caller, Some(0)).await.unwrap();
    let Backlog::Events(events) = subscription.backlog else {
        panic!("events expected")
    };
    assert!(events.iter().any(|e| matches!(&e.event, Event::ControlChanged { view } if view.target_id == target.target_id && view.input_state == InputState::Ready)));
    let command = CommandEnvelope::new(Command::RequestHumanLease {
        target_id: target.target_id,
        geometry_revision: target.geometry.revision,
    });
    let id = command.command_id;
    f.hub.submit(&caller, command).await;
    settle(&f.hub, id).await;
    let view: TargetControlView = f
        .hub
        .read(|r| r.require("control", target.target_id))
        .unwrap();
    assert_eq!(view.input_state, InputState::Human);
    f.close().await;
}

#[tokio::test]
async fn geometry_handover_publishes_revision_and_refuses_only_old_coordinates() {
    let f = Fixture::new().await;
    let caller = f.device().await;
    let target_id = f.desktop.target.lock().unwrap().target_id;
    let command = CommandEnvelope::new(Command::RequestHumanLease {
        target_id,
        geometry_revision: 1,
    });
    let id = command.command_id;
    f.hub.submit(&caller, command).await;
    settle(&f.hub, id).await;
    let view: TargetControlView = f.hub.read(|r| r.require("control", target_id)).unwrap();
    let lease = view.lease.unwrap();
    {
        let mut target = f.desktop.target.lock().unwrap();
        target.geometry.width = 1920;
        target.geometry.height = 1080;
        target.geometry.scale = 1.25;
        target.geometry.revision = 2;
    }
    let input = InputEnvelope {
        lease_id: lease.lease_id,
        generation: lease.generation,
        geometry_revision: 1,
        seq: 1,
        action: InputAction::Click {
            button: PointerButton::Left,
            at: TargetPoint { x: 0.5, y: 0.5 },
            count: 1,
        },
    };
    assert_eq!(
        f.hub.input(&caller, input.clone()).await.outcome,
        InputOutcome::StaleGeometry
    );
    assert_eq!(f.desktop.input.injected.load(Ordering::SeqCst), 0);
    let QueryData::Snapshot(snapshot) = f
        .hub
        .query(
            &caller,
            Query::Snapshot {
                reads_withheld: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("snapshot expected")
    };
    assert_eq!(snapshot.targets[0].geometry.revision, 2);
    assert_eq!(snapshot.targets[0].geometry.scale, 1.25);
    assert_eq!(
        f.hub
            .input(
                &caller,
                InputEnvelope {
                    geometry_revision: 2,
                    seq: 2,
                    ..input
                }
            )
            .await
            .outcome,
        InputOutcome::Injected
    );
    assert_eq!(f.desktop.input.injected.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn private_ready_requires_native_confirmation_and_fences_old_binding() {
    for confirmed in [false, true] {
        let f = Fixture::new().await;
        f.harness
            .controls
            .private_confirmed
            .store(confirmed, Ordering::SeqCst);
        let project = f.project().await;
        let (task, _) = f
            .task(
                project,
                TaskPlacement::StartNow,
                &Caller::local_owner(),
                true,
            )
            .await;
        let run_id = run_started(&f, task).await;
        let target_id = f.desktop.target.lock().unwrap().target_id;
        let binding = BridgeBinding {
            run_id,
            target_id: Some(target_id),
            computer_use: true,
            browser: false,
            github: false,
            checklist_answers: false,
        };
        let before = f
            .hub
            .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
            .await;
        assert!(matches!(before, BridgeResponse::Observation { .. }));
        let command = CommandEnvelope::new(Command::StartPrivateStep { target_id });
        let id = command.command_id;
        f.hub.submit(&Caller::local_owner(), command).await;
        settle(&f.hub, id).await;
        let view: TargetControlView = f.hub.read(|r| r.require("control", target_id)).unwrap();
        let private = view.private_step.unwrap();
        assert_eq!(private.ready, confirmed);
        assert_eq!(private.uncovered_routes.is_empty(), confirmed);
        assert_ne!(view.input_state, InputState::Agent);
        assert_eq!(
            view.model_observation.unwrap().state,
            ObservationState::Withdrawn
        );
        assert!(!matches!(
            f.hub
                .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
                .await,
            BridgeResponse::Observation { .. }
        ));
        assert!(!matches!(
            f.hub
                .handle(
                    &binding,
                    BridgeRequest::TypeText {
                        text: "synthetic late input".into()
                    }
                )
                .await,
            BridgeResponse::Done
        ));
        assert_eq!(f.desktop.input.injected.load(Ordering::SeqCst), 0);
        assert!(f.harness.controls.interrupts.load(Ordering::SeqCst) > 0);
        // Ending the private UI step cannot resurrect the old native binding,
        // even when native pause confirmation was uncertain.
        let command = CommandEnvelope::new(Command::EndPrivateStep { target_id });
        let id = command.command_id;
        f.hub.submit(&Caller::local_owner(), command).await;
        settle(&f.hub, id).await;
        assert!(f.hub.run_stopped(run_id).unwrap());
        for request in [
            BridgeRequest::ObserveTarget { max_dim: None },
            BridgeRequest::TypeText {
                text: "synthetic input after private step".into(),
            },
        ] {
            assert!(!matches!(
                f.hub.handle(&binding, request).await,
                BridgeResponse::Observation { .. } | BridgeResponse::Done
            ));
        }
        assert_eq!(f.desktop.input.injected.load(Ordering::SeqCst), 0);
        f.close().await;
    }
}

#[derive(Default)]
struct Indicator(std::sync::Mutex<Vec<String>>);
#[async_trait::async_trait]
impl ports::desktop::LocalIndicator for Indicator {
    async fn show(&self, holders: &str) -> Result<(), ports::PlatformError> {
        self.0.lock().unwrap().push(holders.into());
        Ok(())
    }
    async fn hide(&self) -> Result<(), ports::PlatformError> {
        self.0.lock().unwrap().push(String::new());
        Ok(())
    }
}
#[tokio::test]
async fn local_stop_fences_computer_run_and_indicator_is_required_before_input() {
    let f = Fixture::new().await;
    f.hub.set_local_indicator(None).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let caller = f.device().await;
    let request = CommandEnvelope::new(Command::RequestHumanLease {
        target_id: target,
        geometry_revision: 1,
    });
    let id = request.command_id;
    f.hub.submit(&caller, request).await;
    settle(&f.hub, id).await;
    let view: TargetControlView = f.hub.read(|r| r.require("control", target)).unwrap();
    assert_ne!(view.input_state, InputState::Human);
    let indicator = Arc::new(Indicator::default());
    f.hub.set_local_indicator(Some(indicator.clone())).await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            true,
        )
        .await;
    let run_id = run_started(&f, task).await;
    assert!(
        indicator
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|name| name.contains("Test task"))
    );
    assert_eq!(f.hub.stop_all_control().await.unwrap(), 1);
    assert!(f.hub.run_stopped(run_id).unwrap());
    let view: TargetControlView = f.hub.read(|r| r.require("control", target)).unwrap();
    assert_ne!(view.input_state, InputState::Agent);
    assert_eq!(indicator.0.lock().unwrap().last().unwrap(), "");
    f.close().await;
}

#[derive(Debug)]
struct LockingDesktop {
    inner: Arc<FakeDesktop>,
    lock: tokio::sync::watch::Sender<DesktopLockState>,
    lock_at_injection: Arc<std::sync::atomic::AtomicBool>,
}
struct LockingSink {
    inner: Box<dyn ports::InputSink>,
    lock: tokio::sync::watch::Sender<DesktopLockState>,
    lock_at_injection: Arc<std::sync::atomic::AtomicBool>,
}
#[async_trait::async_trait]
impl ports::InputSink for LockingSink {
    async fn inject(
        &mut self,
        action: &InputAction,
        geometry: &TargetGeometry,
    ) -> Result<(), ports::PlatformError> {
        if self.lock_at_injection.load(Ordering::SeqCst) {
            self.lock.send_replace(DesktopLockState::Locked);
        }
        if *self.lock.borrow() == DesktopLockState::Locked {
            return Err(ports::PlatformError::Locked);
        }
        self.inner.inject(action, geometry).await
    }
    async fn release_all(&mut self) -> Result<(), ports::PlatformError> {
        self.inner.release_all().await
    }
}
#[async_trait::async_trait]
impl ports::DesktopBackend for LockingDesktop {
    async fn capabilities(&self) -> ports::DesktopCapabilities {
        self.inner.capabilities().await
    }
    async fn targets(&self) -> Result<Vec<ControlTarget>, ports::PlatformError> {
        self.inner.targets().await
    }
    async fn capture_still(
        &self,
        id: TargetId,
    ) -> Result<ports::CapturedFrame, ports::PlatformError> {
        self.inner.capture_still(id).await
    }
    async fn open_stream(
        &self,
        id: TargetId,
        fps: u8,
    ) -> Result<Box<dyn ports::FrameStream>, ports::PlatformError> {
        self.inner.open_stream(id, fps).await
    }
    async fn input_sink(
        &self,
        id: TargetId,
    ) -> Result<Box<dyn ports::InputSink>, ports::PlatformError> {
        Ok(Box::new(LockingSink {
            inner: self.inner.input_sink(id).await?,
            lock: self.lock.clone(),
            lock_at_injection: self.lock_at_injection.clone(),
        }))
    }
    async fn lock_session(&self) -> Result<(), ports::PlatformError> {
        self.lock.send_replace(DesktopLockState::Locked);
        Ok(())
    }
    async fn desktop_lock(&self) -> DesktopLockState {
        *self.lock.borrow()
    }
    fn lock_changes(&self) -> Option<tokio::sync::watch::Receiver<DesktopLockState>> {
        Some(self.lock.subscribe())
    }
}
#[tokio::test]
async fn lock_fences_admitted_and_late_input_and_publishes_why() {
    for at_injection in [false, true] {
        let f = Fixture::new().await;
        let backend = Arc::new(LockingDesktop {
            inner: f.desktop.clone(),
            lock: tokio::sync::watch::channel(DesktopLockState::Unlocked).0,
            lock_at_injection: Default::default(),
        });
        f.hub
            .set_desktop(
                Some(backend.clone()),
                protocol::capability::Availability::Available,
            )
            .await
            .unwrap();
        let caller = f.device().await;
        let target_id = f.desktop.target.lock().unwrap().target_id;
        let command = CommandEnvelope::new(Command::RequestHumanLease {
            target_id,
            geometry_revision: 1,
        });
        let id = command.command_id;
        f.hub.submit(&caller, command).await;
        settle(&f.hub, id).await;
        let view: TargetControlView = f.hub.read(|r| r.require("control", target_id)).unwrap();
        let lease = view.lease.unwrap();
        if at_injection {
            backend.lock_at_injection.store(true, Ordering::SeqCst);
        } else {
            backend.lock.send_replace(DesktopLockState::Locked);
        }
        for seq in 1..=3 {
            let ack = f
                .hub
                .input(
                    &caller,
                    InputEnvelope {
                        lease_id: lease.lease_id,
                        generation: lease.generation,
                        geometry_revision: 1,
                        seq,
                        action: InputAction::Text {
                            text: "LOCK_TEST".into(),
                        },
                    },
                )
                .await;
            assert_eq!(ack.outcome, InputOutcome::Failed);
            assert_eq!(ack.refusal, Some(InputRefusal::Locked));
        }
        let QueryData::Snapshot(snapshot) = f
            .hub
            .query(
                &caller,
                Query::Snapshot {
                    reads_withheld: None,
                },
            )
            .await
            .unwrap()
        else {
            panic!("snapshot expected")
        };
        assert_eq!(
            snapshot.control[0].desktop_lock,
            Some(DesktopLockState::Locked)
        );
        assert_eq!(
            snapshot.control[0].lease.as_ref().unwrap().state,
            LeaseState::Revoked
        );
        assert_eq!(f.desktop.input.injected.load(Ordering::SeqCst), 0);
        f.close().await;
    }
}

/// Times this process's threads named `name` have blocked and woken again so far.
/// Preemption by other load is not counted: it is not the thread waking up.
#[cfg(target_os = "linux")]
fn switches(name: &str) -> u64 {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .flatten()
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm")).is_ok_and(|c| c.trim_end() == name)
        })
        .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
        .flat_map(|status| {
            status
                .lines()
                .filter(|l| l.starts_with("voluntary_ctxt_switches:"))
                .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
                .collect::<Vec<_>>()
        })
        .sum()
}

#[cfg(target_os = "linux")]
#[test]
fn an_idle_hub_does_not_tick() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("idle-hub-worker")
        // I/O-enabled runtimes share Tokio's process-wide signal pipe. Other
        // tests' child exits wake that reactor even when no hub task is ready.
        // This hub has no I/O adapters; keep its timers enabled to catch polling.
        .enable_time()
        .build()
        .unwrap();
    let processes = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    let _signals = processes.block_on(async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child()).unwrap()
    });
    runtime.block_on(async {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/service-tests")
            .join(uuid::Uuid::now_v7().to_string());
        let hub = CompanionHub::builder()
            .data_dir(&path)
            .build()
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let before = switches("idle-hub-worker");
        // Exercise the interference even when this test runs by itself. Keep
        // the window open for at least 500 ms and until every child has exited.
        // The signal runtime is deliberately not driven here: another test
        // need not be polling its reactor when a child exits.
        let exits = std::thread::spawn(|| {
            for _ in 0..20 {
                assert!(
                    std::process::Command::new("true")
                        .status()
                        .unwrap()
                        .success()
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        exits.join().unwrap();
        let woken = switches("idle-hub-worker") - before;
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(path).unwrap();
        assert!(woken <= 3, "idle hub worker woke {woken} times while idle");
    });
}

#[tokio::test]
async fn a_lease_granted_to_an_idle_hub_still_expires() {
    let f = Fixture::new().await;
    let caller = f.device().await;
    let target_id = f.desktop.target.lock().unwrap().target_id;
    // Let the expiry tick find nothing to expire and go to sleep first.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let command = CommandEnvelope::new(Command::RequestHumanLease {
        target_id,
        geometry_revision: 1,
    });
    let id = command.command_id;
    f.hub.submit(&caller, command).await;
    settle(&f.hub, id).await;
    f.clock.advance(Duration::from_secs(4));
    tokio::time::timeout(WAIT, async {
        loop {
            let view: TargetControlView = f.hub.read(|r| r.require("control", target_id)).unwrap();
            if view.input_state != InputState::Human {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("lease expired without a renewal");
    f.close().await;
}

/// The synthetic desktop with a second screen that has its own input.
#[derive(Debug)]
struct TwoScreens {
    inner: Arc<FakeDesktop>,
    second: ControlTarget,
    releases: Arc<std::sync::atomic::AtomicU64>,
    /// While set, the next release on either screen fails, so a failure can
    /// land on whichever screen is visited first.
    fail_next_release: Arc<std::sync::atomic::AtomicBool>,
}
struct CountingSink(Arc<std::sync::atomic::AtomicU64>);
struct Armed(
    Box<dyn ports::InputSink>,
    Arc<std::sync::atomic::AtomicBool>,
);
#[async_trait::async_trait]
impl ports::InputSink for Armed {
    async fn inject(
        &mut self,
        action: &InputAction,
        geometry: &TargetGeometry,
    ) -> Result<(), ports::PlatformError> {
        self.0.inject(action, geometry).await
    }
    async fn release_all(&mut self) -> Result<(), ports::PlatformError> {
        self.0.release_all().await?;
        if self.1.swap(false, Ordering::SeqCst) {
            return Err(ports::PlatformError::Native(
                "Synthetic release failure".into(),
            ));
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl ports::InputSink for CountingSink {
    async fn inject(
        &mut self,
        _: &InputAction,
        _: &TargetGeometry,
    ) -> Result<(), ports::PlatformError> {
        Ok(())
    }
    async fn release_all(&mut self) -> Result<(), ports::PlatformError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[async_trait::async_trait]
impl ports::DesktopBackend for TwoScreens {
    async fn capabilities(&self) -> ports::DesktopCapabilities {
        self.inner.capabilities().await
    }
    async fn targets(&self) -> Result<Vec<ControlTarget>, ports::PlatformError> {
        let mut targets = self.inner.targets().await?;
        targets.push(self.second.clone());
        Ok(targets)
    }
    async fn capture_still(
        &self,
        id: TargetId,
    ) -> Result<ports::CapturedFrame, ports::PlatformError> {
        self.inner.capture_still(id).await
    }
    async fn open_stream(
        &self,
        id: TargetId,
        fps: u8,
    ) -> Result<Box<dyn ports::FrameStream>, ports::PlatformError> {
        self.inner.open_stream(id, fps).await
    }
    async fn input_sink(
        &self,
        id: TargetId,
    ) -> Result<Box<dyn ports::InputSink>, ports::PlatformError> {
        let sink: Box<dyn ports::InputSink> = if id == self.second.target_id {
            Box::new(CountingSink(self.releases.clone()))
        } else {
            self.inner.input_sink(id).await?
        };
        Ok(Box::new(Armed(sink, self.fail_next_release.clone())))
    }
    async fn lock_session(&self) -> Result<(), ports::PlatformError> {
        self.inner.lock_session().await
    }
    async fn desktop_lock(&self) -> DesktopLockState {
        self.inner.desktop_lock().await
    }
}
/// Sets where the journal's numbering stands, returning where it stood. At
/// the safe-integer ceiling every write that records an event fails at once.
fn journal_head(f: &Fixture, head: i64) -> i64 {
    let db = rusqlite::Connection::open(f.path.join("companion.sqlite")).unwrap();
    let before = db
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='journal'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE sqlite_sequence SET seq=?1 WHERE name='journal'",
        [head],
    )
    .unwrap();
    before
}
#[tokio::test]
async fn turning_sharing_off_and_on_tells_devices_what_the_computer_can_do() {
    let f = Fixture::new().await;
    let caller = f.device().await;
    let mut events = f.hub.subscribe(&caller, None).await.unwrap();
    let missing = protocol::capability::Availability::PermissionMissing {
        remedy: "Run ophio desktop enable".into(),
    };
    let mut told = async |capture: &protocol::capability::Availability| {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Outbound::Event(e) = events.live.recv().await.unwrap()
                    && let Event::CapabilitiesChanged { capabilities } = &e.event
                    && capabilities.capture == *capture
                {
                    break;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("devices were not told capture is {capture:?}"));
    };
    f.hub.set_desktop(None, missing.clone()).await.unwrap();
    told(&missing).await;
    f.hub
        .set_desktop(
            Some(f.desktop.clone()),
            protocol::capability::Availability::Available,
        )
        .await
        .unwrap();
    told(&protocol::capability::Availability::Available).await;
}

#[tokio::test]
async fn turning_sharing_off_releases_every_screen_when_nothing_can_be_recorded() {
    let f = Fixture::new().await;
    let first = f.desktop.target.lock().unwrap().clone();
    let mut second = first.clone();
    second.target_id = TargetId::new();
    second.native_ref = "second".into();
    let backend = Arc::new(TwoScreens {
        inner: f.desktop.clone(),
        second: second.clone(),
        releases: Default::default(),
        fail_next_release: Default::default(),
    });
    f.hub
        .set_desktop(
            Some(backend.clone()),
            protocol::capability::Availability::Available,
        )
        .await
        .unwrap();
    let caller = f.device().await;
    for target in [&first, &second] {
        let command = CommandEnvelope::new(Command::RequestHumanLease {
            target_id: target.target_id,
            geometry_revision: 1,
        });
        let id = command.command_id;
        f.hub.submit(&caller, command).await;
        settle(&f.hub, id).await;
        let view: TargetControlView = f
            .hub
            .read(|r| r.require("control", target.target_id))
            .unwrap();
        assert_eq!(view.input_state, InputState::Human);
    }
    let released = || {
        [
            f.desktop.input.releases.load(Ordering::SeqCst),
            backend.releases.load(Ordering::SeqCst),
        ]
    };
    let before = released();
    let off = || {
        f.hub.set_desktop(
            None,
            protocol::capability::Availability::PermissionMissing {
                remedy: "Run ophio desktop enable".into(),
            },
        )
    };
    let head = journal_head(&f, protocol::MAX_SAFE_INTEGER as i64);
    // Both screens let go, but none of it can be recorded.
    let error = off().await.unwrap_err();
    assert_ne!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    assert_eq!(released(), before.map(|n| n + 1));
    // One screen cannot confirm it let go either, which matters more.
    f.desktop.input.fail_release.store(true, Ordering::SeqCst);
    let error = off().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    assert_eq!(released(), before.map(|n| n + 2));
    // Once both can be done, turning off again finishes the job.
    f.desktop.input.fail_release.store(false, Ordering::SeqCst);
    journal_head(&f, head);
    off().await.unwrap();
    assert_eq!(released(), before.map(|n| n + 3));
    for target in [&first, &second] {
        let view: TargetControlView = f
            .hub
            .read(|r| r.require("control", target.target_id))
            .unwrap();
        assert_eq!(view.input_state, InputState::Paused);
    }
    assert!(matches!(
        f.hub.capabilities().await.capture,
        protocol::capability::Availability::PermissionMissing { .. }
    ));
    f.close().await;
}

/// Two screens the owner's device holds, on the synthetic desktop.
async fn two_held_screens(f: &Fixture) -> (Arc<TwoScreens>, Vec<TargetId>, Caller) {
    let first = f.desktop.target.lock().unwrap().clone();
    let mut second = first.clone();
    second.target_id = TargetId::new();
    second.native_ref = "second".into();
    let backend = Arc::new(TwoScreens {
        inner: f.desktop.clone(),
        second: second.clone(),
        releases: Default::default(),
        fail_next_release: Default::default(),
    });
    f.hub
        .set_desktop(
            Some(backend.clone()),
            protocol::capability::Availability::Available,
        )
        .await
        .unwrap();
    let caller = f.device().await;
    for target in [&first, &second] {
        let command = CommandEnvelope::new(Command::RequestHumanLease {
            target_id: target.target_id,
            geometry_revision: 1,
        });
        let id = command.command_id;
        f.hub.submit(&caller, command).await;
        settle(&f.hub, id).await;
        let view: TargetControlView = f
            .hub
            .read(|r| r.require("control", target.target_id))
            .unwrap();
        assert_eq!(view.input_state, InputState::Human);
    }
    (backend, vec![first.target_id, second.target_id], caller)
}

#[tokio::test]
async fn stopping_control_reports_a_screen_that_could_not_let_go_over_what_went_unrecorded() {
    let f = Fixture::new().await;
    let (backend, ..) = two_held_screens(&f).await;
    // Nothing can be recorded, and the first screen stop reaches cannot
    // confirm it let go. The other one can.
    let head = journal_head(&f, protocol::MAX_SAFE_INTEGER as i64);
    backend.fail_next_release.store(true, Ordering::SeqCst);
    let error = f.hub.stop_all_control().await.unwrap_err();
    journal_head(&f, head);
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    f.close().await;
}

#[tokio::test]
async fn pausing_input_reports_a_release_it_could_not_confirm_over_an_unrecorded_note() {
    let f = Fixture::new().await;
    let (backend, targets, caller) = two_held_screens(&f).await;
    // The pause is carried out after the command is admitted, so refuse
    // records only for the pause itself.
    let head = journal_head(&f, protocol::MAX_SAFE_INTEGER as i64);
    backend.fail_next_release.store(true, Ordering::SeqCst);
    let result = f
        .hub
        .control_command(
            &caller,
            &Command::PauseInput {
                target_id: targets[0],
            },
        )
        .await;
    journal_head(&f, head);
    let error = result.expect_err("an unconfirmed release was reported as done");
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    f.close().await;
}

#[tokio::test]
async fn revoking_a_device_fences_every_screen_it_holds_even_when_one_cannot_let_go() {
    let f = Fixture::new().await;
    let (backend, targets, caller) = two_held_screens(&f).await;
    let Principal::Device { device_id } = caller.principal else {
        panic!("device caller")
    };
    backend.fail_next_release.store(true, Ordering::SeqCst);
    let error = f.hub.revoke_device_leases(device_id).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    for target in targets {
        let view: TargetControlView = f.hub.read(|r| r.require("control", target)).unwrap();
        assert_ne!(view.input_state, InputState::Human, "{target:?} still held");
    }
    f.close().await;
}

/// Makes every write SQLite `on` describes fail, as a full disk would.
fn refuse(f: &Fixture, name: &str, on: &str) {
    let db = rusqlite::Connection::open(f.path.join("companion.sqlite")).unwrap();
    db.execute_batch(&format!(
        "CREATE TRIGGER {name} {on} BEGIN SELECT RAISE(ABORT, 'disk full'); END;"
    ))
    .unwrap();
}

#[tokio::test]
async fn stopping_a_run_fences_its_screen_even_when_its_credential_stays() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            true,
        )
        .await;
    let run_id = run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let input = || {
        f.hub
            .read(|r| r.require::<TargetControlView>("control", target))
            .unwrap()
            .input_state
    };
    assert_eq!(input(), InputState::Agent);
    assert!(
        f.hub
            .read(|r| r.get::<crate::bridge::TokenRecord>("run_token", run_id))
            .unwrap()
            .is_some()
    );
    refuse(
        &f,
        "keep_token",
        "BEFORE DELETE ON documents WHEN old.kind = 'run_token'",
    );
    // The screen cannot confirm it let go either, which matters more.
    f.desktop.input.fail_release.store(true, Ordering::SeqCst);
    let before = f.desktop.input.releases.load(Ordering::SeqCst);
    let error = f.hub.revoke_run(run_id).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    assert_eq!(f.desktop.input.releases.load(Ordering::SeqCst), before + 1);
    assert_ne!(input(), InputState::Agent);
    f.desktop.input.fail_release.store(false, Ordering::SeqCst);
    f.close().await;
}

#[tokio::test]
async fn a_lock_fences_every_screen_even_when_one_cannot_let_go_or_be_recorded() {
    let f = Fixture::new().await;
    let (backend, targets, _) = two_held_screens(&f).await;
    let released = || {
        [
            f.desktop.input.releases.load(Ordering::SeqCst),
            backend.releases.load(Ordering::SeqCst),
        ]
    };
    let before = released();
    // Only this observation runs: the watcher waits behind the operation.
    let _watcher = f.hub.inner.desktop_operation.write().await;
    let head = journal_head(&f, protocol::MAX_SAFE_INTEGER as i64);
    // The first screen the lock reaches cannot confirm it let go, and
    // nothing can be recorded.
    backend.fail_next_release.store(true, Ordering::SeqCst);
    let desktop: Arc<dyn ports::DesktopBackend> = backend.clone();
    let error = f
        .hub
        .observe_desktop_lock(&desktop, DesktopLockState::Locked)
        .await
        .unwrap_err();
    journal_head(&f, head);
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    assert_eq!(released(), before.map(|n| n + 1));
    for target in targets {
        let target = f.hub.target(target).await.unwrap();
        assert_ne!(target.lock().await.view.input_state, InputState::Human);
    }
    drop(_watcher);
    f.close().await;
}

#[tokio::test]
async fn a_lock_fence_that_went_unrecorded_is_saved_whole_once_recording_returns() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            true,
        )
        .await;
    run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let stored = || {
        f.hub
            .read(|r| r.require::<TargetControlView>("control", target))
            .unwrap()
    };
    assert_eq!(stored().input_state, InputState::Agent);
    let _watcher = f.hub.inner.desktop_operation.write().await;
    let desktop: Arc<dyn ports::DesktopBackend> = f.desktop.clone();
    // The computer locks while the agent holds input that cannot be
    // confirmed released, and nothing can be recorded.
    f.desktop.input.fail_release.store(true, Ordering::SeqCst);
    let head = journal_head(&f, protocol::MAX_SAFE_INTEGER as i64);
    let error = f
        .hub
        .observe_desktop_lock(&desktop, DesktopLockState::Locked)
        .await
        .unwrap_err();
    journal_head(&f, head);
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    // Recording returns; the computer is still locked.
    f.hub
        .observe_desktop_lock(&desktop, DesktopLockState::Locked)
        .await
        .unwrap();
    let live = f
        .hub
        .target(target)
        .await
        .unwrap()
        .lock()
        .await
        .view
        .clone();
    assert_eq!(live.input_state, InputState::Unknown);
    // The phone sees what the screen is, not only that it locked.
    assert_eq!(stored(), live);
    drop(_watcher);
    f.desktop.input.fail_release.store(false, Ordering::SeqCst);
    f.close().await;
}

#[tokio::test]
async fn a_stop_the_agent_confirms_still_reports_input_it_could_not_release() {
    for recorded in [true, false] {
        let f = Fixture::new().await;
        let project = f.project().await;
        let (task, _) = f
            .task(
                project,
                TaskPlacement::StartNow,
                &Caller::local_owner(),
                true,
            )
            .await;
        let run_id = run_started(&f, task).await;
        f.desktop.input.fail_release.store(true, Ordering::SeqCst);
        // Without a record, the receipt itself cannot be kept either.
        let head = (!recorded).then(|| journal_head(&f, protocol::MAX_SAFE_INTEGER as i64));
        let result = f.hub.stop_task(task, Some(run_id), false).await;
        if let Some(head) = head {
            journal_head(&f, head);
        }
        let error = result.expect_err("a release it could not confirm was reported as done");
        assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
        // The agent did confirm; only the screen could not.
        assert_eq!(f.harness.controls.interrupts.load(Ordering::SeqCst), 1);
        f.desktop.input.fail_release.store(false, Ordering::SeqCst);
        f.close().await;
    }
}

#[tokio::test]
async fn a_private_step_says_input_may_still_be_held_when_nothing_about_it_can_be_recorded() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            true,
        )
        .await;
    let run_id = run_started(&f, task).await;
    let target_id = f.desktop.target.lock().unwrap().target_id;
    refuse(
        &f,
        "keep_control",
        "BEFORE INSERT ON documents WHEN new.kind = 'control'",
    );
    f.desktop.input.fail_release.store(true, Ordering::SeqCst);
    let command = CommandEnvelope::new(Command::StartPrivateStep { target_id });
    let id = command.command_id;
    f.hub.submit(&Caller::local_owner(), command).await;
    settle(&f.hub, id).await;
    let error = f
        .hub
        .read(|r| r.command(id))
        .unwrap()
        .unwrap()
        .error
        .expect("a release nobody confirmed was reported as done");
    assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
    // Unrecorded, the agent is still stopped.
    assert!(f.hub.run_stopped(run_id).unwrap());
    f.desktop.input.fail_release.store(false, Ordering::SeqCst);
    rusqlite::Connection::open(f.path.join("companion.sqlite"))
        .unwrap()
        .execute_batch("DROP TRIGGER keep_control")
        .unwrap();
    f.close().await;
}
