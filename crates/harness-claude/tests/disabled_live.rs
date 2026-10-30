// SPDX-License-Identifier: Apache-2.0
//! Disable in Ophio (D-071) against the installed Claude Code, through
//! Ophio's own adapter: what it launches is what a task launches. Ignored:
//! it uses the person's account. The caller makes the throwaway files:
//! `OPHIO_LIVE_PROJECT` with `.claude/skills/ophio-probe-zebra`,
//! `.claude/agents/ophio-probe-yak.md` and a `CLAUDE.md` canary, and a user
//! skill at `OPHIO_LIVE_USER_SKILL` (`ophio-probe-heron`); points
//! `OPHIO_LIVE_BINARY` at a wrapper that records the command and its output
//! in `OPHIO_LIVE_EVIDENCE`; and removes them all afterwards.
use harness_claude::ClaudeAdapter;
use ports::{Disabled, DisabledEntry, HarnessAdapter, HarnessEvent, PromptInput, RunEnv, RunSpec};
use protocol::{
    capability::HarnessKind,
    model::{LaunchPolicy, TaskMode},
};
use std::{path::PathBuf, time::Duration};

fn var(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set")))
}

fn spec(project: PathBuf, disabled: Disabled, project_settings: bool, text: &str) -> RunSpec {
    RunSpec {
        run_id: protocol::RunId::new(),
        task_id: protocol::TaskId::new(),
        mode: TaskMode::Work,
        cwd: project,
        prompt: PromptInput {
            text: text.into(),
            images: vec![],
        },
        model: Some("sonnet".into()),
        effort: None,
        launch_policy: Some(LaunchPolicy::BypassPermissions),
        resume_session: None,
        resume_from_mode: None,
        mission: None,
        branch: None,
        start: None,
        bridge: None,
        project_settings,
        env: RunEnv::default(),
        github: None,
        scratch: None,
        questions: true,
        sandbox: None,
        web: false,
        disabled,
    }
}

/// Every message and tool the run showed, as text.
async fn run(spec: RunSpec, label: &str) -> String {
    let binary = var("OPHIO_LIVE_BINARY");
    let adapter = ClaudeAdapter::with_binary(binary);
    let mut started = adapter.start_run(spec).await.expect("Claude Code started");
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
                HarnessEvent::Child { label, state, .. } => {
                    out.push_str(&format!("[child] {label} {state:?}\n"))
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
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(420), all)
        .await
        .expect("the run ended in time");
    let evidence = var("OPHIO_LIVE_EVIDENCE");
    std::fs::write(evidence.join(format!("claude-{label}.events.txt")), &out).unwrap();
    out
}

const ASK: &str = "Answer from what you were given, without reading any files first.\n\
1. Write SKILLS: then the exact names of every skill you can invoke, comma separated.\n\
2. Write AGENTS: then the exact names of every subagent type you can spawn, comma separated.\n\
3. Write CANARY: then every project canary word your instructions give, or NONE.\n\
4. Invoke the skill ophio-probe-zebra with the Skill tool, then the skill ophio-probe-heron, and report what each returned or why it failed.\n\
5. Spawn the subagent type ophio-probe-yak with the task \"Reply YAK-OK\" and report what happened.\n\
6. Spawn a general-purpose subagent with the task \"List the exact names of every skill you can invoke, comma separated, and try to invoke the skill ophio-probe-zebra\" and report its answer verbatim.";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed Claude Code with the person's account"]
async fn a_real_claude_run_is_kept_from_what_is_disabled() {
    let project = var("OPHIO_LIVE_PROJECT");
    let user_skill = var("OPHIO_LIVE_USER_SKILL");
    let project_skill = project.join(".claude/skills/ophio-probe-zebra/SKILL.md");
    let agent = project.join(".claude/agents/ophio-probe-yak.md");
    let claude_md = project.join("CLAUDE.md");
    // Claude Code reads a folder's AGENTS.md when its CLAUDE.md isn't loaded.
    let agents_md = project.join("AGENTS.md");
    let entry = |path: PathBuf, name: &str| DisabledEntry {
        path,
        name: name.into(),
        harness: Some(HarnessKind::ClaudeCode),
    };
    let disabled = Disabled {
        skills: vec![
            entry(project_skill, "ophio-probe-zebra"),
            entry(user_skill, "ophio-probe-heron"),
        ],
        agents: vec![entry(agent, "ophio-probe-yak")],
        files: vec![claude_md, agents_md],
        memory: false,
        output_style: false,
    };
    // Nothing disabled: what the same launch sees otherwise.
    let open = run(
        spec(project.clone(), Disabled::default(), true, ASK),
        "open",
    )
    .await;
    for expected in [
        "ophio-probe-zebra",
        "ophio-probe-heron",
        "ophio-probe-yak",
        "PELICAN-77",
    ] {
        assert!(
            open.contains(expected),
            "open run did not expose {expected}: {open}"
        );
    }
    // Disabled, with the project's settings on (project skills and agents
    // load), and in the ordinary shape, with them off.
    let blocked_project = run(
        spec(project.clone(), disabled.clone(), true, ASK),
        "blocked-project-settings",
    )
    .await;
    let blocked_default = run(spec(project, disabled, false, ASK), "blocked-default").await;
    for (label, text) in [
        ("project settings", blocked_project),
        ("default", blocked_default),
    ] {
        // The installed CLI's tool diagnostics name the setting/rule, while
        // natural-language descriptions of a denial vary by model.
        assert!(
            text.contains("skillOverrides") || text.contains("Unknown skill"),
            "{label}: zebra refusal was not visible: {text}"
        );
        assert!(
            text.contains("skillOverrides"),
            "{label}: heron refusal was not visible: {text}"
        );
        assert!(
            text.contains("Agent(ophio-probe-yak)"),
            "{label}: yak denial was not visible: {text}"
        );
        for secret in ["ZEBRA-OK", "HERON-OK", "PELICAN-77", "OSPREY-42"] {
            assert!(!text.contains(secret), "{label}: leaked {secret}: {text}");
        }
    }
}
