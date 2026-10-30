// SPDX-License-Identifier: Apache-2.0
//! The line of green pull requests waiting to merge in Through merge. They
//! merge together, once nothing in their project is running or waiting in
//! Next up, so nothing merges under a run still working from the old
//! default branch.
//!
//! First they are tested together: Øphio merges each into a branch made
//! from the default branch's head and opens a draft pull request of it, the
//! merge check, for the repository's checks to run on. Green, each merges
//! in turn while the default branch is still what was tested; red, the line
//! is halved and each half tested alone, until the one that breaks is found
//! and handed back to its task with what failed. One already up to date
//! with the default branch, or a repository that runs no checks, merges
//! without one.

use crate::CompanionHub;
use crate::github::{REPO, github_error, project_tasks, repo_key, span};
use crate::github_tools::MergeResult;
use crate::github_watch::{MERGE_CHECK, headline_item, raise_item, speaking};
use ports::github::{Github, GithubError, Method};
use protocol::github::*;
use protocol::{
    Event, ProjectId, ProtocolError, TaskId,
    model::{AttentionItem, AttentionKind, Task, TaskState},
    notifications::AlertTopic,
    queue::QueueView,
    time::Timestamp,
};
use serde_json::{Value, json};

/// The line, one per project.
pub(crate) const TRAIN: &str = "github_train";
/// The start of a merge check's branch name.
pub(crate) const MERGE_BRANCH: &str = "ophio/merge-";
/// How long the merge check's checks may take before the person is asked.
pub(crate) const STALL_MS: i64 = 6 * 3_600_000;
/// Times in a row GitHub may refuse to merge one before the person is asked.
pub(crate) const MAX_REFUSED: u32 = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Train {
    /// What is being tested or merged now.
    #[serde(default)]
    pub batch: Option<Batch>,
    /// Halves of a line that broke, each tested alone in turn, before any
    /// that joined since.
    #[serde(default)]
    pub split: Vec<Vec<Car>>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Car {
    pub task_id: TaskId,
    pub number: u64,
    /// The head it waits at.
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Batch {
    pub cars: Vec<Car>,
    /// The default branch's head it was tested on, and then each merge's.
    pub base: String,
    /// The merge check's branch: None when it merges without one.
    #[serde(default)]
    pub branch: Option<String>,
    /// The merge check's pull request, once opened.
    #[serde(default)]
    pub number: Option<u64>,
    pub at: Timestamp,
    /// How many of `cars` have merged, in order.
    #[serde(default)]
    pub merged: usize,
    /// Tested green, or nothing to test: it merges.
    #[serde(default)]
    pub green: bool,
    /// Times in a row GitHub refused the next one.
    #[serde(default)]
    pub refused: u32,
    /// The person was told what merged.
    #[serde(default)]
    pub told: bool,
}

/// What the merge check's checks say.
pub(crate) enum Verdict {
    Waiting,
    Red(Vec<String>),
    Green,
}

/// The required checks, or all of them when none is: any failed, each as
/// "`name` (url)"; any still running, or none reported, waits.
pub(crate) fn verdict(view: &GithubPrView) -> Verdict {
    let required = view.checks.iter().any(|c| c.required);
    let considered: Vec<&PrCheck> = (view.checks.iter())
        .filter(|c| !required || c.required)
        .collect();
    let failed: Vec<String> = (considered.iter())
        .filter(|c| c.state == CheckState::Failed)
        .map(|c| match &c.url {
            Some(url) => format!("`{}` ({})", span(&c.name, 100), span(url, 300)),
            None => format!("`{}`", span(&c.name, 100)),
        })
        .collect();
    if !failed.is_empty() {
        return Verdict::Red(failed);
    }
    if considered.is_empty() || considered.iter().any(|c| c.state == CheckState::Pending) {
        return Verdict::Waiting;
    }
    Verdict::Green
}

/// Whether nothing in the project is running or waiting in Next up: a
/// paused Next up with something in it holds the line too, as does a helper
/// still answering or an answer still to pass back to its caller.
fn quiet(r: &store::Read<'_>, project_id: ProjectId) -> store::Result<bool> {
    let tasks: Vec<Task> = r.list("task")?;
    if (tasks.iter()).any(|t| t.project_id == project_id && crate::scratch::running(r, t)) {
        return Ok(false);
    }
    let helpers = crate::asking::pending_callers(r)?;
    if tasks
        .iter()
        .any(|t| t.project_id == project_id && helpers.contains(&t.task_id))
    {
        return Ok(false);
    }
    let queue: Option<QueueView> = r.get("queue", project_id)?;
    Ok(queue.is_none_or(|q| !q.items.iter().any(|i| i.is_pending())))
}

/// The pull requests waiting to merge on the repository `here`, each by
/// the task speaking for it, in the order they were opened.
fn waiting(tasks: &[Task], here: &str) -> Vec<Car> {
    let mut cars: Vec<Car> = (tasks.iter())
        .filter(|t| t.state != TaskState::Archived)
        .filter_map(|t| {
            let g = t.github.as_deref()?;
            let pr = g.pr.as_ref()?;
            let waits = g.stage == Some(PrStage::Merging)
                && !g.link.read_only()
                && pr.state == PrState::Open
                && g.repo.as_deref().is_none_or(|linked| linked == here)
                && speaking(tasks, here, pr.number).is_some_and(|s| s.task_id == t.task_id);
            waits.then(|| Car {
                task_id: t.task_id,
                number: pr.number,
                head: pr.head.clone(),
            })
        })
        .collect();
    cars.sort_by_key(|c| c.number);
    cars
}

/// "#3, #5 and #8".
fn numbers(cars: &[Car]) -> String {
    let names: Vec<String> = cars.iter().map(|c| format!("#{}", c.number)).collect();
    crate::github_tools::listed(&names)
}

impl CompanionHub {
    /// Moves a project's line on: tests what waits, merges what tested
    /// green, and finds what broke. Reads GitHub only when something waits.
    pub(crate) async fn advance_train(
        &self,
        project_id: ProjectId,
        settings: &GithubSettings,
    ) -> Result<(), ProtocolError> {
        // Merge together merges in the project now; what waits is tested
        // with what it merged, once it has finished.
        if self.read(|r| crate::github_together::together_on(r, project_id))? {
            return Ok(());
        }
        let (mut train, tasks, repo) = self.read(|r| {
            Ok((
                r.get::<Train>(TRAIN, project_id)?.unwrap_or_default(),
                project_tasks(r, project_id)?,
                r.get::<GithubRepo>(REPO, project_id)?,
            ))
        })?;
        let any = (tasks.iter()).any(|t| {
            t.github
                .as_deref()
                .is_some_and(|g| g.stage == Some(PrStage::Merging))
        });
        if train.batch.is_none() && train.split.is_empty() && !any {
            return Ok(());
        }
        let gh = self.github()?;
        let (owner, name) = self.on_github(project_id).await?;
        let here = repo_key(&owner, &name);
        let path = format!("/repos/{owner}/{name}");
        let repo = repo.filter(|kept| repo_key(&kept.owner, &kept.name) == here);
        let waiting = waiting(&tasks, &here);
        // Through merge turned off, or no longer allowed: what waits is the
        // person's to merge.
        if !self.merges(settings, repo.as_ref()) {
            self.abandon(&*gh, &path, project_id, &mut train).await?;
            train.split.clear();
            self.save_train(project_id, &train)?;
            for car in waiting {
                self.to_person(car.task_id, car.number, None)?;
            }
            return Ok(());
        }
        let Some(repo) = repo else {
            return Ok(());
        };
        // A sensitive change leaves the line for the person, who approves
        // it by merging it themselves; the rest go on without it.
        let mut waiting = waiting;
        let mut kept = Vec::new();
        for car in waiting.drain(..) {
            let weighed = self
                .approval_for(
                    project_id,
                    (&owner, &name),
                    car.number,
                    &car.head,
                    &repo.default_branch,
                )
                .await?;
            match weighed.need {
                Some(need) => {
                    let why = crate::review_gate::why(&need);
                    self.to_person(car.task_id, car.number, Some(why))?;
                }
                None => kept.push(car),
            }
        }
        let waiting = kept;
        // What left the line, or changed, leaves its half; what changed is
        // tested again at its new head.
        let before = train.split.clone();
        for half in &mut train.split {
            half.retain_mut(
                |car| match waiting.iter().find(|w| w.task_id == car.task_id) {
                    Some(now) => {
                        *car = now.clone();
                        true
                    }
                    None => false,
                },
            );
        }
        train.split.retain(|half| !half.is_empty());
        if train.split != before {
            self.save_train(project_id, &train)?;
        }
        let quiet = self.read(|r| quiet(r, project_id))?;
        if let Some(batch) = train.batch.clone() {
            let left = &batch.cars[batch.merged.min(batch.cars.len())..];
            if left.is_empty() || !left.iter().all(|car| waiting.contains(car)) {
                return self.abandon(&*gh, &path, project_id, &mut train).await;
            }
            if !quiet {
                return Ok(());
            }
            if !batch.green {
                return self
                    .decide(&*gh, &path, project_id, &mut train, &owner, &name)
                    .await;
            }
            return self.merge_batch(&*gh, &path, project_id, &mut train).await;
        }
        if !quiet {
            return Ok(());
        }
        let cars = match train.split.is_empty() {
            true => waiting,
            false => train.split.remove(0),
        };
        if cars.is_empty() {
            return Ok(());
        }
        self.depart(&*gh, &path, &repo, project_id, &mut train, cars)
            .await
    }

    fn save_train(&self, project_id: ProjectId, train: &Train) -> Result<(), ProtocolError> {
        self.write(|tx| match train == &Train::default() {
            true => tx.delete(TRAIN, project_id),
            false => tx.put(TRAIN, project_id, train),
        })
    }

    /// Starts testing `cars` together, or merging them where there is
    /// nothing to test: one already up to date with the default branch, or
    /// a repository that runs no checks.
    async fn depart(
        &self,
        gh: &dyn Github,
        path: &str,
        repo: &GithubRepo,
        project_id: ProjectId,
        train: &mut Train,
        cars: Vec<Car>,
    ) -> Result<(), ProtocolError> {
        for car in &cars {
            let pr = gh
                .rest(Method::Get, &format!("{path}/pulls/{}", car.number), None)
                .await
                .map_err(github_error)?;
            self.refuse_report_automation(&repo.owner, &repo.name, &pr)
                .await?;
        }
        let default = &repo.default_branch;
        let base = head_of(gh, path, default).await?;
        let now = self.inner.clock.now();
        let mut batch = Batch {
            cars,
            base: base.clone(),
            branch: None,
            number: None,
            at: now,
            merged: 0,
            green: false,
            refused: 0,
            told: false,
        };
        let untested = match batch.cars.as_slice() {
            _ if repo.runs_checks == Some(false) => true,
            [one] => behind_by(gh, path, default, &one.head).await? == Some(0),
            _ => false,
        };
        if untested {
            batch.green = true;
            train.batch = Some(batch);
            self.save_train(project_id, train)?;
            return self.merge_batch(gh, path, project_id, train).await;
        }
        // Kept before the branch is made, so a restart between cleans it up.
        let branch = format!("{MERGE_BRANCH}{}", now.0);
        batch.branch = Some(branch.clone());
        train.batch = Some(batch);
        self.save_train(project_id, train)?;
        let made = json!({ "ref": format!("refs/heads/{branch}"), "sha": base });
        if let Err(e) = gh
            .rest(Method::Post, &format!("{path}/git/refs"), Some(made))
            .await
        {
            self.abandon(gh, path, project_id, train).await?;
            return Err(github_error(e));
        }
        let mut cars = Vec::new();
        for car in train
            .batch
            .as_ref()
            .map(|b| b.cars.clone())
            .unwrap_or_default()
        {
            let merging = json!({
                "base": branch,
                "head": car.head,
                "commit_message": format!("Øphio merge check: #{}", car.number),
            });
            match gh
                .rest(Method::Post, &format!("{path}/merges"), Some(merging))
                .await
            {
                Ok(_) => cars.push(car),
                // It conflicts with one before it: it waits for the next
                // line, by when its own pull request shows the conflict.
                Err(GithubError::Status { code: 409, .. }) => {}
                Err(e) => {
                    self.abandon(gh, path, project_id, train).await?;
                    return Err(github_error(e));
                }
            }
        }
        if cars.is_empty() {
            return self.abandon(gh, path, project_id, train).await;
        }
        let opening = |draft: bool| {
            json!({
                "title": format!("Øphio merge check: {}", numbers(&cars)),
                "head": branch,
                "base": default,
                "body": format!(
                    "Øphio tests {} together with `{default}` here before merging them. It closes this itself; don't merge it.",
                    numbers(&cars)
                ),
                "draft": draft,
            })
        };
        let pulls = format!("{path}/pulls");
        let opened = match gh.rest(Method::Post, &pulls, Some(opening(true))).await {
            // A repository that has no drafts.
            Err(GithubError::Status { code: 422, .. }) => {
                gh.rest(Method::Post, &pulls, Some(opening(false))).await
            }
            other => other,
        };
        let number = match opened {
            Ok(answer) => answer["number"].as_u64(),
            Err(e) => {
                self.abandon(gh, path, project_id, train).await?;
                return Err(github_error(e));
            }
        };
        if let Some(batch) = train.batch.as_mut() {
            batch.cars = cars;
            batch.number = number;
            batch.at = self.inner.clock.now();
        }
        self.save_train(project_id, train)
    }

    /// Reads the merge check: green merges, red is halved or handed back,
    /// and one that hasn't finished in six hours goes to the person.
    async fn decide(
        &self,
        gh: &dyn Github,
        path: &str,
        project_id: ProjectId,
        train: &mut Train,
        owner: &str,
        name: &str,
    ) -> Result<(), ProtocolError> {
        let Some(batch) = train.batch.clone() else {
            return Ok(());
        };
        // A restart before it was opened.
        let Some(number) = batch.number else {
            return self.abandon(gh, path, project_id, train).await;
        };
        let (_, view) = self.read_pr_in(owner, name, number).await?;
        if view.summary.state != PrState::Open {
            return self.abandon(gh, path, project_id, train).await;
        }
        match verdict(&view) {
            Verdict::Waiting => {
                let now = self.inner.clock.now();
                if now.0 - batch.at.0 < STALL_MS {
                    return Ok(());
                }
                let why = format!(
                    "The checks on Øphio's merge check #{number} didn't finish in six hours, so it won't merge by itself."
                );
                for car in &batch.cars {
                    self.cap(car.task_id, car.number, why.clone())?;
                }
                self.abandon(gh, path, project_id, train).await
            }
            Verdict::Red(failed) => {
                if let [car] = batch.cars.as_slice() {
                    self.broke(car, number, &failed)?;
                } else {
                    let (first, second) = batch.cars.split_at(batch.cars.len() / 2);
                    train.split.insert(0, second.to_vec());
                    train.split.insert(0, first.to_vec());
                }
                self.abandon(gh, path, project_id, train).await
            }
            Verdict::Green => {
                if let Some(batch) = train.batch.as_mut() {
                    batch.green = true;
                }
                self.save_train(project_id, train)?;
                let repo = self.read(|r| r.get::<GithubRepo>(REPO, project_id))?;
                match repo {
                    Some(_) => self.merge_batch(gh, path, project_id, train).await,
                    None => Ok(()),
                }
            }
        }
    }

    /// Hands what broke back to the task whose pull request broke it, and
    /// takes it out of the line until it's fixed.
    fn broke(&self, car: &Car, number: u64, failed: &[String]) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        let text = format!(
            "- Tested with the latest default branch on Øphio's merge check #{number}, this failed: {}. Its own checks passed on its branch, so something it changes breaks with what has merged since. Merge the default branch into the branch, find it, and fix it.",
            crate::github_tools::listed(failed)
        );
        self.write(|tx| {
            if !crate::github_watch::change_pr_link(tx, car.task_id, car.number, now, |g| {
                if g.stage == Some(PrStage::Merging) {
                    g.stage = Some(PrStage::Babysitting);
                }
            })? {
                return Ok(());
            }
            tx.put(MERGE_CHECK, car.task_id, &text)
        })
    }

    /// Merges what tested green, in order, while the default branch is what
    /// it was tested on. One GitHub refuses is tried again next time, and
    /// after five times in a row is the person's, and the rest tested again.
    async fn merge_batch(
        &self,
        gh: &dyn Github,
        path: &str,
        project_id: ProjectId,
        train: &mut Train,
    ) -> Result<(), ProtocolError> {
        let Some(mut batch) = train.batch.clone() else {
            return Ok(());
        };
        while let Some(car) = batch.cars.get(batch.merged).cloned() {
            let task: Option<Task> = self.read(|r| r.get("task", car.task_id))?;
            let Some(task) = task else {
                return self.abandon(gh, path, project_id, train).await;
            };
            let merged = match self.posting(&task).await {
                Ok(p) => self.merge_now(&task, &p, &car.head, &batch.base).await,
                Err(e) => Err(e),
            };
            match merged {
                Ok(MergeResult::BaseChanged) => {
                    return self.abandon(gh, path, project_id, train).await;
                }
                Ok(MergeResult::Merged(made)) => {
                    batch.merged += 1;
                    batch.refused = 0;
                    self.say_merged(&mut batch)?;
                    train.batch = Some(batch.clone());
                    let Some(sha) = made else {
                        // The merge happened, but an arbitrary current head
                        // may include somebody else's untested changes.
                        return self.abandon(gh, path, project_id, train).await;
                    };
                    batch.base = sha;
                    train.batch = Some(batch.clone());
                    self.save_train(project_id, train)?;
                }
                Err(e) => {
                    batch.refused += 1;
                    train.batch = Some(batch.clone());
                    if batch.refused < MAX_REFUSED {
                        return self.save_train(project_id, train);
                    }
                    // What merged before it is told with it.
                    let merged = &batch.cars[..batch.merged];
                    let why = match merged.is_empty() {
                        true => e.message,
                        false => format!("{}\n{} merged before it.", e.message, numbers(merged)),
                    };
                    batch.told = true;
                    train.batch = Some(batch.clone());
                    self.to_person(car.task_id, car.number, Some(why))?;
                    return self.abandon(gh, path, project_id, train).await;
                }
            }
        }
        self.abandon(gh, path, project_id, train).await
    }

    /// Says the batch's latest car merged. While more are to merge it says
    /// so quietly, and the last, or the batch ending before them, alerts
    /// once for all that merged.
    fn say_merged(&self, batch: &mut Batch) -> Result<(), ProtocolError> {
        let merged = &batch.cars[..batch.merged.min(batch.cars.len())];
        let Some(car) = merged.last() else {
            return Ok(());
        };
        let last = batch.merged >= batch.cars.len();
        let title = match (last, merged.len()) {
            (true, 2..) => format!("PRs {} merged", numbers(merged)),
            _ => format!("PR #{} merged", car.number),
        };
        let now = self.inner.clock.now();
        let mut item = headline_item(
            car.task_id,
            AttentionKind::Completed,
            AlertTopic::Merged,
            &title,
            None,
            now,
        );
        item.quiet = !last;
        batch.told |= last;
        self.write(|tx| {
            // Said quietly before the batch ended: the alert takes its place.
            for mut said in tx.read().task_attention::<AttentionItem>(car.task_id)? {
                if last
                    && said.quiet
                    && said.resolved_at.is_none()
                    && said.topic() == Some(AlertTopic::Merged)
                {
                    said.resolved_at = Some(now);
                    tx.put("attention", said.attention_id, &said)?;
                    tx.emit(Event::AttentionResolved {
                        attention_id: said.attention_id,
                    })?;
                }
            }
            raise_item(tx, item)
        })
    }

    /// Ends the batch: closes its merge check and deletes its branch.
    /// What GitHub refuses is left; nothing waits on it. What merged before
    /// it ended is told.
    async fn abandon(
        &self,
        gh: &dyn Github,
        path: &str,
        project_id: ProjectId,
        train: &mut Train,
    ) -> Result<(), ProtocolError> {
        let Some(mut batch) = train.batch.take() else {
            return self.save_train(project_id, train);
        };
        if batch.merged > 0 && !batch.told {
            batch.cars.truncate(batch.merged);
            self.say_merged(&mut batch)?;
        }
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
        self.save_train(project_id, train)
    }
}

/// The head commit of `branch`.
pub(crate) async fn head_of(
    gh: &dyn Github,
    path: &str,
    branch: &str,
) -> Result<String, ProtocolError> {
    let answer: Value = (gh.rest(Method::Get, &format!("{path}/git/ref/heads/{branch}"), None))
        .await
        .map_err(github_error)?;
    answer["object"]["sha"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            github_error(GithubError::Failed(format!(
                "GitHub didn't say where `{branch}` is."
            )))
        })
}

/// How far `head` is behind `branch`, when GitHub says.
pub(crate) async fn behind_by(
    gh: &dyn Github,
    path: &str,
    branch: &str,
    head: &str,
) -> Result<Option<u64>, ProtocolError> {
    let answer: Value = (gh.rest(
        Method::Get,
        &format!("{path}/compare/{branch}...{head}"),
        None,
    ))
    .await
    .map_err(github_error)?;
    Ok(answer["behind_by"].as_u64())
}
