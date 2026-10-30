// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]
use protocol::time::Timestamp;
use report_fixer::{
    config::{AgentCli, Config},
    execution::{Limits, SandboxedAgents},
    *,
};
use report_pipeline::{
    honeytoken::Honeytokens,
    schema::{ReproTest, TriageContext, parse_triage},
    signals::Signal,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn shim() -> PathBuf {
    std::env::var_os("OPHIO_TEST_SANDBOX_SHIM")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("ophio-sandbox-shim")
        })
}
#[tokio::test]
#[ignore = "requires real user namespaces"]
async fn real_agent_contains_host_reads_writes_network_and_relay_canary() {
    let root = tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    std::fs::create_dir(&stage).unwrap();
    let canaries = Arc::new(Honeytokens::generate().unwrap());
    canaries.plant(&stage).unwrap();
    let workspace = stage.join("clone");
    std::fs::create_dir_all(workspace.join("client/src/state")).unwrap();
    std::fs::write(
        workspace.join("client/src/state/existing.ts"),
        "export const x=0;\n",
    )
    .unwrap();
    repository::git(&workspace, &["init"]).unwrap();
    repository::git(&workspace, &["add", "client"]).unwrap();
    repository::git(&workspace, &["commit", "-m", "Containment fixture"]).unwrap();
    let base = repository::text(&workspace, &["rev-parse", "HEAD"]).unwrap();
    let v = parse_triage(br#"{"category":"bug","duplicate_of":null,"docs":{"relation":"silent","doc":null},"area":"work","question":null,"confidence":"high","repro":{"kind":"vitest","path":"client/src/state/repro.test.ts"}}"#, TriageContext { candidates: 0, docs: 0 }).unwrap();
    let test = ReproTest::validate(
        &workspace,
        &base,
        v.repro().unwrap(),
        b"test('x',()=>{expect(true).toBe(true);});",
    )
    .unwrap();
    std::fs::remove_dir_all(workspace.join(".git")).unwrap();
    let key = root.path().join("provider-key");
    let github = root.path().join("github-token");
    let host_ssh = root.path().join("host-home/.ssh/id_ed25519");
    let data = root.path().join("host-data/database");
    for p in [&key, &github, &host_ssh, &data] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "synthetic-host-secret").unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let provider = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = vec![];
            while !header.ends_with(b"\r\n\r\n") {
                header.push(stream.read_u8().await.unwrap());
            }
            let header = String::from_utf8(header).unwrap();
            let length: usize = header
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|s| s.parse().ok())
                })
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            assert!(header.contains("synthetic-host-secret"));
            assert!(!String::from_utf8_lossy(&body).contains("GENERATED_CANARY"));
            assert!(!String::from_utf8_lossy(&body).contains("synthetic-host-secret"));
            seen.fetch_add(1, Ordering::SeqCst);
            let body = br#"{"usage":{"input_tokens":1,"output_tokens":1}}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
            stream.write_all(body).await.unwrap();
        }
    });
    let output_dir = stage.join("output");
    std::fs::create_dir(&output_dir).unwrap();
    let output = output_dir.join("result.json");
    let outside = root.path().join("outside");
    let script = workspace.join("scripted-agent");
    let python = format!(
        r#"
import json, os, socket, urllib.request, urllib.error
for path in {secrets}:
    try:
        open(path, 'rb').read()
    except (FileNotFoundError, PermissionError):
        pass
    else:
        raise AssertionError('host read succeeded')
# /tmp is the sandbox's own tmpfs, so this may succeed there; the host check below decides.
try:
    open({outside}, 'w').write('escape')
except OSError:
    pass
for host, port in [('127.0.0.1', {port}), ('198.51.100.1', 443)]:
    try:
        socket.create_connection((host, port), timeout=0.3)
    except OSError:
        pass
    else:
        raise AssertionError('network escape')
def request(text):
    body=json.dumps(dict(model='fake-model',max_output_tokens=12,input=text)).encode()
    return urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:18080/v1/responses',data=body,headers={{'Content-Type':'application/json'}}),timeout=3)
assert request('safe').status == 200
for watched in [open('.env').read(), 'synthetic-host-secret']:
    try:
        request(watched)
    except urllib.error.HTTPError as error:
        assert error.code == 403
    else:
        raise AssertionError('relay sent watched value')
open({output},'w').write('{{}}')
"#,
        secrets = serde_json::to_string(&[&key, &github, &host_ssh, &data]).unwrap(),
        outside = serde_json::to_string(&outside).unwrap(),
        port = address.port(),
        output = serde_json::to_string(&output).unwrap()
    );
    std::fs::write(
        &script,
        format!("#!/bin/sh\ncat >/dev/null\nexec /usr/bin/python3 - <<'PY'\n{python}\nPY\n"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let runner = SandboxedAgents {
        config: Config {
            agent_cli: AgentCli::Codex,
            agent_program: script,
            intake_token_file: key.clone(),
            provider_key_file: key,
            state: root.path().into(),
            shim: shim(),
            fix_model: "fake-model".into(),
            ..Default::default()
        },
        limits: Limits::default(),
        test_upstream: Some(address),
    };
    let out = runner
        .run(&AgentJob {
            prompt: Prompt::fix(&v, test.reduced()).unwrap(),
            workspace,
            home: stage.join("home"),
            output,
            canaries,
            input_limit: 10000,
            output_limit: 1000,
            deadline_ms: Timestamp::now().0 + 30000,
        })
        .await
        .unwrap();
    assert!(out.signals.contains(&Signal::CanarySeen));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(!outside.exists());
    provider.abort();
}

#[tokio::test]
#[ignore = "requires real user namespaces"]
async fn real_tests_have_no_provider_socket_or_host_access() {
    use report_pipeline::gates::TestResult;
    let raw = br#"#[test] fn held_in() {
        assert!(std::net::TcpStream::connect_timeout(&"127.0.0.1:18080".parse().unwrap(), std::time::Duration::from_millis(200)).is_err());
        assert!(std::net::TcpStream::connect_timeout(&"198.51.100.1:443".parse().unwrap(), std::time::Duration::from_millis(200)).is_err());
        assert!(std::env::var("OPHIO_TEST_HOST_SECRET").is_err());
        assert_eq!(0, 1);
    }"#.to_vec();
    let result = offline_repro(
        raw,
        Limits {
            disk_bytes: 64 * 1024 * 1024,
            ..Limits::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(result.repro, TestResult::Failed);
    assert!(result.suites.values().all(|r| *r == TestResult::Passed));
}

async fn offline_repro(raw: Vec<u8>, limits: Limits) -> Result<TestRun> {
    use report_fixer::execution::SandboxedTests;
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir_all(repo.join("crates/sample/tests")).unwrap();
    std::fs::create_dir_all(repo.join("crates/sample/src")).unwrap();
    std::fs::write(
        repo.join("crates/sample/src/lib.rs"),
        "// SPDX-License-Identifier: Apache-2.0\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[workspace]\nresolver='3'\nmembers=['crates/sample']\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("crates/sample/Cargo.toml"),
        "[package]\nname='sample'\nversion='0.1.0'\nedition='2024'\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("crates/sample/tests/existing.rs"),
        "#[test] fn existing() {}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("Cargo.lock"),
        // Exactly what cargo writes, so the read-only workspace needs no lock update.
        "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"sample\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    repository::git(&repo, &["init"]).unwrap();
    repository::git(&repo, &["add", "."]).unwrap();
    repository::git(&repo, &["commit", "-m", "Offline fixture"]).unwrap();
    let base = repository::text(&repo, &["rev-parse", "HEAD"]).unwrap();
    let v = parse_triage(br#"{"category":"bug","duplicate_of":null,"docs":{"relation":"silent","doc":null},"area":"terminal","question":null,"confidence":"high","repro":{"kind":"rust_integration","path":"crates/sample/tests/repro.rs"}}"#, TriageContext { candidates: 0, docs: 0 }).unwrap();
    let cargo_cache = root.path().join("cargo-cache");
    let npm_cache = root.path().join("npm-cache");
    std::fs::create_dir(&cargo_cache).unwrap();
    std::fs::create_dir(&npm_cache).unwrap();
    let runner = SandboxedTests {
        shim: shim(),
        scratch: root.path().into(),
        cargo_cache,
        npm_cache,
        limits,
    };
    runner
        .run(TestJob {
            repository: repo,
            commit: base,
            target: v.repro().unwrap().clone(),
            raw,
            deadline_ms: Timestamp::now().0 + 30000,
            repro_only: false,
        })
        .await
}

#[tokio::test]
#[ignore = "requires real user namespaces"]
async fn real_test_home_and_target_share_an_aggregate_disk_budget() {
    for name in ["HOME", "CARGO_TARGET_DIR"] {
        let raw = format!(r#"#[test] fn disk_flood() {{
            let dir = std::path::PathBuf::from(std::env::var("{name}").unwrap());
            for n in 0..128 {{ std::fs::write(dir.join(format!("flood-{{n}}")), vec![0; 1024 * 1024]).unwrap(); }}
            std::thread::sleep(std::time::Duration::from_secs(10));
        }}"#).into_bytes();
        let limits = Limits {
            disk_bytes: 64 * 1024 * 1024,
            ..Limits::default()
        };
        assert!(
            matches!(
                offline_repro(raw, limits).await,
                Err(Error::Signal(Signal::BudgetSpent))
            ),
            "{name}"
        );
    }
}

#[tokio::test]
#[ignore = "requires real user namespaces"]
async fn real_agent_disk_floods_stop_the_entire_process_tree() {
    for location in [
        "workspace",
        "home",
        "output",
        "provider",
        "tmp",
        "run",
        "shm",
        "root",
    ] {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("stage");
        std::fs::create_dir(&stage).unwrap();
        let canaries = Arc::new(Honeytokens::generate().unwrap());
        canaries.plant(&stage).unwrap();
        let workspace = stage.join("clone");
        let output_dir = stage.join("output");
        std::fs::create_dir(&output_dir).unwrap();
        let key = root.path().join("fake-key");
        std::fs::write(&key, "obviously-fake-provider-key").unwrap();
        let script = root.path().join("agent");
        let python = format!(
            r#"
import os, time
paths = dict(workspace=os.getcwd(), home=os.environ['HOME'], output=os.environ['OPHIO_REPORT_OUTPUT_DIR'], provider='/run/ophio-provider', tmp='/tmp', run='/run', shm='/dev/shm', root='/')
open(os.path.join(paths['output'], 'started'), 'w').write('started')
pid = os.fork()
if pid == 0:
    os.setsid()
    try:
        for n in range(512):
            with open(os.path.join(paths['{location}'], 'flood-'+str(n)), 'wb') as f:
                f.write(bytes(4096))
    except OSError:
        pass
    time.sleep(1)
    open(os.path.join(paths['output'], 'survived'), 'w').write('tree escaped')
time.sleep(10)
"#
        );
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat >/dev/null\nexec /usr/bin/python3 - <<'PY'\n{python}\nPY\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir_all(workspace.join("client/src/state")).unwrap();
        std::fs::write(
            workspace.join("client/src/state/existing.ts"),
            "export const fixture = 0;",
        )
        .unwrap();
        repository::git(&workspace, &["init"]).unwrap();
        repository::git(&workspace, &["add", "client"]).unwrap();
        repository::git(&workspace, &["commit", "-m", "Disk fixture"]).unwrap();
        let base = repository::text(&workspace, &["rev-parse", "HEAD"]).unwrap();
        let verdict = parse_triage(br#"{"category":"bug","duplicate_of":null,"docs":{"relation":"silent","doc":null},"area":"work","question":null,"confidence":"high","repro":{"kind":"vitest","path":"client/src/state/repro.test.ts"}}"#, TriageContext { candidates: 0, docs: 0 }).unwrap();
        let repro = ReproTest::validate(
            &workspace,
            &base,
            verdict.repro().unwrap(),
            b"test('fixture', () => {});",
        )
        .unwrap();
        std::fs::remove_dir_all(workspace.join(".git")).unwrap();
        let runner = SandboxedAgents {
            config: Config {
                agent_program: script,
                intake_token_file: key.clone(),
                provider_key_file: key,
                state: root.path().into(),
                shim: shim(),
                fix_model: "fake-model".into(),
                ..Config::default()
            },
            limits: Limits {
                disk_bytes: 1024 * 1024,
                ..Limits::default()
            },
            test_upstream: None,
        };
        let result = runner
            .run(&AgentJob {
                prompt: Prompt::fix(&verdict, repro.reduced()).unwrap(),
                workspace,
                home: stage.join("home"),
                output: output_dir.join("result.json"),
                canaries,
                input_limit: 10000,
                output_limit: 1000,
                deadline_ms: Timestamp::now().0 + 30000,
            })
            .await;
        assert!(
            matches!(result, Err(Error::Signal(Signal::BudgetSpent))),
            "{location}: {result:?}"
        );
        assert!(
            output_dir.join("started").exists(),
            "{location}: stopped before workload"
        );
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert!(!output_dir.join("survived").exists(), "{location}");
    }
}
