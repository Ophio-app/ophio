// SPDX-License-Identifier: Apache-2.0
//! The checks a run linked to GitHub works under: `gh` that only reads, and
//! a pre-push check. They catch mistakes, and point the agent at the tool
//! that does the job with Øphio's disclosure. An agent set on getting round
//! them can; the default branch's protection is the net for that.
use std::ffi::OsString;
use std::io::BufRead;
use std::process::{Command, ExitCode, Stdio};

/// Where the run was told the real `gh` is.
pub const REAL_GH_ENV: &str = "OPHIO_REAL_GH";

const PR_TOOL: &str = "open or update the pull request with the github_pr tool";
const COMMENT_TOOL: &str = "comment with the github_comment tool";

/// Why `gh` with `args` is refused, or nothing when it only reads.
pub fn gh_refusal(args: &[String]) -> Option<String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let (command, rest) = words.split_first()?;
    let command = *command;
    if matches!(command, "--help" | "-h" | "--version" | "help" | "version") {
        return None;
    }
    if command == "api" {
        return api_refusal(rest);
    }
    let sub = subcommand(rest);
    let named = |tool: &str| Some(format!("`gh {command} {sub}` is refused here; {tool}."));
    let reads: &[&str] = match command {
        "pr" => {
            return match sub {
                "view" | "list" | "checks" | "diff" | "status" | "checkout" | "co" => None,
                "create" | "new" | "edit" => named(PR_TOOL),
                "comment" | "review" => named(
                    "comment with the github_comment tool; approving and requesting changes are the person's",
                ),
                "ready" => named("mark it ready with the github_ready tool, in Through merge"),
                "merge" => named("merge with the github_merge tool, in Through merge"),
                "close" | "reopen" => named("closing and reopening are the person's"),
                _ => refused(command, sub),
            };
        }
        "issue" => {
            return match sub {
                "view" | "list" | "status" => None,
                "create" | "new" => named("file it with the github_issue tool"),
                "comment" => named(COMMENT_TOOL),
                "close" | "reopen" | "delete" | "transfer" => {
                    named("closing, reopening, deleting and moving issues are the person's")
                }
                _ => refused(command, sub),
            };
        }
        "repo" => &["view", "list", "clone"],
        "run" => &["view", "list", "watch", "download"],
        "workflow" => &["view", "list"],
        "release" => &["view", "list", "download"],
        "label" | "cache" | "secret" | "variable" | "gist" | "org" | "ruleset" => &["list", "view"],
        "search" => return None,
        "status" => return None,
        "auth" => &["status"],
        "config" => &["get", "list"],
        "alias" => &["list"],
        _ => {
            return Some(format!(
                "`gh {command}` isn't available here. Use `gh` to read, and Øphio's github_ tools to write."
            ));
        }
    };
    if reads.contains(&sub) {
        None
    } else {
        refused(command, sub)
    }
}

fn refused(command: &str, sub: &str) -> Option<String> {
    Some(format!(
        "`gh {command} {sub}` is refused here. Use `gh` to read, and Øphio's github_ tools to write."
    ))
}

/// The first word that isn't a flag or a flag's value.
fn subcommand<'a>(rest: &[&'a str]) -> &'a str {
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        match *word {
            "-R" | "--repo" => {
                words.next();
            }
            w if w.starts_with('-') => {}
            w => return w,
        }
    }
    ""
}

/// `gh api`'s short flags: the long name each stands for, and whether it
/// takes a value.
fn api_short(c: char) -> Option<(&'static str, bool)> {
    Some(match c {
        'X' => ("method", true),
        'f' => ("raw-field", true),
        'F' => ("field", true),
        'H' => ("header", true),
        'p' => ("preview", true),
        'q' => ("jq", true),
        't' => ("template", true),
        'i' => ("include", false),
        'h' => ("help", false),
        _ => return None,
    })
}

fn api_long(name: &str) -> Option<(&'static str, bool)> {
    const LONG: [(&str, bool); 16] = [
        ("method", true),
        ("raw-field", true),
        ("field", true),
        ("header", true),
        ("preview", true),
        ("jq", true),
        ("template", true),
        ("input", true),
        ("cache", true),
        ("hostname", true),
        ("include", false),
        ("paginate", false),
        ("slurp", false),
        ("silent", false),
        ("verbose", false),
        ("help", false),
    ];
    LONG.iter().find(|(long, _)| *long == name).copied()
}

/// Flags each with its value, by long name, and the other words.
type ApiArgs<'a> = (Vec<(&'static str, &'a str)>, Vec<&'a str>);

/// `gh api`'s arguments read the way gh reads them: short flags clustered
/// (`-iX DELETE`), values attached or following, `--` ending the flags. A
/// flag gh api doesn't have is an error, the spelling it was given.
fn api_args<'a>(rest: &[&'a str]) -> Result<ApiArgs<'a>, String> {
    let mut flags = Vec::new();
    let mut words_left = Vec::new();
    let mut words = rest.iter().copied();
    while let Some(word) = words.next() {
        if word == "--" {
            words_left.extend(words.by_ref());
            break;
        }
        if let Some(long) = word.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let Some((name, takes_value)) = api_long(name) else {
                return Err(word.into());
            };
            let value = match (takes_value, attached) {
                (_, Some(value)) => value,
                (false, None) => "true",
                (true, None) => words.next().ok_or_else(|| word.to_string())?,
            };
            flags.push((name, value));
        } else if word.len() > 1 && word.starts_with('-') {
            let mut cluster = &word[1..];
            while let Some(c) = cluster.chars().next() {
                let Some((name, takes_value)) = api_short(c) else {
                    return Err(format!("-{c}"));
                };
                let after = &cluster[c.len_utf8()..];
                let value = if after.len() > 1 && after.starts_with('=') {
                    cluster = "";
                    &after[1..]
                } else if !takes_value {
                    cluster = after;
                    "true"
                } else if !after.is_empty() {
                    cluster = "";
                    after
                } else {
                    cluster = "";
                    words.next().ok_or_else(|| format!("-{c}"))?
                };
                flags.push((name, value));
            }
        } else {
            words_left.push(word);
        }
    }
    Ok((flags, words_left))
}

/// `gh api` reads with GET, or a GraphQL query that isn't a mutation.
fn api_refusal(rest: &[&str]) -> Option<String> {
    let (flags, words) = match api_args(rest) {
        Ok(read) => read,
        Err(flag) => {
            return Some(format!(
                "`gh api {flag}` isn't understood here. Read with gh api's usual flags: -X GET, -f, -H, -q, -t, -i, --paginate."
            ));
        }
    };
    let mut method: Option<String> = None;
    let mut fields: Vec<&str> = Vec::new();
    let mut input = false;
    for (name, value) in flags {
        match name {
            "method" => method = Some(value.to_ascii_uppercase()),
            "field" | "raw-field" => fields.push(value),
            "input" => input = true,
            _ => {}
        }
    }
    let graphql = words
        .first()
        .is_some_and(|e| e.trim_start_matches('/') == "graphql");
    let method = method.unwrap_or_else(|| {
        if fields.is_empty() && !input {
            "GET".into()
        } else {
            "POST".into()
        }
    });
    if graphql {
        // gh sends the last `query` it was given; every one has to read.
        let mut queries = fields
            .iter()
            .filter_map(|f| f.strip_prefix("query="))
            .peekable();
        let reads = !input
            && matches!(method.as_str(), "GET" | "POST")
            && queries.peek().is_some()
            && queries.all(|q| !q.starts_with('@') && !q.to_ascii_lowercase().contains("mutation"));
        if reads || fields.is_empty() && !input && method == "GET" {
            return None;
        }
    } else if method == "GET" {
        return None;
    }
    Some(format!(
        "`gh api` with {method} is refused here; it writes to GitHub. Use `gh api` to read, and Øphio's github_ tools to write: {PR_TOOL}, {COMMENT_TOOL}, or file an issue with github_issue."
    ))
}

/// The `gh` a linked run calls: refuses writes, and runs the real `gh`
/// for the rest.
pub fn gh(args: Vec<OsString>) -> ExitCode {
    let words: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Some(refusal) = gh_refusal(&words) {
        eprintln!("Øphio: {refusal}");
        return ExitCode::FAILURE;
    }
    let Some(real) = std::env::var_os(REAL_GH_ENV).filter(|r| !r.is_empty()) else {
        eprintln!("Øphio: gh isn't installed on this computer.");
        return ExitCode::from(127);
    };
    let mut command = Command::new(real);
    command.args(&args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        eprintln!("Øphio: couldn't run gh: {error}");
        ExitCode::from(126)
    }
    #[cfg(not(unix))]
    match command.status() {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("Øphio: couldn't run gh: {error}");
            ExitCode::from(126)
        }
    }
}

/// One ref a push would update, as git hands it to a pre-push hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedRef {
    pub local_ref: String,
    pub local_sha: String,
    pub remote_ref: String,
    pub remote_sha: String,
}

impl PushedRef {
    pub fn parse(line: &str) -> Option<Self> {
        let mut words = line.split_whitespace();
        let pushed = Self {
            local_ref: words.next()?.into(),
            local_sha: words.next()?.into(),
            remote_ref: words.next()?.into(),
            remote_sha: words.next()?.into(),
        };
        words.next().is_none().then_some(pushed)
    }
}

fn zero(sha: &str) -> bool {
    sha.bytes().all(|b| b == b'0')
}

/// What a push needs checked: refused outright, or allowed when the
/// branch's commit on GitHub is an ancestor of what is pushed.
#[derive(Debug, PartialEq, Eq)]
pub enum PushCheck {
    Refuse(String),
    New,
    FastForward { remote: String, local: String },
}

pub fn classify_push(defaults: &[&str], pushed: &PushedRef) -> PushCheck {
    let branch = match pushed.remote_ref.strip_prefix("refs/heads/") {
        Some(branch) => branch,
        None if pushed.remote_ref.starts_with("refs/tags/") => {
            return PushCheck::Refuse("Don't push tags; releases are the person's.".into());
        }
        None => {
            return PushCheck::Refuse(format!("Only push branches, not `{}`.", pushed.remote_ref));
        }
    };
    if zero(&pushed.local_sha) {
        return PushCheck::Refuse(format!("Don't delete branches on GitHub, like `{branch}`."));
    }
    if defaults.contains(&branch) {
        return PushCheck::Refuse(format!(
            "Don't push to `{branch}`. Push a branch of your own, and open a pull request with github_pr."
        ));
    }
    if zero(&pushed.remote_sha) {
        return PushCheck::New;
    }
    PushCheck::FastForward {
        remote: pushed.remote_sha.clone(),
        local: pushed.local_sha.clone(),
    }
}

/// A repository's address in one spelling, however git was given it:
/// `https://github.com/octo/app`, `git@github.com:octo/app.git` and
/// `ssh://git@ssh.github.com:443/octo/app/` are the same.
pub fn repository(url: &str) -> String {
    let url = url.trim().to_ascii_lowercase();
    let (host, path) = match url.split_once("://") {
        Some((_, rest)) => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            (host.split(':').next().unwrap_or_default().to_owned(), path)
        }
        // `host:path`, as scp writes it, when no `/` comes before the `:`.
        None => match url.split_once(':') {
            Some((host, path)) if !host.contains('/') && !host.is_empty() => {
                let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
                (host.to_owned(), path)
            }
            _ => (String::new(), url.as_str()),
        },
    };
    let host = match host.as_str() {
        "ssh.github.com" | "www.github.com" => "github.com".to_owned(),
        _ => host,
    };
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    format!("{host}/{path}")
}

/// The branch a repository's HEAD names now, read from it: none when it
/// has no commits yet, and nothing at all when it couldn't be read.
fn default_now(url: &str) -> Option<Option<String>> {
    let out = Command::new("git")
        .args(["ls-remote", "--symref", url, "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(text.lines().find_map(|line| {
        let (target, name) = line.strip_prefix("ref: ")?.split_once('\t')?;
        (name == "HEAD")
            .then(|| target.strip_prefix("refs/heads/"))
            .flatten()
            .map(str::to_owned)
    }))
}

/// The pre-push check: `pre-push <default branch> <repository> <remote>
/// <url>`, with git's list of refs on standard input. Pushes go to the
/// run's repository only, and not to its default branch as it was when the
/// run began nor as it is now.
pub fn pre_push(args: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let [default_branch, repo, _remote, url] = args.as_slice() else {
        eprintln!("Øphio: push refused; the pre-push check couldn't tell where it goes.");
        return ExitCode::FAILURE;
    };
    if repository(url) != repository(repo) {
        eprintln!(
            "Øphio: push refused. This run pushes to {} only, not {}. Push with `git push -u origin <branch>`.",
            repository(repo),
            repository(url)
        );
        return ExitCode::FAILURE;
    }
    let Some(current) = default_now(url) else {
        eprintln!(
            "Øphio: push refused; couldn't read the repository's default branch. Try again in a moment."
        );
        return ExitCode::FAILURE;
    };
    let defaults = [default_branch.as_str()]
        .into_iter()
        .chain(current.as_deref())
        .collect::<Vec<_>>();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            eprintln!("Øphio: couldn't read what's being pushed.");
            return ExitCode::FAILURE;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Some(pushed) = PushedRef::parse(&line) else {
            eprintln!("Øphio: couldn't read what's being pushed.");
            return ExitCode::FAILURE;
        };
        match classify_push(&defaults, &pushed) {
            PushCheck::New => {}
            PushCheck::Refuse(why) => {
                eprintln!("Øphio: push refused. {why}");
                return ExitCode::FAILURE;
            }
            PushCheck::FastForward { remote, local } => {
                let status = Command::new("git")
                    .args(["merge-base", "--is-ancestor", &remote, &local])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                match status.map(|s| s.code()) {
                    Ok(Some(0)) => {}
                    Ok(Some(1)) => {
                        eprintln!(
                            "Øphio: push refused. `{}` on GitHub has commits this push would drop; never force-push. Merge them in instead: git fetch origin, then git merge origin/{}.",
                            pushed.remote_ref.trim_start_matches("refs/heads/"),
                            pushed.remote_ref.trim_start_matches("refs/heads/"),
                        );
                        return ExitCode::FAILURE;
                    }
                    _ => {
                        eprintln!(
                            "Øphio: push refused until the branch is fetched: git fetch origin, merge what's new, then push again."
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(line: &str) -> bool {
        let args: Vec<String> = line.split_whitespace().map(String::from).collect();
        gh_refusal(&args).is_some()
    }

    #[test]
    fn gh_reads_pass_and_writes_name_the_tool() {
        for read in [
            "",
            "--version",
            "pr view 5",
            "pr checks 5 --json name",
            "pr -R octo/app view 5",
            "pr list --search close",
            "issue view 9 --comments",
            "repo view",
            "run view 1 --log-failed",
            "search issues crash",
            "auth status",
            "api repos/octo/app/pulls",
            "api -X GET repos/octo/app/issues -f state=open",
            "api --method=get repos/octo/app",
            "api graphql -f query={viewer{login}}",
            "api -i repos/octo/app",
            "api --paginate --slurp repos/octo/app/issues",
            "api -iq .name repos/octo/app",
            "api -H Accept:x repos/octo/app -t {{.name}}",
            "api -X=GET repos/octo/app",
            "api -- repos/octo/app",
        ] {
            assert!(!refused(read), "{read}");
        }
        for write in [
            "pr create --fill",
            "pr new",
            "pr -R octo/app create",
            "pr edit 5 --body x",
            "pr ready 5",
            "pr merge 5",
            "pr review 5 --approve",
            "pr comment 5 -b x",
            "pr close 5",
            "issue create -t x",
            "issue comment 9 -b x",
            "issue close 9",
            "repo delete octo/app",
            "release create v1",
            "auth token",
            "alias set co pr-create",
            "extension exec x",
            "api -X POST repos/octo/app/issues",
            "api -XPATCH repos/octo/app/pulls/5",
            "api repos/octo/app/issues -f title=x",
            "api repos/octo/app/issues --field=title=x",
            "api repos/octo/app/issues --input body.json",
            "api graphql -f query=mutation{x}",
            "api graphql -F query=@q.graphql",
            "api -iX DELETE repos/octo/app/git/refs/heads/x",
            "api -iXDELETE repos/octo/app/git/refs/heads/x",
            "api -iX=DELETE repos/octo/app/git/refs/heads/x",
            "api -X=PATCH repos/octo/app/pulls/5",
            "api -if title=x repos/octo/app/issues",
            "api -i -fq=x repos/octo/app/issues",
            "api --method DELETE repos/octo/app",
            "api --bogus DELETE repos/octo/app",
            "api -Z repos/octo/app",
            "api repos/octo/app/issues -X",
            "api graphql -f query={viewer{login}} -f query=mutation{x}",
            "api graphql -X PUT -f query={viewer{login}}",
            "api /graphql -f query=mutation{x}",
        ] {
            assert!(refused(write), "{write}");
        }
        let args: Vec<String> = ["pr", "create"].map(String::from).to_vec();
        assert!(gh_refusal(&args).unwrap().contains("github_pr"));
        let args: Vec<String> = ["issue", "comment", "9"].map(String::from).to_vec();
        assert!(gh_refusal(&args).unwrap().contains("github_comment"));
    }

    fn pushed(local: &str, remote_ref: &str, remote: &str) -> PushedRef {
        PushedRef {
            local_ref: "HEAD".into(),
            local_sha: local.into(),
            remote_ref: remote_ref.into(),
            remote_sha: remote.into(),
        }
    }

    #[test]
    fn pushes_are_classified() {
        let zero = "0".repeat(40);
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        assert_eq!(
            classify_push(&["main"], &pushed(&a, "refs/heads/ophio/x", &zero)),
            PushCheck::New
        );
        assert_eq!(
            classify_push(&["main"], &pushed(&a, "refs/heads/ophio/x", &b)),
            PushCheck::FastForward {
                remote: b.clone(),
                local: a.clone()
            }
        );
        for refused in [
            pushed(&a, "refs/heads/main", &b),
            pushed(&a, "refs/heads/main", &zero),
            pushed(&zero, "refs/heads/ophio/x", &b),
            pushed(&a, "refs/tags/v1", &zero),
            pushed(&a, "refs/notes/x", &zero),
        ] {
            assert!(matches!(
                classify_push(&["main"], &refused),
                PushCheck::Refuse(_)
            ));
        }
        assert_eq!(
            PushedRef::parse(&format!("refs/heads/x {a} refs/heads/x {b}")),
            Some(PushedRef {
                local_ref: "refs/heads/x".into(),
                local_sha: a.clone(),
                remote_ref: "refs/heads/x".into(),
                remote_sha: b.clone()
            })
        );
        assert_eq!(PushedRef::parse("too few words"), None);
        assert!(matches!(
            classify_push(
                &["main", "production"],
                &pushed(&a, "refs/heads/production", &b)
            ),
            PushCheck::Refuse(_)
        ));
    }

    #[test]
    fn a_repository_is_the_same_however_it_is_spelt() {
        for url in [
            "https://github.com/octo/app",
            "https://github.com/Octo/App.git",
            "https://x-access-token:abc@github.com/octo/app/",
            "http://github.com/octo/app",
            "git@github.com:octo/app.git",
            "ssh://git@github.com/octo/app.git",
            "ssh://git@ssh.github.com:443/octo/app",
            "git://github.com/octo/app",
        ] {
            assert_eq!(repository(url), "github.com/octo/app", "{url}");
        }
        for other in [
            "https://github.com/octo/app2",
            "https://github.com/other/app",
            "git@gitlab.com:octo/app.git",
            "https://github.com/octo/app/../other",
            "/srv/octo/app",
        ] {
            assert_ne!(repository(other), "github.com/octo/app", "{other}");
        }
        assert_eq!(
            repository("/tmp/x/remote.git"),
            repository("/tmp/x/remote.git/")
        );
    }
}
