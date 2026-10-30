// SPDX-License-Identifier: Apache-2.0
//! Listening sockets of programs working in a project's folder, read from
//! `/proc`: the kernel's socket tables, then each program's working folder,
//! and only for a program working in one of the folders, its open files.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use ports::servers::{Listening, LocalServers};

/// The socket tables' state for a listening socket.
const LISTEN: &str = "0A";

#[derive(Debug)]
pub struct LinuxServers {
    root: PathBuf,
    /// The companion itself, whose own sockets are never a project's.
    own: Option<u32>,
    /// Which folder a path leads to, by device and inode.
    identity: fn(&Path) -> Option<(u64, u64)>,
}

impl LinuxServers {
    pub fn new() -> Self {
        Self {
            own: Some(std::process::id()),
            ..Self::at("/")
        }
    }

    /// Sockets from a tree laid out like `/`, for tests.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            own: None,
            identity,
        }
    }

    /// Which folder a program works in, when it is the one its link names. A
    /// folder deleted under it is still named, as `… (deleted)`, and the name
    /// may since lead to another.
    fn working_in(&self, process: &Path, dir: &Path) -> Option<(u64, u64)> {
        let it = (self.identity)(&process.join("cwd"))?;
        ((self.identity)(dir)? == it).then_some(it)
    }

    /// Listening sockets reachable from this computer, by their inode.
    fn sockets(&self) -> HashMap<u64, (IpAddr, u16)> {
        ["proc/net/tcp", "proc/net/tcp6"]
            .iter()
            .filter_map(|table| std::fs::read_to_string(self.root.join(table)).ok())
            .flat_map(|text| text.lines().skip(1).filter_map(socket).collect::<Vec<_>>())
            .filter(|(_, (address, _))| local(*address))
            .collect()
    }

    fn find(&self, sockets: &HashMap<u64, (IpAddr, u16)>, folders: &[PathBuf]) -> Vec<Listening> {
        let mut found: Vec<Listening> = Vec::new();
        let Ok(processes) = std::fs::read_dir(self.root.join("proc")) else {
            return found;
        };
        for process in processes.flatten() {
            let Some(pid) = process
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if Some(pid) == self.own {
                continue;
            }
            // Its link is read without asking the folder's file system, which
            // may not answer: only one in a project's folder is asked about.
            let Ok(dir) = std::fs::read_link(process.path().join("cwd")) else {
                continue;
            };
            if !folders.iter().any(|folder| dir.starts_with(folder)) {
                continue;
            }
            let Some(working) = self.working_in(&process.path(), &dir) else {
                continue;
            };
            let before = found.len();
            for inode in held(&process.path()) {
                // Workers forked from one server share its socket.
                if let Some(&(address, port)) = sockets.get(&inode)
                    && !found.iter().any(|l| l.socket == inode)
                {
                    found.push(Listening {
                        address,
                        port,
                        dir: dir.clone(),
                        socket: inode,
                    });
                }
            }
            // It may have moved, or its folder gone, while its files were read.
            if self.working_in(&process.path(), &dir) != Some(working) {
                found.truncate(before);
            }
        }
        found.sort_by_key(|l| (l.port, l.socket));
        found
    }
}

impl Default for LinuxServers {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalServers for LinuxServers {
    fn listening(&self, folders: &[PathBuf]) -> Vec<Listening> {
        if folders.is_empty() {
            return Vec::new();
        }
        // Read anew each time: a program may change the folder it works in
        // while it keeps listening.
        let mut sockets = self.sockets();
        // A program the companion has just started shares its sockets until
        // its own program runs, and they are still the companion's.
        if let Some(own) = self.own {
            for inode in held(&self.root.join(format!("proc/{own}"))) {
                sockets.remove(&inode);
            }
        }
        if sockets.is_empty() {
            return Vec::new();
        }
        self.find(&sockets, folders)
    }

    fn open(&self) -> HashMap<u64, u16> {
        (self.sockets().into_iter())
            .map(|(socket, (_, port))| (socket, port))
            .collect()
    }

    fn boot(&self) -> Option<String> {
        let boot =
            std::fs::read_to_string(self.root.join("proc/sys/kernel/random/boot_id")).ok()?;
        Some(boot.trim().to_string()).filter(|b| !b.is_empty())
    }
}

/// The inodes of what the process at `process` in `/proc` has open.
fn held(process: &Path) -> impl Iterator<Item = u64> {
    std::fs::read_dir(process.join("fd"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|fd| {
            std::fs::read_link(fd.path())
                .ok()
                .as_deref()
                .and_then(inode)
        })
}

/// Which folder a path leads to.
fn identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// A listening socket from a line of `/proc/net/tcp` or `tcp6`.
fn socket(line: &str) -> Option<(u64, (IpAddr, u16))> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.get(3) != Some(&LISTEN) {
        return None;
    }
    let (address, port) = fields.get(1)?.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let inode = fields.get(9)?.parse().ok().filter(|&i| i != 0)?;
    Some((inode, (address_from(address)?, port)))
}

/// The kernel prints an address as 32-bit words in the computer's own byte
/// order.
fn address_from(hex: &str) -> Option<IpAddr> {
    let words = (0..hex.len() / 8)
        .map(|i| u32::from_str_radix(hex.get(i * 8..i * 8 + 8)?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_ne_bytes()).collect();
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(bytes).ok()?))),
        32 => Some(IpAddr::V6(Ipv6Addr::from(
            <[u8; 16]>::try_from(bytes).ok()?,
        ))),
        _ => None,
    }
}

/// Whether a page served there can be opened at loopback.
fn local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_unspecified(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|ip| ip.is_loopback() || ip.is_unspecified())
        }
    }
}

/// The socket an open file is, from its link: `socket:[12345]`.
fn inode(link: &Path) -> Option<u64> {
    link.to_str()?
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// An address as the kernel prints it on this computer.
    fn printed(address: IpAddr) -> String {
        let bytes = match address {
            IpAddr::V4(ip) => ip.octets().to_vec(),
            IpAddr::V6(ip) => ip.octets().to_vec(),
        };
        bytes
            .chunks(4)
            .map(|word| format!("{:08X}", u32::from_ne_bytes(word.try_into().unwrap())))
            .collect()
    }

    fn line(address: &str, port: u16, state: &str, inode: u64) -> String {
        let address: IpAddr = address.parse().unwrap();
        format!(
            "   0: {}:{port:04X} {}:0000 {state} 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0",
            printed(address),
            printed(match address {
                IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            }),
        )
    }

    thread_local! {
        /// The paths asked which folder they lead to.
        static ASKED: std::cell::RefCell<Vec<PathBuf>> = Default::default();
    }
    fn asked(path: &Path) -> Option<(u64, u64)> {
        ASKED.with(|asked| asked.borrow_mut().push(path.to_owned()));
        identity(path)
    }

    struct Tree(tempfile::TempDir);
    impl Tree {
        fn new(tcp: &[String], tcp6: &[String]) -> Self {
            let tree = Self(tempfile::tempdir().unwrap());
            std::fs::create_dir_all(tree.0.path().join("proc/net")).unwrap();
            let table = |lines: &[String]| {
                format!(
                    "  sl  local_address rem_address   st\n{}\n",
                    lines.join("\n")
                )
            };
            std::fs::write(tree.0.path().join("proc/net/tcp"), table(tcp)).unwrap();
            std::fs::write(tree.0.path().join("proc/net/tcp6"), table(tcp6)).unwrap();
            tree
        }
        /// A folder of the tree, made, as a program reports it.
        fn folder(&self, path: &str) -> PathBuf {
            let folder = self.0.path().join(path.trim_start_matches('/'));
            std::fs::create_dir_all(&folder).unwrap();
            folder
        }
        fn process(&self, pid: u32, cwd: &str, sockets: &[u64]) {
            let dir = self.0.path().join(format!("proc/{pid}"));
            std::fs::create_dir_all(dir.join("fd")).unwrap();
            symlink(self.folder(cwd), dir.join("cwd")).unwrap();
            symlink("/dev/null", dir.join("fd/0")).unwrap();
            for (n, inode) in sockets.iter().enumerate() {
                symlink(
                    format!("socket:[{inode}]"),
                    dir.join(format!("fd/{}", n + 3)),
                )
                .unwrap();
            }
        }
        fn servers(&self) -> LinuxServers {
            LinuxServers {
                identity: asked,
                ..LinuxServers::at(self.0.path())
            }
        }
    }

    fn found(tree: &Tree, folders: &[&str]) -> Vec<(String, u16, String, u64)> {
        let root = tree.0.path();
        let folders: Vec<PathBuf> = folders.iter().map(|f| tree.folder(f)).collect();
        tree.servers()
            .listening(&folders)
            .into_iter()
            .map(|l| {
                let dir = Path::new("/").join(l.dir.strip_prefix(root).unwrap());
                (
                    l.address.to_string(),
                    l.port,
                    dir.display().to_string(),
                    l.socket,
                )
            })
            .collect()
    }

    #[test]
    fn finds_what_programs_in_a_project_serve_at_loopback() {
        let tree = Tree::new(
            &[
                line("127.0.0.1", 5173, LISTEN, 11),
                line("0.0.0.0", 8000, LISTEN, 12),
                // Reachable only from the network, connected, or another folder's.
                line("192.168.1.5", 9000, LISTEN, 13),
                line("127.0.0.1", 41000, "01", 14),
                line("127.0.0.1", 6006, LISTEN, 15),
            ],
            &[line("::1", 3000, LISTEN, 21), line("::", 3000, LISTEN, 22)],
        );
        tree.process(100, "/home/sam/site", &[11]);
        tree.process(101, "/home/sam/site/api", &[12, 13, 14]);
        tree.process(102, "/home/you", &[15]);
        tree.process(103, "/home/sam/app", &[21, 22]);
        // A worker sharing its parent's socket.
        tree.process(104, "/home/sam/site", &[11]);
        assert_eq!(
            found(&tree, &["/home/sam/site", "/home/sam/app"]),
            [
                ("::1".into(), 3000, "/home/sam/app".into(), 21),
                ("::".into(), 3000, "/home/sam/app".into(), 22),
                ("127.0.0.1".into(), 5173, "/home/sam/site".into(), 11),
                ("0.0.0.0".into(), 8000, "/home/sam/site/api".into(), 12),
            ]
        );
        // A program elsewhere isn't asked about its folder, whose file
        // system may not answer.
        let elsewhere =
            |path: &PathBuf| path.ends_with("proc/102/cwd") || path.ends_with("home/you");
        assert!(!ASKED.with(|asked| asked.borrow().iter().any(elsewhere)));
        assert!(ASKED.with(|asked| asked.borrow().iter().any(|p| p.ends_with("proc/100/cwd"))));
        // A folder whose name only begins the same is another folder.
        assert_eq!(found(&tree, &["/home/sam/sit"]), []);
        assert_eq!(found(&tree, &[]), []);
    }

    #[test]
    fn follows_a_program_into_another_folder() {
        let tree = Tree::new(&[line("127.0.0.1", 5173, LISTEN, 11)], &[]);
        tree.process(100, "/home/sam/site", &[11]);
        let folders = ["/home/sam/site", "/home/sam/app"];
        assert_eq!(found(&tree, &folders)[0].2, "/home/sam/site");
        // Still listening, it moves to another project, then out of them all.
        let cwd = tree.0.path().join("proc/100/cwd");
        for (to, expected) in [("/home/sam/app", 1), ("/tmp", 0)] {
            std::fs::remove_file(&cwd).unwrap();
            symlink(tree.folder(to), &cwd).unwrap();
            let now = found(&tree, &folders);
            assert_eq!(now.len(), expected, "{to}");
            assert!(now.iter().all(|l| l.2 == to), "{to}");
        }
    }

    #[test]
    fn a_program_that_moves_while_its_files_are_read_is_left_out() {
        thread_local! {
            static CWD_ASKED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        // Its working folder is another by the time its files are read.
        fn moving(path: &Path) -> Option<(u64, u64)> {
            if path.ends_with("proc/100/cwd") {
                let asked = CWD_ASKED.with(|n| n.replace(n.get() + 1));
                return if asked == 0 {
                    identity(path)
                } else {
                    Some((0, 0))
                };
            }
            identity(path)
        }
        let tree = Tree::new(&[line("127.0.0.1", 5173, LISTEN, 11)], &[]);
        tree.process(100, "/home/sam/site", &[11]);
        let servers = LinuxServers {
            identity: moving,
            ..tree.servers()
        };
        let site = tree.folder("/home/sam/site");
        assert_eq!(servers.listening(std::slice::from_ref(&site)), []);
        assert_eq!(CWD_ASKED.with(|n| n.get()), 2);
    }

    #[test]
    fn a_socket_the_companion_shares_with_a_program_it_started_stays_its_own() {
        let tree = Tree::new(&[line("127.0.0.1", 5173, LISTEN, 11)], &[]);
        tree.process(100, "/home/sam/site", &[11]);
        tree.process(101, "/home/sam/site", &[11]);
        let servers = LinuxServers {
            own: Some(100),
            ..tree.servers()
        };
        let site = tree.folder("/home/sam/site");
        assert_eq!(servers.listening(std::slice::from_ref(&site)), []);
    }

    #[test]
    fn finds_a_socket_this_test_listens_with() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        let found = LinuxServers::at("/").listening(std::slice::from_ref(&here));
        let Some(socket) = found.iter().find(|l| l.port == port && l.dir == here) else {
            panic!("{found:?}")
        };
        // The companion's own sockets are never a project's.
        let servers = LinuxServers::new();
        assert!(!servers.listening(&[here]).iter().any(|l| l.port == port));
        // Though it is known to be open, as on a computer that has booted.
        assert_eq!(servers.open().get(&socket.socket), Some(&port));
        assert!(servers.boot().is_some_and(|boot| boot.len() > 8));
    }

    #[test]
    fn knows_which_sockets_reachable_here_are_open_whoever_holds_them() {
        let tree = Tree::new(
            &[
                line("127.0.0.1", 5173, LISTEN, 11),
                line("192.168.1.5", 9000, LISTEN, 13),
                line("127.0.0.1", 41000, "01", 14),
            ],
            &[line("::", 3000, LISTEN, 22)],
        );
        tree.process(102, "/home/you", &[11]);
        let servers = tree.servers();
        ASKED.with(|asked| asked.borrow_mut().clear());
        assert_eq!(servers.open(), HashMap::from([(11, 5173), (22, 3000)]));
        // Without looking at any program.
        assert!(ASKED.with(|asked| asked.borrow().is_empty()));
        // And the boot their numbers belong to, when it is known.
        assert_eq!(servers.boot(), None);
        let random = tree.0.path().join("proc/sys/kernel/random");
        std::fs::create_dir_all(&random).unwrap();
        std::fs::write(random.join("boot_id"), "6f1c2a\n").unwrap();
        assert_eq!(servers.boot().as_deref(), Some("6f1c2a"));
    }

    #[test]
    fn a_program_whose_folder_was_deleted_works_in_no_project() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let scratch = tempfile::tempdir().unwrap();
        let project = scratch.path().canonicalize().unwrap();
        let work = project.join("tmp");
        std::fs::create_dir(&work).unwrap();
        // A program holding the socket, working in the project's folder.
        let socket = std::os::fd::OwnedFd::from(listener.try_clone().unwrap());
        struct Running(std::process::Child);
        impl Drop for Running {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let _program = Running(
            std::process::Command::new("sleep")
                .arg("60")
                .current_dir(&work)
                .stdin(std::process::Stdio::from(socket))
                .spawn()
                .unwrap(),
        );
        let servers = LinuxServers::at("/");
        let found = |l: &Listening| l.port == port;
        let now = servers.listening(std::slice::from_ref(&project));
        assert!(now.iter().any(|l| found(l) && l.dir == work), "{now:?}");
        // The kernel still names it, as `tmp (deleted)`: a name that may
        // since lead to another folder.
        std::fs::remove_dir(&work).unwrap();
        for _ in 0..2 {
            let now = servers.listening(std::slice::from_ref(&project));
            assert!(!now.iter().any(found), "{now:?}");
            std::fs::create_dir_all(project.join("tmp (deleted)")).unwrap();
        }
    }

    #[test]
    fn reads_addresses_as_the_kernel_prints_them() {
        for address in [
            "127.0.0.1",
            "0.0.0.0",
            "10.1.2.3",
            "::1",
            "::",
            "::ffff:127.0.0.1",
            "fd00::5",
        ] {
            let ip: IpAddr = address.parse().unwrap();
            assert_eq!(address_from(&printed(ip)), Some(ip), "{address}");
        }
        assert_eq!(address_from("7F00"), None);
        assert!(local("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!local("::ffff:10.0.0.1".parse().unwrap()));
        assert!(!local("fd00::5".parse().unwrap()));
        assert_eq!(inode(Path::new("socket:[42]")), Some(42));
        for other in ["pipe:[42]", "/dev/null", "socket:[x]"] {
            assert_eq!(inode(Path::new(other)), None, "{other}");
        }
    }
}
