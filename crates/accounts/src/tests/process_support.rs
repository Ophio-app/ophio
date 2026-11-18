// SPDX-License-Identifier: Apache-2.0
//! Builds authored fake tools once, or validates an explicitly supplied native
//! build. There is no fallback to installed agents, shells or account state.
#![cfg_attr(windows, allow(unsafe_code))]
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const VERSION: &str = "ophio-accounts-test-agent-v2";
const PREBUILT: &str = "OPHIO_ACCOUNTS_TEST_AGENT";

struct Agent {
    executable: PathBuf,
    // The compiled executable must survive until every fixture has copied it.
    _build: Option<tempfile::TempDir>,
}

fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        if let Some(path) = std::env::var_os(PREBUILT) {
            let executable = PathBuf::from(path);
            validate(&executable).unwrap_or_else(|e| panic!("{PREBUILT}: {e}"));
            return Agent { executable, _build: None };
        }
        let build = tempfile::Builder::new().prefix("accounts-test-agent-").tempdir().unwrap();
        let source = build.path().join("agent.rs");
        std::fs::write(&source, include_str!("agent.rs")).unwrap();
        let executable = build.path().join(format!("agent{}", std::env::consts::EXE_SUFFIX));
        let output = Command::new("rustc")
            .arg("--edition=2024")
            .arg("--crate-name=accounts_test_agent")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap_or_else(|e| panic!("compile authored account fixture: {e}; supply {PREBUILT} with a native prebuilt fixture if rustc is unavailable"));
        assert!(output.status.success(), "fixture compilation failed: {}", String::from_utf8_lossy(&output.stderr));
        validate(&executable).unwrap();
        Agent { executable, _build: Some(build) }
    })
}

fn validate(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || !path.is_file() {
        return Err("expected an absolute path to the authored fixture executable".into());
    }
    let output = Command::new(path)
        .arg("--fixture-version")
        .output()
        .map_err(|e| format!("fixture identity check failed: {e}"))?;
    if !identity_matches(output.status.success(), &output.stdout, &output.stderr) {
        return Err("wrong account fixture identity or version".into());
    }
    Ok(())
}

fn identity_matches(success: bool, stdout: &[u8], stderr: &[u8]) -> bool {
    success && stdout == format!("{VERSION}\n").as_bytes() && stderr.is_empty()
}

pub(super) fn executable(path: &Path) -> PathBuf {
    path.with_extension(std::env::consts::EXE_EXTENSION)
}

pub(super) fn put_agent(path: &Path, mode: &str) {
    let path = executable(path);
    if !path.exists() {
        // Only the installer child opens a write handle. Concurrent launches
        // cannot inherit it and see ETXTBSY before their exec closes it.
        let installed = Command::new(&agent().executable)
            .arg("--fixture-install")
            .arg(&path)
            .status()
            .unwrap();
        assert!(
            installed.success(),
            "could not install authored tool {path:?}"
        );
    }
    let config = path.with_extension("fixture");
    let pending = path.with_extension("pending");
    std::fs::write(&pending, mode).unwrap();
    std::fs::rename(pending, config).unwrap();
}

/// Inspection never signals a Unix PID. Only after the containment assertions,
/// Drop asks this authored helper to exit through its unique fixture stop file.
/// Windows retains its pinned process handle for destructive cleanup.
pub(super) struct HelperProcess {
    #[cfg(unix)]
    pid: u32,
    #[cfg(unix)]
    stop: PathBuf,
    #[cfg(windows)]
    handle: *mut std::ffi::c_void,
}

impl HelperProcess {
    pub(super) fn from_file(path: &Path) -> Self {
        let record = std::fs::read_to_string(path).unwrap();
        let mut lines = record.lines();
        let pid: u32 = lines.next().unwrap().parse().unwrap();
        let nonce: u128 = lines.next().unwrap().parse().unwrap();
        assert!(lines.next().is_none(), "invalid authored helper record");
        #[cfg(windows)]
        let _ = nonce;
        Self {
            #[cfg(unix)]
            pid,
            #[cfg(unix)]
            stop: path.with_extension(format!("{pid}.{nonce}.stop")),
            #[cfg(windows)]
            // SAFETY: the PID was recorded by our authored child. The handle
            // pins its identity; only an absent process may be treated as ended.
            handle: unsafe {
                let handle = OpenProcess(0x0010_0000 | 0x1000 | 0x0001, 0, pid);
                if handle.is_null() {
                    assert_eq!(GetLastError(), 87, "could not inspect fixture process {pid}");
                }
                handle
            },
        }
    }

    pub(super) fn running(&self) -> bool {
        #[cfg(target_os = "linux")]
        return std::fs::read_to_string(format!("/proc/{}/stat", self.pid)).is_ok_and(|stat| {
            !stat
                .rsplit(')')
                .next()
                .unwrap_or("")
                .trim_start()
                .starts_with('Z')
        });
        #[cfg(target_os = "macos")]
        {
            let output = Command::new("/bin/ps")
                .args(["-p", &self.pid.to_string(), "-o", "stat="])
                .output()
                .expect("inspect fixture process");
            assert!(
                matches!(output.status.code(), Some(0 | 1)),
                "process inspection failed"
            );
            assert!(output.stderr.is_empty(), "process inspection failed");
            let state = String::from_utf8(output.stdout).unwrap();
            let state = state.trim();
            if output.status.success() {
                assert!(!state.is_empty(), "process inspection returned no state");
                return !state.starts_with('Z');
            }
            assert!(
                state.is_empty(),
                "failed process inspection returned a state"
            );
            false
        }
        #[cfg(windows)]
        // SAFETY: this is our owned process handle; timeout zero never waits.
        {
            if self.handle.is_null() {
                return false;
            }
            let state = unsafe { WaitForSingleObject(self.handle, 0) };
            assert!(matches!(state, 0 | 258), "could not query fixture process");
            state == 258
        }
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.running() {
            // PID reuse can at worst extend this bounded, read-only wait. No
            // PID-based kill is safe here: the process is not our direct child.
            let _ = std::fs::write(&self.stop, b"");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while std::time::Instant::now() < deadline && self.running() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        #[cfg(windows)]
        if !self.handle.is_null() {
            // SAFETY: only the fixture's owned child is terminated. Its handle
            // pins process identity, so PID reuse cannot affect another process.
            unsafe {
                if self.running() {
                    TerminateProcess(self.handle, 1);
                    WaitForSingleObject(self.handle, 5000);
                }
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
    fn TerminateProcess(handle: *mut std::ffi::c_void, code: u32) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn GetLastError() -> u32;
}

#[test]
fn prebuilt_fixture_requires_an_absolute_existing_executable() {
    assert!(validate(Path::new("missing-fixture")).is_err());
    let root = tempfile::tempdir().unwrap();
    assert!(validate(&root.path().join("missing-fixture")).is_err());
}

#[test]
fn prebuilt_fixture_rejects_failed_or_incompatible_identity() {
    let expected = format!("{VERSION}\n");
    assert!(identity_matches(true, expected.as_bytes(), b""));
    assert!(!identity_matches(false, expected.as_bytes(), b""));
    assert!(!identity_matches(
        true,
        b"ophio-accounts-test-agent-v0\n",
        b""
    ));
    assert!(!identity_matches(
        true,
        b"ophio-accounts-test-agent-v1\n",
        b""
    ));
    assert!(!identity_matches(
        true,
        expected.as_bytes(),
        b"unexpected output"
    ));
}

#[test]
fn authored_tools_never_launch_an_inherited_browser() {
    let root = tempfile::tempdir().unwrap();
    put_agent(&root.path().join("claude"), "claude");
    put_agent(&root.path().join("browser-sentinel"), "browser-sentinel");
    let gate = root.path().join("go");
    std::fs::write(&gate, "").unwrap();
    let sentinel = executable(&root.path().join("browser-sentinel"));
    let output = Command::new(executable(&root.path().join("claude")))
        .args(["auth", "login"])
        .env_clear()
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("BROWSER", &sentinel)
        .env("GH_BROWSER", &sentinel)
        .env("TEST_GO", &gate)
        .output()
        .unwrap();
    // Unix refuses anything but Ophio's interceptor. Windows records the
    // request itself. Neither route executes the inherited browser sentinel.
    assert_eq!(output.status.success(), cfg!(windows));
    assert!(!root.path().join("browser-started").exists());
    #[cfg(windows)]
    assert_eq!(
        std::fs::read_to_string(root.path().join("browser-opened")).unwrap(),
        "https://claude.ai/oauth/authorize?code=true\n"
    );
}

#[cfg(unix)]
#[test]
fn unix_cleanup_requests_only_the_matching_helpers_stop_file() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("held.pid");
    let start = || {
        let child = Command::new(&agent().executable)
            .arg("--fixture-hold-output")
            .env("HOME", root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "authored helper did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        let helper = HelperProcess::from_file(&marker);
        std::fs::remove_file(&marker).unwrap();
        (child, helper)
    };
    let (mut first_child, first) = start();
    let (mut second_child, second) = start();
    assert_ne!(first.stop, second.stop);
    assert!(!first.stop.exists());
    assert!(!second.stop.exists());
    assert!(first.running());
    assert!(second.running());
    let first_stop = first.stop.clone();
    drop(first);
    assert!(
        first_stop.exists(),
        "cleanup did not request cooperative exit"
    );
    assert!(
        first_child.try_wait().unwrap().is_some(),
        "helper did not self-exit"
    );
    assert!(
        second_child.try_wait().unwrap().is_none(),
        "cleanup stopped the other helper"
    );
    assert!(!second.stop.exists());
    drop(second);
    assert!(
        second_child.try_wait().unwrap().is_some(),
        "helper did not self-exit"
    );
}

#[test]
fn authored_fixture_reports_its_identity_without_running_an_account_command() {
    validate(&agent().executable).unwrap();
}
