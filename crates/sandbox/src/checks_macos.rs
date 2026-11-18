// SPDX-License-Identifier: Apache-2.0
//! A check's single-use Mac sandbox, without an agent's sign-ins or proxy.
use super::{Paths, protected_paths};
use crate::macos;
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

/// The caller supplies canonical, validated paths and sets the cleared, fixed
/// environment and working folder after this command has been built.
pub(super) fn command(paths: &Paths, shell_command: &str) -> io::Result<Command> {
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.args(["-p", &profile(paths), "/bin/sh", "-c", shell_command]);
    Ok(command)
}

fn rule(profile: &mut String, operation: &str, path: &Path) {
    profile.push_str(&format!(
        "({operation} (subpath {}))\n",
        macos::quoted(path)
    ));
}

fn profile(paths: &Paths) -> String {
    let mut profile = String::from(macos::BASE);
    for path in macos::runtime_paths() {
        rule(&mut profile, "allow file-read*", Path::new(path));
    }
    for path in macos::RUNTIME_DATA {
        rule(&mut profile, "deny file-read*", Path::new(path));
    }
    for path in &paths.read {
        rule(&mut profile, "allow file-read*", path);
    }
    rule(&mut profile, "allow file-read* file-write*", &paths.project);
    // These denials follow every grant. A toolchain's parent folder must never
    // accidentally give back a provider token or a package manager's sign-in.
    for path in secret_paths(paths) {
        rule(&mut profile, "deny file-read* file-write*", &path);
        // The system compares real paths. Protect a credential moved behind a
        // link as well as its usual name, including a missing file's parent.
        let real = resolved(&path);
        if real != path {
            rule(&mut profile, "deny file-read* file-write*", &real);
        }
    }
    // Scratch may live below Ophio's protected storage. It was just created
    // for this one check, so restore only it, never the rest of that storage.
    rule(&mut profile, "allow file-read* file-write*", &paths.scratch);
    // Match the ordinary agent profile: even an offline task may serve on
    // localhost, but it cannot initiate a loopback connection. The Mac shares
    // its loopback with the host, unlike Linux's private network namespace.
    profile.push_str("(deny network*)\n");
    if let Some(port) = paths.proxy_port {
        // Match the agent's filtered path without sharing its proxy or state.
        profile.push_str(&format!(
            "(allow network-outbound (remote tcp \"localhost:{port}\"))\n"
        ));
    } else if paths.network {
        // An unsandboxed task had unrestricted IP networking. This does not
        // need broad Unix-socket grants; listener rules still match the agent.
        profile.push_str("(allow network-outbound (remote ip \"*:*\"))\n");
        rule(&mut profile, "allow network-outbound", &paths.project);
        rule(&mut profile, "allow network-outbound", &paths.scratch);
    }
    if paths.network && paths.loopback {
        profile.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
    }
    profile.push_str(
        "(allow network-inbound (local ip \"localhost:*\"))\n(allow network-bind (local ip \"localhost:*\"))\n",
    );
    profile
}

fn secret_paths(paths: &Paths) -> Vec<PathBuf> {
    // The shared list includes its state argument. Here that argument is our
    // fresh scratch folder, which is writable and holds no inherited secrets.
    let mut private = protected_paths(&paths.home, &paths.scratch);
    private.retain(|path| path != &paths.scratch);
    private.extend(
        [
            ".config/claude",
            ".config/codex",
            ".config/git/credentials",
            ".kube",
            ".pypirc",
            "Library/Application Support/gh",
            "Library/Application Support/ophio",
            "Library/Caches/ophio",
            "Library/Preferences/ophio.plist",
        ]
        .into_iter()
        .map(|path| paths.home.join(path)),
    );
    private.extend([
        PathBuf::from("/Library/Keychains"),
        PathBuf::from("/private/var/db/SystemKey"),
    ]);
    private
}

/// Resolve the existing prefix too, so an absent token under a linked folder
/// is still protected when another process creates it during the check.
fn resolved(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut prefix = path;
    loop {
        if let Ok(mut real) = prefix.canonicalize() {
            real.extend(missing.iter().rev());
            return real;
        }
        match (prefix.parent(), prefix.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name);
                prefix = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        process::Output,
        time::{Duration, Instant},
    };

    struct Rig {
        _root: tempfile::TempDir,
        paths: Paths,
    }

    impl Rig {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let root_path = root.path().canonicalize().unwrap();
            let paths = Paths {
                project: root_path.join("project"),
                scratch: root_path.join("scratch"),
                home: root_path.join("person"),
                read: Vec::new(),
                network: false,
                proxy_port: None,
                loopback: false,
            };
            for path in [
                paths.project.clone(),
                paths.home.clone(),
                paths.scratch.join("home"),
                paths.scratch.join("tmp"),
            ] {
                std::fs::create_dir_all(path).unwrap();
            }
            Self { _root: root, paths }
        }

        fn run(&self, shell_command: &str) -> Output {
            self.run_command(command(&self.paths, shell_command).unwrap())
        }

        fn run_command(&self, mut command: Command) -> Output {
            command
                .env_clear()
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("HOME", self.paths.scratch.join("home"))
                .env("TMPDIR", self.paths.scratch.join("tmp"))
                .env("LANG", "en_US.UTF-8")
                .current_dir(&self.paths.project)
                .output()
                .unwrap()
        }
    }

    fn success(output: &Output) {
        assert!(
            output.status.success(),
            "status: {}; stdout: {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[test]
    fn uses_inline_profile_and_adds_no_environment_or_proxy() {
        let rig = Rig::new();
        let command = command(&rig.paths, "true").unwrap();
        assert_eq!(command.get_program(), "/usr/bin/sandbox-exec");
        assert_eq!(command.get_envs().count(), 0);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[0], "-p");
        let profile = args[1].to_str().unwrap();
        assert!(profile.starts_with(macos::BASE));
        assert!(profile.contains("(deny network*)\n"));
        assert!(profile.contains("(allow network-bind (local ip \"localhost:*\"))"));
        assert!(profile.contains("(allow network-inbound (local ip \"localhost:*\"))"));
        assert!(!profile.contains("allow network-outbound"));
        assert_eq!(&args[2..], &["/bin/sh", "-c", "true"]);
    }

    #[test]
    fn network_grants_keep_unix_socket_limits_and_the_agents_listener_rules() {
        let mut rig = Rig::new();
        rig.paths.network = true;
        let unrestricted = profile(&rig.paths);
        assert!(!unrestricted.contains("(allow network*)"));
        assert!(unrestricted.contains("(allow network-outbound (remote ip \"*:*\"))"));
        assert!(unrestricted.contains("(allow network-inbound (local ip \"localhost:*\"))"));
        assert!(!unrestricted.contains("(allow network-bind (local ip \"*:*\"))"));
        rig.paths.proxy_port = Some(18080);
        let filtered = profile(&rig.paths);
        assert!(filtered.contains("(allow network-outbound (remote tcp \"localhost:18080\"))\n"));
        assert!(filtered.ends_with("(allow network-bind (local ip \"localhost:*\"))\n"));
        assert!(!filtered.contains("(remote ip \"*:*\")"));
    }

    #[test]
    fn direct_loopback_requires_network_and_the_existing_task_permission() {
        let mut rig = Rig::new();
        for network in [false, true] {
            for allowed in [false, true] {
                rig.paths.network = network;
                rig.paths.loopback = allowed;
                rig.paths.proxy_port = network.then_some(18080);
                let text = profile(&rig.paths);
                assert!(text.contains("(allow network-bind (local ip \"localhost:*\"))"));
                assert!(text.contains("(allow network-inbound (local ip \"localhost:*\"))"));
                assert_eq!(
                    text.contains("(allow network-outbound (remote ip \"localhost:*\"))"),
                    network && allowed
                );
            }
        }
    }

    #[test]
    fn native_network_access_never_exceeds_the_agents_for_the_same_grants() {
        // UDP connect only selects an assigned address; it sends no data.
        let route = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        route.connect("192.0.2.1:9").unwrap();
        let address = route.local_addr().unwrap().ip();
        assert!(!address.is_loopback() && !address.is_unspecified());
        drop(TcpListener::bind((address, 0)).unwrap());
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let script = r#"
import json, socket
def allowed(operation, address):
    try:
        with socket.socket() as connection:
            connection.settimeout(2)
            if operation == 'bind':
                connection.bind(address)
                connection.listen(1)
            else:
                connection.connect(address)
        return True
    except PermissionError:
        return False
print(json.dumps([
    allowed('bind', ('127.0.0.1', 0)),
    allowed('bind', ('__NONLOOPBACK__', 0)),
    allowed('connect', ('127.0.0.1', __TARGET_PORT__)),
    allowed('connect', ('127.0.0.1', __PROXY_PORT__))
]))
"#
        .replace("__NONLOOPBACK__", &address.to_string())
        .replace(
            "__TARGET_PORT__",
            &target.local_addr().unwrap().port().to_string(),
        )
        .replace(
            "__PROXY_PORT__",
            &proxy.local_addr().unwrap().port().to_string(),
        );
        let shell = format!("/usr/bin/python3 -c '{}'", script.replace('\'', "'\\''"));
        let mut rig = Rig::new();
        for (network, local) in [(false, false), (true, false), (true, true)] {
            rig.paths.network = network;
            rig.paths.loopback = network && local;
            rig.paths.proxy_port = network.then(|| proxy.local_addr().unwrap().port());
            // Use the actual agent profile builder, not a second copy of its
            // rules, and only synthetic project/scratch access. No sign-ins.
            let agent_profile = macos::check_network_comparison_profile(
                &rig.paths.project,
                &rig.paths.scratch,
                network && local,
                rig.paths.proxy_port,
            );
            let mut agent_command = Command::new("/usr/bin/sandbox-exec");
            agent_command.args(["-p", &agent_profile, "/bin/sh", "-c", &shell]);
            let agent_output = rig.run_command(agent_command);
            success(&agent_output);
            let check_output = rig.run(&shell);
            success(&check_output);
            let agent: Vec<bool> = serde_json::from_slice(&agent_output.stdout).unwrap();
            let check: Vec<bool> = serde_json::from_slice(&check_output.stdout).unwrap();
            assert_eq!(agent.len(), 4);
            assert_eq!(check.len(), 4);
            eprintln!("network={network}, local={local}: agent={agent:?}, check={check:?}");
            for (operation, (check, agent)) in check.iter().zip(&agent).enumerate() {
                assert!(
                    !check || *agent,
                    "operation {operation} exceeded the agent's access"
                );
            }
            assert!(
                check[0] && agent[0],
                "both profiles must serve on localhost"
            );
            assert_eq!(check[2], network && local);
            assert_eq!(agent[2], network && local);
            assert_eq!(check[3], network);
            assert_eq!(agent[3], network);
        }
    }

    #[test]
    fn approved_network_cannot_connect_to_an_unrelated_readable_unix_socket() {
        let mut rig = Rig::new();
        rig.paths.network = true;
        let tools = rig.paths.home.join("tools");
        std::fs::create_dir(&tools).unwrap();
        let socket = tools.join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        rig.paths.read.push(tools);
        let output = rig.run(&format!(
            "/usr/bin/curl --silent --show-error --max-time 2 --unix-socket '{}' http://localhost/",
            socket.display()
        ));
        assert!(
            !output.status.success(),
            "unrelated Unix socket was reachable"
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn project_and_scratch_are_writable_but_toolchains_are_read_only() {
        let mut rig = Rig::new();
        let tools = rig.paths.home.join("tools");
        std::fs::create_dir(&tools).unwrap();
        std::fs::write(tools.join("compiler"), "toolchain").unwrap();
        rig.paths.read.push(tools.clone());
        let output = rig.run(&format!(
            "printf project > built; printf scratch > \"$TMPDIR/built\"; \
             test -f built && test -f \"$TMPDIR/built\" && \
             /bin/cat '{}' && ! (printf changed > '{}')",
            tools.join("compiler").display(),
            tools.join("compiler").display(),
        ));
        success(&output);
        assert_eq!(
            std::fs::read_to_string(tools.join("compiler")).unwrap(),
            "toolchain"
        );
    }

    #[test]
    fn fresh_scratch_works_inside_protected_application_storage() {
        let mut rig = Rig::new();
        let storage = rig.paths.home.join(".local/share/ophio");
        rig.paths.scratch = storage.join("checks/fresh-check");
        for path in [
            rig.paths.scratch.join("home"),
            rig.paths.scratch.join("tmp"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        let private = storage.join("private-token");
        std::fs::write(&private, "planted-provider-token").unwrap();
        rig.paths.read.push(storage);
        let output = rig.run(&format!(
            "printf home > \"$HOME/file\" && printf temp > \"$TMPDIR/file\" && \
             test -f \"$HOME/file\" && test -f \"$TMPDIR/file\" && \
             ! /bin/cat '{}'",
            private.display(),
        ));
        success(&output);
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn provider_tokens_and_keychains_stay_private_under_a_read_grant() {
        let mut rig = Rig::new();
        // Deliberately wider than the caller allows: explicit credential denials
        // must still win if a future toolchain grant includes a secret's parent.
        rig.paths.read.push(rig.paths.home.clone());
        for name in [
            ".codex/auth.json",
            ".claude/.credentials.json",
            ".config/gh/hosts.yml",
            ".cargo/credentials.toml",
            "Library/Keychains/planted.keychain-db",
        ] {
            let token = rig.paths.home.join(name);
            std::fs::create_dir_all(token.parent().unwrap()).unwrap();
            std::fs::write(&token, "planted-provider-token").unwrap();
            let output = rig.run(&format!(
                "if /bin/cat '{}'; then exit 91; fi",
                token.display(),
            ));
            success(&output);
            assert!(!String::from_utf8_lossy(&output.stdout).contains("planted-provider-token"));
        }
    }

    #[test]
    fn linked_provider_folder_does_not_restore_its_tokens() {
        let mut rig = Rig::new();
        let target = rig.paths.home.join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("auth.json"), "planted-provider-token").unwrap();
        std::os::unix::fs::symlink(&target, rig.paths.home.join(".codex")).unwrap();
        rig.paths.read.push(target.clone());
        let output = rig.run(&format!(
            "if /bin/cat '{}'; then exit 91; fi",
            target.join("auth.json").display(),
        ));
        success(&output);
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn fixed_environment_contains_no_tokens_or_proxy_settings() {
        let rig = Rig::new();
        // Exercise the real common environment, not only this test's helper.
        let mut sandbox = crate::checks::prepare(
            crate::checks::CheckSandboxSpec {
                project: rig.paths.project.clone(),
                state: rig.paths.scratch.clone(),
                read: Vec::new(),
                network: false,
                network_restricted: true,
                local_network: false,
            },
            "/usr/bin/env",
        )
        .unwrap();
        let output = sandbox.take_command().unwrap().output().unwrap();
        success(&output);
        assert_eq!(output.stdout, b"OPHIO_CHECK_STARTED\n");
        for line in String::from_utf8(output.stderr).unwrap().lines() {
            let (name, _) = line.split_once('=').unwrap();
            assert!(
                matches!(
                    name,
                    "PATH"
                        | "HOME"
                        | "TMPDIR"
                        | "TMP"
                        | "TEMP"
                        | "LANG"
                        | "LC_ALL"
                        | "CI"
                        | "NO_COLOR"
                        | "CARGO_HOME"
                        | "GIT_CONFIG_GLOBAL"
                        | "GIT_CONFIG_NOSYSTEM"
                        | "PYTHONNOUSERSITE"
                        | "XDG_CACHE_HOME"
                        | "NPM_CONFIG_CACHE"
                        | "UV_CACHE_DIR"
                        | "PIP_CACHE_DIR"
                        | "NPM_CONFIG_USERCONFIG"
                        | "NPM_CONFIG_GLOBALCONFIG"
                        | "PWD"
                        | "SHLVL"
                        | "_"
                ),
                "unexpected environment variable: {name}",
            );
        }
    }

    #[test]
    fn loopback_http_is_denied_offline_and_allowed_when_approved() {
        let mut rig = Rig::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let request = format!(
            "/usr/bin/curl --silent --show-error --noproxy '*' --connect-timeout 2 --max-time 3 http://{address}/"
        );
        let output = rig.run(&request);
        assert!(!output.status.success(), "offline HTTP unexpectedly worked");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut request = [0; 4096];
                        let count = socket.read(&mut request).unwrap();
                        assert!(count > 0, "HTTP connection did not send a request");
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nallowed").unwrap();
                        return;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "approved HTTP never connected");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("HTTP listener failed: {error}"),
                }
            }
        });
        rig.paths.network = true;
        let output = rig.run(&request);
        server.join().unwrap();
        success(&output);
        assert_eq!(output.stdout, b"allowed");
    }
}
