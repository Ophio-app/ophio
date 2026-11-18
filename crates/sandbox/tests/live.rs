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
        self.launch_seeing(sandbox, &[], script).await
    }
    /// Launched with `visible` shown read-only, as a harness shows a run's
    /// own files.
    async fn launch_seeing(
        &self,
        sandbox: &Arc<dyn Sandbox>,
        visible: &[PathBuf],
        script: &str,
    ) -> tokio::process::Child {
        let env = ports::RunEnv(vec![
            ("HOME".into(), self.home.to_str().unwrap().into()),
            ("CODEX_HOME".into(), self.config.to_str().unwrap().into()),
        ]);
        let wrapped = sandbox
            .wrap(WrapRequest {
                disabled: &ports::Disabled::default(),
                program: Path::new("/bin/sh"),
                visible,
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
async fn a_limited_chat_reads_its_own_attached_files_and_cannot_change_them() {
    let f = Fixture::new();
    let data = f.home.join(".local/share/ophio-data");
    let own = data.join("attached/own-conversation");
    let other = data.join("attached/other-conversation");
    for dir in [&own, &other] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(own.join("report.pdf"), "attached").unwrap();
    std::fs::write(other.join("theirs.txt"), "another conversation").unwrap();
    std::fs::write(data.join("secret"), "private").unwrap();
    let sandbox = f.sandbox_given(f.reach(), vec![], vec![data.clone()], vec![]);
    let child = f
        .launch_seeing(
            &sandbox,
            std::slice::from_ref(&own),
            &format!(
                // `set -e` ignores a negated command, so each must hold.
                "test \"$(cat '{o}/report.pdf')\" = attached \
                 && test ! -e '{x}/theirs.txt' \
                 && test ! -e '{d}/secret' \
                 && ! (echo x > '{o}/report.pdf') 2>/dev/null \
                 && ! chmod u+w '{o}/report.pdf' 2>/dev/null \
                 && ! touch '{o}/new.txt' 2>/dev/null \
                 && ! rm -f '{o}/report.pdf' 2>/dev/null \
                 && test \"$(cat '{o}/report.pdf')\" = attached \
                 && echo seen",
                o = own.display(),
                x = other.display(),
                d = data.display()
            ),
        )
        .await;
    let output = child.wait_with_output().await.unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "seen");
    assert!(own.join("report.pdf").exists());
    assert!(!own.join("new.txt").exists());
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

/// Each way of reaching the agent program through a link, run for real: the
/// program must start, run as its real file, and find the helper beside it.
#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn an_agent_reached_through_a_link_runs_as_its_real_file_beside_its_helper() {
    use std::os::unix::fs::symlink;
    #[derive(Clone, Copy, Debug)]
    enum Sees {
        Project,
        Home,
        Computer,
    }
    #[derive(Clone, Copy, Debug)]
    enum Way {
        Absolute,
        Relative,
        FolderLink,
        ThroughHiddenLink,
        BehindPrivateData,
    }
    let cases = [
        (Sees::Project, Way::Absolute),
        (Sees::Project, Way::Relative),
        (Sees::Project, Way::FolderLink),
        (Sees::Home, Way::Absolute),
        (Sees::Home, Way::Relative),
        (Sees::Home, Way::FolderLink),
        (Sees::Home, Way::ThroughHiddenLink),
        (Sees::Home, Way::BehindPrivateData),
        (Sees::Computer, Way::Absolute),
        (Sees::Computer, Way::Relative),
        (Sees::Computer, Way::FolderLink),
    ];
    let mut failures = vec![];
    for (sees, way) in cases {
        // Not under /tmp: the sandbox gives /tmp its own empty folder, which
        // would hide the fixture even from whole-computer read.
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("home");
        let project = home.join("project");
        let install = root.join("opt/1.0");
        for path in [&project, &home.join("bin"), &install.join("bin")] {
            std::fs::create_dir_all(path).unwrap();
        }
        let shell = Path::new("/bin/sh").canonicalize().unwrap();
        let codex = install.join("bin/codex");
        std::fs::copy(&shell, &codex).unwrap();
        std::fs::copy("/usr/bin/true", install.join("bin/codex-code-mode-host")).unwrap();
        let program = match way {
            Way::Absolute => {
                symlink(&codex, home.join("bin/codex")).unwrap();
                home.join("bin/codex")
            }
            Way::Relative => {
                symlink("../../opt/1.0/bin/codex", home.join("bin/codex")).unwrap();
                home.join("bin/codex")
            }
            Way::FolderLink => {
                symlink(&install, home.join("tools")).unwrap();
                home.join("tools/bin/codex")
            }
            Way::ThroughHiddenLink => {
                symlink("1.0", root.join("opt/current")).unwrap();
                symlink(root.join("opt/current/bin/codex"), home.join("bin/codex")).unwrap();
                home.join("bin/codex")
            }
            Way::BehindPrivateData => {
                let agents = home.join(".local/share/ophio/agents");
                std::fs::create_dir_all(&agents).unwrap();
                symlink(&codex, agents.join("codex")).unwrap();
                agents.join("codex")
            }
        };
        let reach = Reach {
            read: match sees {
                Sees::Home => vec![home.clone()],
                _ => vec![],
            },
            write: vec![],
            whole_read: matches!(sees, Sees::Computer),
            whole_write: false,
            internet: false,
            local_network: false,
        };
        let sandbox = sandbox::Bubblewrap::new(env!("CARGO_BIN_EXE_ophio-sandbox-shim"))
            .create(SandboxSpec {
                offline: false,
                reach,
                harness: HarnessKind::Codex,
                cwd: project.clone(),
                state: root.join("state"),
                always: vec![],
                programs: vec![],
                candidates: vec![],
                private: vec![],
                given: vec![],
                relay: None,
                on_blocked: None,
            })
            .unwrap();
        let env = ports::RunEnv(vec![
            ("HOME".into(), home.to_str().unwrap().into()),
            (
                "CODEX_HOME".into(),
                home.join(".codex").to_str().unwrap().into(),
            ),
        ]);
        let wrapped = match sandbox.wrap(WrapRequest {
            disabled: &ports::Disabled::default(),
            program: &program,
            visible: &[],
            env: &env,
            resume: None,
        }) {
            Ok(wrapped) => wrapped,
            Err(e) => {
                failures.push(format!("{sees:?} {way:?}: not wrapped: {e:?}"));
                continue;
            }
        };
        let mut command = tokio::process::Command::new(wrapped.program);
        if wrapped.clear_env {
            command.env_clear();
        }
        command
            .args(wrapped.args)
            .arg(&program)
            .args([
                "-c",
                r#"exe=$(readlink /proc/$$/exe); echo "$exe"; test -x "${exe%/*}/codex-code-mode-host" && echo helper"#,
            ])
            .envs(env.0)
            .envs(wrapped.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        sandbox.prepare(command.as_std_mut()).unwrap();
        let child = command.spawn().unwrap();
        // A sandbox that never starts reports it here.
        let started = sandbox.started().await;
        let output = child.wait_with_output().await.unwrap();
        let said = String::from_utf8_lossy(&output.stdout);
        let expected = format!("{}\nhelper\n", codex.display());
        let verdict = if started.is_ok() && output.status.success() && said == expected {
            "ok"
        } else {
            failures.push(format!(
                "{sees:?} {way:?}: {:?} {} said {said:?}, stderr {:?}",
                started.err(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
            "FAILED"
        };
        println!("{sees:?} {way:?}: {verdict}: {}", said.replace('\n', " | "));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A fake home with both agents' own instructions and skills, a skill linked
/// from elsewhere, and settings that must never be shared.
struct Setup {
    root: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    other: PathBuf,
    tools: PathBuf,
}
impl Setup {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap().join("home");
        let project = home.join("Projects/app");
        let other = home.join("Projects/other");
        let tools = home.join("Tools");
        let write = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        let skill = "---\nname: s\ndescription: d\n---\n";
        let writer = "---\nname: w\ndescription: d\nallowed-tools: Read Write\n---\n";
        write(tools.join("linked/SKILL.md"), skill);
        write(tools.join("writer/SKILL.md"), writer);
        write(project.join("README"), "project");
        write(other.join("README"), "other");
        for (config, file) in [(".claude", "CLAUDE.md"), (".codex", "AGENTS.md")] {
            let config = home.join(config);
            write(config.join(file), "mine");
            write(config.join("skills/mine/SKILL.md"), skill);
            std::os::unix::fs::symlink(tools.join("linked"), config.join("skills/linked")).unwrap();
            std::os::unix::fs::symlink(tools.join("writer"), config.join("skills/writer")).unwrap();
            write(config.join("settings.json"), "{\"hooks\":1}");
            write(config.join("config.toml"), "[mcp_servers]");
            write(config.join("plugins/p/SKILL.md"), skill);
            write(config.join("agents/a.md"), "agent");
            write(config.join("commands/c.md"), "command");
            write(config.join("history.jsonl"), "old");
        }
        write(home.join(".claude/rules/style.md"), "rule");
        write(home.join(".claude/skills/team/pdf/SKILL.md"), skill);
        write(
            home.join(".claude/skills/synced/bucket/pdf/SKILL.md"),
            skill,
        );
        write(home.join(".codex/AGENTS.override.md"), "override");
        write(home.join(".codex/rules/default.rules"), "policy");
        write(home.join(".codex/skills/.system/imagegen/SKILL.md"), skill);
        write(home.join(".claude.json"), "private metadata");
        // The project's own, which the agents find themselves.
        write(project.join("CLAUDE.md"), "project rules");
        write(project.join("AGENTS.md"), "project rules");
        write(project.join(".claude/skills/local/SKILL.md"), skill);
        write(project.join(".agents/skills/local/SKILL.md"), skill);
        write(project.join("kit/inside/SKILL.md"), skill);
        std::os::unix::fs::symlink("../../kit/inside", project.join(".claude/skills/inside"))
            .unwrap();
        std::os::unix::fs::symlink("../../kit/inside", project.join(".agents/skills/inside"))
            .unwrap();
        Self {
            root,
            home,
            project,
            other,
            tools,
        }
    }
    fn state(&self) -> PathBuf {
        self.home.join(".local/share/ophio/sandboxes/t")
    }
    fn config(&self, kind: HarnessKind) -> PathBuf {
        self.home.join(if kind == HarnessKind::Codex {
            ".codex"
        } else {
            ".claude"
        })
    }
    fn wrap(
        &self,
        kind: HarnessKind,
        reach: Reach,
        script: &str,
        disabled: &ports::Disabled,
    ) -> (tokio::process::Command, Arc<dyn Sandbox>) {
        let candidates = vec![self.home.clone(), self.project.clone(), self.other.clone()];
        let sandbox = sandbox::Bubblewrap::new(env!("CARGO_BIN_EXE_ophio-sandbox-shim"))
            .create(SandboxSpec {
                offline: false,
                reach,
                harness: kind,
                cwd: self.project.clone(),
                state: self.state(),
                always: vec![],
                programs: vec![],
                candidates,
                private: vec![self.home.join(".local/share/ophio")],
                given: vec![],
                relay: None,
                on_blocked: None,
            })
            .unwrap();
        let variable = if kind == HarnessKind::Codex {
            "CODEX_HOME"
        } else {
            "CLAUDE_CONFIG_DIR"
        };
        let env = ports::RunEnv(vec![
            ("HOME".into(), self.home.to_str().unwrap().into()),
            (variable.into(), self.config(kind).to_str().unwrap().into()),
        ]);
        let wrapped = sandbox
            .wrap(WrapRequest {
                disabled,
                program: Path::new("/bin/sh"),
                visible: &[],
                env: &env,
                resume: None,
            })
            .unwrap();
        let mut command = tokio::process::Command::new(wrapped.program);
        command
            .args(wrapped.args)
            .args(["/bin/sh", "-c", script])
            .envs(env.0)
            .envs(wrapped.env)
            .env("CONFIG", self.config(kind))
            .env("TOOLS", &self.tools)
            .env("OTHER", &self.other)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        sandbox.prepare(command.as_std_mut()).unwrap();
        (command, sandbox)
    }
    async fn run(&self, kind: HarnessKind, reach: Reach, script: &str) -> (bool, String) {
        self.run_leaving_out(kind, reach, script, &ports::Disabled::default())
            .await
    }
    async fn run_leaving_out(
        &self,
        kind: HarnessKind,
        reach: Reach,
        script: &str,
        disabled: &ports::Disabled,
    ) -> (bool, String) {
        let (mut command, sandbox) = self.wrap(kind, reach, script, disabled);
        let child = command.spawn().unwrap();
        let _ = sandbox.started().await;
        let output = child.wait_with_output().await.unwrap();
        let said = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        (output.status.success(), said)
    }
}

fn reaches(s: &Setup) -> Vec<(&'static str, Reach)> {
    let base = Reach {
        read: vec![],
        write: vec![],
        whole_read: false,
        whole_write: false,
        internet: false,
        local_network: false,
    };
    vec![
        ("project only", base.clone()),
        (
            "project writable",
            Reach {
                write: vec![s.project.clone()],
                ..base.clone()
            },
        ),
        (
            "readable home",
            Reach {
                read: vec![s.home.clone()],
                ..base.clone()
            },
        ),
        (
            "whole computer read",
            Reach {
                whole_read: true,
                ..base.clone()
            },
        ),
        (
            "whole computer write",
            Reach {
                whole_read: true,
                whole_write: true,
                ..base
            },
        ),
    ]
}

const SHARED_CHECKS: &str = r#"
set -u
fail() { echo "FAIL: $*"; exit 1; }
test "$(cat "$CONFIG/$FILE")" = mine || fail "instructions not readable"
for s in mine linked; do
    test -f "$CONFIG/skills/$s/SKILL.md" || fail "skill $s missing"
    ! touch "$CONFIG/skills/$s/new" 2>/dev/null || fail "skill $s writable"
    ! rm -f "$CONFIG/skills/$s/SKILL.md" 2>/dev/null || fail "skill $s removable"
    ! mkdir "$CONFIG/skills/$s/d" 2>/dev/null || fail "skill $s folder writable"
    ! mv "$CONFIG/skills/$s" "$CONFIG/skills/moved" 2>/dev/null || fail "skill $s movable"
done
! sh -c "echo x >> '$CONFIG/$FILE'" 2>/dev/null || fail "instructions writable"
for never in settings.json config.toml plugins agents commands; do
    test ! -e "$CONFIG/$never" || fail "$never shared"
done
test "$(cat "$CONFIG/history.jsonl" 2>/dev/null)" != old || fail "history shared"
test ! -e "$CONFIG/skills/linked/../../Tools" 2>/dev/null || true
test ! -e "$HOME/.local/share/ophio/sandboxes/t/shared-setup" || fail "record visible"
test -f "$PWD/.claude/skills/local/SKILL.md" || fail "project claude skill"
test -f "$PWD/.claude/skills/inside/SKILL.md" || fail "project linked claude skill"
test -f "$PWD/.agents/skills/local/SKILL.md" || fail "project agents skill"
test -f "$PWD/.agents/skills/inside/SKILL.md" || fail "project linked agents skill"
test -f "$PWD/CLAUDE.md" -a -f "$PWD/AGENTS.md" || fail "project instructions"
test -z "${OPHIO_SANDBOX_SHARED:-}" || fail "check list leaked to the agent"
if [ "$KIND" = claude ]; then
    test -f "$CONFIG/rules/style.md" || fail "rules missing"
    ! sh -c "echo x >> '$CONFIG/rules/style.md'" 2>/dev/null || fail "rules writable"
    ! rm -f "$CONFIG/rules/style.md" 2>/dev/null || fail "rules removable"
    test -f "$CONFIG/skills/team/pdf/SKILL.md" || fail "folder of skills missing"
    ! touch "$CONFIG/skills/team/new" 2>/dev/null || fail "folder of skills writable"
    test ! -e "$CONFIG/skills/synced/bucket/pdf/SKILL.md" || fail "synced shared"
    mkdir -p "$CONFIG/skills/synced/bucket" || fail "synced not the agent's own"
else
    test "$(cat "$CONFIG/AGENTS.override.md")" = override || fail "override missing"
    test ! -e "$CONFIG/rules" || fail "codex rules shared"
    mkdir -p "$CONFIG/skills/.system/fresh" && touch "$CONFIG/skills/.system/fresh/SKILL.md" || fail ".system not writable"
fi
# The agent can make skills of its own beside the shared ones.
mkdir -p "$CONFIG/skills/own" && touch "$CONFIG/skills/own/SKILL.md" || fail "own skill"
if [ -e "$CONFIG/skills/writer" ]; then echo "writer shown"; else echo "writer hidden"; fi
if [ -e "$TOOLS/linked" ]; then echo "source visible"; else echo "source hidden"; fi
echo shared-ok
"#;

#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn own_instructions_and_skills_are_shared_read_only_whatever_the_grant() {
    for kind in [HarnessKind::ClaudeCode, HarnessKind::Codex] {
        let s = Setup::new();
        let (name, file) = if kind == HarnessKind::Codex {
            ("codex", "AGENTS.md")
        } else {
            ("claude", "CLAUDE.md")
        };
        let before = listing(&s.config(kind));
        for (grant, reach) in reaches(&s) {
            let writes = reach.writes();
            let wide = reach.whole_read || !reach.read.is_empty();
            let script = format!("KIND={name} FILE={file}\n{SHARED_CHECKS}");
            let (ok, said) = s.run(kind, reach, &script).await;
            eprintln!("{name} / {grant}:\n{said}");
            assert!(ok && said.contains("shared-ok"), "{name} / {grant}: {said}");
            assert!(
                said.contains(if writes {
                    "writer shown"
                } else {
                    "writer hidden"
                }),
                "{name} / {grant}: {said}"
            );
            assert!(
                said.contains(if wide {
                    "source visible"
                } else {
                    "source hidden"
                }),
                "{name} / {grant}: {said}"
            );
        }
        // Nothing was made or changed in the person's own folder.
        assert_eq!(listing(&s.config(kind)), before, "{name}");
        assert_eq!(
            std::fs::read_to_string(s.tools.join("linked/SKILL.md")).unwrap(),
            "---\nname: s\ndescription: d\n---\n"
        );
        drop(s.root);
    }
}

#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn what_is_disabled_in_ophio_is_not_shared_with_a_limited_chat() {
    let s = Setup::new();
    let reach = reaches(&s).remove(0).1;
    for kind in [HarnessKind::ClaudeCode, HarnessKind::Codex] {
        let config = s.config(kind);
        let file = if kind == HarnessKind::Codex {
            "AGENTS.md"
        } else {
            "CLAUDE.md"
        };
        // As Øphio gives them: a linked skill by both its paths.
        let disabled = ports::Disabled {
            files: vec![config.join(file)],
            skills: [
                config.join("skills/linked/SKILL.md"),
                s.tools.join("linked/SKILL.md"),
            ]
            .into_iter()
            .map(|path| ports::DisabledEntry {
                path,
                name: "linked".into(),
                harness: Some(kind),
            })
            .collect(),
            ..Default::default()
        };
        let script = format!(
            "test ! -e \"$CONFIG/{file}\" && test ! -e \"$CONFIG/skills/linked\" && test -f \"$CONFIG/skills/mine/SKILL.md\""
        );
        let (ok, said) = s
            .run_leaving_out(kind, reach.clone(), &script, &disabled)
            .await;
        assert!(ok, "{kind:?}: {said}");
    }
}

#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn a_link_the_agent_leaves_for_its_skills_folder_never_reaches_the_persons() {
    let s = Setup::new();
    let kind = HarnessKind::ClaudeCode;
    let config = s.config(kind);
    let reach = reaches(&s).remove(0).1;
    // Inside, the settings folder's path is the chat's own copy; outside, a
    // link to that path leads to the person's real one.
    let (ok, said) = s
        .run(
            kind,
            reach.clone(),
            "mv \"$CONFIG/skills\" \"$CONFIG/skills-mine\" && ln -s \"$CONFIG/skills\" \"$CONFIG/skills\"",
        )
        .await;
    assert!(ok, "{said}");
    let planted = s.state().join("native/skills");
    assert!(std::fs::symlink_metadata(&planted).unwrap().is_symlink());
    let before = listing(&config);
    // The linked skill stops being shared before the next turn.
    let disabled = ports::Disabled {
        skills: vec![ports::DisabledEntry {
            path: s.tools.join("linked/SKILL.md"),
            name: "linked".into(),
            harness: None,
        }],
        ..Default::default()
    };
    let (ok, said) = s
        .run_leaving_out(
            kind,
            reach,
            "test ! -e \"$CONFIG/skills/linked\" && test -f \"$CONFIG/skills/mine/SKILL.md\"",
            &disabled,
        )
        .await;
    assert!(ok, "{said}");
    assert_eq!(listing(&config), before);
    assert!(
        std::fs::symlink_metadata(config.join("skills/linked"))
            .unwrap()
            .is_symlink()
    );
    assert!(std::fs::symlink_metadata(&planted).unwrap().is_dir());
}

#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn a_later_turn_sees_skills_added_and_removed_since() {
    let s = Setup::new();
    let kind = HarnessKind::ClaudeCode;
    let config = s.config(kind);
    let reach = reaches(&s).remove(0).1;
    let (ok, said) = s
        .run(
            kind,
            reach.clone(),
            "test -f \"$CONFIG/skills/linked/SKILL.md\"",
        )
        .await;
    assert!(ok, "{said}");
    std::fs::remove_file(config.join("skills/linked")).unwrap();
    std::fs::remove_file(config.join("CLAUDE.md")).unwrap();
    std::fs::create_dir_all(config.join("skills/fresh")).unwrap();
    std::fs::write(config.join("skills/fresh/SKILL.md"), "---\n---\n").unwrap();
    let (ok, said) = s
        .run(
            kind,
            reach,
            "test ! -e \"$CONFIG/skills/linked\" && test ! -e \"$CONFIG/CLAUDE.md\" && test -f \"$CONFIG/skills/fresh/SKILL.md\" && test -f \"$CONFIG/skills/mine/SKILL.md\"",
        )
        .await;
    assert!(ok, "{said}");
}

#[tokio::test]
#[ignore = "requires real user namespaces; run under systemd-run --user"]
async fn a_skill_swapped_after_it_was_checked_stops_the_run_before_the_agent() {
    let s = Setup::new();
    let kind = HarnessKind::Codex;
    let reach = reaches(&s).remove(0).1;
    let (mut command, sandbox) = s.wrap(
        kind,
        reach,
        "echo agent-started",
        &ports::Disabled::default(),
    );
    // Between the check and the mount, the linked skill becomes another folder.
    let secret = s.home.join("Secret");
    std::fs::create_dir_all(&secret).unwrap();
    std::fs::write(secret.join("SKILL.md"), "secret").unwrap();
    std::fs::rename(s.tools.join("linked"), s.home.join("old")).unwrap();
    std::fs::rename(&secret, s.tools.join("linked")).unwrap();
    let child = command.spawn().unwrap();
    let _ = sandbox.started().await;
    let output = child.wait_with_output().await.unwrap();
    let said = String::from_utf8_lossy(&output.stdout);
    eprintln!("swapped: {said}{}", String::from_utf8_lossy(&output.stderr));
    assert!(!output.status.success());
    assert!(!said.contains("agent-started"), "{said}");
}

fn listing(root: &Path) -> Vec<(PathBuf, u64)> {
    fn walk(path: &Path, out: &mut Vec<(PathBuf, u64)>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let metadata = std::fs::symlink_metadata(entry.path()).unwrap();
            use std::os::unix::fs::MetadataExt;
            out.push((entry.path(), metadata.mtime() as u64 ^ metadata.len()));
            if metadata.is_dir() {
                walk(&entry.path(), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

/// The real agents, in a home the caller makes: `OPHIO_LIVE_HOME` holds
/// `.claude/.credentials.json` and `.codex/auth.json` mounted over from the
/// person's own (never copied), and `OPHIO_LIVE_CLAUDE` and `OPHIO_LIVE_CODEX`
/// name the programs. Everything else here is made by this test. Each agent is
/// asked once, read-only and then able to change the project, which skills and
/// instructions it has. Ignored: it uses the person's accounts.
#[tokio::test]
#[ignore = "uses the person's accounts; see the comment"]
async fn real_agents_see_shared_and_project_skills() {
    let Ok(home) = std::env::var("OPHIO_LIVE_HOME") else {
        eprintln!("OPHIO_LIVE_HOME is not set; skipped");
        return;
    };
    let home = PathBuf::from(home).canonicalize().unwrap();
    let project = home.join("Projects/app");
    let write = |path: PathBuf, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let skill = |name: &str, extra: &str| {
        format!(
            "---\nname: {name}\ndescription: Probe skill {name}. Never use it.\n{extra}---\nDo nothing.\n"
        )
    };
    let link = |target: &str, at: PathBuf| {
        let _ = std::fs::remove_file(&at);
        std::os::unix::fs::symlink(target, at).unwrap();
    };
    for (config, file, skills) in [
        (".claude", "CLAUDE.md", ".claude/skills"),
        (".codex", "AGENTS.md", ".agents/skills"),
    ] {
        let config = home.join(config);
        write(config.join(file), "Global canary: LCS-GLOBAL-CANARY.\n");
        write(
            config.join("skills/lcs-global-plain/SKILL.md"),
            &skill("lcs-global-plain", ""),
        );
        write(
            config.join("skills/lcs-global-tools/SKILL.md"),
            &skill("lcs-global-tools", "allowed-tools: Read Write\n"),
        );
        write(
            config.join("skills/lcs-global-marked/SKILL.md"),
            &skill("lcs-global-marked", "metadata:\n  ophio-access: write\n"),
        );
        write(
            home.join("Tools/lcs-global-linked/SKILL.md"),
            &skill("lcs-global-linked", ""),
        );
        link(
            home.join("Tools/lcs-global-linked").to_str().unwrap(),
            config.join("skills/lcs-global-linked"),
        );
        let skills = project.join(skills);
        write(
            skills.join("lcs-project-plain/SKILL.md"),
            &skill("lcs-project-plain", ""),
        );
        write(
            skills.join("lcs-project-tools/SKILL.md"),
            &skill("lcs-project-tools", "allowed-tools: Write\n"),
        );
        write(
            project.join("kit/lcs-project-linked/SKILL.md"),
            &skill("lcs-project-linked", ""),
        );
        link(
            "../../kit/lcs-project-linked",
            skills.join("lcs-project-linked"),
        );
    }
    write(
        home.join(".claude/rules/lcs.md"),
        "Global rule canary: LCS-GLOBAL-RULE.\n",
    );
    // Where Claude Code keeps skills it syncs from the person's account.
    write(
        home.join(".claude/skills/synced/lcs-bucket/lcs-synced/SKILL.md"),
        &skill("lcs-synced", ""),
    );
    write(
        project.join("CLAUDE.md"),
        "Project canary: LCS-PROJECT-CANARY.\n",
    );
    write(
        project.join("AGENTS.md"),
        "Project canary: LCS-PROJECT-CANARY.\n",
    );
    write(
        project.join(".claude/rules/lcs.md"),
        "Project rule canary: LCS-PROJECT-RULE.\n",
    );
    let prompt = "Do not use any tools. Reply in plain text with two parts. First, every skill \
        available to you, one per line, as `SKILL: <name> | <scope or path your context gives, \
        or unknown>`. Second, every line of your instructions that contains `LCS-`, one per line, \
        prefixed `CANARY: `.";
    let mut failures = vec![];
    for (n, (kind, variable, program, args)) in [
        (
            HarnessKind::ClaudeCode,
            "CLAUDE_CONFIG_DIR",
            std::env::var("OPHIO_LIVE_CLAUDE").unwrap(),
            vec![
                "-p",
                prompt,
                "--model",
                "haiku",
                "--output-format",
                "stream-json",
                "--verbose",
                "--max-turns",
                "2",
            ],
        ),
        (
            HarnessKind::Codex,
            "CODEX_HOME",
            std::env::var("OPHIO_LIVE_CODEX").unwrap(),
            vec![
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--dangerously-bypass-approvals-and-sandbox",
                "-m",
                "gpt-6-luna",
                "-c",
                "model_reasoning_effort=\"low\"",
                prompt,
            ],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let config = home.join(if kind == HarnessKind::Codex {
            ".codex"
        } else {
            ".claude"
        });
        for writes in [false, true] {
            let reach = Reach {
                read: if writes {
                    vec![]
                } else {
                    vec![project.clone()]
                },
                write: if writes {
                    vec![project.clone()]
                } else {
                    vec![]
                },
                whole_read: false,
                whole_write: false,
                internet: true,
                local_network: false,
            };
            let sandbox = sandbox::Bubblewrap::new(env!("CARGO_BIN_EXE_ophio-sandbox-shim"))
                .create(SandboxSpec {
                    offline: false,
                    reach,
                    harness: kind,
                    cwd: project.clone(),
                    state: home.join(format!(".local/share/ophio/sandboxes/{n}-{writes}")),
                    always: vec![],
                    programs: vec![],
                    candidates: vec![home.clone(), project.clone()],
                    private: vec![home.join(".local/share/ophio")],
                    given: vec![],
                    relay: None,
                    on_blocked: None,
                })
                .unwrap();
            let env = ports::RunEnv(vec![
                ("HOME".into(), home.to_str().unwrap().into()),
                (variable.into(), config.to_str().unwrap().into()),
            ]);
            let wrapped = sandbox
                .wrap(WrapRequest {
                    disabled: &ports::Disabled::default(),
                    program: Path::new(&program),
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
                .arg(&program)
                .args(&args)
                .envs(env.0)
                .envs(wrapped.env)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            sandbox.prepare(command.as_std_mut()).unwrap();
            let child = command.spawn().unwrap();
            let _ = sandbox.started().await;
            let output = tokio::time::timeout(
                std::time::Duration::from_secs(240),
                child.wait_with_output(),
            )
            .await
            .expect("the agent answered in time")
            .unwrap();
            let said = String::from_utf8_lossy(&output.stdout).into_owned();
            let label = format!("{kind:?} writes={writes}");
            // Claude Code's opening event lists its skills by name.
            let listed: Vec<String> = said
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|v| v["type"] == "system" && v["subtype"] == "init")
                .flat_map(|v| {
                    v["skills"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                })
                .collect();
            // Only how many others there are: the rest come with the account.
            let others = listed.iter().filter(|s| !s.starts_with("lcs-")).count();
            let listed: Vec<String> = listed
                .into_iter()
                .filter(|s| s.starts_with("lcs-"))
                .collect();
            let answer: String = said
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter_map(|v| v["result"].as_str().map(str::to_owned))
                .next()
                .unwrap_or_else(|| said.clone());
            let lines: Vec<&str> = answer
                .lines()
                .filter(|l| l.contains("lcs-") || l.contains("LCS-"))
                .collect();
            eprintln!(
                "== {label}: exit {}\nlisted at start: {listed:?} and {others} others\n{}\nstderr tail: {}",
                output.status,
                lines.join("\n"),
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" / ")
            );
            let seen = |name: &str| listed.iter().any(|s| s == name) || answer.contains(name);
            for name in [
                "lcs-global-plain",
                "lcs-global-linked",
                "lcs-project-plain",
                "lcs-project-linked",
                "lcs-project-tools",
            ] {
                if !seen(name) {
                    failures.push(format!("{label}: {name} missing"));
                }
            }
            for name in ["lcs-global-tools", "lcs-global-marked"] {
                if seen(name) != writes {
                    failures.push(format!("{label}: {name} shown={}", seen(name)));
                }
            }
            for canary in ["LCS-GLOBAL-CANARY", "LCS-PROJECT-CANARY"] {
                if !answer.contains(canary) {
                    failures.push(format!("{label}: {canary} missing"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
