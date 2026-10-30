// SPDX-License-Identifier: Apache-2.0
//! Merge together: pull requests the person picked on their phone, tested
//! together with the default branch and then merged in turn, each as
//! itself, so each keeps its own place in GitHub's history. The person's
//! unlock when they start it is the go-ahead for every merge.
//!
//! As the line of Through merge does, Øphio merges each into a branch made
//! from the default branch's head and opens a draft pull request of it, the
//! merge check, for the repository's checks to run on. Green, each merges
//! while the default branch is still what was tested; red, they are halved
//! and each half tested alone, until what breaks is found and left out with
//! what failed. One that conflicts, has a new commit since it was picked, or
//! that GitHub won't merge is left out too, and the rest carry on. While it
//! is on, the line of Through merge waits.

use crate::github::{REPO, github_error, project_tasks, repo_key, span};
use crate::github_tools::{listed, uncertain};
use crate::github_train::{
    MAX_REFUSED, MERGE_BRANCH, STALL_MS, Verdict, behind_by, head_of, verdict,
};
use crate::github_watch::{headline_item, put_item, speaking};
use crate::{CompanionHub, error};
use ports::github::{Github, GithubError, Method};
use protocol::github::*;
use protocol::{
    ErrorCode, Event, ProjectId, ProtocolError, model::AttentionKind, notifications::AlertTopic,
    review::ApprovalNeed, time::Timestamp,
};
use serde_json::json;

/// Each project's latest Merge together.
pub(crate) const TOGETHER: &str = "github_together";
/// The most pull requests merged together at once.
pub(crate) const MAX_TOGETHER: usize = 20;
/// The most steps one pass takes before it waits for the next.
const MAX_STEPS: usize = 2 * MAX_TOGETHER + 2;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Together {
    pub view: MergeTogether,
    /// The repository it merges in, as named when it started.
    pub owner: String,
    pub name: String,
    pub default_branch: String,
    pub method: MergeMethod,
    /// The heads the person picked.
    pub heads: Vec<ChosenPr>,
    /// What is still to be tested, a group at a time, in turn.
    #[serde(default)]
    pub groups: Vec<Vec<u64>>,
    /// The group being tested or merged now.
    #[serde(default)]
    pub batch: Option<Batch>,
    /// Kept before waiting for the active pass, so Stop also survives a restart.
    #[serde(default)]
    pub stop_requested: bool,
    /// A merge authorized for dispatch whose outcome has not been saved yet.
    #[serde(default)]
    pub dispatched: Option<u64>,
    /// What each picked pull request that needed the person's approval
    /// needed when they picked it, which picking it approved.
    #[serde(default)]
    pub approved: Vec<(u64, ApprovalNeed)>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Batch {
    pub prs: Vec<u64>,
    /// The default branch's head it was tested on, and then each merge's.
    pub base: String,
    /// The merge check's branch: None when it merges without one.
    #[serde(default)]
    pub branch: Option<String>,
    /// The merge check's pull request, once opened.
    #[serde(default)]
    pub number: Option<u64>,
    pub at: Timestamp,
    /// How many of `prs` have been merged or left out, in order.
    #[serde(default)]
    pub merged: usize,
    /// Tested green, or nothing to test: it merges.
    #[serde(default)]
    pub green: bool,
    /// Times in a row GitHub refused the next one.
    #[serde(default)]
    pub refused: u32,
}

/// Whether to take the next step now, or wait for the next pass.
enum Step {
    Again,
    Wait,
}

impl Together {
    fn running(&self) -> bool {
        self.view.ended_at.is_none()
    }

    fn path(&self) -> String {
        format!("/repos/{}/{}", self.owner, self.name)
    }

    fn head(&self, number: u64) -> String {
        (self.heads.iter())
            .find(|c| c.number == number)
            .map(|c| c.head.clone())
            .unwrap_or_default()
    }

    fn state(&self, number: u64) -> Option<TogetherState> {
        (self.view.prs.iter())
            .find(|p| p.number == number)
            .map(|p| p.state)
    }

    fn set(&mut self, number: u64, state: TogetherState, why: Option<String>) {
        if let Some(pr) = self.view.prs.iter_mut().find(|p| p.number == number) {
            pr.state = state;
            pr.why = why;
        }
    }
}

/// The project's latest Merge together, as the phone shows it.
pub(crate) fn together_view(
    r: &store::Read<'_>,
    project_id: ProjectId,
) -> store::Result<Option<MergeTogether>> {
    Ok(r.get::<Together>(TOGETHER, project_id)?.map(|t| t.view))
}

/// Whether Merge together is on in the project.
pub(crate) fn together_on(r: &store::Read<'_>, project_id: ProjectId) -> store::Result<bool> {
    Ok(r.get::<Together>(TOGETHER, project_id)?
        .is_some_and(|t| t.running()))
}

/// "#3, #5 and #8".
fn numbers(prs: &[u64]) -> String {
    let names: Vec<String> = prs.iter().map(|n| format!("#{n}")).collect();
    listed(&names)
}

fn method_name(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Squash => "squash",
        MergeMethod::Merge => "merge",
        MergeMethod::Rebase => "rebase",
    }
}

/// What became of a picked pull request that can't be merged as picked:
/// merged already, or left out and why.
fn gone(view: &GithubPrView, head: &str, default: &str) -> Option<(TogetherState, Option<String>)> {
    let why = match view.summary.state {
        PrState::Merged => return Some((TogetherState::Merged, None)),
        PrState::Closed => "It was closed.".to_owned(),
        PrState::Open if view.summary.head != head => {
            "It has a new commit since you picked it, so it wasn't merged. Pick it again to merge it."
                .to_owned()
        }
        PrState::Open if view.base_branch != default => format!(
            "It merges into `{}` now, not `{default}`.",
            span(&view.base_branch, 200)
        ),
        PrState::Open => return None,
    };
    Some((TogetherState::LeftOut, Some(why)))
}

impl CompanionHub {
    /// Starts Merge together on what the person picked at `owner/name`,
    /// and takes its first steps.
    pub(crate) async fn merge_together(
        &self,
        project_id: ProjectId,
        (owner, name): (&str, &str),
        chosen: &[ChosenPr],
    ) -> Result<(), ProtocolError> {
        if chosen.is_empty() || chosen.len() > MAX_TOGETHER {
            return Err(error(
                ErrorCode::InvalidRequest,
                format!("Pick from 1 to {MAX_TOGETHER} pull requests to merge together"),
            ));
        }
        let mut heads = chosen.to_vec();
        heads.sort_by_key(|c| c.number);
        heads.dedup_by_key(|c| c.number);
        if heads.len() != chosen.len() {
            return Err(error(
                ErrorCode::InvalidRequest,
                "A pull request was picked twice",
            ));
        }
        let _held = self.inner.together.lock().await;
        if self.read(|r| together_on(r, project_id))? {
            return Err(error(
                ErrorCode::Conflict,
                "Merge together is already on in this project. Stop it, or wait for it to finish.",
            ));
        }
        let repo = (self.github_repo_or_kept(project_id, owner, name)).await?;
        let default = repo.default_branch.clone();
        let mut prs = Vec::new();
        let mut method = None;
        let mut approved = Vec::new();
        for c in &heads {
            let number = c.number;
            let (repository, view) = self.read_pr_in(owner, name, number).await?;
            self.refuse_report_automation(owner, name, &repository)
                .await?;
            if view.summary.state != PrState::Open {
                return Err(error(
                    ErrorCode::Conflict,
                    format!("Pull request #{number} isn't open"),
                ));
            }
            if view.summary.head != c.head {
                let short: String = view.summary.head.chars().take(7).collect();
                return Err(error(
                    ErrorCode::Conflict,
                    format!(
                        "Pull request #{number} has a new commit, {short}, since you looked. Check it again before merging."
                    ),
                ));
            }
            if repository["pullRequest"]["isCrossRepository"] == true {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    format!(
                        "Pull request #{number}'s branch is in a fork, so it can't be tested with the others. Merge it on its own."
                    ),
                ));
            }
            if view.base_branch != default {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    format!(
                        "Pull request #{number} merges into `{}`, not `{default}`. Merge it on its own.",
                        span(&view.base_branch, 200)
                    ),
                ));
            }
            // Picked at its commit, a sensitive change is approved by the
            // person here as by its own Merge: once they could see why.
            let weighed = (self.approval_for(
                project_id,
                (owner, name),
                number,
                &c.head,
                &view.base_branch,
            ))
            .await?;
            let unseen = |n: &&_| crate::review_gate::unshown(n, c.approval_shown.as_deref());
            if let Some(need) = (weighed.need.as_ref()).filter(|n| weighed.fresh || unseen(n)) {
                return Err(crate::review_gate::look_again(number, need));
            }
            approved.extend(weighed.need.map(|need| (number, need)));
            method = method.or(view.method);
            prs.push(TogetherPr {
                number,
                title: view.title,
                state: TogetherState::Waiting,
                why: None,
            });
        }
        let Some(method) = method else {
            return Err(error(
                ErrorCode::InvalidRequest,
                "This repository doesn't allow merging pull requests",
            ));
        };
        let together = Together {
            view: MergeTogether {
                prs,
                check: None,
                started_at: self.inner.clock.now(),
                ended_at: None,
                stopped: false,
            },
            owner: owner.to_owned(),
            name: name.to_owned(),
            default_branch: default,
            method,
            groups: vec![heads.iter().map(|c| c.number).collect()],
            heads,
            batch: None,
            stop_requested: false,
            dispatched: None,
            approved,
        };
        self.save_together(project_id, &together)?;
        if let Err(e) = self.together_steps(project_id).await {
            tracing::warn!(code = ?e.code, "starting Merge together failed");
        }
        Ok(())
    }

    /// Stops Merge together, or once it has ended, puts it away.
    pub(crate) async fn stop_together(&self, project_id: ProjectId) -> Result<(), ProtocolError> {
        // Record withdrawal before waiting for the pass doing GitHub I/O.
        // Done only clears an operation that had already ended when asked.
        let stopping = self.write(|tx| {
            let Some(mut t) = tx.read().get::<Together>(TOGETHER, project_id)? else {
                return Ok(false);
            };
            let stopping = t.running();
            if stopping {
                t.stop_requested = true;
                tx.put(TOGETHER, project_id, &t)?;
            } else {
                tx.delete(TOGETHER, project_id)?;
            }
            tx.emit(Event::GithubChanged { project_id })?;
            Ok(stopping)
        })?;
        if !stopping {
            return Ok(());
        }
        let _held = self.inner.together.lock().await;
        let Some(mut t) = self
            .read(|r| r.get::<Together>(TOGETHER, project_id))?
            .filter(|t| t.running() && t.stop_requested)
        else {
            // The active pass already stopped. Keep its result on the phone.
            return Ok(());
        };
        let gh = self.github()?;
        self.finish_together_stop(&*gh, project_id, &mut t).await
    }

    /// Moves a project's Merge together on, as far as it can go now.
    pub(crate) async fn advance_together(
        &self,
        project_id: ProjectId,
    ) -> Result<(), ProtocolError> {
        if !self.read(|r| together_on(r, project_id))? {
            return Ok(());
        }
        let _held = self.inner.together.lock().await;
        self.together_steps(project_id).await
    }

    /// Takes each step that can be taken now. Held under `together`.
    async fn together_steps(&self, project_id: ProjectId) -> Result<(), ProtocolError> {
        let gh = self.github()?;
        let (owner, name) = self.on_github(project_id).await?;
        for _ in 0..MAX_STEPS {
            let Some(mut t) =
                (self.read(|r| r.get::<Together>(TOGETHER, project_id))?).filter(Together::running)
            else {
                return Ok(());
            };
            if self.together_cancelled(&*gh, project_id, &mut t).await? {
                return Ok(());
            }
            if repo_key(&owner, &name) != repo_key(&t.owner, &t.name) {
                let why = "This project's `origin` names another repository now.";
                return self
                    .end_together(&*gh, project_id, &mut t, why, false)
                    .await;
            }
            let step = match t.batch.as_ref().map(|b| b.green) {
                Some(false) => self.together_decide(&*gh, project_id, &mut t).await?,
                Some(true) => self.together_merge(&*gh, project_id, &mut t).await?,
                None if t.groups.is_empty() => {
                    t.view.ended_at = Some(self.inner.clock.now());
                    t.view.check = None;
                    self.save_together(project_id, &t)?;
                    self.tell_together(project_id, &t)?;
                    Step::Wait
                }
                None => {
                    let group = t.groups.remove(0);
                    self.together_depart(&*gh, project_id, &mut t, group)
                        .await?
                }
            };
            if let Step::Wait = step {
                return Ok(());
            }
        }
        Ok(())
    }

    fn save_together(&self, project_id: ProjectId, t: &Together) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let mut t = t.clone();
            // A pass can hold a snapshot from before Stop was requested.
            // Preserve that request, but don't carry it into a new operation.
            if let Some(kept) = tx.read().get::<Together>(TOGETHER, project_id)?
                && kept.running()
            {
                t.stop_requested |= kept.stop_requested;
            }
            tx.put(TOGETHER, project_id, &t)?;
            tx.emit(Event::GithubChanged { project_id })?;
            Ok(())
        })
    }

    /// A checkpoint between steps and before dispatching another GitHub write.
    /// Never cancel an in-flight merge future: its result still belongs here.
    async fn together_cancelled(
        &self,
        gh: &dyn Github,
        project_id: ProjectId,
        t: &mut Together,
    ) -> Result<bool, ProtocolError> {
        let requested = self.read(|r| {
            Ok(r.get::<Together>(TOGETHER, project_id)?
                .is_some_and(|t| t.stop_requested))
        })?;
        if requested {
            t.stop_requested = true;
            self.finish_together_stop(gh, project_id, t).await?;
        }
        Ok(requested)
    }

    /// A lost merge response must be reconciled before Stop becomes final.
    /// An open PR or a failed read does not prove the dispatched merge failed.
    async fn finish_together_stop(
        &self,
        gh: &dyn Github,
        project_id: ProjectId,
        t: &mut Together,
    ) -> Result<(), ProtocolError> {
        if let Some(number) = t.dispatched {
            t.view.stopped = true;
            for pr in &mut t.view.prs {
                if pr.number != number
                    && matches!(pr.state, TogetherState::Waiting | TogetherState::Testing)
                {
                    pr.state = TogetherState::LeftOut;
                    pr.why = Some("You stopped it.".into());
                }
            }
            t.set(
                number,
                TogetherState::Testing,
                Some("Checking the merge already sent to GitHub.".into()),
            );
            self.save_together(project_id, t)?;
            let (_, view) = self.read_pr_in(&t.owner, &t.name, number).await?;
            match view.summary.state {
                PrState::Open => return Ok(()),
                PrState::Merged => {
                    self.record_pr(project_id, (&t.owner, &t.name), number, |g| {
                        if let Some(pr) = g.pr.as_mut() {
                            pr.state = PrState::Merged;
                        }
                        g.stage = Some(PrStage::Merged);
                    })?;
                    t.set(number, TogetherState::Merged, None);
                }
                PrState::Closed => {
                    t.set(
                        number,
                        TogetherState::LeftOut,
                        Some("It was closed.".into()),
                    );
                }
            }
            t.dispatched = None;
        }
        self.end_together(gh, project_id, t, "You stopped it.", true)
            .await
    }

    /// Starts testing `group` together, or merging it where there is
    /// nothing to test: one already up to date with the default branch whose
    /// own checks passed, or a repository that runs no checks.
    async fn together_depart(
        &self,
        gh: &dyn Github,
        project_id: ProjectId,
        t: &mut Together,
        group: Vec<u64>,
    ) -> Result<Step, ProtocolError> {
        let path = t.path();
        let mut kept = Vec::new();
        for number in group {
            if self.together_cancelled(gh, project_id, t).await? {
                return Ok(Step::Wait);
            }
            let (repository, view) = self.read_pr_in(&t.owner, &t.name, number).await?;
            self.refuse_report_automation(&t.owner, &t.name, &repository)
                .await?;
            match gone(&view, &t.head(number), &t.default_branch) {
                Some((state, why)) => t.set(number, state, why),
                None => kept.push((number, view)),
            }
        }
        if kept.is_empty() {
            self.save_together(project_id, t)?;
            return Ok(Step::Again);
        }
        let base = head_of(gh, &path, &t.default_branch).await?;
        let now = self.inner.clock.now();
        let runs_checks = (self.read(|r| r.get::<GithubRepo>(REPO, project_id))?)
            .filter(|repo| repo_key(&repo.owner, &repo.name) == repo_key(&t.owner, &t.name))
            .and_then(|repo| repo.runs_checks);
        let untested = match kept.as_slice() {
            _ if runs_checks == Some(false) => true,
            [(number, view)] => {
                matches!(verdict(view), Verdict::Green)
                    && behind_by(gh, &path, &t.default_branch, &t.head(*number)).await? == Some(0)
            }
            _ => false,
        };
        let mut batch = Batch {
            prs: kept.iter().map(|(n, _)| *n).collect(),
            base,
            branch: None,
            number: None,
            at: now,
            merged: 0,
            green: untested,
            refused: 0,
        };
        if untested {
            t.batch = Some(batch);
            self.save_together(project_id, t)?;
            return Ok(Step::Again);
        }
        // Kept before the branch is made, so a restart between cleans it up.
        if self.together_cancelled(gh, project_id, t).await? {
            return Ok(Step::Wait);
        }
        let branch = format!("{MERGE_BRANCH}together-{}", now.0);
        batch.branch = Some(branch.clone());
        t.batch = Some(batch.clone());
        self.save_together(project_id, t)?;
        let made = json!({ "ref": format!("refs/heads/{branch}"), "sha": batch.base });
        if let Err(e) = (gh.rest(Method::Post, &format!("{path}/git/refs"), Some(made))).await {
            return self.together_failed(gh, project_id, t, e).await;
        }
        let mut tested: Vec<u64> = Vec::new();
        for number in batch.prs.clone() {
            if self.together_cancelled(gh, project_id, t).await? {
                return Ok(Step::Wait);
            }
            let merging = json!({
                "base": branch,
                "head": t.head(number),
                "commit_message": format!("Øphio merge check: #{number}"),
            });
            match (gh.rest(Method::Post, &format!("{path}/merges"), Some(merging))).await {
                Ok(_) => tested.push(number),
                Err(GithubError::Status { code: 409, .. }) => {
                    let default = &t.default_branch;
                    let why = match tested.as_slice() {
                        [] => format!(
                            "It conflicts with `{default}`. Merge `{default}` into its branch, then pick it again."
                        ),
                        before => format!(
                            "It conflicts with `{default}` and {}, which merge before it. Once they have, merge `{default}` into its branch and pick it again.",
                            numbers(before)
                        ),
                    };
                    t.set(number, TogetherState::LeftOut, Some(why));
                }
                Err(e) => return self.together_failed(gh, project_id, t, e).await,
            }
        }
        if tested.is_empty() {
            self.close_check(gh, project_id, t).await?;
            return Ok(Step::Again);
        }
        let default = t.default_branch.clone();
        let opening = |draft: bool| {
            json!({
                "title": format!("Øphio merge check: {}", numbers(&tested)),
                "head": branch,
                "base": default,
                "body": format!(
                    "Øphio tests {} together with `{}` here before merging each of them, as you asked. It closes this itself; don't merge it.",
                    numbers(&tested),
                    default
                ),
                "draft": draft,
            })
        };
        let pulls = format!("{path}/pulls");
        if self.together_cancelled(gh, project_id, t).await? {
            return Ok(Step::Wait);
        }
        let opened = match gh.rest(Method::Post, &pulls, Some(opening(true))).await {
            // A repository that has no drafts.
            Err(GithubError::Status { code: 422, .. }) => {
                if self.together_cancelled(gh, project_id, t).await? {
                    return Ok(Step::Wait);
                }
                gh.rest(Method::Post, &pulls, Some(opening(false))).await
            }
            other => other,
        };
        let number = match opened {
            Ok(answer) => answer["number"].as_u64(),
            Err(e) => return self.together_failed(gh, project_id, t, e).await,
        };
        for &n in &tested {
            t.set(n, TogetherState::Testing, None);
        }
        t.view.check = number;
        if let Some(batch) = t.batch.as_mut() {
            batch.prs = tested;
            batch.number = number;
            batch.at = self.inner.clock.now();
        }
        self.save_together(project_id, t)?;
        Ok(Step::Wait)
    }

    /// Reads the merge check: green merges, red is halved or left out, and
    /// one that hasn't finished in six hours is left out.
    async fn together_decide(
        &self,
        gh: &dyn Github,
        project_id: ProjectId,
        t: &mut Together,
    ) -> Result<Step, ProtocolError> {
        let Some(batch) = t.batch.clone() else {
            return Ok(Step::Again);
        };
        // A restart before it was opened.
        let Some(number) = batch.number else {
            self.retry_together(gh, project_id, t).await?;
            return Ok(Step::Again);
        };
        let (_, view) = self.read_pr_in(&t.owner, &t.name, number).await?;
        if view.summary.state != PrState::Open {
            let why = format!("Øphio's merge check #{number} was closed on GitHub, so it stopped.");
            self.end_together(gh, project_id, t, &why, false).await?;
            return Ok(Step::Wait);
        }
        match verdict(&view) {
            Verdict::Waiting => {
                if self.inner.clock.now().0 - batch.at.0 < STALL_MS {
                    return Ok(Step::Wait);
                }
                let why = format!(
                    "The checks on Øphio's merge check #{number} didn't finish in six hours."
                );
                for &n in &batch.prs {
                    t.set(n, TogetherState::LeftOut, Some(why.clone()));
                }
                self.close_check(gh, project_id, t).await?;
            }
            Verdict::Red(failed) => {
                if let [one] = batch.prs.as_slice() {
                    let why = format!(
                        "Tested with `{}` on Øphio's merge check #{number}, this failed: {}.",
                        t.default_branch,
                        listed(&failed)
                    );
                    t.set(*one, TogetherState::LeftOut, Some(why));
                } else {
                    let (first, second) = batch.prs.split_at(batch.prs.len() / 2);
                    t.groups.insert(0, second.to_vec());
                    t.groups.insert(0, first.to_vec());
                    for &n in &batch.prs {
                        t.set(n, TogetherState::Waiting, None);
                    }
                }
                self.close_check(gh, project_id, t).await?;
            }
            Verdict::Green => {
                if let Some(batch) = t.batch.as_mut() {
                    batch.green = true;
                }
                self.save_together(project_id, t)?;
            }
        }
        Ok(Step::Again)
    }

    /// Whether pull request `number`, picked at `head`, now needs the
    /// person's approval for more than picking it approved: then it is
    /// left out, with why, for them to merge on its own.
    async fn newly_sensitive(
        &self,
        project_id: ProjectId,
        t: &Together,
        number: u64,
        head: &str,
    ) -> Result<Option<(TogetherState, Option<String>)>, ProtocolError> {
        let weighed = (self.approval_for(
            project_id,
            (&t.owner, &t.name),
            number,
            head,
            &t.default_branch,
        ))
        .await?;
        let Some(need) = weighed.need else {
            return Ok(None);
        };
        let picked = (t.approved.iter()).find(|(n, _)| *n == number);
        let covered = picked.is_some_and(|(_, was)| {
            was.head == need.head && need.areas.iter().all(|a| was.areas.contains(a))
        });
        if covered {
            return Ok(None);
        }
        let why = format!(
            "Since you picked it, it needs your approval. {} Merge it on its own.",
            crate::review_gate::why(&need)
        );
        Ok(Some((TogetherState::LeftOut, Some(why))))
    }

    /// Merges what tested green, in order, while the default branch is what
    /// it was tested on. One GitHub refuses is tried again next time, and
    /// after five times in a row is left out, and the rest tested again.
    async fn together_merge(
        &self,
        gh: &dyn Github,
        project_id: ProjectId,
        t: &mut Together,
    ) -> Result<Step, ProtocolError> {
        let Some(mut batch) = t.batch.clone() else {
            return Ok(Step::Again);
        };
        let path = t.path();
        while let Some(number) = batch.prs.get(batch.merged).copied() {
            if self.together_cancelled(gh, project_id, t).await? {
                return Ok(Step::Wait);
            }
            let head = t.head(number);
            let (repository, view) = self.read_pr_in(&t.owner, &t.name, number).await?;
            self.refuse_report_automation(&t.owner, &t.name, &repository)
                .await?;
            let mut left = gone(&view, &head, &t.default_branch);
            if left.is_none() {
                left = self.newly_sensitive(project_id, t, number, &head).await?;
            }
            if let Some((state, why)) = left {
                if t.dispatched == Some(number) {
                    t.dispatched = None;
                }
                t.set(number, state, why);
                batch.merged += 1;
                t.batch = Some(batch.clone());
                // Even if it merged elsewhere, we don't know its merge SHA.
                // The remaining requests need a fresh base and merge check.
                self.retry_together(gh, project_id, t).await?;
                return Ok(Step::Again);
            }
            if self.together_cancelled(gh, project_id, t).await? {
                return Ok(Step::Wait);
            }
            let mut refused = None;
            if view.summary.draft
                && let Err(e) = gh
                    .graphql(
                        "mutation($id:ID!){markPullRequestReadyForReview(input:{pullRequestId:$id}){pullRequest{isDraft}}}",
                        json!({ "id": repository["pullRequest"]["id"] }),
                    )
                    .await
            {
                refused = Some(github_error(e).message);
            }
            let mut made = None;
            if refused.is_none() {
                if self.together_cancelled(gh, project_id, t).await? {
                    return Ok(Step::Wait);
                }
                // Check every time, after reading the PR and marking it ready.
                // Only our own confirmed merge SHA advances the tested base.
                let base = head_of(gh, &path, &t.default_branch).await?;
                if self.together_cancelled(gh, project_id, t).await? {
                    return Ok(Step::Wait);
                }
                if base != batch.base {
                    self.retry_together(gh, project_id, t).await?;
                    return Ok(Step::Again);
                }
                let merging = json!({ "sha": head, "merge_method": method_name(t.method) });
                let pull = format!("{path}/pulls/{number}/merge");
                let previous = t.dispatched.replace(number);
                let dispatch = self.write(|tx| {
                    if tx
                        .read()
                        .get::<Together>(TOGETHER, project_id)?
                        .is_some_and(|kept| kept.stop_requested)
                    {
                        return Ok(false);
                    }
                    tx.put(TOGETHER, project_id, t)?;
                    Ok(true)
                })?;
                if !dispatch {
                    t.dispatched = previous;
                    self.together_cancelled(gh, project_id, t).await?;
                    return Ok(Step::Wait);
                }
                match gh.rest(Method::Put, &pull, Some(merging)).await {
                    Ok(answer) => made = Some(answer["sha"].as_str().map(str::to_owned)),
                    Err(e) if uncertain(&e) => {
                        let (_, now) = self.read_pr_in(&t.owner, &t.name, number).await?;
                        if now.summary.state != PrState::Open {
                            t.dispatched = None;
                        }
                        match now.summary.state == PrState::Merged {
                            true => made = Some(None),
                            false => refused = Some(github_error(e).message),
                        }
                    }
                    Err(e) => {
                        t.dispatched = None;
                        refused = Some(github_error(e).message);
                    }
                }
            }
            let Some(made) = made else {
                if self.together_cancelled(gh, project_id, t).await? {
                    return Ok(Step::Wait);
                }
                batch.refused += 1;
                if batch.refused < MAX_REFUSED {
                    t.batch = Some(batch);
                    self.save_together(project_id, t)?;
                    return Ok(Step::Wait);
                }
                let why = match (&view.blocked, refused) {
                    (Some(blocked), _) => format!("GitHub wouldn't merge it. {blocked}"),
                    (None, Some(refused)) => refused,
                    (None, None) => "GitHub wouldn't merge it.".to_owned(),
                };
                t.set(number, TogetherState::LeftOut, Some(why));
                batch.merged += 1;
                t.batch = Some(batch);
                self.retry_together(gh, project_id, t).await?;
                return Ok(Step::Again);
            };
            t.dispatched = None;
            self.record_pr(project_id, (&t.owner, &t.name), number, |g| {
                if let Some(pr) = g.pr.as_mut() {
                    pr.state = PrState::Merged;
                }
                g.stage = Some(PrStage::Merged);
            })?;
            t.set(number, TogetherState::Merged, None);
            batch.merged += 1;
            batch.refused = 0;
            // Save the dispatched merge's result before any further I/O,
            // including retesting when GitHub returned no merge SHA.
            t.batch = Some(batch.clone());
            self.save_together(project_id, t)?;
            if self.together_cancelled(gh, project_id, t).await? {
                return Ok(Step::Wait);
            }
            let Some(sha) = made else {
                self.retry_together(gh, project_id, t).await?;
                return Ok(Step::Again);
            };
            batch.base = sha;
            t.batch = Some(batch.clone());
            self.save_together(project_id, t)?;
        }
        self.close_check(gh, project_id, t).await?;
        Ok(Step::Again)
    }

    /// GitHub failed part way through making a merge check: one that may
    /// pass is tried again next time, and one it refused ends it.
    async fn together_failed(
        &self,
        gh: &dyn Github,
        project_id: ProjectId,
        t: &mut Together,
        e: GithubError,
    ) -> Result<Step, ProtocolError> {
        if self.together_cancelled(gh, project_id, t).await? {
            return Ok(Step::Wait);
        }
        if uncertain(&e) {
            self.retry_together(gh, project_id, t).await?;
            return Err(github_error(e));
        }
        let why = format!(
            "Øphio couldn't make its merge check: {}",
            github_error(e).message
        );
        self.end_together(gh, project_id, t, &why, false).await?;
        Ok(Step::Wait)
    }

    /// Ends the batch and puts what of it is still to merge first in line,
    /// to be tested again.
    async fn retry_together(
        &self,
        gh: &dyn Github,
        project_id: ProjectId,
        t: &mut Together,
    ) -> Result<(), ProtocolError> {
        if let Some(batch) = &t.batch {
            let again: Vec<u64> = (batch.prs.iter().skip(batch.merged).copied())
                .filter(|&n| {
                    matches!(
                        t.state(n),
                        Some(TogetherState::Waiting | TogetherState::Testing)
                    )
                })
                .collect();
            for &n in &again {
                t.set(n, TogetherState::Waiting, None);
            }
            if !again.is_empty() {
                t.groups.insert(0, again);
            }
        }
        self.close_check(gh, project_id, t).await
    }

    /// Ends it: what hasn't merged is left out, and why.
    async fn end_together(
        &self,
        gh: &dyn Github,
        project_id: ProjectId,
        t: &mut Together,
        why: &str,
        stopped: bool,
    ) -> Result<(), ProtocolError> {
        for pr in &mut t.view.prs {
            if matches!(pr.state, TogetherState::Waiting | TogetherState::Testing) {
                pr.state = TogetherState::LeftOut;
                pr.why = Some(why.to_owned());
            }
        }
        t.groups.clear();
        t.view.ended_at = Some(self.inner.clock.now());
        t.view.stopped = stopped;
        self.close_check(gh, project_id, t).await?;
        self.tell_together(project_id, t)
    }

    /// Tells the person how Merge together went, once it has ended: one
    /// alert for all it merged, or for what it left out and why. Nothing
    /// when they stopped it, or no task speaks for any of it.
    fn tell_together(&self, project_id: ProjectId, t: &Together) -> Result<(), ProtocolError> {
        if t.view.stopped {
            return Ok(());
        }
        let now = self.inner.clock.now();
        self.write(|tx| {
            let tasks = project_tasks(&tx.read(), project_id)?;
            let here = repo_key(&t.owner, &t.name);
            let task_of = |n: u64| speaking(&tasks, &here, n).map(|task| task.task_id);
            let merged: Vec<u64> = (t.view.prs.iter())
                .filter(|p| p.state == TogetherState::Merged)
                .map(|p| p.number)
                .collect();
            let left: Vec<&TogetherPr> = (t.view.prs.iter())
                .filter(|p| p.state == TogetherState::LeftOut)
                .collect();
            let last_merged = || merged.iter().rev().find_map(|&n| task_of(n));
            let item = match (left.is_empty(), merged.as_slice()) {
                (true, []) => None,
                (true, merged) => last_merged().map(|on| {
                    let headline = match merged {
                        [one] => format!("PR #{one} merged"),
                        _ => format!("PRs {} merged", numbers(merged)),
                    };
                    let topic = AlertTopic::Merged;
                    headline_item(on, AttentionKind::Completed, topic, &headline, None, now)
                }),
                (false, _) => (left.iter().find_map(|p| task_of(p.number)))
                    .or_else(last_merged)
                    .map(|on| {
                        let headline = match merged.len() {
                            0 => format!("Merge together: {} left out", left.len()),
                            n => format!("Merge together: {n} merged, {} left out", left.len()),
                        };
                        let why: Vec<String> = (left.iter())
                            .map(|p| {
                                let why = p.why.as_deref().unwrap_or("Left out.");
                                format!("#{}: {why}", p.number)
                            })
                            .collect();
                        let topic = AlertTopic::PrNeedsYou;
                        let kind = AttentionKind::WaitingForYou;
                        headline_item(on, kind, topic, &headline, Some(why.join("\n")), now)
                    }),
            };
            // The person started it: what came of it isn't held for a Run all.
            match item {
                Some(item) => put_item(tx, item),
                None => Ok(()),
            }
        })
    }

    /// Ends the batch: closes its merge check and deletes its branch.
    /// What GitHub refuses is left; nothing waits on it.
    async fn close_check(
        &self,
        gh: &dyn Github,
        project_id: ProjectId,
        t: &mut Together,
    ) -> Result<(), ProtocolError> {
        t.view.check = None;
        if let Some(batch) = t.batch.take() {
            let path = t.path();
            if let Some(number) = batch.number {
                let closing = json!({ "state": "closed" });
                let pull = format!("{path}/pulls/{number}");
                if let Err(e) = gh.rest(Method::Patch, &pull, Some(closing)).await {
                    tracing::warn!(error = %e, "closing a merge check failed");
                }
            }
            if let Some(branch) = &batch.branch {
                let gone = format!("{path}/git/refs/heads/{branch}");
                if let Err(e) = gh.rest(Method::Delete, &gone, None).await {
                    tracing::warn!(error = %e, "deleting a merge check's branch failed");
                }
            }
        }
        self.save_together(project_id, t)
    }
}
