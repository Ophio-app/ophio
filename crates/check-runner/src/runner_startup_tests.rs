// SPDX-License-Identifier: Apache-2.0
//! Live startup timing fixtures use a launcher for this check only, never PATH.
use super::*;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

struct Fixture {
    _root: tempfile::TempDir,
    project: PathBuf,
    data: PathBuf,
    launcher: PathBuf,
}
impl Fixture {
    fn new(wrapper: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let project = root_path.join("project");
        let data = root_path.join("data");
        fs::create_dir(&project).unwrap();
        fs::create_dir(&data).unwrap();
        let launcher = data.join("systemd-launcher");
        fs::write(&launcher, format!("#!/bin/sh\n{wrapper}\n")).unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _root: root,
            project,
            data,
            launcher,
        }
    }
    async fn run(&self, command: &str, timeout: Duration) -> CheckOutput {
        let run = start_inner(
            CheckRequest::new(&self.project, &self.data, command, timeout),
            RunnerOptions::default(),
            false,
            Some(self.launcher.to_str().unwrap()),
        )
        .await
        .unwrap_or_else(|error| panic!("Startup must return a check result, not {error:?}"));
        assert!(
            run.identity().sandbox_unit.is_some(),
            "This fixture needs systemd containment"
        );
        let identity = run.identity().clone();
        let output = run.wait().await.unwrap();
        eprintln!(
            "requested={timeout:?}; outcome={:?}; elapsed_ms={}; exit={:?}; cleanup={:?}",
            output.outcome, output.duration_ms, output.exit_code, output.stop,
        );
        assert_eq!(output.stop, StopStatus::Confirmed, "{output:?}");
        assert_eq!(native::stamp(identity.pid).unwrap(), None);
        assert_eq!(inspect(&identity).await, ProcessState::Stopped);
        output
    }
}

#[tokio::test]
#[ignore = "Live namespaces and a systemd user manager are required"]
async fn delayed_unit_creation_can_finish_after_five_seconds() {
    let f = Fixture::new("/usr/bin/sleep 6\nexec /usr/bin/systemd-run \"$@\"");
    let output = f
        .run("printf 'delayed unit ran\\n'", Duration::from_secs(15))
        .await;
    assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
    assert!(output.duration_ms >= 6_000, "{output:?}");
    assert_eq!(output.output_bytes, b"delayed unit ran\n".len() as u64);
    assert!(output.output_tail.ends_with("delayed unit ran\n"));
}

#[tokio::test]
#[ignore = "Live namespaces and a systemd user manager are required"]
async fn delayed_private_startup_marker_can_finish_after_five_seconds() {
    // The real unit exists immediately, but its trusted sandbox launch is delayed.
    let f =
        Fixture::new("exec /usr/bin/systemd-run --property='ExecStartPre=/usr/bin/sleep 6' \"$@\"");
    let output = f
        .run("printf 'delayed sandbox ran\\n'", Duration::from_secs(15))
        .await;
    assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
    assert!(output.duration_ms >= 6_000, "{output:?}");
    assert_eq!(output.output_bytes, b"delayed sandbox ran\n".len() as u64);
    assert!(output.output_tail.ends_with("delayed sandbox ran\n"));
}

#[tokio::test]
#[ignore = "Live namespaces and a systemd user manager are required"]
async fn startup_deadline_expiry_is_a_timeout_and_prevents_a_late_unit() {
    let f = Fixture::new("/usr/bin/sleep 2\nexec /usr/bin/systemd-run \"$@\"");
    let began = Instant::now();
    let output = f
        .run("touch must-not-run", Duration::from_millis(500))
        .await;
    assert_eq!(output.outcome, CheckOutcome::TimedOut, "{output:?}");
    assert!(output.duration_ms >= 500, "{output:?}");
    assert!(began.elapsed() < Duration::from_secs(2), "{output:?}");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !f.project.join("must-not-run").exists(),
        "A stopped launcher created a late unit"
    );
}

#[tokio::test]
#[ignore = "Live namespaces and a systemd user manager are required"]
async fn private_startup_deadline_expiry_is_a_timeout() {
    let f =
        Fixture::new("exec /usr/bin/systemd-run --property='ExecStartPre=/usr/bin/sleep 5' \"$@\"");
    let output = f
        .run("touch must-not-run", Duration::from_millis(700))
        .await;
    assert_eq!(output.outcome, CheckOutcome::TimedOut, "{output:?}");
    assert!(
        output.duration_ms >= 700 && output.duration_ms < 3_000,
        "{output:?}"
    );
    assert!(!f.project.join("must-not-run").exists());
}

#[tokio::test]
#[ignore = "Live namespaces and a systemd user manager are required"]
async fn successful_startup_does_not_reset_the_overall_timeout() {
    let f = Fixture::new("/usr/bin/sleep 1.2\nexec /usr/bin/systemd-run \"$@\"");
    let output = f
        .run(
            "printf 'check started\\n'; sleep 2.2; touch must-not-finish",
            Duration::from_millis(2_500),
        )
        .await;
    assert_eq!(output.outcome, CheckOutcome::TimedOut, "{output:?}");
    assert!(
        output.duration_ms >= 2_500 && output.duration_ms < 3_400,
        "{output:?}"
    );
    assert!(output.output_tail.contains("check started"), "{output:?}");
    assert!(!f.project.join("must-not-finish").exists());
}

#[tokio::test]
#[ignore = "Live namespaces and a systemd user manager are required"]
async fn trusted_startup_channel_failure_remains_not_started_with_diagnostics() {
    let f = Fixture::new("printf 'deliberate trusted startup failure\\n' >&2\nexit 3");
    let result = start_inner(
        CheckRequest::new(
            &f.project,
            &f.data,
            "touch must-not-run",
            Duration::from_secs(10),
        ),
        RunnerOptions::default(),
        false,
        Some(f.launcher.to_str().unwrap()),
    )
    .await;
    let Err(StartError::NotStarted { reason }) = result else {
        panic!("Trusted startup failure lost its start error: {result:?}");
    };
    assert!(
        reason.contains("deliberate trusted startup failure"),
        "{reason}"
    );
    assert!(!Path::new(&f.project).join("must-not-run").exists());
}

#[tokio::test]
async fn expired_systemd_probe_never_starts_a_sandbox_or_project_command() {
    let f = Fixture::new(": > \"$0.executed\"\nexit 99");
    let probe = f.data.join("slow-systemd-probe");
    fs::write(
        &probe,
        "#!/bin/sh\n: > \"$0.entered\"\nexec /usr/bin/sleep 2\n",
    )
    .unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .args([
            "-c",
            "touch sandbox-launcher; printf 'private startup marker\\n' > startup.marker; /bin/sh -c 'touch project-command'",
        ])
        .current_dir(&f.project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let began = Instant::now();
    let deadline = tokio::time::Instant::from_std(began) + Duration::from_millis(200);
    let result = native::spawn(
        &mut command,
        "ophio-check-0123456789abcdef0123456789abcdef",
        false,
        Some(f.launcher.to_str().unwrap()),
        deadline,
        began,
        Some(probe.to_str().unwrap()),
    )
    .await;
    assert!(f.data.join("slow-systemd-probe.entered").exists());
    let returned_without_spawning = result.is_err();
    let timeout_duration_ms = match &result {
        Err(StartError::TimedOutBeforeStart { duration_ms }) => Some(*duration_ms),
        _ => None,
    };
    if let Ok((mut child, guard)) = result {
        // The before-fix run must clean up its real, owned child without
        // reaping it before the guard has finished signalling its group.
        native::exited(&mut child).await.unwrap();
        let _ = guard.stop().await;
        child.wait().await.unwrap();
    }
    eprintln!(
        "elapsed_ms={}; returned_without_spawning={returned_without_spawning}; sandbox_launcher={}; startup_marker={}; project_command={}",
        began.elapsed().as_millis(),
        f.project.join("sandbox-launcher").exists(),
        f.project.join("startup.marker").exists(),
        f.project.join("project-command").exists(),
    );
    assert!(
        !f.project.join("sandbox-launcher").exists(),
        "A sandbox launcher ran after the deadline"
    );
    assert!(
        !f.project.join("startup.marker").exists(),
        "A startup marker was emitted after the deadline"
    );
    assert!(
        !f.project.join("project-command").exists(),
        "A project command ran after the deadline"
    );
    assert!(
        returned_without_spawning,
        "An expired probe must not return a spawned child"
    );
    assert!(!f.data.join("systemd-launcher.executed").exists());
    assert!(
        timeout_duration_ms.is_some_and(|elapsed| (200..1_000).contains(&elapsed)),
        "An expired probe must return a typed prelaunch timeout with elapsed duration: {timeout_duration_ms:?}"
    );
}
