// SPDX-License-Identifier: Apache-2.0
//! Whether an issue about to be filed says what one on GitHub says
//! already. Titles are compared word by word, so "has a long path" and
//! "path is long" read the same, and a name from the code both mention
//! counts toward it. Where both say the same file, near the same line,
//! less is asked of the words, and where they name different files, more.
//! The issues are read from GitHub's lists, which have
//! one the moment it is filed, where search may not yet.
//!
//! What gets past that, a Sort finds, and the issues an agent filed that
//! repeat older ones are closed here.
use crate::CompanionHub;
use crate::github::{
    TRIAGE, Triaged, github_error, item_status, linked_task, project_tasks, repo_key, text,
    timestamp,
};
use crate::github_after::numbers;
use crate::github_run::take_out_fix;
use crate::github_tools::{AGENT_LABEL, DUPLICATE_WINDOW_MS, Posting};
use crate::github_watch::raise_headline;
use ports::github::{Github, GithubError, Method};
use protocol::github::ItemStatus;
use protocol::model::{AttentionKind, Run, Task};
use protocol::time::Timestamp;
use protocol::{ProtocolError, event::Event};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Pages of a hundred read of the open issues, and of the closed ones.
const PAGES: u32 = 3;
/// Titles this alike may say the same thing: the agent reads the issue
/// before it files.
const ALIKE: f64 = 0.5;
/// As alike, when both name the same thing in the code.
const ALIKE_NAMED: f64 = 0.25;
/// Titles this alike say the same thing, with nobody left to read them.
const SAME: f64 = 0.8;
/// Lines this close in one file are taken for the same place.
pub(crate) const NEAR: u32 = 12;
/// The most an agent is shown at once.
pub(crate) const MAX_SHOWN: usize = 5;

/// Words that say nothing about which problem it is.
const FILLER: &[&str] = &[
    "a", "about", "after", "all", "also", "an", "and", "any", "are", "as", "at", "be", "been",
    "before", "but", "by", "can", "cannot", "could", "do", "does", "doesn", "don", "for", "from",
    "has", "have", "if", "in", "into", "is", "it", "its", "isn", "no", "not", "of", "on", "or",
    "out", "should", "so", "some", "still", "than", "that", "the", "their", "them", "then",
    "there", "they", "this", "to", "too", "up", "very", "was", "wasn", "when", "where", "which",
    "while", "will", "with", "won", "would",
];

/// Words, as `words` keeps them, that say what kind of problem it is but
/// not which: two problems that share only these are two.
const GENERIC: &[&str] = &[
    "break", "broken", "bug", "crash", "drop", "dropp", "error", "fail", "failure", "handl",
    "handle", "invalid", "issu", "issue", "lose", "loses", "losing", "lost", "miss", "panic",
    "panick", "problem", "test", "unhandl", "unwrap", "unwrapp", "wrong",
];

/// An issue on GitHub, open or closed lately.
#[derive(Debug, Clone)]
pub(crate) struct Known {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub open: bool,
    words: BTreeSet<String>,
    names: BTreeSet<String>,
    places: BTreeSet<(String, Option<u32>)>,
}

impl Known {
    fn read(item: &Value) -> Option<Known> {
        let title = text(&item["title"]);
        Some(Known {
            number: item["number"].as_u64()?,
            url: text(&item["html_url"]),
            open: item["state"] == "open",
            words: words(&title),
            names: names(&text(&item["body"])),
            places: places(&text(&item["body"])),
            title,
        })
    }

    /// Whether its title is `title`, but for case and spacing.
    pub(crate) fn titled(&self, title: &str) -> bool {
        let squash = |s: &str| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        squash(&self.title) == squash(title)
    }
}

/// The open issues, and those closed within the window Øphio counts one
/// the same finding, most recently updated first. Pull requests aren't.
pub(crate) async fn known_issues(
    gh: &dyn Github,
    base: &str,
    now: Timestamp,
) -> Result<Vec<Known>, GithubError> {
    let since = now.0 - DUPLICATE_WINDOW_MS;
    let mut known = Vec::new();
    for state in ["open", "closed"] {
        'pages: for page in 1..=PAGES {
            let list = format!("{base}/issues?state={state}&sort=updated&per_page=100&page={page}");
            let items = gh.rest(Method::Get, &list, None).await?;
            let items = items.as_array().cloned().unwrap_or_default();
            for item in &items {
                let closed = state == "closed";
                // Closing updates it, so one updated before then closed
                // before then.
                if closed && timestamp(&item["updated_at"]).0 < since {
                    break 'pages;
                }
                if item.get("pull_request").is_some()
                    || closed && timestamp(&item["closed_at"]).0 < since
                {
                    continue;
                }
                known.extend(Known::read(item));
            }
            if items.len() < 100 {
                break;
            }
        }
    }
    Ok(known)
}

/// An issue that reads like the one about to be filed.
pub(crate) struct Alike<'a> {
    pub known: &'a Known,
    /// The share of the titles' words they have in common.
    pub words: f64,
    /// Whether both name the same file or name in the code.
    pub named: bool,
    /// Whether both name the same place: a file, and lines near each other.
    pub place: bool,
    /// Whether both name places and none is the same: other files, or lines
    /// far apart in one.
    elsewhere: bool,
}

impl Alike<'_> {
    /// Whether it says the same thing: its title does, and they don't name
    /// different places. Pointing to the same lines makes an issue worth
    /// reading, not the same: two problems can sit a few lines apart.
    pub(crate) fn same(&self) -> bool {
        self.words >= SAME && !self.elsewhere
    }
}

/// Those of `known` that read like an issue titled `title` saying `body`,
/// most alike first.
pub(crate) fn alike<'a>(known: &'a [Known], title: &str, body: &str) -> Vec<Alike<'a>> {
    let (mine, named, here) = (words(title), names(body), places(body));
    let mut alike: Vec<Alike> = (known.iter())
        .map(|known| {
            let at = |near: fn(Option<u32>, Option<u32>) -> bool| {
                (known.places.iter())
                    .any(|(path, line)| (here.iter()).any(|(p, l)| p == path && near(*line, *l)))
            };
            Alike {
                known,
                words: share(&mine, &known.words),
                named: !named.is_disjoint(&known.names),
                place: at(near),
                elsewhere: !at(close) && !here.is_empty() && !known.places.is_empty(),
            }
        })
        .filter(|a| a.words >= ALIKE || (a.named || a.place) && a.words >= ALIKE_NAMED)
        .collect();
    alike.sort_by(|a, b| {
        let score = |a: &Alike| {
            let bonus = |yes: bool| if yes { ALIKE_NAMED } else { 0.0 };
            a.words + bonus(a.named) + bonus(a.place)
        };
        score(b).total_cmp(&score(a))
    });
    alike
}

/// A finding as duplicates are judged: what it is called, where it goes
/// wrong and how.
pub(crate) struct Failure<'a> {
    pub title: &'a str,
    pub path: Option<&'a str>,
    pub line: Option<u32>,
    /// What goes wrong, in the finding's own words: its scenario, or failing
    /// that its body.
    pub what: &'a str,
}

/// Whether two findings are one problem: the same failure in the same place,
/// not only alike titles. In different files they are two, however alike
/// their titles; in one file, far apart, too. At lines near each other, what
/// the titles say beyond the kind of problem must read nearly the same, or
/// what goes wrong must be told in the same words: "panics on empty input"
/// and "panics on a malformed header" are two, however alike their
/// scenarios, and so are "export of an empty note crashes" and "export of a
/// large note crashes". Without lines to go by, they must read nearly the
/// same.
pub(crate) fn same_failure(a: &Failure<'_>, b: &Failure<'_>) -> bool {
    let path = |f: &Failure| f.path.map(tidy).filter(|p| !p.is_empty());
    let (titles, what) = (
        share(&specific(a.title), &specific(b.title)),
        share(&specific(a.what), &specific(b.what)),
    );
    let told = |f: &Failure| !f.what.trim().is_empty();
    let unnamed = |f: &Failure| specific(f.title).is_empty();
    let worded = share(&words(a.title), &words(b.title)) >= SAME
        && (titles >= ALIKE || unnamed(a) && unnamed(b));
    let nearly = worded || told(a) && told(b) && what >= SAME;
    match (path(a), path(b)) {
        (Some(x), Some(y)) if x != y => false,
        (Some(_), Some(_)) => match (a.line, b.line) {
            (Some(_), Some(_)) if near(a.line, b.line) => {
                titles >= SAME || told(a) && told(b) && what >= 1.0
            }
            (Some(_), Some(_)) => false,
            _ => nearly,
        },
        _ => nearly,
    }
}

/// Whether two lines are in one place: both known, and near each other.
fn near(a: Option<u32>, b: Option<u32>) -> bool {
    a.zip(b).is_some_and(|(a, b)| a.abs_diff(b) <= NEAR)
}

/// Whether two lines may be in one place: near each other, or one not known.
fn close(a: Option<u32>, b: Option<u32>) -> bool {
    match (a, b) {
        (Some(_), Some(_)) => near(a, b),
        _ => true,
    }
}

/// A path as one spelling: forward slashes, no leading `./` or `/`, no case.
fn tidy(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    let mut path = path.as_str();
    while let Some(rest) = path.strip_prefix("./").or_else(|| path.strip_prefix('/')) {
        path = rest;
    }
    path.to_lowercase()
}

/// The repository files a body points to, each with the line when it gives
/// one: GitHub links to a file (`.../blob/<revision>/src/a.rs#L42`), and
/// paths like `src/a.rs:42` in prose or backticks. Other links are none.
fn places(body: &str) -> BTreeSet<(String, Option<u32>)> {
    let apart = |c: char| c.is_whitespace() || "`'\"()[]<>{},;|".contains(c);
    let mut places = BTreeSet::new();
    for token in body.split(apart) {
        let token = token.trim_end_matches(['.', ',', ':', ';', '!', '?']);
        let found = if token.contains("://") {
            link_place(token)
        } else {
            written_place(token)
        };
        places.extend(found.filter(|(path, _)| !path.is_empty()));
    }
    places
}

/// The file and line of a GitHub link to a file.
fn link_place(link: &str) -> Option<(String, Option<u32>)> {
    let (link, fragment) = link.split_once('#').unwrap_or((link, ""));
    let link = link.split('?').next().unwrap_or(link);
    let (_, rest) = link.split_once("://")?;
    let mut parts = rest.split('/');
    if !parts.next()?.ends_with("github.com") {
        return None;
    }
    // Owner and repository, then `blob` and the revision.
    let mut parts = parts.skip(2);
    if parts.next()? != "blob" {
        return None;
    }
    let path = parts.skip(1).collect::<Vec<_>>().join("/");
    shaped(&path)?;
    let line = fragment.strip_prefix('L').map(digits);
    Some((tidy(&path), line.and_then(|l| l)))
}

/// The file and line of a path written in prose, like `src/a.rs:42:5`.
fn written_place(word: &str) -> Option<(String, Option<u32>)> {
    let (path, line) = word.split_once(':').unwrap_or((word, ""));
    shaped(path)?;
    Some((tidy(path), digits(line)))
}

/// The number a text starts with, if any.
fn digits(text: &str) -> Option<u32> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    text[..end].parse().ok()
}

/// Whether `path` has folders and ends in a file name with an extension.
fn shaped(path: &str) -> Option<()> {
    let plain = |c: char| c.is_alphanumeric() || "_-./".contains(c);
    let (_, file) = path.rsplit_once('/')?;
    let (stem, ending) = file.rsplit_once('.')?;
    let ok = !stem.is_empty()
        && (1..=5).contains(&ending.len())
        && ending.chars().all(|c| c.is_ascii_alphabetic())
        && path.chars().all(plain);
    ok.then_some(())
}

/// What two sets have in common, as a share of all they have.
fn share(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let all = a.union(b).count();
    match all {
        0 => 0.0,
        _ => a.intersection(b).count() as f64 / all as f64,
    }
}

/// What a title is about: its words, but for filler, each without the
/// ending that makes "tests" and "failing" other words than "test" and
/// "fail".
fn words(title: &str) -> BTreeSet<String> {
    (title.to_lowercase())
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 1 && !FILLER.contains(w))
        .map(|w| stem(w).to_owned())
        .collect()
}

/// The words of a title that say which problem it is, not what kind.
fn specific(title: &str) -> BTreeSet<String> {
    let mut words = words(title);
    words.retain(|w| !GENERIC.contains(&w.as_str()));
    words
}

fn stem(word: &str) -> &str {
    for ending in ["ing", "ed", "es", "s"] {
        if word.len() > ending.len() + 3 && word.ends_with(ending) && !word.ends_with("ss") {
            return &word[..word.len() - ending.len()];
        }
    }
    word
}

/// The files and names from the code a body mentions: anything in
/// backticks, and words shaped like a path, a file or an identifier, each
/// without its folders or line number.
fn names(body: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (i, part) in body.split('`').enumerate() {
        let quoted = i % 2 == 1;
        for word in part.split_whitespace() {
            // Links and people are no part of the code.
            if word.contains("://") || word.contains('@') {
                continue;
            }
            let word = word
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .trim_end_matches(|c: char| c.is_ascii_digit() || c == ':');
            let word = word.rsplit('/').next().unwrap_or(word).to_lowercase();
            let shaped = word.contains('_')
                || word.contains("::")
                || (word.split_once('.')).is_some_and(|(stem, ending)| {
                    !stem.is_empty()
                        && (1..=5).contains(&ending.len())
                        && ending.chars().all(|c| c.is_ascii_alphabetic())
                });
            let letters = word.chars().filter(|c| c.is_alphanumeric()).count();
            if letters >= 4 && (shaped || quoted && word.chars().any(char::is_alphabetic)) {
                names.insert(word);
            }
        }
    }
    names
}

impl CompanionHub {
    /// Closes as duplicates the issues of `repeated`, each with the older
    /// one it repeats, that an agent filed as the person's account on their
    /// repository and that no agent has started on: a fix of one waiting in
    /// Next up is taken out first. The rest wait in Needs you, as the Sort
    /// left them. Says what it closed, and what it couldn't.
    pub(crate) async fn close_repeats(
        &self,
        run: &Run,
        task: &Task,
        repeated: &[(u64, u64)],
    ) -> Result<(), ProtocolError> {
        let posting = self.posting(task).await;
        let (mut closed, mut notes) = (Vec::new(), Vec::new());
        for &(number, older) in repeated {
            let closing = match &posting {
                Ok(p) => self.close_repeat(run, task, p, number, older).await,
                Err(e) => Err(Some(e.message.clone())),
            };
            match closing {
                Ok(()) => {
                    closed.push(number);
                    notes.push(format!("#{number} repeats #{older}."));
                }
                Err(Some(why)) => notes.push(format!(
                    "#{number} repeats #{older} and stays open, waiting for you: {why}"
                )),
                // A person's issue, or one elsewhere: Needs you asks them.
                Err(None) => {}
            }
        }
        if notes.is_empty() {
            return Ok(());
        }
        let project_id = task.project_id;
        let now = self.inner.clock.now();
        let headline = match closed.len() {
            0 => "Duplicates found, not closed".to_owned(),
            1 => format!("Closed {} as a duplicate", numbers(&closed)),
            _ => format!("Closed {} as duplicates", numbers(&closed)),
        };
        self.write(|tx| {
            let mut triage: Vec<Triaged> = tx.read().get(TRIAGE, project_id)?.unwrap_or_default();
            triage.retain(|t| t.pr || !closed.contains(&t.number));
            tx.put(TRIAGE, project_id, &triage)?;
            tx.emit(Event::GithubChanged { project_id })?;
            let body = Some(notes.join("\n"));
            raise_headline(
                tx,
                task.task_id,
                AttentionKind::Completed,
                protocol::notifications::AlertTopic::Issues,
                &headline,
                body,
                now,
            )
        })
    }

    /// Closes issue `number` as a duplicate of `older`. Err(None) when it
    /// isn't Øphio's to close; Err(Some) says why it wasn't.
    async fn close_repeat(
        &self,
        run: &Run,
        task: &Task,
        p: &Posting,
        number: u64,
        older: u64,
    ) -> Result<(), Option<String>> {
        if !p.theirs() {
            return Err(None);
        }
        let unread = |_| Some("Øphio couldn't read it on GitHub.".to_owned());
        let path = format!("{}/issues/{number}", p.base());
        let issue = p.gh.rest(Method::Get, &path, None).await.map_err(unread)?;
        let agents = (issue["labels"].as_array().into_iter().flatten())
            .any(|l| l["name"] == AGENT_LABEL)
            || text(&issue["body"]).contains(&format!("in Øphio, working for @{}", p.login));
        let filed_here = text(&issue["user"]["login"]).eq_ignore_ascii_case(&p.login) && agents;
        if issue["state"] != "open" || issue.get("pull_request").is_some() || !filed_here {
            return Err(None);
        }
        let older_path = format!("{}/issues/{older}", p.base());
        let original =
            p.gh.rest(Method::Get, &older_path, None)
                .await
                .map_err(unread)?;
        let id = original["id"].as_u64().filter(|_| {
            older < number && original["state"] == "open" && original.get("pull_request").is_none()
        });
        let Some(id) = id else {
            return Err(Some(format!("#{older} isn't an older open issue.")));
        };
        let here = repo_key(&p.repo.owner, &p.repo.name);
        let now = self.inner.clock.now();
        let why = format!("Taken out of Next up: its issue repeats #{older}");
        let free = self.write(|tx| {
            let tasks = project_tasks(&tx.read(), task.project_id)?;
            let Some(fix) = linked_task(&tasks, &here, number, false) else {
                return Ok(true);
            };
            if item_status(fix) != Some(ItemStatus::Queued) {
                return Ok(false);
            }
            take_out_fix(self, tx, task.project_id, fix.task_id, &why, now)
        });
        match free {
            Ok(true) => {}
            Ok(false) => return Err(Some("an agent has already taken it on.".into())),
            Err(e) => return Err(Some(e.message)),
        }
        let close =
            json!({ "state": "closed", "state_reason": "duplicate", "duplicate_issue_id": id });
        let closed = match p.gh.rest(Method::Patch, &path, Some(close)).await {
            // Where GitHub doesn't take a duplicate's reason, not planned.
            Err(e) if e.status() == Some(422) => {
                let close = json!({ "state": "closed", "state_reason": "not_planned" });
                p.gh.rest(Method::Patch, &path, Some(close)).await
            }
            closed => closed,
        };
        closed.map_err(|e| Some(github_error(e).message))?;
        let by = self
            .this_run(run.run_id, task)
            .map_err(|e| Some(e.message))?;
        let said = format!(
            "Duplicate of #{older}\n\n🤖 Closed by {by} in Øphio, working for @{}",
            p.login
        );
        // GitHub reads the comment's first line as well; it's closed either way.
        let comment = json!({ "body": said });
        let comments = format!("{path}/comments");
        let _ = p.gh.rest(Method::Post, &comments, Some(comment)).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn issue(number: u64, title: &str, body: &str) -> Known {
        let item = json!({
            "number": number, "title": title, "body": body, "state": "open",
            "html_url": format!("https://github.com/octo/app/issues/{number}"),
        });
        Known::read(&item).unwrap()
    }

    #[test]
    fn a_title_said_another_way_is_the_same_issue() {
        let known = [
            issue(
                18,
                "Force-stop tests fail when the temporary directory path is long",
                "",
            ),
            issue(19, "Settings page forgets the theme", ""),
        ];
        let title = "Force-stop tests fail when the temporary directory has a long path";
        let alike = alike(&known, title, "");
        let [first] = &alike[..] else {
            panic!(
                "{:?}",
                alike.iter().map(|a| a.known.number).collect::<Vec<_>>()
            )
        };
        assert_eq!(first.known.number, 18);
        assert!(first.same());
        assert!(
            known[0].titled("  force-stop TESTS fail when the temporary   directory path is long")
        );
        assert!(!known[0].titled(title));
    }

    #[test]
    fn half_the_words_or_a_shared_name_is_worth_reading() {
        let known = [
            issue(3, "Login fails on Windows", ""),
            issue(
                4,
                "Crash while saving notes",
                "Panics in `notes::save` at src/notes.rs:42.",
            ),
            issue(5, "Dark mode colours are off", "See `theme.css`."),
        ];
        // Half the words: worth reading, but not the same.
        let alike_3 = alike(&known, "Login fails on Linux", "");
        assert_eq!(
            alike_3.iter().map(|a| a.known.number).collect::<Vec<_>>(),
            [3]
        );
        assert!(!alike_3[0].same());
        // A quarter of the words, and the same file.
        let found = alike(
            &known,
            "Saving notes drops the last line",
            "In crates/app/src/notes.rs:10.",
        );
        assert_eq!(
            found.iter().map(|a| a.known.number).collect::<Vec<_>>(),
            [4]
        );
        assert!(found[0].named);
        // The same file alone isn't enough.
        assert!(alike(&known, "Export is slow", "See `theme.css`.").is_empty());
    }

    #[test]
    fn names_are_files_and_identifiers_not_prose() {
        let body = "It fails in `force_stop_tests.rs` (see crates/services/src/force_stop_tests.rs:88:5), \
            in `Hub::submit`, with SUN_LEN. See https://example.com/a_b.html, e.g. this, v1.2, or @me_too.";
        let names = names(body);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["force_stop_tests.rs", "hub::submit", "sun_len"]
        );
    }

    #[test]
    fn a_link_or_written_path_to_the_same_place_is_worth_reading() {
        let known = [
            issue(
                7,
                "Saving drops the final line",
                "See https://github.com/octo/app/blob/abc/src/save.rs#L44-L50 for it.",
            ),
            issue(
                8,
                "Saving drops the final line",
                "Look in `src/export.rs:40`.",
            ),
        ];
        let found = alike(
            &known,
            "Last line is lost when saving a note",
            "It goes wrong at `src/save.rs:40`.",
        );
        let [first] = &found[..] else {
            panic!(
                "{:?}",
                found.iter().map(|a| a.known.number).collect::<Vec<_>>()
            )
        };
        assert_eq!(first.known.number, 7);
        // Shown to the agent to read, not taken for the same unread.
        assert!(first.place && !first.same());
    }

    #[test]
    fn distinct_issues_at_near_lines_are_not_refused() {
        let known = [issue(
            31,
            "Export of an empty note crashes",
            "At `src/export.rs:40`.",
        )];
        let found = alike(
            &known,
            "Export of a large note crashes",
            "At `src/export.rs:48`.",
        );
        let [a] = &found[..] else { panic!() };
        assert!(a.place && !a.same());
    }

    #[test]
    fn alike_titles_in_different_files_are_not_the_same() {
        let known = [issue(
            9,
            "Saving a note drops the last line",
            "Happens in src/export.rs:10.",
        )];
        let found = alike(
            &known,
            "Saving a note drops the last line",
            "Happens in src/save.rs:10.",
        );
        assert_eq!(found.len(), 1);
        assert!(!found[0].place && !found[0].same());
        // With no place to go by on one side, the titles decide.
        let found = alike(&known, "Saving a note drops the last line", "");
        assert!(found[0].same());
    }

    #[test]
    fn places_are_repository_files_with_a_line_when_given() {
        let body = "See https://github.com/octo/app/blob/abc123/./src/Save.rs#L42-L50, \
            `crates/a/b.rs:7:3`, (docs/guide.md), .\\win\\path.rs:9. Not these: \
            https://example.com/blob/x/src/other.rs#L1, https://github.com/octo/app/issues/3, \
            v1.2/notes, e.g. and/or, file.rs:5, a/b.verylong.";
        assert_eq!(
            places(body).into_iter().collect::<Vec<_>>(),
            [
                ("crates/a/b.rs".to_owned(), Some(7)),
                ("docs/guide.md".to_owned(), None),
                ("src/save.rs".to_owned(), Some(42)),
            ]
        );
    }

    fn failure<'a>(title: &'a str, path: Option<&'a str>, line: u32, what: &'a str) -> Failure<'a> {
        let line = Some(line).filter(|&l| l > 0);
        Failure {
            title,
            path,
            line,
            what,
        }
    }

    #[test]
    fn one_failure_in_one_place_is_one_problem_however_it_is_worded() {
        let a = failure(
            "Save loses data",
            Some("./Src\\save.rs"),
            40,
            "Saving a note twice in a row drops the last line",
        );
        let b = failure(
            "Notes truncated",
            Some("src/save.rs"),
            52,
            "Saving a note twice in a row drops the last line",
        );
        assert!(same_failure(&a, &b));
        // Somewhere else in the file, or in another file, it is another.
        let far = failure("Save loses data", Some("src/save.rs"), 252, b.what);
        assert!(!same_failure(&a, &far));
        let other = failure("Save loses data", Some("src/load.rs"), 40, a.what);
        assert!(!same_failure(&a, &other));
    }

    #[test]
    fn without_paths_only_nearly_the_same_words_are_one_problem() {
        let a = failure("Saving a note drops the last line", None, 0, "");
        let b = failure("Saving a note drops last line", None, 0, "");
        let c = failure("Saving a note is slow", None, 0, "");
        assert!(same_failure(&a, &b));
        assert!(!same_failure(&a, &c));
    }

    #[test]
    fn distinct_problems_near_each_other_are_two() {
        // Alike only in the kind of problem, at lines near each other.
        let empty = failure("Unwrap panics on empty input", Some("src/save.rs"), 9, "");
        let header = failure(
            "Unwrap panics on a malformed header",
            Some("src/save.rs"),
            18,
            "",
        );
        assert!(!same_failure(&empty, &header));
        // However alike what goes wrong is told.
        let empty = failure(
            "Unwrap panics on empty input",
            Some("src/save.rs"),
            9,
            "parse_save() with an empty buffer; parse_save unwraps read_header() which returns None.",
        );
        let header = failure(
            "Unwrap panics on a malformed header",
            Some("src/save.rs"),
            18,
            "parse_save() with a buffer whose header is malformed; parse_save unwraps read_header() which returns None.",
        );
        assert!(!same_failure(&empty, &header));
        // In one file, near each other or without lines to go by.
        for line in [0, 30] {
            let save = failure("Missing test for save error", Some("src/io.rs"), line, "");
            let load = failure("Missing test for load error", Some("src/io.rs"), 31, "");
            assert!(!same_failure(&save, &load));
            assert!(same_failure(&save, &save));
        }
        // Titles sharing half what they say, at lines near each other.
        let large = |line| {
            failure(
                "Export of a large note crashes",
                Some("src/export.rs"),
                line,
                "",
            )
        };
        let empty = failure(
            "Export of an empty note crashes",
            Some("src/export.rs"),
            4,
            "",
        );
        assert!(!same_failure(&large(2), &empty));
        // Nearly the same title there is the same problem.
        let shorter = failure("Export of a large note crash", Some("src/export.rs"), 6, "");
        assert!(same_failure(&large(2), &shorter));
        // Without a line on one side, a file alone isn't the same place.
        let a = large(0);
        let b = failure(
            "Export of an empty note crashes",
            Some("src/export.rs"),
            4,
            "",
        );
        assert!(!same_failure(&a, &b));
    }

    #[test]
    fn issues_naming_one_file_or_alike_in_kind_are_not_the_same() {
        // A file named with no line points nowhere in particular.
        let known = [issue(
            21,
            "Export of an empty note crashes",
            "It goes wrong in src/export.rs.",
        )];
        let found = alike(
            &known,
            "Export of a large note crashes",
            "It goes wrong in src/export.rs.",
        );
        let [a] = &found[..] else { panic!() };
        assert!(!a.place && !a.same());
        // Near lines, alike only in the kind of problem.
        let known = [issue(
            22,
            "Unwrap panics on empty input",
            "At `src/save.rs:9`.",
        )];
        let found = alike(
            &known,
            "Unwrap panics on a malformed header",
            "At `src/save.rs:18`.",
        );
        assert!(found.iter().all(|a| !a.same()));
        // The same title naming the same file is, as before.
        let found = alike(
            &known,
            "Unwrap panics on empty input",
            "Found in src/save.rs again.",
        );
        assert!(found[0].same());
    }
}
