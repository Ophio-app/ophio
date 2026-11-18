// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use check_runner::runner::{
    self, CheckOutcome, CheckRequest, ProcessState, RunnerOptions, StopStatus,
};
use std::{fs, path::Path, time::Duration};

struct Fixture {
    _root: tempfile::TempDir,
    project: std::path::PathBuf,
    data: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let project = root_path.join("project");
        let data = root_path.join("data");
        fs::create_dir(&project).unwrap();
        Self {
            _root: root,
            project,
            data,
        }
    }
    fn request(&self, command: &str) -> CheckRequest {
        CheckRequest::new(&self.project, &self.data, command, Duration::from_secs(20))
    }
}
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}
fn assert_stopped_honestly(status: &StopStatus, unit: bool) {
    if unit {
        assert_eq!(*status, StopStatus::Confirmed);
    } else {
        assert!(matches!(status, StopStatus::Uncertain { .. }));
    }
}

#[tokio::test]
#[cfg_attr(
    target_os = "linux",
    ignore = "Run outside the agent's restricted user namespace through systemd-run"
)]
async fn success_failure_and_combined_output() {
    let f = Fixture::new();
    let run = runner::start(f.request("printf 'from stdout\\n'; printf 'from stderr\\n' >&2"))
        .await
        .unwrap();
    let unit = run.identity().sandbox_unit.is_some();
    let output = run.wait().await.unwrap();
    assert_eq!(output.outcome, CheckOutcome::Passed);
    assert_eq!(output.exit_code, Some(0));
    assert!(output.output_tail.contains("from stdout"));
    assert!(output.output_tail.contains("from stderr"));
    assert_stopped_honestly(&output.stop, unit);
    let output = runner::start(f.request("printf 'failing check\\n'; exit 7"))
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(output.outcome, CheckOutcome::Failed);
    assert_eq!(output.exit_code, Some(7));
}
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore = "Requires live bubblewrap")]
async fn sandbox_diagnostics_in_check_output_do_not_change_its_result() {
    let f = Fixture::new();
    for (code, expected) in [(0, CheckOutcome::Passed), (7, CheckOutcome::Failed)] {
        let command = format!(
            "printf 'bwrap: fixture diagnostic\\nsandbox-exec: fixture diagnostic\\n'; exit {code}"
        );
        let output = runner::start(f.request(&command))
            .await
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(output.outcome, expected, "{output:?}");
        assert_eq!(output.exit_code, Some(code));
        assert!(output.output_tail.contains("bwrap: fixture diagnostic"));
        assert!(
            output
                .output_tail
                .contains("sandbox-exec: fixture diagnostic")
        );
    }
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore = "Requires live bubblewrap")]
async fn normal_exit_stops_background_programs_in_the_owned_group() {
    let f = Fixture::new();
    let run = runner::start_with_options(
        f.request("(while :; do printf x >> heartbeat; sleep .03; done) & echo $! > background.pid; while [ ! -s heartbeat ]; do sleep .01; done; exit 0"),
        RunnerOptions { without_systemd: true },
    )
    .await
    .unwrap();
    let output = run.wait().await.unwrap();
    assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
    assert!(matches!(output.stop, StopStatus::Uncertain { .. }));
    let size = fs::metadata(f.project.join("heartbeat")).unwrap().len();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = fs::metadata(f.project.join("heartbeat")).unwrap().len();
    #[cfg(target_os = "macos")]
    if size != after {
        // The before-fix regression run must not leave its background fixture alive.
        let pid = fs::read_to_string(f.project.join("background.pid"))
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).unwrap(),
            rustix::process::Signal::KILL,
        );
    }
    assert_eq!(
        size, after,
        "A background program survived normal check exit"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore = "Requires live bubblewrap")]
async fn huge_output_keeps_only_a_bounded_tail() {
    let f = Fixture::new();
    let output = runner::start(f.request(
        "head -c 20000000 /dev/zero | tr '\\000' x; printf '\\nlast output marker\\n' >&2",
    ))
    .await
    .unwrap()
    .wait()
    .await
    .unwrap();
    assert_eq!(output.outcome, CheckOutcome::Passed);
    assert!(output.output_bytes >= 20_000_000);
    assert!(output.output_tail.len() <= runner::OUTPUT_LIMIT + 3);
    assert!(output.output_tail.ends_with("last output marker\n"));
}
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore = "Requires live bubblewrap")]
async fn timeout_and_cancellation_stop_the_tree() {
    let f = Fixture::new();
    let mut request = f.request("sleep 60");
    request.timeout = Duration::from_millis(200);
    let run = runner::start(request).await.unwrap();
    let unit = run.identity().sandbox_unit.is_some();
    let output = run.wait().await.unwrap();
    assert_eq!(output.outcome, CheckOutcome::TimedOut);
    assert!(output.duration_ms < 12_000);
    assert_stopped_honestly(&output.stop, unit);
    let run = runner::start(f.request("sleep 60")).await.unwrap();
    let cancellation = run.cancellation();
    cancellation.cancel();
    let output = run.wait().await.unwrap();
    assert_eq!(output.outcome, CheckOutcome::Cancelled);
    assert_stopped_honestly(&output.stop, unit);
}
#[tokio::test]
#[cfg_attr(
    target_os = "linux",
    ignore = "Requires live bubblewrap and a systemd user manager"
)]
async fn stored_identity_finds_and_stops_a_running_check() {
    let f = Fixture::new();
    let run = runner::start(f.request("sleep 60")).await.unwrap();
    let stored = serde_json::to_string(run.identity()).unwrap();
    let identity = serde_json::from_str(&stored).unwrap();
    assert_eq!(runner::inspect(&identity).await, ProcessState::Running);
    let stop = runner::stop(&identity).await;
    assert_stopped_honestly(&stop, run.identity().sandbox_unit.is_some());
    let _ = run.wait().await.unwrap();
    if identity.sandbox_unit.is_some() {
        assert_eq!(runner::inspect(&identity).await, ProcessState::Stopped);
    }
}
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore = "Requires live bubblewrap")]
async fn wrong_process_stamp_is_never_signalled() {
    let f = Fixture::new();
    let run = runner::start_with_options(
        f.request("sleep 60"),
        RunnerOptions {
            without_systemd: true,
        },
    )
    .await
    .unwrap();
    let mut identity = run.identity().clone();
    identity.start_time = identity.start_time.wrapping_add(1);
    assert!(matches!(
        runner::stop(&identity).await,
        StopStatus::Uncertain { .. }
    ));
    assert_eq!(runner::inspect(run.identity()).await, ProcessState::Running);
    run.cancel_and_wait().await.unwrap();
}
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore = "Requires live bubblewrap")]
async fn network_off_fails_quickly_and_network_on_reaches_the_fixture() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = server.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let _ = stream.read(&mut bytes).await;
            let _ = stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture",
                )
                .await;
        }
    });
    let f = Fixture::new();
    let command = format!(
        "python3 -c \"import urllib.request; print(urllib.request.urlopen('http://127.0.0.1:{port}', timeout=2).read().decode())\""
    );
    let began = std::time::Instant::now();
    let output = runner::start(f.request(&command))
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(output.outcome, CheckOutcome::Failed, "{output:?}");
    assert!(output.output_tail.contains("Network access is off"));
    assert!(began.elapsed() < Duration::from_secs(12));
    let mut allowed = f.request(&command);
    allowed.network = true;
    allowed.task_network_restricted = false;
    let output = runner::start(allowed).await.unwrap().wait().await.unwrap();
    assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
    assert!(output.output_tail.contains("fixture"));
    server_task.abort();
}
#[tokio::test]
#[cfg_attr(
    target_os = "linux",
    ignore = "Requires live bubblewrap and its network shim"
)]
async fn restricted_checks_reach_host_loopback_only_with_existing_mac_permission() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let f = Fixture::new();
    for without_systemd in [false, true] {
        for local_network in [false, true] {
            for through_proxy in [false, true] {
                let options = if through_proxy {
                    "--noproxy '' --proxytunnel --proxy \"$HTTPS_PROXY\""
                } else {
                    "--noproxy '*'"
                };
                let command = format!(
                    "curl --silent --show-error --fail --connect-timeout 1 --max-time 2 {options} http://{address}/"
                );
                let mut request = f.request(&command);
                request.network = true;
                request.local_network = local_network;
                let output = runner::start_with_options(request, RunnerOptions { without_systemd })
                    .await
                    .unwrap()
                    .wait()
                    .await
                    .unwrap();
                assert_eq!(output.outcome, CheckOutcome::Failed, "{output:?}");
                assert_eq!(output.sandbox_level, runner::SandboxLevel::Contained);
                if through_proxy {
                    assert!(
                        output.output_tail.contains("403"),
                        "proxy did not enforce its policy: {output:?}"
                    );
                }
                if cfg!(target_os = "macos") && local_network && !through_proxy {
                    // The Mac agent already has direct loopback with this task
                    // permission. The fixture accepts but deliberately never
                    // answers HTTP; the successful self-server test covers that.
                    assert!(
                        listener.accept().is_ok(),
                        "the granted loopback was blocked"
                    );
                } else {
                    assert_eq!(
                        listener.accept().unwrap_err().kind(),
                        std::io::ErrorKind::WouldBlock,
                        "a restricted check reached the host listener without permission"
                    );
                }
            }
        }
    }
}

#[test]
#[cfg_attr(target_os = "linux", ignore = "Requires live bubblewrap")]
fn provider_home_token_and_parent_environment_are_not_visible() {
    let f = Fixture::new();
    let home = f._root.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(
        home.join(".claude/.credentials.json"),
        "planted-private-token",
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "sanitized_environment_child",
            "--nocapture",
        ])
        .env("OPHIO_RUNNER_ENV_TEST", f._root.path())
        .env("HOME", &home)
        .env("GH_TOKEN", "planted-gh-token")
        .env("GITHUB_TOKEN", "planted-gh-token")
        .env("ANTHROPIC_API_KEY", "planted-provider-token")
        .env("OPENAI_API_KEY", "planted-provider-token")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[tokio::test]
#[ignore = "Only the isolated parent-environment test launches this helper"]
async fn sanitized_environment_child() {
    let Some(root) = std::env::var_os("OPHIO_RUNNER_ENV_TEST") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let token = root.join("home/.claude/.credentials.json");
    let command = format!(
        "if cat {} 2>/dev/null; then exit 91; fi; env",
        quoted(&token)
    );
    let output = runner::start(CheckRequest::new(
        root.join("project"),
        root.join("data"),
        command,
        Duration::from_secs(15),
    ))
    .await
    .unwrap()
    .wait()
    .await
    .unwrap();
    assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
    for secret in [
        "planted-private-token",
        "planted-gh-token",
        "planted-provider-token",
        "GH_TOKEN=",
        "GITHUB_TOKEN=",
        "ANTHROPIC_API_KEY=",
        "OPENAI_API_KEY=",
        "HTTP_PROXY=",
        "HTTPS_PROXY=",
        "ALL_PROXY=",
        "http_proxy=",
        "OPHIO_RUNNER_ENV_TEST=",
    ] {
        assert!(
            !output.output_tail.contains(secret),
            "{secret} leaked: {output:?}"
        );
    }
}
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore = "Requires live bubblewrap")]
async fn detached_grandchild_is_stopped_or_reported_uncertain() {
    for without_systemd in [false, true] {
        let f = Fixture::new();
        fs::write(f.project.join("detach.py"),"import os,time\np=os.fork()\nif p==0:\n os.setsid()\n p=os.fork()\n if p: os._exit(0)\n with open('detached.pid','w') as f: f.write(str(os.getpid()))\n while True:\n  with open('heartbeat','a') as f: f.write('x')\n  time.sleep(.03)\nwhile True: time.sleep(1)\n").unwrap();
        let run = runner::start_with_options(
            f.request("python3 detach.py"),
            RunnerOptions { without_systemd },
        )
        .await
        .unwrap();
        let unit = run.identity().sandbox_unit.is_some();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !f.project.join("heartbeat").exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let output = run.cancel_and_wait().await.unwrap();
        assert_eq!(output.outcome, CheckOutcome::Cancelled);
        assert_stopped_honestly(&output.stop, unit);
        let size = fs::metadata(f.project.join("heartbeat")).unwrap().len();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let after = fs::metadata(f.project.join("heartbeat")).unwrap().len();
        if unit {
            assert_eq!(size, after, "A detached descendant survived the unit stop");
        }
        #[cfg(target_os = "macos")]
        {
            let pid = fs::read_to_string(f.project.join("detached.pid"))
                .unwrap()
                .parse::<i32>()
                .unwrap();
            assert!(pid > 1);
            let _ = rustix::process::kill_process(
                rustix::process::Pid::from_raw(pid).unwrap(),
                rustix::process::Signal::KILL,
            );
        }
    }
}
#[tokio::test]
#[cfg_attr(
    target_os = "linux",
    ignore = "Requires live bubblewrap and pre-cached toolchains"
)]
async fn offline_rust_and_node_projects_can_run_their_checks() {
    let f = Fixture::new();
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .canonicalize()
        .unwrap();
    let reads = check_runner::toolchains::readable_toolchains(&home, &[], true).unwrap();
    fs::create_dir(f.project.join("src")).unwrap();
    fs::write(f.project.join("Cargo.toml"),"[package]\nname='offline-sample'\nversion='0.1.0'\nedition='2021'\n[dependencies]\nitoa='1'\n").unwrap();
    fs::write(
        f.project.join("src/lib.rs"),
        "#[test] fn cached_dependency_works(){assert_eq!(itoa::Buffer::new().format(42),\"42\");}",
    )
    .unwrap();
    let mut request = f.request("cargo test --offline");
    request.timeout = Duration::from_secs(120);
    request.read_toolchains = reads;
    let output = runner::start(request).await.unwrap().wait().await.unwrap();
    assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
    fs::create_dir_all(f.project.join("node_modules/local-fixture")).unwrap();
    fs::write(
        f.project.join("node_modules/local-fixture/index.js"),
        "module.exports=42;",
    )
    .unwrap();
    fs::write(f.project.join("package.json"),"{\"name\":\"offline-sample\",\"scripts\":{\"test\":\"node -e \\\"require('assert').strictEqual(require('local-fixture'),42)\\\"\"}}").unwrap();
    let output = runner::start(f.request("npm test --offline"))
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
}

#[tokio::test]
#[cfg_attr(
    target_os = "linux",
    ignore = "Requires live namespaces and filtered proxy"
)]
async fn a_check_can_fetch_its_own_local_server_without_using_the_proxy() {
    let f = Fixture::new();
    let script = r#"
import http.server, subprocess, threading
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'local-check-server')
    def log_message(self, *args):
        pass
server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
thread = threading.Thread(target=server.handle_request, daemon=True)
thread.start()
result = subprocess.run(['curl', '--fail', '--silent', '--show-error', '--max-time', '3',
                         'http://127.0.0.1:%d/' % server.server_port], capture_output=True, text=True)
print(result.stdout, end='')
print(result.stderr, end='')
assert result.returncode == 0, result.returncode
thread.join(timeout=2)
assert not thread.is_alive()
server.server_close()
"#;
    let command = format!("python3 -c '{}'", script.replace('\'', "'\\''"));
    let mut request = f.request(&command);
    request.network = true;
    // Linux has a private loopback. The Mac shares the host's loopback, and its
    // agent profile permits outbound loopback only with this task permission.
    request.local_network = cfg!(target_os = "macos");
    let output = runner::start(request).await.unwrap().wait().await.unwrap();
    assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
    assert!(
        output.output_tail.contains("local-check-server"),
        "{output:?}"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn an_offline_mac_check_can_accept_a_local_request_without_outbound_access() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let f = Fixture::new();
    let script = r#"
import pathlib, socket
with socket.socket() as server:
    server.bind(('127.0.0.1', 0))
    server.listen(1)
    server.settimeout(10)
    print('offline-listening', flush=True)
    # The offline agent can listen but cannot initiate connections on the
    # Mac's shared loopback. Checks must retain exactly that distinction.
    with socket.socket() as outgoing:
        outgoing.settimeout(2)
        try:
            outgoing.connect(server.getsockname())
        except PermissionError:
            pass
        else:
            raise AssertionError('offline outbound loopback was permitted')
    print('offline-loopback-blocked', flush=True)
    pathlib.Path('server.port').write_text(str(server.getsockname()[1]))
    connection, _ = server.accept()
    with connection:
        connection.recv(4096)
        connection.sendall(b'HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\noffline')
"#;
    let command = format!("python3 -c '{}'", script.replace('\'', "'\\''"));
    let run = runner::start(f.request(&command)).await.unwrap();
    // The native suite can start Python slowly under load. Keep readiness
    // within the check's overall timeout instead of abandoning it after 3 s.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let port = loop {
        if let Ok(text) = fs::read_to_string(f.project.join("server.port"))
            && let Ok(port) = text.parse::<u16>()
        {
            break Some(port);
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let mut response = String::new();
    if let Some(port) = port {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), stream.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
    }
    let output = run.wait().await.unwrap();
    assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
    assert!(response.ends_with("offline"), "{response:?}");
}
