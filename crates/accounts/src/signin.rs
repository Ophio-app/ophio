// SPDX-License-Identifier: Apache-2.0
//! Drives a service's own sign-in command on the computer. The pages it
//! wants opened go to the computer's browser through Ophio, so the phone can
//! follow along; the one-time code it prints is the only output kept.
//!
//! A sign-in finished on the phone ([`Phone`]) opens nothing here: the page's
//! whole link the tool prints goes to the phone, in the stage only, and the
//! code the owner pastes back from that page is written to the tool once,
//! never logged or kept.
use crate::desktop::Opener;
use crate::files::private_dir;
use crate::tool::{Contained, OUTPUT_LIMIT, Tools, plain};
use protocol::accounts::{Secret, SignInStage};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};
use zeroize::Zeroizing;

/// How long the owner has to finish on the provider's page.
pub const LIMIT: Duration = Duration::from_secs(10 * 60);
/// A tool that prints an address instead of opening it is given this long
/// to open one before Ophio opens the printed one.
const PRINTED_GRACE: Duration = Duration::from_secs(2);
const KEPT: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    Exited {
        success: bool,
        /// Why a sign-in on the phone stopped, where the tool said.
        refused: Option<Refused>,
    },
    Cancelled,
    TimedOut,
    Failed(String),
}

/// What the tool said when a sign-in on the phone didn't finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Codex couldn't reach OpenAI, or OpenAI failed (no answer, 5xx).
    Unreachable,
    /// OpenAI wouldn't give Codex a code to start with (4xx on asking).
    NoCode,
    /// OpenAI refused the sign-in once the page was used (4xx after it):
    /// most likely the ChatGPT account hasn't turned device code sign-in on.
    SaidNo,
    /// The one-time code expired before the page was finished.
    CodeExpired,
    /// The tool refused the code pasted back, and gave up.
    LoginFailed,
}

/// A sign-in finished in the phone's own browser.
pub struct Phone {
    /// The page shows a code the owner pastes back (Claude Code).
    pub paste_code: bool,
    /// The codes pasted, each written to the tool once, with Enter.
    pub codes: mpsc::Receiver<Secret>,
}

pub struct Run<'a> {
    pub argv: &'a [String],
    pub extra: Vec<(String, OsString)>,
    pub pty: bool,
    pub tools: &'a Tools,
    pub opener: &'a dyn Opener,
    /// Private and emptied afterwards: the browser stand-in and what it saw.
    pub scratch: PathBuf,
    pub limit: Duration,
    /// Finished on the phone rather than in this computer's browser.
    pub phone: Option<Phone>,
}

/// Runs the sign-in command until it exits, is cancelled or runs out of time.
pub async fn run(
    mut run: Run<'_>,
    stage: &(dyn Fn(SignInStage) + Send + Sync),
    mut cancel: watch::Receiver<bool>,
) -> Ended {
    let _ = std::fs::remove_dir_all(&run.scratch);
    if let Err(e) = private_dir(&run.scratch) {
        return Ended::Failed(format!("Ophio could not prepare the sign-in: {e}"));
    }
    let phone = run.phone.take();
    let ended = drive(&run, phone, stage, &mut cancel).await;
    let _ = std::fs::remove_dir_all(&run.scratch);
    ended
}

async fn drive(
    run: &Run<'_>,
    phone: Option<Phone>,
    stage: &(dyn Fn(SignInStage) + Send + Sync),
    cancel: &mut watch::Receiver<bool>,
) -> Ended {
    let urls = run.scratch.join("urls");
    let mut extra = run.extra.clone();
    if let Err(e) = browser_stand_in(&run.scratch, &urls, run.tools, &mut extra) {
        return Ended::Failed(format!("Ophio could not prepare the sign-in: {e}"));
    }
    // A few reads deep: a tool that prints faster than it is read waits.
    let (out_tx, mut out) = mpsc::channel::<Vec<u8>>(16);
    let mut proc = match Proc::start(run, &extra, out_tx).await {
        Ok(p) => p,
        Err(e) => return Ended::Failed(e),
    };
    let ended = match phone {
        Some(phone) => follow_phone(run, phone, stage, cancel, &mut proc, &mut out).await,
        None => follow(run, stage, cancel, &mut proc, &mut out).await,
    };
    // Readers waiting on a full queue stop, and so stop holding the output.
    drop(out);
    // Nothing the tool started may outlive the sign-in, to write a login
    // after it was cancelled or signed out: until all of it has ended, the
    // sign-in is still stopping.
    proc.end().await;
    ended
}

async fn follow(
    run: &Run<'_>,
    stage: &(dyn Fn(SignInStage) + Send + Sync),
    cancel: &mut watch::Receiver<bool>,
    proc: &mut Proc,
    out: &mut mpsc::Receiver<Vec<u8>>,
) -> Ended {
    let name = &run.argv[0];
    let urls = run.scratch.join("urls");
    let mut seen = Seen {
        tool_opens: !cfg!(unix),
        ..Seen::default()
    };
    let mut printed = 0usize;
    let deadline = tokio::time::sleep(run.limit);
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut read_urls = 0usize;
    loop {
        tokio::select! {
            () = &mut deadline => return Ended::TimedOut,
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return Ended::Cancelled;
                }
            }
            Some(chunk) = out.recv() => {
                printed += chunk.len();
                if printed > OUTPUT_LIMIT {
                    return Ended::Failed(format!("{name} printed more than Ophio reads"));
                }
                let text = plain(&String::from_utf8_lossy(&chunk));
                for _ in 0..seen.feed(&text) {
                    proc.enter();
                }
                seen.show(stage);
            }
            _ = tick.tick() => {
                let text = match pages(&urls) {
                    Ok(text) => text,
                    Err(TooMany) => {
                        return Ended::Failed(format!(
                            "{name} asked for more pages than Ophio opens"
                        ));
                    }
                };
                if let Some(text) = text {
                    let lines: Vec<&str> = text.lines().collect();
                    for url in lines.iter().skip(read_urls) {
                        seen.open(url.trim(), run.opener);
                    }
                    read_urls = lines.len();
                }
                seen.open_printed(run.opener);
                seen.show(stage);
                if let Some(success) = proc.exited() {
                    return Ended::Exited { success, refused: None };
                }
            }
        }
    }
}

/// [`follow`] for a sign-in finished on the phone: nothing opens on this
/// computer, and the link and code go to the phone in the stage.
async fn follow_phone(
    run: &Run<'_>,
    mut phone: Phone,
    stage: &(dyn Fn(SignInStage) + Send + Sync),
    cancel: &mut watch::Receiver<bool>,
    proc: &mut Proc,
    out: &mut mpsc::Receiver<Vec<u8>>,
) -> Ended {
    let name = &run.argv[0];
    let urls = run.scratch.join("urls");
    let mut seen = OnPhone::new(phone.paste_code);
    let mut printed = 0usize;
    let deadline = tokio::time::sleep(run.limit);
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            () = &mut deadline => return Ended::TimedOut,
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return Ended::Cancelled;
                }
            }
            Some(chunk) = out.recv() => {
                printed += chunk.len();
                if printed > OUTPUT_LIMIT {
                    return Ended::Failed(format!("{name} printed more than Ophio reads"));
                }
                seen.feed(&chunk);
                seen.show(stage);
            }
            Some(code) = phone.codes.recv() => {
                // Only a code the tool is waiting for is written; the
                // computer refuses others before they come this far.
                if seen.awaiting_code() {
                    proc.type_code(&code);
                    seen.sent(&code);
                    seen.show(stage);
                }
            }
            _ = tick.tick() => {
                // The page the tool asked to open here is the one that
                // returns to this computer, of no use on the phone; it is
                // written down by the stand-in and left closed.
                if pages(&urls).is_err() {
                    return Ended::Failed(format!(
                        "{name} asked for more pages than Ophio opens"
                    ));
                }
                if let Some(success) = proc.exited() {
                    // What it said last, its error among it, may still be
                    // on its way: read on until the output ends, briefly.
                    let _ = tokio::time::timeout(LAST_WORDS, async {
                        while let Some(chunk) = out.recv().await {
                            seen.feed(&chunk);
                        }
                    })
                    .await;
                    return Ended::Exited { success, refused: seen.refused() };
                }
            }
        }
    }
}

/// How long the output of a tool that has exited is still read.
const LAST_WORDS: Duration = Duration::from_millis(500);

/// What a sign-in on the phone has seen the tool say.
struct OnPhone {
    paste_code: bool,
    /// The tool's output as it came, escapes and all, up to [`KEPT`]. Its
    /// room is kept from the start, so it never moves, and wiped when done.
    raw: Zeroizing<String>,
    /// The code last written, until the terminal's echo of it is found and
    /// wiped from `raw`.
    typed: Option<Zeroizing<String>>,
    link: Option<Secret>,
    code: Option<String>,
    /// The tool asked for the page's code.
    prompted: bool,
    /// Codes written, and how many the tool refused, each after the one
    /// before it was written.
    sent: usize,
    refusals: usize,
    /// Where in `raw` the last code was written.
    sent_at: usize,
    shown: Option<SignInStage>,
}

impl OnPhone {
    fn new(paste_code: bool) -> Self {
        Self {
            paste_code,
            raw: Zeroizing::new(String::with_capacity(KEPT + 2 * 4096)),
            typed: None,
            link: None,
            code: None,
            prompted: false,
            sent: 0,
            refusals: 0,
            sent_at: 0,
            shown: None,
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        self.raw.push_str(&String::from_utf8_lossy(chunk));
        if self.raw.len() > KEPT {
            let mut cut = self.raw.len() - KEPT / 2;
            while !self.raw.is_char_boundary(cut) {
                cut += 1;
            }
            self.raw.drain(..cut);
            self.sent_at = self.sent_at.saturating_sub(cut);
        }
        // The terminal shows what was typed: the code is wiped from what is
        // kept as soon as it shows.
        if let Some(typed) = &self.typed
            && let Some(at) = self.raw[self.sent_at..].find(typed.as_str())
        {
            let at = self.sent_at + at;
            let len = typed.len();
            self.raw.replace_range(at..at + len, &"*".repeat(len));
            self.typed = None;
        }
        let text = plain(&self.raw);
        if self.link.is_none() {
            self.link = phone_link(&self.raw).map(Secret::new);
        }
        if self.code.is_none() {
            self.code = one_time_code(&text);
        }
        if self.paste_code && text.to_ascii_lowercase().contains("paste code") {
            self.prompted = true;
        }
        // Claude Code says so and keeps waiting for another.
        if self.sent > self.refusals
            && plain(&self.raw[self.sent_at..])
                .to_ascii_lowercase()
                .contains("invalid code")
        {
            self.refusals = self.sent;
            self.sent_at = self.raw.len();
        }
    }

    fn awaiting_code(&self) -> bool {
        self.paste_code && self.prompted && self.link.is_some() && self.sent == self.refusals
    }

    fn sent(&mut self, code: &Secret) {
        self.sent += 1;
        self.sent_at = self.raw.len();
        self.typed = Some(Zeroizing::new(code.expose().to_owned()));
    }

    fn stage(&self) -> Option<SignInStage> {
        if self.sent > self.refusals {
            return Some(SignInStage::Checking);
        }
        let link = self.link.as_ref()?;
        Some(SignInStage::OnPhone {
            url_host: host(link.expose()).unwrap_or_default(),
            link: link.clone(),
            code: self.code.clone(),
            paste_code: self.awaiting_code(),
            code_refused: self.refusals > 0,
        })
    }

    fn show(&mut self, stage: &(dyn Fn(SignInStage) + Send + Sync)) {
        let Some(now) = self.stage() else { return };
        if self.shown.as_ref() != Some(&now) {
            self.shown = Some(now.clone());
            stage(now);
        }
    }

    /// Why the tool stopped, from what it said last. Codex 0.153 to 0.164
    /// say, after "Error logging in with device code: ", one of: "failed to
    /// request device code: …" (no answer), "device code request failed with
    /// status …" (asking for a code), "device code login is not enabled …"
    /// (404 asking), "device auth failed with status …" or "device code
    /// exchange failed: …" (after the page was used), or "device auth timed
    /// out …".
    fn refused(&self) -> Option<Refused> {
        let text = plain(&self.raw).to_ascii_lowercase();
        let status = |after: &str| -> Option<u16> {
            let rest = &text[text.find(after)? + after.len()..];
            let at = rest.find("status ")? + "status ".len();
            rest.get(at..at + 3)?.parse().ok()
        };
        let by = |code: Option<u16>, refused: Refused| match code {
            Some(500..=599) => Refused::Unreachable,
            Some(400..=499) => refused,
            _ => Refused::Unreachable,
        };
        if text.contains("device auth timed out") || text.contains("code expired") {
            Some(Refused::CodeExpired)
        } else if text.contains("device code login is not enabled") {
            Some(Refused::NoCode)
        } else if text.contains("failed to request device code") {
            Some(Refused::Unreachable)
        } else if text.contains("device code request failed") {
            Some(by(status("device code request failed"), Refused::NoCode))
        } else if text.contains("device auth failed") {
            Some(by(status("device auth failed"), Refused::SaidNo))
        } else if text.contains("device code exchange failed") {
            // Its words are the server's or the network's; only a 4xx is
            // OpenAI saying no.
            let code = status("device code exchange failed").or_else(|| {
                ["400", "401", "403", "404"]
                    .iter()
                    .find(|c| text.contains(*c))
                    .and_then(|c| c.parse().ok())
            });
            Some(by(code, Refused::SaidNo))
        } else if self.sent > 0 && (text.contains("login failed") || text.contains("invalid code"))
        {
            Some(Refused::LoginFailed)
        } else {
            None
        }
    }
}

/// The page a sign-in on the phone opens: the first provider page the tool
/// printed, as a terminal link's target (whole however the terminal wraps
/// it) or else as text. A page that returns to this computer, by its
/// address or the one it sends the browser back to, is no use on a phone.
fn phone_link(raw: &str) -> Option<String> {
    let mut found = osc8_targets(raw);
    let text = plain(raw);
    let mut from = 0;
    while let Some(at) = text[from..].find("https://") {
        let start = from + at;
        let rest = &text[start..];
        // An address counts once whitespace shows where it ends.
        let Some(len) = rest.find(|c: char| c.is_whitespace()) else {
            break;
        };
        found.push(
            rest[..len]
                .trim_end_matches(['.', ',', ')', '"', '\''])
                .to_owned(),
        );
        from = start + len;
    }
    found.into_iter().find(|url| phone_page(url))
}

/// The targets of the terminal hyperlinks (OSC 8) in `raw`, in order:
/// `ESC ] 8 ; params ; URI` ended by BEL or `ESC \`.
fn osc8_targets(raw: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = raw;
    while let Some(at) = rest.find("\u{1b}]8;") {
        rest = &rest[at + 4..];
        let Some(end) = rest.find(['\u{7}', '\u{1b}']) else {
            break;
        };
        let body = &rest[..end];
        rest = &rest[end..];
        if let Some((_, uri)) = body.split_once(';')
            && !uri.is_empty()
        {
            targets.push(uri.to_owned());
        }
    }
    targets
}

/// An HTTPS provider page that doesn't send the browser back to this
/// computer.
fn phone_page(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let local = |host: Option<&str>| {
        matches!(
            host,
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1" | "0.0.0.0")
        )
    };
    u.scheme() == "https"
        && u.host_str().is_some()
        && !local(u.host_str())
        && u.password().is_none()
        && !u.query_pairs().any(|(k, v)| {
            k == "redirect_uri" && url::Url::parse(&v).is_ok_and(|r| local(r.host_str()))
        })
}

/// The most the addresses the tool asked to open may come to.
const PAGES_LIMIT: u64 = 64 * 1024;

/// The tool asked to open more than [`PAGES_LIMIT`] of addresses.
struct TooMany;

/// The addresses the tool has asked to open so far, `None` while there are
/// none to read.
fn pages(urls: &Path) -> Result<Option<String>, TooMany> {
    let Ok(file) = std::fs::File::open(urls) else {
        return Ok(None);
    };
    let mut text = String::new();
    if std::io::Read::read_to_string(&mut std::io::Read::take(file, PAGES_LIMIT + 1), &mut text)
        .is_err()
    {
        return Ok(None);
    }
    if text.len() as u64 > PAGES_LIMIT {
        return Err(TooMany);
    }
    Ok(Some(text))
}

/// `BROWSER` and an `xdg-open` that only write down the address, so the
/// tool's page is opened by Ophio, once, and the phone learns where.
fn browser_stand_in(
    scratch: &Path,
    urls: &Path,
    tools: &Tools,
    extra: &mut Vec<(String, OsString)>,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use crate::files::write_private;
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {}; done\n",
            shell_words::quote(&urls.to_string_lossy())
        );
        let shim = scratch.join("xdg-open");
        write_private(&shim, script.as_bytes())?;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700))?;
        let mut path = vec![scratch.to_path_buf()];
        path.extend(std::env::split_paths(
            &tools.var("PATH").unwrap_or_default(),
        ));
        extra.push((
            "PATH".into(),
            std::env::join_paths(path).map_err(std::io::Error::other)?,
        ));
        extra.push(("BROWSER".into(), shim.clone().into()));
        extra.push(("GH_BROWSER".into(), shim.into()));
    }
    #[cfg(not(unix))]
    let _ = (scratch, urls, tools, extra);
    Ok(())
}

/// What the tool has said and opened so far.
#[derive(Default)]
struct Seen {
    text: String,
    /// Where the next "Press Enter" and address are looked for.
    enter_from: usize,
    url_from: usize,
    code: Option<String>,
    host: Option<String>,
    opened: HashSet<String>,
    printed: Option<(String, Instant)>,
    shown: Option<SignInStage>,
    /// Where Øphio can't stand in for the browser (see
    /// [`browser_stand_in`]), as on Windows, the tool opens its pages
    /// itself, so the address it printed is the page the owner is shown.
    tool_opens: bool,
}

impl Seen {
    /// Takes in more output; returns how many times the tool asked for Enter.
    fn feed(&mut self, text: &str) -> usize {
        self.text.push_str(text);
        if self.text.len() > KEPT {
            let mut cut = self.text.len() - KEPT / 2;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
            self.enter_from = self.enter_from.saturating_sub(cut);
            self.url_from = self.url_from.saturating_sub(cut);
        }
        let mut enters = 0;
        while let Some(at) = self.text[self.enter_from..].find("Press Enter") {
            self.enter_from += at + "Press Enter".len();
            enters += 1;
        }
        if self.code.is_none() {
            self.code = one_time_code(&self.text);
        }
        // An address counts once whitespace shows where it ends.
        while let Some(at) = self.text[self.url_from..].find("https://") {
            let start = self.url_from + at;
            let rest = &self.text[start..];
            let Some(len) = rest.find(|c: char| c.is_whitespace()) else {
                break;
            };
            let url = rest[..len]
                .trim_end_matches(['.', ',', ')', '"', '\''])
                .to_owned();
            self.url_from = start + len;
            if self.printed.is_none() && allowed(&url) {
                self.printed = Some((url, Instant::now()));
            }
        }
        enters
    }

    fn open(&mut self, url: &str, opener: &dyn Opener) {
        if !allowed(url) || !self.opened.insert(url.to_owned()) {
            return;
        }
        match opener.open(url) {
            Ok(()) => self.host = host(url),
            Err(e) => tracing::warn!("accounts: {e}"),
        }
    }

    fn open_printed(&mut self, opener: &dyn Opener) {
        if !cfg!(unix) || !self.opened.is_empty() {
            return;
        }
        if let Some((url, at)) = self.printed.clone()
            && at.elapsed() >= PRINTED_GRACE
        {
            self.open(&url, opener);
        }
    }

    fn show(&mut self, stage: &(dyn Fn(SignInStage) + Send + Sync)) {
        let printed = (self.printed.as_ref())
            .filter(|_| self.tool_opens)
            .and_then(|(url, _)| host(url));
        let now = match (&self.code, self.host.as_ref().or(printed.as_ref())) {
            (Some(code), host) => SignInStage::Code {
                code: code.clone(),
                url_host: host.cloned().unwrap_or_default(),
            },
            (None, Some(host)) => SignInStage::Browser {
                url_host: host.clone(),
            },
            (None, None) => return,
        };
        if self.shown.as_ref() != Some(&now) {
            self.shown = Some(now.clone());
            stage(now);
        }
    }
}

/// Only the provider's pages, never a file or another kind of address.
fn allowed(url: &str) -> bool {
    match url::Url::parse(url) {
        Ok(u) if u.scheme() == "https" => u.host_str().is_some() && u.password().is_none(),
        Ok(u) if u.scheme() == "http" => matches!(u.host_str(), Some("localhost" | "127.0.0.1")),
        _ => false,
    }
}

fn host(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_owned)
}

/// The code a tool asks the owner to enter on the provider's page: four
/// letters or digits, a dash and four to six more, as `ABCD-1234` (GitHub) or
/// `ABCD-12345` (Codex).
fn one_time_code(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let after = lower.find("one-time code")? + "one-time code".len();
    let bytes = text.as_bytes();
    let part = |b: &[u8]| {
        b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    (after..bytes.len()).find_map(|i| {
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if !boundary || bytes.get(i + 4) != Some(&b'-') || !part(bytes.get(i..i + 4)?) {
            return None;
        }
        (4..=6).rev().find_map(|n| {
            let end = i + 5 + n;
            let tail = bytes.get(i + 5..end)?;
            (part(tail) && bytes.get(end).is_none_or(|c| !c.is_ascii_alphanumeric()))
                .then(|| text[i..end].to_owned())
        })
    })
}

/// The sign-in command, in a container of its own with all it starts.
struct Proc {
    contained: Contained,
    pid: Option<u32>,
    io: Io,
}

// One for each sign-in, so the size of either matters little.
#[allow(clippy::large_enum_variant)]
enum Io {
    Plain {
        child: tokio::process::Child,
        stdin: Option<tokio::process::ChildStdin>,
    },
    Pty {
        child: Box<dyn portable_pty::Child + Send + Sync>,
        press: std::sync::mpsc::SyncSender<Press>,
        _master: Box<dyn portable_pty::MasterPty + Send>,
    },
}

/// What is written to the tool's terminal: a key, or a code pasted on the
/// phone, wiped once written.
enum Press {
    Key(&'static [u8]),
    Code(Zeroizing<Vec<u8>>),
}
impl Press {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Key(bytes) => bytes,
            Self::Code(bytes) => bytes,
        }
    }
}

/// Enter presses waiting to be written. A tool that is not reading isn't
/// waiting for Enter, so presses past these are dropped.
const PRESSES: usize = 4;

/// Writes what it is sent, Enter or a terminal's answer, on `writer`, on a
/// thread of its own: on a terminal whose tool isn't reading the write waits,
/// and the sign-in must still see its cancellation, time limit and output
/// limit meanwhile. The thread ends once every sender is dropped, or once a
/// write fails as the tool's terminal closes.
fn presser(mut writer: Box<dyn std::io::Write + Send>) -> std::sync::mpsc::SyncSender<Press> {
    let (press, pressed) = std::sync::mpsc::sync_channel::<Press>(PRESSES);
    std::thread::spawn(move || {
        while let Ok(press) = pressed.recv() {
            if writer
                .write_all(press.bytes())
                .and_then(|()| writer.flush())
                .is_err()
            {
                break;
            }
        }
    });
    press
}

/// A Windows pseudo-console inherits its cursor, so the first thing it
/// writes is a request for the cursor's position, `ESC [ 6 n`, and it shows
/// nothing more until it hears back. A new one has nothing to inherit, so
/// the top-left corner is the answer. Only output that begins with the
/// request is answered, once.
#[derive(Debug, Default)]
struct CursorRequest {
    seen: Vec<u8>,
    settled: bool,
}

impl CursorRequest {
    const REQUEST: &[u8] = b"\x1b[6n";
    const ANSWER: &[u8] = b"\x1b[1;1R";

    /// Whether `chunk`, the next output, completes the opening request.
    fn answers(&mut self, chunk: &[u8]) -> bool {
        if self.settled {
            return false;
        }
        self.seen.extend_from_slice(chunk);
        if self.seen.len() < Self::REQUEST.len() && Self::REQUEST.starts_with(&self.seen) {
            return false;
        }
        self.settled = true;
        self.seen.starts_with(Self::REQUEST)
    }
}

/// Hands what the tool's terminal shows, read from `reader`, to `out` until
/// either ends. With `answer`, the terminal's opening request for the
/// cursor's position (see [`CursorRequest`]) is answered through it.
fn relay_terminal(
    reader: &mut dyn std::io::Read,
    out: &mpsc::Sender<Vec<u8>>,
    answer: Option<std::sync::mpsc::SyncSender<Press>>,
) {
    let mut buf = [0u8; 4096];
    let mut request = CursorRequest::default();
    while let Ok(n) = reader.read(&mut buf) {
        if n == 0 {
            break;
        }
        if request.answers(&buf[..n])
            && let Some(answer) = &answer
        {
            let _ = answer.try_send(Press::Key(CursorRequest::ANSWER));
        }
        if out.blocking_send(buf[..n].to_vec()).is_err() {
            break;
        }
    }
}

impl Proc {
    async fn start(
        run: &Run<'_>,
        extra: &[(String, OsString)],
        out: mpsc::Sender<Vec<u8>>,
    ) -> Result<Self, String> {
        let name = &run.argv[0];
        let not_started = |e: &dyn std::fmt::Display| format!("{name} could not start: {e}");
        let contained = Contained::new(name)?;
        if run.pty {
            let pair = portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize {
                    rows: 40,
                    cols: 200,
                    ..Default::default()
                })
                .map_err(|e| not_started(&e))?;
            let mut reader = pair
                .master
                .try_clone_reader()
                .map_err(|e| not_started(&e))?;
            let writer = pair.master.take_writer().map_err(|e| not_started(&e))?;
            #[cfg(target_os = "linux")]
            let child: Box<dyn portable_pty::Child + Send + Sync> = {
                let mut command = run.tools.std_command(run.argv, extra);
                command.current_dir(&run.scratch);
                Box::new(
                    on_terminal(&*pair.master, command, contained.container())
                        .map_err(|e| not_started(&e))?,
                )
            };
            #[cfg(not(target_os = "linux"))]
            let child = {
                let mut command = portable_pty::CommandBuilder::new(run.tools.find(name));
                command.args(&run.argv[1..]);
                command.cwd(&run.scratch);
                for (k, v) in run.tools.env.iter() {
                    command.env(k, v);
                }
                for (k, v) in extra {
                    command.env(k, v);
                }
                let child = pair
                    .slave
                    .spawn_command(command)
                    .map_err(|e| not_started(&e))?;
                // Where the container is the tool's process group, the
                // session the pty's spawn makes leads it. On Windows the
                // tool joins its job only once it has started: what it
                // starts before then is outside.
                #[cfg(unix)]
                if let Some(pid) = child.process_id() {
                    contained.container().adopt(pid);
                }
                #[cfg(windows)]
                if let Err(e) = child
                    .as_raw_handle()
                    .ok_or_else(|| "no handle".to_owned())
                    .and_then(|h| contained.container().adopt(h).map_err(|e| e.to_string()))
                {
                    let mut child = child;
                    let _ = child.kill();
                    contained.end(None).await;
                    return Err(not_started(&e));
                }
                child
            };
            drop(pair.slave);
            let press = presser(writer);
            // Windows' pseudo-console shows the tool nothing until its first
            // request, for the cursor's position, is answered (see
            // `CursorRequest`); the phone's terminal answers it the same way.
            let answer = cfg!(windows).then(|| press.clone());
            std::thread::spawn(move || relay_terminal(&mut reader, &out, answer));
            return Ok(Self {
                pid: child.process_id(),
                contained,
                io: Io::Pty {
                    child,
                    press,
                    _master: pair.master,
                },
            });
        }
        let mut command = run.tools.command(run.argv, extra);
        command
            .current_dir(&run.scratch)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = contained
            .container()
            .spawn(&mut command)
            .await
            .map_err(|e| not_started(&e))?;
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        ]
        .into_iter()
        .flatten()
        {
            let out = out.clone();
            tokio::spawn(async move {
                let mut stream = stream;
                let mut buf = [0u8; 4096];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 || out.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            });
        }
        let stdin = child.stdin.take();
        Ok(Self {
            pid: child.id(),
            contained,
            io: Io::Plain { child, stdin },
        })
    }

    fn enter(&mut self) {
        match &mut self.io {
            Io::Plain { stdin, .. } => {
                if let Some(stdin) = stdin.as_mut() {
                    // A full pipe means the tool is not reading; it is not waiting for Enter.
                    let _ = tokio::io::AsyncWriteExt::write_all(stdin, b"\n").now_or_never();
                }
            }
            Io::Pty { press, .. } => {
                // A full queue means the tool is not reading, as above.
                let _ = press.try_send(Press::Key(b"\r"));
            }
        }
    }

    /// Writes `code` and Enter, once, as the owner would type them.
    fn type_code(&mut self, code: &Secret) {
        let mut line = Zeroizing::new(Vec::with_capacity(code.expose().len() + 1));
        line.extend_from_slice(code.expose().as_bytes());
        match &mut self.io {
            Io::Plain { stdin, .. } => {
                line.push(b'\n');
                if let Some(stdin) = stdin.as_mut() {
                    let _ = tokio::io::AsyncWriteExt::write_all(stdin, &line).now_or_never();
                }
            }
            Io::Pty { press, .. } => {
                line.push(b'\r');
                let _ = press.try_send(Press::Code(line));
            }
        }
    }

    /// Whether the tool has exited, leaving it unreaped until all it
    /// started has ended.
    fn exited(&mut self) -> Option<bool> {
        #[cfg(unix)]
        return self.pid.and_then(crate::tool::exited);
        #[cfg(not(unix))]
        match &mut self.io {
            Io::Plain { child, .. } => child.try_wait().ok().flatten().map(|s| s.success()),
            Io::Pty { child, .. } => child.try_wait().ok().flatten().map(|s| s.success()),
        }
    }

    /// Ends the tool and everything it started, and waits until that is
    /// confirmed.
    async fn end(&mut self) {
        self.contained.end(self.pid).await;
        match &mut self.io {
            Io::Plain { child, .. } => {
                let _ = child.try_wait();
            }
            Io::Pty { child, .. } => {
                let _ = child.try_wait();
            }
        }
    }
}

/// Starts `command` on the terminal `master` leads, in a session of its own,
/// as the pty's own spawn does, but inside `container` before its program
/// runs, so nothing it starts is outside.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn on_terminal(
    master: &dyn portable_pty::MasterPty,
    mut command: std::process::Command,
    container: &containment::Container,
) -> std::io::Result<std::process::Child> {
    use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
    let terminal = std::fs::File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(
            master
                .tty_name()
                .ok_or_else(|| std::io::Error::other("the terminal has no name"))?,
        )?;
    command
        .stdin(terminal.try_clone()?)
        .stdout(terminal.try_clone()?)
        .stderr(terminal);
    container
        .prepare_session(&mut command)
        .map_err(std::io::Error::other)?;
    // SAFETY: between fork and exec the new process only makes the
    // async-signal-safe calls signal(2), sigprocmask(2), setsid(2), ioctl(2)
    // and close_range(2), as the pty's own spawn does. Only a kernel without
    // close_range falls back to that spawn's way of closing descriptors.
    unsafe {
        command.pre_exec(|| {
            for signal in [
                libc::SIGCHLD,
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGALRM,
            ] {
                libc::signal(signal, libc::SIG_DFL);
            }
            let empty: libc::sigset_t = std::mem::zeroed();
            libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            #[allow(clippy::cast_lossless)]
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::syscall(
                libc::SYS_close_range,
                3 as libc::c_uint,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) == -1
            {
                portable_pty::unix::close_random_fds();
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    container.adopt(child.id());
    Ok(child)
}

trait NowOrNever: std::future::Future + Sized {
    fn now_or_never(self) -> Option<Self::Output> {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut this = std::pin::pin!(self);
        match this.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(out) => Some(out),
            std::task::Poll::Pending => None,
        }
    }
}
impl<F: std::future::Future> NowOrNever for F {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terminal_s_cursor_request_is_answered_on_the_tool_s_input() {
        #[derive(Clone, Default)]
        struct Written(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Written {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let relayed = |shown: &[u8], answering: bool| {
            let written = Written::default();
            let press = presser(Box::new(written.clone()));
            let (out, mut seen) = mpsc::channel(16);
            relay_terminal(
                &mut std::io::Cursor::new(shown.to_vec()),
                &out,
                answering.then(|| press.clone()),
            );
            drop((out, press));
            let mut shown = Vec::new();
            while let Some(chunk) = seen.blocking_recv() {
                shown.extend(chunk);
            }
            // The writer's thread ends once every sender is gone.
            let deadline = Instant::now() + Duration::from_secs(5);
            while std::sync::Arc::strong_count(&written.0) > 1 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let written = written.0.lock().unwrap().clone();
            (shown, written)
        };
        let opening = b"\x1b[6nOpening browser to sign in";
        let (shown, written) = relayed(opening, true);
        assert_eq!(shown, opening, "everything shown is still read");
        assert_eq!(written, b"\x1b[1;1R", "answered with the top-left corner");
        // Where no answer is wanted, as outside Windows, nothing is written.
        assert_eq!(relayed(opening, false).1, b"");
        // Nor when the tool shows something else first.
        assert_eq!(relayed(b"Opening browser\x1b[6n", true).1, b"");
    }

    #[test]
    fn only_the_opening_cursor_request_is_answered() {
        // Whole, and split across reads.
        assert!(CursorRequest::default().answers(b"\x1b[6nOpening browser"));
        let mut request = CursorRequest::default();
        assert!(!request.answers(b"\x1b["));
        assert!(request.answers(b"6n"));
        assert!(!request.answers(b"\x1b[6n"), "answered once");
        // Output that starts otherwise is never answered, even if it asks later.
        let mut request = CursorRequest::default();
        assert!(!request.answers(b"Opening browser"));
        assert!(!request.answers(b"\x1b[6n"));
    }

    #[test]
    fn finds_the_one_time_code_and_each_request_for_enter() {
        let mut seen = Seen::default();
        assert_eq!(seen.feed("! First copy your one-time code: "), 0);
        assert_eq!(seen.code, None);
        assert_eq!(
            seen.feed(
                "1A2B-3C4D\nPress Enter to open https://github.com/login/device in your browser... "
            ),
            1
        );
        assert_eq!(seen.code.as_deref(), Some("1A2B-3C4D"));
        assert_eq!(
            seen.printed.as_ref().map(|p| p.0.as_str()),
            Some("https://github.com/login/device")
        );
        assert_eq!(seen.feed("still waiting"), 0);
        assert_eq!(
            one_time_code("code ABCD-EFGH"),
            None,
            "only after the words one-time code"
        );
    }

    #[test]
    fn a_page_the_tool_opens_itself_is_named_from_the_address_it_printed() {
        let stages = std::sync::Mutex::new(Vec::new());
        let record = |stage| stages.lock().unwrap().push(stage);
        let printed = "Opening browser to sign in…\nIf the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true\nPaste code here if prompted > ";
        // Where the tool opens its pages itself, as on Windows.
        let mut seen = Seen {
            tool_opens: true,
            ..Seen::default()
        };
        seen.feed(printed);
        seen.show(&record);
        assert_eq!(
            stages.lock().unwrap().as_slice(),
            [SignInStage::Browser {
                url_host: "claude.com".into()
            }]
        );
        // Where Øphio opens them, only a page it opened is named.
        stages.lock().unwrap().clear();
        let mut seen = Seen::default();
        seen.feed(printed);
        seen.show(&record);
        assert!(stages.lock().unwrap().is_empty());
    }

    #[test]
    fn enter_is_never_waited_for() {
        // A terminal whose tool reads nothing: every write waits.
        struct Stuck(std::sync::mpsc::Receiver<()>);
        impl std::io::Write for Stuck {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                let _ = self.0.recv();
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (closed, closes) = std::sync::mpsc::channel();
        let press = presser(Box::new(Stuck(closes)));
        let began = Instant::now();
        let queued = (0..10_000)
            .filter(|_| press.try_send(Press::Key(b"\r")).is_ok())
            .count();
        assert!(began.elapsed() < Duration::from_secs(1));
        // One taken by the waiting write, and as many as wait behind it.
        assert!(queued <= PRESSES + 1, "{queued}");
        // Once the terminal closes the write fails, and the thread ends.
        drop(closed);
        let ended = Instant::now();
        while !matches!(
            press.try_send(Press::Key(b"\r")),
            Err(std::sync::mpsc::TrySendError::Disconnected(_))
        ) {
            assert!(
                ended.elapsed() < Duration::from_secs(5),
                "the thread still runs"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Claude Code 2.1.289's output on a terminal, its link's state shortened.
    const CLAUDE_SAID: &str = "Opening browser to sign in\u{2026}\r\nIf the browser didn't open, visit: \u{1b}]8;;https://claude.com/cai/oauth/authorize?code=true&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=s1\u{7}https://claude.com/cai/oauth/authorize?code=true&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=s1\u{1b}]8;;\u{7}\r\nPaste code here if prompted > ";
    const CLAUDE_LINK: &str = "https://claude.com/cai/oauth/authorize?code=true&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=s1";

    #[test]
    fn the_phone_gets_the_terminal_link_s_target_however_the_output_is_cut() {
        assert_eq!(osc8_targets(CLAUDE_SAID), [CLAUDE_LINK]);
        // Read a byte at a time, the link is still whole once its end shows.
        let mut seen = OnPhone::new(true);
        for b in CLAUDE_SAID.as_bytes().chunks(1) {
            seen.feed(b);
        }
        assert_eq!(seen.link.as_ref().map(Secret::expose), Some(CLAUDE_LINK));
        assert!(seen.awaiting_code(), "asked for the page's code");
        // Where the printed text is broken across lines, the link's target
        // is still whole and is the one taken.
        let (target, text) = CLAUDE_SAID.split_once('\u{7}').unwrap();
        let wrapped = format!(
            "{target}\u{7}{}",
            text.replacen("platform.claude", "platform.\r\nclaude", 1)
        );
        let mut seen = OnPhone::new(true);
        seen.feed(wrapped.as_bytes());
        assert_eq!(seen.link.as_ref().map(Secret::expose), Some(CLAUDE_LINK));
    }

    #[test]
    fn a_page_that_returns_to_this_computer_never_goes_to_the_phone() {
        assert!(phone_page(CLAUDE_LINK));
        assert!(phone_page("https://auth.openai.com/codex/device"));
        assert!(!phone_page(
            "https://claude.com/cai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A40401%2Fcallback"
        ));
        assert!(!phone_page("http://localhost:1455/auth/callback"));
        assert!(!phone_page("https://localhost/x"));
        assert!(!phone_page("https://user:pw@example.com/"));
        // The page opened here, printed first as text, is passed over.
        let said = "visit https://claude.com/a?redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcb \nor https://claude.com/b \n";
        assert_eq!(phone_link(said).as_deref(), Some("https://claude.com/b"));
    }

    #[test]
    fn codex_s_device_page_and_code_are_read_through_its_colours() {
        let said = "\nFollow these steps to sign in with ChatGPT using device code authorization:\n\n1. Open this link in your browser and sign in to your account\n   \u{1b}[94mhttps://auth.openai.com/codex/device\u{1b}[0m\n\n2. Enter this one-time code \u{1b}[90m(expires in 15 minutes)\u{1b}[0m\n   \u{1b}[94mAB12-CDE34\u{1b}[0m\n\n";
        let mut seen = OnPhone::new(false);
        seen.feed(said.as_bytes());
        assert_eq!(
            seen.stage(),
            Some(SignInStage::OnPhone {
                link: Secret::new("https://auth.openai.com/codex/device"),
                url_host: "auth.openai.com".into(),
                code: Some("AB12-CDE34".into()),
                paste_code: false,
                code_refused: false,
            })
        );
        assert!(!seen.awaiting_code(), "Codex takes no code back");
    }

    #[test]
    fn a_refused_code_is_asked_for_again() {
        let mut seen = OnPhone::new(true);
        seen.feed(CLAUDE_SAID.as_bytes());
        assert!(seen.awaiting_code());
        seen.sent(&Secret::new("first-code#s1"));
        // The terminal echoes it, split across reads: wiped once it shows.
        seen.feed(b"first-co");
        seen.feed(b"de#s1\r\n");
        assert!(!seen.raw.contains("first-co"), "the echoed code is wiped");
        assert!(seen.typed.is_none());
        assert_eq!(seen.stage(), Some(SignInStage::Checking));
        assert!(!seen.awaiting_code(), "one code at a time");
        seen.feed(b"Invalid code. Please make sure the full code was copied.\r\n");
        assert!(seen.awaiting_code());
        assert!(matches!(
            seen.stage(),
            Some(SignInStage::OnPhone {
                paste_code: true,
                code_refused: true,
                ..
            })
        ));
        // The refusal before doesn't count against the next code.
        seen.sent(&Secret::new("second-code#s1"));
        seen.feed(b"\r\n");
        assert_eq!(seen.stage(), Some(SignInStage::Checking));
    }

    #[test]
    fn why_a_sign_in_on_the_phone_stopped() {
        let said = |text: &str, sent: usize| {
            let mut seen = OnPhone::new(true);
            seen.feed(text.as_bytes());
            seen.sent = sent;
            seen.refused()
        };
        let codex = |text: &str| said(&format!("Error logging in with device code: {text}"), 0);
        // OpenAI couldn't be reached, or failed itself.
        assert_eq!(
            codex("failed to request device code: error sending request for url"),
            Some(Refused::Unreachable)
        );
        assert_eq!(
            codex("device code request failed with status 503 Service Unavailable"),
            Some(Refused::Unreachable)
        );
        assert_eq!(
            codex("device auth failed with status 502 Bad Gateway"),
            Some(Refused::Unreachable)
        );
        assert_eq!(
            codex("device code exchange failed: error sending request"),
            Some(Refused::Unreachable)
        );
        // OpenAI gave no code to start with.
        assert_eq!(
            codex(
                "device code login is not enabled for this Codex server. Use the browser login or verify the server URL."
            ),
            Some(Refused::NoCode)
        );
        assert_eq!(
            codex("device code request failed with status 403 Forbidden"),
            Some(Refused::NoCode)
        );
        // OpenAI refused once the page was used: the setting may be off.
        assert_eq!(
            codex("device auth failed with status 403 Forbidden"),
            Some(Refused::SaidNo)
        );
        assert_eq!(
            codex("device code exchange failed: token endpoint returned status 400 Bad Request"),
            Some(Refused::SaidNo)
        );
        assert_eq!(
            codex("device auth timed out after 15 minutes"),
            Some(Refused::CodeExpired)
        );
        assert_eq!(
            said("Login failed: invalid_grant", 1),
            Some(Refused::LoginFailed)
        );
        assert_eq!(said("Login failed: invalid_grant", 0), None);
        assert_eq!(said("Something else went wrong", 0), None);
    }

    #[test]
    fn one_time_codes_of_github_and_codex() {
        assert_eq!(
            one_time_code("First copy your one-time code: 1A2B-3C4D\n").as_deref(),
            Some("1A2B-3C4D")
        );
        assert_eq!(
            one_time_code("Enter this one-time code (expires in 15 minutes)\n   AB12-CDE34\n")
                .as_deref(),
            Some("AB12-CDE34")
        );
        assert_eq!(one_time_code("one-time code ABCD-EFGHIJK "), None);
        assert_eq!(one_time_code("one-time code xABCD-EFGH "), None);
    }

    #[test]
    fn only_provider_pages_are_opened() {
        assert!(allowed("https://github.com/login/device"));
        assert!(allowed("http://localhost:1455/auth/callback"));
        assert!(!allowed("http://example.com/"));
        assert!(!allowed("file:///etc/passwd"));
        assert!(!allowed("javascript:alert(1)"));
    }
}
