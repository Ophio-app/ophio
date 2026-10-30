// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result, digest, git,
    schema::{ReproTest, is_test_path},
    signals::Signal,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

pub const MAX_FILES: usize = 8;
pub const MAX_LINES: usize = 300;
/// `*` spans directories; matching is case insensitive and relative to the repository.
pub const SENSITIVE_GLOBS: &[&str] = &[
    "crates/companion/*",
    "crates/services/*",
    "crates/store/*",
    "crates/domain/*",
    "crates/protocol/*",
    "crates/accounts/*",
    "crates/sandbox/*",
    "crates/containment/*",
    "crates/harness-*",
    "crates/platform-*",
    "crates/browser/*",
    "crates/report-*",
    "client/src-tauri/*",
    "client/src/host/*",
    "client/src/protocol/*",
    "client/src/platform/*",
    "client/src/screens/welcome/*",
    "*approv*",
    "*permission*",
    "*access*",
    "*grant*",
    "*pair*",
    "*identity*",
    "*keys*",
    "*update*",
    "*signature*",
    "*auth*",
    ".github/*",
    ".git*",
    "client/scripts/*",
    "scripts/*",
    "packaging/*",
    ".cargo/*",
    "*cargo.toml",
    "*package.json",
    "*lock",
    "*lock.json",
    "*build.rs",
    "*androidmanifest.xml",
    "*tauri.conf*",
    "*capabilities/*",
];

pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let p = pattern.to_ascii_lowercase();
    let s = path.to_ascii_lowercase();
    let (p, s) = (p.as_bytes(), s.as_bytes());
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < s.len() {
        if i < p.len() && p[i] == s[j] {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            mark = j;
        } else if let Some(k) = star {
            i = k + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}
pub fn sensitive_path(path: &str) -> bool {
    SENSITIVE_GLOBS.iter().any(|g| glob_matches(g, path))
        || path
            .rsplit('/')
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case("Info.plist"))
        || [
            "relay",
            "intake",
            "crypto",
            "credential",
            "signing",
            "person_only",
            "endpoint",
            "network",
        ]
        .iter()
        .any(|s| path.to_ascii_lowercase().contains(s))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Pass,
    Owner,
    Fail,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Gate {
    RedGreen,
    Dependencies,
    DiffCap,
    TestIntegrity,
    SensitivePaths,
    SensitiveContent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Flag {
    ReproNotRedGreen,
    Regression,
    DependenciesChanged,
    Lockfile,
    BuildHook,
    VendoredCode,
    TooManyFiles,
    TooManyLines,
    Binary,
    ModeChange,
    DeletedTest,
    ExistingTestChanged,
    ReproTampered,
    SensitivePath,
    UrlOrHost,
    Unsafe,
    Process,
    Environment,
    HomePath,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateResult {
    pub gate: Gate,
    pub outcome: Outcome,
    pub flags: Vec<Flag>,
    pub files: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateReport {
    pub results: Vec<GateResult>,
}
impl GateReport {
    pub fn outcome(&self) -> Outcome {
        if self.results.iter().any(|r| r.outcome == Outcome::Fail) {
            Outcome::Fail
        } else if self.results.iter().any(|r| r.outcome == Outcome::Owner) {
            Outcome::Owner
        } else {
            Outcome::Pass
        }
    }
    pub fn summary(&self) -> String {
        match self.outcome() {
            Outcome::Fail => "Stopped: a code gate failed.",
            Outcome::Owner => "Needs the owner's own review: sensitive changes.",
            Outcome::Pass => "Code gates passed; owner approval still required.",
        }
        .into()
    }
    fn add(&mut self, gate: Gate, outcome: Outcome, flags: Vec<Flag>, files: Vec<String>) {
        self.results.push(GateResult {
            gate,
            outcome,
            flags,
            files,
        });
    }
    pub fn signals(&self) -> Vec<Signal> {
        if self
            .results
            .iter()
            .any(|r| r.flags.contains(&Flag::ReproTampered))
        {
            vec![Signal::GateTampering]
        } else {
            vec![]
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TestResult {
    Passed,
    Failed,
    CompileError,
    TimedOut,
    Skipped,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestEvidence {
    pub repro_base: TestResult,
    pub repro_fix: TestResult,
    pub others_base: BTreeMap<String, TestResult>,
    pub others_fix: BTreeMap<String, TestResult>,
}
pub fn red_green(e: &TestEvidence) -> GateResult {
    let mut flags = vec![];
    if e.repro_base != TestResult::Failed || e.repro_fix != TestResult::Passed {
        flags.push(Flag::ReproNotRedGreen);
    }
    // Missing, skipped and timed-out results are not evidence of non-regression.
    if e.others_base.iter().any(|(k, b)| {
        !matches!(
            (b, e.others_fix.get(k)),
            (TestResult::Passed, Some(TestResult::Passed))
                | (
                    TestResult::Failed,
                    Some(TestResult::Passed | TestResult::Failed)
                )
        )
    }) || e
        .others_fix
        .iter()
        .any(|(k, v)| !e.others_base.contains_key(k) && *v != TestResult::Passed)
    {
        flags.push(Flag::Regression);
    }
    GateResult {
        gate: Gate::RedGreen,
        outcome: if flags.is_empty() {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        flags,
        files: vec![],
    }
}
#[derive(Debug, Clone, Copy)]
pub struct GateConfig {
    pub max_files: usize,
    pub max_lines: usize,
}
impl Default for GateConfig {
    fn default() -> Self {
        Self {
            max_files: MAX_FILES,
            max_lines: MAX_LINES,
        }
    }
}
#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: String,
    pub old_mode: String,
    pub new_mode: String,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub binary: bool,
}
#[derive(Debug, Clone)]
pub struct CommitDiff {
    base: String,
    fix: String,
    files: Vec<FileDiff>,
    text: String,
}
impl CommitDiff {
    pub fn base(&self) -> &str {
        &self.base
    }
    pub fn fix(&self) -> &str {
        &self.fix
    }
    pub fn files(&self) -> &[FileDiff] {
        &self.files
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn inspect(repo: &Path, base: &str, fix: &str) -> Result<Self> {
        git::commit(repo, base)?;
        git::commit(repo, fix)?;
        git::run(repo, &["merge-base", "--is-ancestor", base, fix])?;
        let raw = git::run(
            repo,
            &[
                "diff-tree",
                "-r",
                "--no-commit-id",
                "--no-renames",
                "--raw",
                "-z",
                base,
                fix,
            ],
        )?;
        let chunks: Vec<_> = raw.split(|b| *b == 0).filter(|s| !s.is_empty()).collect();
        if chunks.len() % 2 != 0 || chunks.len() > 2048 {
            return Err(Error::Invalid("diff inventory limit"));
        }
        let mut files = vec![];
        let mut text = String::new();
        let mut blob_bytes = 0;
        for pair in chunks.as_chunks::<2>().0 {
            let header = std::str::from_utf8(pair[0]).map_err(|_| Error::Git)?;
            let h: Vec<_> = header.trim_start_matches(':').split_whitespace().collect();
            if h.len() != 5 {
                return Err(Error::Git);
            }
            let path = std::str::from_utf8(pair[1])
                .map_err(|_| Error::Invalid("path UTF-8"))?
                .to_owned();
            if !crate::schema::safe_text(&path)
                || path.len() > 1024
                || path.chars().any(char::is_control)
            {
                return Err(Error::Invalid("path controls"));
            }
            let before = if matches!(h[0], "000000" | "160000") {
                vec![]
            } else {
                git::blob(repo, h[2])?
            };
            let after = if h[1] == "000000" || h[1] == "160000" {
                vec![]
            } else {
                git::blob(repo, h[3])?
            };
            blob_bytes += before.len() + after.len();
            if blob_bytes > 8 * 1024 * 1024 {
                return Err(Error::Invalid("aggregate blob limit"));
            }
            // Force text so repository attributes cannot hide edits. NUL/UTF-8 checks remain independent.
            let patch = git::run(
                repo,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    "--no-renames",
                    "--text",
                    "--unified=0",
                    base,
                    fix,
                    "--",
                    &path,
                ],
            )?;
            if text.len() + patch.len() > 4 * 1024 * 1024 {
                return Err(Error::Invalid("diff bytes limit"));
            }
            let patch = String::from_utf8_lossy(&patch);
            let mut added = vec![];
            let mut removed = vec![];
            let mut in_hunk = false;
            for line in patch.lines() {
                if line.starts_with("@@ ") {
                    in_hunk = true;
                    continue;
                }
                if !in_hunk {
                    continue;
                }
                if let Some(s) = line.strip_prefix('+') {
                    added.push(s.into());
                }
                if let Some(s) = line.strip_prefix('-') {
                    removed.push(s.into());
                }
            }
            let binary = before.contains(&0)
                || after.contains(&0)
                || std::str::from_utf8(&before).is_err()
                || std::str::from_utf8(&after).is_err();
            text.push_str(&patch);
            files.push(FileDiff {
                path,
                old_mode: h[0].into(),
                new_mode: h[1].into(),
                before,
                after,
                added,
                removed,
                binary,
            });
        }
        Ok(Self {
            base: base.into(),
            fix: fix.into(),
            files,
            text,
        })
    }
    /// This diff as an agent may see it: the repro's new file shows its
    /// reduced source, so the test's comments never reach the reviewer.
    /// Gates still judge the real diff; reduction keeps every line in place.
    pub fn with_reduced_test(&self, test: &crate::schema::ReproTest) -> Result<Self> {
        let path = test.target().path();
        let reduced = test.reduced().source();
        let header = format!("diff --git a/{path} b/{path}\n");
        let mut out = self.clone();
        let Some(file) = out.files.iter_mut().find(|f| f.path == path) else {
            return Ok(out);
        };
        // The file's own header, at a line start, exactly once; a test line
        // that merely contains it can't move the substitution.
        let starts: Vec<usize> = self
            .text
            .match_indices(&header)
            .map(|(at, _)| at)
            .filter(|at| *at == 0 || self.text.as_bytes()[at - 1] == b'\n')
            .collect();
        let [start] = starts[..] else {
            return Err(Error::Invalid("repro header in diff"));
        };
        let end = self.text[start + header.len()..]
            .find("\ndiff --git ")
            .map_or(self.text.len(), |i| start + header.len() + i + 1);
        let mut lines = reduced.split_inclusive('\n');
        let mut segment = String::new();
        let mut in_hunk = false;
        for line in self.text[start..end].split_inclusive('\n') {
            if line.starts_with("@@ ") {
                in_hunk = true;
            } else if in_hunk && line.starts_with('+') {
                let replaced = lines.next().ok_or(Error::Invalid("repro lines"))?;
                segment.push('+');
                segment.push_str(replaced.strip_suffix('\n').unwrap_or(replaced));
                if line.ends_with('\n') {
                    segment.push('\n');
                }
                continue;
            }
            segment.push_str(line);
        }
        if lines.next().is_some() {
            return Err(Error::Invalid("repro lines"));
        }
        out.text.replace_range(start..end, &segment);
        file.after = reduced.as_bytes().to_vec();
        file.added = reduced.lines().map(String::from).collect();
        Ok(out)
    }
}
fn dependencies(path: &str, before: &[u8], after: &[u8], added: bool) -> Result<Option<Flag>> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let lower = path.to_ascii_lowercase();
    if name.ends_with(".lock")
        || name == "package-lock.json"
        || name == "npm-shrinkwrap.json"
        || name == "yarn.lock"
        || name == "pnpm-lock.yaml"
        || name == "bun.lockb"
    {
        return Ok(Some(Flag::Lockfile));
    }
    if (name == "build.rs" && added)
        || lower.starts_with(".github/workflows/")
        || lower.contains(".cargo/config")
        || name == ".npmrc"
        || name == ".gitmodules"
    {
        return Ok(Some(Flag::BuildHook));
    }
    if lower.split('/').any(|p| {
        matches!(
            p,
            "vendor" | "vendored" | "third_party" | "third-party" | "node_modules"
        )
    }) {
        return Ok(Some(Flag::VendoredCode));
    }
    if name == "cargo.toml" {
        fn dependency_values(bytes: &[u8]) -> Result<BTreeMap<String, toml::Value>> {
            fn walk(value: &toml::Value, path: &str, out: &mut BTreeMap<String, toml::Value>) {
                if let Some(t) = value.as_table() {
                    for (k, v) in t {
                        let p = format!("{path}/{k}");
                        if k.ends_with("dependencies") || k == "patch" || k == "replace" {
                            out.insert(p, v.clone());
                        } else {
                            walk(v, &p, out);
                        }
                    }
                }
            }
            if bytes.is_empty() {
                return Ok(BTreeMap::new());
            }
            let v: toml::Value = toml::from_str(
                std::str::from_utf8(bytes).map_err(|_| Error::Invalid("manifest UTF-8"))?,
            )
            .map_err(|_| Error::Invalid("manifest TOML"))?;
            let mut out = BTreeMap::new();
            walk(&v, "", &mut out);
            Ok(out)
        }
        if dependency_values(before)? != dependency_values(after)? {
            return Ok(Some(Flag::DependenciesChanged));
        }
    }
    if name == "package.json" {
        fn fields(bytes: &[u8]) -> Result<BTreeMap<String, serde_json::Value>> {
            if bytes.is_empty() {
                return Ok(BTreeMap::new());
            }
            let v: serde_json::Value = serde_json::from_slice(bytes)?;
            let t = v.as_object().ok_or(Error::Invalid("package object"))?;
            Ok(t.iter()
                .filter(|(k, _)| {
                    k.to_ascii_lowercase().contains("dependencies")
                        || matches!(
                            k.as_str(),
                            "overrides"
                                | "resolutions"
                                | "scripts"
                                | "packageManager"
                                | "workspaces"
                                | "bundledDependencies"
                                | "bundleDependencies"
                        )
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect())
        }
        if fields(before)? != fields(after)? {
            return Ok(Some(Flag::DependenciesChanged));
        }
    }
    Ok(None)
}
pub fn content_flags(lines: &[String]) -> Vec<Flag> {
    let s = lines.join("\n").to_ascii_lowercase();
    let mut flags = vec![];
    // Conservative lexical indicators: review is required even for comments.
    if s.contains("://")
        || s.contains("localhost")
        || s.split(|c: char| !(c.is_ascii_alphanumeric() || ".-:_".contains(c)))
            .any(|w| {
                let host = w.split(':').next().unwrap_or(w).trim_end_matches('.');
                let labels: Vec<_> = host.split('.').collect();
                w.parse::<std::net::IpAddr>().is_ok()
                    || (labels.len() >= 2
                        && labels.iter().all(|l| {
                            !l.is_empty()
                                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        })
                        && labels.last().is_some_and(|l| {
                            l.len() >= 2 && l.bytes().all(|b| b.is_ascii_alphabetic())
                        }))
            })
        || s.contains("[::")
    {
        flags.push(Flag::UrlOrHost);
    }
    if s.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|w| w == "unsafe")
    {
        flags.push(Flag::Unsafe);
    }
    if [
        "command::",
        "process::",
        "child_process",
        "exec(",
        "execfile",
        "spawn(",
        "popen",
        "runtime.getruntime",
    ]
    .iter()
    .any(|p| s.contains(p))
    {
        flags.push(Flag::Process);
    }
    if [
        "env::",
        "process.env",
        "import.meta.env",
        "getenv",
        "env!(",
        "option_env!(",
        "std::env",
    ]
    .iter()
    .any(|p| s.contains(p))
    {
        flags.push(Flag::Environment);
    }
    if [
        concat!("/", "home/"),
        "/users/",
        "~/",
        "$home",
        "userprofile",
        "c:\\users\\",
    ]
    .iter()
    .any(|p| s.contains(p))
    {
        flags.push(Flag::HomePath);
    }
    flags
}
pub fn evaluate(
    diff: &CommitDiff,
    repro: &ReproTest,
    evidence: &TestEvidence,
    config: GateConfig,
) -> Result<GateReport> {
    let mut report = GateReport {
        results: vec![red_green(evidence)],
    };
    if report.outcome() == Outcome::Fail {
        return Ok(report);
    }
    let mut flags = vec![];
    let mut paths = vec![];
    for f in &diff.files {
        if let Some(flag) = dependencies(&f.path, &f.before, &f.after, f.old_mode == "000000")? {
            flags.push(flag);
            paths.push(f.path.clone());
        }
        if f.old_mode == "160000" || f.new_mode == "160000" {
            flags.push(Flag::BuildHook);
            paths.push(f.path.clone());
        }
    }
    let failed = !flags.is_empty();
    report.add(
        Gate::Dependencies,
        if failed { Outcome::Fail } else { Outcome::Pass },
        flags,
        paths,
    );
    if failed {
        return Ok(report);
    }
    let mut flags = vec![];
    if diff.files.len() > config.max_files || diff.files.is_empty() {
        flags.push(Flag::TooManyFiles);
    }
    if diff
        .files
        .iter()
        .map(|f| f.added.len() + f.removed.len())
        .sum::<usize>()
        > config.max_lines
    {
        flags.push(Flag::TooManyLines);
    }
    for f in &diff.files {
        if f.binary {
            flags.push(Flag::Binary);
        }
        if (f.old_mode != "000000" && f.new_mode != "000000" && f.old_mode != f.new_mode)
            || !matches!(f.old_mode.as_str(), "000000" | "100644")
            || !matches!(f.new_mode.as_str(), "000000" | "100644")
        {
            flags.push(Flag::ModeChange);
        }
        if is_test_path(&f.path) && f.new_mode == "000000" {
            flags.push(Flag::DeletedTest);
        }
    }
    let failed = !flags.is_empty();
    report.add(
        Gate::DiffCap,
        if failed { Outcome::Fail } else { Outcome::Pass },
        flags,
        vec![],
    );
    if failed {
        return Ok(report);
    }
    let repro_ok = diff.base() == repro.base()
        && diff.files.iter().any(|f| {
            f.path == repro.target().path()
                && f.old_mode == "000000"
                && f.new_mode == "100644"
                && digest(&f.after) == repro.hash()
        });
    if !repro_ok {
        report.add(
            Gate::TestIntegrity,
            Outcome::Fail,
            vec![Flag::ReproTampered],
            vec![repro.target().path().into()],
        );
        return Ok(report);
    }
    let tests: Vec<_> = diff
        .files
        .iter()
        .filter(|f| {
            (is_test_path(&f.path)
                || String::from_utf8_lossy(&f.before).contains("#[test]")
                || String::from_utf8_lossy(&f.before).contains("#[cfg(test)]")
                || String::from_utf8_lossy(&f.before).contains("::test]"))
                && f.old_mode != "000000"
        })
        .map(|f| f.path.clone())
        .collect();
    report.add(
        Gate::TestIntegrity,
        if tests.is_empty() {
            Outcome::Pass
        } else {
            Outcome::Owner
        },
        if tests.is_empty() {
            vec![]
        } else {
            vec![Flag::ExistingTestChanged]
        },
        tests,
    );
    let paths: Vec<_> = diff
        .files
        .iter()
        .filter(|f| sensitive_path(&f.path))
        .map(|f| f.path.clone())
        .collect();
    report.add(
        Gate::SensitivePaths,
        if paths.is_empty() {
            Outcome::Pass
        } else {
            Outcome::Owner
        },
        if paths.is_empty() {
            vec![]
        } else {
            vec![Flag::SensitivePath]
        },
        paths,
    );
    let mut flags = vec![];
    let mut paths = vec![];
    for f in &diff.files {
        let found = content_flags(&f.added);
        if !found.is_empty() {
            flags.extend(found);
            paths.push(f.path.clone());
        }
    }
    flags.sort_by_key(|f| *f as u8);
    flags.dedup();
    report.add(
        Gate::SensitiveContent,
        if flags.is_empty() {
            Outcome::Pass
        } else {
            Outcome::Owner
        },
        flags,
        paths,
    );
    Ok(report)
}
