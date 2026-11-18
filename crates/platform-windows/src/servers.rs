// SPDX-License-Identifier: Apache-2.0
//! Project preview discovery from native TCP ownership and verified working
//! directories. No command lines, network probes or port-number guesses.
#[cfg(windows)]
pub use crate::win::servers::{WindowsServers, listening};

#[cfg(any(windows, test))]
mod discovery {
    use ports::servers::Listening;
    use std::{
        collections::HashMap,
        hash::{Hash, Hasher},
        net::IpAddr,
        path::PathBuf,
    };

    /// The kernel's listener creation time distinguishes a restarted listener,
    /// even when the process and endpoint are unchanged.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(crate) struct Socket {
        pub pid: u32,
        pub address: IpAddr,
        pub port: u16,
        pub created: i64,
    }

    impl Socket {
        pub fn id(&self) -> u64 {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            self.hash(&mut hash);
            hash.finish()
        }
        pub fn local(&self) -> bool {
            self.pid != 0
                && self.port != 0
                && self.created > 0
                && match self.address {
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
    }

    /// Only produced after same-user token validation and directory-handle
    /// identity comparison. The native adapter reopens the process each time.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Working {
        pub created: u64,
        pub dir: PathBuf,
        pub volume: u64,
        pub file: [u8; 16],
    }

    pub(crate) trait Source {
        fn sockets(&self) -> Vec<Socket>;
        fn working(&self, pid: u32, folders: &[PathBuf]) -> Option<Working>;
        fn canonical(&self, path: &std::path::Path) -> Option<PathBuf>;
    }

    pub(crate) fn find(source: &impl Source, own: u32, folders: &[PathBuf]) -> Vec<Listening> {
        if folders.is_empty() {
            return Vec::new();
        }
        let folders: Vec<_> = folders.iter().filter_map(|p| source.canonical(p)).collect();
        if folders.is_empty() {
            return Vec::new();
        }
        let sockets: Vec<_> = source
            .sockets()
            .into_iter()
            .filter(Socket::local)
            .filter(|s| s.pid != own)
            .collect();
        let mut working = HashMap::new();
        for socket in &sockets {
            working
                .entry(socket.pid)
                .or_insert_with(|| source.working(socket.pid, &folders));
        }
        // Require the same listener in a fresh kernel table after looking up
        // the process. A PID alone is not proof of socket ownership.
        let after = source.sockets();
        let mut found = Vec::new();
        for (pid, before) in working {
            let Some(before) = before else {
                continue;
            };
            if !folders.iter().any(|folder| before.dir.starts_with(folder)) {
                continue;
            }
            if source.working(pid, &folders).as_ref() != Some(&before) {
                continue;
            }
            for socket in sockets
                .iter()
                .filter(|s| s.pid == pid && s.created as u64 >= before.created && after.contains(s))
            {
                let id = socket.id();
                if !found.iter().any(|l: &Listening| l.socket == id) {
                    found.push(Listening {
                        address: socket.address,
                        port: socket.port,
                        dir: ports::library::plain(before.dir.clone()),
                        socket: id,
                    });
                }
            }
        }
        found.sort_by_key(|l| (l.port, l.socket));
        found
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::Cell;
        struct Fake {
            rows: Vec<Socket>,
            after: Vec<Socket>,
            calls: Cell<u32>,
            before: Option<Working>,
            later: Option<Working>,
            reads: Cell<u32>,
        }
        impl Source for Fake {
            fn sockets(&self) -> Vec<Socket> {
                let n = self.calls.replace(self.calls.get() + 1);
                if n == 0 {
                    self.rows.clone()
                } else {
                    self.after.clone()
                }
            }
            fn working(&self, _: u32, _: &[PathBuf]) -> Option<Working> {
                let n = self.reads.replace(self.reads.get() + 1);
                if n == 0 {
                    self.before.clone()
                } else {
                    self.later.clone()
                }
            }
            fn canonical(&self, p: &std::path::Path) -> Option<PathBuf> {
                Some(p.into())
            }
        }
        fn fixture() -> Fake {
            let rows = vec![Socket {
                pid: 42,
                address: "127.0.0.1".parse().unwrap(),
                port: 5173,
                created: 50,
            }];
            let work = Some(Working {
                created: 10,
                dir: PathBuf::from("/project/site"),
                volume: 1,
                file: [1; 16],
            });
            Fake {
                after: rows.clone(),
                rows,
                calls: Cell::new(0),
                before: work.clone(),
                later: work,
                reads: Cell::new(0),
            }
        }
        fn found(f: &Fake) -> Vec<Listening> {
            find(f, 99, &[PathBuf::from("/project")])
        }
        #[test]
        fn requires_verified_process_working_in_project_and_never_uses_port_guesses() {
            assert_eq!(found(&fixture()).len(), 1);
            let mut f = fixture();
            f.before = None; // another user, denied process, or unsupported layout
            assert!(found(&f).is_empty());
            let mut f = fixture();
            f.before.as_mut().unwrap().dir = "/project-other".into();
            f.later = f.before.clone();
            assert!(found(&f).is_empty());
            assert!(find(&fixture(), 42, &["/project".into()]).is_empty());
            assert!(find(&fixture(), 99, &[]).is_empty());
        }
        #[test]
        fn rejects_exited_reused_moving_or_replaced_processes_and_directories() {
            for change in 0..5 {
                let mut f = fixture();
                match change {
                    0 => f.later = None,
                    1 => f.later.as_mut().unwrap().created += 1,
                    2 => f.later.as_mut().unwrap().dir = "/project/other".into(),
                    3 => f.later.as_mut().unwrap().file = [2; 16],
                    _ => f.later.as_mut().unwrap().volume += 1,
                }
                assert!(found(&f).is_empty(), "{change}");
            }
        }
        #[test]
        fn revalidates_listener_lifecycle_and_new_listener_gets_new_identity() {
            let mut f = fixture();
            f.after.clear();
            assert!(found(&f).is_empty());
            let mut f = fixture();
            f.after[0].created += 1;
            assert!(found(&f).is_empty());
            let first = found(&fixture())[0].socket;
            let mut f = fixture();
            f.rows[0].created += 1;
            f.after = f.rows.clone();
            assert_ne!(found(&f)[0].socket, first);
            // A subsequent scan recovers after an unavailable process.
            assert_eq!(found(&fixture()).len(), 1);
        }
        #[test]
        fn a_stale_bind_pid_reused_after_socket_creation_cannot_claim_the_listener() {
            let mut f = fixture();
            f.before.as_mut().unwrap().created = 51;
            f.later = f.before.clone();
            assert!(found(&f).is_empty());
        }
        #[cfg(windows)]
        #[test]
        fn emitted_folder_matches_shared_project_canonical_path_convention() {
            let mut f = fixture();
            let dir = PathBuf::from(r"\\?\C:\project\site");
            f.before.as_mut().unwrap().dir = dir;
            f.later = f.before.clone();
            let found = find(&f, 99, &[PathBuf::from(r"\\?\C:\project")]);
            assert_eq!(found[0].dir, PathBuf::from(r"C:\project\site"));
        }
        #[test]
        fn only_loopback_reachable_valid_kernel_listeners_count() {
            for (ip, yes) in [
                ("127.0.0.1", true),
                ("0.0.0.0", true),
                ("::", true),
                ("::1", true),
                ("::ffff:127.0.0.1", true),
                ("192.0.2.1", false),
                ("fe80::1", false),
            ] {
                let mut f = fixture();
                f.rows[0].address = ip.parse().unwrap();
                f.after = f.rows.clone();
                assert_eq!(!found(&f).is_empty(), yes, "{ip}");
            }
            for change in 0..3 {
                let mut f = fixture();
                match change {
                    0 => f.rows[0].port = 0,
                    1 => f.rows[0].created = 0,
                    _ => f.rows[0].pid = 0,
                }
                f.after = f.rows.clone();
                assert!(found(&f).is_empty());
            }
        }
    }
}
#[cfg(windows)]
pub(crate) use discovery::*;
