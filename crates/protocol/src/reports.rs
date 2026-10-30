// SPDX-License-Identifier: Apache-2.0
//! Problem reports: what the app sends when something goes wrong, what the
//! person hears back, and the private intake both talk to.
//!
//! The app writes the report and the person adds intent. Everything but the
//! person's own words is structured: codes, states, timings and versions,
//! never chat text, code, file contents, paths, project or task names,
//! desktop pictures or credentials. Each device redacts its own part before
//! the person sees "See all", so what they see is what is sent.
//!
//! A test submission ([`ReportTest`]) is the one exception: code the
//! reporter's own model wrote to show the bug, against the source of their
//! build. It is untrusted code, only ever run contained.
//!
//! The route is phone → computer → intake. The phone makes the report and
//! asks its computer for the computer's part ([`Query::ReportPreview`]),
//! shows both, and sends the whole with [`Command::SubmitReport`]. The
//! computer keeps it, queues it while the intake is unreachable, and sends
//! it signed with its reporter key. A phone with no reachable computer sends
//! straight to the intake with its own reporter key; pairing and connection
//! problems are exactly when no computer answers. Neither ever sends a report
//! anywhere public.
//!
//! What comes back is never free text. Status is a [`ReportProgress`] and a
//! question is a [`QuestionTemplate`] the phone words itself, so whoever
//! works on a report, person or agent, can't write to the reporter.
//!
//! [`Query::ReportPreview`]: crate::Query::ReportPreview
//! [`Command::SubmitReport`]: crate::Command::SubmitReport
use crate::{
    ReportId, ReportQuestionId, capability::HarnessKind, notifications::AlertTopic, time::Timestamp,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The bundle layout this module describes.
pub const REPORT_SCHEMA: u32 = 1;
/// Longest serialized [`ReportBundle`], in bytes, so a report fits one control
/// frame with room to spare.
pub const MAX_REPORT_BYTES: usize = 900 * 1024;
/// Largest picture of Ophio's own screen, in bytes before encoding.
pub const MAX_SCREENSHOT_BYTES: usize = 512 * 1024;
/// Longest thing the person writes or says, in characters.
pub const MAX_REPORT_WORDS: usize = 2000;
/// The most events either trail carries. The phone keeps about the last ten
/// minutes, the computer about the last thirty.
pub const MAX_TRAIL_EVENTS: usize = 400;
/// The phone's trail reaches back this far.
pub const PHONE_TRAIL_MS: i64 = 10 * 60 * 1000;
/// The computer's trail reaches back this far.
pub const COMPUTER_TRAIL_MS: i64 = 30 * 60 * 1000;
/// Longest code, route, name or other identifier a report carries.
pub const MAX_REPORT_CODE: usize = 160;
/// The most stack frames an error carries.
pub const MAX_FRAMES: usize = 40;
/// This file's path as the compiler named it, after `--remap-path-prefix`.
/// Release builds remap the checkout, its staged copy and the target folder,
/// so this is the neutral prefix this crate's panic locations already carry.
/// A reporter strips that prefix from stack frames. `CARGO_MANIFEST_DIR` is
/// never remapped and would compile the builder's real folder into the program.
pub const SOURCE_FILE: &str = file!();
/// The most reports `Query::Reports` lists, newest first.
pub const MAX_LISTED_REPORTS: usize = 200;
/// The most report ids one intake status request names.
pub const MAX_STATUS_IDS: usize = 200;

/// How a report's short reference is spelled: Crockford's base32.
const REFERENCE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The short reference the person reads, such as `A7K3QX`: the low 30 bits
/// of the id's last four bytes, in six characters of Crockford's base32. The
/// phone spells it the same way, so it can say "Got it, report A7K3QX"
/// before anything else answers.
pub fn report_reference(id: &ReportId) -> String {
    let b = id.as_uuid().as_bytes();
    let n = u32::from_be_bytes([b[12], b[13], b[14], b[15]]) & 0x3fff_ffff;
    (0..6)
        .rev()
        .map(|i| REFERENCE_ALPHABET[((n >> (5 * i)) & 31) as usize] as char)
        .collect()
}

/// What an id names, as its pseudonym's prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PseudonymKind {
    Project,
    Task,
    Run,
    Approval,
    Device,
    Command,
    Attention,
    /// Any other id.
    Other,
}

impl PseudonymKind {
    pub fn prefix(self) -> &'static str {
        match self {
            PseudonymKind::Project => "p",
            PseudonymKind::Task => "t",
            PseudonymKind::Run => "r",
            PseudonymKind::Approval => "a",
            PseudonymKind::Device => "d",
            PseudonymKind::Command => "c",
            PseudonymKind::Attention => "n",
            PseudonymKind::Other => "x",
        }
    }
}

/// An id as one report names it: the kind's prefix, a colon and the first
/// eight hex digits of SHA-256 over `salt`, a colon and the id as lowercase
/// text, such as `t:3f9a1c2b`. The phone mints `salt` (32 hex digits) for
/// each report and gives it to its computer with the preview, so both parts
/// name the same task alike. The salt never leaves the person's devices, so
/// the intake can't tell which task or project a report is about.
pub fn pseudonym(salt: &str, kind: PseudonymKind, id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(salt.as_bytes());
    hash.update(b":");
    hash.update(id.to_ascii_lowercase().as_bytes());
    let digest = hash.finalize();
    let hex: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("{}:{hex}", kind.prefix())
}

/// One problem report, whole, as the intake receives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportBundle {
    /// [`REPORT_SCHEMA`].
    pub schema: u32,
    /// Minted by the device that made the report; the intake keeps the first
    /// it receives under an id and answers a resend as received.
    pub report_id: ReportId,
    pub created_at: Timestamp,
    pub origin: ReportOrigin,
    /// The person's own words, typed or spoken, at most
    /// [`MAX_REPORT_WORDS`] characters. The only free text the app sends
    /// unasked, and the only place a report can carry instructions: whatever
    /// reads it treats it as data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub words: Option<String>,
    pub signature: ReportSignature,
    pub versions: ReportVersions,
    pub context: ReportContext,
    /// What the phone did, oldest first.
    #[serde(default)]
    pub phone_trail: Vec<TrailEvent>,
    /// What the computer did, oldest first. Empty in a report the phone sent
    /// straight to the intake.
    #[serde(default)]
    pub computer_trail: Vec<TrailEvent>,
    /// The phone's store with every id a pseudonym and every text its length,
    /// for the simulator to load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub state: Option<StateShape>,
    /// Crashes recorded since the last report, newest first.
    #[serde(default)]
    pub crashes: Vec<CrashRecord>,
    /// A picture of Ophio's own screen, perhaps marked up. Never the
    /// desktop or another app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub screenshot: Option<ReportScreenshot>,
    /// What the person chose to add beyond the default, each of which they
    /// saw before sending.
    #[serde(default)]
    pub extras: Vec<ReportExtraPart>,
    /// A failing test the reporter's own model wrote against the source of
    /// their build, which makes this a test submission. Its words, trails
    /// and extras may all be empty. Everything in it is the reporter's claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub test: Option<ReportTest>,
}

/// Longest submitted test file, in bytes.
pub const MAX_TEST_BYTES: usize = 8 * 1024;
/// Longest suggested fix, in bytes.
pub const MAX_PATCH_BYTES: usize = 32 * 1024;
/// Longest path a submitted test may name, in bytes.
pub const MAX_TEST_PATH: usize = 240;

/// A test that should show the bug alone: it fails with an assertion at the
/// reporter's build and is meant to pass once the bug is fixed.
///
/// Every field is the reporter's claim, and nothing attests to it: the
/// investigator is open source and runs on their computer. The test is
/// untrusted code. It only ever runs contained, offline and without
/// credentials, and only the code that checks it decides what it shows.
/// The suggested fix is a reference for the owner alone: it is never
/// applied, never run and never shown to an agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportTest {
    /// The full commit the reporter's build was made from, 40 lowercase hex
    /// digits: the companion's `OPHIO_SOURCE_COMMIT`. Their claim.
    pub source_commit: String,
    pub kind: ReportTestKind,
    /// Where the new test file goes, following [`valid_test_path`].
    pub path: String,
    /// The test file, UTF-8, at most [`MAX_TEST_BYTES`]. Untrusted code.
    pub source: String,
    /// Their suggested fix, as a unified diff of at most
    /// [`MAX_PATCH_BYTES`]. Shown to the owner as written by the reporter's
    /// model and not checked; never applied, never run and never shown to
    /// an agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub patch: Option<String>,
    /// Who the reporter says wrote it.
    pub investigator: ReportInvestigator,
}

/// The test and optional suggested fix for an owner's inbox report. These
/// are untrusted reporter-supplied bytes, not instructions to execute or apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct InboxReportCode {
    pub kind: ReportTestKind,
    pub path: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub patch: Option<String>,
}

/// The program and model the reporter says wrote a test. Their claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportInvestigator {
    pub harness: HarnessKind,
    /// The model's name as the reporter gives it, at most
    /// [`MAX_REPORT_CODE`] characters with no control characters.
    pub model: String,
}

/// What kind of test a submission or repro is, which decides where it may
/// go and how it runs.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReportTestKind {
    /// A new file under `crates/<crate>/tests/`.
    RustIntegration,
    /// A new `*_tests.rs` file under `crates/<crate>/src/`.
    RustUnit,
    /// A new `*.test.ts` or `*.test.tsx` file under `client/src/`.
    Vitest,
}

/// Which rule a submitted test breaks, before anything looks at the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportTestError {
    Commit,
    Path,
    Source,
    Patch,
    Model,
}

impl ReportTest {
    /// The string rules a submitted test must meet: the commit's form, the
    /// path's shape for its kind, sizes and characters. Whether the path is
    /// free and its folder exists at that commit depends on the tree, and is
    /// checked where the tree is.
    pub fn check(&self) -> Result<(), ReportTestError> {
        if !valid_source_commit(&self.source_commit) {
            return Err(ReportTestError::Commit);
        }
        if !valid_test_path(self.kind, &self.path) {
            return Err(ReportTestError::Path);
        }
        if !valid_test_source(&self.source) {
            return Err(ReportTestError::Source);
        }
        if self
            .patch
            .as_ref()
            .is_some_and(|p| p.is_empty() || p.len() > MAX_PATCH_BYTES || !safe_code_text(p))
        {
            return Err(ReportTestError::Patch);
        }
        let model = &self.investigator.model;
        if model.is_empty()
            || model.chars().count() > MAX_REPORT_CODE
            || model.chars().any(char::is_control)
        {
            return Err(ReportTestError::Model);
        }
        Ok(())
    }
}

/// A full commit id: exactly 40 lowercase hex digits.
pub fn valid_source_commit(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Where a test of `kind` may go: a relative path of at most
/// [`MAX_TEST_PATH`] bytes whose segments are ASCII letters, digits, `_`,
/// `-` and `.`, none empty or starting with a dot, in the folder its kind
/// runs from.
pub fn valid_test_path(kind: ReportTestKind, path: &str) -> bool {
    if path.len() > MAX_TEST_PATH
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == ".." || s.starts_with('.'))
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
    {
        return false;
    }
    let parts: Vec<_> = path.split('/').collect();
    match kind {
        ReportTestKind::RustIntegration => {
            parts.len() >= 4 && parts[0] == "crates" && parts[2] == "tests" && path.ends_with(".rs")
        }
        ReportTestKind::RustUnit => {
            parts.len() >= 4
                && parts[0] == "crates"
                && parts[2] == "src"
                && path.ends_with("_tests.rs")
        }
        ReportTestKind::Vitest => {
            parts.len() >= 4
                && path.starts_with("client/src/")
                && (path.ends_with(".test.ts") || path.ends_with(".test.tsx"))
        }
    }
}

/// A test file's text: not empty, at most [`MAX_TEST_BYTES`], and
/// [`safe_code_text`].
pub fn valid_test_source(source: &str) -> bool {
    !source.is_empty() && source.len() <= MAX_TEST_BYTES && safe_code_text(source)
}

/// Whether code is free of NUL, bidirectional controls and invisible format
/// characters (zero-width characters, tags, variation selectors and the
/// like), which can make code read other than it runs or carry text nobody
/// sees into a prompt or a pull request.
pub fn safe_code_text(text: &str) -> bool {
    !text.chars().any(|c| c == '\0' || invisible(c))
}

/// Unicode format (Cf) characters, the bidirectional controls among them,
/// and variation selectors.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}'
        | '\u{0890}'..='\u{0891}' | '\u{08E2}' | '\u{180E}' | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}'
        | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}' | '\u{110BD}'
        | '\u{110CD}' | '\u{13430}'..='\u{1343F}' | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{1D173}'..='\u{1D17A}' | '\u{E0001}' | '\u{E0020}'..='\u{E007F}'
        | '\u{E0100}'..='\u{E01EF}')
}

/// What started the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ReportOrigin {
    /// Report on an error notice, refused command or failed state.
    Error,
    /// "Ophio closed unexpectedly. Send what happened?" on the next start.
    Crash,
    /// The chip that follows a screenshot taken in Ophio.
    Screenshot,
    /// Settings › Report a problem.
    Settings,
    /// A triage question's choice to send a fresh report.
    FollowUp { report_id: ReportId },
}

/// What known issues are matched on: the error's code, the screen and the
/// build. The intake answers a match at once, such as "Known, fixed in build
/// X. Update now", with no agent involved.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct ReportSignature {
    /// An [`ErrorCode`](crate::error::ErrorCode) in its wire spelling, a crash's code (see
    /// [`CrashRecord::code`]), or `none` for a report on no error.
    pub code: String,
    /// The route pattern the person was on, such as `/work/:task`.
    pub screen: String,
    /// The build the report came from: the phone's for a phone error, the
    /// computer's for a computer one.
    pub build: String,
}

impl ReportSignature {
    /// The part that names the problem rather than the build: what known
    /// issues and the release feed's `fixes` list.
    pub fn key(&self) -> String {
        format!("{}@{}", self.code, self.screen)
    }

    /// Whether reports sharing this signature are one problem. A report on no
    /// error is told apart only by its words and picture, so it never groups.
    pub fn groups(&self) -> bool {
        self.code != NO_ERROR_CODE
    }
}

/// The signature code of a report on no error.
pub const NO_ERROR_CODE: &str = "none";

/// Every version that can bear on a report.
#[derive(
    Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct ReportVersions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub phone: Option<PhoneVersions>,
    /// Absent in a report the phone sent straight to the intake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub computer: Option<ComputerVersions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct PhoneVersions {
    /// The app's build id.
    pub build: String,
    pub platform: ReportPlatform,
    /// Such as `Android 15 (API 35)` or `iOS 18.2`.
    pub os: String,
    /// The phone's make and model, such as `Google Pixel 8`.
    pub model: String,
    pub protocol: crate::ProtocolVersion,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReportPlatform {
    Android,
    Ios,
    /// The browser client, including the simulator.
    Web,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ComputerVersions {
    /// The companion's build id.
    pub build: String,
    /// Such as `Arch Linux (kernel 7.2.7)`.
    pub os: String,
    pub protocol: crate::ProtocolVersion,
    /// The features the computer advertises.
    #[serde(default)]
    pub features: Vec<String>,
    /// Each agent program's version, as the computer runs it.
    #[serde(default)]
    pub harnesses: Vec<HarnessVersion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct HarnessVersion {
    pub harness: HarnessKind,
    /// None when the program isn't installed or didn't say.
    pub version: Option<String>,
}

/// Where the person was and what had just gone wrong.
#[derive(
    Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct ReportContext {
    /// The route pattern, as in [`ReportSignature::screen`].
    pub screen: String,
    /// The task on screen, as a pseudonym.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub harness: Option<HarnessKind>,
    /// The model the task runs, such as `claude-opus-5-5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// The command the report is on, by its wire name, such as `start_task`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<String>,
    /// The error the report is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<ReportedError>,
}

/// An error as a report carries it: its code and a scrubbed message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportedError {
    /// As in [`ReportSignature::code`].
    pub code: String,
    /// The message with quoted text, paths, URLs, addresses and long
    /// numbers replaced (see [`scrub`]), at most [`MAX_REPORT_CODE`]
    /// characters. None when nothing safe was left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
}

/// One thing that happened, on the phone or the computer. Ids are
/// pseudonyms; there is no free text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TrailEvent {
    pub at: Timestamp,
    pub event: Trail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum Trail {
    // The phone's.
    /// A route opened, by its pattern.
    Route { route: String },
    /// A command sent, how it ended and how long it took.
    Command {
        command: String,
        outcome: CommandOutcome,
        /// The error code of a refusal or failure, in its wire spelling.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        code: Option<String>,
        ms: u32,
    },
    /// A notice shown, by its key in the client's source or its error code.
    Notice { key: String, tone: NoticeTone },
    /// The phone's link to its computer changed.
    Connection { state: LinkState },
    /// The app went to the background or came back.
    Lifecycle { state: LifecycleState },
    /// An alert reached the phone, and whether it showed.
    AlertReceived {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        topic: Option<AlertTopic>,
        quiet: bool,
    },
    // The computer's.
    /// A task changed state, by the state's wire name.
    TaskState {
        task: String,
        state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        harness: Option<HarnessKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        model: Option<String>,
    },
    /// A run ended: how, and with what code.
    RunExit {
        task: String,
        run: String,
        exit: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        code: Option<String>,
    },
    /// The sandbox refused a network host or a path. Neither is named.
    SandboxBlock { task: String, kind: BlockKind },
    /// An approval asked, or answered and how.
    Approval {
        task: String,
        approval: String,
        step: ApprovalStep,
    },
    /// An alert the computer raised, and whether it was quiet.
    AlertRaised {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        topic: Option<AlertTopic>,
        quiet: bool,
    },
    /// A GitHub call by its operation, such as `pr_merge`, and its HTTP status.
    Github { operation: String, status: u16 },
    /// An update installed, refused or rolled back.
    Update {
        from: String,
        to: String,
        outcome: UpdateOutcome,
    },
    /// The companion started or stopped.
    Service { state: ServiceState },
    // Either.
    /// An error that reached no notice, such as a failed background request.
    Error { error: ReportedError },
    /// A crash; the full record is in [`ReportBundle::crashes`].
    Crash { code: String },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CommandOutcome {
    Accepted,
    Refused,
    Failed,
    /// No answer came before the phone gave up waiting.
    TimedOut,
    /// Still waiting when the report was made.
    Pending,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NoticeTone {
    Info,
    Error,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LinkState {
    Direct,
    Relay,
    Reconnecting,
    Offline,
    Unpaired,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LifecycleState {
    Foreground,
    Background,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BlockKind {
    Network,
    Path,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalStep {
    Asked,
    Allowed,
    Denied,
    Answered,
    Expired,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum UpdateOutcome {
    Installed,
    Refused,
    RolledBack,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ServiceState {
    Started,
    Stopping,
}

/// The phone's store as the simulator loads it. Every id is a pseudonym and
/// every string that isn't an id, an enum's wire name, a route pattern or a
/// model name is `~<kind>:<length>`, where kind is `text`, `url` or `path`.
/// Numbers, booleans and the store's shape stay as they are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct StateShape {
    /// The client's own layout version, so the simulator knows what it loads.
    pub version: u32,
    #[ts(type = "unknown")]
    pub store: serde_json::Value,
}

/// A crash, as it was recorded on the device it happened on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CrashRecord {
    pub at: Timestamp,
    pub side: ReportSide,
    pub kind: CrashKind,
    /// What the crash is matched on: `panic:<file>:<line>` for a Rust panic,
    /// `js:<name>@<function>` for a JavaScript error, `exit:<reason>` for
    /// Android's last-exit reason and `metric:<type>` for MetricKit.
    pub code: String,
    /// The build that crashed.
    pub build: String,
    /// As in [`ReportedError::message`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
    /// Innermost first, at most [`MAX_FRAMES`].
    #[serde(default)]
    pub frames: Vec<StackFrame>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReportSide {
    Phone,
    Computer,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CrashKind {
    /// An uncaught error or rejected promise in the app's JavaScript.
    Script,
    /// A render error the app's error boundary caught.
    Render,
    /// A Rust panic, on the phone or the computer.
    Panic,
    /// The phone's system ended the app: Android's exit reasons or MetricKit.
    System,
}

/// One frame of a stack: a function in Ophio's own code and where it is.
/// Frames outside Ophio's code are left out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct StackFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub function: Option<String>,
    /// A source file relative to Ophio's repository, or a bundled asset's
    /// name such as `index-3fa9.js`; never a path on the device.
    pub file: String,
    pub line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub column: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportScreenshot {
    /// `image/jpeg`, `image/webp` or `image/png`.
    pub media_type: String,
    /// Base64, standard alphabet with padding, of at most
    /// [`MAX_SCREENSHOT_BYTES`].
    pub data: String,
    pub width: u32,
    pub height: u32,
    /// The person marked it up.
    #[serde(default)]
    pub marked: bool,
}

/// Something the person may add beyond the default, off unless they turn it
/// on under "See all".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ReportExtra {
    /// The transcript of the task on screen, as the computer holds it.
    TaskTranscript { task_id: crate::TaskId },
    /// The task's and project's real names, in place of pseudonyms.
    Names,
}

/// An extra as it is sent. Its text is the person's and their agents', and
/// is treated as data by whatever reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ReportExtraPart {
    TaskTranscript {
        task: String,
        /// Oldest first, cut to fit [`MAX_REPORT_BYTES`].
        entries: Vec<TranscriptEntry>,
        /// Older entries were left out.
        cut: bool,
    },
    Names {
        names: Vec<NamedPseudonym>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TranscriptEntry {
    pub at: Timestamp,
    /// `person`, `agent`, `tool` or `system`.
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct NamedPseudonym {
    pub pseudonym: String,
    pub name: String,
}

/// The computer's part of a report, exactly as it will be sent: the person
/// sees this under "See all" before sending.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ComputerPart {
    pub versions: ComputerVersions,
    pub trail: Vec<TrailEvent>,
    /// Computer crashes since its last report, newest first.
    pub crashes: Vec<CrashRecord>,
    pub extras: Vec<ReportExtraPart>,
    /// Whether the computer will send reports at all; the phone sends
    /// straight to the intake when it won't.
    pub sending: bool,
}

/// Where a report stands, as the reporter hears it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum ReportProgress {
    /// Kept on the person's device, waiting to reach the intake.
    Queued,
    /// The intake has it.
    Received,
    /// It matches a known problem, fixed in `fixed_in` when that's set.
    Known {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        fixed_in: Option<String>,
    },
    /// Waiting for the person to answer a question.
    Question,
    Reproduced,
    NotReproduced,
    FixWritten,
    InReview,
    /// A fix merged; it ships in `build`.
    Fixed {
        build: String,
    },
    /// A build with the fix is out, and the person can update to it.
    Shipped {
        build: String,
    },
    Declined {
        reason: DeclineReason,
    },
    /// The person took it back; the intake keeps nothing of it.
    Withdrawn,
}

impl ReportProgress {
    /// Whether moving to this status alerts the reporter. Only progress they
    /// would act on does: reproduced, fixed or shipped, or a question.
    pub fn alerts(&self) -> bool {
        matches!(
            self,
            ReportProgress::Question
                | ReportProgress::Reproduced
                | ReportProgress::Fixed { .. }
                | ReportProgress::Shipped { .. }
        )
    }

    /// Nothing further will happen to the report.
    pub fn closed(&self) -> bool {
        matches!(
            self,
            ReportProgress::Shipped { .. }
                | ReportProgress::Declined { .. }
                | ReportProgress::Withdrawn
        )
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DeclineReason {
    /// Ophio does what its documentation says; the report became a request.
    WorksAsDocumented,
    FeatureRequest,
    /// Another report is about the same problem; this one follows it.
    Duplicate,
    NotEnoughInformation,
    /// About something other than Ophio, such as an agent program itself.
    OutsideOphio,
}

/// What the owner can tell everyone who sent a report in their inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "verdict", rename_all = "snake_case")]
#[ts(export)]
pub enum ReportVerdict {
    Reproduced,
    NotReproduced,
    /// Fixed in the build the owner's computer runs.
    Fixed,
    Declined {
        reason: DeclineReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct StatusChange {
    pub status: ReportProgress,
    pub at: Timestamp,
}

/// A question about a report. The phone words it from its template, so
/// nobody working on the report writes to the reporter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportQuestion {
    pub question_id: ReportQuestionId,
    pub template: QuestionTemplate,
    pub asked_at: Timestamp,
    /// None until answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer: Option<ReportAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "template", rename_all = "snake_case")]
#[ts(export)]
pub enum QuestionTemplate {
    /// "Does it still happen in build {build}?" Yes, no or not sure.
    StillHappens { build: String },
    /// "Has it happened again since you reported it?" Yes, no or not sure.
    HappenedAgain,
    /// "Did it start after an update?" Yes, no or not sure.
    AfterUpdate,
    /// "Can we see {part}?" Allow or don't. Allowing sends the part as a
    /// follow-up report the person sees first.
    MayWeSee { part: ReportExtraKind },
    /// "Which comes closest to what you expected?" One of `options`.
    Expected { options: Vec<ExpectedOption> },
}

impl QuestionTemplate {
    /// The answers the phone offers.
    pub fn choices(&self) -> Vec<QuestionChoice> {
        use QuestionChoice::*;
        match self {
            QuestionTemplate::StillHappens { .. }
            | QuestionTemplate::HappenedAgain
            | QuestionTemplate::AfterUpdate => vec![Yes, No, NotSure],
            QuestionTemplate::MayWeSee { .. } => vec![Allow, Deny],
            QuestionTemplate::Expected { options } => options.iter().map(|o| Option(*o)).collect(),
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReportExtraKind {
    TaskTranscript,
    Names,
}

/// What the person might have expected, worded by the phone.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ExpectedOption {
    /// "It should have worked."
    ShouldWork,
    /// "It should have asked me first."
    ShouldAsk,
    /// "It shouldn't have notified me."
    ShouldNotNotify,
    /// "It should have been faster."
    ShouldBeFaster,
    /// "Something else."
    SomethingElse,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(tag = "choice", content = "option", rename_all = "snake_case")]
#[ts(export)]
pub enum QuestionChoice {
    Yes,
    No,
    NotSure,
    Allow,
    Deny,
    Option(ExpectedOption),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportAnswer {
    pub choice: QuestionChoice,
    /// A few words of the person's own, at most [`MAX_REPORT_WORDS`]
    /// characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub words: Option<String>,
    pub at: Timestamp,
}

/// One report in Settings › Your reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportSummary {
    pub report_id: ReportId,
    /// [`report_reference`].
    pub reference: String,
    pub created_at: Timestamp,
    pub origin: ReportOrigin,
    /// The person's words, cut to [`crate::alerts::MAX_ALERT_TEXT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub words: Option<String>,
    pub signature: ReportSignature,
    /// The latest last.
    pub history: Vec<StatusChange>,
    /// Open questions first.
    #[serde(default)]
    pub questions: Vec<ReportQuestion>,
    /// When the intake last answered about it; none before it reached the
    /// intake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checked_at: Option<Timestamp>,
}

impl ReportSummary {
    pub fn status(&self) -> ReportProgress {
        self.history
            .last()
            .map(|c| c.status.clone())
            .unwrap_or(ReportProgress::Queued)
    }
}

/// Answer to `Query::Reports`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportsView {
    /// Newest first, at most [`MAX_LISTED_REPORTS`].
    pub reports: Vec<ReportSummary>,
    pub settings: ReportSettings,
    pub intake: IntakeLink,
    /// The computer's crashes no report has carried yet, newest first, so
    /// the phone can offer to send what happened.
    #[serde(default)]
    pub crashes: Vec<CrashRecord>,
    /// A release the person's computer runs was halted: they can go back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub halted: Option<HaltedRelease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportSettings {
    /// The computer sends reports to the intake. On by default; reports made
    /// while off stay on the computer.
    pub sending: bool,
    /// Hear about fixed builds early. Never installs anything.
    pub ring: ReleaseRing,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    #[serde(default)]
    pub revision: u64,
}

impl Default for ReportSettings {
    fn default() -> Self {
        Self {
            sending: true,
            ring: ReleaseRing::Everyone,
            revision: 0,
        }
    }
}

/// How the computer reaches the intake now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum IntakeLink {
    /// No intake is set up on this computer.
    None,
    Reachable,
    Unreachable {
        since: Timestamp,
        /// Reports waiting to go.
        queued: u32,
    },
    /// The intake paused new reports.
    Paused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct HaltedRelease {
    pub build: String,
    /// The build `ophio update --rollback` returns to, when the computer
    /// still has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub back_to: Option<String>,
}

/// One report with what was sent, for "See what was sent".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportDetail {
    pub summary: ReportSummary,
    pub bundle: ReportBundle,
}

/// A report alert, carried in [`crate::alerts::Alert::report`] to a phone
/// whose `Watch` said `reports`. Its `project_id` and `task_id` are the nil
/// id, and its `project` and `task` are empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportAlert {
    pub report_id: ReportId,
    pub reference: String,
    pub status: ReportProgress,
    /// The question to answer from the notification, with
    /// `AnswerReportQuestion`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub question: Option<ReportQuestion>,
}

// The intake's HTTP interface. Every request from a reporter is signed: see
// `signing_payload`. Paths are relative to the intake's base URL.

/// Header carrying the reporter key: base64url Ed25519 public key, no padding.
pub const KEY_HEADER: &str = "ophio-report-key";
/// Header carrying Unix epoch milliseconds when the request was signed.
pub const TIME_HEADER: &str = "ophio-report-time";
/// Header carrying the base64url Ed25519 signature, no padding.
pub const SIGNATURE_HEADER: &str = "ophio-report-signature";
/// How far a request's time may be from the intake's clock.
pub const MAX_CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;

/// `POST`: a [`IntakeSubmission`], answered with [`IntakeReceipt`].
pub const PATH_SUBMIT: &str = "/v1/reports";
/// `POST`: a [`StatusRequest`], answered with [`StatusResponse`].
pub const PATH_STATUS: &str = "/v1/reports/status";
/// `POST`: an [`IntakeAnswer`], answered with [`StatusResponse`].
pub const PATH_ANSWER: &str = "/v1/reports/answer";
/// `POST`: a [`WithdrawRequest`], answered with [`StatusResponse`].
pub const PATH_WITHDRAW: &str = "/v1/reports/withdraw";
/// `GET`: the [`ReleaseFeed`]. Signed like the rest.
pub const PATH_RELEASES: &str = "/v1/releases";

/// What a reporter key signs: the request's method, path, time and the
/// SHA-256 of its body, each on its own line after a fixed first line.
pub fn signing_payload(method: &str, path: &str, time_ms: i64, body: &[u8]) -> Vec<u8> {
    let digest = Sha256::digest(body);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "ophio-report-v1\n{}\n{path}\n{time_ms}\n{hex}",
        method.to_ascii_uppercase()
    )
    .into_bytes()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct IntakeSubmission {
    pub bundle: ReportBundle,
    pub sender: ReportSide,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct IntakeReceipt {
    pub report_id: ReportId,
    pub reference: String,
    pub status: ReportProgress,
}

/// The reports this reporter key sent, and what changed since `since`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct StatusRequest {
    /// At most [`MAX_STATUS_IDS`].
    pub report_ids: Vec<ReportId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub since: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct StatusResponse {
    /// Only reports this key sent; an id it didn't send is left out.
    pub reports: Vec<ReportUpdate>,
    pub now: Timestamp,
    /// The intake takes no new reports for now.
    #[serde(default)]
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReportUpdate {
    pub report_id: ReportId,
    /// The whole history, oldest first.
    pub history: Vec<StatusChange>,
    pub questions: Vec<ReportQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct IntakeAnswer {
    pub report_id: ReportId,
    pub question_id: ReportQuestionId,
    pub answer: ReportAnswer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct WithdrawRequest {
    pub report_id: ReportId,
}

/// Which builds are out, to whom, and what they fixed. Ophio never downloads
/// a build itself: the feed tells computers with an open report, or that
/// asked to hear early, that a fix is out, and tells one running a halted
/// build that it can go back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReleaseFeed {
    /// Newest first.
    pub releases: Vec<ReleaseEntry>,
    pub now: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReleaseEntry {
    pub build: String,
    pub published_at: Timestamp,
    /// The widest ring offered it so far.
    pub ring: ReleaseRing,
    pub state: ReleaseState,
    /// The [`ReportSignature::key`]s it fixes.
    #[serde(default)]
    pub fixes: Vec<String>,
}

/// Who hears of a build, widening in this order.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReleaseRing {
    /// The owner's own computers and phones.
    Owner,
    /// People who asked to hear early.
    Early,
    Everyone,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReleaseState {
    /// Being offered ring by ring.
    Rolling,
    /// Stopped: crash reports from it passed the limit, or the owner halted
    /// it. Computers running it are told they can go back.
    Halted,
    /// Offered to everyone.
    Complete,
}

/// `text` with whatever might identify the person replaced, cut to
/// [`MAX_REPORT_CODE`] characters, or none when nothing is left. Quoted
/// spans become `"…"`, paths `<path>`, URLs `<url>`, email addresses
/// `<email>` and runs of five or more digits `<n>`. The phone does the same
/// in `client/src/diagnostics/scrub.ts`; both are checked against the same
/// cases.
pub fn scrub(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' | '`' | '“' | '‘' => {
                let close = match c {
                    '“' => '”',
                    '‘' => '’',
                    other => other,
                };
                // A lone apostrophe, as in "can't", isn't a quote.
                let rest: String = chars.clone().collect();
                if let Some(end) = rest.find(close) {
                    out.push_str("\"…\"");
                    for _ in 0..rest[..end].chars().count() + 1 {
                        chars.next();
                    }
                } else {
                    out.push(c);
                }
            }
            c if c.is_whitespace() => out.push(' '),
            _ => {
                let mut word = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() || matches!(n, '"' | '`' | '“' | '‘') {
                        break;
                    }
                    word.push(n);
                    chars.next();
                }
                out.push_str(&scrub_word(&word));
            }
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let out = crate::alerts::cut(&out, MAX_REPORT_CODE);
    (!out.is_empty()).then_some(out)
}

/// A build id as installed, such as a commit's short hash: kept whole when
/// it is a plain identifier, since scrubbing would hide its digit runs, and
/// scrubbed like any other text when it isn't.
pub fn build_id(text: &str) -> Option<String> {
    let plain = !text.is_empty()
        && text.chars().count() <= MAX_REPORT_CODE
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    if plain {
        Some(text.to_owned())
    } else {
        scrub(text)
    }
}

fn scrub_word(word: &str) -> String {
    let core = word.trim_end_matches(['.', ',', ';', ':', ')', ']']);
    let tail = &word[core.len()..];
    let lead = core.chars().take_while(|c| matches!(c, '(' | '[')).count();
    let (open, core) = core.split_at(lead);
    let replaced = if core.contains("://") {
        Some("<url>")
    } else if core.contains('@') && core.contains('.') && !core.starts_with('@') {
        Some("<email>")
    } else if core.starts_with('/')
        || core.starts_with("~/")
        || core.starts_with("./")
        || core.starts_with("../")
        || core.contains('\\')
        || (core.len() > 2 && core.as_bytes()[1] == b':' && core.as_bytes()[2] == b'\\')
        || core.matches('/').count() >= 2
    {
        Some("<path>")
    } else {
        None
    };
    let body = match replaced {
        Some(r) => r.to_owned(),
        None => digits(core),
    };
    format!("{open}{body}{tail}")
}

fn digits(word: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    for c in word.chars() {
        if c.is_ascii_digit() {
            run.push(c);
            continue;
        }
        out.push_str(if run.len() >= 5 { "<n>" } else { &run });
        run.clear();
        out.push(c);
    }
    out.push_str(if run.len() >= 5 { "<n>" } else { &run });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_are_six_crockford_characters_from_the_ids_last_bytes() {
        let id: ReportId = "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b".parse().unwrap();
        // 0x3e4f5a6b & 0x3fffffff = 0x3e4f5a6b → 30 bits in groups of five.
        assert_eq!(report_reference(&id), "Z4YPKB");
        let zero: ReportId = "00000000-0000-7000-8000-000000000000".parse().unwrap();
        assert_eq!(report_reference(&zero), "000000");
    }

    #[test]
    fn pseudonyms_depend_on_the_salt_and_ignore_case() {
        let id = "0199F1C2-3B4D-7E5F-8A6B-1C2D3E4F5A6B";
        let a = pseudonym("00112233445566778899aabbccddeeff", PseudonymKind::Task, id);
        let b = pseudonym(
            "00112233445566778899aabbccddeeff",
            PseudonymKind::Task,
            &id.to_lowercase(),
        );
        let c = pseudonym("ffeeddccbbaa99887766554433221100", PseudonymKind::Task, id);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("t:") && a.len() == 10);
        // The value the client's implementation is checked against.
        assert_eq!(a, "t:bc25fcec");
    }

    #[test]
    fn signing_payload_names_every_part() {
        let p = signing_payload("post", PATH_SUBMIT, 1_760_000_000_000, b"{}");
        assert_eq!(
            String::from_utf8(p).unwrap(),
            "ophio-report-v1\nPOST\n/v1/reports\n1760000000000\n\
             44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }

    #[test]
    fn scrub_removes_what_could_identify_someone() {
        let cases: Vec<(String, Option<String>)> =
            serde_json::from_str(include_str!("../tests/fixtures/report-scrub.json")).unwrap();
        for (input, want) in cases {
            assert_eq!(scrub(&input), want, "{input}");
        }
    }

    #[test]
    fn build_ids_keep_their_digits() {
        assert_eq!(build_id("4074f12345cd").as_deref(), Some("4074f12345cd"));
        assert_eq!(
            build_id("0.4.1+20261015").as_deref(),
            Some("0.4.1+20261015")
        );
        assert_eq!(build_id("/home/sam/build").as_deref(), Some("<path>"));
        assert_eq!(build_id("call 5551234567").as_deref(), Some("call <n>"));
        assert_eq!(build_id(""), None);
    }

    #[test]
    fn statuses_alert_only_on_progress_the_reporter_acts_on() {
        assert!(!ReportProgress::Received.alerts());
        assert!(!ReportProgress::InReview.alerts());
        assert!(ReportProgress::Reproduced.alerts());
        assert!(ReportProgress::Question.alerts());
        assert!(ReportProgress::Shipped { build: "b".into() }.alerts());
        let text = serde_json::to_string(&ReportProgress::Fixed {
            build: "abc".into(),
        })
        .unwrap();
        assert_eq!(text, r#"{"state":"fixed","build":"abc"}"#);
    }

    #[test]
    fn choices_follow_the_template() {
        let t = QuestionTemplate::Expected {
            options: vec![ExpectedOption::ShouldAsk],
        };
        assert_eq!(
            t.choices(),
            vec![QuestionChoice::Option(ExpectedOption::ShouldAsk)]
        );
        let text =
            serde_json::to_string(&QuestionChoice::Option(ExpectedOption::ShouldAsk)).unwrap();
        assert_eq!(text, r#"{"choice":"option","option":"should_ask"}"#);
        assert_eq!(
            serde_json::to_string(&QuestionChoice::Yes).unwrap(),
            r#"{"choice":"yes"}"#
        );
    }

    fn submitted() -> ReportTest {
        ReportTest {
            source_commit: "0123456789abcdef0123456789abcdef01234567".into(),
            kind: ReportTestKind::RustIntegration,
            path: "crates/store/tests/lost_write.rs".into(),
            source: "#[test]\nfn lost() { assert_eq!(1, 2); }\n".into(),
            patch: Some("--- a/x\n+++ b/x\n".into()),
            investigator: ReportInvestigator {
                harness: HarnessKind::Codex,
                model: "some-model".into(),
            },
        }
    }

    #[test]
    fn a_submitted_test_follows_the_shared_string_rules() {
        assert_eq!(submitted().check(), Ok(()));
        let with = |f: &dyn Fn(&mut ReportTest)| {
            let mut t = submitted();
            f(&mut t);
            t.check()
        };
        for commit in [
            "0123",
            "0123456789ABCDEF0123456789ABCDEF01234567",
            &"g".repeat(40),
        ] {
            assert_eq!(
                with(&|t| t.source_commit = commit.into()),
                Err(ReportTestError::Commit)
            );
        }
        for path in [
            "crates/store/tests/../src/lib.rs",
            "/crates/store/tests/a.rs",
            "crates/store/tests/.hidden/a.rs",
            "crates/store/tests//a.rs",
            "crates/store/src/a.rs",
            "client/src/a.test.ts",
            "crates/store/tests/a\u{202e}.rs",
            "crates/store/tests/a b.rs",
        ] {
            assert_eq!(
                with(&|t| t.path = path.into()),
                Err(ReportTestError::Path),
                "{path}"
            );
        }
        let long = format!("crates/store/tests/{}.rs", "a".repeat(MAX_TEST_PATH));
        assert_eq!(with(&|t| t.path = long.clone()), Err(ReportTestError::Path));
        assert!(valid_test_path(
            ReportTestKind::RustUnit,
            "crates/store/src/deep/lost_tests.rs"
        ));
        assert!(!valid_test_path(
            ReportTestKind::RustUnit,
            "crates/store/src/lost.rs"
        ));
        assert!(valid_test_path(
            ReportTestKind::Vitest,
            "client/src/state/lost.test.tsx"
        ));
        assert!(!valid_test_path(
            ReportTestKind::Vitest,
            "client/scripts/lost.test.ts"
        ));
        assert_eq!(
            with(&|t| t.source = "x".repeat(MAX_TEST_BYTES + 1)),
            Err(ReportTestError::Source)
        );
        assert_eq!(
            with(&|t| t.source = String::new()),
            Err(ReportTestError::Source)
        );
        assert_eq!(
            with(&|t| t.source = "fn a() {}\0".into()),
            Err(ReportTestError::Source)
        );
        for invisible in [
            '\u{2067}',
            '\u{200B}',
            '\u{FEFF}',
            '\u{E0041}',
            '\u{FE0F}',
            '\u{00AD}',
        ] {
            assert_eq!(
                with(&|t| t.source = format!("let ok = \"{invisible}\";")),
                Err(ReportTestError::Source),
                "{invisible:?}"
            );
        }
        assert_eq!(
            with(&|t| t.source = "let ok = \"é ✓\";\n\tdone();".into()),
            Ok(())
        );
        assert_eq!(
            with(&|t| t.patch = Some("x".repeat(MAX_PATCH_BYTES + 1))),
            Err(ReportTestError::Patch)
        );
        assert_eq!(
            with(&|t| t.patch = Some("+ \u{202e}".into())),
            Err(ReportTestError::Patch)
        );
        assert_eq!(with(&|t| t.patch = None), Ok(()));
        assert_eq!(
            with(&|t| t.investigator.model = "a\nb".into()),
            Err(ReportTestError::Model)
        );
        assert_eq!(
            with(&|t| t.investigator.model = "m".repeat(MAX_REPORT_CODE + 1)),
            Err(ReportTestError::Model)
        );
    }

    #[test]
    fn a_test_submission_round_trips_and_older_bundles_still_read() {
        let text = serde_json::to_value(submitted()).unwrap();
        assert_eq!(text["kind"], "rust_integration");
        assert_eq!(text["investigator"]["harness"], "codex");
        let back: ReportTest = serde_json::from_value(text).unwrap();
        assert_eq!(back, submitted());
    }
}
