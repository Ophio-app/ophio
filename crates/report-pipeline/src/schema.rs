// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result, digest, git, signals::Signal};
use protocol::reports::{
    DeclineReason, ExpectedOption, QuestionTemplate, ReportExtraKind, ReportProgress,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MAX_AGENT_BYTES: usize = 4096;
/// The test-path, size and character rules are protocol's, shared with the
/// intake so a submitted test and an agent's repro meet the same ones.
pub use protocol::reports::{MAX_TEST_BYTES, ReportTestKind as TestKind};
pub const MAX_LITERAL_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Category {
    Bug,
    Duplicate,
    WorksAsDocumented,
    FeatureRequest,
    SetupProblem,
    OutsideOphio,
    NotEnoughInformation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum DocsRelation {
    Matches,
    Contradicts,
    Silent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Area {
    Annotate,
    Contribute,
    Control,
    Findings,
    Github,
    Launch,
    Library,
    Settings,
    Talk,
    Transfers,
    Welcome,
    Work,
    Computer,
    Services,
    Store,
    Protocol,
    Sandbox,
    Containment,
    Accounts,
    Channels,
    HarnessClaude,
    HarnessCodex,
    Media,
    Terminal,
    Speech,
    VoiceControl,
    Browser,
    PlatformLinux,
    PlatformWindows,
    Device,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Confidence {
    Low,
    Medium,
    High,
}
/// String-bearing contract questions are deliberately unavailable to agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "template", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentQuestion {
    HappenedAgain {},
    AfterUpdate {},
    MayWeSee {
        part: ExtraKind,
    },
    Expected {
        #[schemars(length(min = 1, max = 5))]
        options: Vec<Expected>,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtraKind {
    TaskTranscript,
    Names,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Expected {
    ShouldWork,
    ShouldAsk,
    ShouldNotNotify,
    ShouldBeFaster,
    SomethingElse,
}
impl AgentQuestion {
    pub fn template(&self) -> QuestionTemplate {
        match self {
            Self::HappenedAgain {} => QuestionTemplate::HappenedAgain,
            Self::AfterUpdate {} => QuestionTemplate::AfterUpdate,
            Self::MayWeSee { part } => QuestionTemplate::MayWeSee {
                part: match part {
                    ExtraKind::TaskTranscript => ReportExtraKind::TaskTranscript,
                    ExtraKind::Names => ReportExtraKind::Names,
                },
            },
            Self::Expected { options } => QuestionTemplate::Expected {
                options: options
                    .iter()
                    .map(|o| match o {
                        Expected::ShouldWork => ExpectedOption::ShouldWork,
                        Expected::ShouldAsk => ExpectedOption::ShouldAsk,
                        Expected::ShouldNotNotify => ExpectedOption::ShouldNotNotify,
                        Expected::ShouldBeFaster => ExpectedOption::ShouldBeFaster,
                        Expected::SomethingElse => ExpectedOption::SomethingElse,
                    })
                    .collect(),
            },
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocsCheck {
    pub relation: DocsRelation,
    pub doc: Option<u16>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestTarget {
    kind: TestKind,
    #[schemars(length(min = 1, max = 240))]
    path: String,
}
impl TestTarget {
    /// A target for a test someone else wrote, such as a test submission,
    /// when its path meets the shared rules for its kind.
    pub fn new(kind: TestKind, path: &str) -> Option<Self> {
        let target = Self {
            kind,
            path: path.into(),
        };
        valid_test_path(&target).then_some(target)
    }
    pub fn kind(&self) -> TestKind {
        self.kind
    }
    pub fn path(&self) -> &str {
        &self.path
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TriageWire {
    category: Category,
    duplicate_of: Option<u16>,
    docs: DocsCheck,
    area: Area,
    question: Option<AgentQuestion>,
    confidence: Confidence,
    repro: Option<TestTarget>,
}
/// Only the checked parser can construct a verdict.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct TriageVerdict(TriageWire);
impl TriageVerdict {
    pub fn category(&self) -> Category {
        self.0.category
    }
    pub fn area(&self) -> Area {
        self.0.area
    }
    pub fn repro(&self) -> Option<&TestTarget> {
        self.0.repro.as_ref()
    }
    pub fn question(&self) -> Option<QuestionTemplate> {
        self.0.question.as_ref().map(AgentQuestion::template)
    }
    /// A bug remains received until code proves reproduction. No model declares it reproduced.
    pub fn progress(&self) -> ReportProgress {
        use Category::*;
        match self.0.category {
            Bug => ReportProgress::Received,
            Duplicate => ReportProgress::Known { fixed_in: None },
            SetupProblem => ReportProgress::Question,
            NotEnoughInformation if self.0.question.is_some() => ReportProgress::Question,
            WorksAsDocumented => ReportProgress::Declined {
                reason: DeclineReason::WorksAsDocumented,
            },
            FeatureRequest => ReportProgress::Declined {
                reason: DeclineReason::FeatureRequest,
            },
            OutsideOphio => ReportProgress::Declined {
                reason: DeclineReason::OutsideOphio,
            },
            NotEnoughInformation => ReportProgress::Declined {
                reason: DeclineReason::NotEnoughInformation,
            },
        }
    }
}
impl TriageVerdict {
    /// The verdict for a test submission, built in trusted code with no
    /// model asked: a bug, in the area its path names, to be reproduced by
    /// the submitted test. It reads back through [`parse_triage`] with no
    /// documentation or duplicate candidates.
    pub fn submitted(target: &TestTarget) -> Self {
        Self(TriageWire {
            category: Category::Bug,
            duplicate_of: None,
            docs: DocsCheck {
                relation: DocsRelation::Silent,
                doc: None,
            },
            area: area_of(target.path()),
            question: None,
            confidence: Confidence::High,
            repro: Some(target.clone()),
        })
    }
}

/// The area a test path names, decided by its crate or client screen alone.
pub fn area_of(path: &str) -> Area {
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        ["crates", name, ..] => match *name {
            "services" => Area::Services,
            "store" => Area::Store,
            "protocol" => Area::Protocol,
            "sandbox" => Area::Sandbox,
            "containment" => Area::Containment,
            "accounts" => Area::Accounts,
            "channels" => Area::Channels,
            "harness-claude" => Area::HarnessClaude,
            "harness-codex" => Area::HarnessCodex,
            "media" => Area::Media,
            "terminal" => Area::Terminal,
            "speech" => Area::Speech,
            "voice-control" => Area::VoiceControl,
            "browser" => Area::Browser,
            "platform-linux" => Area::PlatformLinux,
            "platform-windows" => Area::PlatformWindows,
            _ => Area::Computer,
        },
        ["client", "src", "screens", screen, ..] => match *screen {
            "annotate" => Area::Annotate,
            "contribute" => Area::Contribute,
            "control" => Area::Control,
            "findings" => Area::Findings,
            "github" => Area::Github,
            "launch" => Area::Launch,
            "library" => Area::Library,
            "settings" => Area::Settings,
            "talk" => Area::Talk,
            "transfers" => Area::Transfers,
            "welcome" => Area::Welcome,
            "work" => Area::Work,
            _ => Area::Device,
        },
        _ => Area::Device,
    }
}
#[derive(Debug, Clone, Copy)]
pub struct TriageContext {
    pub candidates: usize,
    pub docs: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rejection {
    pub signal: Signal,
}
fn reject<T>() -> std::result::Result<T, Rejection> {
    Err(Rejection {
        signal: Signal::OffSchemaOutput,
    })
}

pub fn triage_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(TriageWire)).expect("schema serializes")
}
pub fn parse_triage(
    raw: &[u8],
    ctx: TriageContext,
) -> std::result::Result<TriageVerdict, Rejection> {
    if raw.len() > MAX_AGENT_BYTES {
        return reject();
    }
    let Ok(v) = serde_json::from_slice::<TriageWire>(raw) else {
        return reject();
    };
    if v.duplicate_of
        .is_some_and(|i| usize::from(i) >= ctx.candidates)
        || (v.category == Category::Duplicate) != v.duplicate_of.is_some()
        || v.docs.doc.is_some_and(|i| usize::from(i) >= ctx.docs)
        || (v.docs.relation != DocsRelation::Silent && v.docs.doc.is_none())
        || (v.category == Category::Bug) != v.repro.is_some()
        || (v.category == Category::Bug && v.docs.relation == DocsRelation::Matches)
        || (v.category == Category::SetupProblem && v.question.is_none())
        || v.repro.as_ref().is_some_and(|t| !valid_test_path(t))
    {
        return reject();
    }
    if let Some(AgentQuestion::Expected { options }) = &v.question
        && (options.is_empty()
            || options.len() > 5
            || options
                .iter()
                .enumerate()
                .any(|(i, o)| options[..i].contains(o)))
    {
        return reject();
    }
    Ok(TriageVerdict(v))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewDecision {
    Pass,
    Block,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewFlag {
    WeakensSecurityCheck,
    TouchesCredentials,
    NetworkAccessAdded,
    Obfuscation,
    UnrelatedChange,
    TestWeakened,
    Other,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewVerdict {
    pub decision: ReviewDecision,
    #[schemars(length(max = 7))]
    pub flags: Vec<ReviewFlag>,
}
pub fn review_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(ReviewVerdict)).expect("schema serializes")
}
pub fn parse_review(raw: &[u8]) -> (ReviewVerdict, Option<Signal>) {
    if raw.len() <= MAX_AGENT_BYTES
        && let Ok(v) = serde_json::from_slice::<ReviewVerdict>(raw)
        && v.flags.len() <= 7
        && (v.decision == ReviewDecision::Block || v.flags.is_empty())
        && !v
            .flags
            .iter()
            .enumerate()
            .any(|(i, f)| v.flags[..i].contains(f))
    {
        return (v, None);
    }
    (
        ReviewVerdict {
            decision: ReviewDecision::Block,
            flags: vec![ReviewFlag::Other],
        },
        Some(Signal::OffSchemaOutput),
    )
}
fn valid_test_path(t: &TestTarget) -> bool {
    protocol::reports::valid_test_path(t.kind, &t.path)
}
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    lower
        .split('/')
        .any(|p| matches!(p, "test" | "tests" | "__tests__" | "androidtest"))
        || matches!(name, "tests.rs" | "test.rs")
        || name.ends_with("_tests.rs")
        || name.contains(".test.")
        || name.contains(".spec.")
        || (name.starts_with("test_") && name.ends_with(".py"))
        || name.ends_with("test.kt")
        || name.ends_with("tests.swift")
}

pub(crate) fn safe_text(s: &str) -> bool {
    protocol::reports::safe_code_text(s)
}
#[derive(Debug, Clone, Serialize)]
pub struct RemovedComment {
    pub offset: usize,
    pub bytes: usize,
    pub hash: [u8; 32],
}
#[derive(Debug, Clone, Serialize)]
pub struct ReducedTest {
    target: TestTarget,
    source: String,
    removed: Vec<RemovedComment>,
    long_literals: Vec<usize>,
}
impl ReducedTest {
    /// The form an agent may be shown, independent of any tree: comments
    /// removed, long literals noted. An error means the small lexer can't
    /// read the code safely, so no agent may be shown it.
    pub fn new(target: TestTarget, source: &str) -> Result<Self> {
        reduce(target, source)
    }
    pub fn removed_comments(&self) -> &[RemovedComment] {
        &self.removed
    }
    pub fn long_literals(&self) -> &[usize] {
        &self.long_literals
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn target(&self) -> &TestTarget {
        &self.target
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct ReproTest {
    base: String,
    target: TestTarget,
    hash: [u8; 32],
    reduced: ReducedTest,
}
impl ReproTest {
    pub fn base(&self) -> &str {
        &self.base
    }
    pub fn target(&self) -> &TestTarget {
        &self.target
    }
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }
    pub fn reduced(&self) -> &ReducedTest {
        &self.reduced
    }
    pub fn validate(repo: &Path, base: &str, target: &TestTarget, raw: &[u8]) -> Result<Self> {
        let paths = git::paths(repo, base)?;
        let parent = target
            .path
            .rsplit_once('/')
            .ok_or(Error::Rejected(Signal::OffSchemaOutput))?
            .0;
        if !valid_test_path(target)
            || paths.contains(&target.path)
            || !paths.iter().any(|p| p.starts_with(&format!("{parent}/")))
            || paths
                .iter()
                .any(|p| target.path.starts_with(&format!("{p}/")))
        {
            return Err(Error::Rejected(Signal::OffSchemaOutput));
        }
        let source =
            std::str::from_utf8(raw).map_err(|_| Error::Rejected(Signal::OffSchemaOutput))?;
        if !protocol::reports::valid_test_source(source) {
            return Err(Error::Rejected(Signal::OffSchemaOutput));
        }
        Ok(Self {
            base: base.into(),
            target: target.clone(),
            hash: digest(raw),
            reduced: reduce(target.clone(), source)
                .map_err(|_| Error::Rejected(Signal::OffSchemaOutput))?,
        })
    }
}

/// A deliberately small lexer. Ambiguous TS regex/division is rejected; template
/// strings are preserved whole. Reduction is not a proof that code is benign.
fn reduce(target: TestTarget, source: &str) -> Result<ReducedTest> {
    let b = source.as_bytes();
    let mut out = b.to_vec();
    let mut removed = Vec::new();
    let mut long_literals = Vec::new();
    let rust = target.kind != TestKind::Vitest;
    let mut i = 0;
    while i < b.len() {
        let start = i;
        if b[i..].starts_with(b"//") || b[i..].starts_with(b"/*") {
            let line = b[i + 1] == b'/';
            i += 2;
            if line {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            } else {
                let mut depth = 1;
                while i < b.len() && depth > 0 {
                    if rust && b[i..].starts_with(b"/*") {
                        depth += 1;
                        i += 2;
                    } else if b[i..].starts_with(b"*/") {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if depth != 0 {
                    return Err(Error::Invalid("unclosed comment"));
                }
            }
            removed.push(RemovedComment {
                offset: start,
                bytes: i - start,
                hash: digest(&b[start..i]),
            });
            for c in &mut out[start..i] {
                if !matches!(*c, b'\n' | b'\r') {
                    *c = b' ';
                }
            }
            continue;
        }
        // Raw Rust strings, including byte/C strings; hashes define their terminator.
        let raw_start = if rust && b[i] == b'r' {
            Some(i + 1)
        } else if rust && (b[i..].starts_with(b"br") || b[i..].starts_with(b"cr")) {
            Some(i + 2)
        } else {
            None
        };
        if let Some(mut j) = raw_start {
            let hstart = j;
            while j < b.len() && b[j] == b'#' {
                j += 1;
            }
            if j < b.len() && b[j] == b'"' {
                let hashes = j - hstart;
                let content = j + 1;
                i = content;
                while i < b.len()
                    && !(b[i] == b'"'
                        && b.get(i + 1..i + 1 + hashes)
                            .is_some_and(|s| s.iter().all(|c| *c == b'#')))
                {
                    i += 1;
                }
                if i == b.len() {
                    return Err(Error::Invalid("unclosed raw literal"));
                }
                if i - content > MAX_LITERAL_BYTES {
                    long_literals.push(start);
                }
                i += 1 + hashes;
                continue;
            }
        }
        let char_literal = rust
            && b[i] == b'\''
            && (b.get(i + 2) == Some(&b'\'')
                || b.get(i + 1) == Some(&b'\\')
                || source[i + 1..]
                    .chars()
                    .next()
                    .is_some_and(|c| b.get(i + 1 + c.len_utf8()) == Some(&b'\'')));
        if b[i] == b'"' || (!rust && matches!(b[i], b'\'' | b'`')) || char_literal {
            let quote = b[i];
            i += 1;
            let content = i;
            while i < b.len() && b[i] != quote {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            if i >= b.len() {
                return Err(Error::Invalid("unclosed literal"));
            }
            if i - content > MAX_LITERAL_BYTES {
                long_literals.push(start);
            }
            i += 1;
        } else {
            if !rust && b[i] == b'/' {
                return Err(Error::Invalid("ambiguous TS slash"));
            }
            // Advancing by bytes is safe except at the char-literal lookahead above.
            i += source[i..].chars().next().expect("in bounds").len_utf8();
        }
    }
    Ok(ReducedTest {
        target,
        source: String::from_utf8(out).map_err(|_| Error::Invalid("reduction UTF-8"))?,
        removed,
        long_literals,
    })
}
