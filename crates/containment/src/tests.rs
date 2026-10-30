// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]
use super::*;
use std::{path::Path, process::Stdio};
use test_support::detached;

/// A stand-in agent that backgrounds a child in each way, then keeps running
/// unless `exit` is set.
async fn agent(container: &Container, dir: &Path, exit: bool) -> (Child, Vec<(&'static str, u32)>) {
    std::fs::write(dir.join("detach.sh"), detached::SCRIPT).unwrap();
    let mut command = Command::new("sh");
    command
        .current_dir(dir)
        .args([
            "-c",
            if exit {
                "sh detach.sh"
            } else {
                "sh detach.sh; exec sleep 300"
            },
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = container.spawn(&mut command).await.unwrap();
    let children = detached::children(dir);
    for (way, pid) in &children {
        assert!(detached::running(*pid), "{way} child isn't running");
    }
    (child, children)
}

fn assert_ended(container: &Container, children: &[(&str, u32)]) {
    detached::assert_ended(container.containment() == Containment::Complete, children);
}

#[tokio::test]
async fn ending_a_running_agent_ends_every_child_however_it_left() {
    let dir = tempfile::tempdir().unwrap();
    let container = Container::new().unwrap();
    println!("containment here: {:?}", container.containment());
    let (mut child, children) = agent(&container, dir.path(), false).await;
    // As a run's task does: kill, confirm the end, and only then reap the
    // agent.
    container.kill().unwrap();
    container.end().await.unwrap();
    assert!(child.wait().await.is_ok());
    assert_ended(&container, &children);
    // Ending again confirms again.
    container.end().await.unwrap();
}

#[tokio::test]
async fn the_end_of_an_agent_that_exited_still_ends_what_it_left_running() {
    let dir = tempfile::tempdir().unwrap();
    let container = Container::new().unwrap();
    let (mut child, children) = agent(&container, dir.path(), true).await;
    // The agent has exited, and is reaped only once the end is confirmed.
    let pid = child.id().unwrap();
    while detached::running(pid) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    for (way, pid) in &children {
        assert!(detached::running(*pid), "{way} child ended with its parent");
    }
    container.end().await.unwrap();
    assert!(child.wait().await.unwrap().success());
    assert_ended(&container, &children);
}

#[tokio::test]
async fn a_cancelled_run_leaves_nothing_running() {
    let dir = tempfile::tempdir().unwrap();
    let container = Arc::new(Container::new().unwrap());
    let (mut child, children) = agent(&container, dir.path(), false).await;
    let guard = Guard::new(container.clone());
    let task = tokio::spawn(async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    });
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(child.wait().await.is_ok());
    assert_ended(&container, &children);
}

#[tokio::test]
async fn a_process_group_alone_misses_a_new_session_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let container = Container::process_group("fixture reason");
    assert_eq!(
        container.containment(),
        Containment::ProcessGroup {
            reason: "fixture reason".into()
        }
    );
    let (mut child, children) = agent(&container, dir.path(), false).await;
    container.kill().unwrap();
    container.end().await.unwrap();
    assert!(child.wait().await.is_ok());
    assert_ended(&container, &children);
}

#[tokio::test]
async fn a_process_group_is_confirmed_ended_while_its_agent_is_left_unreaped() {
    // A run's task reaps the agent only once the end is confirmed, so the
    // group's id stays the run's own until nothing is left to signal.
    let dir = tempfile::tempdir().unwrap();
    let container = Container::process_group("fixture reason");
    let (mut child, children) = agent(&container, dir.path(), false).await;
    container.end().await.unwrap();
    assert_ended(&container, &children);
    let pid = child.id().unwrap();
    assert!(!detached::running(pid), "the agent is still running");
    assert!(
        Path::new(&format!("/proc/{pid}")).exists(),
        "the agent was reaped"
    );
    assert!(child.wait().await.is_ok());
}

#[tokio::test]
async fn a_process_group_whose_id_was_taken_by_another_is_never_signalled() {
    use rustix::process::{Pid, PidfdFlags, pidfd_open};
    let container = Arc::new(Container::process_group("fixture reason"));
    // An agent reaped before its group was ended, and another program's
    // group now under its id.
    let mut reaped = Command::new("true").spawn().unwrap();
    let pid = Pid::from_raw(reaped.id().unwrap().try_into().unwrap()).unwrap();
    let pidfd = pidfd_open(pid, PidfdFlags::empty()).unwrap();
    assert!(reaped.wait().await.unwrap().success());
    let mut other = Command::new("sleep");
    let mut other = other
        .arg("300")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let id = Pid::from_raw(other.id().unwrap().try_into().unwrap()).unwrap();
    container.as_if_reused(id, pidfd);
    assert!(container.kill().is_err());
    assert!(container.end().await.is_err());
    drop(Guard::new(container.clone()));
    assert!(
        other.try_wait().unwrap().is_none(),
        "another program's group was signalled"
    );
    other.kill().await.unwrap();
}

#[tokio::test]
async fn a_cgroup_is_removed_once_its_run_has_ended() {
    let container = Container::new().unwrap();
    let Inner::Cgroup(cgroup) = &container.inner else {
        println!("skipped: {:?}", container.containment());
        return;
    };
    let dir = cgroup.dir().to_owned();
    assert!(dir.is_dir());
    container.end().await.unwrap();
    assert!(!dir.exists());
}

#[tokio::test]
async fn a_cgroup_is_removed_while_its_agent_is_left_unreaped() {
    let dir = tempfile::tempdir().unwrap();
    let container = Container::new().unwrap();
    let Inner::Cgroup(cgroup) = &container.inner else {
        println!("skipped: {:?}", container.containment());
        return;
    };
    let cgroup = cgroup.dir().to_owned();
    let (mut child, children) = agent(&container, dir.path(), false).await;
    // An agent that has exited but is not yet reaped leaves the cgroup
    // empty, so its end is confirmed before it is reaped.
    container.end().await.unwrap();
    assert!(!cgroup.exists());
    assert_ended(&container, &children);
    let pid = child.id().unwrap();
    assert!(
        Path::new(&format!("/proc/{pid}")).exists(),
        "the agent was reaped"
    );
    assert!(child.wait().await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn an_unconfirmed_end_is_tried_again_until_it_is_confirmed() {
    let container = hooks::unconfirmed(3, async { Container::new().unwrap() }).await;
    let guard = Guard::new(Arc::new(container));
    assert!(guard.end().await.is_err());
    let started = tokio::time::Instant::now();
    guard.confirm_end().await;
    // Two more unconfirmed tries, one second then two apart, and the third
    // four seconds later is confirmed.
    assert_eq!(started.elapsed().as_secs(), 7);
    assert!(guard.end().await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn a_stop_has_an_unconfirmed_end_tried_again_at_once() {
    let container = hooks::unconfirmed(1, hooks::held(async { Container::new().unwrap() })).await;
    let container = Arc::new(container);
    let guard = Guard::new(container.clone());
    assert!(guard.end().await.is_err());
    let confirmed = tokio::spawn(async move { guard.confirm_end().await });
    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
    assert!(!confirmed.is_finished());
    let stopped = tokio::time::Instant::now();
    container.retry_now();
    confirmed.await.unwrap();
    assert!(stopped.elapsed() < std::time::Duration::from_secs(1));
    assert!(container.end().await.is_ok());
}

#[tokio::test]
async fn an_agent_whose_name_is_not_text_is_still_seen_to_end() {
    use std::os::unix::ffi::OsStrExt;
    // A program's name is whatever bytes it was started under.
    let dir = tempfile::tempdir().unwrap();
    let named = dir.path().join(std::ffi::OsStr::from_bytes(b"\xffagent"));
    std::os::unix::fs::symlink("/bin/sh", &named).unwrap();
    let container = Container::process_group("fixture reason");
    let mut command = Command::new(&named);
    command.arg0("sh").args(["-c", "exit 0"]);
    let mut child = container.spawn(&mut command).await.unwrap();
    let pid = child.id().unwrap();
    while detached::running(pid) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let stat = std::fs::read(format!("/proc/{pid}/stat")).unwrap();
    assert!(std::str::from_utf8(&stat).is_err(), "the name is text");
    container.end().await.unwrap();
    assert!(child.wait().await.unwrap().success());
}

/// The device of this computer's /proc, as major and minor numbers.
fn proc_device() -> (u32, u32) {
    use std::os::unix::fs::MetadataExt;
    let dev = std::fs::metadata("/proc").unwrap().dev();
    (rustix::fs::major(dev), rustix::fs::minor(dev))
}

/// The line the mount table gives this computer's /proc, with `options`.
fn proc_line(options: &str) -> String {
    let (major, minor) = proc_device();
    format!("22 1 {major}:{minor} / /proc rw,nosuid shared:5 - proc proc rw{options}\n")
}

#[tokio::test]
async fn a_process_group_is_not_confirmed_ended_where_proc_hides_processes() {
    // Where /proc shows the companion only some processes, a group with none
    // it can see left may still have members.
    let dir = tempfile::tempdir().unwrap();
    let mounts = dir.path().join("mountinfo");
    std::fs::write(&mounts, proc_line(",hidepid=invisible")).unwrap();
    // The companion's user namespace, first the initial one.
    let user_namespace = dir.path().join("user");
    let in_namespace = |number: u32| {
        let _ = std::fs::remove_file(&user_namespace);
        std::os::unix::fs::symlink(format!("user:[{number}]"), &user_namespace).unwrap();
    };
    in_namespace(4026531837);
    let container = Container::process_group("fixture reason");
    let Inner::Group(group, _) = &container.inner else {
        unreachable!()
    };
    group.mounts.set(mounts.clone()).unwrap();
    group.user_namespace.set(user_namespace.clone()).unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", "exit 0"]);
    let mut child = container.spawn(&mut command).await.unwrap();
    let pid = child.id().unwrap();
    while detached::running(pid) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(container.end().await.is_err());
    // Unless the companion is in the group /proc hides nothing from.
    let gid = rustix::process::getegid().as_raw();
    std::fs::write(&mounts, proc_line(&format!(",hidepid=invisible,gid={gid}"))).unwrap();
    // The option numbers that group as the initial user namespace does, and
    // in another the same number can be another group.
    in_namespace(4026532001);
    assert!(container.end().await.is_err());
    in_namespace(4026531837);
    container.end().await.unwrap();
    assert!(child.wait().await.unwrap().success());
}

#[tokio::test]
async fn a_process_group_is_not_confirmed_ended_where_proc_numbers_processes_differently() {
    // A /proc made for an enclosing PID namespace gives every process, the
    // agent's group included, that namespace's numbers, so a group with none
    // it can see left may still have members.
    let dir = tempfile::tempdir().unwrap();
    let status = dir.path().join("status");
    let own = std::fs::read_to_string("/proc/self/status").unwrap();
    let with_ids = |ids: &str| {
        own.lines()
            .map(|line| {
                if line.starts_with("NSpid:") {
                    format!("NSpid:\t{ids}\n")
                } else {
                    format!("{line}\n")
                }
            })
            .collect::<String>()
    };
    let pid = std::process::id();
    std::fs::write(&status, with_ids(&format!("{}\t{pid}", pid + 4096))).unwrap();
    let container = Container::process_group("fixture reason");
    let Inner::Group(group, _) = &container.inner else {
        unreachable!()
    };
    group.status.set(status.clone()).unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", "exit 0"]);
    let mut child = container.spawn(&mut command).await.unwrap();
    let child_pid = child.id().unwrap();
    while detached::running(child_pid) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(container.end().await.is_err());
    // A /proc made for the companion's own namespace numbers them as it does.
    std::fs::write(&status, with_ids(&pid.to_string())).unwrap();
    container.end().await.unwrap();
    assert!(child.wait().await.unwrap().success());
}

#[tokio::test]
async fn a_mount_whose_path_is_not_text_does_not_keep_an_end_unconfirmed() {
    // A mount point is whatever bytes it was made at.
    let dir = tempfile::tempdir().unwrap();
    let mounts = dir.path().join("mountinfo");
    std::fs::write(
        &mounts,
        [
            proc_line("").as_bytes(),
            b"40 1 0:50 / /mnt/\xffdisk rw shared:9 - tmpfs tmpfs rw\n",
        ]
        .concat(),
    )
    .unwrap();
    let container = Container::process_group("fixture reason");
    let Inner::Group(group, _) = &container.inner else {
        unreachable!()
    };
    group.mounts.set(mounts).unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", "exit 0"]);
    let mut child = container.spawn(&mut command).await.unwrap();
    let pid = child.id().unwrap();
    while detached::running(pid) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    container.end().await.unwrap();
    assert!(child.wait().await.unwrap().success());
}

#[tokio::test]
async fn only_the_proc_that_is_read_says_what_it_hides() {
    // Another proc in the table, even one on top of /proc there, is not the
    // one whose processes are read.
    let dir = tempfile::tempdir().unwrap();
    let mounts = dir.path().join("mountinfo");
    let (major, minor) = proc_device();
    let other = |options: &str| {
        format!(
            "40 22 {major}:{} / /proc rw shared:9 - proc proc rw{options}\n",
            minor + 1
        )
    };
    std::fs::write(&mounts, proc_line("") + &other(",hidepid=invisible")).unwrap();
    let container = Container::process_group("fixture reason");
    let Inner::Group(group, _) = &container.inner else {
        unreachable!()
    };
    group.mounts.set(mounts.clone()).unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", "exit 0"]);
    let mut child = container.spawn(&mut command).await.unwrap();
    let pid = child.id().unwrap();
    while detached::running(pid) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    std::fs::write(&mounts, other("")).unwrap();
    assert!(container.end().await.is_err());
    std::fs::write(&mounts, proc_line(",hidepid=invisible") + &other("")).unwrap();
    assert!(container.end().await.is_err());
    std::fs::write(&mounts, proc_line("") + &other(",hidepid=invisible")).unwrap();
    container.end().await.unwrap();
    assert!(child.wait().await.unwrap().success());
}

#[tokio::test]
#[allow(unsafe_code)]
async fn an_end_is_not_confirmed_while_the_agent_itself_runs() {
    // An agent may join another process group of its session, leaving its
    // own empty, and one that was killed may not have exited yet.
    let container = Container::process_group("fixture reason");
    let elsewhere = rustix::process::getpgrp();
    let mut command = Command::new("sleep");
    command.arg("300").kill_on_drop(true);
    // SAFETY: between fork and exec the new process only makes one
    // setpgid(2) call, which is async-signal-safe.
    unsafe {
        command.pre_exec(move || {
            rustix::process::setpgid(None, Some(elsewhere))?;
            Ok(())
        });
    }
    let mut child = container.spawn(&mut command).await.unwrap();
    assert!(container.end().await.is_err());
    // As what drives a run kills the agent itself.
    child.start_kill().unwrap();
    container.end().await.unwrap();
    assert!(child.wait().await.is_ok());
}

#[test]
fn a_terminals_program_starts_inside_its_group_which_ends_nothing() {
    let Ok(group) = TerminalGroup::new() else {
        println!("skipped: no cgroup for a terminal here");
        return;
    };
    let dir = group.0.dir().to_owned();
    // The program is inside from its first instruction, and so is what it
    // leaves running at once. It makes groups of its own inside: one it
    // leaves something running in, and others it leaves empty.
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg(
        "cat /proc/$$/cgroup
         mkdir \"$0/inner\" \"$0/empty\" \"$0/empty/deeper\" || exit
         sleep 300 </dev/null >/dev/null 2>&1 &
         echo $! >\"$0/inner/cgroup.procs\" && echo $!",
    );
    command.arg(&dir);
    group.prepare(&mut command).unwrap();
    let output = command.output().unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    let mut lines = output.lines();
    let started_in = lines.next().unwrap().strip_prefix("0::").unwrap();
    assert_eq!(Path::new(started_in).file_name(), dir.file_name());
    let sleeper: u32 = lines.next().unwrap().parse().unwrap();
    let procs = std::fs::read_to_string(dir.join("inner/cgroup.procs")).unwrap();
    assert!(procs.lines().any(|pid| pid == sleeper.to_string()));
    // What the terminal left running keeps its group; the empty groups go.
    drop(group);
    assert!(detached::running(sleeper));
    assert!(dir.join("inner").is_dir());
    assert!(!dir.join("empty").exists());
    std::fs::write(dir.join("cgroup.kill"), b"1").unwrap();
    assert!(detached::ends(sleeper));
    // Once that has ended, the group and the one it ran in are removed
    // before another is made. The kernel empties a killed group a moment
    // after its last process ends.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while dir.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "an emptied group was kept"
        );
        drop(TerminalGroup::new().unwrap());
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn a_program_that_cannot_join_a_terminals_group_never_runs() {
    let dir = tempfile::tempdir().unwrap();
    let ran = dir.path().join("ran");
    let group = TerminalGroup(cgroup::Cgroup::unjoinable(dir.path().join("group")));
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg("touch \"$0\"").arg(&ran);
    group.prepare(&mut command).unwrap();
    assert!(command.status().is_err());
    assert!(!ran.exists());
}

#[test]
fn groups_a_companion_no_longer_running_left_empty_are_removed() {
    let Ok(group) = TerminalGroup::new() else {
        println!("skipped: no cgroup for a terminal here");
        return;
    };
    let own = group.0.dir().parent().unwrap().to_owned();
    drop(group);
    let mut ended = std::process::Command::new("true").spawn().unwrap();
    ended.wait().unwrap();
    let mut running = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let left = |companion: u32| own.join(format!("{}0", ports::run_cgroup_prefix(companion)));
    std::fs::create_dir_all(left(ended.id()).join("inner")).unwrap();
    std::fs::create_dir(left(running.id())).unwrap();
    drop(TerminalGroup::new().unwrap());
    let kept = left(running.id()).exists();
    let _ = std::fs::remove_dir(left(running.id()));
    running.kill().unwrap();
    running.wait().unwrap();
    assert!(!left(ended.id()).exists());
    assert!(kept, "the group of a companion still running was removed");
}
