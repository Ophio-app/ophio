// SPDX-License-Identifier: Apache-2.0
//! The restricted computer specialist's private pause on Windows, where the
//! run's job holds its whole process tree.
use crate::private::unverified;
use ports::{HarnessError, RunControl};
use std::time::Duration;

impl crate::Control {
    pub(crate) async fn pause_windows(&self) -> Result<(), HarnessError> {
        let killed = self.container.kill();
        // Always close the actor, including when termination fails. Admission
        // was permanently fenced by pause_for_private before entering here.
        let joined = tokio::time::timeout(Duration::from_secs(8), self.shutdown()).await;
        killed?;
        joined
            .map_err(|_| unverified("Native session pause timed out"))?
            .map_err(|_| unverified("Native session completion could not be established"))?;
        self.container.end().await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{ClaudeAdapter, Control, MessageDelivery, MessageRef, PromptInput, RunSpec};
    use std::{
        collections::{HashMap, VecDeque},
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        path::Path,
        process::Stdio,
        sync::Arc,
    };
    use tokio::{
        process::Command,
        sync::{mpsc, watch},
    };
    use windows_sys::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::Threading::{
            CREATE_NO_WINDOW, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            WaitForSingleObject,
        },
    };

    // Also supplies the platform-independent delegation contract test with a
    // spec. This helper never invokes a native harness executable.
    pub(crate) fn fixture() -> (tempfile::TempDir, ClaudeAdapter, RunSpec) {
        let dir = tempfile::tempdir().unwrap();
        let spec = RunSpec {
            run_id: protocol::RunId::new(),
            task_id: protocol::TaskId::new(),
            mode: protocol::model::TaskMode::Work,
            cwd: dir.path().to_owned(),
            prompt: PromptInput::default(),
            model: None,
            effort: None,
            launch_policy: None,
            resume_session: None,
            resume_from_mode: None,
            mission: None,
            branch: None,
            start: None,
            bridge: Some(ports::ToolBridge {
                server_name: "computer".into(),
                command: "/fixture/computer-mcp".into(),
                args: vec![],
                env: vec![
                    (ports::bridge::RUN_TOKEN_ENV.into(), "fixture-secret".into()),
                    (ports::bridge::SOCKET_ENV.into(), "/fixture/socket".into()),
                ],
                permission_tool: None,
                computer_use: true,
                browser: false,
                github: false,
                checklist_answers: false,
            }),
            project_settings: false,
            env: Default::default(),
            github: None,
            scratch: None,
            questions: true,
            sandbox: None,
            web: false,
            disabled: Default::default(),
        };
        (dir, ClaudeAdapter::with_binary("unused-fixture.exe"), spec)
    }

    /// An inert subprocess of this test executable, never a real agent session.
    #[test]
    fn job_fixture() {
        let Ok(role) = std::env::var("OPHIO_JOB_TEST_ROLE") else {
            return;
        };
        if role == "child" {
            loop {
                std::thread::park();
            }
        }
        use std::os::windows::process::CommandExt;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "windows::tests::job_fixture", "--nocapture"])
            .env("OPHIO_JOB_TEST_ROLE", "child")
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write("descendant.pid.part", child.id().to_string()).unwrap();
        std::fs::rename("descendant.pid.part", "descendant.pid").unwrap();
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).unwrap() != 0 {
            line.clear();
        }
        // Deliberately leave the descendant alive on root exit. The enclosing
        // job owns cleanup; keep its handle until this fixture exits.
        let _ = child.try_wait().unwrap();
    }

    fn command(path: &Path) -> Command {
        let mut command = crate::io::command(&std::env::current_exe().unwrap());
        command
            .current_dir(path)
            .args(["--exact", "windows::tests::job_fixture", "--nocapture"])
            .env("OPHIO_JOB_TEST_ROLE", "parent");
        command
    }

    /// The descendant the fixture reports, which lives until its job ends.
    async fn descendant(path: &Path) -> OwnedHandle {
        let pid = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(path.join("descendant.pid"))
                    && let Ok(pid) = text.parse::<u32>()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // SAFETY: opens only synchronization and query rights to the inert
        // fixture descendant.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        assert!(!handle.is_null());
        // SAFETY: OpenProcess returned a unique owned handle.
        unsafe { OwnedHandle::from_raw_handle(handle) }
    }

    fn assert_exited(process: &OwnedHandle) {
        // SAFETY: the handle has synchronization rights and is held through the
        // bounded wait. This touches no desktop or session services.
        assert_eq!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 3000) },
            WAIT_OBJECT_0
        );
    }

    #[tokio::test]
    async fn private_pause_fences_queued_and_late_messages_and_kills_tree() {
        let dir = tempfile::tempdir().unwrap();
        let container = Arc::new(containment::Container::new().unwrap());
        let mut child = container.spawn(&mut command(dir.path())).await.unwrap();
        let descendant = descendant(dir.path()).await;
        let (events, mut received) = mpsc::channel(256);
        let (normal, normal_rx) = mpsc::channel(32);
        let (urgent, urgent_rx) = mpsc::channel(8);
        let (done_tx, done) = watch::channel(false);
        let (background, background_rx) = crate::background::Background::new();
        let admission = Arc::new(crate::private::Admission::default());
        let control = Control {
            normal,
            urgent,
            done,
            cwd: dir.path().to_owned(),
            process_id: child.id(),
            restricted: true,
            background: background_rx,
            admission: admission.clone(),
            container: container.clone(),
        };
        let mut actor = crate::Actor {
            sandbox: None,
            exact_model: None,
            stdin: child.stdin.take(),
            frames: crate::io::Frames::new(child.stdout.take().unwrap()),
            child,
            events,
            queue: VecDeque::new(),
            pending: HashMap::new(),
            busy: true,
            sequence: 1,
            state: crate::stream::StreamState::new(protocol::model::TaskMode::Work),
            sent_messages: HashMap::new(),
            interrupted: false,
            turn_exit: None,
            background,
            parent_ended: false,
            admission,
        };
        let actor_task = tokio::spawn(async move {
            let result = actor.run(serde_json::json!({}), normal_rx, urgent_rx).await;
            done_tx.send(true).unwrap();
            result
        });
        assert_eq!(
            control
                .send_message(
                    MessageRef(protocol::CommandId::new()),
                    PromptInput::default(),
                    MessageDelivery::NextTurn
                )
                .await
                .unwrap(),
            protocol::model::DeliveryState::QueuedForBoundary
        );
        control.pause_for_private().await.unwrap();
        assert!(matches!(
            control
                .send_message(
                    MessageRef(protocol::CommandId::new()),
                    PromptInput::default(),
                    MessageDelivery::Steer
                )
                .await,
            Err(HarnessError::NotActive)
        ));
        actor_task.await.unwrap().unwrap();
        assert_exited(&descendant);
        while let Ok(event) = received.try_recv() {
            assert!(!matches!(
                event,
                ports::HarnessEvent::MessageDelivery {
                    state: protocol::model::DeliveryState::HostAccepted
                        | protocol::model::DeliveryState::NativeDelivered,
                    ..
                }
            ));
        }
        // A repeated pause can confirm the same retained job, without reviving it.
        control.pause_for_private().await.unwrap();
    }
}
