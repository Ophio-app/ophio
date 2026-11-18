// SPDX-License-Identifier: Apache-2.0
use super::{Paths, protected_paths};
use std::{io, path::Path, process::Command};

pub(super) fn command(paths: &Paths, shell_command: &str) -> io::Result<Command> {
    let mut command = Command::new("/usr/bin/bwrap");
    command.args([
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
    ]);
    if !paths.network || paths.proxy_socket.is_some() {
        command.arg("--unshare-net");
    }
    command.args(["--tmpfs", "/"]);
    for path in crate::runtime_paths() {
        if Path::new(path).exists() {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
    command.args([
        "--tmpfs", "/tmp", "--tmpfs", "/run", "--proc", "/proc", "--dev", "/dev",
    ]);
    for path in &paths.read {
        command.arg("--ro-bind").arg(path).arg(path);
    }
    command
        .arg("--bind")
        .arg(&paths.project)
        .arg(&paths.project);
    command
        .arg("--bind")
        .arg(&paths.scratch)
        .arg(&paths.scratch);
    // A read grant never restores a provider's state or a credential file.
    // The overlap validator rejects parent grants; masks cover unusual nested
    // credential paths and aliases already exposed through a project/tool tree.
    for secret in protected_paths(&paths.home, &paths.scratch) {
        if secret == paths.scratch {
            continue;
        }
        let exposed = paths
            .read
            .iter()
            .chain([&paths.project])
            .any(|p| secret.starts_with(p));
        if exposed && secret.exists() {
            if secret.is_dir() {
                command
                    .arg("--tmpfs")
                    .arg(&secret)
                    .arg("--remount-ro")
                    .arg(&secret);
            } else {
                command.arg("--ro-bind").arg("/dev/null").arg(&secret);
            }
        }
    }
    if let Some(shim) = &paths.shim {
        command.arg("--ro-bind").arg(shim).arg(shim);
    }
    if let Some(socket) = &paths.proxy_socket {
        command
            .arg("--ro-bind")
            .arg(socket)
            .arg("/run/ophio-connect.sock");
    }
    command.arg("--chdir").arg(&paths.project).arg("--");
    if let Some(shim) = &paths.shim {
        command.arg(shim).args(["run", "/run/ophio-connect.sock"]);
    }
    command.arg("/bin/sh").arg("-c").arg(shell_command);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn every_network_mode_starts_a_new_session_and_filtered_network_is_isolated() {
        for (network, filtered) in [(false, false), (true, false), (true, true)] {
            let paths = Paths {
                project: PathBuf::from("/project"),
                scratch: PathBuf::from("/scratch"),
                home: PathBuf::from("/home/you"),
                read: vec![],
                network,
                proxy_socket: filtered.then(|| PathBuf::from("/scratch/connect.sock")),
                shim: filtered.then(|| PathBuf::from("/usr/bin/ophio-sandbox-shim")),
            };
            let command = command(&paths, "true").unwrap();
            let args: Vec<_> = command.get_args().map(|s| s.to_string_lossy()).collect();
            assert!(args.iter().any(|arg| arg == "--new-session"));
            assert_eq!(
                args.iter().any(|arg| arg == "--unshare-net"),
                !network || filtered
            );
            if filtered {
                assert!(args.iter().any(|arg| arg == "/run/ophio-connect.sock"));
                assert!(
                    args.windows(2)
                        .any(|pair| pair == ["/usr/bin/ophio-sandbox-shim", "run"])
                );
            }
        }
    }
}
