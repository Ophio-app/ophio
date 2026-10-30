// SPDX-License-Identifier: Apache-2.0
//! What an agent says its turn came to. Every run that answers to the
//! person is asked to end its last message with an `ophio-outcome` block
//! (`ports::harness::conduct_instruction`): done, needs the person, or
//! unfinished, with its questions, what is left and any new work. Ophio
//! keeps it on the run's outcome, takes it out of the summary the person
//! reads, and offers what comes next from it. It is the agent's own word,
//! read only as data: it never verifies anything or starts anything by
//! itself.
use crate::github_after::fence_line;
use protocol::{
    RunId, TaskId,
    model::{
        CheckStatus, ChildState, NewWork, ReportStatus, Run, RunOutcome, RunReport, TaskMode,
        TimelineBody,
    },
};
use serde_json::Value;

pub(crate) const KIND: &str = "ophio-outcome";
/// What else agents report to Ophio in a block of its own: a review's
/// findings, triage or remarks. Ophio reads them from the run's messages
/// alone, so the summary goes without them as well. How to start the app
/// stays, since it is read from the summary when the messages lack it.
const OTHERS: [&str; 3] = ["ophio-findings", "ophio-triage", "ophio-review"];
/// At most this many questions, things left or pieces of new work.
const MAX_ITEMS: usize = 8;
const MAX_LINE: usize = 600;
const MAX_TITLE: usize = 120;
const MAX_BRIEF: usize = 4000;
/// How far back the run's last message is looked for when its summary was
/// cut short of the block.
const LOOK_BACK: usize = 50;

/// Keeps the report a run ended with on its outcome, and takes the block out
/// of the summary. The summary is cut to a bound, so a long last message
/// may have lost the block there; the run's last message on the timeline
/// still has it.
pub(crate) fn keep(
    r: &store::Read<'_>,
    task_id: TaskId,
    run_id: RunId,
    outcome: &mut RunOutcome,
) -> store::Result<()> {
    if let Some(summary) = &outcome.summary {
        let rest = split(summary, |info| OTHERS.iter().any(|kind| is(info, kind))).0;
        if rest != *summary {
            outcome.summary = Some(rest).filter(|s| !s.trim().is_empty());
        }
    }
    if outcome.report.is_some() {
        return Ok(());
    }
    if let Some(summary) = &outcome.summary {
        let (rest, block) = take(summary);
        if let Some(block) = block {
            outcome.summary = Some(rest).filter(|s| !s.trim().is_empty());
            outcome.report = read(&block);
            if outcome.report.is_some() {
                return Ok(());
            }
        }
    }
    let (page, _) = r.timeline(task_id, None, LOOK_BACK)?;
    let last = page.into_iter().rev().find_map(|item| match item.item {
        TimelineBody::AgentMessage { text, .. } if item.run_id == Some(run_id) => Some(text),
        _ => None,
    });
    if let Some(text) = last
        && let (_, Some(block)) = take(&text)
    {
        outcome.report = read(&block);
    }
    Ok(())
}

/// A message without its `ophio-outcome` blocks, and the last one's body.
/// Only a block a line opens outside any other fence counts; one left open
/// runs to the end.
pub(crate) fn take(message: &str) -> (String, Option<String>) {
    split(message, |info| is(info, KIND))
}

/// Whether a fence's `info` opens a `kind` block.
fn is(info: &str, kind: &str) -> bool {
    info.strip_prefix(kind).is_some_and(|rest| {
        let rest = rest.trim();
        rest.is_empty() || rest.eq_ignore_ascii_case("json")
    })
}

/// A message without the blocks whose fence `ours` matches, and the last
/// one's body.
fn split(message: &str, ours: impl Fn(&str) -> bool) -> (String, Option<String>) {
    let mut kept: Vec<&str> = Vec::new();
    let mut found = None;
    // The fence open now, and its lines when it is ours.
    let mut open: Option<(char, usize, Option<Vec<&str>>)> = None;
    for line in message.lines() {
        let fence = fence_line(line);
        open = match open.take() {
            Some((c, n, body)) => {
                let closes = matches!(fence, Some((close, m, info)) if close == c && m >= n && info.is_empty());
                match (closes, body) {
                    (true, Some(body)) => {
                        found = Some(body);
                        None
                    }
                    (true, None) => {
                        kept.push(line);
                        None
                    }
                    (false, Some(mut body)) => {
                        body.push(line);
                        Some((c, n, Some(body)))
                    }
                    (false, None) => {
                        kept.push(line);
                        Some((c, n, None))
                    }
                }
            }
            None => match fence {
                Some((c, n, info)) if c == '`' && ours(info) => Some((c, n, Some(Vec::new()))),
                Some((c, n, _)) => {
                    kept.push(line);
                    Some((c, n, None))
                }
                None => {
                    kept.push(line);
                    None
                }
            },
        };
    }
    if let Some((_, _, Some(body))) = open {
        found = Some(body);
    }
    let rest = kept.join("\n").trim_end().to_owned();
    (rest, found.map(|lines| lines.join("\n").trim().to_owned()))
}

/// The report a block holds, read leniently: a status in other words, a
/// question as `actions`, an item as an object with a title. `None` when it
/// isn't an object with a status Ophio knows.
pub(crate) fn read(block: &str) -> Option<RunReport> {
    let said: Value = serde_json::from_str(block).ok()?;
    let said = said.as_object()?;
    let status = said.get("status")?.as_str()?;
    let status = match status
        .trim()
        .to_lowercase()
        .replace([' ', '-'], "_")
        .as_str()
    {
        "done" | "complete" | "completed" | "finished" => ReportStatus::Done,
        "needs_you" | "needs_input" | "needs_person" | "blocked" | "waiting" => {
            ReportStatus::NeedsYou
        }
        "unfinished" | "incomplete" | "partial" | "in_progress" | "not_finished" => {
            ReportStatus::Unfinished
        }
        _ => return None,
    };
    let lines = |key: &str| -> Vec<String> {
        let Some(list) = said.get(key).and_then(Value::as_array) else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.as_str()),
                Value::Object(o) => ["text", "title", "question"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(Value::as_str)),
                _ => None,
            })
            .map(|s| clip(s, MAX_LINE))
            .filter(|s| !s.is_empty())
            .take(MAX_ITEMS)
            .collect()
    };
    let mut questions = lines("questions");
    for action in lines("actions") {
        if questions.len() < MAX_ITEMS && !questions.contains(&action) {
            questions.push(action);
        }
    }
    let new_work = (said.get("new_work").and_then(Value::as_array))
        .map(|list| list.iter().filter_map(work).take(MAX_ITEMS).collect())
        .unwrap_or_default();
    Some(RunReport {
        status,
        questions,
        remaining: lines("remaining"),
        new_work,
    })
}

fn work(v: &Value) -> Option<NewWork> {
    let (title, brief, mode, issue) = match v {
        Value::String(s) => (s.as_str(), s.as_str(), None, None),
        Value::Object(o) => {
            let text = |k: &str| o.get(k).and_then(Value::as_str);
            let title = text("title").or_else(|| text("brief"))?;
            let issue = match o.get("issue") {
                Some(Value::Number(n)) => n.as_u64(),
                Some(Value::String(s)) => s.trim().trim_start_matches('#').parse().ok(),
                _ => None,
            };
            (
                title,
                text("brief").unwrap_or(title),
                text("mode").or_else(|| text("kind")),
                issue.filter(|n| (1..=protocol::MAX_SAFE_INTEGER).contains(n)),
            )
        }
        _ => return None,
    };
    let title = clip(title, MAX_TITLE);
    if title.is_empty() {
        return None;
    }
    let mode = match mode.map(|m| m.trim().to_lowercase()) {
        Some(m) if m == "plan" => TaskMode::Plan,
        Some(m) if m == "ask" => TaskMode::Ask,
        _ => TaskMode::Work,
    };
    Some(NewWork {
        title,
        brief: clip(brief, MAX_BRIEF),
        mode,
        issue,
    })
}

/// `text` trimmed and cut to `max` characters on a character boundary.
fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", text[..at].trim_end()),
        None => text.to_owned(),
    }
}

/// What a run leaves in doubt, whatever the agent called its turn: checks
/// that failed, helpers or work still running, unknowns. The phone's
/// receipt and task rows say the same (`runDoubts` in the client).
pub(crate) fn doubts(run: &Run) -> Vec<String> {
    let outcome = run.outcome.as_ref();
    let s = |n: usize| if n == 1 { "" } else { "s" };
    // Of a run whose checks are read for verification, a check that failed
    // and was run again counts by its rerun: an earlier failure it fixed is
    // no doubt, and one never run again still is.
    let (failed, not_rerun) = outcome.map_or((0, 0), |o| match &o.verification {
        Some(v) => (v.failed as usize, v.failed_not_rerun as usize),
        None => (
            o.checks
                .iter()
                .filter(|c| c.status == CheckStatus::Failed)
                .count(),
            0,
        ),
    });
    let active = run
        .children
        .iter()
        .filter(|c| c.state == ChildState::Active)
        .count();
    let unknown = run.children.iter().any(|c| c.state == ChildState::Unknown)
        || outcome.is_some_and(|o| !o.unknowns.is_empty());
    let rest = outcome.is_some_and(|o| !o.still_active.is_empty());
    [
        (failed > 0).then(|| format!("{failed} check{} failed", s(failed))),
        (not_rerun > 0).then(|| {
            format!(
                "{not_rerun} check{} failed and {} run again",
                s(not_rerun),
                if not_rerun == 1 {
                    "wasn’t"
                } else {
                    "weren’t"
                }
            )
        }),
        (active > 0).then(|| format!("{active} helper{} still running", s(active))),
        rest.then(|| "work still active".to_owned()),
        unknown.then(|| "unknowns remain".to_owned()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// What the person reads in the notification for a run that has ended:
/// the agent's report where it needs them or stopped short, else what the
/// run left in doubt, so a turn it called done with a failed check reads
/// as finished and needing a look. None when it is simply done.
pub(crate) fn ended(run: &Run) -> Option<String> {
    let report = run.outcome.as_ref().and_then(|o| o.report.as_ref());
    let doubts = doubts(run);
    match report {
        Some(report) if report.status != ReportStatus::Done => headline(report),
        _ if doubts.is_empty() => report.and_then(headline),
        _ => Some(format!(
            "Finished, needs a look: {}.{}",
            doubts.join(" · "),
            report
                .and_then(suggests)
                .map(|it| format!(" {it}"))
                .unwrap_or_default()
        )),
    }
}

/// The new work a report suggests, in a sentence.
fn suggests(report: &RunReport) -> Option<String> {
    match report.new_work.len() {
        0 => None,
        1 => Some(format!(
            "It suggests new work: {}",
            report.new_work[0].title
        )),
        n => Some(format!("It suggests {n} new tasks.")),
    }
}

/// What the person reads in the notification for a run that ended with a
/// report: what it needs from them, that it stopped short, or the work it
/// suggests. None when it is simply done.
pub(crate) fn headline(report: &RunReport) -> Option<String> {
    let more = |n: usize| match n {
        0 => String::new(),
        n => format!(" (and {n} more)"),
    };
    match report.status {
        ReportStatus::NeedsYou => Some(match report.questions.split_first() {
            Some((first, rest)) => format!("It needs you: {first}{}", more(rest.len())),
            None => "It needs your answer or action to go on.".into(),
        }),
        ReportStatus::Unfinished => Some(match report.remaining.split_first() {
            Some((first, rest)) => format!("Not finished. Left: {first}{}", more(rest.len())),
            None => "Not finished: it stopped with work left.".into(),
        }),
        ReportStatus::Done => suggests(report).map(|it| format!("Done. {it}")),
    }
}
