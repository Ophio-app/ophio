// SPDX-License-Identifier: Apache-2.0
//! What owner commands print for people. Each summary shows part of the
//! command's `--json` output: states in words and, for anything not working,
//! the problem and what to do. Account names are never shown.
use crate::{
    cli::{InboxStatus, ago},
    config::LibraryRootConfig,
    frontdoors::{FrontdoorStatus, VoiceStatus},
};
use channels::{ChannelState, ChannelStatus};
use ports::{COMMAND, PRODUCT_NAME};
use protocol::{
    auth::ChannelKind,
    capability::{AccountReadiness, Availability, HarnessInfo, IntegrationLevel},
    model::{CheckStatus, Project, ReadinessArea, ReadinessCheck, ReadinessReport, Snapshot},
    time::Timestamp,
};
use std::fmt::Write;

/// Colour is for terminals only, and never when `NO_COLOR` is set or the
/// terminal says it is too basic for it (`TERM=dumb`).
#[derive(Debug, Clone, Copy)]
pub struct Style {
    colour: bool,
}
impl Style {
    pub const PLAIN: Self = Self { colour: false };
    pub fn stdout() -> Self {
        use std::io::IsTerminal;
        Self {
            colour: wants_colour(
                std::io::stdout().is_terminal(),
                std::env::var_os("NO_COLOR").is_some(),
                std::env::var("TERM").ok().as_deref(),
            ),
        }
    }
    fn paint(self, state: &State) -> String {
        match (self.colour, state.tone) {
            (true, Tone::Good) => format!("\x1b[32m{}\x1b[0m", state.words),
            (true, Tone::Attention) => format!("\x1b[33m{}\x1b[0m", state.words),
            _ => state.words.clone(),
        }
    }
}

fn wants_colour(terminal: bool, no_color: bool, term: Option<&str>) -> bool {
    terminal && !no_color && term != Some("dumb")
}

/// Stored text as printed. A name, path or detail can hold characters that
/// move the cursor, hide what follows, start a line of its own or reverse the
/// text around it; each of those is written as its escape instead.
pub fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            c if c.is_control() => out.extend(c.escape_default()),
            // Direction marks, embeddings, overrides and isolates, and the
            // line and paragraph separators.
            '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => out.extend(c.escape_default()),
            c => out.push(c),
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tone {
    Good,
    Neutral,
    /// Works once set up; not broken, but the doctor's summary names it.
    Setup,
    Attention,
}
struct State {
    tone: Tone,
    words: String,
    notes: Vec<String>,
}
struct Row {
    label: String,
    state: State,
}

fn state<'a>(
    tone: Tone,
    words: impl Into<String>,
    notes: impl IntoIterator<Item = &'a str>,
) -> State {
    State {
        tone,
        words: shown(&words.into()),
        notes: notes.into_iter().map(|note| shown(&advice(note))).collect(),
    }
}
fn row(label: impl Into<String>, state: State) -> Row {
    Row {
        label: shown(&label.into()),
        state,
    }
}
/// A remedy that is only a command becomes a sentence.
fn advice(note: &str) -> String {
    if note.starts_with(&format!("{COMMAND} ")) {
        format!("Run `{note}`.")
    } else {
        note.to_owned()
    }
}
fn table(rows: &[Row], indent: &str, style: Style) -> String {
    let width = rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (i, Row { label, state }) in rows.iter().enumerate() {
        let _ = writeln!(out, "{indent}{label:width$}  {}", style.paint(state));
        // Several checks often share one cause; its fix is shown once.
        let same = rows[..i]
            .iter()
            .find(|r| !state.notes.is_empty() && r.state.notes == state.notes);
        let notes = match same {
            Some(first) => vec![format!("Same as {}.", first.label)],
            None => state.notes.clone(),
        };
        for note in notes {
            let _ = writeln!(out, "{indent}{:width$}  {note}", "");
        }
    }
    out
}

fn availability(a: &Availability) -> State {
    match a {
        Availability::Available => state(Tone::Good, "ready", None),
        Availability::Unsupported { reason } => state(Tone::Neutral, "not available", [&**reason]),
        Availability::PermissionMissing { remedy } => {
            state(Tone::Setup, "needs setup", [&**remedy])
        }
        Availability::TemporarilyUnavailable { reason } | Availability::Incompatible { reason } => {
            state(Tone::Attention, "not ready", [&**reason])
        }
        Availability::Revoked => state(Tone::Attention, "access revoked", None),
        Availability::Unknown { reason } => state(Tone::Neutral, "not known", [&**reason]),
    }
}
/// The desktop ends its sharing, at a screen lock for one; the command that
/// started it starts it again.
fn desktop_state(a: &Availability) -> State {
    match a {
        Availability::Revoked => {
            let enable = format!("{COMMAND} desktop enable");
            state(Tone::Attention, "access revoked", [enable.as_str()])
        }
        other => availability(other),
    }
}
fn check(c: &ReadinessCheck) -> State {
    let (detail, remedy) = (c.detail.as_deref(), c.remedy.as_deref());
    match c.status {
        CheckStatus::Passed => state(Tone::Good, "ready", None),
        CheckStatus::Failed => state(
            Tone::Attention,
            "not ready",
            detail.into_iter().chain(remedy),
        ),
        // A setup check's detail only repeats that it needs setup.
        CheckStatus::NotRun if remedy.is_some() => state(Tone::Setup, "needs setup", remedy),
        CheckStatus::NotRun => state(Tone::Neutral, "not available", detail),
        CheckStatus::Unknown => state(Tone::Neutral, "not known", detail),
    }
}
fn voice_state(v: &VoiceStatus) -> State {
    match &v.availability {
        Availability::Available => state(Tone::Good, format!("ready, using {}", v.model), None),
        other => availability(other),
    }
}
fn harness(h: &HarnessInfo) -> State {
    match (&h.installed, &h.account) {
        (Availability::Available, AccountReadiness::SignedIn { .. }) => {
            state(Tone::Good, "signed in", None)
        }
        (Availability::Available, AccountReadiness::SignedOut { remedy }) => {
            state(Tone::Attention, "signed out", [&**remedy])
        }
        (Availability::Available, AccountReadiness::Unknown { reason }) => {
            state(Tone::Neutral, "sign-in not known", [&**reason])
        }
        (Availability::Unsupported { reason }, _) => {
            state(Tone::Neutral, "not installed", [&**reason])
        }
        (installed, _) => availability(installed),
    }
}
fn channel(status: &ChannelStatus, live: bool) -> Row {
    let remedy = status.remedy.as_deref();
    let state = match status.state {
        ChannelState::Disabled => state(Tone::Neutral, "off", remedy),
        ChannelState::TokenMissing => state(Tone::Attention, "needs its token", remedy),
        ChannelState::Connecting => state(Tone::Neutral, "connecting", None),
        ChannelState::Connected => state(Tone::Good, "connected", None),
        // With Ophio stopped, an enabled connector with a token waits for the next start.
        ChannelState::PlatformError if !live => state(Tone::Neutral, "starts with Øphio", remedy),
        ChannelState::Conflict | ChannelState::PlatformError => {
            state(Tone::Attention, "not working", remedy)
        }
    };
    let label = match status.channel {
        ChannelKind::Telegram => "Telegram",
        ChannelKind::Discord => "Discord",
    };
    row(label, state)
}
fn channel_rows(frontdoors: &FrontdoorStatus) -> Vec<Row> {
    frontdoors
        .channels
        .iter()
        .map(|c| channel(c, frontdoors.applied_live))
        .collect()
}
fn inbox_rows(inbox: &InboxStatus) -> Vec<Row> {
    let mut rows = vec![row(
        "Received files",
        match &inbox.folder {
            Some(folder) => state(Tone::Neutral, folder.display().to_string(), None),
            None => state(Tone::Attention, "no folder", inbox.remedy),
        },
    )];
    if let Some(old) = &inbox.legacy_folder {
        rows.push(row(
            "Older received files",
            state(Tone::Neutral, old.display().to_string(), None),
        ));
    }
    rows
}

/// `ophio status` while Ophio runs.
pub fn status(
    snapshot: &Snapshot,
    harnesses: &[HarnessInfo],
    frontdoors: &FrontdoorStatus,
    inbox_status: &InboxStatus,
    style: Style,
) -> String {
    let host = &snapshot.host;
    let mut rows = vec![
        row("Desktop view", desktop_state(&host.capabilities.capture)),
        row("Desktop control", desktop_state(&host.capabilities.input)),
        row("Terminal", availability(&host.capabilities.terminal)),
        row("Voice", voice_state(&frontdoors.voice)),
    ];
    rows.extend(
        harnesses
            .iter()
            .filter(|h| h.integration != IntegrationLevel::NotIntegrated)
            .map(|h| row(&h.display_name, harness(h))),
    );
    rows.extend(channel_rows(frontdoors));
    rows.push(row(
        "Projects",
        match snapshot.projects.len() {
            0 => state(
                Tone::Neutral,
                "none yet",
                Some(&*format!("{COMMAND} projects add <PATH>")),
            ),
            n => state(Tone::Neutral, format!("{n} registered"), None),
        },
    ));
    rows.push(row(
        "Library folders",
        match snapshot.library_roots.len() {
            0 => state(Tone::Neutral, "none shared", None),
            n => state(Tone::Neutral, format!("{n} shared"), None),
        },
    ));
    rows.extend(inbox_rows(inbox_status));
    format!(
        "{PRODUCT_NAME} {} is running on {} ({}).\n\n{}\nRun `{COMMAND} doctor` for every check and what to do about it.\n",
        shown(&host.companion_version),
        shown(&host.name),
        shown(&host.os_label),
        table(&rows, "", style)
    )
}

/// `ophio status` while Ophio is stopped.
pub fn stopped(inbox_status: &InboxStatus, style: Style) -> String {
    format!(
        "{}\n\n{}",
        crate::Error::NotRunning,
        table(&inbox_rows(inbox_status), "", style)
    )
}

/// `ophio doctor`: every area, then one line on what needs attention.
pub fn doctor(
    report: &ReadinessReport,
    frontdoors: &FrontdoorStatus,
    inbox_status: &InboxStatus,
    style: Style,
) -> String {
    let (mut computer, mut agents) = (Vec::new(), Vec::new());
    for c in &report.checks {
        match c.area {
            // The front doors also name the speech model and say how each chat connector is doing.
            ReadinessArea::Speech => computer.push(row("Voice", voice_state(&frontdoors.voice))),
            ReadinessArea::Channels => {}
            // Each agent's installation and sign-in, and any other check that needs something.
            ReadinessArea::Accounts
                if c.label.ends_with(" installation")
                    || c.label.ends_with(" sign-in")
                    || c.status == CheckStatus::Failed
                    || (c.status == CheckStatus::NotRun && c.remedy.is_some()) =>
            {
                agents.push(row(&c.label, check(c)))
            }
            ReadinessArea::Accounts => {}
            _ => computer.push(row(&c.label, check(c))),
        }
    }
    computer.extend(inbox_rows(inbox_status));
    let sections = [
        ("Computer", computer),
        ("Agents", agents),
        ("Chat connectors", channel_rows(frontdoors)),
    ];
    let mut out = String::new();
    for (heading, rows) in sections.iter().filter(|(_, rows)| !rows.is_empty()) {
        let _ = writeln!(out, "{heading}\n{}", table(rows, "  ", style));
    }
    let labels = |tone: Tone| -> Vec<&str> {
        sections
            .iter()
            .flat_map(|(_, rows)| rows)
            .filter(|r| r.state.tone == tone)
            .map(|r| r.label.as_str())
            .collect()
    };
    let (attention, setup) = (labels(Tone::Attention), labels(Tone::Setup));
    let _ = writeln!(
        out,
        "Run `{COMMAND} doctor --json` for every check, including those not shown."
    );
    match attention.as_slice() {
        [] if setup.is_empty() => out.push_str("Nothing needs attention.\n"),
        [] => out.push_str("Nothing is broken.\n"),
        [one] => {
            let _ = writeln!(out, "1 thing needs attention: {one}.");
        }
        many => {
            let _ = writeln!(
                out,
                "{} things need attention: {}.",
                many.len(),
                many.join(", ")
            );
        }
    }
    if !setup.is_empty() {
        let _ = writeln!(out, "Still to set up: {}.", setup.join(", "));
    }
    out
}

/// Every `ophio channels` command: each connector's state.
pub fn channels(frontdoors: &FrontdoorStatus, style: Style) -> String {
    table(&channel_rows(frontdoors), "", style)
}

/// `ophio inbox` and `ophio inbox set`.
pub fn inbox(inbox_status: &InboxStatus, style: Style) -> String {
    table(&inbox_rows(inbox_status), "", style)
}

/// Every `ophio voice` command: whether voice input is ready.
pub fn voice(status: &VoiceStatus, style: Style) -> String {
    table(&[row("Voice", voice_state(status))], "", style)
}

/// Every `ophio desktop` command. The view's state matters only while access is on.
pub fn desktop(enabled: bool, view: &Availability, style: Style) -> String {
    let mut rows = vec![row(
        "Desktop access",
        state(Tone::Neutral, if enabled { "on" } else { "off" }, None),
    )];
    if enabled {
        rows.push(row("Desktop view", desktop_state(view)));
    }
    table(&rows, "", style)
}

/// `ophio retention set`.
pub fn retention(days: u16) -> String {
    let unit = if days == 1 { "day" } else { "days" };
    format!("Event history is kept for {days} {unit}.\n")
}

/// Every `ophio roots` command: each shared folder, its settings and its ID.
pub fn roots(roots: &[LibraryRootConfig]) -> String {
    if roots.is_empty() {
        return format!("No folders are shared. Run `{COMMAND} roots add <PATH>` to share one.\n");
    }
    let mut out = String::new();
    for root in roots {
        // The same words `ophio roots set` takes.
        let mut settings = vec![
            if root.writable {
                "Writable"
            } else {
                "Read-only"
            }
            .to_owned(),
            format!("kind {}", name(root.kind)),
        ];
        if let Some(project) = root.project_id {
            settings.push(format!("project {project}"));
        }
        if let Some(harness) = root.harness {
            settings.push(format!("agent {}", name(harness)));
        }
        let _ = writeln!(
            out,
            "{}: {}\n  {}.\n  ID {}",
            shown(&root.label),
            shown(&root.path.display().to_string()),
            settings.join(", "),
            root.root_id
        );
    }
    out
}
/// A setting's name as the command line accepts it.
fn name(value: impl serde::Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
}

/// `ophio projects list`.
pub fn projects(projects: &[Project]) -> String {
    if projects.is_empty() {
        return format!("No projects yet. Run `{COMMAND} projects add <PATH>` to add one.\n");
    }
    let mut out = String::new();
    for project in projects {
        let _ = writeln!(
            out,
            "{}: {}\n  ID {}",
            shown(&project.name),
            shown(&project.host_path),
            project.project_id
        );
    }
    out
}

/// `ophio projects add`.
pub fn project_added(project: &Project) -> String {
    format!(
        "Added {} at {}.\nID {}\n",
        shown(&project.name),
        shown(&project.host_path),
        project.project_id
    )
}

/// `ophio projects settings`.
pub fn project_settings(project: &Project, apply: bool) -> String {
    if apply {
        format!(
            "Work runs in {} now use the Claude Code settings in its folder, including any hooks and allowed tools.\n",
            shown(&project.name)
        )
    } else {
        format!(
            "Work runs in {} now ignore the Claude Code settings in its folder. Its CLAUDE.md instructions still apply.\n",
            shown(&project.name)
        )
    }
}

/// Every `ophio jev` command. The key itself is never part of the answer.
pub fn jev(access: &::jev::Access, now: Timestamp, style: Style) -> String {
    let connect = format!("{COMMAND} jev connect");
    let jev = match access.connection {
        ::jev::Connection::Connected => state(Tone::Good, "connected", None),
        ::jev::Connection::NotConnected => state(Tone::Neutral, "not connected", Some(&*connect)),
        ::jev::Connection::LastCheckFailed => state(
            Tone::Attention,
            "last check failed",
            Some(&*format!(
                "Check your TypeSafe access, or run `{connect}` to replace the key."
            )),
        ),
    };
    let mut rows = vec![row("Jev access", jev)];
    if let Some(at) = access.checked_at {
        rows.push(row("Last check", state(Tone::Neutral, ago(at, now), None)));
    }
    table(&rows, "", style)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colour_only_in_a_terminal_that_wants_it() {
        assert!(wants_colour(true, false, Some("xterm-256color")));
        assert!(wants_colour(true, false, None));
        assert!(!wants_colour(false, false, Some("xterm-256color")));
        assert!(!wants_colour(true, true, Some("xterm-256color")));
        assert!(!wants_colour(true, false, Some("dumb")));
    }
    #[test]
    fn colour_marks_only_the_state_words() {
        let colour = Style { colour: true };
        let rows = [
            row("Terminal", state(Tone::Good, "ready", None)),
            row("Input", state(Tone::Attention, "not ready", Some("Locked"))),
            row("Voice", state(Tone::Neutral, "needs setup", None)),
        ];
        assert_eq!(
            table(&rows, "", colour),
            "Terminal  \x1b[32mready\x1b[0m\nInput     \x1b[33mnot ready\x1b[0m\n          Locked\nVoice     needs setup\n"
        );
        assert!(!table(&rows, "", Style::PLAIN).contains('\x1b'));
    }
    #[test]
    fn ended_desktop_sharing_says_how_to_start_it_again() {
        assert_eq!(
            desktop(true, &Availability::Revoked, Style::PLAIN),
            format!(
                "Desktop access  on\nDesktop view    access revoked\n                Run `{COMMAND} desktop enable`.\n"
            )
        );
    }

    #[test]
    fn stored_text_cannot_rewrite_what_is_printed() {
        assert_eq!(
            shown("Phone\x1b[8m\r\nPixel\u{9b}\u{202e}9\u{2028}"),
            "Phone\\u{1b}[8m\\r\\nPixel\\u{9b}\\u{202e}9\\u{2028}"
        );
        assert_eq!(shown("Café — Pixel 9\\n"), "Café — Pixel 9\\n");
        // A readiness check as `ophio doctor` shows it, and a project as listed.
        let check = ReadinessCheck {
            area: ReadinessArea::Accounts,
            label: "Claude\x1b[2K sign-in".into(),
            status: CheckStatus::Failed,
            detail: Some("expired\x1b[1A\nAll checks passed".into()),
            remedy: None,
            summary: None,
        };
        let text = table(&[row(&check.label, super::check(&check))], "", Style::PLAIN);
        assert_eq!(
            text,
            "Claude\\u{1b}[2K sign-in  not ready\n                         expired\\u{1b}[1A\\nAll checks passed\n"
        );
        let project = Project {
            project_id: protocol::ProjectId::new(),
            name: "site\x1b]0;owned\x07".into(),
            host_path: "/work/site\n".into(),
            default_harness: None,
            default_model: None,
            default_effort: None,
            active_writer: None,
            vcs: None,
            created_at: Timestamp(0),
            stage: None,
            one_off: false,
            home: false,
        };
        for text in [
            projects(std::slice::from_ref(&project)),
            project_added(&project),
        ] {
            assert!(text.contains("site\\u{1b}]0;owned\\u{7}"), "{text}");
            assert!(text.contains("/work/site\\n"), "{text}");
            assert!(!text.contains('\x1b') && !text.contains('\x07'), "{text}");
        }
        for apply in [true, false] {
            let text = project_settings(&project, apply);
            assert!(text.contains("site\\u{1b}]0;owned\\u{7}"), "{text}");
            assert!(!text.contains('\x1b') && !text.contains('\x07'), "{text}");
        }
    }
}
