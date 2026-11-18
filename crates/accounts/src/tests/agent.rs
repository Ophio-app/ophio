// SPDX-License-Identifier: Apache-2.0
//! Authored account tools. Compiled as a standalone, dependency-free executable
//! by the test harness, or supplied prebuilt for a native test-only machine.
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::{Duration, Instant};

const VERSION: &str = "ophio-accounts-test-agent-v2";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--fixture-version"] {
        println!("{VERSION}");
        return;
    }
    if args.len() == 2 && args[0] == "--fixture-install" {
        let destination = PathBuf::from(&args[1]);
        let pending = destination.with_extension("installing");
        // Only this child holds the write handle. Concurrent tool launches in
        // the test process cannot inherit it and cause Unix ETXTBSY failures.
        fs::copy(env::current_exe().unwrap(), &pending).unwrap();
        fs::rename(pending, destination).unwrap();
        return;
    }
    if args == ["--fixture-flood-stderr"] {
        start_helper("err.pid");
        flood(&mut io::stderr().lock(), b"err\n");
        return;
    }
    if args == ["--fixture-hold-output"] {
        start_helper("held.pid");
        // The helper's own watchdog exits, even if its parent or test vanished.
        std::thread::sleep(Duration::from_secs(60));
        return;
    }
    let exe = env::current_exe().unwrap();
    let mode = fs::read_to_string(exe.with_extension("fixture")).unwrap();
    let success = run(mode.trim(), &args);
    // Never let Rust's own buffered shutdown keep a pipe alive.
    io::stdout().flush().unwrap();
    io::stderr().flush().unwrap();
    process::exit(if success { 0 } else { 1 });
}

fn home() -> PathBuf {
    PathBuf::from(env::var_os("HOME").expect("fixture HOME"))
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "fixture gate did not open: {path:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn start_helper(marker: &str) {
    let marker = home().join(marker);
    let pid = process::id();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let stop = marker.with_extension(format!("{pid}.{nonce}.stop"));
    // Publish a complete record before the leader announces readiness.
    let pending = marker.with_extension("pending");
    fs::write(&pending, format!("{pid}\n{nonce}\n")).unwrap();
    fs::rename(pending, marker).unwrap();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !stop.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // This exits only this authored helper. A separate thread also bounds
        // a flood blocked in a pipe write; it never signals another PID.
        process::exit(0);
    });
}

fn wait_for_pid(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while !fs::read_to_string(path).is_ok_and(|record| {
        let mut lines = record.lines();
        lines.next().is_some_and(|pid| pid.parse::<u32>().is_ok())
            && lines.next().is_some_and(|nonce| nonce.parse::<u128>().is_ok())
            && lines.next().is_none()
    }) {
        assert!(Instant::now() < deadline, "fixture child did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn flood(writer: &mut dyn Write, text: &[u8]) {
    let bytes = text.repeat(2048);
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline && writer.write_all(&bytes).is_ok() {}
}

fn go() {
    wait_for(&PathBuf::from(
        env::var_os("TEST_GO").expect("fixture gate"),
    ));
}

fn line() -> Option<String> {
    let mut line = String::new();
    (io::stdin().lock().read_line(&mut line).unwrap() != 0)
        .then(|| line.trim_end_matches(['\r', '\n']).to_owned())
}

fn say(text: &str) {
    print!("{text}");
    io::stdout().flush().unwrap();
}

/// Unix exercises Ophio's real browser interceptor. Windows has no interceptor:
/// the fake tool records its own request instead of invoking a live browser.
fn browser(url: &str, variable: &str) {
    #[cfg(unix)]
    {
        let shim = PathBuf::from(env::var_os(variable).expect("browser interceptor"));
        // Refuse inherited browser settings: only Ophio's scratch shim is safe.
        assert_eq!(shim.file_name().unwrap(), "xdg-open");
        assert_eq!(shim.parent().unwrap().file_name().unwrap(), "signin");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match Command::new(&shim).arg(url).status() {
                Ok(status) => {
                    assert!(status.success());
                    break;
                }
                // A concurrent test's fork can briefly inherit the interceptor's
                // write handle before exec closes it. Shells retry ETXTBSY too;
                // retry only that error, never a missing or refused interceptor.
                Err(e) if e.raw_os_error() == Some(26) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("authored browser interceptor did not start: {e}"),
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = variable;
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(home().join("browser-opened"))
            .unwrap();
        writeln!(log, "{url}").unwrap();
        say(&format!("{url}\n"));
    }
}

fn run(mode: &str, args: &[String]) -> bool {
    let command = args
        .iter()
        .take(2)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    match mode {
        "browser-sentinel" => {
            fs::write(home().join("browser-started"), "").unwrap();
            true
        }
        "flood" | "flood-login" => {
            if mode == "flood-login" && command != "auth login" {
                return false;
            }
            let _child = Command::new(env::current_exe().unwrap())
                .arg("--fixture-flood-stderr")
                .stdin(Stdio::null())
                .spawn()
                .unwrap();
            wait_for_pid(&home().join("err.pid"));
            flood(&mut io::stdout().lock(), b"out\n");
            true
        }
        "chatty" => {
            let _ = io::stdout().write_all(&vec![b'x'; 3_000_000]);
            true
        }
        "slow-status" => {
            std::thread::sleep(Duration::from_millis(400));
            say("{\"loggedIn\":true}\n");
            true
        }
        "path" => {
            say(&env::var("PATH").unwrap());
            true
        }
        "holds-output" => {
            let mut command = Command::new(env::current_exe().unwrap());
            command.arg("--fixture-hold-output").stdin(Stdio::null());
            // Leave the leader's process group. A cgroup or Job still holds
            // the helper; a process-group-only backend must bound pipe reads.
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            let _child = command.spawn().unwrap();
            wait_for_pid(&home().join("held.pid"));
            say("done\n");
            true
        }
        "codex" => {
            eprintln!("Not logged in");
            false
        }
        "codex-phone" => match command.as_str() {
            "login --device-auth" => {
                say(
                    "\nWelcome to Codex [v\x1b[90m0.164.0\x1b[0m]\n\nFollow these steps to sign in with ChatGPT using device code authorization:\n\n1. Open this link in your browser and sign in to your account\n   \x1b[94mhttps://auth.openai.com/codex/device\x1b[0m\n\n2. Enter this one-time code \x1b[90m(expires in 15 minutes)\x1b[0m\n   \x1b[94mAB12-CDE34\x1b[0m\n\n",
                );
                if env::var_os("TEST_OFF").is_some_and(|p| PathBuf::from(p).exists()) {
                    eprintln!(
                        "Error logging in with device code: device auth failed with status 403 Forbidden"
                    );
                    return false;
                }
                go();
                fs::write(home().join("codex-in"), "").unwrap();
                eprintln!("Successfully logged in");
                true
            }
            "login status" => {
                let active = home().join("codex-in").exists();
                eprintln!(
                    "{}",
                    if active {
                        "Logged in using ChatGPT"
                    } else {
                        "Not logged in"
                    }
                );
                active
            }
            _ => false,
        },
        mode if mode.starts_with("claude") => claude(mode, &command),
        mode if mode.starts_with("gh") => github(mode, &command, args),
        _ => panic!("unknown fixture mode {mode:?}"),
    }
}

fn claude(mode: &str, command: &str) -> bool {
    let state = home().join("claude-in");
    match command {
        "auth login" => {
            if mode == "claude-phone" {
                browser(
                    "https://claude.com/cai/oauth/authorize?code=true&redirect_uri=http%3A%2F%2Flocalhost%3A40401%2Fcallback&state=s1",
                    "BROWSER",
                );
                let link = "https://claude.com/cai/oauth/authorize?code=true&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=s1";
                say(&format!(
                    "Opening browser to sign in…\r\nIf the browser didn't open, visit: \x1b]8;;{link}\x07{link}\x1b]8;;\x07\r\nPaste code here if prompted > "
                ));
                while let Some(code) = line() {
                    let mut file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(home().join("claude-codes"))
                        .unwrap();
                    writeln!(file, "{code}").unwrap();
                    if code == "good-code#s1" {
                        fs::write(state, "").unwrap();
                        say("Login successful.\r\n");
                        return true;
                    }
                    eprintln!("Invalid code. Please make sure the full code was copied.\r");
                }
                false
            } else {
                say("Opening your browser to sign in...\n");
                browser("https://claude.ai/oauth/authorize?code=true", "BROWSER");
                go();
                fs::write(state, "").unwrap();
                mode != "claude-failed-login"
            }
        }
        "auth status" => {
            if mode == "claude-held-status" {
                if !home().join("started").exists() {
                    fs::write(home().join("started"), "").unwrap();
                    go();
                    fs::write(home().join("finished"), "").unwrap();
                }
                say("{\"loggedIn\":true}\n");
            } else if state.exists() {
                say("{\"loggedIn\":true,\"email\":\"me@example.com\"}\n");
            } else {
                say("{\"loggedIn\":false}\n");
            }
            true
        }
        "auth logout" => {
            if mode == "claude-failed-logout" {
                return false;
            }
            let _ = fs::remove_file(state);
            true
        }
        _ => false,
    }
}

fn github(mode: &str, command: &str, args: &[String]) -> bool {
    let state =
        PathBuf::from(env::var_os("GH_CONFIG_DIR").expect("private gh config")).join("hosts.yml");
    match command {
        "auth login" => {
            say(
                "! First copy your one-time code: ABCD-1234\nPress Enter to open https://github.com/login/device in your browser... ",
            );
            if line().is_none() {
                return false;
            }
            browser("https://github.com/login/device", "GH_BROWSER");
            go();
            fs::write(state, format!("user: octo\nscopes: {}\n", args.join(" "))).unwrap();
            true
        }
        "auth status" => {
            if state.exists() {
                let scopes = if mode == "gh-scopes" {
                    let workflow = fs::read_to_string(&state)
                        .unwrap()
                        .contains("--scopes workflow");
                    format!(
                        ",\"scopes\":\"gist, read:org, repo{}\"",
                        if workflow { ", workflow" } else { "" }
                    )
                } else {
                    String::new()
                };
                say(&format!(
                    "{{\"hosts\":{{\"github.com\":[{{\"state\":\"success\",\"active\":true,\"login\":\"octo\"{scopes}}}]}}}}\n"
                ));
                true
            } else {
                say("{\"hosts\":{}}\n");
                false
            }
        }
        "auth token" => {
            if !state.exists() {
                return false;
            }
            if mode == "gh-held-token" {
                if home().join("hold").exists() {
                    fs::remove_file(home().join("hold")).unwrap();
                    fs::write(home().join("held"), "").unwrap();
                    wait_for(&home().join("release"));
                    say("gho_Before0123456789\n");
                } else {
                    say("gho_After01234567890\n");
                }
            } else {
                say("gho_TestToken0123456789\n");
            }
            true
        }
        "auth logout" => {
            if mode == "gh-held-logout" {
                go();
            }
            let _ = fs::remove_file(state);
            true
        }
        _ => false,
    }
}
