// SPDX-License-Identifier: Apache-2.0
//! The computer's accounts: signed in once, given to every task it starts.
//! Keys and tokens stay behind this trait; services see names and states.
use crate::harness::RunEnv;
use async_trait::async_trait;
use protocol::TaskId;
use protocol::accounts::{AccountAction, AccountsView};
use protocol::capability::HarnessKind;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Debug, thiserror::Error)]
pub enum AccountsError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    /// Another sign-in is running.
    #[error("{0}")]
    Busy(String),
    /// The task's agent account is paused or signed out.
    #[error("{0}")]
    NotReady(String),
    #[error("{0}")]
    Failed(String),
}

/// What one run receives from Accounts.
#[derive(Default, Clone)]
pub struct RunAccounts {
    pub env: RunEnv,
    /// Every value in `env` and every token the run can reach, masked in what
    /// the run says before it is stored or sent.
    pub secrets: Vec<String>,
    /// Files and folders `env` names, such as a command-line tool's sign-in
    /// folder, which a run held in a sandbox is let read.
    pub files: Vec<PathBuf>,
}
impl std::fmt::Debug for RunAccounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunAccounts")
            .field("env", &self.env)
            .field("secrets", &self.secrets.len())
            .field("files", &self.files)
            .finish()
    }
}

/// Replaces each secret in `text` with a marker. Values shorter than
/// [`MASKED_FROM`] are left: they match ordinary words more often than leaks,
/// and Accounts accepts no key that short. Every secret is found in the text
/// as it was given before any is replaced, so a secret that begins a longer
/// one, or overlaps it, can't hide only part of it and leave the rest showing.
pub fn mask(text: &mut String, secrets: &[String]) {
    let found = whole(text, secrets);
    redact(text, found);
}
pub const MASKED_FROM: usize = 8;
pub const MASK: &str = "[hidden by Ophio]";

/// Whether Accounts can hold `value` and hide it wherever it shows: long
/// enough to mask, and only characters that JSON, HTML and shell quoting
/// write as themselves, so an escaped copy is still the same text.
pub fn maskable(value: &str) -> bool {
    value.len() >= MASKED_FROM
        && value.len() <= 8192
        && value
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"\"\\'`<>&".contains(&b))
}

/// The fewest leading characters of a cut secret that are hidden. Fewer than
/// this are the start of many words, and of every key from the same provider.
pub const CUT_SHOWN: usize = 4;

/// [`mask`] for whole text that may have been cut to fit: the start of a
/// secret left where it was cut, at the end of the text or just before a
/// `…`, is hidden too. Cut and whole secrets are found in the same unmasked
/// text, so hiding one secret can't stop a longer one's start being seen.
pub fn mask_cut(text: &mut String, secrets: &[String]) {
    mask_cut_codes(text, secrets, &[]);
}

/// [`mask_cut`], and each of `codes` hidden wherever it is whole, however
/// short. A sign-in code is a few characters, too short for [`mask`]
/// everywhere, so it is hidden only in what the run it was typed for says.
pub fn mask_cut_codes(text: &mut String, secrets: &[String], codes: &[String]) {
    let found = cut_codes_found(text, secrets, codes, MASKED_FROM);
    redact(text, found);
}

/// [`mask_cut_codes`] for text hidden in before: what is hidden stays as it
/// is. A code can be a word of the marker itself, and hidden again each time
/// the text would only grow. Each of `secrets` is as the text shows one the
/// caller found long enough to hide, so it is hidden whole and cut however
/// short it shows.
pub fn mask_again(text: &mut String, secrets: &[String], codes: &[String]) {
    let hidden: Vec<(usize, usize)> = (text.match_indices(MASK))
        .map(|(at, mask)| (at, at + mask.len()))
        .collect();
    let mut found = cut_codes_found(text, secrets, codes, 1);
    found.retain(|&(at, end)| {
        let before = hidden.partition_point(|&(from, _)| from <= at);
        !before.checked_sub(1).is_some_and(|i| end <= hidden[i].1)
    });
    redact(text, found);
}

/// Where [`mask_cut_codes`] hides what is in `text`, for secrets of at least
/// `shortest` bytes.
fn cut_codes_found(
    text: &str,
    secrets: &[String],
    codes: &[String],
    shortest: usize,
) -> Vec<(usize, usize)> {
    let mut found = whole_from(text, secrets, shortest);
    found.extend(whole_from(text, codes, 1));
    let mut cuts: Vec<usize> = text.match_indices('…').map(|(at, _)| at).collect();
    cuts.push(text.len());
    let mut longest = vec![0; cuts.len()];
    for secret in secrets.iter().filter(|s| s.len() >= shortest) {
        for (most, n) in
            longest
                .iter_mut()
                .zip(cut_starts(text.as_bytes(), secret.as_bytes(), &cuts))
        {
            *most = (*most).max(n);
        }
    }
    for (cut, n) in cuts.into_iter().zip(longest) {
        if n >= CUT_SHOWN {
            // A start that begins inside a character begins at that character.
            let mut at = cut - n;
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            found.push((at, cut));
        }
    }
    found
}

/// Every place a whole secret is in `text`, in order, those that overlap
/// joined as [`redact`] joins them. Each secret is searched for in one pass
/// and joined in as it is found, so what is kept grows with the length of the
/// text, however many times the secrets in it overlap.
fn whole(text: &str, secrets: &[String]) -> Vec<(usize, usize)> {
    whole_from(text, secrets, MASKED_FROM)
}

/// [`whole`] for secrets of at least `shortest` bytes.
fn whole_from(text: &str, secrets: &[String], shortest: usize) -> Vec<(usize, usize)> {
    let mut joined = Vec::new();
    for secret in secrets.iter().filter(|s| s.len() >= shortest.max(1)) {
        let search = Search::new(secret.as_bytes());
        let mut found = Vec::new();
        let mut n = 0;
        // A secret is whole text, so where its bytes are is where it is.
        for (i, &byte) in text.as_bytes().iter().enumerate() {
            n = search.next(n, byte);
            if n == secret.len() {
                add(&mut found, (i + 1 - n, i + 1));
            }
        }
        joined = merge(joined, found);
    }
    joined
}

/// Adds a range that starts no earlier than the last of `joined`, joined to
/// it if they overlap.
fn add(joined: &mut Vec<(usize, usize)>, (at, end): (usize, usize)) {
    match joined.last_mut() {
        Some(last) if at < last.1 => last.1 = last.1.max(end),
        _ => joined.push((at, end)),
    }
}

/// Two lists of ranges, each in order and joined, as one.
fn merge(a: Vec<(usize, usize)>, b: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if a.is_empty() || b.is_empty() {
        return if a.is_empty() { b } else { a };
    }
    let mut joined = Vec::with_capacity(a.len().max(b.len()));
    let (mut a, mut b) = (a.into_iter().peekable(), b.into_iter().peekable());
    while let Some(next) = match (a.peek(), b.peek()) {
        (Some(x), Some(y)) if y < x => b.next(),
        (Some(_), _) => a.next(),
        (None, _) => b.next(),
    } {
        add(&mut joined, next);
    }
    joined
}

/// A secret searched for a byte at a time, in one pass however it repeats
/// itself.
struct Search<'a> {
    secret: &'a [u8],
    /// `border[n]`: the longest start of `secret` that also ends its first
    /// `n` bytes, shorter than `n`.
    border: Vec<usize>,
}
impl<'a> Search<'a> {
    fn new(secret: &'a [u8]) -> Self {
        let mut border = vec![0; secret.len() + 1];
        let mut k = 0;
        for i in 1..secret.len() {
            while k > 0 && secret[i] != secret[k] {
                k = border[k];
            }
            if secret[i] == secret[k] {
                k += 1;
            }
            border[i + 1] = k;
        }
        Self { secret, border }
    }
    /// The longest start of the secret that ends at `byte`, from `n`, the
    /// longest that ended just before it. All of it is the whole secret.
    fn next(&self, mut n: usize, byte: u8) -> usize {
        if n == self.secret.len() {
            n = self.border[n];
        }
        while n > 0 && self.secret[n] != byte {
            n = self.border[n];
        }
        if self.secret.get(n) == Some(&byte) {
            n += 1;
        }
        n
    }
}

/// Replaces each of `ranges` in `text` with one marker, those that overlap
/// joined first.
fn redact(text: &mut String, mut ranges: Vec<(usize, usize)>) {
    if ranges.is_empty() {
        return;
    }
    ranges.sort_unstable();
    let mut joined: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for range in ranges {
        add(&mut joined, range);
    }
    // Built forward into new text: replacing each range where it is would
    // move all the text after it, once for every range.
    let mut masked = String::with_capacity(text.len());
    let mut kept = 0;
    for (at, end) in joined {
        masked.push_str(&text[kept..at]);
        masked.push_str(MASK);
        kept = end;
    }
    masked.push_str(&text[kept..]);
    *text = masked;
}

/// For each of `cuts`, in order, the length of the longest start of `secret`,
/// short of all of it, that ends there. One pass over `text`, however many
/// cuts and however the secret repeats itself.
fn cut_starts(text: &[u8], secret: &[u8], cuts: &[usize]) -> Vec<usize> {
    let search = Search::new(secret);
    let mut starts = Vec::with_capacity(cuts.len());
    let mut cuts = cuts.iter().peekable();
    // The longest start of `secret` that ends where the pass has reached.
    let mut n = 0;
    for at in 0..=text.len() {
        while cuts.next_if(|&&cut| cut == at).is_some() {
            starts.push(if n == secret.len() {
                search.border[n]
            } else {
                n
            });
        }
        let Some(&byte) = text.get(at) else { break };
        n = search.next(n, byte);
    }
    starts
}

/// Masks every string in a JSON value, its object keys included, before
/// anything shown is drawn from it.
pub fn mask_value(value: &mut serde_json::Value, secrets: &[String]) {
    mask_strings(value, &mut |text| mask(text, secrets));
}

/// Applies `hide` to every string in a JSON value, its object keys included.
pub fn mask_strings(value: &mut serde_json::Value, hide: &mut dyn FnMut(&mut String)) {
    use serde_json::Value;
    match value {
        Value::String(text) => hide(text),
        Value::Array(items) => items.iter_mut().for_each(|v| mask_strings(v, hide)),
        Value::Object(fields) => {
            let mut masked = serde_json::Map::new();
            for (mut key, mut field) in std::mem::take(fields) {
                hide(&mut key);
                mask_strings(&mut field, hide);
                masked.insert(key, field);
            }
            *fields = masked;
        }
        _ => {}
    }
}

/// Where text arriving in pieces can be let out: the end of `text` that could
/// be the start of a secret is held back until the next piece shows whether it
/// is one. That includes a whole secret that a longer one begins with. `text`
/// is not yet masked.
pub fn held_from(text: &str, secrets: &[String]) -> usize {
    let mut from = text.len();
    for secret in secrets.iter().filter(|s| s.len() >= MASKED_FROM) {
        // Only an end shorter than the secret is held: a whole one is masked.
        // The longest start of it that ends the text, found in one pass.
        let search = Search::new(secret.as_bytes());
        let end = &text.as_bytes()[text.len().saturating_sub(secret.len() - 1)..];
        let n = end.iter().fold(0, |n, &byte| search.next(n, byte));
        from = from.min(text.len() - n);
    }
    from
}

/// [`mask`] for text arriving in pieces: masks what can be let out and
/// returns the end held back, from [`held_from`], to put before the next
/// piece. A secret that runs into the held end, or into one that does, is
/// held whole with it. The text is searched once, so the work grows with its
/// length and no faster however its secrets overlap.
pub fn mask_streamed(text: &mut String, secrets: &[String]) -> String {
    let mut found = whole(text, secrets);
    let mut from = held_from(text, secrets);
    // If the last run of overlapping secrets that starts before the held end
    // reaches past it, all of the run is held.
    if let Some(&(start, reach)) = found.iter().take_while(|&&(at, _)| at < from).last()
        && reach > from
    {
        from = start;
    }
    let held = text.split_off(from);
    found.retain(|&(_, end)| end <= from);
    redact(text, found);
    held
}

/// The variable that lets `ophio token`, run by a task, renew what the task
/// was given. Its value is the run's own and is masked like the rest.
pub const RENEW_ENV: &str = "OPHIO_ACCOUNTS_RUN";

#[async_trait]
pub trait Accounts: Send + Sync + std::fmt::Debug + 'static {
    async fn view(&self) -> AccountsView;
    /// `by` names the paired device or local user, for the history.
    async fn act(&self, action: AccountAction, by: &str) -> Result<(), AccountsError>;
    /// The environment for a task about to start on `harness`, or why it may
    /// not start: its agent account is paused.
    async fn for_run(
        &self,
        harness: HarnessKind,
        task: TaskId,
    ) -> Result<RunAccounts, AccountsError>;
    /// A key or fresh access token for `account`, which `task` must have been
    /// given when it started, for `ophio token` run inside the task.
    async fn renew(&self, task: TaskId, account: &str) -> Result<String, AccountsError>;
    /// Every key and token held since Accounts started, masked in every run:
    /// a key replaced or a token renewed while a run is going is hidden in it
    /// too, and so is the value it replaced.
    fn secrets(&self) -> Arc<Vec<String>>;
    /// The task's run ended; it no longer uses the accounts it was given.
    fn run_ended(&self, task: TaskId);
    /// Changes whenever the view does.
    fn changed(&self) -> watch::Receiver<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where `secret` occurs in `text`, overlapping ones included, as the
    /// search before [`whole`] found them.
    fn occurrences<'a>(
        text: &'a str,
        secret: &'a str,
    ) -> impl Iterator<Item = (usize, usize)> + 'a {
        let mut at = 0;
        std::iter::from_fn(move || {
            let found = at + text.get(at..)?.find(secret)?;
            at = found + text[found..].chars().next().map_or(1, char::len_utf8);
            Some((found, found + secret.len()))
        })
    }

    /// What a reader sees of `pieces` streamed through [`mask_streamed`],
    /// as a run's text arrives, and the held end masked once it ends.
    fn streamed(pieces: &[&str], secrets: &[String]) -> String {
        let (mut shown, mut held) = (String::new(), String::new());
        for piece in pieces {
            let mut text = std::mem::take(&mut held) + piece;
            held = mask_streamed(&mut text, secrets);
            shown += &text;
        }
        mask(&mut held, secrets);
        shown + &held
    }

    #[test]
    fn codes_are_hidden_however_short() {
        let codes = vec!["123456".to_owned(), "den".to_owned()];
        let secrets = vec!["pass-123456-word".to_owned()];
        let mut text = "Invalid code 123456; try den, 12345 or pass-123456-word".to_owned();
        mask_cut_codes(&mut text, &secrets, &codes);
        assert_eq!(
            text,
            "Invalid code [hidden by Ophio]; try [hidden by Ophio], 12345 or [hidden by Ophio]"
        );
    }

    #[test]
    fn what_was_hidden_stays_as_it_is_when_hidden_again() {
        // Codes that are words of the marker, and a secret that reaches
        // into one.
        let codes = vec!["Ophio".to_owned(), "den".to_owned()];
        let secrets = vec!["pass-word-1234".to_owned(), "Ophio]tail-1234".to_owned()];
        let mut text = format!("{MASK} den {MASK}pass-word-1234 {MASK}tail-1234 ok");
        for _ in 0..3 {
            mask_again(&mut text, &secrets, &codes);
        }
        let hidden = format!("{MASK} {MASK} {MASK}{MASK} [hid{MASK} by {MASK} ok");
        assert_eq!(text, hidden);
        mask_again(&mut text, &secrets, &codes);
        assert_eq!(text, hidden);
        // And one that begins as the marker does.
        let mut text = MASK.to_owned();
        mask_again(&mut text, &["[hidden by".to_owned()], &[]);
        assert_eq!(text, MASK);
    }

    #[test]
    fn a_secret_too_short_to_hide_is_shown_whole_or_cut() {
        let secrets = vec!["abc def".to_owned()];
        for shown in ["x abc def y", "x abc d… y", "x abc d"] {
            let mut text = shown.to_owned();
            mask_cut_codes(&mut text, &secrets, &[]);
            assert_eq!(text, shown);
        }
    }

    #[test]
    fn what_is_hidden_again_is_hidden_however_short_it_shows() {
        // Long enough to hide before it was spaced to show.
        let secrets = vec!["abc def".to_owned()];
        for (shown, hidden) in [
            ("x abc def y", format!("x {MASK} y")),
            ("x abc d… y", format!("x {MASK}… y")),
            ("x abc d", format!("x {MASK}")),
            // Fewer than are hidden of any cut secret.
            ("x abc", "x abc".to_owned()),
        ] {
            let mut text = shown.to_owned();
            mask_again(&mut text, &secrets, &[]);
            assert_eq!(text, hidden, "{shown}");
            mask_again(&mut text, &secrets, &[]);
            assert_eq!(text, hidden, "{shown}");
        }
    }

    #[test]
    fn a_secret_split_anywhere_is_never_shown() {
        let secrets = vec!["ghp_Abc123XYZ789".to_owned(), "sk-ü-0123456789".to_owned()];
        for secret in &secrets {
            let line = format!("token: {secret} done");
            let bounds: Vec<usize> = (1..line.len())
                .filter(|&i| line.is_char_boundary(i))
                .collect();
            for &a in &bounds {
                for &b in bounds.iter().filter(|&&b| b > a) {
                    let shown = streamed(&[&line[..a], &line[a..b], &line[b..]], &secrets);
                    assert!(!shown.contains(&secret[..MASKED_FROM]), "{a} {b}: {shown}");
                    assert!(
                        shown.starts_with("token: [hidden by Ophio]"),
                        "{a} {b}: {shown}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_longer_secret_beginning_with_a_shorter_one_is_never_shown() {
        let (short, long) = ("sk-or-v1", "sk-or-v1-0123456789abcdef");
        let secrets = vec![long.to_owned(), short.to_owned()];
        for line in [
            format!("key {long} end"),
            format!("key {short} {long}{short}"),
        ] {
            let bounds: Vec<usize> = (1..line.len()).collect();
            for &a in &bounds {
                for &b in bounds.iter().filter(|&&b| b > a) {
                    let shown = streamed(&[&line[..a], &line[a..b], &line[b..]], &secrets);
                    assert!(!shown.contains("-0123"), "{a} {b}: {shown}");
                    assert!(!shown.contains("sk-or"), "{a} {b}: {shown}");
                    let mut whole = line.clone();
                    mask(&mut whole, &secrets);
                    assert_eq!(shown, whole, "{a} {b}");
                }
            }
        }
    }

    #[test]
    fn a_shorter_secret_listed_first_still_leaves_none_of_a_longer_one() {
        let (short, long) = ("sk-or-v1", "sk-or-v1-0123456789abcdef");
        // As Accounts keeps them: sorted, so the shorter comes first.
        let secrets = vec![short.to_owned(), long.to_owned()];
        let mut text = format!("identity {long} and {short}");
        mask(&mut text, &secrets);
        assert_eq!(text, format!("identity {MASK} and {MASK}"));
        let mut value = serde_json::json!({ "email": format!("{long}@example.com") });
        mask_value(&mut value, &secrets);
        assert_eq!(value["email"], format!("{MASK}@example.com"));
        let mut cut = format!("said {long}");
        mask_cut(&mut cut, &secrets);
        assert_eq!(cut, format!("said {MASK}"));
    }

    #[test]
    fn a_secret_cut_anywhere_to_fit_is_never_shown() {
        let secret = "ghp_Abc123XYZ789".to_owned();
        let secrets = vec![secret.clone()];
        let line = format!("curl -H 'Bearer {secret}' https://api");
        for cut in 1..line.len() {
            for ellipsis in [false, true] {
                let mut text = line[..cut].to_owned();
                if ellipsis {
                    text.push('…');
                    text.push_str(" and more");
                }
                mask_cut(&mut text, &secrets);
                assert!(!text.contains(&secret[..CUT_SHOWN]), "{cut}: {text}");
            }
        }
        // Text that only ends like a few letters of a key is left.
        let mut text = "use gh".to_owned();
        mask_cut(&mut text, &secrets);
        assert_eq!(text, "use gh");
    }

    #[test]
    fn a_cut_secret_another_secret_begins_is_never_shown() {
        let (short, long) = ("sk-or-v1", "sk-or-v1-0123456789abcdef");
        for secrets in [
            vec![short.to_owned(), long.to_owned()],
            vec![long.to_owned(), short.to_owned()],
        ] {
            let line = format!("said {short} then {long} done");
            for cut in 1..line.len() {
                for ellipsis in [false, true] {
                    let mut text = line[..cut].to_owned();
                    if ellipsis {
                        text.push('…');
                        text.push_str(" more");
                    }
                    mask_cut(&mut text, &secrets);
                    assert!(!text.contains("sk-o"), "{cut}: {text}");
                    assert!(!text.contains("-0123"), "{cut}: {text}");
                    assert!(!text.contains("v1-"), "{cut}: {text}");
                }
            }
        }
        // Overlapping secrets are hidden as one.
        let secrets = vec!["abcdefgh12".to_owned(), "gh12345678".to_owned()];
        let mut text = "x abcdefgh12345678 y".to_owned();
        mask(&mut text, &secrets);
        assert_eq!(text, format!("x {MASK} y"));
    }

    #[test]
    fn a_secret_named_like_an_enum_leaves_the_rest_masked() {
        use protocol::model::{ChangedFile, FileChange, ReportedDiff, RunOutcome};
        let key = "ghp_Abc123XYZ789".to_owned();
        // An accepted key that is also the name a changed file is reported by.
        let secrets = vec!["modified".to_owned(), key.clone()];
        let mut outcome = RunOutcome {
            summary: Some(format!("set {key}")),
            changed_files: vec![ChangedFile {
                path: "modified/config".into(),
                change: FileChange::Modified,
                added: Some(1),
                removed: None,
                diff: Some(ReportedDiff {
                    text: format!("+TOKEN={key}"),
                    truncated: false,
                }),
                provenance: None,
            }],
            checks: vec![],
            still_active: vec![],
            unknowns: vec![format!("{key} modified")],
            usage: None,
            report: None,
            verification: None,
        };
        crate::harness::visit_serialized(&mut outcome, &mut |text| mask(text, &secrets));
        let shown = serde_json::to_string(&outcome).unwrap();
        assert!(!shown.contains(&key[..MASKED_FROM]), "{shown}");
        assert_eq!(outcome.changed_files[0].change, FileChange::Modified);
        assert_eq!(outcome.changed_files[0].path, format!("{MASK}/config"));
        assert_eq!(outcome.unknowns, vec![format!("{MASK} {MASK}")]);
    }

    #[test]
    fn json_keeps_a_maskable_value_as_itself() {
        for refused in [
            "short",
            "has space1",
            "quote\"d12",
            "back\\slash",
            "tag<b>12",
            "ünicode12",
        ] {
            assert!(!maskable(refused), "{refused}");
        }
        let key = "sk-or-v1_AbC+/=:%#~123".to_owned();
        assert!(maskable(&key));
        let mut value =
            serde_json::json!({ "command": format!("echo {key}"), key.clone(): [key.clone()] });
        assert!(serde_json::to_string(&value).unwrap().contains(&key));
        mask_value(&mut value, std::slice::from_ref(&key));
        assert!(!value.to_string().contains(&key[..MASKED_FROM]), "{value}");
    }

    #[test]
    fn only_a_possible_start_of_a_secret_is_held() {
        let secrets = vec!["ghp_Abc123XYZ789".to_owned()];
        assert_eq!(held_from("plain text", &secrets), 10);
        assert_eq!(held_from("see gh", &secrets), 4);
        assert_eq!(held_from("ghp_Abc123XYZ78", &secrets), 0);
        // A short value is neither masked nor held.
        assert_eq!(held_from("abc", &["abcdefg".to_owned()]), 3);
    }

    #[test]
    fn overlapping_secrets_are_held_whole_in_one_search() {
        // Every copy of the secret overlaps the next, so all of it is held.
        // The old search went back one copy at a time and searched again.
        let secrets = vec!["AAAAAAAA".to_owned(), "AAAAB-tail".to_owned()];
        let mut text = "A".repeat(200_000);
        let started = std::time::Instant::now();
        let mut held = mask_streamed(&mut text, &secrets);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!((text.as_str(), held.len()), ("", 200_000));
        // Text that ends the run lets all of it out, masked as one.
        let mut text = held + "B-tail done";
        held = mask_streamed(&mut text, &secrets);
        assert_eq!(
            (text.as_str(), held.as_str()),
            (format!("{MASK} done").as_str(), "")
        );

        // Held ends match the old search, which went back one secret at a
        // time and searched again.
        let secrets = vec!["abab-abab".to_owned(), "b-ababab".to_owned()];
        for line in ["xx abab-abab-abab-ababab end", "ab-ababab-abab", "abab-ab"] {
            for cut in 1..=line.len() {
                let mut text = line[..cut].to_owned();
                let mut from = held_from(&text, &secrets);
                while let Some(at) = secrets
                    .iter()
                    .flat_map(|s| occurrences(&text, s))
                    .filter(|&(at, end)| at < from && end > from)
                    .map(|(at, _)| at)
                    .min()
                {
                    from = at;
                }
                let held = mask_streamed(&mut text, &secrets);
                assert_eq!(held, line[from..cut], "{line} {cut}");
                let mut before = line[..from].to_owned();
                mask(&mut before, &secrets);
                assert_eq!(text, before, "{line} {cut}");
            }
        }
    }

    #[test]
    fn many_separate_secrets_are_masked_in_one_pass() {
        // Replacing each where it is moved the rest of the text every time.
        let secrets = vec!["AAAAAAAA".to_owned(), "AAAAB-tail".to_owned()];
        let said = "AAAAAAAA ".repeat(1_000_000);
        let hidden = format!("{MASK} ").repeat(1_000_000);
        let started = std::time::Instant::now();
        let mut text = said.clone();
        mask(&mut text, &secrets);
        assert_eq!(text, hidden);
        let mut text = said.clone();
        mask_cut(&mut text, &secrets);
        assert_eq!(text, hidden);
        let mut text = said;
        let held = mask_streamed(&mut text, &secrets);
        assert_eq!((text, held.as_str()), (hidden, ""));
        // A cut after every start of a secret is searched once, not once
        // for each cut.
        let mut text = "AAAA…".repeat(1_000_000);
        mask_cut(&mut text, &secrets);
        assert_eq!(text, format!("{MASK}…").repeat(1_000_000));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn many_overlapping_secrets_keep_no_more_than_the_text_needs() {
        // Every key an added service has had is kept to be masked; here each
        // overlaps all the others everywhere in the text.
        let secrets: Vec<String> = (MASKED_FROM..136).map(|n| "A".repeat(n)).collect();
        let said = "A".repeat(300_000);
        let found = whole(&said, &secrets);
        assert_eq!(found, vec![(0, said.len())]);
        let mut text = said.clone();
        mask(&mut text, &secrets);
        assert_eq!(text, MASK);
        let mut text = said + " done";
        let held = mask_streamed(&mut text, &secrets);
        assert_eq!((text, held.as_str()), (format!("{MASK} done"), ""));
        // The held end is found in one pass too: a long key that a text ends
        // nearly with.
        let secrets = vec![format!("{}B", "A".repeat(8191))];
        let text = format!("{}C", "A".repeat(8190));
        assert_eq!(held_from(&text, &secrets), text.len());
        assert_eq!(held_from(&text[..8190], &secrets), 0);
    }

    #[test]
    fn whole_finds_what_the_old_search_did() {
        let secrets: Vec<String> = ["abab-abab", "b-ababab", "é-éé-ééé", "ab-abab-", "xyzxyzxy"]
            .map(String::from)
            .into();
        for text in [
            "xx abab-abab-abab-ababab end",
            "ab-abab-abab-ab-ababab-é-éé-ééé-éé-ééé",
            "xyzxyzxyzxyzxy ab-abab-abab",
            "nothing here",
            "",
        ] {
            let mut old: Vec<(usize, usize)> =
                secrets.iter().flat_map(|s| occurrences(text, s)).collect();
            old.sort_unstable();
            let mut joined = Vec::new();
            for range in old {
                add(&mut joined, range);
            }
            assert_eq!(whole(text, &secrets), joined, "{text}");
            for cut in (0..=text.len()).filter(|&c| text.is_char_boundary(c)) {
                let text = &text[..cut];
                let old = secrets
                    .iter()
                    .filter_map(|s| {
                        (1..s.len())
                            .rev()
                            .find(|&n| text.as_bytes().ends_with(&s.as_bytes()[..n]))
                    })
                    .map(|n| text.len() - n)
                    .min()
                    .unwrap_or(text.len());
                assert_eq!(held_from(text, &secrets), old, "{text}");
            }
        }
    }

    #[test]
    fn cut_starts_match_the_old_search_back_from_each_cut() {
        let secrets = ["abab-abab", "aaaaaaab", "ab…abab…ab", "é-éé-ééé"];
        let texts = [
            "xx abab-aba…b abab-ab…",
            "aaaaaaaaaa…aaaab…",
            "ab…abab…ab…abab…",
            "é-éé-é…éé-éé",
        ];
        for secret in secrets {
            for text in texts {
                let mut cuts: Vec<usize> = text.match_indices('…').map(|(at, _)| at).collect();
                cuts.push(text.len());
                let old: Vec<usize> = cuts
                    .iter()
                    .map(|&cut| {
                        (1..secret.len().min(cut + 1))
                            .rev()
                            .find(|&n| text.as_bytes()[..cut].ends_with(&secret.as_bytes()[..n]))
                            .unwrap_or(0)
                    })
                    .collect();
                assert_eq!(
                    cut_starts(text.as_bytes(), secret.as_bytes(), &cuts),
                    old,
                    "{secret} {text}"
                );
            }
        }
    }
}
