// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::sandbox::*;
use std::sync::Mutex;
#[derive(Debug, Default)]
struct Tracked(Mutex<Vec<&'static str>>, bool, bool, bool);

pub(super) fn with_skills() -> Arc<dyn Sandbox> {
    Arc::new(Tracked(Mutex::new(vec![]), false, false, true))
}
#[async_trait::async_trait]
impl Sandbox for Tracked {
    fn relay(&self) -> Option<RelayEndpoint> {
        self.1.then_some(RelayEndpoint {
            provider: RelayProvider::Anthropic,
            port: 18080,
        })
    }
    fn wrap(&self, request: WrapRequest<'_>) -> Result<Wrapped, HarnessError> {
        assert!(request.program.is_absolute());
        self.0.lock().unwrap().push("wrap");
        Ok(Wrapped {
            shared_skills: self.3,
            program: "/bin/sh".into(),
            clear_env: self.1,
            args: vec![
                "-c".into(),
                "test \"$OPHIO_TEST_WRAP\" = applied || exit 97; exec \"$@\"".into(),
                "wrapper".into(),
            ],
            env: vec![
                ("OPHIO_TEST_WRAP".into(), "applied".into()),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ],
        })
    }
    fn prepare(&self, _: &mut std::process::Command) -> std::io::Result<()> {
        self.0.lock().unwrap().push("prepare");
        Ok(())
    }
    async fn started(&self) -> Result<(), HarnessError> {
        self.0.lock().unwrap().push("started");
        Ok(())
    }
    fn session_started(&self, _: &str) -> Result<(), HarnessError> {
        self.0.lock().unwrap().push("session");
        Ok(())
    }
    async fn update(&self, _: Reach) -> Applied {
        Applied::Applied
    }
    async fn finished(&self) -> Result<(), HarnessError> {
        self.0.lock().unwrap().push("finished");
        if self.2 {
            return Err(HarnessError::SpawnFailed(
                "History copy failed: disk full.".into(),
            ));
        }
        Ok(())
    }
}
#[tokio::test]
async fn sandbox_wrap_and_lifecycle_surround_the_native_process() {
    let (dir, adapter, mut spec) = fixture();
    let sandbox = Arc::new(Tracked(Mutex::new(vec![]), false, false, true));
    spec.sandbox = Some(sandbox.clone());
    spec.prompt.text = "wait".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        if matches!(event(&mut run.events).await, HarnessEvent::TurnStarted) {
            break;
        }
    }
    run.control.terminate().await.unwrap();
    loop {
        if matches!(event(&mut run.events).await, HarnessEvent::Exited { .. }) {
            break;
        }
    }
    let args: Vec<String> =
        serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap()).unwrap();
    assert!(args.windows(2).any(|p| p
        == [
            "--tools",
            "Bash,Read,Write,Edit,Glob,Grep,TaskOutput,TaskStop,Skill"
        ]));
    let calls = sandbox.0.lock().unwrap();
    assert_eq!(&calls[..3], ["wrap", "prepare", "started"]);
    assert!(calls.contains(&"session"));
    assert_eq!(calls.last(), Some(&"finished"));
}

#[tokio::test]
async fn relay_uses_dummy_auth_without_account_credentials_or_subscription() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("api-auth"), "fixture").unwrap();
    spec.sandbox = Some(Arc::new(Tracked(Mutex::new(vec![]), true, false, false)));
    spec.bridge = None;
    spec.resume_session = None;
    spec.env = ports::RunEnv(vec![
        ("PRIVATE_ACCOUNT_TOKEN".into(), "fixture-private".into()),
        ("ANTHROPIC_AUTH_TOKEN".into(), "fixture-private".into()),
    ]);
    spec.prompt.text = "wait".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::SessionStarted { .. } => break,
            HarnessEvent::Exited { .. } => panic!("relay could not start without a subscription"),
            _ => {}
        }
    }
    assert!(dir.path().join("relay-checked").exists());
    let args: Vec<String> =
        serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap()).unwrap();
    assert!(args.iter().any(|a| a == "--bare"));
    assert!(args.windows(2).any(|a| a == ["--setting-sources", ""]));
    run.control.terminate().await.unwrap();
}

#[derive(Debug)]
struct LocalCapability;
#[async_trait::async_trait]
impl Sandbox for LocalCapability {
    fn relay(&self) -> Option<RelayEndpoint> {
        Some(RelayEndpoint {
            provider: RelayProvider::Anthropic,
            port: 18080,
        })
    }
    fn wrap(&self, request: WrapRequest<'_>) -> Result<Wrapped, HarnessError> {
        let mut wrapped = Tracked(Mutex::new(vec![]), true, false, false).wrap(request)?;
        wrapped.env.extend([
            (
                "ANTHROPIC_API_KEY".into(),
                "fixture-local-capability".into(),
            ),
            (
                "ANTHROPIC_BASE_URL".into(),
                "http://untrusted.invalid".into(),
            ),
            ("CLAUDE_CODE_SIMPLE".into(), "0".into()),
            ("DISABLE_TELEMETRY".into(), "0".into()),
        ]);
        Ok(wrapped)
    }
    fn prepare(&self, command: &mut std::process::Command) -> std::io::Result<()> {
        let value = |name: &str| {
            command
                .get_envs()
                .find(|(key, _)| *key == name)
                .and_then(|(_, value)| value)
        };
        assert!(
            value("ANTHROPIC_API_KEY") == Some(std::ffi::OsStr::new("fixture-local-capability"))
        );
        assert!(
            value("ANTHROPIC_BASE_URL") == Some(std::ffi::OsStr::new("http://127.0.0.1:18080"))
        );
        for name in [
            "CLAUDE_CODE_SIMPLE",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            "DISABLE_TELEMETRY",
            "DISABLE_ERROR_REPORTING",
            "DISABLE_AUTOUPDATER",
        ] {
            assert!(value(name) == Some(std::ffi::OsStr::new("1")));
        }
        assert!(value("PRIVATE_ACCOUNT_TOKEN").is_none());
        assert!(value("ANTHROPIC_AUTH_TOKEN").is_none());
        assert!(
            command
                .get_args()
                .all(|arg| !arg.to_string_lossy().contains("fixture-local-capability"))
        );
        Ok(())
    }
    async fn started(&self) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn update(&self, _: Reach) -> Applied {
        Applied::Applied
    }
}

#[tokio::test]
async fn relay_preserves_only_the_wrapped_local_capability() {
    let (_dir, adapter, mut spec) = fixture();
    spec.sandbox = Some(Arc::new(LocalCapability));
    spec.bridge = None;
    spec.resume_session = None;
    spec.env = ports::RunEnv(vec![
        ("ANTHROPIC_API_KEY".into(), "fixture-account-key".into()),
        ("PRIVATE_ACCOUNT_TOKEN".into(), "fixture-account-key".into()),
        ("ANTHROPIC_AUTH_TOKEN".into(), "fixture-account-key".into()),
    ]);
    spec.prompt.text = "wait".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::SessionStarted { .. } => break,
            HarnessEvent::Exited { .. } => panic!("authored relay fixture did not start"),
            _ => {}
        }
    }
    run.control.terminate().await.unwrap();
}

#[tokio::test]
async fn history_copy_warning_reports_the_error_without_claiming_start_failed() {
    for stopped in [false, true] {
        let (_dir, adapter, mut spec) = fixture();
        spec.sandbox = Some(Arc::new(Tracked(Mutex::new(vec![]), false, true, false)));
        spec.prompt.text = if stopped { "wait" } else { "complete" }.into();
        let mut run = adapter.start_run(spec).await.unwrap();
        let mut warnings = Vec::new();
        loop {
            match event(&mut run.events).await {
                HarnessEvent::TurnStarted if stopped => run.control.terminate().await.unwrap(),
                HarnessEvent::Warning { text } => warnings.push(text),
                HarnessEvent::Exited { exit, .. } => {
                    assert_eq!(
                        exit,
                        if stopped {
                            RunExit::Interrupted
                        } else {
                            RunExit::Completed
                        }
                    );
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(warnings, ["History copy failed: disk full."]);
    }
}

/// A sandbox that keeps the agent's settings in a folder of the
/// conversation's own, and says so of each file in it.
#[derive(Debug)]
struct Keeps(Tracked, PathBuf);
#[async_trait::async_trait]
impl Sandbox for Keeps {
    fn wrap(&self, request: WrapRequest<'_>) -> Result<Wrapped, HarnessError> {
        self.0.wrap(request)
    }
    fn prepare(&self, command: &mut std::process::Command) -> std::io::Result<()> {
        self.0.prepare(command)
    }
    async fn started(&self) -> Result<(), HarnessError> {
        self.0.started().await
    }
    async fn update(&self, reach: Reach) -> Applied {
        self.0.update(reach).await
    }
    fn own_settings_file(&self, path: &Path) -> bool {
        path.starts_with(&self.1)
    }
}

/// The receipt of a run that wrote a note in the agent's settings folder
/// and a project file, kept in `sandbox`.
async fn receipt_of_a_note(sandbox: Option<&str>) -> Vec<String> {
    let (dir, adapter, mut spec) = fixture();
    let settings = dir.path().join("settings");
    let note = settings.join("projects/-p/memory/note.md");
    let source = dir.path().join("src/lib.rs");
    let frames = serde_json::json!([
        {"type": "assistant", "message": {"id": "m1", "content": [
            {"type": "tool_use", "id": "w1", "name": "Write",
             "input": {"file_path": note, "content": "remember this"}},
            {"type": "tool_use", "id": "w2", "name": "Write",
             "input": {"file_path": source, "content": "fn main() {}"}}]}},
        {"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "w1", "content": "written"},
            {"type": "tool_result", "tool_use_id": "w2", "content": "written"}]}}
    ]);
    std::fs::write(dir.path().join("after-init.json"), frames.to_string()).unwrap();
    spec.sandbox = match sandbox {
        Some("keeps") => Some(Arc::new(Keeps(Tracked::default(), settings))),
        Some(_) => Some(Arc::new(Tracked::default())),
        None => None,
    };
    spec.prompt.text = "complete".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut listed = Vec::new();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Results { outcome } => {
                listed = outcome.changed_files.into_iter().map(|f| f.path).collect();
            }
            HarnessEvent::Exited { outcome, .. } => {
                if let Some(outcome) = outcome {
                    listed = outcome.changed_files.into_iter().map(|f| f.path).collect();
                }
                break;
            }
            _ => {}
        }
    }
    let name = |path: &str| {
        path.strip_prefix(dir.path().to_str().unwrap())
            .unwrap()
            .to_owned()
    };
    listed.iter().map(|p| name(p)).collect()
}

#[tokio::test]
async fn a_limited_chats_own_settings_note_is_left_off_its_receipt() {
    assert_eq!(receipt_of_a_note(Some("keeps")).await, ["/src/lib.rs"]);
}

#[tokio::test]
async fn a_settings_note_is_listed_unsandboxed_or_where_the_sandbox_cant_say() {
    let both = ["/settings/projects/-p/memory/note.md", "/src/lib.rs"];
    assert_eq!(receipt_of_a_note(None).await, both);
    assert_eq!(receipt_of_a_note(Some("unknown")).await, both);
}

/// Records what a run was given to read.
#[derive(Debug, Default)]
struct Seeing(Mutex<Vec<PathBuf>>);
#[async_trait::async_trait]
impl Sandbox for Seeing {
    fn wrap(&self, request: WrapRequest<'_>) -> Result<Wrapped, HarnessError> {
        self.0
            .lock()
            .unwrap()
            .extend(request.visible.iter().cloned());
        Ok(Wrapped {
            shared_skills: false,
            program: "/bin/sh".into(),
            clear_env: false,
            args: vec!["-c".into(), "exec \"$@\"".into(), "wrapper".into()],
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
        })
    }
    fn prepare(&self, _: &mut std::process::Command) -> std::io::Result<()> {
        Ok(())
    }
    async fn started(&self) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn update(&self, _: Reach) -> Applied {
        Applied::Applied
    }
}

#[tokio::test]
async fn a_limited_chat_is_shown_its_attached_files_and_told_where_they_are() {
    let (dir, adapter, mut spec) = fixture();
    let attached = dir.path().join("data/attached/conversation");
    std::fs::create_dir_all(&attached).unwrap();
    let sandbox = Arc::new(Seeing::default());
    spec.sandbox = Some(sandbox.clone());
    spec.attached = Some(attached.clone());
    let note = format!(
        "\n\nThe person attached these files to this message.\n- \"report.pdf\" (application/pdf, 3 bytes): \"{}\"",
        attached.join("report.pdf").display()
    );
    spec.prompt.text = format!("wait{note}");
    let mut run = adapter.start_run(spec).await.unwrap();
    loop {
        match event(&mut run.events).await {
            HarnessEvent::TurnStarted => break,
            HarnessEvent::Exited { .. } => panic!("the run did not start"),
            _ => {}
        }
    }
    assert!(
        sandbox.0.lock().unwrap().contains(&attached),
        "{:?}",
        sandbox.0.lock().unwrap()
    );
    let args: Vec<String> =
        serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap()).unwrap();
    // Claude Code reads it without asking first.
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--add-dir" && w[1] == attached.to_str().unwrap()),
        "{args:?}"
    );
    // The agent is told which files came and where each is.
    let input = std::fs::read_to_string(dir.path().join("stdin.jsonl")).unwrap();
    let quoted = serde_json::to_string(&note).unwrap();
    assert!(input.contains(&quoted[1..quoted.len() - 1]), "{input}");
    run.control.terminate().await.unwrap();
}
