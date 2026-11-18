// SPDX-License-Identifier: Apache-2.0
//! Library check approvals are admitted with the command, never by an agent tool.
use crate::{CompanionHub, api::Caller, error};
use protocol::{
    auth::Principal,
    checks::*,
    command::CommandResult,
    model::{AttentionItem, AttentionKind, Project, Task, TaskState},
    *,
};
use std::{collections::HashSet, path::Path};

impl CompanionHub {
    pub(crate) fn checks_query(&self, query: Query) -> Result<QueryData, ProtocolError> {
        self.read(|r| match query {
            Query::ChecksList { project_id } => {
                let projects: Vec<Project> = match project_id {
                    Some(id) => vec![r.require("project", id)?],
                    None => r.list("project")?,
                };
                let mut views = Vec::new();
                let mut projects_without_checks = 0;
                for project in projects {
                    let approved = r
                        .project_checks(project.project_id)?
                        .unwrap_or_else(|| store::checks::empty_checks(project.project_id));
                    if approved.checks.is_empty() {
                        projects_without_checks += 1;
                    }
                    let drafts = r
                        .checks_drafts(project.project_id)?
                        .into_iter()
                        .filter(|d| {
                            matches!(
                                d.state,
                                ChecksDraftState::Proving
                                    | ChecksDraftState::Ready
                                    | ChecksDraftState::Stale
                            )
                        })
                        .map(|d| draft_view(r, d))
                        .collect::<store::Result<Vec<_>>>()?;
                    if project_id.is_none() && approved.checks.is_empty() && drafts.is_empty() {
                        continue;
                    }
                    let jobs = r.project_check_jobs(project.project_id)?;
                    // Prefer the unfinished job holding the writer, including standalone
                    // checks and after-run jobs. Otherwise use the newest unfinished job;
                    // project_check_jobs returns newest first, regardless of purpose.
                    let unfinished_jobs = || {
                        jobs.iter().filter(|job| {
                            matches!(
                                job.state,
                                CheckJobState::Pending
                                    | CheckJobState::Running
                                    | CheckJobState::Cancelling
                                    | CheckJobState::Uncertain
                            )
                        })
                    };
                    let unfinished_job = unfinished_jobs()
                        .find(|job| {
                            project.active_writer.is_some_and(|writer| {
                                writer == crate::check_execution::job_writer(job.id)
                                    || Some(writer) == job.run_id
                            })
                        })
                        .or_else(|| unfinished_jobs().next())
                        .cloned();
                    let checks = approved
                        .checks
                        .iter()
                        .map(|check| {
                            // Proofs never replace the last result of an approved check.
                            let last = jobs
                                .iter()
                                .filter(|job| {
                                    !matches!(job.purpose, CheckJobPurpose::DraftProof { .. })
                                })
                                .find_map(|job| {
                                    job.results
                                        .iter()
                                        .find(|result| result.check_id == check.check_id)
                                        .map(|result| (job, result))
                                });
                            let last_task_id = last.and_then(|(job, _)| job.task_id);
                            let last_task_title = last_task_id
                                .map(|id| r.get::<Task>("task", id))
                                .transpose()?
                                .flatten()
                                .map(|task| task.title);
                            Ok(CheckView {
                                check: check.clone(),
                                facts: store::checks::check_facts(check),
                                last_result: last.map(|(_, result)| result.clone()),
                                last_task_id,
                                last_task_title,
                                last_finished_at: last.and_then(|(job, _)| job.finished_at),
                                inputs_changed: store::checks::changed_inputs(
                                    Path::new(&project.host_path),
                                    &approved,
                                    check,
                                )?,
                            })
                        })
                        .collect::<store::Result<Vec<_>>>()?;
                    views.push(ProjectChecksView {
                        project_id: project.project_id,
                        project_name: project.name,
                        approved,
                        checks,
                        drafts,
                        unfinished_job,
                    });
                }
                Ok(QueryData::ChecksList {
                    projects: views,
                    projects_without_checks,
                })
            }
            Query::ChecksHistory { project_id } => {
                let _: Project = r.require("project", project_id)?;
                Ok(QueryData::ChecksHistory {
                    versions: r.checks_history(project_id)?,
                })
            }
            Query::TaskChecks { task_id } => {
                let _: Task = r.require("task", task_id)?;
                let drafts = r
                    .task_checks_draft(task_id)?
                    .map(|draft| draft_view(r, draft))
                    .transpose()?
                    .into_iter()
                    .collect();
                Ok(QueryData::TaskChecks {
                    jobs: r.task_check_jobs(task_id)?,
                    drafts,
                })
            }
            Query::CheckJob { job_id } => Ok(QueryData::CheckJob {
                job: r.require(store::checks::JOBS, job_id)?,
            }),
            _ => Err(error(
                ErrorCode::InvalidRequest,
                "This is not a request about checks.",
            )
            .into()),
        })
    }

    pub(crate) fn checks_change(
        &self,
        tx: &mut store::Write<'_>,
        caller: &Caller,
        command: &Command,
    ) -> store::Result<CommandResult> {
        let device_id = match caller.principal {
            Principal::Device { device_id } => Some(device_id),
            _ => None,
        };
        match command {
            Command::ChecksSave {
                project_id,
                base_version,
                edit,
            } => {
                let project: Project = tx.read().require("project", project_id)?;
                let approved = tx.change_checks(
                    *project_id,
                    *base_version,
                    edit.clone(),
                    device_id,
                    Path::new(&project.host_path),
                )?;
                let job_id = match edit {
                    CheckEdit::Save { check } => {
                        // A saved definition is approved even if its proof fails. Execution
                        // is handed to the lifecycle worker through this durable pending job.
                        let check = approved
                            .checks
                            .iter()
                            .find(|c| c.check_id == check.check_id)
                            .expect("saved check")
                            .clone();
                        Some(
                            pending_job(
                                tx,
                                &project,
                                &approved,
                                vec![check],
                                self.inner.clock.now(),
                            )?
                            .id,
                        )
                    }
                    _ => None,
                };
                Ok(CommandResult::ChecksSaved { approved, job_id })
            }
            Command::ChecksApplyDraft {
                draft_id,
                base_version,
                keep_items,
            } => {
                let draft: ChecksDraft = tx.read().require(store::checks::DRAFTS, draft_id)?;
                if matches!(
                    draft.state,
                    ChecksDraftState::Applied | ChecksDraftState::Discarded
                ) {
                    return Err(error(
                        ErrorCode::AlreadyResolved,
                        "This suggestion has already been used or discarded.",
                    )
                    .into());
                }
                let approved = tx
                    .read()
                    .project_checks(draft.project_id)?
                    .unwrap_or_else(|| store::checks::empty_checks(draft.project_id));
                if *base_version != draft.base_version || *base_version != approved.version {
                    return Err(error(ErrorCode::StaleRevision, "These checks changed since you opened them. Open the suggestions again before using them.").into());
                }
                let mut unique = HashSet::new();
                let ids = keep_items
                    .iter()
                    .map(|index| {
                        if !unique.insert(*index) {
                            return Err(error(
                                ErrorCode::InvalidRequest,
                                "Choose each suggested change only once.",
                            )
                            .into());
                        }
                        draft
                            .items
                            .get(*index as usize)
                            .map(item_id)
                            .ok_or_else(|| {
                                error(
                                    ErrorCode::InvalidRequest,
                                    "That suggested change is no longer available.",
                                )
                                .into()
                            })
                    })
                    .collect::<store::Result<Vec<_>>>()?;
                let project: Project = tx.read().require("project", draft.project_id)?;
                let approved = tx.apply_checks_draft(
                    *draft_id,
                    &ids,
                    device_id,
                    Path::new(&project.host_path),
                )?;
                Ok(CommandResult::ChecksSaved {
                    approved,
                    job_id: None,
                })
            }
            Command::ChecksDiscardDraft { draft_id } => Ok(CommandResult::ChecksDraft {
                draft: tx.discard_checks_draft(*draft_id)?,
            }),
            Command::ChecksRunNow {
                project_id,
                check_ids,
            } => {
                let project: Project = tx.read().require("project", project_id)?;
                require_idle(&tx.read(), &project)?;
                let approved = tx
                    .read()
                    .project_checks(*project_id)?
                    .unwrap_or_else(|| store::checks::empty_checks(*project_id));
                let mut unique = HashSet::new();
                let checks = if check_ids.is_empty() {
                    approved
                        .checks
                        .iter()
                        .filter(|c| c.enabled && c.when == CheckWhen::EveryTask)
                        .cloned()
                        .collect()
                } else {
                    check_ids
                        .iter()
                        .map(|id| {
                            if !unique.insert(*id) {
                                return Err(error(
                                    ErrorCode::InvalidRequest,
                                    "Choose each check only once.",
                                )
                                .into());
                            }
                            approved
                                .checks
                                .iter()
                                .find(|c| c.check_id == *id)
                                .cloned()
                                .ok_or_else(|| {
                                    error(
                                        ErrorCode::NotFound,
                                        "That check is no longer in this project.",
                                    )
                                    .into()
                                })
                        })
                        .collect::<store::Result<Vec<_>>>()?
                };
                if checks.is_empty() {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "There are no checks to run yet. Set up checks first.",
                    )
                    .into());
                }
                Ok(CommandResult::CheckJob {
                    job: pending_job(tx, &project, &approved, checks, self.inner.clock.now())?,
                })
            }
            Command::ChecksStopJob {
                job_id,
                confirmed_stopped,
            } => {
                let mut job: CheckJob = tx.read().require(store::checks::JOBS, job_id)?;
                if matches!(job.purpose, CheckJobPurpose::AfterRun) && !*confirmed_stopped {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Stop this task to stop the checks of its work.",
                    )
                    .into());
                }
                if *confirmed_stopped {
                    if job.state != CheckJobState::Uncertain {
                        return Err(error(
                            ErrorCode::Conflict,
                            "Only an uncertain check run needs your confirmation that it stopped.",
                        )
                        .into());
                    }
                    let inputs: std::collections::BTreeMap<CheckId, Vec<String>> = tx
                        .read()
                        .get("check_job_inputs", job.id)?
                        .unwrap_or_default();
                    for check in &job.checks {
                        if !job
                            .results
                            .iter()
                            .any(|result| result.check_id == check.check_id)
                        {
                            job.results.push(CheckResult {
                                check_id: check.check_id,
                                name: check.name.clone(),
                                outcome: CheckOutcome::CouldNotRun {
                                    reason:
                                        "Not checked: the person resolved an uncertain check run"
                                            .into(),
                                },
                                duration_secs: 0.0,
                                exit_code: None,
                                output: String::new(),
                                inputs_changed: inputs
                                    .get(&check.check_id)
                                    .cloned()
                                    .unwrap_or_default(),
                            });
                        }
                    }
                    let now = self.inner.clock.now();
                    job.state = CheckJobState::Finished;
                    job.process = None;
                    job.finished_at = Some(now);
                    job.message =
                        Some("Not checked: the person resolved an uncertain check run".into());
                    tx.put_check_job(&job)?;
                    tx.record_check_durations(job.id)?;
                    tx.put("check_human_resolution", job.id, &serde_json::json!({ "at": now, "by": caller.principal, "confirmed_stopped": true }))?;
                    if let CheckJobPurpose::DraftProof { draft_id } = job.purpose
                        && tx
                            .read()
                            .get::<ChecksDraft>(store::checks::DRAFTS, draft_id)?
                            .is_some_and(|draft| {
                                matches!(
                                    draft.state,
                                    ChecksDraftState::Proving | ChecksDraftState::Stale
                                )
                            })
                    {
                        tx.finish_checks_proof(draft_id, job.results.clone())?;
                    }
                    if let Some(task_id) = job.task_id {
                        for mut item in tx.read().list::<AttentionItem>("attention")? {
                            if item.task_id == Some(task_id)
                                && item.run_id == job.run_id
                                && item.kind == AttentionKind::ReconciliationRequired
                                && item.resolved_at.is_none()
                            {
                                item.resolved_at = Some(now);
                                tx.put("attention", item.attention_id, &item)?;
                                tx.emit(Event::AttentionResolved {
                                    attention_id: item.attention_id,
                                })?;
                            }
                        }
                        let mut task: Task = tx.read().require("task", task_id)?;
                        // A legacy stale recovery observation could have raised
                        // uncertainty after completion already released the slot.
                        // Confirming the last job resolves that false task state too.
                        if task.state == TaskState::ReconciliationRequired
                            && task.checking.is_none()
                            && tx
                                .read()
                                .task_check_jobs(task_id)?
                                .iter()
                                .all(|job| job.state == CheckJobState::Finished)
                            && let Some(run_id) =
                                job.run_id.filter(|id| task.current_run == Some(*id))
                            && let Some(run) =
                                tx.read().get::<protocol::model::Run>("run", run_id)?
                            && run.state.is_terminal()
                            && crate::runs::blocking_attention_count(&tx.read(), task_id)? == 0
                        {
                            task.state = match run.state {
                                protocol::model::RunState::Completed => TaskState::Completed,
                                protocol::model::RunState::Failed => TaskState::Failed,
                                _ => TaskState::Interrupted,
                            };
                            task.attention_count = 0;
                            task.revision += 1;
                            task.updated_at = now;
                            tx.put("task", task_id, &task)?;
                            tx.emit(Event::TaskUpserted { task })?;
                        }
                    }
                    if job.task_id.is_none() {
                        self.release_writer(
                            tx,
                            job.project_id,
                            crate::check_execution::job_writer(job.id),
                        )?;
                    }
                    // Task-bound jobs hand over through the post-commit callback.
                    return Ok(CommandResult::CheckJob { job });
                }
                let project: Project = tx.read().require("project", job.project_id)?;
                let owns_slot =
                    project.active_writer == Some(crate::check_execution::job_writer(job.id));
                let has_launch_record = tx
                    .read()
                    .get::<serde_json::Value>(crate::check_execution::PROCESSES, job.id)?
                    .is_some();
                match job.state {
                    CheckJobState::Pending
                        if job.process.is_none() && !owns_slot && !has_launch_record =>
                    {
                        job.state = CheckJobState::Finished;
                        job.finished_at = Some(self.inner.clock.now());
                        job.message = Some("Not checked: stopped before starting.".into());
                        let inputs: std::collections::BTreeMap<CheckId, Vec<String>> = tx
                            .read()
                            .get("check_job_inputs", job.id)?
                            .unwrap_or_default();
                        for check in &job.checks {
                            if !job
                                .results
                                .iter()
                                .any(|result| result.check_id == check.check_id)
                            {
                                job.results.push(CheckResult {
                                    check_id: check.check_id,
                                    name: check.name.clone(),
                                    outcome: CheckOutcome::Stopped,
                                    duration_secs: 0.0,
                                    exit_code: None,
                                    output: String::new(),
                                    inputs_changed: inputs
                                        .get(&check.check_id)
                                        .cloned()
                                        .unwrap_or_default(),
                                });
                            }
                        }
                        if let CheckJobPurpose::DraftProof { draft_id } = job.purpose
                            && tx
                                .read()
                                .get::<ChecksDraft>(store::checks::DRAFTS, draft_id)?
                                .is_some_and(|draft| {
                                    matches!(
                                        draft.state,
                                        ChecksDraftState::Proving | ChecksDraftState::Stale
                                    )
                                })
                        {
                            tx.finish_checks_proof(draft_id, job.results.clone())?;
                        }
                    }
                    CheckJobState::Pending | CheckJobState::Running => {
                        job.state = CheckJobState::Cancelling;
                        job.message = Some(
                            "Stopping checks. The project stays reserved until they have stopped."
                                .into(),
                        );
                    }
                    CheckJobState::Uncertain => {
                        job.message = Some("Ophio cannot confirm these checks stopped. Check the computer, then explicitly confirm they are no longer running.".into());
                    }
                    CheckJobState::Cancelling | CheckJobState::Finished => {}
                }
                // Lifecycle consumes Cancelling and confirms actual process termination.
                tx.put_check_job(&job)?;
                if job.state == CheckJobState::Finished {
                    tx.record_check_durations(job.id)?;
                }
                Ok(CommandResult::CheckJob { job })
            }
            _ => Err(error(ErrorCode::InvalidRequest, "This is not a change to checks.").into()),
        }
    }
}

fn item_id(item: &CheckDraftItem) -> CheckId {
    match item {
        CheckDraftItem::Add { check, .. } | CheckDraftItem::Change { check, .. } => check.check_id,
        CheckDraftItem::Remove { check_id, .. } => *check_id,
    }
}

fn draft_view(r: &store::Read<'_>, draft: ChecksDraft) -> store::Result<ChecksDraftView> {
    let base = r
        .checks_history(draft.project_id)?
        .into_iter()
        .find(|v| v.version == draft.base_version)
        .unwrap_or_else(|| store::checks::empty_checks(draft.project_id));
    let differences = draft
        .items
        .iter()
        .map(|item| match item {
            CheckDraftItem::Add { .. } => vec!["Adds a new check.".into()],
            CheckDraftItem::Change { check, .. } => base
                .checks
                .iter()
                .find(|old| old.check_id == check.check_id)
                .map(|old| store::checks::check_differences(old, check))
                .unwrap_or_else(|| {
                    vec!["The approved checks have changed. Ask for this suggestion again.".into()]
                }),
            CheckDraftItem::Remove { .. } => vec!["Removes this check.".into()],
        })
        .collect();
    let facts = draft
        .items
        .iter()
        .map(|item| match item {
            CheckDraftItem::Add { check, .. } | CheckDraftItem::Change { check, .. } => {
                Some(store::checks::check_facts(check))
            }
            CheckDraftItem::Remove { .. } => None,
        })
        .collect();
    Ok(ChecksDraftView {
        draft,
        differences,
        facts,
    })
}

fn require_idle(r: &store::Read<'_>, project: &Project) -> store::Result<()> {
    let pending = r
        .project_check_jobs(project.project_id)?
        .iter()
        .any(|job| job.state != CheckJobState::Finished);
    if project.active_writer.is_some() || pending {
        return Err(error(ErrorCode::Conflict, "This project is busy. Wait for its work and checks to finish before running a check now.").into());
    }
    Ok(())
}

fn pending_job(
    tx: &mut store::Write<'_>,
    project: &Project,
    approved: &ProjectChecks,
    checks: Vec<Check>,
    now: protocol::time::Timestamp,
) -> store::Result<CheckJob> {
    let job = CheckJob {
        id: CheckJobId::new(),
        project_id: project.project_id,
        task_id: None,
        run_id: None,
        purpose: CheckJobPurpose::RunNow {
            check_ids: checks.iter().map(|check| check.check_id).collect(),
        },
        checks_version: approved.version,
        checks,
        state: CheckJobState::Pending,
        process: None,
        created_at: now,
        started_at: None,
        finished_at: None,
        sandbox: None,
        results: vec![],
        message: None,
    };
    let inputs = job
        .checks
        .iter()
        .map(|check| {
            Ok((
                check.check_id,
                store::checks::changed_inputs(Path::new(&project.host_path), approved, check)?,
            ))
        })
        .collect::<store::Result<std::collections::BTreeMap<_, _>>>()?;
    tx.put("check_job_inputs", job.id, &inputs)?;
    tx.put_check_job(&job)?;
    // The service dispatcher takes the writer slot before launching this frozen job.
    Ok(job)
}
