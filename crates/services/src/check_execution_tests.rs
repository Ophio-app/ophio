// SPDX-License-Identifier: Apache-2.0
//! Exercise durable execution with real hub/store transactions and no processes.
use super::*;
use crate::testing::{FakeClock, FakeDesktop, FakeHarness, FakeMedia, FakeSpeech, FakeTerminal};
use crate::{
    api::{Caller, Hub},
    bridge,
};
use protocol::{
    capability::HarnessKind,
    command::{CommandResult, Reconciliation},
};
use std::{collections::VecDeque, sync::atomic::AtomicUsize};
use tokio::sync::Semaphore;

const WAIT: Duration = Duration::from_secs(5);

struct Step {
    output: CheckOutput,
    gate: Option<Arc<Semaphore>>,
}

struct FakeExecutor {
    steps: Mutex<VecDeque<Step>>,
    start_errors: Mutex<VecDeque<runner::StartError>>,
    requests: Mutex<Vec<CheckRequest>>,
    inspections: Mutex<Vec<ProcessIdentity>>,
    stops: Mutex<Vec<ProcessIdentity>>,
    inspect_state: Mutex<ProcessState>,
    stop_status: Mutex<StopStatus>,
    inspect_gate: Mutex<Option<Arc<Semaphore>>>,
    cancelled: Arc<AtomicUsize>,
}

impl Default for FakeExecutor {
    fn default() -> Self {
        Self {
            steps: Mutex::new(VecDeque::new()),
            start_errors: Mutex::new(VecDeque::new()),
            requests: Mutex::new(vec![]),
            inspections: Mutex::new(vec![]),
            stops: Mutex::new(vec![]),
            inspect_state: Mutex::new(ProcessState::Stopped),
            stop_status: Mutex::new(StopStatus::Confirmed),
            inspect_gate: Mutex::new(None),
            cancelled: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl FakeExecutor {
    fn push(
        &self,
        outcome: runner::CheckOutcome,
        duration_ms: u64,
        gate: Option<Arc<Semaphore>>,
        stop: StopStatus,
    ) {
        self.steps.lock().unwrap().push_back(Step {
            output: CheckOutput {
                exit_code: match outcome {
                    runner::CheckOutcome::Passed => Some(0),
                    runner::CheckOutcome::Failed => Some(7),
                    _ => None,
                },
                outcome,
                duration_ms,
                output_tail: "captured check output".into(),
                output_bytes: 21,
                stop,
                sandbox_level: runner::SandboxLevel::Contained,
            },
            gate,
        });
    }
}

#[async_trait::async_trait]
impl Executor for FakeExecutor {
    async fn start(&self, request: CheckRequest) -> Result<Execution, runner::StartError> {
        let identity = identity(request.project.clone());
        self.requests.lock().unwrap().push(request);
        if let Some(error) = self.start_errors.lock().unwrap().pop_front() {
            return Err(error);
        }
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected check launch");
        let (cancel, mut cancelled) = watch::channel(false);
        let count = self.cancelled.clone();
        Ok(Execution {
            identity,
            sandbox: CheckSandboxLevel::Contained,
            cancel: Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                cancel.send_replace(true);
            }),
            result: Box::pin(async move {
                if let Some(gate) = step.gate {
                    tokio::select! {
                        permit = gate.acquire() => { permit.expect("result gate closed").forget(); },
                        _ = cancelled.changed() => {},
                    }
                }
                Ok(step.output)
            }),
        })
    }
    async fn inspect(&self, identity: &ProcessIdentity) -> ProcessState {
        self.inspections.lock().unwrap().push(identity.clone());
        let gate = self.inspect_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.acquire().await.expect("inspect gate closed").forget();
        }
        self.inspect_state.lock().unwrap().clone()
    }
    async fn stop(&self, identity: &ProcessIdentity) -> StopStatus {
        self.stops.lock().unwrap().push(identity.clone());
        self.stop_status.lock().unwrap().clone()
    }
}

fn identity(project: PathBuf) -> ProcessIdentity {
    ProcessIdentity {
        pid: 4242,
        start_time: 12345,
        sandbox_unit: Some("fake-check-scope".into()),
        project,
        boot_id: "fake-boot".into(),
    }
}

fn check(command: &str) -> Check {
    Check {
        check_id: CheckId::new(),
        name: format!("Check {command}"),
        about: "A deterministic test check.".into(),
        run: command.into(),
        setup: Some("SETUP MUST NEVER RUN".into()),
        paths: vec![],
        when: CheckWhen::EveryTask,
        timeout_secs: 17,
        network: true,
        inputs: vec!["package.json".into()],
        enabled: true,
        typical_secs: None,
    }
}

struct ExecutionFixture {
    hub: CompanionHub,
    fake: Arc<FakeExecutor>,
    project: ProjectId,
    root: tempfile::TempDir,
}
impl ExecutionFixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_path = root.path().join("project");
        std::fs::create_dir_all(&project_path).unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(home.join(".cargo/bin")).unwrap();
        let hub = CompanionHub::builder()
            .data_dir(root.path().join("data"))
            .inbox_dir(root.path().join("inbox"))
            .home_dir(home)
            .clock(Arc::new(FakeClock::default()))
            .harness(Arc::new(FakeHarness::default()))
            .desktop(Arc::new(FakeDesktop::default()))
            .media(Arc::new(FakeMedia))
            .terminal(Arc::new(FakeTerminal::default()))
            .speech(Arc::new(FakeSpeech))
            .bridge(bridge::BridgeConfig::new(
                "synthetic-bridge",
                "synthetic-socket",
            ))
            .build()
            .await
            .unwrap();
        hub.inner.checks.paused.store(true, Ordering::Release);
        // These tests drive execution directly, not through the dispatcher.
        hub.inner.checks.recovering.store(true, Ordering::Release);
        until(|| !hub.inner.checks.scheduling.load(Ordering::Acquire)).await;
        let fake = Arc::new(FakeExecutor::default());
        *hub.inner.checks.runner.lock().unwrap() = fake.clone();
        let receipt = hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::AddProject {
                    host_path: project_path.to_string_lossy().into_owned(),
                    name: Some("Execution test".into()),
                }),
            )
            .await;
        let Some(CommandResult::ProjectAdded { project_id }) = receipt.result else {
            panic!("project creation failed: {receipt:?}");
        };
        Self {
            hub,
            fake,
            project: project_id,
            root,
        }
    }
    fn seed(
        &self,
        checks: Vec<Check>,
        results: Vec<CheckResult>,
        state: CheckJobState,
    ) -> CheckJob {
        let job = CheckJob {
            id: CheckJobId::new(),
            project_id: self.project,
            task_id: None,
            run_id: None,
            purpose: CheckJobPurpose::RunNow {
                check_ids: checks.iter().map(|c| c.check_id).collect(),
            },
            checks_version: self
                .hub
                .read(|r| {
                    Ok(r.project_checks(self.project)?
                        .map_or(0, |checks| checks.version))
                })
                .unwrap(),
            checks,
            state,
            process: None,
            created_at: self.hub.inner.clock.now(),
            started_at: Some(self.hub.inner.clock.now()),
            finished_at: None,
            sandbox: None,
            results,
            message: None,
        };
        self.hub
            .write(|tx| {
                tx.put_check_job(&job)?;
                let mut project: Project = tx.read().require("project", self.project)?;
                let mut queue: QueueView = tx.read().require("queue", self.project)?;
                project.active_writer = Some(job_writer(job.id));
                queue.writer = project.active_writer;
                tx.put("project", self.project, &project)?;
                tx.put("queue", self.project, &queue)
            })
            .unwrap();
        job
    }
    fn job(&self, id: CheckJobId) -> CheckJob {
        self.hub
            .read(|r| r.require(store::checks::JOBS, id))
            .unwrap()
    }
    fn record(&self, job: &CheckJob, record: ProcessRecord) {
        self.hub
            .write(|tx| tx.put(PROCESSES, job.id, &record))
            .unwrap();
    }
    fn reserved(&self, job: &CheckJob, reserved: bool) {
        self.hub
            .read(|r| {
                let project: Project = r.require("project", self.project)?;
                let queue: QueueView = r.require("queue", self.project)?;
                let expected = reserved.then_some(job_writer(job.id));
                assert_eq!(project.active_writer, expected);
                assert_eq!(queue.writer, expected);
                Ok(())
            })
            .unwrap();
    }
    async fn execute(&self, job: &CheckJob) {
        tokio::time::timeout(
            WAIT,
            self.hub.execute_check_job(
                job.id,
                Arc::new(Control {
                    cancel: watch::channel(false).0,
                }),
            ),
        )
        .await
        .expect("execution stalled")
        .unwrap();
    }
    async fn queued_task(&self) -> TaskId {
        let receipt = self
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::CreateTask {
                    project_id: self.project,
                    kind: TaskKind::Coding,
                    origin: None,
                    from_chat: None,
                    mode: None,
                    title: Some("Check access test".into()),
                    prompt: "test".into(),
                    bundle: None,
                    harness: HarnessKind::ClaudeCode,
                    model: None,
                    effort: None,
                    launch_policy: None,
                    branch: None,
                    start: None,
                    place: None,
                    browser: None,
                    conversation: None,
                    access: None,
                    github: None,
                    mission: None,
                    target_id: None,
                    placement: protocol::command::TaskPlacement::Queue {
                        position: None,
                        authorization: protocol::queue::QueueAuthorization::AskBeforeStart,
                    },
                    agent_chosen: None,
                    review: None,
                }),
            )
            .await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            panic!("task creation failed: {receipt:?}");
        };
        task_id
    }
    async fn checking_task(&self) -> (TaskId, RunId, CheckJob) {
        self.checking_task_with_proof(false).await
    }
    async fn checking_task_with_proof(&self, proof: bool) -> (TaskId, RunId, CheckJob) {
        self.hub
            .write(|tx| {
                tx.change_checks(
                    self.project,
                    0,
                    CheckEdit::Save {
                        check: check("the approved command"),
                    },
                    None,
                    &self.root.path().join("project"),
                )?;
                Ok(())
            })
            .unwrap();
        let task_id = self.queued_task().await;
        // Admit a real Work run without dispatching an agent process. Its
        // ordinary end captures files and reserves the actual check job.
        let run_id = self
            .hub
            .write(|tx| {
                let mut task: Task = tx.read().require("task", task_id)?;
                let run_id =
                    self.hub
                        .new_run(tx, &mut task, CommandId::new(), None, None, false)?;
                tx.put("run_access", run_id, &task.access)?;
                tx.put("task", task_id, &task)?;
                Ok(run_id)
            })
            .unwrap();
        self.hub.capture_checks_start(run_id).await.unwrap();
        if proof {
            self.hub
                .write(|tx| {
                    tx.propose_checks(
                        self.project,
                        1,
                        Some(task_id),
                        None,
                        vec![CheckDraftItem::Add {
                            check: check("proposed command"),
                            note: "Check the suggested change.".into(),
                        }],
                        &self.root.path().join("project"),
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        std::fs::write(self.root.path().join("project/changed.txt"), "changed").unwrap();
        self.hub
            .prepare_checks_end(&crate::check_lifecycle::DeferredEnd {
                run_id,
                state: RunState::Completed,
                outcome: None,
                helpers_running: false,
                why: None,
            })
            .await
            .unwrap();
        self.hub
            .finish_run(run_id, RunState::Completed, None)
            .unwrap();
        let job = self
            .hub
            .read(|r| {
                let task: Task = r.require("task", task_id)?;
                assert_eq!(task.state, TaskState::Working);
                r.require(store::checks::JOBS, task.checking.unwrap().job_id)
            })
            .unwrap();
        (task_id, run_id, job)
    }
    async fn reopened(self) -> Self {
        let Self {
            hub,
            fake,
            project,
            root,
        } = self;
        hub.shutdown().await.unwrap();
        crate::testing::drop_hub_and_wait(hub, WAIT).await;
        let hub = CompanionHub::builder()
            .data_dir(root.path().join("data"))
            .inbox_dir(root.path().join("inbox"))
            .home_dir(root.path().join("home"))
            .clock(Arc::new(FakeClock::default()))
            .harness(Arc::new(FakeHarness::default()))
            .desktop(Arc::new(FakeDesktop::default()))
            .media(Arc::new(FakeMedia))
            .terminal(Arc::new(FakeTerminal::default()))
            .speech(Arc::new(FakeSpeech))
            .bridge(bridge::BridgeConfig::new(
                "synthetic-bridge",
                "synthetic-socket",
            ))
            .build()
            .await
            .unwrap();
        hub.inner.checks.paused.store(true, Ordering::Release);
        hub.inner.checks.recovering.store(true, Ordering::Release);
        until(|| !hub.inner.checks.scheduling.load(Ordering::Acquire)).await;
        *hub.inner.checks.runner.lock().unwrap() = fake.clone();
        Self {
            hub,
            fake,
            project,
            root,
        }
    }
    async fn close(self) {
        self.hub.shutdown().await.unwrap();
    }
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fake executor did not reach expected boundary");
}

#[tokio::test]
async fn records_runner_outcomes_output_and_measured_durations() {
    let f = ExecutionFixture::new().await;
    let outcomes = [
        (runner::CheckOutcome::Passed, CheckOutcome::Passed),
        (runner::CheckOutcome::Failed, CheckOutcome::Failed),
        (runner::CheckOutcome::TimedOut, CheckOutcome::TimedOut),
        (
            runner::CheckOutcome::CouldNotRun {
                reason: "tool unavailable".into(),
            },
            CheckOutcome::CouldNotRun {
                reason: "tool unavailable".into(),
            },
        ),
    ];
    let checks: Vec<_> = (0..outcomes.len())
        .map(|i| check(&format!("frozen {i}")))
        .collect();
    for (i, (outcome, _)) in outcomes.iter().enumerate() {
        f.fake.push(
            outcome.clone(),
            1250 + i as u64 * 1000,
            None,
            StopStatus::Confirmed,
        );
    }
    let job = f.seed(checks, vec![], CheckJobState::Running);
    f.execute(&job).await;
    let finished = f.job(job.id);
    assert_eq!(finished.state, CheckJobState::Finished);
    assert!(finished.finished_at.is_some());
    assert!(finished.process.is_none());
    assert_eq!(finished.sandbox, Some(CheckSandboxLevel::Contained));
    for (i, result) in finished.results.iter().enumerate() {
        assert_eq!(result.outcome, outcomes[i].1);
        assert_eq!(result.duration_secs, 1.25 + i as f64);
        assert_eq!(result.output, "captured check output");
    }
    assert_eq!(finished.results[0].exit_code, Some(0));
    assert_eq!(finished.results[1].exit_code, Some(7));
    f.reserved(&job, false);
    f.close().await;
}

#[tokio::test]
async fn frozen_checks_run_sequentially_without_setup_and_keep_skip_off_and_inputs() {
    let f = ExecutionFixture::new().await;
    let first = check("frozen first");
    let second = check("frozen second");
    let mut off = check("disabled command");
    off.enabled = false;
    let skipped = check("unmatched paths command");
    let mut off_result = failed_start(&off, "");
    off_result.outcome = CheckOutcome::Off;
    let mut skipped_result = failed_start(&skipped, "");
    skipped_result.outcome = CheckOutcome::SkippedPaths;
    let job = f.seed(
        vec![first.clone(), second.clone(), off, skipped],
        vec![off_result.clone(), skipped_result.clone()],
        CheckJobState::Running,
    );
    f.hub
        .write(|tx| {
            tx.put(
                "check_job_inputs",
                job.id,
                &BTreeMap::from([
                    (first.check_id, vec!["package.json".to_string()]),
                    (second.check_id, vec!["lock.json".to_string()]),
                ]),
            )?;
            // A later approval must not replace the job's frozen command.
            tx.change_checks(
                f.project,
                0,
                CheckEdit::Save {
                    check: Check {
                        run: "later approval".into(),
                        ..first.clone()
                    },
                },
                None,
                &f.root.path().join("project"),
            )?;
            Ok(())
        })
        .unwrap();
    let gate = Arc::new(Semaphore::new(0));
    f.fake.push(
        runner::CheckOutcome::Passed,
        2500,
        Some(gate.clone()),
        StopStatus::Confirmed,
    );
    f.fake.push(
        runner::CheckOutcome::Failed,
        500,
        None,
        StopStatus::Confirmed,
    );
    let control = Arc::new(Control {
        cancel: watch::channel(false).0,
    });
    let run = f.hub.execute_check_job(job.id, control);
    let observe = async {
        until(|| f.fake.requests.lock().unwrap().len() == 1).await;
        assert_eq!(
            f.job(job.id).results,
            vec![off_result.clone(), skipped_result.clone()]
        );
        f.reserved(&job, true);
        let durable: ProcessRecord = f.hub.read(|r| r.require(PROCESSES, job.id)).unwrap();
        assert!(!durable.launching);
        assert_eq!(
            durable.identity,
            Some(identity(f.root.path().join("project")))
        );
        assert!(f.job(job.id).process.is_some());
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            f.fake.requests.lock().unwrap().len(),
            1,
            "second check started before first finished"
        );
        gate.add_permits(1);
    };
    let (result, ()) = tokio::time::timeout(WAIT, async { tokio::join!(run, observe) })
        .await
        .unwrap();
    result.unwrap();
    let requests = f.fake.requests.lock().unwrap().clone();
    assert_eq!(
        requests
            .iter()
            .map(|r| r.command.as_str())
            .collect::<Vec<_>>(),
        vec!["frozen first", "frozen second"]
    );
    for request in requests {
        assert_eq!(request.project, f.root.path().join("project"));
        assert_eq!(request.data, f.root.path().join("data/checks"));
        assert_eq!(request.timeout, Duration::from_secs(17));
        assert!(
            request.network,
            "this approved check requests network access and ordinary Work permits it"
        );
        assert_eq!(
            request.read_toolchains,
            vec![f.root.path().join("home/.cargo/bin")]
        );
    }
    let finished = f.job(job.id);
    assert_eq!(&finished.results[..2], &[off_result, skipped_result]);
    assert_eq!(finished.results[2].inputs_changed, vec!["package.json"]);
    assert_eq!(finished.results[3].inputs_changed, vec!["lock.json"]);
    f.reserved(&job, false);
    f.close().await;
}

#[tokio::test]
async fn cancellation_waits_for_confirmed_cleanup_or_keeps_the_writer_uncertain() {
    for stop in [
        StopStatus::Confirmed,
        StopStatus::Uncertain {
            reason: "child cannot be accounted for".into(),
        },
    ] {
        let f = ExecutionFixture::new().await;
        let first = check("cancel this");
        let second = check("must not launch");
        let job = f.seed(
            vec![first.clone(), second.clone()],
            vec![],
            CheckJobState::Running,
        );
        f.fake.push(
            runner::CheckOutcome::Cancelled,
            875,
            Some(Arc::new(Semaphore::new(0))),
            stop.clone(),
        );
        let control = Arc::new(Control {
            cancel: watch::channel(false).0,
        });
        let execute = f.hub.execute_check_job(job.id, control.clone());
        let cancel = async {
            until(|| f.job(job.id).process.is_some()).await;
            f.reserved(&job, true);
            f.hub
                .write(|tx| {
                    let mut current: CheckJob = tx.read().require(store::checks::JOBS, job.id)?;
                    current.state = CheckJobState::Cancelling;
                    tx.put_check_job(&current)
                })
                .unwrap();
            control.cancel.send_replace(true);
        };
        let (result, ()) = tokio::time::timeout(WAIT, async { tokio::join!(execute, cancel) })
            .await
            .unwrap();
        result.unwrap();
        assert_eq!(f.fake.cancelled.load(Ordering::SeqCst), 1);
        assert_eq!(f.fake.requests.lock().unwrap().len(), 1);
        let final_job = f.job(job.id);
        assert_eq!(final_job.results[0].outcome, CheckOutcome::Stopped);
        if stop == StopStatus::Confirmed {
            assert_eq!(final_job.state, CheckJobState::Finished);
            assert_eq!(final_job.message.as_deref(), Some("Not checked: stopped"));
            assert_eq!(final_job.results[1].outcome, CheckOutcome::Stopped);
            assert_eq!(f.fake.stops.lock().unwrap().len(), 1);
            f.reserved(&job, false);
        } else {
            assert_eq!(final_job.state, CheckJobState::Uncertain);
            assert!(final_job.finished_at.is_none());
            f.reserved(&job, true);
        }
        f.close().await;
    }
}

#[tokio::test]
async fn cancellation_of_persisted_process_requires_confirmed_stop() {
    for status in [
        StopStatus::Confirmed,
        StopStatus::Uncertain {
            reason: "identity cannot be verified".into(),
        },
    ] {
        let f = ExecutionFixture::new().await;
        *f.fake.stop_status.lock().unwrap() = status.clone();
        let job = f.seed(
            vec![check("do not restart")],
            vec![],
            CheckJobState::Cancelling,
        );
        let process = identity(f.root.path().join("project"));
        f.record(
            &job,
            ProcessRecord {
                launching: false,
                identity: Some(process.clone()),
                partial: None,
            },
        );
        f.execute(&job).await;
        assert_eq!(*f.fake.stops.lock().unwrap(), vec![process]);
        assert!(f.fake.requests.lock().unwrap().is_empty());
        let uncertain = status != StopStatus::Confirmed;
        assert_eq!(
            f.job(job.id).state,
            if uncertain {
                CheckJobState::Uncertain
            } else {
                CheckJobState::Finished
            }
        );
        f.reserved(&job, uncertain);
        f.close().await;
    }
}

#[tokio::test]
async fn restart_with_stopped_process_replays_results_but_preserves_off_and_skipped() {
    let f = ExecutionFixture::new().await;
    let checks = vec![
        check("passed previously"),
        check("failed previously"),
        check("off"),
        check("skipped"),
    ];
    let results = checks
        .iter()
        .zip([
            CheckOutcome::Passed,
            CheckOutcome::Failed,
            CheckOutcome::Off,
            CheckOutcome::SkippedPaths,
        ])
        .map(|(check, outcome)| CheckResult {
            outcome,
            duration_secs: 9.0,
            ..failed_start(check, "")
        })
        .collect();
    let job = f.seed(checks, results, CheckJobState::Running);
    let process = identity(f.root.path().join("project"));
    f.record(
        &job,
        ProcessRecord {
            launching: false,
            identity: Some(process.clone()),
            partial: None,
        },
    );
    f.hub.recover_check_jobs().await.unwrap();
    let recovered = f.job(job.id);
    assert_eq!(recovered.state, CheckJobState::Pending);
    assert_eq!(recovered.results, job.results[2..]);
    assert!(recovered.started_at.is_none());
    assert!(recovered.finished_at.is_none());
    assert!(recovered.process.is_none());
    assert!(
        f.hub
            .read(|r| r.get::<ProcessRecord>(PROCESSES, job.id))
            .unwrap()
            .is_none()
    );
    assert_eq!(*f.fake.inspections.lock().unwrap(), vec![process]);
    f.reserved(&job, true);
    f.fake.push(
        runner::CheckOutcome::Passed,
        1000,
        None,
        StopStatus::Confirmed,
    );
    f.fake.push(
        runner::CheckOutcome::Passed,
        2000,
        None,
        StopStatus::Confirmed,
    );
    f.execute(&recovered).await;
    assert_eq!(f.fake.requests.lock().unwrap().len(), 2);
    assert_eq!(f.job(job.id).state, CheckJobState::Finished);
    f.reserved(&job, false);
    f.close().await;
}

#[tokio::test]
async fn restart_never_assumes_running_unknown_or_missing_identities_are_stopped() {
    for case in 0..5 {
        let f = ExecutionFixture::new().await;
        let mut job = f.seed(vec![check("never repeat")], vec![], CheckJobState::Running);
        let process = identity(f.root.path().join("project"));
        match case {
            0 | 1 => {
                *f.fake.inspect_state.lock().unwrap() = if case == 0 {
                    ProcessState::Running
                } else {
                    ProcessState::Uncertain {
                        reason: "cannot inspect".into(),
                    }
                };
                f.record(
                    &job,
                    ProcessRecord {
                        launching: false,
                        identity: Some(process.clone()),
                        partial: None,
                    },
                );
            }
            2 => {
                // The public identity lacks boot/project data and is insufficient.
                job.process = Some(wire_identity(&process));
                f.hub.write(|tx| tx.put_check_job(&job)).unwrap();
            }
            3 => f.record(
                &job,
                ProcessRecord {
                    launching: true,
                    identity: None,
                    partial: None,
                },
            ),
            4 => f.record(
                &job,
                ProcessRecord {
                    launching: false,
                    identity: None,
                    partial: None,
                },
            ),
            _ => unreachable!(),
        }
        f.hub.recover_check_jobs().await.unwrap();
        assert_eq!(
            f.job(job.id).state,
            CheckJobState::Uncertain,
            "recovery case {case}"
        );
        assert_eq!(
            f.fake.inspections.lock().unwrap().len(),
            usize::from(case < 2)
        );
        assert!(f.fake.requests.lock().unwrap().is_empty());
        f.reserved(&job, true);
        f.close().await;
    }
}

#[tokio::test]
async fn startup_recovery_guard_prevents_writes_from_dispatching_other_pending_jobs() {
    let f = ExecutionFixture::new().await;
    let job = f.seed(
        vec![check("recover before running")],
        vec![],
        CheckJobState::Running,
    );
    f.record(
        &job,
        ProcessRecord {
            launching: false,
            identity: Some(identity(f.root.path().join("project"))),
            partial: None,
        },
    );
    let gate = Arc::new(Semaphore::new(0));
    *f.fake.inspect_gate.lock().unwrap() = Some(gate.clone());
    let recover = f.hub.recover_check_jobs();
    let observe = async {
        until(|| !f.fake.inspections.lock().unwrap().is_empty()).await;
        assert!(f.hub.inner.checks.recovering.load(Ordering::Acquire));
        f.hub.inner.checks.paused.store(false, Ordering::Release);
        f.hub.wake_checks();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(f.fake.requests.lock().unwrap().is_empty());
        assert!(!f.hub.inner.checks.scheduling.load(Ordering::Acquire));
        f.hub.inner.checks.paused.store(true, Ordering::Release);
        gate.add_permits(1);
    };
    let (result, ()) = tokio::time::timeout(WAIT, async { tokio::join!(recover, observe) })
        .await
        .unwrap();
    result.unwrap();
    assert!(!f.hub.inner.checks.recovering.load(Ordering::Acquire));
    assert_eq!(f.job(job.id).state, CheckJobState::Pending);
    f.close().await;
}

#[tokio::test]
async fn only_passing_runs_update_the_approved_typical_duration() {
    let f = ExecutionFixture::new().await;
    let passing = check("passing measurement");
    let failing = check("failing measurement");
    f.hub
        .write(|tx| {
            tx.change_checks(
                f.project,
                0,
                CheckEdit::Save {
                    check: passing.clone(),
                },
                None,
                &f.root.path().join("project"),
            )?;
            tx.change_checks(
                f.project,
                1,
                CheckEdit::Save {
                    check: failing.clone(),
                },
                None,
                &f.root.path().join("project"),
            )?;
            Ok(())
        })
        .unwrap();
    let job = f.seed(
        vec![passing.clone(), failing.clone()],
        vec![],
        CheckJobState::Running,
    );
    assert_eq!(job.checks_version, 2);
    f.fake.push(
        runner::CheckOutcome::Passed,
        3750,
        None,
        StopStatus::Confirmed,
    );
    f.fake.push(
        runner::CheckOutcome::Failed,
        8250,
        None,
        StopStatus::Confirmed,
    );
    f.execute(&job).await;
    let approved = f
        .hub
        .read(|r| r.project_checks(f.project))
        .unwrap()
        .unwrap();
    assert_eq!(
        approved
            .checks
            .iter()
            .find(|c| c.check_id == passing.check_id)
            .unwrap()
            .typical_secs,
        Some(3.75)
    );
    assert_eq!(
        approved
            .checks
            .iter()
            .find(|c| c.check_id == failing.check_id)
            .unwrap()
            .typical_secs,
        None
    );
    f.close().await;
}

#[tokio::test]
async fn restart_leaves_an_untouched_pending_job_ready_without_inspection() {
    let f = ExecutionFixture::new().await;
    let mut job = f.seed(
        vec![check("not launched yet")],
        vec![],
        CheckJobState::Pending,
    );
    job.started_at = None;
    f.hub.write(|tx| tx.put_check_job(&job)).unwrap();
    f.hub.recover_check_jobs().await.unwrap();
    assert_eq!(f.job(job.id), job);
    assert!(f.fake.inspections.lock().unwrap().is_empty());
    assert!(f.fake.requests.lock().unwrap().is_empty());
    f.reserved(&job, true);
    f.close().await;
}

#[tokio::test]
async fn prelaunch_timeout_records_timed_out_without_a_process_and_releases() {
    let f = ExecutionFixture::new().await;
    let first = check("must never launch");
    let job = f.seed(vec![first.clone()], vec![], CheckJobState::Running);
    f.hub
        .write(|tx| {
            tx.put(
                "check_job_inputs",
                job.id,
                &BTreeMap::from([(first.check_id, vec!["package.json".to_string()])]),
            )
        })
        .unwrap();
    f.fake
        .start_errors
        .lock()
        .unwrap()
        .push_back(runner::StartError::TimedOutBeforeStart { duration_ms: 1_250 });
    f.execute(&job).await;
    let finished = f.job(job.id);
    assert_eq!(finished.state, CheckJobState::Finished);
    assert!(finished.process.is_none());
    assert!(finished.sandbox.is_none());
    assert_eq!(finished.results.len(), 1);
    let result = &finished.results[0];
    assert_eq!(result.outcome, CheckOutcome::TimedOut);
    assert_eq!(result.duration_secs, 1.25);
    assert_eq!(result.exit_code, None);
    assert!(result.output.contains("No check command was started"));
    assert_eq!(result.inputs_changed, vec!["package.json"]);
    assert!(
        f.hub
            .read(|r| r.get::<ProcessRecord>(PROCESSES, job.id))
            .unwrap()
            .is_none()
    );
    assert_eq!(f.fake.requests.lock().unwrap().len(), 1);
    assert!(f.fake.inspections.lock().unwrap().is_empty());
    assert!(f.fake.stops.lock().unwrap().is_empty());
    assert_eq!(f.fake.cancelled.load(Ordering::SeqCst), 0);
    f.reserved(&job, false);
    f.close().await;
}

#[tokio::test]
async fn confirmed_failed_start_records_could_not_run_then_continues_and_releases() {
    let f = ExecutionFixture::new().await;
    let first = check("cannot start");
    let second = check("still run this check");
    let job = f.seed(vec![first.clone(), second], vec![], CheckJobState::Running);
    f.hub
        .write(|tx| {
            tx.put(
                "check_job_inputs",
                job.id,
                &BTreeMap::from([(first.check_id, vec!["package.json".to_string()])]),
            )
        })
        .unwrap();
    f.fake
        .start_errors
        .lock()
        .unwrap()
        .push_back(runner::StartError::NotStarted {
            reason: "sandbox unavailable before spawning".into(),
        });
    f.fake.push(
        runner::CheckOutcome::Passed,
        1500,
        None,
        StopStatus::Confirmed,
    );
    f.execute(&job).await;
    let finished = f.job(job.id);
    assert_eq!(finished.state, CheckJobState::Finished);
    assert_eq!(finished.results.len(), 2);
    assert!(matches!(
        finished.results[0].outcome,
        CheckOutcome::CouldNotRun { .. }
    ));
    assert_eq!(finished.results[0].duration_secs, 0.0);
    assert_eq!(finished.results[0].inputs_changed, vec!["package.json"]);
    assert_eq!(finished.results[1].outcome, CheckOutcome::Passed);
    assert_eq!(f.fake.requests.lock().unwrap().len(), 2);
    f.reserved(&job, false);
    f.close().await;
}

#[tokio::test]
async fn uncertain_failed_start_preserves_partial_identity_and_reserves_the_project() {
    for complete in [false, true] {
        let f = ExecutionFixture::new().await;
        let job = f.seed(
            vec![check("ambiguous launch"), check("must wait")],
            vec![],
            CheckJobState::Running,
        );
        let process = identity(f.root.path().join("project"));
        let partial = runner::PartialProcessIdentity {
            pid: Some(process.pid),
            start_time: complete.then_some(process.start_time),
            sandbox_unit: process.sandbox_unit.clone(),
            project: process.project.clone(),
            boot_id: complete.then_some(process.boot_id.clone()),
        };
        f.fake
            .start_errors
            .lock()
            .unwrap()
            .push_back(runner::StartError::Uncertain {
                reason: "cleanup could not be confirmed".into(),
                identity: Box::new(partial.clone()),
            });
        f.execute(&job).await;
        let uncertain = f.job(job.id);
        assert_eq!(uncertain.state, CheckJobState::Uncertain);
        assert!(uncertain.finished_at.is_none());
        assert_eq!(uncertain.results.len(), 1);
        assert!(matches!(
            uncertain.results[0].outcome,
            CheckOutcome::CouldNotRun { .. }
        ));
        assert_eq!(f.fake.requests.lock().unwrap().len(), 1);
        let record: ProcessRecord = f.hub.read(|r| r.require(PROCESSES, job.id)).unwrap();
        assert!(!record.launching);
        assert_eq!(record.partial, Some(partial));
        assert_eq!(record.identity, complete.then_some(process));
        f.reserved(&job, true);
        f.close().await;
    }
}

#[tokio::test]
async fn task_with_missing_check_identity_needs_explicit_resolution_even_after_stop_and_restart() {
    let f = ExecutionFixture::new().await;
    let (task_id, run_id, mut job) = f.checking_task().await;
    job.state = CheckJobState::Running;
    f.hub.write(|tx| tx.put_check_job(&job)).unwrap();
    // A saved launch intent, unlike no record at all, means a process may
    // have started before its full identity could be recorded.
    f.record(
        &job,
        ProcessRecord {
            launching: true,
            identity: None,
            partial: None,
        },
    );
    let f = f.reopened().await;
    assert_eq!(f.job(job.id).state, CheckJobState::Uncertain);
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.state, TaskState::ReconciliationRequired);
    f.reserved(&job, true);

    let mut stop = CommandEnvelope::new(Command::StopTask {
        task_id,
        force: false,
    });
    stop.expected_revision = Some(task.revision);
    f.hub.submit(&Caller::local_owner(), stop).await;
    assert_eq!(f.job(job.id).state, CheckJobState::Uncertain);
    f.reserved(&job, true);
    assert!(
        f.hub
            .read(|r| r.get::<serde_json::Value>("check_human_resolution", job.id))
            .unwrap()
            .is_none()
    );

    let f = f.reopened().await;
    assert_eq!(f.job(job.id).state, CheckJobState::Uncertain);
    f.reserved(&job, true);
    let owner = Caller::local_owner();
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let mut resolve = CommandEnvelope::new(Command::ReconcileTask {
        task_id,
        resolution: Reconciliation::MarkCompleted,
    });
    resolve.expected_revision = Some(task.revision);
    let receipt = f.hub.submit(&owner, resolve).await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    let finished = f.job(job.id);
    assert_eq!(finished.state, CheckJobState::Finished);
    assert_eq!(finished.results.len(), 1);
    assert!(
        matches!(&finished.results[0].outcome, CheckOutcome::CouldNotRun { reason } if reason.contains("person resolved"))
    );
    f.hub
        .read(|r| {
            let task: Task = r.require("task", task_id)?;
            let run: Run = r.require("run", run_id)?;
            assert_eq!(task.state, TaskState::Completed);
            assert!(task.checking.is_none());
            assert!(!run.holds_writer);
            assert!(!run.cleanup_pending);
            let human = run.human_resolution.unwrap();
            assert_eq!(human.by, owner.principal);
            assert_eq!(human.resolution, Reconciliation::MarkCompleted);
            let recorded: HumanResolution = r.require("check_human_resolution", job.id)?;
            assert_eq!(recorded.by, owner.principal);
            assert!(
                r.get::<serde_json::Value>(crate::check_lifecycle::DEFERRED_END, task_id)?
                    .is_none()
            );
            assert!(
                r.task_attention::<AttentionItem>(task_id)?
                    .iter()
                    .all(|item| item.kind != AttentionKind::ReconciliationRequired
                        || item.resolved_at.is_some())
            );
            Ok(())
        })
        .unwrap();
    assert!(f.fake.requests.lock().unwrap().is_empty());
    assert!(f.fake.inspections.lock().unwrap().is_empty());
    f.reserved(&job, false);
    f.close().await;
}

#[tokio::test]
async fn task_reconciliation_refuses_running_complete_and_partial_only_check_identities() {
    for partial_only in [false, true] {
        let f = ExecutionFixture::new().await;
        let (task_id, run_id, job) = f.checking_task().await;
        let process = identity(f.root.path().join("project"));
        f.record(
            &job,
            ProcessRecord {
                launching: false,
                identity: (!partial_only).then_some(process.clone()),
                partial: partial_only.then_some(runner::PartialProcessIdentity {
                    pid: Some(process.pid),
                    start_time: Some(process.start_time),
                    sandbox_unit: process.sandbox_unit.clone(),
                    project: process.project.clone(),
                    boot_id: Some(process.boot_id.clone()),
                }),
            },
        );
        f.hub
            .mark_check_job_uncertain(job.id, "This check may still be running.")
            .unwrap();
        *f.fake.inspect_state.lock().unwrap() = ProcessState::Running;
        let before: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        let before_job = f.job(job.id);
        let mut resolve = CommandEnvelope::new(Command::ReconcileTask {
            task_id,
            resolution: Reconciliation::MarkCompleted,
        });
        resolve.expected_revision = Some(before.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), resolve).await;
        assert_eq!(
            receipt
                .error
                .expect("a running check cannot be resolved")
                .code,
            ErrorCode::OutcomeUnknown
        );
        let after: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        assert_eq!(
            serde_json::to_value(&after).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
        assert_eq!(f.job(job.id), before_job);
        assert_eq!(*f.fake.inspections.lock().unwrap(), vec![process]);
        assert!(f.fake.requests.lock().unwrap().is_empty());
        f.hub
            .read(|r| {
                assert!(r.require::<Run>("run", run_id)?.human_resolution.is_none());
                assert!(
                    r.get::<serde_json::Value>("check_human_resolution", job.id)?
                        .is_none()
                );
                assert!(
                    r.get::<serde_json::Value>(crate::check_lifecycle::DEFERRED_END, task_id)?
                        .is_some()
                );
                Ok(())
            })
            .unwrap();
        f.reserved(&job, true);
        f.close().await;
    }
}

#[tokio::test]
async fn uncertain_job_requires_person_confirmation_and_records_unchecked_resolution() {
    let f = ExecutionFixture::new().await;
    let job = f.seed(
        vec![check("never observed")],
        vec![],
        CheckJobState::Uncertain,
    );
    let owner = Caller::local_owner();
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::ChecksStopJob {
                job_id: job.id,
                confirmed_stopped: false,
            }),
        )
        .await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    assert_eq!(f.job(job.id).state, CheckJobState::Uncertain);
    f.reserved(&job, true);
    assert!(
        f.hub
            .read(|r| r.get::<serde_json::Value>("check_human_resolution", job.id))
            .unwrap()
            .is_none()
    );

    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::ChecksStopJob {
                job_id: job.id,
                confirmed_stopped: true,
            }),
        )
        .await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    let resolved = f.job(job.id);
    assert_eq!(resolved.state, CheckJobState::Finished);
    assert!(resolved.finished_at.is_some());
    assert!(
        matches!(&resolved.results[0].outcome, CheckOutcome::CouldNotRun { reason } if reason.contains("person resolved"))
    );
    let record: serde_json::Value = f
        .hub
        .read(|r| r.require("check_human_resolution", job.id))
        .unwrap();
    assert_eq!(record["by"], serde_json::to_value(owner.principal).unwrap());
    assert_eq!(record["confirmed_stopped"], true);
    assert!(f.fake.requests.lock().unwrap().is_empty());
    assert!(f.fake.inspections.lock().unwrap().is_empty());
    f.reserved(&job, false);
    f.close().await;
}

#[tokio::test]
async fn confirmed_resolution_refuses_running_complete_and_partial_only_identities() {
    for partial_only in [false, true] {
        let f = ExecutionFixture::new().await;
        let job = f.seed(vec![check("still alive")], vec![], CheckJobState::Uncertain);
        let process = identity(f.root.path().join("project"));
        *f.fake.inspect_state.lock().unwrap() = ProcessState::Running;
        f.record(
            &job,
            ProcessRecord {
                launching: false,
                identity: (!partial_only).then_some(process.clone()),
                partial: partial_only.then_some(runner::PartialProcessIdentity {
                    pid: Some(process.pid),
                    start_time: Some(process.start_time),
                    sandbox_unit: process.sandbox_unit.clone(),
                    project: process.project.clone(),
                    boot_id: Some(process.boot_id.clone()),
                }),
            },
        );
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::ChecksStopJob {
                    job_id: job.id,
                    confirmed_stopped: true,
                }),
            )
            .await;
        assert_eq!(
            receipt
                .error
                .expect("running process must refuse resolution")
                .code,
            ErrorCode::OutcomeUnknown
        );
        assert_eq!(f.job(job.id).state, CheckJobState::Uncertain);
        assert_eq!(*f.fake.inspections.lock().unwrap(), vec![process]);
        assert!(
            f.hub
                .read(|r| r.get::<serde_json::Value>("check_human_resolution", job.id))
                .unwrap()
                .is_none()
        );
        f.reserved(&job, true);
        f.close().await;
    }
}

#[tokio::test]
async fn restart_inspects_complete_partial_only_identity_before_replay() {
    for stopped in [false, true] {
        let f = ExecutionFixture::new().await;
        let job = f.seed(
            vec![check("inspect before replay")],
            vec![],
            CheckJobState::Running,
        );
        let process = identity(f.root.path().join("project"));
        *f.fake.inspect_state.lock().unwrap() = if stopped {
            ProcessState::Stopped
        } else {
            ProcessState::Running
        };
        f.record(
            &job,
            ProcessRecord {
                launching: true,
                identity: None,
                partial: Some(runner::PartialProcessIdentity {
                    pid: Some(process.pid),
                    start_time: Some(process.start_time),
                    sandbox_unit: process.sandbox_unit.clone(),
                    project: process.project.clone(),
                    boot_id: Some(process.boot_id.clone()),
                }),
            },
        );
        f.hub.recover_check_jobs().await.unwrap();
        assert_eq!(*f.fake.inspections.lock().unwrap(), vec![process]);
        assert_eq!(
            f.job(job.id).state,
            if stopped {
                CheckJobState::Pending
            } else {
                CheckJobState::Uncertain
            }
        );
        assert!(f.fake.requests.lock().unwrap().is_empty());
        f.reserved(&job, true);
        f.close().await;
    }
}

#[tokio::test]
async fn requests_intersect_current_and_saved_project_toolchain_and_network_grants() {
    let f = ExecutionFixture::new().await;
    let mut job = f.seed(vec![check("frozen access")], vec![], CheckJobState::Running);
    let task_id = f.queued_task().await;
    job.task_id = Some(task_id);
    let parent = ports::library::canonical(&f.root.path().join("home/.cargo/bin")).unwrap();
    let child = parent.join("selected");
    std::fs::create_dir_all(&child).unwrap();
    let parent_id = ProjectId::new();
    let child_id = ProjectId::new();
    f.hub
        .write(|tx| {
            let template: Project = tx.read().require("project", f.project)?;
            for (project_id, path) in [(parent_id, &parent), (child_id, &child)] {
                let mut project = template.clone();
                project.project_id = project_id;
                project.host_path = path.to_string_lossy().into_owned();
                project.active_writer = None;
                tx.put("project", project_id, &project)?;
            }
            Ok(())
        })
        .unwrap();
    for case in [
        "both",
        "reverse-subtree",
        "current-no-read",
        "saved-no-read",
        "current-no-local",
        "saved-no-local",
        "current-offline",
        "saved-offline",
        "definition-offline",
        "current-read-only",
        "saved-read-only",
        "missing-snapshot",
    ] {
        let mut current = ConversationAccess {
            read: vec![parent_id],
            write: vec![f.project],
            internet: true,
            local_network: true,
            ..Default::default()
        };
        let mut saved = ConversationAccess {
            read: vec![child_id],
            ..current.clone()
        };
        let mut definition = job.checks[0].clone();
        match case {
            "reverse-subtree" => {
                current.read = vec![child_id];
                saved.read = vec![parent_id];
            }
            "current-no-read" => current.read.clear(),
            "saved-no-read" => saved.read.clear(),
            "current-no-local" => current.local_network = false,
            "saved-no-local" => saved.local_network = false,
            "current-offline" => current.internet = false,
            "saved-offline" => saved.internet = false,
            "definition-offline" => definition.network = false,
            "current-read-only" => current.write.clear(),
            "saved-read-only" => saved.write.clear(),
            _ => {}
        }
        let run_id = RunId::new();
        job.run_id = Some(run_id);
        f.hub
            .write(|tx| {
                let mut task: Task = tx.read().require("task", task_id)?;
                task.access = Some(current.clone());
                tx.put("task", task_id, &task)?;
                if case != "missing-snapshot" {
                    tx.put("run_access", run_id, &Some(saved.clone()))?;
                }
                Ok(())
            })
            .unwrap();
        let result = f.hub.check_request(&job, &definition);
        if matches!(
            case,
            "current-read-only" | "saved-read-only" | "missing-snapshot"
        ) {
            assert_eq!(
                result.unwrap_err().code,
                ErrorCode::PermissionDenied,
                "{case}"
            );
            continue;
        }
        let request = result.unwrap();
        assert_eq!(
            request.network,
            matches!(
                case,
                "both"
                    | "reverse-subtree"
                    | "current-no-read"
                    | "saved-no-read"
                    | "current-no-local"
                    | "saved-no-local"
            ),
            "{case}"
        );
        assert!(request.task_network_restricted, "{case}");
        assert_eq!(
            request.local_network,
            current.internet && saved.internet && current.local_network && saved.local_network,
            "{case}"
        );
        let expected = if matches!(case, "current-no-read" | "saved-no-read") {
            vec![]
        } else {
            vec![child.clone()]
        };
        assert_eq!(request.read_toolchains, expected, "{case}");
        assert_eq!(request.command, definition.run);
    }
    assert!(f.fake.requests.lock().unwrap().is_empty());
    f.close().await;
}

#[tokio::test]
async fn standalone_checks_use_work_toolchain_grants_but_not_private_home_files() {
    let f = ExecutionFixture::new().await;
    let home = f.root.path().join("home");
    let node = home.join(".nvm/versions/node/test/bin");
    std::fs::create_dir_all(&node).unwrap();
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/private"), "private key").unwrap();
    std::fs::write(home.join(".cargo/credentials.toml"), "private token").unwrap();
    std::fs::write(home.join("private.txt"), "private document").unwrap();
    let definition = check("approved standalone command");
    let job = f.seed(vec![definition.clone()], vec![], CheckJobState::Running);
    let ordinary = crate::access::launch_access(None);
    let reach = f.hub.read(|r| f.hub.access_reach(r, &ordinary)).unwrap();
    for purpose in [
        job.purpose.clone(),
        CheckJobPurpose::DraftProof {
            draft_id: CheckDraftId::new(),
        },
    ] {
        let job = CheckJob {
            purpose,
            ..job.clone()
        };
        for network in [false, true] {
            let check = Check {
                network,
                ..definition.clone()
            };
            let request = f.hub.check_request(&job, &check).unwrap();
            assert_eq!(request.network, network && reach.internet);
            assert!(!request.task_network_restricted);
            assert!(request.local_network);
            assert!(request.read_toolchains.contains(&home.join(".cargo/bin")));
            assert!(
                request
                    .read_toolchains
                    .contains(&home.join(".nvm/versions"))
            );
            for private in [
                home.join(".cargo/credentials.toml"),
                home.join(".ssh/private"),
                home.join("private.txt"),
            ] {
                assert!(
                    !request
                        .read_toolchains
                        .iter()
                        .any(|allowed| private.starts_with(allowed)),
                    "private file exposed: {private:?}"
                );
            }
            assert_eq!(request.command, check.run);
        }
    }
    f.close().await;
}

#[test]
fn intentionally_unfiltered_network_is_described_as_permission_not_failure() {
    let level = sandbox_level(&runner::SandboxLevel::ContainedWithoutNetworkLimit);
    let CheckSandboxLevel::ContainedWithoutNetworkLimit { reason } = level else {
        panic!("unexpected sandbox level");
    };
    assert!(reason.contains("was allowed to use the internet"));
    assert!(reason.contains("without limiting which destinations"));
    assert!(!reason.contains("could not"));
}

fn assert_completed_without_uncertainty(f: &ExecutionFixture, task_id: TaskId) {
    f.hub
        .read(|r| {
            let task: Task = r.require("task", task_id)?;
            assert_eq!(task.state, TaskState::Completed);
            assert!(task.checking.is_none());
            assert!(
                r.task_check_jobs(task_id)?
                    .iter()
                    .all(|job| job.state == CheckJobState::Finished)
            );
            assert!(
                !r.list::<AttentionItem>("attention")?
                    .iter()
                    .any(|item| item.task_id == Some(task_id)
                        && item.kind == AttentionKind::ReconciliationRequired
                        && item.resolved_at.is_none())
            );
            let project: Project = r.require("project", f.project)?;
            let queue: QueueView = r.require("queue", f.project)?;
            assert!(project.active_writer.is_none());
            assert!(queue.writer.is_none());
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn restart_during_stop_finishes_waiting_proof_without_false_uncertainty() {
    let f = ExecutionFixture::new().await;
    let (task_id, _, after) = f.checking_task_with_proof(true).await;
    f.record(
        &after,
        ProcessRecord {
            launching: false,
            identity: Some(identity(f.root.path().join("project"))),
            partial: None,
        },
    );
    f.hub
        .write(|tx| {
            for mut job in tx.read().task_check_jobs(task_id)? {
                job.state = CheckJobState::Cancelling;
                tx.put_check_job(&job)?;
            }
            Ok(())
        })
        .unwrap();
    // Startup scans both Cancelling jobs. Finishing the first also finishes the
    // never-started proof, so its later scan entry must not resurrect it.
    f.hub.recover_check_jobs().await.unwrap();
    assert_completed_without_uncertainty(&f, task_id);
    let draft = f
        .hub
        .read(|r| r.task_checks_draft(task_id))
        .unwrap()
        .unwrap();
    assert_eq!(draft.state, ChecksDraftState::Ready);
    assert_eq!(draft.proofs[0].outcome, CheckOutcome::Stopped);
    assert!(f.fake.requests.lock().unwrap().is_empty());
    f.close().await;
}

#[tokio::test]
async fn restart_finishes_never_launched_running_and_cancelling_jobs_as_stopped() {
    for state in [CheckJobState::Running, CheckJobState::Cancelling] {
        let f = ExecutionFixture::new().await;
        let job = f.seed(vec![check("never launched")], vec![], state);
        f.hub.recover_check_jobs().await.unwrap();
        let finished = f.job(job.id);
        assert_eq!(finished.state, CheckJobState::Finished);
        assert_eq!(finished.results[0].outcome, CheckOutcome::Stopped);
        assert_eq!(finished.message.as_deref(), Some("Not checked: stopped"));
        f.reserved(&job, false);
        assert!(f.fake.inspections.lock().unwrap().is_empty());
        f.close().await;
    }
}

#[tokio::test]
async fn scheduling_snapshot_cannot_turn_a_finished_job_uncertain() {
    let f = ExecutionFixture::new().await;
    let job = f.seed(vec![check("just finished")], vec![], CheckJobState::Running);
    f.hub.inner.checks.paused.store(false, Ordering::Release);
    let hub = f.hub.clone();
    let id = job.id;
    *f.hub.inner.checks.snapshot_hook.lock().unwrap() = Some(Box::new(move || {
        hub.finish_check_job(id, false, true).unwrap();
    }));
    f.hub.schedule_check_jobs().unwrap();
    assert_eq!(f.job(job.id).state, CheckJobState::Finished);
    f.reserved(&job, false);
    assert!(f.fake.requests.lock().unwrap().is_empty());
    f.close().await;
}

#[tokio::test]
async fn recovery_inspection_cannot_overwrite_a_job_finished_while_awaiting() {
    let f = ExecutionFixture::new().await;
    let job = f.seed(
        vec![check("finished during inspection")],
        vec![],
        CheckJobState::Running,
    );
    f.record(
        &job,
        ProcessRecord {
            launching: false,
            identity: Some(identity(f.root.path().join("project"))),
            partial: None,
        },
    );
    *f.fake.inspect_state.lock().unwrap() = ProcessState::Running;
    let gate = Arc::new(Semaphore::new(0));
    *f.fake.inspect_gate.lock().unwrap() = Some(gate.clone());
    let recover = f.hub.recover_check_jobs();
    let finish = async {
        until(|| !f.fake.inspections.lock().unwrap().is_empty()).await;
        f.hub.finish_check_job(job.id, true, true).unwrap();
        gate.add_permits(1);
    };
    let (result, ()) = tokio::join!(recover, finish);
    result.unwrap();
    assert_eq!(f.job(job.id).state, CheckJobState::Finished);
    f.reserved(&job, false);
    f.close().await;
}

#[tokio::test]
async fn live_stop_as_after_run_finishes_also_finishes_the_drafts_proof() {
    let f = ExecutionFixture::new().await;
    let (task_id, _, _) = f.checking_task_with_proof(true).await;
    let gate = Arc::new(Semaphore::new(0));
    f.fake.push(
        runner::CheckOutcome::Passed,
        1000,
        Some(gate.clone()),
        StopStatus::Confirmed,
    );
    f.hub.inner.checks.paused.store(false, Ordering::Release);
    f.hub
        .inner
        .checks
        .recovering
        .store(false, Ordering::Release);
    f.hub.wake_checks();
    until(|| f.fake.requests.lock().unwrap().len() == 1).await;
    // Both events are live: Stop commits while the executor returns its result.
    let stop = f.hub.stop_task_checks(task_id);
    let result = async {
        gate.add_permits(1);
    };
    let (stopped, ()) = tokio::time::timeout(WAIT, async { tokio::join!(stop, result) })
        .await
        .unwrap();
    assert!(stopped.unwrap());
    assert_completed_without_uncertainty(&f, task_id);
    let draft = f
        .hub
        .read(|r| r.task_checks_draft(task_id))
        .unwrap()
        .unwrap();
    assert_eq!(draft.state, ChecksDraftState::Ready);
    assert_eq!(draft.proofs[0].outcome, CheckOutcome::Stopped);
    assert_eq!(f.fake.requests.lock().unwrap().len(), 1);
    f.close().await;
}

#[tokio::test]
async fn confirmed_stop_restores_a_task_completed_before_false_uncertainty() {
    for proof in [false, true] {
        let f = ExecutionFixture::new().await;
        let (task_id, _, after) = f.checking_task_with_proof(proof).await;
        f.hub.finish_check_job(after.id, true, true).unwrap();
        assert_completed_without_uncertainty(&f, task_id);
        let mut job = f
            .hub
            .read(|r| r.task_check_jobs(task_id))
            .unwrap()
            .into_iter()
            .find(|job| matches!(job.purpose, CheckJobPurpose::DraftProof { .. }) == proof)
            .unwrap();
        // Reproduce an old companion's incorrect persisted recovery state.
        f.hub
            .write(|tx| {
                job.state = CheckJobState::Running;
                tx.put_check_job(&job)
            })
            .unwrap();
        f.hub
            .mark_check_job_uncertain(job.id, "Old recovery could not account for this job.")
            .unwrap();
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::ChecksStopJob {
                    job_id: job.id,
                    confirmed_stopped: true,
                }),
            )
            .await;
        assert!(receipt.error.is_none(), "{receipt:?}");
        assert_completed_without_uncertainty(&f, task_id);
        f.close().await;
    }
}

#[tokio::test]
async fn final_handover_retries_a_failed_store_transaction_without_restart() {
    let f = ExecutionFixture::new().await;
    let (task_id, _, job) = f.checking_task().await;
    f.hub
        .inner
        .checks
        .finalization_failures
        .store(1, Ordering::Release);
    f.hub.finish_check_job(job.id, false, true).unwrap();
    assert_completed_without_uncertainty(&f, task_id);
    assert_eq!(
        f.hub
            .inner
            .checks
            .finalization_failures
            .load(Ordering::Acquire),
        0
    );
    f.close().await;
}

#[tokio::test]
async fn failed_final_handover_can_be_finished_by_stop_or_next_scheduling_pass() {
    for stop in [false, true] {
        let f = ExecutionFixture::new().await;
        let (task_id, run_id, job) = f.checking_task().await;
        f.hub
            .inner
            .checks
            .finalization_failures
            .store(2, Ordering::Release);
        assert!(f.hub.finish_check_job(job.id, false, true).is_err());
        assert_eq!(f.job(job.id).state, CheckJobState::Finished);
        f.hub
            .read(|r| {
                assert!(r.require::<bool>(crate::check_lifecycle::FINALIZING, run_id)?);
                assert!(r.require::<Task>("task", task_id)?.checking.is_some());
                assert_eq!(
                    r.require::<Project>("project", f.project)?.active_writer,
                    Some(run_id)
                );
                Ok(())
            })
            .unwrap();
        if stop {
            assert!(
                tokio::time::timeout(WAIT, f.hub.stop_task_checks(task_id))
                    .await
                    .unwrap()
                    .unwrap()
            );
        } else {
            f.hub.schedule_check_jobs().unwrap();
        }
        assert_completed_without_uncertainty(&f, task_id);
        f.close().await;
    }
}

#[tokio::test]
async fn stop_between_after_run_result_and_handover_finishes_the_pending_proof() {
    let f = ExecutionFixture::new().await;
    let (task_id, _, after) = f.checking_task_with_proof(true).await;
    // The after-run worker committed its final result, but has not handed its
    // reservation on yet. Stop therefore sees a Finished job and pending proof.
    f.hub.finish_check_job(after.id, false, false).unwrap();
    let stop = f.hub.stop_task_checks(task_id);
    let handover = async {
        f.hub.check_job_ended(after.id).unwrap();
    };
    let (stopped, ()) = tokio::time::timeout(WAIT, async { tokio::join!(stop, handover) })
        .await
        .unwrap();
    assert!(stopped.unwrap());
    assert_completed_without_uncertainty(&f, task_id);
    let draft = f
        .hub
        .read(|r| r.task_checks_draft(task_id))
        .unwrap()
        .unwrap();
    assert_eq!(draft.state, ChecksDraftState::Ready);
    assert_eq!(draft.proofs[0].outcome, CheckOutcome::Stopped);
    f.close().await;
}

#[tokio::test]
async fn finishing_uses_current_cancelling_state_even_if_worker_observed_running() {
    let f = ExecutionFixture::new().await;
    let job = f.seed(
        vec![check("stop won the final write")],
        vec![],
        CheckJobState::Cancelling,
    );
    // false is the worker's stale observation; the store already says Stop.
    f.hub.finish_check_job(job.id, false, true).unwrap();
    let finished = f.job(job.id);
    assert_eq!(finished.results[0].outcome, CheckOutcome::Stopped);
    assert_eq!(finished.message.as_deref(), Some("Not checked: stopped"));
    f.reserved(&job, false);
    f.close().await;
}

#[tokio::test]
async fn finished_job_awaiting_ownership_transfer_is_resumable_by_stop_or_scheduling() {
    for stop in [false, true] {
        let f = ExecutionFixture::new().await;
        let (task_id, _, job) = f.checking_task().await;
        // Durable state left if the first hand-over write fails: the job is
        // Finished but still owns the project, without a finalizing marker yet.
        f.hub.finish_check_job(job.id, false, false).unwrap();
        if stop {
            assert!(f.hub.stop_task_checks(task_id).await.unwrap());
        } else {
            f.hub.schedule_check_jobs().unwrap();
        }
        assert_completed_without_uncertainty(&f, task_id);
        f.close().await;
    }
}

#[tokio::test]
async fn startup_reconciliation_discards_prepared_end_journal_after_failed_completion() {
    let f = ExecutionFixture::new().await;
    let task_id = f.queued_task().await;
    let run_id = f
        .hub
        .write(|tx| {
            let mut task: Task = tx.read().require("task", task_id)?;
            let run_id = f
                .hub
                .new_run(tx, &mut task, CommandId::new(), None, None, false)?;
            tx.put("task", task_id, &task)?;
            Ok(run_id)
        })
        .unwrap();
    let journal = f
        .hub
        .inner
        .data
        .join("check-snapshots/prepared-ends")
        .join(format!("{run_id}.json"));
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(&journal, "snapshot journal left by failed completion").unwrap();
    let f = f.reopened().await;
    let run: Run = f.hub.read(|r| r.require("run", run_id)).unwrap();
    assert_eq!(run.state, RunState::ReconciliationRequired);
    assert!(
        !journal.exists(),
        "startup reconciliation must remove the abandoned journal"
    );
    f.close().await;
}
