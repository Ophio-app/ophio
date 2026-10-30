// SPDX-License-Identifier: Apache-2.0
//! Device diagnostics cross the same narrow boundary as other native operations.
use crate::{
    error::{Error, Result},
    ipc::AppCore,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use protocol::{
    reports::{
        CrashKind, CrashRecord, MAX_FRAMES, MAX_SCREENSHOT_BYTES, ReportScreenshot, ReportSide,
        StackFrame, scrub,
    },
    time::Timestamp,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    backtrace::Backtrace,
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::{State, ipc::Channel};

pub(crate) fn mobile_reports() -> bool {
    cfg!(mobile) && !cfg!(all(target_os = "ios", target_abi = "sim"))
}

const MAX_PENDING: usize = 16;
const MAX_PENDING_BYTES: u64 = 256 * 1024;

#[derive(Clone)]
pub struct PendingCrashes {
    directory: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl PendingCrashes {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn install(&self, build: String) {
        let previous = std::panic::take_hook();
        let pending = self.clone();
        std::panic::set_hook(Box::new(move |info| {
            let location = info.location();
            let file = location.and_then(|location| repository_file(location.file()));
            let line = location
                .filter(|_| file.is_some())
                .map_or(0, |location| location.line());
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str));
            let mut frames = own_frames(&Backtrace::force_capture().to_string());
            if frames.is_empty()
                && let Some(file) = file.clone()
            {
                frames.push(StackFrame {
                    function: None,
                    file,
                    line,
                    column: location.map(|location| location.column()),
                });
            }
            let record = CrashRecord {
                at: Timestamp::now(),
                side: ReportSide::Phone,
                kind: CrashKind::Panic,
                code: format!("panic:{}:{line}", file.as_deref().unwrap_or("unknown")),
                build: build.clone(),
                message: message.and_then(scrub),
                frames,
            };
            // A panic may abort immediately after this hook; no async work can save it.
            let _ = pending.record(record);
            previous(info);
        }));
    }

    fn path(&self) -> PathBuf {
        self.directory.join("pending-crashes.jsonl")
    }

    fn read(&self) -> std::io::Result<Vec<CrashRecord>> {
        let file = match File::open(self.path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut text = String::new();
        file.take(MAX_PENDING_BYTES).read_to_string(&mut text)?;
        Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .take(MAX_PENDING)
            .collect())
    }

    fn record(&self, record: CrashRecord) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        let mut records = self.read()?;
        records.insert(0, record);
        records.truncate(MAX_PENDING);
        fs::create_dir_all(&self.directory)?;
        let temporary = self.directory.join("pending-crashes.tmp");
        let mut file = File::create(&temporary)?;
        for record in records {
            serde_json::to_writer(&mut file, &record)?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        fs::rename(temporary, self.path())?;
        File::open(&self.directory)?.sync_all()
    }

    fn take(&self) -> std::io::Result<Vec<CrashRecord>> {
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        let records = self.read()?;
        match fs::remove_file(self.path()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(records)
    }
}

fn repository_file(file: &str) -> Option<String> {
    repository_file_in(checkout_root(), file)
}

/// The checkout as this build's own source paths name it. It comes from a
/// shared crate's compiled path, which release builds remap like every other
/// builder folder, so it adds no path the app does not already carry.
fn checkout_root() -> Option<&'static Path> {
    let root = protocol::reports::SOURCE_FILE.strip_suffix("crates/protocol/src/reports.rs")?;
    let root = Path::new(root);
    root.is_absolute().then_some(root)
}

fn repository_file_in(root: Option<&Path>, file: &str) -> Option<String> {
    let path = Path::new(file);
    let relative = if path.is_absolute() {
        path.strip_prefix(root?).ok()?.to_path_buf()
    } else if file.starts_with("src/") {
        Path::new("client/src-tauri").join(path)
    } else {
        path.to_path_buf()
    };
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let name = relative.to_str()?;
    let own = name.starts_with("client/src-tauri/src/")
        || name.starts_with("client/src-tauri/plugins/device/src/")
        || (name.starts_with("crates/") && name.split('/').nth(2) == Some("src"));
    (own && name.ends_with(".rs")).then(|| name.to_owned())
}

fn own_frames(backtrace: &str) -> Vec<StackFrame> {
    backtrace
        .lines()
        .filter_map(|line| {
            let location = line.trim().strip_prefix("at ")?;
            let mut parts = location.rsplitn(3, ':');
            let column = parts.next()?.parse().ok()?;
            let line = parts.next()?.parse().ok()?;
            let file = repository_file(parts.next()?)?;
            Some(StackFrame {
                function: None,
                file,
                line,
                column: Some(column),
            })
        })
        .take(MAX_FRAMES)
        .collect()
}

#[derive(Deserialize, Default)]
struct NativeCrashes {
    crashes: Vec<CrashRecord>,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<i64>,
}
/// The phone's crashes, and when this app was installed or last updated, if
/// the phone says. Nothing from before then is worth sending.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TakenCrashes {
    crashes: Vec<CrashRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<i64>,
}
#[derive(Deserialize)]
struct Supported {
    supported: bool,
}
#[derive(Deserialize)]
struct Picture {
    screenshot: Option<ReportScreenshot>,
}

#[tauri::command]
pub async fn take_native_crashes(
    core: State<'_, AppCore>,
    pending: State<'_, PendingCrashes>,
) -> Result<TakenCrashes> {
    if !mobile_reports() {
        return Ok(TakenCrashes::default());
    }
    let native: NativeCrashes = core
        .device
        .call("takeNativeCrashes", json!({}))
        .await
        .unwrap_or_default();
    let mut records = pending.take().map_err(|_| Error::Storage)?;
    records.extend(native.crashes.into_iter().map(|mut record| {
        record.side = ReportSide::Phone;
        record.kind = CrashKind::System;
        record.message = record.message.as_deref().and_then(scrub);
        record.frames.clear();
        record
    }));
    records.sort_by_key(|record| std::cmp::Reverse(record.at));
    Ok(TakenCrashes {
        crashes: records,
        updated_at: native.updated_at,
    })
}

#[tauri::command]
pub async fn screenshot_supported(core: State<'_, AppCore>) -> Result<bool> {
    Ok(mobile_reports()
        && core
            .device
            .call::<Supported>("screenshotSupported", json!({}))
            .await
            .is_ok_and(|reply| reply.supported))
}

#[tauri::command]
pub async fn start_screenshot_watch(
    core: State<'_, AppCore>,
    events: Channel<serde_json::Value>,
) -> Result<bool> {
    if !mobile_reports() {
        return Ok(false);
    }
    let result: Supported = core
        .device
        .call("startScreenshotWatch", json!({"events":events}))
        .await?;
    Ok(result.supported)
}

#[tauri::command]
pub async fn stop_screenshot_watch(core: State<'_, AppCore>) -> Result<()> {
    if !mobile_reports() {
        return Ok(());
    }
    core.device.call("stopScreenshotWatch", json!({})).await
}

fn valid_picture(picture: &ReportScreenshot) -> bool {
    matches!(
        picture.media_type.as_str(),
        "image/jpeg" | "image/webp" | "image/png"
    ) && picture.width > 0
        && picture.height > 0
        && picture.width <= 4096
        && picture.height <= 4096
        && picture.data.len() <= MAX_SCREENSHOT_BYTES.div_ceil(3) * 4
        && STANDARD
            .decode(&picture.data)
            .is_ok_and(|bytes| !bytes.is_empty() && bytes.len() <= MAX_SCREENSHOT_BYTES)
}

#[tauri::command]
pub async fn capture_own_view(core: State<'_, AppCore>) -> Result<Option<ReportScreenshot>> {
    if !mobile_reports() {
        return Ok(None);
    }
    let result: Picture = core.device.call("captureOwnView", json!({})).await?;
    Ok(result.screenshot.filter(valid_picture))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_hook_writes_before_chaining_in_an_isolated_process() {
        let directory = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "reports::tests::panic_hook_child"])
            .env("PHONE_REPORT_TEST_DIRECTORY", directory.path())
            .status()
            .unwrap();
        assert!(status.success());
        let records = PendingCrashes::new(directory.path().into()).take().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, CrashKind::Panic);
        assert!(
            records[0]
                .code
                .starts_with("panic:client/src-tauri/src/reports.rs:")
        );
        assert_eq!(
            records[0].message.as_deref(),
            Some("read <path> failed for \"…\"")
        );
        assert!(!records[0].frames.is_empty());
        assert!(records[0].frames.len() <= MAX_FRAMES);
    }

    #[test]
    fn panic_hook_child() {
        let Some(directory) = std::env::var_os("PHONE_REPORT_TEST_DIRECTORY") else {
            return;
        };
        let pending = PendingCrashes::new(directory.into());
        let path = pending.path();
        let chained = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = chained.clone();
        std::panic::set_hook(Box::new(move |_| {
            flag.store(path.is_file(), std::sync::atomic::Ordering::SeqCst);
        }));
        pending.install("test".into());
        let _ = std::panic::catch_unwind(|| panic!("read /home/sam/private failed for 'secret'"));
        assert!(chained.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn pending_survives_restart_is_bounded_and_is_taken_once() {
        let directory = tempfile::tempdir().unwrap();
        let pending = PendingCrashes::new(directory.path().into());
        for at in 0..20 {
            pending
                .record(CrashRecord {
                    at: Timestamp(at),
                    side: ReportSide::Phone,
                    kind: CrashKind::Panic,
                    code: "panic:client/src-tauri/src/lib.rs:1".into(),
                    build: "test".into(),
                    message: scrub("read /home/sam/private failed for 'secret'"),
                    frames: vec![],
                })
                .unwrap();
        }
        let reopened = PendingCrashes::new(directory.path().into());
        let records = reopened.take().unwrap();
        assert_eq!(records.len(), MAX_PENDING);
        assert_eq!(records[0].at, Timestamp(19));
        assert_eq!(
            records[0].message.as_deref(),
            Some("read <path> failed for \"…\"")
        );
        assert!(reopened.take().unwrap().is_empty());
    }

    #[test]
    fn frames_keep_only_repository_sources_and_never_host_paths() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let frames = own_frames(&format!(
            "at {manifest}/src/lib.rs:10:2\nat /home/sam/.cargo/registry/src/library.rs:20:3\nat /private/file.rs:1:2\nat crates/protocol/src/reports.rs:5:4"
        ));
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].file, "client/src-tauri/src/lib.rs");
        assert_eq!(frames[1].file, "crates/protocol/src/reports.rs");
        assert!(repository_file("crates/../private/src/file.rs").is_none());
        assert_eq!(
            own_frames(&"at src/lib.rs:1:2\n".repeat(100)).len(),
            MAX_FRAMES
        );
    }

    #[test]
    fn the_checkout_root_is_the_one_this_build_compiled() {
        // Tests are not remapped, so the shared crate names the real checkout.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(checkout_root(), manifest.parent().and_then(Path::parent));
    }

    #[test]
    fn remapped_release_frames_keep_only_repository_sources() {
        let root = Some(Path::new("/src/target/packaging/android/project/"));
        let file = |path: &str| repository_file_in(root, path);
        assert_eq!(
            file("/src/target/packaging/android/project/crates/protocol/src/reports.rs").as_deref(),
            Some("crates/protocol/src/reports.rs")
        );
        assert_eq!(
            file("/src/target/packaging/android/project/client/src-tauri/src/lib.rs").as_deref(),
            Some("client/src-tauri/src/lib.rs")
        );
        assert_eq!(
            file("src/alerts/android.rs").as_deref(),
            Some("client/src-tauri/src/alerts/android.rs")
        );
        for outside in [
            "/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tauri-2.12.0/src/lib.rs",
            "/home/sam/ophio/crates/protocol/src/reports.rs",
            "/src/target/packaging/android/project/../private/crates/x/src/y.rs",
            "/src/target/packaging/android/project/crates/protocol/build.rs",
            "/src/target/packaging/android/project/crates/protocol/src/data.json",
        ] {
            assert!(file(outside).is_none(), "{outside}");
        }
        // With no known checkout, no absolute path is trusted.
        assert!(
            repository_file_in(
                None,
                "/src/target/packaging/android/project/crates/protocol/src/reports.rs"
            )
            .is_none()
        );
        assert_eq!(
            repository_file_in(None, "src/lib.rs").as_deref(),
            Some("client/src-tauri/src/lib.rs")
        );
    }

    /// The text outside inline `#[cfg(test)] mod tests { … }` blocks. Sources
    /// are rustfmt-formatted, so a block ends at the next line holding only the
    /// closing brace at the module's own indentation; code after it is kept.
    fn without_test_modules(text: &str) -> String {
        let lines: Vec<&str> = text.lines().collect();
        let mut kept = String::new();
        let mut end: Option<String> = None;
        for (index, line) in lines.iter().enumerate() {
            if let Some(close) = &end {
                if line == close {
                    end = None;
                }
                continue;
            }
            if line.trim() == "#[cfg(test)]"
                && lines
                    .get(index + 1)
                    .is_some_and(|next| next.trim() == "mod tests {")
            {
                let indent = &line[..line.len() - line.trim_start().len()];
                end = Some(format!("{indent}}}"));
                continue;
            }
            kept.push_str(line);
            kept.push('\n');
        }
        kept
    }

    #[test]
    fn test_modules_are_skipped_but_code_after_them_is_kept() {
        let text = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() { \"x\" }\n}\nfn b() {}\n";
        assert_eq!(without_test_modules(text), "fn a() {}\nfn b() {}\n");
        let nested = "mod m {\n    #[cfg(test)]\n    mod tests {\n    }\n    fn c() {}\n}\n";
        assert_eq!(without_test_modules(nested), "mod m {\n    fn c() {}\n}\n");
    }

    #[test]
    fn shipped_phone_code_compiles_in_no_build_folder() {
        // env!("CARGO_MANIFEST_DIR") outside tests puts the builder's real
        // folder into the app; path remapping does not reach it. These are the
        // phone shell's own sources and the shared crates compiled into it.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let crates = manifest.join("../../crates");
        let mut pending = vec![
            manifest.join("src"),
            manifest.join("plugins/device/src"),
            crates.join("protocol/src"),
            crates.join("ports/src"),
        ];
        let mut checked = 0;
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if path.is_dir() {
                    if name != "tests" {
                        pending.push(path);
                    }
                } else if name.ends_with(".rs")
                    && name != "tests.rs"
                    && !name.ends_with("_tests.rs")
                {
                    let text = fs::read_to_string(&path).unwrap();
                    let shipped = without_test_modules(&text);
                    assert!(
                        !shipped.contains("\"CARGO_MANIFEST_DIR\""),
                        "{} names the build folder outside its tests",
                        path.display()
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 20);
    }

    #[test]
    fn screenshot_bytes_are_checked_after_base64_decoding() {
        let mut picture = ReportScreenshot {
            media_type: "image/jpeg".into(),
            data: STANDARD.encode(vec![1; MAX_SCREENSHOT_BYTES]),
            width: 500,
            height: 1000,
            marked: false,
        };
        assert!(valid_picture(&picture));
        picture.data = STANDARD.encode(vec![1; MAX_SCREENSHOT_BYTES + 1]);
        assert!(!valid_picture(&picture));
        picture.data = "invalid!".into();
        assert!(!valid_picture(&picture));
    }
}
