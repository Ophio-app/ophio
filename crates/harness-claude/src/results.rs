// SPDX-License-Identifier: Apache-2.0
//! Evidence extracted from native tool acknowledgements; prose is never a file change.
use ports::{checks, tool_approval::DENIED_WITHOUT_REASON};
use protocol::model::{
    ChangedFile, CheckResult, CheckStatus, FileChange, Provenance, ReportedDiff, RunOutcome,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

const MAX_ITEMS: usize = 128;
const MAX_TEXT: usize = 16 * 1024;
const MAX_OUTCOME: usize = 128 * 1024;

#[derive(Default)]
pub(crate) struct Results {
    pending: HashMap<String, Pending>,
    files: BTreeMap<String, ChangedFile>,
    /// In the order they ran.
    checks: Vec<Check>,
    /// How many file changes the run had made: native file edits, and Bash
    /// commands that may have changed files.
    changes: u32,
    /// Why evidence of a change or a check was left out, if it was.
    lost: Option<String>,
    summary: Option<String>,
    limited: bool,
}
/// A tool call that has started and not yet answered.
struct Pending {
    tool: String,
    input: Value,
    /// A Bash command as Ophio reads it.
    line: Option<checks::ShellLine>,
    /// The changes made when it started, and its check's place among them.
    at: u32,
}
struct Check {
    tool_use_id: String,
    /// How many file changes the run had made when it started.
    changes_before: u32,
    result: CheckResult,
    /// What the tool said when it failed without an exit status; the reason,
    /// should the result frame show it was denied rather than run.
    error: Option<String>,
}
impl Results {
    pub(crate) fn observe(&mut self, value: &Value) {
        let root = value["parent_tool_use_id"].is_null();
        if value["type"] == "assistant" {
            if let Some(content) = value["message"]["content"].as_array() {
                if root {
                    let text = content
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        self.limited |= text.len() > MAX_TEXT;
                        self.summary = Some(bounded(&text));
                    }
                }
                for block in content.iter().filter(|b| b["type"] == "tool_use") {
                    let (Some(id), Some(tool)) = (block["id"].as_str(), block["name"].as_str())
                    else {
                        continue;
                    };
                    if !matches!(
                        tool,
                        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Bash"
                    ) {
                        continue;
                    }
                    if self.pending.len() >= MAX_ITEMS
                        || block["input"].to_string().len() > MAX_TEXT * 4
                    {
                        self.limited = true;
                        self.lose(
                            "A tool call too large or too many to read may have changed files or run a check.",
                        );
                        continue;
                    }
                    let line = (tool == "Bash").then(|| {
                        checks::shell_line(block["input"]["command"].as_str().unwrap_or(""))
                    });
                    let at = line
                        .as_ref()
                        .map_or(self.changes, |l| l.check_at(self.changes));
                    self.pending.insert(
                        id.into(),
                        Pending {
                            tool: tool.into(),
                            input: block["input"].clone(),
                            line,
                            at,
                        },
                    );
                }
            }
        } else if value["type"] == "user" {
            if let Some(content) = value["message"]["content"].as_array() {
                for block in content.iter().filter(|b| b["type"] == "tool_result") {
                    let Some(id) = block["tool_use_id"].as_str() else {
                        continue;
                    };
                    let Some(Pending {
                        tool,
                        input,
                        line,
                        at,
                    }) = self.pending.remove(id)
                    else {
                        continue;
                    };
                    // The SDK supplies structured data beside the single result block.
                    let result = if content
                        .iter()
                        .filter(|b| b["type"] == "tool_result")
                        .count()
                        == 1
                    {
                        &value["tool_use_result"]
                    } else {
                        &Value::Null
                    };
                    if let Some(line) = line {
                        self.bash(id, line, at, &input, block, result);
                    } else if block["is_error"] != true {
                        self.changes += 1;
                        self.file(&tool, &input, result);
                    }
                }
            }
        } else if root && value["type"] == "result" {
            if let Some(text) = value["result"].as_str().filter(|t| !t.is_empty()) {
                self.limited |= text.len() > MAX_TEXT;
                self.summary = Some(bounded(text));
            }
            // A denied tool also fails, so only this record tells the two apart.
            for denial in value["permission_denials"].as_array().into_iter().flatten() {
                if let Some(check) = self
                    .checks
                    .iter_mut()
                    .find(|c| denial["tool_use_id"] == c.tool_use_id.as_str())
                {
                    check.denied();
                }
            }
        }
    }
    fn file(&mut self, tool: &str, input: &Value, result: &Value) {
        let Some(path) = input["file_path"]
            .as_str()
            .or_else(|| input["notebook_path"].as_str())
        else {
            return;
        };
        if path.len() > 4096 || (!self.files.contains_key(path) && self.files.len() >= MAX_ITEMS) {
            self.limited = true;
            return;
        }
        let mut patch = String::new();
        let mut added = None;
        let mut removed = None;
        if let Some(hunks) = result["structuredPatch"].as_array() {
            let mut plus = 0u32;
            let mut minus = 0u32;
            for hunk in hunks {
                if let Some(lines) = hunk["lines"].as_array() {
                    for line in lines.iter().filter_map(Value::as_str) {
                        if line.starts_with('+') {
                            plus = plus.saturating_add(1);
                        }
                        if line.starts_with('-') {
                            minus = minus.saturating_add(1);
                        }
                        if patch.len() <= MAX_TEXT {
                            patch.push_str(line);
                            patch.push('\n');
                        }
                    }
                }
            }
            if !patch.is_empty() {
                added = Some(plus);
                removed = Some(minus);
            }
        } else if tool == "Edit" {
            patch = edit_excerpt(input);
        } else if tool == "MultiEdit" {
            if let Some(edits) = input["edits"].as_array() {
                for edit in edits {
                    if patch.len() > MAX_TEXT {
                        break;
                    }
                    patch.push_str(&edit_excerpt(edit));
                }
            }
        } else if tool == "Write" {
            if let Some(content) = input["content"].as_str() {
                patch = format!("Written content (previous content not reported):\n{content}");
            }
        } else if tool == "NotebookEdit"
            && let Some(source) = input["new_source"].as_str()
        {
            patch = format!(
                "Notebook cell {} ({}):\n{source}",
                input["cell_id"].as_str().unwrap_or("not reported"),
                input["edit_mode"].as_str().unwrap_or("mode not reported")
            );
        }
        let previous = self.files.get(path);
        let diff = if patch.is_empty() {
            None
        } else {
            let combined = previous.and_then(|p| p.diff.as_ref()).map_or_else(
                || patch.clone(),
                |p| format!("{}\n\nNext tool edit:\n{patch}", p.text),
            );
            Some(ReportedDiff {
                truncated: combined.len() > MAX_TEXT
                    || previous
                        .and_then(|p| p.diff.as_ref())
                        .is_some_and(|d| d.truncated),
                text: bounded(&combined),
            })
        };
        let repeated = previous.is_some();
        let change = if result["type"] == "create" {
            FileChange::Added
        } else {
            FileChange::Modified
        };
        self.files.insert(
            path.into(),
            ChangedFile {
                path: path.into(),
                change: previous.map_or(change, |p| p.change),
                // Counts for several operations need not equal a net file diff.
                added: if repeated { None } else { added },
                removed: if repeated { None } else { removed },
                diff,
                provenance: Some(Provenance::Native),
            },
        );
    }
    /// A Bash tool's result. Claude Code reports no exit status when a
    /// command succeeds: its result is the output, and `is_error` false. A
    /// command that exits non-zero comes back as an error whose text starts
    /// `Exit code N`, unless Claude Code read that status as no error (`grep`
    /// finding nothing), which it says in `returnCodeInterpretation`.
    fn bash(
        &mut self,
        id: &str,
        line: checks::ShellLine,
        at: u32,
        input: &Value,
        block: &Value,
        result: &Value,
    ) {
        let failed = block["is_error"] == true;
        let raw = raw_text_of(&block["content"]);
        let first_line = raw.lines().next().unwrap_or_default();
        let blocked = failed && first_line.contains("hook") && first_line.contains("PreToolUse");
        if !blocked {
            // Whatever it may have written counts, whether or not it was a
            // check and whatever its status.
            self.changes += line.changes();
            let background =
                input["run_in_background"] == true || !result["backgroundTaskId"].is_null();
            if line.background_writes || (background && line.writes()) {
                self.lose("A command that may change files went on in the background.");
            }
        }
        let Some(check) = &line.check else {
            return;
        };
        if self.checks.len() >= MAX_ITEMS || line.script.len() > MAX_TEXT {
            self.limited = true;
            self.lose("Some checks were too many or too long to keep.");
            return;
        }
        let reported = result["exit_code"]
            .as_i64()
            .or_else(|| result["exitCode"].as_i64())
            .and_then(|code| i32::try_from(code).ok());
        let stated = raw
            .strip_prefix("Exit code ")
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit() && c != '-').next())
            .and_then(|code| code.parse::<i32>().ok());
        let (printed, exit_code) = if failed {
            let rest = raw.split_once('\n').map_or("", |(_, rest)| rest);
            (
                if stated.is_some() {
                    rest.to_owned()
                } else {
                    raw.clone()
                },
                reported.or(stated),
            )
        } else {
            let stdout = result["stdout"].as_str().unwrap_or_default();
            let stderr = result["stderr"].as_str().unwrap_or_default();
            let printed = if !result["stdout"].is_string() {
                raw.clone()
            } else if stderr.is_empty() {
                stdout.to_owned()
            } else {
                format!("{stdout}\n{stderr}")
            };
            // Its result's shape is how a finished Bash command is told.
            let finished = result["stdout"].is_string()
                && result["interrupted"] == false
                && result["backgroundTaskId"].is_null()
                && result["returnCodeInterpretation"].is_null();
            (printed, reported.or(finished.then_some(0)))
        };
        let (status, detail) = if let Some(code) = exit_code {
            check.result(code, &printed)
        } else if blocked {
            (
                CheckStatus::NotRun,
                "A hook stopped it before it ran.".into(),
            )
        } else if failed {
            (
                CheckStatus::Unknown,
                "Claude Code reported an error, without an exit status.".into(),
            )
        } else if !result["backgroundTaskId"].is_null() {
            (
                CheckStatus::Unknown,
                "It went on in the background; its result isn't in this run's record.".into(),
            )
        } else if result["interrupted"] == true {
            (
                CheckStatus::Unknown,
                "It was interrupted before it finished.".into(),
            )
        } else if let Some(reading) = result["returnCodeInterpretation"].as_str() {
            (
                CheckStatus::Unknown,
                format!(
                    "It exited with a status other than 0, which Claude Code read as “{reading}”."
                ),
            )
        } else {
            (
                CheckStatus::Unknown,
                "Claude Code reported no exit status.".into(),
            )
        };
        self.checks.push(Check {
            tool_use_id: id.into(),
            changes_before: at,
            result: CheckResult {
                name: "Check command".into(),
                command: Some(line.script.clone()),
                status,
                detail: Some(detail),
                provenance: Provenance::Native,
                exit_code,
                output: checks::output_tail(&printed),
                after_last_change: None,
            },
            error: (failed && exit_code.is_none()).then(|| bounded(&raw)),
        });
    }
    fn lose(&mut self, why: &str) {
        self.lost.get_or_insert_with(|| why.to_owned());
    }
    pub(crate) fn outcome(&self) -> RunOutcome {
        let mut unknowns = Vec::new();
        if self.limited {
            unknowns.push("Some native result evidence exceeded the receipt limit.".into());
        }
        if !self.pending.is_empty() {
            unknowns.push("Some native tools have no recorded result.".into());
        }
        if let Some(summary) = &self.summary {
            unknowns.extend(open_gaps(summary));
        }
        let mut checks: Vec<_> = self.checks.iter().map(|c| c.result.clone()).collect();
        let before: Vec<_> = self.checks.iter().map(|c| c.changes_before).collect();
        // A tool still under way, or never answered, may have changed files.
        let open = self
            .pending
            .values()
            .any(|p| p.line.as_ref().is_none_or(checks::ShellLine::writes));
        let verification = checks::verify(
            &mut checks,
            &before,
            self.changes + u32::from(open),
            self.lost.as_deref(),
        );
        checks::fit_outcome(
            RunOutcome {
                summary: self.summary.clone(),
                changed_files: self.files.values().cloned().collect(),
                checks,
                still_active: Vec::new(),
                unknowns,
                usage: None,
                report: None,
                verification: Some(verification),
            },
            MAX_OUTCOME,
        )
    }
}
impl Check {
    fn denied(&mut self) {
        let reason = self
            .error
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty() && *r != DENIED_WITHOUT_REASON);
        self.result.status = CheckStatus::NotRun;
        self.result.exit_code = None;
        self.result.output = None;
        self.result.detail = Some(reason.map_or_else(
            || "Denied before it ran.".into(),
            |reason| format!("Denied before it ran: {reason}"),
        ));
    }
}
/// A tool result's whole text, whether given as a string or as text blocks.
fn raw_text_of(content: &Value) -> String {
    content.as_str().map_or_else(
        || {
            content
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        },
        str::to_owned,
    )
}
fn edit_excerpt(input: &Value) -> String {
    match (input["old_string"].as_str(), input["new_string"].as_str()) {
        (Some(old), Some(new)) => format!(
            "Native edit excerpt:\n{}{}",
            old.lines().map(|l| format!("-{l}\n")).collect::<String>(),
            new.lines().map(|l| format!("+{l}\n")).collect::<String>()
        ),
        _ => String::new(),
    }
}
fn bounded(text: &str) -> String {
    let mut end = text.len().min(MAX_TEXT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
fn open_gaps(summary: &str) -> Vec<String> {
    let mut in_gaps = false;
    let mut lines = Vec::new();
    for line in summary.lines() {
        let heading = line
            .trim()
            .trim_start_matches('#')
            .trim()
            .trim_end_matches(':');
        if ["open gaps", "remaining work", "unresolved", "not completed"]
            .iter()
            .any(|h| heading.eq_ignore_ascii_case(h))
        {
            in_gaps = true;
        } else if line.starts_with('#') {
            in_gaps = false;
        } else if in_gaps && !line.trim().is_empty() && lines.len() < 16 {
            lines.push(format!("Agent reported: {}", line.trim()));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::model::VerificationStatus;
    #[test]
    fn serialized_receipts_stay_bounded_and_disclose_omitted_evidence() {
        let mut results = Results::default();
        for i in 0..MAX_ITEMS {
            results.files.insert(
                i.to_string(),
                ChangedFile {
                    path: format!("file-{i}"),
                    change: FileChange::Modified,
                    added: None,
                    removed: None,
                    provenance: Some(Provenance::Native),
                    diff: Some(ReportedDiff {
                        text: "\"\n".repeat(MAX_TEXT / 2),
                        truncated: false,
                    }),
                },
            );
        }
        let out = results.outcome();
        assert!(serde_json::to_vec(&out).unwrap().len() <= MAX_OUTCOME);
        assert!(out.unknowns.iter().any(|s| s.contains("size limit")));
        assert_eq!(out.changed_files.len(), MAX_ITEMS);
    }
    #[test]
    fn fixture_records_acknowledged_edits_checks_and_final_prose_only() {
        let mut results = Results::default();
        for line in include_str!("tests/results.jsonl").lines() {
            results.observe(&serde_json::from_str(line).unwrap());
        }
        let out = results.outcome();
        assert_eq!(out.changed_files.len(), 4);
        assert!(
            !out.changed_files
                .iter()
                .any(|f| f.path == "imaginary.rs" || f.path == "refused.rs")
        );
        assert!(
            out.changed_files
                .iter()
                .all(|f| f.provenance == Some(Provenance::Native))
        );
        assert!(
            out.changed_files[0]
                .diff
                .as_ref()
                .unwrap()
                .text
                .contains("+new")
        );
        let ran: Vec<_> = out
            .checks
            .iter()
            .map(|c| {
                (
                    c.command.as_deref().unwrap(),
                    c.status,
                    c.detail.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            ran,
            [
                ("cargo test", CheckStatus::Passed, "Exit status 0"),
                ("npm run build", CheckStatus::Failed, "Exit status 2"),
                (
                    "pytest",
                    CheckStatus::Unknown,
                    "Claude Code reported no exit status."
                ),
                (
                    "npm test",
                    CheckStatus::NotRun,
                    "Denied before it ran: Not against the shared database."
                ),
                (
                    "cargo clippy",
                    CheckStatus::Unknown,
                    "Claude Code reported an error, without an exit status."
                ),
                ("make check", CheckStatus::NotRun, "Denied before it ran."),
                (
                    "cargo test 2>&1 | tail -3",
                    CheckStatus::Unknown,
                    "The command line ended with exit status 0, but its output went through `tail`, whose status that is."
                ),
                (
                    "npm run lint",
                    CheckStatus::NotRun,
                    "A hook stopped it before it ran."
                ),
                (
                    "npm test",
                    CheckStatus::Unknown,
                    "It went on in the background; its result isn't in this run's record."
                ),
                (
                    "npx vitest run | grep -c FAIL",
                    CheckStatus::Unknown,
                    "It exited with a status other than 0, which Claude Code read as “No matches found”."
                ),
            ]
        );
        assert_eq!(out.checks[0].exit_code, Some(0));
        assert_eq!(
            out.checks[0].output.as_deref(),
            Some("running 3 tests\ntest result: ok. 3 passed; 0 failed")
        );
        assert_eq!(out.checks[1].exit_code, Some(2));
        assert_eq!(
            out.checks[1].output.as_deref(),
            Some("src/main.ts(3,1): error TS2304: Cannot find name x.")
        );
        assert!(out.checks[3].exit_code.is_none());
        assert!(out.checks[3].output.is_none());
        assert!(
            out.checks
                .iter()
                .all(|c| c.provenance == Provenance::Native && c.after_last_change == Some(true))
        );
        let verification = out.verification.unwrap();
        assert_eq!(verification.status, VerificationStatus::Failed);
        assert_eq!(
            (
                verification.passed,
                verification.failed,
                verification.unknown
            ),
            (1, 1, 5)
        );
        assert!(out.summary.unwrap().starts_with("Finished"));
        assert!(out.unknowns.iter().any(|s| s.contains("Windows")));
    }
    #[test]
    fn checks_keep_the_order_they_ran_in_not_the_order_of_their_ids() {
        let mut results = Results::default();
        for (id, exit_code) in [("toolu_z", 1), ("toolu_a", 0)] {
            results.observe(&serde_json::json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":"cargo test"}}]}}));
            let result = if exit_code == 0 {
                serde_json::json!({"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":id,"is_error":false,"content":"ok"}]},"tool_use_result":{"stdout":"ok","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false}})
            } else {
                serde_json::json!({"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":id,"is_error":true,"content":format!("Exit code {exit_code}\nfailed")}]},"tool_use_result":format!("Error: Exit code {exit_code}\nfailed")})
            };
            results.observe(&result);
        }
        let out = results.outcome();
        assert_eq!(
            out.checks.iter().map(|c| c.exit_code).collect::<Vec<_>>(),
            [Some(1), Some(0)]
        );
    }
    #[test]
    fn bounds_text() {
        assert!(bounded(&"é".repeat(MAX_TEXT)).len() <= MAX_TEXT);
    }
    fn bash(id: &str, command: &str, result: serde_json::Value) -> [serde_json::Value; 2] {
        [
            serde_json::json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":command}}]}}),
            serde_json::json!({"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":id,"is_error":false,"content":""}]},"tool_use_result":result}),
        ]
    }
    fn ok() -> serde_json::Value {
        serde_json::json!({"stdout":"","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false})
    }
    fn status(results: &Results) -> VerificationStatus {
        results.outcome().verification.unwrap().status
    }
    #[test]
    fn a_command_that_may_change_files_after_the_last_check_leaves_the_run_unverified() {
        let mut results = Results::default();
        for event in bash("t1", "cargo test", ok()) {
            results.observe(&event);
        }
        assert_eq!(status(&results), VerificationStatus::Verified);
        // Read-only commands change nothing.
        for event in bash("g", "git status && rg -n main src", ok()) {
            results.observe(&event);
        }
        assert_eq!(status(&results), VerificationStatus::Verified);
        // Without Claude Code saying what it changed.
        for event in bash("s", "sed -i s/a/b/ src/a.rs", ok()) {
            results.observe(&event);
        }
        let out = results.outcome();
        assert_eq!(
            out.verification.unwrap().status,
            VerificationStatus::NotChecked
        );
        assert_eq!(out.checks[0].after_last_change, Some(false));
        // A writer after the check on its line comes after it.
        for event in bash("t2", "cargo test && cargo fmt", ok()) {
            results.observe(&event);
        }
        assert_eq!(status(&results), VerificationStatus::NotChecked);
        for event in bash("t3", "cargo fmt && cargo test", ok()) {
            results.observe(&event);
        }
        assert_eq!(status(&results), VerificationStatus::Verified);
        // A command blocked by a hook never ran.
        results.observe(&serde_json::json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"h","name":"Bash","input":{"command":"rm -rf src"}}]}}));
        results.observe(&serde_json::json!({"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"h","is_error":true,"content":"PreToolUse:Bash hook error: no"}]},"tool_use_result":"Error: PreToolUse:Bash hook error: no"}));
        assert_eq!(status(&results), VerificationStatus::Verified);
    }
    #[test]
    fn a_check_counts_from_when_it_started_not_when_it_finished() {
        let mut results = Results::default();
        let [check_use, check_result] = bash("t", "cargo test", ok());
        results.observe(&check_use);
        // An edit lands while the check runs.
        results.observe(&serde_json::json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"e","name":"Edit","input":{"file_path":"a.rs","old_string":"a","new_string":"b"}}]}}));
        results.observe(&serde_json::json!({"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"e","is_error":false}]}}));
        results.observe(&check_result);
        let out = results.outcome();
        assert_eq!(out.checks[0].after_last_change, Some(false));
        assert_eq!(
            out.verification.unwrap().status,
            VerificationStatus::NotChecked
        );
        // An edit with no answer yet may still change the file.
        let mut results = Results::default();
        for event in bash("t", "cargo test", ok()) {
            results.observe(&event);
        }
        results.observe(&serde_json::json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"e","name":"Write","input":{"file_path":"a.rs","content":"b"}}]}}));
        assert_eq!(status(&results), VerificationStatus::NotChecked);
    }
    #[test]
    fn a_tool_call_too_large_to_read_never_leaves_the_run_verified() {
        let mut results = Results::default();
        let huge = "x".repeat(MAX_TEXT * 4 + 1);
        results.observe(&serde_json::json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"w","name":"Write","input":{"file_path":"a.rs","content":huge}}]}}));
        for event in bash("t", "cargo test", ok()) {
            results.observe(&event);
        }
        let verification = results.outcome().verification.unwrap();
        assert_eq!(verification.status, VerificationStatus::Unknown);
        assert!(verification.incomplete.unwrap().contains("too large"));
        // Nor does a writer sent to the background.
        let mut results = Results::default();
        for event in bash("dev", "npm run dev", ok()) {
            results.observe(&event);
        }
        results.observe(&serde_json::json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"bg","name":"Bash","input":{"command":"npm run dev","run_in_background":true}}]}}));
        results.observe(&serde_json::json!({"type":"user","parent_tool_use_id":null,"message":{"content":[{"type":"tool_result","tool_use_id":"bg","is_error":false,"content":"Command running in background with ID: b1"}]},"tool_use_result":{"stdout":"","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false,"backgroundTaskId":"b1"}}));
        for event in bash("t", "cargo test", ok()) {
            results.observe(&event);
        }
        assert_eq!(status(&results), VerificationStatus::Unknown);
    }
}
