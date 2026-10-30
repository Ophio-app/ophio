// SPDX-License-Identifier: Apache-2.0
//! Disable in Ophio (D-071) against the installed Codex, through Ophio's own
//! adapter: what it launches is what a task launches. Ignored: it uses the
//! person's account. The caller makes the throwaway files: a git repository
//! at `OPHIO_LIVE_PROJECT` with `.agents/skills/ophio-probe-zebra` and an
//! `AGENTS.md` canary, and a user skill at `OPHIO_LIVE_USER_SKILL`
//! (`ophio-probe-heron`); points `OPHIO_LIVE_BINARY` at a wrapper that
//! records the command and its output in `OPHIO_LIVE_EVIDENCE`; and removes
//! them all afterwards.
use harness_codex::CodexAdapter;
use ports::{Disabled, DisabledEntry, HarnessAdapter, HarnessEvent, PromptInput, RunEnv, RunSpec};
use protocol::{
    capability::HarnessKind,
    model::{LaunchPolicy, TaskMode},
};
use std::{path::PathBuf, time::Duration};

fn var(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set")))
}

fn spec(project: PathBuf, disabled: Disabled, text: &str) -> RunSpec {
    RunSpec {
        run_id: protocol::RunId::new(),
        task_id: protocol::TaskId::new(),
        mode: TaskMode::Work,
        cwd: project,
        prompt: PromptInput {
            text: text.into(),
            images: vec![],
        },
        model: Some(std::env::var("OPHIO_LIVE_MODEL").unwrap_or_else(|_| "gpt-5.6-luna".into())),
        effort: Some("low".into()),
        launch_policy: Some(LaunchPolicy::BypassPermissions),
        resume_session: None,
        resume_from_mode: None,
        mission: None,
        branch: None,
        start: None,
        bridge: None,
        project_settings: false,
        env: RunEnv::default(),
        github: None,
        scratch: None,
        questions: true,
        sandbox: None,
        web: false,
        disabled,
    }
}

async fn run(spec: RunSpec, label: &str) -> String {
    let adapter = CodexAdapter::with_binary(var("OPHIO_LIVE_BINARY"));
    let mut started = adapter.start_run(spec).await.expect("Codex started");
    let mut out = String::new();
    let all = async {
        while let Some(event) = started.events.recv().await {
            match event {
                HarnessEvent::AgentMessage { text, .. } => {
                    out.push_str(&format!("[message] {text}\n"))
                }
                HarnessEvent::Activity {
                    tool,
                    summary,
                    status,
                    detail,
                    ..
                } => out.push_str(&format!(
                    "[activity] {tool} {status:?}: {summary}{}\n",
                    detail.map(|d| format!(" — {d}")).unwrap_or_default()
                )),
                HarnessEvent::SessionStarted { native_session, .. } => {
                    out.push_str(&format!("[session] {native_session}\n"))
                }
                HarnessEvent::TurnCompleted { .. } => {
                    out.push_str("[turn completed]\n");
                    started.control.close_admission();
                }
                HarnessEvent::Exited { exit, .. } => {
                    out.push_str(&format!("[exited] {exit:?}\n"));
                    break;
                }
                HarnessEvent::Warning { text } => out.push_str(&format!("[warning] {text}\n")),
                HarnessEvent::ApprovalRequested(_) => out.push_str("[approval requested]\n"),
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(420), all)
        .await
        .expect("the run ended in time");
    std::fs::write(
        var("OPHIO_LIVE_EVIDENCE").join(format!("codex-{label}.events.txt")),
        &out,
    )
    .unwrap();
    out
}

const ASK: &str = "Answer from what you were given, without running any commands or reading any files.\n\
1. Write SKILLS: then the exact names of every skill listed in your instructions, comma separated.\n\
2. Write CANARY: then the project canary word from the AGENTS.md instructions you were given, or NONE.\n\
3. If you have a skill named ophio-probe-zebra, say what its instructions tell you to reply; otherwise say you have no such skill. Do the same for ophio-probe-heron.";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed Codex with the person's account"]
async fn a_real_codex_run_is_kept_from_what_is_disabled() {
    let project = var("OPHIO_LIVE_PROJECT");
    let entry = |path: PathBuf, name: &str| DisabledEntry {
        path,
        name: name.into(),
        harness: Some(HarnessKind::Codex),
    };
    let disabled = Disabled {
        skills: vec![
            entry(
                project.join(".agents/skills/ophio-probe-zebra/SKILL.md"),
                "ophio-probe-zebra",
            ),
            entry(var("OPHIO_LIVE_USER_SKILL"), "ophio-probe-heron"),
        ],
        agents: vec![],
        files: vec![project.join("AGENTS.md")],
        memory: false,
        output_style: false,
    };
    let open = run(spec(project.clone(), Disabled::default(), ASK), "open").await;
    let blocked = run(spec(project, disabled, ASK), "blocked").await;
    // The question names both skills, so the reply may too: what counts is
    // the list the model gives, the canary, and that it has no such skill.
    let skills = |text: &str| {
        text.lines()
            .find_map(|line| line.split_once("SKILLS:").map(|(_, rest)| rest.to_owned()))
            .unwrap_or_default()
    };
    for name in ["ophio-probe-zebra", "ophio-probe-heron"] {
        assert!(
            skills(&open).contains(name),
            "open run omitted {name}: {open}"
        );
        assert!(
            !skills(&blocked).contains(name),
            "blocked run listed {name}: {blocked}"
        );
    }
    assert!(
        open.contains("OSPREY-42"),
        "open run omitted the canary: {open}"
    );
    assert!(
        !blocked.contains("OSPREY-42"),
        "blocked run had the canary: {blocked}"
    );
    assert!(
        blocked.to_lowercase().contains("no skill named")
            || blocked.to_lowercase().contains("don't have")
            || blocked.to_lowercase().contains("do not have"),
        "blocked run claimed a disabled skill: {blocked}"
    );
}

/// Uses only an isolated CODEX_HOME, never the person's credentials.
#[test]
#[ignore = "runs installed Codex app-server without an account"]
fn codex_merges_command_line_skill_config_with_personal_config() {
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
    };
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    for name in ["A", "B"] {
        let folder = home.join("skills").join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Probe {name}\n---\n"),
        )
        .unwrap();
    }
    let a = home.join("skills/A/SKILL.md");
    let b = home.join("skills/B/SKILL.md");
    std::fs::write(
        home.join("config.toml"),
        format!(
            "[[skills.config]]\npath = {}\nenabled = false\n",
            serde_json::to_string(&a.to_string_lossy()).unwrap()
        ),
    )
    .unwrap();
    let mut child = Command::new("codex")
        .args([
            "app-server",
            "--listen",
            "stdio://",
            "-c",
            &format!(
                "skills.config=[{{path={},enabled=false}}]",
                serde_json::to_string(&b.to_string_lossy()).unwrap()
            ),
        ])
        .env("CODEX_HOME", &home)
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for request in [
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"ophio-test","version":"1"},"capabilities":{}}}),
        serde_json::json!({"jsonrpc":"2.0","method":"initialized"}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"skills/list","params":{"cwds":[cwd],"forceReload":true}}),
    ] {
        writeln!(input, "{request}").unwrap();
    }
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let response = loop {
        let mut line = String::new();
        assert!(
            output.read_line(&mut line).unwrap() > 0,
            "Codex exited before skills/list"
        );
        let data: serde_json::Value = serde_json::from_str(&line).unwrap();
        if data["id"] == 2 {
            break data;
        }
    };
    child.kill().unwrap();
    child.wait().unwrap();
    let skills = response["result"]["data"][0]["skills"].as_array().unwrap();
    for name in ["A", "B"] {
        let skill = skills.iter().find(|skill| skill["name"] == name).unwrap();
        assert_eq!(skill["enabled"], false, "{name} should remain disabled");
    }
}

/// No account or model: verifies the installed Codex receives both the
/// person's effective instructions and Ophio's kept project chain.
#[test]
#[ignore = "runs installed Codex prompt-input without an account"]
fn personal_developer_instructions_survive_terminal_overrides() {
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join("AGENTS.override.md"), "DISABLED-CANARY").unwrap();
    std::fs::write(repo.join("AGENTS.md"), "CHAIN-CANARY").unwrap();
    let disabled = Disabled {
        files: vec![repo.join("AGENTS.override.md")],
        ..Disabled::default()
    };
    let evidence = PathBuf::from(
        std::env::var("OPHIO_LIVE_EVIDENCE")
            // Without an evidence folder, the record stays in the test's own.
            .unwrap_or_else(|_| tmp.path().to_string_lossy().into_owned()),
    )
    .join("raw/codex-developer-instructions-check.txt");
    std::fs::create_dir_all(evidence.parent().unwrap()).unwrap();
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&evidence)
        .unwrap();
    let config = "developer_instructions = 'MINE-CANARY'\n";
    std::fs::write(home.join("config.toml"), config).unwrap();
    let args = harness_codex::terminal_arguments_at(&repo, &disabled, Some(&home)).unwrap();
    let output = std::process::Command::new("codex")
        .args(["debug", "prompt-input"])
        .args(&args)
        .arg("hi")
        .current_dir(&repo)
        .env("CODEX_HOME", &home)
        .env("HOME", &home)
        .output()
        .expect("installed codex runs");
    let shown = |text: &str| text.replace(&tmp.path().to_string_lossy().into_owned(), "<tmp>");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    writeln!(
        log,
        "== top-level ==\nconfig: {config}\nargs: {}\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        shown(&format!("{args:?}")),
        output.status,
        shown(&stdout),
        shown(&stderr)
    )
    .unwrap();
    assert!(output.status.success(), "{stderr}");
    assert!(
        stdout.contains("MINE-CANARY") && stdout.contains("CHAIN-CANARY"),
        "{stdout}"
    );
    assert!(!stdout.contains("DISABLED-CANARY"), "{stdout}");
}

/// The chain Ophio rebuilds when an AGENTS.md in it is disabled reaches a
/// real Codex: a fallback file Codex is set to read is kept, the disabled
/// file isn't there. Uses only an isolated CODEX_HOME and `codex debug
/// prompt-input`, so no account and no model.
#[test]
#[ignore = "runs the installed Codex"]
fn the_rebuilt_document_chain_reaches_a_real_codex() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "project_doc_fallback_filenames = [\"NOTES.md\"]\n",
    )
    .unwrap();
    let repo = tmp.path().join("repo");
    let sub = repo.join("sub");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(repo.join("AGENTS.md"), "Root canary OSPREY-42.\n").unwrap();
    std::fs::write(sub.join("NOTES.md"), "Fallback canary HERON-77.\n").unwrap();
    let prompt = |args: &[String]| {
        let out = std::process::Command::new("codex")
            .args(["debug", "prompt-input"])
            .args(args)
            .arg("hi")
            .current_dir(&sub)
            .env("CODEX_HOME", &home)
            .env("HOME", &home)
            .output()
            .expect("codex runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    // As Codex reads it by itself: both.
    let plain = prompt(&[]);
    assert!(
        plain.contains("OSPREY-42") && plain.contains("HERON-77"),
        "{plain}"
    );
    let disabled = Disabled {
        files: vec![repo.join("AGENTS.md")],
        ..Disabled::default()
    };
    let args = harness_codex::terminal_arguments_at(&sub, &disabled, Some(&home)).unwrap();
    assert!(
        args.iter().any(|a| a == "project_doc_max_bytes=0"),
        "{args:?}"
    );
    let kept = prompt(&args);
    assert!(!kept.contains("OSPREY-42"), "{kept}");
    assert!(kept.contains("HERON-77"), "{kept}");
    if let Ok(evidence) = std::env::var("OPHIO_LIVE_EVIDENCE") {
        let shown = |text: &str| text.replace(&tmp.path().to_string_lossy().into_owned(), "<tmp>");
        std::fs::write(
            PathBuf::from(evidence).join("raw/codex-chain-check.txt"),
            format!(
                "args: {}\n\n== plain ==\n{}\n\n== with Ophio's arguments ==\n{}",
                shown(&format!("{args:?}")),
                shown(&plain),
                shown(&kept)
            ),
        )
        .unwrap();
    }
}
