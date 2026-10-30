// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use async_trait::async_trait;
use computer_mcp::serve::serve_bridge;
use ports::bridge::*;
use protocol::ids::{RunId, TargetId};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Notify, oneshot},
};

struct Handler {
    computer: bool,
    answers: bool,
    unbound: AtomicUsize,
    called: Notify,
    block: bool,
}
#[async_trait]
impl BridgeHandler for Handler {
    async fn bind(&self, token: &str) -> Option<BridgeBinding> {
        (token == "fixture-token").then(|| BridgeBinding {
            run_id: RunId::new(),
            target_id: Some(TargetId::new()),
            computer_use: self.computer,
            browser: false,
            github: false,
            checklist_answers: self.answers,
        })
    }
    async fn handle(&self, _: &BridgeBinding, request: BridgeRequest) -> BridgeResponse {
        self.called.notify_one();
        if self.block {
            std::future::pending::<()>().await;
        }
        match request {
            BridgeRequest::ObserveTarget { .. } => BridgeResponse::Observation {
                image_base64: "aW1hZ2U=".into(),
                media_type: "image/png".into(),
                width: 100,
                height: 50,
                geometry_revision: 4,
            },
            BridgeRequest::Pointer {
                at,
                observation_revision,
                ..
            } => {
                assert_eq!((at.x, at.y), (0.5, 0.5));
                assert_eq!(observation_revision, 4);
                BridgeResponse::Refused {
                    reason: "stale observation: broker geometry advanced".into(),
                }
            }
            BridgeRequest::PermissionPrompt {
                input,
                native_request_id,
                ..
            } => {
                assert_eq!(native_request_id.as_deref(), Some("6"));
                BridgeResponse::Permission {
                    decision: json!({"behavior":"allow","updatedInput":input}),
                }
            }
            BridgeRequest::Person { .. } => BridgeResponse::PersonResult {
                text: "shown".into(),
            },
            BridgeRequest::Key { .. } => BridgeResponse::Resumed {
                text: "Not sent: you were paused".into(),
                image_base64: "bm93".into(),
                media_type: "image/png".into(),
                width: 200,
                height: 100,
                geometry_revision: 7,
            },
            _ => BridgeResponse::Done,
        }
    }
    async fn unbound(&self, _: &BridgeBinding) {
        self.unbound.fetch_add(1, Ordering::SeqCst);
    }
}
struct Fixture {
    child: Child,
    input: ChildStdin,
    output: tokio::io::Lines<BufReader<ChildStdout>>,
    stop: Option<oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<Result<(), computer_mcp::BridgeError>>,
    _dir: tempfile::TempDir,
    handler: Arc<Handler>,
    path: PathBuf,
}
impl Fixture {
    async fn new(computer: bool, token: &str, block: bool) -> Self {
        Self::with(computer, token, block, false).await
    }
    async fn with(computer: bool, token: &str, block: bool, answers: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private").join("bridge.sock");
        let handler = Arc::new(Handler {
            computer,
            answers,
            unbound: AtomicUsize::new(0),
            called: Notify::new(),
            block,
        });
        let (tx, rx) = oneshot::channel();
        let server_path = path.clone();
        let h = handler.clone();
        let server = tokio::spawn(async move {
            serve_bridge(server_path, h, async {
                let _ = rx.await;
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !path.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_computer-mcp"))
            .env(RUN_TOKEN_ENV, token)
            .env(SOCKET_ENV, &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            child,
            input,
            output,
            stop: Some(tx),
            server,
            _dir: dir,
            handler,
            path,
        }
    }
    async fn send(&mut self, value: Value) {
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
    }
    async fn read(&mut self) -> Value {
        serde_json::from_str(
            &tokio::time::timeout(Duration::from_secs(3), self.output.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }
    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        self.read().await
    }
    async fn init(&mut self) {
        let result=self.call(1,"initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}})).await;
        assert_eq!(result["result"]["protocolVersion"], "2025-11-25");
        self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
    }
    async fn stop(mut self) {
        drop(self.input);
        let status = tokio::time::timeout(Duration::from_secs(3), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.server.await.unwrap().unwrap();
        assert_eq!(self.handler.unbound.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn end_to_end_observation_fencing_and_permissions() {
    let mut f = Fixture::new(true, "fixture-token", false).await;
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&f.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    f.init().await;
    assert_eq!(
        f.call(2, "tools/list", json!({})).await["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    let result = f
        .call(
            3,
            "tools/call",
            json!({"name":"observe_target","arguments":{}}),
        )
        .await;
    assert_eq!(result["result"]["content"][0]["type"], "image");
    assert_eq!(result["result"]["content"][0]["data"], "aW1hZ2U=");
    let result=f.call(4,"tools/call",json!({"name":"pointer_action","arguments":{"action":{"kind":"click","button":"left","count":1},"x":50,"y":25,"observation_revision":4}})).await;
    assert_eq!(result["result"]["isError"], true);
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("broker geometry")
    );
    let result=f.call(5,"tools/call",json!({"name":"pointer_action","arguments":{"action":{"kind":"move"},"x":50,"y":25,"observation_revision":3}})).await;
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("stale observation_revision")
    );
    let result=f.call(6,"tools/call",json!({"name":"permission_prompt","arguments":{"tool_name":"Bash","input":{"command":"pwd"}}})).await;
    assert_eq!(result["result"]["isError"], true);
    f.stop().await;
}
#[tokio::test]
async fn an_action_the_person_paused_comes_back_with_the_screen_to_act_again_from() {
    let mut f = Fixture::new(true, "fixture-token", false).await;
    f.init().await;
    f.call(
        2,
        "tools/call",
        json!({"name":"observe_target","arguments":{}}),
    )
    .await;
    let result = f
        .call(
            3,
            "tools/call",
            json!({"name":"key_action","arguments":{"keys":["CTRL","ALT","T"]}}),
        )
        .await;
    assert_eq!(result["result"]["isError"], false);
    let content = &result["result"]["content"];
    assert_eq!(content[0]["text"], "Not sent: you were paused");
    assert_eq!(content[1]["data"], "bm93");
    assert!(
        content[2]["text"]
            .as_str()
            .unwrap()
            .contains("observation_revision: 7")
    );
    // What it acts on next is placed on the screen it was just shown.
    let result=f.call(4,"tools/call",json!({"name":"pointer_action","arguments":{"action":{"kind":"move"},"x":50,"y":25,"observation_revision":4}})).await;
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("stale observation_revision")
    );
    f.stop().await;
}
#[tokio::test]
async fn permission_only_and_modern_negotiation() {
    let mut f = Fixture::new(false, "fixture-token", false).await;
    let meta = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}});
    let result = f.call(1, "server/discover", json!({"_meta":meta})).await;
    assert_eq!(result["result"]["resultType"], "complete");
    assert_eq!(result["result"]["supportedVersions"][0], "2026-07-28");
    let result = f.call(2, "tools/list", json!({"_meta":meta})).await;
    // A 2026-07-28 client rejects a list without its cache hints, and then
    // has none of the tools, the permission prompt included.
    assert!(result["result"]["ttlMs"].as_u64().is_some());
    assert_eq!(result["result"]["cacheScope"], "private");
    let names: Vec<_> = result["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    let mut want = vec![computer_mcp::PERMISSION_TOOL.to_owned()];
    want.extend(PERSON_TOOLS.iter().map(|t| t.to_string()));
    want.extend(WORK_TOOLS.iter().map(|t| t.to_string()));
    assert_eq!(names, want);
    let result = f
        .call(
            3,
            "tools/call",
            json!({"_meta":meta,"name":"observe_target"}),
        )
        .await;
    assert_eq!(result["result"]["isError"], true);
    // A coding task's run reaches Ophio when it shows the person something.
    let shown = f.call(5,"tools/call",json!({"_meta":meta,"name":"show_preview","arguments":{"url":"http://127.0.0.1:5194/","label":"Site"}})).await;
    assert_eq!(shown["result"]["isError"], false);
    assert_eq!(shown["result"]["content"][0]["text"], "shown");
    let permission = f.call(6,"tools/call",json!({"_meta":meta,"name":"permission_prompt","arguments":{"tool_name":"Bash","input":{"command":"pwd"}}})).await;
    let decision: Value =
        serde_json::from_str(permission["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(decision["behavior"], "allow");
    let result = f
        .call(
            4,
            "tools/list",
            json!({"_meta":{"io.modelcontextprotocol/protocolVersion":"2099-01-01"}}),
        )
        .await;
    assert_eq!(result["error"]["code"], -32022);
    f.stop().await;
}
#[tokio::test]
async fn invalid_token_exits_without_echoing_secret() {
    let f = Fixture::new(false, "fixture-bad-token", false).await;
    drop(f.input);
    let output = f.child.wait_with_output().await.unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("authentication rejected"));
    assert!(!stderr.contains("fixture-bad-token"));
    f.stop.unwrap().send(()).unwrap();
    f.server.await.unwrap().unwrap();
    assert_eq!(f.handler.unbound.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn socket_loss_during_call_is_explicit_and_not_retried() {
    let mut f = Fixture::new(true, "fixture-token", true).await;
    f.init().await;
    f.send(
        json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"observe_target"}}),
    )
    .await;
    f.handler.called.notified().await;
    f.stop.take().unwrap().send(()).unwrap();
    let result = f.read().await;
    assert_eq!(result["result"]["isError"], true);
    let next = f
        .call(10, "tools/call", json!({"name":"observe_target"}))
        .await;
    assert!(
        next["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("connection closed")
    );
    f.stop().await;
}

#[tokio::test]
async fn a_turn_answering_on_the_checklist_sees_and_calls_its_two_tools_alone() {
    let mut f = Fixture::with(false, "fixture-token", false, true).await;
    f.init().await;
    let result = f.call(2, "tools/list", json!({})).await;
    let names: Vec<_> = result["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ANSWER_TOOLS);
    let read = f
        .call(
            3,
            "tools/call",
            json!({"name":"checklist_read","arguments":{}}),
        )
        .await;
    assert_eq!(read["result"]["isError"], false);
    for (id, name, arguments) in [
        (4, "checklist_add", json!({"items":[]})),
        (5, "show_preview", json!({"url":"http://127.0.0.1:5194/"})),
        (
            6,
            "permission_prompt",
            json!({"tool_name":"Bash","input":{"command":"pwd"}}),
        ),
    ] {
        let refused = f
            .call(id, "tools/call", json!({"name":name,"arguments":arguments}))
            .await;
        assert_eq!(refused["result"]["isError"], true, "{name}");
        assert!(
            refused["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("this turn only reads the checklist"),
            "{refused}"
        );
    }
    f.stop().await;
}
