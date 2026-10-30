// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::sandbox::{RelayBudget, RelayPolicy};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixStream},
    sync::Notify,
};

struct Fake {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Fake {
    async fn new(content_type: &str, chunks: Vec<Vec<u8>>, gate: Option<Arc<Notify>>) -> Self {
        Self::status(200, content_type, chunks, gate).await
    }
    async fn status(
        status: u16,
        content_type: &str,
        chunks: Vec<Vec<u8>>,
        gate: Option<Arc<Notify>>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(vec![]));
        let captured = requests.clone();
        let content_type = content_type.to_owned();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let captured = captured.clone();
                let content_type = content_type.clone();
                let chunks = chunks.clone();
                let gate = gate.clone();
                tokio::spawn(async move {
                    let mut request = vec![];
                    while !request.ends_with(b"\r\n\r\n") {
                        request.push(stream.read_u8().await.unwrap());
                    }
                    let header = String::from_utf8(request.clone()).unwrap();
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse().ok())
                        })
                        .unwrap_or(0);
                    request.resize(request.len() + length, 0);
                    let start = request.len() - length;
                    stream.read_exact(&mut request[start..]).await.unwrap();
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(request).unwrap());
                    let head = format!(
                        "HTTP/1.1 {status} Fake\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                    );
                    stream.write_all(head.as_bytes()).await.unwrap();
                    for (i, chunk) in chunks.into_iter().enumerate() {
                        if i > 0
                            && let Some(gate) = &gate
                        {
                            gate.notified().await;
                        }
                        if stream
                            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                            .await
                            .is_err()
                        {
                            return;
                        }
                        if stream.write_all(&chunk).await.is_err() {
                            return;
                        }
                        if stream.write_all(b"\r\n").await.is_err() {
                            return;
                        }
                    }
                    let _ = stream.write_all(b"0\r\n\r\n").await;
                });
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
    async fn json() -> Self {
        Self::new(
            "application/json",
            vec![
                br#"{"text":"response-private-text","usage":{"input_tokens":7,"output_tokens":3}}"#
                    .to_vec(),
            ],
            None,
        )
        .await
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    spec: RelaySpec,
    signals: Arc<Mutex<Vec<RelaySignal>>>,
}
impl Fixture {
    fn new(provider: RelayProvider) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("key");
        std::fs::write(&key_file, "fixture-provider-key\n").unwrap();
        let signals = Arc::new(Mutex::new(vec![]));
        let captured = signals.clone();
        let spec = RelaySpec {
            provider,
            key_file,
            budget: RelayBudget {
                max_requests: 100,
                max_input_tokens: 1000,
                max_output_tokens: 1000,
                deadline: None,
            },
            policy: RelayPolicy::default(),
            log: dir.path().join("audit.jsonl"),
            on_signal: Some(Arc::new(move |s| captured.lock().unwrap().push(s))),
            inspect: None,
            test_upstream: None,
        };
        Self { dir, spec, signals }
    }
    fn relay(&self, fake: &Fake) -> Relay {
        Relay::test_upstream(self.dir.path(), self.spec.clone(), &fake.url).unwrap()
    }
    fn logs(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.spec.log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}
async fn raw(relay: &Relay, request: &str) -> String {
    let mut stream = UnixStream::connect(relay.socket()).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    response
}
fn request(path: &str, body: &str, extra: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: relay\r\nContent-Length: {}\r\nContent-Type: application/json\r\n{extra}\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn inspector_refuses_raw_and_json_escaped_canaries_before_upstream() {
    let fake = Fake::json().await;
    let mut fixture = Fixture::new(RelayProvider::Anthropic);
    fixture.spec.inspect = Some(Arc::new(|b| b.windows(12).any(|w| w == b"TEST_CANARY!")));
    let relay = fixture.relay(&fake);
    for body in [
        r#"{"model":"allowed","messages":[{"content":"TEST_CANARY!"}]}"#,
        r#"{"model":"allowed","messages":[{"content":"TEST_\u0043ANARY!"}]}"#,
    ] {
        let answer = raw(&relay, &request("/v1/messages", body, "")).await;
        assert!(answer.starts_with("HTTP/1.1 403"));
    }
    let body = r#"{"model":"allowed","input":"TEST_CANARY!"}"#;
    assert!(
        raw(&relay, &request("/v1/files", body, ""))
            .await
            .starts_with("HTTP/1.1 403")
    );
    assert!(fake.requests.lock().unwrap().is_empty());
    assert_eq!(
        *fixture.signals.lock().unwrap(),
        [
            RelaySignal::HijackAttempt,
            RelaySignal::HijackAttempt,
            RelaySignal::HijackAttempt
        ]
    );
}
async fn forwarded(relay: &Relay) -> (String, tokio::task::JoinHandle<io::Result<()>>) {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", tcp.local_addr().unwrap());
    let socket = relay.socket();
    let forward = tokio::spawn(async move { crate::shim::forward(tcp, &socket).await });
    (url, forward)
}

#[tokio::test]
async fn injects_only_the_configured_key_and_logs_metadata_for_both_providers() {
    for provider in [RelayProvider::Anthropic, RelayProvider::OpenAi] {
        let fake = Fake::json().await;
        let f = Fixture::new(provider);
        let relay = f.relay(&fake);
        let path = if provider == RelayProvider::Anthropic {
            "/v1/messages"
        } else {
            "/v1/responses"
        };
        let response = raw(&relay, &request(path, r#"{"model":"fixture-model","messages":["request-private-text"]}"#,
            "Authorization: Bearer attacker-dummy\r\nX-Api-Key: attacker-dummy\r\nProxy-Authorization: attacker-dummy\r\nCookie: identity-private\r\nOpenAI-Organization: identity-private\r\nOpenAI-Project: identity-private\r\nX-Forwarded-For: identity-private\r\nX-Unknown: identity-private\r\nConnection: x-client-request-id\r\nX-Client-Request-Id: hop-private\r\nAnthropic-Version: 2023-06-01\r\nAnthropic-Beta: fixture-beta\r\nUser-Agent: fixture-agent\r\n")).await;
        assert!(response.starts_with("HTTP/1.1 200"));
        let requests = fake.requests.lock().unwrap();
        let upstream = requests[0].to_ascii_lowercase();
        assert!(upstream.starts_with(&format!("post {path} http/1.1")));
        for forbidden in [
            "attacker-dummy",
            "identity-private",
            "hop-private",
            "cookie:",
            "proxy-authorization:",
            "openai-project:",
            "x-forwarded-",
        ] {
            assert!(!upstream.contains(forbidden), "{upstream}");
        }
        match provider {
            RelayProvider::Anthropic => {
                assert!(upstream.contains("x-api-key: fixture-provider-key\r\n"));
                assert!(!upstream.contains("authorization:"));
            }
            RelayProvider::OpenAi => {
                assert!(upstream.contains("authorization: bearer fixture-provider-key\r\n"));
                assert!(!upstream.contains("x-api-key:"));
            }
        }
        assert!(upstream.contains("anthropic-version: 2023-06-01"));
        assert_eq!(
            relay.totals(),
            RelayTotals {
                requests: 1,
                input_tokens: 7,
                output_tokens: 3
            }
        );
        let logs = f.logs();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0]["verdict"], "allowed");
        assert!(logs[0]["request_bytes"].as_u64().unwrap() > 0);
        assert!(logs[0]["response_bytes"].as_u64().unwrap() > 0);
        let log = logs[0].to_string();
        for secret in [
            "request-private-text",
            "response-private-text",
            "fixture-provider-key",
            "attacker-dummy",
            "identity-private",
            "fixture-agent",
            "fixture-beta",
            "2023-06-01",
        ] {
            assert!(!log.contains(secret));
        }
        assert!(f.signals.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn refuses_paths_methods_upgrades_absolute_form_connect_and_large_bodies() {
    let fake = Fake::json().await;
    let f = Fixture::new(RelayProvider::Anthropic);
    let relay = f.relay(&fake);
    let body = r#"{"model":"fixture"}"#;
    let mut requests = vec![
        request("/v1/files?private-path-text", body, ""),
        request("/v1/responses", body, ""),
        request("/v1/messages", body, "").replace("HTTP/1.1", "HTTP/1.0"),
        request("/v1/messages?private-query", body, ""),
        request("/v1/%6dessages", body, ""),
        request("http://attacker.invalid/v1/messages", body, ""),
        request(
            "/v1/messages",
            body,
            "Upgrade: websocket\r\nConnection: Upgrade\r\n",
        ),
        request("/v1/messages", body, "Connection: keep-alive, uPgRaDe\r\n"),
        "CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: relay\r\n\r\n".into(),
        format!(
            "POST /v1/messages HTTP/1.1\r\nHost: relay\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        ),
        request("/v1/messages", "not-json-private", ""),
        request("/v1/messages", body, "Content-Encoding: gzip\r\n"),
    ];
    for method in ["GET", "DELETE", "PUT", "HEAD", "OPTIONS"] {
        requests.push(request("/v1/messages", body, "").replacen("POST", method, 1));
    }
    for req in &requests {
        let response = raw(&relay, req).await;
        assert_eq!(
            response.split_whitespace().nth(1),
            Some("403"),
            "{req}: {response}"
        );
    }
    assert_eq!(f.logs().len(), requests.len());
    assert_eq!(
        f.signals.lock().unwrap().as_slice(),
        vec![RelaySignal::HijackAttempt; requests.len()]
    );
    assert!(fake.requests.lock().unwrap().is_empty());
    assert_eq!(relay.totals().requests, 0);
    let log = std::fs::read_to_string(&f.spec.log).unwrap();
    for secret in [
        "private-path-text",
        "attacker.invalid",
        "private-query",
        "not-json-private",
    ] {
        assert!(!log.contains(secret));
    }
}
#[tokio::test]
async fn listens_under_a_home_folder_too_long_for_a_socket_address() {
    use std::os::unix::fs::FileTypeExt;
    let fake = Fake::new("application/json", vec![], None).await;
    let f = Fixture::new(RelayProvider::Anthropic);
    let state = f.dir.path().join("h".repeat(100));
    std::fs::create_dir(&state).unwrap();
    let relay = Relay::test_upstream(&state, f.spec.clone(), &fake.url).unwrap();
    assert!(relay.socket().as_os_str().len() > 108);
    assert!(
        std::fs::metadata(relay.socket())
            .unwrap()
            .file_type()
            .is_socket()
    );
}
#[tokio::test]
async fn chunked_requests_are_bounded_and_count_tokens_is_allowed() {
    let fake = Fake::new(
        "application/json",
        vec![br#"{"input_tokens":12}"#.to_vec()],
        None,
    )
    .await;
    let f = Fixture::new(RelayProvider::Anthropic);
    let relay = f.relay(&fake);
    let body = r#"{"model":"fixture"}"#;
    let req = format!(
        "POST /v1/messages/count_tokens HTTP/1.1\r\nHost: relay\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
        body.len()
    );
    assert!(raw(&relay, &req).await.starts_with("HTTP/1.1 200"));
    assert_eq!(relay.totals().input_tokens, 12);
    let (url, forward) = forwarded(&relay).await;
    let huge = format!(
        "{{\"model\":\"fixture\",\"text\":\"{}\"}}",
        "x".repeat(MAX_BODY_BYTES)
    );
    // The relay answers 403 once the body passes the cap and closes, so the
    // sender may see the connection drop before it reads the answer.
    match reqwest::Client::new()
        .post(format!("{url}/v1/messages"))
        .body(huge)
        .send()
        .await
    {
        Ok(response) => assert_eq!(response.status(), 403),
        Err(error) => assert!(error.is_request() || error.is_body(), "{error}"),
    }
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
    forward.abort();
}
#[tokio::test]
async fn sse_arrives_before_upstream_finishes_and_accounts_usage_across_chunks() {
    for provider in [RelayProvider::Anthropic, RelayProvider::OpenAi] {
        let gate = Arc::new(Notify::new());
        let (first, last, path, input, output) = match provider {
            RelayProvider::Anthropic => (
                "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"cache_read_input_tokens\":4,\"cache_creation_input_tokens\":2,\"output_tokens\":1}}}\r\n\r\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                "/v1/messages",
                13,
                5,
            ),
            RelayProvider::OpenAi => (
                "event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
                "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":9,\"output_tokens\":6}}}\n\n",
                "/v1/responses",
                9,
                6,
            ),
        };
        let split = last.len() / 2;
        let fake = Fake::new(
            "text/event-stream",
            vec![
                first.as_bytes().to_vec(),
                last.as_bytes()[..split].to_vec(),
                last.as_bytes()[split..].to_vec(),
            ],
            Some(gate.clone()),
        )
        .await;
        let f = Fixture::new(provider);
        let relay = f.relay(&fake);
        let (url, forward) = forwarded(&relay).await;
        let body = r#"{"model":"fixture","stream":true}"#;
        let mut response = reqwest::Client::new()
            .post(format!("{url}{path}"))
            .body(body)
            .send()
            .await
            .unwrap();
        let first_chunk = tokio::time::timeout(Duration::from_secs(1), async {
            let mut bytes = vec![];
            while bytes.len() < first.len() {
                bytes.extend_from_slice(&response.chunk().await.unwrap().unwrap());
            }
            bytes
        })
        .await
        .unwrap();
        assert_eq!(first_chunk, first.as_bytes());
        assert!(
            f.logs().is_empty(),
            "upstream still waits for the test to release it"
        );
        assert!(relay.totals().input_tokens >= body.len() as u64);
        assert_eq!(
            relay.totals().output_tokens,
            f.spec.budget.max_output_tokens
        );
        gate.notify_one();
        let middle = response.chunk().await.unwrap().unwrap();
        gate.notify_one();
        let mut rest = middle.to_vec();
        while let Some(chunk) = response.chunk().await.unwrap() {
            rest.extend_from_slice(&chunk);
        }
        assert_eq!(rest, last.as_bytes());
        assert_eq!(
            relay.totals(),
            RelayTotals {
                requests: 1,
                input_tokens: input,
                output_tokens: output
            }
        );
        assert_eq!(f.logs()[0]["input_tokens"], input);
        assert!(f.signals.lock().unwrap().is_empty());
        forward.abort();
    }
}
#[tokio::test]
async fn budgets_refuse_subsequent_requests_and_deadlines_refuse_without_upstream() {
    for limit in ["requests", "input", "output", "deadline"] {
        let fake = Fake::json().await;
        let mut f = Fixture::new(RelayProvider::OpenAi);
        match limit {
            "requests" => f.spec.budget.max_requests = 1,
            "input" => f.spec.budget.max_input_tokens = 7,
            "output" => f.spec.budget.max_output_tokens = 3,
            _ => f.spec.budget.deadline = Some(SystemTime::now() - Duration::from_secs(1)),
        }
        let relay = f.relay(&fake);
        let req = request("/v1/responses", r#"{"model":"fixture"}"#, "");
        if matches!(limit, "requests" | "output") {
            assert!(raw(&relay, &req).await.starts_with("HTTP/1.1 200"));
        }
        assert!(raw(&relay, &req).await.starts_with("HTTP/1.1 429"));
        assert_eq!(
            f.signals.lock().unwrap().as_slice(),
            [RelaySignal::BudgetSpent]
        );
        assert_eq!(f.logs().last().unwrap()["verdict"], "budget_spent");
        assert_eq!(
            fake.requests.lock().unwrap().len(),
            usize::from(matches!(limit, "requests" | "output"))
        );
    }
}
#[tokio::test]
async fn model_and_output_token_limits_refuse_without_rewriting() {
    for provider in [RelayProvider::Anthropic, RelayProvider::OpenAi] {
        let fake = Fake::json().await;
        let mut f = Fixture::new(provider);
        f.spec.policy = RelayPolicy {
            allowed_models: Some(vec!["allowed".into()]),
            max_output_tokens: Some(20),
        };
        let relay = f.relay(&fake);
        let (path, field) = match provider {
            RelayProvider::Anthropic => ("/v1/messages", "max_tokens"),
            RelayProvider::OpenAi => ("/v1/responses", "max_output_tokens"),
        };
        for body in [
            json!({"model":"other",field:20}),
            json!({"model":"allowed",field:21}),
            json!({"model":"allowed",field:-1}),
            json!({"model":"allowed",field:"20"}),
            json!({"model":"allowed"}),
        ] {
            assert!(
                raw(&relay, &request(path, &body.to_string(), ""))
                    .await
                    .starts_with("HTTP/1.1 403")
            );
        }
        let body = json!({"model":"allowed",field:20,"text":"preserved"}).to_string();
        assert!(
            raw(&relay, &request(path, &body, ""))
                .await
                .starts_with("HTTP/1.1 200")
        );
        assert!(fake.requests.lock().unwrap()[0].ends_with(&body));
    }
}
#[tokio::test]
async fn missing_usage_fails_closed_and_deadline_ends_an_active_stream() {
    let fake = Fake::new(
        "application/json",
        vec![br#"{"text":"no usage"}"#.to_vec()],
        None,
    )
    .await;
    let f = Fixture::new(RelayProvider::OpenAi);
    let relay = f.relay(&fake);
    let req = request("/v1/responses", r#"{"model":"fixture"}"#, "");
    assert!(raw(&relay, &req).await.starts_with("HTTP/1.1 200"));
    assert!(raw(&relay, &req).await.starts_with("HTTP/1.1 429"));
    assert_eq!(f.logs()[0]["verdict"], "incomplete");
    let gate = Arc::new(Notify::new());
    let fake = Fake::new(
        "text/event-stream",
        vec![b"data: {}\n\n".to_vec(), b"data: {}\n\n".to_vec()],
        Some(gate),
    )
    .await;
    let mut f = Fixture::new(RelayProvider::OpenAi);
    f.spec.budget.deadline = Some(SystemTime::now() + Duration::from_millis(150));
    let relay = f.relay(&fake);
    let (url, forward) = forwarded(&relay).await;
    let response = reqwest::Client::new()
        .post(format!("{url}/v1/responses"))
        .body(r#"{"model":"fixture"}"#)
        .send()
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), response.bytes())
            .await
            .unwrap()
            .is_err()
    );
    assert!(raw(&relay, &req).await.starts_with("HTTP/1.1 429"));
    assert_eq!(
        f.signals.lock().unwrap().as_slice(),
        [RelaySignal::BudgetSpent]
    );
    forward.abort();
}

#[tokio::test]
async fn refuses_hosted_tools_remote_inputs_background_work_and_duplicate_keys() {
    let fake = Fake::json().await;
    let f = Fixture::new(RelayProvider::OpenAi);
    let relay = f.relay(&fake);
    for body in [
        r#"{"model":"fixture","tools":[{"type":"mcp","server_url":"https://attacker.invalid"}]}"#,
        r#"{"model":"fixture","tools":[{"type":"web_search"}]}"#,
        r#"{"model":"fixture","mcp_servers":[{"url":"https://attacker.invalid"}]}"#,
        r#"{"model":"fixture","input":[{"type":"input_image","image_url":"https://attacker.invalid"}]}"#,
        r#"{"model":"fixture","messages":[{"content":[{"source":{"type":"url","url":"https://attacker.invalid"}}]}]}"#,
        r#"{"model":"fixture","background":true}"#,
        r#"{"model":"first","model":"fixture"}"#,
        r#"{"model":"fixture","max_output_tokens":999,"max_output_tokens":1}"#,
        r#"{"model":"fixture","tools":[{"type":"mcp","type":"function"}]}"#,
    ] {
        assert!(
            raw(&relay, &request("/v1/responses", body, ""))
                .await
                .starts_with("HTTP/1.1 403")
        );
    }
    assert_eq!(fake.requests.lock().unwrap().len(), 0);
    let body = r#"{"model":"fixture","tools":[{"type":"function","name":"local"},{"type":"custom","name":"local_custom"}],"input":[{"type":"input_image","image_url":"data:image/png;base64,fixture"}],"background":false}"#;
    assert!(
        raw(&relay, &request("/v1/responses", body, ""))
            .await
            .starts_with("HTTP/1.1 200")
    );
    assert_eq!(f.signals.lock().unwrap().len(), 9);
}

#[tokio::test]
async fn concurrent_requests_recheck_budget_after_usage_and_disconnects_still_count() {
    let gate = Arc::new(Notify::new());
    let fake = Fake::new("text/event-stream", vec![
        b"data: {\"type\":\"response.created\"}\n\n".to_vec(),
        b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":3}}}\n\n".to_vec(),
    ], Some(gate.clone())).await;
    let mut f = Fixture::new(RelayProvider::OpenAi);
    f.spec.budget.max_output_tokens = 3;
    let relay = f.relay(&fake);
    let req = request("/v1/responses", r#"{"model":"fixture"}"#, "");
    let mut first = UnixStream::connect(relay.socket()).await.unwrap();
    first.write_all(req.as_bytes()).await.unwrap();
    first.read_u8().await.unwrap();
    let socket = relay.socket();
    let mut second = tokio::spawn(async move {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    });
    drop(first);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err()
    );
    gate.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap()
            .starts_with("HTTP/1.1 429")
    );
    let input_bound = fake.requests.lock().unwrap()[0]
        .split_once("\r\n\r\n")
        .unwrap()
        .1
        .len() as u64;
    assert_eq!(
        relay.totals(),
        RelayTotals {
            requests: 1,
            input_tokens: input_bound,
            output_tokens: 3
        }
    );
    assert_eq!(f.logs()[0]["verdict"], "incomplete");
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn upstream_redirects_never_forward_credentials_to_a_second_origin() {
    let destination = Fake::json().await;
    let fake = Fake::status(
        307,
        &format!(
            "application/json\r\nLocation: {}/v1/responses",
            destination.url
        ),
        vec![b"{}".to_vec()],
        None,
    )
    .await;
    let f = Fixture::new(RelayProvider::OpenAi);
    let relay = f.relay(&fake);
    let response = raw(
        &relay,
        &request("/v1/responses", r#"{"model":"fixture"}"#, ""),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 307"));
    assert!(!response.to_ascii_lowercase().contains("location:"));
    assert!(destination.requests.lock().unwrap().is_empty());
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
    assert_eq!(f.logs()[0]["status"], 307);
}

#[tokio::test]
async fn one_token_budget_refuses_a_seven_token_input_before_forwarding() {
    let fake = Fake::json().await;
    let mut f = Fixture::new(RelayProvider::OpenAi);
    f.spec.budget.max_input_tokens = 1;
    let relay = f.relay(&fake);
    let req = request("/v1/responses", r#"{"model":"fixture"}"#, "");
    assert!(raw(&relay, &req).await.starts_with("HTTP/1.1 429"));
    assert!(fake.requests.lock().unwrap().is_empty());
    assert_eq!(relay.totals(), RelayTotals::default());
    assert_eq!(*f.signals.lock().unwrap(), [RelaySignal::BudgetSpent]);
}

#[tokio::test]
async fn output_limits_are_clamped_to_the_remaining_budget_for_both_providers() {
    for provider in [RelayProvider::Anthropic, RelayProvider::OpenAi] {
        let fake = Fake::json().await;
        let mut f = Fixture::new(provider);
        f.spec.budget.max_output_tokens = 8;
        f.spec.policy.max_output_tokens = Some(20);
        let relay = f.relay(&fake);
        let (path, field) = match provider {
            RelayProvider::Anthropic => ("/v1/messages", "max_tokens"),
            RelayProvider::OpenAi => ("/v1/responses", "max_output_tokens"),
        };
        let body = json!({"model":"fixture",field:20,"input":"preserved"}).to_string();
        for limit in [8, 5] {
            assert!(
                raw(&relay, &request(path, &body, ""))
                    .await
                    .starts_with("HTTP/1.1 200")
            );
            let requests = fake.requests.lock().unwrap();
            let (_, body) = requests.last().unwrap().split_once("\r\n\r\n").unwrap();
            let body: Value = serde_json::from_str(body).unwrap();
            assert_eq!(body[field], limit);
            assert_eq!(body["input"], "preserved");
        }
        assert_eq!(relay.totals().output_tokens, 6);
        assert!(f.signals.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn omitted_output_limits_are_bounded_for_both_providers() {
    for provider in [RelayProvider::Anthropic, RelayProvider::OpenAi] {
        let fake = Fake::json().await;
        let mut f = Fixture::new(provider);
        f.spec.budget.max_output_tokens = 4;
        let relay = f.relay(&fake);
        let (path, field) = match provider {
            RelayProvider::Anthropic => ("/v1/messages", "max_tokens"),
            RelayProvider::OpenAi => ("/v1/responses", "max_output_tokens"),
        };
        assert!(
            raw(&relay, &request(path, r#"{"model":"fixture"}"#, ""))
                .await
                .starts_with("HTTP/1.1 200")
        );
        let requests = fake.requests.lock().unwrap();
        let (_, body) = requests[0].split_once("\r\n\r\n").unwrap();
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(body[field], 4);
    }
}

#[tokio::test]
async fn concurrent_requests_cannot_share_the_last_input_allowance() {
    let gate = Arc::new(Notify::new());
    let fake = Fake::new("text/event-stream", vec![
        b"data: {\"type\":\"response.created\"}\n\n".to_vec(),
        b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":3}}}\n\n".to_vec(),
    ], Some(gate.clone())).await;
    let body = r#"{"max_output_tokens":10,"model":"fixture"}"#;
    let mut f = Fixture::new(RelayProvider::OpenAi);
    f.spec.budget.max_input_tokens = body.len() as u64;
    let relay = f.relay(&fake);
    let (url, forward) = forwarded(&relay).await;
    let url = format!("{url}/v1/responses");
    let mut first = reqwest::Client::new()
        .post(&url)
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(first.chunk().await.unwrap().is_some());
    assert_eq!(
        relay.totals(),
        RelayTotals {
            requests: 1,
            input_tokens: body.len() as u64,
            output_tokens: 10,
        }
    );
    assert!(f.signals.lock().unwrap().is_empty());
    let mut second = tokio::spawn(async move {
        reqwest::Client::new()
            .post(url)
            .body(body)
            .send()
            .await
            .unwrap()
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err()
    );
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
    gate.notify_one();
    first.bytes().await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap()
            .status(),
        429
    );
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
    assert_eq!(
        relay.totals(),
        RelayTotals {
            requests: 1,
            input_tokens: 7,
            output_tokens: 3
        }
    );
    forward.abort();
}

#[tokio::test]
async fn settlement_releases_unused_reservations_for_another_request() {
    let gate = Arc::new(Notify::new());
    let fake = Fake::new(
        "application/json",
        vec![
            br#"{"usage":{"input_tokens":7,"output_tokens":3}"#.to_vec(),
            b"}".to_vec(),
        ],
        Some(gate.clone()),
    )
    .await;
    let body = r#"{"max_output_tokens":5,"model":"fixture"}"#;
    let mut f = Fixture::new(RelayProvider::OpenAi);
    f.spec.budget.max_input_tokens = body.len() as u64 + 7;
    f.spec.budget.max_output_tokens = 8;
    let relay = f.relay(&fake);
    let (url, forward) = forwarded(&relay).await;
    for requests in 1..=2 {
        let mut response = reqwest::Client::new()
            .post(format!("{url}/v1/responses"))
            .body(body)
            .send()
            .await
            .unwrap();
        assert!(response.chunk().await.unwrap().is_some());
        assert_eq!(
            relay.totals(),
            RelayTotals {
                requests,
                input_tokens: (requests - 1) * 7 + body.len() as u64,
                output_tokens: (requests - 1) * 3 + 5,
            }
        );
        assert!(f.signals.lock().unwrap().is_empty());
        gate.notify_one();
        response.bytes().await.unwrap();
        assert_eq!(
            relay.totals(),
            RelayTotals {
                requests,
                input_tokens: requests * 7,
                output_tokens: requests * 3
            }
        );
    }
    assert_eq!(fake.requests.lock().unwrap().len(), 2);
    assert!(f.signals.lock().unwrap().is_empty());
    forward.abort();
}

#[tokio::test]
async fn usage_exceeding_either_reservation_is_charged_and_closes_admission() {
    for (input, output) in [(100, 3), (7, 6)] {
        let fake = Fake::new(
            "application/json",
            vec![
                json!({"usage":{"input_tokens":input,"output_tokens":output}})
                    .to_string()
                    .into_bytes(),
            ],
            None,
        )
        .await;
        let f = Fixture::new(RelayProvider::OpenAi);
        let relay = f.relay(&fake);
        let req = request(
            "/v1/responses",
            r#"{"model":"fixture","max_output_tokens":5}"#,
            "",
        );
        assert!(raw(&relay, &req).await.starts_with("HTTP/1.1 200"));
        assert_eq!(
            relay.totals(),
            RelayTotals {
                requests: 1,
                input_tokens: input,
                output_tokens: output
            }
        );
        assert!(raw(&relay, &req).await.starts_with("HTTP/1.1 429"));
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
        assert_eq!(*f.signals.lock().unwrap(), [RelaySignal::BudgetSpent]);
    }
}

#[tokio::test]
async fn cancelled_stream_keeps_its_reservation_and_admitted_request_count() {
    let gate = Arc::new(Notify::new());
    let fake = Fake::new("text/event-stream", vec![
        b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":1}}}\n\n".to_vec(),
        b"data: {}\n\n".to_vec(),
    ], Some(gate)).await;
    let mut f = Fixture::new(RelayProvider::Anthropic);
    f.spec.budget.max_requests = 1;
    let relay = f.relay(&fake);
    let body = r#"{"max_tokens":10,"model":"fixture","stream":true}"#;
    let mut stream = UnixStream::connect(relay.socket()).await.unwrap();
    stream
        .write_all(request("/v1/messages", body, "").as_bytes())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut bytes = vec![];
        while !bytes.ends_with(b"\n\n\r\n") {
            bytes.push(stream.read_u8().await.unwrap());
        }
    })
    .await
    .unwrap();
    let charged = RelayTotals {
        requests: 1,
        input_tokens: body.len() as u64,
        output_tokens: 10,
    };
    assert_eq!(relay.totals(), charged);
    let shared = relay.shared.clone();
    drop(relay);
    assert_eq!(shared.accounting.lock().unwrap().totals, charged);
    assert_eq!(f.logs()[0]["input_tokens"], charged.input_tokens);
    assert_eq!(f.logs()[0]["output_tokens"], charged.output_tokens);
    assert_eq!(f.logs()[0]["verdict"], "incomplete");
    assert_eq!(*f.signals.lock().unwrap(), [RelaySignal::BudgetSpent]);
}
