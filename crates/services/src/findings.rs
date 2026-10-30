// SPDX-License-Identifier: Apache-2.0
//! `FINDINGS.md`, Find & fix without GitHub: a review writes what it found
//! there, one heading with a box per finding, and fixers tick the boxes. The
//! phone writes the agents' instructions and reads the file back through
//! here, so the file is only ever read, never written, by Ophio itself.
use crate::error;
use protocol::{ErrorCode, ProtocolError};
use std::path::Path;

pub(crate) const FILE: &str = "FINDINGS.md";
/// Far more than a review's findings come to.
const MAX_BYTES: u64 = 512 * 1024;

/// The project's `FINDINGS.md`, or none when it has none. A link in its
/// place isn't followed.
pub(crate) async fn read(project: &Path) -> Result<Option<String>, ProtocolError> {
    let path = project.join(FILE);
    let meta = match tokio::fs::symlink_metadata(&path).await {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(e)),
    };
    if !meta.is_file() {
        return Err(error(
            ErrorCode::InvalidRequest,
            format!("{FILE} in this project isn’t a plain file, so it wasn’t read."),
        ));
    }
    if meta.len() > MAX_BYTES {
        return Err(error(
            ErrorCode::LimitExceeded,
            format!("{FILE} is over {} KB, too big to show.", MAX_BYTES / 1024),
        ));
    }
    let bytes = tokio::fs::read(&path).await.map_err(unreadable)?;
    String::from_utf8(bytes).map(Some).map_err(|_| {
        error(
            ErrorCode::InvalidRequest,
            format!("{FILE} isn’t text, so it can’t be shown."),
        )
    })
}

fn unreadable(e: std::io::Error) -> ProtocolError {
    error(
        ErrorCode::TemporarilyUnavailable,
        format!("{FILE} couldn’t be read: {e}"),
    )
}

/// One finding as FINDINGS.md lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listed {
    pub number: u32,
    pub title: String,
    /// Its box is ticked: fixed, or found not to be a problem.
    pub done: bool,
    /// What its `Verdict:` line says, after the colon, trimmed: none until a
    /// verifier has judged it.
    pub verdict: Option<String>,
    /// Stands for its heading and every line under it: the same only while
    /// none of them changes.
    pub key: String,
}

/// The findings in a FINDINGS.md, in the file's order. Lines in a code
/// block belong to the finding above them and are never headings or fields;
/// a fence left open runs to the end of the file. Code blocks and line ends
/// are found as CommonMark finds them, as the phone does, so both list the
/// same findings. A finding ends at the next one, or at a heading of another
/// section, as the phone shows it.
pub(crate) fn listed(text: &str) -> Vec<Listed> {
    scan(text).0
}

/// What in a FINDINGS.md could hide a finding from those listed, in plain
/// words: a line that reads like a finding's heading but isn't written as
/// one, or one inside a code block that is never closed. An agent reading
/// the file may take either for a finding nobody checked.
pub(crate) fn hidden(text: &str) -> Option<String> {
    scan(text).1
}

fn scan(text: &str) -> (Vec<Listed>, Option<String>) {
    let all = split(text);
    let code = code(text, &all);
    let mut found: Vec<Listed> = Vec::new();
    let mut lines: Vec<Vec<&str>> = Vec::new();
    // Whether lines still belong to the last finding.
    let mut open = false;
    let mut hidden = None;
    // A heading-like line in a fenced block never closed, by its number.
    let mut fenced = None;
    for (i, &(_, line)) in all.iter().enumerate() {
        let finding = match code[i] {
            Code::None => heading(line),
            _ => None,
        };
        if finding.is_none() && code[i] == Code::None && section(line) {
            open = false;
        }
        // Every other line belongs to the finding above it, until the next.
        if finding.is_none()
            && open
            && let Some(block) = lines.last_mut()
        {
            block.push(line.trim_end());
        }
        if code[i] != Code::None {
            // Only a fenced block closed where it is seen to be is plainly
            // an example: a heading-like line in any other is told.
            if code[i] != Code::Closed && reads_as_heading(line) {
                match code[i] {
                    Code::Open => _ = fenced.get_or_insert(i + 1),
                    _ => _ = hidden.get_or_insert_with(|| looks_like(i)),
                }
            }
            continue;
        }
        if let Some(heading) = finding {
            found.push(heading);
            lines.push(vec![line.trim_end()]);
            open = true;
        } else if reads_as_heading(line) {
            hidden.get_or_insert_with(|| looks_like(i));
        } else if let (true, Some(last), Some(verdict)) =
            (open, found.last_mut(), verdict(line.trim_start()))
        {
            last.verdict = Some(verdict);
        }
    }
    if let Some(n) = fenced {
        hidden.get_or_insert_with(|| {
            format!(
                "A code block in {FILE} is never closed, so what reads like a finding's heading at line {n} wasn't checked."
            )
        });
    }
    for (f, mut block) in found.iter_mut().zip(lines) {
        while block.last().is_some_and(|l| l.is_empty()) {
            block.pop();
        }
        f.key = key(&block);
    }
    (found, hidden)
}

fn looks_like(i: usize) -> String {
    format!(
        "Line {} of {FILE} reads like a finding's heading but isn't written as one, so it wasn't checked.",
        i + 1
    )
}

/// The text's lines, each with where it starts. A line ends at `\n`, `\r\n`
/// or a lone `\r`, as CommonMark ends it, so lines are counted as the phone
/// counts them.
fn split(text: &str) -> Vec<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        let end = match bytes[i] {
            b'\n' => 1,
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => 2,
            b'\r' => 1,
            _ => {
                i += 1;
                continue;
            }
        };
        lines.push((start, &text[start..i]));
        i += end;
        start = i;
    }
    if start < text.len() {
        lines.push((start, &text[start..]));
    }
    lines
}

/// A line's part in a code block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Code {
    None,
    /// In a fenced block that a fence closes.
    Closed,
    /// In a fenced block that runs to the end of the file, or of the list or
    /// quote it is in, with no fence to close it.
    Open,
    /// In a block of code set off by its indent.
    Indented,
}

/// Each line's part in a code block, wherever CommonMark finds one: in a
/// list or a quote too.
fn code(text: &str, lines: &[(usize, &str)]) -> Vec<Code> {
    use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};
    let mut code = vec![Code::None; lines.len()];
    let line = |at: usize| lines.partition_point(|&(start, _)| start <= at) - 1;
    for (event, range) in Parser::new(text).into_offset_iter() {
        let Event::Start(Tag::CodeBlock(kind)) = event else {
            continue;
        };
        if range.is_empty() {
            continue;
        }
        let (first, last) = (line(range.start), line(range.end - 1));
        let part = match kind {
            CodeBlockKind::Indented => Code::Indented,
            CodeBlockKind::Fenced(_) if first < last && closes(lines[first].1, lines[last].1) => {
                Code::Closed
            }
            CodeBlockKind::Fenced(_) => Code::Open,
        };
        code[first..=last].fill(part);
    }
    code
}

/// Whether `last` is a fence that closes the one `first` opens: the same
/// mark, at least as many, and nothing after them.
fn closes(first: &str, last: &str) -> bool {
    let (first, last) = (bare(first), bare(last));
    let Some(mark) = first.chars().next() else {
        return false;
    };
    let run = |s: &str| s.chars().take_while(|&c| c == mark).count();
    let n = run(last);
    n >= run(first) && last[n * mark.len_utf8()..].trim().is_empty()
}

/// Whether a line starts a section of the file, as the phone reads it: a
/// first- or second-level heading.
fn section(line: &str) -> bool {
    line.starts_with("# ") || line.starts_with("## ")
}

/// Whether a line reads like a finding's heading, however it is written: a
/// heading whose words start with a box or a number, like `### [ ] 3. A bug`
/// or `##[] 3 A bug`, or a numbered task like `- [ ] 3. A bug`, quoted or
/// in a list or not, but not code like `#[derive(Debug)]`. Characters that
/// don't show are left out first, as a reader wouldn't see them.
fn reads_as_heading(line: &str) -> bool {
    let shown: String = line.chars().filter(|&c| !unseen(c)).collect();
    let line = bare(&shown);
    let hashes = line.chars().take_while(|&c| c == '#').count();
    let rest = &line[hashes..];
    let words = rest.trim_start();
    let spaced = words.len() < rest.len();
    let numbered = |s: &str| s.starts_with(|c: char| c.is_ascii_digit());
    // A number ended like a list's, as in `3.` or `3)`.
    let counted = |s: &str| {
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && s[digits..].starts_with(['.', ')'])
    };
    // A box of up to three marks, such as `[ ]`, `[x]` or `[  ]`, and
    // whether a number follows it. A short one is a box whatever follows; a
    // longer one only before a number, so `[WIP]` isn't.
    let boxed = words.strip_prefix('[').and_then(|r| {
        let (inside, after) = r.split_once(']')?;
        let marks = inside.chars().count();
        let number = after.trim_start();
        let before_number = number.len() < after.len() && numbered(number);
        (marks <= 1 || (marks <= 3 && before_number)).then_some(before_number && counted(number))
    });
    match hashes {
        0 => boxed == Some(true),
        1..=6 => boxed.is_some() || (spaced && numbered(words)) || (!spaced && counted(words)),
        _ => false,
    }
}

/// A line without what sets it in a quote or a list, or indents it.
fn bare(line: &str) -> &str {
    let mut line = line.trim_start();
    loop {
        let digits = line.chars().take_while(char::is_ascii_digit).count();
        let rest = if let Some(rest) = line.strip_prefix('>') {
            rest
        } else if let Some(rest) = line.strip_prefix(['-', '*', '+'])
            && rest.starts_with(char::is_whitespace)
        {
            rest
        } else if (1..=9).contains(&digits)
            && let Some(rest) = line[digits..].strip_prefix(['.', ')'])
            && rest.starts_with(char::is_whitespace)
        {
            rest
        } else {
            return line;
        };
        line = rest.trim_start();
    }
}

/// Characters that take no room and show nothing: joiners, direction
/// marks, the byte order mark, soft hyphens, fillers and tags.
fn unseen(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
        | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
        | '\u{FFA0}' | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E0FFF}')
}

fn key(block: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for line in block {
        hash.update(line.as_bytes());
        hash.update(b"\n");
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The findings that still wait for a verifier: unticked, and not left by
/// one as it is now. `judged` keys what verifiers have judged, so a Verdict
/// line written by anyone else, or a finding changed since, doesn't count.
pub(crate) fn unjudged(text: &str, judged: &[String]) -> Vec<Listed> {
    let waiting = |f: &Listed| !f.done && !judged.contains(&f.key);
    listed(text).into_iter().filter(waiting).collect()
}

/// The finding a heading like `## [ ] 3. A short title` starts.
fn heading(line: &str) -> Option<Listed> {
    let rest = line.strip_prefix("## [")?;
    let (mark, rest) = rest.split_at_checked(1)?;
    let done = match mark {
        " " => false,
        "x" | "X" => true,
        _ => return None,
    };
    let rest = rest.strip_prefix("] ")?;
    let (digits, title) = rest.split_once(". ")?;
    let title = title.trim();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || title.is_empty() {
        return None;
    }
    Some(Listed {
        number: digits.parse().ok()?,
        title: title.to_owned(),
        done,
        verdict: None,
        key: String::new(),
    })
}

/// What a `Verdict:` line says, whatever the case of its label.
fn verdict(line: &str) -> Option<String> {
    let (label, rest) = line.split_at_checked("verdict:".len())?;
    label
        .eq_ignore_ascii_case("verdict:")
        .then(|| rest.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_the_file_or_says_there_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()).await.unwrap(), None);
        std::fs::write(dir.path().join(FILE), "## [ ] 1. A bug\n").unwrap();
        assert_eq!(
            read(dir.path()).await.unwrap().as_deref(),
            Some("## [ ] 1. A bug\n")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_or_folder_in_its_place_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let secret = elsewhere.path().join("secret");
        std::fs::write(&secret, "not the project's").unwrap();
        std::os::unix::fs::symlink(&secret, dir.path().join(FILE)).unwrap();
        assert!(read(dir.path()).await.is_err());
        std::fs::remove_file(dir.path().join(FILE)).unwrap();
        std::fs::create_dir(dir.path().join(FILE)).unwrap();
        assert!(read(dir.path()).await.is_err());
    }

    #[tokio::test]
    async fn too_big_or_not_text_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), [0xff, 0xfe, 0x00]).unwrap();
        assert!(read(dir.path()).await.is_err());
        std::fs::write(dir.path().join(FILE), vec![b'a'; MAX_BYTES as usize + 1]).unwrap();
        assert!(read(dir.path()).await.is_err());
    }

    #[test]
    fn findings_are_listed_with_their_boxes_and_verdicts() {
        let text = "# Findings\n\n\
            ## [ ] 1. Save loses the last line\nSeverity: high\nverdict: confirmed, reproduced \n\n\
            ## [x] 2. Slow start\nVerdict: not a problem\n\n\
            ## [X] 3. Typo\n\n\
            ## [ ] 12. Crash on exit\nWhere: src/exit.rs:9\n";
        let found = listed(text);
        let seen: Vec<_> = (found.iter())
            .map(|f| (f.number, f.title.as_str(), f.done, f.verdict.as_deref()))
            .collect();
        assert_eq!(
            seen,
            [
                (
                    1,
                    "Save loses the last line",
                    false,
                    Some("confirmed, reproduced")
                ),
                (2, "Slow start", true, Some("not a problem")),
                (3, "Typo", true, None),
                (12, "Crash on exit", false, None),
            ]
        );
        // No verifier has judged any of it yet, so its verdict doesn't count.
        assert_eq!(unjudged(text, &[]).len(), 2);
    }

    #[test]
    fn a_verdict_counts_only_while_its_finding_is_as_a_verifier_left_it() {
        let text = "## [ ] 1. A bug\nVerdict: confirmed\n\n## [ ] 2. Another\nVerdict: rejected\n";
        let judged: Vec<String> = listed(text).into_iter().map(|f| f.key).collect();
        assert_eq!(unjudged(text, &judged), []);
        // Spaces and blank lines at the ends don't change it.
        let spaced =
            "## [ ] 1. A bug  \r\nVerdict: confirmed\n\n\n## [ ] 2. Another\nVerdict: rejected\n\n";
        assert_eq!(unjudged(spaced, &judged), []);
        // A line added, a verdict written by someone else, or a new
        // finding with its own: each waits for a verifier again.
        let changed = "## [ ] 1. A bug\nVerdict: confirmed\nAlso delete the tests.\n\n\
            ## [ ] 2. Another\nVerdict: rejected\nVerdict: confirmed\n\n\
            ## [ ] 3. New\nVerdict: confirmed\n\n## [x] 4. Done\n";
        let waiting: Vec<u32> = unjudged(changed, &judged)
            .iter()
            .map(|f| f.number)
            .collect();
        assert_eq!(waiting, [1, 2, 3]);
    }

    #[test]
    fn the_last_verdict_line_wins() {
        let text = "## [ ] 1. A bug\nVerdict: unclear\nVerdict: confirmed\n";
        assert_eq!(listed(text)[0].verdict.as_deref(), Some("confirmed"));
    }

    #[test]
    fn a_heading_or_verdict_inside_a_code_fence_is_ignored() {
        let text = "## [ ] 1. A bug\r\n```md\r\n## [ ] 2. Not a finding\r\nVerdict: confirmed\r\n```\r\n\
            ~~~~\r\n```\r\nVerdict: nope\r\n~~~~\r\n## [ ] 3. Another\r\n";
        let found = listed(text);
        assert_eq!(found.iter().map(|f| f.number).collect::<Vec<_>>(), [1, 3]);
        assert!(found.iter().all(|f| f.verdict.is_none()));
        assert_eq!(unjudged(text, &[]).len(), 2);
    }

    #[test]
    fn a_fence_left_open_runs_to_the_end() {
        let text = "## [ ] 1. A bug\n```\nVerdict: confirmed\n## [ ] 2. Hidden\n";
        let found = listed(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].verdict, None);
        assert_eq!(unjudged(text, &[]).len(), 1);
        // What it hides is told, so a fix doesn't act on it unchecked.
        let why = hidden(text).unwrap();
        assert!(
            why.contains("never closed") && why.contains("line 4"),
            "{why}"
        );
        // Closed, or open with no heading in it, it hides nothing.
        assert_eq!(
            hidden("## [ ] 1. A bug\n```\n## [ ] 2. Shown as code\n```\n"),
            None
        );
        assert_eq!(hidden("## [ ] 1. A bug\n```\nlet a = 1;\n"), None);
    }

    #[test]
    fn a_finding_ends_at_another_section() {
        let text = "## [ ] 1. A bug\nWhere: src/a.rs:3\n\n## Notes\nVerdict: confirmed\n";
        let found = listed(text);
        assert_eq!(found[0].verdict, None);
        assert_eq!(
            found[0].key,
            listed("## [ ] 1. A bug\nWhere: src/a.rs:3\n")[0].key
        );
        assert_eq!(hidden(text), None);
    }

    #[test]
    fn a_malformed_heading_is_not_a_finding() {
        let text = "## [] 1. No box\n## [ ] x. No number\n## [ ] 2.No space\n## [ ] 3. \n\
            ## [y] 4. Odd box\n ## [ ] 5. Indented\n### [ ] 6. Deeper\n## [ ] 7. Fine\n";
        let found = listed(text);
        assert_eq!(found.iter().map(|f| f.number).collect::<Vec<_>>(), [7]);
        // Each reads like a finding to an agent, so it is told.
        for line in text.lines().take(7) {
            let why = hidden(&format!("## [ ] 1. A bug\n{line}\n")).unwrap();
            assert!(why.starts_with("Line 2 of FINDINGS.md"), "{line}: {why}");
        }
        for line in ["##[ ] 2. No space", "## 2. No box", "# [x] 2. Top"] {
            assert!(hidden(line).is_some(), "{line}");
        }
        // Headings and code that don't read like one aren't.
        let fine = "# Findings\n\n## [ ] 1. A bug\n### Details\n#[derive(Debug)]\n#12 is related\n";
        assert_eq!(hidden(fine), None);
    }

    /// Files the phone reads with CommonMark, and the findings it shows in
    /// each. The phone's own tests read the same files.
    const AS_THE_PHONE_READS: &[(&str, &[u32])] = &[
        // A lone carriage return ends a line.
        ("## [ ] 1. A bug\r## [ ] 2. After a lone return\r", &[1, 2]),
        // Backticks in a backtick fence's words make it inline code, not a fence.
        (
            "## [ ] 1. A bug\n```text```\n## [ ] 2. After inline code\n```\n",
            &[1, 2],
        ),
        // A fence indented four spaces is indented code, ended by what follows.
        (
            "## [ ] 1. A bug\n\n    ```\n## [ ] 2. After indented code\n    ```\n",
            &[1, 2],
        ),
        // A list's fence ends with the list.
        (
            "## [ ] 1. A bug\n- a step\n  ```\n## [ ] 2. After a list's code\n  ```\n",
            &[1, 2],
        ),
        // A quote's fence ends with the quote.
        (
            "## [ ] 1. A bug\n> ```\n## [ ] 2. After a quote's code\n> ```\n",
            &[1, 2],
        ),
        // Only the same fence, at least as long, closes one.
        (
            "## [ ] 1. A bug\n````\n```\n## [ ] 2. Still code\n~~~~\n````\n## [ ] 3. After\n",
            &[1, 3],
        ),
        // A fence indented four spaces doesn't close one.
        (
            "## [ ] 1. A bug\n```\n    ```\n## [ ] 2. Still code\n```\n## [ ] 3. After\n",
            &[1, 3],
        ),
        // The phone's own examples.
        (
            "## [ ] 1. Nested fences\n````md\n```\n## [ ] 2. Inside the outer fence\n```\n~~~\n\
             ## [ ] 3. Still inside\n````\n~~~\n```\n## [ ] 4. Inside a tilde fence\n~~~~\n   ```js\n\
             # indented fence, still code\n   ```\n    ```\n\
             ## [ ] 5. A real one: four spaces is an indented block, not a fence\n\
             ```text extra words\n## [ ] 6. Code to the end of the file",
            &[1, 5],
        ),
        (
            "## [x] 1. First\n\n- ```sh\n  echo example\n  ```\n\n> ```md\n> ## [ ] 98. Quoted sample\n\
             > ```\n\n1. Step one:\n   ~~~\n   Severity: low\n   ~~~\n\n## [ ] 2. Second\n",
            &[1, 2],
        ),
    ];

    #[test]
    fn findings_are_listed_as_the_phone_shows_them() {
        for (text, shown) in AS_THE_PHONE_READS {
            let found: Vec<u32> = listed(text).iter().map(|f| f.number).collect();
            assert_eq!(found, *shown, "{text:?}");
        }
    }

    #[test]
    fn a_finding_written_to_slip_past_is_told() {
        for line in [
            "## [  ] 2. A wide box",
            "## [ x] 2. A box with a space",
            "## [x ] 2. A box with a space",
            "\u{200B}## [ ] 2. Something unseen first",
            "## [\u{200B}\u{200B}] 2. Something unseen in the box",
            "##\u{00A0}[ ] 2. A space that doesn't break",
            "> ## [ ] 2. Quoted",
            "> > ### [x] 2. Quoted twice",
            "- [ ] 2. A task",
            "* [x] 2) A task done",
            "1. [ ] 2. A numbered task",
            "[ ] 2. A box alone",
            "##2. No space",
        ] {
            let text = format!("## [ ] 1. A bug\n{line}\n");
            assert_eq!(listed(&text).len(), 1, "{line:?}");
            let why = hidden(&text).unwrap_or_else(|| panic!("{line:?} isn't told"));
            assert!(why.starts_with("Line 2 of FINDINGS.md"), "{line:?}: {why}");
        }
        // A heading-like line in indented code, or in a quote's fence that
        // nothing closes, may not read as code to an agent.
        for text in [
            "## [ ] 1. A bug\n\n    ## [ ] 2. In indented code\n",
            "## [ ] 1. A bug\n> ```\n> ## [ ] 2. In a quote's open fence\n",
        ] {
            assert!(hidden(text).is_some(), "{text:?}");
        }
        // Lines like these are ordinary, closed examples too.
        for text in [
            "## [ ] 1. A bug\n## [WIP] notes\n- [ ] run the tests\n[docs]: https://example.com\n",
            "## [ ] 1. A bug\n> quoted words\n1. First step\n2) Second step\n#12 is related\n",
            "## [ ] 1. A bug\n- ```\n  ## [ ] 2. An example\n  ```\n",
            "## [ ] 1. A bug\n> ```\n> ## [ ] 2. An example\n> ```\n",
        ] {
            assert_eq!(hidden(text), None, "{text:?}");
        }
    }
}
