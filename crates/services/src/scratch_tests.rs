// SPDX-License-Identifier: Apache-2.0
//! A task's scratch folder: given to its runs, shown from, and deleted once
//! the task is done with it.
use super::shown::{PNG, bound, person};
use super::*;
use crate::scratch::{KEPT_FOR, Need, name, scratch_env, sweep};
use ports::bridge::PersonCall;
use std::{collections::HashMap, time::SystemTime};

fn show(path: &Path) -> PersonCall {
    PersonCall::Show {
        path: path.to_str().unwrap().into(),
        caption: None,
        invite: None,
    }
}
fn filled(dir: &Path, bytes: usize) {
    std::fs::create_dir_all(dir.join("shots")).unwrap();
    std::fs::write(dir.join("shots/recording.webm"), vec![0; bytes]).unwrap();
}

#[tokio::test]
async fn a_run_is_given_its_tasks_scratch_folder() {
    let f = Fixture::new().await;
    let (task, _, _) = bound(&f).await;
    let spec = f.harness.specs.lock().unwrap().last().cloned().unwrap();
    let scratch = f.hub.scratch_dir(task);
    assert_eq!(spec.scratch.as_ref(), Some(&scratch));
    assert!(scratch.is_dir());
    let path = scratch.to_str().unwrap();
    assert!(
        (spec.env.0.iter()).any(|(key, value)| key == ports::SCRATCH_ENV && value == path),
        "{:?}",
        spec.env
    );
    assert!(
        ports::harness::scratch_instruction(&spec)
            .unwrap()
            .contains(path)
    );
    f.close().await;
}

#[tokio::test]
async fn a_tasks_scratch_folder_goes_once_it_has_been_finished_a_while() {
    let f = Fixture::new().await;
    let (task, run, _) = bound(&f).await;
    let scratch = f.hub.scratch_dir(task);
    filled(&scratch, 16);
    // However long it runs.
    f.clock.advance(KEPT_FOR * 2);
    f.hub.collect_scratch().await.unwrap();
    assert!(scratch.join("shots/recording.webm").exists());

    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    let mut wait = waiting();
    loop {
        let (task, run): (Task, Run) = f
            .hub
            .read(|r| Ok((r.require("task", task)?, r.require("run", run)?)))
            .unwrap();
        if task.state == TaskState::Completed && !crate::runs::still_finishing(&run) {
            break;
        }
        assert!(wait(), "the run did not finish");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    f.hub.collect_scratch().await.unwrap();
    assert!(scratch.exists(), "kept for a look back");
    f.clock.advance(KEPT_FOR);
    f.hub.collect_scratch().await.unwrap();
    assert!(!scratch.exists());
    f.close().await;
}

#[tokio::test]
async fn a_queued_continuation_keeps_expired_scratch() {
    queued_continuation_keeps_scratch(false).await;
}

#[tokio::test]
async fn a_queued_continuation_keeps_scratch_under_size_pressure() {
    queued_continuation_keeps_scratch(true).await;
}

async fn queued_continuation_keeps_scratch(size_pressure: bool) {
    let f = Fixture::new().await;
    let (task_id, run_id, _) = bound(&f).await;
    let scratch = f.hub.scratch_dir(task_id);
    filled(&scratch, 16);
    let recording = scratch.join("shots/recording.webm");
    if size_pressure {
        // A sparse file exercises the real 10 GiB collector limit.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&recording)
            .unwrap()
            .set_len(11 * 1024 * 1024 * 1024)
            .unwrap();
    }
    f.harness
        .emit(
            run_id,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    let mut wait = waiting();
    let task = loop {
        let (task, run): (Task, Run) = f
            .hub
            .read(|r| Ok((r.require("task", task_id)?, r.require("run", run_id)?)))
            .unwrap();
        if task.state == TaskState::Completed && !crate::runs::still_finishing(&run) {
            break task;
        }
        assert!(wait(), "the run did not finish");
        tokio::time::sleep(Duration::from_millis(1)).await;
    };
    let mut command = CommandEnvelope::new(Command::QueueContinuation {
        task_id,
        text: "Continue using the recording".into(),
        bundle: None,
        authorization: QueueAuthorization::AskBeforeStart,
    });
    command.expected_revision = Some(task.revision);
    let receipt = f.hub.submit(&Caller::local_owner(), command).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    if !size_pressure {
        f.clock.advance(KEPT_FOR);
    }
    f.hub.collect_scratch().await.unwrap();
    let queue: QueueView = f.hub.read(|r| r.require("queue", task.project_id)).unwrap();
    let item = queue
        .items
        .iter()
        .find(|i| i.task_id == task_id && i.is_pending())
        .unwrap();
    assert!(recording.exists(), "Next up still needs the recording");

    let mut command = CommandEnvelope::new(Command::QueueRemove {
        item_id: item.item_id,
    });
    command.expected_revision = Some(queue.revision);
    let receipt = f.hub.submit(&Caller::local_owner(), command).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    // Removing the queued message updates the task's retention timestamp.
    if !size_pressure {
        f.clock.advance(KEPT_FOR);
    }
    f.hub.collect_scratch().await.unwrap();
    assert!(
        !scratch.exists(),
        "removed continuations no longer protect scratch"
    );
    f.close().await;
}

#[tokio::test]
async fn an_agent_shows_the_person_a_picture_from_its_scratch_folder() {
    let f = Fixture::new().await;
    let (task, _, binding) = bound(&f).await;
    let scratch = f.hub.scratch_dir(task);
    std::fs::create_dir_all(scratch.join("shots")).unwrap();
    std::fs::write(scratch.join("shots/home.png"), PNG).unwrap();
    let said = person(&f, &binding, show(&scratch.join("shots/home.png")))
        .await
        .unwrap();
    assert!(
        said.starts_with("Shown to the person in Chat: shots/home.png (image/png)."),
        "{said}"
    );
    // A relative path is the project's, which hasn't that file.
    person(&f, &binding, show(Path::new("shots/home.png")))
        .await
        .unwrap_err();
    // Another task's scratch folder isn't this one's, however it's reached.
    let other = scratch.with_file_name("0123456789ab");
    std::fs::create_dir_all(other.join("shots")).unwrap();
    std::fs::write(other.join("shots/home.png"), PNG).unwrap();
    for path in [
        other.join("shots/home.png"),
        scratch.join("../0123456789ab/shots/home.png"),
    ] {
        let refused = person(&f, &binding, show(&path)).await.unwrap_err();
        assert!(refused.contains("your scratch folder"), "{refused}");
    }
    f.close().await;
}

#[test]
fn a_sweep_keeps_what_tasks_still_need() {
    let root = tempfile::tempdir().unwrap();
    let now = 10 * KEPT_FOR.as_millis() as i64;
    let tasks = HashMap::from([
        ("running".to_owned(), Need::Running),
        ("recent".to_owned(), Need::Finished(now - 1000)),
        (
            "old".to_owned(),
            Need::Finished(now - KEPT_FOR.as_millis() as i64),
        ),
        ("archived".to_owned(), Need::Done),
    ]);
    for name in ["running", "recent", "old", "archived", "unknown"] {
        filled(&root.path().join(name), 16);
    }
    let kept = || {
        let mut names: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    let wall = SystemTime::now();
    sweep(root.path(), &tasks, now, wall, u64::MAX).unwrap();
    // A folder no task is known for may be a task's made since they were read.
    assert_eq!(kept(), ["recent", "running", "unknown"]);
    sweep(
        root.path(),
        &tasks,
        now,
        wall + Duration::from_secs(2 * 60 * 60),
        u64::MAX,
    )
    .unwrap();
    assert_eq!(kept(), ["recent", "running"]);
    // Nothing made yet.
    sweep(&root.path().join("none"), &tasks, now, wall, 0).unwrap();
}

#[test]
fn past_the_limit_the_longest_finished_go_first() {
    let root = tempfile::tempdir().unwrap();
    let tasks = HashMap::from([
        ("running".to_owned(), Need::Running),
        ("first".to_owned(), Need::Finished(1)),
        ("second".to_owned(), Need::Finished(2)),
        ("third".to_owned(), Need::Finished(3)),
    ]);
    for name in ["running", "first", "second", "third"] {
        filled(&root.path().join(name), 1000);
    }
    sweep(root.path(), &tasks, 4, SystemTime::now(), 2500).unwrap();
    for (name, kept) in [
        ("running", true),
        ("first", false),
        ("second", false),
        ("third", true),
    ] {
        assert_eq!(root.path().join(name).exists(), kept, "{name}");
    }
    // A running task keeps its folder whatever its size.
    sweep(root.path(), &tasks, 4, SystemTime::now(), 0).unwrap();
    assert!(root.path().join("running").exists());
    assert!(!root.path().join("third").exists());
}

#[cfg(unix)]
#[test]
fn a_sweep_removes_links_not_what_they_reach_and_read_only_folders() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep.txt"), b"keep").unwrap();
    let done = root.path().join("done");
    std::fs::create_dir_all(done.join("cache/inner")).unwrap();
    std::fs::write(done.join("cache/inner/file"), b"x").unwrap();
    std::os::unix::fs::symlink(outside.path(), done.join("project")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("keep.txt"), done.join("keep.txt")).unwrap();
    for dir in ["cache/inner", "cache"] {
        std::fs::set_permissions(done.join(dir), std::fs::Permissions::from_mode(0o500)).unwrap();
    }
    let tasks = HashMap::from([("done".to_owned(), Need::Done)]);
    sweep(root.path(), &tasks, 0, SystemTime::now(), u64::MAX).unwrap();
    assert!(!done.exists());
    assert_eq!(
        std::fs::read(outside.path().join("keep.txt")).unwrap(),
        b"keep"
    );
}

#[test]
fn scratch_folders_are_named_for_the_random_end_of_the_task_id() {
    let task = TaskId::new();
    let name = name(task);
    assert_eq!(name.len(), 12);
    assert!(task.as_uuid().simple().to_string().ends_with(&name));
}

#[test]
fn a_short_scratch_folder_is_also_the_temporary_folder() {
    let mut env = ports::RunEnv::default();
    scratch_env(
        "/home/sam/.local/share/ophio/db/scratch/0123456789ab",
        &mut env,
    );
    let names = |env: &ports::RunEnv| env.0.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
    #[cfg(unix)]
    assert_eq!(names(&env), ["TMPDIR", ports::SCRATCH_ENV]);
    #[cfg(windows)]
    assert_eq!(names(&env), ["TEMP", "TMP", ports::SCRATCH_ENV]);
    // Too long for the sockets programs put in it.
    #[cfg(unix)]
    {
        let mut env = ports::RunEnv::default();
        scratch_env(
            &format!("/{}/scratch/0123456789ab", "x".repeat(60)),
            &mut env,
        );
        assert_eq!(names(&env), [ports::SCRATCH_ENV]);
    }
}
