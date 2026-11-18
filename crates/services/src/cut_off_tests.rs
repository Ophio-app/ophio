// SPDX-License-Identifier: Apache-2.0
//! A chat whose reply was cut off carries on with the person's next message,
//! once the computer knows nothing the old run started is still running. A
//! task keeps its review.
use super::conversation_access::{Backend, allow_ask, create};
use super::*;
use protocol::{access::ConversationAccess, capability::Availability};

/// A cgroup hierarchy the fake run says holds its processes, with the run's
/// group in it reporting whether anything is still inside.
struct Group {
    _root: tempfile::TempDir,
    dir: PathBuf,
}
impl Group {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("cgroup.controllers"), "cpu pids\n").unwrap();
        let dir = root.path().join("ophio-run-1");
        std::fs::create_dir(&dir).unwrap();
        let group = Self { _root: root, dir };
        group.populated(true);
        group
    }
    fn populated(&self, yes: bool) {
        let value = if yes { 1 } else { 0 };
        std::fs::write(
            self.dir.join("cgroup.events"),
            format!("populated {value}\nfrozen 0\n"),
        )
        .unwrap();
    }
}

async fn chat_fixture() -> Fixture {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    allow_ask(&f);
    f
}

/// A chat whose first reply has started, in session `native`.
async fn chat(f: &Fixture, native: &str) -> (TaskId, RunId) {
    let project = f.project().await;
    let access = ConversationAccess {
        read: vec![project],
        internet: false,
        ..Default::default()
    };
    let receipt = f
        .hub
        .submit(&Caller::local_owner(), create(project, access))
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let run = run_started(f, task_id).await;
    session(f, run, native).await;
    (task_id, run)
}

async fn session(f: &Fixture, run: RunId, native: &str) {
    f.harness
        .emit(
            run,
            ports::HarnessEvent::SessionStarted {
                native_session: native.into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    let mut more = waiting();
    while f
        .hub
        .read(|r| r.require::<Run>("run", run))
        .unwrap()
        .native_session
        .is_none()
    {
        assert!(more(), "session not recorded");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Øphio ends without settling its runs, as a crash or power cut leaves
/// them, and starts again.
async fn crashed(f: Fixture) -> Fixture {
    let f = stopped(f).await;
    allow_ask(&f);
    f
}

async fn stop_jobs(f: &Fixture) {
    let jobs = std::mem::take(&mut *f.hub.inner.jobs.lock().unwrap());
    for job in &jobs {
        job.abort();
    }
    for job in jobs {
        let _ = job.await;
    }
}

async fn stopped(f: Fixture) -> Fixture {
    stop_jobs(&f).await;
    // Do not call reopened(): it performs graceful shutdown, which saves the
    // very memory tail a hard-crash test must discard.
    let Fixture {
        path,
        hub,
        clock,
        harness,
        desktop,
    } = f;
    drop_hub_and_wait(hub, WAIT).await;
    let hub = Fixture::build(&path, &clock, &harness, &desktop, |b| {
        b.sandboxes(Arc::new(Backend::default()))
    })
    .await;
    Fixture {
        path,
        hub,
        clock,
        harness,
        desktop,
    }
}

#[tokio::test]
async fn a_simulated_crash_waits_for_a_blocking_worker_to_release_the_old_hub() {
    use std::{future::Future, task::Poll};

    let f = chat_fixture().await;
    stop_jobs(&f).await;
    let weak = Arc::downgrade(&f.hub.inner);
    let held = f.hub.clone();
    let (release, released) = std::sync::mpsc::channel();
    let worker = tokio::task::spawn_blocking(move || {
        // A running blocking worker cannot be cancelled with the async jobs.
        released.recv().unwrap();
        drop(held);
    });
    let restart = stopped(f);
    tokio::pin!(restart);
    let first_poll = std::future::poll_fn(|cx| Poll::Ready(restart.as_mut().poll(cx))).await;
    assert!(
        first_poll.is_pending(),
        "restart must wait for the old owner"
    );
    assert_eq!(weak.strong_count(), 1, "only the held worker remains");
    release.send(()).unwrap();
    worker.await.unwrap();
    let f = restart.await;
    assert_eq!(weak.strong_count(), 0);
    f.close().await;
}

fn run_of(f: &Fixture, id: RunId) -> Run {
    f.hub.read(|r| r.require("run", id)).unwrap()
}
fn task_of(f: &Fixture, id: TaskId) -> Task {
    f.hub.read(|r| r.require("task", id)).unwrap()
}
fn open_reviews(f: &Fixture, task: TaskId) -> usize {
    f.hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap()
        .iter()
        .filter(|a| {
            a.task_id == Some(task)
                && a.kind == AttentionKind::ReconciliationRequired
                && a.resolved_at.is_none()
        })
        .count()
}

fn reply_notices(f: &Fixture, run: RunId) -> Vec<AttentionItem> {
    f.hub
        .read(|r| r.list::<AttentionItem>("attention"))
        .unwrap()
        .into_iter()
        .filter(|a| a.run_id == Some(run) && a.kind == AttentionKind::Completed)
        .collect()
}

#[tokio::test]
async fn a_confirmed_cut_off_tells_once_as_a_reply_even_after_being_read_and_restarted() {
    use protocol::notifications::AlertType;
    for restart in [true, false] {
        let group = Group::new();
        let f = chat_fixture().await;
        *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
        *f.harness.controls.containment.lock().unwrap() = Some(ports::Containment::Complete);
        let (task, old) = chat(&f, "native-1").await;
        add_helper(
            &f,
            old,
            Some(ChildLifetime::SeparateProcess),
            ChildState::Active,
        );
        group.populated(false);
        let f = if restart {
            crashed(f).await
        } else {
            unknown_exit(&f, old).await;
            f
        };
        let notices = reply_notices(&f, old);
        assert_eq!(notices.len(), 1, "restart={restart}");
        let notice = &notices[0];
        assert_eq!(notice.title, "Reply cut off. Write to carry on.");
        assert_eq!(notice.task_id, Some(task));
        assert_eq!(AlertType::of(notice, true), Some(AlertType::Reply));
        assert_eq!(open_reviews(&f, task), 0);
        assert_eq!(task_of(&f, task).attention_count, 0);
        // Background phones receive the same headline through the existing alert route.
        let device = f.device().await;
        let (alerts, _) = f.hub.open_alerts(&device, true).await.unwrap();
        let alert = alerts
            .iter()
            .find(|a| a.attention_id == notice.attention_id)
            .unwrap();
        assert_eq!(alert.headline.as_deref(), Some(notice.title.as_str()));
        // Even unread, a second start keeps the identity rather than raising another.
        let f = crashed(f).await;
        assert_eq!(reply_notices(&f, old).len(), 1);
        assert_eq!(reply_notices(&f, old)[0].attention_id, notice.attention_id);
        let read = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::AcknowledgeAttention {
                    attention_id: notice.attention_id,
                }),
            )
            .await;
        assert_eq!(read.status, ReceiptStatus::Accepted, "{read:?}");
        assert!(reply_notices(&f, old)[0].resolved_at.is_some());
        let f = crashed(f).await;
        assert_eq!(reply_notices(&f, old).len(), 1);
        assert!(reply_notices(&f, old)[0].resolved_at.is_some());
        assert!(
            f.hub
                .open_alerts(&device, true)
                .await
                .unwrap()
                .0
                .iter()
                .all(|a| a.attention_id != notice.attention_id)
        );
        f.close().await;
    }
}

#[tokio::test]
async fn restarting_does_not_tell_about_an_old_already_carried_on_run() {
    let f = chat_fixture().await;
    *f.harness.controls.containment.lock().unwrap() = Some(ports::Containment::Complete);
    let (task, old) = chat(&f, "native-1").await;
    unknown_exit(&f, old).await;
    assert_eq!(
        reply(&f, task, "Carry on").await.status,
        ReceiptStatus::Accepted
    );
    let next = run_started(&f, task).await;
    complete(&f, next).await;
    // An old version carried this on before cut-off reply notices existed.
    let notices = reply_notices(&f, old);
    f.hub
        .write(|tx| {
            tx.delete("cut_off_told", old)?;
            for item in &notices {
                tx.delete("attention", item.attention_id)?;
            }
            Ok(())
        })
        .unwrap();
    let f = crashed(f).await;
    assert!(run_of(&f, old).carried_on.is_some());
    assert!(reply_notices(&f, old).is_empty());
    f.close().await;
}

async fn reply(f: &Fixture, task: TaskId, text: &str) -> CommandReceipt {
    let revision = task_of(f, task).revision;
    f.hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ResumeTask {
                    observed_reply: None,
                    harness: None,
                    task_id: task,
                    text: Some(text.into()),
                    bundle: None,
                    mode: None,
                    model: None,
                    effort: None,
                    carried_plan: None,
                    read_only: false,
                },
                revision,
            ),
        )
        .await
}

async fn mark(f: &Fixture, task: TaskId, resolution: Reconciliation) -> CommandReceipt {
    let revision = task_of(f, task).revision;
    f.hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ReconcileTask {
                    task_id: task,
                    resolution,
                },
                revision,
            ),
        )
        .await
}

fn add_helper(f: &Fixture, id: RunId, lifetime: Option<ChildLifetime>, state: ChildState) {
    f.hub
        .write(|tx| {
            let mut run: Run = tx.read().require("run", id)?;
            let child = ChildAgent {
                child_id: ChildId::new(),
                lifetime,
                native_ref: Some("local-helper".into()),
                label: "Background helper".into(),
                state,
                started_at: run.admitted_at,
                ended_at: None,
                provenance: Provenance::Native,
                stop: None,
            };
            let item = AttentionItem {
                attention_id: AttentionId::new(),
                child_id: Some(child.child_id),
                kind: AttentionKind::ReconciliationRequired,
                title: "Check helper".into(),
                body: None,
                task_id: Some(run.task_id),
                run_id: Some(id),
                approval_id: None,
                created_at: run.admitted_at,
                resolved_at: None,
                priority: 5,
                topic: None,
                quiet: false,
                report_id: None,
                account_id: None,
                alert: None,
                alert_handled_at: None,
                agent_update: None,
                new_version: None,
            };
            tx.put("attention", item.attention_id, &item)?;
            run.children.push(child);
            tx.put("run", id, &run)?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn restart_settles_only_local_helpers_with_process_proof() {
    for lifetime in [
        Some(ChildLifetime::InProcess),
        Some(ChildLifetime::SeparateProcess),
        None,
    ] {
        for state in [ChildState::Active, ChildState::Unknown] {
            for populated in [false, true] {
                let group = Group::new();
                let f = chat_fixture().await;
                *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
                *f.harness.controls.containment.lock().unwrap() =
                    Some(ports::Containment::Complete);
                let (task, old) = chat(&f, "native-1").await;
                add_helper(&f, old, lifetime, state);
                group.populated(populated);
                let f = crashed(f).await;
                let cut = run_of(&f, old);
                let allowed = !populated && lifetime.is_some();
                assert_eq!(reply_notices(&f, old).len(), usize::from(allowed));
                assert_eq!(
                    cut.nothing_running, allowed,
                    "{lifetime:?} {state:?} populated={populated}"
                );
                let child = &cut.children[0];
                if allowed {
                    assert_eq!(child.state, ChildState::Stopped);
                    assert!(child.ended_at.is_some());
                    assert_eq!(child.provenance, Provenance::Observed);
                    assert_eq!(
                        child.stop,
                        Some(Availability::Unsupported {
                            reason: "Ended with the run's processes".into()
                        })
                    );
                    assert_eq!(open_reviews(&f, task), 0);
                    assert_eq!(task_of(&f, task).attention_count, 0);
                } else {
                    assert_eq!(child.state, state);
                    assert_eq!(child.ended_at, None);
                    assert_eq!(open_reviews(&f, task), 1);
                }
                let receipt = reply(&f, task, "Please carry on").await;
                assert_eq!(
                    receipt.status,
                    if allowed {
                        ReceiptStatus::Accepted
                    } else {
                        ReceiptStatus::Rejected
                    }
                );
                f.close().await;
            }
        }
    }
}

#[tokio::test]
async fn restart_keeps_uncontained_helpers_and_unconfirmed_cleanup_for_review() {
    for (contained, cleanup, grouped) in [
        (false, false, true),
        (false, false, false),
        (true, true, true),
    ] {
        let group = Group::new();
        let f = chat_fixture().await;
        if grouped {
            *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
        }
        if contained {
            *f.harness.controls.containment.lock().unwrap() = Some(ports::Containment::Complete);
        }
        let (task, old) = chat(&f, "native-1").await;
        add_helper(
            &f,
            old,
            Some(ChildLifetime::SeparateProcess),
            ChildState::Active,
        );
        if cleanup {
            f.hub
                .write(|tx| {
                    let mut run: Run = tx.read().require("run", old)?;
                    run.state = RunState::Completed;
                    run.ended_at = Some(run.admitted_at);
                    run.cleanup_pending = true;
                    tx.put("run", old, &run)
                })
                .unwrap();
        }
        group.populated(false);
        let f = crashed(f).await;
        let cut = run_of(&f, old);
        assert!(!cut.nothing_running);
        assert_eq!(cut.children[0].state, ChildState::Active);
        assert!(open_reviews(&f, task) > 0);
        assert_eq!(
            reply(&f, task, "Carry on").await.status,
            ReceiptStatus::Rejected
        );
        f.close().await;
    }
}

#[tokio::test]
async fn restart_clears_an_older_versions_decision_request_for_a_confirmed_chat() {
    let f = chat_fixture().await;
    let (task, old) = chat(&f, "native-1").await;
    add_helper(&f, old, Some(ChildLifetime::InProcess), ChildState::Stopped);
    f.hub
        .write(|tx| {
            let mut run: Run = tx.read().require("run", old)?;
            run.state = RunState::ReconciliationRequired;
            run.nothing_running = true;
            tx.put("run", old, &run)?;
            for mut item in tx.read().list::<AttentionItem>("attention")? {
                if item.run_id == Some(old) {
                    item.child_id = None;
                    tx.put("attention", item.attention_id, &item)?;
                }
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(open_reviews(&f, task), 1);
    let f = crashed(f).await;
    assert!(run_of(&f, old).nothing_running);
    assert_eq!(open_reviews(&f, task), 0);
    assert_eq!(task_of(&f, task).attention_count, 0);
    f.close().await;
}

#[tokio::test]
async fn a_confirmed_contained_exit_settles_local_helpers_but_not_unknown_lifetimes() {
    for contained in [true, false] {
        for lifetime in [
            Some(ChildLifetime::InProcess),
            Some(ChildLifetime::SeparateProcess),
            None,
        ] {
            let f = chat_fixture().await;
            if contained {
                *f.harness.controls.containment.lock().unwrap() =
                    Some(ports::Containment::Complete);
            }
            let (task, old) = chat(&f, "native-1").await;
            add_helper(&f, old, lifetime, ChildState::Active);
            unknown_exit(&f, old).await;
            let allowed = contained && lifetime.is_some();
            assert_eq!(reply_notices(&f, old).len(), usize::from(allowed));
            let cut = run_of(&f, old);
            assert_eq!(
                cut.nothing_running, allowed,
                "{lifetime:?}, contained={contained}"
            );
            if allowed {
                assert_eq!(cut.children[0].state, ChildState::Stopped);
                assert_eq!(open_reviews(&f, task), 0);
                assert_eq!(task_of(&f, task).attention_count, 0);
            } else {
                assert!(open_reviews(&f, task) > 0);
            }
            assert_eq!(
                reply(&f, task, "Carry on").await.status,
                if allowed {
                    ReceiptStatus::Accepted
                } else {
                    ReceiptStatus::Rejected
                }
            );
            f.close().await;
        }
    }
}

/// What the agent gets on the turn that carries the chat on.
fn told(text: &str) -> String {
    format!("{}\n\n{text}", crate::cut_off::CUT_OFF_NOTE)
}

/// A turn that ends as done.
async fn complete(f: &Fixture, run: RunId) {
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    let mut more = waiting();
    while run_of(f, run).state != RunState::Completed {
        assert!(more(), "run did not complete");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The new turn's start, once the adapter has it.
async fn next_spec(f: &Fixture, after: usize) -> ports::RunSpec {
    let mut more = waiting();
    loop {
        if let Some(spec) = f.harness.specs.lock().unwrap().get(after).cloned() {
            return spec;
        }
        assert!(more(), "no new turn started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_chat_cut_off_by_a_restart_carries_on_in_the_same_session() {
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    // The agent and everything it started ended with Øphio.
    group.populated(false);
    let f = crashed(f).await;
    let cut = run_of(&f, old);
    assert_eq!(cut.state, RunState::ReconciliationRequired);
    assert!(cut.nothing_running);
    assert_eq!(task_of(&f, task).state, TaskState::ReconciliationRequired);
    assert_eq!(task_of(&f, task).attention_count, 0);
    assert_eq!(open_reviews(&f, task), 0);

    let starts = f.harness.specs.lock().unwrap().len();
    let receipt = reply(&f, task, "Where were we?").await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let spec = next_spec(&f, starts).await;
    // The same session, given only the new message, with one note for the
    // agent that its last reply was cut off.
    assert_eq!(spec.resume_session.as_deref(), Some("native-1"));
    assert_eq!(spec.prompt.text, told("Where were we?"));
    assert_eq!(f.harness.specs.lock().unwrap().len(), starts + 1);

    // The cut-off turn reads as stopped, carried on by the person; never as
    // completed, and never as their verdict.
    let settled = run_of(&f, old);
    assert_eq!(settled.state, RunState::Interrupted);
    assert!(settled.human_resolution.is_none());
    let by = settled.carried_on.expect("who carried on");
    assert_eq!(by.by, Caller::local_owner().principal);
    assert!(!settled.holds_writer);
    let now = task_of(&f, task);
    assert_ne!(now.current_run, Some(old));
    assert_eq!(now.attention_count, 0);
    assert_eq!(open_reviews(&f, task), 0);
    f.close().await;
}

#[tokio::test]
async fn a_chat_whose_old_run_may_still_be_running_keeps_its_review() {
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    // Something the run started outlived Øphio.
    let f = crashed(f).await;
    let cut = run_of(&f, old);
    assert_eq!(cut.state, RunState::ReconciliationRequired);
    assert!(!cut.nothing_running);

    let starts = f.harness.specs.lock().unwrap().len();
    let receipt = reply(&f, task, "Where were we?").await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    let refused = receipt.error.unwrap();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(
        refused.message.contains("can’t confirm"),
        "{}",
        refused.message
    );
    assert_eq!(f.harness.specs.lock().unwrap().len(), starts);
    assert_eq!(run_of(&f, old).state, RunState::ReconciliationRequired);
    assert_eq!(task_of(&f, task).state, TaskState::ReconciliationRequired);

    // The person checks it and marks it failed, as an older phone does.
    let receipt = mark(&f, task, Reconciliation::MarkFailed).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let marked = run_of(&f, old);
    assert_eq!(marked.state, RunState::Failed);
    assert!(marked.carried_on.is_none());
    assert_eq!(
        marked.human_resolution.map(|r| r.resolution),
        Some(Reconciliation::MarkFailed)
    );
    assert_eq!(task_of(&f, task).state, TaskState::Failed);
    f.close().await;
}

#[tokio::test]
async fn a_cut_off_chat_can_still_be_marked_completed() {
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    group.populated(false);
    let f = crashed(f).await;
    assert!(run_of(&f, old).nothing_running);
    let receipt = mark(&f, task, Reconciliation::MarkCompleted).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let marked = run_of(&f, old);
    assert_eq!(marked.state, RunState::Completed);
    assert!(marked.carried_on.is_none());
    assert_eq!(task_of(&f, task).state, TaskState::Completed);
    f.close().await;
}

#[tokio::test]
async fn a_task_cut_off_by_a_restart_is_refused_as_before() {
    let group = Group::new();
    let f = Fixture::new().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    *f.harness.controls.containment.lock().unwrap() = Some(ports::Containment::Complete);
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let old = run_started(&f, task).await;
    session(&f, old, "native-1").await;
    add_helper(
        &f,
        old,
        Some(ChildLifetime::SeparateProcess),
        ChildState::Active,
    );
    group.populated(false);
    let f = stopped(f).await;
    let cut = run_of(&f, old);
    assert_eq!(cut.state, RunState::ReconciliationRequired);
    assert!(!cut.nothing_running, "only a chat carries on");
    assert!(reply_notices(&f, old).is_empty());
    assert_eq!(cut.children[0].state, ChildState::Active);
    assert_eq!(open_reviews(&f, task), 1);

    let starts = f.harness.specs.lock().unwrap().len();
    let receipt = reply(&f, task, "Keep going").await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    let refused = receipt.error.unwrap();
    assert_eq!(refused.code, ErrorCode::Conflict, "{refused:?}");
    assert_eq!(
        refused.message,
        "Task cannot be resumed in its current state"
    );
    assert_eq!(f.harness.specs.lock().unwrap().len(), starts);
    assert_eq!(run_of(&f, old).state, RunState::ReconciliationRequired);
    f.close().await;
}

#[tokio::test]
async fn a_tasks_confirmed_unknown_exit_still_asks_for_review_not_a_reply() {
    let f = Fixture::new().await;
    *f.harness.controls.containment.lock().unwrap() = Some(ports::Containment::Complete);
    let project = f.project().await;
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let old = run_started(&f, task).await;
    session(&f, old, "native-1").await;
    unknown_exit(&f, old).await;
    assert!(!run_of(&f, old).nothing_running);
    assert_eq!(open_reviews(&f, task), 1);
    assert!(task_of(&f, task).attention_count > 0);
    assert!(reply_notices(&f, old).is_empty());
    f.close().await;
}

/// The agent's result was lost, but its exit came: where its processes are
/// kept together, that exit says they all ended.
async fn unknown_exit(f: &Fixture, run: RunId) {
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Unknown {
                    reason: "synthetic lost result".into(),
                },
                outcome: None,
            },
        )
        .await;
    let mut more = waiting();
    while run_of(f, run).state != RunState::ReconciliationRequired {
        assert!(more(), "run did not end");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_chat_with_an_unknown_result_from_a_contained_run_carries_on() {
    let f = chat_fixture().await;
    *f.harness.controls.containment.lock().unwrap() = Some(ports::harness::Containment::Complete);
    let (task, old) = chat(&f, "native-1").await;
    unknown_exit(&f, old).await;
    let mut more = waiting();
    while !run_of(&f, old).nothing_running {
        assert!(more(), "not marked");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        open_reviews(&f, task),
        0,
        "no decision needed for a cut-off chat"
    );

    let starts = f.harness.specs.lock().unwrap().len();
    let receipt = reply(&f, task, "Try that again?").await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let spec = next_spec(&f, starts).await;
    assert_eq!(spec.resume_session.as_deref(), Some("native-1"));
    assert_eq!(spec.prompt.text, told("Try that again?"));
    let settled = run_of(&f, old);
    assert_eq!(settled.state, RunState::Interrupted);
    assert!(settled.carried_on.is_some() && settled.human_resolution.is_none());
    assert_eq!(open_reviews(&f, task), 0);
    assert_eq!(task_of(&f, task).attention_count, 0);
    f.close().await;
}

#[tokio::test]
async fn a_chat_with_an_unknown_result_outside_a_full_container_keeps_its_review() {
    let f = chat_fixture().await;
    *f.harness.controls.containment.lock().unwrap() =
        Some(ports::harness::Containment::ProcessGroup {
            reason: "fixture".into(),
        });
    let (task, old) = chat(&f, "native-1").await;
    unknown_exit(&f, old).await;
    // Give a wrong mark the time it would take.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!run_of(&f, old).nothing_running);
    let receipt = reply(&f, task, "Try that again?").await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    assert_eq!(receipt.error.unwrap().code, ErrorCode::Conflict);
    assert_eq!(run_of(&f, old).state, RunState::ReconciliationRequired);
    f.close().await;
}

#[tokio::test]
async fn a_cut_off_chat_carries_on_only_with_a_new_message() {
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    group.populated(false);
    let f = crashed(f).await;
    let starts = f.harness.specs.lock().unwrap().len();
    // Nothing is sent again by itself: an empty Continue is refused.
    let receipt = reply(&f, task, "  ").await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    assert_eq!(f.harness.specs.lock().unwrap().len(), starts);
    assert_eq!(run_of(&f, old).state, RunState::ReconciliationRequired);
    f.close().await;
}

#[tokio::test]
async fn the_agent_is_told_once_on_the_turn_that_carries_on() {
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, _) = chat(&f, "native-1").await;
    group.populated(false);
    let f = crashed(f).await;
    let starts = f.harness.specs.lock().unwrap().len();
    let receipt = reply(&f, task, "Where were we?").await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(
        next_spec(&f, starts).await.prompt.text,
        told("Where were we?")
    );
    // The person reads only their own words.
    let said: Vec<String> = f
        .hub
        .read(|r| r.timeline(task, None, 200))
        .unwrap()
        .0
        .into_iter()
        .filter_map(|i| match i.item {
            TimelineBody::UserMessage { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(said.last().map(String::as_str), Some("Where were we?"));
    assert!(said.iter().all(|t| !t.contains("cut off")));
    // The next turn is ordinary.
    let carried = task_of(&f, task).current_run.unwrap();
    session(&f, carried, "native-1").await;
    complete(&f, carried).await;
    let receipt = reply(&f, task, "Thanks, and the weekly chart?").await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(
        next_spec(&f, starts + 1).await.prompt.text,
        "Thanks, and the weekly chart?"
    );
    f.close().await;
}

#[tokio::test]
async fn a_storage_failure_as_the_turn_ends_loses_no_confirmation() {
    let f = chat_fixture().await;
    *f.harness.controls.containment.lock().unwrap() = Some(ports::harness::Containment::Complete);
    let (task, old) = chat(&f, "native-1").await;
    add_helper(
        &f,
        old,
        Some(ChildLifetime::SeparateProcess),
        ChildState::Active,
    );
    // The write that records the end, with its confirmation, fails once.
    let db = rusqlite::Connection::open(f.path.join("companion.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_mark BEFORE INSERT ON documents
         WHEN NEW.kind = 'run' AND json_extract(NEW.json, '$.nothing_running') = 1
         BEGIN SELECT RAISE(ABORT, 'synthetic disk full'); END;",
    )
    .unwrap();
    f.harness
        .emit(
            old,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Unknown {
                    reason: "synthetic lost result".into(),
                },
                outcome: None,
            },
        )
        .await;
    let mut more = waiting();
    while !f
        .hub
        .read(|r| r.get::<bool>("run_process_ended", old))
        .unwrap()
        .unwrap_or(false)
    {
        assert!(more(), "exit never seen");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Neither the end nor the confirmation was kept without the other.
    let failed = run_of(&f, old);
    assert!(
        failed.ended_at.is_none() && !failed.nothing_running,
        "{failed:?}"
    );
    assert_eq!(failed.children[0].state, ChildState::Unknown);
    assert!(failed.children[0].ended_at.is_none());
    assert!(open_reviews(&f, task) > 0);
    db.execute_batch("DROP TRIGGER refuse_mark;").unwrap();
    drop(db);
    // The exit is tried again, and both are kept together.
    let mut more = waiting();
    while !run_of(&f, old).nothing_running {
        assert!(more(), "the retried exit didn't confirm it");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let ended = run_of(&f, old);
    assert_eq!(ended.state, RunState::ReconciliationRequired);
    assert!(ended.ended_at.is_some());
    assert_eq!(ended.children[0].state, ChildState::Stopped);
    assert_eq!(open_reviews(&f, task), 0);
    let receipt = reply(&f, task, "Try that again?").await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    f.close().await;
}

#[tokio::test]
async fn two_replies_at_once_settle_the_cut_off_turn_once() {
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    group.populated(false);
    let f = crashed(f).await;
    let starts = f.harness.specs.lock().unwrap().len();
    let (a, b) = tokio::join!(
        reply(&f, task, "First thought"),
        reply(&f, task, "Second thought")
    );
    let accepted = [&a, &b]
        .iter()
        .filter(|r| r.status == ReceiptStatus::Accepted)
        .count();
    assert_eq!(accepted, 1, "{a:?} {b:?}");
    let refused = if a.status == ReceiptStatus::Accepted {
        &b
    } else {
        &a
    };
    assert_eq!(refused.status, ReceiptStatus::Rejected, "{refused:?}");
    // Settled once, by the reply that carried on; one turn, told once.
    let settled = run_of(&f, old);
    assert_eq!(settled.state, RunState::Interrupted);
    assert!(settled.carried_on.is_some());
    let spec = next_spec(&f, starts).await;
    assert!(spec.prompt.text.starts_with(crate::cut_off::CUT_OFF_NOTE));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.harness.specs.lock().unwrap().len(), starts + 1);
    f.close().await;
}

#[tokio::test]
async fn a_cut_off_side_question_returns_the_chat_to_where_it_was() {
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, first) = chat(&f, "native-1").await;
    complete(&f, first).await;
    assert_eq!(task_of(&f, task).state, TaskState::Completed);
    // A read-only question about the finished chat, cut off by a restart.
    let revision = task_of(&f, task).revision;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ResumeTask {
                    observed_reply: None,
                    harness: None,
                    task_id: task,
                    text: Some("Sum up".into()),
                    bundle: None,
                    mode: None,
                    model: None,
                    effort: None,
                    carried_plan: None,
                    read_only: true,
                },
                revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let side = run_started(&f, task).await;
    assert_ne!(side, first);
    session(&f, side, "native-1").await;
    group.populated(false);
    let f = crashed(f).await;
    assert!(run_of(&f, side).nothing_running);
    assert_eq!(task_of(&f, task).state, TaskState::ReconciliationRequired);

    let starts = f.harness.specs.lock().unwrap().len();
    let receipt = reply(&f, task, "Go on").await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    // The side question no longer stands in for the chat's own state: the
    // reply is an ordinary turn, which ends the chat as it ends.
    assert!(
        f.hub
            .read(|r| r.get::<TaskState>(crate::runs::SIDE_TURN, side))
            .unwrap()
            .is_none()
    );
    let spec = next_spec(&f, starts).await;
    assert_eq!(spec.prompt.text, told("Go on"));
    let next = task_of(&f, task).current_run.unwrap();
    assert_ne!(next, side);
    session(&f, next, "native-1").await;
    complete(&f, next).await;
    assert_eq!(task_of(&f, task).state, TaskState::Completed);
    assert_eq!(run_of(&f, side).state, RunState::Interrupted);
    f.close().await;
}

/// Wait for the streamed text on the live connection. Delivery does not prove
/// the words reached disk; crash tests must force a checkpoint first.
async fn streamed(f: &Fixture, run: RunId, key: &str, text: &str) {
    use crate::api::Outbound;
    let mut live = f
        .hub
        .subscribe(&Caller::local_owner(), None)
        .await
        .unwrap()
        .live;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::AgentTextDelta {
                message_key: key.into(),
                text: text.into(),
            },
        )
        .await;
    let mut more = waiting();
    loop {
        assert!(more(), "streamed reply never arrived");
        if let Ok(Ok(Outbound::Live(event))) =
            tokio::time::timeout(Duration::from_millis(100), live.recv()).await
            && matches!(&*event, protocol::event::LiveEvent::AgentTextDelta {
                run_id,
                message_key,
                text: received,
                ..
            } if *run_id == run && uuid::Uuid::parse_str(message_key).is_ok() && received == text)
        {
            break;
        }
    }
}

fn agent_replies(f: &Fixture, task: TaskId, run: RunId) -> Vec<(u64, String, bool)> {
    f.hub
        .read(|r| r.timeline(task, None, 200))
        .unwrap()
        .0
        .into_iter()
        .filter_map(|entry| match entry.item {
            TimelineBody::AgentMessage { text, partial, .. } if entry.run_id == Some(run) => {
                Some((entry.seq, text, partial))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_streamed_reply_survives_two_restarts_once_and_keeps_its_order() {
    for processes_remain in [false, true] {
        let group = Group::new();
        let f = chat_fixture().await;
        *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
        let (task, run) = chat(&f, "native-1").await;
        streamed(&f, run, "reply-1", "The ").await;
        streamed(&f, run, "reply-1", "first half").await;
        f.harness
            .emit(
                run,
                ports::HarnessEvent::Activity {
                    key: "tool-1".into(),
                    tool: "Read".into(),
                    summary: "Read finished".into(),
                    status: ActivityStatus::Succeeded,
                    detail: None,
                },
            )
            .await;
        let mut more = waiting();
        while !f
            .hub
            .read(|r| r.timeline(task, None, 200))
            .unwrap()
            .0
            .iter()
            .any(|item| matches!(item.item, TimelineBody::Activity { .. }))
        {
            assert!(more(), "activity never recorded");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        streamed(&f, run, "reply-2", "And a second thought").await;
        // Neither key ends the other message. Save both pending tails before
        // simulating an abrupt restart.
        f.hub.save_replies().await.unwrap();
        group.populated(processes_remain);
        let f = crashed(f).await;
        assert_eq!(run_of(&f, run).state, RunState::ReconciliationRequired);
        assert_eq!(run_of(&f, run).nothing_running, !processes_remain);
        let timeline = |f: &Fixture| {
            f.hub
                .read(|r| r.timeline(task, None, 200))
                .unwrap()
                .0
                .into_iter()
                .filter(|item| item.run_id == Some(run))
                .filter_map(|item| match item.item {
                    TimelineBody::AgentMessage { text, partial, .. } => {
                        Some((item.seq, text, partial))
                    }
                    TimelineBody::Activity { .. } => {
                        Some((item.seq, "Read finished".into(), false))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let first = timeline(&f);
        assert_eq!(first.len(), 3, "{first:?}");
        assert_eq!(first[0].1, "Read finished");
        assert_eq!(first[1].1, "The first half");
        assert!(first[1].2);
        assert_eq!(first[2].1, "And a second thought");
        assert!(first[2].2);
        assert!(first[0].0 < first[1].0 && first[1].0 < first[2].0);
        let f = crashed(f).await;
        assert_eq!(timeline(&f), first, "no replay duplicates");
        if !processes_remain {
            let starts = f.harness.specs.lock().unwrap().len();
            let receipt = reply(&f, task, "Carry on").await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            let spec = next_spec(&f, starts).await;
            assert_eq!(spec.resume_session.as_deref(), Some("native-1"));
        }
        f.close().await;
    }
}

#[tokio::test]
async fn a_final_reply_replaces_a_pending_stream_instead_of_repeating_it() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "reply", "The first half").await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::AgentMessage {
                message_key: "reply".into(),
                text: "The first half and the end".into(),
            },
        )
        .await;
    let mut more = waiting();
    while agent_replies(&f, task, run).is_empty() {
        assert!(more(), "final reply never recorded");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].1, "The first half and the end");
    assert!(!replies[0].2);
    f.close().await;
}

#[tokio::test]
async fn helper_activity_does_not_split_one_streamed_reply_when_final_arrives() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "reply", "Before the tool. ").await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Activity {
                key: "tool-1".into(),
                tool: "Read".into(),
                summary: "Read finished".into(),
                status: ActivityStatus::Succeeded,
                detail: None,
            },
        )
        .await;
    let mut more = waiting();
    while !f
        .hub
        .read(|r| r.timeline(task, None, 200))
        .unwrap()
        .0
        .iter()
        .any(|item| matches!(item.item, TimelineBody::Activity { .. }))
    {
        assert!(more(), "activity never recorded");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let before = agent_replies(&f, task, run);
    assert!(before.is_empty(), "helper activity must not flush a reply");
    streamed(&f, run, "reply", "After the tool. ").await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::AgentMessage {
                message_key: "reply".into(),
                text: "Before the tool. After the tool. The end.".into(),
            },
        )
        .await;
    let mut more = waiting();
    while agent_replies(&f, task, run).is_empty() {
        assert!(more(), "final reply never recorded");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let after = agent_replies(&f, task, run);
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0].1, "Before the tool. After the tool. The end.");
    assert!(!after[0].2);
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run), after);
    f.close().await;
}

#[tokio::test]
async fn a_long_stream_survives_in_deliverable_frames_without_losing_characters() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    let piece = "é\n\u{1}".repeat(20_000);
    for _ in 0..8 {
        streamed(&f, run, "long", &piece).await;
    }
    f.hub.save_replies().await.unwrap();
    let f = crashed(f).await;
    let replies = agent_replies(&f, task, run);
    assert!(replies.len() > 1);
    assert!(replies.iter().all(|(_, _, partial)| *partial));
    let joined: String = replies.iter().map(|(_, text, _)| text.as_str()).collect();
    assert!(joined == piece.repeat(8));
    for event in f.hub.read(|r| r.journal(0)).unwrap().events {
        assert!(serde_json::to_vec(&event).unwrap().len() < 1024 * 1024);
    }
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run).len(), replies.len());
    f.close().await;
}

#[tokio::test]
async fn a_helpers_final_does_not_split_the_parents_streamed_reply() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "parent", "Earlier ").await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::AgentMessage {
                message_key: "helper".into(),
                text: "Helper's whole reply".into(),
            },
        )
        .await;
    let mut more = waiting();
    while agent_replies(&f, task, run).is_empty() {
        assert!(more(), "helper final never recorded");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(agent_replies(&f, task, run).len(), 1);
    streamed(&f, run, "parent", "words").await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::AgentMessage {
                message_key: "parent".into(),
                text: "Earlier words".into(),
            },
        )
        .await;
    let mut more = waiting();
    while agent_replies(&f, task, run).len() < 2 {
        assert!(more(), "parent final never recorded");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0].1, "Helper's whole reply");
    assert_eq!(replies[1].1, "Earlier words");
    assert!(replies.iter().all(|(_, _, partial)| !partial));
    unknown_exit(&f, run).await;
    assert_eq!(agent_replies(&f, task, run), replies);
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run), replies);
    f.close().await;
}

fn reply_chunks(f: &Fixture, run: RunId) -> Vec<serde_json::Value> {
    f.hub
        .read(|r| r.entries::<serde_json::Value>(&format!("partial_reply_chunks:{run}")))
        .unwrap()
        .into_iter()
        .map(|(_, chunk)| chunk)
        .collect()
}

#[tokio::test]
async fn a_quiet_stream_is_checkpointed_by_the_timer_without_another_event() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "quiet", "Timer-only words").await;
    tokio::time::timeout(Duration::from_secs(4), async {
        while reply_chunks(&f, run).is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timer never saved the quiet reply");
    assert_eq!(reply_chunks(&f, run)[0]["text"], "Timer-only words");
    assert!(agent_replies(&f, task, run).is_empty());
    f.close().await;
}

#[tokio::test]
async fn a_large_piece_checkpoints_without_waiting_for_the_timer() {
    let f = chat_fixture().await;
    let (_, run) = chat(&f, "native-1").await;
    let text = "x".repeat(8 * 1024);
    streamed(&f, run, "large", &text).await;
    tokio::time::timeout(Duration::from_millis(900), async {
        while reply_chunks(&f, run).is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("size threshold did not checkpoint the reply");
    assert_eq!(reply_chunks(&f, run)[0]["text"], text);
    f.close().await;
}

#[tokio::test]
async fn graceful_shutdown_saves_the_last_unsaved_words() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "reply", "Saved on shutdown").await;
    assert!(agent_replies(&f, task, run).is_empty());
    // Unlike a power cut, the ordinary stop gets a chance to save this tail.
    let f = f
        .reopened(|b| b.sandboxes(Arc::new(Backend::default())))
        .await;
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].1, "Saved on shutdown");
    assert!(replies[0].2);
    f.close().await;
}

#[tokio::test]
async fn final_after_timer_checkpoint_and_more_words_is_not_duplicated() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "reply", "Saved first. ").await;
    tokio::time::timeout(Duration::from_secs(4), async {
        while reply_chunks(&f, run).is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timer did not checkpoint the first part");
    streamed(&f, run, "reply", "Unsaved next. ").await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::AgentMessage {
                message_key: "reply".into(),
                text: "Saved first. Unsaved next. Final.".into(),
            },
        )
        .await;
    let mut more = waiting();
    while agent_replies(&f, task, run).is_empty() {
        assert!(more(), "final reply never recorded");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].1, "Saved first. Unsaved next. Final.");
    assert!(!replies[0].2);
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run), replies);
    f.close().await;
}

#[tokio::test]
async fn many_small_pieces_checkpoint_once_without_rewriting_the_saved_text() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    let writes_before = f
        .hub
        .inner
        .completed_writes
        .load(std::sync::atomic::Ordering::Relaxed);
    for _ in 0..100 {
        streamed(&f, run, "tiny", "x").await;
    }
    let writes = f
        .hub
        .inner
        .completed_writes
        .load(std::sync::atomic::Ordering::Relaxed)
        - writes_before;
    assert!(
        writes <= 3,
        "100 small pieces caused {writes} database writes"
    );
    let before = reply_chunks(&f, run);
    assert!(
        before.len() <= 1,
        "timer may have checkpointed once: {before:?}"
    );
    assert!(agent_replies(&f, task, run).is_empty());
    f.hub.save_replies().await.unwrap();
    let saved = reply_chunks(&f, run);
    assert!((1..=2).contains(&saved.len()), "{saved:?}");
    let text: String = saved
        .iter()
        .map(|chunk| chunk["text"].as_str().unwrap())
        .collect();
    assert_eq!(text, "x".repeat(100));
    f.hub.save_replies().await.unwrap();
    assert_eq!(reply_chunks(&f, run), saved, "no delta means no new chunk");
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run).len(), 1);
    assert_eq!(agent_replies(&f, task, run)[0].1, text);
    f.close().await;
}

#[tokio::test]
async fn different_message_keys_stay_pending_until_recovery() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "first", "Earlier words").await;
    streamed(&f, run, "second", "Later words").await;
    assert!(
        agent_replies(&f, task, run).is_empty(),
        "a new key is not a boundary"
    );
    f.hub.save_replies().await.unwrap();
    let f = crashed(f).await;
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0].1, "Earlier words");
    assert_eq!(replies[1].1, "Later words");
    f.close().await;
}

#[tokio::test]
async fn a_run_end_checkpoints_its_unsaved_tail() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "reply", "Words at the end").await;
    unknown_exit(&f, run).await;
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].1, "Words at the end");
    assert!(replies[0].2);
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run), replies);
    f.close().await;
}

#[tokio::test]
async fn a_failed_partial_flush_keeps_its_pending_words_for_retry() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "reply", "Words kept through a failed write").await;
    let saved = run_of(&f, run);
    let error = f.hub.write(|tx| {
        f.hub.finish_reply(tx, &saved)?;
        Err::<(), store::Error>(store::Error::IntegerExhausted)
    });
    assert!(error.is_err());
    assert!(agent_replies(&f, task, run).is_empty());
    // The failed transaction must not consume the pending in-memory tail.
    f.hub.write(|tx| f.hub.finish_reply(tx, &saved)).unwrap();
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1, "Words kept through a failed write");
    assert!(replies[0].2);
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run), replies);
    f.close().await;
}

#[tokio::test]
async fn a_hard_crash_keeps_the_checkpoint_but_not_the_unsaved_tail() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "reply", "Saved words. ").await;
    f.hub.save_replies().await.unwrap();
    stop_jobs(&f).await;
    // No timer or run worker can save this tail before the simulated crash.
    f.hub.buffer_reply(run, "reply", "Unsaved words.");
    let f = crashed(f).await;
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1, "Saved words. ");
    assert!(replies[0].2);
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run), replies);
    f.close().await;
}

#[tokio::test]
async fn a_committed_checkpoint_can_be_retried_before_memory_acknowledgement() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    stop_jobs(&f).await;
    f.hub.buffer_reply(run, "reply", "Saved once. ");
    let saved = run_of(&f, run);
    // Commit directly, deliberately bypassing the hub's acknowledgement. This
    // models an error after SQLite commit and before memory was consumed.
    f.hub
        .inner
        .db
        .lock()
        .unwrap()
        .write(f.hub.inner.clock.now(), |tx| {
            f.hub.checkpoint_reply(tx, &saved)
        })
        .unwrap();
    assert_eq!(reply_chunks(&f, run).len(), 1);
    f.hub.save_replies().await.unwrap();
    assert_eq!(reply_chunks(&f, run).len(), 1, "retry appended a duplicate");
    f.hub.buffer_reply(run, "reply", "Then more.");
    f.hub.save_replies().await.unwrap();
    let chunks = reply_chunks(&f, run);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0]["text"], "Saved once. ");
    assert_eq!(chunks[1]["text"], "Then more.");
    f.hub.write(|tx| f.hub.finish_reply(tx, &saved)).unwrap();
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1, "Saved once. Then more.");
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run), replies);
    f.close().await;
}

#[tokio::test]
async fn an_admitted_person_message_saves_the_unsaved_reply_before_it() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "reply", "Before your message. ").await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SendMessage {
                task_id: task,
                text: "One more thing".into(),
                bundle: None,
                delivery: MessageDelivery::NextTurn,
                harness: None,
                model: None,
                effort: None,
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1, "Before your message. ");
    assert!(replies[0].2);
    let timeline = f.hub.read(|r| r.timeline(task, None, 200)).unwrap().0;
    let person = timeline
        .iter()
        .find(|i| {
            matches!(&i.item,
        TimelineBody::UserMessage { text, .. } if text == "One more thing")
        })
        .unwrap();
    assert!(replies[0].0 < person.seq);
    f.harness
        .emit(
            run,
            ports::HarnessEvent::AgentMessage {
                message_key: "reply".into(),
                text: "Before your message. After it.".into(),
            },
        )
        .await;
    let mut more = waiting();
    while agent_replies(&f, task, run).len() < 2 {
        assert!(more());
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(agent_replies(&f, task, run)[1].1, "After it.");
    f.close().await;
}

async fn native_final(f: &Fixture, task: TaskId, run: RunId, key: &str, text: &str) {
    f.harness
        .emit(
            run,
            ports::HarnessEvent::AgentMessage {
                message_key: key.into(),
                text: text.into(),
            },
        )
        .await;
    let mut more = waiting();
    while !agent_replies(f, task, run)
        .iter()
        .any(|(_, said, partial)| said == text && !partial)
    {
        assert!(more(), "final message never recorded");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn interleaved_messages_finish_whole_in_either_order_without_piece_writes() {
    for keys in [["parent", "helper"], ["helper", "parent"]] {
        let f = chat_fixture().await;
        let (task, run) = chat(&f, "native-1").await;
        let before = f
            .hub
            .inner
            .completed_writes
            .load(std::sync::atomic::Ordering::Relaxed);
        for _ in 0..100 {
            streamed(&f, run, "parent", "p").await;
            streamed(&f, run, "helper", "h").await;
        }
        let writes = f
            .hub
            .inner
            .completed_writes
            .load(std::sync::atomic::Ordering::Relaxed)
            - before;
        assert!(writes <= 3, "200 interleaved pieces caused {writes} writes");
        assert!(agent_replies(&f, task, run).is_empty());
        for key in keys {
            let text = if key == "parent" { "p" } else { "h" }.repeat(100);
            native_final(&f, task, run, key, &text).await;
        }
        let replies = agent_replies(&f, task, run);
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert!(replies.iter().all(|(_, _, partial)| !partial));
        let f = crashed(f).await;
        assert_eq!(agent_replies(&f, task, run), replies);
        f.close().await;
    }
}

#[tokio::test]
async fn interleaved_checkpoint_recovers_both_whole_unmixed_partial_messages() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    for piece in ["one ", "two ", "three"] {
        streamed(&f, run, "parent", &format!("Parent {piece}")).await;
        streamed(&f, run, "helper", &format!("Helper {piece}")).await;
    }
    stop_jobs(&f).await;
    // Ensure both keys have an unsaved tail even if a timer fired during setup.
    f.hub.buffer_reply(run, "parent", " end");
    f.hub.buffer_reply(run, "helper", " end");
    let before = f
        .hub
        .inner
        .completed_writes
        .load(std::sync::atomic::Ordering::Relaxed);
    f.hub.save_replies().await.unwrap();
    assert_eq!(
        f.hub
            .inner
            .completed_writes
            .load(std::sync::atomic::Ordering::Relaxed)
            - before,
        1
    );
    let f = crashed(f).await;
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0].1, "Parent one Parent two Parent three end");
    assert_eq!(replies[1].1, "Helper one Helper two Helper three end");
    assert!(replies.iter().all(|(_, _, partial)| *partial));
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run), replies);
    f.close().await;
}

#[tokio::test]
async fn completing_one_stream_preserves_the_other_for_restart() {
    for completed in ["parent", "helper"] {
        let pending = if completed == "parent" {
            "helper"
        } else {
            "parent"
        };
        let f = chat_fixture().await;
        let (task, run) = chat(&f, "native-1").await;
        streamed(&f, run, completed, "Finished ").await;
        streamed(&f, run, pending, "Still ").await;
        f.hub.save_replies().await.unwrap();
        streamed(&f, run, pending, "writing ").await;
        native_final(&f, task, run, completed, "Finished reply").await;
        let replies = agent_replies(&f, task, run);
        assert_eq!(replies.len(), 1);
        assert!(!replies[0].2);
        streamed(&f, run, pending, "the reply").await;
        f.hub.save_replies().await.unwrap();
        let f = crashed(f).await;
        let replies = agent_replies(&f, task, run);
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert_eq!(replies[0].1, "Finished reply");
        assert!(!replies[0].2);
        assert_eq!(replies[1].1, "Still writing the reply");
        assert!(replies[1].2);
        let f = crashed(f).await;
        assert_eq!(agent_replies(&f, task, run), replies);
        f.close().await;
    }
}

#[tokio::test]
async fn size_checkpoint_counts_all_message_keys_together() {
    let f = chat_fixture().await;
    let (_, run) = chat(&f, "native-1").await;
    streamed(&f, run, "parent", &"p".repeat(4096)).await;
    streamed(&f, run, "helper", &"h".repeat(4096)).await;
    tokio::time::timeout(Duration::from_millis(900), async {
        while reply_chunks(&f, run).len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("combined size did not save both keys");
    let chunks = reply_chunks(&f, run);
    assert_eq!(chunks[0]["text"], "p".repeat(4096));
    assert_eq!(chunks[1]["text"], "h".repeat(4096));
    f.close().await;
}

#[tokio::test]
async fn a_failed_run_checkpoint_does_not_skip_the_other_runs() {
    let f = chat_fixture().await;
    let (_, run) = chat(&f, "native-1").await;
    streamed(&f, run, "reply", "Save despite another run's error").await;
    stop_jobs(&f).await;
    // Deliberately place a missing run first, rather than depending on hash order.
    let result = f.hub.save_reply_ids(vec![RunId::new(), run]).await;
    assert!(result.is_err());
    assert_eq!(reply_chunks(&f, run).len(), 1);
    assert_eq!(
        reply_chunks(&f, run)[0]["text"],
        "Save despite another run's error"
    );
    f.close().await;
}

#[tokio::test]
async fn concurrent_keys_retry_independently_and_completion_leaves_other_bookkeeping_unchanged() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    stop_jobs(&f).await;
    let saved_run = run_of(&f, run);
    f.hub.buffer_reply(run, "a", "First ");
    f.hub.buffer_reply(run, "b", "Other ");
    let failed = f.hub.write(|tx| {
        f.hub.checkpoint_reply(tx, &saved_run)?;
        Err::<(), store::Error>(store::Error::IntegerExhausted)
    });
    assert!(failed.is_err());
    assert!(reply_chunks(&f, run).is_empty());
    // Commit both keys without acknowledging either in memory.
    f.hub
        .inner
        .db
        .lock()
        .unwrap()
        .write(f.hub.inner.clock.now(), |tx| {
            f.hub.checkpoint_reply(tx, &saved_run)
        })
        .unwrap();
    f.hub.buffer_reply(run, "a", "reply");
    f.hub.buffer_reply(run, "b", "reply");
    f.hub.save_replies().await.unwrap();
    assert_eq!(
        reply_chunks(&f, run).len(),
        4,
        "retry duplicated a saved prefix"
    );
    f.hub.buffer_reply(run, "b", " tail");
    let marker = || {
        f.hub
            .read(|r| r.require::<serde_json::Value>(crate::partial_reply::WATERMARK, run))
            .unwrap()
    };
    let other_before = marker()["messages"]["b"].clone();
    let chunks_before: Vec<_> = reply_chunks(&f, run)
        .into_iter()
        .filter(|c| c["key"] == "b")
        .collect();
    let complete = f
        .hub
        .write(|tx| {
            f.hub
                .complete_reply(tx, &saved_run, "a", "First reply".into())
        })
        .unwrap();
    assert_eq!(complete.text, "First reply");
    assert_eq!(
        marker()["messages"]["b"],
        other_before,
        "another final altered this key's progress"
    );
    assert_eq!(
        reply_chunks(&f, run),
        chunks_before,
        "another final consumed this key's chunks"
    );
    f.hub
        .write(|tx| f.hub.finish_reply(tx, &saved_run))
        .unwrap();
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1, "Other reply tail");
    assert!(replies[0].2);
    f.close().await;
}

async fn parent_finished(f: &Fixture, run: RunId) {
    f.harness
        .emit(
            run,
            ports::HarnessEvent::ParentEnded {
                exit: ports::RunExit::Completed,
            },
        )
        .await;
    let mut more = waiting();
    while run_of(f, run).ended_at.is_none() {
        assert!(more(), "the parent's end was never recorded");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_helper_still_writing_when_the_parent_finishes_is_not_shown_twice() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "helper", "Helper so ").await;
    f.hub.save_replies().await.unwrap();
    native_final(&f, task, run, "parent", "Parent done").await;
    parent_finished(&f, run).await;
    streamed(&f, run, "helper", "far").await;
    native_final(&f, task, run, "helper", "Helper so far").await;
    let replies: Vec<_> = agent_replies(&f, task, run)
        .into_iter()
        .map(|(_, text, partial)| (text, partial))
        .collect();
    assert_eq!(
        replies,
        [
            ("Parent done".to_owned(), false),
            ("Helper so far".to_owned(), false)
        ]
    );
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run).len(), 2);
    f.close().await;
}

#[tokio::test]
async fn a_helper_cut_short_by_the_process_ending_is_shown_without_a_restart() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    streamed(&f, run, "helper", "Helper so ").await;
    native_final(&f, task, run, "parent", "Parent done").await;
    parent_finished(&f, run).await;
    streamed(&f, run, "helper", "far").await;
    assert_eq!(agent_replies(&f, task, run).len(), 1);
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    let mut more = waiting();
    while agent_replies(&f, task, run).len() < 2 {
        assert!(more(), "the helper's words were never shown");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let replies = agent_replies(&f, task, run);
    assert_eq!(replies[1].1, "Helper so far");
    assert!(replies[1].2);
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, run), replies);
    f.close().await;
}

#[tokio::test]
async fn review_recovery_phone_only_observation_does_not_block_a_new_turn_or_bypass_process_review()
{
    for (processes_remain, prior_reply) in [(false, false), (false, true), (true, false)] {
        let group = Group::new();
        let f = chat_fixture().await;
        *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
        let (task, old) = chat(&f, "native-1").await;
        if prior_reply {
            native_final(&f, task, old, "earlier", "Earlier complete message.").await;
        }
        stop_jobs(&f).await;
        let text = "The phone saw this unfinished sentence before the first checkpoint";
        let (_, key, offset) = f.hub.buffer_reply(old, "reply", text);
        assert_eq!(offset, 0);
        assert!(reply_chunks(&f, old).is_empty());
        group.populated(processes_remain);
        let f = crashed(f).await;
        let durable_before = agent_replies(&f, task, old);
        assert_eq!(durable_before.len(), usize::from(prior_reply));
        assert!(durable_before.iter().all(|(_, _, partial)| !partial));
        // An older phone or already saved command can still include an
        // observation from before any checkpoint; it must not trap this turn.
        let body = serde_json::from_value(serde_json::json!({
            "command": "resume_task", "task_id": task, "text": "Finish that sentence.",
            "observed_reply": {
                "run_id": old, "message_key": key, "text_offset": 0, "text": text
            }
        }))
        .unwrap();
        let starts = f.harness.specs.lock().unwrap().len();
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                edit(body, task_of(&f, task).revision),
            )
            .await;
        if processes_remain {
            assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
            assert_eq!(receipt.error.unwrap().code, ErrorCode::Conflict);
            assert_eq!(f.harness.specs.lock().unwrap().len(), starts);
            assert_eq!(run_of(&f, old).state, RunState::ReconciliationRequired);
        } else {
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            let spec = next_spec(&f, starts).await;
            assert_eq!(spec.resume_session.as_deref(), Some("native-1"));
            assert_eq!(spec.prompt.text, told("Finish that sentence."));
            assert!(!spec.prompt.text.contains(text));
            let settled = run_of(&f, old);
            assert_eq!(settled.state, RunState::Interrupted);
            assert!(settled.carried_on.is_some() && settled.human_resolution.is_none());
        }
        assert_eq!(agent_replies(&f, task, old), durable_before);
        f.close().await;
    }
}

#[tokio::test]
async fn review_recovery_joins_the_observed_main_stream_not_the_last_recovered_helper() {
    use protocol::command::ObservedReply;
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    stop_jobs(&f).await;
    let prefix = "é🌿 Main reply is interrupted during";
    let extra = " its newest unfinished sentence";
    let (_, key, _) = f.hub.buffer_reply(old, "main", prefix);
    let (_, helper_key, _) = f
        .hub
        .buffer_reply(old, "helper", "Helper's separate partial");
    f.hub.save_replies().await.unwrap();
    f.hub.buffer_reply(old, "main", extra);
    group.populated(false);
    let f = crashed(f).await;
    let replies = agent_replies(&f, task, old);
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0].1, prefix);
    assert_eq!(replies[1].1, "Helper's separate partial");
    let valid = ObservedReply {
        run_id: old,
        message_key: key,
        text_offset: 0,
        text: format!("{prefix}{extra}"),
    };
    let mut invalid = Vec::new();
    let mut bad = valid.clone();
    bad.run_id = RunId::new();
    invalid.push(bad);
    let mut bad = valid.clone();
    bad.message_key = uuid::Uuid::now_v7().to_string();
    invalid.push(bad);
    let mut bad = valid.clone();
    bad.message_key = helper_key;
    invalid.push(bad);
    let mut bad = valid.clone();
    bad.text = "conflicting main text".into();
    invalid.push(bad);
    let mut bad = valid.clone();
    bad.text_offset = 1; // Not a UTF-8 boundary in the durable prefix.
    invalid.push(bad);
    let mut bad = valid.clone();
    bad.text_offset = prefix.len() as u64 + 1;
    invalid.push(bad);
    let starts = f.harness.specs.lock().unwrap().len();
    for observed in invalid.into_iter().map(Some).chain([Some(valid.clone())]) {
        let expected = if observed.as_ref() == Some(&valid) {
            ReceiptStatus::Accepted
        } else {
            ReceiptStatus::Rejected
        };
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                edit(
                    Command::ResumeTask {
                        task_id: task,
                        harness: None,
                        mode: None,
                        model: None,
                        effort: None,
                        text: Some("Finish that sentence.".into()),
                        bundle: None,
                        carried_plan: None,
                        read_only: false,
                        observed_reply: observed,
                    },
                    task_of(&f, task).revision,
                ),
            )
            .await;
        assert_eq!(receipt.status, expected, "{receipt:?}");
        if expected == ReceiptStatus::Rejected {
            assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
            assert_eq!(task_of(&f, task).current_run, Some(old));
            assert_eq!(run_of(&f, old).state, RunState::ReconciliationRequired);
            assert_eq!(f.harness.specs.lock().unwrap().len(), starts);
        }
    }
    let spec = next_spec(&f, starts).await;
    assert_eq!(spec.resume_session.as_deref(), Some("native-1"));
    assert!(
        spec.prompt
            .text
            .contains(&serde_json::to_string(prefix).unwrap())
    );
    assert!(
        spec.prompt
            .text
            .contains(&serde_json::to_string(&valid.text).unwrap())
    );
    assert!(!spec.prompt.text.contains("Helper's separate partial"));
    assert!(
        spec.prompt
            .text
            .contains("phone observation, not authoritative native history")
    );
    assert_eq!(agent_replies(&f, task, old), replies);
    let settled = run_of(&f, old);
    assert_eq!(settled.state, RunState::Interrupted);
    assert!(settled.carried_on.is_some() && settled.human_resolution.is_none());
    f.close().await;
}

#[tokio::test]
async fn review_recovery_omitting_an_uncheckpointed_helper_allows_the_new_turn() {
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    stop_jobs(&f).await;
    let main = "Main durable prefix.";
    let helper = "A newer helper stream with no surviving checkpoint";
    f.hub.buffer_reply(old, "main", main);
    f.hub.save_replies().await.unwrap();
    f.hub.buffer_reply(old, "helper", helper);
    group.populated(false);
    let f = crashed(f).await;
    let before = agent_replies(&f, task, old);
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].1, main);
    // The current phone deliberately omits the unanchored newest identity.
    // The automatic-send regression exercises this exact command shape.
    let body = serde_json::from_value(serde_json::json!({
        "command": "resume_task", "task_id": task, "text": "Please carry on."
    }))
    .unwrap();
    let starts = f.harness.specs.lock().unwrap().len();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(body, task_of(&f, task).revision),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let spec = next_spec(&f, starts).await;
    assert_eq!(spec.resume_session.as_deref(), Some("native-1"));
    assert!(
        spec.prompt
            .text
            .contains(&serde_json::to_string(main).unwrap())
    );
    assert!(!spec.prompt.text.contains(helper));
    assert_eq!(agent_replies(&f, task, old), before);
    let settled = run_of(&f, old);
    assert_eq!(settled.state, RunState::Interrupted);
    assert!(settled.carried_on.is_some() && settled.human_resolution.is_none());
    f.close().await;
}

#[tokio::test]
async fn review_recovery_does_not_resurrect_a_closed_identity_from_an_older_partial() {
    use protocol::command::ObservedReply;
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    stop_jobs(&f).await;
    let (_, key, _) = f.hub.buffer_reply(old, "main", "Earlier ");
    let saved = run_of(&f, old);
    f.hub
        .write(|tx| f.hub.partial_boundary(tx, &saved))
        .unwrap();
    f.hub
        .write(|tx| {
            let done = f
                .hub
                .complete_reply(tx, &saved, "main", "Earlier complete".into())?;
            tx.emit(Event::TimelineAppended {
                task_id: task,
                item: TimelineItem {
                    seq: 0,
                    at: saved.admitted_at,
                    run_id: Some(old),
                    item: TimelineBody::AgentMessage {
                        text: done.text,
                        partial: false,
                        message_key: done.message_key,
                        text_offset: Some(done.text_offset),
                    },
                },
            })?;
            Ok(())
        })
        .unwrap();
    f.hub
        .buffer_reply(old, "helper", "Separate interrupted helper");
    f.hub.save_replies().await.unwrap();
    group.populated(false);
    let f = crashed(f).await;
    let starts = f.harness.specs.lock().unwrap().len();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::ResumeTask {
                    task_id: task,
                    harness: None,
                    mode: None,
                    model: None,
                    effort: None,
                    text: Some("Carry on".into()),
                    bundle: None,
                    carried_plan: None,
                    read_only: false,
                    observed_reply: Some(ObservedReply {
                        run_id: old,
                        message_key: key,
                        text_offset: 0,
                        text: "Earlier complete but wrongly treated as unfinished".into(),
                    }),
                },
                task_of(&f, task).revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
    assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
    assert_eq!(f.harness.specs.lock().unwrap().len(), starts);
    assert_eq!(run_of(&f, old).state, RunState::ReconciliationRequired);
    f.close().await;
}

#[tokio::test]
async fn carry_on_freezes_exact_durable_and_newer_phone_observed_endings_after_a_hard_crash() {
    use protocol::command::ObservedReply;
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    stop_jobs(&f).await;
    let prefix = "41. The crescent-moon pond held visitors’ fond";
    let extra = " memories.\n42. Pears protect their delicate sweetness from";
    let (_, key, offset) = f.hub.buffer_reply(old, "reply", prefix);
    assert_eq!(offset, 0);
    f.hub.save_replies().await.unwrap();
    let (_, same, offset) = f.hub.buffer_reply(old, "reply", extra);
    assert_eq!(same, key);
    assert_eq!(offset, prefix.len() as u64);
    group.populated(false);
    let f = crashed(f).await;
    assert_eq!(agent_replies(&f, task, old)[0].1, prefix);
    assert!(run_of(&f, old).nothing_running);
    let text = "Carry on. Finish the interrupted sentence and add just one more sentence.";
    let mut command = edit(
        Command::ResumeTask {
            task_id: task,
            harness: None,
            mode: None,
            model: None,
            effort: None,
            text: Some(text.into()),
            bundle: None,
            carried_plan: None,
            read_only: false,
            observed_reply: Some(ObservedReply {
                run_id: old,
                message_key: key.clone(),
                text_offset: 0,
                text: format!("{prefix}{extra}"),
            }),
        },
        task_of(&f, task).revision,
    );
    let starts = f.harness.specs.lock().unwrap().len();
    let receipt = f.hub.submit(&Caller::local_owner(), command.clone()).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let spec = next_spec(&f, starts).await;
    assert_eq!(spec.resume_session.as_deref(), Some("native-1"));
    assert!(spec.prompt.text.starts_with(crate::cut_off::CUT_OFF_NOTE));
    assert!(
        spec.prompt
            .text
            .contains(&serde_json::to_string(prefix).unwrap())
    );
    assert!(
        spec.prompt
            .text
            .contains(&serde_json::to_string(&format!("{prefix}{extra}")).unwrap())
    );
    assert!(
        spec.prompt
            .text
            .contains("phone observation, not authoritative native history")
    );
    assert!(
        spec.prompt
            .text
            .contains("Do not restart the opening or repeat the quoted words")
    );
    assert!(spec.prompt.text.ends_with(text));
    let frozen: String = f
        .hub
        .read(|r| r.require(crate::cut_off::CONTEXT, spec.run_id))
        .unwrap();
    assert!(spec.prompt.text.starts_with(&frozen));
    let duplicate = f.hub.submit(&Caller::local_owner(), command.clone()).await;
    assert_eq!(duplicate.status, ReceiptStatus::Duplicate);
    // A retry cannot replace the frozen observation under the same id.
    if let Command::ResumeTask {
        observed_reply: Some(observed),
        ..
    } = &mut command.body
    {
        observed.text.push_str(" replaced");
    }
    assert_eq!(
        f.hub.submit(&Caller::local_owner(), command).await.status,
        ReceiptStatus::Rejected
    );
    assert_eq!(
        f.hub
            .read(|r| r.require::<String>(crate::cut_off::CONTEXT, spec.run_id))
            .unwrap(),
        frozen
    );
    let settled = run_of(&f, old);
    assert_eq!(settled.state, RunState::Interrupted);
    assert!(settled.human_resolution.is_none() && settled.carried_on.is_some());
    assert_eq!(
        agent_replies(&f, task, old)[0].1,
        prefix,
        "phone text never rewrites durable native output"
    );
    f.close().await;
}

#[tokio::test]
async fn recovery_rejects_unanchored_wrong_conflicting_and_oversized_phone_text_without_starting() {
    use protocol::command::ObservedReply;
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    stop_jobs(&f).await;
    let prefix = "é🌿 the pond held";
    let (_, key, _) = f.hub.buffer_reply(old, "reply", prefix);
    f.hub.save_replies().await.unwrap();
    group.populated(false);
    let f = crashed(f).await;
    let valid = ObservedReply {
        run_id: old,
        message_key: key,
        text_offset: 0,
        text: format!("{prefix} silver"),
    };
    let mut cases = Vec::new();
    let mut bad = valid.clone();
    bad.run_id = RunId::new();
    cases.push(bad);
    let mut bad = valid.clone();
    bad.message_key = uuid::Uuid::now_v7().to_string();
    cases.push(bad);
    let mut bad = valid.clone();
    bad.text = "different opening".into();
    cases.push(bad);
    let mut bad = valid.clone();
    bad.text_offset = 1;
    cases.push(bad); // middle of é
    let mut bad = valid.clone();
    bad.text_offset = prefix.len() as u64 + 1;
    cases.push(bad);
    let mut bad = valid.clone();
    bad.text = "x".repeat(8193);
    cases.push(bad);
    let mut bad = valid.clone();
    bad.text_offset = u64::MAX;
    cases.push(bad);
    let starts = f.harness.specs.lock().unwrap().len();
    for observed in cases {
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                edit(
                    Command::ResumeTask {
                        task_id: task,
                        harness: None,
                        mode: None,
                        model: None,
                        effort: None,
                        text: Some("Carry on".into()),
                        bundle: None,
                        carried_plan: None,
                        read_only: false,
                        observed_reply: Some(observed),
                    },
                    task_of(&f, task).revision,
                ),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        assert_eq!(task_of(&f, task).current_run, Some(old));
        assert_eq!(run_of(&f, old).state, RunState::ReconciliationRequired);
    }
    assert_eq!(f.harness.specs.lock().unwrap().len(), starts);
    // A UTF-8-aligned suffix with matching overlap is useful, even if the
    // phone joined after the start of this message.
    let suffix = ObservedReply {
        text_offset: "é".len() as u64,
        text: "🌿 the pond held silver".to_owned(),
        ..valid
    };
    let current = task_of(&f, task);
    let context = f
        .hub
        .read(|r| crate::cut_off::context(r, &current, Some(&suffix)))
        .unwrap();
    assert!(context.contains("🌿 the pond held silver"));
    f.close().await;
}

#[tokio::test]
async fn carry_on_without_a_phone_cache_includes_the_exact_bounded_durable_tail() {
    let group = Group::new();
    let f = chat_fixture().await;
    *f.harness.controls.run_group.lock().unwrap() = Some(group.dir.clone());
    let (task, old) = chat(&f, "native-1").await;
    stop_jobs(&f).await;
    let long = format!("{}41. The pond held visitors’ fond", "é🌿".repeat(4000));
    f.hub.buffer_reply(old, "reply", &long);
    f.hub.save_replies().await.unwrap();
    group.populated(false);
    let f = crashed(f).await;
    let starts = f.harness.specs.lock().unwrap().len();
    assert_eq!(
        reply(&f, task, "Finish that sentence.").await.status,
        ReceiptStatus::Accepted
    );
    let spec = next_spec(&f, starts).await;
    assert!(
        spec.prompt
            .text
            .contains("41. The pond held visitors’ fond")
    );
    assert!(!spec.prompt.text.contains("phone reports"));
    assert!(spec.prompt.text.len() < 8192 + 2000);
    assert!(spec.prompt.text.ends_with("Finish that sentence."));
    f.close().await;
}

#[tokio::test]
async fn partial_and_final_segments_keep_unicode_offsets_and_native_key_reuse_gets_new_identity() {
    let f = chat_fixture().await;
    let (task, run) = chat(&f, "native-1").await;
    stop_jobs(&f).await;
    let prefix = "é🌿 ";
    let (_, first, zero) = f.hub.buffer_reply(run, "reused", prefix);
    assert_eq!(zero, 0);
    let saved = run_of(&f, run);
    f.hub
        .write(|tx| f.hub.partial_boundary(tx, &saved))
        .unwrap();
    let (_, same, offset) = f.hub.buffer_reply(run, "reused", "pond");
    assert_eq!(same, first);
    assert_eq!(offset, prefix.len() as u64);
    let completed = f
        .hub
        .write(|tx| {
            f.hub
                .complete_reply(tx, &saved, "reused", format!("{prefix}pond"))
        })
        .unwrap();
    assert_eq!(completed.text, "pond");
    assert_eq!(completed.text_offset, prefix.len() as u64);
    assert_eq!(completed.message_key.as_deref(), Some(first.as_str()));
    let (_, second, zero) = f.hub.buffer_reply(run, "reused", "New");
    assert_ne!(
        first, second,
        "native key reuse must not reuse stream identity"
    );
    assert_eq!(zero, 0);
    f.hub
        .write(|tx| f.hub.partial_boundary(tx, &saved))
        .unwrap();
    let completed = f
        .hub
        .write(|tx| f.hub.complete_reply(tx, &saved, "reused", "New".into()))
        .unwrap();
    assert!(
        completed.text.is_empty(),
        "an already-journaled final still closes the identified stream"
    );
    assert_eq!(completed.text_offset, 3);
    let items = f.hub.read(|r| r.timeline(task, None, 200)).unwrap().0;
    let ranges: Vec<_> = items
        .into_iter()
        .filter_map(|item| match item.item {
            TimelineBody::AgentMessage {
                text,
                message_key,
                text_offset,
                ..
            } => Some((text, message_key, text_offset)),
            _ => None,
        })
        .collect();
    assert_eq!(ranges[0], (prefix.into(), Some(first), Some(0)));
    assert_eq!(ranges[1], ("New".into(), Some(second), Some(0)));
    f.close().await;
}
