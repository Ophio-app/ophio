// SPDX-License-Identifier: Apache-2.0
//! The report worker uses the host's GitHub account; agents never receive it.
use crate::{CompanionHub, github::REPO, github_watch::put_item};
use ports::github::{Github, Method, ReportGit};
use protocol::{
    Event, ProjectId, TaskId, auth::Principal, capability::HarnessKind, github::*, model::*,
};
use report_fixer::{
    config::Config,
    execution::{SandboxedAgents, SandboxedTests},
    repository, *,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};

const PENDING: &str = "report_fix_approval";
pub use report_fixer::RunningBuild as RunningBuildInfo;
pub use report_fixer::config::Config as PipelineConfig;
const TAP: &str = "report_fix_merge_tap";
const COMMITS: &str = "report_fix_commit";
const PRS: &str = "report_fix_pr";
const REPOSITORIES: &str = "report_fix_repository";
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PublishedCommit {
    repository: String,
    commit: String,
    prs: std::collections::BTreeSet<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    view: ApprovalView,
    task_id: TaskId,
    attention_id: protocol::AttentionId,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MergeTap {
    pub head: String,
    pub who: String,
    pub when_ms: i64,
}

#[derive(Debug)]
pub struct GithubForge {
    pub github: Arc<dyn Github>,
    pub repository: String,
    pub mirror: PathBuf,
    pub hub: CompanionHub,
}
fn github_read_error(error: ports::github::GithubError) -> Error {
    use ports::github::GithubError;
    match error {
        GithubError::Failed(_)
        | GithubError::Status {
            code: 408 | 429 | 500..=599,
            ..
        } => Error::Transient("GitHub read"),
        _ => Error::Operation("GitHub read"),
    }
}
impl GithubForge {
    fn base(&self) -> String {
        format!("/repos/{}", self.repository)
    }
    async fn get(&self, suffix: &str) -> Result<Value> {
        self.github
            .rest(Method::Get, &format!("{}{suffix}", self.base()), None)
            .await
            .map_err(github_read_error)
    }
    fn owned(&self) -> Result<()> {
        if self.mirror.exists() {
            let m = std::fs::symlink_metadata(&self.mirror)?;
            if m.file_type().is_symlink()
                || std::fs::read_to_string(self.mirror.join("report-fixer-owned"))
                    .ok()
                    .as_deref()
                    != Some("report-fixer-v1\n")
            {
                return Err(Error::Operation("mirror is not owned by pipeline"));
            }
        } else {
            std::fs::create_dir_all(&self.mirror)?;
            let out = repository::command(&self.mirror)
                .args(["init", "--bare"])
                .output()?;
            if !out.status.success() {
                return Err(Error::Operation("mirror initialization"));
            }
            std::fs::write(self.mirror.join("report-fixer-owned"), "report-fixer-v1\n")?;
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl Forge for GithubForge {
    async fn prepare(&self) -> Result<String> {
        self.owned()?;
        self.github
            .report_git(&self.repository, &self.mirror, ReportGit::Fetch)
            .await
            .map_err(|_| Error::Transient("mirror fetch"))?;
        let repo = self.get("").await?;
        let branch = repo["default_branch"]
            .as_str()
            .ok_or(Error::Operation("default branch"))?;
        if branch.starts_with('-') || branch.contains("..") || branch.chars().any(char::is_control)
        {
            return Err(Error::Operation("default branch"));
        }
        repository::text(
            &self.mirror,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/remotes/origin/{branch}^{{commit}}"),
            ],
        )
    }
    async fn push_open(&self, proposal: &Proposal) -> Result<PullRequest> {
        // The host authors one commit per proposal. Keep its identity before
        // any remote write: an interrupted push or PR creation can still publish it.
        self.hub
            .remember_report_fix(&self.repository, &proposal.head, None)
            .map_err(|_| Error::Operation("report commit provenance"))?;
        self.github
            .report_git(
                &self.repository,
                &self.mirror,
                ReportGit::Push {
                    commit: proposal.head.clone(),
                    branch: proposal.branch.clone(),
                },
            )
            .await
            .map_err(|_| Error::Operation("report branch push"))?;
        let repo = self.get("").await?;
        let base = repo["default_branch"]
            .as_str()
            .ok_or(Error::Operation("default branch"))?;
        let answer = self.github.rest(Method::Post, &format!("{}/pulls", self.base()), Some(json!({ "head":proposal.branch, "base":base, "title":proposal.title, "body":proposal.body, "draft":false }))).await.map_err(|_| Error::Operation("report PR"))?;
        let number = answer["number"]
            .as_u64()
            .ok_or(Error::Operation("PR number"))?;
        self.hub
            .remember_report_fix(&self.repository, &proposal.head, Some(number))
            .map_err(|_| Error::Operation("report PR provenance"))?;
        let pr = PullRequest {
            number,
            head: answer["head"]["sha"]
                .as_str()
                .ok_or(Error::Operation("PR head"))?
                .into(),
        };
        if pr.head != proposal.head {
            return Err(Error::Operation("PR head changed"));
        }
        Ok(pr)
    }
    async fn ci(&self, pr: &PullRequest) -> Result<CiState> {
        let pull = self.get(&format!("/pulls/{}", pr.number)).await?;
        if pull["head"]["sha"] != pr.head
            || !pull["auto_merge"].is_null()
            || !pull["head"]["ref"]
                .as_str()
                .is_some_and(|s| s.starts_with("report/"))
        {
            return Ok(CiState::Failed);
        }
        let checks = self
            .get(&format!("/commits/{}/check-runs?per_page=100", pr.head))
            .await?;
        let checks = checks["check_runs"]
            .as_array()
            .ok_or(Error::Operation("check inventory"))?;
        if checks.is_empty() {
            return Ok(CiState::Pending);
        }
        if checks.len() >= 100 {
            return Ok(CiState::Failed);
        }
        if checks.iter().any(|c| c["status"] != "completed") {
            return Ok(CiState::Pending);
        }
        if checks.iter().any(|c| {
            !matches!(
                c["conclusion"].as_str(),
                Some("success" | "skipped" | "neutral")
            )
        }) {
            return Ok(CiState::Failed);
        }
        // Trust neither a check name alone nor a label supplied by the changed tree.
        let runs = self
            .get(&format!(
                "/actions/workflows/checks.yml/runs?head_sha={}&per_page=100",
                pr.head
            ))
            .await?;
        let runs = runs["workflow_runs"]
            .as_array()
            .ok_or(Error::Operation("workflow inventory"))?;
        let Some(run) = runs
            .iter()
            .filter(|r| r["head_sha"] == pr.head && r["event"] == "pull_request")
            .max_by_key(|r| r["id"].as_u64())
        else {
            return Ok(CiState::Pending);
        };
        if run["status"] != "completed" {
            return Ok(CiState::Pending);
        }
        if run["conclusion"] != "success" {
            return Ok(CiState::Failed);
        }
        let id = run["id"].as_u64().ok_or(Error::Operation("workflow id"))?;
        let jobs = self
            .get(&format!("/actions/runs/{id}/jobs?per_page=100"))
            .await?;
        let jobs = jobs["jobs"]
            .as_array()
            .ok_or(Error::Operation("isolated jobs"))?;
        if jobs.is_empty()
            || jobs.len() >= 100
            || jobs
                .iter()
                .filter(|j| j["conclusion"] != "skipped")
                .any(|j| {
                    j["conclusion"] != "success"
                        || !j["labels"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|l| l == "ophio-isolated"))
                })
        {
            return Ok(CiState::Failed);
        }
        if !jobs
            .iter()
            .any(|j| j["name"] == "linux" && j["conclusion"] == "success")
        {
            return Ok(CiState::Failed);
        }
        Ok(CiState::Passed {
            run: format!("github-actions/{id}"),
        })
    }
    async fn merged(&self, pr: &PullRequest) -> Result<Option<Merged>> {
        let tap: Option<MergeTap> = self
            .hub
            .read(|r| {
                r.get(
                    TAP,
                    format!("{}/{}", self.repository.to_ascii_lowercase(), pr.number),
                )
            })
            .map_err(|_| Error::Operation("merge authorization"))?;
        let Some(tap) = tap.filter(|t| t.head == pr.head) else {
            return Ok(None);
        };
        let pull = self.get(&format!("/pulls/{}", pr.number)).await?;
        if pull["merged"] != true {
            return Ok(None);
        }
        let commit = pull["merge_commit_sha"]
            .as_str()
            .ok_or(Error::Operation("merge commit"))?
            .into();
        Ok(Some(Merged {
            commit,
            who: tap.who,
            when_ms: tap.when_ms,
        }))
    }
    async fn contains(&self, commit: &str, build_commit: &str) -> Result<bool> {
        repository::oid(commit)?;
        repository::oid(build_commit)?;
        self.github
            .report_git(&self.repository, &self.mirror, ReportGit::Fetch)
            .await
            .map_err(|_| Error::Transient("release ancestry fetch"))?;
        Ok(repository::command(&self.mirror)
            .args(["merge-base", "--is-ancestor", commit, build_commit])
            .status()?
            .success())
    }
}

#[derive(Debug)]
pub struct OwnerApprovals {
    pub hub: CompanionHub,
    pub project_id: ProjectId,
    pub repository: String,
    pub forge: Arc<GithubForge>,
}
fn plain_gates(gates: &report_pipeline::gates::GateReport) -> String {
    use report_pipeline::gates::{Flag, Gate, Outcome};
    gates
        .results
        .iter()
        .map(|r| {
            let name = match r.gate {
                Gate::RedGreen => "Reproduction and other tests",
                Gate::Dependencies => "Dependencies",
                Gate::DiffCap => "Change size",
                Gate::TestIntegrity => "Frozen test",
                Gate::SensitivePaths => "Security-sensitive files",
                Gate::SensitiveContent => "Security-sensitive code",
            };
            let outcome = match r.outcome {
                Outcome::Pass => "passed",
                Outcome::Owner => "needs your own review",
                Outcome::Fail => "stopped",
            };
            let flags = r
                .flags
                .iter()
                .map(|flag| match flag {
                    Flag::SensitivePath => "It changes a security-sensitive file.",
                    Flag::ExistingTestChanged => "It changes tests that already existed.",
                    Flag::UrlOrHost => "It adds a URL or network host.",
                    Flag::Unsafe => "It adds unsafe Rust code.",
                    Flag::Process => "It can start another program.",
                    Flag::Environment => "It reads environment values.",
                    Flag::HomePath => "It names a home folder.",
                    _ => "A code check needs attention.",
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("{name}: {outcome}. {flags}").trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
#[async_trait::async_trait]
impl Approvals for OwnerApprovals {
    async fn revoke(&self, report_id: protocol::ReportId) -> Result<()> {
        self.hub
            .write(|tx| {
                let Some(p) = tx.read().get::<Pending>(PENDING, report_id)? else {
                    return Ok(());
                };
                if let Some(mut item) = tx
                    .read()
                    .get::<AttentionItem>("attention", p.attention_id)?
                    && item.resolved_at.is_none()
                {
                    item.resolved_at = Some(self.hub.inner.clock.now());
                    tx.put("attention", item.attention_id, &item)?;
                    tx.emit(Event::AttentionResolved {
                        attention_id: item.attention_id,
                    })?;
                    crate::runs::recount_attention(tx, p.task_id)?;
                }
                tx.delete(PENDING, report_id)?;
                tx.delete(
                    TAP,
                    format!(
                        "{}/{}",
                        self.repository.to_ascii_lowercase(),
                        p.view.pr.number
                    ),
                )?;
                Ok(())
            })
            .map_err(|_| Error::Operation("revoke owner attention"))?;
        Ok(())
    }
    async fn raise(&self, view: &ApprovalView) -> Result<()> {
        let now = self.hub.inner.clock.now();
        let title = format!("A fix for report {} is ready", view.reference);
        let body = format!(
            "Area: {:?}.\nTest: {}\n{} files, {} changed lines.\n{}{}\nMerge this fix after reviewing it, or close this request.",
            view.area,
            view.test,
            view.files,
            view.lines,
            plain_gates(&view.gates),
            if view.conflict {
                "\nAnother report changed this area; review both fixes."
            } else {
                ""
            }
        );
        self.hub
            .write(|tx| {
                if tx.read().get::<Pending>(PENDING, view.report_id)?.is_some() {
                    return Ok(());
                }
                let task_id = TaskId::new();
                let task = Task {
                    task_id,
                    project_id: self.project_id,
                    kind: TaskKind::Coding,
                    mode: TaskMode::Work,
                    title: title.clone(),
                    state: TaskState::Completed,
                    revision: 1,
                    harness: HarnessKind::Codex,
                    model: None,
                    effort: None,
                    launch_policy: None,
                    branch: None,
                    start: None,
                    browser: false,
                    conversation: false,
                    access: None,
                    access_blocked: None,
                    github: Some(Box::new(TaskGithub {
                        link: GithubLink::Pr {
                            number: view.pr.number,
                        },
                        issue: None,
                        pr: Some(PrSummary {
                            number: view.pr.number,
                            url: format!(
                                "https://github.com/{}/pull/{}",
                                self.repository, view.pr.number
                            ),
                            state: PrState::Open,
                            draft: false,
                            head: view.pr.head.clone(),
                            passed: 1,
                            failed: 0,
                            pending: 0,
                            unresolved: 0,
                            conflict: false,
                        }),
                        stage: Some(PrStage::Ready),
                        found_by: None,
                        checked_by: None,
                        check_with: None,
                        from_stranger: false,
                        base: None,
                        rounds: 0,
                        repo: Some(self.repository.to_ascii_lowercase()),
                        finding: None,
                        already_fixed: false,
                        approval: None,
                        review: None,
                    })),
                    wake: None,
                    asked_by: None,
                    created_by: Principal::System,
                    created_at: now,
                    updated_at: now,
                    current_run: None,
                    target_id: None,
                    mission: None,
                    attention_count: 1,
                    status_line: None,
                    review: None,
                };
                tx.put("task", task_id, &task)?;
                tx.emit(Event::TaskUpserted { task })?;
                let item = AttentionItem {
                    attention_id: protocol::AttentionId::new(),
                    child_id: None,
                    kind: AttentionKind::WaitingForYou,
                    title: title.clone(),
                    body: Some(body.clone()),
                    task_id: Some(task_id),
                    run_id: None,
                    approval_id: None,
                    created_at: now,
                    resolved_at: None,
                    priority: 10,
                    topic: Some(protocol::notifications::AlertTopic::PrNeedsYou),
                    quiet: false,
                    report_id: Some(view.report_id),
                };
                tx.put(
                    PENDING,
                    view.report_id,
                    &Pending {
                        view: view.clone(),
                        task_id,
                        attention_id: item.attention_id,
                    },
                )?;
                put_item(tx, item)
            })
            .map_err(|_| Error::Operation("owner attention"))?;
        Ok(())
    }
    async fn decision(&self, view: &ApprovalView) -> Result<OwnerDecision> {
        if self.forge.merged(&view.pr).await?.is_some() {
            self.hub
                .write(|tx| {
                    let p: Pending = tx.read().require(PENDING, view.report_id)?;
                    let mut item: AttentionItem = tx.read().require("attention", p.attention_id)?;
                    if item.resolved_at.is_none() {
                        item.resolved_at = Some(self.hub.inner.clock.now());
                        tx.put("attention", item.attention_id, &item)?;
                        tx.emit(Event::AttentionResolved {
                            attention_id: item.attention_id,
                        })?;
                        crate::runs::recount_attention(tx, p.task_id)?;
                    }
                    Ok(())
                })
                .map_err(|_| Error::Operation("settle owner attention"))?;
            return Ok(OwnerDecision::Merged);
        }
        let closed = self
            .hub
            .read(|r| {
                let p: Pending = r.require(PENDING, view.report_id)?;
                Ok(r.get::<AttentionItem>("attention", p.attention_id)?
                    .is_some_and(|a| a.resolved_at.is_some()))
            })
            .map_err(|_| Error::Operation("owner decision"))?;
        let pull = self
            .forge
            .get(&format!("/pulls/{}", view.pr.number))
            .await?;
        Ok(
            if closed || (pull["state"] == "closed" && pull["merged"] != true) {
                OwnerDecision::Closed
            } else {
                OwnerDecision::Waiting
            },
        )
    }
}

impl CompanionHub {
    pub(crate) fn remember_report_fix(
        &self,
        repository: &str,
        commit: &str,
        number: Option<u64>,
    ) -> std::result::Result<(), protocol::ProtocolError> {
        let repository = repository.to_ascii_lowercase();
        let commit = commit.to_ascii_lowercase();
        let key = format!("{repository}/{commit}");
        self.write(|tx| {
            let mut published =
                tx.read()
                    .get::<PublishedCommit>(COMMITS, &key)?
                    .unwrap_or(PublishedCommit {
                        repository: repository.clone(),
                        commit,
                        prs: Default::default(),
                    });
            if let Some(number) = number {
                published.prs.insert(number);
                tx.put(PRS, format!("{repository}/{number}"), &true)?;
            }
            tx.put(COMMITS, key, &published)?;
            tx.put(REPOSITORIES, repository, &true)
        })
    }

    pub(crate) fn report_fix_identity(
        &self,
        repository: &str,
        number: Option<u64>,
        commits: &[&str],
    ) -> std::result::Result<(bool, bool), protocol::ProtocolError> {
        let repository = repository.to_ascii_lowercase();
        self.read(|r| {
            let tracked = r.get::<bool>(REPOSITORIES, &repository)?.unwrap_or(false);
            if let Some(number) = number
                && r.get::<bool>(PRS, format!("{repository}/{number}"))? == Some(true)
            {
                return Ok((tracked, true));
            }
            for commit in commits {
                if r.get::<PublishedCommit>(
                    COMMITS,
                    format!("{repository}/{}", commit.to_ascii_lowercase()),
                )?
                .is_some()
                {
                    return Ok((tracked, true));
                }
            }
            Ok((tracked, false))
        })
    }

    pub(crate) async fn check_report_fix_merge(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        head: &str,
    ) -> std::result::Result<(), protocol::ProtocolError> {
        let repository = format!("{owner}/{name}");
        let known = self.read(|r| {
            Ok(r.list::<Pending>(PENDING)?.iter().any(|p| {
                p.view.pr.number == number
                    && p.view.pr.head == head
                    && p.view.gates.outcome() != report_pipeline::gates::Outcome::Fail
                    && r.get::<Task>("task", p.task_id)
                        .ok()
                        .flatten()
                        .and_then(|t| t.github.and_then(|g| g.repo))
                        .is_some_and(|s| s.eq_ignore_ascii_case(&repository))
                    && r.get::<AttentionItem>("attention", p.attention_id)
                        .ok()
                        .flatten()
                        .is_some_and(|a| a.resolved_at.is_none())
            }))
        })?;
        if !known {
            return Err(crate::error(
                protocol::ErrorCode::PermissionDenied,
                "This report fix no longer has an open owner review",
            ));
        }
        let forge = GithubForge {
            github: self.github()?,
            repository,
            mirror: PathBuf::new(),
            hub: self.clone(),
        };
        let ci = forge
            .ci(&PullRequest {
                number,
                head: head.into(),
            })
            .await
            .map_err(|_| {
                crate::error(
                    protocol::ErrorCode::NativeFailure,
                    "The report fix's isolated checks could not be read",
                )
            })?;
        if !matches!(ci, CiState::Passed { .. }) {
            return Err(crate::error(
                protocol::ErrorCode::Conflict,
                "The report fix needs passing isolated checks on this commit",
            ));
        }
        Ok(())
    }
    pub(crate) fn report_fix_tap(
        &self,
        repository: &str,
        number: u64,
        head: &str,
        caller: &crate::Caller,
    ) -> std::result::Result<(), protocol::ProtocolError> {
        let who = match caller.principal {
            Principal::Device { device_id } => format!("device:{device_id}"),
            Principal::LocalOwner => "local-owner".into(),
            _ => {
                return Err(crate::error(
                    protocol::ErrorCode::PermissionDenied,
                    "Only the person can merge a report fix",
                ));
            }
        };
        let tap = MergeTap {
            head: head.into(),
            who,
            when_ms: self.inner.clock.now().0,
        };
        self.write(|tx| {
            tx.put(
                TAP,
                format!("{}/{number}", repository.to_ascii_lowercase()),
                &tap,
            )
        })
    }
    /// Called only by companion composition after explicit enabled configuration.
    pub fn start_report_fixes(&self, config: Config, build: Option<RunningBuild>) {
        if !config.enabled {
            return;
        }
        if let Err(why) = config.readiness() {
            tracing::warn!(reason = why, "Report pipeline stays off");
            return;
        }
        if let Err(why) = report_fixer::execution::readiness(&config) {
            tracing::warn!(reason=%why, "Report pipeline stays off");
            return;
        }
        let Ok(github) = self.github() else {
            tracing::warn!("Report pipeline stays off: GitHub is unavailable");
            return;
        };
        let project = self.read(|r| {
            for project in r.list::<Project>("project")? {
                if r.get::<GithubRepo>(REPO, project.project_id)?
                    .is_some_and(|g| {
                        format!("{}/{}", g.owner, g.name).eq_ignore_ascii_case(&config.repository)
                    })
                {
                    return Ok(Some(project.project_id));
                }
            }
            Ok(None)
        });
        let Ok(Some(project_id)) = project else {
            tracing::warn!("Report pipeline stays off: connect its repository to a project first");
            return;
        };
        let token = match std::fs::read_to_string(&config.intake_token_file) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!("Report pipeline stays off: intake token is unreadable");
                return;
            }
        };
        let intake = match intake::client::Client::new(&config.intake_url, token.trim()) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("Report pipeline stays off: intake configuration is invalid");
                return;
            }
        };
        let forge = Arc::new(GithubForge {
            github,
            repository: config.repository.clone(),
            mirror: config.mirror.clone(),
            hub: self.clone(),
        });
        let approvals = Arc::new(OwnerApprovals {
            hub: self.clone(),
            project_id,
            repository: config.repository.clone(),
            forge: forge.clone(),
        });
        let deps = Dependencies {
            intake: intake.clone(),
            agents: Arc::new(SandboxedAgents {
                test_upstream: None,
                config: config.clone(),
                limits: config.limits.clone(),
            }),
            tests: Arc::new(SandboxedTests {
                shim: config.shim.clone(),
                scratch: config.state.clone(),
                cargo_cache: config.cargo_cache.clone(),
                npm_cache: config.npm_cache.clone(),
                limits: config.limits.clone(),
            }),
            forge,
            approvals,
            releases: Arc::new(intake),
        };
        let worker = format!("fixer-{}", self.inner.host_id);
        let mut orchestrator = match Orchestrator::open(
            &config.state,
            config.mirror.clone(),
            worker,
            config.budgets.clone(),
            deps,
            build,
        ) {
            Ok(o) => o,
            Err(_) => {
                tracing::warn!("Report pipeline stays off: state is unavailable");
                return;
            }
        };
        orchestrator.set_recovery(config.recovery.clone());
        if config.paused && orchestrator.set_local_pause(true).is_err() {
            tracing::warn!("Report pipeline stays off: pause state is unavailable");
            return;
        }
        self.inner
            .reports
            .fixer
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let mut shutdown = self.inner.shutdown.subscribe();
        self.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(config.poll_seconds));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! { biased; _ = shutdown.changed() => break, _ = tick.tick() => {} }
                tokio::select! { biased; _ = shutdown.changed() => break, result = orchestrator.step() => { if result.is_err() { tracing::warn!("Report pipeline step unavailable"); } } }
                // Test submissions are checked apart from the active report.
                tokio::select! { biased; _ = shutdown.changed() => break, result = orchestrator.check() => { if result.is_err() { tracing::warn!("Report test check unavailable"); } } }
            }
        });
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use ports::github::GithubError;
    use protocol::ReportId;

    #[derive(Debug)]
    struct NoNetwork;
    #[async_trait::async_trait]
    impl Github for NoNetwork {
        async fn rest(
            &self,
            _: Method,
            _: &str,
            _: Option<Value>,
        ) -> std::result::Result<Value, GithubError> {
            panic!("approval cleanup must be local")
        }
        async fn graphql(&self, _: &str, _: Value) -> std::result::Result<Value, GithubError> {
            panic!("approval cleanup must be local")
        }
    }

    #[tokio::test]
    async fn revoking_an_owner_request_resolves_attention_and_removes_merge_authority() {
        let dir = tempfile::tempdir().unwrap();
        let hub = CompanionHub::builder()
            .data_dir(dir.path().join("hub"))
            .build()
            .await
            .unwrap();
        let approvals = OwnerApprovals {
            hub: hub.clone(),
            project_id: ProjectId::new(),
            repository: "example/repository".into(),
            forge: Arc::new(GithubForge {
                github: Arc::new(NoNetwork),
                repository: "example/repository".into(),
                mirror: dir.path().join("mirror"),
                hub: hub.clone(),
            }),
        };
        let id = ReportId::new();
        let view = ApprovalView {
            report_id: id,
            reference: "A7K3QX".into(),
            area: report_pipeline::schema::Area::Work,
            test: "client/src/repro.test.ts".into(),
            files: 1,
            lines: 1,
            gates: report_pipeline::gates::GateReport::default(),
            conflict: false,
            pr: PullRequest {
                number: 7,
                head: "a".repeat(40),
            },
        };
        approvals.raise(&view).await.unwrap();
        let p = hub.read(|r| r.require::<Pending>(PENDING, id)).unwrap();
        hub.report_fix_tap(
            "example/repository",
            7,
            &view.pr.head,
            &crate::Caller::local_owner(),
        )
        .unwrap();
        approvals.revoke(id).await.unwrap();
        approvals.revoke(id).await.unwrap();
        hub.read(|r| {
            assert!(r.get::<Pending>(PENDING, id)?.is_none());
            assert!(r.get::<MergeTap>(TAP, "example/repository/7")?.is_none());
            assert!(
                r.require::<AttentionItem>("attention", p.attention_id)?
                    .resolved_at
                    .is_some()
            );
            assert_eq!(r.require::<Task>("task", p.task_id)?.attention_count, 0);
            Ok(())
        })
        .unwrap();
        assert!(
            hub.check_report_fix_merge("example", "repository", 7, &view.pr.head)
                .await
                .is_err()
        );
    }
}
