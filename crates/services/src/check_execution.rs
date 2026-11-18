// SPDX-License-Identifier: Apache-2.0
//! Durable check execution. Only a job holding its project's writer may launch.
use crate::{CompanionHub, error};
use check_runner::runner::{
    self, CheckOutput, CheckRequest, ProcessIdentity, ProcessState, StopStatus,
};
use protocol::checks::CheckResult;
use protocol::{access::ConversationAccess, checks::*, model::*, queue::QueueView, *};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    io,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, watch};

pub(crate) const PROCESSES: &str = "checks_process";

/// This private record retains the boot identity and project omitted on the wire.
/// A launch intent without an identity is not evidence that nothing started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProcessRecord {
    launching: bool,
    identity: Option<ProcessIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    partial: Option<runner::PartialProcessIdentity>,
}

pub(crate) fn job_writer(id: CheckJobId) -> RunId {
    RunId(id.0)
}

struct Control {
    cancel: watch::Sender<bool>,
}

pub(crate) struct Runtime {
    scheduling: AtomicBool,
    recovering: AtomicBool,
    #[cfg(test)]
    pub(crate) paused: AtomicBool,
    #[cfg(test)]
    snapshot_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    pub(crate) finalization_failures: std::sync::atomic::AtomicUsize,
    dirty: AtomicBool,
    active: Mutex<HashMap<CheckJobId, Arc<Control>>>,
    pub(crate) changed: Notify,
    runner: Mutex<Arc<dyn Executor>>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            scheduling: AtomicBool::new(false),
            recovering: AtomicBool::new(true),
            #[cfg(test)]
            paused: AtomicBool::new(false),
            #[cfg(test)]
            snapshot_hook: Mutex::new(None),
            #[cfg(test)]
            finalization_failures: std::sync::atomic::AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
            active: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            runner: Mutex::new(Arc::new(NativeExecutor)),
        }
    }
}

// Keep the service state machine testable without replacing containment code.
struct Execution {
    identity: ProcessIdentity,
    sandbox: CheckSandboxLevel,
    cancel: Box<dyn Fn() + Send + Sync>,
    result: Pin<Box<dyn Future<Output = io::Result<CheckOutput>> + Send>>,
}
#[async_trait::async_trait]
trait Executor: Send + Sync {
    async fn start(&self, request: CheckRequest) -> Result<Execution, runner::StartError>;
    async fn inspect(&self, identity: &ProcessIdentity) -> ProcessState;
    async fn stop(&self, identity: &ProcessIdentity) -> StopStatus;
}
struct NativeExecutor;
#[async_trait::async_trait]
impl Executor for NativeExecutor {
    async fn start(&self, request: CheckRequest) -> Result<Execution, runner::StartError> {
        let running = runner::start(request).await?;
        let cancellation = running.cancellation();
        Ok(Execution {
            identity: running.identity().clone(),
            sandbox: sandbox_level(running.sandbox_level()),
            cancel: Box::new(move || cancellation.cancel()),
            result: Box::pin(running.wait()),
        })
    }
    async fn inspect(&self, identity: &ProcessIdentity) -> ProcessState {
        runner::inspect(identity).await
    }
    async fn stop(&self, identity: &ProcessIdentity) -> StopStatus {
        runner::stop(identity).await
    }
}
fn sandbox_level(level: &runner::SandboxLevel) -> CheckSandboxLevel {
    match level {
        runner::SandboxLevel::Contained => CheckSandboxLevel::Contained,
        runner::SandboxLevel::ContainedWithoutNetworkLimit => {
            CheckSandboxLevel::ContainedWithoutNetworkLimit {
                reason: "This check was allowed to use the internet without limiting which destinations it could reach.".into(),
            }
        }
        runner::SandboxLevel::Unsandboxed { reason } => CheckSandboxLevel::Unsandboxed {
            reason: reason.clone(),
        },
    }
}

impl CompanionHub {
    /// Writes only request a scan. A single dispatcher drains these requests;
    /// its own writes do not recursively create dispatchers.
    pub(crate) fn wake_checks(&self) {
        if *self.inner.shutdown.borrow() || self.inner.checks.recovering.load(Ordering::Acquire) {
            return;
        }
        let runtime = &self.inner.checks;
        runtime.dirty.store(true, Ordering::Release);
        if runtime.scheduling.swap(true, Ordering::AcqRel) {
            return;
        }
        let hub = self.clone();
        self.spawn(async move {
            loop {
                hub.inner.checks.dirty.store(false, Ordering::Release);
                if let Err(e) = hub.schedule_check_jobs() {
                    tracing::warn!(code = ?e.code, "Could not schedule project checks");
                }
                if hub.inner.checks.dirty.swap(false, Ordering::AcqRel) {
                    continue;
                }
                hub.inner.checks.scheduling.store(false, Ordering::Release);
                if !hub.inner.checks.dirty.load(Ordering::Acquire)
                    || hub.inner.checks.scheduling.swap(true, Ordering::AcqRel)
                {
                    break;
                }
            }
        });
    }

    fn schedule_check_jobs(&self) -> Result<(), ProtocolError> {
        if *self.inner.shutdown.borrow() {
            return Ok(());
        }
        // A previous hand-over can fail either before or after returning the
        // writer to the original run. Both durable positions are retryable.
        self.retry_finished_check_handovers()?;
        self.finish_recovered_checks()?;
        let mut jobs = self.read(|r| r.pending_check_jobs())?;
        jobs.sort_by_key(|j| (j.created_at, j.id));
        #[cfg(test)]
        {
            let hook = self.inner.checks.snapshot_hook.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        for candidate in jobs {
            // The scan is only an index of ids. Every decision, including orphan
            // detection and cancellation, uses the current state in this write.
            let (admitted, finished) = self.write(|tx| {
                let job: CheckJob = tx.read().require(store::checks::JOBS, candidate.id)?;
                if matches!(job.state, CheckJobState::Finished | CheckJobState::Uncertain) {
                    return Ok((false, None));
                }
                #[cfg(test)]
                if self.inner.checks.paused.load(Ordering::Acquire)
                    && job.state != CheckJobState::Cancelling
                {
                    return Ok((false, None));
                }
                {
                    let active = self.inner.checks.active.lock().unwrap();
                    if let Some(control) = active.get(&job.id) {
                        if job.state == CheckJobState::Cancelling {
                            control.cancel.send_replace(true);
                        }
                        return Ok((false, None));
                    }
                }
                let record: Option<ProcessRecord> = tx.read().get(PROCESSES, job.id)?;
                if matches!(job.state, CheckJobState::Running | CheckJobState::Cancelling) {
                    if job.process.is_none() && record.is_none() {
                        let finished = self.finish_check_job_in(tx, job.id, true)?;
                        return Ok((false, Some(finished)));
                    }
                    if job.state == CheckJobState::Running {
                        self.mark_check_job_uncertain_in(tx, job.id, "Ophio lost control of this check. Check whether it is still running before continuing.")?;
                        return Ok((false, None));
                    }
                }
                let project: Project = tx.read().require("project", job.project_id)?;
                if job.task_id.is_some() && project.active_writer != Some(job_writer(job.id)) {
                    if job.state == CheckJobState::Cancelling {
                        self.mark_check_job_uncertain_in(tx, job.id, "Ophio could not account for a check that no longer holds the project. Check it before continuing.")?;
                    }
                    return Ok((false, None));
                }
                let mut project = project;
                let sentinel = job_writer(job.id);
                if project.active_writer != Some(sentinel)
                    && self.inner.checking_continuations.lock().map_err(crate::internal)?.contains_key(&job.project_id)
                {
                    return Ok((false, None));
                }
                if let Some(writer) = project.active_writer {
                    if writer != sentinel {
                        return Ok((false, None));
                    }
                } else {
                    if job.task_id.is_some() {
                        return Ok((false, None));
                    }
                    let mut queue: QueueView = tx.read().require("queue", job.project_id)?;
                    if queue.writer.is_some() {
                        return Ok((false, None));
                    }
                    project.active_writer = Some(sentinel);
                    queue.writer = Some(sentinel);
                    crate::runs::bump_queue_revision(tx, &mut queue)?;
                    tx.put("project", project.project_id, &project)?;
                    tx.put("queue", queue.project_id, &queue)?;
                    tx.emit(Event::ProjectUpserted { project })?;
                    tx.emit(Event::QueueChanged { queue })?;
                }
                if job.state == CheckJobState::Pending {
                    let mut job = job;
                    job.state = CheckJobState::Running;
                    job.started_at.get_or_insert(self.inner.clock.now());
                    tx.put_check_job(&job)?;
                    progress(tx, &job)?;
                }
                Ok((true, None))
            })?;
            if let Some(finished) = finished {
                self.end_check_job_reservation(&finished)?;
                self.inner.checks.changed.notify_waiters();
            }
            if !admitted {
                continue;
            }
            let control = Arc::new(Control {
                cancel: watch::channel(false).0,
            });
            // Only this dispatcher inserts controls, and it never awaits a worker.
            self.inner
                .checks
                .active
                .lock()
                .unwrap()
                .insert(candidate.id, control.clone());
            let hub = self.clone();
            self.spawn(async move {
                if let Err(e) = hub.execute_check_job(candidate.id, control).await {
                    tracing::warn!(code = ?e.code, "Project check execution needs reconciliation");
                    if hub.read(|r| Ok(r.require::<CheckJob>(store::checks::JOBS, candidate.id)?.state != CheckJobState::Finished)).unwrap_or(false) {
                        let _ = hub.mark_check_job_uncertain(candidate.id, "Ophio could not finish recording this check. Check whether it is still running before continuing.");
                    }
                }
                hub.inner.checks.active.lock().unwrap().remove(&candidate.id);
                hub.inner.checks.changed.notify_waiters();
                hub.wake_checks();
            });
        }
        Ok(())
    }

    async fn execute_check_job(
        &self,
        id: CheckJobId,
        control: Arc<Control>,
    ) -> Result<(), ProtocolError> {
        let mut cancelled = control.cancel.subscribe();
        let mut shutdown = self.inner.shutdown.subscribe();
        loop {
            let job = self.read(|r| r.require::<CheckJob>(store::checks::JOBS, id))?;
            if matches!(
                job.state,
                CheckJobState::Finished | CheckJobState::Uncertain
            ) {
                return Ok(());
            }
            if *shutdown.borrow() {
                return Ok(());
            }
            if job.state == CheckJobState::Cancelling || *cancelled.borrow() {
                let record = self.read(|r| r.get::<ProcessRecord>(PROCESSES, id))?;
                if let Some(record) = record {
                    let Some(identity) = recorded_identity(record) else {
                        return self.mark_check_job_uncertain(id, "Ophio could not identify the check's processes, so it cannot confirm they stopped.");
                    };
                    let executor = self.inner.checks.runner.lock().unwrap().clone();
                    if executor.stop(&identity).await != StopStatus::Confirmed {
                        return self.mark_check_job_uncertain(id, "Ophio could not confirm the check stopped. The project stays reserved.");
                    }
                }
                return self.finish_check_job(id, true, true);
            }
            let Some(check) = job
                .checks
                .iter()
                .find(|check| !job.results.iter().any(|r| r.check_id == check.check_id))
                .cloned()
            else {
                return self.finish_check_job(id, false, true);
            };
            if let CheckJobPurpose::DraftProof { draft_id } = job.purpose {
                let current = self.read(|r| {
                    Ok(r.get::<ChecksDraft>(store::checks::DRAFTS, draft_id)?
                        .is_some_and(|draft| {
                            matches!(
                                draft.state,
                                ChecksDraftState::Proving | ChecksDraftState::Stale
                            )
                        }))
                })?;
                if !current {
                    self.record_check_result(
                        id,
                        failed_start(
                            &check,
                            "The suggested checks were discarded before their proof could run.",
                        ),
                    )?;
                    continue;
                }
            }
            let request = match self.check_request(&job, &check) {
                Ok(request) => request,
                Err(e) => {
                    self.record_check_result(
                        id,
                        failed_start(&check, "The check's approved access could not be prepared."),
                    )?;
                    tracing::warn!(code = ?e.code, "Could not prepare check access");
                    continue;
                }
            };
            // Commit the launch boundary before any process can be created.
            let launch = self.write(|tx| {
                let current: CheckJob = tx.read().require(store::checks::JOBS, id)?;
                if current.state == CheckJobState::Cancelling {
                    return Ok(false);
                }
                tx.put(
                    PROCESSES,
                    id,
                    &ProcessRecord {
                        launching: true,
                        identity: None,
                        partial: None,
                    },
                )?;
                Ok(true)
            })?;
            if !launch {
                continue;
            }
            let executor = self.inner.checks.runner.lock().unwrap().clone();
            let mut execution = match executor.start(request).await {
                Ok(execution) => execution,
                Err(runner::StartError::TimedOutBeforeStart { duration_ms }) => {
                    self.record_check_result(
                        id,
                        CheckResult {
                            outcome: CheckOutcome::TimedOut,
                            duration_secs: duration_ms as f64 / 1000.0,
                            output: "[Ophio] This check timed out before its sandbox could start. No check command was started.\n".into(),
                            ..failed_start(&check, "")
                        },
                    )?;
                    // No check launcher ran. There is no check process
                    // identity to persist, inspect or stop.
                    self.write(|tx| tx.delete(PROCESSES, id))?;
                    continue;
                }
                Err(runner::StartError::NotStarted { reason }) => {
                    self.record_check_result(
                        id,
                        failed_start(
                            &check,
                            &format!("The computer could not start this check. {reason}"),
                        ),
                    )?;
                    // The runner has confirmed that nothing survived this attempt.
                    self.write(|tx| tx.delete(PROCESSES, id))?;
                    continue;
                }
                Err(runner::StartError::Uncertain { identity, .. }) => {
                    self.record_check_result(
                        id,
                        failed_start(&check, "The computer could not start this check."),
                    )?;
                    self.write(|tx| {
                        let full = full_identity(&identity);
                        let mut current: CheckJob = tx.read().require(store::checks::JOBS, id)?;
                        current.process = full.as_ref().map(wire_identity);
                        tx.put_check_job(&current)?;
                        tx.put(
                            PROCESSES,
                            id,
                            &ProcessRecord {
                                launching: false,
                                identity: full,
                                partial: Some(*identity),
                            },
                        )
                    })?;
                    return self.mark_check_job_uncertain(id, "The check could not start, and Ophio could not confirm that its processes stopped.");
                }
            };
            let persisted = self.write(|tx| {
                let mut current: CheckJob = tx.read().require(store::checks::JOBS, id)?;
                current.process = Some(wire_identity(&execution.identity));
                current.sandbox = Some(execution.sandbox.clone());
                tx.put(
                    PROCESSES,
                    id,
                    &ProcessRecord {
                        launching: false,
                        identity: Some(execution.identity.clone()),
                        partial: None,
                    },
                )?;
                tx.put_check_job(&current)?;
                Ok(current.state == CheckJobState::Cancelling)
            });
            let stopping = match persisted {
                Ok(stopping) => stopping,
                Err(e) => {
                    (execution.cancel)();
                    let _ = execution.result.await;
                    return Err(e);
                }
            };
            if stopping || *cancelled.borrow() || *shutdown.borrow() {
                (execution.cancel)();
            }
            let output = tokio::select! {
                output = &mut execution.result => output,
                _ = cancelled.changed() => { (execution.cancel)(); execution.result.await },
                _ = shutdown.changed() => { (execution.cancel)(); execution.result.await },
            };
            // Leave the durable identity for startup inspection during shutdown.
            if *shutdown.borrow() {
                return Ok(());
            }
            let output = match output {
                Ok(output) => output,
                Err(_) => return self.mark_check_job_uncertain(id, "Ophio lost the check's result and could not confirm that its processes stopped."),
            };
            self.record_check_result(id, output_result(&check, &output))?;
            if let StopStatus::Uncertain { .. } = output.stop {
                return self.mark_check_job_uncertain(id, "Ophio could not confirm that everything started by this check stopped. The project stays reserved.");
            }
            self.write(|tx| {
                let mut current: CheckJob = tx.read().require(store::checks::JOBS, id)?;
                current.process = None;
                tx.put_check_job(&current)?;
                // Keep the last confirmed identity: a restart between checks may
                // safely rerun only after the runner inspects it as Stopped.
                Ok(())
            })?;
        }
    }

    fn check_request(&self, job: &CheckJob, check: &Check) -> Result<CheckRequest, ProtocolError> {
        let (project, reaches) = self.read(|r| {
            let project: Project = r.require("project", job.project_id)?;
            let mut reaches = vec![];
            if let Some(task_id) = job.task_id {
                let task: Task = r.require("task", task_id)?;
                let current = crate::access::launch_access(task.access.as_ref());
                let reach = match job.run_id {
                    Some(run_id) => self.conversation_reach(r, &task, run_id, &current)?,
                    None => self.access_reach(r, &current)?,
                };
                reaches.push(reach);
                if let Some(run_id) = job.run_id {
                    // A missing old snapshot cannot justify granting more access.
                    let saved = r.get::<Option<ConversationAccess>>("run_access", run_id)?;
                    let saved = match saved {
                        Some(Some(access)) => access,
                        Some(None) => crate::access::launch_access(None).into_owned(),
                        None => ConversationAccess {
                            internet: false,
                            ..Default::default()
                        },
                    };
                    reaches.push(self.conversation_reach(r, &task, run_id, &saved)?);
                }
            } else {
                // Match a new ordinary Work task, not the more restrictive
                // Chat default. The runner still admits only its toolchain
                // allowlist from these reads and approved check networking.
                reaches.push(self.access_reach(r, &crate::access::launch_access(None))?);
            }
            Ok((project, reaches))
        })?;
        for reach in &reaches {
            let root = ports::library::comparison(std::path::Path::new(&project.host_path))
                .map_err(|_| {
                    error(
                        ErrorCode::PathNotAllowed,
                        "The check's project folder could not be verified.",
                    )
                })?;
            if !reach.whole_write
                && !reach.write.iter().any(|path| {
                    ports::library::comparison(path).is_ok_and(|allowed| root.starts_with(allowed))
                })
            {
                return Err(error(
                    ErrorCode::PermissionDenied,
                    "This task no longer has permission to run checks that can change the project.",
                ));
            }
        }
        let mut request = CheckRequest::new(
            project.host_path,
            self.inner.data.join("checks"),
            &check.run,
            Duration::from_secs(check.timeout_secs.into()),
        );
        // Only genuinely unsandboxed tasks can authorize direct host networking.
        // Restricted tasks retain the agent's filtered path and its stricter
        // private-network policy, across both saved and current permissions.
        request.network =
            check.network && !reaches.is_empty() && reaches.iter().all(|r| r.internet);
        request.task_network_restricted =
            reaches.is_empty() || reaches.iter().any(|reach| !reach.unrestricted());
        request.local_network = !reaches.is_empty()
            && reaches
                .iter()
                .all(|reach| reach.internet && reach.local_network);
        if let Some(home) = &self.inner.home {
            let mut allowed: Option<Vec<PathBuf>> = None;
            for reach in &reaches {
                let read: Vec<_> = reach.read.iter().chain(&reach.write).cloned().collect();
                let paths = check_runner::toolchains::readable_toolchains(
                    home,
                    &read,
                    reach.whole_read || reach.whole_write,
                )
                .map_err(|_| {
                    error(
                        ErrorCode::PathNotAllowed,
                        "The check's toolchain access could not be verified.",
                    )
                })?;
                allowed = Some(match allowed {
                    None => paths,
                    Some(previous) => intersect_paths(&previous, &paths),
                });
            }
            request.read_toolchains = allowed.unwrap_or_default();
        }
        Ok(request)
    }

    fn record_check_result(
        &self,
        id: CheckJobId,
        mut result: CheckResult,
    ) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let mut job: CheckJob = tx.read().require(store::checks::JOBS, id)?;
            let inputs: BTreeMap<CheckId, Vec<String>> =
                tx.read().get("check_job_inputs", id)?.unwrap_or_default();
            result.inputs_changed = inputs.get(&result.check_id).cloned().unwrap_or_default();
            if !job.results.iter().any(|r| r.check_id == result.check_id) {
                job.results.push(result);
            }
            tx.put_check_job(&job)?;
            progress(tx, &job)
        })
    }

    #[cfg(test)]
    pub(crate) fn finish_check_job_for_test(
        &self,
        id: CheckJobId,
        stopped: bool,
    ) -> Result<(), ProtocolError> {
        self.finish_check_job(id, stopped, true)
    }

    fn finish_check_job(
        &self,
        id: CheckJobId,
        stopped: bool,
        release: bool,
    ) -> Result<(), ProtocolError> {
        let job = self.write(|tx| self.finish_check_job_in(tx, id, stopped))?;
        if release {
            self.end_check_job_reservation(&job)?;
        }
        self.inner.checks.changed.notify_waiters();
        Ok(())
    }

    fn finish_check_job_in(
        &self,
        tx: &mut store::Write<'_>,
        id: CheckJobId,
        stopped: bool,
    ) -> store::Result<CheckJob> {
        let mut job: CheckJob = tx.read().require(store::checks::JOBS, id)?;
        if job.state == CheckJobState::Finished {
            return Ok(job);
        }
        // Stop may have committed after the worker's last read of this job.
        let stopped = stopped || job.state == CheckJobState::Cancelling;
        if stopped {
            let inputs: BTreeMap<CheckId, Vec<String>> =
                tx.read().get("check_job_inputs", id)?.unwrap_or_default();
            for check in &job.checks {
                if !job.results.iter().any(|r| r.check_id == check.check_id) {
                    let mut result = stopped_result(check);
                    result.inputs_changed =
                        inputs.get(&check.check_id).cloned().unwrap_or_default();
                    job.results.push(result);
                }
            }
            job.message = Some("Not checked: stopped".into());
        }
        job.state = CheckJobState::Finished;
        job.process = None;
        job.finished_at = Some(self.inner.clock.now());
        tx.put_check_job(&job)?;
        tx.record_check_durations(id)?;
        if let CheckJobPurpose::DraftProof { draft_id } = job.purpose
            && let Some(draft) = tx
                .read()
                .get::<ChecksDraft>(store::checks::DRAFTS, draft_id)?
            && matches!(
                draft.state,
                ChecksDraftState::Proving | ChecksDraftState::Stale
            )
        {
            tx.finish_checks_proof(draft_id, job.results.clone())?;
        }
        progress(tx, &job)?;
        Ok(job)
    }

    fn retry_finished_check_handovers(&self) -> Result<(), ProtocolError> {
        let ids = self.read(|r| {
            let mut ids = Vec::new();
            for job in r.list::<CheckJob>(store::checks::JOBS)? {
                if job.state == CheckJobState::Finished
                    && let Some(task_id) = job.task_id
                    && r.require::<Project>("project", job.project_id)?
                        .active_writer
                        == Some(job_writer(job.id))
                {
                    let task: Task = r.require("task", task_id)?;
                    if task.state != TaskState::ReconciliationRequired
                        || r.get::<serde_json::Value>("check_human_resolution", job.id)?
                            .is_some()
                    {
                        ids.push(job.id);
                    }
                }
            }
            Ok(ids)
        })?;
        for id in ids {
            // advance_task_checks re-reads inside its write before acting.
            self.check_job_ended(id)?;
        }
        Ok(())
    }

    fn end_check_job_reservation(&self, job: &CheckJob) -> Result<(), ProtocolError> {
        if job.task_id.is_some() {
            self.check_job_ended(job.id)
        } else {
            self.write(|tx| {
                let current: CheckJob = tx.read().require(store::checks::JOBS, job.id)?;
                if current.state == CheckJobState::Finished {
                    self.release_writer(tx, current.project_id, job_writer(current.id))?;
                }
                Ok(())
            })
        }
    }

    fn mark_check_job_uncertain(&self, id: CheckJobId, reason: &str) -> Result<(), ProtocolError> {
        self.write(|tx| self.mark_check_job_uncertain_in(tx, id, reason))?;
        self.inner.checks.changed.notify_waiters();
        Ok(())
    }

    fn mark_check_job_uncertain_in(
        &self,
        tx: &mut store::Write<'_>,
        id: CheckJobId,
        reason: &str,
    ) -> store::Result<()> {
        let mut job: CheckJob = tx.read().require(store::checks::JOBS, id)?;
        // A worker or another recovery step may already have finished it.
        if job.state == CheckJobState::Finished {
            return Ok(());
        }
        job.state = CheckJobState::Uncertain;
        job.message = Some(reason.into());
        tx.put_check_job(&job)?;
        if let Some(task_id) = job.task_id {
            let mut task: Task = tx.read().require("task", task_id)?;
            let now = self.inner.clock.now();
            let has_notice = tx
                .read()
                .list::<AttentionItem>("attention")?
                .iter()
                .any(|item| {
                    item.task_id == Some(task_id)
                        && item.kind == AttentionKind::ReconciliationRequired
                        && item.resolved_at.is_none()
                });
            if !has_notice {
                let item = AttentionItem {
                    attention_id: AttentionId::new(),
                    kind: AttentionKind::ReconciliationRequired,
                    title: task.title.clone(),
                    body: Some(reason.into()),
                    task_id: Some(task_id),
                    run_id: job.run_id,
                    child_id: None,
                    approval_id: None,
                    created_at: now,
                    resolved_at: None,
                    priority: 5,
                    topic: None,
                    quiet: false,
                    report_id: None,
                    account_id: None,
                    alert: None,
                    alert_handled_at: None,
                    agent_update: None,
                    new_version: None,
                };
                tx.put("attention", item.attention_id, &item)?;
                tx.emit(Event::AttentionUpserted { item })?;
                tx.emit(Event::TimelineAppended {
                    task_id,
                    item: TimelineItem {
                        seq: 0,
                        at: now,
                        run_id: job.run_id,
                        item: TimelineBody::Note {
                            text: reason.into(),
                        },
                    },
                })?;
            }
            if task.state != TaskState::ReconciliationRequired {
                task.revision += 1;
            }
            task.state = TaskState::ReconciliationRequired;
            task.updated_at = now;
            task.attention_count = crate::runs::blocking_attention_count(&tx.read(), task_id)?;
            tx.put("task", task_id, &task)?;
            tx.emit(Event::TaskUpserted { task })?;
        }
        Ok(())
    }

    /// Stops all jobs in the task's reserved sequence, not just its current check.
    pub(crate) async fn stop_task_checks(&self, task_id: TaskId) -> Result<bool, ProtocolError> {
        let stopped = self.write(|tx| {
            let task: Task = tx.read().require("task", task_id)?;
            if task.checking.is_none() {
                return Ok(false);
            }
            for mut job in tx.read().task_check_jobs(task_id)? {
                if matches!(
                    job.state,
                    CheckJobState::Pending | CheckJobState::Running | CheckJobState::Cancelling
                ) {
                    job.state = CheckJobState::Cancelling;
                    tx.put_check_job(&job)?;
                }
            }
            Ok(true)
        })?;
        if !stopped {
            return Ok(false);
        }
        self.wake_checks();
        loop {
            let changed = self.inner.checks.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let jobs = self.read(|r| r.task_check_jobs(task_id))?;
            if jobs.iter().any(|j| j.state == CheckJobState::Uncertain) {
                return Err(uncertain_error());
            }
            let active = {
                let active = self.inner.checks.active.lock().unwrap();
                jobs.iter().any(|j| active.contains_key(&j.id))
            };
            if !active && jobs.iter().all(|j| j.state == CheckJobState::Finished) {
                if let Some(checking) =
                    self.read(|r| Ok(r.require::<Task>("task", task_id)?.checking))?
                {
                    self.check_job_ended(checking.job_id)?;
                }
                self.finish_recovered_checks()?;
                return Ok(true);
            }
            changed.await;
        }
    }

    /// Recover ids from the scan, never its stale states. The launch record is
    /// committed before spawn: no record and no process means never started.
    pub(crate) async fn recover_check_jobs(&self) -> Result<(), ProtocolError> {
        self.inner.checks.recovering.store(true, Ordering::Release);
        let jobs = self.read(|r| r.list::<CheckJob>(store::checks::JOBS))?;
        let executor = self.inner.checks.runner.lock().unwrap().clone();
        for candidate in jobs {
            let (job, record) = self.write(|tx| {
                let job: CheckJob = tx.read().require(store::checks::JOBS, candidate.id)?;
                let record: Option<ProcessRecord> = tx.read().get(PROCESSES, job.id)?;
                if matches!(
                    job.state,
                    CheckJobState::Running | CheckJobState::Cancelling
                ) && job.process.is_none()
                    && record.is_none()
                {
                    return Ok((self.finish_check_job_in(tx, job.id, true)?, None));
                }
                Ok((job, record))
            })?;
            if job.state == CheckJobState::Finished {
                let owns = self.read(|r| {
                    let owns = r
                        .get::<Project>("project", job.project_id)?
                        .is_some_and(|project| project.active_writer == Some(job_writer(job.id)));
                    let awaiting_person = job
                        .task_id
                        .map(|id| r.require::<Task>("task", id))
                        .transpose()?
                        .is_some_and(|task| task.state == TaskState::ReconciliationRequired);
                    let explicitly_resolved = r
                        .get::<serde_json::Value>("check_human_resolution", job.id)?
                        .is_some();
                    Ok(owns && (!awaiting_person || explicitly_resolved))
                })?;
                if owns {
                    self.end_check_job_reservation(&job)?;
                }
                continue;
            }
            if job.state == CheckJobState::Pending && job.process.is_none() && record.is_none() {
                continue;
            }
            let state = match record.clone().and_then(recorded_identity) {
                Some(identity) => executor.inspect(&identity).await,
                None => ProcessState::Uncertain {
                    reason: "The check's full process identity was not saved.".into(),
                },
            };
            let finished = self.write(|tx| {
                let mut current: CheckJob = tx.read().require(store::checks::JOBS, job.id)?;
                let current_record: Option<ProcessRecord> = tx.read().get(PROCESSES, job.id)?;
                // Inspection awaited the operating system. Do not apply its
                // observation to a finished job or a different launch attempt.
                if current.state == CheckJobState::Finished || current_record != record {
                    return Ok(None);
                }
                match state {
                    ProcessState::Stopped if current.state == CheckJobState::Cancelling => {
                        return Ok(Some(self.finish_check_job_in(tx, job.id, true)?));
                    }
                    ProcessState::Stopped if current.state != CheckJobState::Uncertain => {
                        current.state = CheckJobState::Pending;
                        current.process = None;
                        current.started_at = None;
                        current.finished_at = None;
                        current.results.retain(|r| matches!(r.outcome, CheckOutcome::Off | CheckOutcome::SkippedPaths));
                        current.message = Some("Ophio restarted. These checks will run again from the beginning.".into());
                        tx.delete(PROCESSES, job.id)?;
                        tx.put_check_job(&current)?;
                        progress(tx, &current)?;
                    }
                    ProcessState::Stopped => {}
                    ProcessState::Running | ProcessState::Uncertain { .. } => {
                        self.mark_check_job_uncertain_in(tx, job.id, "Ophio restarted and could not confirm that this check stopped. Check it before continuing.")?;
                    }
                }
                Ok(None)
            })?;
            if let Some(finished) = finished {
                self.end_check_job_reservation(&finished)?;
            }
        }
        self.inner.checks.recovering.store(false, Ordering::Release);
        Ok(())
    }

    /// Inspection for an explicit person-confirmed job resolution. This makes no
    /// changes: admission and the person's identity commit with the command.
    pub(crate) async fn prepare_check_job_resolution(
        &self,
        job_id: CheckJobId,
    ) -> Result<(), ProtocolError> {
        let record = self.read(|r| {
            let job: CheckJob = r.require(store::checks::JOBS, job_id)?;
            if job.state != CheckJobState::Uncertain {
                return Err(error(
                    ErrorCode::Conflict,
                    "Only an uncertain check run needs your confirmation that it stopped.",
                )
                .into());
            }
            r.get::<ProcessRecord>(PROCESSES, job_id)
        })?;
        if let Some(identity) = record.and_then(recorded_identity) {
            let executor = self.inner.checks.runner.lock().unwrap().clone();
            if executor.inspect(&identity).await == ProcessState::Running {
                return Err(error(
                    ErrorCode::OutcomeUnknown,
                    "These checks are still running. Stop them on the computer before confirming that they stopped.",
                ));
            }
        }
        Ok(())
    }

    /// Prepare an explicitly requested human resolution, refusing processes the
    /// runner positively sees alive. The person's ReconcileTask transaction owns
    /// releasing the writer and recording their chosen outcome and identity.
    pub(crate) async fn reconcile_check_jobs(
        &self,
        task_id: TaskId,
    ) -> Result<bool, ProtocolError> {
        let jobs = self.read(|r| r.task_check_jobs(task_id))?;
        if !jobs.iter().any(|j| j.state == CheckJobState::Uncertain) {
            return Ok(false);
        }
        let unfinished: Vec<_> = jobs
            .into_iter()
            .filter(|j| j.state != CheckJobState::Finished)
            .collect();
        let executor = self.inner.checks.runner.lock().unwrap().clone();
        for job in &unfinished {
            let record = self.read(|r| r.get::<ProcessRecord>(PROCESSES, job.id))?;
            if let Some(identity) = record.and_then(recorded_identity)
                && executor.inspect(&identity).await == ProcessState::Running
            {
                return Err(error(
                    ErrorCode::OutcomeUnknown,
                    "This task's checks are still running. Stop them before resolving the task.",
                ));
            }
        }
        // This is the person's explicit resolution, not computer verification.
        // Preserve actual results; never convert an unobserved check to Passed.
        for job in unfinished {
            self.write(|tx| {
                let mut current: CheckJob = tx.read().require(store::checks::JOBS, job.id)?;
                let inputs: BTreeMap<CheckId, Vec<String>> = tx
                    .read()
                    .get("check_job_inputs", job.id)?
                    .unwrap_or_default();
                for check in &current.checks {
                    if !current
                        .results
                        .iter()
                        .any(|result| result.check_id == check.check_id)
                    {
                        let mut result = failed_start(
                            check,
                            "Not checked: the person resolved an uncertain check run",
                        );
                        result.inputs_changed =
                            inputs.get(&check.check_id).cloned().unwrap_or_default();
                        current.results.push(result);
                    }
                }
                current.message =
                    Some("Not checked: the person resolved an uncertain check run".into());
                tx.put_check_job(&current)
            })?;
            self.finish_check_job(job.id, false, false)?;
        }
        Ok(true)
    }
}

fn progress(tx: &mut store::Write<'_>, job: &CheckJob) -> store::Result<()> {
    if let Some(id) = job.task_id {
        let mut task: Task = tx.read().require("task", id)?;
        if let Some(checking) = &mut task.checking
            && checking.job_id == job.id
        {
            checking.done = job.results.len() as u32;
            checking.total = job.checks.len() as u32;
            tx.put("task", id, &task)?;
            tx.emit(Event::TaskUpserted { task })?;
        }
    }
    Ok(())
}
fn intersect_paths(first: &[PathBuf], second: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = vec![];
    for a in first {
        for b in second {
            if a.starts_with(b) {
                paths.push(a.clone());
            } else if b.starts_with(a) {
                paths.push(b.clone());
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}
fn full_identity(identity: &runner::PartialProcessIdentity) -> Option<ProcessIdentity> {
    Some(ProcessIdentity {
        pid: identity.pid?,
        start_time: identity.start_time?,
        sandbox_unit: identity.sandbox_unit.clone(),
        project: identity.project.clone(),
        boot_id: identity.boot_id.clone()?,
    })
}
fn recorded_identity(record: ProcessRecord) -> Option<ProcessIdentity> {
    record
        .identity
        .or_else(|| record.partial.as_ref().and_then(full_identity))
}
fn wire_identity(identity: &ProcessIdentity) -> CheckProcessIdentity {
    CheckProcessIdentity {
        pid: identity.pid,
        start_time: identity.start_time.to_string(),
        sandbox_unit: identity.sandbox_unit.clone(),
    }
}
fn failed_start(check: &Check, reason: &str) -> CheckResult {
    CheckResult {
        check_id: check.check_id,
        name: check.name.clone(),
        outcome: CheckOutcome::CouldNotRun {
            reason: reason.into(),
        },
        duration_secs: 0.0,
        exit_code: None,
        output: String::new(),
        inputs_changed: vec![],
    }
}
fn stopped_result(check: &Check) -> CheckResult {
    CheckResult {
        outcome: CheckOutcome::Stopped,
        ..failed_start(check, "")
    }
}
fn output_result(check: &Check, output: &CheckOutput) -> CheckResult {
    let outcome = match &output.outcome {
        runner::CheckOutcome::Passed => CheckOutcome::Passed,
        runner::CheckOutcome::Failed => CheckOutcome::Failed,
        runner::CheckOutcome::TimedOut => CheckOutcome::TimedOut,
        runner::CheckOutcome::Cancelled => CheckOutcome::Stopped,
        runner::CheckOutcome::CouldNotRun { reason } => CheckOutcome::CouldNotRun {
            reason: reason.clone(),
        },
    };
    CheckResult {
        check_id: check.check_id,
        name: check.name.clone(),
        outcome,
        duration_secs: output.duration_ms as f64 / 1000.0,
        exit_code: output.exit_code,
        output: output.output_tail.clone(),
        inputs_changed: vec![],
    }
}
fn uncertain_error() -> ProtocolError {
    error(
        ErrorCode::OutcomeUnknown,
        "Ophio cannot confirm that this task's checks stopped. The project stays reserved until their processes can be accounted for.",
    )
}

#[cfg(test)]
#[path = "check_execution_tests.rs"]
mod tests;
