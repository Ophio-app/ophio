// SPDX-License-Identifier: Apache-2.0
//! Native app-server result evidence. Item completion is not a check exit status.
use ports::checks;
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
    files: BTreeMap<String, ChangedFile>,
    // Run order; `seen` keeps a replayed item from adding a second check.
    checks: Vec<CheckResult>,
    /// How many file changes the run had made when each check started.
    changes_before: Vec<u32>,
    /// File changes, and finished commands that may have changed files.
    changes: u32,
    /// Commands under way: the changes made when each started, and whether
    /// it may change files.
    started: HashMap<String, (u32, bool)>,
    /// Why evidence of a change or a check was left out, if it was.
    lost: Option<String>,
    seen: std::collections::HashSet<String>,
    summary: Option<String>,
    limited: bool,
}
impl Results {
    pub(crate) fn observe(&mut self, method: &str, params: &Value) {
        let item = &params["item"];
        let Some(id) = item["id"].as_str() else {
            return;
        };
        let key = format!("{}:{id}", params["turnId"].as_str().unwrap_or_default());
        // A check is placed among the changes by when it started.
        if method == "item/started" && item["type"] == "commandExecution" {
            if self.started.len() >= MAX_ITEMS * 8 {
                self.lose("Ophio stopped following the run's commands after 1,024 at once.");
            } else if !self.seen.contains(&key) {
                let writes = item["command"]
                    .as_str()
                    .is_none_or(|c| checks::shell_line(c).writes());
                self.started.entry(key).or_insert((self.changes, writes));
            }
            return;
        }
        if method != "item/completed" {
            return;
        }
        // Only result-bearing items consume the bounded replay set.
        if !matches!(
            item["type"].as_str(),
            Some("agentMessage" | "commandExecution" | "fileChange")
        ) {
            return;
        }
        if self.seen.contains(&key) {
            return;
        }
        if self.seen.len() >= MAX_ITEMS * 8 {
            self.limited = true;
            self.lose("Ophio stopped reading the run's steps after 1,024.");
            return;
        }
        self.seen.insert(key.clone());
        match item["type"].as_str() {
            Some("agentMessage") => {
                if let Some(text) = item["text"].as_str().filter(|s| !s.is_empty()) {
                    self.limited |= text.len() > MAX_TEXT;
                    self.summary = Some(bounded(text));
                }
            }
            Some("fileChange") if item["status"] == "completed" => {
                self.changes += 1;
                if let Some(changes) = item["changes"].as_array() {
                    for change in changes {
                        let Some(path) = change["path"].as_str() else {
                            continue;
                        };
                        if path.len() > 4096
                            || (!self.files.contains_key(path) && self.files.len() >= MAX_ITEMS)
                        {
                            self.limited = true;
                            continue;
                        }
                        let kind = change["kind"]["type"]
                            .as_str()
                            .or_else(|| change["kind"].as_str());
                        let kind = match kind {
                            Some("add") => FileChange::Added,
                            Some("delete") => FileChange::Deleted,
                            Some("update")
                                if change["kind"]["move_path"].is_string()
                                    || change["kind"]["movePath"].is_string() =>
                            {
                                FileChange::Renamed
                            }
                            Some("update") => FileChange::Modified,
                            _ => {
                                self.limited = true;
                                continue;
                            }
                        };
                        let raw = change["diff"].as_str();
                        let previous = self.files.get(path);
                        let diff = raw.map(|text| {
                            let combined = previous.and_then(|p| p.diff.as_ref()).map_or_else(
                                || text.to_owned(),
                                |p| format!("{}\n\nNext tool edit:\n{text}", p.text),
                            );
                            ReportedDiff {
                                truncated: combined.len() > MAX_TEXT
                                    || previous
                                        .and_then(|p| p.diff.as_ref())
                                        .is_some_and(|d| d.truncated),
                                text: bounded(&combined),
                            }
                        });
                        let counts = raw.map(|s| {
                            (
                                s.lines()
                                    .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                                    .count() as u32,
                                s.lines()
                                    .filter(|l| l.starts_with('-') && !l.starts_with("---"))
                                    .count() as u32,
                            )
                        });
                        self.files.insert(
                            path.into(),
                            ChangedFile {
                                path: path.into(),
                                change: kind,
                                added: if previous.is_none() {
                                    counts.map(|c| c.0)
                                } else {
                                    None
                                },
                                removed: if previous.is_none() {
                                    counts.map(|c| c.1)
                                } else {
                                    None
                                },
                                diff,
                                provenance: Some(Provenance::Native),
                            },
                        );
                    }
                }
            }
            Some("commandExecution") => {
                let started = self.started.remove(&key).map(|(at, _)| at);
                let Some(command) = item["command"].as_str() else {
                    self.lose("Codex reported a command without saying what it ran.");
                    return;
                };
                let line = checks::shell_line(command);
                let declined = item["status"] == "declined";
                let at = line.check_at(started.unwrap_or(self.changes));
                if !declined {
                    self.changes += line.changes();
                    if line.background_writes {
                        self.lose("A command that may change files went on in the background.");
                    }
                }
                let Some(check) = line.check else {
                    return;
                };
                if self.checks.len() >= MAX_ITEMS || line.script.len() > MAX_TEXT {
                    self.limited = true;
                    self.lose("Some checks were too many or too long to keep.");
                    return;
                }
                let exit_code = item["exitCode"]
                    .as_i64()
                    .and_then(|code| i32::try_from(code).ok());
                let output = item["aggregatedOutput"].as_str().unwrap_or_default();
                let (status, detail) = match (item["status"].as_str(), exit_code) {
                    (Some("declined"), _) => {
                        (CheckStatus::NotRun, "Declined before it ran.".into())
                    }
                    (_, Some(code)) => check.result(code, output),
                    (Some("failed"), None) => (
                        CheckStatus::Unknown,
                        "Codex reported it failed, without an exit status.".into(),
                    ),
                    _ => (
                        CheckStatus::Unknown,
                        "Codex reported no exit status.".into(),
                    ),
                };
                self.checks.push(CheckResult {
                    name: "Check command".into(),
                    command: Some(line.script),
                    status,
                    detail: Some(detail),
                    provenance: Provenance::Native,
                    exit_code,
                    output: checks::output_tail(output),
                    after_last_change: None,
                });
                self.changes_before.push(at);
            }
            _ => {}
        }
    }
    pub(crate) fn outcome(&self) -> RunOutcome {
        let mut unknowns = Vec::new();
        if self.limited {
            unknowns.push("Some native result evidence exceeded the receipt limit.".into());
        }
        if let Some(summary) = &self.summary {
            unknowns.extend(open_gaps(summary));
        }
        let mut checks = self.checks.clone();
        // A command still under way may yet change files.
        let running = self.started.values().any(|(_, writes)| *writes);
        let verification = checks::verify(
            &mut checks,
            &self.changes_before,
            self.changes + u32::from(running),
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
    fn lose(&mut self, why: &str) {
        self.lost.get_or_insert_with(|| why.to_owned());
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
    fn notifications_preserve_native_evidence_and_unknown_exit_status() {
        let mut results = Results::default();
        for line in include_str!("tests/results.jsonl").lines() {
            let value: Value = serde_json::from_str(line).unwrap();
            for _ in 0..2 {
                results.observe(value["method"].as_str().unwrap(), &value["params"]);
            }
        }
        let out = results.outcome();
        assert_eq!(out.changed_files.len(), 1);
        assert_eq!(out.changed_files[0].added, Some(1));
        assert_eq!(out.changed_files[0].removed, Some(1));
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
                    c.exit_code,
                    c.after_last_change,
                )
            })
            .collect();
        assert_eq!(
            ran,
            [
                (
                    "cargo test -p app",
                    CheckStatus::Failed,
                    Some(101),
                    Some(false)
                ),
                (
                    "cargo test -p app",
                    CheckStatus::Passed,
                    Some(0),
                    Some(true)
                ),
                (
                    "npm run build 2>&1 | tail -3",
                    CheckStatus::Unknown,
                    Some(0),
                    Some(true)
                ),
                ("pytest", CheckStatus::Unknown, None, Some(true)),
                ("make check", CheckStatus::NotRun, None, Some(true)),
            ]
        );
        assert_eq!(out.checks[0].detail.as_deref(), Some("Exit status 101"));
        assert!(
            out.checks[0]
                .output
                .as_deref()
                .unwrap()
                .ends_with("test result: FAILED. 1 passed; 1 failed")
        );
        assert!(out.checks[2].detail.as_deref().unwrap().contains("`tail`"));
        assert_eq!(
            out.checks[3].detail.as_deref(),
            Some("Codex reported no exit status.")
        );
        assert_eq!(
            out.checks[4].detail.as_deref(),
            Some("Declined before it ran.")
        );
        let verification = out.verification.unwrap();
        assert_eq!(verification.status, VerificationStatus::Unknown);
        assert_eq!(
            (
                verification.passed,
                verification.failed,
                verification.unknown
            ),
            (1, 0, 2)
        );
        assert!(verification.failed_before_changes);
        assert!(
            out.checks
                .iter()
                .all(|c| c.provenance == Provenance::Native)
        );
        assert!(out.unknowns.iter().any(|s| s.contains("Windows")));
    }
    #[test]
    fn checks_keep_the_order_they_ran_in_not_the_order_of_their_ids() {
        let mut results = Results::default();
        for (id, exit_code) in [("9", 1), ("10", 0)] {
            results.observe(
                "item/completed",
                &serde_json::json!({"turnId":"t","item":{"id":id,"type":"commandExecution","command":"/usr/bin/bash -lc 'cargo test'","status":"completed","exitCode":exit_code}}),
            );
        }
        let out = results.outcome();
        assert_eq!(
            out.checks.iter().map(|c| c.exit_code).collect::<Vec<_>>(),
            [Some(1), Some(0)]
        );
    }
    #[test]
    fn missing_completion_and_unrecognized_change_do_not_become_changes() {
        let mut results = Results::default();
        let item = serde_json::json!({"item":{"id":"1","type":"fileChange","status":"inProgress","changes":[{"path":"a","kind":{"type":"update"}}]}});
        results.observe("item/started", &item);
        results.observe("item/completed", &item);
        assert!(results.outcome().changed_files.is_empty());
    }
    #[test]
    fn a_check_after_the_last_change_that_passed_verifies_the_run() {
        let mut results = Results::default();
        let command = |id: &str, exit: i64| serde_json::json!({"turnId":"t","item":{"id":id,"type":"commandExecution","command":"/usr/bin/bash -lc 'cargo fmt --check && cargo test'","status":if exit == 0 {"completed"} else {"failed"},"exitCode":exit,"aggregatedOutput":""}});
        let change = serde_json::json!({"turnId":"t","item":{"id":"f","type":"fileChange","status":"completed","changes":[{"path":"a.rs","kind":{"type":"update"},"diff":"+a"}]}});
        results.observe("item/completed", &change);
        results.observe("item/completed", &command("c1", 1));
        assert_eq!(
            results.outcome().verification.unwrap().status,
            VerificationStatus::Failed
        );
        results.observe("item/completed", &command("c2", 0));
        let out = results.outcome();
        assert_eq!(
            out.verification.unwrap().status,
            VerificationStatus::Verified
        );
        assert!(out.checks.iter().all(|c| c.output.is_none()));
    }
    fn command(id: &str, command: &str, exit: i64) -> Value {
        serde_json::json!({"turnId":"t","item":{"id":id,"type":"commandExecution","command":format!("/usr/bin/bash -lc '{command}'"),"status":if exit == 0 {"completed"} else {"failed"},"exitCode":exit,"aggregatedOutput":""}})
    }
    fn status(results: &Results) -> VerificationStatus {
        results.outcome().verification.unwrap().status
    }
    #[test]
    fn a_shell_command_that_may_change_files_after_the_last_check_unverifies_the_run() {
        let mut results = Results::default();
        results.observe("item/completed", &command("c1", "cargo test", 0));
        assert_eq!(status(&results), VerificationStatus::Verified);
        // Reading changes nothing.
        results.observe(
            "item/completed",
            &command("r", "git status --short && rg -n main", 0),
        );
        assert_eq!(status(&results), VerificationStatus::Verified);
        results.observe("item/completed", &command("w", "sed -i s/a/b/ src/a.rs", 0));
        assert_eq!(status(&results), VerificationStatus::NotChecked);
        // A declined writer never ran.
        results.observe("item/completed", &command("c2", "cargo test", 0));
        let mut declined = command("w2", "rm -rf src", 1);
        declined["item"]["status"] = "declined".into();
        results.observe("item/completed", &declined);
        assert_eq!(status(&results), VerificationStatus::Verified);
        // A writer after the check on the same line comes after it.
        results.observe(
            "item/completed",
            &command("c3", "cargo test && cargo fmt", 0),
        );
        assert_eq!(status(&results), VerificationStatus::NotChecked);
        results.observe(
            "item/completed",
            &command("c4", "cargo fmt && cargo test", 0),
        );
        assert_eq!(status(&results), VerificationStatus::Verified);
    }
    #[test]
    fn a_check_counts_from_when_it_started_not_when_it_finished() {
        let mut results = Results::default();
        let check = command("c", "cargo test", 0);
        results.observe("item/started", &check);
        let change = serde_json::json!({"turnId":"t","item":{"id":"f","type":"fileChange","status":"completed","changes":[{"path":"a.rs","kind":{"type":"update"},"diff":"+a"}]}});
        results.observe("item/completed", &change);
        results.observe("item/completed", &check);
        let out = results.outcome();
        assert_eq!(out.checks[0].after_last_change, Some(false));
        assert_eq!(
            out.verification.unwrap().status,
            VerificationStatus::NotChecked
        );
        // A writer still running when the outcome is read may yet change files.
        let mut results = Results::default();
        results.observe("item/completed", &command("c", "cargo test", 0));
        results.observe("item/started", &command("w", "npm run dev", 0));
        assert_eq!(status(&results), VerificationStatus::NotChecked);
    }
    #[test]
    fn steps_left_unread_never_leave_the_run_verified() {
        let mut results = Results::default();
        for i in 0..MAX_ITEMS * 8 {
            results.observe("item/completed", &command(&format!("r{i}"), "ls", 0));
        }
        results.observe("item/completed", &command("c", "cargo test", 0));
        let verification = results.outcome().verification.unwrap();
        assert_eq!(verification.status, VerificationStatus::NotChecked);
        assert!(verification.incomplete.unwrap().contains("1,024"));
        let mut results = Results::default();
        results.observe("item/completed", &command("c", "cargo test", 0));
        results.observe("item/completed", &command("bg", "npm run dev &", 0));
        results.observe("item/completed", &command("c2", "cargo test", 0));
        let verification = results.outcome().verification.unwrap();
        assert_eq!(verification.status, VerificationStatus::Unknown);
        assert!(verification.incomplete.unwrap().contains("background"));
    }
}
