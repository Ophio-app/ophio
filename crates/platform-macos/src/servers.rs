// SPDX-License-Identifier: Apache-2.0
//! Listening sockets of programs working in a project's folder, read from the
//! system's process list. Only this user's programs can be looked at.

use crate::process;
use ports::servers::{Listening, LocalServers};
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct MacServers {
    /// The companion itself, whose own sockets are never a project's.
    own: i32,
}

impl MacServers {
    pub fn new() -> Self {
        Self {
            own: i32::try_from(std::process::id()).unwrap_or(0),
        }
    }
}

impl Default for MacServers {
    fn default() -> Self {
        Self::new()
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

/// The folder as the system names a working folder: the real path, without the
/// links a Mac has at `/tmp` and `/var`.
fn real(folder: &Path) -> PathBuf {
    folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf())
}

impl LocalServers for MacServers {
    fn listening(&self, folders: &[PathBuf]) -> Vec<Listening> {
        if folders.is_empty() {
            return Vec::new();
        }
        let folders: Vec<PathBuf> = folders.iter().map(|folder| real(folder)).collect();
        // A program the companion has just started shares its sockets until
        // its own program runs, and they are still the companion's.
        let own: HashSet<u64> = process::listening(self.own)
            .into_iter()
            .map(|socket| socket.id)
            .collect();
        let mut found: Vec<Listening> = Vec::new();
        for pid in process::all().unwrap_or_default() {
            if pid == self.own {
                continue;
            }
            let Some(dir) = process::working_folder(pid) else {
                continue;
            };
            if !folders.iter().any(|folder| dir.starts_with(folder)) {
                continue;
            }
            let before = found.len();
            for socket in process::listening(pid) {
                // Workers forked from one server share its socket.
                if local(socket.address)
                    && !own.contains(&socket.id)
                    && !found.iter().any(|listening| listening.socket == socket.id)
                {
                    found.push(Listening {
                        address: socket.address,
                        port: socket.port,
                        dir: dir.clone(),
                        socket: socket.id,
                    });
                }
            }
            // It may have moved, or gone, while its files were read.
            if process::working_folder(pid).as_deref() != Some(dir.as_path()) {
                found.truncate(before);
            }
        }
        found.sort_by_key(|listening| (listening.port, listening.socket));
        found
    }

    fn open(&self) -> HashMap<u64, u16> {
        process::all()
            .unwrap_or_default()
            .into_iter()
            .flat_map(process::listening)
            .filter(|socket| local(socket.address))
            .map(|socket| (socket.id, socket.port))
            .collect()
    }

    fn boot(&self) -> Option<String> {
        process::sysctl_text(c"kern.bootsessionuuid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    /// A server in `dir` started by a script, since the companion's own
    /// sockets never count.
    fn serve(dir: &Path, address: &str) -> (std::process::Child, u16) {
        let script = format!(
            "import socket,sys\ns=socket.socket(socket.AF_INET)\ns.bind(('{address}',0))\ns.listen()\nprint(s.getsockname()[1],flush=True)\nimport time\ntime.sleep(60)\n"
        );
        let mut child = std::process::Command::new("/usr/bin/python3")
            .args(["-c", &script])
            .current_dir(dir)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        (child, line.trim().parse().unwrap())
    }

    #[test]
    fn a_server_working_in_a_project_is_found_and_others_are_not() {
        let project = tempfile::tempdir().unwrap();
        let inside = project.path().join("site");
        std::fs::create_dir(&inside).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let (mut here, here_port) = serve(&inside, "127.0.0.1");
        let (mut there, there_port) = serve(elsewhere.path(), "127.0.0.1");
        let (mut outward, outward_port) = serve(project.path(), "0.0.0.0");
        let servers = MacServers::new();

        let found = servers.listening(&[project.path().to_path_buf()]);
        let ports: Vec<u16> = found.iter().map(|listening| listening.port).collect();
        assert!(ports.contains(&here_port), "{found:?}");
        assert!(ports.contains(&outward_port), "{found:?}");
        assert!(!ports.contains(&there_port), "{found:?}");
        assert_eq!(
            found
                .iter()
                .find(|listening| listening.port == here_port)
                .unwrap()
                .dir,
            inside.canonicalize().unwrap()
        );
        assert!(
            servers.open().values().any(|&port| port == there_port),
            "any program's socket is open"
        );
        for child in [&mut here, &mut there, &mut outward] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }

    #[test]
    fn the_companions_own_sockets_never_count() {
        let project = std::env::current_dir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let found = MacServers::new().listening(&[project]);
        assert!(found.iter().all(|listening| listening.port != port));
    }

    #[test]
    fn the_boot_is_named_and_no_folders_find_nothing() {
        let servers = MacServers::new();
        assert!(servers.boot().is_some());
        assert_eq!(servers.boot(), servers.boot());
        assert!(servers.listening(&[]).is_empty());
    }
}
