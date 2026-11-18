// SPDX-License-Identifier: Apache-2.0
//! Bundle identities are observations, not commands. Launch Services owns app
//! lifetimes; killing `open` cannot withdraw a request it has already queued.
use async_trait::async_trait;
use protocol::voice::VoiceApp;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    fs, io,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use voice_control::{
    Authority, Error, Launcher,
    apps::{AppRole, LaunchApp},
};

const CATALOG: Duration = Duration::from_millis(2000);
const SCAN: Duration = Duration::from_millis(1750);
const REVALIDATE: Duration = Duration::from_millis(500);
const DISPATCH: Duration = Duration::from_millis(3000);
const REAP: Duration = Duration::from_millis(250);
const MAX_PLIST: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 12000;
const MAX_APPS: usize = 2048;
const _: () = {
    assert!(
        DISPATCH.as_millis() + REAP.as_millis() <= voice_control::LAUNCH_AFTER_CHECK.as_millis()
    );
    assert!(
        CATALOG.as_millis() + REVALIDATE.as_millis() + DISPATCH.as_millis() + REAP.as_millis()
            <= voice_control::LAUNCH_LIMIT.as_millis()
    );
};

#[derive(Debug)]
pub struct MacLauncher {
    roots: Vec<PathBuf>,
    home: Option<PathBuf>,
    driver: Arc<dyn NativeDriver>,
}
impl MacLauncher {
    /// Does not inspect the filesystem or contact Launch Services until asked.
    pub fn from_environment(environment: &BTreeMap<String, String>) -> Self {
        let home = environment
            .get("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        let mut roots = vec![
            PathBuf::from("/Applications"),
            PathBuf::from("/System/Applications"),
            PathBuf::from("/System/Library/CoreServices"),
        ];
        if let Some(home) = &home {
            roots.push(home.join("Applications"));
        }
        Self {
            roots,
            home,
            driver: Arc::new(SystemDriver),
        }
    }

    async fn catalog(&self) -> Result<Vec<Bundle>, Error> {
        let roots = self.roots.clone();
        let driver = self.driver.clone();
        bounded_read(CATALOG, move || discover(&roots, driver.as_ref())).await
    }

    async fn dispatch(&self, request: Request, authority: &dyn Authority) -> Result<(), Error> {
        // Preparation may inspect the helper, but must not dispatch anything.
        let prepared = self.driver.prepare(request, self.home.as_deref())?;
        // No await, metadata lookup or command construction between this check
        // and starting the prepared, literal-argument helper.
        authority.check()?;
        let mut pending = prepared
            .start()
            .map_err(|_| unavailable("macOS could not start its app opener"))?;
        match tokio::time::timeout(DISPATCH, pending.wait()).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err(unavailable(
                "macOS refused to open the app or address. Check that it is available and try again",
            )),
            Ok(Err(_)) | Err(_) => {
                let _ = pending.kill();
                let _ = tokio::time::timeout(REAP, pending.wait()).await;
                // Launch Services might already have accepted the request. A
                // reaped helper proves nothing about that queue, so there is no
                // LaunchCheck that could truthfully clear this uncertainty.
                Err(Error::LaunchUnstopped)
            }
        }
    }
}

#[async_trait]
impl Launcher for MacLauncher {
    async fn apps(&self) -> Result<Vec<LaunchApp>, Error> {
        Ok(self.catalog().await?.into_iter().map(|b| b.app).collect())
    }

    async fn launch(&self, app: &VoiceApp, authority: &dyn Authority) -> Result<(), Error> {
        let bundle = self
            .catalog()
            .await?
            .into_iter()
            .find(|b| b.app.id == app.id && b.app.name == app.name)
            .ok_or(Error::StaleTarget)?;
        let driver = self.driver.clone();
        let expected = bundle.app.id.clone();
        let path = bundle.path.clone();
        // Read the selected bundle again after discovery. A detached filesystem
        // read can finish after a timeout, but can never dispatch a launch.
        bounded_read(REVALIDATE, move || {
            let fresh = inspect(&path, driver.as_ref(), None).ok_or(Error::StaleTarget)?;
            if fresh.app.id != expected {
                return Err(Error::StaleTarget);
            }
            Ok(())
        })
        .await?;
        self.dispatch(Request::App(bundle.path), authority).await
    }

    async fn open_url(&self, url: &str, authority: &dyn Authority) -> Result<(), Error> {
        if !safe_url(url) {
            return Err(Error::StaleTarget);
        }
        self.dispatch(Request::Url(url.to_owned()), authority).await
    }
}

fn unavailable(reason: &str) -> Error {
    Error::Unavailable(reason.into())
}
async fn bounded_read<T: Send + 'static>(
    limit: Duration,
    read: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::time::timeout(limit, tokio::task::spawn_blocking(read))
        .await
        .map_err(|_| {
            unavailable(
                "The computer took too long to read its apps. Nothing was opened; try again",
            )
        })?
        .map_err(|_| unavailable("The app list could not be read"))?
}

/// Only absolute HTTP(S) URLs with a nonempty host. No credentials, controls,
/// backslashes or command-line options, including encoded control characters.
fn safe_url(url: &str) -> bool {
    if url.len() > 2048
        || url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return false;
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.contains('@') || host.contains('%') {
        return false;
    }
    let (hostname, port) = if let Some(after) = host.strip_prefix('[') {
        let Some((ip, suffix)) = after.split_once(']') else {
            return false;
        };
        if ip.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        if suffix.is_empty() {
            (ip, None)
        } else if let Some(port) = suffix.strip_prefix(':') {
            (ip, Some(port))
        } else {
            return false;
        }
    } else {
        let (name, port) = host
            .split_once(':')
            .map_or((host, None), |(n, p)| (n, Some(p)));
        if name.is_empty()
            || name
                .chars()
                .any(|c| !c.is_alphanumeric() && c != '-' && c != '.')
        {
            return false;
        }
        (name, port)
    };
    if hostname.is_empty() || port.is_some_and(|p| p.parse::<u16>().ok().is_none_or(|p| p == 0)) {
        return false;
    }
    let bytes = url.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            if at + 2 >= bytes.len() {
                return false;
            }
            let hex = |b: u8| (b as char).to_digit(16);
            let (Some(a), Some(b)) = (hex(bytes[at + 1]), hex(bytes[at + 2])) else {
                return false;
            };
            let value = (a * 16 + b) as u8;
            if value < 0x20 || value == 0x7f || value == b'\\' {
                return false;
            }
            at += 3;
        } else {
            at += 1;
        }
    }
    true
}

#[derive(Debug, Clone, Default)]
struct Info {
    identifier: String,
    name: String,
    display_name: String,
    executable: String,
    package: String,
    category: String,
    schemes: Vec<String>,
    content_types: Vec<String>,
    background: bool,
}
#[derive(Debug)]
struct Bundle {
    path: PathBuf,
    app: LaunchApp,
}

fn discover(roots: &[PathBuf], driver: &dyn NativeDriver) -> Result<Vec<Bundle>, Error> {
    let deadline = Instant::now() + SCAN;
    let browser = driver.default_browser();
    let mut pending: Vec<_> = roots.iter().map(|p| (p.clone(), 0)).collect();
    let mut seen = BTreeSet::new();
    let mut bundles = Vec::new();
    let mut entries = 0;
    let mut readable = false;
    while let Some((directory, depth)) = pending.pop() {
        if Instant::now() >= deadline {
            return Err(unavailable(
                "The computer took too long to read its apps. Nothing was opened; try again",
            ));
        }
        let Ok(children) = fs::read_dir(&directory) else {
            continue;
        };
        readable = true;
        let mut paths = Vec::new();
        for child in children {
            entries += 1;
            if entries > MAX_ENTRIES {
                return Err(unavailable(
                    "The app folders contain too many entries to read safely",
                ));
            }
            let Ok(child) = child else {
                continue;
            };
            let Ok(kind) = child.file_type() else {
                continue;
            };
            // Do not follow aliases/symlinks into arbitrary folders or cycles.
            if kind.is_dir() {
                paths.push(child.path());
            }
        }
        paths.sort();
        for path in paths {
            if Instant::now() >= deadline {
                return Err(unavailable(
                    "The computer took too long to read its apps. Nothing was opened; try again",
                ));
            }
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
            {
                if let Some(bundle) = inspect(&path, driver, browser.as_deref())
                    && seen.insert(bundle.path.clone())
                {
                    bundles.push(bundle);
                    if bundles.len() > MAX_APPS {
                        return Err(unavailable(
                            "There are too many installed apps to read safely",
                        ));
                    }
                }
                // Never discover helpers embedded inside another app bundle.
            } else if depth < 4
                && !path
                    .file_name()
                    .is_some_and(|s| s.as_encoded_bytes().starts_with(b"."))
            {
                pending.push((path, depth + 1));
            }
        }
    }
    if !readable {
        return Err(unavailable("The Mac application folders could not be read"));
    }
    // Launch Services' legacy preference identifies a bundle id, not a copy.
    // If several copies claim it, do not guess which one is the default.
    if bundles
        .iter()
        .filter(|b| b.app.defaults.contains(&AppRole::Browser))
        .count()
        > 1
    {
        for bundle in &mut bundles {
            bundle.app.defaults.retain(|r| *r != AppRole::Browser);
        }
    }
    bundles.sort_by(|a, b| a.app.name.cmp(&b.app.name).then(a.path.cmp(&b.path)));
    Ok(bundles)
}

fn inspect(path: &Path, driver: &dyn NativeDriver, browser: Option<&str>) -> Option<Bundle> {
    let link = fs::symlink_metadata(path).ok()?;
    if !link.is_dir() || link.file_type().is_symlink() {
        return None;
    }
    let path = fs::canonicalize(path).ok()?;
    let contents = path.join("Contents");
    let plist = contents.join("Info.plist");
    // Bundles cannot redirect their metadata or executable outside themselves.
    let plist_path = fs::canonicalize(&plist).ok()?;
    if !plist_path.starts_with(&path) {
        return None;
    }
    let metadata = fs::metadata(&plist_path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_PLIST {
        return None;
    }
    // `take` also bounds a file that grows after its metadata was checked.
    use io::Read;
    let mut data = Vec::new();
    fs::File::open(&plist_path)
        .ok()?
        .take(MAX_PLIST + 1)
        .read_to_end(&mut data)
        .ok()?;
    if data.len() as u64 > MAX_PLIST {
        return None;
    }
    let info = driver.info(&data)?;
    if info.package != "APPL" || info.background || !clean(&info.identifier, 255) {
        return None;
    }
    let mut components = Path::new(&info.executable).components();
    if !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
        || !clean(&info.executable, 255)
        || info.executable.contains('\\')
    {
        return None;
    }
    let executable_link = contents.join("MacOS").join(&info.executable);
    let executable = fs::canonicalize(&executable_link).ok()?;
    if !executable.starts_with(&path) {
        return None;
    }
    let executable_metadata = fs::metadata(&executable).ok()?;
    if !executable_metadata.is_file() || !is_executable(&executable_metadata) {
        return None;
    }
    let name = [&info.display_name, &info.name]
        .into_iter()
        .find(|s| clean(s, 256))
        .cloned()
        .or_else(|| {
            path.file_stem()?
                .to_str()
                .filter(|s| clean(s, 256))
                .map(str::to_owned)
        })?;
    let mut hash = Sha256::new();
    hash.update(b"mac-app-bundle-v1\0");
    hash_path(&mut hash, &path);
    hash_metadata(&mut hash, &link);
    hash_path(&mut hash, &plist_path);
    hash_metadata(&mut hash, &metadata);
    hash.update((data.len() as u64).to_le_bytes());
    hash.update(&data);
    hash_path(&mut hash, &executable);
    hash_metadata(&mut hash, &fs::symlink_metadata(&executable_link).ok()?);
    hash_metadata(&mut hash, &executable_metadata);
    // A concurrent edit during the read must not produce a usable identity.
    let after = fs::metadata(&plist_path).ok()?;
    if metadata_stamp(&metadata) != metadata_stamp(&after) {
        return None;
    }
    let roles = roles(&info);
    let mut defaults = Vec::new();
    if browser == Some(info.identifier.as_str()) && roles.contains(&AppRole::Browser) {
        defaults.push(AppRole::Browser);
    }
    if info.identifier == "com.apple.finder"
        && path == Path::new("/System/Library/CoreServices/Finder.app")
    {
        defaults.push(AppRole::FileManager);
    }
    let mut aliases = vec![info.identifier, info.executable];
    if clean(&info.name, 256) {
        aliases.push(info.name);
    }
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        aliases.push(stem.into());
    }
    aliases.sort();
    aliases.dedup();
    Some(Bundle {
        app: LaunchApp {
            id: format!("mac-app:{:x}", hash.finalize()),
            name,
            aliases,
            roles,
            defaults,
            listed: true,
            detail: Some(path.parent()?.to_string_lossy().into_owned()),
        },
        path,
    })
}
fn clean(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn hash_path(hash: &mut Sha256, path: &Path) {
    let bytes = path.as_os_str().as_encoded_bytes();
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}
fn hash_metadata(hash: &mut Sha256, metadata: &fs::Metadata) {
    hash.update(metadata_stamp(metadata));
}
#[cfg(unix)]
fn metadata_stamp(metadata: &fs::Metadata) -> Vec<u8> {
    use std::os::unix::fs::MetadataExt;
    [
        metadata.dev(),
        metadata.ino(),
        metadata.mode() as u64,
        metadata.size(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
    ]
    .into_iter()
    .flat_map(u64::to_le_bytes)
    .collect()
}
#[cfg(not(unix))]
fn metadata_stamp(metadata: &fs::Metadata) -> Vec<u8> {
    let mut bytes = metadata.len().to_le_bytes().to_vec();
    for time in [metadata.created().ok(), metadata.modified().ok()] {
        bytes.extend(
            time.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos())
                .to_le_bytes(),
        );
    }
    bytes
}
#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn is_executable(_: &fs::Metadata) -> bool {
    true
}

fn roles(info: &Info) -> Vec<AppRole> {
    use AppRole::*;
    let mut roles = BTreeSet::new();
    for (id, role) in [
        ("com.apple.Safari", Browser),
        ("com.apple.Terminal", Terminal),
        ("com.googlecode.iterm2", Terminal),
        ("com.apple.finder", FileManager),
        ("com.apple.TextEdit", TextEditor),
        ("com.apple.mail", Email),
        ("com.apple.calculator", Calculator),
        ("com.apple.systempreferences", Settings),
        ("com.apple.Music", Music),
        ("com.apple.QuickTimePlayerX", Video),
        ("com.apple.Photos", Images),
        ("com.apple.Preview", Documents),
        ("com.apple.iCal", Calendar),
    ] {
        if info.identifier == id {
            roles.insert(role);
        }
    }
    for scheme in &info.schemes {
        match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" => {
                roles.insert(Browser);
            }
            "mailto" => {
                roles.insert(Email);
            }
            _ => {}
        }
    }
    for content in &info.content_types {
        match content.as_str() {
            "public.plain-text" | "public.text" => {
                roles.insert(TextEditor);
            }
            "com.adobe.pdf" => {
                roles.insert(Documents);
            }
            "public.image" | "public.jpeg" | "public.png" => {
                roles.insert(Images);
            }
            "public.audio" | "public.mp3" => {
                roles.insert(Music);
            }
            "public.movie" | "public.video" => {
                roles.insert(Video);
            }
            "com.apple.ical.ics" | "public.calendar-event" => {
                roles.insert(Calendar);
            }
            _ => {}
        }
    }
    match info.category.as_str() {
        "public.app-category.music" => {
            roles.insert(Music);
        }
        "public.app-category.video" => {
            roles.insert(Video);
        }
        _ => {}
    }
    roles.into_iter().collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    App(PathBuf),
    Url(String),
}
trait NativeDriver: Debug + Send + Sync {
    fn info(&self, bytes: &[u8]) -> Option<Info>;
    fn default_browser(&self) -> Option<String>;
    fn prepare(
        &self,
        request: Request,
        home: Option<&Path>,
    ) -> Result<Box<dyn PreparedOpen>, Error>;
}
trait PreparedOpen: Send {
    fn start(self: Box<Self>) -> io::Result<Box<dyn PendingOpen>>;
}
#[async_trait]
trait PendingOpen: Send {
    async fn wait(&mut self) -> io::Result<bool>;
    fn kill(&mut self) -> io::Result<()>;
}

#[derive(Debug)]
struct SystemDriver;
#[cfg(target_os = "macos")]
impl NativeDriver for SystemDriver {
    fn info(&self, bytes: &[u8]) -> Option<Info> {
        native::info(bytes)
    }
    fn default_browser(&self) -> Option<String> {
        native::default_browser()
    }
    fn prepare(
        &self,
        request: Request,
        home: Option<&Path>,
    ) -> Result<Box<dyn PreparedOpen>, Error> {
        let mut command = tokio::process::Command::new("/usr/bin/open");
        command.env_clear().env("PATH", "/usr/bin:/bin");
        if let Some(home) = home {
            command.env("HOME", home);
        }
        command
            .current_dir("/")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        // -a selects this exact bundle, not a bundle-id registration that might
        // resolve to a different copy. -- ends option parsing for all targets.
        match request {
            Request::App(path) => {
                command.arg("-a").arg(path).arg("--");
            }
            Request::Url(url) => {
                command.arg("--").arg(url);
            }
        }
        Ok(Box::new(PreparedCommand(command)))
    }
}
#[cfg(not(target_os = "macos"))]
impl NativeDriver for SystemDriver {
    fn info(&self, _: &[u8]) -> Option<Info> {
        None
    }
    fn default_browser(&self) -> Option<String> {
        None
    }
    fn prepare(&self, _: Request, _: Option<&Path>) -> Result<Box<dyn PreparedOpen>, Error> {
        Err(unavailable("Opening Mac apps requires macOS"))
    }
}
#[cfg(target_os = "macos")]
struct PreparedCommand(tokio::process::Command);
#[cfg(target_os = "macos")]
impl PreparedOpen for PreparedCommand {
    fn start(mut self: Box<Self>) -> io::Result<Box<dyn PendingOpen>> {
        Ok(Box::new(PendingCommand(containment::inherit::tokio_spawn(
            &mut self.0,
        )?)))
    }
}
#[cfg(target_os = "macos")]
struct PendingCommand(tokio::process::Child);
#[cfg(target_os = "macos")]
#[async_trait]
impl PendingOpen for PendingCommand {
    async fn wait(&mut self) -> io::Result<bool> {
        let status = self.0.wait().await?;
        // A signal is not Launch Services' refusal: the request might already
        // be queued even when somebody else killed the helper.
        if status.code().is_none() {
            return Err(io::Error::other(
                "The Mac opener ended without acknowledgement",
            ));
        }
        Ok(status.success())
    }
    fn kill(&mut self) -> io::Result<()> {
        self.0.start_kill()
    }
}

#[cfg(target_os = "macos")]
mod native;
#[cfg(test)]
mod tests;
