// SPDX-License-Identifier: Apache-2.0
//! Account-safe Mac fixtures: authored shells, dummy credentials and zero
//! provider budget only. These tests never need installed agents or a keychain.
use super::*;
use ports::{
    harness::RunEnv,
    sandbox::{RelayBudget, RelayPolicy, RelaySpec},
};
use std::{ffi::OsString, net::TcpListener, process::Output};

const SECRET: &str = "fixture-host-only-credential";

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    state: PathBuf,
    host: PathBuf,
    upstream: TcpListener,
    previous_keychain: Option<PathBuf>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("mac-relay-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let base = root.path().canonicalize().unwrap();
        for name in ["home", "cwd", "state", "host"] {
            std::fs::create_dir(base.join(name)).unwrap();
        }
        let home = base.join("home");
        for provider in [".claude", ".codex"] {
            std::fs::create_dir(home.join(provider)).unwrap();
            for file in [
                "auth.json",
                ".credentials.json",
                "config.toml",
                "settings.json",
            ] {
                std::fs::write(home.join(provider).join(file), SECRET).unwrap();
            }
        }
        std::fs::write(base.join("cwd/allowed"), "project fixture").unwrap();
        std::fs::write(base.join("host/key"), SECRET).unwrap();
        std::fs::write(base.join("host/private"), SECRET).unwrap();
        // A future accidental call into sign-in code may fail, but must never
        // search the person's real login keychain. No keychain is created.
        let previous_keychain =
            TEST_KEYCHAIN.with(|k| k.replace(Some(base.join("absent.keychain"))));
        Self {
            home,
            cwd: base.join("cwd"),
            state: base.join("state"),
            host: base.join("host"),
            upstream: TcpListener::bind("127.0.0.1:0").unwrap(),
            previous_keychain,
            _root: root,
        }
    }
    fn spec(&self) -> SandboxSpec {
        SandboxSpec {
            offline: false,
            reach: Reach {
                read: vec![],
                write: vec![],
                whole_read: false,
                whole_write: false,
                internet: false,
                local_network: false,
            },
            harness: HarnessKind::Codex,
            cwd: self.cwd.clone(),
            state: self.state.clone(),
            always: vec![],
            programs: vec![],
            candidates: vec![],
            private: vec![],
            given: vec![self.home.join(".codex/auth.json")],
            on_blocked: None,
            relay: Some(RelaySpec {
                provider: RelayProvider::OpenAi,
                key_file: self.host.join("key"),
                // Zero budget and a pinned local fake upstream: even a broken
                // admission assertion cannot make a public provider request.
                budget: RelayBudget {
                    max_requests: 0,
                    max_input_tokens: 0,
                    max_output_tokens: 0,
                    deadline: None,
                },
                policy: RelayPolicy::default(),
                log: self.host.join("audit.jsonl"),
                on_signal: None,
                inspect: None,
                test_upstream: Some(self.upstream.local_addr().unwrap()),
            }),
        }
    }
    fn env(&self) -> RunEnv {
        RunEnv(vec![
            ("HOME".into(), self.home.to_string_lossy().into_owned()),
            ("ANTHROPIC_API_KEY".into(), SECRET.into()),
            ("OPENAI_API_KEY".into(), SECRET.into()),
            ("HTTPS_PROXY".into(), "http://untrusted.invalid".into()),
            ("FIXTURE_INHERITED_SECRET".into(), SECRET.into()),
        ])
    }
    fn wrap(&self, sandbox: &MacSandbox) -> Wrapped {
        sandbox
            .wrap(WrapRequest {
                program: Path::new("/bin/sh"),
                visible: &[],
                env: &self.env(),
                resume: None,
                disabled: &ports::Disabled::default(),
            })
            .unwrap()
    }
    fn run(&self, wrapped: &Wrapped, line: &str) -> Output {
        let mut command = Command::new(&wrapped.program);
        command.args(&wrapped.args).args(["/bin/sh", "-c", line]);
        command.env("FIXTURE_INHERITED_SECRET", SECRET);
        if wrapped.clear_env {
            command.env_clear();
        }
        command.envs(wrapped.env.iter().map(|(key, value)| (key, value)));
        let output = inherit::output(command.current_dir(&self.cwd)).unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("sandbox-exec:"),
            "profile did not start: {stderr}"
        );
        output
    }
    fn refused(&self, wrapped: &Wrapped, line: &str) {
        let output = self.run(wrapped, line);
        assert!(!output.status.success(), "unexpected success: {line}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        TEST_KEYCHAIN.with(|k| *k.borrow_mut() = self.previous_keychain.take());
    }
}

#[test]
fn relay_uses_fresh_state_and_clears_credentials_without_touching_accounts() {
    for harness in [HarnessKind::Codex, HarnessKind::ClaudeCode] {
        let f = Fixture::new();
        let mut spec = f.spec();
        spec.harness = harness;
        spec.relay.as_mut().unwrap().provider = if harness == HarnessKind::Codex {
            RelayProvider::OpenAi
        } else {
            RelayProvider::Anthropic
        };
        let sandbox = MacSandbox::new(spec).unwrap();
        let wrapped = f.wrap(&sandbox);
        assert!(wrapped.clear_env);
        assert!(sandbox.proxy.is_none());
        assert!(!wrapped.shared_skills);
        assert!(wrapped.env.iter().all(|(_, value)| !value.contains(SECRET)));
        let local_key = sandbox.relay.as_ref().unwrap().local_key().unwrap();
        assert!(
            !wrapped
                .args
                .iter()
                .any(|a| a.to_string_lossy().contains(local_key))
        );
        assert!(!format!("{sandbox:?}").contains(local_key));
        assert!(!format!("{wrapped:?}").contains(local_key));
        assert!(!format!("{:?}", Some(vec![wrapped.clone()])).contains(local_key));
        assert!(
            !wrapped
                .args
                .iter()
                .any(|a| a.to_string_lossy().contains(SECRET))
        );
        let run = sandbox.run.lock().unwrap();
        assert!(run.keychain.is_none() && run.credentials.is_none() && run.history.is_none());
        drop(run);
        let line = r#"set -eu
            test -z "${FIXTURE_INHERITED_SECRET-}${OPENAI_API_KEY-}${HTTPS_PROXY-}"
            local_key="${OPHIO_RELAY_DUMMY_KEY-${ANTHROPIC_API_KEY-}}"
            test "${#local_key}" -eq 64
            c="${CODEX_HOME-${CLAUDE_CONFIG_DIR-}}"
            test -n "$c" && test -d "$c"
            test -z "$(/bin/ls -A "$c")"
            /bin/cat allowed
            echo scratch > "$TMPDIR/file"
            /bin/cat "$TMPDIR/file"
        "#;
        let output = f.run(&wrapped, line);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for path in [
            f.host.join("key"),
            f.host.join("audit.jsonl"),
            f.home.join(".codex/auth.json"),
            f.home.join(".claude/.credentials.json"),
        ] {
            f.refused(
                &wrapped,
                &format!("/bin/cat {}", sh_word(&path.to_string_lossy())),
            );
        }
        assert_eq!(std::fs::read_to_string(f.host.join("key")).unwrap(), SECRET);
    }
}

#[test]
fn relay_refuses_resumption_wrong_provider_offline_and_whole_computer_grants() {
    let f = Fixture::new();
    for variant in 0..4 {
        let mut spec = f.spec();
        match variant {
            0 => spec.relay.as_mut().unwrap().provider = RelayProvider::Anthropic,
            1 => spec.offline = true,
            2 => spec.reach.whole_read = true,
            _ => spec.reach.whole_write = true,
        }
        assert!(MacSandbox::new(spec).is_err());
    }
    let sandbox = MacSandbox::new(f.spec()).unwrap();
    assert!(
        sandbox
            .wrap(WrapRequest {
                program: Path::new("/bin/sh"),
                visible: &[],
                env: &f.env(),
                resume: Some("old-session"),
                disabled: &ports::Disabled::default()
            })
            .is_err()
    );
    assert!(!f.home.join(".codex/sessions").exists());
}

#[test]
fn relay_rejects_grants_exposing_keys_audit_accounts_or_aliases() {
    for variant in 0..7 {
        let f = Fixture::new();
        let mut spec = f.spec();
        let mut visible = Vec::new();
        match variant {
            0 => spec.reach.read.push(f.host.clone()),
            1 => spec.always.push(f.host.clone()),
            2 => visible.push(f.host.join("key")),
            3 => visible.push(f.host.join("audit.jsonl")),
            4 => spec.cwd = f.host.clone(),
            5 => spec.reach.read.push(f.home.clone()),
            _ => {
                let alias = f.cwd.join("alias");
                std::os::unix::fs::symlink(f.host.join("key"), &alias).unwrap();
                visible.push(alias);
            }
        }
        let sandbox = MacSandbox::new(spec).unwrap();
        assert!(
            sandbox
                .wrap(WrapRequest {
                    program: Path::new("/bin/sh"),
                    visible: &visible,
                    env: &f.env(),
                    resume: None,
                    disabled: &ports::Disabled::default()
                })
                .is_err(),
            "variant {variant}"
        );
    }
    for file in ["key", "audit.jsonl"] {
        let f = Fixture::new();
        if file == "audit.jsonl" {
            std::fs::write(f.host.join(file), "fixture audit").unwrap();
        }
        std::fs::hard_link(f.host.join(file), f.cwd.join("alias")).unwrap();
        assert!(MacSandbox::new(f.spec()).is_err());
    }
    let f = Fixture::new();
    std::os::unix::fs::symlink(f.cwd.join("allowed"), f.host.join("audit.jsonl")).unwrap();
    assert!(MacSandbox::new(f.spec()).is_err());
}

#[test]
fn relay_has_one_network_destination_even_with_network_grants() {
    let f = Fixture::new();
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let other_port = other.local_addr().unwrap().port();
    let unix = f.cwd.join("bridge.sock");
    let _bridge = std::os::unix::net::UnixListener::bind(&unix).unwrap();
    let mut spec = f.spec();
    spec.reach.internet = true;
    spec.reach.local_network = true;
    spec.always.push(f.cwd.clone());
    let sandbox = MacSandbox::new(spec).unwrap();
    let wrapped = f.wrap(&sandbox);
    let port = sandbox.relay().unwrap().port;
    assert!(!sandbox.run.lock().unwrap().loopback);
    let line = format!(
        r#"/usr/bin/python3 -I -c '
import socket, os
s=socket.create_connection(("127.0.0.1",{port}),2)
s.sendall(b"GET / HTTP/1.1\r\nHost: probe\r\n\r\n")
assert b"401" in s.recv(1024)
s.close()
s=socket.create_connection(("127.0.0.1",{port}),2)
body=b"{{\"model\":\"fixture\"}}"
auth=os.environ["OPHIO_RELAY_DUMMY_KEY"].encode()
s.sendall(b"POST /v1/responses HTTP/1.1\r\nHost: relay\r\nAuthorization: Bearer "+auth+b"\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n" % len(body)+body)
assert b"429" in s.recv(1024)
s.close()
for address in [("127.0.0.1",{other_port}),("::1",{other_port}),("192.0.2.1",443)]:
    try:
        s=socket.create_connection(address,1)
    except OSError:
        continue
    s.close()
    raise AssertionError("network bypass")
for action in ["bind", "udp", "unix"]:
    try:
        if action == "bind":
            s=socket.socket();s.bind(("127.0.0.1",0))
        elif action == "udp":
            s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.sendto(b"fixture",("127.0.0.1",{other_port}))
        else:
            s=socket.socket(socket.AF_UNIX);s.connect("{unix}")
    except OSError:
        continue
    finally:
        s.close()
    raise AssertionError(action+" bypass")
print("relay only")
'"#,
        unix = unix.display()
    );
    let output = f.run(&wrapped, &line);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "relay only");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut reach = sandbox.spec.reach.clone();
    reach.internet = false;
    reach.local_network = false;
    assert_eq!(runtime.block_on(sandbox.update(reach)), Applied::Applied);
    assert_eq!(sandbox.relay().unwrap().port, port);
    assert!(
        runtime
            .block_on(sandbox.connect(&[other.local_addr().unwrap()]))
            .is_err()
    );
    assert_eq!(sandbox.relay_totals(), Some(RelayTotals::default()));
}

#[test]
fn changed_or_invalid_grants_stay_pending_until_a_fresh_profile_starts() {
    let f = Fixture::new();
    let sandbox = MacSandbox::new(f.spec()).unwrap();
    let wrapped = f.wrap(&sandbox);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let original = sandbox.spec.reach.clone();
    let mut changed = original.clone();
    changed.read.push(f.host.clone());
    assert!(matches!(
        runtime.block_on(sandbox.update(changed)),
        Applied::NeedsRestart(_)
    ));
    assert!(matches!(
        runtime.block_on(sandbox.update(original)),
        Applied::NeedsRestart(_)
    ));
    f.refused(
        &wrapped,
        &format!(
            "/bin/cat {}",
            sh_word(&f.host.join("private").to_string_lossy())
        ),
    );

    let next = f.cwd.join("new-grant");
    std::fs::create_dir(&next).unwrap();
    std::fs::write(next.join("file"), "new grant").unwrap();
    let second = MacSandbox::new(f.spec()).unwrap();
    let original = second.spec.reach.clone();
    let mut invalid = original.clone();
    invalid.read.push(f.cwd.join("missing"));
    assert!(matches!(
        runtime.block_on(second.update(invalid)),
        Applied::NeedsRestart(_)
    ));
    assert!(matches!(
        runtime.block_on(second.update(original)),
        Applied::NeedsRestart(_)
    ));

    // Stale permission changes are not acknowledged, while a replacement run
    // really builds and enforces its own new write permission.
    let mut replacement = f.spec();
    replacement.reach.write.push(next.clone());
    let replacement = MacSandbox::new(replacement).unwrap();
    let output = f.run(
        &f.wrap(&replacement),
        &format!(
            "echo replacement > {}",
            sh_word(&next.join("file").to_string_lossy())
        ),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(next.join("file")).unwrap().trim(),
        "replacement"
    );
}

#[test]
fn loopback_grant_changes_and_failed_offline_updates_cannot_be_acknowledged_as_live() {
    let f = Fixture::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut spec = f.spec();
    spec.relay = None;
    let sandbox = MacSandbox::new(spec).unwrap();
    let original = sandbox.spec.reach.clone();
    let mut granted = original.clone();
    granted.internet = true;
    granted.local_network = true;
    assert!(matches!(
        runtime.block_on(sandbox.update(granted)),
        Applied::NeedsRestart(_)
    ));
    assert!(matches!(
        runtime.block_on(sandbox.update(original)),
        Applied::NeedsRestart(_)
    ));

    let mut spec = f.spec();
    spec.relay = None;
    spec.offline = true;
    let offline = MacSandbox::new(spec).unwrap();
    let original = offline.spec.reach.clone();
    let mut forbidden = original.clone();
    forbidden.internet = true;
    assert!(matches!(
        runtime.block_on(offline.update(forbidden)),
        Applied::NeedsRestart(_)
    ));
    assert!(matches!(
        runtime.block_on(offline.update(original)),
        Applied::NeedsRestart(_)
    ));
}

#[test]
fn finished_closes_the_relay_even_if_the_sandbox_is_retained() {
    let f = Fixture::new();
    let sandbox = MacSandbox::new(f.spec()).unwrap();
    let wrapped = f.wrap(&sandbox);
    let endpoint = ("127.0.0.1", sandbox.relay().unwrap().port);
    let mut partial = TcpStream::connect(endpoint).unwrap();
    partial
        .write_all(b"POST /v1/responses HTTP/1.1\r\n")
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(sandbox.finished()).unwrap();
    runtime.block_on(sandbox.finished()).unwrap();
    assert!(TcpStream::connect(endpoint).is_err());
    // No dummy credential is embedded in the sandbox-exec argument list.
    let args = wrapped.args.into_iter().collect::<Vec<OsString>>();
    assert!(!format!("{args:?}").contains(SECRET));
}
