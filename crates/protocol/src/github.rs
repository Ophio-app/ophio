// SPDX-License-Identifier: Apache-2.0
//! The GitHub workflow: an opinionated way for agents to work through a
//! project's issues and pull requests, on only when the person turns it on
//! for a project whose `origin` is on GitHub. Advertised as `github_v1`;
//! review profiles, file-only reviews and a review's own limit as
//! `github_review_v1`; reviewers that use the app on a screen, as
//! `github_uat_v1`.
//!
//! The agent still runs `git` itself. What it writes to GitHub that people
//! read (pull requests, comments, issues, marking ready, merging) goes
//! through the computer, which says the agent did it.
use crate::capability::HarnessKind;
use crate::ids::{ProjectId, TaskId};
use crate::model::LaunchPolicy;
use crate::project_folders::ProjectFolder;
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

/// The most a project's agent may file in one finder run.
pub const MAX_FINDINGS_PER_RUN: u32 = 20;
/// The longest branch prefix.
pub const MAX_BRANCH_PREFIX: usize = 40;

/// The warning shown before Through merge is kept. `{branch}` is the
/// repository's default branch.
pub const THROUGH_MERGE_WARNING: &str = "The agent will merge its own pull requests into `{branch}` on {repo} when every check passes and no review comment is open, without asking you first. A merge can be undone with a revert, but anything that deploys from `{branch}` deploys it.";
/// The setting row's reminder while Through merge is on.
pub const THROUGH_MERGE_REMINDER: &str =
    "The agent merges its own pull requests when they're green.";

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum GithubMode {
    /// Today's behaviour: tasks aren't linked to GitHub.
    #[default]
    Off,
    /// The agent takes its PRs through to a squash merge once they're green.
    ThroughMerge,
    /// The same up to green; the person marks the PR ready and merges it.
    Drafts,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FixFindings {
    /// Each issue a finder files is queued in Next up to start by itself.
    #[default]
    ByItself,
    /// Each is queued to wait for the person.
    AskFirst,
    /// The issues are filed and nothing is queued: the person starts the
    /// fixes they want from the issue list.
    FileOnly,
}

/// A finder and a fixer: what happens to what the finder files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct GithubWorkflow {
    pub fix_findings: FixFindings,
    /// The finder reviews a fixer's green PR once before it merges.
    pub check_before_merge: bool,
    /// The most issues one finder run files, most serious first.
    pub findings_per_run: u32,
    /// Who Run all has fix each issue, with its launch flags. Run all asks
    /// for one before it starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fix_with: Option<LaunchChoice>,
    /// Who reviews each fix Run all starts before it merges, and sorts new
    /// issues first. Omitted, only Check before merging reviews, and the
    /// fixer sorts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review_with: Option<LaunchChoice>,
}
impl Default for GithubWorkflow {
    fn default() -> Self {
        Self {
            fix_findings: FixFindings::ByItself,
            check_before_merge: true,
            findings_per_run: 5,
            fix_with: None,
            review_with: None,
        }
    }
}

/// One project's GitHub settings, kept on the computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct GithubSettings {
    pub mode: GithubMode,
    /// The person answered the Through merge warning. Nothing merges by
    /// itself before they have.
    #[serde(default)]
    pub warning_answered: bool,
    /// The repository the warning was answered for, as `owner/name` in
    /// lower case: the answer covers that repository alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub warned_repo: Option<String>,
    /// Branches the agent makes start with this.
    pub branch_prefix: String,
    /// The `agent` label goes on what the agent opens, on repositories the
    /// person owns.
    pub label: bool,
    #[serde(default)]
    pub workflow: GithubWorkflow,
}
impl Default for GithubSettings {
    fn default() -> Self {
        Self {
            mode: GithubMode::Off,
            warning_answered: false,
            warned_repo: None,
            branch_prefix: "ophio/".into(),
            label: true,
            workflow: GithubWorkflow::default(),
        }
    }
}

/// What the computer read about a project's repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct GithubRepo {
    pub owner: String,
    pub name: String,
    pub url: String,
    pub default_branch: String,
    pub private: bool,
    /// The signed-in account administers it, so Through merge is offered.
    pub admin: bool,
    /// The signed-in account can push to it.
    pub push: bool,
    /// The repository belongs to the signed-in account. Elsewhere the agent
    /// asks before posting anything.
    pub owned: bool,
    /// Whether the default branch is protected; absent when it couldn't be
    /// read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub protected: Option<bool>,
    /// A workflow runs on pull requests; absent when the workflows
    /// couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runs_checks: Option<bool>,
    /// A workflow runs on pushes to the default branch and deploys.
    pub deploys: bool,
    /// GitHub's private vulnerability reporting is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub private_reporting: Option<bool>,
}

/// Work › GitHub for one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct GithubProjectView {
    pub project_id: ProjectId,
    /// The account signed in under Accounts; absent when none is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub login: Option<String>,
    /// The project's `origin` on GitHub; absent when it isn't there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub repo: Option<GithubRepo>,
    /// Why the repository couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub problem: Option<String>,
    /// When the project's folder isn't a Git repository, the folders
    /// directly in it that are, with their `origin` on GitHub: GitHub
    /// works in a project of one of those.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ProjectFolder>>", optional)]
    pub repo_folders: Vec<ProjectFolder>,
    pub settings: GithubSettings,
    /// What the warning adds for this repository, read when shown.
    pub warnings: Vec<String>,
    /// Open PRs the agent opened in this repository.
    pub open_agent_prs: u32,
    /// The project's latest Run all, while on and once it has ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub run: Option<IssueRun>,
    /// The project's latest Merge together, while on and until the person
    /// puts it away. Advertised as `github_together_v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub together: Option<MergeTogether>,
}

/// Merge together: pull requests the person picked, tested together with
/// the default branch on a merge check of Øphio's, then merged in turn,
/// each as itself. One that breaks the others, or that GitHub won't merge,
/// is left out and the rest carry on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct MergeTogether {
    /// In the order they merge.
    pub prs: Vec<TogetherPr>,
    /// The merge check they are being tested on, while one is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub check: Option<u64>,
    pub started_at: Timestamp,
    /// When it ended: each merged or was left out, or the person stopped
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ended_at: Option<Timestamp>,
    pub stopped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TogetherPr {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub number: u64,
    pub title: String,
    pub state: TogetherState,
    /// Why it was left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub why: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TogetherState {
    Waiting,
    /// On the merge check now.
    Testing,
    Merged,
    LeftOut,
}

/// A pull request the person picked to merge, at the head they saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChosenPr {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub number: u64,
    pub head: String,
    /// What the person was shown as needing their approval for it, empty
    /// when nothing was. Picking it approves only that: a need with
    /// anything else asks them to look again. Older phones leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub approval_shown: Option<Vec<crate::review::SensitiveArea>>,
}

/// Run all: every issue in Agent can take it fixed in turn with the
/// project's defaults, new ones sorted first, until none is left or the
/// person stops it. While on, its fixes start by themselves however long
/// they wait in Next up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct IssueRun {
    pub started_at: Timestamp,
    /// The fixes it started or took on, in order.
    pub fixes: Vec<RunFix>,
    /// The Sort it started, while what it found is still to be taken on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sorting: Option<TaskId>,
    /// Paused: Next up holds, and the fixes it stopped carry on at Resume.
    pub paused: bool,
    /// When it ended: nothing was left, or the person stopped it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ended_at: Option<Timestamp>,
    /// Ended by the person rather than by running out of issues.
    pub stopped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct RunFix {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub number: u64,
    pub task_id: TaskId,
}

/// What the person does to Run all.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunStep {
    /// Fix every issue in Agent can take it, sorting new ones first. On
    /// again, it takes on what has been added since.
    Start,
    /// Hold Next up and stop the fixes under way.
    Pause,
    /// Carry on the fixes Pause stopped, and Next up.
    Resume,
    /// Stop the fixes under way and take the rest out of Next up.
    Stop,
}

/// An agent, model and effort, as Review picks them, and for fixing, the
/// launch flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct LaunchChoice {
    pub harness: HarnessKind,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The launch flags a Find's fixes start with. Omitted, they keep the
    /// agent's own approvals, as before there was a choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub launch_policy: Option<LaunchPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "scope", rename_all = "snake_case")]
#[ts(export)]
pub enum FindScope {
    Project,
    /// A folder, relative to the project.
    Folder {
        path: String,
    },
    /// The commits since the project's last finder run.
    SinceLastRun,
    Pr {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FindFocus {
    Bugs,
    Security,
    Tests,
    Accessibility,
    /// Flows, states and layouts, as a UI/UX developer reviews them.
    UiUx,
    Performance,
    /// Crashes, error handling, data loss, concurrency.
    Reliability,
    /// Code that makes the next bug or change harder.
    Maintainability,
    /// Dependencies, build and CI: the supply chain.
    Dependencies,
    /// Personal data: what is collected, kept, logged and shared.
    Privacy,
    /// APIs, formats and messages against what they promise.
    Api,
    Docs,
    /// Uses the app as most people would, on a screen.
    EverydayUser,
    /// Uses the app as someone who finds technology hard, and is upset by
    /// anything that isn't simple.
    NoviceUser,
    /// Uses the app as an expert who is quick to learn it, and is annoyed by
    /// every extra step.
    PowerUser,
}
impl FindFocus {
    /// The first four were all there were before `github_review_v1`.
    pub fn since_review_v1(self) -> bool {
        !matches!(
            self,
            Self::Bugs | Self::Security | Self::Tests | Self::Accessibility
        )
    }
    /// A reviewer that uses the app, as computer use on a screen, rather
    /// than reading its source. Only a computer advertising `github_uat_v1`
    /// knows them.
    pub fn uses_it(self) -> bool {
        matches!(
            self,
            Self::EverydayUser | Self::NoviceUser | Self::PowerUser
        )
    }
}

/// What GitHub work a task is for. Saved with the task like its branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum GithubLink {
    /// Fix an open issue on a new branch.
    Issue {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
        /// Another agent checks the change before it merges: one codes, the
        /// other reviews. Omitted, only the project's Check before merging.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        review_with: Option<LaunchChoice>,
    },
    /// Take over an open pull request.
    Pr {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
    },
    /// Open a new pull request for the work.
    NewPr,
    /// Open a new pull request stacked on open pull request `number`: the
    /// branch starts from that one's and its pull request targets it.
    Stack {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
    },
    /// A finder run: read-only, it reports what it finds and the computer
    /// files each as an issue, queuing a fix with `fix_with`.
    Find {
        scope: FindScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        focus: Option<FindFocus>,
        fix_with: LaunchChoice,
        /// The most issues this run files; omitted, the project's
        /// `findings_per_run`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        max_issues: Option<u32>,
        /// What happens to what it files; omitted, the project's
        /// `fix_findings`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        fix_findings: Option<FixFindings>,
    },
    /// A triage run: read-only, it sorts the open issues and pull requests.
    Triage,
    /// The check before merging: a read-only look at a fixer's green PR.
    Check {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
    },
}
impl GithubLink {
    /// Runs of these tasks never change the project.
    pub fn read_only(&self) -> bool {
        matches!(self, Self::Find { .. } | Self::Triage | Self::Check { .. })
    }
    /// A finder that uses the app on a screen: a computer-use task, in
    /// Work, that still changes no file.
    pub fn uses_it(&self) -> bool {
        matches!(self, Self::Find { focus: Some(f), .. } if f.uses_it())
    }
    /// Read-only runs that read the source begin in Plan.
    pub fn plan(&self) -> bool {
        self.read_only() && !self.uses_it()
    }
    /// Whether only a computer advertising `github_review_v1` can follow it.
    pub fn needs_review_v1(&self) -> bool {
        match self {
            Self::Find {
                focus,
                max_issues,
                fix_findings,
                ..
            } => {
                focus.is_some_and(FindFocus::since_review_v1)
                    || max_issues.is_some()
                    || fix_findings.is_some()
            }
            _ => false,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// A pull request at a glance, for the task's PR chip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct PrSummary {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub number: u64,
    pub url: String,
    pub state: PrState,
    pub draft: bool,
    /// The commit the checks are for.
    pub head: String,
    pub passed: u32,
    pub failed: u32,
    pub pending: u32,
    /// Review threads nobody has resolved.
    pub unresolved: u32,
    /// GitHub reports a merge conflict.
    pub conflict: bool,
}

/// Where a linked task stands in the watcher.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PrStage {
    /// The agent is working, or will be when something new arrives.
    Babysitting,
    /// Green on the latest commit with nothing unresolved.
    Ready,
    /// The finder is checking it before it merges.
    Checking,
    /// Green and checked in Through merge: it merges, tested with `main`
    /// and the others waiting, once nothing in its project is running or
    /// waiting in Next up.
    Merging,
    Merged,
    /// Five fix rounds, or six hours without progress: it needs the person.
    Capped,
    /// The fixer couldn't reproduce the finding.
    NotReproduced,
    /// Closed without merging.
    Closed,
}

/// A task's GitHub link, saved with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TaskGithub {
    pub link: GithubLink,
    /// The issue the work is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub issue: Option<u64>,
    /// The pull request the work is in, once opened or taken over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pr: Option<PrSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stage: Option<PrStage>,
    /// A fix queued from a finder run: who found it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub found_by: Option<LaunchChoice>,
    /// Who checked it before it merged, once that ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checked_by: Option<LaunchChoice>,
    /// The agent the person chose to check it before it merges, whatever
    /// the project's Check before merging says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub check_with: Option<LaunchChoice>,
    /// Written by someone without write access to the repository: data, not
    /// instructions, and the task began in Plan.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub from_stranger: bool,
    /// Stacked on another PR: the branch this one's PR targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base: Option<String>,
    /// Fix rounds the watcher has started.
    #[serde(default)]
    pub rounds: u32,
    /// The repository it was linked on, as `owner/name` in lower case. If
    /// the project's `origin` moves elsewhere, the link no longer acts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub repo: Option<String>,
    /// What the fixer found when it couldn't reproduce the issue, in its
    /// own words: what it tried, or where the code fixed it already.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub finding: Option<String>,
    /// The fixer found the issue already fixed in the code.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub already_fixed: bool,
    /// Where the pull request's review stands, round by round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review: Option<crate::review::PrReview>,
    /// The change touches something sensitive: it merges only once the
    /// person approves this exact revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub approval: Option<crate::review::ApprovalNeed>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum GithubItemKind {
    Issues,
    Prs,
}

/// The triage run's view of an item.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TriageGroup {
    /// Clear enough for an agent to finish.
    AgentCanTake,
    /// A human review, a decision, or a PR failing or capped.
    NeedsYou,
}

/// Where an item stands with this computer's agents.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ItemStatus {
    AgentWorking,
    WaitingForYou,
    Green,
    /// Its task waits in Next up.
    Queued,
    /// Its task was stopped part way and can carry on.
    Stopped,
}

/// One open issue or pull request in the project's GitHub list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct GithubItem {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub number: u64,
    pub pr: bool,
    pub title: String,
    pub url: String,
    pub author: String,
    /// The author can write to the repository. Text from anyone else is data.
    pub author_writes: bool,
    /// Opened by the signed-in account.
    pub mine: bool,
    pub draft: bool,
    pub labels: Vec<String>,
    pub updated_at: Timestamp,
    /// The task working on it, newest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<ItemStatus>,
    /// What the latest triage run said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub group: Option<TriageGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub triage_note: Option<String>,
    /// For one that needs the person, the answer the Sort would give its own
    /// question: the person may start theirs from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub triage_pick: Option<String>,
    /// The person put it in its group, not a Sort.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub grouped_by_you: bool,
    /// A pull request's checks taken together, when it has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checks: Option<CheckState>,
    /// Whether an open pull request can merge as it is, once GitHub has
    /// worked it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merge: Option<MergeState>,
    /// A pull request's head commit, as Merge together names it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub head: Option<String>,
}

/// Whether a pull request can merge, as GitHub's merge box says.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MergeState {
    Ready,
    /// A required check or review is missing.
    Blocked,
    /// GitHub wants it brought up to date with its base first.
    Behind,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct GithubItems {
    pub items: Vec<GithubItem>,
    /// More matched than are listed.
    pub truncated: bool,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CheckState {
    Pending,
    Passed,
    Failed,
    /// Skipped or neutral: neither green nor red. A cancelled or stale
    /// check failed.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct PrCheck {
    pub name: String,
    pub state: CheckState,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct PrThread {
    pub id: String,
    pub resolved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub line: Option<u64>,
    pub author: String,
    /// The thread's first comment, cut to fit.
    pub body: String,
    pub url: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

/// One pull request in full, for its sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct GithubPrView {
    /// The repository it was read from, as `owner/name`. Mark ready and
    /// Merge name it back, and act there or not at all.
    pub repo: String,
    pub summary: PrSummary,
    pub title: String,
    /// The description's Markdown, cut to fit.
    pub body: String,
    pub author: String,
    pub head_branch: String,
    pub base_branch: String,
    pub checks: Vec<PrCheck>,
    pub threads: Vec<PrThread>,
    /// GitHub's own merge rules pass.
    pub mergeable: bool,
    /// Why it can't merge, in GitHub's words made plain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub blocked: Option<String>,
    /// How Merge merges it: squash where the repository allows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub method: Option<MergeMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task_id: Option<TaskId>,
    /// "Found by … · Fixed by … · Checked by …", each a model name.
    pub roles: Vec<String>,
}

/// The person's GitHub actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum GithubAction {
    /// Change a project's settings. Turning Through merge on leaves the
    /// warning unanswered.
    SetSettings {
        project_id: ProjectId,
        settings: GithubSettings,
    },
    /// The warning's answer: Keep Through merge, or Use Drafts.
    AnswerWarning {
        project_id: ProjectId,
        /// The repository the warning was shown for, as `owner/name`. Keep
        /// Through merge is refused when `origin` names another one now.
        repo: String,
        keep_through_merge: bool,
    },
    /// Protect the default branch: changes go through a PR whose checks
    /// pass.
    ProtectBranch { project_id: ProjectId },
    /// Refused when `origin` no longer names `repo`, the repository the
    /// pull request was read from, as `owner/name`.
    MarkPrReady {
        project_id: ProjectId,
        repo: String,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
    },
    /// File an issue the person wrote. Refused when `origin` no longer
    /// names `repo`, the repository the form was shown for, as `owner/name`.
    FileIssue {
        project_id: ProjectId,
        repo: String,
        title: String,
        body: String,
    },
    /// Refused when `origin` no longer names `repo`, or the PR's head is no
    /// longer `expected_head`.
    MergePr {
        project_id: ProjectId,
        repo: String,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
        expected_head: String,
        method: MergeMethod,
        /// What the person was shown as needing their approval, empty when
        /// nothing was. Merging approves only that: a need with anything
        /// else asks them to look again. Older phones leave it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        approval_shown: Option<Vec<crate::review::SensitiveArea>>,
    },
    /// Start, pause, resume or stop Run all.
    RunIssues {
        project_id: ProjectId,
        step: RunStep,
    },
    /// Close an issue in the person's name: done, or not going to be done.
    /// Its fix leaves Next up and Run all, and what it asked of the person
    /// is answered. Refused when `origin` no longer names `repo`, the
    /// repository the issue was read from, as `owner/name`.
    CloseIssue {
        project_id: ProjectId,
        repo: String,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
        /// Closed as done, not as not planned.
        done: bool,
    },
    /// Move an issue or pull request to a group by hand; none, back to Not
    /// sorted. Moving an issue to Needs you takes a fix that hasn't started
    /// out of Next up: it waits for the person.
    SetGroup {
        project_id: ProjectId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
        pr: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        group: Option<TriageGroup>,
    },
    /// Test pull requests together and merge each in turn, at the heads the
    /// person saw. Refused when `origin` no longer names `repo`, while one
    /// is on, or when any isn't open at its head into the default branch.
    MergeTogether {
        project_id: ProjectId,
        repo: String,
        prs: Vec<ChosenPr>,
    },
    /// Stop Merge together: what hasn't merged is left out. Once it has
    /// ended, put it away.
    StopTogether { project_id: ProjectId },
}
