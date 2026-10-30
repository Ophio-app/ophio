// SPDX-License-Identifier: Apache-2.0
//! The agent's own process group, where nothing wider is available. A process
//! that starts a new session with `setsid` leaves it.
//!
//! The group is signalled by number, its leader's id, and that number is the
//! run's own only while the leader is unreaped: an agent that has exited stays
//! a zombie, keeping its id, until the companion reaps it. So what drives a run
//! reaps the agent only once [`Group::end`] has confirmed the group ended, and
//! from then on the group is never signalled. Each signal is sent under the
//! lock that records that end, so none can follow it. On Linux the leader's
//! pidfd also says when it has been reaped anyway, as it may be when a run's
//! task is dropped along with it, and then nothing is sent.
//!
//! An end is confirmed once only zombies are left in the group, the unreaped
//! leader among them. Where `/proc`'s `hidepid` option, or a mount over a
//! process's entry, hides some processes from the companion, one of them may
//! still be in the group, so there an end is not confirmed while the leader is
//! left unreaped. Nor is it where `/proc`
//! was made for another PID namespace, whose numbers aren't the companion's.
//! `/proc` is opened once for each look, and all of it is read through that
//! one handle without crossing a mount, so none made meanwhile can change what
//! is seen. A remount can still change what that one shows, so a look that
//! finds none left reads its options again and counts only if they held.
use crate::{END_WITHIN, leader::Leader, unconfirmed};
use ports::HarnessError;
use rustix::{
    io::Errno,
    process::{Pid, Signal},
};
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;
use std::{
    path::Path,
    sync::{Mutex, PoisonError},
    time::Duration,
};

#[derive(Debug, Default)]
pub(crate) struct Group {
    /// Set once the group is confirmed ended, after which its leader may be
    /// reaped and its id reused: it is never signalled again. Held while
    /// signalling.
    ended: Mutex<bool>,
    /// A mount table a test gives in place of the companion's own.
    #[cfg(test)]
    pub(crate) mounts: std::sync::OnceLock<std::path::PathBuf>,
    /// A `/proc` status a test gives in place of the companion's own.
    #[cfg(test)]
    pub(crate) status: std::sync::OnceLock<std::path::PathBuf>,
    /// A user namespace link a test gives in place of the companion's own.
    #[cfg(test)]
    pub(crate) user_namespace: std::sync::OnceLock<std::path::PathBuf>,
}

impl Group {
    pub(crate) fn kill(&self, leader: Option<&Leader>) -> Result<(), HarnessError> {
        let ended = self.ended.lock().unwrap_or_else(PoisonError::into_inner);
        match leader {
            Some(leader) if !*ended => {
                if !leader.unreaped() {
                    return Err(unconfirmed(
                        "The agent was reaped before its process group was ended",
                    ));
                }
                match rustix::process::kill_process_group(leader.pid, Signal::KILL) {
                    Ok(()) | Err(Errno::SRCH) => Ok(()),
                    Err(e) => Err(unconfirmed(format!(
                        "Ending the agent's process group: {}",
                        std::io::Error::from(e)
                    ))),
                }
            }
            _ => Ok(()),
        }
    }

    pub(crate) async fn end(&self, leader: Option<&Leader>) -> Result<(), HarnessError> {
        let Some(leader) = leader else {
            return Ok(());
        };
        let mut killed = self.kill(Some(leader)).err();
        let deadline = tokio::time::Instant::now() + END_WITHIN;
        loop {
            let live = {
                let mut ended = self.ended.lock().unwrap_or_else(PoisonError::into_inner);
                if *ended {
                    return Ok(());
                }
                let live = live_members(
                    leader.pid,
                    self.mounts(),
                    self.status(),
                    self.user_namespace(),
                );
                if matches!(live, Ok(false)) {
                    *ended = true;
                    return Ok(());
                }
                live
            };
            if tokio::time::Instant::now() >= deadline {
                return Err(killed.take().unwrap_or_else(|| match live {
                    Err(e) => unconfirmed(format!("Looking for the agent's processes: {e}")),
                    Ok(_) => unconfirmed("The agent's process group still has processes"),
                }));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Group {
    /// The mount table a test gives, where `/proc`'s own is otherwise read.
    fn mounts(&self) -> Option<&Path> {
        #[cfg(test)]
        if let Some(mounts) = self.mounts.get() {
            return Some(mounts);
        }
        None
    }

    /// The status a test gives, where the companion's own in `/proc` is
    /// otherwise read.
    fn status(&self) -> Option<&Path> {
        #[cfg(test)]
        if let Some(status) = self.status.get() {
            return Some(status);
        }
        None
    }

    /// The user namespace link a test gives, where the companion's own in
    /// `/proc` is otherwise read.
    fn user_namespace(&self) -> Option<&Path> {
        #[cfg(test)]
        if let Some(user_namespace) = self.user_namespace.get() {
            return Some(user_namespace);
        }
        None
    }
}

/// The inode number procfs gives its root (`PROC_ROOT_INO` in
/// `include/linux/proc_ns.h`).
#[cfg(target_os = "linux")]
const PROC_ROOT_INO: u64 = 1;

/// Whether the group `pgid` has a member that is not a zombie. `/proc` is
/// opened once, and everything is read through that one handle without
/// crossing a mount, so a mount made at any moment can't show the scan other
/// than what was checked. Tests may give the `mounts` table, the companion's
/// `status` and its `user_namespace` link in place of those it reads there.
#[cfg(target_os = "linux")]
fn live_members(
    pgid: Pid,
    mounts: Option<&Path>,
    status: Option<&Path>,
    user_namespace: Option<&Path>,
) -> std::io::Result<bool> {
    use rustix::fs::{Mode, OFlags};
    match rustix::process::test_kill_process_group(pgid) {
        Err(Errno::SRCH) => return Ok(false),
        Err(e) => return Err(e.into()),
        Ok(()) => {}
    }
    let proc = rustix::fs::open(
        "/proc",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let read = |given: Option<&Path>, own| match given {
        Some(path) => std::fs::read(path),
        None => read_beneath(&proc, own),
    };
    let root = rustix::fs::fstat(&proc)?;
    let namespace = || match user_namespace {
        Some(link) => {
            std::fs::read_link(link).map(|target| target.into_os_string().into_encoded_bytes())
        }
        None => read_link_beneath(&proc, "self/ns/user"),
    };
    let member = |gid| in_group(gid) && in_initial_user_namespace(namespace());
    let mountinfo = read(mounts, "self/mountinfo")?;
    // Only the root of a proc shows every process in it.
    if root.st_ino != PROC_ROOT_INO || hides(&mountinfo, root.st_dev, member) {
        return Err(std::io::Error::other(
            "this computer’s /proc hides some processes from Øphio, so it can’t see whether any are left",
        ));
    }
    if !numbers_as_companion(&read(status, "self/status")?, rustix::process::getpid()) {
        return Err(std::io::Error::other(
            "this computer’s /proc numbers processes differently from Øphio, so it can’t tell which are the agent’s",
        ));
    }
    members_left_as_shown(&proc, pgid, root.st_dev, &mountinfo, || {
        read(mounts, "self/mountinfo")
    })
}

/// Whether the group `pgid` has a member that is not a zombie, as the proc
/// open as `proc`, on the device `dev`, shows it with the options it had in
/// the mount table `before`. A remount can change them at any moment, and
/// one that hid a member during the scan would let it seem gone. So none is
/// found gone unless the table read by `now` afterwards gives the same
/// options. A change undone within one scan can't be seen from here.
#[cfg(target_os = "linux")]
fn members_left_as_shown(
    proc: &OwnedFd,
    pgid: Pid,
    dev: rustix::fs::Dev,
    before: &[u8],
    now: impl FnOnce() -> std::io::Result<Vec<u8>>,
) -> std::io::Result<bool> {
    if members_left(proc, pgid)? {
        return Ok(true);
    }
    if proc_options(&now()?, dev) != proc_options(before, dev) {
        return Err(std::io::Error::other(
            "this computer’s /proc changed what it shows Øphio while it looked, so it can’t see whether any processes are left",
        ));
    }
    Ok(false)
}

/// Whether the group `pgid` has a member that is not a zombie, as the proc
/// open as `proc` shows it.
#[cfg(target_os = "linux")]
fn members_left(proc: &OwnedFd, pgid: Pid) -> std::io::Result<bool> {
    use rustix::fs::{Dir, OFlags};
    let gone = |e: &std::io::Error| {
        e.kind() == std::io::ErrorKind::NotFound
            || e.raw_os_error() == Some(Errno::SRCH.raw_os_error())
    };
    let pgid = pgid.as_raw_nonzero().to_string();
    for entry in Dir::read_from(proc)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name.is_empty() || !name.iter().all(u8::is_ascii_digit) {
            continue;
        }
        let name = String::from_utf8_lossy(name);
        let stat = match read_beneath(proc, &format!("{name}/stat")) {
            Ok(stat) => stat,
            Err(e) if gone(&e) => continue,
            Err(e) => return Err(e),
        };
        let Some((state, group)) = state_and_group(&stat) else {
            continue;
        };
        if group != pgid.as_bytes() {
            continue;
        }
        if !matches!(state, b"Z" | b"X") {
            return Ok(true);
        }
        // A leader whose own thread exited while its others still run.
        match open_beneath(proc, &format!("{name}/task"), OFlags::DIRECTORY) {
            Ok(task) => {
                let mut threads = 0;
                for thread in Dir::read_from(&task)? {
                    if !matches!(thread?.file_name().to_bytes(), b"." | b"..") {
                        threads += 1;
                    }
                }
                if threads > 1 {
                    return Ok(true);
                }
            }
            Err(e) if gone(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

/// Opens `path` in the directory `dir`, read-only, refusing any mount on the
/// way, which could show something other than what `dir` holds.
#[cfg(target_os = "linux")]
fn open_beneath(dir: &OwnedFd, path: &str, flags: rustix::fs::OFlags) -> std::io::Result<OwnedFd> {
    use rustix::fs::{Mode, OFlags, ResolveFlags};
    rustix::fs::openat2(
        dir,
        path,
        flags | OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_XDEV | ResolveFlags::NO_MAGICLINKS,
    )
    .map_err(|e| match e {
        Errno::XDEV => std::io::Error::other(
            "a mount covers part of this computer’s /proc, so Øphio can’t see whether any processes are left",
        ),
        Errno::NOSYS => std::io::Error::other(
            "this Linux kernel can’t read /proc without following mounts; that needs 5.6 or later",
        ),
        e => e.into(),
    })
}

/// Reads the file at `path` in the directory `dir`, as [`open_beneath`] opens it.
#[cfg(target_os = "linux")]
fn read_beneath(dir: &OwnedFd, path: &str) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::from(open_beneath(dir, path, rustix::fs::OFlags::empty())?)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Reads the link at `path` in the directory `dir`, the link itself rather
/// than where it leads, as [`open_beneath`] opens it.
#[cfg(target_os = "linux")]
fn read_link_beneath(dir: &OwnedFd, path: &str) -> std::io::Result<Vec<u8>> {
    use rustix::fs::OFlags;
    let link = open_beneath(dir, path, OFlags::PATH | OFlags::NOFOLLOW)?;
    Ok(rustix::fs::readlinkat(&link, "", Vec::new())?.into_bytes())
}

/// A process's state and group from its `/proc` stat. The name before them,
/// in parentheses, may hold any bytes, even a parenthesis, and needn't be text.
#[cfg(target_os = "linux")]
fn state_and_group(stat: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = stat.iter().rposition(|b| *b == b')')?;
    let mut fields = stat[end + 1..]
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty());
    let state = fields.next()?;
    let _parent = fields.next()?;
    Some((state, fields.next()?))
}

/// Whether `/proc` numbers processes as the companion does, which it does
/// only if it was made for the companion's own PID namespace. The companion's
/// `status` there lists its id in each namespace from the one `/proc` was made
/// for down to its own. A Linux too old to list them is not trusted either.
#[cfg(target_os = "linux")]
fn numbers_as_companion(status: &[u8], own: Pid) -> bool {
    status
        .split(|b| *b == b'\n')
        .find_map(|line| line.strip_prefix(b"NSpid:"))
        .is_some_and(|ids| {
            let mut ids = ids
                .split(u8::is_ascii_whitespace)
                .filter(|id| !id.is_empty());
            ids.next() == Some(own.as_raw_nonzero().to_string().as_bytes()) && ids.next().is_none()
        })
}

/// Whether the proc on the device `dev` hides some processes from the
/// companion, as the mount table `mountinfo` gives its options. Its `hidepid`
/// option hides other users' processes, or those the companion may not trace.
/// Other users' are hidden from all but members of the group its `gid` option
/// names, as `member` says; what it may not trace is hidden from them too.
///
/// Those options are the proc's own, whichever of its mounts shows them, and
/// no other proc is on its device. The table's paths may hold any bytes, and
/// needn't be text.
#[cfg(target_os = "linux")]
fn hides(mountinfo: &[u8], dev: rustix::fs::Dev, member: impl Fn(u32) -> bool) -> bool {
    // Without them, nothing is known of what it hides.
    let Some(options) = proc_options(mountinfo, dev) else {
        return true;
    };
    let (mut level, mut exempt) = (&b"off"[..], false);
    for option in options.split(|b| *b == b',') {
        if let Some(hidepid) = option.strip_prefix(b"hidepid=") {
            level = hidepid;
        } else if let Some(gid) = option.strip_prefix(b"gid=") {
            exempt = std::str::from_utf8(gid)
                .ok()
                .and_then(|gid| gid.parse().ok())
                .is_some_and(&member);
        }
    }
    match level {
        b"0" | b"off" => false,
        b"4" | b"ptraceable" => true,
        _ => !exempt,
    }
}

/// The options of the proc on the device `dev`, as the mount table
/// `mountinfo` gives them, if it lists one there.
#[cfg(target_os = "linux")]
fn proc_options(mountinfo: &[u8], dev: rustix::fs::Dev) -> Option<&[u8]> {
    fn words(text: &[u8]) -> impl Iterator<Item = &[u8]> {
        text.split(|b| *b == b' ')
    }
    let opened = format!("{}:{}", rustix::fs::major(dev), rustix::fs::minor(dev));
    mountinfo.split(|b| *b == b'\n').find_map(|line| {
        let split = line.windows(3).position(|w| w == b" - ")?;
        (words(&line[..split]).nth(2)? == opened.as_bytes()).then_some(())?;
        let mut filesystem = words(&line[split + 3..]);
        (filesystem.next()? == b"proc").then_some(())?;
        filesystem.nth(1)
    })
}

/// Whether the companion is in the group numbered `gid` in its own user
/// namespace.
#[cfg(target_os = "linux")]
fn in_group(gid: u32) -> bool {
    use rustix::process::{getegid, getgroups};
    getegid().as_raw() == gid
        || getgroups().is_ok_and(|groups| groups.iter().any(|group| group.as_raw() == gid))
}

/// Whether the companion is in the initial user namespace, as its namespace
/// `link` in `/proc` says. `/proc`'s options number groups as that namespace
/// does, and another may number them otherwise, even one that maps each id to
/// itself inside a namespace that doesn't. The kernel gives the initial one a
/// fixed number. A link that can't be read says nothing, even on a Linux
/// without user namespaces, which has only that one.
#[cfg(target_os = "linux")]
fn in_initial_user_namespace(link: std::io::Result<Vec<u8>>) -> bool {
    link.is_ok_and(|target| target == b"user:[4026531837]")
}

/// Whether the group `pgid` has any member. Here a zombie cannot be told from
/// a live process, so while the run's task leaves its agent unreaped the end
/// is never confirmed and the run stays open, as for any unconfirmed end.
/// Ophio supports no such system; this only keeps it from ending wrongly.
#[cfg(not(target_os = "linux"))]
fn live_members(
    pgid: Pid,
    _mounts: Option<&Path>,
    _status: Option<&Path>,
    _user_namespace: Option<&Path>,
) -> std::io::Result<bool> {
    match rustix::process::test_kill_process_group(pgid) {
        Err(Errno::SRCH) => Ok(false),
        Err(e) => Err(e.into()),
        Ok(()) => Ok(true),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn state_and_group_are_read_past_any_name() {
        assert_eq!(
            state_and_group(b"12 (a) b) c) S 1 12 12 0 -1"),
            Some((&b"S"[..], &b"12"[..]))
        );
        assert_eq!(
            state_and_group(b"13 (\xff\xfe) Z 1 12 12 0 -1"),
            Some((&b"Z"[..], &b"12"[..]))
        );
        assert_eq!(state_and_group(b"14 (cut) S 1"), None);
    }

    #[test]
    fn proc_numbers_processes_as_the_companion_only_for_its_own_namespace() {
        let own = Pid::from_raw(7).unwrap();
        let status = |ids: &str| format!("Name:\tophio\nNSpid:\t{ids}\nNSpgid:\t7\n").into_bytes();
        assert!(numbers_as_companion(&status("7"), own));
        // Made for an enclosing namespace, where the companion has another id.
        assert!(!numbers_as_companion(&status("4031\t7"), own));
        assert!(!numbers_as_companion(&status("8"), own));
        assert!(!numbers_as_companion(b"Name:\tophio\nPid:\t7\n", own));
    }

    /// The device of the proc most fixtures describe.
    fn proc_device() -> rustix::fs::Dev {
        rustix::fs::makedev(0, 21)
    }

    #[test]
    fn proc_hides_processes_where_hidepid_is_set_for_the_companion() {
        let proc = |options: &str| {
            format!(
                "22 1 0:21 / /proc rw,nosuid shared:5 - proc proc rw{options}\n\
                 23 22 0:36 / /proc/sys/fs/binfmt_misc rw - binfmt_misc binfmt_misc rw\n"
            )
            .into_bytes()
        };
        let none = |_| false;
        assert!(!hides(&proc(""), proc_device(), none));
        for level in ["0", "off"] {
            assert!(!hides(
                &proc(&format!(",hidepid={level}")),
                proc_device(),
                none
            ));
        }
        for level in ["1", "noaccess", "2", "invisible", "4", "ptraceable"] {
            assert!(hides(
                &proc(&format!(",hidepid={level}")),
                proc_device(),
                none
            ));
        }
        assert!(!hides(
            &proc(",hidepid=invisible,gid=26"),
            proc_device(),
            |gid| gid == 26
        ));
        assert!(hides(
            &proc(",hidepid=invisible,gid=26"),
            proc_device(),
            |gid| gid == 27
        ));
        // Where /proc hides what the companion may not trace, it hides it from
        // that group too.
        for level in ["4", "ptraceable"] {
            let options = format!(",hidepid={level},gid=26");
            assert!(hides(&proc(&options), proc_device(), |gid| gid == 26));
        }
        assert!(hides(b"", proc_device(), none));
        // Other mounts' paths may not be text.
        let beside = b"40 1 0:50 / /mnt/\xffdisk rw - tmpfs tmpfs rw\n";
        assert!(!hides(
            &[proc(""), beside.to_vec()].concat(),
            proc_device(),
            none
        ));
        assert!(hides(
            &[proc(",hidepid=2"), beside.to_vec()].concat(),
            proc_device(),
            none
        ));
    }

    #[test]
    fn only_the_options_of_the_proc_that_was_opened_count() {
        let mount = |id: u32, parent: u32, kind: &str, options: &str| {
            format!("{id} {parent} 0:{id} / /proc rw shared:{id} - {kind} {kind} rw{options}\n")
                .into_bytes()
        };
        let device = |id| rustix::fs::makedev(0, id);
        let none = |_| false;
        // Whichever the table lists first, and whichever is on top.
        let (under, over) = (
            mount(22, 1, "proc", ",hidepid=2"),
            mount(40, 22, "proc", ""),
        );
        for table in [
            [under.clone(), over.clone()].concat(),
            [over, under].concat(),
        ] {
            assert!(!hides(&table, device(40), none));
            assert!(hides(&table, device(22), none));
        }
        // A proc shows its own options at each of its mounts, wherever made.
        let elsewhere = b"41 1 0:22 /4031 /mnt/\xffproc rw - proc proc rw,hidepid=2\n";
        assert!(hides(elsewhere, device(22), none));
        assert!(!hides(
            &[mount(22, 1, "proc", ""), elsewhere.to_vec()].concat(),
            device(22),
            none
        ));
        // Nothing else on a device is a proc, and a device none is on is
        // not known.
        assert!(hides(&mount(40, 22, "tmpfs", ""), device(40), none));
        assert!(hides(&mount(22, 1, "proc", ""), device(40), none));
        // What covers an entry of a proc that is itself covered hides nothing,
        // and what covers an entry of the one opened is met as it is read.
        let old = [
            mount(22, 1, "proc", ""),
            b"30 22 0:50 / /proc/4031 rw shared:9 - tmpfs tmpfs rw\n".to_vec(),
        ]
        .concat();
        assert!(!hides(
            &[old, mount(40, 22, "proc", "")].concat(),
            device(40),
            none
        ));
        assert!(!hides(
            b"40 22 0:40 / /proc rw - proc proc rw\n\
              41 40 0:50 / /proc/4031 rw - tmpfs tmpfs rw\n",
            device(40),
            none
        ));
    }

    #[test]
    fn what_proc_shows_is_read_without_crossing_a_mount() {
        use rustix::fs::{Mode, OFlags};
        let open = |path| {
            rustix::fs::open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .unwrap()
        };
        // /proc is always a mount of its own.
        let error = read_beneath(&open("/"), "proc/self/stat").unwrap_err();
        assert!(error.to_string().contains("a mount covers"), "{error}");
        assert!(
            read_beneath(&open("/proc"), "self/stat")
                .unwrap()
                .starts_with(format!("{} (", std::process::id()).as_bytes())
        );
        // A link is read as itself, even one that leads out of /proc.
        let error = read_link_beneath(&open("/"), "proc/self/ns/user").unwrap_err();
        assert!(error.to_string().contains("a mount covers"), "{error}");
        let own = std::fs::read_link("/proc/self/ns/user").unwrap();
        assert_eq!(
            read_link_beneath(&open("/proc"), "self/ns/user").unwrap(),
            own.into_os_string().into_encoded_bytes()
        );
    }

    #[test]
    fn a_group_is_left_while_proc_shows_a_member_running() {
        use rustix::fs::{Mode, OFlags};
        let dir = tempfile::tempdir().unwrap();
        let process = |pid: u32, state: &str, group: u32, threads: &[u32]| {
            let entry = dir.path().join(pid.to_string());
            std::fs::create_dir_all(entry.join("task")).unwrap();
            std::fs::write(
                entry.join("stat"),
                format!("{pid} (a) b) {state} 1 {group} {group} 0 -1"),
            )
            .unwrap();
            for thread in threads {
                std::fs::create_dir(entry.join("task").join(thread.to_string())).unwrap();
            }
        };
        let left = |pgid| {
            let proc = rustix::fs::open(
                dir.path(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .unwrap();
            members_left(&proc, Pid::from_raw(pgid).unwrap())
        };
        // Other groups and entries that are no process are passed over.
        process(9, "S", 8, &[9]);
        std::fs::create_dir(dir.path().join("self")).unwrap();
        std::fs::create_dir(dir.path().join("12x")).unwrap();
        std::fs::write(dir.path().join("uptime"), "1.0 1.0").unwrap();
        // One that exited as it was listed is gone.
        std::fs::create_dir(dir.path().join("13")).unwrap();
        assert!(!left(12).unwrap());
        // A zombie is gone, unless it is a leader whose other threads run.
        process(12, "Z", 12, &[12]);
        assert!(!left(12).unwrap());
        std::fs::create_dir(dir.path().join("12/task/14")).unwrap();
        assert!(left(12).unwrap());
        process(15, "Z", 15, &[]);
        assert!(!left(15).unwrap());
        process(16, "S", 15, &[16]);
        assert!(left(15).unwrap());
        // An entry that leads anywhere else can't be seen, so none is certain.
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir2.path().join("17")).unwrap();
        std::os::unix::fs::symlink(dir2.path().join("17"), dir.path().join("17")).unwrap();
        // A group with none left is scanned to the end, where it is met.
        assert!(left(20).is_err());
    }

    #[test]
    fn none_is_gone_where_proc_changed_what_it_shows_during_the_scan() {
        use rustix::fs::{Mode, OFlags};
        let dir = tempfile::tempdir().unwrap();
        let proc = rustix::fs::open(
            dir.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let dev = rustix::fs::makedev(0, 23);
        let table = |options: &str| {
            format!("22 1 0:23 / /proc rw,nosuid shared:5 - proc proc rw{options}\n").into_bytes()
        };
        let left = |now: std::io::Result<Vec<u8>>| {
            members_left_as_shown(&proc, Pid::from_raw(12).unwrap(), dev, &table(""), || now)
        };
        // With none found, the options it had throughout make it certain.
        assert!(!left(Ok(table(""))).unwrap());
        // A remount that hid more by the end could have hidden a member.
        assert!(left(Ok(table(",hidepid=invisible"))).is_err());
        // As could one no longer listed, or a table that can't be read.
        assert!(left(Ok(Vec::new())).is_err());
        assert!(left(Err(std::io::ErrorKind::PermissionDenied.into())).is_err());
        // A member seen is left, whatever the options have become.
        std::fs::create_dir_all(dir.path().join("12/task/12")).unwrap();
        std::fs::write(dir.path().join("12/stat"), "12 (a) S 1 12 12 0 -1").unwrap();
        assert!(left(Ok(table(",hidepid=invisible"))).unwrap());
    }

    #[test]
    fn only_the_initial_user_namespace_numbers_groups_as_proc_does() {
        let link = |target: &str| Ok(target.as_bytes().to_vec());
        assert!(in_initial_user_namespace(link("user:[4026531837]")));
        assert!(!in_initial_user_namespace(link("user:[4026532001]")));
        // Where the link is missing, as on a Linux without user namespaces, or
        // can't be read for a mount, nothing is known.
        assert!(!in_initial_user_namespace(Err(
            std::io::ErrorKind::NotFound.into()
        )));
        assert!(!in_initial_user_namespace(Err(std::io::Error::other(
            "a mount covers part of this computer’s /proc"
        ))));
    }
}
