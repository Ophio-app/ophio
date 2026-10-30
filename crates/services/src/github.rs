// SPDX-License-Identifier: Apache-2.0
//! The GitHub workflow, as the phone reaches it: each project's settings,
//! what the computer reads about the project's repository, its open issues
//! and pull requests, and the person's own actions on a pull request. All of
//! it goes to GitHub as the account signed in under Accounts.
use crate::{CompanionHub, api::Caller, error, unsupported};
use ports::github::{Github, GithubError, Method};
use ports::model_name::model_name;
use protocol::github::*;
use protocol::{
    ErrorCode, Event, ProjectId, ProtocolError,
    auth::Principal,
    capability::{HarnessInfo, HarnessKind},
    command::CommandResult,
    model::{Project, Task, TaskBranch, TaskKind, TaskMode, TaskState},
    project_folders::ProjectFolder,
    time::Timestamp,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

pub(crate) const SETTINGS: &str = "github_settings";
/// What was last read about a project's repository, for the watcher.
pub(crate) const REPO: &str = "github_repo";
/// The project's last triage: a `Vec<Triaged>`.
pub(crate) const TRIAGE: &str = "github_triage";

/// One open issue or pull request as the last triage run sorted it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Triaged {
    pub pr: bool,
    pub number: u64,
    pub group: TriageGroup,
    pub note: String,
    /// The answer the Sort would give its own question, for the person to
    /// start theirs from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick: Option<String>,
    /// Put there by the person, so a later sort leaves it where it is.
    #[serde(default)]
    pub by_you: bool,
}

/// The most of a pull request's description shown on its sheet.
const MAX_BODY: usize = 20_000;
const PR_AGAIN: &str = "whose pull request you read. Read it again.";
const ISSUE_AGAIN: &str = "you were filing an issue in. Start the issue again.";
const ISSUE_READ_AGAIN: &str = "whose issue you read. Open it again.";
const PRS_AGAIN: &str = "whose pull requests you picked. Pick them again.";
/// Why an issue an agent checked before waits for the person, once the
/// task that checked it is gone.
const CHECKED_BEFORE: &str = "An agent checked this before and couldn't make it happen, or found it fixed. Its comment on GitHub says which.";
/// The most of a review thread's first comment shown.
const MAX_COMMENT: usize = 2_000;
/// The longest filter a list takes.
const MAX_FILTER: usize = 512;

const NOT_ON_GITHUB: &str = "This project's `origin` isn't on github.com.";
pub(crate) const NOT_A_REPO: &str = "This project's folder isn't a Git repository, so GitHub can't be used with it. GitHub works in a project whose folder is one, with its `origin` on github.com.";
/// The most folders read, and offered, looking for a project's repository.
const REPO_FOLDERS_READ: usize = 200;
const REPO_FOLDERS_SHOWN: usize = 5;

/// The settings a project keeps, checked before they're admitted.
pub(crate) fn valid_settings(settings: &GithubSettings) -> Result<(), ProtocolError> {
    let prefix = &settings.branch_prefix;
    if prefix.chars().count() > MAX_BRANCH_PREFIX
        || !crate::git::branch_name_ok(&format!("{prefix}x"))
    {
        return Err(error(
            ErrorCode::InvalidRequest,
            "That branch prefix isn't one Git accepts at the start of a branch name",
        ));
    }
    if !(1..=MAX_FINDINGS_PER_RUN).contains(&settings.workflow.findings_per_run) {
        return Err(error(
            ErrorCode::InvalidRequest,
            format!("A finder run files 1 to {MAX_FINDINGS_PER_RUN} issues"),
        ));
    }
    if let Some(fix) = &settings.workflow.fix_with {
        valid_choice(fix, "fix with")?;
    }
    if let Some(review) = &settings.workflow.review_with {
        valid_choice(review, "review with")?;
    }
    Ok(())
}

/// A repository as a link and the warning's answer remember it.
pub(crate) fn repo_key(owner: &str, name: &str) -> String {
    format!("{owner}/{name}").to_lowercase()
}

/// Whether the person answered Through merge's warning for this repository.
pub(crate) fn warning_answered(settings: &GithubSettings, repo: &GithubRepo) -> bool {
    settings.warning_answered
        && settings.warned_repo.as_deref() == Some(repo_key(&repo.owner, &repo.name).as_str())
}

/// A task's link acts only on the repository it was made on.
pub(crate) fn same_repo(github: &TaskGithub, owner: &str, name: &str) -> Result<(), ProtocolError> {
    match github.repo.as_deref() {
        Some(linked) if linked != repo_key(owner, name) => Err(error(
            ErrorCode::PermissionDenied,
            format!(
                "This task was linked on {linked}, and the project's origin is {owner}/{name} now, so it doesn't act on GitHub. Link a new task."
            ),
        )),
        _ => Ok(()),
    }
}

/// The owner and name of a remote on github.com, from any address form Git
/// takes. Any other host, GitHub Enterprise's included, isn't GitHub here.
pub(crate) fn github_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let (authority, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            let scheme = scheme.to_ascii_lowercase();
            if !matches!(
                scheme.as_str(),
                "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git"
            ) {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            (host.split_once(':').map_or(host, |(h, _)| h), path)
        }
        // scp-like: [user@]github.com:owner/name
        None => {
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            (
                authority.rsplit_once('@').map_or(authority, |(_, h)| h),
                path,
            )
        }
    };
    if !authority.eq_ignore_ascii_case("github.com")
        && !authority.eq_ignore_ascii_case("www.github.com")
    {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let owner_ok = (1..=39).contains(&owner.len())
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let name_ok = (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && (name.chars()).all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    (owner_ok && name_ok).then(|| (owner.to_owned(), name.to_owned()))
}

/// GitHub's refusal, said for the person.
pub(crate) fn github_error(e: GithubError) -> ProtocolError {
    match e {
        GithubError::NotSignedIn => error(ErrorCode::AccountNotReady, e.to_string()),
        GithubError::Failed(message) => error(ErrorCode::TemporarilyUnavailable, message),
        GithubError::Status { code, message } => match code {
            200 => error(
                ErrorCode::NativeFailure,
                format!("GitHub refused the request: {message}"),
            ),
            301 => error(
                ErrorCode::NotFound,
                "GitHub says this repository moved. Point the project's `origin` at its new address.",
            ),
            401 => error(
                ErrorCode::AccountNotReady,
                "GitHub refused the account signed in under Accounts. Sign in to GitHub there again.",
            ),
            403 => error(
                ErrorCode::PermissionDenied,
                format!("GitHub refused: {message}"),
            ),
            404 => error(
                ErrorCode::NotFound,
                "GitHub couldn't find it, or the account signed in under Accounts can't see it.",
            ),
            405 | 409 => error(ErrorCode::Conflict, format!("GitHub refused: {message}")),
            422 => error(
                ErrorCode::InvalidRequest,
                format!("GitHub refused: {message}"),
            ),
            429 => error(
                ErrorCode::TemporarilyUnavailable,
                format!("GitHub asked to slow down: {message}"),
            ),
            _ => error(
                ErrorCode::NativeFailure,
                format!("GitHub answered {code}: {message}"),
            ),
        },
    }
}

/// A branch or login as one part of an API path.
pub(crate) fn segment(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub(crate) fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

pub(crate) fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// Text from GitHub made safe inside a one-line `code span`: no line
/// breaks, control characters or backticks to end it, and cut to `max`.
pub(crate) fn span(s: &str, max: usize) -> String {
    let flat: String = (s.chars())
        .map(|c| match c {
            '`' => '\'',
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') => ' ',
            c => c,
        })
        .collect();
    cut(flat.trim(), max)
}

/// GitHub's ISO 8601 time.
pub(crate) fn timestamp(v: &Value) -> Timestamp {
    let parsed = v.as_str().and_then(|s| {
        time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
    });
    Timestamp(parsed.map_or(0, |t| (t.unix_timestamp_nanos() / 1_000_000) as i64))
}

/// An author GitHub says can write to the repository. Anyone else's text is
/// data, not instructions.
pub(crate) fn writes(association: &str) -> bool {
    matches!(association, "OWNER" | "MEMBER" | "COLLABORATOR")
}

pub(crate) async fn login(gh: &dyn Github) -> Result<String, ProtocolError> {
    let user = gh
        .rest(Method::Get, "/user", None)
        .await
        .map_err(github_error)?;
    user["login"].as_str().map(str::to_owned).ok_or_else(|| {
        error(
            ErrorCode::NativeFailure,
            "GitHub didn't say who is signed in",
        )
    })
}

const WORKFLOWS_QUERY: &str = "query($owner:String!,$name:String!,$expression:String!){repository(owner:$owner,name:$name){object(expression:$expression){... on Tree{entries{name object{... on Blob{text}}}}}}}";

/// Whether the workflows run on pull requests, and whether one deploys on a
/// push. Read from their triggers, which come before their jobs.
fn workflow_triggers(files: &[String]) -> (bool, bool) {
    let (mut checks, mut deploys) = (false, false);
    for file in files {
        let lower = file.to_ascii_lowercase();
        let triggers = lower.split("\njobs:").next().unwrap_or_default();
        checks |= triggers.contains("pull_request");
        deploys |=
            triggers.contains("push") && (lower.contains("deploy") || lower.contains("pages"));
    }
    (checks, deploys)
}

/// What the account can do with the repository, and what guards its default
/// branch. Reads that fail leave what they would have said unknown.
pub(crate) async fn read_repo(
    gh: &dyn Github,
    owner: &str,
    name: &str,
    login: &str,
) -> Result<GithubRepo, ProtocolError> {
    let base = format!("/repos/{owner}/{name}");
    let repo = gh
        .rest(Method::Get, &base, None)
        .await
        .map_err(github_error)?;
    let default_branch = text(&repo["default_branch"]);
    let private = repo["private"].as_bool().unwrap_or(true);
    let admin = repo["permissions"]["admin"].as_bool().unwrap_or(false);
    let branch = segment(&default_branch);
    let rules_path = format!("{base}/rules/branches/{branch}");
    let classic_path = if admin {
        format!("{base}/branches/{branch}/protection")
    } else {
        format!("{base}/branches/{branch}")
    };
    let pages_path = format!("{base}/pages");
    let deployments_path = format!("{base}/deployments?ref={branch}&per_page=1");
    let reporting_path = format!("{base}/private-vulnerability-reporting");
    let has_pages = repo["has_pages"].as_bool().unwrap_or(false);
    let (rules, classic, workflows, pages, deployments, reporting) = tokio::join!(
        gh.rest(Method::Get, &rules_path, None),
        gh.rest(Method::Get, &classic_path, None),
        gh.graphql(
            WORKFLOWS_QUERY,
            json!({ "owner": owner, "name": name, "expression": format!("{default_branch}:.github/workflows") }),
        ),
        async {
            match has_pages {
                true => gh.rest(Method::Get, &pages_path, None).await.ok(),
                false => None,
            }
        },
        gh.rest(Method::Get, &deployments_path, None),
        async {
            match private {
                true => None,
                false => gh.rest(Method::Get, &reporting_path, None).await.ok(),
            }
        },
    );
    // A pull request is required by a ruleset, or by classic protection.
    let by_rules = rules
        .ok()
        .map(|r| (r.as_array().into_iter().flatten()).any(|rule| rule["type"] == "pull_request"));
    let by_classic = match (admin, classic) {
        (true, Ok(p)) => Some(p.get("required_pull_request_reviews").is_some()),
        (true, Err(e)) if e.status() == Some(404) => Some(false),
        // Only an admin reads classic protection; anyone sees it's absent.
        (false, Ok(b)) if b["protected"] == false => Some(false),
        _ => None,
    };
    let protected = match (by_rules, by_classic) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    };
    // Workflows that couldn't be read leave whether checks run unknown;
    // a repository without any runs none.
    let read = workflows.is_ok();
    let files: Vec<String> = workflows
        .ok()
        .and_then(|w| w["repository"]["object"]["entries"].as_array().cloned())
        .into_iter()
        .flatten()
        .filter(|e| {
            let n = e["name"].as_str().unwrap_or_default();
            n.ends_with(".yml") || n.ends_with(".yaml")
        })
        .filter_map(|e| e["object"]["text"].as_str().map(str::to_owned))
        .collect();
    let (runs_checks, mut deploys) = workflow_triggers(&files);
    let runs_checks = read.then_some(runs_checks);
    deploys |= pages.is_some_and(|p| {
        p["build_type"] == "legacy" && p["source"]["branch"] == default_branch.as_str()
    });
    deploys |= deployments.is_ok_and(|d| d.as_array().is_some_and(|d| !d.is_empty()));
    Ok(GithubRepo {
        owner: owner.to_owned(),
        name: name.to_owned(),
        url: text(&repo["html_url"]),
        private,
        admin,
        push: repo["permissions"]["push"].as_bool().unwrap_or(false),
        owned: repo["owner"]["login"]
            .as_str()
            .is_some_and(|o| o.eq_ignore_ascii_case(login)),
        default_branch,
        protected,
        runs_checks,
        deploys,
        private_reporting: reporting.and_then(|r| r["enabled"].as_bool()),
    })
}

/// What the Through merge warning adds for this repository.
pub(crate) fn warnings(repo: &GithubRepo) -> Vec<String> {
    let branch = &repo.default_branch;
    let mut warnings = Vec::new();
    match repo.protected {
        Some(false) => warnings.push(format!(
            "`{branch}` isn't protected, so nothing stops a push that skips the PR."
        )),
        None => warnings.push(format!(
            "Whether `{branch}` is protected couldn't be read, so a push that skips the PR may not be stopped."
        )),
        Some(true) => {}
    }
    match repo.runs_checks {
        Some(false) => warnings.push("This repository runs no checks on pull requests, so nothing tests the agent's work before it merges.".into()),
        None => warnings.push("This repository's workflows couldn't be read, so whether it runs checks on pull requests, or deploys on a push, isn't known. Nothing merges by itself before a check has reported.".into()),
        Some(true) => {}
    }
    if repo.deploys {
        warnings.push(format!("Pushes to `{branch}` trigger a deployment."));
    }
    warnings
}

/// Whether a task's GitHub link is about item `number` on repository
/// `here`, as `repo_key` names it. A task that has opened a pull request
/// of its own is about that one, not the one it was linked to.
pub(crate) fn refers_to(github: &TaskGithub, here: &str, number: u64, pr: bool) -> bool {
    if github.repo.as_deref().is_some_and(|linked| linked != here) {
        return false;
    }
    if pr {
        match &github.pr {
            Some(p) => p.number == number,
            None => matches!(github.link, GithubLink::Pr { number: n } if n == number),
        }
    } else {
        github.issue == Some(number)
    }
}

/// The newest task in the project working on item `number` on `here`: one
/// that changes it before one that only reads it.
pub(crate) fn linked_task<'a>(
    tasks: &'a [Task],
    here: &str,
    number: u64,
    pr: bool,
) -> Option<&'a Task> {
    tasks
        .iter()
        .filter(|t| t.state != TaskState::Archived)
        .filter(|t| (t.github.as_ref()).is_some_and(|g| refers_to(g, here, number, pr)))
        .max_by_key(|t| (!writes_nothing(t), t.created_at))
}

/// A task whose runs only read and report.
pub(crate) fn writes_nothing(task: &Task) -> bool {
    (task.github.as_ref()).is_some_and(|g| g.link.read_only())
}

/// Whether a finder's fixes start by themselves: as its run chose, or as
/// the project says now. Never while the workflow is off.
pub(crate) fn fixes_by_itself(
    r: &store::Read<'_>,
    project_id: ProjectId,
    chosen: Option<FixFindings>,
) -> store::Result<bool> {
    let settings: GithubSettings = r.get(SETTINGS, project_id)?.unwrap_or_default();
    Ok(settings.mode != GithubMode::Off
        && chosen.unwrap_or(settings.workflow.fix_findings) == FixFindings::ByItself)
}

/// The most issues a finder run files, and what happens to them: its own
/// choice, or the project's.
pub(crate) fn find_limits(link: &GithubLink, settings: &GithubSettings) -> (u32, FixFindings) {
    match link {
        GithubLink::Find {
            max_issues,
            fix_findings,
            ..
        } => (
            max_issues.unwrap_or(settings.workflow.findings_per_run),
            fix_findings.unwrap_or(settings.workflow.fix_findings),
        ),
        _ => (
            settings.workflow.findings_per_run,
            settings.workflow.fix_findings,
        ),
    }
}

pub(crate) fn project_tasks(
    r: &store::Read<'_>,
    project_id: ProjectId,
) -> store::Result<Vec<Task>> {
    Ok(r.list::<Task>("task")?
        .into_iter()
        .filter(|t| t.project_id == project_id && t.github.is_some())
        .collect())
}

/// A new fix for an issue an agent couldn't reproduce answers what that
/// agent asked of the person, so its alert is resolved; the new task speaks
/// for the issue now.
pub(crate) fn tried_again(
    tx: &mut store::Write<'_>,
    task: &Task,
    now: Timestamp,
) -> store::Result<()> {
    let Some(github) = task.github.as_deref() else {
        return Ok(());
    };
    let GithubLink::Issue { number, .. } = github.link else {
        return Ok(());
    };
    for earlier in project_tasks(&tx.read(), task.project_id)? {
        let Some(g) = earlier.github.as_deref() else {
            continue;
        };
        if earlier.task_id != task.task_id
            && g.issue == Some(number)
            && g.repo == github.repo
            && g.stage == Some(PrStage::NotReproduced)
        {
            crate::runs::resolve_task_attention(tx, earlier.task_id, now)?;
            crate::runs::recount_attention(tx, earlier.task_id)?;
        }
    }
    Ok(())
}

/// The agent's pull requests open in the project.
pub(crate) fn open_agent_prs(tasks: &[Task]) -> u32 {
    let open: BTreeSet<u64> = tasks
        .iter()
        .filter_map(|t| t.github.as_ref()?.pr.as_ref())
        .filter(|p| p.state == PrState::Open)
        .map(|p| p.number)
        .collect();
    open.len() as u32
}

/// The pull requests on `here` recorded open on the project's tasks,
/// whether or not the watcher reads them.
pub(crate) fn recorded_open(tasks: &[Task], here: &str) -> BTreeSet<u64> {
    (tasks.iter())
        .filter_map(|t| t.github.as_deref())
        .filter(|g| g.repo.as_deref().is_none_or(|linked| linked == here))
        .filter_map(|g| g.pr.as_ref())
        .filter(|p| p.state == PrState::Open)
        .map(|p| p.number)
        .collect()
}

/// Where an item stands with its task. One waiting its turn in Next up, or
/// stopped part way, is the agent's still, not the person's: only a
/// question, a failure or a pull request it couldn't finish waits for them.
pub(crate) fn item_status(task: &Task) -> Option<ItemStatus> {
    let stage = task.github.as_ref().and_then(|g| g.stage);
    match (task.state, stage) {
        (_, Some(PrStage::Ready | PrStage::Merging)) => Some(ItemStatus::Green),
        (_, Some(PrStage::Capped | PrStage::NotReproduced)) => Some(ItemStatus::WaitingForYou),
        (TaskState::Accepted, _) if task.current_run.is_none() => Some(ItemStatus::Queued),
        (TaskState::Interrupted, _) => Some(ItemStatus::Stopped),
        (
            TaskState::AwaitingDecision | TaskState::Failed | TaskState::ReconciliationRequired,
            _,
        ) => Some(ItemStatus::WaitingForYou),
        _ if task.attention_count > 0 => Some(ItemStatus::WaitingForYou),
        (TaskState::Dispatching | TaskState::Working, _)
        | (_, Some(PrStage::Babysitting | PrStage::Checking)) => Some(ItemStatus::AgentWorking),
        _ => None,
    }
}

/// The item a filter names by number or link, if it names one.
fn wanted_number(filter: &str, owner: &str, name: &str) -> Result<Option<u64>, ProtocolError> {
    if let Ok(n) = filter.trim_start_matches('#').parse::<u64>() {
        return Ok(Some(n));
    }
    let Some(rest) = ["https://", "http://", ""]
        .iter()
        .find_map(|scheme| filter.strip_prefix(scheme)?.strip_prefix("github.com/"))
    else {
        return Ok(None);
    };
    let parts: Vec<&str> = rest.split(['/', '?', '#']).collect();
    let number = match parts.as_slice() {
        [o, n, "issues" | "pull", number, ..] => {
            if !o.eq_ignore_ascii_case(owner) || !n.eq_ignore_ascii_case(name) {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "That link is to another repository",
                ));
            }
            number.parse().ok()
        }
        _ => None,
    };
    number.map(Some).ok_or_else(|| {
        error(
            ErrorCode::InvalidRequest,
            "That isn't a link to an issue or pull request",
        )
    })
}

fn list_query(list: GithubItemKind) -> String {
    let (field, draft) = match list {
        GithubItemKind::Issues => ("issues", ""),
        GithubItemKind::Prs => (
            "pullRequests",
            " isDraft headRefName headRefOid mergeStateStatus commits(last:1){nodes{commit{statusCheckRollup{state}}}}",
        ),
    };
    format!(
        "query($owner:String!,$name:String!){{viewer{{login}} repository(owner:$owner,name:$name){{{field}(states:OPEN,first:100,orderBy:{{field:UPDATED_AT,direction:DESC}}){{totalCount nodes{{number title url updatedAt authorAssociation{draft} author{{login}} labels(first:20){{nodes{{name}}}}}}}}}}}}"
    )
}

fn listed_item(node: &Value, pr: bool, login: &str) -> Option<GithubItem> {
    let author = text(&node["author"]["login"]);
    Some(GithubItem {
        number: node["number"].as_u64()?,
        pr,
        title: text(&node["title"]),
        url: text(&node["url"]),
        author_writes: writes(node["authorAssociation"].as_str().unwrap_or_default()),
        mine: author.eq_ignore_ascii_case(login),
        author,
        draft: node["isDraft"].as_bool().unwrap_or(false),
        labels: (node["labels"]["nodes"].as_array().into_iter().flatten())
            .filter_map(|l| l["name"].as_str().map(str::to_owned))
            .collect(),
        updated_at: timestamp(&node["updatedAt"]),
        task_id: None,
        status: None,
        group: None,
        triage_note: None,
        triage_pick: None,
        grouped_by_you: false,
        checks: pr.then(|| rollup(node)).flatten(),
        merge: pr.then(|| merge_state(node)).flatten(),
        head: (node["headRefOid"].as_str())
            .filter(|_| pr)
            .map(str::to_owned),
    })
}

/// The checks on a pull request's last commit, taken together.
fn rollup(node: &Value) -> Option<CheckState> {
    let state = &node["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"];
    match state.as_str()? {
        "SUCCESS" => Some(CheckState::Passed),
        "FAILURE" | "ERROR" => Some(CheckState::Failed),
        "PENDING" | "EXPECTED" => Some(CheckState::Pending),
        _ => Some(CheckState::Other),
    }
}

/// Whether it can merge; none while GitHub works it out, or for a draft.
fn merge_state(node: &Value) -> Option<MergeState> {
    match node["mergeStateStatus"].as_str()? {
        "CLEAN" | "HAS_HOOKS" | "UNSTABLE" => Some(MergeState::Ready),
        "BLOCKED" => Some(MergeState::Blocked),
        "BEHIND" => Some(MergeState::Behind),
        "DIRTY" => Some(MergeState::Conflict),
        _ => None,
    }
}

/// What is read of a pull request numbered `{n}`.
pub(crate) const PR_FIELDS: &str = "{id number title url body state isDraft isCrossRepository headRefName baseRefName headRefOid autoMergeRequest{enabledAt} reportCommits:commits(first:100){totalCount nodes{commit{oid}}} authorAssociation author{__typename login} mergeable mergeStateStatus reviewThreads(first:100){totalCount nodes{id isResolved path line comments(first:1){nodes{authorAssociation author{__typename login} body url}} latest:comments(last:1){nodes{authorAssociation author{__typename login} body url}}}} commits(last:1){nodes{commit{statusCheckRollup{contexts(first:100){totalCount nodes{__typename ... on CheckRun{name status conclusion detailsUrl isRequired(pullRequestNumber:{n})} ... on StatusContext{context state targetUrl isRequired(pullRequestNumber:{n})}}}}}}}}";

/// What is read of a repository with its pull requests.
pub(crate) const REPO_FIELDS: &str = "squashMergeAllowed mergeCommitAllowed rebaseMergeAllowed";

/// The query behind a pull request's sheet.
fn pr_query() -> String {
    format!(
        "query($owner:String!,$name:String!,$number:Int!){{repository(owner:$owner,name:$name){{{REPO_FIELDS} pullRequest(number:$number){}}}}}",
        PR_FIELDS.replace("{n}", "$number")
    )
}

fn check(node: &Value) -> Option<PrCheck> {
    let (name, state, url) = match node["__typename"].as_str()? {
        "CheckRun" => {
            let state = match (node["status"].as_str(), node["conclusion"].as_str()) {
                (Some("COMPLETED"), Some("SUCCESS")) => CheckState::Passed,
                // Only a check that chose not to judge is neither: one that
                // was cancelled, went stale or ended any other way didn't
                // pass.
                (Some("COMPLETED"), Some("NEUTRAL" | "SKIPPED")) => CheckState::Other,
                (Some("COMPLETED"), _) => CheckState::Failed,
                _ => CheckState::Pending,
            };
            (&node["name"], state, &node["detailsUrl"])
        }
        "StatusContext" => {
            let state = match node["state"].as_str() {
                Some("SUCCESS") => CheckState::Passed,
                Some("FAILURE" | "ERROR") => CheckState::Failed,
                _ => CheckState::Pending,
            };
            (&node["context"], state, &node["targetUrl"])
        }
        _ => return None,
    };
    Some(PrCheck {
        name: text(name),
        state,
        required: node["isRequired"].as_bool().unwrap_or(false),
        url: url.as_str().map(str::to_owned),
    })
}

/// A pull request as `PR_FIELDS` reads it under `pullRequest`, beside the
/// repository's `REPO_FIELDS`, with no task of its own yet.
pub(crate) fn pr_view(repository: &Value) -> Option<GithubPrView> {
    let pr = &repository["pullRequest"];
    let number = pr["number"].as_u64()?;
    let contexts = &pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"];
    let read = contexts["nodes"].as_array().map_or(0, Vec::len);
    let mut checks: Vec<PrCheck> = (contexts["nodes"].as_array().into_iter().flatten())
        .filter_map(check)
        .collect();
    // Checks past the first hundred weren't read, so they count as still
    // running, and as required, which they may be.
    let unread =
        (contexts["totalCount"].as_u64()).map_or(0, |total| total.saturating_sub(read as u64));
    if unread > 0 {
        checks.push(PrCheck {
            name: format!("{unread} more not read here"),
            state: CheckState::Pending,
            required: true,
            url: pr["url"].as_str().map(|url| format!("{url}/checks")),
        });
    }
    let threads: Vec<PrThread> = (pr["reviewThreads"]["nodes"]
        .as_array()
        .into_iter()
        .flatten())
    .map(|t| {
        let first = &t["comments"]["nodes"][0];
        PrThread {
            id: text(&t["id"]),
            resolved: t["isResolved"].as_bool().unwrap_or(false),
            path: t["path"].as_str().map(str::to_owned),
            line: t["line"].as_u64(),
            author: text(&first["author"]["login"]),
            body: cut(first["body"].as_str().unwrap_or_default(), MAX_COMMENT),
            url: text(&first["url"]),
        }
    })
    .collect();
    let count = |state| checks.iter().filter(|c| c.state == state).count() as u32;
    // Threads past the first hundred weren't read, so count them as open.
    let unread = (pr["reviewThreads"]["totalCount"].as_u64())
        .map_or(0, |total| total.saturating_sub(threads.len() as u64));
    let unresolved = threads.iter().filter(|t| !t.resolved).count() as u64 + unread;
    let state = match pr["state"].as_str() {
        Some("MERGED") => PrState::Merged,
        Some("CLOSED") => PrState::Closed,
        _ => PrState::Open,
    };
    let base_branch = text(&pr["baseRefName"]);
    let merge_state = pr["mergeStateStatus"].as_str().unwrap_or_default();
    let blocked = match (state, merge_state) {
        (PrState::Merged, _) => Some("It's merged.".to_owned()),
        (PrState::Closed, _) => Some("It's closed.".to_owned()),
        (_, "CLEAN" | "HAS_HOOKS" | "UNSTABLE") => None,
        (_, "DRAFT") => Some("It's still a draft.".to_owned()),
        (_, "DIRTY") => Some(format!("It conflicts with `{base_branch}`.")),
        (_, "BEHIND") => Some(format!(
            "It's behind `{base_branch}`, and GitHub wants it up to date first."
        )),
        (_, "BLOCKED") => Some(format!(
            "GitHub's rules for `{base_branch}` hold it: a required check or review is missing."
        )),
        _ => Some("GitHub is still working out whether it can merge.".to_owned()),
    };
    let method = [
        ("squashMergeAllowed", MergeMethod::Squash),
        ("mergeCommitAllowed", MergeMethod::Merge),
        ("rebaseMergeAllowed", MergeMethod::Rebase),
    ]
    .into_iter()
    .find(|(allowed, _)| repository[*allowed] == true)
    .map(|(_, method)| method);
    Some(GithubPrView {
        repo: String::new(),
        summary: PrSummary {
            number,
            url: text(&pr["url"]),
            state,
            draft: pr["isDraft"].as_bool().unwrap_or(false),
            head: text(&pr["headRefOid"]),
            passed: count(CheckState::Passed),
            failed: count(CheckState::Failed),
            pending: count(CheckState::Pending),
            unresolved: u32::try_from(unresolved).unwrap_or(u32::MAX),
            conflict: pr["mergeable"] == "CONFLICTING",
        },
        title: text(&pr["title"]),
        body: cut(pr["body"].as_str().unwrap_or_default(), MAX_BODY),
        author: text(&pr["author"]["login"]),
        head_branch: text(&pr["headRefName"]),
        base_branch,
        checks,
        threads,
        mergeable: blocked.is_none(),
        blocked,
        method,
        task_id: None,
        roles: Vec::new(),
    })
}

pub(crate) fn merge_allowed(repository: &Value, method: MergeMethod) -> bool {
    let field = match method {
        MergeMethod::Squash => "squashMergeAllowed",
        MergeMethod::Merge => "mergeCommitAllowed",
        MergeMethod::Rebase => "rebaseMergeAllowed",
    };
    repository[field] == true
}

/// Who ran as `harness` on `model`, by name.
pub(crate) fn who(harness: HarnessKind, model: Option<&str>) -> String {
    model
        .map(model_name)
        .unwrap_or_else(|| crate::agent_browser::harness_name(harness).to_owned())
}

/// "Found by … · Fixed by … · Checked by …" for a pull request's task.
fn roles(task: &Task) -> Vec<String> {
    let Some(github) = &task.github else {
        return Vec::new();
    };
    let maker = who(task.harness, task.model.as_deref());
    let mut roles = Vec::new();
    match &github.found_by {
        Some(f) => {
            roles.push(format!("Found by {}", who(f.harness, f.model.as_deref())));
            roles.push(format!("Fixed by {maker}"));
        }
        None => roles.push(format!("Made by {maker}")),
    }
    if let Some(c) = &github.checked_by {
        roles.push(format!("Checked by {}", who(c.harness, c.model.as_deref())));
    }
    roles
}

/// A short, branch-safe name from a title.
fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// What a linked task starts with, read from GitHub as it is created.
pub(crate) struct PreparedLink {
    pub github: TaskGithub,
    /// The issue's or pull request's title, for a task given none.
    pub title: Option<String>,
    /// Where the work goes, for a task given no branch.
    pub branch: Option<TaskBranch>,
    /// Read-only work, and work from strangers, begins in Plan.
    pub plan: bool,
}

impl CompanionHub {
    pub(crate) fn github(&self) -> Result<Arc<dyn Github>, ProtocolError> {
        let inner = self
            .inner
            .github
            .clone()
            .ok_or_else(|| unsupported("The GitHub workflow"))?;
        Ok(Arc::new(crate::reports::github::Calls {
            inner,
            hub: Arc::downgrade(&self.inner),
        }))
    }

    pub(crate) fn github_settings(
        &self,
        project_id: ProjectId,
    ) -> Result<GithubSettings, ProtocolError> {
        self.read(|r| Ok(r.get(SETTINGS, project_id)?.unwrap_or_default()))
    }

    /// The project's `origin`, when it is on GitHub.
    async fn origin(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<(String, String)>, ProtocolError> {
        let project: Project = self.read(|r| r.require("project", project_id))?;
        let url = crate::git::output(
            Path::new(&project.host_path),
            &["remote", "get-url", "origin"],
        )
        .await;
        Ok(url.as_deref().and_then(github_remote))
    }

    /// When the project's folder isn't a Git repository, the folders
    /// directly in it that are, with their `origin` on GitHub, by name.
    /// Worktrees and hidden folders are passed over: a worktree's
    /// repository is listed itself.
    async fn not_a_repo(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<Vec<ProjectFolder>>, ProtocolError> {
        let project: Project = self.read(|r| r.require("project", project_id))?;
        let root = Path::new(&project.host_path);
        if crate::git::output(root, &["rev-parse", "--git-dir"])
            .await
            .is_some()
        {
            return Ok(None);
        }
        let mut candidates = Vec::new();
        if let Ok(mut dir) = tokio::fs::read_dir(root).await {
            while candidates.len() < REPO_FOLDERS_READ
                && let Ok(Some(entry)) = dir.next_entry().await
            {
                let name = entry.file_name().to_string_lossy().into_owned();
                let git = tokio::fs::metadata(entry.path().join(".git")).await;
                if !name.starts_with('.') && git.is_ok_and(|m| m.is_dir()) {
                    candidates.push((name, entry.path()));
                }
            }
        }
        candidates.sort();
        let projects = self.read(|r| r.list::<Project>("project"))?;
        let mut folders = Vec::new();
        for (name, path) in candidates {
            let url = crate::git::output(&path, &["remote", "get-url", "origin"]).await;
            if url.as_deref().and_then(github_remote).is_none() {
                continue;
            }
            let path = path.to_string_lossy().into_owned();
            folders.push(ProjectFolder {
                project_id: (projects.iter())
                    .find(|p| p.host_path == path)
                    .map(|p| p.project_id),
                name,
                path,
                unavailable: None,
            });
            if folders.len() == REPO_FOLDERS_SHOWN {
                break;
            }
        }
        Ok(Some(folders))
    }

    pub(crate) async fn on_github(
        &self,
        project_id: ProjectId,
    ) -> Result<(String, String), ProtocolError> {
        self.origin(project_id)
            .await?
            .ok_or_else(|| error(ErrorCode::Unsupported, NOT_ON_GITHUB))
    }

    /// The project's repository read now, and kept for the watcher.
    pub(crate) async fn github_repo(
        &self,
        project_id: ProjectId,
    ) -> Result<GithubRepo, ProtocolError> {
        let gh = self.github()?;
        let (owner, name) = self.on_github(project_id).await?;
        let login = login(&*gh).await?;
        let repo = read_repo(&*gh, &owner, &name, &login).await?;
        self.write(|tx| tx.put(REPO, project_id, &repo))?;
        Ok(repo)
    }

    /// `owner/name` read now, or while GitHub can't be read, what was kept
    /// of it: never the repository `origin` names by then, or named
    /// before.
    pub(crate) async fn github_repo_or_kept(
        &self,
        project_id: ProjectId,
        owner: &str,
        name: &str,
    ) -> Result<GithubRepo, ProtocolError> {
        let read = async {
            let gh = self.github()?;
            let login = login(&*gh).await?;
            read_repo(&*gh, owner, name, &login).await
        };
        match read.await {
            Ok(repo) => {
                self.write(|tx| tx.put(REPO, project_id, &repo))?;
                Ok(repo)
            }
            Err(e) => (self.read(|r| r.get::<GithubRepo>(REPO, project_id))?)
                .filter(|kept| repo_key(&kept.owner, &kept.name) == repo_key(owner, name))
                .ok_or(e),
        }
    }

    pub(crate) async fn github_project(
        &self,
        project_id: ProjectId,
    ) -> Result<GithubProjectView, ProtocolError> {
        let gh = self.github()?;
        let settings = self.github_settings(project_id)?;
        let tasks = self.read(|r| project_tasks(r, project_id))?;
        let mut view = GithubProjectView {
            project_id,
            login: None,
            repo: None,
            problem: None,
            repo_folders: Vec::new(),
            settings,
            warnings: Vec::new(),
            open_agent_prs: open_agent_prs(&tasks),
            run: self.read(|r| crate::github_run::issue_run(r, project_id))?,
            together: self.read(|r| crate::github_together::together_view(r, project_id))?,
        };
        let origin = self.origin(project_id).await?;
        // Signing in wouldn't help a folder that isn't a repository.
        let not_a_repo = match origin {
            Some(_) => None,
            None => self.not_a_repo(project_id).await?,
        };
        if let Some(folders) = &not_a_repo {
            view.repo_folders = folders.clone();
            view.problem = Some(match folders.as_slice() {
                [] => NOT_A_REPO.into(),
                [one] => format!(
                    "This project's folder isn't a Git repository, so GitHub can't be used with it. The folder {} inside it is one, on GitHub: use GitHub in a project of that folder instead.",
                    one.name
                ),
                more => format!(
                    "This project's folder isn't a Git repository, so GitHub can't be used with it. The folders {} inside it are, on GitHub: use GitHub in a project of one of those instead.",
                    more.iter()
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        let login = match login(&*gh).await {
            Ok(login) => login,
            Err(e) => {
                view.problem.get_or_insert(e.message);
                return Ok(view);
            }
        };
        view.login = Some(login.clone());
        let Some((owner, name)) = origin else {
            view.problem.get_or_insert_with(|| NOT_ON_GITHUB.into());
            return Ok(view);
        };
        match read_repo(&*gh, &owner, &name, &login).await {
            Ok(repo) => {
                self.write(|tx| tx.put(REPO, project_id, &repo))?;
                // An answer for another repository isn't one for this.
                view.settings.warning_answered = warning_answered(&view.settings, &repo);
                view.warnings = warnings(&repo);
                view.repo = Some(repo);
            }
            Err(e) => view.problem = Some(e.message),
        }
        Ok(view)
    }

    pub(crate) async fn github_items(
        &self,
        project_id: ProjectId,
        list: GithubItemKind,
        filter: Option<&str>,
    ) -> Result<GithubItems, ProtocolError> {
        let gh = self.github()?;
        let filter = filter.map(str::trim).filter(|f| !f.is_empty());
        if filter.is_some_and(|f| f.len() > MAX_FILTER) {
            return Err(error(ErrorCode::LimitExceeded, "That filter is too long"));
        }
        let (owner, name) = self.on_github(project_id).await?;
        let pr = list == GithubItemKind::Prs;
        let wanted = filter
            .map(|f| wanted_number(f, &owner, &name))
            .transpose()?
            .flatten();
        // Every open pull request, once all were listed.
        let mut all_open: Option<BTreeSet<u64>> = None;
        let (mut items, truncated): (Vec<GithubItem>, bool) = match wanted {
            Some(number) => {
                let login = login(&*gh).await?;
                let path = format!("/repos/{owner}/{name}/issues/{number}");
                let found = match gh.rest(Method::Get, &path, None).await {
                    Ok(found) => found,
                    Err(e) if e.status() == Some(404) => Value::Null,
                    Err(e) => return Err(github_error(e)),
                };
                let open = found["state"] == "open" && found.get("pull_request").is_some() == pr;
                let node = json!({
                    "number": found["number"],
                    "title": found["title"],
                    "url": found["html_url"],
                    "updatedAt": found["updated_at"],
                    "authorAssociation": found["author_association"],
                    "isDraft": found["draft"],
                    "author": { "login": found["user"]["login"] },
                    "labels": { "nodes": found["labels"] },
                });
                let item = open.then(|| listed_item(&node, pr, &login)).flatten();
                (item.into_iter().collect(), false)
            }
            None => {
                let data = gh
                    .graphql(&list_query(list), json!({ "owner": owner, "name": name }))
                    .await
                    .map_err(github_error)?;
                let login = text(&data["viewer"]["login"]);
                let field = if pr { "pullRequests" } else { "issues" };
                let listed = &data["repository"][field];
                let nodes = listed["nodes"].as_array().cloned().unwrap_or_default();
                let truncated = listed["totalCount"].as_u64().unwrap_or(0) > nodes.len() as u64;
                if pr && !truncated {
                    all_open = Some(nodes.iter().filter_map(|n| n["number"].as_u64()).collect());
                }
                let words = filter.map(str::to_lowercase);
                // Øphio's own merge checks open and close by themselves.
                let mine = |n: &&Value| {
                    pr && text(&n["headRefName"]).starts_with(crate::github_train::MERGE_BRANCH)
                };
                let items = nodes
                    .iter()
                    .filter(|n| !mine(n))
                    .filter_map(|n| listed_item(n, pr, &login))
                    .filter(|i| {
                        (words.as_deref()).is_none_or(|w| i.title.to_lowercase().contains(w))
                    })
                    .collect();
                (items, truncated)
            }
        };
        let (tasks, triage) = self.read(|r| {
            let triage: Vec<Triaged> = r.get(TRIAGE, project_id)?.unwrap_or_default();
            Ok((project_tasks(r, project_id)?, triage))
        })?;
        let here = repo_key(&owner, &name);
        // One recorded open here and not listed was merged or closed on
        // GitHub since, where the watcher doesn't read it.
        if let Some(open) = all_open {
            let lapsed: BTreeSet<u64> = (recorded_open(&tasks, &here).into_iter())
                .filter(|n| !open.contains(n))
                .collect();
            if !lapsed.is_empty()
                && let Err(e) = (self.settle_prs(&*gh, project_id, (&owner, &name), &lapsed)).await
            {
                tracing::warn!(code = ?e.code, "reading merged or closed pull requests failed");
            }
        }
        for item in &mut items {
            if let Some(task) = linked_task(&tasks, &here, item.number, pr) {
                item.task_id = Some(task.task_id);
                item.status = item_status(task);
            }
            let triaged = (triage.iter()).find(|t| t.pr == pr && t.number == item.number);
            // An agent found it couldn't make the issue happen, and its task
            // is gone: the person decides, not another fix. Moved back by
            // hand, it's the agent's again.
            let checked = !pr
                && item.task_id.is_none()
                && (item.labels.iter()).any(|l| l == crate::github_tools::NOT_REPRODUCED_LABEL);
            match triaged {
                Some(t) if t.by_you || !checked => {
                    item.group = Some(t.group);
                    item.triage_note = Some(t.note.clone());
                    item.triage_pick = (!t.by_you).then(|| t.pick.clone()).flatten();
                    item.grouped_by_you = t.by_you;
                }
                _ if checked => {
                    item.group = Some(TriageGroup::NeedsYou);
                    item.triage_note = Some(CHECKED_BEFORE.into());
                }
                _ => {}
            }
        }
        items.sort_by(|a, b| b.mine.cmp(&a.mine).then(b.updated_at.cmp(&a.updated_at)));
        Ok(GithubItems { items, truncated })
    }

    /// One pull request read now, with the repository it was read under.
    pub(crate) async fn read_pr(
        &self,
        project_id: ProjectId,
        number: u64,
    ) -> Result<(String, String, Value, GithubPrView), ProtocolError> {
        self.github()?;
        let (owner, name) = self.on_github(project_id).await?;
        let (repository, view) = self.read_pr_in(&owner, &name, number).await?;
        Ok((owner, name, repository, view))
    }

    /// One pull request of a named repository, whatever `origin` names now.
    pub(crate) async fn read_pr_in(
        &self,
        owner: &str,
        name: &str,
        number: u64,
    ) -> Result<(Value, GithubPrView), ProtocolError> {
        let gh = self.github()?;
        let mut data = gh
            .graphql(
                &pr_query(),
                json!({ "owner": owner, "name": name, "number": number }),
            )
            .await
            .map_err(github_error)?;
        let repository = data["repository"].take();
        let view = pr_view(&repository).ok_or_else(|| {
            error(
                ErrorCode::NotFound,
                format!("{owner}/{name} has no pull request #{number}"),
            )
        })?;
        Ok((repository, view))
    }

    /// The project's repository on GitHub while it is still `shown`, the
    /// one the person read: what they confirmed acts there or not at all.
    async fn shown_repo(
        &self,
        project_id: ProjectId,
        shown: &str,
        again: &str,
    ) -> Result<(String, String), ProtocolError> {
        let (owner, name) = self.on_github(project_id).await?;
        if repo_key(&owner, &name) != shown.to_lowercase() {
            return Err(error(
                ErrorCode::Conflict,
                format!(
                    "This project's `origin` is {owner}/{name} now, not the repository {again}"
                ),
            ));
        }
        Ok((owner, name))
    }

    pub(crate) async fn github_pr(
        &self,
        project_id: ProjectId,
        number: u64,
    ) -> Result<GithubPrView, ProtocolError> {
        let (owner, name, _, mut view) = self.read_pr(project_id, number).await?;
        view.repo = format!("{owner}/{name}");
        let tasks = self.read(|r| project_tasks(r, project_id))?;
        if let Some(task) = linked_task(&tasks, &repo_key(&owner, &name), number, true) {
            view.task_id = Some(task.task_id);
            view.roles = roles(task);
        }
        Ok(view)
    }

    /// Records what changed about pull request `number` on `owner/name` on
    /// the tasks about it, and says it changed.
    pub(crate) fn record_pr(
        &self,
        project_id: ProjectId,
        (owner, name): (&str, &str),
        number: u64,
        change: impl Fn(&mut TaskGithub),
    ) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        let here = repo_key(owner, name);
        self.write(|tx| {
            let mut changed = Vec::new();
            for mut task in project_tasks(&tx.read(), project_id)? {
                let Some(github) = task.github.as_mut() else {
                    continue;
                };
                if !refers_to(github, &here, number, true) {
                    continue;
                }
                change(github);
                task.revision += 1;
                task.updated_at = now;
                crate::runs::carry_permits(tx, &task, task.revision - 1)?;
                tx.put("task", task.task_id, &task)?;
                tx.emit(Event::TaskUpserted { task: task.clone() })?;
                changed.push(task);
            }
            tx.emit(Event::PrChanged {
                project_id,
                number,
                task_id: linked_task(&changed, &here, number, true).map(|t| t.task_id),
            })?;
            Ok(())
        })
    }

    pub(crate) async fn github_action(
        &self,
        caller: &Caller,
        action: &GithubAction,
    ) -> Result<Option<CommandResult>, ProtocolError> {
        match action {
            GithubAction::SetSettings {
                project_id,
                settings,
            } => {
                valid_settings(settings)?;
                if settings.mode != GithubMode::Off {
                    let repo = self.github_repo(*project_id).await?;
                    through_merge_allowed(settings.mode, &repo)?;
                }
                self.write(|tx| {
                    let current: GithubSettings =
                        tx.read().get(SETTINGS, project_id)?.unwrap_or_default();
                    let mut next = settings.clone();
                    // Only the warning's own answer answers it.
                    (next.warning_answered, next.warned_repo) = match next.mode {
                        GithubMode::ThroughMerge if current.mode != GithubMode::ThroughMerge => {
                            (false, None)
                        }
                        _ => (current.warning_answered, current.warned_repo),
                    };
                    tx.put(SETTINGS, project_id, &next)?;
                    tx.emit(Event::GithubChanged {
                        project_id: *project_id,
                    })?;
                    Ok(())
                })?;
            }
            GithubAction::AnswerWarning {
                project_id,
                repo: shown,
                keep_through_merge,
            } => {
                let warned = match keep_through_merge {
                    true => {
                        let repo = self.github_repo(*project_id).await?;
                        through_merge_allowed(GithubMode::ThroughMerge, &repo)?;
                        // Consent is for the repository whose warning was
                        // read, not whichever `origin` names by now.
                        let key = repo_key(&repo.owner, &repo.name);
                        if key != shown.to_lowercase() {
                            return Err(error(
                                ErrorCode::Conflict,
                                format!(
                                    "This project's `origin` is {}/{} now, not the repository whose warning was answered. Read its warning again.",
                                    repo.owner, repo.name
                                ),
                            ));
                        }
                        key
                    }
                    false => {
                        let (owner, name) = self.on_github(*project_id).await?;
                        repo_key(&owner, &name)
                    }
                };
                self.write(|tx| {
                    let mut settings: GithubSettings =
                        tx.read().get(SETTINGS, project_id)?.unwrap_or_default();
                    if settings.mode != GithubMode::ThroughMerge {
                        return Err(error(
                            ErrorCode::Conflict,
                            "Through merge isn't chosen for this project any more",
                        )
                        .into());
                    }
                    if !keep_through_merge {
                        settings.mode = GithubMode::Drafts;
                    }
                    settings.warning_answered = true;
                    settings.warned_repo = Some(warned);
                    tx.put(SETTINGS, project_id, &settings)?;
                    tx.emit(Event::GithubChanged {
                        project_id: *project_id,
                    })?;
                    Ok(())
                })?;
            }
            GithubAction::ProtectBranch { project_id } => {
                let gh = self.github()?;
                let repo = self.github_repo(*project_id).await?;
                if !repo.admin {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        format!(
                            "Only an admin of {}/{} can protect `{}`",
                            repo.owner, repo.name, repo.default_branch
                        ),
                    ));
                }
                let path = format!(
                    "/repos/{}/{}/branches/{}/protection",
                    repo.owner,
                    repo.name,
                    segment(&repo.default_branch)
                );
                // Protecting replaces what the branch had, so what it had
                // is read first and kept; one unread protects nothing.
                let had = match gh.rest(Method::Get, &path, None).await {
                    Ok(had) => Some(had),
                    Err(e) if e.status() == Some(404) => None,
                    Err(e) => return Err(github_error(e)),
                };
                let checks = recent_pr_checks(&*gh, &repo).await.map_err(github_error)?;
                if let Some(body) = protection(had.as_ref(), checks) {
                    gh.rest(Method::Put, &path, Some(body))
                        .await
                        .map_err(github_error)?;
                }
                self.github_repo(*project_id).await?;
                self.write(|tx| {
                    tx.emit(Event::GithubChanged {
                        project_id: *project_id,
                    })?;
                    Ok(())
                })?;
            }
            GithubAction::MarkPrReady {
                project_id,
                repo: shown,
                number,
            } => {
                unlocked(caller)?;
                let gh = self.github()?;
                let (owner, name) = (self.shown_repo(*project_id, shown, PR_AGAIN)).await?;
                let (repository, view) = self.read_pr_in(&owner, &name, *number).await?;
                if view.summary.state != PrState::Open {
                    return Err(error(
                        ErrorCode::Conflict,
                        format!("Pull request #{number} isn't open"),
                    ));
                }
                if view.summary.draft {
                    gh.graphql(
                        "mutation($id:ID!){markPullRequestReadyForReview(input:{pullRequestId:$id}){pullRequest{isDraft}}}",
                        json!({ "id": repository["pullRequest"]["id"] }),
                    )
                    .await
                    .map_err(github_error)?;
                }
                self.record_pr(*project_id, (&owner, &name), *number, |g| {
                    if let Some(pr) = g.pr.as_mut() {
                        pr.draft = false;
                    }
                })?;
            }
            GithubAction::FileIssue {
                project_id,
                repo: shown,
                title,
                body,
            } => {
                from_phone(
                    caller,
                    "File issues from your phone, since they're filed in your name",
                )?;
                let title = crate::github_tools::checked_title(title)?;
                let body = body.trim().replace('\r', "");
                crate::github_tools::checked_body(&body)?;
                let gh = self.github()?;
                let (owner, name) = (self.shown_repo(*project_id, shown, ISSUE_AGAIN)).await?;
                let base = format!("/repos/{owner}/{name}");
                let made = json!({ "title": title, "body": body });
                let issue = match gh
                    .rest(Method::Post, &format!("{base}/issues"), Some(made))
                    .await
                {
                    Ok(issue) => issue,
                    // It may have been filed before the answer was lost.
                    Err(e) if crate::github_tools::uncertain(&e) => {
                        let login = login(&*gh).await?;
                        crate::github_tools::recently_filed_in(&*gh, &base, &login, &title, &body)
                            .await
                            .ok_or_else(|| github_error(e))?
                    }
                    Err(e) => return Err(github_error(e)),
                };
                let number = issue["number"].as_u64().ok_or_else(|| {
                    error(
                        ErrorCode::NativeFailure,
                        "GitHub didn't say which issue it filed",
                    )
                })?;
                self.write(|tx| {
                    tx.emit(Event::GithubChanged {
                        project_id: *project_id,
                    })?;
                    Ok(())
                })?;
                return Ok(Some(CommandResult::IssueFiled {
                    number,
                    url: text(&issue["html_url"]),
                }));
            }
            GithubAction::CloseIssue {
                project_id,
                repo: shown,
                number,
                done,
            } => {
                from_phone(
                    caller,
                    "Close issues from your phone, since they're closed in your name",
                )?;
                let gh = self.github()?;
                let (owner, name) = (self.shown_repo(*project_id, shown, ISSUE_READ_AGAIN)).await?;
                let path = format!("/repos/{owner}/{name}/issues/{number}");
                let issue = gh
                    .rest(Method::Get, &path, None)
                    .await
                    .map_err(github_error)?;
                if issue.get("pull_request").is_some() {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        format!("#{number} is a pull request, not an issue"),
                    ));
                }
                if issue["state"] == "open" {
                    let reason = if *done { "completed" } else { "not_planned" };
                    let close = json!({ "state": "closed", "state_reason": reason });
                    (gh.rest(Method::Patch, &path, Some(close)).await).map_err(github_error)?;
                }
                self.issue_closed(*project_id, &repo_key(&owner, &name), *number)?;
            }
            GithubAction::RunIssues { project_id, step } => {
                self.run_issues(caller, *project_id, *step).await?;
            }
            GithubAction::SetGroup {
                project_id,
                number,
                pr,
                group,
            } => {
                self.set_group(*project_id, *number, *pr, *group).await?;
            }
            GithubAction::MergePr {
                project_id,
                repo: shown,
                number,
                expected_head,
                method,
                approval_shown,
            } => {
                unlocked(caller)?;
                let gh = self.github()?;
                let (owner, name) = (self.shown_repo(*project_id, shown, PR_AGAIN)).await?;
                let (repository, view) = self.read_pr_in(&owner, &name, *number).await?;
                if view.summary.state != PrState::Open {
                    return Err(error(
                        ErrorCode::Conflict,
                        format!("Pull request #{number} isn't open"),
                    ));
                }
                if view.summary.head != *expected_head {
                    let short: String = view.summary.head.chars().take(7).collect();
                    return Err(error(
                        ErrorCode::Conflict,
                        format!(
                            "Pull request #{number} has a new commit, {short}, since you looked. Check it again before merging."
                        ),
                    ));
                }
                if view.summary.draft {
                    return Err(error(
                        ErrorCode::Conflict,
                        format!("Pull request #{number} is a draft. Mark it ready first."),
                    ));
                }
                if !merge_allowed(&repository, *method) {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "This repository doesn't allow that kind of merge",
                    ));
                }
                let merge_method = match method {
                    MergeMethod::Squash => "squash",
                    MergeMethod::Merge => "merge",
                    MergeMethod::Rebase => "rebase",
                };
                // The person's Merge, pinned to this commit, approves a
                // sensitive change: once they could have seen why it asks.
                let weighed = (self.approval_for(
                    *project_id,
                    (&owner, &name),
                    *number,
                    expected_head,
                    &view.base_branch,
                ))
                .await?;
                // Nor does it approve a reason they weren't shown.
                let unseen = |n: &&_| crate::review_gate::unshown(n, approval_shown.as_deref());
                if let Some(need) = (weighed.need.as_ref()).filter(|n| weighed.fresh || unseen(n)) {
                    return Err(crate::review_gate::look_again(*number, need));
                }
                let report_fix = self.is_report_fix(&owner, &name, &repository).await?;
                if report_fix {
                    if !repository["pullRequest"]["autoMergeRequest"].is_null() {
                        return Err(error(
                            ErrorCode::PermissionDenied,
                            "Turn off auto-merge before merging this report fix",
                        ));
                    }
                    self.check_report_fix_merge(&owner, &name, *number, expected_head)
                        .await?;
                }
                gh.rest(
                    Method::Put,
                    &format!("/repos/{owner}/{name}/pulls/{number}/merge"),
                    Some(json!({ "sha": expected_head, "merge_method": merge_method })),
                )
                .await
                .map_err(github_error)?;
                if report_fix {
                    self.report_fix_tap(&repo_key(&owner, &name), *number, expected_head, caller)?;
                }
                self.record_pr(*project_id, (&owner, &name), *number, |g| {
                    if let Some(pr) = g.pr.as_mut() {
                        pr.state = PrState::Merged;
                    }
                    g.stage = Some(PrStage::Merged);
                })?;
            }
            GithubAction::MergeTogether {
                project_id,
                repo: shown,
                prs,
            } => {
                unlocked(caller)?;
                self.github()?;
                let (owner, name) = (self.shown_repo(*project_id, shown, PRS_AGAIN)).await?;
                self.merge_together(*project_id, (&owner, &name), prs)
                    .await?;
            }
            GithubAction::StopTogether { project_id } => {
                self.stop_together(*project_id).await?;
            }
        }
        Ok(None)
    }

    /// What the person closing issue `number` on `here` settles: its fix
    /// waiting in Next up leaves it and Run all, a fixer's report that it
    /// couldn't reproduce it asks nothing more, and the Sort's word on it
    /// goes.
    fn issue_closed(
        &self,
        project_id: ProjectId,
        here: &str,
        number: u64,
    ) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            let tasks = project_tasks(&tx.read(), project_id)?;
            if let Some(task) = linked_task(&tasks, here, number, false) {
                let task_id = task.task_id;
                let reported =
                    (task.github.as_ref()).is_some_and(|g| g.stage == Some(PrStage::NotReproduced));
                if item_status(task) == Some(ItemStatus::Queued) {
                    let why = "Taken out of Next up: you closed its issue";
                    crate::github_run::take_out_fix(self, tx, project_id, task_id, why, now)?;
                } else if reported {
                    let mut task = task.clone();
                    if let Some(g) = task.github.as_mut() {
                        g.stage = Some(PrStage::Closed);
                    }
                    task.revision += 1;
                    task.updated_at = now;
                    tx.put("task", task_id, &task)?;
                    tx.emit(Event::TaskUpserted { task })?;
                    crate::runs::resolve_task_attention(tx, task_id, now)?;
                    crate::runs::recount_attention(tx, task_id)?;
                }
            }
            let mut triage: Vec<Triaged> = tx.read().get(TRIAGE, project_id)?.unwrap_or_default();
            triage.retain(|t| t.pr || t.number != number);
            tx.put(TRIAGE, project_id, &triage)?;
            tx.emit(Event::GithubChanged { project_id })?;
            Ok(())
        })
    }

    /// Reads what a new task linked to GitHub starts with: the issue or pull
    /// request it is for, whether a stranger wrote it, and where its work
    /// goes.
    pub(crate) async fn prepare_link(
        &self,
        project_id: ProjectId,
        kind: TaskKind,
        info: Option<&HarnessInfo>,
        link: &GithubLink,
    ) -> Result<PreparedLink, ProtocolError> {
        // A finder that uses the app does it on a screen; every other link
        // is work on the source.
        match (link.uses_it(), kind) {
            (false, TaskKind::Coding) | (true, TaskKind::ComputerUse) => {}
            (true, _) => {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "A reviewer that uses the app works on a screen, so it starts as computer use",
                ));
            }
            (false, _) => return Err(unsupported("A GitHub link for this kind of task")),
        }
        let settings = self.github_settings(project_id)?;
        if settings.mode == GithubMode::Off {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Turn on the GitHub workflow for this project in Tasks › GitHub first",
            ));
        }
        let gh = self.github()?;
        let (owner, name) = self.on_github(project_id).await?;
        let base = format!("/repos/{owner}/{name}");
        let mut prepared = PreparedLink {
            github: TaskGithub {
                link: link.clone(),
                issue: None,
                pr: None,
                stage: None,
                found_by: None,
                checked_by: None,
                check_with: None,
                from_stranger: false,
                base: None,
                rounds: 0,
                repo: Some(repo_key(&owner, &name)),
                finding: None,
                already_fixed: false,
                approval: None,
                review: None,
            },
            title: None,
            branch: None,
            plan: link.plan(),
        };
        let prefix = &settings.branch_prefix;
        match link {
            GithubLink::Issue {
                number,
                review_with,
            } => {
                if let Some(by) = review_with {
                    valid_choice(by, "review with")?;
                }
                prepared.github.check_with = review_with.clone();
                let issue = gh
                    .rest(Method::Get, &format!("{base}/issues/{number}"), None)
                    .await
                    .map_err(github_error)?;
                if issue.get("pull_request").is_some() {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        format!("#{number} is a pull request; link it as one"),
                    ));
                }
                if issue["state"] != "open" {
                    return Err(error(
                        ErrorCode::Conflict,
                        format!("Issue #{number} is closed"),
                    ));
                }
                let title = text(&issue["title"]);
                prepared.github.issue = Some(*number);
                prepared.github.from_stranger =
                    crate::github_access::untrusted(Some(false), std::slice::from_ref(&issue));
                prepared.branch = Some(TaskBranch {
                    name: format!("{prefix}{number}-{}", slug(&title)),
                    new: true,
                });
                prepared.title = Some(cut(&title, 120));
            }
            GithubLink::Pr { number } | GithubLink::Check { number } => {
                let pr = gh
                    .rest(Method::Get, &format!("{base}/pulls/{number}"), None)
                    .await
                    .map_err(github_error)?;
                if pr["state"] != "open" {
                    return Err(error(
                        ErrorCode::Conflict,
                        format!("Pull request #{number} isn't open"),
                    ));
                }
                let title = text(&pr["title"]);
                let head = text(&pr["head"]["ref"]);
                let ours = pr["head"]["repo"]["full_name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&format!("{owner}/{name}")));
                prepared.github.from_stranger =
                    crate::github_access::untrusted(Some(false), std::slice::from_ref(&pr));
                prepared.github.pr = Some(PrSummary {
                    number: *number,
                    url: text(&pr["html_url"]),
                    state: PrState::Open,
                    draft: pr["draft"].as_bool().unwrap_or(false),
                    head: text(&pr["head"]["sha"]),
                    passed: 0,
                    failed: 0,
                    pending: 0,
                    unresolved: 0,
                    conflict: pr["mergeable_state"] == "dirty",
                });
                if matches!(link, GithubLink::Pr { .. }) {
                    // A fork's branch is out of reach: the work goes on a
                    // new branch, for a replacement that credits it.
                    prepared.branch = Some(match ours {
                        true => TaskBranch {
                            name: head,
                            new: false,
                        },
                        false => TaskBranch {
                            name: format!("{prefix}pr-{number}-{}", slug(&title)),
                            new: true,
                        },
                    });
                    prepared.github.stage = Some(PrStage::Babysitting);
                }
                prepared.title = Some(match link {
                    GithubLink::Check { .. } => format!("Check #{number}: {}", cut(&title, 120)),
                    _ => cut(&title, 120),
                });
            }
            GithubLink::NewPr => {}
            GithubLink::Stack { number } => {
                let pr = gh
                    .rest(Method::Get, &format!("{base}/pulls/{number}"), None)
                    .await
                    .map_err(github_error)?;
                if pr["state"] != "open" {
                    return Err(error(
                        ErrorCode::Conflict,
                        format!("Pull request #{number} isn't open"),
                    ));
                }
                // A pull request here can't target a fork's branch.
                let ours = pr["head"]["repo"]["full_name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&format!("{owner}/{name}")));
                if !ours {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        format!(
                            "Pull request #{number}'s branch is on a fork, so nothing can stack on it"
                        ),
                    ));
                }
                prepared.github.base = Some(text(&pr["head"]["ref"]));
            }
            GithubLink::Find {
                scope,
                fix_with,
                max_issues,
                ..
            } => {
                if let FindScope::Folder { path } = scope {
                    let project: Project = self.read(|r| r.require("project", project_id))?;
                    review_folder(Path::new(&project.host_path), path)?;
                }
                if link.uses_it() && !matches!(scope, FindScope::Project | FindScope::SinceLastRun)
                {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "A reviewer that uses the app tries all of it, or what changed since its last run",
                    ));
                }
                if max_issues.is_some_and(|n| !(1..=MAX_FINDINGS_PER_RUN).contains(&n)) {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        format!("A finder run files 1 to {MAX_FINDINGS_PER_RUN} issues"),
                    ));
                }
                // Checked now, or the Find would run and its fixes be refused.
                valid_choice(fix_with, "fix with")?;
            }
            GithubLink::Triage => {}
        }
        if let Some(branch) = &prepared.branch
            && !crate::git::branch_name_ok(&branch.name)
        {
            return Err(error(
                ErrorCode::InvalidRequest,
                format!("`{}` isn't a branch name Git accepts", branch.name),
            ));
        }
        prepared.plan |= prepared.github.from_stranger;
        if prepared.plan
            && let Some(info) = info
        {
            domain::task_mode(info, TaskMode::Plan)?;
        }
        Ok(prepared)
    }

    /// Forgets a removed project's GitHub settings and what was read.
    pub(crate) fn forget_github(
        tx: &mut store::Write<'_>,
        project_id: ProjectId,
    ) -> store::Result<()> {
        tx.delete(SETTINGS, project_id)?;
        tx.delete(TRIAGE, project_id)?;
        tx.delete(crate::github_run::RUN, project_id)?;
        tx.delete(crate::github_watch::FOUND_AT, project_id)?;
        tx.delete(REPO, project_id)
    }
}

/// Review scopes name existing folders inside the project. Check both the
/// portable relative spelling and its destination, so a symlink cannot name
/// another project's private files. The run's sandbox enforces its read grant.
fn review_folder(project: &Path, path: &str) -> Result<(), ProtocolError> {
    use std::path::Component;
    let refused = || {
        error(
            ErrorCode::PathNotAllowed,
            "Choose an existing folder inside the project using a relative path without parent segments or links outside the project",
        )
    };
    let relative = Path::new(path);
    if path.trim().is_empty()
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(refused());
    }
    let root = ports::library::canonical(project).map_err(|_| refused())?;
    let folder = ports::library::canonical(&root.join(relative)).map_err(|_| refused())?;
    if !folder.starts_with(&root) || !folder.is_dir() {
        return Err(refused());
    }
    Ok(())
}

/// Through merge only where the account administers the repository.
pub(crate) fn through_merge_allowed(
    mode: GithubMode,
    repo: &GithubRepo,
) -> Result<(), ProtocolError> {
    if mode == GithubMode::ThroughMerge && !repo.admin {
        return Err(error(
            ErrorCode::PermissionDenied,
            format!(
                "Through merge needs an admin of {}/{}. Use Drafts here.",
                repo.owner, repo.name
            ),
        ));
    }
    if !repo.push {
        return Err(error(
            ErrorCode::PermissionDenied,
            format!(
                "The account signed in under Accounts can't push to {}/{}",
                repo.owner, repo.name
            ),
        ));
    }
    Ok(())
}

/// Ready and Merge act as the person, so they come from the phone with its
/// unlock, not from a chat.
fn unlocked(caller: &Caller) -> Result<(), ProtocolError> {
    if !matches!(
        caller.principal,
        Principal::Device { .. } | Principal::LocalOwner
    ) {
        return Err(error(
            ErrorCode::PermissionDenied,
            "Only the person can mark ready or merge a pull request",
        ));
    }
    Ok(())
}

impl CompanionHub {
    /// Report fixes remain the owner's decision after copying or composing branches.
    pub(crate) async fn refuse_report_automation(
        &self,
        owner: &str,
        name: &str,
        repository: &Value,
    ) -> Result<(), ProtocolError> {
        if self.is_report_fix(owner, name, repository).await? {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Report fixes need the owner's own Merge tap",
            ));
        }
        Ok(())
    }

    pub(crate) async fn is_report_fix(
        &self,
        owner: &str,
        name: &str,
        repository: &Value,
    ) -> Result<bool, ProtocolError> {
        let pr = repository.get("pullRequest").unwrap_or(repository);
        let branch = pr["headRefName"]
            .as_str()
            .or_else(|| pr["head"]["ref"].as_str());
        if branch.is_some_and(|b| b.to_ascii_lowercase().starts_with("report/")) {
            return Ok(true);
        }
        let head = pr["headRefOid"]
            .as_str()
            .or_else(|| pr["head"]["sha"].as_str());
        let number = pr["number"].as_u64();
        let key = repo_key(owner, name);
        let (tracked, known) =
            self.report_fix_identity(&key, number, &head.into_iter().collect::<Vec<_>>())?;
        if known || !tracked {
            return Ok(known);
        }
        let unreadable = || {
            error(
                ErrorCode::PermissionDenied,
                "The pull request's full commit history could not be checked",
            )
        };
        // GraphQL reads already include a bounded inventory. REST callers need
        // at most one additional page; incomplete histories always hold the merge.
        let (commits, complete): (Vec<&str>, bool);
        let rest;
        if let Some(inventory) = pr.get("reportCommits") {
            let nodes = inventory["nodes"].as_array().ok_or_else(unreadable)?;
            commits = nodes
                .iter()
                .map(|n| n["commit"]["oid"].as_str().ok_or_else(unreadable))
                .collect::<Result<_, _>>()?;
            complete = inventory["totalCount"].as_u64() == Some(nodes.len() as u64)
                && nodes.len() <= 100
                && !nodes.is_empty();
        } else {
            let number = number.ok_or_else(unreadable)?;
            rest = self
                .github()?
                .rest(
                    Method::Get,
                    &format!("/repos/{owner}/{name}/pulls/{number}/commits?per_page=100"),
                    None,
                )
                .await
                .map_err(github_error)?;
            let nodes = rest.as_array().ok_or_else(unreadable)?;
            commits = nodes
                .iter()
                .map(|n| n["sha"].as_str().ok_or_else(unreadable))
                .collect::<Result<_, _>>()?;
            complete = !nodes.is_empty()
                && nodes.len() <= 100
                && match pr["commits"].as_u64() {
                    Some(count) => count == nodes.len() as u64,
                    None => nodes.len() < 100,
                };
        }
        if self.report_fix_identity(&key, None, &commits)?.1 {
            return Ok(true);
        }
        if !complete || head.is_none() || !commits.contains(&head.unwrap_or_default()) {
            return Err(unreadable());
        }
        Ok(false)
    }
}

/// Refuses a chat what only the person, at their phone, may do.
fn from_phone(caller: &Caller, why: &str) -> Result<(), ProtocolError> {
    if matches!(caller.principal, Principal::Channel { .. }) {
        return Err(error(ErrorCode::PermissionDenied, why));
    }
    Ok(())
}

const RECENT_CHECKS_QUERY: &str = "query($owner:String!,$name:String!){repository(owner:$owner,name:$name){pullRequests(last:10,states:[OPEN,MERGED]){nodes{commits(last:1){nodes{commit{statusCheckRollup{contexts(first:100){nodes{__typename ... on CheckRun{name} ... on StatusContext{context}}}}}}}}}}}";

/// The checks every recent pull request ran, to require of the next. A
/// check only some ran, like one for some paths, would hold the others.
async fn recent_pr_checks(gh: &dyn Github, repo: &GithubRepo) -> Result<Vec<String>, GithubError> {
    let data = gh
        .graphql(
            RECENT_CHECKS_QUERY,
            json!({ "owner": repo.owner, "name": repo.name }),
        )
        .await?;
    let mut common: Option<BTreeSet<String>> = None;
    for pr in (data["repository"]["pullRequests"]["nodes"]
        .as_array()
        .into_iter())
    .flatten()
    {
        let names: BTreeSet<String> =
            (pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"]
                .as_array()
                .into_iter()
                .flatten())
            .filter_map(|c| c["name"].as_str().or(c["context"].as_str()))
            .map(str::to_owned)
            .collect();
        if names.is_empty() {
            continue;
        }
        common = Some(match common {
            Some(c) => c.intersection(&names).cloned().collect(),
            None => names,
        });
    }
    Ok(common.unwrap_or_default().into_iter().collect())
}

/// The protection that requires a pull request on top of what the branch
/// `had`: its required checks and the `recent` ones, its restrictions and
/// its other rules, each kept as it was. None when it requires one already.
fn protection(had: Option<&Value>, recent: Vec<String>) -> Option<Value> {
    let had = had.unwrap_or(&Value::Null);
    if had.get("required_pull_request_reviews").is_some() {
        return None;
    }
    let required = &had["required_status_checks"];
    let mut checks: Vec<Value> = match required["checks"].as_array() {
        Some(checks) => (checks.iter())
            .map(|c| match c["app_id"].as_i64() {
                Some(app) => json!({ "context": c["context"], "app_id": app }),
                None => json!({ "context": c["context"] }),
            })
            .collect(),
        None => (required["contexts"].as_array().into_iter().flatten())
            .map(|c| json!({ "context": c }))
            .collect(),
    };
    for name in recent {
        if !checks.iter().any(|c| c["context"] == name.as_str()) {
            checks.push(json!({ "context": name }));
        }
    }
    let names = |list: &Value, key: &str| -> Vec<Value> {
        (list.as_array().into_iter().flatten())
            .map(|v| v[key].clone())
            .collect()
    };
    let restrictions = &had["restrictions"];
    let mut body = json!({
        "required_status_checks": match required.is_null() && checks.is_empty() {
            true => Value::Null,
            false => json!({ "strict": required["strict"].as_bool().unwrap_or(false), "checks": checks }),
        },
        "enforce_admins": true,
        "required_pull_request_reviews": { "required_approving_review_count": 0 },
        "restrictions": match restrictions.is_null() {
            true => Value::Null,
            false => json!({
                "users": names(&restrictions["users"], "login"),
                "teams": names(&restrictions["teams"], "slug"),
                "apps": names(&restrictions["apps"], "slug"),
            }),
        },
    });
    for rule in [
        "required_linear_history",
        "allow_force_pushes",
        "allow_deletions",
        "block_creations",
        "required_conversation_resolution",
        "lock_branch",
        "allow_fork_syncing",
    ] {
        if let Some(on) = had[rule]["enabled"].as_bool() {
            body[rule] = json!(on);
        }
    }
    Some(body)
}

/// An agent chosen for later work, to `what` ("fix with"): its model and
/// effort are labels, and its launch flags are ones it has.
fn valid_choice(choice: &LaunchChoice, what: &str) -> Result<(), ProtocolError> {
    for label in [&choice.model, &choice.effort].into_iter().flatten() {
        crate::commands::valid_label(label)?;
    }
    if choice
        .launch_policy
        .is_some_and(|p| !p.supports(choice.harness))
    {
        return Err(error(
            ErrorCode::Unsupported,
            format!("Those launch flags aren't available for the agent chosen to {what}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protection_adds_a_pull_request_to_what_the_branch_had() {
        let none = protection(None, vec![]).unwrap();
        assert_eq!(none["required_status_checks"], Value::Null);
        assert_eq!(none["restrictions"], Value::Null);
        assert_eq!(none.get("required_linear_history"), None);
        // One required already is left alone.
        let had =
            json!({ "required_pull_request_reviews": { "required_approving_review_count": 2 } });
        assert_eq!(protection(Some(&had), vec!["test".into()]), None);
        // The older shape names its checks alone.
        let had = json!({ "required_status_checks": { "strict": false, "contexts": ["ci"] } });
        assert_eq!(
            protection(Some(&had), vec!["ci".into()]).unwrap()["required_status_checks"],
            json!({ "strict": false, "checks": [{ "context": "ci" }] })
        );
    }

    #[test]
    fn a_remote_is_on_github_in_every_form_git_takes() {
        let ours = Some(("octo".to_owned(), "app.js".to_owned()));
        for url in [
            "https://github.com/octo/app.js.git",
            "https://user@GitHub.com/octo/app.js",
            "http://github.com/octo/app.js/",
            "ssh://git@github.com:22/octo/app.js.git",
            "git@github.com:octo/app.js.git",
            "github.com:octo/app.js",
            "git://github.com/octo/app.js",
        ] {
            assert_eq!(github_remote(url), ours, "{url}");
        }
        for url in [
            "https://gitlab.com/octo/app",
            "https://github.example.com/octo/app",
            "https://github.com.evil.example/octo/app",
            "file:///home/you/app",
            "/home/you/app",
            "C:/work/app",
            "https://github.com/octo",
            "https://github.com/octo/app/extra",
            "https://github.com/octo/../app",
            "https://github.com/oc to/app",
        ] {
            assert_eq!(github_remote(url), None, "{url}");
        }
    }

    #[test]
    fn a_filter_names_an_item_by_number_or_by_this_repositorys_link() {
        let n = |f| wanted_number(f, "octo", "app");
        assert_eq!(n("#12").unwrap(), Some(12));
        assert_eq!(n("12").unwrap(), Some(12));
        assert_eq!(
            n("https://github.com/Octo/App/pull/7#discussion_r1").unwrap(),
            Some(7)
        );
        assert_eq!(n("github.com/octo/app/issues/9?x=1").unwrap(), Some(9));
        assert_eq!(n("flaky login").unwrap(), None);
        assert_eq!(
            n("https://github.com/other/app/issues/9").unwrap_err().code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            n("https://github.com/octo/app/wiki").unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn workflows_say_whether_prs_are_checked_and_pushes_deploy() {
        let ci = "on:\n  pull_request:\n  push:\n    branches: [main]\njobs:\n  test:\n    steps: [run: deploy-docs-later]\n".to_owned();
        assert_eq!(workflow_triggers(std::slice::from_ref(&ci)), (true, true));
        let nightly =
            "on:\n  schedule: []\njobs:\n  x:\n    run: echo pull_request push\n".to_owned();
        assert_eq!(workflow_triggers(&[nightly]), (false, false));
        let pages = "on: [push]\njobs:\n  pages:\n    uses: actions/deploy-pages@v4\n".to_owned();
        assert_eq!(workflow_triggers(&[pages]), (false, true));
    }

    #[test]
    fn settings_out_of_bounds_are_refused() {
        assert!(valid_settings(&GithubSettings::default()).is_ok());
        for bad in [
            GithubSettings {
                branch_prefix: "bad prefix/".into(),
                ..Default::default()
            },
            GithubSettings {
                branch_prefix: "x".repeat(MAX_BRANCH_PREFIX + 1),
                ..Default::default()
            },
            GithubSettings {
                workflow: GithubWorkflow {
                    findings_per_run: MAX_FINDINGS_PER_RUN + 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        ] {
            assert!(valid_settings(&bad).is_err(), "{bad:?}");
        }
        let empty = GithubSettings {
            branch_prefix: String::new(),
            ..Default::default()
        };
        assert!(valid_settings(&empty).is_ok());
    }

    #[test]
    fn a_slug_is_short_and_branch_safe() {
        assert_eq!(
            slug("Crash when saving: ÜTF-8 names!"),
            "crash-when-saving-tf-8-names"
        );
        assert!(slug(&"word ".repeat(40)).len() <= 40);
        assert!(crate::git::branch_name_ok(&format!(
            "ophio/1-{}",
            slug("..lock//")
        )));
    }
}
