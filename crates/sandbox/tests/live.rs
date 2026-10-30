// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]
use ports::sandbox::*;
use protocol::capability::HarnessKind;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::io::{AsyncBufReadExt, BufReader};

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    a: PathBuf,
    b: PathBuf,
    config: PathBuf,
    state: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let a = home.join("A");
        let b = home.join("B");
        let config = home.join(".codex");
        let state = root.path().join("state");
        for p in [
            &a,
            &b,
            &config,
            &home.join(".claude"),
            &home.join(".config/ophio"),
        ] {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(config.join("other-history"), "secret").unwrap();
        std::fs::write(home.join(".claude/other-history"), "secret").unwrap();
        std::fs::write(home.join(".claude.json"), "private metadata").unwrap();
        std::fs::write(home.join(".config/ophio/secret"), "private").unwrap();
        Self {
            _root: root,
            home,
            a,
            b,
            config,
            state,
        }
    }
    fn reach(&self) -> Reach {
        Reach {
            read: vec![self.a.clone(), self.b.clone()],
            write: vec![],
            whole_read: false,
            whole_write: false,
            internet: false,
            local_network: false,
        }
    }
    fn sandbox(&self, reach: Reach, candidates: Vec<PathBuf>) -> Arc<dyn Sandbox> {
        self.sandbox_given(reach, candidates, vec![], vec![])
    }
    fn sandbox_given(
        &self,
        reach: Reach,
        candidates: Vec<PathBuf>,
        private: Vec<PathBuf>,
        given: Vec<PathBuf>,
    ) -> Arc<dyn Sandbox> {
        sandbox::Bubblewrap::new(env!("CARGO_BIN_EXE_ophio-sandbox-shim"))
            .create(SandboxSpec {
                offline: false,
                reach,
                harness: HarnessKind::Codex,
                cwd: self.a.clone(),
                state: self.state.clone(),
                always: vec![],
                programs: vec![],
                candidates,
                private,
                given,
                relay: None,
                on_blocked: None,
            })
            .unwrap()
    }
    async fn launch(&self, sandbox: &Arc<dyn Sandbox>, script: &str) -> tokio::process::Child {
        let env = ports::RunEnv(vec![
            ("HOME".into(), self.home.to_str().unwrap().into()),
            ("CODEX_HOME".into(), self.config.to_str().unwrap().into()),
        ]);
        let wrapped = sandbox
            .wrap(WrapRequest {
                program: Path::new("/bin/sh"),
                visible: &[],
                env: &env,
                resume: None,
            })
            .unwrap();
        let mut command = tokio::process::Command::new(wrapped.program);
        if wrapped.clear_env {
            command.env_clear();
        }
        command
            .args(wrapped.args)
            .args(["/bin/sh", "-c", script])
            .envs(env.0)
            .envs(wrapped.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        sandbox.prepare(command.as_std_mut()).unwrap();
        let child = command.spawn().unwrap();
        sandbox.started().await.unwrap();
        child
    }
}
#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn open_writer_requires_restart_and_read_revocation_never_claims_applied() {
    let f = Fixture::new();
    let mut reach = f.reach();
    reach.write.push(f.b.clone());
    let sandbox = f.sandbox(reach.clone(), vec![f.b.clone()]);
    let mut child = f
        .launch(
            &sandbox,
            &format!("exec 3>'{}/held'; echo ready; sleep 20", f.b.display()),
        )
        .await;
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .await
        .unwrap();
    assert_eq!(line, "ready\n");
    reach.write.clear();
    assert!(matches!(
        sandbox.update(reach).await,
        Applied::NeedsRestart(_)
    ));
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let f = Fixture::new();
    let reach = f.reach();
    let sandbox = f.sandbox(reach.clone(), vec![f.b.clone()]);
    let mut child = f.launch(&sandbox, "echo ready; sleep 20").await;
    let mut reach = reach;
    reach.read.retain(|p| p != &f.b);
    assert!(matches!(
        sandbox.update(reach).await,
        Applied::NeedsRestart(_)
    ));
    child.kill().await.unwrap();
    child.wait().await.unwrap();
}
#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn home_and_nested_projects_preserve_private_and_other_history_masks() {
    let f = Fixture::new();
    let mut reach = f.reach();
    reach.read = vec![f.home.clone()];
    let sandbox = f.sandbox(reach, vec![f.home.clone(), f.a.clone(), f.b.clone()]);
    let child = f.launch(&sandbox, &format!(
        "test -d '{}' && test ! -e '{}/other-history' && test ! -e '{}/.config/ophio/secret' && test ! -e '{}/.claude/other-history' && test \"$(cat '{}/.claude.json')\" = '{{}}'",
        f.b.display(), f.config.display(), f.home.display(), f.home.display(), f.home.display()
    )).await;
    assert!(child.wait_with_output().await.unwrap().status.success());
}
#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn an_accounts_sign_in_folder_shows_through_private_data_read_only() {
    let f = Fixture::new();
    let data = f.home.join(".local/share/ophio-data");
    let github = data.join("accounts/config/github");
    std::fs::create_dir_all(&github).unwrap();
    std::fs::write(github.join("hosts.yml"), "signed-in").unwrap();
    std::fs::write(data.join("secret"), "private").unwrap();
    let sandbox = f.sandbox_given(
        f.reach(),
        vec![],
        vec![data.clone()],
        vec![github.clone(), data.join("accounts/gone")],
    );
    let child = f
        .launch(
            &sandbox,
            &format!(
                "test \"$(cat '{g}/hosts.yml')\" = signed-in && test ! -e '{d}/secret' && ! (echo x > '{g}/hosts.yml') 2>/dev/null",
                g = github.display(),
                d = data.display()
            ),
        )
        .await;
    assert!(child.wait_with_output().await.unwrap().status.success());
}
#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn executable_cannot_uncover_hidden_project_from_a_nested_user_namespace() {
    let f = Fixture::new();
    std::fs::write(f.b.join("hidden"), "hidden").unwrap();
    let mut reach = f.reach();
    reach.read.retain(|p| p != &f.b);
    let sandbox = f.sandbox(reach, vec![f.b.clone()]);
    let child = f
        .launch(
            &sandbox,
            &format!(
                "unshare -Urnm sh -c 'umount {}; cat {}/hidden'; test ! -e {}/hidden",
                f.b.display(),
                f.b.display(),
                f.b.display()
            ),
        )
        .await;
    assert!(child.wait_with_output().await.unwrap().status.success());
}
#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn the_computer_reaches_a_server_on_the_sandboxs_own_loopback() {
    use std::io::{Read, Write};
    let f = Fixture::new();
    let sandbox = f.sandbox(f.reach(), vec![]);
    let port = 47391;
    let address: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let script = format!(
        "exec python3 -c 'import socket\n\
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n\
s.bind((\"127.0.0.1\", {port})); s.listen(); print(\"ready\", flush=True)\n\
c,_=s.accept(); c.sendall(b\"from inside\"); c.close()'"
    );
    let mut child = f.launch(&sandbox, &script).await;
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .await
        .unwrap();
    assert_eq!(line, "ready\n");
    // The sandbox has its own network: the server isn't on the computer's.
    assert!(std::net::TcpStream::connect(address).is_err());
    let mut stream = sandbox.connect(&[address]).await.unwrap();
    let mut said = String::new();
    stream.read_to_string(&mut said).unwrap();
    assert_eq!(said, "from inside");
    let _ = stream.write_all(b"done");
    child.wait().await.unwrap();
    let unanswered: std::net::SocketAddr = ([127, 0, 0, 1], port + 1).into();
    assert!(sandbox.connect(&[unanswered]).await.is_err());
    let elsewhere: std::net::SocketAddr = ([192, 0, 2, 1], 80).into();
    assert!(sandbox.connect(&[elsewhere]).await.is_err());
}

#[tokio::test]
#[ignore = "requires real user namespaces"]
async fn relay_is_the_only_network_route_and_credentials_do_not_exist() {
    let f = Fixture::new();
    let key = f._root.path().join("relay-key");
    let log = f._root.path().join("relay-audit.jsonl");
    let account = f._root.path().join("account");
    std::fs::create_dir(&account).unwrap();
    std::fs::write(account.join("auth.json"), "fixture-account-key").unwrap();
    std::fs::write(&key, "fixture-provider-key").unwrap();
    let mut reach = f.reach();
    reach.internet = true;
    reach.local_network = true;
    let sandbox = sandbox::Bubblewrap::new(env!("CARGO_BIN_EXE_ophio-sandbox-shim"))
        .create(SandboxSpec {
            offline: false,
            reach,
            harness: HarnessKind::Codex,
            cwd: f.a.clone(),
            state: f.state.clone(),
            always: vec![],
            programs: vec![],
            candidates: vec![],
            private: vec![],
            given: vec![account.clone()],
            on_blocked: None,
            relay: Some(RelaySpec {
                provider: RelayProvider::OpenAi,
                key_file: key.clone(),
                log: log.clone(),
                budget: RelayBudget {
                    max_requests: 0,
                    max_input_tokens: 100,
                    max_output_tokens: 100,
                    deadline: None,
                },
                policy: RelayPolicy::default(),
                on_signal: None,
                inspect: None,
                test_upstream: None,
            }),
        })
        .unwrap();
    let host = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = host.local_addr().unwrap().port();
    let script = format!(
        r#"
set -eu
test ! -e '{key}'
test ! -e '{log}'
test ! -e '{account}/auth.json'
test ! -e '{config}/auth.json'
test ! -e "$CODEX_HOME/auth.json"
test "$(curl --noproxy '*' -s -o /dev/null -w '%{{http_code}}' http://127.0.0.1:18080/forbidden)" = 403
test "$(curl --noproxy '*' -s -o /dev/null -w '%{{http_code}}' -H 'Content-Type: application/json' -d '{{"model":"fixture"}}' http://127.0.0.1:18080/v1/responses)" = 429
for provider in api.openai.com api.anthropic.com; do
    if curl --noproxy '*' --resolve "$provider:443:192.0.2.1" -s --connect-timeout 1 --max-time 2 "https://$provider/"; then exit 91; fi
done
if curl --noproxy '*' -s --connect-timeout 1 --max-time 2 https://1.1.1.1/; then exit 92; fi
if curl --noproxy '*' -s --connect-timeout 1 --max-time 2 http://127.0.0.1:{port}/; then exit 93; fi
if curl --proxy http://127.0.0.1:18080 --noproxy '' -s --connect-timeout 1 --max-time 2 https://api.openai.com/; then exit 94; fi
echo relay-isolated
"#,
        key = key.display(),
        log = log.display(),
        account = account.display(),
        config = f.config.display()
    );
    let mut child = f.launch(&sandbox, &script).await;
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .await
        .unwrap();
    assert!(child.wait().await.unwrap().success());
    assert_eq!(line, "relay-isolated\n");
    assert_eq!(sandbox.relay_totals().unwrap().requests, 0);
}
