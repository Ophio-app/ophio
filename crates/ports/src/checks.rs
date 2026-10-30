// SPDX-License-Identifier: Apache-2.0
//! Which shell commands an agent ran are a project's checks (its tests,
//! builds and linters), which may have changed files, whether a command
//! line's exit status is the check's own, and whether a run's work was
//! checked after its last change.
//!
//! Shared by the harness adapters, which read the native tools' results; the
//! agent's own account of its checks never decides any of this.

use protocol::model::{CheckResult, CheckStatus, RunOutcome, RunVerification, VerificationStatus};
use std::collections::HashMap;

/// How much of a check's output is kept: its last lines, cut to a bound.
pub const OUTPUT_LINES: usize = 20;
pub const MAX_OUTPUT: usize = 2 * 1024;
/// How much of a check's command a receipt keeps when it must save room.
const SHORT_COMMAND: usize = 512;

/// A shell command line as Ophio reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellLine {
    /// What the agent asked the shell to run, without the shell it was
    /// wrapped in.
    pub script: String,
    /// The check it runs, if it runs one.
    pub check: Option<CheckLine>,
    /// It may change files before its first check runs, or anywhere when it
    /// runs no check.
    pub writes_before: bool,
    /// It may change files after its first check started.
    pub writes_after: bool,
    /// Something in it that may change files went on in the background, so
    /// a change may land after any later check.
    pub background_writes: bool,
}

impl ShellLine {
    /// Whether running it may have changed files.
    pub fn writes(&self) -> bool {
        self.writes_before || self.writes_after
    }
    /// The number of changes the run has made when its check starts, given
    /// `changes` made before the line started.
    pub fn check_at(&self, changes: u32) -> u32 {
        changes + u32::from(self.writes_before)
    }
    /// The changes it counts once it has run: one before its check and one
    /// after, so a check with a change after it is never after the last.
    pub fn changes(&self) -> u32 {
        u32::from(self.writes_before) + u32::from(self.writes_after)
    }
}

/// A command line that runs at least one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckLine {
    /// Why the line's exit status may not be the check's: None when it is.
    not_own: Option<String>,
    /// Another command joined to the check with `&&`, whose failure would
    /// also end the line with a status other than 0.
    chained: Option<String>,
}

impl CheckLine {
    /// The check's status and the words that say why, from the command
    /// line's exit status and what it printed.
    pub fn result(&self, exit_code: i32, output: &str) -> (CheckStatus, String) {
        let shown = failure_shown(output);
        match (&self.not_own, &self.chained, exit_code) {
            (None, _, 0) => (CheckStatus::Passed, "Exit status 0".into()),
            (None, None, code) => (CheckStatus::Failed, format!("Exit status {code}")),
            (None, Some(_), code) if shown => (
                CheckStatus::Failed,
                format!("Exit status {code}, and its output shows the check failed."),
            ),
            (None, Some(other), code) => (
                CheckStatus::Unknown,
                format!(
                    "The command line ended with exit status {code}, which may be `{other}`'s, run with && beside the check."
                ),
            ),
            (Some(why), _, code) if shown && why.starts_with("its output went through") => (
                CheckStatus::Failed,
                format!(
                    "Its output shows the check failed (the command line ended with exit status {code}, but {why})."
                ),
            ),
            (Some(why), _, code) => (
                CheckStatus::Unknown,
                format!("The command line ended with exit status {code}, but {why}."),
            ),
        }
    }
}

/// How Ophio reads `command`. It may be wrapped in a shell
/// (`/usr/bin/bash -lc '…'`), as Codex reports it.
pub fn shell_line(command: &str) -> ShellLine {
    let script = unwrapped(command.trim());
    let Some(segments) = segments(&script) else {
        // A here-document's lines can't be told from commands: anything
        // may run, and anything may be written.
        return ShellLine {
            script,
            check: None,
            writes_before: true,
            writes_after: false,
            background_writes: false,
        };
    };
    let writes: Vec<bool> = segments.iter().map(may_write).collect();
    let background_writes = segments
        .iter()
        .zip(&writes)
        .any(|(s, w)| *w && s.then == Some(Op::Background));
    let Some(first) = segments.iter().position(|s| is_check(&s.words)) else {
        return ShellLine {
            script,
            check: None,
            writes_before: writes.contains(&true),
            writes_after: false,
            background_writes,
        };
    };
    let check = check_of(&segments, first);
    ShellLine {
        script,
        check: Some(check),
        writes_before: writes[..first].contains(&true),
        // What the check itself writes to a file is a change after it starts.
        writes_after: segments[first].redirects || writes[first + 1..].contains(&true),
        background_writes,
    }
}

/// Whether the exit status is the check's, from its first check on.
fn check_of(segments: &[Segment], first: usize) -> CheckLine {
    let mut pipefail = false;
    for segment in &segments[..first] {
        if let Some(on) = pipefail_set(&segment.words) {
            pipefail = on;
        }
    }
    let mut not_own = None;
    for (i, segment) in segments.iter().enumerate().skip(first) {
        let next = segments.get(i + 1);
        let why = match (segment.then, next) {
            (Some(Op::Background), _) => Some("it went on in the background".to_owned()),
            (_, None) | (Some(Op::And), Some(_)) => None,
            (Some(Op::Pipe), Some(next))
                if pipefail
                    && next
                        .words
                        .first()
                        .is_some_and(|w| ["tail", "cat", "tee"].contains(&w.as_str())) =>
            {
                None
            }
            (Some(Op::Pipe), Some(next)) => Some(format!(
                "its output went through `{}`, whose status that is",
                next.words.first().map_or("another command", String::as_str)
            )),
            (Some(Op::Or), _) => Some("a command after `||` may have set it".to_owned()),
            (Some(Op::Seq) | None, Some(_)) => Some("other commands ran after it".to_owned()),
        };
        if why.is_some() {
            not_own = why;
            break;
        }
    }
    // The commands joined to it by `&&`, before and after.
    let mut from = first;
    while from > 0 && segments[from - 1].then == Some(Op::And) {
        from -= 1;
    }
    let mut to = first;
    while to + 1 < segments.len() && segments[to].then == Some(Op::And) {
        to += 1;
    }
    let chained = segments[from..=to]
        .iter()
        .filter(|s| !is_check(&s.words))
        .map(|s| command_words(&s.words))
        .find(|w| {
            !w.first()
                .is_none_or(|w| ["cd", "pushd", "export", "set", "unset", "true"].contains(w))
        })
        .and_then(|w| w.first().map(|w| (*w).to_owned()));
    CheckLine { not_own, chained }
}

/// What a `set` command does to pipefail: Some(true) turns it on.
fn pipefail_set(words: &[String]) -> Option<bool> {
    if words.first().is_none_or(|w| w != "set") {
        return None;
    }
    let mut state = None;
    for pair in words.windows(2) {
        let (flag, name) = (pair[0].as_str(), pair[1].as_str());
        if name == "pipefail" && flag.len() > 1 && flag[1..].ends_with('o') {
            match flag.as_bytes()[0] {
                b'-' => state = Some(true),
                b'+' => state = Some(false),
                _ => {}
            }
        }
    }
    state
}

/// Whether what a check printed shows that it failed: a test runner's,
/// compiler's or package manager's own failure line.
pub fn failure_shown(output: &str) -> bool {
    output.lines().map(str::trim).any(|t| {
        t.starts_with("test result: FAILED")
            || t.starts_with("error: could not compile")
            || t.starts_with("error: test failed")
            || (t.starts_with("error: ") && t.contains("target") && t.ends_with("failed:"))
            || t.starts_with("error[E")
            || t.contains(": error TS")
            || t.starts_with("error TS")
            || t.starts_with("FAIL ")
            || t.starts_with("FAIL\t")
            || t == "FAIL"
            || ((t.starts_with("Tests ") || t.starts_with("Test Files ")) && t.contains(" failed"))
            || (t.starts_with("==") && t.contains(" failed"))
            || t.starts_with("npm ERR!")
            || t.starts_with("npm error")
            || (t.starts_with("ℹ fail ") && t != "ℹ fail 0")
            || (t.starts_with("✖ ") && t.contains(" problem"))
            || t.starts_with("make: ***")
    })
}

/// The end of a check's output: its last lines without terminal colour
/// codes, cut to [`MAX_OUTPUT`] bytes. None when it printed nothing.
///
/// Where the bound cuts a line, the cut moves on past the next space, and a
/// `… ` marks it. A secret Ophio hides never holds a space, so no end of
/// one is left that the masking, which hides whole secrets and a cut one's
/// start, couldn't see.
pub fn output_tail(text: &str) -> Option<String> {
    let plain = without_escapes(text);
    let trimmed = plain.trim_end();
    let lines: Vec<&str> = trimmed.lines().collect();
    let tail = lines[lines.len().saturating_sub(OUTPUT_LINES)..].join("\n");
    if tail.len() <= MAX_OUTPUT {
        return (!tail.trim().is_empty()).then_some(tail);
    }
    let mut start = tail.len() - MAX_OUTPUT + "… ".len();
    while !tail.is_char_boundary(start) {
        start += 1;
    }
    // Inside a word, the cut goes on to the next whitespace.
    if !tail[..start].ends_with(char::is_whitespace) {
        start = tail[start..]
            .find(char::is_whitespace)
            .map_or(tail.len(), |at| start + at);
    }
    let rest = tail[start..].trim_start();
    (!rest.is_empty()).then(|| format!("… {rest}"))
}

/// Marks each check with whether it started after the run's last change,
/// and says whether the run's work was checked after it.
///
/// `changes_before[i]` is how many file changes the run had made when
/// `checks[i]` started, and `changes` how many it made in all, counting
/// every shell command that may have changed files. Of the checks after the
/// last change, each command line counts once, by its latest result; checks
/// that never ran are left out. Verified when at least one counts and all
/// passed; failed when any failed; unknown when none failed but one has no
/// result of its own, a check that failed earlier wasn't run again, or
/// `lost` says evidence of a change or a check was left out; and not
/// checked when none ran after the last change.
pub fn verify(
    checks: &mut [CheckResult],
    changes_before: &[u32],
    changes: u32,
    lost: Option<&str>,
) -> RunVerification {
    let mut failed_before_changes = false;
    for (check, before) in checks.iter_mut().zip(changes_before) {
        check.after_last_change = Some(*before >= changes);
        failed_before_changes |= *before == 0 && check.status == CheckStatus::Failed;
    }
    // Each command line by its latest result, and whether that came after
    // the last change.
    let mut latest: HashMap<&str, (CheckStatus, bool)> = HashMap::new();
    for check in checks.iter().filter(|c| c.status != CheckStatus::NotRun) {
        latest.insert(
            key(check),
            (check.status, check.after_last_change == Some(true)),
        );
    }
    let count = |status, after| {
        latest
            .values()
            .filter(|(s, a)| *s == status && *a == after)
            .count() as u32
    };
    let (passed, failed, unknown) = (
        count(CheckStatus::Passed, true),
        count(CheckStatus::Failed, true),
        count(CheckStatus::Unknown, true),
    );
    let failed_not_rerun = count(CheckStatus::Failed, false);
    let checked = passed + failed + unknown > 0;
    let status = if failed > 0 {
        VerificationStatus::Failed
    } else if !checked {
        VerificationStatus::NotChecked
    } else if unknown > 0 || failed_not_rerun > 0 || lost.is_some() {
        VerificationStatus::Unknown
    } else {
        VerificationStatus::Verified
    };
    RunVerification {
        status,
        passed,
        failed,
        unknown,
        failed_not_rerun,
        failed_before_changes,
        incomplete: lost.map(str::to_owned),
    }
}

fn key(check: &CheckResult) -> &str {
    check.command.as_deref().unwrap_or(&check.name)
}

/// Fits `outcome` into `max` bytes of JSON, keeping room for the run around
/// it. The checks that decide its verification come first: earlier checks'
/// outputs go first, oldest first, then long commands are shortened, then
/// earlier checks go, and only then a deciding one, which leaves the run
/// unverified. Files' diffs, then files, go to fit what is left.
pub fn fit_outcome(mut outcome: RunOutcome, max: usize) -> RunOutcome {
    let files = std::mem::take(&mut outcome.changed_files);
    let mut checks = std::mem::take(&mut outcome.checks);
    let mut remaining = max.saturating_sub(json_len(&outcome) + 1024);
    let mut limited = false;
    let deciding: Vec<bool> = deciding(&checks);
    let total = |checks: &[CheckResult]| checks.iter().map(|c| json_len(c) + 1).sum::<usize>();
    // Outputs, oldest first: the ones that decide nothing, then the rest.
    for pass in [false, true] {
        for i in 0..checks.len() {
            if total(&checks) <= remaining {
                break;
            }
            if deciding[i] == pass && checks[i].output.take().is_some() {
                limited = true;
            }
        }
    }
    if total(&checks) > remaining {
        for check in &mut checks {
            if let Some(command) = &mut check.command
                && command.len() > SHORT_COMMAND
            {
                let mut end = SHORT_COMMAND;
                while !command.is_char_boundary(end) {
                    end -= 1;
                }
                command.truncate(end);
                command.push('…');
                limited = true;
            }
        }
    }
    let mut keep = vec![true; checks.len()];
    let mut lost_deciding = false;
    for pass in [false, true] {
        for i in 0..checks.len() {
            let size: usize = checks
                .iter()
                .zip(&keep)
                .filter(|(_, k)| **k)
                .map(|(c, _)| json_len(c) + 1)
                .sum();
            if size <= remaining {
                break;
            }
            if keep[i] && deciding[i] == pass {
                keep[i] = false;
                limited = true;
                lost_deciding |= pass;
            }
        }
    }
    outcome.checks = checks
        .into_iter()
        .zip(keep)
        .filter_map(|(c, k)| k.then_some(c))
        .collect();
    remaining = remaining.saturating_sub(total(&outcome.checks));
    if lost_deciding
        && let Some(v) = outcome.verification.as_mut()
        && v.status == VerificationStatus::Verified
    {
        v.status = VerificationStatus::Unknown;
        v.incomplete = Some("Checks that decide it exceeded the receipt size limit.".into());
    }
    for mut file in files {
        let mut size = json_len(&file) + 1;
        if size > remaining {
            file.diff = None;
            limited = true;
            size = json_len(&file) + 1;
        }
        if size <= remaining {
            remaining -= size;
            outcome.changed_files.push(file);
        } else {
            limited = true;
        }
    }
    if limited {
        outcome
            .unknowns
            .push("Some file details or checks exceeded the receipt size limit.".into());
    }
    outcome
}

/// Which checks decide the run's verification or its doubts: those after
/// its last change, and a failure no later run of the same check replaced.
fn deciding(checks: &[CheckResult]) -> Vec<bool> {
    checks
        .iter()
        .enumerate()
        .map(|(i, c)| {
            c.after_last_change != Some(false)
                || (c.status == CheckStatus::Failed
                    && !checks[i + 1..]
                        .iter()
                        .any(|later| later.status != CheckStatus::NotRun && key(later) == key(c)))
        })
        .collect()
}

fn json_len<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX / 4, |v| v.len())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    And,
    Or,
    Pipe,
    Seq,
    Background,
}

#[derive(Debug, Default)]
struct Segment {
    words: Vec<String>,
    /// What joins it to the next one.
    then: Option<Op>,
    /// It redirects output into a file that may be the project's.
    redirects: bool,
    /// The commands its `$(…)` and backticks run.
    inner: Vec<String>,
}

/// The script inside `sh -c '…'`, `/usr/bin/bash -lc '…'` and the like;
/// otherwise the command itself.
fn unwrapped(command: &str) -> String {
    if let Some(words) = shell_words(command)
        && let [shell, flag, script] = words.as_slice()
        && ["sh", "bash", "zsh", "dash"].contains(&shell.rsplit('/').next().unwrap_or(shell))
        && ["-c", "-lc", "-cl"].contains(&flag.as_str())
    {
        return script.clone();
    }
    command.to_owned()
}

/// A command's words as a POSIX shell splits them, or None when its
/// quoting doesn't close.
fn shell_words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            c if c.is_whitespace() => {
                words.extend(word.take());
            }
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => w.push(c),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => w.push(c),
                            '\n' => {}
                            c => {
                                w.push('\\');
                                w.push(c);
                            }
                        },
                        c => w.push(c),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some(c) => word.get_or_insert_with(String::new).push(c),
            },
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Some(words)
}

/// A script's simple commands and what joins them. Separators inside
/// quotes, `$(…)` or backticks are data, and so is a comment. None for a
/// script with a here-document, whose lines can't be told from commands
/// here.
fn segments(script: &str) -> Option<Vec<Segment>> {
    if script.contains("<<") {
        return None;
    }
    let mut out: Vec<Segment> = Vec::new();
    let mut current = String::new();
    let mut building = Segment::default();
    let mut quote: Option<char> = None;
    let mut depth = 0usize;
    let mut inner_from = 0usize;
    let mut backtick: Option<usize> = None;
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;
    let mut push = |current: &mut String, building: &mut Segment, then: Option<Op>| {
        let words: Vec<String> = shell_words(current)
            .unwrap_or_else(|| current.split_whitespace().map(str::to_owned).collect());
        current.clear();
        let mut segment = std::mem::take(building);
        if words.is_empty() && !segment.redirects && segment.inner.is_empty() {
            // An empty command: a trailing `;` or newline, or a blank line.
            if let (Some(last), Some(Op::Seq) | None) = (out.last_mut(), then)
                && last.then == Some(Op::Seq)
            {
                last.then = then;
            }
            return;
        }
        segment.words = words;
        segment.then = then;
        out.push(segment);
    };
    while i < chars.len() {
        let ch = chars[i];
        let next = chars.get(i + 1).copied();
        let prev = i.checked_sub(1).map(|p| chars[p]);
        if let Some(q) = quote {
            current.push(ch);
            if ch == '\\' && q == '"' {
                if let Some(n) = next {
                    current.push(n);
                    i += 1;
                }
            } else if ch == q {
                quote = None;
            }
        } else if ch == '\\' {
            current.push(ch);
            if let Some(n) = next {
                current.push(n);
                i += 1;
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            current.push(ch);
        } else if ch == '`' && depth == 0 {
            match backtick.take() {
                Some(from) => building.inner.push(chars[from..i].iter().collect()),
                None => backtick = Some(i + 1),
            }
            current.push(ch);
        } else if ch == '$' && next == Some('(') {
            if depth == 0 && backtick.is_none() {
                inner_from = i + 2;
            }
            depth += 1;
            current.push_str("$(");
            i += 1;
        } else if depth > 0 || backtick.is_some() {
            match ch {
                '(' if depth > 0 => depth += 1,
                ')' if depth > 0 => {
                    depth -= 1;
                    if depth == 0 && backtick.is_none() {
                        building.inner.push(chars[inner_from..i].iter().collect());
                    }
                }
                _ => {}
            }
            current.push(ch);
        } else if ch == '#' && current.chars().last().is_none_or(char::is_whitespace) {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        } else if ch == '>' {
            building.redirects |= !harmless_target(&chars[i + 1..]);
            current.push(ch);
        } else if ch == '&' && (matches!(prev, Some('>' | '<')) || next == Some('>')) {
            // A redirection: `2>&1`, `>&2`, `&>file`.
            current.push(ch);
        } else if ch == '&' && next == Some('&') {
            push(&mut current, &mut building, Some(Op::And));
            i += 1;
        } else if ch == '&' {
            push(&mut current, &mut building, Some(Op::Background));
        } else if ch == '|' && next == Some('|') {
            push(&mut current, &mut building, Some(Op::Or));
            i += 1;
        } else if ch == '|' {
            push(&mut current, &mut building, Some(Op::Pipe));
            if next == Some('&') {
                i += 1;
            }
        } else if ch == ';' || ch == '\n' {
            push(&mut current, &mut building, Some(Op::Seq));
        } else {
            current.push(ch);
        }
        i += 1;
    }
    if let Some(from) = backtick {
        building.inner.push(chars[from..].iter().collect());
    }
    if depth > 0 {
        building
            .inner
            .push(chars[inner_from.min(chars.len())..].iter().collect());
    }
    push(&mut current, &mut building, None);
    // A line that ends in `;` or a newline ends there.
    if let Some(last) = out.last_mut()
        && last.then == Some(Op::Seq)
    {
        last.then = None;
    }
    Some(out)
}

/// Whether the file a `>` writes to, in what follows it, is somewhere no
/// project lives: a stream, a descriptor, or the temporary folder.
fn harmless_target(after: &[char]) -> bool {
    let mut rest = after;
    if let [first, more @ ..] = rest
        && matches!(first, '>' | '|')
    {
        rest = more;
    }
    if let ['&', more @ ..] = rest {
        // `>&2`, `>&-`: another descriptor.
        return more
            .first()
            .is_some_and(|c| c.is_ascii_digit() || *c == '-');
    }
    let target: String = rest
        .iter()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| !c.is_whitespace() && !";&|()<>".contains(**c))
        .filter(|c| !matches!(c, '"' | '\''))
        .collect();
    harmless_path(&target)
}

fn harmless_path(path: &str) -> bool {
    ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"].contains(&path)
        || [
            "/dev/fd/",
            "/tmp/",
            "$TMPDIR/",
            "${TMPDIR}/",
            "~/.cache/",
            "$HOME/.cache/",
        ]
        .iter()
        .any(|p| path.starts_with(p))
}

/// Whether a simple command may change files. Unknown commands may. Known
/// readers don't: listing, reading and searching files, Git commands that
/// leave the working tree as it is, and checks that only read.
fn may_write(segment: &Segment) -> bool {
    if segment.redirects || segment.inner.iter().any(|inner| shell_line(inner).writes()) {
        return true;
    }
    if is_check(&segment.words) {
        return false;
    }
    let words = command_words(&segment.words);
    let Some(first) = words.first() else {
        return false;
    };
    let name = first.rsplit('/').next().unwrap_or(first);
    let rest = &words[1..];
    let flag = |f: &[&str]| rest.iter().any(|w| f.contains(w));
    match name {
        "ls" | "cat" | "head" | "tail" | "wc" | "grep" | "egrep" | "fgrep" | "rg" | "ag" | "fd"
        | "pwd" | "cd" | "pushd" | "popd" | "echo" | "printf" | "which" | "type" | "true"
        | "false" | "test" | "[" | "[[" | "stat" | "file" | "du" | "df" | "sort" | "uniq"
        | "cut" | "tr" | "jq" | "diff" | "cmp" | "comm" | "sleep" | "date" | "whoami" | "id"
        | "uname" | "hostname" | "ps" | "pgrep" | "ss" | "lsof" | "nproc" | "free" | "uptime"
        | "tree" | "realpath" | "dirname" | "basename" | "readlink" | "sha256sum" | "sha1sum"
        | "md5sum" | "b2sum" | "xxd" | "od" | "hexdump" | "less" | "more" | "column" | "nl"
        | "seq" | "env" | "printenv" | "export" | "set" | "unset" | "exit" | "wait" | "jobs"
        | "kill" | "pkill" | "strings" | "paste" | "fold" | "rev" | "tac" | "expr" | "bc"
        | "locate" => false,
        "sed" => rest.iter().any(|w| {
            w.starts_with("--in-place")
                || (w.starts_with('-') && !w.starts_with("--") && w.contains('i'))
        }),
        "awk" | "gawk" => rest.windows(2).any(|w| w == ["-i", "inplace"]),
        "find" => flag(&[
            "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf",
            "-fls",
        ]),
        "tee" => !rest
            .iter()
            .filter(|w| !w.starts_with('-'))
            .all(|w| harmless_path(w)),
        "curl" | "wget" => {
            flag(&["-o", "-O", "--output", "--remote-name", "-P"])
                || (name == "wget" && !flag(&["-O-", "-qO-", "--spider"]))
        }
        "git" => git_writes(rest),
        "gh" => matches!(
            rest,
            ["pr", "checkout", ..] | ["repo", "clone", ..] | ["release" | "run", "download", ..]
        ),
        _ => true,
    }
}

/// Whether a Git command may change the working tree. Committing, pushing,
/// fetching and reading history leave its files as they are.
fn git_writes(args: &[&str]) -> bool {
    let mut args = args;
    while let [first, more @ ..] = args {
        match *first {
            "-C" | "-c" | "--git-dir" | "--work-tree" => args = more.get(1..).unwrap_or(&[]),
            w if w.starts_with('-') => args = more,
            _ => break,
        }
    }
    match args {
        [] => false,
        ["stash", "list" | "show", ..] => false,
        [command, ..] => !matches!(
            *command,
            "status"
                | "diff"
                | "log"
                | "show"
                | "rev-parse"
                | "rev-list"
                | "ls-files"
                | "ls-tree"
                | "blame"
                | "grep"
                | "describe"
                | "shortlog"
                | "cat-file"
                | "merge-base"
                | "for-each-ref"
                | "show-ref"
                | "reflog"
                | "config"
                | "remote"
                | "branch"
                | "tag"
                | "fetch"
                | "add"
                | "commit"
                | "push"
                | "notes"
                | "worktree"
                | "help"
                | "version"
        ),
    }
}

/// The command a simple command runs, past grouping, variable assignments
/// and wrappers such as `timeout 600` or `env -u NAME`.
fn command_words(words: &[String]) -> Vec<&str> {
    let mut words: Vec<&str> = words
        .iter()
        .map(|w| {
            w.trim_start_matches(['(', '{'])
                .trim_end_matches([')', '}'])
        })
        .filter(|w| !w.is_empty())
        .collect();
    while let Some(first) = words.first().copied() {
        let skip = if first.contains('=') && !first.starts_with('-') && !first.starts_with('=') {
            1
        } else {
            match first {
                "time" | "command" | "exec" | "nohup" => 1,
                "env" => {
                    let mut n = 1;
                    while let Some(w) = words.get(n) {
                        if *w == "-u" || *w == "--unset" || *w == "-C" {
                            n += 2;
                        } else if w.starts_with('-') || w.contains('=') {
                            n += 1;
                        } else {
                            break;
                        }
                    }
                    n
                }
                "xargs" => {
                    let mut n = 1;
                    while let Some(w) = words.get(n) {
                        if ["-I", "-n", "-P", "-L", "-d", "-s", "-E"].contains(w) {
                            n += 2;
                        } else if w.starts_with('-') {
                            n += 1;
                        } else {
                            break;
                        }
                    }
                    n
                }
                "timeout" | "nice" | "stdbuf" | "ionice" => {
                    let mut n = 1;
                    while let Some(w) = words.get(n) {
                        if *w == "-n" || *w == "-s" || *w == "-k" || *w == "-c" {
                            n += 2;
                        } else if w.starts_with('-') {
                            n += 1;
                        } else {
                            break;
                        }
                    }
                    // And timeout's duration.
                    if first == "timeout" && words.get(n).is_some() {
                        n += 1;
                    }
                    n
                }
                _ => 0,
            }
        };
        if skip == 0 || skip > words.len() {
            break;
        }
        words.drain(..skip);
    }
    words
}

const SCRIPTS: [&str; 5] = ["test", "build", "lint", "typecheck", "check"];
const RUNNERS: [&str; 8] = [
    "tsc",
    "vitest",
    "jest",
    "eslint",
    "prettier",
    "mocha",
    "playwright",
    "biome",
];

/// Whether a simple command runs a check that only reads the project. A
/// formatter or linter that rewrites files is a change, not a check.
fn is_check(words: &[String]) -> bool {
    let words = command_words(words);
    if words
        .iter()
        .any(|w| ["--fix", "--write", "-w", "--fix-dry-run"].contains(w))
    {
        return false;
    }
    let script = |name: &str| {
        SCRIPTS.contains(&name)
            || SCRIPTS.iter().any(|s| {
                name.strip_prefix(s)
                    .is_some_and(|rest| rest.starts_with(':'))
            })
    };
    let runner = |rest: &[&str]| match rest {
        ["prettier", more @ ..] => more.iter().any(|w| *w == "--check" || *w == "-c"),
        [name, ..] => RUNNERS.contains(name),
        [] => false,
    };
    let base = |w: &str| w.rsplit('/').next().unwrap_or(w).to_owned();
    let Some(first) = words.first().map(|w| base(w)) else {
        return false;
    };
    let rest = &words[1..];
    match first.as_str() {
        "cargo" => {
            let rest: Vec<&str> = rest
                .iter()
                .copied()
                .skip_while(|w| w.starts_with('+') || w.starts_with('-'))
                .collect();
            match rest.as_slice() {
                ["fmt", more @ ..] => more.contains(&"--check"),
                [action, ..] => ["test", "check", "clippy", "build", "nextest"].contains(action),
                [] => false,
            }
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            let rest: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|w| !w.starts_with('-'))
                .collect();
            match rest.as_slice() {
                ["run" | "run-script", name, ..] => script(name),
                ["exec" | "x" | "dlx", more @ ..] => runner(more),
                [name, ..] => script(name) || *name == "t" || runner(&rest),
                [] => false,
            }
        }
        "npx" | "bunx" | "pnpx" => {
            let rest: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|w| !w.starts_with('-'))
                .collect();
            runner(&rest)
        }
        "node" => rest.first() == Some(&"--test"),
        "pytest" | "ctest" | "mypy" | "tsc" | "vitest" | "jest" | "eslint" => true,
        "prettier" => runner(&words),
        "ruff" => match rest {
            ["check", ..] => true,
            ["format", more @ ..] => more.contains(&"--check"),
            _ => false,
        },
        "python" | "python3" => matches!(rest, ["-m", "pytest" | "unittest" | "mypy", ..]),
        "go" => matches!(rest, ["test" | "build" | "vet", ..]),
        "dotnet" => matches!(rest, ["test" | "build", ..]),
        "make" | "just" => rest.iter().any(|w| SCRIPTS.contains(w)),
        "gradlew" | "gradle" => rest.iter().any(|w| {
            ["test", "check", "build", "lint"].contains(&w.rsplit(':').next().unwrap_or(w))
        }),
        _ => false,
    }
}

/// `text` without terminal escape sequences (colours, cursor moves).
fn without_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            } else {
                chars.next();
            }
        } else if ch == '\r' {
            // A progress line redrawn in place keeps its last state.
            if chars.peek() != Some(&'\n') {
                let start = out.rfind('\n').map_or(0, |i| i + 1);
                out.truncate(start);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::model::Provenance;

    fn status(command: &str, exit: i32) -> Option<CheckStatus> {
        status_with(command, exit, "")
    }
    fn status_with(command: &str, exit: i32, output: &str) -> Option<CheckStatus> {
        shell_line(command).check.map(|l| l.result(exit, output).0)
    }

    #[test]
    fn a_line_whose_status_is_the_checks_own_passes_or_fails_by_it() {
        assert_eq!(status("cargo test", 0), Some(CheckStatus::Passed));
        assert_eq!(status("cargo test", 101), Some(CheckStatus::Failed));
        assert_eq!(status("cd app && npm test", 0), Some(CheckStatus::Passed));
        assert_eq!(status("cd app && npm test", 1), Some(CheckStatus::Failed));
        assert_eq!(
            status(
                "cargo fmt --check && cargo clippy -- -D warnings && cargo test",
                1
            ),
            Some(CheckStatus::Failed)
        );
        assert_eq!(
            status("cargo test > /tmp/log 2>&1", 0),
            Some(CheckStatus::Passed)
        );
        assert_eq!(
            status("unset DISPLAY; cargo test\n", 0),
            Some(CheckStatus::Passed)
        );
        assert_eq!(
            status(
                "env -u WAYLAND_DISPLAY RUST_LOG=off timeout 600 cargo test -p x",
                0
            ),
            Some(CheckStatus::Passed)
        );
        assert_eq!(
            status("set -o pipefail; cargo test 2>&1 | tail -20", 0),
            Some(CheckStatus::Passed)
        );
        assert_eq!(
            status("set -euo pipefail\ncargo test 2>&1 | tail -20", 101),
            Some(CheckStatus::Failed)
        );
    }

    #[test]
    fn set_plus_o_turns_pipefail_off() {
        assert_eq!(
            status("set -o pipefail; set +o pipefail; cargo test | tail -3", 0),
            Some(CheckStatus::Unknown)
        );
        assert_eq!(
            status("set +o pipefail; cargo test | tail -3", 0),
            Some(CheckStatus::Unknown)
        );
        assert_eq!(
            status("set +o pipefail; set -eo pipefail; cargo test | tail -3", 0),
            Some(CheckStatus::Passed)
        );
    }

    #[test]
    fn a_command_after_and_and_failing_says_nothing_of_the_check() {
        // A passing check with nothing to commit after it.
        let line = shell_line("cargo test && git commit -m x").check.unwrap();
        let (read, detail) = line.result(1, "nothing to commit, working tree clean");
        assert_eq!(read, CheckStatus::Unknown);
        assert!(detail.contains("`git`"), "{detail}");
        assert_eq!(
            status_with("cargo build && cargo run -- foo", 1, "error: bad flag"),
            Some(CheckStatus::Unknown)
        );
        assert_eq!(
            status_with("cd app && npm ci && npm test", 1, ""),
            Some(CheckStatus::Unknown)
        );
        // Unless what it printed shows the check failed.
        assert_eq!(
            status_with(
                "cargo test && git commit -m x",
                101,
                "test result: FAILED. 1 passed; 1 failed"
            ),
            Some(CheckStatus::Failed)
        );
        assert_eq!(
            status("cargo test && git commit -m x", 0),
            Some(CheckStatus::Passed)
        );
    }

    #[test]
    fn a_line_whose_status_is_another_commands_has_no_result() {
        for command in [
            "cargo test 2>&1 | tail -5",
            "npm run build 2>&1 | tail -6 && grep -c x dist/index.html",
            "cargo test; git status",
            "cargo test || true",
            "cargo test &",
            "set -o pipefail; cargo test | grep -E 'FAILED|test result'",
            "(npx --no-install tsc --noEmit -p . && echo OK) 2>&1 | tail -5",
        ] {
            let line = shell_line(command)
                .check
                .unwrap_or_else(|| panic!("{command}"));
            let (status, detail) = line.result(0, "");
            assert_eq!(status, CheckStatus::Unknown, "{command}");
            assert!(
                detail.starts_with("The command line ended with exit status 0, but"),
                "{detail}"
            );
        }
        // Its own failure line shows through a pipe.
        assert_eq!(
            status_with(
                "cargo test 2>&1 | tail -3",
                0,
                "test result: FAILED. 0 passed; 1 failed"
            ),
            Some(CheckStatus::Failed)
        );
        assert_eq!(
            status_with("cargo test; git status", 0, "test result: FAILED."),
            Some(CheckStatus::Unknown)
        );
    }

    #[test]
    fn shell_wrappers_as_codex_reports_them_are_unwrapped() {
        let line = shell_line("/usr/bin/bash -lc 'cd app && cargo test'");
        assert_eq!(line.script, "cd app && cargo test");
        assert_eq!(line.check.unwrap().result(0, "").0, CheckStatus::Passed);
        let line = shell_line(r#"/usr/bin/bash -lc "npm test -- -t \"it's\"""#);
        assert_eq!(line.script, r#"npm test -- -t "it's""#);
        assert!(shell_line("/bin/zsh -c 'pytest -q'").check.is_some());
        assert!(shell_line("/usr/bin/bash -lc 'git status'").check.is_none());
    }

    #[test]
    fn only_commands_that_check_are_checks() {
        for command in [
            "echo cargo test",
            "echo 'text && cargo test target'",
            "echo text #; cargo test target",
            "cat test.txt",
            "cargo fmt",
            "cargo fmt --all",
            "npx eslint --fix src",
            "npx prettier --write .",
            "npm install",
            "git commit -m 'npm test passes'",
            "python3 - <<'PY'\nimport os\nPY\ncargo test",
            "echo $(cargo test)",
        ] {
            assert!(shell_line(command).check.is_none(), "{command}");
        }
        for command in [
            "cargo fmt --all -- --check",
            "cargo +nightly clippy",
            "npm run test:unit",
            "npx --no-install tsc --noEmit -p .",
            "npx vitest run src/a.test.ts",
            "pnpm exec vitest",
            "node --test tests/",
            "python -m pytest -q",
            "ruff check .",
            "make -C client check",
            "./gradlew :app:testDebugUnitTest :app:lint",
            "go vet ./...",
        ] {
            assert!(shell_line(command).check.is_some(), "{command}");
        }
    }

    #[test]
    fn commands_that_may_change_files_are_changes() {
        let writes = |c: &str| shell_line(c).writes();
        for command in [
            "ls -la src",
            "cat a.rs | grep fn",
            "rg -n 'fn main' crates",
            "find . -name '*.rs'",
            "git status --short && git diff --stat",
            "git -C app log --oneline -3",
            "git add -A && git commit -m 'Fix' && git push",
            "sed -n 1,20p a.rs",
            "echo done > /dev/null",
            "cargo test 2>&1 | tee /tmp/test.log",
            "cd $(git rev-parse --show-toplevel) && ls",
            "cargo fmt --check",
            "ls | xargs grep -l main",
            "export X=1",
        ] {
            assert!(!writes(command), "{command}");
        }
        for command in [
            "cargo fmt",
            "sed -i s/a/b/ a.rs",
            "sed -Ei 's/a/b/' a.rs",
            "find . -name '*.orig' -delete",
            "echo x > a.rs",
            "echo x >> notes.md",
            "python3 scripts/gen.py",
            "python3 - <<'PY'\nopen('a','w')\nPY",
            "git checkout -- a.rs",
            "git stash",
            "npm install",
            "curl -o a.bin https://example.com",
            "touch a.rs",
            "echo $(touch a.rs)",
            "cat a | tee a.out",
            "npx prettier --write .",
        ] {
            assert!(writes(command), "{command}");
        }
    }

    #[test]
    fn a_change_after_a_check_on_the_same_line_comes_after_it() {
        let line = shell_line("cargo test && cargo fmt");
        assert!(!line.writes_before && line.writes_after);
        assert_eq!(line.check_at(3), 3);
        assert_eq!(line.changes(), 1);
        let line = shell_line("cargo fmt && cargo test");
        assert!(line.writes_before && !line.writes_after);
        assert_eq!(line.check_at(3), 4);
        let line = shell_line("cargo fmt && cargo test && sed -i s/a/b/ x");
        assert_eq!((line.check_at(0), line.changes()), (1, 2));
        assert!(shell_line("npm test > report.txt").writes_after);
        assert!(!shell_line("npm test > /tmp/report.txt").writes());
        assert!(shell_line("npm run dev &").background_writes);
        assert!(!shell_line("sleep 5 &").background_writes);
    }

    #[test]
    fn output_keeps_its_last_lines_within_the_bound() {
        let text = (1..=50)
            .map(|i| format!("\u{1b}[32mline {i}\u{1b}[0m"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = output_tail(&text).unwrap();
        assert!(tail.starts_with("line 31\n"));
        assert!(tail.ends_with("line 50"));
        let long = format!("{} end", "é ".repeat(MAX_OUTPUT * 2));
        assert!(output_tail(&long).unwrap().len() <= MAX_OUTPUT);
        assert_eq!(
            output_tail("50%\r100%\ndone\n\n").as_deref(),
            Some("100%\ndone")
        );
        assert_eq!(output_tail(" \n\n"), None);
        assert_eq!(output_tail(&"x".repeat(MAX_OUTPUT * 2)), None);
    }

    #[test]
    fn no_end_of_a_secret_is_left_where_the_output_is_cut() {
        let secret = "sk-test-0123456789abcdefghijklmnopqrstuvwxyz".to_owned();
        let mut straddled = 0;
        for after in MAX_OUTPUT - secret.len() - 8..MAX_OUTPUT {
            // One long last line, the cut moving across the secret.
            let lead = "word ".repeat(1000);
            let text = format!("{lead}{secret} {}", "z".repeat(after));
            let cut = text.len() - MAX_OUTPUT + "… ".len();
            let at = text.find(&secret).unwrap();
            straddled += usize::from(at < cut && cut < at + secret.len());
            // Cut inside the last word, nothing is left to show.
            let Some(mut tail) = output_tail(&text) else {
                continue;
            };
            assert!(tail.len() <= MAX_OUTPUT);
            crate::accounts::mask_cut(&mut tail, std::slice::from_ref(&secret));
            for n in crate::accounts::CUT_SHOWN..secret.len() {
                assert!(
                    !tail.contains(&secret[secret.len() - n..]),
                    "{after}: {tail}"
                );
            }
        }
        assert!(straddled > 30);
    }

    fn check(command: &str, status: CheckStatus) -> CheckResult {
        CheckResult {
            name: "Check command".into(),
            command: Some(command.into()),
            status,
            detail: None,
            provenance: Provenance::Native,
            exit_code: None,
            output: None,
            after_last_change: None,
        }
    }

    #[test]
    fn verified_means_every_check_after_the_last_change_passed() {
        use CheckStatus::*;
        let mut checks = vec![
            check("cargo test", Failed),
            check("cargo test", Failed),
            check("cargo test", Passed),
            check("npm run build", Passed),
        ];
        // Failed before any change, fixed, then passed after the last one.
        let v = verify(&mut checks, &[0, 1, 2, 2], 2, None);
        assert_eq!(v.status, VerificationStatus::Verified);
        assert_eq!(
            (v.passed, v.failed, v.unknown, v.failed_not_rerun),
            (2, 0, 0, 0)
        );
        assert!(v.failed_before_changes);
        assert_eq!(
            checks
                .iter()
                .map(|c| c.after_last_change)
                .collect::<Vec<_>>(),
            [Some(false), Some(false), Some(true), Some(true)]
        );
        // A flaky check run again: its latest result counts.
        let mut checks = vec![check("cargo test", Failed), check("cargo test", Passed)];
        assert_eq!(
            verify(&mut checks, &[1, 1], 1, None).status,
            VerificationStatus::Verified
        );
        let mut checks = vec![check("cargo test", Passed), check("npm test", Failed)];
        let v = verify(&mut checks, &[1, 1], 1, None);
        assert_eq!(v.status, VerificationStatus::Failed);
        assert!(!v.failed_before_changes);
        let mut checks = vec![
            check("cargo test", Passed),
            check("npm test | tail", Unknown),
        ];
        assert_eq!(
            verify(&mut checks, &[0, 0], 0, None).status,
            VerificationStatus::Unknown
        );
        // Checked, then changed again.
        let mut checks = vec![check("cargo test", Passed)];
        assert_eq!(
            verify(&mut checks, &[0], 1, None).status,
            VerificationStatus::NotChecked
        );
        // Denied checks never ran.
        let mut checks = vec![check("cargo test", NotRun)];
        assert_eq!(
            verify(&mut checks, &[1], 1, None).status,
            VerificationStatus::NotChecked
        );
        assert_eq!(
            verify(&mut [], &[], 0, None).status,
            VerificationStatus::NotChecked
        );
    }

    #[test]
    fn a_failure_never_run_again_stays_a_failure() {
        use CheckStatus::*;
        // Failed, edited, never run again.
        let mut checks = vec![check("cargo test", Failed)];
        let v = verify(&mut checks, &[0], 1, None);
        assert_eq!(v.status, VerificationStatus::NotChecked);
        assert_eq!(v.failed_not_rerun, 1);
        // Another check passing after the change doesn't clear it.
        let mut checks = vec![check("cargo test", Failed), check("npm run build", Passed)];
        let v = verify(&mut checks, &[0, 1], 1, None);
        assert_eq!(v.status, VerificationStatus::Unknown);
        assert_eq!((v.passed, v.failed_not_rerun), (1, 1));
        // A denied rerun doesn't replace it.
        let mut checks = vec![check("cargo test", Failed), check("cargo test", NotRun)];
        assert_eq!(verify(&mut checks, &[0, 1], 1, None).failed_not_rerun, 1);
    }

    #[test]
    fn lost_evidence_is_never_verified() {
        let mut checks = vec![check("cargo test", CheckStatus::Passed)];
        let v = verify(
            &mut checks,
            &[1],
            1,
            Some("A tool call was too large to read."),
        );
        assert_eq!(v.status, VerificationStatus::Unknown);
        assert_eq!(
            v.incomplete.as_deref(),
            Some("A tool call was too large to read.")
        );
        let mut checks = vec![check("cargo test", CheckStatus::Failed)];
        let v = verify(&mut checks, &[1], 1, Some("lost"));
        assert_eq!(v.status, VerificationStatus::Failed);
    }

    #[test]
    fn the_size_limit_drops_earlier_outputs_before_any_check_that_decides() {
        let mut checks: Vec<CheckResult> = (0..60)
            .map(|i| {
                let mut c = check(&format!("cargo test -p crate{i}"), CheckStatus::Failed);
                c.output = Some("x ".repeat(MAX_OUTPUT / 2));
                c
            })
            .collect();
        let mut last = check("cargo test", CheckStatus::Passed);
        last.output = Some("test result: ok".into());
        checks.push(last);
        let mut before: Vec<u32> = (0..60).map(|i| i as u32).collect();
        before.push(60);
        let verification = verify(&mut checks, &before, 60, None);
        let outcome = RunOutcome {
            summary: None,
            changed_files: vec![],
            checks,
            still_active: vec![],
            unknowns: vec![],
            usage: None,
            report: None,
            verification: Some(verification),
        };
        let fitted = fit_outcome(outcome, 64 * 1024);
        assert!(json_len(&fitted) <= 64 * 1024);
        assert_eq!(fitted.checks.len(), 61);
        let last = fitted.checks.last().unwrap();
        assert_eq!(last.output.as_deref(), Some("test result: ok"));
        // The oldest outputs went first.
        assert!(fitted.checks[0].output.is_none());
        assert!(fitted.unknowns.iter().any(|u| u.contains("size limit")));
    }
}
