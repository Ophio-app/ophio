// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::sandbox::*;
use std::sync::Mutex;
#[derive(Debug, Default)]
struct Tracked(Mutex<Vec<&'static str>>, bool, bool);
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
    let (_dir, adapter, mut spec) = fixture();
    let sandbox = Arc::new(Tracked::default());
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
    let calls = sandbox.0.lock().unwrap();
    assert_eq!(&calls[..3], ["wrap", "prepare", "started"]);
    assert!(calls.contains(&"session"));
    assert_eq!(calls.last(), Some(&"finished"));
}

#[tokio::test]
async fn relay_uses_dummy_auth_without_account_credentials_or_subscription() {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("api-auth"), "fixture").unwrap();
    spec.sandbox = Some(Arc::new(Tracked(Mutex::new(vec![]), true, false)));
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

#[tokio::test]
async fn history_copy_warning_reports_the_error_without_claiming_start_failed() {
    for stopped in [false, true] {
        let (_dir, adapter, mut spec) = fixture();
        spec.sandbox = Some(Arc::new(Tracked(Mutex::new(vec![]), false, true)));
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
