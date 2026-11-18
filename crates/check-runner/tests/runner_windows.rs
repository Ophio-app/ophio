// SPDX-License-Identifier: Apache-2.0
#![cfg(windows)]
#![allow(unsafe_code)]

use check_runner::runner::{
    self, CheckOutcome, CheckRequest, ProcessIdentity, ProcessState, StopStatus,
};
use containment::NamedJob;
use std::{
    io,
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;
use windows_sys::Win32::{
    Foundation::{FILETIME, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS, GetProcessTimes, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    },
};

/// Authored inert subprocesses only. The intermediate process exits deliberately;
/// the grandchild detaches from the console and outlives it until the Job stops.
#[test]
fn windows_fixture() {
    let role = std::env::var("OPHIO_CHECK_WINDOWS_FIXTURE")
        .ok()
        .or_else(|| std::fs::read_to_string("fixture-role").ok());
    let Some(role) = role else {
        return;
    };
    if role == "leaf" {
        let process = open(std::process::id());
        std::fs::write(
            "leaf.identity.part",
            format!("{} {}", std::process::id(), born(&process)),
        )
        .unwrap();
        std::fs::rename("leaf.identity.part", "leaf.identity").unwrap();
    } else {
        let next = if role == "root-detached" {
            "middle"
        } else {
            "leaf"
        };
        if role == "root-detached" {
            std::fs::write("middle.expected", b"").unwrap();
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "windows_fixture", "--nocapture"])
            .env("OPHIO_CHECK_WINDOWS_FIXTURE", next)
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        if role == "root-detached" {
            assert!(child.wait().unwrap().success());
            std::fs::write("middle.exited", b"").unwrap();
        }
        // A std Child drop does not kill or wait. Detachment cannot escape a Job.
        drop(child);
        if role == "middle" || role == "root-exit" {
            return;
        }
    }
    loop {
        std::thread::park();
    }
}

fn name() -> String {
    format!("ophio-check-{}", uuid::Uuid::new_v4())
}

fn fixture_command(project: &Path, role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .current_dir(project)
        .args(["--exact", "windows_fixture", "--nocapture"])
        .env("OPHIO_CHECK_WINDOWS_FIXTURE", role)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn open(pid: u32) -> OwnedHandle {
    // SAFETY: opens only non-inheritable query/synchronization rights to an
    // authored fixture process, never any destructive process rights.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    assert!(
        !handle.is_null(),
        "opening fixture {pid}: {}",
        io::Error::last_os_error()
    );
    // SAFETY: OpenProcess returned one uniquely owned handle.
    unsafe { OwnedHandle::from_raw_handle(handle) }
}

fn born(process: &OwnedHandle) -> u64 {
    let (mut birth, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: the owned handle and four writable FILETIME buffers remain live.
    assert_ne!(
        unsafe {
            GetProcessTimes(
                process.as_raw_handle(),
                &mut birth,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        },
        0
    );
    (u64::from(birth.dwHighDateTime) << 32) | u64::from(birth.dwLowDateTime)
}

fn state(process: &OwnedHandle) -> u32 {
    // SAFETY: the process object is pinned by this synchronization handle.
    unsafe { WaitForSingleObject(process.as_raw_handle(), 0) }
}

async fn leaf(project: &Path) -> OwnedHandle {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if (!project.join("middle.expected").exists() || project.join("middle.exited").exists())
                && let Ok(text) = std::fs::read_to_string(project.join("leaf.identity"))
            {
                let values: Vec<_> = text.split_whitespace().collect();
                let process = open(values[0].parse().unwrap());
                assert_eq!(born(&process), values[1].parse::<u64>().unwrap());
                assert_eq!(state(&process), WAIT_TIMEOUT);
                return process;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture grandchild became ready")
}

fn identity(child: &tokio::process::Child, project: &Path, name: &str) -> ProcessIdentity {
    let pid = child.id().unwrap();
    ProcessIdentity {
        pid,
        start_time: born(&open(pid)),
        sandbox_unit: Some(name.into()),
        project: project.into(),
        boot_id: "windows-filetime-v1".into(),
    }
}

#[tokio::test]
async fn named_job_stops_detached_grandchild_after_intermediate_exit() {
    let project = tempfile::tempdir().unwrap();
    let job = NamedJob::new(&name()).unwrap();
    let mut child = job
        .spawn(&mut fixture_command(project.path(), "root-detached"))
        .await
        .unwrap();
    let root = open(child.id().unwrap());
    let grandchild = leaf(project.path()).await;
    job.end().await.unwrap();
    assert_eq!(state(&root), WAIT_OBJECT_0);
    assert_eq!(state(&grandchild), WAIT_OBJECT_0);
    child.wait().await.unwrap();
}

#[tokio::test]
async fn recovery_roundtrip_stops_pinned_tree_after_creator_handle_lost() {
    let project = tempfile::tempdir().unwrap();
    let name = name();
    let creator = NamedJob::new(&name).unwrap();
    let mut child = creator
        .spawn(&mut fixture_command(project.path(), "root-simple"))
        .await
        .unwrap();
    let saved = identity(&child, project.path(), &name);
    let saved: ProcessIdentity =
        serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
    let root = open(saved.pid);
    let grandchild = leaf(project.path()).await;
    // A surviving independent handle simulates recovery; a real crash with no
    // such handle is covered separately by the kill-on-last-close test.
    let retained = NamedJob::open(&name).unwrap().unwrap();
    drop(creator);
    assert_eq!(runner::inspect(&saved).await, ProcessState::Running);
    assert_eq!(runner::stop(&saved).await, StopStatus::Confirmed);
    assert_eq!(state(&root), WAIT_OBJECT_0);
    assert_eq!(state(&grandchild), WAIT_OBJECT_0);
    child.wait().await.unwrap();
    drop(retained);
    assert_eq!(runner::inspect(&saved).await, ProcessState::Stopped);
    assert_eq!(runner::stop(&saved).await, StopStatus::Confirmed);
}

#[tokio::test]
async fn recovery_finds_detached_descendant_after_root_exit() {
    let project = tempfile::tempdir().unwrap();
    let name = name();
    let creator = NamedJob::new(&name).unwrap();
    let mut child = creator
        .spawn(&mut fixture_command(project.path(), "root-exit"))
        .await
        .unwrap();
    let saved = identity(&child, project.path(), &name);
    let grandchild = leaf(project.path()).await;
    child.wait().await.unwrap();
    assert_eq!(runner::inspect(&saved).await, ProcessState::Running);
    // Recovery can stop the tree, but cannot reconstruct the departed root's
    // native exit proof. The original owner still has its notification history.
    assert!(matches!(
        runner::stop(&saved).await,
        StopStatus::Uncertain { .. }
    ));
    creator.end().await.unwrap();
    assert_eq!(state(&grandchild), WAIT_OBJECT_0);
}

#[tokio::test]
async fn last_handle_close_kills_tree_and_restart_finds_it_stopped() {
    let project = tempfile::tempdir().unwrap();
    let name = name();
    let job = NamedJob::new(&name).unwrap();
    let mut child = job
        .spawn(&mut fixture_command(project.path(), "root-detached"))
        .await
        .unwrap();
    let saved = identity(&child, project.path(), &name);
    let grandchild = leaf(project.path()).await;
    drop(job);
    tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    // SAFETY: this pins the exact descendant and waits only for its native exit.
    assert_eq!(
        unsafe { WaitForSingleObject(grandchild.as_raw_handle(), 3000) },
        WAIT_OBJECT_0
    );
    assert_eq!(runner::inspect(&saved).await, ProcessState::Stopped);
    assert_eq!(runner::stop(&saved).await, StopStatus::Confirmed);
}

#[tokio::test]
async fn changed_stamp_or_wrong_job_never_stops_unrelated_processes() {
    let project = tempfile::tempdir().unwrap();
    let name = name();
    let job = NamedJob::new(&name).unwrap();
    let mut child = job
        .spawn(&mut fixture_command(project.path(), "root-simple"))
        .await
        .unwrap();
    let saved = identity(&child, project.path(), &name);
    let root = open(saved.pid);
    let grandchild = leaf(project.path()).await;
    let mut reused = saved.clone();
    reused.start_time += 1;
    assert!(matches!(
        runner::inspect(&reused).await,
        ProcessState::Uncertain { .. }
    ));
    assert!(matches!(
        runner::stop(&reused).await,
        StopStatus::Uncertain { .. }
    ));
    assert_eq!(state(&root), WAIT_TIMEOUT);
    assert_eq!(state(&grandchild), WAIT_TIMEOUT);
    for malformed in [
        None,
        Some("not-a-job".into()),
        Some("ophio-check-00000000-0000-0000-0000-000000000000".into()),
    ] {
        let mut invalid = saved.clone();
        invalid.sandbox_unit = malformed;
        assert!(matches!(
            runner::stop(&invalid).await,
            StopStatus::Uncertain { .. }
        ));
    }
    let mut invalid_boot = saved.clone();
    invalid_boot.boot_id = "different-format".into();
    assert!(matches!(
        runner::stop(&invalid_boot).await,
        StopStatus::Uncertain { .. }
    ));
    let another = NamedJob::new(&self::name()).unwrap();
    let mut wrong = saved.clone();
    wrong.pid = std::process::id();
    wrong.start_time = born(&open(wrong.pid));
    assert!(matches!(
        runner::stop(&wrong).await,
        StopStatus::Uncertain { .. }
    ));
    wrong = saved.clone();
    wrong.sandbox_unit = Some(self::name());
    assert!(matches!(
        runner::stop(&wrong).await,
        StopStatus::Uncertain { .. }
    ));
    assert_eq!(state(&root), WAIT_TIMEOUT);
    // Recreating an occupied name must not alter its limits or kill its tree.
    assert!(NamedJob::new(&name).is_err());
    assert_eq!(state(&root), WAIT_TIMEOUT);
    job.end().await.unwrap();
    child.wait().await.unwrap();
    drop(another);
}

#[tokio::test]
async fn incomplete_recovered_census_stops_tree_but_reports_uncertain() {
    let project = tempfile::tempdir().unwrap();
    let name = name();
    let creator = NamedJob::new(&name).unwrap();
    let mut child = creator
        .spawn(&mut fixture_command(project.path(), "root-detached"))
        .await
        .unwrap();
    let saved = identity(&child, project.path(), &name);
    let grandchild = leaf(project.path()).await;
    // The ready marker proves the intermediate process exited. The creator has
    // its notification history, but a new handle cannot reconstruct that proof.
    let retained = NamedJob::open(&name).unwrap().unwrap();
    drop(creator);
    assert!(matches!(
        runner::stop(&saved).await,
        StopStatus::Uncertain { .. }
    ));
    tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    // SAFETY: this only waits for the pinned fixture descendant to signal exit.
    assert_eq!(
        unsafe { WaitForSingleObject(grandchild.as_raw_handle(), 3000) },
        WAIT_OBJECT_0
    );
    drop(retained);
}

#[tokio::test]
async fn runner_timeout_and_cancel_stop_detached_descendants() {
    for cancel in [false, true] {
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("fixture-role"), "root-detached").unwrap();
        let command = format!(
            "\"{}\" --exact windows_fixture --nocapture",
            std::env::current_exe().unwrap().display()
        );
        let request = CheckRequest::new(
            project.path(),
            data.path(),
            command,
            if cancel {
                Duration::from_secs(30)
            } else {
                Duration::from_secs(5)
            },
        );
        let running = runner::start(request).await.unwrap();
        let grandchild =
            match tokio::time::timeout(Duration::from_secs(12), leaf(project.path())).await {
                Ok(process) => process,
                Err(_) => panic!(
                    "fixture did not start: {:?}",
                    running.cancel_and_wait().await
                ),
            };
        let output = if cancel {
            running.cancel_and_wait().await
        } else {
            running.wait().await
        }
        .unwrap();
        assert_eq!(
            output.outcome,
            if cancel {
                CheckOutcome::Cancelled
            } else {
                CheckOutcome::TimedOut
            }
        );
        assert_eq!(output.stop, StopStatus::Confirmed);
        assert_eq!(state(&grandchild), WAIT_OBJECT_0);
    }
}

#[tokio::test]
async fn runner_finds_granted_home_toolchains_with_plain_and_extended_paths() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let bin = home.join(".cargo/bin");
    let project = root.path().join("project");
    let data = root.path().join("data");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        bin.join("ophio-home-toolchain.cmd"),
        "@echo off\r\necho home-toolchain-started\r\n",
    )
    .unwrap();
    let home = home.canonicalize().unwrap();
    let bin = bin.canonicalize().unwrap();
    let plain = |path: &Path| {
        std::path::PathBuf::from(path.to_str().unwrap().strip_prefix(r"\\?\").unwrap())
    };
    for (home, bin) in [(plain(&home), plain(&bin)), (home, bin)] {
        let reads =
            check_runner::toolchains::readable_toolchains(&home, std::slice::from_ref(&bin), false)
                .unwrap();
        assert_eq!(reads, vec![bin]);
        let mut request = CheckRequest::new(
            &project,
            &data,
            "ophio-home-toolchain.cmd",
            Duration::from_secs(15),
        );
        request.read_toolchains = reads;
        let output = runner::start(request).await.unwrap().wait().await.unwrap();
        assert_eq!(output.outcome, CheckOutcome::Passed, "{output:?}");
        assert_eq!(output.stop, StopStatus::Confirmed, "{output:?}");
        assert!(
            output.output_tail.contains("home-toolchain-started"),
            "{output:?}"
        );
    }
}

#[tokio::test]
async fn runner_reports_exit_code_combined_output_and_no_windows_sandbox() {
    let project = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let running = runner::start(CheckRequest::new(
        project.path(),
        data.path(),
        "echo stdout-check & echo stderr-check 1>&2 & exit /b 7",
        Duration::from_secs(10),
    ))
    .await
    .unwrap();
    let output = running.wait().await.unwrap();
    assert_eq!(output.outcome, CheckOutcome::Failed);
    assert_eq!(output.exit_code, Some(7));
    assert_eq!(output.stop, StopStatus::Confirmed);
    assert!(output.output_tail.contains("stdout-check"), "{output:?}");
    assert!(output.output_tail.contains("stderr-check"));
    assert!(
        output
            .output_tail
            .contains("Windows cannot enforce a network limit")
    );
    assert!(matches!(
        output.sandbox_level,
        sandbox::checks::SandboxLevel::Unsandboxed { .. }
    ));
}
