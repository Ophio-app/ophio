// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result};
use ports::PRODUCT_NAME;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Names the command, the login service and the settings and data directories.
pub const APP_NAME: &str = ports::PROGRAM_NAME;
pub const EARLIER_APP_NAME: &str = ports::EARLIER_PROGRAM_NAME;
pub const PAIR_SCHEME: &str = APP_NAME;
pub const DEFAULT_PORT: u16 = 47100;

pub(crate) fn running_build() -> Option<services::report_fixes::RunningBuildInfo> {
    use sha2::{Digest, Sha256};
    let commit = option_env!("OPHIO_SOURCE_COMMIT")?;
    let bytes = std::fs::read(std::env::current_exe().ok()?).ok()?;
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(services::report_fixes::RunningBuildInfo {
        id: digest[..12].into(),
        commit: commit.into(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub port: u16,
    /// Private problem report intake. None keeps reports on this computer.
    pub intake_url: Option<String>,
    /// Received files live outside Ophio's private folders. None uses Downloads.
    pub inbox_path: Option<PathBuf>,
    /// Explicit addresses. Unspecified addresses require both explicit listing
    /// and LAN opt-in. Automatic discovery never adds wildcard addresses.
    pub listen: Vec<IpAddr>,
    pub allow_lan: bool,
    pub retention_days: u16,
    pub desktop_enabled: bool,
    pub roots: Vec<LibraryRootConfig>,
    pub voice_model: String,
    pub channels: channels::Config,
    /// Where tasks started "On its own" get their folders. None uses
    /// `Ophio` in the home folder.
    pub one_offs_path: Option<PathBuf>,
    pub pipeline: Option<Box<services::report_fixes::PipelineConfig>>,
    /// The owner's inbox of problem reports others send to their intake.
    /// None raises nothing for them.
    pub report_inbox: Option<services::report_inbox::InboxConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryRootConfig {
    pub root_id: protocol::LibraryRootId,
    pub label: String,
    pub path: PathBuf,
    pub writable: bool,
    #[serde(default = "default_root_kind")]
    pub kind: protocol::library::RootKind,
    #[serde(default)]
    pub project_id: Option<protocol::ProjectId>,
    #[serde(default)]
    pub harness: Option<protocol::capability::HarnessKind>,
}
fn default_root_kind() -> protocol::library::RootKind {
    protocol::library::RootKind::Notes
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            intake_url: None,
            inbox_path: None,
            listen: vec![],
            allow_lan: false,
            retention_days: 30,
            desktop_enabled: false,
            roots: vec![],
            voice_model: speech::DEFAULT_MODEL.into(),
            channels: channels::Config::default(),
            one_offs_path: None,
            pipeline: None,
            report_inbox: None,
        }
    }
}

fn downloads_directory() -> Option<PathBuf> {
    let user = directories::UserDirs::new()?;
    // One compiled path on every platform, including the home fallback.
    available_downloads(Some(user.home_dir()), user.download_dir())
}
fn available_downloads(home: Option<&Path>, downloads: Option<&Path>) -> Option<PathBuf> {
    let directory = downloads
        .map(Path::to_owned)
        .or_else(|| home.map(|p| p.join("Downloads")))?;
    directory.is_dir().then_some(directory)
}

impl Config {
    pub fn inbox_directory(&self) -> Option<PathBuf> {
        self.inbox_path
            .clone()
            .or_else(|| downloads_directory().map(|p| p.join(APP_NAME)))
    }

    /// Resolve for this use without replacing the person's saved preference.
    /// An unavailable automatic default disables receiving, not startup.
    pub fn resolved_inbox_directory(&self, paths: &Paths) -> Result<Option<PathBuf>> {
        let Some(mut inbox) = self.inbox_directory() else {
            return Ok(None);
        };
        for private in [&paths.data, &paths.config, &paths.runtime] {
            match services::inbox::outside_data(&inbox, private) {
                Ok(path) => inbox = path,
                Err(error) if self.inbox_path.is_some() => return Err(error.into()),
                Err(_) => return Ok(None),
            }
        }
        Ok(Some(inbox))
    }

    pub fn retention(&self) -> store::Retention {
        store::Retention {
            max_age_ms: i64::from(self.retention_days) * 86_400_000,
            ..store::Retention::default()
        }
    }
    pub fn root_specs(&self) -> Vec<ports::RootSpec> {
        self.roots
            .iter()
            .map(|r| ports::RootSpec {
                root_id: r.root_id,
                label: r.label.clone(),
                path: r.path.clone(),
                kind: r.kind,
                writable: r.writable,
                project_id: r.project_id,
                harness: r.harness,
                only: None,
            })
            .collect()
    }
    pub fn validate(&mut self, paths: &Paths) -> Result<()> {
        self.resolved_inbox_directory(paths)?;
        self.channels
            .validate()
            .map_err(|e| Error::Config(e.to_string()))?;
        crate::frontdoors::model(&self.voice_model)?;
        if self.retention_days == 0 {
            return Err(Error::Config("retention days must be positive".into()));
        }
        if self
            .one_offs_path
            .as_ref()
            .is_some_and(|p| !p.is_absolute())
        {
            return Err(Error::Config(
                "one_offs_path must be an absolute path".into(),
            ));
        }
        let private = [&paths.data, &paths.config, &paths.runtime]
            .into_iter()
            .map(|p| ports::library::canonical(p))
            .collect::<std::io::Result<Vec<_>>>()?;
        if let Some(one_offs) = &self.one_offs_path {
            let one_offs = ports::library::resolved(one_offs).unwrap_or_else(|_| one_offs.clone());
            if private
                .iter()
                .any(|p| p.starts_with(&one_offs) || one_offs.starts_with(p))
            {
                return Err(Error::Config(
                    "one_offs_path must not overlap Øphio's own folders".into(),
                ));
            }
        }
        let mut seen = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for root in &mut self.roots {
            root.path = ports::library::canonical(&root.path)?;
            if !root.path.is_dir() || root.path.to_str().is_none() {
                return Err(Error::Config(
                    "Library root must be an existing directory with a UTF-8 path".into(),
                ));
            }
            if private
                .iter()
                .any(|p| p.starts_with(&root.path) || root.path.starts_with(p))
            {
                return Err(Error::Config(
                    "Library folders must not overlap Øphio's own folders".into(),
                ));
            }
            if !seen.insert(root.path.clone()) || !ids.insert(root.root_id) {
                return Err(Error::Config("Library root already registered".into()));
            }
        }
        // A read-only folder inside a writable one would be writable through
        // the outer folder, and the reverse would promise writes it refuses.
        for outer in &self.roots {
            if let Some(inner) = self.roots.iter().find(|inner| {
                inner.root_id != outer.root_id
                    && inner.path.starts_with(&outer.path)
                    && inner.writable != outer.writable
            }) {
                return Err(Error::Config(format!(
                    "The Library folder “{}” ({}) is inside “{}” ({}), and only one of them is read-only. Make both read-only or both writable, or remove one of them.",
                    inner.label,
                    inner.path.display(),
                    outer.label,
                    outer.path.display()
                )));
            }
        }
        Ok(())
    }
    pub fn apply(&mut self, request: crate::local::OwnerRequest, paths: &Paths) -> Result<bool> {
        use crate::local::OwnerRequest;
        match request {
            // Turning sharing off cannot make the settings invalid, and never
            // waits on one that has become invalid since, such as a Library
            // folder that was removed.
            OwnerRequest::Desktop {
                enabled: Some(false),
            } => {
                self.desktop_enabled = false;
                return Ok(true);
            }
            OwnerRequest::Desktop {
                enabled: Some(true),
            } => self.desktop_enabled = true,
            OwnerRequest::RootsAdd {
                path,
                label,
                writable,
            } => {
                let path = ports::library::canonical(&path)?;
                let label = label.unwrap_or_else(|| {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
                self.roots.push(LibraryRootConfig {
                    root_id: protocol::LibraryRootId::new(),
                    label,
                    path,
                    writable,
                    kind: default_root_kind(),
                    project_id: None,
                    harness: None,
                });
            }
            OwnerRequest::RootsSet {
                root_id,
                kind,
                project_id,
                clear_project,
                writable,
                harness,
                clear_harness,
            } => {
                let root = self
                    .roots
                    .iter_mut()
                    .find(|r| r.root_id == root_id)
                    .ok_or_else(|| Error::Config("Library root not found".into()))?;
                if let Some(kind) = kind {
                    root.kind = kind;
                }
                if let Some(writable) = writable {
                    root.writable = writable;
                }
                if clear_project {
                    root.project_id = None;
                } else if project_id.is_some() {
                    root.project_id = project_id;
                }
                if clear_harness {
                    root.harness = None;
                } else if harness.is_some() {
                    root.harness = harness;
                }
            }
            OwnerRequest::RootsRemove { root_id } => {
                if !self.roots.iter().any(|r| r.root_id == root_id) {
                    return Err(Error::Config("Library root not found".into()));
                }
                self.roots.retain(|r| r.root_id != root_id);
            }
            OwnerRequest::InboxSet { path } => self.inbox_path = Some(path),
            OwnerRequest::RetentionSet { days } => self.retention_days = days,
            OwnerRequest::RootsList | OwnerRequest::Desktop { enabled: None } => return Ok(false),
            OwnerRequest::Voice { action } => match action {
                crate::frontdoors::VoiceCommand::Use { model }
                | crate::frontdoors::VoiceCommand::Install { model } => self.voice_model = model,
                crate::frontdoors::VoiceCommand::Remove { .. } => {}
                crate::frontdoors::VoiceCommand::Status => return Ok(false),
            },
            OwnerRequest::Channels {
                action: crate::frontdoors::ChannelCommand::Status,
            } => return Ok(false),
            OwnerRequest::Channels { action } => {
                crate::frontdoors::apply_channel_config(&mut self.channels, action)?
            }
            OwnerRequest::TokenSet { .. } | OwnerRequest::TokenClear { .. } => return Ok(false),
            OwnerRequest::FrontdoorStatus | OwnerRequest::Jev { .. } => return Ok(false),
            OwnerRequest::Stop | OwnerRequest::StopTask { .. } | OwnerRequest::Quit => {
                return Err(Error::Config("Stop needs Øphio to be running".into()));
            }
            OwnerRequest::AccountToken { .. } => {
                return Err(Error::Config("Tokens need Øphio to be running".into()));
            }
            OwnerRequest::ProjectSettings { .. } => {
                return Err(Error::Config(
                    "Changing a project needs Øphio to be running".into(),
                ));
            }
            OwnerRequest::Checklist { .. } => {
                return Err(Error::Config("Checklists need Øphio to be running".into()));
            }
            OwnerRequest::OpenPairing
            | OwnerRequest::WaitingPairing
            | OwnerRequest::AnswerPairing { .. } => {
                return Err(Error::Config("Pairing needs Øphio to be running".into()));
            }
        }
        self.validate(paths)?;
        Ok(true)
    }

    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let config: Self = toml::from_str(&text)
                    .map_err(|_| Error::Config("invalid config TOML".into()))?;
                config
                    .channels
                    .validate()
                    .map_err(|e| Error::Config(e.to_string()))?;
                crate::frontdoors::model(&config.voice_model)?;
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|_| Error::Config("cannot encode config".into()))?;
        crate::identity::atomic_write(path, text.as_bytes(), true)
    }

    pub fn addresses(&self, tailscale: &[IpAddr]) -> Result<Vec<std::net::SocketAddr>> {
        let mut ips = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
        ips.extend(
            tailscale
                .iter()
                .copied()
                .filter(|ip| !ip.is_unspecified() && !ip.is_multicast()),
        );
        for ip in &self.listen {
            if ip.is_multicast()
                || (!self.allow_lan && !ip.is_loopback() && !tailscale.contains(ip))
            {
                return Err(Error::Config(
                    "non-tailnet listeners require allow_lan = true".into(),
                ));
            }
            ips.push(*ip);
        }
        ips.sort();
        ips.dedup();
        // A wildcard already covers loopback of its family. Binding both fails.
        let v4_any = ips.contains(&IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let v6_any = ips.contains(&IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED));
        ips.retain(|ip| ip.is_unspecified() || !(v4_any && ip.is_ipv4() || v6_any && ip.is_ipv6()));
        Ok(ips.into_iter().map(|ip| (ip, self.port).into()).collect())
    }
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub runtime: PathBuf,
}

impl Paths {
    /// This install's directories. Settings and data an earlier install kept
    /// under [`EARLIER_APP_NAME`] are moved here first.
    pub fn discover() -> Result<Self> {
        let (paths, problems) = Self::named(APP_NAME)?.carry_over(&Self::named(EARLIER_APP_NAME)?);
        for problem in problems {
            eprintln!("{problem}");
        }
        Ok(paths)
    }

    pub(crate) fn named(name: &str) -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", name)
            .ok_or_else(|| Error::Config("home directory unavailable".into()))?;
        Ok(Self {
            config: dirs.config_dir().to_owned(),
            data: dirs.data_local_dir().to_owned(),
            runtime: dirs
                .runtime_dir()
                .unwrap_or(dirs.data_local_dir())
                .join("run"),
        })
    }

    /// Renames the earlier install's settings and data directories to these,
    /// each only when its new directory does not exist yet: an existing one is
    /// never merged into or replaced. A directory that cannot move stays in use
    /// where it is, so nothing starts empty; the sentences returned say why.
    pub fn carry_over(self, earlier: &Paths) -> (Paths, Vec<String>) {
        let started = Instant::now();
        loop {
            let moves = [
                Move::needed("settings", &earlier.config, &self.config),
                Move::needed("data", &earlier.data, &self.data),
            ];
            if moves.iter().all(Option::is_none) {
                return (self, Vec::new());
            }
            // A running earlier version still writes to these directories.
            // Holding its lock also keeps it from starting while they move.
            // A concurrent migration may have removed the old directory. Do
            // not recreate it while deciding whether there is still work to do.
            let lock = moves[1]
                .as_ref()
                .map(|_| earlier.lock_with(false, crate::identity::private_open))
                .transpose();
            // The lock may instead belong to another start of this version,
            // such as the service and a command started together, that is
            // moving them now. Look again once it would have finished.
            if lock.is_err() && started.elapsed() < MOVE_WAIT {
                std::thread::sleep(MOVE_POLL);
                continue;
            }
            let mut problems = Vec::new();
            let stays = match lock {
                Err(error) => {
                    problems.push(still_in_use(error, &moves));
                    moves.each_ref().map(Option::is_some)
                }
                Ok(held) => {
                    // Windows cannot rename a directory while a file in it is
                    // open, and the lock is a file in the data directory.
                    // Having held it shows the earlier version is not running;
                    // if it starts now, its own open lock fails the rename
                    // rather than letting the directory move under it.
                    #[cfg(windows)]
                    drop(held);
                    #[cfg(not(windows))]
                    let _held = held;
                    let results = moves.map(|step| step.map(Move::run));
                    // Another start checking the lock also holds it open for a
                    // moment, which fails the rename too. Look again while
                    // there is time; if that start moved them, nothing is left.
                    #[cfg(windows)]
                    if results.iter().any(|r| matches!(r, Some(Err(_))))
                        && started.elapsed() < MOVE_WAIT
                    {
                        std::thread::sleep(MOVE_POLL);
                        continue;
                    }
                    results.map(|result| match result {
                        Some(Err(problem)) => {
                            problems.push(problem);
                            true
                        }
                        _ => false,
                    })
                }
            };
            let mut paths = self;
            if stays[0] {
                paths.config.clone_from(&earlier.config);
            }
            if stays[1] {
                paths.data.clone_from(&earlier.data);
                paths.runtime.clone_from(&earlier.runtime);
            }
            return (paths, problems);
        }
    }

    pub fn initialize(&self) -> Result<()> {
        for path in [
            &self.config,
            &self.data,
            &self.runtime,
            &self.data.join("artifacts"),
            &self.data.join("staging"),
            &self.data.join("logs"),
        ] {
            crate::identity::private_dir(path)?;
        }
        Ok(())
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }
    pub fn socket(&self) -> PathBuf {
        self.runtime.join("owner.sock")
    }
    pub fn database(&self) -> PathBuf {
        self.data.join("db")
    }
    pub fn identity(&self) -> PathBuf {
        self.data.join("identity.json")
    }

    pub fn lock(&self) -> Result<store::InstanceLock> {
        self.lock_with(true, crate::identity::private_open)
    }

    fn lock_with(
        &self,
        recreate: bool,
        mut open: impl FnMut(&Path) -> Result<fs::File>,
    ) -> Result<store::InstanceLock> {
        let path = self.data.join("instance.lock");
        // Uninstall can remove both the file and its directory. Retry the
        // entire open/validation/acquisition, not just a stale locked handle.
        for attempt in 0..3 {
            let file = match open(&path) {
                Ok(file) => file,
                Err(Error::Io(error))
                    if recreate && attempt < 2 && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    crate::identity::private_dir(&self.data)?;
                    continue;
                }
                // On Unix an unlink between open and fstat gives private_open
                // a zero-link file. Retry without weakening its checks; a
                // persistently invalid file still returns the original error.
                Err(Error::Identity) if attempt < 2 => continue,
                Err(error) => return Err(error),
            };
            if let Some(lock) = lock_if_current(&path, file)? {
                return Ok(lock);
            }
        }
        Err(Error::Running)
    }
}

/// Locks `file`, opened from `path`, if `path` still names it. Windows refuses
/// to delete an open file, so there it always does.
pub(crate) fn lock_if_current(path: &Path, file: fs::File) -> Result<Option<store::InstanceLock>> {
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata()?;
        (meta.dev(), meta.ino())
    };
    let lock = store::InstanceLock::acquire(file).map_err(|_| Error::Running)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match fs::symlink_metadata(path) {
            Ok(meta) if (meta.dev(), meta.ino()) == opened => {}
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(Some(lock))
}

/// How long a start waits for another start that holds the earlier lock, and
/// how often it looks. The move itself is two renames.
const MOVE_WAIT: Duration = Duration::from_secs(2);
const MOVE_POLL: Duration = Duration::from_millis(50);

/// One of an earlier install's directories and the name it moves to.
struct Move {
    what: &'static str,
    from: PathBuf,
    to: PathBuf,
}

impl Move {
    /// Each program keeps a directory named after itself: on Linux the
    /// settings or data directory itself, on Windows its AppData parent.
    /// Nothing moves without an earlier directory, or when the new one exists.
    fn needed(what: &'static str, earlier: &Path, current: &Path) -> Option<Self> {
        let from = earlier
            .ancestors()
            .find(|p| p.ends_with(EARLIER_APP_NAME))?;
        let to = current.ancestors().find(|p| p.ends_with(APP_NAME))?;
        (present(from) && !present(to)).then(|| Self {
            what,
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }

    fn run(self) -> std::result::Result<(), String> {
        match rename_new(&self.from, &self.to) {
            // Another start moved it first, or made the new directory.
            Err(_) if present(&self.to) => Ok(()),
            Err(error) => Err(format!(
                "{PRODUCT_NAME} could not move its {} from {} to {} ({error}), so it is using {} until the move succeeds.",
                self.what,
                self.from.display(),
                self.to.display(),
                self.from.display()
            )),
            Ok(()) => Ok(()),
        }
    }
}

fn still_in_use(error: Error, moves: &[Option<Move>]) -> String {
    let places = moves
        .iter()
        .flatten()
        .map(|m| m.from.display().to_string())
        .collect::<Vec<_>>()
        .join(" and ");
    match error {
        Error::Running => format!(
            "The earlier {EARLIER_APP_NAME} program is still running, so {PRODUCT_NAME} is using its settings and data where they are, in {places}. Stop that program, then start {PRODUCT_NAME} again to move them."
        ),
        error => format!(
            "{PRODUCT_NAME} could not check whether the earlier {EARLIER_APP_NAME} program is running ({error}), so it is using its settings and data where they are, in {places}."
        ),
    }
}

fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// A rename on the same filesystem that never replaces an existing entry.
fn rename_new(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).map_err(Into::into)
    }
    #[cfg(not(target_os = "linux"))]
    {
        fs::rename(from, to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phone_uses_the_same_names() {
        let brand = include_str!("../../../client/src/brand.ts");
        for (name, value) in [
            ("PRODUCT_NAME", ports::PRODUCT_NAME),
            ("PROGRAM_NAME", APP_NAME),
            ("PAIRING_SCHEME", PAIR_SCHEME),
        ] {
            assert!(
                brand.contains(&format!("export const {name} = \"{value}\";")),
                "{name}"
            );
        }
    }

    #[test]
    fn missing_home_or_downloads_leaves_no_default_folder() {
        assert_eq!(available_downloads(None, None), None);
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(available_downloads(Some(temp.path()), None), None);
        let downloads = temp.path().join("Downloads");
        fs::create_dir(&downloads).unwrap();
        assert_eq!(
            available_downloads(Some(temp.path()), None),
            Some(downloads.clone())
        );
        assert_eq!(available_downloads(None, Some(&downloads)), Some(downloads));
    }

    #[test]
    fn settings_save_preserves_unset_and_explicit_inbox_preferences() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("runtime"),
        };
        paths.initialize().unwrap();
        for preference in [None, Some(temp.path().join("received/../pictures"))] {
            let mut config = Config {
                inbox_path: preference.clone(),
                ..Default::default()
            };
            config
                .apply(
                    crate::local::OwnerRequest::RetentionSet { days: 14 },
                    &paths,
                )
                .unwrap();
            config.save(&paths.config_file()).unwrap();
            assert_eq!(
                Config::load(&paths.config_file()).unwrap().inbox_path,
                preference
            );
        }
    }

    fn layout(root: &Path, name: &str) -> Paths {
        Paths {
            config: root.join("config").join(name),
            data: root.join("data").join(name),
            runtime: root.join("runtime").join(name).join("run"),
        }
    }

    fn earlier_install(root: &Path) -> Paths {
        let earlier = layout(root, EARLIER_APP_NAME);
        earlier.initialize().unwrap();
        fs::write(earlier.config_file(), "retention_days = 9\n").unwrap();
        fs::write(earlier.identity(), "earlier identity").unwrap();
        earlier
    }

    #[test]
    fn locking_recreates_removed_data_but_not_unrelated_directories() {
        let temp = tempfile::tempdir().unwrap();
        let paths = layout(temp.path(), APP_NAME);
        let _lock = paths.lock().unwrap();
        assert!(paths.data.join("instance.lock").is_file());
        assert!(!paths.config.exists());
        assert!(!paths.runtime.exists());
        assert!(matches!(paths.lock(), Err(Error::Running)));
    }

    #[test]
    fn migration_lock_does_not_recreate_a_directory_that_moved() {
        let temp = tempfile::tempdir().unwrap();
        let paths = layout(temp.path(), EARLIER_APP_NAME);
        assert!(matches!(
            paths.lock_with(false, crate::identity::private_open),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(!paths.data.exists());
    }

    #[cfg(unix)]
    #[test]
    fn locking_retries_when_uninstall_removes_the_open_file_and_directory() {
        for removed_during_validation in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let paths = layout(temp.path(), APP_NAME);
            crate::identity::private_dir(&paths.data).unwrap();
            let mut calls = 0;
            let _lock = paths
                .lock_with(true, |path| {
                    calls += 1;
                    let file = crate::identity::private_open(path)?;
                    if calls == 1 {
                        fs::remove_file(path).unwrap();
                        fs::remove_dir(&paths.data).unwrap();
                        if removed_during_validation {
                            // The result of private_open's nlink check when
                            // unlink wins the race against its metadata read.
                            return Err(Error::Identity);
                        }
                    }
                    Ok(file)
                })
                .unwrap();
            assert_eq!(calls, 3);
            assert!(matches!(paths.lock(), Err(Error::Running)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn locking_retries_never_accept_an_invalid_private_file() {
        let temp = tempfile::tempdir().unwrap();
        let paths = layout(temp.path(), APP_NAME);
        crate::identity::private_dir(&paths.data).unwrap();
        let path = paths.data.join("instance.lock");
        drop(crate::identity::private_open(&path).unwrap());
        fs::hard_link(&path, paths.data.join("other-link")).unwrap();
        let mut calls = 0;
        assert!(matches!(
            paths.lock_with(true, |path| {
                calls += 1;
                crate::identity::private_open(path)
            }),
            Err(Error::Identity)
        ));
        assert_eq!(calls, 3);
    }

    #[cfg(unix)]
    #[test]
    fn locking_rejects_a_symlink_even_when_it_points_to_the_open_file() {
        let temp = tempfile::tempdir().unwrap();
        let paths = layout(temp.path(), APP_NAME);
        crate::identity::private_dir(&paths.data).unwrap();
        let path = paths.data.join("instance.lock");
        let opened = crate::identity::private_open(&path).unwrap();
        let moved = paths.data.join("moved.lock");
        fs::rename(&path, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &path).unwrap();
        assert!(matches!(lock_if_current(&path, opened), Ok(None)));
        assert!(paths.lock().is_err());
        // Rejecting the path must also release the lock on its old file.
        let _lock =
            store::InstanceLock::acquire(crate::identity::private_open(&moved).unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn locking_stops_after_three_stale_files_without_retaining_any_lock() {
        let temp = tempfile::tempdir().unwrap();
        let paths = layout(temp.path(), APP_NAME);
        crate::identity::private_dir(&paths.data).unwrap();
        let mut stale = Vec::new();
        assert!(matches!(
            paths.lock_with(true, |path| {
                let file = crate::identity::private_open(path)?;
                let moved = paths.data.join(format!("stale-{}", stale.len()));
                fs::rename(path, &moved).unwrap();
                stale.push(moved);
                Ok(file)
            }),
            Err(Error::Running)
        ));
        assert_eq!(stale.len(), 3);
        for path in stale {
            let _lock = store::InstanceLock::acquire(crate::identity::private_open(&path).unwrap())
                .unwrap();
        }
        let _lock = paths.lock().unwrap();
    }

    #[test]
    fn an_earlier_install_moves_to_the_new_names() {
        let temp = tempfile::tempdir().unwrap();
        let earlier = earlier_install(temp.path());
        let current = layout(temp.path(), APP_NAME);
        let (paths, problems) = current.clone().carry_over(&earlier);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            (&paths.config, &paths.data, &paths.runtime),
            (&current.config, &current.data, &current.runtime)
        );
        assert_eq!(
            fs::read_to_string(paths.config_file()).unwrap(),
            "retention_days = 9\n"
        );
        assert_eq!(fs::read(paths.identity()).unwrap(), b"earlier identity");
        assert!(!earlier.config.exists() && !earlier.data.exists());
        // Nothing is left to move at the next start.
        let (again, problems) = current.clone().carry_over(&earlier);
        assert!(problems.is_empty());
        assert_eq!(again.data, current.data);
    }

    #[test]
    fn an_existing_new_directory_is_never_merged_or_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let earlier = earlier_install(temp.path());
        let current = layout(temp.path(), APP_NAME);
        crate::identity::private_dir(&current.config).unwrap();
        fs::write(current.config_file(), "retention_days = 5\n").unwrap();
        let (paths, problems) = current.clone().carry_over(&earlier);
        assert!(problems.is_empty());
        assert_eq!(paths.config, current.config);
        assert_eq!(
            fs::read_to_string(current.config_file()).unwrap(),
            "retention_days = 5\n"
        );
        assert_eq!(
            fs::read_to_string(earlier.config_file()).unwrap(),
            "retention_days = 9\n"
        );
        assert_eq!(fs::read_dir(&current.config).unwrap().count(), 1);
        // The data directory had no new counterpart, so it still moves.
        assert_eq!(paths.data, current.data);
        assert!(!earlier.data.exists());
    }

    #[test]
    fn a_running_earlier_version_keeps_using_its_directories() {
        let temp = tempfile::tempdir().unwrap();
        let earlier = earlier_install(temp.path());
        let running = earlier.lock().unwrap();
        let (paths, problems) = layout(temp.path(), APP_NAME).carry_over(&earlier);
        assert_eq!(
            (&paths.config, &paths.data, &paths.runtime),
            (&earlier.config, &earlier.data, &earlier.runtime)
        );
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("The earlier companion program is still running"));
        assert!(earlier.identity().exists());
        drop(running);
        let (paths, _) = layout(temp.path(), APP_NAME).carry_over(&earlier);
        assert!(paths.identity().exists() && !earlier.data.exists());
    }

    // The other start here renames while it holds the lock, as on Linux.
    // Windows cannot rename the directory the open lock is in, so a start
    // there lets go of it first; the next test covers that.
    #[cfg(not(windows))]
    #[test]
    fn a_start_during_another_starts_move_uses_the_new_names() {
        let temp = tempfile::tempdir().unwrap();
        let earlier = earlier_install(temp.path());
        let current = layout(temp.path(), APP_NAME);
        // The other start holds the earlier lock while it moves both.
        let moving = earlier.lock().unwrap();
        let waiting = std::thread::spawn({
            let (current, earlier) = (current.clone(), earlier.clone());
            move || current.carry_over(&earlier)
        });
        std::thread::sleep(MOVE_POLL * 3);
        rename_new(&earlier.config, &current.config).unwrap();
        rename_new(&earlier.data, &current.data).unwrap();
        drop(moving);
        let (paths, problems) = waiting.join().unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            (&paths.config, &paths.data, &paths.runtime),
            (&current.config, &current.data, &current.runtime)
        );
        assert_eq!(fs::read(paths.identity()).unwrap(), b"earlier identity");
    }

    #[test]
    fn starts_together_all_use_the_new_names() {
        let temp = tempfile::tempdir().unwrap();
        let earlier = earlier_install(temp.path());
        let current = layout(temp.path(), APP_NAME);
        let ready = std::sync::Arc::new(std::sync::Barrier::new(4));
        let starts: Vec<_> = (0..4)
            .map(|_| {
                let (current, earlier, ready) = (current.clone(), earlier.clone(), ready.clone());
                std::thread::spawn(move || {
                    ready.wait();
                    current.carry_over(&earlier)
                })
            })
            .collect();
        for start in starts {
            let (paths, problems) = start.join().unwrap();
            assert!(problems.is_empty(), "{problems:?}");
            assert_eq!(
                (&paths.config, &paths.data, &paths.runtime),
                (&current.config, &current.data, &current.runtime)
            );
        }
        assert_eq!(fs::read(current.identity()).unwrap(), b"earlier identity");
        assert!(!earlier.config.exists() && !earlier.data.exists());
    }

    #[test]
    fn a_directory_that_cannot_move_stays_in_use() {
        let temp = tempfile::tempdir().unwrap();
        let earlier = earlier_install(temp.path());
        let mut current = layout(temp.path(), APP_NAME);
        // A missing parent makes the rename fail after every check passed.
        current.config = temp.path().join("missing/ophio");
        let (paths, problems) = current.clone().carry_over(&earlier);
        assert_eq!(paths.config, earlier.config);
        assert_eq!(paths.data, current.data);
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].starts_with(&format!(
                "Øphio could not move its settings from {} to {}",
                earlier.config.display(),
                current.config.display()
            )),
            "{problems:?}"
        );
        assert!(earlier.config_file().exists());
    }

    #[test]
    fn windows_moves_each_program_directory_above_config_and_data() {
        let temp = tempfile::tempdir().unwrap();
        let named = |name: &str| Paths {
            config: temp.path().join("Roaming").join(name).join("config"),
            data: temp.path().join("Local").join(name).join("data"),
            runtime: temp.path().join("Local").join(name).join("data/run"),
        };
        let earlier = named(EARLIER_APP_NAME);
        earlier.initialize().unwrap();
        fs::write(earlier.identity(), "earlier identity").unwrap();
        let (paths, problems) = named(APP_NAME).carry_over(&earlier);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(paths.data, named(APP_NAME).data);
        assert_eq!(fs::read(paths.identity()).unwrap(), b"earlier identity");
        assert!(paths.config.is_dir());
        for side in ["Roaming", "Local"] {
            assert!(!temp.path().join(side).join(EARLIER_APP_NAME).exists());
        }
    }

    #[test]
    fn private_network_defaults_and_explicit_opt_in() {
        let tail = "100.100.1.2".parse().unwrap();
        let mut config = Config::default();
        assert_eq!(config.addresses(&[tail]).unwrap().len(), 2);
        config.listen.push("0.0.0.0".parse().unwrap());
        assert!(config.addresses(&[tail]).is_err());
        config.allow_lan = true;
        assert_eq!(
            config.addresses(&[tail]).unwrap(),
            vec!["0.0.0.0:47100".parse().unwrap()]
        );
    }
}

#[cfg(test)]
mod library_config_tests {
    use super::*;
    #[test]
    fn pipeline_section_is_explicit_and_missing_files_do_not_enable_it() {
        let absent: Config = toml::from_str("").unwrap();
        assert!(absent.pipeline.is_none());
        let disabled: Config = toml::from_str("[pipeline]\n").unwrap();
        assert!(!disabled.pipeline.unwrap().enabled);
        let enabled: Config = toml::from_str("[pipeline]\nenabled=true\nrepository='example/sample'\nmirror='/tmp/missing-mirror'\nstate='/tmp/missing-state'\nintake_token_file='/no-such-file/token'\n").unwrap();
        assert!(enabled.pipeline.unwrap().readiness().is_err());
    }
    #[test]
    fn root_metadata_round_trip_and_old_defaults() {
        let id = protocol::LibraryRootId::new();
        let old = format!(
            "[[roots]]\nroot_id = '{id}'\nlabel = 'Notes'\npath = '/notes'\nwritable = true\n"
        );
        let mut config: Config = toml::from_str(&old).unwrap();
        assert_eq!(config.roots[0].kind, protocol::library::RootKind::Notes);
        assert_eq!(config.roots[0].project_id, None);
        config.roots[0].kind = protocol::library::RootKind::Drafts;
        config.roots[0].project_id = Some(protocol::ProjectId::new());
        config.roots[0].harness = Some(protocol::capability::HarnessKind::Codex);
        config.roots[0].writable = false;
        let temp = tempfile::tempdir().unwrap();
        // Saving creates an owner-only directory; the temporary one follows the umask.
        let path = temp.path().join("config/config.toml");
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(config.root_specs(), loaded.root_specs());
        assert_eq!(
            loaded.root_specs()[0].kind,
            protocol::library::RootKind::Drafts
        );
        assert!(!loaded.root_specs()[0].writable);
    }

    #[test]
    fn nested_folders_share_one_writability() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("runtime"),
        };
        paths.initialize().unwrap();
        let outer = temp.path().join("documents");
        let inner = outer.join("records");
        fs::create_dir_all(&inner).unwrap();
        let root = |label: &str, path: &Path, writable| LibraryRootConfig {
            root_id: protocol::LibraryRootId::new(),
            label: label.into(),
            path: path.to_owned(),
            writable,
            kind: default_root_kind(),
            project_id: None,
            harness: None,
        };
        for (outer_writable, inner_writable) in [(true, false), (false, true)] {
            let mut config = Config {
                roots: vec![
                    root("Records", &inner, inner_writable),
                    root("Documents", &outer, outer_writable),
                ],
                ..Default::default()
            };
            let Err(Error::Config(message)) = config.validate(&paths) else {
                panic!("nested folders with different writability")
            };
            assert!(
                message.starts_with("The Library folder “Records” (")
                    && message.contains(") is inside “Documents” ("),
                "{message}"
            );
        }
        let mut config = Config {
            roots: vec![
                root("Records", &inner, false),
                root("Documents", &outer, false),
            ],
            ..Default::default()
        };
        config.validate(&paths).unwrap();
    }
}
