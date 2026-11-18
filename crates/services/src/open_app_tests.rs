// SPDX-License-Identifier: Apache-2.0
//! A computer-use agent opens installed apps through voice control's
//! launcher, under its agent lease, and never by a command line.
use super::*;
use async_trait::async_trait;
use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse};
use protocol::capability::Availability;
use std::sync::Mutex;
use voice_control::apps::{AppRole, LaunchApp};

/// A stand-in launcher: it records what it opens; nothing appears.
#[derive(Debug, Default)]
struct Opened(
    Mutex<Vec<String>>,
    std::sync::atomic::AtomicBool,
    Gate,
    /// The launch panics after its helper has started.
    std::sync::atomic::AtomicBool,
    /// How a launch that couldn't be stopped can be checked again.
    Mutex<Option<Arc<Proven>>>,
);
/// A launch's scope, which the computer says is gone once set.
#[derive(Debug, Default)]
struct Proven(std::sync::atomic::AtomicBool);
#[async_trait]
impl ports::LaunchCheck for Proven {
    async fn stopped(&self, _: bool) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}
/// Holds the launcher just before its final check while closed.
#[derive(Debug)]
struct Gate {
    closed: std::sync::atomic::AtomicBool,
    /// Hold after the final check instead: the helper is running.
    after_check: std::sync::atomic::AtomicBool,
    reached: tokio::sync::Notify,
    open: tokio::sync::Semaphore,
}
impl Default for Gate {
    fn default() -> Self {
        Self {
            closed: Default::default(),
            after_check: Default::default(),
            reached: Default::default(),
            open: tokio::sync::Semaphore::new(0),
        }
    }
}
#[async_trait]
impl voice_control::Launcher for Opened {
    async fn apps(&self) -> Result<Vec<LaunchApp>, voice_control::Error> {
        let app = |id: &str, name: &str, roles: Vec<AppRole>, detail: Option<&str>| LaunchApp {
            id: id.into(),
            name: name.into(),
            aliases: vec![],
            defaults: roles.clone(),
            roles,
            listed: true,
            detail: detail.map(str::to_owned),
        };
        Ok(vec![
            app(
                "Alacritty.desktop",
                "Alacritty",
                vec![AppRole::Terminal],
                None,
            ),
            app("firefox.desktop", "Firefox", vec![], None),
            app(
                "org.mozilla.firefox.desktop",
                "Firefox",
                vec![],
                Some("Flatpak"),
            ),
            app("reboot.desktop", "Reboot", vec![], None),
        ])
    }
    async fn launch(
        &self,
        _: &protocol::voice::VoiceApp,
        _: &dyn voice_control::Authority,
    ) -> Result<(), voice_control::Error> {
        Err(voice_control::Error::Unavailable(
            "The agent app-opening path must retain owned authority".into(),
        ))
    }
    async fn launch_authorized(
        &self,
        app: &protocol::voice::VoiceApp,
        authority: Arc<dyn voice_control::Authority>,
    ) -> Result<(), voice_control::Error> {
        let after = self.2.after_check.load(std::sync::atomic::Ordering::SeqCst);
        if !after && self.2.closed.load(std::sync::atomic::Ordering::SeqCst) {
            self.2.reached.notify_one();
            self.2.open.acquire().await.unwrap().forget();
        }
        authority.check()?;
        if after && self.2.closed.load(std::sync::atomic::Ordering::SeqCst) {
            self.2.reached.notify_one();
            self.2.open.acquire().await.unwrap().forget();
        }
        assert!(
            !self.3.load(std::sync::atomic::Ordering::SeqCst),
            "the launch panicked after starting its helper"
        );
        // Its launch hung and its scope couldn't be confirmed stopped.
        if self.1.load(std::sync::atomic::Ordering::SeqCst) {
            if let Some(check) = self.4.lock().unwrap().clone() {
                authority.unstopped(check);
            }
            return Err(voice_control::Error::LaunchUnstopped);
        }
        self.0.lock().unwrap().push(app.id.clone());
        Ok(())
    }
    async fn open_url(
        &self,
        _: &str,
        _: &dyn voice_control::Authority,
    ) -> Result<(), voice_control::Error> {
        Ok(())
    }
}
#[derive(Debug)]
struct Unused;
#[async_trait]
impl voice_control::Accessibility for Unused {
    async fn snapshot(&self) -> Result<voice_control::Snapshot, voice_control::Error> {
        Err(voice_control::Error::Unavailable("unused".into()))
    }
    async fn activate(
        &self,
        _: &protocol::voice::VoiceElement,
        _: bool,
    ) -> Result<bool, voice_control::Error> {
        Ok(false)
    }
}
#[async_trait]
impl voice_control::DriverFactory for Unused {
    async fn availability(&self, _: protocol::voice::VoiceTarget) -> Availability {
        Availability::Unsupported {
            reason: "unused".into(),
        }
    }
    async fn open(
        &self,
        _: protocol::voice::VoiceTarget,
    ) -> Result<Arc<dyn voice_control::Driver>, voice_control::Error> {
        Err(voice_control::Error::Unavailable("unused".into()))
    }
}
#[async_trait]
impl ::jev::questions::VoiceAssessor for Unused {
    async fn connection(&self) -> ::jev::Connection {
        ::jev::Connection::NotConnected
    }
    async fn assess_voice(
        &self,
        _: &::jev::questions::VoiceRequest,
    ) -> Result<::jev::questions::VoiceAnswer, ::jev::Error> {
        Err(::jev::Error::NotConnected)
    }
}

fn text(response: BridgeResponse) -> Result<String, String> {
    match response {
        BridgeResponse::AppOpened { text } => Ok(text),
        BridgeResponse::Refused { reason } => Err(reason),
        BridgeResponse::Error { message } => Err(message),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_computer_use_agent_opens_apps_by_name_under_its_lease() {
    let f = Fixture::new().await;
    let opened = Arc::new(Opened::default());
    f.hub
        .configure_voice(crate::voice::VoiceConfig {
            browser: Arc::new(Unused),
            desktop: Arc::new(Unused),
            launcher: opened.clone(),
            assessor: Arc::new(Unused),
        })
        .unwrap();
    let phone = f.device().await;
    let project = f.project().await;
    let (task, _) = f.task(project, TaskPlacement::StartNow, &phone, true).await;
    let run = run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let binding = BridgeBinding {
        checks: false,
        run_id: run,
        target_id: Some(target),
        computer_use: true,
        browser: false,
        github: false,
        checklist_answers: false,
        read_only: false,
    };
    let open = |name: &str| {
        let (hub, binding) = (f.hub.clone(), binding.clone());
        let name = name.to_owned();
        async move { text(hub.handle(&binding, BridgeRequest::OpenApp { name }).await) }
    };
    // Like any input, only after it has looked at the screen.
    assert!(
        open("a terminal")
            .await
            .unwrap_err()
            .contains("observe_target")
    );
    assert!(matches!(
        f.hub
            .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
            .await,
        BridgeResponse::Observation { .. }
    ));
    let done = open("a terminal").await.unwrap();
    assert!(done.contains("Alacritty"), "{done}");
    // Two of one name: the agent is told how to say which.
    let several = open("firefox").await.unwrap_err();
    assert!(several.contains("Firefox, Firefox (Flatpak)"), "{several}");
    assert!(open("reboot").await.unwrap_err().contains("request_human"));
    assert!(
        open("blender")
            .await
            .unwrap_err()
            .starts_with("Not opened: No app called")
    );
    // A name is never a command line.
    assert!(open("sh -c 'rm -rf ~'").await.is_err());
    assert_eq!(*opened.0.lock().unwrap(), ["Alacritty.desktop"]);
    // Once the lease lapses, nothing opens.
    f.clock.advance(Duration::from_secs(4));
    let late = tokio::time::timeout(Duration::from_millis(300), open("a terminal")).await;
    assert!(late.is_err() || late.unwrap().is_err());
    assert_eq!(opened.0.lock().unwrap().len(), 1);
    f.close().await;
}

/// A launch whose processes couldn't be confirmed stopped ends the agent's
/// control, and no later Stop claims a clean release: an app may still
/// open, and every release says so.
#[tokio::test]
async fn a_launch_that_could_not_be_stopped_ends_control_and_stop_says_an_app_may_open() {
    let f = Fixture::new().await;
    let opened = Arc::new(Opened::default());
    opened.1.store(true, std::sync::atomic::Ordering::SeqCst);
    f.hub
        .configure_voice(crate::voice::VoiceConfig {
            browser: Arc::new(Unused),
            desktop: Arc::new(Unused),
            launcher: opened.clone(),
            assessor: Arc::new(Unused),
        })
        .unwrap();
    let phone = f.device().await;
    let project = f.project().await;
    let (task, _) = f.task(project, TaskPlacement::StartNow, &phone, true).await;
    let run = run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let binding = BridgeBinding {
        checks: false,
        run_id: run,
        target_id: Some(target),
        computer_use: true,
        browser: false,
        github: false,
        checklist_answers: false,
        read_only: false,
    };
    assert!(matches!(
        f.hub
            .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
            .await,
        BridgeResponse::Observation { .. }
    ));
    let failed = text(
        f.hub
            .handle(
                &binding,
                BridgeRequest::OpenApp {
                    name: "a terminal".into(),
                },
            )
            .await,
    )
    .unwrap_err();
    assert!(failed.contains("may still open"), "{failed}");
    // The agent no longer holds control.
    let view = f
        .hub
        .target(target)
        .await
        .unwrap()
        .lock()
        .await
        .view
        .clone();
    assert!(
        view.lease
            .as_ref()
            .is_none_or(|l| l.state != protocol::control::LeaseState::Active),
        "{view:?}"
    );
    assert_ne!(view.input_state, protocol::control::InputState::Agent);
    // Stop doesn't claim a clean release, now or later.
    for _ in 0..2 {
        let stop = f.hub.revoke_target(target, false).await.unwrap_err();
        assert_eq!(stop.code, ErrorCode::OutcomeUnknown);
        assert!(stop.message.contains("may still open"), "{stop:?}");
    }
    // The agent's next input is refused at once with the same words: no
    // one is asked to hand back a screen that can't be handed back.
    let typed = tokio::time::timeout(
        Duration::from_secs(2),
        f.hub
            .handle(&binding, BridgeRequest::TypeText { text: "x".into() }),
    )
    .await
    .expect("refused at once, not waiting for a hand-back");
    let typed = text(typed).unwrap_err();
    assert!(typed.contains("may still open"), "{typed}");
    // Taking control says the same.
    let taken = f
        .hub
        .control_command(
            &phone,
            &Command::TakeOver {
                target_id: target,
                geometry_revision: 1,
            },
        )
        .await
        .unwrap_err();
    assert!(taken.message.contains("may still open"), "{taken:?}");
    // The run still ends, so its project's queue moves on; the display
    // keeps the mark.
    f.hub.revoke_run(run).await.unwrap();
    assert!(f.hub.revoke_target(target, false).await.is_err());
    let _ = f.hub.shutdown().await;
}

/// A Local Stop pressed while the agent's app is being got ready to open
/// stops it opening, though the lease still looks active until the stop
/// reaches the display.
#[tokio::test]
async fn a_local_stop_during_an_agents_launch_stops_the_app_opening() {
    let f = Fixture::new().await;
    let opened = Arc::new(Opened::default());
    opened
        .2
        .closed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    f.hub
        .configure_voice(crate::voice::VoiceConfig {
            browser: Arc::new(Unused),
            desktop: Arc::new(Unused),
            launcher: opened.clone(),
            assessor: Arc::new(Unused),
        })
        .unwrap();
    let phone = f.device().await;
    let project = f.project().await;
    let (task, _) = f.task(project, TaskPlacement::StartNow, &phone, true).await;
    let run = run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let binding = BridgeBinding {
        checks: false,
        run_id: run,
        target_id: Some(target),
        computer_use: true,
        browser: false,
        github: false,
        checklist_answers: false,
        read_only: false,
    };
    assert!(matches!(
        f.hub
            .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
            .await,
        BridgeResponse::Observation { .. }
    ));
    let launch = {
        let (hub, binding) = (f.hub.clone(), binding.clone());
        tokio::spawn(async move {
            text(
                hub.handle(
                    &binding,
                    BridgeRequest::OpenApp {
                        name: "a terminal".into(),
                    },
                )
                .await,
            )
        })
    };
    opened.2.reached.notified().await;
    // Local Stop has begun: its flag is set; it waits for the display.
    f.hub
        .inner
        .local_stops
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    opened.2.open.add_permits(1);
    assert!(launch.await.unwrap().is_err());
    assert!(opened.0.lock().unwrap().is_empty());
    f.hub
        .inner
        .local_stops
        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    f.close().await;
}
/// An agent's launch whose tool call is dropped (its connection closed)
/// still runs to its end: a Stop meanwhile waits for it, and a launch that
/// couldn't be stopped is kept, so Stop says an app may still open.
#[tokio::test]
async fn a_dropped_launch_is_waited_for_and_kept_when_it_cannot_be_stopped() {
    let f = Fixture::new().await;
    let opened = Arc::new(Opened::default());
    opened.1.store(true, std::sync::atomic::Ordering::SeqCst);
    opened
        .2
        .closed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    // The helper is already running when the call is dropped.
    opened
        .2
        .after_check
        .store(true, std::sync::atomic::Ordering::SeqCst);
    f.hub
        .configure_voice(crate::voice::VoiceConfig {
            browser: Arc::new(Unused),
            desktop: Arc::new(Unused),
            launcher: opened.clone(),
            assessor: Arc::new(Unused),
        })
        .unwrap();
    let phone = f.device().await;
    let project = f.project().await;
    let (task, _) = f.task(project, TaskPlacement::StartNow, &phone, true).await;
    let run = run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let binding = BridgeBinding {
        checks: false,
        run_id: run,
        target_id: Some(target),
        computer_use: true,
        browser: false,
        github: false,
        checklist_answers: false,
        read_only: false,
    };
    assert!(matches!(
        f.hub
            .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
            .await,
        BridgeResponse::Observation { .. }
    ));
    let call = {
        let (hub, binding) = (f.hub.clone(), binding.clone());
        tokio::spawn(async move {
            hub.handle(
                &binding,
                BridgeRequest::OpenApp {
                    name: "a terminal".into(),
                },
            )
            .await
        })
    };
    opened.2.reached.notified().await;
    // The agent's connection closes: its call is dropped mid-launch.
    call.abort();
    let _ = call.await;
    let stop = {
        let hub = f.hub.clone();
        tokio::spawn(async move { hub.revoke_target(target, false).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    opened.2.open.add_permits(1);
    let stopped = stop.await.unwrap().unwrap_err();
    assert!(stopped.message.contains("may still open"), "{stopped:?}");
    let _ = f.hub.shutdown().await;
}

/// A launch that panics after its helper started, with the agent's call
/// already gone, is kept as unresolved, as it is with the call still there.
#[tokio::test]
async fn a_launch_that_panics_after_its_call_is_dropped_is_kept_as_unresolved() {
    let f = Fixture::new().await;
    let opened = Arc::new(Opened::default());
    opened.3.store(true, std::sync::atomic::Ordering::SeqCst);
    opened
        .2
        .closed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    // The helper is already running when the call is dropped.
    opened
        .2
        .after_check
        .store(true, std::sync::atomic::Ordering::SeqCst);
    f.hub
        .configure_voice(crate::voice::VoiceConfig {
            browser: Arc::new(Unused),
            desktop: Arc::new(Unused),
            launcher: opened.clone(),
            assessor: Arc::new(Unused),
        })
        .unwrap();
    let phone = f.device().await;
    let project = f.project().await;
    let (task, _) = f.task(project, TaskPlacement::StartNow, &phone, true).await;
    let run = run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let binding = BridgeBinding {
        checks: false,
        run_id: run,
        target_id: Some(target),
        computer_use: true,
        browser: false,
        github: false,
        checklist_answers: false,
        read_only: false,
    };
    assert!(matches!(
        f.hub
            .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
            .await,
        BridgeResponse::Observation { .. }
    ));
    let call = {
        let (hub, binding) = (f.hub.clone(), binding.clone());
        tokio::spawn(async move {
            hub.handle(
                &binding,
                BridgeRequest::OpenApp {
                    name: "a terminal".into(),
                },
            )
            .await
        })
    };
    opened.2.reached.notified().await;
    // The agent's connection closes: its call is dropped mid-launch.
    call.abort();
    let _ = call.await;
    let stop = {
        let hub = f.hub.clone();
        tokio::spawn(async move { hub.revoke_target(target, false).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    opened.2.open.add_permits(1);
    let stopped = stop.await.unwrap().unwrap_err();
    assert!(stopped.message.contains("may still open"), "{stopped:?}");
    let _ = f.hub.shutdown().await;
}

/// Stop task on a marked display records the agent's desktop access as
/// unknown, saying an app may still open, not as withdrawn. A run whose
/// input wasn't let go keeps being retried until it is; once only the
/// launch is unresolved, the run's end completes.
#[tokio::test]
async fn stop_task_on_a_marked_display_records_it_truthfully() {
    let f = Fixture::new().await;
    let opened = Arc::new(Opened::default());
    opened.1.store(true, std::sync::atomic::Ordering::SeqCst);
    f.hub
        .configure_voice(crate::voice::VoiceConfig {
            browser: Arc::new(Unused),
            desktop: Arc::new(Unused),
            launcher: opened.clone(),
            assessor: Arc::new(Unused),
        })
        .unwrap();
    let phone = f.device().await;
    let project = f.project().await;
    let (task, _) = f.task(project, TaskPlacement::StartNow, &phone, true).await;
    let run = run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let binding = BridgeBinding {
        checks: false,
        run_id: run,
        target_id: Some(target),
        computer_use: true,
        browser: false,
        github: false,
        checklist_answers: false,
        read_only: false,
    };
    assert!(matches!(
        f.hub
            .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
            .await,
        BridgeResponse::Observation { .. }
    ));
    // The launch can't be stopped, and input can't be let go either.
    f.desktop
        .input
        .fail_release
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let opened_now = text(
        f.hub
            .handle(
                &binding,
                BridgeRequest::OpenApp {
                    name: "a terminal".into(),
                },
            )
            .await,
    )
    .unwrap_err();
    // The agent is told input wasn't let go too.
    assert!(
        opened_now.contains("let go of this display’s input"),
        "{opened_now}"
    );
    // While input is held, ending the run is still retried.
    assert!(f.hub.revoke_run(run).await.is_err());
    // Once input is let go, only the launch is left: the run's end completes.
    f.desktop
        .input
        .fail_release
        .store(false, std::sync::atomic::Ordering::SeqCst);
    f.hub.revoke_run(run).await.unwrap();
    let stop = CommandEnvelope::new(Command::StopTask {
        task_id: task,
        force: false,
    });
    let id = stop.command_id;
    f.hub.submit(&phone, stop).await;
    settle(&f.hub, id).await;
    let QueryData::TaskTimeline { items, .. } = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::TaskTimeline {
                task_id: task,
                before_seq: None,
                limit: 100,
            },
        )
        .await
        .unwrap()
    else {
        panic!("timeline")
    };
    let fact = items
        .iter()
        .find_map(|i| match &i.item {
            TimelineBody::Receipt { receipt } if receipt.kind == InterventionKind::StopTask => {
                receipt
                    .facts
                    .iter()
                    .find(|f| f.label == "Desktop access withdrawn from the agent")
                    .cloned()
            }
            _ => None,
        })
        .expect("Stop receipt");
    assert_eq!(fact.status, FactStatus::Unknown, "{fact:?}");
    assert!(
        fact.detail
            .as_deref()
            .is_some_and(|d| d.contains("may still open")),
        "{fact:?}"
    );
    let _ = f.hub.shutdown().await;
}

/// An agent's launch that couldn't be stopped marks its display until the
/// computer confirms the launch stopped; then the display can be taken
/// again, without restarting Ophio.
#[tokio::test]
async fn a_display_an_agents_launch_marked_clears_once_the_launch_is_confirmed_stopped() {
    let f = Fixture::new().await;
    let opened = Arc::new(Opened::default());
    opened.1.store(true, std::sync::atomic::Ordering::SeqCst);
    let proven = Arc::new(Proven::default());
    *opened.4.lock().unwrap() = Some(proven.clone());
    f.hub
        .configure_voice(crate::voice::VoiceConfig {
            browser: Arc::new(Unused),
            desktop: Arc::new(Unused),
            launcher: opened.clone(),
            assessor: Arc::new(Unused),
        })
        .unwrap();
    let phone = f.device().await;
    let project = f.project().await;
    let (task, _) = f.task(project, TaskPlacement::StartNow, &phone, true).await;
    let run = run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let binding = BridgeBinding {
        checks: false,
        run_id: run,
        target_id: Some(target),
        computer_use: true,
        browser: false,
        github: false,
        checklist_answers: false,
        read_only: false,
    };
    let _ = f
        .hub
        .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
        .await;
    let failed = text(
        f.hub
            .handle(
                &binding,
                BridgeRequest::OpenApp {
                    name: "a terminal".into(),
                },
            )
            .await,
    )
    .unwrap_err();
    assert!(failed.contains("may still open"), "{failed}");
    let display = f.hub.target(target).await.unwrap();
    // Not confirmed yet: still marked after a few checks.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(display.lock().await.unresolved_launch);
    proven.0.store(true, std::sync::atomic::Ordering::SeqCst);
    let began = std::time::Instant::now();
    while display.lock().await.unresolved_launch {
        assert!(began.elapsed() < Duration::from_secs(5), "never cleared");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    f.hub.revoke_run(run).await.unwrap();
    assert!(matches!(
        f.hub
            .control_command(
                &phone,
                &Command::RequestHumanLease {
                    target_id: target,
                    geometry_revision: 1,
                },
            )
            .await,
        Ok(Some(protocol::command::CommandResult::LeaseGranted { .. }))
    ));
    let _ = f.hub.shutdown().await;
}
