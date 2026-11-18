// SPDX-License-Identifier: Apache-2.0
//! Snapshot-based admission and uninterrupted writer ownership across task checks.
use crate::{CompanionHub, error};
use check_runner::snapshot::{DEFAULT_RETENTION_DAYS, Manifest, SnapshotDiff, SnapshotStore};
use protocol::{
    CheckId, CheckJobId, ErrorCode, Event, ProtocolError, RunId,
    checks::*,
    model::{Project, ReportStatus, Run, RunOutcome, RunState, Task, TaskMode, TaskState},
    queue::QueueView,
    time::Timestamp,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, OnceLock, Weak},
};

pub(crate) const FINALIZING: &str = "check_finalizing";
pub(crate) const DEFERRED_END: &str = "check_deferred_end";
pub(crate) const HELPERS_END: &str = "check_helpers_end";
pub(crate) const JOB_INPUTS: &str = "check_job_inputs";
const SNAPSHOTS: &str = "check_run_snapshots";
const STOPPED: &str = "Not checked: stopped";

/// Original run completion, replayed only after all reserved check work ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DeferredEnd {
    pub run_id: RunId,
    pub state: RunState,
    pub outcome: Option<RunOutcome>,
    pub helpers_running: bool,
    pub why: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeferredChecks {
    end: DeferredEnd,
    job_ids: Vec<CheckJobId>,
}

/// Manifest contents and blobs stay in the private snapshot store. Documents
/// only link the two captures, and retain failures instead of claiming no change.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RunSnapshots {
    before: Option<String>,
    after: Option<String>,
    error: Option<String>,
    #[serde(default)]
    prepared: Option<PreparedChecks>,
}

/// Only the small admission decision is kept in SQLite, not a 50,000-file diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PreparedChecks {
    version: u64,
    changed: bool,
    results: Vec<CheckResult>,
    inputs: BTreeMap<CheckId, Vec<String>>,
}

type CaptureLocks = Mutex<BTreeMap<RunId, Weak<tokio::sync::Mutex<()>>>>;

fn capture_lock(run_id: RunId) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<CaptureLocks> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&run_id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(run_id, Arc::downgrade(&lock));
    lock
}

impl CompanionHub {
    pub(crate) async fn capture_checks_start(&self, run_id: RunId) -> Result<(), ProtocolError> {
        let lock = capture_lock(run_id);
        let _guard = lock.lock().await;
        let project = self.read(|r| {
            let Some((_, task, project)) = self.checks_capture_owner(r, run_id)? else {
                return Ok(None);
            };
            if r.get::<RunSnapshots>(SNAPSHOTS, run_id)?.is_some()
                || r.project_checks(task.project_id)?
                    .is_none_or(|approved| approved.checks.is_empty())
            {
                return Ok(None);
            }
            Ok(Some(project))
        })?;
        let Some(project) = project else {
            return Ok(());
        };
        let data = self.inner.data.join("check-snapshots");
        let record = tokio::task::spawn_blocking(move || {
            let captured = SnapshotStore::new(data).and_then(|snapshots| {
                snapshots.gc(DEFAULT_RETENTION_DAYS)?;
                snapshots.capture(&project.host_path, None)
            });
            match captured {
                Ok(manifest) => RunSnapshots {
                    before: Some(manifest.id),
                    ..Default::default()
                },
                Err(cause) => RunSnapshots {
                    error: Some(cause.to_string()),
                    ..Default::default()
                },
            }
        })
        .await
        .map_err(|_| {
            error(
                ErrorCode::Internal,
                "The file snapshot worker stopped unexpectedly.",
            )
        })?;
        self.write(|tx| {
            // Dispatch retries must not replace a capture made before Work.
            if self.checks_capture_owner(&tx.read(), run_id)?.is_some()
                && tx.read().get::<RunSnapshots>(SNAPSHOTS, run_id)?.is_none()
            {
                tx.put(SNAPSHOTS, run_id, &record)?;
            }
            Ok(())
        })
    }

    /// Called before normal end-run mutations, while the original writer is
    /// still reserved. Job creation, task progress and ownership commit together.
    pub(crate) fn defer_run_checks(
        &self,
        tx: &mut store::Write<'_>,
        run: &mut Run,
        task: &mut Task,
        end: DeferredEnd,
    ) -> store::Result<bool> {
        let Some(end) = self.checks_end_eligible(&tx.read(), run, task, end)? else {
            return Ok(false);
        };
        let project: Project = tx.read().require("project", task.project_id)?;
        let queue: QueueView = tx.read().require("queue", task.project_id)?;
        if project.active_writer != Some(run.run_id) || queue.writer != Some(run.run_id) {
            return Ok(false);
        }
        if tx
            .read()
            .get::<DeferredChecks>(DEFERRED_END, task.task_id)?
            .is_some()
        {
            return Ok(false);
        }
        let approved = tx
            .read()
            .project_checks(task.project_id)?
            .unwrap_or_else(|| store::checks::empty_checks(task.project_id));
        let draft = tx
            .read()
            .task_checks_draft(task.task_id)?
            .filter(|draft| draft.state == ChecksDraftState::Proving);
        let now = self.inner.clock.now();
        let mut jobs = Vec::new();
        if approved.checks.iter().any(|check| check.enabled) {
            let record: RunSnapshots = tx.read().get(SNAPSHOTS, run.run_id)?.unwrap_or_default();
            let prepared = record
                .prepared
                .filter(|prepared| prepared.version == approved.version)
                .ok_or_else(|| {
                    store::Error::from(error(
                        ErrorCode::Conflict,
                        "The file record must be prepared before this task can finish. Please retry.",
                    ))
                })?;
            if prepared.changed {
                let mut job = job_for(
                    task,
                    run.run_id,
                    approved.version,
                    approved.checks.clone(),
                    CheckJobPurpose::AfterRun,
                    now,
                );
                job.message = record.error.map(|cause| format!("File changes could not be fully recorded; checks were selected conservatively. {cause}"));
                job.results = prepared.results;
                tx.put(JOB_INPUTS, job.id, &prepared.inputs)?;
                jobs.push(job);
            }
        }
        // Proposals never enter an AfterRun job. A proof is explicit, isolated,
        // and required even for a first setup task or a run with no file changes.
        if let Some(draft) = draft {
            let candidates = draft
                .items
                .into_iter()
                .filter_map(|item| match item {
                    CheckDraftItem::Add { check, .. } | CheckDraftItem::Change { check, .. } => {
                        Some(check)
                    }
                    CheckDraftItem::Remove { .. } => None,
                })
                .collect();
            jobs.push(job_for(
                task,
                run.run_id,
                draft.base_version,
                candidates,
                CheckJobPurpose::DraftProof { draft_id: draft.id },
                now,
            ));
        }
        let Some(first) = jobs.first() else {
            return Ok(false);
        };
        let first_id = first.id;
        let checking = progress(first, now);
        let deferred = DeferredChecks {
            end: end.clone(),
            job_ids: jobs.iter().map(|job| job.id).collect(),
        };
        for job in &jobs {
            tx.put_check_job(job)?;
        }
        tx.put(DEFERRED_END, task.task_id, &deferred)?;
        transfer_writer(tx, task.project_id, run.run_id, RunId(first_id.0))?;
        run.state = end.state;
        run.ended_at = Some(now);
        run.outcome = end.outcome;
        run.holds_writer = false;
        // Older phones recognize a terminal run with pending cleanup as still
        // stoppable. The check job, not this run, owns the actual writer.
        run.cleanup_pending = true;
        tx.put("run", run.run_id, run)?;
        tx.emit(Event::RunUpserted {
            run: Box::new(run.clone()),
        })?;
        task.state = TaskState::Working;
        task.checking = Some(checking);
        save_task(tx, task, now)?;
        Ok(true)
    }

    /// Call only after agent processes have ended. Filesystem work, selection
    /// and input hashing happen on a blocking worker before completion's write.
    pub(crate) async fn prepare_checks_end(&self, end: &DeferredEnd) -> Result<(), ProtocolError> {
        let hub = self.clone();
        let end = end.clone();
        let lock = capture_lock(end.run_id);
        tokio::task::spawn_blocking(move || {
            let _guard = lock.blocking_lock();
            let context = hub.read(|read| {
                let Some((run, task, project)) = hub.checks_capture_owner(read, end.run_id)? else {
                    return Ok(None);
                };
                // Completion first flushes the final native reply. Its report
                // may differ from the previous stored reply, so only completion
                // decides whether that report permits a check job.
                if !hub.checks_end_can_prepare(read, &run, &task, &end)? {
                    return Ok(None);
                }
                let Some(approved) = read.project_checks(task.project_id)? else {
                    return Ok(None);
                };
                if !approved.checks.iter().any(|check| check.enabled) {
                    return Ok(None);
                }
                let record: RunSnapshots = read.get(SNAPSHOTS, end.run_id)?.unwrap_or_default();
                if record.prepared.as_ref().is_some_and(|ready| ready.version == approved.version) {
                    return Ok(None);
                }
                Ok(Some((project, approved, record)))
            })?;
            let Some((project, approved, mut record)) = context else {
                return Ok(());
            };
            let pending = hub.inner.data.join("check-snapshots").join("prepared-ends");
            std::fs::create_dir_all(&pending).map_err(preparation_error)?;
            let pending = pending.join(format!("{}.json", end.run_id));
            // A filesystem journal survives a transient SQLite failure. Retrying
            // must reuse the same end, not replace it with later project contents.
            match std::fs::read(&pending) {
                Ok(bytes) if record.after.is_none() => {
                    let saved: RunSnapshots = serde_json::from_slice(&bytes).map_err(preparation_error)?;
                    if saved.before != record.before {
                        return Err(error(ErrorCode::Conflict, "The start-of-run file record changed. The task was not finished."));
                    }
                    record = saved;
                }
                Ok(_) => {}
                Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
                Err(cause) => return Err(preparation_error(cause)),
            }
            if record.prepared.as_ref().is_none_or(|ready| ready.version != approved.version) {
                let diff = hub.capture_checks_end_record(&mut record, &project);
                record.prepared = Some(prepare_selection(&project, &approved, &diff));
            }
            let temporary = pending.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
            let saved = (|| {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
                serde_json::to_writer(&mut file, &record)?;
                file.flush()?;
                file.sync_all()?;
                std::fs::rename(&temporary, &pending)
            })();
            if let Err(cause) = saved {
                let _ = std::fs::remove_file(&temporary);
                return Err(preparation_error(cause));
            }
            hub.write(|tx| {
                let read = tx.read();
                let Some((run, task, _)) = hub.checks_capture_owner(&read, end.run_id)? else {
                    return Ok(());
                };
                if !hub.checks_end_can_prepare(&read, &run, &task, &end)? {
                    return Ok(());
                }
                if read.project_checks(task.project_id)?.is_none_or(|current| current.version != approved.version) {
                    return Err(error(ErrorCode::Conflict, "The approved checks changed while the file record was prepared. Please retry.").into());
                }
                tx.put(SNAPSHOTS, end.run_id, &record)
            })?;
            // On database failure the journal stays for the next exit attempt.
            let _ = std::fs::remove_file(pending);
            Ok(())
        })
        .await
        .map_err(|_| error(ErrorCode::Internal, "The file snapshot worker stopped unexpectedly."))?
    }

    fn capture_checks_end_record(
        &self,
        record: &mut RunSnapshots,
        project: &Project,
    ) -> SnapshotDiff {
        let captured =
            SnapshotStore::new(self.inner.data.join("check-snapshots")).and_then(|snapshots| {
                let before: Option<Manifest> = record
                    .before
                    .as_deref()
                    .map(|id| snapshots.load_manifest(id))
                    .transpose()?;
                let after = match record.after.as_deref() {
                    Some(id) => snapshots.load_manifest(id)?,
                    None => snapshots.capture(&project.host_path, before.as_ref())?,
                };
                record.after = Some(after.id.clone());
                before.map(|before| before.diff(&after)).transpose()
            });
        match captured {
            Ok(Some(diff)) => diff,
            Ok(None) => {
                record
                    .error
                    .get_or_insert_with(|| "The start-of-run file record is unavailable.".into());
                unknown_diff()
            }
            Err(cause) => {
                record.error = Some(cause.to_string());
                unknown_diff()
            }
        }
    }

    pub(crate) fn defer_helper_checks(
        &self,
        tx: &mut store::Write<'_>,
        run: &mut Run,
        task: &mut Task,
        mut end: DeferredEnd,
    ) -> store::Result<bool> {
        if !end.helpers_running {
            return Ok(false);
        }
        end.helpers_running = false;
        let Some(end) = self.checks_end_eligible(&tx.read(), run, task, end)? else {
            return Ok(false);
        };
        let approved = tx.read().project_checks(task.project_id)?;
        let proof = tx
            .read()
            .task_checks_draft(task.task_id)?
            .is_some_and(|draft| draft.state == ChecksDraftState::Proving);
        if !proof && approved.is_none_or(|checks| !checks.checks.iter().any(|check| check.enabled))
        {
            return Ok(false);
        }
        tx.put(HELPERS_END, run.run_id, &end)?;
        run.state = end.state;
        run.ended_at = Some(self.inner.clock.now());
        run.outcome = end.outcome;
        run.cleanup_pending = true;
        tx.put("run", run.run_id, run)?;
        tx.emit(Event::RunUpserted {
            run: Box::new(run.clone()),
        })?;
        task.state = TaskState::Working;
        save_task(tx, task, self.inner.clock.now())?;
        Ok(true)
    }

    pub(crate) fn remove_prepared_checks_end(&self, run_id: RunId) {
        let pending = self
            .inner
            .data
            .join("check-snapshots")
            .join("prepared-ends")
            .join(format!("{run_id}.json"));
        if let Err(cause) = std::fs::remove_file(pending)
            && cause.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%run_id, %cause, "removing abandoned prepared check ending failed");
        }
    }

    fn checks_capture_owner(
        &self,
        read: &store::Read<'_>,
        run_id: RunId,
    ) -> store::Result<Option<(Run, Task, Project)>> {
        let run: Run = read.require("run", run_id)?;
        let task: Task = read.require("task", run.task_id)?;
        let helpers_done =
            read.get::<DeferredEnd>(HELPERS_END, run_id)?.is_some() && !run.cleanup_pending;
        if (run.state.is_terminal() && !helpers_done)
            || (run.ended_at.is_some() && !helpers_done)
            || run.cleanup_pending
            || task.current_run != Some(run_id)
            || task.checking.is_some()
            || read.get::<bool>(FINALIZING, run_id)?.unwrap_or(false)
            || read
                .get::<DeferredChecks>(DEFERRED_END, task.task_id)?
                .is_some()
            || !self.checks_work_writer(read, &run, &task)?
        {
            return Ok(None);
        }
        let project: Project = read.require("project", task.project_id)?;
        let queue: QueueView = read.require("queue", task.project_id)?;
        if project.active_writer != Some(run_id) || queue.writer != Some(run_id) {
            return Ok(None);
        }
        Ok(Some((run, task, project)))
    }

    fn checks_end_can_prepare(
        &self,
        read: &store::Read<'_>,
        run: &Run,
        task: &Task,
        end: &DeferredEnd,
    ) -> store::Result<bool> {
        Ok(end.run_id == run.run_id
            && self.checks_work_writer(read, run, task)?
            && matches!(end.state, RunState::Completed | RunState::Failed)
            && (!run.state.is_terminal()
                || read.get::<DeferredEnd>(HELPERS_END, run.run_id)?.is_some())
            && !matches!(
                run.state,
                RunState::Interrupted | RunState::ReconciliationRequired | RunState::Stopping
            )
            && !end.helpers_running
            && !run.cleanup_pending
            && !read.get::<bool>(FINALIZING, run.run_id)?.unwrap_or(false))
    }

    fn checks_end_eligible(
        &self,
        read: &store::Read<'_>,
        run: &Run,
        task: &Task,
        mut end: DeferredEnd,
    ) -> store::Result<Option<DeferredEnd>> {
        if !self.checks_end_can_prepare(read, run, task, &end)? {
            return Ok(None);
        }
        // Native statements can exclude checks, but are not execution evidence.
        end.outcome = end.outcome.or_else(|| run.outcome.clone());
        let mut effective = end.outcome.clone().unwrap_or_else(empty_outcome);
        crate::reported::keep(read, task.task_id, run.run_id, &mut effective)?;
        if effective.report.as_ref().is_some_and(|report| {
            report.status == ReportStatus::NeedsYou || !report.questions.is_empty()
        }) || crate::carry_on::run_blocked(read, run.run_id)?
        {
            return Ok(None);
        }
        if end.outcome.is_some() || effective.report.is_some() {
            end.outcome = Some(effective);
        }
        Ok(Some(end))
    }

    /// End one job without ever freeing the project between the after-run
    /// checks, its proof, and the original completion transaction.
    pub(crate) fn advance_task_checks(
        &self,
        tx: &mut store::Write<'_>,
        finished_job_id: CheckJobId,
    ) -> store::Result<Option<DeferredEnd>> {
        let Some(finished) = tx.read().check_job(finished_job_id)? else {
            return Ok(None);
        };
        let Some(task_id) = finished.task_id else {
            return Ok(None);
        };
        let Some(mut deferred) = tx.read().get::<DeferredChecks>(DEFERRED_END, task_id)? else {
            return Ok(None);
        };
        if finished.state != CheckJobState::Finished
            || !deferred.job_ids.contains(&finished_job_id)
            || tx
                .read()
                .get::<bool>(FINALIZING, deferred.end.run_id)?
                .unwrap_or(false)
        {
            return Ok(None);
        }
        let mut task: Task = tx.read().require("task", task_id)?;
        let project: Project = tx.read().require("project", task.project_id)?;
        if project.active_writer != Some(RunId(finished_job_id.0)) {
            return Ok(None);
        }
        let mut stopped = false;
        for id in &deferred.job_ids {
            let job: CheckJob = tx.read().require(store::checks::JOBS, id)?;
            stopped |= job.state == CheckJobState::Cancelling
                || job.message.as_deref() == Some(STOPPED)
                || job
                    .results
                    .iter()
                    .any(|result| result.outcome == CheckOutcome::Stopped);
        }
        let now = self.inner.clock.now();
        if stopped {
            for id in deferred.job_ids.iter().filter(|id| **id != finished_job_id) {
                let mut job: CheckJob = tx.read().require(store::checks::JOBS, id)?;
                if job.state == CheckJobState::Finished {
                    continue;
                }
                // A launch record is committed before spawn. Running without
                // either identity or launch record has never started, just as
                // Cancelling without either record has never started.
                if job.process.is_some()
                    || tx
                        .read()
                        .get::<serde_json::Value>(crate::check_execution::PROCESSES, job.id)?
                        .is_some()
                    || job.state == CheckJobState::Uncertain
                {
                    return Ok(None);
                }
                for check in &job.checks {
                    if !job
                        .results
                        .iter()
                        .any(|result| result.check_id == check.check_id)
                    {
                        job.results.push(result_for(check, CheckOutcome::Stopped));
                    }
                }
                job.state = CheckJobState::Finished;
                job.finished_at = Some(now);
                job.message = Some(STOPPED.into());
                tx.put_check_job(&job)?;
                if let CheckJobPurpose::DraftProof { draft_id } = job.purpose
                    && tx.read().checks_draft(draft_id)?.is_some_and(|draft| {
                        matches!(
                            draft.state,
                            ChecksDraftState::Proving | ChecksDraftState::Stale
                        )
                    })
                {
                    tx.finish_checks_proof(draft_id, job.results.clone())?;
                }
            }
            deferred.end.why = Some(match deferred.end.why.take() {
                Some(why) if !why.contains(STOPPED) => format!("{why}\n\n{STOPPED}"),
                Some(why) => why,
                None => STOPPED.into(),
            });
        } else {
            for id in &deferred.job_ids {
                let mut job: CheckJob = tx.read().require(store::checks::JOBS, id)?;
                if job.state == CheckJobState::Finished {
                    continue;
                }
                if job.state != CheckJobState::Pending || job.process.is_some() {
                    return Ok(None);
                }
                if let CheckJobPurpose::DraftProof { draft_id } = job.purpose
                    && !tx.read().checks_draft(draft_id)?.is_some_and(|draft| {
                        draft.state == ChecksDraftState::Proving && draft.task_id == Some(task_id)
                    })
                {
                    let reason = "The suggested checks were discarded before their proof could run";
                    for check in &job.checks {
                        if !job
                            .results
                            .iter()
                            .any(|result| result.check_id == check.check_id)
                        {
                            job.results.push(result_for(
                                check,
                                CheckOutcome::CouldNotRun {
                                    reason: reason.into(),
                                },
                            ));
                        }
                    }
                    job.state = CheckJobState::Finished;
                    job.finished_at = Some(now);
                    job.message = Some(reason.into());
                    tx.put_check_job(&job)?;
                    continue;
                }
                transfer_writer(
                    tx,
                    task.project_id,
                    RunId(finished_job_id.0),
                    RunId(job.id.0),
                )?;
                task.checking = Some(progress(&job, now));
                save_task(tx, &mut task, now)?;
                return Ok(None);
            }
        }
        let mut run: Run = tx.read().require("run", deferred.end.run_id)?;
        transfer_writer(tx, task.project_id, RunId(finished_job_id.0), run.run_id)?;
        run.holds_writer = true;
        run.cleanup_pending = false;
        tx.put("run", run.run_id, &run)?;
        tx.emit(Event::RunUpserted { run: Box::new(run) })?;
        // Keep Stop routable until the replayed end_run clears this detail.
        tx.put(DEFERRED_END, task_id, &deferred)?;
        tx.put(FINALIZING, deferred.end.run_id, &true)?;
        Ok(Some(deferred.end))
    }

    fn checks_work_writer(
        &self,
        read: &store::Read<'_>,
        run: &Run,
        task: &Task,
    ) -> store::Result<bool> {
        if !run.holds_writer || run.mode.unwrap_or(task.mode) != TaskMode::Work {
            return Ok(false);
        }
        let saved: Option<protocol::access::ConversationAccess> = read
            .get::<Option<protocol::access::ConversationAccess>>("run_access", run.run_id)?
            .flatten();
        for access in task.access.as_ref().into_iter().chain(saved.as_ref()) {
            let reach = self.conversation_reach(read, task, run.run_id, access)?;
            if reach.whole_write {
                continue;
            }
            let project: Project = read.require("project", task.project_id)?;
            let Ok(root) = ports::library::comparison(Path::new(&project.host_path)) else {
                return Ok(false);
            };
            if !reach.write.iter().any(|path| {
                ports::library::comparison(path).is_ok_and(|writable| root.starts_with(writable))
            }) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A crash after returning ownership to the original run must not strand
    /// it behind the usual terminal-run guard on the next companion start.
    pub(crate) fn recoverable_deferred_ends(&self) -> Result<Vec<DeferredEnd>, ProtocolError> {
        self.read(|r| {
            r.list::<DeferredChecks>(DEFERRED_END)?
                .into_iter()
                .filter_map(
                    |deferred| match r.get::<bool>(FINALIZING, deferred.end.run_id) {
                        Ok(Some(true)) => Some(Ok(deferred.end)),
                        Ok(_) => None,
                        Err(cause) => Some(Err(cause)),
                    },
                )
                .collect()
        })
    }
}

fn preparation_error(cause: impl std::fmt::Display) -> ProtocolError {
    error(
        ErrorCode::Internal,
        format!("The end-of-run file record could not be saved. Please retry. {cause}"),
    )
}

fn prepare_selection(
    project: &Project,
    approved: &ProjectChecks,
    diff: &SnapshotDiff,
) -> PreparedChecks {
    let mut prepared = PreparedChecks {
        version: approved.version,
        changed: !diff.is_empty(),
        results: vec![],
        inputs: BTreeMap::new(),
    };
    if !prepared.changed {
        return prepared;
    }
    for check in &approved.checks {
        let changed = store::checks::changed_inputs(Path::new(&project.host_path), approved, check)
            .unwrap_or_else(|_| check.inputs.clone());
        prepared.inputs.insert(check.check_id, changed.clone());
        let outcome = if !check.enabled {
            Some(CheckOutcome::Off)
        } else if check.when != CheckWhen::EveryTask {
            Some(CheckOutcome::SkippedPaths)
        } else {
            match diff.matches_globs(&check.paths) {
                Ok(true) => None,
                Ok(false) => Some(CheckOutcome::SkippedPaths),
                Err(cause) => Some(CheckOutcome::CouldNotRun {
                    reason: cause.to_string(),
                }),
            }
        };
        if let Some(outcome) = outcome {
            let mut result = result_for(check, outcome);
            result.inputs_changed = changed;
            prepared.results.push(result);
        }
    }
    prepared
}

fn empty_outcome() -> RunOutcome {
    RunOutcome {
        summary: None,
        changed_files: vec![],
        checks: vec![],
        still_active: vec![],
        unknowns: vec![],
        usage: None,
        report: None,
        verification: None,
    }
}

fn unknown_diff() -> SnapshotDiff {
    SnapshotDiff {
        changes: BTreeMap::new(),
        incomplete: true,
    }
}

fn job_for(
    task: &Task,
    run_id: RunId,
    version: u64,
    checks: Vec<Check>,
    purpose: CheckJobPurpose,
    now: Timestamp,
) -> CheckJob {
    CheckJob {
        id: CheckJobId::new(),
        project_id: task.project_id,
        task_id: Some(task.task_id),
        run_id: Some(run_id),
        purpose,
        checks_version: version,
        checks,
        state: CheckJobState::Pending,
        process: None,
        created_at: now,
        started_at: None,
        finished_at: None,
        sandbox: None,
        results: vec![],
        message: None,
    }
}

fn result_for(check: &Check, outcome: CheckOutcome) -> CheckResult {
    CheckResult {
        check_id: check.check_id,
        name: check.name.clone(),
        outcome,
        duration_secs: 0.0,
        exit_code: None,
        output: String::new(),
        inputs_changed: vec![],
    }
}

fn progress(job: &CheckJob, now: Timestamp) -> TaskChecking {
    TaskChecking {
        job_id: job.id,
        started_at: now,
        done: job.results.len() as u32,
        total: job.checks.len() as u32,
    }
}

fn save_task(tx: &mut store::Write<'_>, task: &mut Task, now: Timestamp) -> store::Result<()> {
    task.updated_at = now;
    task.revision += 1;
    tx.put("task", task.task_id, task)?;
    tx.emit(Event::TaskUpserted { task: task.clone() })?;
    Ok(())
}

fn transfer_writer(
    tx: &mut store::Write<'_>,
    project_id: protocol::ProjectId,
    from: RunId,
    to: RunId,
) -> store::Result<()> {
    let mut project: Project = tx.read().require("project", project_id)?;
    let mut queue: QueueView = tx.read().require("queue", project_id)?;
    if project.active_writer != Some(from) || queue.writer != Some(from) {
        return Err(error(
            ErrorCode::Conflict,
            "The project's writer changed before its checks could take over.",
        )
        .into());
    }
    project.active_writer = Some(to);
    queue.writer = Some(to);
    crate::runs::bump_queue_revision(tx, &mut queue)?;
    tx.put("project", project_id, &project)?;
    tx.emit(Event::ProjectUpserted { project })?;
    tx.put("queue", project_id, &queue)?;
    tx.emit(Event::QueueChanged { queue })?;
    Ok(())
}
