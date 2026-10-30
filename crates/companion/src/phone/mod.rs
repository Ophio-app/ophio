// SPDX-License-Identifier: Apache-2.0
//! `ophio phone`: the Øphio app put on an Android phone over Wireless
//! debugging, one step at a time, through Google's own `adb`.
//!
//! Each step is a command with a clear result, so setup, a person and an
//! agent all take the same steps: find, pair, connect, install, open, link,
//! alerts and off. Nothing here uninstalls an app, and an app is installed
//! only once it is shown to be signed with Øphio's release key.
use crate::{Error, Result, config::Paths, readable::shown};
use clap::{Subcommand, ValueEnum};
use ports::{COMMAND, PRODUCT_NAME, PROGRAM_NAME};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) mod apk;
#[cfg(test)]
mod tests;

/// The app's Android package.
pub const PACKAGE: &str = "app.ophio";
const ACTIVITY: &str = "app.ophio/.MainActivity";
/// The SHA-256 of the certificate Øphio's public Android releases are signed
/// with.
const PUBLIC_CERT_SHA256: &str = "2df838f528a3edf7e6450eeac2b584e86820640876448cb5e2844c1cdd4b9516";
/// The SHA-256 of the development certificate that personal builds were signed
/// with before the public release, which phones installed from them still run.
const PERSONAL_CERT_SHA256: &str =
    "a4b1d0fb6bb576729111ef5ddd6d333dfa76275710606fbfdb574ced0d9741bf";
/// The SHA-256 of the certificate this build's Android app must be signed
/// with: the public one, unless the build was made with
/// `OPHIO_PERSONAL_SIGNING=1`, which trusts the development one instead.
pub const RELEASE_CERT_SHA256: &str = if ::core::cfg!(ophio_personal_signing) {
    PERSONAL_CERT_SHA256
} else {
    PUBLIC_CERT_SHA256
};
// Compiler flags must not override the build script's explicit trust choice.
const _: () = ::core::assert!(
    (::core::cfg!(ophio_personal_signing) as u8 + b'0')
        == ::core::env!("OPHIO_COMPILED_PERSONAL_SIGNING").as_bytes()[0],
    "Set OPHIO_PERSONAL_SIGNING instead of injecting ophio_personal_signing compiler flags"
);
const TAILSCALE: &str = "com.tailscale.ipn";
/// Where Google offers `adb`, in its SDK Platform-Tools.
pub(crate) const PLATFORM_TOOLS_URL: &str =
    "https://developer.android.com/tools/releases/platform-tools";
/// How long a phone has to show up after it scanned the code or was paired.
const CONNECT_LOOKS: u32 = 15;

#[derive(Debug, Subcommand)]
pub enum PhoneCommand {
    /// List the Android phones this computer reaches, and any asking to pair.
    Find,
    /// Pair with an Android phone's Wireless debugging.
    ///
    /// Shows Android's pairing QR code. On the phone, open Settings ›
    /// Developer options › Wireless debugging and tap "Pair device with QR
    /// code", then scan it. With --code, tap "Pair device with pairing code"
    /// instead and type the code it shows here.
    #[command(after_help = "Examples:
  ophio phone pair                         Pair by scanning a QR code
  ophio phone pair --code                  Pair with the six-digit code the phone shows
  ophio phone pair --code 192.168.1.20:37123   The same, at the address the phone shows")]
    Pair {
        /// Pair with the six-digit code the phone shows.
        #[arg(long)]
        code: bool,
        /// The IP address and port the phone shows beside the code; found
        /// on the network when left out.
        #[arg(requires = "code")]
        address: Option<String>,
    },
    /// Connect to a phone paired before.
    Connect {
        /// The IP address and port the phone's Wireless debugging screen
        /// shows.
        address: String,
    },
    /// Install the Øphio app on the phone.
    ///
    /// The app is the one that came with this computer's download, unless
    /// --apk names another. It is installed only if it is signed with
    /// Øphio's release key, and never in place of an Øphio signed otherwise.
    Install {
        /// The app to install.
        #[arg(long, value_name = "FILE")]
        apk: Option<PathBuf>,
    },
    /// Open Øphio, or Tailscale, on the phone.
    ///
    /// Tailscale opens if the phone has it, and its store page if not.
    Open {
        /// Which app to open.
        #[arg(value_enum, default_value_t = Open::App)]
        app: Open,
    },
    /// Open Øphio on the phone with this computer's pairing link filled in.
    ///
    /// Øphio must be running. The phone still shows this computer's
    /// fingerprint and a code to compare, and pairs only once you confirm
    /// here, as with `ophio pair`.
    Link {
        /// Set the phone's answer to alerts while the app is closed.
        #[arg(long, value_enum)]
        alerts: Option<Switch>,
    },
    /// Let Øphio's alerts through on the phone.
    ///
    /// Allows its notifications and lets it run without battery limits, so
    /// alerts arrive while the app is closed.
    Alerts,
    /// Turn Wireless debugging off on the phone.
    ///
    /// The pairing stays on the phone, under Wireless debugging › Paired
    /// devices, until you forget it there.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Open {
    /// Øphio.
    App,
    /// Tailscale, or its store page.
    Tailscale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

/// What `adb` said.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}
impl Output {
    fn text(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// Runs Google's `adb`.
pub(crate) trait Adb {
    /// Runs it with `args`, writing `input` to it; gives up after `limit`.
    async fn adb(&mut self, args: &[&str], input: Option<&str>, limit: Duration) -> Result<Output>;
}

/// The `adb` on this computer. It honours `ADB_SERVER_SOCKET` and the other
/// settings it reads from the environment.
pub(crate) struct SystemAdb(PathBuf);
impl SystemAdb {
    /// `adb` on `PATH`, or where Android's SDK keeps it.
    pub fn find() -> Option<Self> {
        let name = if cfg!(windows) { "adb.exe" } else { "adb" };
        let mut folders: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        for sdk in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
            if let Some(sdk) = std::env::var_os(sdk) {
                folders.push(PathBuf::from(sdk).join("platform-tools"));
            }
        }
        if let Some(dirs) = directories::BaseDirs::new() {
            folders.push(dirs.home_dir().join("Android/Sdk/platform-tools"));
            folders.push(dirs.data_local_dir().join("Android/Sdk/platform-tools"));
        }
        folders
            .into_iter()
            .map(|folder| folder.join(name))
            .find(|path| path.is_file())
            .map(Self)
    }
}
impl Adb for SystemAdb {
    async fn adb(&mut self, args: &[&str], input: Option<&str>, limit: Duration) -> Result<Output> {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;
        let mut child = tokio::process::Command::new(&self.0)
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin.write_all(input.as_bytes()).await?;
        }
        let output = tokio::time::timeout(limit, child.wait_with_output())
            .await
            .map_err(|_| {
                Error::Ended(format!(
                    "adb {} took too long and was stopped.",
                    shown(&args.join(" "))
                ))
            })??;
        Ok(Output {
            ok: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// A phone as `adb devices` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Device {
    pub serial: String,
    /// `device` once it can be used; `unauthorized` until the phone allows
    /// this computer; `offline`.
    pub state: String,
    pub model: Option<String>,
}
impl Device {
    pub fn ready(&self) -> bool {
        self.state == "device"
    }
    /// Its model, or its serial when `adb` does not know the model, as it
    /// may be printed: the phone and the network name it, not this computer.
    pub fn name(&self) -> String {
        shown(self.model.as_deref().unwrap_or(&self.serial))
    }
    /// Its name and serial, as they may be printed.
    pub fn label(&self) -> String {
        format!("{} ({})", self.name(), shown(&self.serial))
    }
    /// Connected over Wireless debugging rather than a cable.
    pub fn wireless(&self) -> bool {
        self.serial.contains("._adb-tls-connect.") || address_host(&self.serial).is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    /// A phone asking to pair, from its pairing screen.
    Pairing,
    /// A paired phone that can be connected to.
    Connect,
}

/// A phone offering Wireless debugging on this network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Service {
    pub name: String,
    pub kind: ServiceKind,
    /// Its IP address and port.
    pub address: String,
}

/// The name and password Android's pairing QR code carries. The phone that
/// scans it offers to pair under the name, and pairs with whoever knows
/// the password.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct QrPairing {
    pub name: String,
    password: String,
}
impl std::fmt::Debug for QrPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QrPairing")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
impl QrPairing {
    pub fn new() -> Result<Self> {
        Ok(Self {
            name: format!("{PROGRAM_NAME}-{}", random_text(10)?),
            password: random_text(12)?,
        })
    }
    #[cfg(test)]
    pub fn fixed(name: &str, password: &str) -> Self {
        Self {
            name: name.into(),
            password: password.into(),
        }
    }
    /// What the QR code holds, in the form Android's camera reads.
    pub fn code(&self) -> String {
        format!("WIFI:T:ADB;S:{};P:{};;", self.name, self.password)
    }
}

/// Letters and digits that need no escaping in the QR code's text.
fn random_text(length: usize) -> Result<String> {
    use ring::rand::SecureRandom;
    const LETTERS: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut bytes = vec![0; length];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::Unsupported("this computer has no source of random numbers"))?;
    Ok(bytes
        .iter()
        .map(|b| char::from(LETTERS[usize::from(*b) % LETTERS.len()]))
        .collect())
}

/// How installing ended, when the phone answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Installed {
    /// Installed, or updated in place.
    Done,
    /// An Øphio signed with another key is on the phone. Android keeps an
    /// app's key, so only removing that one, with its data, makes room.
    OtherKey,
    /// The phone has a newer Øphio.
    Newer,
    /// The phone or the person on it stopped the install, with Android's
    /// reason.
    Refused(String),
}

/// What opened on the phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opened {
    App,
    Tailscale,
    /// Tailscale's page in the phone's app store.
    Store,
}

/// What `ophio phone alerts` let through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AlertsAllowed {
    pub notifications: bool,
    pub battery: bool,
}

/// How `ophio phone off` left the phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Off {
    /// The phone took the setting.
    Done,
    /// It is on a cable, and stays reachable while plugged in.
    Cable,
    /// The phone refused, in adb's words.
    Failed(String),
    /// The connection ended before the phone answered, as it does when the
    /// setting takes, and also when something else ended it.
    Unsure,
}

/// The phone steps, over an [`Adb`].
pub(crate) struct Phones<A> {
    pub adb: A,
    /// The certificate digest an app must be signed with.
    pub release: String,
}

/// Android 10, the oldest the app runs on.
const MIN_SDK: u32 = 29;
/// Far larger than the app, which is tens of megabytes.
const MOST_APP: u64 = 1 << 30;
const QUICK: Duration = Duration::from_secs(30);
/// Long enough for a person to answer the phone's own question, as when
/// Play Protect asks to scan the app.
const INSTALL: Duration = Duration::from_secs(600);

impl<A: Adb> Phones<A> {
    pub fn new(adb: A) -> Self {
        Self {
            adb,
            release: RELEASE_CERT_SHA256.into(),
        }
    }

    async fn run(&mut self, args: &[&str]) -> Result<Output> {
        self.adb.adb(args, None, QUICK).await
    }

    async fn shell(&mut self, serial: &str, command: &str) -> Result<Output> {
        self.run(&["-s", serial, "shell", command]).await
    }

    pub async fn devices(&mut self) -> Result<Vec<Device>> {
        let output = self.run(&["devices", "-l"]).await?;
        if !output.ok {
            return Err(failed("list the phones", &output));
        }
        Ok(parse_devices(&output.stdout))
    }

    pub async fn services(&mut self) -> Result<Vec<Service>> {
        let output = self.run(&["mdns", "services"]).await?;
        if !output.ok {
            return Err(failed("look for phones on this network", &output));
        }
        Ok(parse_services(&output.stdout))
    }

    /// The phone to use: the one named, or the only one connected.
    pub async fn choose(&mut self, wanted: Option<&str>) -> Result<Device> {
        let devices = self.devices().await?;
        let device = match wanted {
            Some(serial) => devices.into_iter().find(|d| d.serial == serial).ok_or_else(|| {
                Error::Ended(format!(
                    "No phone {} is connected. `{COMMAND} phone find` lists the phones that are.",
                    shown(serial)
                ))
            })?,
            None => {
                let mut ready: Vec<_> = devices.iter().filter(|d| d.ready()).cloned().collect();
                match (ready.len(), devices.len()) {
                    (1, _) => ready.remove(0),
                    (0, 0) => {
                        return Err(Error::Ended(format!(
                            "No phone is connected. Pair one with `{COMMAND} phone pair`."
                        )));
                    }
                    (0, _) => devices.into_iter().next().expect("one at least"),
                    _ => {
                        let names: Vec<_> =
                            ready.iter().map(Device::label).collect();
                        return Err(Error::Ended(format!(
                            "Several phones are connected: {}. Choose one with --device.",
                            names.join(", ")
                        )));
                    }
                }
            }
        };
        match device.state.as_str() {
            "device" => Ok(device),
            "unauthorized" => Err(Error::Ended(format!(
                "{} hasn't allowed this computer yet. Unlock it and allow debugging when it asks.",
                device.name()
            ))),
            state => Err(Error::Ended(format!(
                "{} is {}. Check that Wireless debugging is still on, then try again.",
                device.name(),
                shown(state)
            ))),
        }
    }

    /// One look for the phone that scanned `pairing`'s code; once it is
    /// there, pairs and connects. `None` while it is not there yet.
    pub async fn pair_qr(&mut self, pairing: &QrPairing) -> Result<Option<Device>> {
        let found = self
            .services()
            .await?
            .into_iter()
            .find(|s| s.kind == ServiceKind::Pairing && s.name == pairing.name);
        match found {
            Some(service) => self
                .pair_at(&service.address, &pairing.password)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    /// Pairs with the phone showing `code`, at `address` or the only phone
    /// asking to pair, then connects.
    pub async fn pair_code(&mut self, address: Option<&str>, code: &str) -> Result<Device> {
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Ended(
                "The pairing code is the six digits the phone shows.".into(),
            ));
        }
        let address = match address {
            Some(address) => address.to_owned(),
            None => self.asking().await?,
        };
        self.pair_at(&address, &code).await
    }

    /// The address of the only phone asking to pair with a code.
    pub async fn asking(&mut self) -> Result<String> {
        let asking: Vec<_> = self
            .services()
            .await?
            .into_iter()
            .filter(|s| s.kind == ServiceKind::Pairing)
            .collect();
        match asking.as_slice() {
            [one] => Ok(one.address.clone()),
            [] => Err(Error::Ended(
                "No phone is asking to pair on this network. On the phone, tap \"Pair device with pairing code\", and keep it on the same Wi-Fi as this computer; or give the IP address and port it shows.".into(),
            )),
            _ => Err(Error::Ended(
                "Several phones are asking to pair. Give the IP address and port your phone shows.".into(),
            )),
        }
    }

    async fn pair_at(&mut self, address: &str, secret: &str) -> Result<Device> {
        check_address(address)?;
        // The secret goes in on standard input, never on a command line.
        let output = self
            .adb
            .adb(&["pair", address], Some(&format!("{secret}\n")), QUICK)
            .await?;
        if !output.stdout.contains("Successfully paired") {
            let text = output.text();
            return Err(Error::Ended(if text.contains("Wrong password") {
                "The phone refused the code. Check it, or start again from the phone's pairing screen.".into()
            } else {
                format!(
                    "Couldn't pair with the phone at {}. Keep its pairing screen open and the phone on the same Wi-Fi as this computer. adb said: {}",
                    shown(address),
                    said(&output)
                )
            }));
        }
        let host = address_host(address).unwrap_or_default();
        for look in 0..CONNECT_LOOKS {
            if look > 0 {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let found =
                self.services().await?.into_iter().find(|s| {
                    s.kind == ServiceKind::Connect && address_host(&s.address) == Some(host)
                });
            if let Some(service) = found {
                return self.connect(&service.address).await;
            }
        }
        Err(Error::Ended(format!(
            "Paired, but the phone isn't offering a connection. Keep Wireless debugging open on it, then run `{COMMAND} phone connect` with the IP address and port it shows."
        )))
    }

    pub async fn connect(&mut self, address: &str) -> Result<Device> {
        check_address(address)?;
        let output = self.run(&["connect", address]).await?;
        let text = output.text();
        if !text.contains(&format!("connected to {address}")) {
            return Err(Error::Ended(format!(
                "Couldn't connect to the phone at {}. adb said: {}",
                shown(address),
                said(&output)
            )));
        }
        let devices = self.devices().await?;
        Ok(devices
            .into_iter()
            .find(|d| d.serial == address)
            .unwrap_or(Device {
                serial: address.into(),
                state: "device".into(),
                model: None,
            }))
    }

    /// Android's user the phone shows now: 0 unless it has several people.
    async fn user(&mut self, serial: &str) -> Result<u32> {
        let output = self.shell(serial, "am get-current-user").await?;
        Ok(output.stdout.trim().parse().unwrap_or(0))
    }

    /// Android's version on the phone, as its API level.
    async fn sdk(&mut self, device: &Device) -> Result<u32> {
        let output = self
            .shell(&device.serial, "getprop ro.build.version.sdk")
            .await?;
        output.stdout.trim().parse().map_err(|_| {
            Error::Ended(format!(
                "Couldn't tell which Android {} runs. adb said: {}",
                device.name(),
                said(&output)
            ))
        })
    }

    /// Checks `apk` is signed with the release key, then installs it for
    /// the phone's current user. Both are done to a private copy, so the
    /// file named can't be changed between them.
    pub async fn install(&mut self, device: &Device, apk: &Path) -> Result<Installed> {
        let staged = stage(apk)?;
        check_signer(&staged.path, apk, &self.release)?;
        // An older Android checks only the JAR signature, which is not read.
        if self.sdk(device).await? < MIN_SDK {
            return Err(Error::Ended(format!(
                "{} runs an Android older than 10, and {PRODUCT_NAME} needs 10 or later.",
                device.name()
            )));
        }
        let user = self.user(&device.serial).await?.to_string();
        let path = staged
            .path
            .to_str()
            .ok_or_else(|| Error::Ended("The app's file name needs to be UTF-8.".into()))?;
        let output = self
            .adb
            .adb(
                &["-s", &device.serial, "install", "-r", "--user", &user, path],
                None,
                INSTALL,
            )
            .await?;
        let text = output.text();
        if output.ok && text.contains("Success") {
            return Ok(Installed::Done);
        }
        let reason = text
            .split(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .find(|word| word.starts_with("INSTALL_"))
            .map(str::to_owned);
        Ok(match reason.as_deref() {
            Some("INSTALL_FAILED_UPDATE_INCOMPATIBLE") => Installed::OtherKey,
            Some("INSTALL_FAILED_VERSION_DOWNGRADE") => Installed::Newer,
            Some(
                reason @ ("INSTALL_FAILED_ABORTED"
                | "INSTALL_FAILED_USER_RESTRICTED"
                | "INSTALL_FAILED_VERIFICATION_FAILURE"),
            ) => Installed::Refused(reason.into()),
            _ => {
                return Err(Error::Ended(format!(
                    "{} didn't install {PRODUCT_NAME}. adb said: {}",
                    device.name(),
                    said(&output)
                )));
            }
        })
    }

    pub async fn open(&mut self, device: &Device, app: Open) -> Result<Opened> {
        match app {
            Open::App => {
                let output = self
                    .shell(&device.serial, &format!("am start -W -n {ACTIVITY}"))
                    .await?;
                started(&output, device)?;
                Ok(Opened::App)
            }
            Open::Tailscale => {
                let listed = self
                    .shell(&device.serial, &format!("pm list packages {TAILSCALE}"))
                    .await?;
                if listed
                    .stdout
                    .lines()
                    .any(|l| l.trim() == format!("package:{TAILSCALE}"))
                {
                    let output = self
                        .shell(
                            &device.serial,
                            &format!("monkey -p {TAILSCALE} -c android.intent.category.LAUNCHER 1"),
                        )
                        .await?;
                    if output.ok && !output.text().contains("No activities found") {
                        return Ok(Opened::Tailscale);
                    }
                }
                for page in [
                    format!("market://details?id={TAILSCALE}"),
                    format!("https://play.google.com/store/apps/details?id={TAILSCALE}"),
                ] {
                    let output = self
                        .shell(
                            &device.serial,
                            &format!("am start -a android.intent.action.VIEW -d '{page}'"),
                        )
                        .await?;
                    if output.ok && !output.text().contains("Error") {
                        return Ok(Opened::Store);
                    }
                }
                Err(Error::Ended(format!(
                    "{} couldn't open Tailscale or its store page.",
                    device.name()
                )))
            }
        }
    }

    /// Opens Øphio on the phone with `link`, a pairing link, filled in.
    pub async fn link(&mut self, device: &Device, link: &str) -> Result<()> {
        let safe = |c: char| c.is_ascii_alphanumeric() || ":/?=&-_.".contains(c);
        if !link.starts_with(&format!("{}://pair?", crate::config::PAIR_SCHEME))
            || !link.chars().all(safe)
        {
            return Err(crate::error::invalid("not a pairing link"));
        }
        // On standard input, so the link's secret is on no command line here.
        let command =
            format!("am start -W -a android.intent.action.VIEW -d '{link}' -n {ACTIVITY}\nexit\n");
        let output = self
            .adb
            .adb(&["-s", &device.serial, "shell"], Some(&command), QUICK)
            .await?;
        started(&output, device)
    }

    pub async fn alerts(&mut self, device: &Device) -> Result<AlertsAllowed> {
        let user = self.user(&device.serial).await?;
        let granted = self
            .shell(
                &device.serial,
                &format!("pm grant --user {user} {PACKAGE} android.permission.POST_NOTIFICATIONS"),
            )
            .await?;
        if granted.text().contains("Unknown package") {
            return Err(not_installed(device));
        }
        // Before Android 13, apps may notify without asking.
        let notifications = granted.ok && !granted.text().contains("Exception")
            || granted.text().contains("Unknown permission");
        let exempt = self
            .shell(
                &device.serial,
                &format!("dumpsys deviceidle whitelist +{PACKAGE}"),
            )
            .await?;
        Ok(AlertsAllowed {
            notifications,
            battery: exempt.stdout.contains(&format!("Added: {PACKAGE}")),
        })
    }

    pub async fn off(&mut self, device: &Device) -> Result<Off> {
        if !device.wireless() {
            return Ok(Off::Cable);
        }
        // Turning it off ends this connection, so the answer may not come.
        let put = self
            .shell(&device.serial, "settings put global adb_wifi_enabled 0")
            .await;
        let mut off = match put {
            Ok(output) if output.ok && !refused(&output) => Off::Done,
            Ok(output) if refused(&output) => Off::Failed(said(&output)),
            _ => Off::Unsure,
        };
        // While the connection lasts, the phone says what it has now.
        if let Ok(now) = self
            .shell(&device.serial, "settings get global adb_wifi_enabled")
            .await
            && now.ok
        {
            match now.stdout.trim() {
                "0" if off == Off::Unsure => off = Off::Done,
                "1" if !matches!(off, Off::Failed(_)) => {
                    off = Off::Failed("Wireless debugging is still on.".into());
                }
                _ => {}
            }
        }
        let _ = self.run(&["disconnect", &device.serial]).await;
        Ok(off)
    }
}

fn parse_devices(listing: &str) -> Vec<Device> {
    listing
        .lines()
        .skip_while(|line| !line.starts_with("List of devices"))
        .skip(1)
        // adb's own news, as when its server starts, begins with a star.
        .filter(|line| !line.starts_with('*'))
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let serial = words.next()?.to_owned();
            let state = words.next()?.to_owned();
            let model = words
                .find_map(|word| word.strip_prefix("model:"))
                .map(|model| model.replace('_', " "));
            Some(Device {
                serial,
                state,
                model,
            })
        })
        .collect()
}

fn parse_services(listing: &str) -> Vec<Service> {
    listing
        .lines()
        .filter_map(|line| {
            let words: Vec<_> = line.split_whitespace().collect();
            let [name, kind, address] = words.as_slice() else {
                return None;
            };
            let kind = match kind.trim_end_matches('.') {
                "_adb-tls-pairing._tcp" => ServiceKind::Pairing,
                "_adb-tls-connect._tcp" => ServiceKind::Connect,
                _ => return None,
            };
            Some(Service {
                name: (*name).into(),
                kind,
                address: (*address).into(),
            })
        })
        .collect()
}

/// The host of an `ip:port` address, as written.
fn address_host(address: &str) -> Option<&str> {
    let (host, port) = address.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let ip = host.split('%').next()?;
    ip.parse::<std::net::IpAddr>().ok().map(|_| host)
}

fn check_address(address: &str) -> Result<()> {
    match address_host(address) {
        Some(_) => Ok(()),
        None => Err(Error::Ended(format!(
            "{} isn't an IP address and port, such as 192.168.1.20:37123.",
            shown(address)
        ))),
    }
}

/// A private copy of an app, gone when dropped.
struct Staged {
    path: PathBuf,
    _folder: tempfile::TempDir,
}

/// Copies `apk` into a folder only this user can reach. A link is refused:
/// the app installed is the file named.
fn stage(apk: &Path) -> Result<Staged> {
    let shown = shown(&apk.display().to_string());
    let metadata = match std::fs::symlink_metadata(apk) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::Ended(format!("There is no app at {shown}.")));
        }
        result => result?,
    };
    if !metadata.is_file() {
        return Err(Error::Ended(format!(
            "{shown} isn't a file. Name the app itself, not a link to it."
        )));
    }
    let mut file = std::fs::File::open(apk)?;
    if !file.metadata()?.is_file() {
        return Err(Error::Ended(format!("{shown} isn't a file.")));
    }
    let folder = tempfile::Builder::new().prefix("ophio-app-").tempdir()?;
    let path = folder.path().join("ophio.apk");
    let mut copy = std::fs::File::create_new(&path)?;
    let copied = std::io::copy(&mut std::io::Read::take(&mut file, MOST_APP + 1), &mut copy)?;
    if copied > MOST_APP {
        return Err(Error::Ended(format!(
            "{shown} is far larger than {PRODUCT_NAME}'s app."
        )));
    }
    copy.sync_all()?;
    Ok(Staged {
        path,
        _folder: folder,
    })
}

/// Checks the app at `apk`, which is named `named` to the person.
fn check_signer(apk: &Path, named: &Path, release: &str) -> Result<()> {
    let shown = shown(&named.display().to_string());
    let problem = match apk::signers(apk) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            format!("There is no app at {shown}.")
        }
        Err(error) => return Err(error.into()),
        Ok(Ok(signers)) if signers.iter().all(|s| apk::hex(s) == release) => return Ok(()),
        Ok(Ok(signers)) => format!(
            "{shown} isn't signed with {PRODUCT_NAME}'s release key; its certificate is {}. Install only the app that came with {PRODUCT_NAME}.",
            apk::hex(&signers[0])
        ),
        Ok(Err(apk::Unreadable::NotApp)) => format!("{shown} isn't an Android app."),
        Ok(Err(apk::Unreadable::Unsigned)) => format!("{shown} isn't signed for release."),
        Ok(Err(apk::Unreadable::Damaged)) => format!("{shown} has a damaged signature."),
    };
    Err(Error::Ended(problem))
}

/// Whether `am start` started what it was asked to.
fn started(output: &Output, device: &Device) -> Result<()> {
    let text = output.text();
    if text.contains("does not exist") || text.contains("unable to resolve") {
        return Err(not_installed(device));
    }
    if !output.ok || text.contains("Error") {
        return Err(Error::Ended(format!(
            "{} couldn't open {PRODUCT_NAME}. adb said: {}",
            device.name(),
            said(output)
        )));
    }
    Ok(())
}

fn not_installed(device: &Device) -> Error {
    Error::Ended(format!(
        "{PRODUCT_NAME} isn't installed on {}. Install it with `{COMMAND} phone install`.",
        device.name()
    ))
}

fn failed(doing: &str, output: &Output) -> Error {
    Error::Ended(format!("adb couldn't {doing}: {}", said(output)))
}

/// Whether Android itself refused a command, rather than the connection
/// ending.
fn refused(output: &Output) -> bool {
    let text = output.text();
    ["Exception", "Permission denial", "not allowed"]
        .iter()
        .any(|word| text.contains(word))
}

/// adb's own words, on one line, as they may be printed.
fn said(output: &Output) -> String {
    let text = output.text();
    let words: Vec<_> = text.split_whitespace().collect();
    let line = words.join(" ");
    if line.is_empty() {
        "nothing".into()
    } else {
        shown(&line)
    }
}

/// The app that came with this computer's download: beside `ophio`, or in
/// the `app` folder beside its `bin`.
pub(crate) fn bundled_app() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join("ophio.apk"), dir.join("../app/ophio.apk")]
        .into_iter()
        .find(|path| path.is_file())
}

fn no_adb() -> Error {
    Error::Ended(format!(
        "This needs Google's adb, from Android's SDK Platform-Tools. Download them from {PLATFORM_TOOLS_URL} (you accept Google's terms there), or install your system's android-tools package, then try again."
    ))
}

pub(crate) fn system() -> Result<Phones<SystemAdb>> {
    SystemAdb::find().map(Phones::new).ok_or_else(no_adb)
}

/// A phone to send the pairing link to, as `ophio phone link` does.
pub(crate) struct LinkTo {
    phones: Phones<SystemAdb>,
    device: Device,
    alerts: Option<Switch>,
}
impl LinkTo {
    /// Sends `link`; what to call the phone.
    pub async fn send(mut self, link: &str) -> Result<String> {
        let link = match self.alerts {
            Some(Switch::On) => format!("{link}&alerts=on"),
            Some(Switch::Off) => format!("{link}&alerts=off"),
            None => link.to_owned(),
        };
        self.phones.link(&self.device, &link).await?;
        Ok(self.device.name())
    }
}

pub async fn execute(
    paths: &Paths,
    action: PhoneCommand,
    device: Option<String>,
    json: bool,
) -> Result<()> {
    let mut phones = system()?;
    let wanted = device.as_deref();
    match action {
        PhoneCommand::Find => {
            let devices = phones.devices().await?;
            let services = phones.services().await.unwrap_or_default();
            if json {
                let found = serde_json::json!({ "devices": devices, "services": services });
                println!("{found:#}");
                return Ok(());
            }
            if devices.is_empty() && services.is_empty() {
                println!(
                    "No phone found. Turn on Wireless debugging on the phone, then run `{COMMAND} phone pair`."
                );
            }
            for device in &devices {
                let state = match device.state.as_str() {
                    "device" => "connected",
                    "unauthorized" => "waiting for you to allow this computer on the phone",
                    other => &shown(other),
                };
                println!("{}: {state}", device.label());
            }
            for service in &services {
                let what = match service.kind {
                    ServiceKind::Pairing => "asking to pair",
                    ServiceKind::Connect => "paired; can be connected",
                };
                println!(
                    "{} at {}: {what}",
                    shown(&service.name),
                    shown(&service.address)
                );
            }
            Ok(())
        }
        PhoneCommand::Pair {
            code: true,
            address,
        } => {
            let address = match address {
                Some(address) => address,
                None => phones.asking().await?,
            };
            eprint!("The six-digit code the phone shows: ");
            let mut code = String::new();
            std::io::stdin().read_line(&mut code)?;
            let device = phones.pair_code(Some(&address), &code).await?;
            println!("Paired and connected to {}.", device.label());
            Ok(())
        }
        PhoneCommand::Pair { code: false, .. } => {
            let pairing = QrPairing::new()?;
            println!(
                "On the phone, open Settings › Developer options › Wireless debugging,\ntap \"Pair device with QR code\" and scan this code.{}",
                crate::cli::pairing_qr(&pairing.code())?
            );
            for _ in 0..180 {
                if let Some(device) = phones.pair_qr(&pairing).await? {
                    println!("Paired and connected to {}.", device.label());
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(Error::Ended(format!(
                "No phone scanned the code. Check that the phone is on the same Wi-Fi as this computer, then try again, or pair with `{COMMAND} phone pair --code`."
            )))
        }
        PhoneCommand::Connect { address } => {
            let device = phones.connect(&address).await?;
            println!("Connected to {}.", device.label());
            Ok(())
        }
        PhoneCommand::Install { apk } => {
            let apk = apk.or_else(bundled_app).ok_or_else(|| {
                Error::Ended(format!(
                    "This download has no {PRODUCT_NAME} app beside it. Name one with --apk."
                ))
            })?;
            let device = phones.choose(wanted).await?;
            println!(
                "Installing {PRODUCT_NAME} on {}. If the phone asks to scan the app, let it.",
                device.name()
            );
            match phones.install(&device, &apk).await? {
                Installed::Done => {
                    println!("Installed {PRODUCT_NAME} on {}.", device.name());
                    Ok(())
                }
                Installed::Newer => {
                    println!(
                        "{} already has a newer {PRODUCT_NAME}; it was left as it is.",
                        device.name()
                    );
                    Ok(())
                }
                Installed::OtherKey => Err(Error::Ended(other_key(&device))),
                Installed::Refused(reason) => Err(Error::Ended(format!(
                    "{} refused the install ({reason}). If the phone asked, allow it; some phones also need \"Install via USB\" turned on in Developer options.",
                    device.name()
                ))),
            }
        }
        PhoneCommand::Open { app } => {
            let device = phones.choose(wanted).await?;
            match phones.open(&device, app).await? {
                Opened::App => println!("Opened {PRODUCT_NAME} on {}.", device.name()),
                Opened::Tailscale => println!("Opened Tailscale on {}.", device.name()),
                Opened::Store => println!("Opened Tailscale's store page on {}.", device.name()),
            }
            Ok(())
        }
        PhoneCommand::Link { alerts } => {
            let device = phones.choose(wanted).await?;
            let to = LinkTo {
                phones,
                device,
                alerts,
            };
            crate::cli::pair(paths, Some(to)).await
        }
        PhoneCommand::Alerts => {
            let device = phones.choose(wanted).await?;
            let allowed = phones.alerts(&device).await?;
            println!("{}", alerts_said(&device, allowed));
            Ok(())
        }
        PhoneCommand::Off => {
            let device = phones.choose(wanted).await?;
            match phones.off(&device).await? {
                Off::Done => println!(
                    "Turned Wireless debugging off on {}. Its pairing with this computer stays there, under Wireless debugging › Paired devices, until you forget it.",
                    device.name()
                ),
                Off::Cable => println!(
                    "{} is connected by cable; unplug it to end this computer's access.",
                    device.name()
                ),
                Off::Failed(why) => {
                    return Err(Error::Ended(format!(
                        "{} didn't turn Wireless debugging off ({why}). Turn it off on the phone, under Settings › Developer options › Wireless debugging.",
                        device.name()
                    )));
                }
                Off::Unsure => println!(
                    "{} stopped answering before it said Wireless debugging was off. Check on the phone, under Settings › Developer options › Wireless debugging, and turn it off there if it's still on.",
                    device.name()
                ),
            }
            Ok(())
        }
    }
}

pub(crate) fn other_key(device: &Device) -> String {
    format!(
        "{} already has an {PRODUCT_NAME} signed with a different key, which this one can't replace. Removing it deletes its pairings and settings; if that's what you want, uninstall it on the phone (Settings › Apps › {PRODUCT_NAME} › Uninstall), then install again.",
        device.name()
    )
}

pub(crate) fn alerts_said(device: &Device, allowed: AlertsAllowed) -> String {
    match (allowed.notifications, allowed.battery) {
        (true, true) => format!(
            "{PRODUCT_NAME} on {} may show notifications and runs without battery limits.",
            device.name()
        ),
        (true, false) => format!(
            "{PRODUCT_NAME} on {} may show notifications. Android didn't lift its battery limits; the app asks for that itself.",
            device.name()
        ),
        (false, true) => format!(
            "{PRODUCT_NAME} on {} runs without battery limits. Android didn't allow its notifications; the app asks for that itself.",
            device.name()
        ),
        (false, false) => format!(
            "Android didn't change {PRODUCT_NAME}'s notifications or battery limits on {}; the app asks for them itself.",
            device.name()
        ),
    }
}
