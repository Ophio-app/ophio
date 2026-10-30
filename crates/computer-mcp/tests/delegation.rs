// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]
use async_trait::async_trait;
use ports::{HarnessAdapter, HarnessEvent, PromptInput, RunSpec, ToolBridge, bridge::*};
use protocol::{RunId, TargetId, TaskId, model::ChildState};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    sync::oneshot,
};
struct Broker {
    binding: BridgeBinding,
    stopped: AtomicBool,
    requests: Mutex<Vec<BridgeRequest>>,
}
#[async_trait]
impl BridgeHandler for Broker {
    async fn bind(&self, token: &str) -> Option<BridgeBinding> {
        (token == "synthetic-binding").then(|| self.binding.clone())
    }
    async fn handle(&self, binding: &BridgeBinding, request: BridgeRequest) -> BridgeResponse {
        assert_eq!(binding.run_id, self.binding.run_id);
        assert_eq!(binding.target_id, self.binding.target_id);
        if self.stopped.load(Ordering::SeqCst) {
            return BridgeResponse::Refused {
                reason: "Run stopped".into(),
            };
        }
        self.requests.lock().unwrap().push(request.clone());
        match request {
            BridgeRequest::ObserveTarget { .. } => BridgeResponse::Observation {
                image_base64: "Zml4dHVyZQ==".into(),
                media_type: "image/png".into(),
                width: 10,
                height: 10,
                geometry_revision: 4,
            },
            BridgeRequest::RequestHuman { .. } => BridgeResponse::HumanResult {
                text: "Reviewed fixture".into(),
            },
            _ => BridgeResponse::Done,
        }
    }
    async fn unbound(&self, _: &BridgeBinding) {}
}
#[tokio::test]
async fn native_parent_delegates_six_tools_child_identity_and_late_call_is_fenced() {
    let dir = test_support::private_tempdir().unwrap();
    let binary = dir.path().join("fake-claude");
    test_support::executable(
        &binary,
        include_str!("../../harness-claude/tests/fake_claude.py"),
    )
    .unwrap();
    let socket = dir.path().join("bridge.sock");
    let gate = UnixListener::bind(dir.path().join("child.sock")).unwrap();
    let broker = Arc::new(Broker {
        binding: BridgeBinding {
            run_id: RunId::new(),
            target_id: Some(TargetId::new()),
            computer_use: true,
            browser: false,
            github: false,
            checklist_answers: false,
        },
        stopped: AtomicBool::new(false),
        requests: Mutex::new(Vec::new()),
    });
    let (stop, ended) = oneshot::channel();
    let handler = broker.clone();
    let path = socket.clone();
    let server = tokio::spawn(async move {
        computer_mcp::serve::serve_bridge(path, handler, async {
            let _ = ended.await;
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let spec = RunSpec {
        run_id: broker.binding.run_id,
        task_id: TaskId::new(),
        cwd: dir.path().into(),
        mode: protocol::model::TaskMode::Work,
        prompt: PromptInput {
            text: "delegate-computer".into(),
            images: Vec::new(),
        },
        model: None,
        effort: None,
        launch_policy: None,
        resume_session: None,
        resume_from_mode: None,
        mission: None,
        branch: None,
        start: None,
        bridge: Some(ToolBridge {
            server_name: "computer".into(),
            command: env!("CARGO_BIN_EXE_computer-mcp").into(),
            args: Vec::new(),
            env: vec![
                (RUN_TOKEN_ENV.into(), "synthetic-binding".into()),
                (SOCKET_ENV.into(), socket.to_str().unwrap().into()),
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
    let mut run = harness_claude::ClaudeAdapter::with_binary(binary)
        .start_run(spec)
        .await
        .unwrap();
    let (mut child, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ready = [0; 5];
    child.read_exact(&mut ready).await.unwrap();
    assert_eq!(ready, *b"ready");
    assert_eq!(broker.requests.lock().unwrap().len(), 6);
    broker.stopped.store(true, Ordering::SeqCst);
    child.write_all(b"x").await.unwrap();
    let mut parent = None;
    let mut child_id = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), run.events.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            HarnessEvent::SessionStarted { native_session, .. } => {
                assert!(parent.is_none());
                parent = Some(native_session);
            }
            HarnessEvent::Child {
                native_ref,
                state: ChildState::Active,
                ..
            } => {
                assert_eq!(native_ref, "delegate-1");
                child_id = true;
            }
            HarnessEvent::AgentMessage { text, .. } if text == "Late call was fenced" => break,
            HarnessEvent::TurnCompleted { .. } => {
                panic!("Child completion must not finish the parent turn")
            }
            _ => {}
        }
    }
    assert_eq!(parent.as_deref(), Some("native-session"));
    assert!(child_id);
    assert_eq!(broker.requests.lock().unwrap().len(), 6);
    run.control.pause_for_private().await.unwrap();
    let _ = stop.send(());
    server.await.unwrap().unwrap();
}
