// SPDX-License-Identifier: Apache-2.0
use crate::{Caller, CompanionHub, Hub};
use protocol::{command::*, model::*, *};
use std::{os::fd::AsRawFd, path::Path, sync::Arc, time::Duration};

fn fixture_dir() -> tempfile::TempDir {
    fixture_dir_in(&std::env::temp_dir())
}

fn fixture_dir_in(parent: &Path) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("force-stop-")
        .tempdir_in(parent)
        .unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();
    root
}

fn bind_gate(dir: &Path, name: &str) -> std::io::Result<tokio::net::UnixListener> {
    // This module is Linux-only. Resolve the directory through its descriptor
    // so sockaddr_un contains a short path even when TMPDIR is very long.
    // The socket inode stays in the fixture directory, and the child connects
    // using its relative name. Never change the test process's shared cwd.
    let directory = std::fs::File::open(dir)?;
    tokio::net::UnixListener::bind(format!("/proc/self/fd/{}/{name}", directory.as_raw_fd()))
}

async fn setup_or_shutdown<T: Send + 'static>(
    hub: &CompanionHub,
    setup: impl std::future::Future<Output = T> + Send + 'static,
) -> T {
    // Catch setup panics in a task while the runtime is still available to
    // stop the native process group, then preserve the original failure.
    match tokio::spawn(setup).await {
        Ok(value) => value,
        Err(error) => {
            let _ = hub.shutdown().await;
            std::panic::resume_unwind(error.into_panic());
        }
    }
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture did not settle");
}
fn live(pid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        !s.rsplit_once(')')
            .unwrap()
            .1
            .trim_start()
            .starts_with(['Z', 'X'])
    })
}
async fn fixture(
    harness: capability::HarnessKind,
) -> (tempfile::TempDir, CompanionHub, ProjectId, TaskId) {
    fixture_in(harness, fixture_dir(), "wait").await
}
async fn fixture_in(
    harness: capability::HarnessKind,
    root: tempfile::TempDir,
    prompt: &str,
) -> (tempfile::TempDir, CompanionHub, ProjectId, TaskId) {
    let dir = root.path().join("project");
    let binary = dir.join("fixture");
    let script = match harness {
        capability::HarnessKind::ClaudeCode => include_str!("../../harness-claude/tests/fake_claude.py").to_owned(),
        capability::HarnessKind::Codex => include_str!("../../harness-codex/tests/fake_codex.py")
            .replace("import json", "import json\nimport os\nimport subprocess")
            .replace("        turn_number += 1", "        Path(\"fixture.pid\").write_text(str(os.getpid()))\n        turn_number += 1"),
        _ => panic!("fixture supports Claude and Codex"),
    };
    // Existence signals readiness, so publish only after the PID is complete.
    // The optional gate lets the regression hold the writer just after create.
    let script = script.replacen(
        "Path(\"fixture.pid\").write_text(str(os.getpid()))",
        r#"Path("fixture.pid").write_text(str(os.getpid()))
        child = subprocess.Popen(["sleep", "120"])
        with Path("child.pid.tmp").open("w") as pid_file:
            gate("child-pid-created.sock")
            pid_file.write(str(child.pid))
        Path("child.pid.tmp").replace("child.pid")"#,
        1,
    );
    let script = script.replace("gate(\"boundary.sock\")\nsys.exit", "gate(\"boundary.sock\")\nif \"child\" in globals():\n    child.terminate()\n    child.wait()\nsys.exit");
    test_support::executable(&binary, script).unwrap();
    std::fs::write(dir.join("ignore-interrupt"), "").unwrap();
    let hub = CompanionHub::builder()
        .data_dir(root.path().join("data"))
        .harness(match harness {
            capability::HarnessKind::ClaudeCode => {
                Arc::new(harness_claude::ClaudeAdapter::with_binary(binary))
                    as Arc<dyn ports::HarnessAdapter>
            }
            capability::HarnessKind::Codex => {
                Arc::new(harness_codex::CodexAdapter::with_binary(binary))
            }
            _ => panic!("fixture supports Claude and Codex"),
        })
        .bridge(crate::bridge::BridgeConfig::new(
            "fixture-bridge",
            "fixture-socket",
        ))
        .build()
        .await
        .unwrap();
    let starting_hub = hub.clone();
    let prompt = prompt.to_owned();
    let (project_id, task_id) = setup_or_shutdown(&hub, async move {
        let hub = starting_hub;
        let owner = Caller::local_owner();
        let added = hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::AddProject {
                    host_path: dir.to_string_lossy().into(),
                    name: Some("Force stop fixture".into()),
                }),
            )
            .await;
        let Some(CommandResult::ProjectAdded { project_id }) = added.result else {
            panic!("{added:?}")
        };
        let created = hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::CreateTask {
                    project_id,
                    kind: TaskKind::Coding,
                    mode: None,
                    title: Some("Ignore interrupt".into()),
                    prompt,
                    bundle: None,
                    harness,
                    model: None,
                    effort: None,
                    mission: None,
                    target_id: None,
                    placement: TaskPlacement::StartNow,
                    launch_policy: None,
                    branch: None,
                    start: None,
                    place: None,
                    browser: None,
                    conversation: None,
                    access: None,
                    github: None,
                    agent_chosen: None,
                    review: None,
                }),
            )
            .await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = created.result else {
            panic!("{created:?}")
        };
        until(|| dir.join("child.pid").exists()).await;
        (project_id, task_id)
    })
    .await;
    (root, hub, project_id, task_id)
}

#[tokio::test]
async fn claude_child_pid_is_published_only_after_write() {
    child_pid_publication_fixture(capability::HarnessKind::ClaudeCode).await;
}

#[tokio::test]
async fn codex_child_pid_is_published_only_after_write() {
    child_pid_publication_fixture(capability::HarnessKind::Codex).await;
}

async fn child_pid_publication_fixture(harness: capability::HarnessKind) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = fixture_dir();
    let dir = root.path().join("project");
    let gate = bind_gate(&dir, "child-pid-created.sock").unwrap();
    let starting = tokio::spawn(fixture_in(harness, root, "wait"));
    let (mut writer, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ready = [0; 5];
    writer.read_exact(&mut ready).await.unwrap();
    assert_eq!(&ready, b"ready");
    // The writer has created its file but cannot write until we release it.
    let prematurely_published = dir.join("child.pid").try_exists().unwrap();
    writer.write_all(b"x").await.unwrap();
    let (_root, hub, _, _) = starting.await.unwrap();
    let parent = std::fs::read_to_string(dir.join("fixture.pid")).unwrap();
    let child = std::fs::read_to_string(dir.join("child.pid")).unwrap();
    assert!(parent.parse::<u32>().unwrap() > 0);
    assert!(child.parse::<u32>().unwrap() > 0);
    assert!(live(&parent) && live(&child));
    hub.shutdown().await.unwrap();
    until(|| !live(&parent) && !live(&child)).await;
    // Clean up the fixture before reporting a failed publication assertion.
    assert!(
        !prematurely_published,
        "{harness:?}: child.pid was visible before the PID was written"
    );
}

#[tokio::test]
async fn force_stop_ends_owned_fixture_group_and_releases_writer() {
    for harness in [
        capability::HarnessKind::ClaudeCode,
        capability::HarnessKind::Codex,
    ] {
        force_stop_fixture(harness).await;
    }
}

async fn force_stop_fixture(harness: capability::HarnessKind) {
    let (root, hub, project_id, task_id) = fixture(harness).await;
    let dir = root.path().join("project");
    let owner = Caller::local_owner();
    until(|| dir.join("child.pid").exists()).await;
    let parent = std::fs::read_to_string(dir.join("fixture.pid")).unwrap();
    let child = std::fs::read_to_string(dir.join("child.pid")).unwrap();
    assert!(live(&parent) && live(&child));
    let held = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SendMessage {
                task_id,
                text: "held until stop".into(),
                bundle: None,
                delivery: MessageDelivery::NextTurn,
            }),
        )
        .await;
    assert_eq!(held.status, ReceiptStatus::Accepted, "{held:?}");
    let timeline = || {
        hub.read(|r| r.list::<TimelineItem>(&format!("timeline:{task_id}")))
            .unwrap()
    };
    // The adapter holds the follow-up for the turn boundary.
    let held = || {
        let items = timeline();
        let seq = items
            .iter()
            .find(|i| {
                matches!(&i.item,
                    TimelineBody::UserMessage { text, .. } if text == "held until stop"
                )
            })?
            .seq;
        items.iter().any(|i| matches!(i.item,
            TimelineBody::Delivery { message_seq, state: DeliveryState::QueuedForBoundary, .. } if message_seq == seq
        )).then_some(seq)
    };
    until(|| held().is_some()).await;
    let held = held().unwrap();
    let stopped = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::StopTask {
                task_id,
                force: false,
            }),
        )
        .await;
    assert_eq!(stopped.status, ReceiptStatus::Accepted);
    let queue: protocol::queue::QueueView = hub.read(|r| r.require("queue", project_id)).unwrap();
    assert!(queue.paused);
    assert!(
        queue
            .pause_reason
            .unwrap()
            .contains("Next up paused when you stopped")
    );
    until(|| {
        std::fs::read_to_string(dir.join("stdin.jsonl")).is_ok_and(|s| s.contains("interrupt"))
    })
    .await;
    hub.submit(
        &owner,
        CommandEnvelope::new(Command::StopTask {
            task_id,
            force: true,
        }),
    )
    .await;
    until(|| {
        hub.read(|r| r.require::<Task>("task", task_id))
            .unwrap()
            .state
            == TaskState::Interrupted
    })
    .await;
    until(|| !live(&parent) && !live(&child)).await;
    let task: Task = hub.read(|r| r.require("task", task_id)).unwrap();
    let run: Run = hub
        .read(|r| r.require("run", task.current_run.unwrap()))
        .unwrap();
    let project: Project = hub.read(|r| r.require("project", project_id)).unwrap();
    assert!(!run.holds_writer);
    assert!(project.active_writer.is_none());
    until(|| hub.read(|r| r.list::<TimelineItem>(&format!("timeline:{task_id}"))).unwrap().iter().any(|item|
        matches!(&item.item, TimelineBody::Receipt { receipt } if receipt.facts.iter().any(|fact| fact.label == "Agent process ended on the computer" && fact.status == FactStatus::Done)))).await;
    // Stop withdrew the held follow-up before the run's end was recorded.
    let history = timeline();
    let settled: Vec<_> = history
        .iter()
        .filter_map(|i| match i.item {
            TimelineBody::Delivery {
                message_seq,
                state,
                withdrawn_by_stop,
                ..
            } if message_seq == held && state != DeliveryState::QueuedForBoundary => {
                Some((i.seq, state, withdrawn_by_stop))
            }
            _ => None,
        })
        .collect();
    let ended = history
        .iter()
        .find(|i| matches!(i.item, TimelineBody::RunEnded { .. }))
        .unwrap()
        .seq;
    assert!(
        matches!(settled[..], [(seq, DeliveryState::Cancelled, true)] if seq < ended),
        "{settled:?}"
    );
    let input = std::fs::read_to_string(dir.join("stdin.jsonl")).unwrap();
    assert!(!input.contains("held until stop"));
    hub.shutdown().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn stop_ends_what_the_agent_left_running_and_the_receipt_says_so() {
    for harness in [
        capability::HarnessKind::ClaudeCode,
        capability::HarnessKind::Codex,
    ] {
        stop_detached_fixture(harness).await;
    }
}

#[cfg(target_os = "linux")]
async fn stop_detached_fixture(harness: capability::HarnessKind) {
    use test_support::detached;
    let root = fixture_dir();
    let dir = root.path().join("project");
    std::fs::write(dir.join("detach.sh"), detached::SCRIPT).unwrap();
    let (_root, hub, _, task_id) = fixture_in(harness, root, "detach wait").await;
    // This Stop is answered.
    std::fs::remove_file(dir.join("ignore-interrupt")).unwrap();
    let children = detached::children(&dir);
    let stopped = hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::StopTask {
                task_id,
                force: false,
            }),
        )
        .await;
    assert_eq!(stopped.status, ReceiptStatus::Accepted);
    let fact = || {
        hub.read(|r| r.list::<TimelineItem>(&format!("timeline:{task_id}")))
            .unwrap()
            .into_iter()
            .find_map(|item| match item.item {
                TimelineBody::Receipt { receipt } => receipt
                    .facts
                    .into_iter()
                    .find(|fact| fact.label == "Everything the agent started"),
                _ => None,
            })
    };
    until(|| fact().is_some()).await;
    let fact = fact().unwrap();
    println!("{harness:?}: {fact:?}");
    // A receipt that says everything ended is only true if it did, and one
    // that says some may still run names exactly those that left the group.
    detached::assert_ended(fact.status == FactStatus::Done, &children);
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn hub_shutdown_closes_stdin_before_killing_claude() {
    hub_shutdown_fixture(capability::HarnessKind::ClaudeCode, fixture_dir()).await;
}

#[tokio::test]
async fn hub_shutdown_closes_stdin_before_killing_codex() {
    hub_shutdown_fixture(capability::HarnessKind::Codex, fixture_dir()).await;
}

async fn hub_shutdown_fixture(harness: capability::HarnessKind, root: tempfile::TempDir) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = root.path().join("project");
    // Bind before starting native processes, including on setup failure.
    let listener = bind_gate(&dir, "boundary.sock").unwrap();
    let (_root, hub, _, _) = fixture_in(harness, root, "wait").await;
    let parent = std::fs::read_to_string(dir.join("fixture.pid")).unwrap();
    let child = std::fs::read_to_string(dir.join("child.pid")).unwrap();
    let started = std::time::Instant::now();
    let mut shutdown = tokio::spawn(async move { hub.shutdown().await });
    // Shutdown is itself bounded, so this limit only catches a hang.
    let accepted = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::select! {
            biased;
            accepted = listener.accept() => Some(accepted.unwrap()),
            result = &mut shutdown => {
                result.unwrap().unwrap();
                None
            }
        }
    })
    .await
    .expect("hub shutdown must finish");
    let Some((mut connection, _)) = accepted else {
        // A fixture too slow to report within the adapters' three-second grace
        // is killed before it connects. Its marker still shows that stdin
        // closed while it was alive.
        assert!(
            dir.join("stdin-closed").exists(),
            "hub must close stdin before killing the fixture"
        );
        until(|| !live(&parent) && !live(&child)).await;
        return;
    };
    let mut ready = [0; 5];
    connection.read_exact(&mut ready).await.unwrap();
    assert_eq!(&ready, b"ready");
    assert!(dir.join("stdin-closed").exists());
    // The grace starts after `started`, so a report well inside three seconds
    // of it must find both processes alive. A machine too loaded to report in
    // time can only show the grace ending, which is not a failure.
    let alive = live(&parent) && live(&child);
    let early = started.elapsed() < Duration::from_millis(2500);
    assert!(alive || !early, "grace period must allow native cleanup");
    if early {
        assert!(!shutdown.is_finished());
    }
    let _ = connection.write_all(b"x").await;
    shutdown.await.unwrap().unwrap();
    until(|| !live(&parent) && !live(&child)).await;
}

// Stop is accepted (run Stopping, receipt pending) before the adapter is
// told anything. `stop_task` awaits `revoke_run`, which waits for every
// computer-control target lock, before it sends the interrupt. A turn that
// ends inside that window must not hand the held follow-up to the agent after
// the person pressed Stop.
#[tokio::test]
async fn claude_held_message_is_not_delivered_after_stop_is_accepted() {
    held_message_after_stop_fixture(capability::HarnessKind::ClaudeCode).await;
}

#[tokio::test]
async fn codex_held_message_is_not_delivered_after_stop_is_accepted() {
    held_message_after_stop_fixture(capability::HarnessKind::Codex).await;
}

async fn held_message_after_stop_fixture(harness: capability::HarnessKind) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = fixture_dir();
    let dir = root.path().join("project");
    // The first turn stays open until the test lets it finish.
    let turn = bind_gate(&dir, "turn.sock").unwrap();
    let (_root, hub, _, task_id) = fixture_in(harness, root, "complete").await;
    let owner = Caller::local_owner();
    let (mut turn_end, _) = tokio::time::timeout(Duration::from_secs(10), turn.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ready = [0; 5];
    turn_end.read_exact(&mut ready).await.unwrap();
    std::fs::remove_file(dir.join("turn.sock")).unwrap();
    let sent = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SendMessage {
                task_id,
                text: "held until stop".into(),
                bundle: None,
                delivery: MessageDelivery::NextTurn,
            }),
        )
        .await;
    assert_eq!(sent.status, ReceiptStatus::Accepted, "{sent:?}");
    let timeline = || {
        hub.read(|r| r.list::<TimelineItem>(&format!("timeline:{task_id}")))
            .unwrap()
    };
    let held = || {
        let items = timeline();
        let seq = items
            .iter()
            .find(|i| {
                matches!(&i.item,
                    TimelineBody::UserMessage { text, .. } if text == "held until stop"
                )
            })?
            .seq;
        items.iter().any(|i| matches!(i.item,
            TimelineBody::Delivery { message_seq, state: DeliveryState::QueuedForBoundary, .. } if message_seq == seq
        )).then_some(seq)
    };
    until(|| held().is_some()).await;
    let held = held().unwrap();
    // A computer-control target is busy, as during a slow observation.
    let busy = hub.inner.controls.lock().await;
    let stopping = {
        let hub = hub.clone();
        tokio::spawn(async move {
            hub.submit(
                &owner,
                CommandEnvelope::new(Command::StopTask {
                    task_id,
                    force: false,
                }),
            )
            .await
        })
    };
    let run = || {
        let task: Task = hub.read(|r| r.require("task", task_id)).unwrap();
        hub.read(|r| r.require::<Run>("run", task.current_run.unwrap()))
            .unwrap()
    };
    until(|| run().state == RunState::Stopping).await;
    // Stop is accepted. The native turn now ends on its own.
    turn_end.write_all(b"x").await.unwrap();
    let settled = || {
        timeline().iter().find_map(|i| match i.item {
            TimelineBody::Delivery {
                message_seq,
                state,
                withdrawn_by_stop,
                ..
            } if message_seq == held && state != DeliveryState::QueuedForBoundary => {
                Some((state, withdrawn_by_stop))
            }
            _ => None,
        })
    };
    until(|| settled().is_some()).await;
    let outcome = settled();
    let input = std::fs::read_to_string(dir.join("stdin.jsonl")).unwrap_or_default();
    drop(busy);
    let _ = tokio::time::timeout(Duration::from_secs(20), stopping).await;
    hub.shutdown().await.unwrap();
    assert_eq!(
        outcome,
        Some((DeliveryState::Cancelled, true)),
        "{harness:?}: held message reached the agent after Stop was accepted"
    );
    assert!(!input.contains("held until stop"), "{harness:?}: {input}");
}

#[tokio::test]
async fn claude_held_message_is_withdrawn_when_force_stop_waits() {
    held_message_after_force_stop_fixture(capability::HarnessKind::ClaudeCode, fixture_dir()).await;
}

#[tokio::test]
async fn codex_held_message_is_withdrawn_when_force_stop_waits() {
    held_message_after_force_stop_fixture(capability::HarnessKind::Codex, fixture_dir()).await;
}

/// Force stop waits on the same computer-control locks before it terminates
/// the agent. The turn ends in that wait; the held follow-up is withdrawn and
/// recorded so before the run's end.
async fn held_message_after_force_stop_fixture(
    harness: capability::HarnessKind,
    root: tempfile::TempDir,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = root.path().join("project");
    let turn = bind_gate(&dir, "turn.sock").unwrap();
    let (_root, hub, _, task_id) = fixture_in(harness, root, "complete").await;
    let owner = Caller::local_owner();
    let (mut turn_end, _) = tokio::time::timeout(Duration::from_secs(10), turn.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ready = [0; 5];
    turn_end.read_exact(&mut ready).await.unwrap();
    std::fs::remove_file(dir.join("turn.sock")).unwrap();
    let sent = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SendMessage {
                task_id,
                text: "held until force stop".into(),
                bundle: None,
                delivery: MessageDelivery::NextTurn,
            }),
        )
        .await;
    assert_eq!(sent.status, ReceiptStatus::Accepted, "{sent:?}");
    let timeline = || {
        hub.read(|r| r.list::<TimelineItem>(&format!("timeline:{task_id}")))
            .unwrap()
    };
    let held = || {
        let items = timeline();
        let seq = items
            .iter()
            .find(|i| {
                matches!(&i.item,
                    TimelineBody::UserMessage { text, .. } if text == "held until force stop"
                )
            })?
            .seq;
        items.iter().any(|i| matches!(i.item,
            TimelineBody::Delivery { message_seq, state: DeliveryState::QueuedForBoundary, .. } if message_seq == seq
        )).then_some(seq)
    };
    until(|| held().is_some()).await;
    let held = held().unwrap();
    let busy = hub.inner.controls.lock().await;
    let stopping = {
        let hub = hub.clone();
        tokio::spawn(async move {
            hub.submit(
                &owner,
                CommandEnvelope::new(Command::StopTask {
                    task_id,
                    force: true,
                }),
            )
            .await
        })
    };
    let run = || {
        let task: Task = hub.read(|r| r.require("task", task_id)).unwrap();
        hub.read(|r| r.require::<Run>("run", task.current_run.unwrap()))
            .unwrap()
    };
    until(|| run().state == RunState::Stopping).await;
    turn_end.write_all(b"x").await.unwrap();
    let settled = |history: &[TimelineItem]| -> Vec<_> {
        history
            .iter()
            .filter_map(|i| match i.item {
                TimelineBody::Delivery {
                    message_seq,
                    state,
                    withdrawn_by_stop,
                    ..
                } if message_seq == held && state != DeliveryState::QueuedForBoundary => {
                    Some((i.seq, state, withdrawn_by_stop))
                }
                _ => None,
            })
            .collect()
    };
    until(|| !settled(&timeline()).is_empty()).await;
    drop(busy);
    tokio::time::timeout(Duration::from_secs(20), stopping)
        .await
        .unwrap()
        .unwrap();
    let ended = || {
        timeline()
            .iter()
            .find(|i| matches!(i.item, TimelineBody::RunEnded { .. }))
            .map(|i| i.seq)
    };
    until(|| ended().is_some()).await;
    let history = timeline();
    let ended = ended().unwrap();
    let state = run().state;
    let input = std::fs::read_to_string(dir.join("stdin.jsonl")).unwrap_or_default();
    hub.shutdown().await.unwrap();
    assert_eq!(state, RunState::Interrupted, "{harness:?}");
    assert!(
        matches!(settled(&history)[..], [(seq, DeliveryState::Cancelled, true)] if seq < ended),
        "{harness:?}: {:?}",
        settled(&history)
    );
    assert!(
        !input.contains("held until force stop"),
        "{harness:?}: {input}"
    );
}

#[tokio::test]
async fn socket_fixtures_work_below_long_temporary_paths() {
    let scratch = tempfile::tempdir().unwrap();
    let long = scratch.path().join("long-temporary-directory-".repeat(6));
    std::fs::create_dir(&long).unwrap();
    assert!(long.as_os_str().len() > 108);
    for harness in [
        capability::HarnessKind::ClaudeCode,
        capability::HarnessKind::Codex,
    ] {
        hub_shutdown_fixture(harness, fixture_dir_in(&long)).await;
        held_message_after_force_stop_fixture(harness, fixture_dir_in(&long)).await;
    }
    // Sockets, executables, and hub data all stay under the chosen parent and
    // are removed with their fixture, including when gate setup fails.
    {
        let root = fixture_dir_in(&long);
        let dir = root.path().join("project");
        let _listener = bind_gate(&dir, "turn.sock").unwrap();
        assert!(dir.join("turn.sock").exists());
        assert_eq!(
            bind_gate(&dir, "turn.sock").unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse
        );
    }
    assert!(std::fs::read_dir(&long).unwrap().next().is_none());
}

#[tokio::test]
async fn failed_setup_shuts_down_native_fixtures_and_removes_artifacts() {
    for harness in [
        capability::HarnessKind::ClaudeCode,
        capability::HarnessKind::Codex,
    ] {
        let (root, hub, _, _) = fixture(harness).await;
        let path = root.path().to_owned();
        let dir = path.join("project");
        let parent = std::fs::read_to_string(dir.join("fixture.pid")).unwrap();
        let child = std::fs::read_to_string(dir.join("child.pid")).unwrap();
        assert!(live(&parent) && live(&child));
        let failure = tokio::spawn(async move {
            let _root = root;
            setup_or_shutdown(&hub, async {
                panic!("injected fixture setup failure");
            })
            .await;
        })
        .await
        .unwrap_err();
        let panic = failure.into_panic();
        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"injected fixture setup failure")
        );
        until(|| !live(&parent) && !live(&child)).await;
        assert!(!path.exists(), "fixture artifacts survived failed setup");
    }
}
