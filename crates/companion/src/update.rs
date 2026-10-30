// SPDX-License-Identifier: Apache-2.0
//! Installing Øphio on this computer, and updating it from a later download.
//!
//! An install lives in `~/.local/opt/ophio` and keeps only this there:
//!
//! - `bin/`: the programs the login service runs;
//! - `app/`: the phone apps this computer offers its devices;
//! - `previous/`: the install before the last update, to go back to;
//! - `installed.json`, `last-update.json` and `update.log`, which is kept short.
//!
//! A release is copied into `staging/` and checked there first. An update
//! waits until no task is running, swaps the programs while Øphio is stopped,
//! starts it again and checks that it came back; if it did not, the previous
//! programs go back. Whatever happens, it then removes what it staged and what
//! any interrupted update left, so that old builds never pile up.
use crate::{Error, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, clap::Args)]
pub struct InstallArgs {
    /// Install without waiting for running tasks to finish. Øphio stops
    /// them, as it does when it is restarted.
    #[arg(long)]
    pub now: bool,
    /// Don't start setup after a first install.
    #[arg(long)]
    pub no_setup: bool,
    /// Carries out an update already staged. Øphio runs this itself.
    #[arg(long, hide = true, value_name = "ID")]
    pub apply: Option<String>,
}

#[derive(Debug, Clone, Default, clap::Args)]
pub struct UpdateArgs {
    /// A downloaded Øphio release: its .tar.gz, or the folder it was
    /// extracted to. Without one, shows what is installed.
    #[arg(value_name = "DOWNLOAD", conflicts_with_all = ["rollback", "cancel"])]
    pub download: Option<PathBuf>,
    /// Install without waiting for running tasks to finish. Øphio stops
    /// them, as it does when it is restarted.
    #[arg(long)]
    pub now: bool,
    /// Go back to the version installed before the last update.
    #[arg(long, conflicts_with = "cancel")]
    pub rollback: bool,
    /// Stop an update that is waiting for Øphio to be idle.
    #[arg(long)]
    pub cancel: bool,
    /// Print what is installed as JSON, for scripts.
    #[arg(long)]
    pub json: bool,
}

impl UpdateArgs {
    /// Whether this changes the install, rather than only showing it.
    pub(crate) fn changes(&self) -> bool {
        self.download.is_some() || self.rollback || self.cancel
    }
}

pub async fn install(args: InstallArgs) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::install(args).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err(unsupported())
    }
}

pub async fn update(args: UpdateArgs) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::update(args).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err(unsupported())
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> Error {
    Error::Unsupported(
        "Installing and updating with this command is for Linux. On Windows, replace ophio.exe and computer-mcp.exe with the new download's while Øphio is stopped.",
    )
}

/// Finish an interrupted replacement before the installed command (including
/// the login service) reads its sibling programs or install metadata.
pub fn recover_on_start() -> Result<()> {
    #[cfg(target_os = "linux")]
    linux::recover_on_start()?;
    Ok(())
}

/// The phone apps an install offers its devices: `app/*.apk` beside `bin/`.
/// There is one app per signing key, so that each device is offered the one
/// it can update itself with; an app that isn't signed for that is skipped.
pub(crate) fn app_offers(program_dir: &Path) -> Vec<(protocol::capability::AppOffer, PathBuf)> {
    let Some(opt) = program_dir.parent() else {
        return Vec::new();
    };
    let mut apps: Vec<PathBuf> = std::fs::read_dir(opt.join("app"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|e| e == "apk")
                && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
        })
        .collect();
    apps.sort();
    apps.into_iter()
        .filter_map(|path| {
            let Ok(Ok(signers)) = crate::phone::apk::signers(&path) else {
                tracing::warn!(app = %path.display(), "not offering an app that isn't signed for updates");
                return None;
            };
            // Each signing scheme names the signer again; the offer names it once.
            let mut named: Vec<String> = Vec::new();
            for signer in signers.iter().map(|s| hex(s)) {
                if !named.contains(&signer) {
                    named.push(signer);
                }
            }
            let offer = protocol::capability::AppOffer {
                sha256: sha256(&path).ok()?,
                size: std::fs::metadata(&path).ok()?.len(),
                signers: named,
                version: env!("CARGO_PKG_VERSION").into(),
            };
            Some((offer, path))
        })
        .collect()
}

pub(crate) fn sha256(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok(hex(&hash.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use super::*;
    use crate::{
        config::{APP_NAME, Paths},
        identity::private_dir,
        local::OwnerClient,
    };
    use ports::COMMAND;
    use protocol::{
        Query, QueryData,
        model::{ApprovalState, ChildState, RunState, Snapshot, TaskState},
    };
    use serde::{Deserialize, Serialize};
    use std::{
        cell::Cell,
        io::{Read, Seek, SeekFrom, Write},
        path::Component,
        rc::Rc,
        time::Duration,
    };
    // Tokio's, so that tests can let time pass.
    use tokio::time::Instant;

    pub(crate) const PROGRAMS: [&str; 3] = ["ophio", "computer-mcp", "ophio-sandbox-shim"];
    /// How long Øphio must have had nothing running before an update stops it.
    const IDLE_FOR: Duration = Duration::from_secs(60);
    const IDLE_POLL: Duration = Duration::from_secs(15);
    /// An update that never finds a quiet minute stops waiting after this.
    const GIVE_UP: Duration = Duration::from_secs(48 * 3600);
    /// How long Øphio has to answer again after an update starts it.
    const HEALTH_WAIT: Duration = Duration::from_secs(90);
    /// How long it must then keep running to count as working.
    const STEADY: Duration = Duration::from_secs(15);
    /// How long the update lock is waited for before another update is
    /// taken to hold it.
    const LOCK_WAIT: Duration = Duration::from_secs(2);
    /// How much of `update.log` is kept.
    const LOG_KEEP: u64 = 256 << 10;
    const UNPACKED_LIMIT: u64 = 2 << 30;
    const ENTRY_LIMIT: usize = 4096;
    const METADATA_LIMIT: u64 = 1 << 20;

    fn unit() -> String {
        format!("{APP_NAME}-update")
    }

    /// Where an install's files go. Fixed under the home directory, as the
    /// archive's install.sh always put them.
    #[derive(Debug, Clone)]
    pub(crate) struct Layout {
        pub(crate) opt: PathBuf,
        links: PathBuf,
        icons: PathBuf,
    }

    impl Layout {
        pub(crate) fn discover() -> Result<Self> {
            let base = directories::BaseDirs::new()
                .ok_or_else(|| Error::Config("home directory unavailable".into()))?;
            let home = base.home_dir();
            Ok(Self {
                opt: home.join(".local/opt").join(APP_NAME),
                links: home.join(".local/bin"),
                icons: base.data_dir().join("icons/hicolor"),
            })
        }
        pub(crate) fn bin(&self) -> PathBuf {
            self.opt.join("bin")
        }
        fn app(&self) -> PathBuf {
            self.opt.join("app")
        }
        fn previous(&self) -> PathBuf {
            self.opt.join("previous")
        }
        /// `previous` while it is being put together.
        fn partial(&self) -> PathBuf {
            self.opt.join("previous.partial")
        }
        fn staging(&self) -> PathBuf {
            self.opt.join("staging")
        }
        fn installed(&self) -> PathBuf {
            self.opt.join("installed.json")
        }
        fn last(&self) -> PathBuf {
            self.opt.join("last-update.json")
        }
        fn log(&self) -> PathBuf {
            self.opt.join("update.log")
        }
        /// Held for as long as an update changes the install. A program
        /// started while it was held keeps it until its own program runs, so
        /// one released a moment ago is waited for briefly.
        fn lock(&self) -> Result<std::fs::File> {
            private_dir(&self.opt)?;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.opt.join("update.lock"))?;
            let deadline = std::time::Instant::now() + LOCK_WAIT;
            loop {
                match file.try_lock() {
                    Ok(()) => return Ok(file),
                    Err(std::fs::TryLockError::WouldBlock)
                        if std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(std::fs::TryLockError::WouldBlock) => {
                        return Err(Error::Ended(format!(
                            "Another update of Øphio is under way. `{COMMAND} update` shows how it is going."
                        )));
                    }
                    Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
                }
            }
        }
    }

    /// One install of Øphio, as `installed.json` and each staged release
    /// record it.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) struct Release {
        pub(crate) version: String,
        /// The start of the `ophio` program's SHA-256, which tells builds of
        /// one version apart.
        pub(crate) build: String,
        /// Seconds since 1970; 0 until installed.
        #[serde(default)]
        pub(crate) installed_at: i64,
        /// The phone apps it offers, by file name.
        #[serde(default)]
        pub(crate) apps: Vec<String>,
    }

    impl Release {
        fn named(&self) -> String {
            format!("Øphio {} (build {})", self.version, self.build)
        }
    }

    /// The log's first line for replacing one install with another; going
    /// back to an earlier version is not called updating.
    fn changing(from: &Release, to: &Release, rollback: bool) -> String {
        let numbers = |version: &str| -> Vec<u64> {
            let release = version.split(['-', '+']).next().unwrap_or_default();
            release.split('.').map(|n| n.parse().unwrap_or(0)).collect()
        };
        if rollback || numbers(&to.version) < numbers(&from.version) {
            format!("Going back from {} to {}.", from.named(), to.named())
        } else {
            format!("Updating {} to {}.", from.named(), to.named())
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub(crate) enum Result_ {
        Installed,
        /// Installed, then put back: the new version didn't come back working.
        RolledBack,
        /// Nothing changed.
        NotInstalled,
        Cancelled,
    }

    /// How the last update ended, in `last-update.json`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) struct Outcome {
        pub(crate) id: String,
        pub(crate) result: Result_,
        pub(crate) release: Release,
        pub(crate) from: Option<Release>,
        pub(crate) at: i64,
        pub(crate) message: String,
    }

    /// What the running Øphio looked like before an update stopped it.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct Before {
        /// Øphio answered on its owner socket.
        pub(crate) running: bool,
        /// It runs as the login service, which is active.
        pub(crate) service: bool,
        /// Addresses beyond this computer it listened on, which it must
        /// listen on again.
        pub(crate) listeners: Vec<String>,
        pub(crate) boot: Option<String>,
    }

    /// What an update needs from this computer, apart from files.
    #[allow(async_fn_in_trait)]
    pub(crate) trait Host {
        /// The version a program says it is, which also shows it runs here.
        async fn version(&self, program: &Path) -> Result<String>;
        async fn before(&self) -> Result<Before>;
        /// Why Øphio is busy now; empty when it is idle.
        async fn busy(&self) -> Result<Vec<String>>;
        async fn stop(&self, before: &Before) -> Result<()>;
        async fn start(&self, before: &Before, program: &Path) -> Result<()>;
        /// Whether Øphio came back as it was, and if not, why not.
        async fn healthy(&self, before: &Before) -> std::result::Result<(), String>;
        /// Rewrites the login service's unit for `program`, if it runs as
        /// one; what to say about it, if anything.
        async fn refresh_unit(&self, program: &Path) -> Result<Option<String>>;
    }

    /// Lines for `update.log`, also printed.
    pub(crate) struct Log {
        file: Option<std::fs::File>,
    }

    impl Log {
        pub(crate) fn open(layout: &Layout) -> Self {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(layout.log())
                .ok();
            Self { file }
        }
        pub(crate) fn line(&mut self, text: impl AsRef<str>) {
            let text = text.as_ref();
            println!("{text}");
            if let Some(file) = &mut self.file {
                let _ = writeln!(file, "{} {text}", stamp(now()));
            }
        }
    }

    pub(crate) fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    }

    fn stamp(at: i64) -> String {
        let Ok(time) = time::OffsetDateTime::from_unix_timestamp(at) else {
            return at.to_string();
        };
        let time =
            time::UtcOffset::current_local_offset().map_or(time, |offset| time.to_offset(offset));
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            time.year(),
            u8::from(time.month()),
            time.day(),
            time.hour(),
            time.minute(),
            time.second()
        )
    }

    fn ago(at: i64) -> String {
        let seconds = (now() - at).max(0);
        match seconds {
            0..60 => "just now".into(),
            60..3600 => plural(seconds / 60, "minute"),
            3600..86400 => plural(seconds / 3600, "hour"),
            _ => plural(seconds / 86400, "day"),
        }
    }

    fn plural(n: i64, unit: &str) -> String {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    }

    fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
        let text = serde_json::to_vec_pretty(value)
            .map_err(|_| Error::Config("cannot record the install".into()))?;
        let partial = path.with_extension("json.new");
        std::fs::write(&partial, text)?;
        std::fs::rename(&partial, path)?;
        Ok(())
    }

    /// What is installed now. An install from before `installed.json` is
    /// described by its programs.
    fn installed(layout: &Layout, host_version: Option<&str>) -> Option<Release> {
        if let Some(release) = read_json(&layout.installed()) {
            return Some(release);
        }
        let program = layout.bin().join("ophio");
        let build = sha256(&program).ok()?;
        Some(Release {
            version: host_version.unwrap_or("unknown").into(),
            build: build[..12].into(),
            installed_at: std::fs::metadata(&program)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64),
            apps: Vec::new(),
        })
    }

    fn remove_all(path: &Path) -> std::io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
            Ok(_) => std::fs::remove_file(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn regular(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
    }

    fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }

    // ---- Staging ----

    /// Copies a release into `staging/<id>` and checks it there. A release
    /// is the folder a download was extracted to, or the download itself.
    pub(crate) async fn stage(
        layout: &Layout,
        source: &Path,
        host: &impl Host,
    ) -> Result<(String, Release)> {
        let id = format!("{}-{}", now(), std::process::id());
        let root = layout.staging().join(&id);
        private_dir(&root)?;
        let staged = async {
            if source.is_dir() {
                gather(source, &root, false)?;
            } else {
                let unpacked = root.join("download");
                let top = unpack(source, &unpacked)?;
                gather(&top, &root, true)?;
                remove_all(&unpacked)?;
            }
            let version = host.version(&root.join("bin/ophio")).await?;
            let build = sha256(&root.join("bin/ophio"))?[..12].to_owned();
            let apps = std::fs::read_dir(root.join("app"))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect::<Vec<_>>();
            let release = Release {
                version,
                build,
                installed_at: 0,
                apps,
            };
            write_json(&root.join("release.json"), &release)?;
            Ok::<_, Error>(release)
        }
        .await;
        match staged {
            Ok(release) => Ok((id, release)),
            Err(error) => {
                let _ = remove_all(&root);
                let _ = std::fs::remove_dir(layout.staging());
                Err(error)
            }
        }
    }

    /// Puts a release's programs, phone apps and icons in `root`'s `bin`,
    /// `app` and `icons`, moving them when `owned`, else copying.
    fn gather(from: &Path, root: &Path, owned: bool) -> Result<()> {
        let place = |source: &Path, target: &Path, mode: u32| -> Result<()> {
            if let Some(parent) = target.parent() {
                private_dir(parent)?;
            }
            if owned {
                std::fs::rename(source, target)?;
            } else {
                std::fs::copy(source, target)?;
            }
            set_mode(target, mode)?;
            Ok(())
        };
        // Programs sit at the top of a download, and in `bin/` in an
        // install going back to its previous version.
        let earlier = from.join("bin/ophio").is_file() && !from.join("ophio").exists();
        let programs = if earlier {
            from.join("bin")
        } else {
            from.to_owned()
        };
        for program in PROGRAMS {
            // An install from before the sandbox shim existed has none to go back to.
            if earlier && program == "ophio-sandbox-shim" {
                continue;
            }
            let source = programs.join(program);
            if !regular(&source) || !elf(&source) {
                return Err(Error::Ended(format!(
                    "{} isn't an Øphio download for Linux: it has no {program} program.",
                    from.display()
                )));
            }
        }
        for program in PROGRAMS.into_iter().filter(|p| programs.join(p).exists()) {
            place(
                &programs.join(program),
                &root.join("bin").join(program),
                0o755,
            )?;
        }
        // Phone apps sit beside the programs in a download, or in `app/` in
        // an install going back to its previous version.
        for folder in [from.to_owned(), from.join("app")] {
            for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                let path = entry.path();
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if name.starts_with("ophio") && name.ends_with(".apk") && regular(&path) {
                    place(&path, &root.join("app").join(&name), 0o644)?;
                }
            }
        }
        let icons = from.join("icons/hicolor");
        for size in std::fs::read_dir(&icons).into_iter().flatten().flatten() {
            let apps = size.path().join("apps");
            for entry in std::fs::read_dir(&apps).into_iter().flatten().flatten() {
                let path = entry.path();
                let name = entry.file_name();
                if name.to_string_lossy().starts_with("ophio.") && regular(&path) {
                    let target = root
                        .join("icons/hicolor")
                        .join(size.file_name())
                        .join("apps")
                        .join(name);
                    place(&path, &target, 0o644)?;
                }
            }
        }
        Ok(())
    }

    fn elf(path: &Path) -> bool {
        let mut magic = [0; 4];
        std::fs::File::open(path)
            .and_then(|mut f| f.read_exact(&mut magic))
            .is_ok()
            && magic == *b"\x7fELF"
    }

    const UNSAFE: &str = "The download couldn't be unpacked safely, so it wasn't used.";

    /// Unpacks a release's `.tar.gz` into `into`, within limits, and returns
    /// its one top-level folder.
    fn unpack(archive: &Path, into: &Path) -> Result<PathBuf> {
        use crate::agent_updates::Budgeted;
        let unsafe_ = || Error::Ended(UNSAFE.into());
        let file = std::fs::File::open(archive)
            .map_err(|e| Error::Ended(format!("Couldn't open {}: {e}", archive.display())))?;
        let allowance = Rc::new(Cell::new(0));
        let mut tar = tar::Archive::new(Budgeted {
            inner: flate2::read::GzDecoder::new(file),
            allowance: allowance.clone(),
        });
        private_dir(into)?;
        let mut top: Option<PathBuf> = None;
        let mut total = 0u64;
        let mut entries = tar.entries().map_err(|_| unsafe_())?;
        for count in 0.. {
            allowance.set(METADATA_LIMIT);
            let Some(entry) = entries.next() else { break };
            if count >= ENTRY_LIMIT {
                return Err(unsafe_());
            }
            let mut entry = entry.map_err(|_| unsafe_())?;
            let kind = entry.header().entry_type();
            if kind.is_pax_global_extensions() {
                continue;
            }
            let path = entry.path().map_err(|_| unsafe_())?.into_owned();
            if !path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            {
                return Err(unsafe_());
            }
            let mut parts = path.components();
            let Some(Component::Normal(first)) = parts.next() else {
                return Err(unsafe_());
            };
            match &top {
                Some(top) if top.as_os_str() != first => return Err(unsafe_()),
                Some(_) => {}
                None => top = Some(PathBuf::from(first)),
            }
            let target = into.join(&path);
            if kind.is_dir() {
                private_dir(&target)?;
                continue;
            }
            if !kind.is_file() || parts.as_path().as_os_str().is_empty() {
                return Err(unsafe_());
            }
            let size = entry.size();
            total += size;
            if total > UNPACKED_LIMIT {
                return Err(unsafe_());
            }
            if let Some(parent) = target.parent() {
                private_dir(parent)?;
            }
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|_| unsafe_())?;
            allowance.set(size);
            let copied =
                std::io::copy(&mut (&mut entry).take(size), &mut out).map_err(|_| unsafe_())?;
            if copied != size {
                return Err(unsafe_());
            }
        }
        top.map(|top| into.join(top)).ok_or_else(unsafe_)
    }

    // ---- Applying ----

    /// Carries out the update staged as `id`: waits for Øphio to be idle,
    /// swaps the programs, starts it again and checks it, going back if it
    /// fails. Removes everything staged however it ends.
    pub(crate) async fn apply(
        layout: &Layout,
        id: &str,
        now_: bool,
        host: &impl Host,
        cancel: &mut (impl std::future::Future<Output = ()> + Unpin),
    ) -> Result<Outcome> {
        let _lock = layout.lock()?;
        let mut log = Log::open(layout);
        let root = layout.staging().join(id);
        let Some(release) = read_json::<Release>(&root.join("release.json")) else {
            return Err(Error::Ended(
                "That update is no longer staged. Start it again from the download.".into(),
            ));
        };
        sweep(layout, Some(id))?;
        let before = host.before().await?;
        let mut from = installed(layout, None);
        // An install from before `installed.json` gets one, so the version
        // kept to go back to says what it is.
        if let Some(found) = from.as_mut().filter(|_| !layout.installed().exists()) {
            if let Ok(version) = host.version(&layout.bin().join("ophio")).await {
                found.version = version;
            }
            write_json(&layout.installed(), found)?;
        }
        let mut outcome = Outcome {
            id: id.into(),
            result: Result_::NotInstalled,
            release: release.clone(),
            from: from.clone(),
            at: now(),
            message: String::new(),
        };
        match &from {
            Some(from) => log.line(changing(from, &release, root.join("rollback").is_file())),
            None => log.line(format!("Installing {}.", release.named())),
        }
        let result = carry_out(
            layout, &root, &release, &before, now_, host, cancel, &mut log,
        )
        .await;
        let _ = remove_all(&root);
        match result {
            Ok((result, message)) => {
                outcome.result = result;
                outcome.message = message;
            }
            Err(error) => {
                outcome.message = error.to_string();
                log.line(format!("The update stopped: {error}"));
            }
        }
        outcome.at = now();
        sweep(layout, None)?;
        let _ = write_json(&layout.last(), &outcome);
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    async fn carry_out(
        layout: &Layout,
        root: &Path,
        release: &Release,
        before: &Before,
        now_: bool,
        host: &impl Host,
        cancel: &mut (impl std::future::Future<Output = ()> + Unpin),
        log: &mut Log,
    ) -> Result<(Result_, String)> {
        if before.running && !now_ && !idle(host, cancel, log).await? {
            let message = "The update was cancelled before it changed anything.".to_owned();
            log.line(&message);
            return Ok((Result_::Cancelled, message));
        }
        if before.running {
            host.stop(before).await?;
            log.line("Stopped Øphio.");
        }
        let had_previous = swap(layout, root, release);
        let had_previous = match had_previous {
            Ok(had) => had,
            Err(error) => {
                // Finish a recorded replacement before restarting the installed programs.
                sweep(layout, None)?;
                if before.running {
                    let _ = host.start(before, &layout.bin().join("ophio")).await;
                }
                return Err(error);
            }
        };
        integrate(layout, root, log);
        match host.refresh_unit(&layout.bin().join("ophio")).await {
            Ok(Some(note)) => log.line(note),
            Ok(None) => {}
            Err(error) => log.line(format!("Couldn't update the login service: {error}")),
        }
        if !before.running {
            let message = format!("Installed {}.", release.named());
            log.line(&message);
            return Ok((Result_::Installed, message));
        }
        log.line("Starting Øphio again…");
        let started = match host.start(before, &layout.bin().join("ophio")).await {
            Ok(()) => host.healthy(before).await,
            Err(error) => Err(error.to_string()),
        };
        let problem = match started {
            Ok(()) => {
                let message = format!("Installed {}; Øphio is running.", release.named());
                log.line(&message);
                return Ok((Result_::Installed, message));
            }
            Err(problem) => problem,
        };
        log.line(format!(
            "The new version didn't come back working: {problem}"
        ));
        if !had_previous {
            return Err(Error::Ended(format!(
                "The new version didn't come back working ({problem}), and there was no earlier install to go back to."
            )));
        }
        let _ = host.stop(before).await;
        restore(layout)?;
        let _ = host.refresh_unit(&layout.bin().join("ophio")).await;
        host.start(before, &layout.bin().join("ophio")).await?;
        let message = match host.healthy(before).await {
            Ok(()) => format!(
                "The new version didn't come back working ({problem}), so the previous one was put back and is running."
            ),
            Err(again) => format!(
                "The new version didn't come back working ({problem}). The previous one was put back, but it isn't working either: {again}"
            ),
        };
        log.line(&message);
        Ok((Result_::RolledBack, message))
    }

    /// Waits until Øphio has been idle for [`IDLE_FOR`]. False when cancelled
    /// or after [`GIVE_UP`].
    async fn idle(
        host: &impl Host,
        cancel: &mut (impl std::future::Future<Output = ()> + Unpin),
        log: &mut Log,
    ) -> Result<bool> {
        let started = Instant::now();
        let mut idle_since: Option<Instant> = None;
        let mut said: Option<Vec<String>> = None;
        loop {
            let reasons = match host.busy().await {
                Ok(reasons) => reasons,
                Err(error) => vec![format!("Øphio didn't answer ({error})")],
            };
            if reasons.is_empty() {
                let since = *idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= IDLE_FOR {
                    log.line("Øphio has been idle for a minute.");
                    return Ok(true);
                }
                if said.as_ref().is_some_and(|said| !said.is_empty()) || said.is_none() {
                    log.line("Øphio is idle; installing once it has been for a minute.");
                    said = Some(Vec::new());
                }
            } else {
                idle_since = None;
                if said.as_ref() != Some(&reasons) {
                    log.line(format!(
                        "Waiting until Øphio is idle: {}.",
                        reasons.join("; ")
                    ));
                    said = Some(reasons);
                }
            }
            if started.elapsed() >= GIVE_UP {
                log.line(
                    "Øphio wasn't idle for a minute in two days, so the update stopped waiting.",
                );
                return Ok(false);
            }
            tokio::select! {
                () = &mut *cancel => return Ok(false),
                () = tokio::time::sleep(if idle_since.is_some() { IDLE_FOR / 4 } else { IDLE_POLL }) => {}
            }
        }
    }

    /// Why a snapshot shows Øphio busy: a run that hasn't ended or cleaned
    /// up, a task starting or working, or an approval waiting for an answer.
    /// Tasks in Next up, ones waiting on the person, finished runs waiting to
    /// be checked and questions their agent went on without don't hold an
    /// update: they are kept on disk and come back as they were.
    pub(crate) fn busy(snapshot: &Snapshot) -> Vec<String> {
        let mut reasons = Vec::new();
        let title = |task_id| {
            snapshot
                .tasks
                .iter()
                .find(|t| t.task_id == task_id)
                .map_or_else(|| "a task".to_owned(), |t| format!("“{}”", t.title))
        };
        for run in &snapshot.runs {
            // A reconciled run keeps its helpers' last state; only a run that
            // ended normally or has not ended can still have live ones.
            let live = run.children.iter().any(|c| {
                matches!(c.state, ChildState::Active | ChildState::Unknown)
                    && (run.ended_at.is_none() || run.state == RunState::Completed)
            });
            if run.ended_at.is_none() || run.holds_writer || run.cleanup_pending || live {
                reasons.push(format!("{} has an agent running", title(run.task_id)));
            }
        }
        for task in &snapshot.tasks {
            if matches!(task.state, TaskState::Dispatching | TaskState::Working) {
                let reason = format!("“{}” is working", task.title);
                if !reasons.contains(&reason) {
                    reasons.push(reason);
                }
            }
        }
        // A request outlives its run only once its agent went on without
        // it; answered later, it reaches the task as its next message.
        let waiting = snapshot
            .approvals
            .iter()
            .filter(|a| {
                a.state == ApprovalState::Pending
                    && (snapshot.runs.iter()).any(|r| r.run_id == a.run_id && r.ended_at.is_none())
            })
            .count()
            + snapshot.withheld_approvals.len();
        if waiting > 0 {
            reasons.push(if waiting == 1 {
                "an approval is waiting for an answer".into()
            } else {
                format!("{waiting} approvals are waiting for answers")
            });
        }
        reasons
    }

    /// Identifies a prepared entry across an atomic exchange. Recording the
    /// inode before moving it makes replay safe even if the process dies before
    /// recording that the move finished.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    struct FileId {
        device: u64,
        inode: u64,
    }

    impl FileId {
        fn at(path: &Path) -> std::io::Result<Self> {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(path)?;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Replacement {
        bin: FileId,
        app: Option<FileId>,
        installed: Option<FileId>,
        previous: Option<FileId>,
        programs_last: bool,
    }

    /// Hard links keep a complete earlier install without copying its large
    /// executables or ever removing the installed entry point. Neither snapshot
    /// is modified in place; replacements only rename or unlink entries.
    fn snapshot(from: &Path, to: &Path) -> Result<()> {
        let metadata = std::fs::symlink_metadata(from)?;
        if metadata.is_dir() {
            private_dir(to)?;
            for entry in std::fs::read_dir(from)? {
                let entry = entry?;
                snapshot(&entry.path(), &to.join(entry.file_name()))?;
            }
        } else if metadata.is_file() {
            std::fs::hard_link(from, to)?;
        } else {
            return Err(Error::Config(format!(
                "Cannot keep an install containing a link: {}",
                from.display()
            )));
        }
        Ok(())
    }

    fn move_entry(from: &Path, to: &Path) -> Result<()> {
        std::fs::rename(from, to)?;
        replacement_boundary();
        Ok(())
    }

    fn exchange(from: &Path, to: &Path) -> Result<()> {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        // No two-rename fallback: a filesystem without atomic exchange must
        // leave the runnable programs where they are.
        renameat_with(CWD, from, CWD, to, RenameFlags::EXCHANGE).map_err(std::io::Error::from)?;
        replacement_boundary();
        Ok(())
    }

    fn replacement_boundary() {
        #[cfg(test)]
        tests::replacement_boundary();
    }

    /// Prepare everything before publishing a journal. Upgrades exchange the
    /// programs first: an older install stays untouched until the new executable
    /// can recover it. Rollback exchanges them last so an older executable with
    /// no recovery hook finds its matching apps and metadata already in place.
    fn replace(
        layout: &Layout,
        source: &Path,
        keep_previous: bool,
        programs_last: bool,
    ) -> Result<()> {
        let replacement = layout.opt.join("replacement");
        remove_all(&replacement)?;
        remove_all(&layout.partial())?;
        private_dir(&replacement)?;
        // Check support before publishing recovery work. Otherwise a filesystem
        // that rejects EXCHANGE would make every later startup retry that same
        // unsupported operation instead of running the unchanged install.
        let probe_a = replacement.join("exchange-a");
        let probe_b = replacement.join("exchange-b");
        private_dir(&probe_a)?;
        private_dir(&probe_b)?;
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &probe_a,
            rustix::fs::CWD,
            &probe_b,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .map_err(std::io::Error::from)?;
        remove_all(&probe_a)?;
        remove_all(&probe_b)?;
        for name in ["bin", "app", "installed.json"] {
            if source.join(name).exists() {
                snapshot(&source.join(name), &replacement.join(name))?;
            }
        }
        let previous = if keep_previous && layout.bin().is_dir() {
            private_dir(&layout.partial())?;
            for name in ["bin", "app", "installed.json"] {
                if layout.opt.join(name).exists() {
                    snapshot(&layout.opt.join(name), &layout.partial().join(name))?;
                }
            }
            Some(FileId::at(&layout.partial())?)
        } else {
            None
        };
        let journal = Replacement {
            bin: FileId::at(&replacement.join("bin"))?,
            app: replacement
                .join("app")
                .exists()
                .then(|| FileId::at(&replacement.join("app")))
                .transpose()?,
            installed: replacement
                .join("installed.json")
                .exists()
                .then(|| FileId::at(&replacement.join("installed.json")))
                .transpose()?,
            previous,
            programs_last,
        };
        write_json(&layout.opt.join("replacement.json"), &journal)?;
        replacement_boundary();
        finish_replacement(layout)
    }

    /// Replay each move by identity, never by an easily interrupted phase
    /// counter. The journal stays until the installed and previous releases are
    /// complete; cleanup can safely be interrupted and replayed too.
    fn finish_replacement(layout: &Layout) -> Result<()> {
        let path = layout.opt.join("replacement.json");
        if !path.exists() {
            return Ok(());
        }
        let journal: Replacement = serde_json::from_slice(&std::fs::read(&path)?)
            .map_err(|_| Error::Config("Cannot read the interrupted update's record.".into()))?;
        let replacement = layout.opt.join("replacement");
        let mut entries = [
            ("bin", Some(journal.bin)),
            ("app", journal.app),
            ("installed.json", journal.installed),
        ];
        if journal.programs_last {
            entries.rotate_left(1);
        }
        for (name, wanted) in entries {
            if name == "bin" && journal.programs_last {
                finish_previous(layout, journal.previous)?;
            }
            let target = layout.opt.join(name);
            let Some(wanted) = wanted else {
                remove_all(&target)?;
                replacement_boundary();
                continue;
            };
            if FileId::at(&target).ok() == Some(wanted) {
                continue;
            }
            let source = replacement.join(name);
            if FileId::at(&source)? != wanted {
                return Err(Error::Config(
                    "The interrupted update's files changed.".into(),
                ));
            }
            if target.exists() {
                exchange(&source, &target)?;
            } else {
                move_entry(&source, &target)?;
            }
        }
        if !journal.programs_last {
            finish_previous(layout, journal.previous)?;
        }
        remove_all(&replacement)?;
        replacement_boundary();
        std::fs::remove_file(path)?;
        replacement_boundary();
        Ok(())
    }

    fn finish_previous(layout: &Layout, previous: Option<FileId>) -> Result<()> {
        if let Some(previous) = previous {
            if FileId::at(&layout.previous()).ok() != Some(previous) {
                remove_all(&layout.previous())?;
                replacement_boundary();
                move_entry(&layout.partial(), &layout.previous())?;
            }
        } else {
            remove_all(&layout.previous())?;
            replacement_boundary();
        }
        Ok(())
    }

    /// Atomically replaces the programs while retaining a complete previous
    /// release. `bin/ophio` exists on both sides of every update boundary.
    fn swap(layout: &Layout, root: &Path, release: &Release) -> Result<bool> {
        let had = layout.bin().is_dir();
        let mut installed = release.clone();
        installed.installed_at = now();
        write_json(&root.join("installed.json"), &installed)?;
        replace(layout, root, true, root.join("rollback").is_file())?;
        Ok(had)
    }

    /// Puts `previous` back using the same atomic program exchange, including
    /// when the earlier version predates this recovery code.
    fn restore(layout: &Layout) -> Result<()> {
        if !layout.previous().join("bin").is_dir() {
            return Err(Error::Ended(
                "There is no earlier install to go back to.".into(),
            ));
        }
        replace(layout, &layout.previous(), false, true)
    }

    pub(super) fn recover_on_start() -> Result<()> {
        let layout = Layout::discover()?;
        #[cfg(test)]
        tests::pause_startup(&layout, "entry");
        // current_exe resolves symlinked ancestors. Only the install root is
        // canonicalized: a displaced executable's parent may already be gone.
        let Ok(opt) = layout.opt.canonicalize() else {
            return Ok(());
        };
        #[cfg(test)]
        tests::pause_startup(&layout, "executable");
        let executable = std::env::current_exe()?;
        // Downloaded/staged updaters recover under their apply lock. Only an
        // installed invocation (including one moved aside by another launch)
        // performs startup recovery.
        let bin = opt.join("bin");
        if executable.parent() != Some(bin.as_path())
            && executable.parent() != Some(opt.join("replacement/bin").as_path())
        {
            return Ok(());
        }
        let running = FileId::at(Path::new("/proc/self/exe"))?;
        let journal = layout.opt.join("replacement.json");
        // The updater keeps its lock while starting and checking the service.
        // A completed replacement must allow that startup without taking it.
        let lock = if journal.exists() {
            Some(layout.lock()?)
        } else {
            None
        };
        if lock.is_some() && journal.exists() {
            sweep(&layout, None)?;
        }
        let program = layout.bin().join("ophio");
        // Another launch may have finished recovery before we saw the journal
        // or acquired the lock. Check again even with no journal, and include
        // the directory: hard-linked binaries can share an inode while the
        // running executable's old directory (and its helpers) was removed.
        let changed = FileId::at(&program)? != running
            || std::env::current_exe()?.parent() != Some(bin.as_path());
        drop(lock);
        if changed {
            use std::os::unix::process::CommandExt;
            return Err(std::process::Command::new(program)
                .args(std::env::args_os().skip(1))
                .exec()
                .into());
        }
        Ok(())
    }

    /// What an update or an interrupted one may leave behind, removed: any
    /// staged release but `keep`, the files install.sh wrote before moving
    /// them into place, and an earlier install half moved aside, which goes
    /// back if the programs are missing.
    pub(crate) fn sweep(layout: &Layout, keep: Option<&str>) -> Result<()> {
        finish_replacement(layout)?;
        // An interruption while preparing has not changed the install.
        remove_all(&layout.opt.join("replacement"))?;
        let partial = layout.partial();
        if !layout.bin().is_dir() {
            for source in [partial.clone(), layout.previous()] {
                if source.join("bin").is_dir()
                    && std::fs::rename(source.join("bin"), layout.bin()).is_ok()
                {
                    if source.join("app").is_dir() && !layout.app().exists() {
                        let _ = std::fs::rename(source.join("app"), layout.app());
                    }
                    if source.join("installed.json").exists() {
                        let _ = std::fs::rename(source.join("installed.json"), layout.installed());
                    }
                    break;
                }
            }
        }
        if partial.exists() && layout.bin().is_dir() {
            let _ = remove_all(&partial);
        }
        for entry in std::fs::read_dir(layout.staging())
            .into_iter()
            .flatten()
            .flatten()
        {
            if keep.is_none_or(|keep| entry.file_name() != keep) {
                let _ = remove_all(&entry.path());
            }
        }
        if keep.is_none() {
            let _ = std::fs::remove_dir(layout.staging());
        }
        for folder in [layout.bin(), layout.app()] {
            for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') && name.ends_with(".new") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let _ = std::fs::remove_file(layout.opt.join("installed.json.new"));
        let _ = std::fs::remove_file(layout.opt.join("last-update.json.new"));
        let _ = std::fs::remove_file(layout.opt.join("replacement.json.new"));
        Ok(())
    }

    /// Keeps the last `keep` bytes of a log, from a line's start.
    fn trim(path: &Path, keep: u64) {
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        let Ok(size) = file.metadata().map(|m| m.len()) else {
            return;
        };
        if size <= keep {
            return;
        }
        let mut tail = Vec::new();
        if file.seek(SeekFrom::Start(size - keep)).is_err() || file.read_to_end(&mut tail).is_err()
        {
            return;
        }
        let start = tail.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
        let partial = path.with_extension("log.new");
        if std::fs::write(&partial, &tail[start..]).is_ok() {
            let _ = std::fs::rename(&partial, path);
        }
    }

    /// What an install adds outside its folder: the `ophio` command, and the
    /// icon its notifications show.
    fn integrate(layout: &Layout, root: &Path, log: &mut Log) {
        let staged_icons = root.join("icons/hicolor");
        let mut icons = 0;
        for size in std::fs::read_dir(&staged_icons)
            .into_iter()
            .flatten()
            .flatten()
        {
            for entry in std::fs::read_dir(size.path().join("apps"))
                .into_iter()
                .flatten()
                .flatten()
            {
                let target = layout
                    .icons
                    .join(size.file_name())
                    .join("apps")
                    .join(entry.file_name());
                let copied = target
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::copy(entry.path(), &target))
                    .and_then(|_| set_mode(&target, 0o644));
                if copied.is_ok() {
                    icons += 1;
                }
            }
        }
        // A theme with a cache finds a new icon only once it is rebuilt.
        if icons > 0 && layout.icons.join("icon-theme.cache").exists() {
            let _ = std::process::Command::new("gtk-update-icon-cache")
                .args(["-q", "-t"])
                .arg(&layout.icons)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        // A command link is replaced only when it points into this install.
        let link = layout.links.join(APP_NAME);
        let target = layout.bin().join("ophio");
        let ours = |link: &Path, into: &Path| {
            std::fs::read_link(link).is_ok_and(|to| to.starts_with(into))
        };
        let absent = std::fs::symlink_metadata(&link).is_err();
        if absent || ours(&link, &layout.opt) {
            let partial = layout.links.join(format!(".{APP_NAME}.new"));
            let linked = std::fs::create_dir_all(&layout.links)
                .and_then(|()| {
                    let _ = std::fs::remove_file(&partial);
                    std::os::unix::fs::symlink(&target, &partial)
                })
                .and_then(|()| std::fs::rename(&partial, &link));
            if linked.is_ok() && absent {
                log.line(format!("The ophio command is {}.", link.display()));
            }
        } else {
            log.line(format!(
                "{} is not a link to this install, so it was left alone. Run {} instead, or remove it and install again.",
                link.display(),
                target.display()
            ));
        }
        let earlier = layout.opt.with_file_name(crate::config::EARLIER_APP_NAME);
        let earlier_link = layout.links.join(crate::config::EARLIER_APP_NAME);
        if ours(&earlier_link, &earlier) && std::fs::remove_file(&earlier_link).is_ok() {
            log.line("Removed the earlier companion command; use ophio instead.");
        }
    }

    // ---- This computer ----

    /// The real computer: the login service, the owner socket, and `/proc`.
    pub(crate) struct System {
        paths: Paths,
    }

    impl System {
        pub(crate) fn new() -> Result<Self> {
            Ok(Self {
                paths: Paths::named(APP_NAME)?,
            })
        }

        async fn snapshot(&self) -> Result<Snapshot> {
            let mut client = OwnerClient::connect(&self.paths.socket()).await?;
            match crate::cli::query(
                &mut client,
                Query::Snapshot {
                    reads_withheld: Some(true),
                },
            )
            .await?
            {
                QueryData::Snapshot(snapshot) => Ok(*snapshot),
                _ => Err(Error::Closed),
            }
        }
    }

    async fn systemctl(args: &[&str]) -> std::io::Result<std::process::Output> {
        tokio::process::Command::new("systemctl")
            .arg("--user")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .await
    }

    async fn active(unit: &str) -> bool {
        systemctl(&["is-active", "--quiet", unit])
            .await
            .is_ok_and(|o| o.status.success())
    }

    fn service_name() -> String {
        format!("{APP_NAME}.service")
    }

    impl Host for System {
        async fn version(&self, program: &Path) -> Result<String> {
            let output = tokio::time::timeout(
                Duration::from_secs(30),
                tokio::process::Command::new(program)
                    .arg("--version")
                    .stdin(std::process::Stdio::null())
                    .output(),
            )
            .await;
            let said = match output {
                Ok(Ok(output)) if output.status.success() => {
                    String::from_utf8_lossy(&output.stdout).into_owned()
                }
                _ => {
                    return Err(Error::Ended(
                        "The new version didn't start when tried, so it wasn't installed. Is the download for this computer?".into(),
                    ));
                }
            };
            said.trim()
                .strip_prefix(&format!("{APP_NAME} "))
                .map(str::to_owned)
                .ok_or_else(|| Error::Ended("That program isn't Øphio.".into()))
        }

        async fn before(&self) -> Result<Before> {
            let service = active(&service_name()).await;
            let snapshot = self.snapshot().await.ok();
            let listeners = if service {
                main_pid().await.map(listeners).unwrap_or_default()
            } else {
                Vec::new()
            };
            Ok(Before {
                running: snapshot.is_some() || service,
                service,
                listeners,
                boot: snapshot.map(|s| s.host.boot_epoch.to_string()),
            })
        }

        async fn busy(&self) -> Result<Vec<String>> {
            Ok(busy(&self.snapshot().await?))
        }

        async fn stop(&self, before: &Before) -> Result<()> {
            if before.service {
                let output = systemctl(&["stop", &service_name()]).await?;
                if !output.status.success() {
                    return Err(Error::Ended("Øphio's service didn't stop.".into()));
                }
                Ok(())
            } else {
                crate::install::stop_now(&self.paths).await.map(|_| ())
            }
        }

        async fn start(&self, before: &Before, program: &Path) -> Result<()> {
            if before.service {
                let output = systemctl(&["start", &service_name()]).await?;
                if !output.status.success() {
                    return Err(Error::Ended("Øphio's service didn't start.".into()));
                }
            } else {
                crate::install::detached(program)?;
            }
            Ok(())
        }

        async fn healthy(&self, before: &Before) -> std::result::Result<(), String> {
            let deadline = Instant::now() + HEALTH_WAIT;
            let mut problem;
            loop {
                problem = self.check(before).await.err();
                if problem.is_none() || Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            if let Some(problem) = problem {
                return Err(problem);
            }
            // Øphio restarts a service that ends; one that keeps ending fails here.
            let steady = Instant::now() + STEADY;
            while Instant::now() < steady {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if before.service && !active(&service_name()).await {
                    return Err("it stopped again just after starting".into());
                }
            }
            self.check(before).await
        }

        async fn refresh_unit(&self, program: &Path) -> Result<Option<String>> {
            crate::install::refresh_unit(program).await
        }
    }

    impl System {
        async fn check(&self, before: &Before) -> std::result::Result<(), String> {
            if before.service && !active(&service_name()).await {
                return Err("its service isn't running".into());
            }
            let snapshot = self
                .snapshot()
                .await
                .map_err(|_| "it isn't answering".to_owned())?;
            if before
                .boot
                .as_ref()
                .is_some_and(|boot| *boot == snapshot.host.boot_epoch.to_string())
            {
                return Err("the old version is still the one answering".into());
            }
            if before.service && !before.listeners.is_empty() {
                let now = main_pid().await.map(listeners).unwrap_or_default();
                let missing: Vec<&String> = before
                    .listeners
                    .iter()
                    .filter(|l| !now.contains(l))
                    .collect();
                if !missing.is_empty() {
                    return Err(format!(
                        "it isn't listening for devices on {} again",
                        missing
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            Ok(())
        }
    }

    async fn main_pid() -> Option<u32> {
        let output = systemctl(&["show", "-p", "MainPID", "--value", &service_name()])
            .await
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .ok()
            .filter(|pid| *pid != 0)
    }

    /// The addresses beyond this computer that process `pid` listens on for
    /// TCP connections.
    pub(crate) fn listeners(pid: u32) -> Vec<String> {
        let mut inodes = std::collections::HashSet::new();
        for fd in std::fs::read_dir(format!("/proc/{pid}/fd"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if let Ok(target) = std::fs::read_link(fd.path())
                && let Some(inode) = target
                    .to_str()
                    .and_then(|t| t.strip_prefix("socket:["))
                    .and_then(|t| t.strip_suffix(']'))
            {
                inodes.insert(inode.to_owned());
            }
        }
        let mut found = Vec::new();
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            let text = std::fs::read_to_string(table).unwrap_or_default();
            found.extend(listening(&text, &inodes));
        }
        found.sort();
        found.dedup();
        found
    }

    /// The non-loopback listening addresses in a `/proc/net/tcp` table among
    /// sockets `inodes`.
    pub(crate) fn listening(
        table: &str,
        inodes: &std::collections::HashSet<String>,
    ) -> Vec<String> {
        let mut found = Vec::new();
        for line in table.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 10 || fields[3] != "0A" || !inodes.contains(fields[9]) {
                continue;
            }
            if let Some(address) = socket_address(fields[1])
                && !address.ip().is_loopback()
                && !address.ip().to_canonical().is_loopback()
            {
                found.push(address.to_string());
            }
        }
        found
    }

    fn socket_address(text: &str) -> Option<std::net::SocketAddr> {
        let (address, port) = text.split_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        let ip: std::net::IpAddr = match address.len() {
            8 => std::net::Ipv4Addr::from(u32::from_str_radix(address, 16).ok()?.to_le_bytes())
                .into(),
            32 => {
                let mut bytes = [0u8; 16];
                for (i, chunk) in bytes.chunks_mut(4).enumerate() {
                    let word = u32::from_str_radix(&address[i * 8..i * 8 + 8], 16).ok()?;
                    chunk.copy_from_slice(&word.to_le_bytes());
                }
                std::net::Ipv6Addr::from(bytes).into()
            }
            _ => return None,
        };
        Some(std::net::SocketAddr::new(ip, port))
    }

    // ---- Commands ----

    pub(crate) async fn install(args: InstallArgs) -> Result<()> {
        let layout = Layout::discover()?;
        let host = System::new()?;
        if let Some(id) = args.apply {
            return applied(&layout, &id, args.now, &host).await;
        }
        let program = std::env::current_exe()?;
        let folder = program
            .parent()
            .ok_or_else(|| Error::Config("program folder unavailable".into()))?;
        if same(folder, &layout.bin()) || folder.starts_with(&layout.opt) {
            return Err(Error::Ended(format!(
                "This is the Øphio already installed. To update it, run `{COMMAND} update` with a newer download, or run the ophio program in that download with `install`."
            )));
        }
        let fresh = !layout.bin().join("ophio").exists();
        run_update(&layout, &host, folder, args.now).await?;
        if fresh {
            next_steps(&layout, args.no_setup);
        }
        Ok(())
    }

    fn same(a: &Path, b: &Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    pub(crate) async fn update(args: UpdateArgs) -> Result<()> {
        let layout = Layout::discover()?;
        let host = System::new()?;
        if args.cancel {
            return cancel(&layout).await;
        }
        if args.rollback {
            return rollback(&layout, &host, args.now).await;
        }
        match args.download {
            Some(download) => {
                let download = std::path::absolute(&download)?;
                if !download.exists() {
                    return Err(Error::Ended(format!(
                        "{} doesn't exist.",
                        download.display()
                    )));
                }
                run_update(&layout, &host, &download, args.now).await
            }
            None => status(&layout, args.json).await,
        }
    }

    /// Stages a release and hands it to its own program to carry out, so
    /// that the new version's rules apply. When Øphio runs as the login
    /// service, that happens in a service of its own, which outlives this
    /// terminal and Øphio's restart; this follows it until it ends.
    async fn run_update(layout: &Layout, host: &System, source: &Path, now_: bool) -> Result<()> {
        if active(&unit()).await {
            return Err(Error::Ended(format!(
                "An update is already waiting for Øphio to be idle. `{COMMAND} update --cancel` stops it."
            )));
        }
        let (id, release) = {
            let _lock = layout.lock()?;
            sweep(layout, None)?;
            trim(&layout.log(), LOG_KEEP);
            stage(layout, source, host).await?
        };
        println!("Checked {}.", release.named());
        let program = layout.staging().join(&id).join("bin/ophio");
        hand_over(layout, &program, &id, now_).await
    }

    async fn hand_over(layout: &Layout, program: &Path, id: &str, now_: bool) -> Result<()> {
        let mut args = vec!["install".to_owned(), "--apply".into(), id.into()];
        if now_ {
            args.push("--now".into());
        }
        if !active(&service_name()).await {
            let mut child = match tokio::process::Command::new(program).args(&args).spawn() {
                Ok(child) => child,
                Err(error) => {
                    let _ = remove_all(&layout.staging().join(id));
                    return Err(error.into());
                }
            };
            // Ctrl-C reaches the update too, which stops if it is still
            // waiting; this waits to say how it ended.
            loop {
                tokio::select! {
                    _ = child.wait() => break,
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            return finished(layout, id);
        }
        let offset = std::fs::metadata(layout.log()).map_or(0, |m| m.len());
        let started = tokio::process::Command::new("systemd-run")
            .args([
                "--user",
                "--quiet",
                "--collect",
                "--service-type=exec",
                "--property=TimeoutStopSec=300",
                &format!("--unit={}", unit()),
                &format!("--description={} update", ports::PRODUCT_NAME),
                "--",
            ])
            .arg(program)
            .args(&args)
            .status()
            .await;
        if !started.is_ok_and(|s| s.success()) {
            let _ = remove_all(&layout.staging().join(id));
            return Err(Error::Ended(
                "Couldn't start the update in the background with systemd-run.".into(),
            ));
        }
        follow(layout, id, offset).await
    }

    /// Prints the update's log as it goes, until it ends.
    async fn follow(layout: &Layout, id: &str, mut offset: u64) -> Result<()> {
        let mut said_waiting = false;
        loop {
            let running = active(&unit()).await;
            if let Ok(mut file) = std::fs::File::open(layout.log()) {
                if file.metadata().is_ok_and(|m| m.len() < offset) {
                    offset = 0;
                }
                let mut text = String::new();
                if file.seek(SeekFrom::Start(offset)).is_ok()
                    && file.read_to_string(&mut text).is_ok()
                {
                    offset += text.len() as u64;
                    for line in text.lines() {
                        // Each line starts with its date and time.
                        let line = line.splitn(3, ' ').nth(2).unwrap_or(line);
                        println!("{line}");
                        if line.starts_with("Waiting until Øphio is idle") && !said_waiting {
                            said_waiting = true;
                            println!(
                                "The update carries on if you close this terminal. `{COMMAND} update` shows how it is going, and `{COMMAND} update --cancel` stops it."
                            );
                        }
                    }
                }
            }
            if !running {
                return finished(layout, id);
            }
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("The update carries on in the background. `{COMMAND} update` shows how it is going.");
                    return Ok(());
                }
                () = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    }

    /// The command's result, from how the update `id` ended.
    fn finished(layout: &Layout, id: &str) -> Result<()> {
        match read_json::<Outcome>(&layout.last()) {
            Some(outcome) if outcome.id == id => match outcome.result {
                Result_::Installed => Ok(()),
                Result_::Cancelled => Err(Error::Ended(outcome.message)),
                Result_::RolledBack | Result_::NotInstalled => Err(Error::Ended(outcome.message)),
            },
            _ => Err(Error::Ended(format!(
                "The update ended without saying how. `{COMMAND} update` shows what is installed."
            ))),
        }
    }

    /// `install --apply`: carries out a staged update in this process.
    async fn applied(layout: &Layout, id: &str, now_: bool, host: &System) -> Result<()> {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut cancel = Box::pin(async move {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        });
        let outcome = apply(layout, id, now_, host, &mut cancel).await?;
        match outcome.result {
            Result_::Installed => Ok(()),
            _ => Err(Error::Ended(outcome.message)),
        }
    }

    async fn cancel(layout: &Layout) -> Result<()> {
        if !active(&unit()).await {
            println!("No update is waiting.");
            return Ok(());
        }
        let output = systemctl(&["stop", &unit()]).await?;
        if !output.status.success() {
            return Err(Error::Ended("Couldn't stop the waiting update.".into()));
        }
        match read_json::<Outcome>(&layout.last()) {
            Some(outcome) if outcome.result == Result_::Cancelled => {
                println!("{}", outcome.message)
            }
            Some(outcome) => println!("{}", outcome.message),
            None => println!("The update was stopped."),
        }
        Ok(())
    }

    async fn rollback(layout: &Layout, host: &System, now_: bool) -> Result<()> {
        if active(&unit()).await {
            return Err(Error::Ended(format!(
                "An update is waiting for Øphio to be idle. `{COMMAND} update --cancel` stops it first."
            )));
        }
        let previous = layout.previous();
        if !previous.join("bin/ophio").is_file() {
            return Err(Error::Ended(
                "There is no earlier install to go back to.".into(),
            ));
        }
        let (id, release) = {
            let _lock = layout.lock()?;
            sweep(layout, None)?;
            trim(&layout.log(), LOG_KEEP);
            stage(layout, &previous, host).await?
        };
        // Its own record says when and what it was.
        let release = read_json::<Release>(&previous.join("installed.json"))
            .map(|r| Release {
                installed_at: 0,
                build: release.build.clone(),
                apps: release.apps.clone(),
                ..r
            })
            .unwrap_or(release);
        write_json(&layout.staging().join(&id).join("release.json"), &release)?;
        // The earlier executable may not know how to recover an interruption.
        // Put its apps and metadata in place before exchanging the programs.
        std::fs::write(layout.staging().join(&id).join("rollback"), b"")?;
        println!("Going back to {}.", release.named());
        // This version carries out the swap: the earlier one may predate it.
        hand_over(layout, &std::env::current_exe()?, &id, now_).await
    }

    async fn status(layout: &Layout, json: bool) -> Result<()> {
        let host_version = env!("CARGO_PKG_VERSION");
        let installed = installed(layout, Some(host_version));
        let previous: Option<Release> = read_json(&layout.previous().join("installed.json"))
            .or_else(|| {
                let program = layout.previous().join("bin/ophio");
                program.is_file().then(|| Release {
                    version: "unknown".into(),
                    build: sha256(&program)
                        .map(|s| s[..12].to_owned())
                        .unwrap_or_default(),
                    installed_at: 0,
                    apps: Vec::new(),
                })
            });
        let last: Option<Outcome> = read_json(&layout.last());
        let waiting = active(&unit()).await;
        let size = folder_size(&layout.opt);
        if json {
            let value = serde_json::json!({
                "installed": installed,
                "previous": previous,
                "last_update": last,
                "waiting": waiting,
                "folder": layout.opt,
                "bytes": size,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(|_| Error::Closed)?
            );
            return Ok(());
        }
        match &installed {
            Some(release) if release.installed_at > 0 => println!(
                "Installed: {}, {}.",
                release.named(),
                ago(release.installed_at)
            ),
            Some(release) => println!("Installed: {}.", release.named()),
            None => println!(
                "Øphio isn't installed in {}. Run `ophio install` from an extracted download.",
                layout.opt.display()
            ),
        }
        if let Some(previous) = &previous {
            println!(
                "Kept to go back to: {}. `{COMMAND} update --rollback` goes back.",
                previous.named()
            );
        }
        if let Some(last) = &last {
            let what = match last.result {
                Result_::Installed => "installed",
                Result_::RolledBack => "rolled back",
                Result_::NotInstalled => "not installed",
                Result_::Cancelled => "cancelled",
            };
            println!(
                "Last update: {} {}, {}. {}",
                last.release.named(),
                what,
                ago(last.at),
                last.message
            );
        }
        if waiting {
            let line = std::fs::read_to_string(layout.log())
                .ok()
                .and_then(|log| {
                    log.lines()
                        .last()
                        .map(|l| l.splitn(3, ' ').nth(2).unwrap_or(l).to_owned())
                })
                .unwrap_or_default();
            println!(
                "An update is under way: {line} `{COMMAND} update --cancel` stops it while it waits."
            );
        }
        if installed.is_some() {
            println!("{} uses {}.", layout.opt.display(), size_text(size));
        }
        Ok(())
    }

    fn size_text(bytes: u64) -> String {
        const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
        if bytes < 1000 {
            return format!("{bytes} bytes");
        }
        let mut value = bytes as f64 / 1000.0;
        let mut unit = 0;
        while value >= 1000.0 && unit + 1 < UNITS.len() {
            value /= 1000.0;
            unit += 1;
        }
        if value < 10.0 {
            format!("{value:.1} {}", UNITS[unit])
        } else {
            format!("{value:.0} {}", UNITS[unit])
        }
    }

    fn folder_size(path: &Path) -> u64 {
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return 0;
        };
        if !metadata.is_dir() {
            return metadata.len();
        }
        std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| folder_size(&entry.path()))
            .sum()
    }

    /// After a first install, what to do next, as install.sh said it.
    fn next_steps(layout: &Layout, no_setup: bool) {
        let link = layout.links.join(APP_NAME);
        let on_path = std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|p| p == layout.links));
        if !on_path {
            println!(
                "{} is not on your PATH. Add it to type ophio, or run {}.",
                layout.links.display(),
                layout.bin().join("ophio").display()
            );
        }
        let units = directories::BaseDirs::new()
            .map(|b| b.config_dir().join("systemd/user"))
            .unwrap_or_default();
        let earlier_unit = units.join(format!("{}.service", crate::config::EARLIER_APP_NAME));
        let earlier = layout.opt.with_file_name(crate::config::EARLIER_APP_NAME);
        let earlier_managed = std::fs::read_to_string(&earlier_unit).is_ok_and(|text| {
            text.starts_with(&format!(
                "# Managed by {};",
                crate::config::EARLIER_APP_NAME
            ))
        });
        if earlier_managed {
            println!("Next, run: ophio service install");
            println!(
                "It stops the earlier companion service and starts Øphio in its place. Øphio then moves the earlier settings, paired devices and data to its own folders."
            );
            println!("Afterwards you can delete {}.", earlier.display());
            return;
        }
        if earlier.is_dir() {
            println!("Stop the earlier companion program, then run: ophio run");
            println!(
                "Øphio moves the earlier settings, paired devices and data to its own folders when it starts. Afterwards you can delete {}.",
                earlier.display()
            );
            return;
        }
        use std::io::IsTerminal;
        let starts = !no_setup && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        if starts {
            // A setup left part way carries on with `ophio setup`, so the
            // install has still succeeded however it ends.
            println!();
            let program = if link.exists() {
                link
            } else {
                layout.bin().join("ophio")
            };
            start_setup(&program, &mut std::io::stdout());
        } else {
            println!("{}", setup_next(false));
        }
    }

    fn start_setup(program: &Path, out: &mut impl Write) {
        // Said before setup starts: once it runs, its screen owns the terminal.
        let _ = writeln!(out, "{}", setup_next(true));
        let _ = out.flush();
        match std::process::Command::new(program)
            .args(["setup", "--first-run"])
            .spawn()
        {
            Ok(mut child) => {
                if !child.wait().is_ok_and(|status| status.success()) {
                    let _ = writeln!(out, "Setup did not finish. To try again, run: ophio setup");
                }
            }
            Err(error) => {
                let _ = writeln!(out, "Setup could not start: {error}");
                let _ = writeln!(out, "{}", setup_next(false));
            }
        }
    }

    /// What a first install says about setup: that it starts now, or how to
    /// start it.
    fn setup_next(starts: bool) -> &'static str {
        if starts {
            "Starting setup now. It starts Øphio, connects your phone and checks what works."
        } else {
            "Next, run: ophio setup\nIt starts Øphio, connects your phone and checks what works."
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{cell::RefCell, collections::VecDeque, os::unix::fs::PermissionsExt};

        #[test]
        fn going_back_to_an_earlier_version_is_not_called_updating() {
            let release = |version: &str, build: &str| Release {
                version: version.into(),
                build: build.into(),
                installed_at: 0,
                apps: vec![],
            };
            let (old, new) = (
                release("0.1.0", "d77d6ddd53e9"),
                release("0.1.1", "ca215a2e1970"),
            );
            assert_eq!(
                changing(&new, &old, false),
                "Going back from Øphio 0.1.1 (build ca215a2e1970) to Øphio 0.1.0 (build d77d6ddd53e9)."
            );
            assert_eq!(
                changing(&old, &new, false),
                "Updating Øphio 0.1.0 (build d77d6ddd53e9) to Øphio 0.1.1 (build ca215a2e1970)."
            );
            assert!(
                changing(&release("0.9.0", "a"), &release("0.10.0", "b"), false)
                    .starts_with("Updating")
            );
            assert!(changing(&old, &release("0.1.0", "b"), false).starts_with("Updating"));
        }

        #[test]
        fn a_first_install_says_next_run_setup_only_when_it_does_not_start_it() {
            assert!(setup_next(false).starts_with("Next, run: ophio setup\n"));
            assert!(setup_next(true).starts_with("Starting setup now."));
            assert!(
                !setup_next(true).contains("Next, run"),
                "{}",
                setup_next(true)
            );
        }

        #[test]
        fn setup_that_cannot_start_gives_the_manual_command_instead() {
            let dir = tempfile::tempdir().unwrap();
            let program = dir.path().join("ophio");
            std::fs::write(&program, "not executable").unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o600)).unwrap();
            let mut output = Vec::new();
            start_setup(&program, &mut output);
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("Setup could not start"), "{output}");
            assert!(output.contains("Next, run: ophio setup"), "{output}");
        }

        #[test]
        fn starting_setup_does_not_also_tell_you_to_start_it() {
            let dir = tempfile::tempdir().unwrap();
            let program = dir.path().join("ophio");
            let marker = dir.path().join("started");
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\ntest \"$1\" = setup && test \"$2\" = --first-run && touch '{}'\n",
                    marker.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            let mut output = Vec::new();
            start_setup(&program, &mut output);
            let output = String::from_utf8(output).unwrap();
            assert!(marker.exists());
            assert!(output.contains("Starting setup now"), "{output}");
            assert!(!output.contains("Next, run:"), "{output}");
        }

        pub(super) fn pause_startup(layout: &Layout, phase: &str) {
            // Remove the marker before stopping, so the competing launch and
            // a subsequent exec can finish without stopping here again.
            if std::fs::remove_file(layout.opt.join(format!("pause-startup-{phase}"))).is_ok() {
                // libtest runs recovery on a worker. A process-directed stop
                // can reach the runner while this thread continues into exec.
                nix::sys::signal::raise(nix::sys::signal::Signal::SIGSTOP).unwrap();
            }
        }

        struct StartupChild(std::process::Child);

        impl Drop for StartupChild {
            fn drop(&mut self) {
                // A failed assertion must not leave a stopped process behind.
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        // Only compiled into the test executable. SIGKILL cannot run cleanup
        // or unwind guards, matching an updater terminated between syscalls.
        pub(super) fn replacement_boundary() {
            thread_local! {
                static COUNT: Cell<usize> = const { Cell::new(0) };
            }
            let Ok(stop) = std::env::var("OPHIO_TEST_KILL_AT") else {
                return;
            };
            let stop: usize = stop.parse().unwrap();
            COUNT.with(|count| {
                count.set(count.get() + 1);
                if count.get() == stop {
                    rustix::process::kill_process(
                        rustix::process::getpid(),
                        rustix::process::Signal::KILL,
                    )
                    .unwrap();
                    unreachable!();
                }
            });
        }

        #[test]
        fn replacement_subprocess() {
            let Ok(action) = std::env::var("OPHIO_TEST_REPLACEMENT") else {
                return;
            };
            let layout = Layout::discover().unwrap();
            match action.as_str() {
                "swap" | "rollback" => {
                    let _lock = layout.lock().unwrap();
                    let source = layout.opt.parent().unwrap().join("download");
                    let release = read_json(&source.join("installed.json")).unwrap();
                    swap(&layout, &source, &release).unwrap();
                }
                "restore" => {
                    let _lock = layout.lock().unwrap();
                    restore(&layout).unwrap();
                }
                "recover-staged" => {
                    let executable = std::env::current_exe().unwrap();
                    crate::update::recover_on_start().unwrap();
                    assert_eq!(std::env::current_exe().unwrap(), executable);
                    assert!(layout.opt.join("replacement.json").exists());
                }
                "recover" => {
                    // Same entry as main(), reached through the installed
                    // command or the service's bin/ophio path, never sweep().
                    crate::update::recover_on_start().unwrap();
                    assert_eq!(
                        crate::compose::ComposeOptions::from_process()
                            .unwrap()
                            .executable_dir,
                        layout.bin().canonicalize().unwrap(),
                        "helpers must be resolved beside the installed executable"
                    );
                    assert_eq!(
                        FileId::at(Path::new("/proc/self/exe")).unwrap(),
                        FileId::at(&layout.bin().join("ophio")).unwrap(),
                        "startup must execute the installed version"
                    );
                    let version = std::fs::read_to_string(layout.bin().join("version")).unwrap();
                    let release: Release = read_json(&layout.installed()).unwrap();
                    assert_eq!(release.version, version);
                    assert_eq!(
                        std::fs::read_to_string(layout.app().join("ophio.apk")).unwrap(),
                        version
                    );
                    assert!(!layout.opt.join("replacement.json").exists());
                }
                _ => panic!("unexpected subprocess action"),
            }
        }

        fn executable_release(root: &Path, executable: &Path, version: &str) {
            private_dir(&root.join("bin")).unwrap();
            private_dir(&root.join("app")).unwrap();
            std::fs::hard_link(executable, root.join("bin/ophio")).unwrap();
            std::fs::write(root.join("bin/version"), version).unwrap();
            std::fs::write(root.join("app/ophio.apk"), version).unwrap();
            write_json(
                &root.join("installed.json"),
                &Release {
                    version: version.into(),
                    build: version.into(),
                    installed_at: 1,
                    apps: vec!["ophio.apk".into()],
                },
            )
            .unwrap();
        }

        fn replacement_child(program: &Path, home: &Path, action: &str) -> std::process::Command {
            let mut child = std::process::Command::new(program);
            child
                .args([
                    "--exact",
                    "update::linux::tests::replacement_subprocess",
                    "--nocapture",
                ])
                .env("HOME", home)
                .env("OPHIO_TEST_REPLACEMENT", action)
                .env_remove("OPHIO_TEST_KILL_AT");
            child
        }

        #[test]
        fn installed_entry_recovers_after_every_update_and_restore_boundary() {
            check_replacement_boundaries(false);
        }

        #[test]
        fn installed_entry_recovers_with_a_symlinked_home() {
            check_replacement_boundaries(true);
        }

        fn check_replacement_boundaries(symlinked_home: bool) {
            use std::os::unix::process::ExitStatusExt;
            let binaries = tempfile::tempdir().unwrap();
            // CI's build cache and temporary homes can be different mounts.
            // Copy both fixture executables onto the homes' filesystem before
            // using hard links for the many per-boundary installations.
            let old = binaries.path().join("old-ophio");
            std::fs::copy(std::env::current_exe().unwrap(), &old).unwrap();
            // A separate inode exercises startup's exec into the recovered version.
            let new = binaries.path().join("new-ophio");
            std::fs::copy(&old, &new).unwrap();
            for action in ["swap", "rollback", "restore"] {
                for service in [false, true] {
                    for (killed, boundary) in (1..=20).enumerate() {
                        let home = home();
                        let startup_home = if symlinked_home {
                            let link = home.root.join("home-link");
                            std::os::unix::fs::symlink(&home.root, &link).unwrap();
                            link
                        } else {
                            home.root.clone()
                        };
                        let layout = &home.layout;
                        executable_release(&layout.opt, &old, "old");
                        let source = if action != "restore" {
                            layout.opt.parent().unwrap().join("download")
                        } else {
                            layout.previous()
                        };
                        executable_release(&source, &new, "new");
                        if action == "rollback" {
                            std::fs::write(source.join("rollback"), b"").unwrap();
                        }
                        if action != "restore" {
                            executable_release(&layout.previous(), &old, "older");
                        }
                        private_dir(&layout.links).unwrap();
                        let command = layout.links.join("ophio");
                        std::os::unix::fs::symlink(layout.bin().join("ophio"), &command).unwrap();
                        let result = replacement_child(&command, &home.root, action)
                            .env("OPHIO_TEST_KILL_AT", boundary.to_string())
                            .output()
                            .unwrap();
                        if result.status.success() {
                            assert!(killed >= 7, "no interruption coverage for {action}");
                            break;
                        }
                        assert_eq!(
                            result.status.signal(),
                            Some(9),
                            "{action} {boundary}: {result:?}"
                        );
                        let entry = if service {
                            layout.bin().join("ophio")
                        } else {
                            command
                        };
                        let resumed = replacement_child(&entry, &startup_home, "recover")
                            .output()
                            .unwrap_or_else(|e| {
                                panic!("missing entry after {action} boundary {boundary}: {e}")
                            });
                        assert!(resumed.status.success(), "{action} {boundary}: {resumed:?}");
                        assert_eq!(
                            std::fs::read_to_string(layout.bin().join("version")).unwrap(),
                            "new"
                        );
                        // Recovery must be idempotent on a second launch.
                        assert!(
                            replacement_child(&entry, &startup_home, "recover")
                                .output()
                                .unwrap()
                                .status
                                .success()
                        );
                        if action != "restore" {
                            assert_eq!(
                                std::fs::read_to_string(layout.previous().join("bin/version"))
                                    .unwrap(),
                                "old"
                            );
                        } else {
                            assert!(!layout.previous().exists());
                        }
                        assert!(!layout.partial().exists());
                        assert!(!layout.opt.join("replacement").exists());
                        assert!(
                            boundary < 20,
                            "boundary limit must cover the whole replacement"
                        );
                    }
                }
            }
        }

        #[test]
        fn concurrent_startup_executes_the_recovered_install() {
            use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};
            use std::os::unix::process::ExitStatusExt;

            let binaries = tempfile::tempdir().unwrap();
            let old = binaries.path().join("old-ophio");
            let new = binaries.path().join("new-ophio");
            std::fs::copy(std::env::current_exe().unwrap(), &old).unwrap();
            std::fs::copy(&old, &new).unwrap();
            for phase in ["entry", "executable"] {
                for same_program in [false, true] {
                    for service in [false, true] {
                        let case =
                            format!("{phase}, same program: {same_program}, service: {service}");
                        let home = home();
                        let layout = &home.layout;
                        executable_release(&layout.opt, &old, "old");
                        let source = layout.opt.parent().unwrap().join("download");
                        executable_release(&source, if same_program { &old } else { &new }, "new");
                        private_dir(&layout.links).unwrap();
                        let command = layout.links.join("ophio");
                        std::os::unix::fs::symlink(layout.bin().join("ophio"), &command).unwrap();
                        // Publish the journal, leaving the current executable
                        // installed so two launches can load it before exchange.
                        let killed = replacement_child(&command, &home.root, "swap")
                            .env("OPHIO_TEST_KILL_AT", "1")
                            .output()
                            .unwrap();
                        assert_eq!(killed.status.signal(), Some(9), "{case}: {killed:?}");
                        std::fs::write(layout.opt.join(format!("pause-startup-{phase}")), b"")
                            .unwrap();
                        let entry = if service {
                            layout.bin().join("ophio")
                        } else {
                            command
                        };
                        let mut first = StartupChild(
                            replacement_child(&entry, &home.root, "recover")
                                .spawn()
                                .unwrap(),
                        );
                        let pid = Pid::from_child(&first.0);
                        let (_, stopped) =
                            waitpid(Some(pid), WaitOptions::UNTRACED).unwrap().unwrap();
                        assert_eq!(
                            stopped.stopping_signal(),
                            Some(Signal::STOP.as_raw()),
                            "{case}: waiting for startup stop: {stopped:?}"
                        );
                        assert!(layout.opt.join("replacement.json").exists(), "{case}");
                        assert_eq!(
                            std::fs::read_to_string(layout.bin().join("version")).unwrap(),
                            "old",
                            "{case}: recovery advanced before SIGCONT: {stopped:?}"
                        );
                        let second = replacement_child(&entry, &home.root, "recover")
                            .output()
                            .unwrap();
                        assert!(second.status.success(), "{case}: second launch: {second:?}");
                        assert!(!layout.opt.join("replacement.json").exists());
                        assert!(!layout.opt.join("replacement").exists());
                        // The first process now points into a deleted directory,
                        // even when both versions share an executable inode.
                        kill_process(pid, Signal::CONT).unwrap();
                        let resumed = first.0.wait().unwrap();
                        assert!(resumed.success(), "{case}: after SIGCONT: {resumed:?}");
                    }
                }
            }
        }

        #[test]
        fn completed_startup_does_not_wait_for_the_updater_or_remove_staging() {
            let home = home();
            let layout = &home.layout;
            let binary = home.root.join("test-ophio");
            std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
            executable_release(&layout.opt, &binary, "installed");
            let staged = layout.staging().join("pending");
            private_dir(&staged).unwrap();
            std::fs::write(staged.join("marker"), b"keep").unwrap();
            // apply() retains this lock while starting/checking the service.
            let _updater = layout.lock().unwrap();
            let result = replacement_child(&layout.bin().join("ophio"), &home.root, "recover")
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
            assert!(staged.join("marker").exists());
        }

        #[test]
        fn staged_updater_does_not_perform_installed_startup_recovery() {
            use std::os::unix::process::ExitStatusExt;

            let home = home();
            let layout = &home.layout;
            let binary = home.root.join("test-ophio");
            std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
            executable_release(&layout.opt, &binary, "installed");
            let source = layout.opt.parent().unwrap().join("download");
            executable_release(&source, &binary, "new");
            let killed = replacement_child(&layout.bin().join("ophio"), &home.root, "swap")
                .env("OPHIO_TEST_KILL_AT", "1")
                .output()
                .unwrap();
            assert_eq!(killed.status.signal(), Some(9));
            let staged = layout.staging().join("pending");
            executable_release(&staged, &binary, "new");
            let _updater = layout.lock().unwrap();
            let result = replacement_child(&staged.join("bin/ophio"), &home.root, "recover-staged")
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
            assert!(layout.opt.join("replacement.json").exists());
            assert!(staged.join("bin/ophio").exists());
        }

        #[derive(Default)]
        struct Fake {
            running: bool,
            service: bool,
            busy: RefCell<VecDeque<Vec<String>>>,
            health: RefCell<VecDeque<std::result::Result<(), String>>>,
            calls: RefCell<Vec<String>>,
        }

        impl Fake {
            fn running() -> Self {
                Self {
                    running: true,
                    service: true,
                    ..Self::default()
                }
            }
            fn calls(&self) -> Vec<String> {
                self.calls.borrow().clone()
            }
        }

        impl Host for Fake {
            async fn version(&self, program: &Path) -> Result<String> {
                let text = std::fs::read(program)?;
                Ok(String::from_utf8_lossy(&text[4..]).into_owned())
            }
            async fn before(&self) -> Result<Before> {
                Ok(Before {
                    running: self.running,
                    service: self.service,
                    listeners: Vec::new(),
                    boot: None,
                })
            }
            async fn busy(&self) -> Result<Vec<String>> {
                self.calls.borrow_mut().push("busy".into());
                Ok(self.busy.borrow_mut().pop_front().unwrap_or_default())
            }
            async fn stop(&self, _: &Before) -> Result<()> {
                self.calls.borrow_mut().push("stop".into());
                Ok(())
            }
            async fn start(&self, _: &Before, program: &Path) -> Result<()> {
                let version = self.version(program).await?;
                self.calls.borrow_mut().push(format!("start {version}"));
                Ok(())
            }
            async fn healthy(&self, _: &Before) -> std::result::Result<(), String> {
                self.calls.borrow_mut().push("healthy".into());
                self.health.borrow_mut().pop_front().unwrap_or(Ok(()))
            }
            async fn refresh_unit(&self, _: &Path) -> Result<Option<String>> {
                Ok(None)
            }
        }

        struct Home {
            _temp: tempfile::TempDir,
            root: PathBuf,
            layout: Layout,
        }

        fn home() -> Home {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_owned();
            let layout = Layout {
                opt: root.join(".local/opt/ophio"),
                links: root.join(".local/bin"),
                icons: root.join(".local/share/icons/hicolor"),
            };
            Home {
                _temp: temp,
                root,
                layout,
            }
        }

        /// An extracted download whose programs say they are `version`.
        fn download(home: &Home, version: &str) -> PathBuf {
            let dir = home.root.join(format!("download-{version}"));
            std::fs::create_dir_all(dir.join("icons/hicolor/64x64/apps")).unwrap();
            for program in PROGRAMS {
                std::fs::write(dir.join(program), format!("\x7fELF{version}")).unwrap();
            }
            std::fs::write(dir.join("ophio.apk"), format!("apk {version}")).unwrap();
            std::fs::write(dir.join("INSTALL.md"), "read me").unwrap();
            std::fs::write(dir.join("icons/hicolor/64x64/apps/ophio.png"), "icon").unwrap();
            dir
        }

        async fn update(home: &Home, version: &str, host: &Fake) -> Outcome {
            let (id, _) = stage(&home.layout, &download(home, version), host)
                .await
                .unwrap();
            apply(&home.layout, &id, true, host, &mut std::future::pending())
                .await
                .unwrap()
        }

        fn installed_version(home: &Home) -> String {
            String::from_utf8(std::fs::read(home.layout.bin().join("ophio")).unwrap()).unwrap()[4..]
                .to_owned()
        }

        fn staged(home: &Home) -> Vec<String> {
            std::fs::read_dir(home.layout.staging())
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        }

        #[tokio::test]
        async fn installs_fresh_without_starting() {
            let home = home();
            let host = Fake::default();
            let outcome = update(&home, "1.0", &host).await;
            assert_eq!(outcome.result, Result_::Installed);
            assert_eq!(outcome.from, None);
            assert_eq!(installed_version(&home), "1.0");
            let mode = std::fs::metadata(home.layout.bin().join("computer-mcp"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
            assert_eq!(
                std::fs::read_to_string(home.layout.app().join("ophio.apk")).unwrap(),
                "apk 1.0"
            );
            assert!(!home.layout.bin().join("INSTALL.md").exists());
            assert_eq!(
                std::fs::read_link(home.layout.links.join("ophio")).unwrap(),
                home.layout.bin().join("ophio")
            );
            assert!(home.layout.icons.join("64x64/apps/ophio.png").is_file());
            let release: Release = read_json(&home.layout.installed()).unwrap();
            assert_eq!(release.version, "1.0");
            assert!(release.installed_at > 0);
            assert!(!home.layout.previous().exists());
            assert!(!home.layout.staging().exists());
            assert!(host.calls().is_empty(), "{:?}", host.calls());
        }

        #[tokio::test]
        async fn describes_an_install_from_before_it_kept_records() {
            let home = home();
            update(&home, "1.0", &Fake::default()).await;
            std::fs::remove_file(home.layout.installed()).unwrap();
            let outcome = update(&home, "2.0", &Fake::running()).await;
            assert_eq!(outcome.from.unwrap().version, "1.0");
            let previous: Release =
                read_json(&home.layout.previous().join("installed.json")).unwrap();
            assert_eq!(previous.version, "1.0");
        }

        #[tokio::test]
        async fn replaces_only_command_links_it_made() {
            let home = home();
            let earlier = home.root.join(".local/opt/companion/bin/companion");
            std::fs::create_dir_all(earlier.parent().unwrap()).unwrap();
            std::fs::write(&earlier, "earlier").unwrap();
            std::fs::create_dir_all(&home.layout.links).unwrap();
            let elsewhere = home.root.join("elsewhere");
            std::os::unix::fs::symlink(&elsewhere, home.layout.links.join("ophio")).unwrap();
            std::os::unix::fs::symlink(&earlier, home.layout.links.join("companion")).unwrap();
            update(&home, "1.0", &Fake::default()).await;
            assert_eq!(
                std::fs::read_link(home.layout.links.join("ophio")).unwrap(),
                elsewhere
            );
            assert!(std::fs::symlink_metadata(home.layout.links.join("companion")).is_err());
            // The earlier program stays for its owner to delete.
            assert!(earlier.is_file());
            // Someone else's companion command is left alone.
            std::fs::write(home.layout.links.join("companion"), "theirs").unwrap();
            std::fs::remove_file(home.layout.links.join("ophio")).unwrap();
            update(&home, "2.0", &Fake::default()).await;
            assert_eq!(
                std::fs::read_to_string(home.layout.links.join("companion")).unwrap(),
                "theirs"
            );
            assert_eq!(
                std::fs::read_link(home.layout.links.join("ophio")).unwrap(),
                home.layout.bin().join("ophio")
            );
        }

        #[tokio::test]
        async fn drops_a_phone_app_the_new_version_does_not_carry() {
            let home = home();
            update(&home, "1.0", &Fake::default()).await;
            let download = download(&home, "2.0");
            std::fs::remove_file(download.join("ophio.apk")).unwrap();
            let (id, _) = stage(&home.layout, &download, &Fake::default())
                .await
                .unwrap();
            apply(
                &home.layout,
                &id,
                true,
                &Fake::default(),
                &mut std::future::pending(),
            )
            .await
            .unwrap();
            assert!(!home.layout.app().exists());
            assert!(app_offers(&home.layout.bin()).is_empty());
        }

        #[test]
        fn an_app_signed_in_several_schemes_names_its_signer_once() {
            use sha2::Digest;
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("bin")).unwrap();
            std::fs::create_dir_all(dir.path().join("app")).unwrap();
            std::fs::write(
                dir.path().join("app/ophio.apk"),
                crate::phone::apk::tests::signed_app(b"release"),
            )
            .unwrap();
            let offers = app_offers(&dir.path().join("bin"));
            assert_eq!(offers.len(), 1);
            assert_eq!(
                offers[0].0.signers,
                [hex(&<[u8; 32]>::from(sha2::Sha256::digest(b"release")))]
            );
        }

        #[tokio::test]
        async fn keeps_only_one_previous_version() {
            let home = home();
            update(&home, "1.0", &Fake::default()).await;
            let host = Fake::running();
            let outcome = update(&home, "2.0", &host).await;
            assert_eq!(outcome.result, Result_::Installed, "{}", outcome.message);
            assert_eq!(outcome.from.unwrap().version, "1.0");
            assert_eq!(host.calls(), ["stop", "start 2.0", "healthy"]);
            update(&home, "3.0", &Fake::running()).await;
            assert_eq!(installed_version(&home), "3.0");
            let previous: Release =
                read_json(&home.layout.previous().join("installed.json")).unwrap();
            assert_eq!(previous.version, "2.0");
            let kept: Vec<_> = std::fs::read_dir(&home.layout.opt)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| !name.ends_with(".json") && !name.starts_with("update."))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            assert_eq!(kept, ["app", "bin", "previous"]);
            assert!(!home.layout.staging().exists());
            let last: Outcome = read_json(&home.layout.last()).unwrap();
            assert_eq!(last.release.version, "3.0");
        }

        #[tokio::test]
        async fn rollback_log_names_the_action_even_when_versions_are_equal() {
            let home = home();
            update(&home, "0.1.0", &Fake::default()).await;
            let newer = download(&home, "0.1.0");
            std::fs::write(newer.join("ophio.apk"), "another build").unwrap();
            let host = Fake::running();
            let (id, _) = stage(&home.layout, &newer, &host).await.unwrap();
            apply(&home.layout, &id, true, &host, &mut std::future::pending())
                .await
                .unwrap();
            let from = installed(&home.layout, None).unwrap();
            let (id, to) = stage(&home.layout, &home.layout.previous(), &host)
                .await
                .unwrap();
            std::fs::write(home.layout.staging().join(&id).join("rollback"), b"").unwrap();
            let result = apply(&home.layout, &id, true, &host, &mut std::future::pending())
                .await
                .unwrap();
            assert_eq!(result.result, Result_::Installed);
            let log = std::fs::read_to_string(home.layout.log()).unwrap();
            assert!(
                log.contains(&format!(
                    "Going back from {} to {}.",
                    from.named(),
                    to.named()
                )),
                "{log}"
            );
        }

        #[tokio::test]
        async fn goes_back_to_the_version_it_kept() {
            let home = home();
            update(&home, "1.0", &Fake::default()).await;
            let newer = download(&home, "2.0");
            std::fs::write(newer.join("ophio-test.apk"), "test apk 2.0").unwrap();
            let (id, _) = stage(&home.layout, &newer, &Fake::default()).await.unwrap();
            apply(
                &home.layout,
                &id,
                true,
                &Fake::running(),
                &mut std::future::pending(),
            )
            .await
            .unwrap();
            // What `ophio update --rollback` stages: the kept install itself.
            let host = Fake::running();
            let (id, release) = stage(&home.layout, &home.layout.previous(), &host)
                .await
                .unwrap();
            assert_eq!(release.version, "1.0");
            assert!(home.layout.previous().join("bin/ophio").is_file());
            let outcome = apply(&home.layout, &id, true, &host, &mut std::future::pending())
                .await
                .unwrap();
            assert_eq!(outcome.result, Result_::Installed, "{}", outcome.message);
            assert_eq!(installed_version(&home), "1.0");
            assert_eq!(
                std::fs::read_to_string(home.layout.app().join("ophio.apk")).unwrap(),
                "apk 1.0"
            );
            assert!(!home.layout.app().join("ophio-test.apk").exists());
            let previous: Release =
                read_json(&home.layout.previous().join("installed.json")).unwrap();
            assert_eq!(previous.version, "2.0");
            assert!(home.layout.previous().join("app/ophio-test.apk").is_file());
            assert!(!home.layout.staging().exists());
        }

        #[tokio::test]
        async fn goes_back_to_an_install_from_before_the_sandbox_shim() {
            let home = home();
            update(&home, "1.0", &Fake::default()).await;
            std::fs::remove_file(home.layout.bin().join("ophio-sandbox-shim")).unwrap();
            update(&home, "2.0", &Fake::running()).await;
            assert!(
                !home
                    .layout
                    .previous()
                    .join("bin/ophio-sandbox-shim")
                    .exists()
            );
            let host = Fake::running();
            let (id, release) = stage(&home.layout, &home.layout.previous(), &host)
                .await
                .unwrap();
            assert_eq!(release.version, "1.0");
            let outcome = apply(&home.layout, &id, true, &host, &mut std::future::pending())
                .await
                .unwrap();
            assert_eq!(outcome.result, Result_::Installed, "{}", outcome.message);
            assert_eq!(installed_version(&home), "1.0");
            // A download still has to carry it.
            let bare = download(&home, "3.0");
            std::fs::remove_file(bare.join("ophio-sandbox-shim")).unwrap();
            assert!(stage(&home.layout, &bare, &host).await.is_err());
        }

        #[tokio::test]
        async fn goes_back_when_the_new_version_does_not_work() {
            let home = home();
            update(&home, "1.0", &Fake::default()).await;
            let host = Fake::running();
            host.health
                .borrow_mut()
                .push_back(Err("it isn't answering".into()));
            let outcome = update(&home, "2.0", &host).await;
            assert_eq!(outcome.result, Result_::RolledBack);
            assert!(outcome.message.contains("it isn't answering"));
            assert_eq!(
                host.calls(),
                [
                    "stop",
                    "start 2.0",
                    "healthy",
                    "stop",
                    "start 1.0",
                    "healthy"
                ]
            );
            assert_eq!(installed_version(&home), "1.0");
            let release: Release = read_json(&home.layout.installed()).unwrap();
            assert_eq!(release.version, "1.0");
            assert_eq!(
                std::fs::read_to_string(home.layout.app().join("ophio.apk")).unwrap(),
                "apk 1.0"
            );
            assert!(!home.layout.previous().exists());
            assert!(!home.layout.staging().exists());
        }

        #[tokio::test(start_paused = true)]
        async fn waits_until_idle_for_a_minute() {
            let home = home();
            update(&home, "1.0", &Fake::default()).await;
            let host = Fake::running();
            host.busy.borrow_mut().extend([
                vec!["“Fix it” is working".to_owned()],
                Vec::new(),
                vec!["an approval is waiting for an answer".to_owned()],
            ]);
            let (id, _) = stage(&home.layout, &download(&home, "2.0"), &host)
                .await
                .unwrap();
            let started = Instant::now();
            let outcome = apply(&home.layout, &id, false, &host, &mut std::future::pending())
                .await
                .unwrap();
            assert_eq!(outcome.result, Result_::Installed);
            assert!(started.elapsed() >= IDLE_POLL * 2 + IDLE_FOR);
            let calls = host.calls();
            let first_stop = calls.iter().position(|c| c == "stop").unwrap();
            assert!(calls[..first_stop].iter().filter(|c| *c == "busy").count() >= 4);
            let log = std::fs::read_to_string(home.layout.log()).unwrap();
            assert!(log.contains("Waiting until Øphio is idle: “Fix it” is working."));
            assert!(log.contains("an approval is waiting for an answer"));
        }

        /// A snapshot with one run, ended or not, and a question it asked
        /// that nobody has answered.
        fn asked(ended: bool) -> Snapshot {
            use protocol::capability::{Availability, HostCapabilities, HostOs, InputScope};
            let (run_id, task_id) = (protocol::RunId::new(), protocol::TaskId::new());
            let run = serde_json::json!({
                "run_id": run_id, "task_id": task_id, "harness": "codex",
                "requested_model": null, "actual_model": null, "requested_effort": null,
                "actual_effort": null, "account_label": null, "native_session": null,
                "state": if ended { "completed" } else { "working" },
                "admitted_at": 1, "dispatched_at": 1, "acknowledged_at": 1,
                "ended_at": ended.then_some(2), "children": [], "outcome": null,
                "holds_writer": false,
            });
            let approval = serde_json::json!({
                "approval_id": protocol::ApprovalId::new(), "run_id": run_id,
                "task_id": task_id, "project_id": protocol::ProjectId::new(),
                "native_request_id": "question", "kind": "question",
                "title": "Who should be the first pilot volunteer?", "detail": null,
                "detail_lines": [], "raw_request": null, "scope": "This request once",
                "revision": 1, "state": "pending", "requested_at": 1,
                "expires_at": null, "resolved_by": null, "resolved_at": null,
            });
            let a = Availability::Available;
            Snapshot {
                journal_head: 0,
                host: protocol::model::HostInfo {
                    inbox_path: None,
                    host_id: protocol::HostId::new(),
                    name: "studio".into(),
                    os_label: "Linux".into(),
                    companion_version: "0.1.0".into(),
                    boot_epoch: protocol::BootEpoch::new(),
                    started_at: protocol::time::Timestamp(0),
                    capabilities: HostCapabilities {
                        conversation_access: None,
                        os: HostOs::Linux,
                        desktop_backend: "test".into(),
                        capture: a.clone(),
                        input: a.clone(),
                        input_scope: InputScope::None,
                        terminal: a.clone(),
                        lock_session: a.clone(),
                        speech: a.clone(),
                        library: a.clone(),
                        transfers: a.clone(),
                        computer_use: a.clone(),
                        telegram: a.clone(),
                        discord: a,
                        features: Vec::new(),
                        window_actions: None,
                        unlock_session: None,
                        app_updates: None,
                    },
                },
                projects: Vec::new(),
                tasks: Vec::new(),
                runs: vec![serde_json::from_value(run).unwrap()],
                approvals: vec![serde_json::from_value(approval).unwrap()],
                attention: Vec::new(),
                targets: Vec::new(),
                control: Vec::new(),
                library_roots: Vec::new(),
                older_tasks: None,
                withheld_approvals: Vec::new(),
            }
        }

        #[test]
        fn a_question_its_agent_went_on_without_does_not_hold_an_update() {
            assert_eq!(
                busy(&asked(false)),
                vec![
                    "a task has an agent running".to_owned(),
                    "an approval is waiting for an answer".to_owned(),
                ]
            );
            // Its run has ended, so the question was handed back to the agent
            // and is kept on disk for a later answer.
            assert_eq!(busy(&asked(true)), Vec::<String>::new());
            let mut withheld = asked(true);
            withheld
                .withheld_approvals
                .push(protocol::ApprovalId::new());
            assert_eq!(
                busy(&withheld),
                vec!["an approval is waiting for an answer".to_owned()]
            );
        }

        #[tokio::test]
        async fn cancelling_while_waiting_changes_nothing() {
            let home = home();
            update(&home, "1.0", &Fake::default()).await;
            let host = Fake::running();
            host.busy.borrow_mut().push_back(vec!["busy".into()]);
            let (id, _) = stage(&home.layout, &download(&home, "2.0"), &host)
                .await
                .unwrap();
            let outcome = apply(&home.layout, &id, false, &host, &mut std::future::ready(()))
                .await
                .unwrap();
            assert_eq!(outcome.result, Result_::Cancelled);
            assert_eq!(host.calls(), ["busy"]);
            assert_eq!(installed_version(&home), "1.0");
            assert!(!home.layout.staging().exists());
        }

        #[tokio::test]
        async fn rejects_a_download_without_its_programs() {
            let home = home();
            let source = download(&home, "1.0");
            std::fs::remove_file(source.join("computer-mcp")).unwrap();
            let error = stage(&home.layout, &source, &Fake::default())
                .await
                .unwrap_err();
            assert!(error.to_string().contains("computer-mcp"), "{error}");
            assert!(staged(&home).is_empty());
            std::fs::write(source.join("computer-mcp"), "#!/bin/sh").unwrap();
            assert!(
                stage(&home.layout, &source, &Fake::default())
                    .await
                    .is_err()
            );
            assert!(!home.layout.staging().exists());
        }

        #[tokio::test]
        async fn sweeps_what_an_interrupted_update_left() {
            let home = home();
            update(&home, "1.0", &Fake::default()).await;
            let layout = &home.layout;
            // Interrupted between moving the programs aside and putting the
            // new ones in place.
            private_dir(&layout.partial()).unwrap();
            std::fs::rename(layout.bin(), layout.partial().join("bin")).unwrap();
            std::fs::rename(layout.app(), layout.partial().join("app")).unwrap();
            private_dir(&layout.staging().join("1-1/bin")).unwrap();
            std::fs::write(layout.staging().join("1-1/bin/ophio"), "big").unwrap();
            sweep(layout, None).unwrap();
            assert_eq!(installed_version(&home), "1.0");
            assert!(layout.app().join("ophio.apk").is_file());
            assert!(!layout.partial().exists());
            assert!(!layout.staging().exists());
            std::fs::write(layout.bin().join(".ophio.new"), "half").unwrap();
            sweep(layout, None).unwrap();
            assert!(!layout.bin().join(".ophio.new").exists());
        }

        #[tokio::test]
        async fn unpacks_a_download_archive() {
            let home = home();
            let source = download(&home, "1.0");
            let archive = home.root.join("ophio.tar.gz");
            let pack = |extra: Option<&str>| {
                let file = std::fs::File::create(&archive).unwrap();
                let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
                    file,
                    flate2::Compression::fast(),
                ));
                tar.append_dir_all("ophio-1.0-x86_64-linux", &source)
                    .unwrap();
                if let Some(extra) = extra {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(1);
                    header.set_mode(0o644);
                    header.set_cksum();
                    tar.append_data(&mut header, extra, &b"x"[..]).unwrap();
                }
                tar.into_inner().unwrap().finish().unwrap();
            };
            pack(None);
            let host = Fake::default();
            let (id, release) = stage(&home.layout, &archive, &host).await.unwrap();
            assert_eq!(release.version, "1.0");
            assert_eq!(release.apps, ["ophio.apk"]);
            let root = home.layout.staging().join(&id);
            assert!(!root.join("download").exists());
            assert!(root.join("icons/hicolor/64x64/apps/ophio.png").is_file());
            sweep(&home.layout, None).unwrap();
            pack(Some("elsewhere/file"));
            let error = stage(&home.layout, &archive, &host).await.unwrap_err();
            assert_eq!(error.to_string(), Error::Ended(UNSAFE.into()).to_string());
            assert!(staged(&home).is_empty());
        }

        #[test]
        fn trims_the_log_at_a_line() {
            let temp = tempfile::tempdir().unwrap();
            let log = temp.path().join("update.log");
            let lines: String = (0..100).map(|i| format!("line {i:03}\n")).collect();
            std::fs::write(&log, &lines).unwrap();
            trim(&log, 50);
            let kept = std::fs::read_to_string(&log).unwrap();
            assert!(kept.len() <= 50);
            assert!(kept.starts_with("line "));
            assert!(kept.ends_with("line 099\n"));
        }

        #[test]
        fn finds_listeners_beyond_this_computer() {
            let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:B7CC 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 111 1 0000000000000000 100 0 0 10 0
   1: 0132A864:B7CC 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 222 1 0000000000000000 100 0 0 10 0
   2: 0132A864:B7CD 0232A864:0050 01 00000000:00000000 00:00000000 00000000  1000        0 333 1 0000000000000000 100 0 0 10 0
   3: 0132A864:B7CE 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 444 1 0000000000000000 100 0 0 10 0
";
            let ours: std::collections::HashSet<String> =
                ["111", "222", "333"].map(String::from).into();
            assert_eq!(listening(table, &ours), ["100.168.50.1:47052"]);
            let v6 = "  sl  local_address remote_address st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:B7CC 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 555 1
   1: 00000000000000000000000001000000:B7CC 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 666 1
";
            let ours: std::collections::HashSet<String> = ["555", "666"].map(String::from).into();
            assert_eq!(listening(v6, &ours), ["[::]:47052"]);
        }

        /// Next up waits on disk through a restart, so it doesn't hold an
        /// update; an agent that is working does.
        #[tokio::test]
        async fn next_up_does_not_hold_an_update_and_comes_back_after_a_restart() {
            use protocol::{
                Command, CommandEnvelope, ProjectId, ReceiptStatus,
                capability::HarnessKind,
                command::{CommandResult, TaskPlacement},
                model::TaskKind,
                queue::{QueueAuthorization, QueueItemState},
            };
            use services::{Caller, CompanionHub, Hub, testing::FakeClock};
            use std::sync::Arc;

            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/update-tests")
                .join(protocol::HostId::new().to_string());
            let folder = path.with_extension("project");
            std::fs::create_dir_all(&folder).unwrap();
            let clock = Arc::new(FakeClock::default());
            let harness = Arc::new(services::testing::FakeHarness::default());
            let open = || {
                CompanionHub::builder()
                    .data_dir(&path)
                    .clock(clock.clone())
                    .harness(harness.clone())
                    .bridge(services::bridge::BridgeConfig::new(
                        "synthetic-bridge",
                        "synthetic-socket",
                    ))
                    .build()
            };
            let owner = Caller::local_owner();
            let snapshot = |hub: &CompanionHub| {
                let hub = hub.clone();
                let owner = owner.clone();
                async move {
                    match hub
                        .query(
                            &owner,
                            Query::Snapshot {
                                reads_withheld: Some(true),
                            },
                        )
                        .await
                        .unwrap()
                    {
                        QueryData::Snapshot(snapshot) => *snapshot,
                        _ => panic!("snapshot"),
                    }
                }
            };
            let create = |hub: &CompanionHub, project_id: ProjectId, placement| {
                let hub = hub.clone();
                let owner = owner.clone();
                async move {
                    let receipt = hub
                        .submit(
                            &owner,
                            CommandEnvelope::new(Command::CreateTask {
                                github: None,
                                agent_chosen: None,
                                mode: None,
                                project_id,
                                kind: TaskKind::Coding,
                                title: Some("Fix it".into()),
                                prompt: "fix it".into(),
                                bundle: None,
                                harness: HarnessKind::ClaudeCode,
                                model: None,
                                effort: None,
                                launch_policy: None,
                                branch: None,
                                mission: None,
                                target_id: None,
                                placement,
                                start: None,
                                place: None,
                                browser: None,
                                conversation: None,
                                access: None,
                                review: None,
                            }),
                        )
                        .await;
                    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
                    match receipt.result.unwrap() {
                        CommandResult::TaskCreated {
                            task_id,
                            queue_item_id,
                        } => (task_id, queue_item_id),
                        _ => panic!("task"),
                    }
                }
            };
            let queue = |hub: &CompanionHub, project_id| {
                let hub = hub.clone();
                let owner = owner.clone();
                async move {
                    match hub
                        .query(&owner, Query::Queue { project_id })
                        .await
                        .unwrap()
                    {
                        QueryData::Queue { queue } => queue,
                        _ => panic!("queue"),
                    }
                }
            };

            let hub = open().await.unwrap();
            let project_id = match hub
                .submit(
                    &owner,
                    CommandEnvelope::new(Command::AddProject {
                        host_path: folder.to_string_lossy().into_owned(),
                        name: Some("Test project".into()),
                    }),
                )
                .await
                .result
                .unwrap()
            {
                CommandResult::ProjectAdded { project_id } => project_id,
                _ => panic!("project"),
            };
            let (queued, item) = create(
                &hub,
                project_id,
                TaskPlacement::Queue {
                    position: None,
                    authorization: QueueAuthorization::AskBeforeStart,
                },
            )
            .await;
            let item = item.expect("a Next up item");
            let before = queue(&hub, project_id).await;
            let shown = snapshot(&hub).await;
            assert_eq!(
                shown
                    .tasks
                    .iter()
                    .find(|t| t.task_id == queued)
                    .unwrap()
                    .state,
                TaskState::Accepted
            );
            assert_eq!(busy(&shown), Vec::<String>::new());

            hub.shutdown().await.unwrap();
            drop(hub);
            let hub = open().await.unwrap();
            let after = queue(&hub, project_id).await;
            assert_eq!(after.items, before.items);
            assert_eq!(after.paused, before.paused);
            let back = &after.items[0];
            assert_eq!(back.item_id, item);
            assert_eq!(back.task_id, queued);
            assert_eq!(back.authorization, QueueAuthorization::AskBeforeStart);
            assert!(!matches!(back.state, QueueItemState::Started { .. }));
            let shown = snapshot(&hub).await;
            assert_eq!(
                shown
                    .tasks
                    .iter()
                    .find(|t| t.task_id == queued)
                    .unwrap()
                    .state,
                TaskState::Accepted
            );
            assert_eq!(busy(&shown), Vec::<String>::new());

            create(&hub, project_id, TaskPlacement::StartNow).await;
            let reasons = busy(&snapshot(&hub).await);
            assert!(
                reasons.iter().any(|r| r.starts_with("“Fix it”")),
                "{reasons:?}"
            );

            hub.shutdown().await.unwrap();
            drop(hub);
            std::fs::remove_dir_all(&path).unwrap();
            std::fs::remove_dir_all(&folder).unwrap();
        }
    }
}
