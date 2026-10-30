// SPDX-License-Identifier: Apache-2.0
use crate::{CompanionHub, api::Caller, error, unsupported};
#[path = "admitted_followups.rs"]
mod admitted_followups;
pub(crate) use admitted_followups::fail_undispatched_followup;
#[path = "queue_completion.rs"]
pub(crate) mod queue_completion;
use ports::{Containment, HarnessError, HarnessEvent, MessageRef, PromptInput, RunExit};
use protocol::{
    annotation::AnnotationBundle,
    command::*,
    event::{EventSource, LiveEvent},
    model::*,
    queue::*,
    *,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunIntent {
    command_id: CommandId,
    resume: Option<String>,
    #[serde(default)]
    resume_from_mode: Option<TaskMode>,
    text: Option<String>,
    #[serde(default)]
    bundle: Option<Box<AnnotationBundle>>,
}

/// Whether `run_id`, `task`'s turn, was one Ophio started to see its pull
/// request through (a wake the agent asked for, or news from GitHub handed
/// over) while Ophio still watches that pull request. A message the person
/// left in Next up opens such a turn, so a turn they asked for never counts.
fn ophio_round(r: &store::Read<'_>, run_id: RunId, task: &Task) -> store::Result<bool> {
    use protocol::github::{GithubMode, GithubSettings, PrStage, PrState};
    let ophios = |text: &str| {
        text.starts_with(crate::watching::WAKE) || crate::github_watch::handed_over(text)
    };
    let Some(text) = r
        .get::<RunIntent>("run_intent", run_id)?
        .and_then(|i| i.text)
    else {
        return Ok(false);
    };
    if !ophios(&text) {
        return Ok(false);
    }
    // Nor one the person spoke into while it ran: read back to its start,
    // or to an earlier run's, as a run need not report its start.
    let mut before = None;
    let mut read = false;
    'pages: for _ in 0..4 {
        let (page, more) = r.timeline(task.task_id, before, 200)?;
        before = page.first().map(|i| i.seq);
        for item in page.into_iter().rev() {
            match (&item.item, item.run_id) {
                (TimelineBody::RunStarted { run_id: s }, _) if *s == run_id => {
                    read = true;
                    break 'pages;
                }
                (_, Some(earlier)) if earlier != run_id => {
                    read = true;
                    break 'pages;
                }
                (TimelineBody::UserMessage { text, .. }, Some(_)) if !ophios(text) => {
                    return Ok(false);
                }
                _ => {}
            }
        }
        if !more {
            read = true;
            break;
        }
    }
    if !read {
        return Ok(false);
    }
    let github = task.github.as_deref();
    let open = (github.and_then(|g| g.pr.as_ref())).is_some_and(|p| p.state == PrState::Open);
    let watched = matches!(
        github.and_then(|g| g.stage),
        Some(PrStage::Babysitting | PrStage::Checking | PrStage::Merging)
    );
    if !(open && watched) {
        return Ok(false);
    }
    let settings: GithubSettings = r
        .get(crate::github::SETTINGS, task.project_id)?
        .unwrap_or_default();
    Ok(settings.mode != GithubMode::Off)
}

/// The state a task had before a read-only turn, put back when that turn
/// ends: what it answered doesn't change how the task's work ended.
pub(crate) const SIDE_TURN: &str = "side_turn_state";

/// The mode `run` works in. A read-only turn runs in Ask while its task
/// keeps Work or Plan; runs saved before runs had a mode use the task's.
/// Whether run `id` has ended or been stopped, as `r` reads it: checked
/// again in the write that acts for it, so a Stop that commits first wins.
pub(crate) fn stopped(r: &store::Read<'_>, id: RunId) -> store::Result<bool> {
    let run: Run = r.require("run", id)?;
    Ok(!matches!(
        run.state,
        RunState::Dispatching
            | RunState::Working
            | RunState::AwaitingApproval
            | RunState::AwaitingInput
    ) || r.get::<bool>("stopped", id)?.unwrap_or(false))
}

pub(crate) fn run_mode(run: &Run, task: &Task) -> TaskMode {
    run.mode.unwrap_or(task.mode)
}

/// Whether `harness` asks for permission through the computer bridge's
/// permission tool once that bridge is bound. Only Claude Code does; Codex
/// sends its approvals and questions over its own app-server.
pub(crate) fn asks_through_bridge(harness: protocol::capability::HarnessKind) -> bool {
    harness == protocol::capability::HarnessKind::ClaudeCode
}

/// Persisted alongside the message/update so restart cannot lose correlation.
/// Initial and resume prompts are not follow-ups and never enter this list.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InFlightMessage {
    // Older records have no identity and cannot be matched to a native update.
    #[serde(default)]
    message_ref: Option<MessageRef>,
    seq: u64,
    state: DeliveryState,
    // Older records were written only after native dispatch.
    #[serde(default)]
    undispatched: bool,
}

fn advances_delivery(previous: DeliveryState, next: DeliveryState) -> bool {
    if previous == next {
        return false;
    }
    match next {
        DeliveryState::HostAccepted => matches!(
            previous,
            DeliveryState::QueuedForBoundary | DeliveryState::Unknown
        ),
        DeliveryState::NativeDelivered
        | DeliveryState::Failed
        | DeliveryState::Unknown
        | DeliveryState::Cancelled => true,
        DeliveryState::QueuedForBoundary => false,
    }
}

fn in_flight(state: DeliveryState) -> bool {
    matches!(
        state,
        DeliveryState::QueuedForBoundary | DeliveryState::HostAccepted | DeliveryState::Unknown
    )
}

/// A message still held when its run ends was never written. A Stop, including
/// a Force stop recorded before the adapter's own report, withdraws it; any
/// other end fails it.
fn unsent(tx: &store::Write<'_>, id: RunId, message_seq: u64) -> store::Result<TimelineBody> {
    let stopped = tx.read().get::<bool>("stopped", id)?.unwrap_or(false);
    Ok(TimelineBody::Delivery {
        message_seq,
        state: if stopped {
            DeliveryState::Cancelled
        } else {
            DeliveryState::Failed
        },
        withdrawn_by_stop: stopped,
        withdrawn_by_revocation: false,
    })
}

/// How long "Start by itself" holds for an item a person put in Next up:
/// long enough to wait behind a day of earlier work, as an agent's and a
/// review's fixes do, and bounded so a computer that was off a while doesn't
/// start old work by itself.
pub(crate) const QUEUE_PERMIT_MS: i64 = 24 * 3_600_000;
/// A decision that reaches the computer once the run that asked has ended or
/// is stopping: no agent is left to act on it.
pub(crate) const ARRIVED_LATE: &str = "Arrived after the run ended; not applied";
/// A decision the agent's route took just as the run that asked ended: the
/// agent may have acted on it, and nothing left can say whether it did.
pub(crate) const ENDED_AS_DELIVERED: &str = "Reached the agent as its run ended; check what it did";
/// The tool call an approval asked to run, keyed by the approval.
pub(crate) const APPROVAL_TOOL_CALL: &str = "approval_tool_call";
/// Marks an approval whose Allow or answer may have reached the agent: its
/// tool may run even if Stop cancels the request afterwards.
const ALLOW_HANDED: &str = "approval_allow_handed";

/// Why the failed tool call `key` never ran, from the request that asked to
/// run it: the person denied it, or Stop cancelled it before any Allow could
/// reach the agent.
fn not_run(tx: &store::Write<'_>, run: RunId, key: &str) -> store::Result<Option<NotRun>> {
    for a in tx.read().list::<Approval>("approval")? {
        if a.run_id != run
            || tx
                .read()
                .get::<String>(APPROVAL_TOOL_CALL, a.approval_id)?
                .as_deref()
                != Some(key)
        {
            continue;
        }
        return Ok(match a.state {
            ApprovalState::Denied => Some(NotRun::Denied),
            ApprovalState::ResolvedElsewhere if a.outcome == Some(ApprovalOutcome::Denied) => {
                Some(NotRun::Denied)
            }
            ApprovalState::Cancelled
                if tx
                    .read()
                    .get::<bool>(ALLOW_HANDED, a.approval_id)?
                    .is_none() =>
            {
                Some(NotRun::Stopped)
            }
            _ => None,
        });
    }
    Ok(None)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct QueuePermit {
    pub task_revision: u64,
    pub queue_revision: u64,
    pub remaining: u32,
    pub principal: protocol::auth::Principal,
    #[serde(default)]
    pub expires_at: Option<protocol::time::Timestamp>,
}

/// The message that waited in Next up for a turn started with another
/// message, which carried it along: run id → its timeline sequence.
const CARRIED_MESSAGE: &str = "carried_start_message";

impl CompanionHub {
    /// Correlate the admitted prompt with its journal sequence before dispatch.
    /// Queued prompts acquire a run only when Next up actually starts them.
    pub(crate) fn track_start_message(
        &self,
        tx: &mut store::Write<'_>,
        task_id: TaskId,
        run_id: Option<RunId>,
        seq: u64,
    ) -> store::Result<()> {
        if let Some(id) = run_id {
            tx.put("start_message", id, &seq)
        } else {
            tx.put("queued_start_message", task_id, &seq)
        }
    }

    fn settle_start_message(
        &self,
        tx: &mut store::Write<'_>,
        run: &Run,
        state: DeliveryState,
    ) -> store::Result<()> {
        if let Some(seq) = tx.read().get::<u64>(CARRIED_MESSAGE, run.run_id)? {
            tx.delete(CARRIED_MESSAGE, run.run_id)?;
            self.settle_message(tx, run, seq, state)?;
        }
        let Some(message_seq) = tx.read().get::<u64>("start_message", run.run_id)? else {
            // Older runs have no correlation; never guess at a follow-up.
            return Ok(());
        };
        tx.delete("start_message", run.run_id)?;
        self.settle_message(tx, run, message_seq, state)
    }

    fn settle_message(
        &self,
        tx: &mut store::Write<'_>,
        run: &Run,
        message_seq: u64,
        state: DeliveryState,
    ) -> store::Result<()> {
        let event = Event::TimelineAppended {
            task_id: run.task_id,
            item: TimelineItem {
                seq: 0,
                at: self.inner.clock.now(),
                run_id: Some(run.run_id),
                item: TimelineBody::Delivery {
                    message_seq,
                    state,
                    withdrawn_by_stop: false,
                    withdrawn_by_revocation: false,
                },
            },
        };
        if state == DeliveryState::NativeDelivered {
            tx.emit_from(
                event,
                EventSource::Harness {
                    harness: run.harness,
                },
                Provenance::Native,
            )?;
        } else {
            tx.emit(event)?;
        }
        Ok(())
    }

    fn message_lock(&self, id: RunId) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.inner.message_locks.lock().expect("message locks");
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&id).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(id, std::sync::Arc::downgrade(&lock));
        lock
    }

    pub(crate) fn new_run(
        &self,
        tx: &mut store::Write<'_>,
        task: &mut Task,
        command_id: CommandId,
        text: Option<String>,
        bundle: Option<Box<AnnotationBundle>>,
        read_only: bool,
    ) -> store::Result<RunId> {
        // A read-only turn answers in Ask whatever the task's mode, so it
        // takes no writer and can change nothing.
        let mode = if read_only { TaskMode::Ask } else { task.mode };
        if task
            .launch_policy
            .is_some_and(|p| !p.supports(task.harness))
            || (task.launch_policy.is_some() && task.kind != TaskKind::Coding)
        {
            return Err(unsupported("Launch flags for this task and agent").into());
        }
        let catalog: Vec<protocol::capability::HarnessInfo> =
            tx.read().get("meta", "harnesses")?.unwrap_or_default();
        let info = catalog
            .iter()
            .find(|info| info.kind == task.harness)
            .ok_or_else(|| unsupported("Selected harness readiness"))?;
        domain::availability(&info.capabilities.start_run)?;
        domain::task_mode(info, mode)?;
        if task.current_run.is_some() {
            domain::availability(&info.capabilities.resume_session)?;
        }
        if task.kind == TaskKind::ComputerUse && mode != TaskMode::Work {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Computer-use tasks require Work mode",
            )
            .into());
        }
        let mut project: Project = tx.read().require("project", task.project_id)?;
        if mode == TaskMode::Work
            && let Some(writer) = project.active_writer
        {
            if let Some(holder) = tx.read().get::<Run>("run", writer)?
                && helpers_finishing(&holder)
            {
                let holder: Task = tx.read().require("task", holder.task_id)?;
                return Err(error(
                    ErrorCode::Conflict,
                    format!(
                        "Helpers from “{}” are still finishing in this project. This can start once they end, or stop them.",
                        holder.title
                    ),
                )
                .into());
            }
            return Err(error(
                ErrorCode::Conflict,
                "Project already has an active or uncertain writer",
            )
            .into());
        }
        // A turn that ended while what it started may still run keeps its
        // task. Replacing it would leave nothing to stop that with, and
        // nothing to raise for the person if it can't be confirmed ended.
        if let Some(id) = task.current_run
            && let Some(current) = tx.read().get::<Run>("run", id)?
            && still_finishing(&current)
        {
            return Err(error(
                ErrorCode::Conflict,
                "Something this task started earlier hasn’t ended yet. Stop it, or wait for it to end, then continue.",
            )
            .into());
        }
        let previous_mode = tx
            .read()
            .get::<Task>("task", task.task_id)?
            .map(|previous| previous.mode);
        let (resume, resume_from_mode) = if let Some(id) = task.current_run {
            let previous: Run = tx.read().require("run", id)?;
            if previous.harness != task.harness {
                (None, None)
            } else if let Some(session) = previous.native_session {
                (Some(session), previous.mode.or(previous_mode))
            } else if let Some(intent) = tx.read().get::<RunIntent>("run_intent", id)? {
                // A failed launch never established a session. Carry the durable
                // intent forward, including the session's mode (not the failed
                // attempt's mode), without fabricating native acknowledgement.
                (intent.resume, intent.resume_from_mode)
            } else {
                (None, previous_mode)
            }
        } else {
            (None, previous_mode)
        };
        // The next message waiting in Next up goes with this turn, ahead of
        // anything sent with it. A side question leaves it waiting.
        let continuation = tx
            .read()
            .get::<String>(crate::runs::queue_completion::CONTINUATION, task.task_id)?;
        let leaves_queue = !(read_only && continuation.is_some());
        let carried = continuation.is_some() && (text.is_some() || bundle.is_some());
        let waiting_markup = tx
            .read()
            .get::<Box<AnnotationBundle>>(
                crate::runs::queue_completion::CONTINUATION_BUNDLE,
                task.task_id,
            )?
            .filter(|_| !read_only);
        // One turn takes one piece of markup.
        if waiting_markup.is_some() && bundle.is_some() {
            return Err(error(
                ErrorCode::Conflict,
                "The message waiting in Next up for this task has markup of its own. Start it from Next up, or remove it there, then send this markup.",
            )
            .into());
        }
        let bundle = match waiting_markup {
            Some(waiting) => {
                tx.delete(
                    crate::runs::queue_completion::CONTINUATION_BUNDLE,
                    task.task_id,
                )?;
                Some(waiting)
            }
            None => bundle,
        };
        let text = match continuation.filter(|_| !read_only) {
            Some(waiting) => {
                tx.delete(crate::runs::queue_completion::CONTINUATION, task.task_id)?;
                match text {
                    Some(text) if waiting.is_empty() => Some(text),
                    Some(text) => Some(format!("{waiting}\n\n{text}")),
                    // Markup waited alone.
                    None if waiting.is_empty() => None,
                    None => Some(waiting),
                }
            }
            None => text,
        };
        let run_id = RunId::new();
        let now = self.inner.clock.now();
        let run = Run {
            human_resolution: None,
            helpers_checked: None,
            force_stoppable: false,
            cleanup_pending: false,
            run_id,
            task_id: task.task_id,
            harness: task.harness,
            requested_model: task.model.clone(),
            actual_model: None,
            requested_effort: task.effort.clone(),
            launch_policy: task.launch_policy,
            actual_effort: None,
            account_label: None,
            native_session: None,
            state: RunState::Admitted,
            admitted_at: now,
            dispatched_at: None,
            acknowledged_at: None,
            ended_at: None,
            children: Vec::new(),
            earlier_children: None,
            outcome: None,
            holds_writer: mode == TaskMode::Work,
            mode: Some(mode),
            revisions: None,
            follow_up: None,
        };
        if mode == TaskMode::Work {
            project.active_writer = Some(run_id);
        }
        if read_only {
            tx.put(SIDE_TURN, run_id, &task.state)?;
        }
        task.current_run = Some(run_id);
        task.state = TaskState::Dispatching;
        task.updated_at = now;
        task.revision += 1;
        tx.put("run", run_id, &run)?;
        tx.put("run_task_revision", run_id, &task.revision)?;
        // A check of a pull request decides it by this run, however it was
        // started or run again, and holds its merge until that reports.
        crate::github_watch::asked_to_check(tx, task, Some(run_id), now)?;
        if leaves_queue
            && let Some(seq) = tx.read().get::<u64>("queued_start_message", task.task_id)?
        {
            // The message sent with it has its own delivery to follow.
            let key = if carried {
                CARRIED_MESSAGE
            } else {
                "start_message"
            };
            tx.put(key, run_id, &seq)?;
            tx.delete("queued_start_message", task.task_id)?;
        }
        let original: (String, Option<Box<AnnotationBundle>>) =
            tx.read().require("prompt", task.task_id)?;
        if let Some(reviewed) = bundle.as_deref().or(if text.is_none() {
            original.1.as_deref()
        } else {
            None
        }) {
            crate::results::link_bundle(tx, task.task_id, Some(run_id), reviewed)?;
        }
        // A start's effect runs later. It dispatches, and on failure settles,
        // only the run it admitted, never one another device started since.
        tx.put("start_run", command_id, &run_id)?;
        tx.put(
            "run_intent",
            run_id,
            &RunIntent {
                command_id,
                resume,
                resume_from_mode,
                text,
                bundle,
            },
        )?;
        if mode == TaskMode::Work {
            tx.put("project", project.project_id, &project)?;
            tx.emit(Event::ProjectUpserted { project })?;
        }
        tx.emit(Event::RunUpserted { run: Box::new(run) })?;
        let mut q: QueueView = tx.read().require("queue", task.project_id)?;
        if mode == TaskMode::Work {
            q.writer = Some(run_id);
        }
        bump_queue_revision(tx, &mut q)?;
        if let Some(item) = q
            .items
            .iter_mut()
            .find(|i| i.task_id == task.task_id && i.is_pending())
            .filter(|_| leaves_queue)
        {
            item.state = QueueItemState::Started { run_id };
            item.authorization = QueueAuthorization::AskBeforeStart;
            item.authorization_expires_at = None;
            // The run holds the request now; Next up no longer waits on it.
            item.request = None;
        }
        forget_finished(tx, &mut q)?;
        tx.put("queue", q.project_id, &q)?;
        crate::runs::sync_queue_attention(tx, &q, self.inner.clock.now())?;
        tx.emit(Event::QueueChanged { queue: q })?;
        task.attention_count = blocking_attention_count(&tx.read(), task.task_id)?;
        Ok(run_id)
    }
    pub(crate) fn release_writer(
        &self,
        tx: &mut store::Write<'_>,
        project_id: ProjectId,
        run_id: RunId,
    ) -> store::Result<()> {
        let mut project: Project = tx.read().require("project", project_id)?;
        if project.active_writer == Some(run_id) {
            project.active_writer = None;
            tx.put("project", project_id, &project)?;
            tx.emit(Event::ProjectUpserted { project })?;
        }
        let mut q: QueueView = tx.read().require("queue", project_id)?;
        let released = q.writer == Some(run_id);
        if released {
            q.writer = None;
            bump_queue_revision(tx, &mut q)?;
            self.refresh_queue(tx, &mut q)?;
        }
        if forget_finished(tx, &mut q)? || released {
            tx.put("queue", project_id, &q)?;
            crate::runs::sync_queue_attention(tx, &q, self.inner.clock.now())?;
            tx.emit(Event::QueueChanged { queue: q })?;
        }
        Ok(())
    }
    /// Drops from `project_id`'s Next up the started work it no longer needs,
    /// and tells phones when that changes it.
    pub(crate) fn tidy_queue(
        &self,
        tx: &mut store::Write<'_>,
        project_id: ProjectId,
    ) -> store::Result<()> {
        let Some(mut q) = tx.read().get::<QueueView>("queue", project_id)? else {
            return Ok(());
        };
        if forget_finished(tx, &mut q)? {
            tx.put("queue", project_id, &q)?;
            crate::runs::sync_queue_attention(tx, &q, self.inner.clock.now())?;
            tx.emit(Event::QueueChanged { queue: q })?;
        }
        Ok(())
    }
    pub(crate) fn refresh_queue(
        &self,
        tx: &store::Write<'_>,
        queue: &mut QueueView,
    ) -> store::Result<()> {
        // Run all's go-ahead outlasts the day a permit lasts, and an edit
        // to Next up that asks again of the rest.
        crate::github_run::keep_running(tx, queue, self.inner.clock.now())?;
        // Only pending items consult prerequisites. Read each referenced run
        // once from its diff-free summary, never from retained run payloads.
        let dependencies: HashSet<_> = queue
            .items
            .iter()
            .filter(|i| i.is_pending())
            .flat_map(|i| i.depends_on.iter().copied())
            .collect();
        let runs: HashSet<_> = queue
            .items
            .iter()
            .filter(|i| dependencies.contains(&i.item_id))
            .filter_map(|i| match i.state {
                QueueItemState::Started { run_id } => Some(run_id),
                _ => None,
            })
            .collect();
        let mut outcomes = HashMap::new();
        for run_id in runs {
            if let Some(r) = tx.read().history_run(run_id)? {
                outcomes.insert(
                    run_id,
                    if r.outcome
                        .as_ref()
                        .is_some_and(|o| !o.still_active.is_empty() || !o.unknowns.is_empty())
                        || r.children
                            .iter()
                            .any(|c| matches!(c.state, ChildState::Active | ChildState::Unknown))
                    {
                        RunState::ReconciliationRequired
                    } else {
                        r.state
                    },
                );
            }
        }
        let snapshot = queue.clone();
        for item in &mut queue.items {
            if item.is_pending() {
                if let Some(prompt) = tx
                    .read()
                    .get::<String>(crate::runs::queue_completion::CONTINUATION, item.task_id)?
                {
                    let bundle = tx.read().get(
                        crate::runs::queue_completion::CONTINUATION_BUNDLE,
                        item.task_id,
                    )?;
                    item.request = Some(QueuedRequest { prompt, bundle });
                } else if let Some((prompt, bundle)) =
                    tx.read()
                        .get::<(String, Option<Box<AnnotationBundle>>)>("prompt", item.task_id)?
                {
                    item.request = Some(QueuedRequest { prompt, bundle });
                }
                if item.authorization == QueueAuthorization::AutoStart
                    && item
                        .authorization_expires_at
                        .is_none_or(|end| self.inner.clock.now() >= end)
                {
                    item.authorization = QueueAuthorization::AskBeforeStart;
                    item.authorization_expires_at = None;
                }
                item.state = match domain::queue::dependency_block(&snapshot, item, &outcomes) {
                    Some(reason) => QueueItemState::Blocked { reason },
                    None if item.authorization == QueueAuthorization::AskBeforeStart => {
                        QueueItemState::AwaitingStart
                    }
                    None => QueueItemState::Waiting,
                };
            }
        }
        Ok(())
    }
    /// Takes `item`'s go-ahead back: it waits in `project_id`'s Next up
    /// for the person, who is asked.
    fn ask_instead(
        &self,
        tx: &mut store::Write<'_>,
        project_id: ProjectId,
        item: &QueueItem,
    ) -> store::Result<()> {
        let mut q: QueueView = tx.read().require("queue", project_id)?;
        if let Some(waiting) = q.items.iter_mut().find(|i| i.item_id == item.item_id) {
            waiting.authorization = QueueAuthorization::AskBeforeStart;
            waiting.authorization_expires_at = None;
        }
        tx.delete("queue_permit", item.item_id)?;
        // Run all would only give it the go-ahead again.
        crate::github_run::let_go(tx, project_id, item.task_id)?;
        self.refresh_queue(tx, &mut q)?;
        tx.put("queue", project_id, &q)?;
        crate::runs::sync_queue_attention(tx, &q, self.inner.clock.now())?;
        tx.emit(Event::QueueChanged { queue: q })?;
        Ok(())
    }
    pub(crate) async fn advance_queues(&self) -> Result<(), ProtocolError> {
        if *self.inner.shutdown.borrow() {
            return Ok(());
        }
        let tasks = self.write(|tx| {
            if *self.inner.shutdown.borrow() {
                return Ok(Vec::new());
            }
            let mut starts = Vec::new();
            for queue in tx.read().list::<QueueView>("queue")? {
                loop {
                    let mut q: QueueView = tx.read().require("queue", queue.project_id)?;
                    if q.paused {
                        break;
                    }
                    self.refresh_queue(tx, &mut q)?;
                    let next = q.items.iter().find(|i| i.is_pending()).cloned();
                    tx.put("queue", q.project_id, &q)?;
                    crate::github_run::settle(tx, q.project_id, false, self.inner.clock.now())?;
                    if let Some(item) = next {
                        if item.authorization != QueueAuthorization::AutoStart
                            || !matches!(item.state, QueueItemState::Waiting)
                        {
                            crate::runs::sync_queue_attention(tx, &q, self.inner.clock.now())?;
                            tx.emit(Event::QueueChanged { queue: q })?;
                            break;
                        }
                        let mut task: Task = tx.read().require("task", item.task_id)?;
                        if task.mode == TaskMode::Work && q.writer.is_some() {
                            break;
                        }
                        // A local review's verifier goes first: what is
                        // queued after it may act on what it confirms.
                        if task.mode == TaskMode::Work
                            && crate::review_loop::holds_queue(&tx.read(), q.project_id)?
                        {
                            break;
                        }
                        // A task's next message waits while a side question
                        // it was asked meanwhile is still answering.
                        if !crate::runs::queue_completion::waits_to_start(&tx.read(), &task)? {
                            break;
                        }
                        // Each authorization covers this exact item once. GUI runs
                        // always require an explicit supervised start.
                        if task.kind == TaskKind::ComputerUse {
                            break;
                        }
                        // A go-ahead that no longer holds, used, given for
                        // the task as it was or by someone no longer allowed
                        // to start tasks, never starts it. Kept waiting as if
                        // about to start, it would hold up everything after
                        // it with nothing said: the person is asked instead.
                        let permit = tx
                            .read()
                            .get::<QueuePermit>("queue_permit", item.item_id)?
                            .filter(|permit| {
                                domain::queue::permit_valid(
                                    &q,
                                    &item,
                                    task.revision,
                                    &domain::queue::StartPermit {
                                        task_revision: permit.task_revision,
                                        queue_revision: permit.queue_revision,
                                        remaining: permit.remaining,
                                        expires_at: permit.expires_at,
                                    },
                                    self.inner.clock.now(),
                                ) && Self::current_caller(
                                    &tx.read(),
                                    &Caller {
                                        principal: permit.principal,
                                        grants: protocol::auth::Grants::owner(),
                                    },
                                )
                                .is_ok_and(|c| c.grants.tasks)
                            });
                        let Some(mut permit) = permit else {
                            self.ask_instead(tx, queue.project_id, &item)?;
                            break;
                        };
                        let id = CommandId::new();
                        let envelope = CommandEnvelope {
                            command_id: id,
                            issued_at: self.inner.clock.now(),
                            expected_revision: Some(task.revision),
                            expires_at: None,
                            expires_in_ms: None,
                            body: Command::ResumeTask {
                                mode: None,
                                task_id: task.task_id,
                                text: None,
                                bundle: None,
                                carried_plan: None,
                                read_only: false,
                                model: None,
                                effort: None,
                            },
                        };
                        permit.remaining = 0;
                        let started = tx.attempt(|tx| {
                            tx.put("queue_permit", item.item_id, &permit)?;
                            let run = self.new_run(tx, &mut task, id, None, None, false)?;
                            let seq = tx.emit(Event::CommandProgress {
                                result: None,
                                error: None,
                                command_id: id,
                                stage: CommandStage::HostAccepted,
                                detail: Some("One authorized queue item admitted".into()),
                            })?;
                            tx.command(&store::StoredCommand {
                                result: None,
                                error: None,
                                envelope,
                                principal: permit.principal,
                                receipt: CommandReceipt {
                                    command_id: id,
                                    status: ReceiptStatus::Accepted,
                                    seq: Some(seq),
                                    result: None,
                                    error: None,
                                },
                                stage: CommandStage::HostAccepted,
                                pending: true,
                            })?;
                            tx.put("task", task.task_id, &task)?;
                            tx.emit(Event::TaskUpserted { task: task.clone() })?;
                            // A start the person asked for is refused when it would
                            // leave the app unable to load; this one is too.
                            crate::history::admit(&tx.read())?;
                            Ok(run)
                        });
                        let run = match started {
                            Ok(run) => run,
                            Err(store::Error::Rule(e)) if e.code == ErrorCode::LimitExceeded => {
                                // Undone whole. It waits for a person, who is told why
                                // when they try to start it.
                                self.ask_instead(tx, queue.project_id, &item)?;
                                break;
                            }
                            Err(e) => return Err(e),
                        };
                        starts.push((task.task_id, run));
                    } else {
                        break;
                    }
                }
            }
            Ok(starts)
        })?;
        // Each start stands on its own. One the agent refuses has already
        // ended with its reason and must not strand the others admitted here.
        let mut outcome = Ok(());
        for (task, run) in tasks {
            if let Err(e) = self.dispatch_task(task, run).await
                && outcome.is_ok()
            {
                outcome = Err(e);
            }
        }
        outcome
    }
    pub(crate) fn dispatch_task(
        &self,
        task_id: TaskId,
        id: RunId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ProtocolError>> + Send + '_>>
    {
        Box::pin(async move {
            let access_lock = self.access_lock(task_id);
            let access_guard = access_lock.lock().await;
            if *self.inner.shutdown.borrow() {
                return Ok(());
            }
            let Some((task, run, intent, project, prompt)) = self.write(|tx| {
                let task: Task = tx.read().require("task", task_id)?;
                let mut run: Run = tx.read().require("run", id)?;
                if run.state != RunState::Admitted {
                    // A Stop admitted first ends the run itself.
                    if tx.read().get::<bool>("stopped", id)?.unwrap_or(false) {
                        return Ok(None);
                    }
                    return Err(error(
                        ErrorCode::Conflict,
                        "Run was already dispatched or stopped",
                    )
                    .into());
                }
                let intent: RunIntent = tx.read().require("run_intent", id)?;
                let project: Project = tx.read().require("project", task.project_id)?;
                let prompt: (String, Option<Box<AnnotationBundle>>) =
                    tx.read().require("prompt", task_id)?;
                run.state = RunState::Dispatching;
                run.dispatched_at = Some(self.inner.clock.now());
                tx.put("run", id, &run)?;
                tx.emit(Event::RunUpserted {
                    run: Box::new(run.clone()),
                })?;
                Ok(Some((task, run, intent, project, prompt)))
            })?
            else {
                return Ok(());
            };
            // Nothing reaches the agent before the adapter starts it, so a
            // failure here ends the run with its reason and leaves nothing to
            // reconcile.
            let prepared = async {
                // A native tool started in a missing folder fails in ways that
                // hide the cause (Claude Code then reads as signed out).
                if !tokio::fs::metadata(&project.host_path)
                    .await
                    .is_ok_and(|m| m.is_dir())
                {
                    return Err(error(
                        ErrorCode::NotFound,
                        format!(
                            "The project folder {} can’t be found. Reconnect its drive or move it back, then try again.",
                            project.host_path
                        ),
                    ));
                }
                let (github_data, outside) = self.github_start_content(&task, &project).await?;
                let task = self.hold_github_content(&task, run.run_id, outside, intent.resume.is_some()).await?;
                let github_held = task.github.is_some() && self.github_tools_held(&task)?;
                // A review using the screen may start a helper on the host.
                // Its content must pass the same gate before that helper.
                if task.kind == TaskKind::ComputerUse {
                    self.write(|tx| {
                        let mut run: Run = tx.read().require("run", id)?;
                        run.state = RunState::Admitted;
                        tx.put("run", id, &run)
                    })?;
                    if self.app_setup_first(task_id, id).await {
                        return Ok(None);
                    }
                    self.write(|tx| {
                        let mut run: Run = tx.read().require("run", id)?;
                        if run.state == RunState::Admitted {
                            run.state = RunState::Dispatching;
                            tx.put("run", id, &run)?;
                        }
                        Ok(())
                    })?;
                }
                self.instruction_baseline(run.run_id, &project, &task)
                    .await?;
                let adapter = self
                    .inner
                    .harnesses
                    .iter()
                    .find(|a| a.kind() == task.harness)
                    .ok_or_else(|| unsupported("Selected harness"))?;
                let mut input = if intent.text.is_some() || intent.bundle.is_some() {
                    self.prompt_input(
                        intent.text.as_deref().unwrap_or_default(),
                        intent.bundle.as_deref(),
                    )?
                } else {
                    self.prompt_input(&prompt.0, prompt.1.as_deref())?
                };
                // A review task's rules open each new session of it, the
                // same for every agent. Ordinary tasks have none.
                if task.review.is_some() && intent.resume.is_none() {
                    let rules: Option<String> =
                        self.read(|r| r.get(crate::review_rules::PROMPT, task_id))?;
                    if let Some(rules) = rules {
                        input.text.push_str("\n\n");
                        input.text.push_str(&rules);
                    }
                }
                let mode = run_mode(&run, &task);
                let side = self.side_turn(run.run_id)?;
                // A side turn of a task held to its access only reads it.
                let task = Task {
                    access: crate::access::held_to(task.access.as_ref(), side),
                    ..task.clone()
                };
                // A run that only reads gets no tools, so it starts nothing
                // itself: the new work it reports waits for the person. A
                // conversation's turn acts within its access, and has Ophio's
                // tools as a task doing Work does; each checks that access.
                let talks = task.conversation && task.access.is_some();
                let acts = !side && (mode == TaskMode::Work || talks);
                // A turn answering the person on the checklist, for an agent
                // put away, reads it and answers, and has no other tool.
                let answers = !side
                    && mode == TaskMode::Ask
                    && self.read(|r| crate::checklist::answers(r, task_id))?;
                let bridge = if self.inner.bridge.is_some() && (acts || answers) && !github_held {
                    Some(self.spawn_config(run.run_id).await?)
                } else {
                    None
                };
                // Started before the agent is, which has only the screen.
                let app = match bridge {
                    Some(_) if acts => self.bring_app_up(&task, &project, run.run_id).await?,
                    _ => None,
                };
                // Granted once the app is up: the lease lapses three seconds
                // after the person stops watching, and asking how to start
                // the app takes them elsewhere.
                if task.kind == TaskKind::ComputerUse {
                    self.grant_agent(
                        run.run_id,
                        task.target_id.ok_or_else(|| {
                            error(ErrorCode::InvalidRequest, "Computer use target required")
                        })?,
                    )
                    .await?;
                }
                let (mut env, mut given) = self
                    .accounts_for_run(run.run_id, task_id, task.harness)
                    .await?;
                if github_held {
                    // Accounts ordinarily gives every signed-in service to
                    // a task. Outside content needs only its agent's provider.
                    env.0.retain(|(key, _)| match task.harness {
                        protocol::capability::HarnessKind::ClaudeCode => key == "ANTHROPIC_API_KEY",
                        protocol::capability::HarnessKind::Codex => key == "OPENAI_API_KEY",
                        _ => false,
                    });
                    given.clear();
                }
                env.0.push((ports::TASK_ENV.into(), task_id.to_string()));
                let scratch = self.prepare_scratch(task_id, &mut env).await;
                let github = if task.github.as_ref().is_some_and(|g| !g.link.read_only()) {
                    if let Some(data) = github_data {
                        input.text.push_str("\n\n");
                        input.text.push_str(&data);
                    }
                    if github_held {
                        Some("This task read outside content. GitHub tools and direct GitHub access are off. Work from the supplied snapshot and project files, and give the person your changes and what you would post.".into())
                    } else if bridge.as_ref().is_some_and(|b| b.github) {
                        Some(self.github_run(&task, &project, &mut env).await?)
                    } else { None }
                } else {
                    match self.github_read_run(&task, &project, app.as_deref(), github_data).await? {
                        Some((framing, data)) => {
                            if let Some(data) = data {
                                input.text.push_str("\n\n");
                                input.text.push_str(&data);
                            }
                            Some(framing)
                        }
                        None => None,
                    }
                };
                if let Some(app) = app.as_deref().filter(|_| github.is_none()) {
                    input.text = format!("{app}\n\n{}", input.text);
                }
                let sandbox = self.conversation_sandbox(&task, run.run_id, &project, scratch.as_deref(), given, bridge.is_some())?;
                // A conversation, and a task carried on from one, is held
                // to its access, and runs as Work inside the sandbox that
                // holds it. A turn answering on the checklist for one keeps
                // that sandbox's reach, read-only, and still runs as Ask:
                // its agent has only the reading tools and those two.
                let controlled = task.access.is_some() && !answers;
                self.write(|tx| tx.put("run_access", run.run_id, &task.access))?;
                let reach = match task.access.as_ref() {
                    Some(access) => Some(self.read(|r| self.access_reach(r, access))?),
                    None => None,
                };
                // Told once per native session, then only when it changes.
                let told = match &reach {
                    Some(reach) => self.access_note(&task, intent.resume.is_some(), reach, scratch.is_some())?,
                    None => None,
                };
                if let Some((note, _)) = &told {
                    input.text = format!("{note}\n\n{}", input.text);
                }
                let spec = ports::RunSpec {
                    run_id: run.run_id,
                    task_id,
                    mode: if controlled { TaskMode::Work } else { mode },
                    cwd: PathBuf::from(&project.host_path),
                    prompt: input,
                    model: task.model.clone(),
                    effort: task.effort.clone(),
                    launch_policy: if controlled { Some(LaunchPolicy::BypassPermissions) } else { task.launch_policy },
                    resume_session: intent.resume.clone(),
                    resume_from_mode: if controlled { None } else { intent.resume_from_mode },
                    mission: task.mission.clone(),
                    branch: task.branch.clone(),
                    bridge,
                    project_settings: self
                        .read(|r| Self::project_settings(r, project.project_id))?,
                    env,
                    github,
                    scratch,
                    questions: self.read(Self::agent_preferences)?.questions,
                    sandbox,
                    web: controlled && task.access.as_ref().is_some_and(|a| a.internet),
                    disabled: self.disabled_for_run(&crate::disabled::RunPlace {
                        cwd: Path::new(&project.host_path),
                        project_ids: self.projects_at(Path::new(&project.host_path))?,
                        project_settings: self
                            .read(|r| Self::project_settings(r, project.project_id))?,
                    })?,
                    start: self
                        .run_start(task_id, Path::new(&project.host_path))
                        .await?,
                };
                self.record_repository_revision(run.run_id, true).await?;
                Ok::<_, ProtocolError>(Some((adapter, spec, told.map(|(_, access)| access))))
            }
            .await;
            let (adapter, spec, told) = match prepared {
                Ok(Some(prepared)) => prepared,
                Ok(None) => return Ok(()),
                Err(e) => {
                    // Withdraw a bridge credential or desktop grant issued above.
                    let _ = self.revoke_run(run.run_id).await;
                    self.release_accounts(run.run_id);
                    // A Stop admitted meanwhile is why this start could not go on.
                    if self
                        .read(|r| r.get::<bool>("stopped", run.run_id))?
                        .unwrap_or(false)
                    {
                        return self.finish_run(run.run_id, RunState::Interrupted, None);
                    }
                    return self.refuse_start(task_id, run.run_id, e.code, e.message);
                }
            };
            if let Some(sandbox) = &spec.sandbox {
                self.inner
                    .sandbox_runs
                    .lock()
                    .unwrap()
                    .insert(run.run_id, sandbox.clone());
            }
            let started =
                tokio::time::timeout(Duration::from_secs(30), adapter.start_run(spec)).await;
            if !matches!(started, Ok(Ok(_))) {
                self.inner.sandbox_runs.lock().unwrap().remove(&run.run_id);
                self.release_accounts(run.run_id);
            }
            let mut started = match started {
                Ok(Ok(s)) => s,
                // These refusals come before the prompt is written, so nothing
                // reached the agent and there is nothing to reconcile.
                Ok(Err(HarnessError::NotInstalled(name))) => {
                    return self.refuse_start(
                        task_id,
                        run.run_id,
                        ErrorCode::Unsupported,
                        format!("{name} isn’t installed on this computer."),
                    );
                }
                Ok(Err(HarnessError::AccountNotReady(why))) => {
                    return self.refuse_start(task_id, run.run_id, ErrorCode::AccountNotReady, why);
                }
                Ok(Err(HarnessError::Unsupported(why))) => {
                    return self.refuse_start(task_id, run.run_id, ErrorCode::Unsupported, why);
                }
                Ok(Err(HarnessError::SpawnFailed(why))) => {
                    return self.refuse_start(task_id, run.run_id, ErrorCode::NativeFailure, why);
                }
                _ => {
                    self.finish_run(run.run_id, RunState::ReconciliationRequired, None)?;
                    return Err(error(
                        ErrorCode::OutcomeUnknown,
                        "The agent hasn’t confirmed it got the task. Check the task before starting it again",
                    ));
                }
            };
            // Serialize insertion with shutdown's snapshot; a late spawn owns
            // its cleanup instead of disappearing from the shutdown census.
            {
                let mut runs = self.inner.runs.lock().await;
                if *self.inner.shutdown.borrow() {
                    drop(runs);
                    let _ = started.control.shutdown().await;
                    self.release_accounts(run.run_id);
                    self.finish_run(run.run_id, RunState::ReconciliationRequired, None)?;
                    return Ok(());
                }
                runs.insert(run.run_id, started.control.clone());
            }
            drop(access_guard);
            self.guard_run(run.run_id);
            self.write(|tx| {
                let mut owned: Run = tx.read().require("run", run.run_id)?;
                owned.force_stoppable = true;
                tx.put("run", owned.run_id, &owned)?;
                if let Some(access) = &told {
                    tx.put(crate::access::TOLD, task_id, access)?;
                }
                tx.emit(Event::RunUpserted {
                    run: Box::new(owned),
                })?;
                Ok(())
            })?;
            if self.run_stopped(run.run_id)? {
                let _ = started.control.interrupt().await;
            }
            let hub = self.clone();
            let mut shutdown = self.inner.shutdown.subscribe();
            self.spawn(async move {
                let mut exit = None;
                let mut settled = false;
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        event = started.events.recv() => {
                            let Some(mut event) = event else { break };
                            hub.saw(run.run_id, &event);
                            if matches!(event, HarnessEvent::Exited { .. }) {
                                // Keep the evidence until finalization succeeds.
                                // Mask it before releasing the run's secrets.
                                hub.mask_run_event(run.run_id, &mut event);
                                exit = Some(event.clone());
                            }
                            if let Err(e) = hub.harness_event(run.run_id, event).await {
                                tracing::error!(code=?e.code, "harness event persistence failed");
                                if exit.is_none() {
                                    let _ = started.control.interrupt().await;
                                }
                                break;
                            }
                            if exit.is_some() {
                                settled = true;
                                break;
                            }
                        }
                    }
                }
                // This tracked worker now owns finalization, including retries.
                // Deregister first: a Force stop that found the control has
                // already recorded that its end is unconfirmed, and one that
                // comes later finds nothing to stop. The watchdog must not
                // interrupt a finished agent while its exit waits for storage.
                hub.inner.runs.lock().await.remove(&run.run_id);
                hub.inner.sandbox_runs.lock().unwrap().remove(&run.run_id);
                hub.unguard_run(run.run_id);
                hub.release_accounts(run.run_id);
                loop {
                    let result = if settled {
                        Ok(())
                    } else if let Some(event) = &exit {
                        hub.harness_event(run.run_id, event.clone()).await
                    } else {
                        hub.child_control_closed(run.run_id)
                            .and_then(|_| hub.finish_run(run.run_id, RunState::ReconciliationRequired, None))
                            .and_then(|_| hub.events_lost(run.run_id))
                    };
                    settled = result.is_ok();
                    // Fence input even when recording the end failed.
                    let revoked = hub.revoke_run(run.run_id).await;
                    match result.and(revoked) {
                        Ok(()) => break,
                        Err(e) => tracing::error!(code=?e.code, "run finalization failed; retaining recovery responsibility"),
                    }
                    // Shutdown must not wait for a broken disk. Startup parks
                    // any run still unsettled on disk for reconciliation.
                    if *shutdown.borrow() {
                        break;
                    }
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                    }
                }
            });
            Ok(())
        })
    }
    /// Ends a run the agent refused before its prompt was written, saying why.
    fn refuse_start(
        &self,
        task_id: TaskId,
        run_id: RunId,
        code: ErrorCode,
        reason: String,
    ) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let run: Run = tx.read().require("run", run_id)?;
            self.settle_start_message(tx, &run, DeliveryState::Failed)?;
            tx.emit(Event::TimelineAppended {
                task_id,
                item: TimelineItem {
                    seq: 0,
                    at: self.inner.clock.now(),
                    run_id: Some(run_id),
                    item: TimelineBody::Note {
                        text: format!("The agent didn’t start. {reason}"),
                    },
                },
            })?;
            Ok(())
        })?;
        self.finish_run(run_id, RunState::Failed, None)?;
        Err(error(code, reason))
    }
    pub(crate) fn run_stopped(&self, id: RunId) -> Result<bool, ProtocolError> {
        self.read(|r| stopped(r, id))
    }
    async fn harness_event(&self, id: RunId, mut event: HarnessEvent) -> Result<(), ProtocolError> {
        self.conversation_block_event(id, &event)?;
        // Masked before it is cut to fit, so no part of a value survives.
        self.mask_run_event(id, &mut event);
        let mut event = crate::native_text::fitted(event);
        if let HarnessEvent::ApprovalRequested(native) = &event
            && !crate::native_text::native_approval_fits(native)
        {
            // Too large to show on the phone whole, and so to decide there.
            let control = self.inner.runs.lock().await.get(&id).cloned();
            if let Some(control) = control {
                let deny = ApprovalDecision::Deny {
                    reason: Some("The request is too large to review on the phone.".into()),
                };
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    control.resolve_approval(&native.native_request_id, &deny),
                )
                .await;
            }
            event = HarnessEvent::Warning {
                text: "The agent asked for approval of a request too large to show on the phone, so it was declined.".into(),
            };
        }
        if let HarnessEvent::ReadinessWarning { reason } = &event {
            self.native_readiness_warning(id, reason)?;
        }
        if let HarnessEvent::UsageLimited { resets_at } = event {
            let run: Run = self.read(|r| r.require("run", id))?;
            return self.usage_limited(run.harness, resets_at);
        }
        if matches!(event, HarnessEvent::CleanupPending) {
            // The run stays as it is, holding what it holds and stoppable,
            // until the adapter confirms its processes ended.
            return self.write(|tx| set_cleanup_pending(tx, id, true));
        }
        let parent_ended = matches!(event, HarnessEvent::ParentEnded { .. });
        let event = match event {
            HarnessEvent::ParentEnded { exit } => HarnessEvent::Exited {
                exit,
                outcome: None,
            },
            other => other,
        };
        // A native echo can arrive before send_message returns. Serialize its
        // application with the durable dispatch result. The urgent
        // interrupt path remains separate from message submission.
        let lock = matches!(
            event,
            HarnessEvent::MessageDelivery { .. } | HarnessEvent::Exited { .. }
        )
        .then(|| self.message_lock(id));
        let _message_guard = match &lock {
            Some(lock) => Some(lock.lock().await),
            None => None,
        };
        let run: Run = self.read(|r| r.require("run", id))?;
        if let HarnessEvent::MessageDelivery { message_ref, state } = event {
            return self.write(|tx| {
                let mut messages: Vec<InFlightMessage> =
                    tx.read().get("message_delivery", id)?.unwrap_or_default();
                let Some(index) = messages.iter().position(|message| {
                    message.message_ref == Some(message_ref)
                        && !message.undispatched
                        && advances_delivery(message.state, state)
                }) else {
                    // Includes the initial turn and replies already confirmed
                    // as delivered. No conversation row or guessed state.
                    return Ok(());
                };
                let message_seq = messages[index].seq;
                if in_flight(state) {
                    messages[index].state = state;
                } else {
                    messages.remove(index);
                }
                tx.put("message_delivery", id, &messages)?;
                // A person's own withdrawal settles its record before this
                // echo; a record still here was withdrawn by the adapter.
                let withdrawn_by_stop = state == DeliveryState::Cancelled
                    && tx.read().get::<bool>("stopped", id)?.unwrap_or(false);
                tx.emit_from(
                    Event::TimelineAppended {
                        task_id: run.task_id,
                        item: TimelineItem {
                            seq: 0,
                            at: self.inner.clock.now(),
                            run_id: Some(id),
                            item: TimelineBody::Delivery {
                                message_seq,
                                state,
                                withdrawn_by_stop,
                                withdrawn_by_revocation: false,
                            },
                        },
                    },
                    EventSource::Harness {
                        harness: run.harness,
                    },
                    Provenance::Native,
                )?;
                Ok(())
            });
        }
        // A Claude run with a bound permission bridge gets its approvals from
        // that bridge alone, which records each with the route its reply
        // takes. A copy in the event stream would make the bridge refuse the
        // request as already recorded, or close it before the person decides.
        // Codex asks through its own app-server whether or not the computer
        // tools are bound, so its event stream is the only copy there is:
        // dropping it left the agent waiting on a question nobody could see.
        if matches!(
            event,
            HarnessEvent::ApprovalRequested(_) | HarnessEvent::ApprovalClosed { .. }
        ) && asks_through_bridge(run.harness)
            && self.read(|r| Ok(r.get::<bool>("bridge_bound", id)?.unwrap_or(false)))?
        {
            return Ok(());
        }
        // The agent's own questions, while the person has them off, go
        // straight back to it. A tool's request for input is still asked,
        // and the computer specialist answers to another task.
        if let HarnessEvent::ApprovalRequested(native) = &event
            && native.kind == ApprovalKind::Question
            && !native.questions.is_empty()
            && !self.questions_on()?
            && self.read(|r| {
                Ok(r.require::<Task>("task", run.task_id)?.kind != TaskKind::ComputerUse)
            })?
            && self
                .decline_natively(
                    id,
                    &native.native_request_id,
                    ports::harness::HOLD_QUESTIONS,
                )
                .await
        {
            return self.held_question(run.task_id, id, &native.title);
        }
        if let HarnessEvent::AgentTextDelta { message_key, text } = event {
            if text.is_empty() {
                // All of it held back until the next piece shows what it is.
                return Ok(());
            }
            self.live(LiveEvent::AgentTextDelta {
                task_id: run.task_id,
                run_id: id,
                message_key,
                text,
            });
            return Ok(());
        }
        if let HarnessEvent::Exited { exit, outcome } = event {
            if !parent_ended {
                self.write(|tx| {
                    tx.put("run_process_ended", id, &true)?;
                    set_cleanup_pending(tx, id, false)
                })?;
                self.child_control_closed(id)?;
            }
            let run: Run = self.read(|r| r.require("run", id))?;
            // An uncertain parent has an end timestamp, but only process exit
            // can finish its pending Force stop and release the writer.
            let finishing_force_stop = !parent_ended
                && !run.state.is_terminal()
                && self
                    .read(|r| r.get::<bool>("force_stopped", id))?
                    .unwrap_or(false);
            if run.ended_at.is_some() && !finishing_force_stop {
                // The parent turn ended earlier, while helpers still ran.
                // They have now ended with the process: the writer they kept
                // is free, and Next up held anything waiting on them.
                if !parent_ended {
                    self.helpers_ended(id)?;
                }
                return self.after_run_exit(id, run.task_id, run.state).await;
            }
            self.report_trail(protocol::reports::Trail::RunExit {
                task: run.task_id.to_string(),
                run: id.to_string(),
                exit: match &exit {
                    RunExit::Completed => "completed",
                    RunExit::Interrupted => "interrupted",
                    RunExit::Failed { .. } => "failed",
                    RunExit::Unknown { .. } => "unknown",
                }
                .into(),
                code: match &exit {
                    RunExit::Failed { .. } => Some("harness_failed".into()),
                    RunExit::Unknown { .. } => Some("outcome_unknown".into()),
                    _ => None,
                },
            });
            self.record_repository_revision(id, false).await?;
            let mut state = match exit {
                RunExit::Completed => RunState::Completed,
                RunExit::Interrupted => RunState::Interrupted,
                RunExit::Failed { .. } => RunState::Failed,
                RunExit::Unknown { .. } => RunState::ReconciliationRequired,
            };
            if outcome
                .as_ref()
                .is_some_and(|o| !o.still_active.is_empty() || !o.unknowns.is_empty())
            {
                state = RunState::ReconciliationRequired;
            }
            if self
                .read(|r| r.get::<bool>("force_stopped", id))?
                .unwrap_or(false)
            {
                state = RunState::Interrupted;
            }
            // The run's state alone doesn't say why; the adapter's reason does.
            let why = match &exit {
                RunExit::Failed { reason } if state == RunState::Failed => {
                    Some(format!("The agent stopped with an error. {reason}"))
                }
                RunExit::Unknown { reason } if state == RunState::ReconciliationRequired => Some(
                    format!("The agent’s result couldn’t be confirmed. {reason}"),
                ),
                _ => None,
            };
            self.end_run(id, state, outcome, parent_ended, why)?;
            return self.after_run_exit(id, run.task_id, state).await;
        }
        let child_stop = if let HarnessEvent::Child { native_ref, .. } = &event {
            self.inner
                .runs
                .lock()
                .await
                .get(&id)
                .map(|control| control.child_stop_availability(native_ref))
        } else {
            None
        };
        self.write(|tx| {
            let mut run: Run = tx.read().require("run", id)?;
            let mut task: Task = tx.read().require("task", run.task_id)?;
            let now = self.inner.clock.now();
            let mut timeline = None;
            match event {
                HarnessEvent::SessionStarted {
                    native_session,
                    actual_model,
                    actual_effort,
                    account_label,
                } => {
                    // Codex creates its session before submitting the prompt.
                    // Claude's init is emitted after it consumes the initial input.
                    if run.harness == protocol::capability::HarnessKind::ClaudeCode {
                        self.settle_start_message(tx, &run, DeliveryState::NativeDelivered)?;
                    }
                    run.native_session = Some(native_session);
                    run.actual_model = actual_model;
                    run.actual_effort = actual_effort;
                    run.account_label = account_label;
                    run.acknowledged_at = Some(now);
                    if run.state == RunState::Dispatching {
                        run.state = RunState::Working;
                        task.state = TaskState::Working;
                    }
                    let intent: RunIntent = tx.read().require("run_intent", id)?;
                    if let Some(mut c) = tx.read().command(intent.command_id)? {
                        c.pending = false;
                        c.stage = CommandStage::NativeAcknowledged;
                        tx.command(&c)?;
                        tx.emit(Event::CommandProgress {
                            result: None,
                            error: None,
                            command_id: intent.command_id,
                            stage: c.stage,
                            detail: None,
                        })?;
                    }
                    timeline = Some(TimelineBody::RunStarted { run_id: id });
                }
                HarnessEvent::TurnStarted => {
                    self.settle_start_message(tx, &run, DeliveryState::NativeDelivered)?;
                    // A helper's model turn can begin after the parent turn has
                    // ended, or while another request waits for a decision.
                    // Neither is new work for the run.
                    if run.ended_at.is_none()
                        && !matches!(run.state, RunState::Stopping | RunState::AwaitingApproval)
                    {
                        run.state = RunState::Working;
                        task.state = TaskState::Working;
                    }
                }
                HarnessEvent::AgentMessage { text, .. } => {
                    crate::usage_limits::usage_returned(tx, run.harness)?;
                    timeline = Some(TimelineBody::AgentMessage {
                        text,
                        partial: false,
                    })
                }
                HarnessEvent::Activity {
                    key,
                    tool,
                    summary,
                    status,
                    detail,
                } => {
                    crate::usage_limits::usage_returned(tx, run.harness)?;
                    let not_run = if status == ActivityStatus::Failed && !key.is_empty() {
                        not_run(tx, id, &key)?
                    } else {
                        None
                    };
                    timeline = Some(TimelineBody::Activity {
                        tool,
                        summary,
                        status,
                        detail,
                        key: Some(key).filter(|k| !k.is_empty()),
                        not_run,
                    })
                }
                HarnessEvent::ApprovalRequested(native) => {
                    if run.state == RunState::Stopping {
                        return Ok(());
                    }
                    if tx
                        .read()
                        .list::<Approval>("approval")?
                        .iter()
                        .any(|a| a.run_id == id && a.native_request_id == native.native_request_id)
                    {
                        return Ok(());
                    }
                    let approval = Approval {
                        approval_id: ApprovalId::new(),
                        run_id: id,
                        task_id: task.task_id,
                        project_id: task.project_id,
                        native_request_id: native.native_request_id,
                        kind: native.kind,
                        title: native.title,
                        detail: native.detail,
                        detail_lines: native.detail_lines,
                        questions: native.questions,
                        raw_request: native.raw_request,
                        scope: native.scope,
                        shortened: native.shortened,
                        revision: 1,
                        state: ApprovalState::Pending,
                        delivery: None,
                        outcome: None,
                        requested_at: now,
                        expires_at: None,
                        resolved_by: None,
                        resolved_at: None,
                    };
                    self.put_approval(tx, &approval)?;
                    if let Some(call) = native.tool_call {
                        tx.put(APPROVAL_TOOL_CALL, approval.approval_id, &call)?;
                    }
                    run.state = RunState::AwaitingApproval;
                    task.state = TaskState::AwaitingDecision;
                    timeline = Some(TimelineBody::Approval {
                        approval_id: approval.approval_id,
                    });
                }
                HarnessEvent::ApprovalClosed {
                    native_request_id,
                    outcome,
                } => {
                    // One given back to the agent unanswered stays open for
                    // an answer, though its native request has closed.
                    for mut a in tx.read().list::<Approval>("approval")? {
                        if a.run_id == id
                            && a.native_request_id == native_request_id
                            && crate::carry_on::blocks(&tx.read(), &a)?
                        {
                            a.state = ApprovalState::ResolvedElsewhere;
                            a.outcome = outcome;
                            a.revision += 1;
                            a.resolved_at = Some(now);
                            tx.put("approval", a.approval_id, &a)?;
                            tx.emit(Event::ApprovalUpserted {
                                approval: a.clone(),
                            })?;
                            resolve_attention(tx, a.approval_id, now)?;
                        }
                    }
                }
                HarnessEvent::Child {
                    native_ref,
                    label,
                    state,
                    lifetime,
                } => {
                    let unlisted = crate::children::unlisted(id);
                    if let Some(child) = run
                        .children
                        .iter_mut()
                        .find(|c| c.native_ref.as_ref() == Some(&native_ref))
                    {
                        child.state = state;
                        child.lifetime = lifetime.or(child.lifetime);
                        child.provenance = Provenance::Native;
                        child.stop = child_stop.clone();
                        if state != ChildState::Active {
                            child.ended_at = Some(now);
                        }
                    } else if let Some(ended) =
                        tx.read().get::<ChildState>(&unlisted, &native_ref)?
                        && !crate::children::stoppable(state)
                    {
                        // Already counted among the run's earlier helpers.
                        if ended != state {
                            tx.put(&unlisted, &native_ref, &state)?;
                        }
                    } else {
                        // One that runs again is listed again.
                        if tx
                            .read()
                            .get::<ChildState>(&unlisted, &native_ref)?
                            .is_some()
                        {
                            tx.delete(&unlisted, &native_ref)?;
                            run.earlier_children = run
                                .earlier_children
                                .and_then(|n| n.checked_sub(1))
                                .filter(|n| *n > 0);
                        }
                        run.children.push(ChildAgent {
                            lifetime,
                            child_id: ChildId::new(),
                            native_ref: Some(native_ref),
                            label,
                            state,
                            started_at: now,
                            ended_at: (state != ChildState::Active).then_some(now),
                            provenance: Provenance::Native,
                            stop: child_stop.clone(),
                        });
                    }
                    for child in crate::children::list_recent(&mut run) {
                        if let Some(native) = &child.native_ref {
                            tx.put(&unlisted, native, &child.state)?;
                        }
                    }
                }
                HarnessEvent::TurnCompleted { usage } => {
                    let outcome = run.outcome.get_or_insert_with(empty_outcome);
                    outcome.usage = usage;
                }
                HarnessEvent::Warning { text }
                | HarnessEvent::ReadinessWarning { reason: text } => {
                    timeline = Some(TimelineBody::Note { text })
                }
                HarnessEvent::Results { mut outcome } => {
                    outcome.usage = run.outcome.as_ref().and_then(|o| o.usage.clone());
                    run.outcome = Some(outcome);
                }
                HarnessEvent::AgentTextDelta { .. }
                | HarnessEvent::Exited { .. }
                | HarnessEvent::MessageDelivery { .. }
                | HarnessEvent::ParentEnded { .. }
                | HarnessEvent::CleanupPending
                | HarnessEvent::UsageLimited { .. } => unreachable!(),
            }
            if run.state == RunState::AwaitingApproval
                && !crate::carry_on::run_blocked(&tx.read(), id)?
            {
                run.state = RunState::Working;
                task.state = TaskState::Working;
            }
            if let Some(item) = timeline {
                tx.emit_from(
                    Event::TimelineAppended {
                        task_id: task.task_id,
                        item: TimelineItem {
                            seq: 0,
                            at: now,
                            run_id: Some(id),
                            item,
                        },
                    },
                    EventSource::Harness {
                        harness: run.harness,
                    },
                    Provenance::Native,
                )?;
            }
            // Every change to attention recounts the stored task, including
            // an approval opened or closed above. Take that count instead of
            // rescanning all attention ever raised on every native event.
            task.attention_count = tx
                .read()
                .require::<Task>("task", task.task_id)?
                .attention_count;
            task.updated_at = now;
            task.revision += 1;
            tx.put("task", task.task_id, &task)?;
            tx.emit(Event::TaskUpserted { task })?;
            tx.put("run", id, &run)?;
            tx.emit_from(
                Event::RunUpserted {
                    run: Box::new(run.clone()),
                },
                EventSource::Harness {
                    harness: run.harness,
                },
                Provenance::Native,
            )?;
            Ok(())
        })
    }
    pub(crate) fn progress_queues(&self) {
        let hub = self.clone();
        self.spawn(async move {
            if let Err(e) = hub.advance_queues().await {
                tracing::error!(code=?e.code, "queue progression failed");
            }
        });
    }
    pub(crate) fn finish_message_delivery(
        &self,
        tx: &mut store::Write<'_>,
        id: RunId,
        task_id: TaskId,
    ) -> store::Result<()> {
        let now = self.inner.clock.now();
        let run: Run = tx.read().require("run", id)?;
        self.settle_start_message(tx, &run, DeliveryState::Unknown)?;
        let messages: Vec<InFlightMessage> =
            tx.read().get("message_delivery", id)?.unwrap_or_default();
        for message in messages {
            // An undispatched or still-queued message was never written.
            // After dispatch, HostAccepted is not proof, and Unknown stays
            // unknown. Neither can be turned into a failure just because we exit.
            if message.undispatched || message.state == DeliveryState::QueuedForBoundary {
                tx.emit(Event::TimelineAppended {
                    task_id,
                    item: TimelineItem {
                        seq: 0,
                        at: now,
                        run_id: Some(id),
                        item: unsent(tx, id, message.seq)?,
                    },
                })?;
            }
        }
        tx.delete("message_delivery", id)?;
        Ok(())
    }
    pub(crate) fn finish_run(
        &self,
        id: RunId,
        state: RunState,
        outcome: Option<RunOutcome>,
    ) -> Result<(), ProtocolError> {
        self.end_run(id, state, outcome, false, None)
    }
    /// Retryable even when the run's terminal record already committed.
    async fn after_run_exit(
        &self,
        id: RunId,
        task_id: TaskId,
        state: RunState,
    ) -> Result<(), ProtocolError> {
        self.revoke_run(id).await?;
        // Native completion moves Next up. Run all also carries on past a
        // failed or stopped fix, leaving that task for the person to check.
        if state == RunState::Completed
            || (matches!(state, RunState::Failed | RunState::Interrupted)
                && self.read(|r| crate::github_run::moves_run(r, task_id))?)
        {
            self.progress_queues();
        }
        Ok(())
    }
    /// With `helpers_running`, the turn ended while helpers it started still
    /// run in the agent's process. The run keeps the outcome the turn left,
    /// and records them as unconfirmed: it stays stoppable, and a Work run
    /// keeps the project's writer, until the adapter confirms they ended or
    /// the person reconciles the task.
    fn end_run(
        &self,
        id: RunId,
        state: RunState,
        outcome: Option<RunOutcome>,
        helpers_running: bool,
        why: Option<String>,
    ) -> Result<(), ProtocolError> {
        let (after, replied, set_up, local, judged) = self.write(|tx| {
            let mut run: Run = tx.read().require("run", id)?;
            // A run already ended for reconciliation ends once; only a later
            // terminal outcome may replace that.
            if run.state.is_terminal() || (run.ended_at.is_some() && !state.is_terminal()) {
                return Ok((false, false, None, false, None));
            }
            let mut task: Task = tx.read().require("task", run.task_id)?;
            let now = self.inner.clock.now();
            // The explanation commits with the end, so a retried exit cannot
            // repeat it after a later write in this transaction fails.
            if let Some(text) = why {
                tx.emit_from(
                    Event::TimelineAppended {
                        task_id: run.task_id,
                        item: TimelineItem {
                            seq: 0,
                            at: now,
                            run_id: Some(id),
                            item: TimelineBody::Note { text },
                        },
                    },
                    EventSource::Harness {
                        harness: run.harness,
                    },
                    Provenance::Native,
                )?;
            }
            let was_unresolved = run.state == RunState::ReconciliationRequired;
            run.state = state;
            self.finish_message_delivery(tx, id, task.task_id)?;
            run.ended_at = Some(now);
            let mode = run_mode(&run, &task);
            run.holds_writer = mode == TaskMode::Work && (helpers_running || !state.is_terminal());
            // Recorded with the turn's end, so events lost before the adapter
            // says more leave the helpers for the person to check.
            if helpers_running && state.is_terminal() {
                run.cleanup_pending = true;
            }
            if outcome.is_some() {
                run.outcome = outcome;
            }
            if state.is_terminal()
                && let Some(outcome) = run.outcome.as_mut()
            {
                crate::reported::keep(&tx.read(), task.task_id, id, outcome)?;
            }
            if state.is_terminal() {
                crate::follow_up::note(tx, &run, now)?;
            }
            task.state = match state {
                RunState::Completed => TaskState::Completed,
                RunState::Failed => TaskState::Failed,
                RunState::Interrupted => TaskState::Interrupted,
                _ => TaskState::ReconciliationRequired,
            };
            if state.is_terminal()
                && let Some(before) = tx.read().get::<TaskState>(SIDE_TURN, id)?
            {
                task.state = before;
                tx.delete(SIDE_TURN, id)?;
            }
            task.revision += 1;
            task.updated_at = now;
            tx.delete("run_token", id)?;
            if let Some(intent) = tx.read().get::<RunIntent>("run_intent", id)?
                && let Some(mut command) = tx.read().command(intent.command_id)?
            {
                command.pending = false;
                command.stage = if state.is_terminal() {
                    CommandStage::Observed
                } else {
                    CommandStage::ReconciliationRequired
                };
                command.error = (!state.is_terminal()).then(|| {
                    error(
                        ErrorCode::OutcomeUnknown,
                        "Run outcome requires reconciliation",
                    )
                });
                tx.command(&command)?;
                tx.emit(Event::CommandProgress {
                    command_id: intent.command_id,
                    stage: command.stage,
                    detail: None,
                    result: None,
                    error: command.error,
                })?;
            }
            tx.put("run", id, &run)?;
            tx.emit(Event::RunUpserted { run: Box::new(run) })?;
            tx.emit(Event::TimelineAppended {
                task_id: task.task_id,
                item: TimelineItem {
                    seq: 0,
                    at: now,
                    run_id: Some(id),
                    item: TimelineBody::RunEnded { run_id: id, state },
                },
            })?;
            // An interrupt is someone's Stop, not a failure: the task keeps its
            // stopped state and Continue, but nothing is raised for attention.
            // The kind says what happened; the title says which task.
            let raised = match state {
                RunState::Completed => Some(AttentionKind::Completed),
                RunState::Failed => Some(AttentionKind::Failure),
                RunState::Interrupted => None,
                _ => Some(AttentionKind::ReconciliationRequired),
            };
            // What a linked read-only run found is acted on once it has
            // ended, here or by the watcher's sweep after a restart. A
            // finder's or a check's finish is told by what comes of it.
            let after =
                self.inner.github.is_some() && crate::github_after::acts_after(tx, &task, state)?;
            if after {
                let pending = crate::github_after::After {
                    run_id: id,
                    claimed: None,
                };
                tx.put(crate::github_after::AFTER, id, &pending)?;
            }
            // A local review's findings are verified before they're told:
            // the person hears once that has settled.
            let epoch = self.inner.epoch;
            let local =
                crate::review_loop::reviewed_locally(tx, &task, id, (state, mode), (epoch, now))?;
            // One that didn't finish leaves its findings unchecked: a fix
            // waiting for them is held, and that is what the person hears.
            let unfinished = self.unfinished_locally(tx, &task, (state, mode), now)?;
            // A local review's verifier: what it wrote is looked at before
            // a fix waiting on it starts.
            let judged = crate::review_loop::local_verifier_ended(tx, &task, id, state, epoch)?;
            // An agent that asked to be woken goes on by itself: nothing
            // is the person's until it says it is done or needs them.
            let quiet = state == RunState::Completed
                && crate::watching::quietly_watching(&tx.read(), &task)?;
            // Another agent asked it: what it answered goes back to that one.
            let replied =
                state.is_terminal() && crate::asking::answered(tx, &task, id, state, now)?;
            // It worked out how to start a review's app: that review starts.
            let set_up = crate::app_start::answered(tx, &task, id, state)?;
            // A finder's, a check's or a verifier's end is told by what
            // comes of it.
            let raised = raised.filter(|_| {
                !quiet
                    && !replied
                    && set_up.is_none()
                    && !local
                    && !unfinished
                    && !(after
                        && (crate::review_loop::verifies(&task)
                            || (task.github.as_deref()).is_some_and(|g| {
                                matches!(
                                    g.link,
                                    protocol::github::GithubLink::Find { .. }
                                        | protocol::github::GithubLink::Check { .. }
                                )
                            })))
            });
            // A run that waited for the person has now ended on the computer's
            // own confirmation, so there is nothing left for them to decide.
            // A helper whose state is unknown is still theirs to check.
            if was_unresolved && state.is_terminal() {
                for mut item in tx.read().list::<AttentionItem>("attention")? {
                    if item.run_id == Some(id)
                        && item.child_id.is_none()
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
            }
            if let Some(kind) = raised {
                let body = match kind {
                    AttentionKind::Completed => {
                        crate::watching::ended_body(&tx.read(), &task, id, now)?
                    }
                    _ => None,
                };
                let mut attention = AttentionItem {
                    child_id: None,
                    attention_id: AttentionId::new(),
                    kind,
                    title: task.title.clone(),
                    body,
                    task_id: Some(task.task_id),
                    run_id: Some(id),
                    approval_id: None,
                    created_at: now,
                    resolved_at: None,
                    priority: 5,
                    topic: None,
                    quiet: false,
                    report_id: None,
                };
                // Done, with nothing for the person, as part of something
                // that alerts as a whole: a Run all, or a pull request Ophio
                // is seeing through. What comes of those alerts instead.
                attention.quiet = kind == AttentionKind::Completed
                    && attention.body.is_none()
                    && (crate::calm::held(&tx.read(), &attention)?
                        || ophio_round(&tx.read(), id, &task)?);
                tx.put("attention", attention.attention_id, &attention)?;
                tx.emit(Event::AttentionUpserted { item: attention })?;
            }
            if mode == TaskMode::Work && state == RunState::Failed {
                self.pause_after_failure(tx, &task)?;
            }
            if mode == TaskMode::Work && state.is_terminal() && !helpers_running {
                self.release_writer(tx, task.project_id, id)?;
            } else {
                self.tidy_queue(tx, task.project_id)?;
            }
            // What it went on without stays open: an answer starts it again.
            for mut a in tx.read().list::<Approval>("approval")? {
                if a.run_id == id && crate::carry_on::blocks(&tx.read(), &a)? {
                    a.state = ApprovalState::Cancelled;
                    a.revision += 1;
                    tx.put("approval", a.approval_id, &a)?;
                    tx.emit(Event::ApprovalUpserted {
                        approval: a.clone(),
                    })?;
                    resolve_attention(tx, a.approval_id, now)?;
                }
            }
            // Nobody needs to choose an agent for a run that has ended.
            for mut item in tx.read().list::<AttentionItem>("attention")? {
                if item.run_id == Some(id)
                    && item.kind == AttentionKind::WaitingForYou
                    && item.resolved_at.is_none()
                {
                    item.resolved_at = Some(now);
                    tx.put("attention", item.attention_id, &item)?;
                    tx.emit(Event::AttentionResolved {
                        attention_id: item.attention_id,
                    })?;
                }
            }
            // Publish the terminal task only after approval blockers are closed,
            // so channel completion notifications never see a stale count.
            task.attention_count = blocking_attention_count(&tx.read(), task.task_id)?;
            tx.put("task", task.task_id, &task)?;
            tx.emit(Event::TaskUpserted { task })?;
            Ok((after, replied, set_up, local, judged))
        })?;
        if after {
            let hub = self.clone();
            self.spawn(async move { hub.github_after_run(id).await });
        }
        if local {
            let hub = self.clone();
            let review = self.read(|r| Ok(r.require::<Run>("run", id)?.task_id))?;
            self.spawn(async move {
                if let Err(e) = hub.start_verifier(review).await {
                    tracing::warn!(code = ?e.code, "starting a local review's verifier failed");
                }
            });
        }
        if let Some(review) = judged {
            let hub = self.clone();
            self.spawn(async move {
                if let Err(e) = hub.local_verdicts(review).await {
                    tracing::warn!(code = ?e.code, "looking at a local verifier's verdicts failed");
                }
            });
        }
        if let Some((review, run)) = set_up {
            let hub = self.clone();
            self.spawn(async move {
                if let Err(e) = hub.dispatch_task(review, run).await {
                    tracing::warn!(code = ?e.code, "starting a review after its app's setup failed");
                }
            });
        }
        if replied {
            self.wake_guard();
        }
        Ok(())
    }
    /// The helpers a turn left running have ended, or can no longer be
    /// followed now that the agent's process is gone, so the writer slot
    /// they kept is free.
    fn helpers_ended(&self, id: RunId) -> Result<(), ProtocolError> {
        self.write(|tx| {
            let run: Run = tx.read().require("run", id)?;
            self.free_helpers_writer(tx, run)
        })
    }
    fn free_helpers_writer(&self, tx: &mut store::Write<'_>, mut run: Run) -> store::Result<()> {
        if !helpers_finishing(&run) {
            return Ok(());
        }
        run.holds_writer = false;
        tx.put("run", run.run_id, &run)?;
        let task: Task = tx.read().require("task", run.task_id)?;
        let id = run.run_id;
        tx.emit(Event::RunUpserted { run: Box::new(run) })?;
        self.release_writer(tx, task.project_id, id)
    }
    /// The adapter's events ended before it confirmed the run's processes
    /// had. Nothing can confirm what the run started ended now, so a turn
    /// that ended while it still ran is left for the person to check.
    /// Decided in one write, so a Force stop's record can't land between.
    fn events_lost(&self, id: RunId) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            let mut run: Run = tx.read().require("run", id)?;
            let mut task: Task = tx.read().require("task", run.task_id)?;
            cleanup_unconfirmed(tx, &mut run, &mut task, now)
        })
    }
    /// A failed Work run may have left the project half changed. Nothing
    /// else starts by itself until the person has looked and resumed.
    fn pause_after_failure(&self, tx: &mut store::Write<'_>, task: &Task) -> store::Result<()> {
        let mut q: QueueView = tx.read().require("queue", task.project_id)?;
        if q.paused {
            return Ok(());
        }
        q.paused = true;
        q.pause_reason = Some(format!(
            "Next up paused because “{}” failed, so nothing starts by itself. Check it, then resume when you’re ready.",
            task.title
        ));
        bump_queue_revision(tx, &mut q)?;
        tx.put("queue", q.project_id, &q)?;
        sync_queue_attention(tx, &q, self.inner.clock.now())?;
        tx.emit(Event::QueueChanged { queue: q })?;
        Ok(())
    }
    /// Stops `run`, the run Stop was accepted for, or without one the task's
    /// current run. A turn that has taken its place since is not the one
    /// the person stopped, and is left running.
    pub(crate) async fn stop_task(
        &self,
        task_id: TaskId,
        run: Option<RunId>,
        force: bool,
    ) -> Result<(), ProtocolError> {
        let task: Task = self.read(|r| r.require("task", task_id))?;
        let id = run
            .or(task.current_run)
            .ok_or_else(|| error(ErrorCode::Conflict, "No active run"))?;
        let before: Run = self.read(|r| r.require("run", id))?;
        // The turn already ended; Stop reaches only what it left running.
        let helpers_only = still_finishing(&before);
        let control = {
            let runs = self.inner.runs.lock().await;
            let control = runs.get(&id).cloned();
            if force && before.force_stoppable && control.is_some() {
                // Something the run started may outlive the agent. Until this
                // Stop or the adapter confirms it ended, or the person
                // reconciles the task, the run keeps the project and stays
                // stoppable. Recorded before anything below waits, and while
                // the run is still registered, so events lost from here on
                // leave it for the person to check.
                self.write(|tx| set_cleanup_pending(tx, id, true))?;
            }
            control
        };
        let containment = control.as_ref().and_then(|c| c.containment());
        // It, and all it started, ended after this Stop was accepted:
        // nothing is left to stop, and no agent will answer.
        let ended_first = control.is_none()
            && before.state.is_terminal()
            && !helpers_only
            && self
                .read(|r| r.get::<bool>("run_process_ended", id))?
                .unwrap_or(false);
        if force && !ended_first && (!before.force_stoppable || control.is_none()) {
            return Err(error(
                ErrorCode::Unsupported,
                "Øphio on this computer does not own a live agent process for this run. Stop it in the agent’s terminal on the computer.",
            ));
        }
        // Everything below can wait. A turn that ends meanwhile must not hand
        // a held message to the agent after this Stop was accepted. A refused
        // Force stop above leaves the run, and its admission, as it was.
        if let Some(control) = &control {
            control.close_admission();
        }
        // Dispatch starts only an admitted run, and this Stop's admission has
        // already moved it on. A run that was never dispatched reached no
        // agent and can no longer start, so no agent will ever answer Stop.
        let never_started = control.is_none() && before.dispatched_at.is_none();
        if never_started {
            self.finish_run(id, RunState::Interrupted, None)?;
        }
        let revoked = self.revoke_run(id).await;
        let stopped = async {
            let interrupt = if !force && let Some(control) = &control {
                tokio::time::timeout(Duration::from_secs(5), control.interrupt())
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .is_some()
            } else {
                false
            };
            let children = self
                .stop_run_children(id, before.children, interrupt)
                .await?;
            let process_ended = if ended_first {
                true
            } else if force {
                tokio::time::timeout(
                    Duration::from_secs(8),
                    control.as_ref().expect("owned control").terminate(),
                )
                .await
                .is_ok_and(|result| result.is_ok())
                    || self.read(|r| r.get::<bool>("run_process_ended", id))?.unwrap_or(false)
            } else if (!interrupt || helpers_only || containment.is_some()) && control.is_some() {
                // The adapters allow five seconds for native Stop, then shut down
                // the process. Our interrupt timeout can win that race. Give the
                // exit event the same five seconds plus three seconds of grace.
                // A finished turn's process ends once the helpers it left have;
                // only that says Stop reached them. An adapter that keeps the
                // run's processes together ends them all before reporting the
                // exit, so that exit also says the work stopped.
                tokio::time::timeout(Duration::from_secs(8), async {
                    loop {
                        let run: Run = self.read(|r| r.require("run", id))?;
                        // A completed parent turn or a lost event stream alone
                        // does not establish process exit, even if the run ended.
                        if run.ended_at.is_some()
                            && self
                                .read(|r| r.get::<bool>("run_process_ended", id))?
                                .unwrap_or(false)
                        {
                            return Ok::<_, ProtocolError>(true);
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await
                .is_ok_and(|result| result.unwrap_or(false))
            } else {
                false
            };
            if force && process_ended && !ended_first {
                self.write(|tx| set_cleanup_pending(tx, id, false))?;
                self.finish_run(id, RunState::Interrupted, None)?;
                self.helpers_ended(id)?;
            }
            let count = self.read(|r| r.get::<u32>("stop_count", id))?.unwrap_or(1);
            let no_helpers = children.is_empty();
            let receipt = InterventionReceipt {
                kind: InterventionKind::StopTask,
                requested_at: self.inner.clock.now(),
                facts: {
                    let mut facts = vec![ReceiptFact {
                        label: "Agent confirmed Stop".into(),
                        status: if never_started || helpers_only || ended_first {
                            FactStatus::NotApplicable
                        } else if interrupt {
                            FactStatus::Done
                        } else {
                            FactStatus::Unknown
                        },
                        detail: if never_started {
                            Some("The agent hadn’t started, so nothing reached it.".into())
                        } else if ended_first {
                            Some("It had already ended.".into())
                        } else if helpers_only {
                            Some("Its turn had already finished.".into())
                        } else {
                            None
                        },
                    }];
                    facts.push(ReceiptFact {
                        label: "Stop requests".into(),
                        status: FactStatus::Done,
                        detail: Some(format!(
                            "Stop sent {}{}",
                            match count {
                                1 => "once".into(),
                                2 => "twice".into(),
                                _ => format!("{count} times"),
                            },
                            if force { " · then Force stop" } else { "" }
                        )),
                    });
                    if force {
                        facts.push(ReceiptFact { label: "Agent process ended on the computer".into(),
                            status: if process_ended { FactStatus::Done } else { FactStatus::Unknown },
                            detail: Some("Files already changed stay changed; interrupted writes may be incomplete. Detached or remote children are reported separately.".into()) });
                    }
                    if !force && helpers_only {
                        facts.push(ReceiptFact {
                            label: "Agent process ended on the computer".into(),
                            status: if process_ended {
                                FactStatus::Done
                            } else {
                                FactStatus::Unknown
                            },
                            detail: Some(
                                if process_ended {
                                    "It ended once its helpers had. Files already changed stay changed."
                                } else {
                                    "It, or something it started, is still running on the computer. Stop again, or force stop it."
                                }
                                .into(),
                            ),
                        });
                    } else if !force && !interrupt && process_ended && !ended_first {
                        facts.push(ReceiptFact {
                            label: "Agent process ended on the computer".into(),
                            status: FactStatus::Done,
                            detail: Some("The agent didn’t confirm Stop within 5 s, and its process has now ended. Files already changed stay changed; interrupted writes may be incomplete. Detached or remote children are reported separately.".into()),
                        });
                    }
                    if let Some(containment) = &containment {
                        facts.push(started_work(containment, process_ended));
                    }
                    if children.is_empty() && !never_started {
                        facts.push(ReceiptFact {
                            label: "Helpers".into(),
                            status: FactStatus::NotApplicable,
                            detail: Some("The agent reported no running helpers".into()),
                        });
                    }
                    facts.extend(children);
                    facts.push(ReceiptFact {
                        label: "Desktop access withdrawn from the agent".into(),
                        status: if revoked.is_ok() {
                            FactStatus::Done
                        } else {
                            FactStatus::Unknown
                        },
                        detail: None,
                    });
                    facts.push(ReceiptFact {
                        label: "Next up paused".into(),
                        status: FactStatus::Done,
                        detail: Some(format!(
                            "Next up paused when you stopped “{}”, so nothing starts by itself. Resume when you’re ready.",
                            task.title
                        )),
                    });
                    facts
                },
                summary: if ended_first {
                    "It had already ended before Stop reached it. Prior file changes remain.".into()
                } else if force {
                    if process_ended {
                        "Agent process ended on the computer. Prior file changes remain.".into()
                    } else {
                        "Force stop requested; process exit is unconfirmed. Check the agent’s terminal on the computer.".into()
                    }
                } else if never_started {
                    "Stopped before the agent started. Nothing reached the agent.".into()
                } else if helpers_only && process_ended {
                    "The turn had already finished. Its helpers have ended, and so has the agent’s process.".into()
                } else if helpers_only && no_helpers {
                    "The turn had already finished, and the agent reports no helpers still running. Its process, or something it started, hasn’t ended yet.".into()
                } else if helpers_only {
                    "The turn had already finished, so Stop went to the helpers it left running. The receipt says which have stopped.".into()
                } else if interrupt {
                    "The agent confirmed Stop. The receipt lists what else ended.".into()
                } else if process_ended {
                    "Agent process ended on the computer. Prior file changes remain.".into()
                } else {
                    "The agent hasn’t confirmed Stop. Check the task on the computer.".into()
                },
            };
            let recorded = self.write(|tx| {
                // The conversation keeps the receipt beside the run it stopped.
                tx.emit(Event::TimelineAppended {
                    task_id,
                    item: TimelineItem {
                        seq: 0,
                        at: receipt.requested_at,
                        run_id: Some(id),
                        item: TimelineBody::Receipt {
                            receipt: Box::new(receipt.clone()),
                        },
                    },
                })?;
                tx.emit(Event::InterventionRecorded {
                    task_id: Some(task_id),
                    target_id: task.target_id,
                    receipt: Box::new(receipt),
                })?;
                Ok(())
            });
            // A Stop nobody confirmed is reported as such even when its
            // receipt could not be kept. The agent's acknowledgement confirms
            // it only where nothing else is known to be running: a finished
            // turn's helpers, or work kept together with the agent, stopped
            // only once the computer says they ended.
            let confirmed = never_started
                || process_ended
                || (interrupt && !helpers_only && containment.is_none());
            let mut failure = (!confirmed).then(|| {
                error(
                    ErrorCode::OutcomeUnknown,
                    if force {
                        "The computer hasn’t confirmed the agent’s process ended"
                    } else if helpers_only {
                        "The computer hasn’t confirmed what the finished turn left running has ended"
                    } else if interrupt {
                        "The agent confirmed Stop, but the computer can’t yet tell that everything it started has ended"
                    } else {
                        "The agent hasn’t confirmed Stop"
                    },
                )
            });
            crate::control::keep_worse(&mut failure, recorded);
            failure.map_or(Ok(()), Err)
        }
        .await;
        // Input the agent may still hold is reported first, even once the
        // agent has confirmed Stop and whatever else went wrong after it.
        let mut failure = revoked.err();
        crate::control::keep_worse(&mut failure, stopped);
        failure.map_or(Ok(()), Err)
    }
    pub(crate) async fn send_message(
        &self,
        caller: &Caller,
        command_id: CommandId,
        id: RunId,
        text: &str,
        bundle: Option<&AnnotationBundle>,
        delivery: MessageDelivery,
    ) -> Result<(), ProtocolError> {
        self.send_message_with_followup(caller, command_id, id, text, bundle, delivery, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn send_message_with_followup(
        &self,
        caller: &Caller,
        command_id: CommandId,
        id: RunId,
        text: &str,
        bundle: Option<&AnnotationBundle>,
        delivery: MessageDelivery,
        followup: Option<&crate::followups::Followup>,
    ) -> Result<(), ProtocolError> {
        let lock = self.message_lock(id);
        let _message_guard = lock.lock().await;
        // Revoking a chat waits for this lock before it withdraws what the chat
        // left held, so a message it sends after that finds its access ended.
        self.authorize(caller, |g| g.tasks)?;
        if self.run_stopped(id)? {
            return Err(error(
                ErrorCode::Conflict,
                "This task finished or is stopping; send it again to continue",
            ));
        }
        let task_id = self.read(|r| r.require::<Run>("run", id))?.task_id;
        let control = self
            .inner
            .runs
            .lock()
            .await
            .get(&id)
            .cloned()
            // Nothing is sent without a live agent, so the outcome is known.
            .ok_or_else(|| {
                error(
                    ErrorCode::Conflict,
                    "No agent is running for this task, so the message wasn’t sent. Start or continue the task, then send it again.",
                )
            })?;
        if let Some(bundle) = bundle {
            // Reject changed reviewed revisions before delivering their contents.
            self.write(|tx| crate::results::link_bundle(tx, task_id, Some(id), bundle))?;
        }
        let input = self.prompt_input(text, bundle)?;
        // Commands save their words during admission. Internally generated
        // answers and wakes enter here directly, still pinned to this run.
        let message: admitted_followups::Followup = self.write(|tx| {
            if let Some(f) = followup {
                f.require_current(&tx.read())?;
            }
            if let Some(saved) = tx.read().get("followup", command_id)? {
                return Ok(saved);
            }
            let task: Task = tx.read().require("task", task_id)?;
            if task.current_run != Some(id) {
                return Err(error(ErrorCode::Conflict, "This task started another turn").into());
            }
            self.admit_followup(tx, command_id, &task, text, bundle)?;
            tx.read().require("followup", command_id)
        })?;
        if message.run_id != id {
            return Err(error(
                ErrorCode::Conflict,
                "This message belongs to another turn",
            ));
        }
        let message_ref = MessageRef(command_id);
        self.write(|tx| {
            let mut messages: Vec<InFlightMessage> =
                tx.read().get("message_delivery", id)?.unwrap_or_default();
            let message = messages
                .iter_mut()
                .find(|m| m.message_ref == Some(message_ref))
                .filter(|m| m.undispatched)
                .ok_or_else(|| error(ErrorCode::Conflict, "This message is already settled"))?;
            message.undispatched = false;
            tx.put("message_delivery", id, &messages)
        })?;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            control.send_message(message_ref, input, delivery),
        )
        .await;
        let (state, rejection) = match result {
            Ok(Ok(state)) => (state, None),
            Ok(Err(ports::HarnessError::NotActive)) => (
                DeliveryState::Failed,
                Some(
                    error(
                        ErrorCode::Conflict,
                        "This task finished; send it again to continue",
                    )
                    .with_detail(serde_json::json!({"action":"resume_task", "task_id": task_id})),
                ),
            ),
            // The adapter refused the message before writing any of it.
            Ok(Err(HarnessError::Unsupported(why))) => (
                DeliveryState::Failed,
                Some(error(ErrorCode::Unsupported, why)),
            ),
            _ => (DeliveryState::Unknown, None),
        };
        self.write(|tx| {
            let seq = message.seq;
            let mut messages: Vec<InFlightMessage> =
                tx.read().get("message_delivery", id)?.unwrap_or_default();
            let previous = messages
                .iter()
                .find(|m| m.message_ref == Some(message_ref))
                .map(|m| m.state)
                .unwrap_or(DeliveryState::HostAccepted);
            messages.retain(|m| m.message_ref != Some(message_ref));
            let run: Run = tx.read().require("run", id)?;
            let update = if run.ended_at.is_some() && state == DeliveryState::QueuedForBoundary {
                Some(unsent(tx, id, seq)?)
            } else if state != previous {
                Some(TimelineBody::Delivery {
                    message_seq: seq,
                    state,
                    withdrawn_by_stop: false,
                    withdrawn_by_revocation: false,
                })
            } else {
                None
            };
            if let Some(item) = update {
                tx.emit(Event::TimelineAppended {
                    task_id,
                    item: TimelineItem {
                        seq: 0,
                        at: self.inner.clock.now(),
                        run_id: Some(id),
                        item,
                    },
                })?;
            }
            if run.ended_at.is_none() && in_flight(state) {
                messages.push(InFlightMessage {
                    message_ref: Some(message_ref),
                    seq,
                    state,
                    undispatched: false,
                });
            }
            tx.put("message_delivery", id, &messages)?;
            Ok(())
        })?;
        if let Some(rejection) = rejection {
            return Err(rejection);
        }
        if state == DeliveryState::Unknown {
            return Err(error(
                ErrorCode::OutcomeUnknown,
                "Message delivery cannot be established",
            ));
        }
        Ok(())
    }
    pub(crate) fn put_approval(
        &self,
        tx: &mut store::Write<'_>,
        approval: &Approval,
    ) -> store::Result<()> {
        tx.put("approval", approval.approval_id, approval)?;
        tx.emit(Event::ApprovalUpserted {
            approval: approval.clone(),
        })?;
        let item = AttentionItem {
            child_id: None,
            attention_id: AttentionId::new(),
            // An agent's question reads as one: "Question from Codex", not
            // "Approval waiting".
            kind: if approval.kind == ApprovalKind::Question {
                AttentionKind::Question
            } else {
                AttentionKind::Approval
            },
            title: approval.title.clone(),
            body: (tx
                .read()
                .get::<ApprovalId>(crate::github_access::START_APPROVAL, approval.run_id)?
                == Some(approval.approval_id))
            .then(|| approval.detail.clone())
            .flatten(),
            task_id: Some(approval.task_id),
            run_id: Some(approval.run_id),
            approval_id: Some(approval.approval_id),
            created_at: approval.requested_at,
            resolved_at: None,
            priority: 10,
            topic: None,
            quiet: false,
            report_id: None,
        };
        tx.put("attention", item.attention_id, &item)?;
        tx.emit(Event::AttentionUpserted { item })?;
        recount_attention(tx, approval.task_id)?;
        Ok(())
    }
    pub(crate) async fn resolve_approval(
        &self,
        caller: &Caller,
        id: ApprovalId,
        decision: &ApprovalDecision,
    ) -> Result<(), ProtocolError> {
        let approval: Approval = self.read(|r| r.require("approval", id))?;
        if approval.state == ApprovalState::Cancelled {
            return Err(error(ErrorCode::Expired, ARRIVED_LATE));
        }
        if approval.state != ApprovalState::Pending {
            return Err(error(
                ErrorCode::AlreadyResolved,
                "Approval already resolved",
            ));
        }
        // Its agent went on without it: the answer is its next message,
        // whether or not the turn that asked is still running.
        if self.read(|r| crate::carry_on::carried_on(r, id))? {
            // The guard passes it on as soon as it is stored.
            return self.answer_later(caller, id, decision);
        }
        if self.run_stopped(approval.run_id)? {
            return Err(error(ErrorCode::Expired, ARRIVED_LATE));
        }
        // Recorded before the hand-off, so a Stop that cancels the request
        // meanwhile cannot claim its tool never ran.
        let marked = !matches!(decision, ApprovalDecision::Deny { .. })
            && self.write(|tx| {
                if tx.read().get::<bool>(ALLOW_HANDED, id)?.is_some() {
                    return Ok(false);
                }
                tx.put(ALLOW_HANDED, id, &true)?;
                Ok(true)
            })?;
        let wait = self.inner.waits.lock().await.remove(&id);
        let bridge = wait.is_some()
            || self.read(|r| Ok(r.get::<bool>("bridge_approval", id)?.unwrap_or(false)))?;
        // Once the decision is in the agent's hands, a run that ends before it
        // is recorded may have acted on it.
        let mut handed = false;
        let delivery = async {
            if let Some(wait) = wait {
                self.login_decided(caller, &approval, decision);
                wait.send(decision.clone()).map_err(|_| {
                    error(
                        ErrorCode::NativeFailure,
                        "The agent did not receive this. It may still be waiting.",
                    )
                })?;
                handed = true;
                return Ok(());
            }
            if bridge {
                return Err(error(
                    ErrorCode::NativeFailure,
                    "The agent did not receive this. It may still be waiting.",
                ));
            }
            let control = self
                .inner
                .runs
                .lock()
                .await
                .get(&approval.run_id)
                .cloned()
                .ok_or_else(|| {
                    error(
                        ErrorCode::OutcomeUnknown,
                        "Native approval route unavailable",
                    )
                })?;
            handed = true;
            tokio::time::timeout(
                Duration::from_secs(5),
                control.resolve_approval(&approval.native_request_id, decision),
            )
            .await
            .map_err(|_| {
                error(
                    ErrorCode::OutcomeUnknown,
                    "The agent hasn’t confirmed it got the decision",
                )
            })?
            .map_err(|e| match e {
                // The adapter refused this decision before answering the agent.
                HarnessError::Unsupported(why) => {
                    handed = false;
                    error(ErrorCode::Unsupported, why)
                }
                _ => error(
                    ErrorCode::OutcomeUnknown,
                    "The computer couldn’t confirm the decision reached the agent",
                ),
            })?;
            Ok(())
        }
        .await;
        let overtaken = self.write(|tx| {
            if marked && !handed {
                tx.delete(ALLOW_HANDED, id)?;
            }
            let mut a: Approval = tx.read().require("approval", id)?;
            let delivered = ApprovalDelivery {
                decision: decision.without_secrets(Some(&a)),
                at: self.inner.clock.now(),
                error: delivery.as_ref().err().cloned(),
            };
            if a.state == ApprovalState::Cancelled {
                // The run ended before the decision reached its agent, so
                // nothing was left to act on it.
                if !handed {
                    return Ok(Some(error(ErrorCode::Expired, ARRIVED_LATE)));
                }
                // The run ended as its agent took the decision: keep what was
                // sent, and say the effect is unknown rather than absent.
                a.delivery = Some(delivered);
                a.revision += 1;
                tx.put("approval", id, &a)?;
                tx.emit(Event::ApprovalUpserted { approval: a })?;
                return Ok(Some(match &delivery {
                    Ok(()) => error(ErrorCode::OutcomeUnknown, ENDED_AS_DELIVERED),
                    Err(e) => e.clone(),
                }));
            }
            if a.state != ApprovalState::Pending {
                return Ok(None);
            }
            a.delivery = Some(delivered);
            if delivery.is_err() {
                a.revision += 1;
                tx.put("approval", id, &a)?;
                tx.emit(Event::ApprovalUpserted {
                    approval: a.clone(),
                })?;
                tx.delete("approval_claim", id)?;
                // A person may have dismissed attention while the hand-off was
                // in flight. It is raised again as a new item, never reopened
                // under its old id: phones that showed or cleared that one,
                // even across a reconnect, notify this one as any other.
                let items: Vec<AttentionItem> = (tx.read().list::<AttentionItem>("attention")?)
                    .into_iter()
                    .filter(|item| item.approval_id == Some(id))
                    .collect();
                if items.iter().all(|item| item.resolved_at.is_some())
                    && let Some(last) = items.into_iter().max_by_key(|item| item.created_at)
                {
                    let item = AttentionItem {
                        attention_id: AttentionId::new(),
                        created_at: self.inner.clock.now(),
                        resolved_at: None,
                        ..last
                    };
                    tx.put("attention", item.attention_id, &item)?;
                    tx.emit(Event::AttentionUpserted { item })?;
                }
                recount_attention(tx, a.task_id)?;
                return Ok(None);
            }
            a.state = if matches!(decision, ApprovalDecision::Deny { .. }) {
                ApprovalState::Denied
            } else {
                ApprovalState::Allowed
            };
            a.resolved_at = Some(self.inner.clock.now());
            a.resolved_by = Some(caller.principal);
            a.revision += 1;
            tx.put("approval", id, &a)?;
            let run_id = a.run_id;
            tx.emit(Event::ApprovalUpserted { approval: a })?;
            resolve_attention(tx, id, self.inner.clock.now())?;
            if !crate::carry_on::run_blocked(&tx.read(), run_id)? {
                let mut run: Run = tx.read().require("run", run_id)?;
                if run.state == RunState::AwaitingApproval {
                    run.state = RunState::Working;
                    let mut task: Task = tx.read().require("task", run.task_id)?;
                    task.state = TaskState::Working;
                    task.revision += 1;
                    task.updated_at = self.inner.clock.now();
                    tx.put("task", task.task_id, &task)?;
                    tx.emit(Event::TaskUpserted { task })?;
                    tx.put("run", run_id, &run)?;
                    tx.emit(Event::RunUpserted { run: Box::new(run) })?;
                }
            }
            Ok(None)
        })?;
        match overtaken {
            Some(refusal) => Err(refusal),
            None => delivery,
        }
    }
    pub(crate) fn validate_bundle_for(
        &self,
        caller: &Caller,
        bundle: &AnnotationBundle,
    ) -> Result<(), ProtocolError> {
        self.validate_bundle(bundle)?;
        if !caller.grants.admin {
            for source in &bundle.sources {
                let creator: Option<protocol::auth::Principal> =
                    self.read(|r| r.get("artifact_creator", source.artifact_id))?;
                // What an agent showed in a task the caller can read may be drawn on and sent back.
                let shown = matches!(source.origin, protocol::annotation::SourceOrigin::Shown { artifact_id } if artifact_id == source.artifact_id)
                    && creator == Some(protocol::auth::Principal::System)
                    && self.read(|r| {
                        let meta: ArtifactMeta = r.require("artifact", source.artifact_id)?;
                        crate::CompanionHub::sees_artifact(r, caller, &meta)
                    })?;
                if creator != Some(caller.principal) && !shown {
                    return Err(error(
                        ErrorCode::PermissionDenied,
                        "Annotation artifact belongs to another principal",
                    ));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn validate_bundle(&self, bundle: &AnnotationBundle) -> Result<(), ProtocolError> {
        let source_keys: std::collections::HashSet<_> =
            bundle.sources.iter().map(|s| &s.key).collect();
        if source_keys.len() != bundle.sources.len() || bundle.sources.len() > 16 {
            return Err(error(
                ErrorCode::InvalidRequest,
                "Annotation source keys must be unique and bounded",
            ));
        }
        for source in &bundle.sources {
            let artifact: ArtifactMeta =
                self.read(|r| r.require("artifact", source.artifact_id))?;
            if artifact.sha256 != source.sha256 || artifact.media_type != source.media_type {
                return Err(error(
                    ErrorCode::StaleRevision,
                    "Annotation source does not match the reviewed artifact",
                ));
            }
            if !source.media_type.starts_with("image/") {
                return Err(unsupported(
                    "Video annotation; supply reviewed still frames",
                ));
            }
        }
        for mark in &bundle.marks {
            if !source_keys.contains(&mark.source_key) {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "Drawing source is missing",
                ));
            }
        }
        for comment in &bundle.comments {
            if comment.mark_ids.len() != comment.link_origins.len()
                || comment
                    .mark_ids
                    .iter()
                    .any(|id| !bundle.marks.iter().any(|m| m.mark_id == *id && !m.deleted))
            {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "Resolve annotation links before sending",
                ));
            }
            if let Some(moment) = &comment.moment
                && (!source_keys.contains(&moment.still) || moment.start_ms > moment.end_ms)
            {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "A comment's moment needs a still from the video and a start before its end",
                ));
            }
        }
        Ok(())
    }
    fn prompt_input(
        &self,
        text: &str,
        bundle: Option<&AnnotationBundle>,
    ) -> Result<PromptInput, ProtocolError> {
        let mut prompt = PromptInput {
            text: text.into(),
            images: Vec::new(),
        };
        if let Some(bundle) = bundle {
            self.validate_bundle(bundle)?;
            // Include exactly the selected source bytes and what the agent
            // needs of the drawing (`crate::drawn`); do not attach retained
            // audio or a derivative's parent.
            for source in &bundle.sources {
                prompt.images.push(ports::ImageInput {
                    path: self.artifact_path(&source.sha256)?,
                    media_type: source.media_type.clone(),
                });
            }
            prompt.text.push_str(
                "\n\nReviewed annotation (coordinates are fractions of each source's width and height; points are [x, y]):\n",
            );
            prompt
                .text
                .push_str(&crate::drawn::for_agent(bundle).to_string());
            let spoken: Vec<_> = (bundle.comments.iter())
                .filter_map(|c| c.moment.as_ref().map(|m| (m, &c.text)))
                .collect();
            if !spoken.is_empty() {
                prompt.text.push_str(
                    "\n\nSaid while watching the video (times are positions in the video; each still is a frame from then):",
                );
                for (moment, text) in spoken {
                    prompt.text.push_str(&format!(
                        "\n- {}–{}, still {}: “{text}”",
                        clock(moment.start_ms),
                        clock(moment.end_ms),
                        moment.still
                    ));
                }
            }
        }
        Ok(prompt)
    }
}
/// A position in a video as a person reads it: 0:12, or 1:02:03.
fn clock(ms: u64) -> String {
    let s = ms / 1000;
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, m, s) => format!("{m}:{s:02}"),
        (h, m, s) => format!("{h}:{m:02}:{s:02}"),
    }
}
/// A lifecycle update or permission for another item does not change a
/// reviewed request. Carry forward only permits matching the old revision.
/// Forgets the started items `queue` no longer needs: their run has ended
/// with nothing it started left running, and no waiting item follows them.
/// The task and its runs keep that history. Kept, every task ever started
/// from Next up would ride in each queue reply and first load for good.
pub(crate) fn forget_finished(
    tx: &mut store::Write<'_>,
    queue: &mut QueueView,
) -> store::Result<bool> {
    let followed: std::collections::HashSet<_> = (queue.items.iter())
        .filter(|item| item.is_pending())
        .flat_map(|item| item.depends_on.iter().copied())
        .collect();
    let mut finished = Vec::new();
    for item in &queue.items {
        if let QueueItemState::Started { run_id } = item.state
            && !followed.contains(&item.item_id)
            && tx.read().get::<Run>("run", run_id)?.is_none_or(|run| {
                run.state.is_terminal()
                    && !still_finishing(&run)
                    && run
                        .outcome
                        .as_ref()
                        .is_none_or(|o| o.still_active.is_empty() && o.unknowns.is_empty())
                    && !(run.children.iter())
                        .any(|c| matches!(c.state, ChildState::Active | ChildState::Unknown))
            })
        {
            finished.push(item.item_id);
        }
    }
    if finished.is_empty() {
        return Ok(false);
    }
    queue.items.retain(|item| !finished.contains(&item.item_id));
    for (position, item) in queue.items.iter_mut().enumerate() {
        item.position = position as u32;
    }
    for item_id in finished {
        tx.delete("queue_permit", item_id)?;
    }
    bump_queue_revision(tx, queue)?;
    Ok(true)
}
pub(crate) fn bump_queue_revision(
    tx: &mut store::Write<'_>,
    queue: &mut QueueView,
) -> store::Result<()> {
    let previous = queue.revision;
    queue.revision += 1;
    for item in &queue.items {
        if let Some(mut permit) = tx.read().get::<QueuePermit>("queue_permit", item.item_id)?
            && permit.queue_revision == previous
        {
            permit.queue_revision = queue.revision;
            tx.put("queue_permit", item.item_id, &permit)?;
        }
    }
    Ok(())
}
/// Keeps the go-ahead of `task`'s place in Next up when only Ophio's own
/// record of it moved on from `previous`, a pull request's state, say, and
/// not what the person let start. Left behind, it would wait unseen at the
/// head of Next up, holding up everything after it.
pub(crate) fn carry_permits(
    tx: &mut store::Write<'_>,
    task: &Task,
    previous: u64,
) -> store::Result<()> {
    let Some(queue) = tx.read().get::<QueueView>("queue", task.project_id)? else {
        return Ok(());
    };
    for item in (queue.items.iter()).filter(|i| i.task_id == task.task_id && i.is_pending()) {
        if let Some(mut permit) = tx.read().get::<QueuePermit>("queue_permit", item.item_id)?
            && permit.task_revision == previous
        {
            permit.task_revision = task.revision;
            tx.put("queue_permit", item.item_id, &permit)?;
        }
    }
    Ok(())
}
/// A turn that ended while helpers it started still run keeps the project's
/// writer, and stays stoppable, until those helpers end.
pub(crate) fn helpers_finishing(run: &Run) -> bool {
    run.ended_at.is_some() && run.state.is_terminal() && run.holds_writer
}
/// Records whether what the run started is still unconfirmed ended, and
/// publishes the run when that changes, so the phone keeps or drops Stop.
pub(crate) fn set_cleanup_pending(
    tx: &mut store::Write<'_>,
    id: RunId,
    pending: bool,
) -> store::Result<()> {
    let mut run: Run = tx.read().require("run", id)?;
    if run.cleanup_pending != pending {
        run.cleanup_pending = pending;
        tx.put("run", id, &run)?;
        tx.emit(Event::RunUpserted { run: Box::new(run) })?;
    }
    Ok(())
}
/// A run whose turn ended while something it started may still run: helpers
/// a Work turn left, or leftovers not yet confirmed ended. Stop still reaches
/// it, it keeps its task, and the task can't be archived.
pub(crate) fn still_finishing(run: &Run) -> bool {
    helpers_finishing(run)
        || (run.ended_at.is_some() && run.state.is_terminal() && run.cleanup_pending)
}
/// A task's ended run whose leftovers were never confirmed ended, once
/// nothing is left to confirm it: the person checks the computer and
/// reconciles the task. A Work run keeps the project until they do. Helpers
/// a Work turn kept the project for count, even as saved before they were
/// recorded unconfirmed.
pub(crate) fn cleanup_unconfirmed(
    tx: &mut store::Write<'_>,
    run: &mut Run,
    task: &mut Task,
    now: protocol::time::Timestamp,
) -> store::Result<()> {
    if !still_finishing(run) || task.current_run != Some(run.run_id) {
        return Ok(());
    }
    run.state = RunState::ReconciliationRequired;
    tx.put("run", run.run_id, &*run)?;
    tx.emit(Event::RunUpserted {
        run: Box::new(run.clone()),
    })?;
    tx.emit(Event::TimelineAppended {
        task_id: task.task_id,
        item: TimelineItem {
            seq: 0,
            at: now,
            run_id: Some(run.run_id),
            item: TimelineBody::Note {
                text: "Øphio can no longer confirm that everything the agent started has ended. Check the computer, then mark the task completed or failed.".into(),
            },
        },
    })?;
    let attention = AttentionItem {
        child_id: None,
        attention_id: AttentionId::new(),
        kind: AttentionKind::ReconciliationRequired,
        title: task.title.clone(),
        body: None,
        task_id: Some(task.task_id),
        run_id: Some(run.run_id),
        approval_id: None,
        created_at: now,
        resolved_at: None,
        priority: 5,
        topic: None,
        quiet: false,
        report_id: None,
    };
    tx.put("attention", attention.attention_id, &attention)?;
    tx.emit(Event::AttentionUpserted { item: attention })?;
    task.state = TaskState::ReconciliationRequired;
    task.revision += 1;
    task.updated_at = now;
    task.attention_count = blocking_attention_count(&tx.read(), task.task_id)?;
    tx.put("task", task.task_id, &*task)?;
    tx.emit(Event::TaskUpserted { task: task.clone() })?;
    Ok(())
}
/// What a Stop receipt says about everything the run started, from how far
/// its adapter reaches on this computer.
fn started_work(containment: &Containment, process_ended: bool) -> ReceiptFact {
    let (status, detail) = match containment {
        Containment::Complete if process_ended => (
            FactStatus::Done,
            "Ended with the agent, including work it had moved into the background. Anything it had the system start separately, like a service or a scheduled task, isn’t covered.".to_owned(),
        ),
        Containment::Complete => (
            FactStatus::Unknown,
            "It all ends with the agent, but it hadn’t all ended when this receipt was written. Check the computer.".to_owned(),
        ),
        Containment::ProcessGroup { reason } => (
            FactStatus::Unknown,
            format!("Only work that stayed in the agent’s process group ends with it. Work it moved into a session of its own may still be running, because {reason}. Check the computer."),
        ),
    };
    ReceiptFact {
        label: "Everything the agent started".into(),
        status,
        detail: Some(detail),
    }
}
fn empty_outcome() -> RunOutcome {
    RunOutcome {
        summary: None,
        changed_files: Vec::new(),
        checks: Vec::new(),
        still_active: Vec::new(),
        unknowns: Vec::new(),
        usage: None,
        report: None,
        verification: None,
    }
}
/// The Next up item whose go-ahead the person was last asked for, one per
/// project.
const QUEUE_ASKED: &str = "queue_asked";

/// Queue permission is companion-owned attention, so background notifications
/// and foreground badges refer to the same durable decision. One per project,
/// raised only once the go-ahead is all that holds the front item back, so
/// its age is how long the start has waited on the person.
pub(crate) fn sync_queue_attention(
    tx: &mut store::Write<'_>,
    queue: &QueueView,
    now: protocol::time::Timestamp,
) -> store::Result<()> {
    let front = queue.items.iter().find(|item| item.is_pending());
    let waiting = if !queue.paused {
        front.filter(|item| matches!(item.state, QueueItemState::AwaitingStart))
    } else {
        None
    };
    let task = match waiting {
        Some(item) => tx.read().get::<Task>("task", item.task_id)?,
        None => None,
    }
    .map(|task| {
        crate::runs::queue_completion::waits_to_start(&tx.read(), &task)
            .map(|waits| waits.then_some(task))
    })
    .transpose()?
    .flatten()
    .filter(|task| task.mode != TaskMode::Work || queue.writer.is_none());
    let previous = tx
        .read()
        .get::<AttentionId>("queue_attention", queue.project_id)?;
    if let Some(id) = previous {
        let mut item: AttentionItem = tx.read().require("attention", id)?;
        if item.resolved_at.is_none() && item.task_id == task.as_ref().map(|task| task.task_id) {
            return Ok(());
        }
        if item.resolved_at.is_none() {
            item.resolved_at = Some(now);
            tx.put("attention", id, &item)?;
            tx.emit(Event::AttentionResolved { attention_id: id })?;
            if let Some(task_id) = item.task_id {
                recount_attention(tx, task_id)?;
            }
        }
        tx.delete("queue_attention", queue.project_id)?;
    }
    // The person was asked for this one's go-ahead already: put away while
    // the project was busy or Next up paused, it comes back without another
    // alert. Asked again once it has left Next up and come back.
    let kept = tx
        .read()
        .get::<QueueItemId>(QUEUE_ASKED, queue.project_id)?;
    let asked = kept.filter(|id| {
        queue
            .items
            .iter()
            .any(|i| i.item_id == *id && i.is_pending())
    });
    if kept.is_some() && asked.is_none() {
        tx.delete(QUEUE_ASKED, queue.project_id)?;
    }
    if let Some(task) = task {
        let mut item = go_ahead(&task, now);
        if let Some(front) = waiting {
            item.quiet = asked == Some(front.item_id);
            tx.put(QUEUE_ASKED, queue.project_id, &front.item_id)?;
        }
        tx.put("queue_attention", queue.project_id, &item.attention_id)?;
        tx.put("attention", item.attention_id, &item)?;
        tx.emit(Event::AttentionUpserted { item })?;
        recount_attention(tx, task.task_id)?;
    }
    Ok(())
}

/// What asks a person, at `now`, for the go-ahead `task` waits on at the
/// front of Next up.
pub(crate) fn go_ahead(task: &Task, now: protocol::time::Timestamp) -> AttentionItem {
    AttentionItem {
        attention_id: AttentionId::new(),
        child_id: None,
        kind: AttentionKind::WaitingForYou,
        title: task.title.clone(),
        body: Some("Next up needs your go-ahead · Review and start".into()),
        task_id: Some(task.task_id),
        run_id: None,
        approval_id: None,
        created_at: now,
        resolved_at: None,
        priority: 70,
        topic: None,
        quiet: false,
        report_id: None,
    }
}

pub(crate) fn resolve_attention(
    tx: &mut store::Write<'_>,
    approval_id: ApprovalId,
    now: protocol::time::Timestamp,
) -> store::Result<()> {
    for mut item in tx.read().list::<AttentionItem>("attention")? {
        if item.approval_id == Some(approval_id) && item.resolved_at.is_none() {
            item.resolved_at = Some(now);
            tx.put("attention", item.attention_id, &item)?;
            tx.emit(Event::AttentionResolved {
                attention_id: item.attention_id,
            })?;
            if let Some(task_id) = item.task_id {
                recount_attention(tx, task_id)?;
            }
        }
    }
    Ok(())
}

/// Resolves what is still open about `task_id`: an archived task asks
/// nothing more of anyone, so its notifications leave every phone.
pub(crate) fn resolve_task_attention(
    tx: &mut store::Write<'_>,
    task_id: TaskId,
    now: protocol::time::Timestamp,
) -> store::Result<()> {
    for mut item in tx.read().task_attention::<AttentionItem>(task_id)? {
        if item.task_id == Some(task_id) && item.resolved_at.is_none() {
            item.resolved_at = Some(now);
            tx.put("attention", item.attention_id, &item)?;
            tx.emit(Event::AttentionResolved {
                attention_id: item.attention_id,
            })?;
        }
    }
    Ok(())
}

/// Resolves what archived tasks left open: archiving once left a task's
/// notifications showing on every phone.
pub(crate) fn settle_archived_attention(
    tx: &mut store::Write<'_>,
    now: protocol::time::Timestamp,
) -> store::Result<()> {
    let mut archived = std::collections::BTreeSet::new();
    for item in tx.read().list::<AttentionItem>("attention")? {
        let Some(task_id) = item.task_id.filter(|_| item.resolved_at.is_none()) else {
            continue;
        };
        if !archived.contains(&task_id)
            && (tx.read().get::<Task>("task", task_id)?)
                .is_some_and(|t| t.state == TaskState::Archived)
        {
            archived.insert(task_id);
        }
    }
    for task_id in archived {
        resolve_task_attention(tx, task_id, now)?;
        recount_attention(tx, task_id)?;
    }
    Ok(())
}

/// Results remain available for review, but only unresolved blockers count.
pub(crate) fn blocking_attention_count(
    read: &store::Read<'_>,
    task_id: TaskId,
) -> store::Result<u32> {
    Ok(read
        .task_attention::<AttentionItem>(task_id)?
        .iter()
        .filter(|a| {
            a.task_id == Some(task_id)
                && a.resolved_at.is_none()
                && !matches!(a.kind, AttentionKind::Completed | AttentionKind::Failure)
        })
        .count() as u32)
}

/// Stop fences on the run the person saw, not on the task's latest revision:
/// the agent's own progress on that run moves the revision all the time. A
/// revision taken while the run was already current names it; one from
/// before it began never does. Runs admitted by older versions match exactly.
pub(crate) fn names_run(
    read: &store::Read<'_>,
    task: &Task,
    run_id: RunId,
    expected: u64,
) -> store::Result<bool> {
    Ok(task.current_run == Some(run_id)
        && read
            .get::<u64>("run_task_revision", run_id)?
            .is_some_and(|since| since <= expected && expected <= task.revision))
}

pub(crate) fn recount_attention(tx: &mut store::Write<'_>, task_id: TaskId) -> store::Result<()> {
    let count = blocking_attention_count(&tx.read(), task_id)?;
    let mut task: Task = tx.read().require("task", task_id)?;
    if task.attention_count != count {
        task.attention_count = count;
        tx.put("task", task_id, &task)?;
        tx.emit(Event::TaskUpserted { task })?;
    }
    Ok(())
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use crate::{
        Hub,
        api::Backlog,
        testing::{FakeClock, FakeHarness},
    };
    use protocol::{annotation::*, capability::*};
    use std::sync::{Arc, atomic::Ordering};

    struct Fixture {
        hub: CompanionHub,
        path: PathBuf,
        harness: Arc<FakeHarness>,
        clock: Arc<FakeClock>,
        project: ProjectId,
    }

    thread_local! {
        static EXIT_WRITE_FAILURES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn count_exit_write_failures(event: rusqlite::trace::TraceEvent<'_>) {
        if let rusqlite::trace::TraceEvent::Stmt(_, sql) = event
            && sql.contains("issue215 disk full")
        {
            EXIT_WRITE_FAILURES.with(|count| count.set(count.get() + 1));
        }
    }

    async fn exit_failure_fixture() -> (Fixture, TaskId, RunId) {
        let f = Fixture::new().await;
        let task = f.task(Some(TaskMode::Work)).await;
        let id = task.current_run.unwrap();
        f.dispatched(id).await;
        f.hub
            .harness_event(
                id,
                HarnessEvent::SessionStarted {
                    native_session: "exit-recovery".into(),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await
            .unwrap();
        (f, task.task_id, id)
    }

    fn refuse_exit_write(f: &Fixture, on: &str) -> rusqlite::Connection {
        let db = rusqlite::Connection::open(f.path.join("companion.sqlite")).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER refuse_exit {on}
             BEGIN SELECT RAISE(ABORT, 'issue215 disk full'); END;"
        ))
        .unwrap();
        EXIT_WRITE_FAILURES.with(|count| count.set(0));
        f.hub
            .inner
            .db
            .lock()
            .unwrap()
            .trace(Some(count_exit_write_failures));
        db
    }

    async fn exit_write_failed(times: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while EXIT_WRITE_FAILURES.with(|count| count.get()) < times {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the exit write was never attempted");
    }

    fn refuse_post_completion_revocation(f: &Fixture) -> rusqlite::Connection {
        // Seed a synthetic revocation row after end_run's token deletion,
        // so the failing DELETE occurs after completion has committed.
        let db = refuse_exit_write(
            f,
            "BEFORE DELETE ON documents WHEN OLD.kind = 'run_token' AND EXISTS (SELECT 1 FROM documents WHERE kind = 'run' AND id = OLD.id AND json_extract(json, '$.state') = 'completed')",
        );
        db.execute_batch("CREATE TRIGGER seed_revocation AFTER UPDATE ON documents
            WHEN NEW.kind = 'run' AND json_extract(NEW.json, '$.state') = 'completed'
            BEGIN INSERT OR IGNORE INTO documents(kind, id, json) VALUES ('run_token', NEW.id, '\"synthetic\"'); END;").unwrap();
        db
    }

    #[tokio::test]
    async fn exit_write_failure_recovers_the_outcome_and_releases_the_writer() {
        for (on, failures, exit, state, task_state) in [
            (
                "BEFORE INSERT ON documents WHEN NEW.kind = 'run_process_ended'",
                1,
                RunExit::Completed,
                RunState::Completed,
                TaskState::Completed,
            ),
            (
                "BEFORE INSERT ON documents WHEN NEW.kind = 'task' AND json_extract(NEW.json, '$.state') = 'completed'",
                3,
                RunExit::Completed,
                RunState::Completed,
                TaskState::Completed,
            ),
            (
                "BEFORE INSERT ON documents WHEN NEW.kind = 'task' AND json_extract(NEW.json, '$.state') = 'failed'",
                2,
                RunExit::Failed {
                    reason: "Synthetic native failure".into(),
                },
                RunState::Failed,
                TaskState::Failed,
            ),
        ] {
            let (f, task, id) = exit_failure_fixture().await;
            let db = refuse_exit_write(&f, on);
            let mut outcome = empty_outcome();
            outcome.summary = Some("The finished agent's result".into());
            f.harness
                .emit(
                    id,
                    HarnessEvent::Exited {
                        exit,
                        outcome: Some(outcome.clone()),
                    },
                )
                .await;
            exit_write_failed(failures).await;
            assert_writer(&f, Some(id));
            assert_eq!(
                f.hub
                    .read(|r| r.require::<Task>("task", task))
                    .unwrap()
                    .state,
                TaskState::Working
            );
            assert!(!f.hub.inner.runs.lock().await.contains_key(&id));
            assert!(!f.hub.inner.liveness.lock().unwrap().contains_key(&id));
            assert_eq!(f.harness.controls.interrupts.load(Ordering::SeqCst), 0);
            db.execute_batch("DROP TRIGGER refuse_exit;").unwrap();
            let recovered = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
                    if run.state == state {
                        return run;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await;
            let saved: Task = f.hub.read(|r| r.require("task", task)).unwrap();
            let project: Project = f.hub.read(|r| r.require("project", f.project)).unwrap();
            let history = timeline(&f, task);
            f.hub.inner.db.lock().unwrap().trace(None);
            f.close().await;
            let run = recovered.unwrap_or_else(|_| {
                panic!(
                    "exit recovery lost: task={:?}, writer={:?}",
                    saved.state, project.active_writer,
                )
            });
            assert_eq!(run.outcome, Some(outcome));
            assert!(!run.holds_writer);
            assert_eq!(saved.state, task_state);
            assert_eq!(project.active_writer, None);
            assert_eq!(history.iter().filter(|item| matches!(item.item, TimelineBody::RunEnded { run_id, .. } if run_id == id)).count(), 1);
            assert_eq!(history.iter().filter(|item| matches!(&item.item, TimelineBody::Note { text } if text.contains("Synthetic native failure"))).count(), usize::from(state == RunState::Failed));
        }
    }

    #[tokio::test]
    async fn exit_write_failure_after_completion_still_advances_the_queue() {
        let (f, _, id) = exit_failure_fixture().await;
        let mut command = f.create(Some(TaskMode::Work));
        if let Command::CreateTask { placement, .. } = &mut command.body {
            *placement = TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            };
        }
        let receipt = f.hub.submit(&Caller::local_owner(), command).await;
        let Some(CommandResult::TaskCreated {
            task_id: waiting, ..
        }) = receipt.result
        else {
            panic!("{receipt:?}")
        };
        let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        let mut resume = CommandEnvelope::new(Command::QueueResume {
            project_id: f.project,
        });
        resume.expected_revision = Some(queue.revision);
        assert_eq!(
            f.hub.submit(&Caller::local_owner(), resume).await.status,
            ReceiptStatus::Accepted
        );
        let db = refuse_post_completion_revocation(&f);
        f.harness
            .emit(
                id,
                HarnessEvent::Exited {
                    exit: RunExit::Completed,
                    outcome: None,
                },
            )
            .await;
        exit_write_failed(1).await;
        assert_eq!(
            f.hub.read(|r| r.require::<Run>("run", id)).unwrap().state,
            RunState::Completed
        );
        assert!(
            f.hub
                .read(|r| r.require::<Task>("task", waiting))
                .unwrap()
                .current_run
                .is_none()
        );
        db.execute_batch("DROP TRIGGER refuse_exit; DROP TRIGGER seed_revocation;")
            .unwrap();
        let next = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let task: Task = f.hub.read(|r| r.require("task", waiting)).unwrap();
                if let Some(run) = task.current_run {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("recovered completion did not start queued work");
        assert_writer(&f, Some(next));
        f.dispatched(next).await;
        f.close().await;
    }

    #[tokio::test]
    async fn exit_write_failure_keeps_a_continuation_authorized_during_recovery() {
        let (f, task, id) = exit_failure_fixture().await;
        let db = refuse_post_completion_revocation(&f);
        f.harness
            .emit(
                id,
                HarnessEvent::Exited {
                    exit: RunExit::Completed,
                    outcome: None,
                },
            )
            .await;
        exit_write_failed(1).await;

        // Completion freed the writer before its revocation could be saved.
        // The person's next message waits behind another task's Work run.
        let writer = f.task(Some(TaskMode::Work)).await.current_run.unwrap();
        f.dispatched(writer).await;
        let finished: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        let mut next = CommandEnvelope::new(Command::QueueContinuation {
            task_id: task,
            text: "Continue when the project is free".into(),
            bundle: None,
            authorization: QueueAuthorization::AutoStart,
        });
        next.expected_revision = Some(finished.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), next).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");

        // Force more than one retry after that go-ahead was recorded.
        let failures = EXIT_WRITE_FAILURES.with(|count| count.get());
        exit_write_failed(failures + 3).await;
        db.execute_batch("DROP TRIGGER refuse_exit; DROP TRIGGER seed_revocation;")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while f
                .hub
                .read(|r| r.get::<String>("run_token", id))
                .unwrap()
                .is_some()
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("completion revocation never recovered");

        f.hub
            .harness_event(
                writer,
                HarnessEvent::Exited {
                    exit: RunExit::Completed,
                    outcome: None,
                },
            )
            .await
            .unwrap();
        let resumed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let task: Task = f.hub.read(|r| r.require("task", task)).unwrap();
                if let Some(run) = task.current_run.filter(|run| *run != id) {
                    return f.dispatched(run).await;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        f.close().await;
        let resumed = resumed.unwrap_or_else(|_| {
            panic!("exit retries withdrew the continuation's go-ahead: {queue:?}")
        });
        assert_eq!(resumed.prompt.text, "Continue when the project is free");
    }

    #[tokio::test]
    async fn exit_write_failure_without_exit_evidence_retries_reconciliation() {
        let (f, task, id) = exit_failure_fixture().await;
        let db = refuse_exit_write(&f, "BEFORE INSERT ON journal");
        f.harness
            .emit(
                id,
                HarnessEvent::Warning {
                    text: "Synthetic event".into(),
                },
            )
            .await;
        exit_write_failed(3).await;
        db.execute_batch("DROP TRIGGER refuse_exit;").unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while f
                .hub
                .read(|r| r.require::<Task>("task", task))
                .unwrap()
                .state
                != TaskState::ReconciliationRequired
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("lost events never became visible for reconciliation");
        assert_writer(&f, Some(id));
        assert_eq!(f.harness.controls.interrupts.load(Ordering::SeqCst), 1);
        f.close().await;
    }

    #[tokio::test]
    async fn exit_write_failure_does_not_hold_shutdown_and_restart_reconciles() {
        let (mut f, task, id) = exit_failure_fixture().await;
        let db = refuse_exit_write(
            &f,
            "BEFORE INSERT ON documents WHEN NEW.kind = 'run_process_ended'",
        );
        f.harness
            .emit(
                id,
                HarnessEvent::Exited {
                    exit: RunExit::Completed,
                    outcome: None,
                },
            )
            .await;
        exit_write_failed(2).await;
        tokio::time::timeout(Duration::from_secs(2), f.hub.shutdown())
            .await
            .expect("exit recovery held shutdown")
            .unwrap();
        db.execute_batch("DROP TRIGGER refuse_exit;").unwrap();
        drop(f.hub);
        f.hub = CompanionHub::builder()
            .data_dir(&f.path)
            .clock(f.clock.clone())
            .harness(f.harness.clone())
            .build()
            .await
            .unwrap();
        assert_eq!(
            f.hub
                .read(|r| r.require::<Task>("task", task))
                .unwrap()
                .state,
            TaskState::ReconciliationRequired
        );
        assert_writer(&f, Some(id));
        assert_eq!(f.harness.starts.load(Ordering::SeqCst), 1);
        f.close().await;
    }

    #[tokio::test]
    async fn failed_continuations_preserve_session_and_original_mode() {
        for original_mode in [TaskMode::Work, TaskMode::Plan] {
            let f = Fixture::new().await;
            let task = f.task(Some(original_mode)).await;
            let first = task.current_run.unwrap();
            f.dispatched(first).await;
            f.hub
                .harness_event(
                    first,
                    HarnessEvent::SessionStarted {
                        native_session: "session-A".into(),
                        actual_model: None,
                        actual_effort: None,
                        account_label: None,
                    },
                )
                .await
                .unwrap();
            f.hub.finish_run(first, RunState::Completed, None).unwrap();
            for attempt in 0..4 {
                f.harness.fail_spawn.store(attempt < 2, Ordering::SeqCst);
                let mut request = f.resume(task.task_id, Some("Continue".into()), None);
                if original_mode == TaskMode::Plan
                    && attempt == 0
                    && let Command::ResumeTask { mode, .. } = &mut request.body
                {
                    *mode = Some(TaskMode::Work);
                }
                let receipt = f.hub.submit(&Caller::local_owner(), request).await;
                assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
                let current: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
                let run = current.current_run.unwrap();
                let spec = f.dispatched(run).await;
                assert_eq!(
                    spec.resume_session.as_deref(),
                    Some(if attempt == 3 {
                        "session-B"
                    } else {
                        "session-A"
                    }),
                    "attempt {attempt}"
                );
                assert_eq!(
                    spec.resume_from_mode,
                    Some(if attempt == 3 {
                        TaskMode::Work
                    } else {
                        original_mode
                    }),
                    "attempt {attempt}"
                );
                assert_eq!(spec.mode, TaskMode::Work);
                if attempt < 2 {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            let failed: Run = f.hub.read(|r| r.require("run", run)).unwrap();
                            if failed.state == RunState::Failed {
                                assert!(failed.native_session.is_none());
                                assert!(failed.acknowledged_at.is_none());
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .unwrap();
                } else {
                    f.hub
                        .harness_event(
                            run,
                            HarnessEvent::SessionStarted {
                                native_session: "session-B".into(),
                                actual_model: None,
                                actual_effort: None,
                                account_label: None,
                            },
                        )
                        .await
                        .unwrap();
                    f.hub.finish_run(run, RunState::Completed, None).unwrap();
                }
            }
            f.close().await;
        }
    }

    #[tokio::test]
    async fn continuation_does_not_inherit_another_harness_session() {
        for acknowledged in [false, true] {
            let f = Fixture::new().await;
            let task = f.task(Some(TaskMode::Work)).await;
            let first = task.current_run.unwrap();
            f.dispatched(first).await;
            f.hub.finish_run(first, RunState::Completed, None).unwrap();
            f.hub
                .write(|tx| {
                    let mut run: Run = tx.read().require("run", first)?;
                    run.harness = HarnessKind::Codex;
                    run.native_session = acknowledged.then(|| "other-session".into());
                    tx.put("run", first, &run)?;
                    let mut intent: RunIntent = tx.read().require("run_intent", first)?;
                    intent.resume = Some("other-session".into());
                    intent.resume_from_mode = Some(TaskMode::Plan);
                    tx.put("run_intent", first, &intent)
                })
                .unwrap();
            let receipt = f
                .hub
                .submit(&Caller::local_owner(), f.resume(task.task_id, None, None))
                .await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted);
            let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
            let spec = f.dispatched(task.current_run.unwrap()).await;
            assert!(spec.resume_session.is_none());
            assert!(spec.resume_from_mode.is_none());
            f.close().await;
        }
    }

    async fn settle_followup(f: &Fixture, command_id: CommandId) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while f
                .hub
                .read(|r| r.command(command_id))
                .unwrap()
                .unwrap()
                .pending
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    async fn restart_after_crash(f: Fixture) -> Fixture {
        // Abort before yielding: models a crash before the admitted worker runs.
        let jobs = std::mem::take(&mut *f.hub.inner.jobs.lock().unwrap());
        for job in &jobs {
            job.abort();
        }
        for job in jobs {
            let _ = job.await;
        }
        drop(f.hub);
        let hub = CompanionHub::builder()
            .data_dir(&f.path)
            .clock(f.clock.clone())
            .harness(f.harness.clone())
            .build()
            .await
            .unwrap();
        Fixture { hub, ..f }
    }

    async fn assert_recoverable_followup(
        f: &Fixture,
        task: TaskId,
        run: RunId,
        seq: u64,
        bundle: &AnnotationBundle,
        state: DeliveryState,
    ) {
        // Query the same public conversation projection the phone reads.
        let messages = f.messages(task).await;
        let found: Vec<_> = messages
            .iter()
            .filter(|m| {
                matches!(&m.item,
                    TimelineBody::UserMessage { text, .. } if text == "keep these words"
                )
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "accepted follow-up disappeared or duplicated"
        );
        assert_eq!(found[0].seq, seq);
        assert_eq!(found[0].run_id, Some(run));
        assert!(matches!(&found[0].item,
            TimelineBody::UserMessage { bundle: Some(saved), .. } if saved.as_ref() == bundle
        ));
        assert_eq!(deliveries(f, task), vec![(seq, state)]);
        assert!(f.harness.controls.message_refs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn admitted_followup_survives_stop_before_dispatch() {
        followup_execution_race("stop").await;
    }

    #[tokio::test]
    async fn admitted_followup_survives_completion_and_continuation_before_dispatch() {
        followup_execution_race("completed").await;
        followup_execution_race("continued").await;
    }

    #[tokio::test]
    async fn admitted_followup_survives_expiry_and_missing_control() {
        followup_execution_race("expired").await;
        followup_execution_race("no_control").await;
    }

    async fn followup_execution_race(end: &str) {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let task = f.task(Some(TaskMode::Work)).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        f.hub
            .harness_event(run, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        let bundle = f.bundle(task.task_id).await;
        let mut send = CommandEnvelope::new(Command::SendMessage {
            task_id: task.task_id,
            text: "keep these words".into(),
            bundle: Some(bundle.clone()),
            delivery: MessageDelivery::NextTurn,
        });
        send.expires_in_ms = Some(1000);
        let receipt = f.hub.submit(&owner, send.clone()).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let seq = f.messages(task.task_id).await.last().unwrap().seq;
        assert_eq!(
            f.hub.submit(&owner, send.clone()).await.status,
            ReceiptStatus::Duplicate
        );
        match end {
            "stop" => {
                let stop = CommandEnvelope::new(Command::StopTask {
                    task_id: task.task_id,
                    force: false,
                });
                assert_eq!(
                    f.hub.submit(&owner, stop).await.status,
                    ReceiptStatus::Accepted
                );
            }
            "completed" | "continued" => {
                // The same transaction used by a native Exited event.
                f.hub.finish_run(run, RunState::Completed, None).unwrap();
                if end == "continued" {
                    let resume = f.resume(task.task_id, Some("a new turn".into()), None);
                    assert_eq!(
                        f.hub.submit(&owner, resume).await.status,
                        ReceiptStatus::Accepted
                    );
                    let current: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
                    assert_ne!(current.current_run, Some(run));
                }
            }
            "expired" => f.clock.advance(Duration::from_secs(2)),
            "no_control" => {
                f.hub.inner.runs.lock().await.remove(&run);
            }
            _ => unreachable!(),
        }
        // No native send may run between admission and the competing event.
        assert!(f.harness.controls.message_refs.lock().unwrap().is_empty());
        settle_followup(&f, send.command_id).await;
        let state = if end == "stop" {
            DeliveryState::Cancelled
        } else {
            DeliveryState::Failed
        };
        assert_recoverable_followup(&f, task.task_id, run, seq, &bundle, state).await;
        let f = restart_after_crash(f).await;
        assert_eq!(
            f.hub.submit(&owner, send).await.status,
            ReceiptStatus::Duplicate
        );
        assert_recoverable_followup(&f, task.task_id, run, seq, &bundle, state).await;
        f.close().await;
    }

    #[tokio::test]
    async fn admitted_followup_survives_restart_before_execution() {
        for stopped in [false, true] {
            let f = Fixture::new().await;
            let owner = Caller::local_owner();
            let task = f.task(Some(TaskMode::Work)).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            f.hub
                .harness_event(run, HarnessEvent::TurnStarted)
                .await
                .unwrap();
            let bundle = f.bundle(task.task_id).await;
            let send = CommandEnvelope::new(Command::SendMessage {
                task_id: task.task_id,
                text: "keep these words".into(),
                bundle: Some(bundle.clone()),
                delivery: MessageDelivery::Steer,
            });
            assert_eq!(
                f.hub.submit(&owner, send.clone()).await.status,
                ReceiptStatus::Accepted
            );
            let seq = f.messages(task.task_id).await.last().unwrap().seq;
            if stopped {
                assert_eq!(
                    f.hub
                        .submit(
                            &owner,
                            CommandEnvelope::new(Command::StopTask {
                                task_id: task.task_id,
                                force: false,
                            })
                        )
                        .await
                        .status,
                    ReceiptStatus::Accepted
                );
            }
            assert!(f.harness.controls.message_refs.lock().unwrap().is_empty());
            let f = restart_after_crash(f).await;
            let state = if stopped {
                DeliveryState::Cancelled
            } else {
                DeliveryState::Failed
            };
            assert_recoverable_followup(&f, task.task_id, run, seq, &bundle, state).await;
            assert_eq!(
                f.hub.submit(&owner, send.clone()).await.status,
                ReceiptStatus::Duplicate
            );
            let f = restart_after_crash(f).await;
            assert_recoverable_followup(&f, task.task_id, run, seq, &bundle, state).await;
            f.close().await;
        }
    }

    async fn force_stop_fixture(hold_terminate: bool) -> (Fixture, Task, RunId) {
        let f = Fixture::new().await;
        let task = f.task(Some(TaskMode::Work)).await;
        let id = task.current_run.unwrap();
        f.dispatched(id).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            while !f
                .hub
                .read(|r| r.require::<Run>("run", id))
                .unwrap()
                .force_stoppable
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        f.harness
            .controls
            .hold_terminate
            .store(hold_terminate, Ordering::SeqCst);
        (f, task, id)
    }

    async fn submit_force_stop(f: &Fixture, task: TaskId) -> CommandId {
        let command = CommandEnvelope::new(Command::StopTask {
            task_id: task,
            force: true,
        });
        let id = command.command_id;
        let receipt = f.hub.submit(&Caller::local_owner(), command).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        tokio::time::timeout(
            Duration::from_secs(15),
            f.harness.controls.terminate_entered.acquire(),
        )
        .await
        .unwrap()
        .unwrap()
        .forget();
        id
    }

    async fn force_stop_result(f: &Fixture, command: CommandId) -> store::StoredCommand {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let command = f.hub.read(|r| r.command(command)).unwrap().unwrap();
                if !command.pending {
                    return command;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("Force stop did not settle")
    }

    fn force_stop_fact(f: &Fixture, task: TaskId) -> FactStatus {
        timeline(f, task)
            .into_iter()
            .rev()
            .find_map(|item| {
                if let TimelineBody::Receipt { receipt } = item.item {
                    receipt
                        .facts
                        .into_iter()
                        .find(|fact| fact.label == "Agent process ended on the computer")
                        .map(|fact| fact.status)
                } else {
                    None
                }
            })
            .expect("Force stop receipt missing")
    }

    #[tokio::test]
    async fn failed_force_stop_keeps_writer_until_exit() {
        uncertain_force_stop_keeps_writer(false, None).await;
    }

    #[tokio::test]
    async fn timed_out_force_stop_keeps_writer_until_exit() {
        uncertain_force_stop_keeps_writer(true, None).await;
    }

    #[tokio::test]
    async fn failed_force_stop_of_finished_parent_keeps_helpers_writer_until_exit() {
        uncertain_force_stop_keeps_writer(false, Some(RunExit::Completed)).await;
    }

    #[tokio::test]
    async fn failed_force_stop_of_uncertain_parent_releases_writer_on_late_exit() {
        uncertain_force_stop_keeps_writer(
            false,
            Some(RunExit::Unknown {
                reason: "parent outcome unconfirmed".into(),
            }),
        )
        .await;
    }

    #[tokio::test]
    async fn timed_out_force_stop_of_uncertain_parent_releases_writer_on_late_exit() {
        uncertain_force_stop_keeps_writer(
            true,
            Some(RunExit::Unknown {
                reason: "parent outcome unconfirmed".into(),
            }),
        )
        .await;
    }

    async fn uncertain_force_stop_keeps_writer(timeout: bool, parent_exit: Option<RunExit>) {
        let parent_ended = parent_exit.is_some();
        let parent_completed = parent_exit == Some(RunExit::Completed);
        let (f, task, id) = force_stop_fixture(timeout).await;
        if let Some(exit) = parent_exit {
            f.hub
                .harness_event(id, HarnessEvent::ParentEnded { exit })
                .await
                .unwrap();
        }
        let command = submit_force_stop(&f, task.task_id).await;
        if timeout {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(7)).await;
            assert!(f.hub.read(|r| r.command(command)).unwrap().unwrap().pending);
            assert_writer(&f, Some(id));
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        let result = force_stop_result(&f, command).await;
        if timeout {
            tokio::time::resume();
        }
        let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
        // The stream is still open. Neither a failed request nor a completed
        // parent turn establishes that the process can no longer write.
        assert_writer(&f, Some(id));
        assert!(run.holds_writer && run.force_stoppable);
        assert_eq!(
            run.state,
            if parent_completed {
                RunState::Completed
            } else {
                RunState::Stopping
            }
        );
        assert_eq!(run.ended_at.is_some(), parent_ended);
        assert_eq!(result.stage, CommandStage::ReconciliationRequired);
        assert_eq!(result.error.unwrap().code, ErrorCode::OutcomeUnknown);
        assert_eq!(force_stop_fact(&f, task.task_id), FactStatus::Unknown);
        let second = f
            .hub
            .submit(&Caller::local_owner(), f.create(Some(TaskMode::Work)))
            .await;
        assert_eq!(second.status, ReceiptStatus::Rejected, "{second:?}");
        assert_eq!(second.error.unwrap().code, ErrorCode::Conflict);
        let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        assert!(queue.paused);

        if parent_ended && !parent_completed {
            // Repeated parent evidence still does not establish process exit.
            f.hub
                .harness_event(
                    id,
                    HarnessEvent::ParentEnded {
                        exit: RunExit::Unknown {
                            reason: "parent outcome still unconfirmed".into(),
                        },
                    },
                )
                .await
                .unwrap();
            assert_writer(&f, Some(id));
            assert_eq!(
                f.hub.read(|r| r.require::<Run>("run", id)).unwrap().state,
                RunState::Stopping
            );
        }

        // A late, independent exit settles the run and releases both writer
        // projections, even after the Force stop command returned unknown.
        f.harness
            .emit(
                id,
                HarnessEvent::Exited {
                    exit: RunExit::Failed {
                        reason: "killed".into(),
                    },
                    outcome: None,
                },
            )
            .await;
        tokio::time::timeout(Duration::from_secs(15), async {
            while f.hub.inner.runs.lock().await.contains_key(&id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_writer(&f, None);
        let ended: Run = f.hub.read(|r| r.require("run", id)).unwrap();
        assert_eq!(
            ended.state,
            if parent_completed {
                RunState::Completed
            } else {
                RunState::Interrupted
            }
        );
        assert!(ended.ended_at.is_some());
        let second = f
            .hub
            .submit(&Caller::local_owner(), f.create(Some(TaskMode::Work)))
            .await;
        assert_eq!(second.status, ReceiptStatus::Accepted, "{second:?}");
        f.close().await;
    }

    #[tokio::test]
    async fn confirmed_force_stop_releases_writer_without_exit_event() {
        for parent_ended in [false, true] {
            let (f, task, id) = force_stop_fixture(false).await;
            if parent_ended {
                f.hub
                    .harness_event(
                        id,
                        HarnessEvent::ParentEnded {
                            exit: RunExit::Completed,
                        },
                    )
                    .await
                    .unwrap();
            }
            // An unsuccessful attempt remains retryable, including for helpers.
            let first = submit_force_stop(&f, task.task_id).await;
            assert_eq!(
                force_stop_result(&f, first).await.error.unwrap().code,
                ErrorCode::OutcomeUnknown
            );
            assert_writer(&f, Some(id));
            f.harness
                .controls
                .confirm_terminate
                .store(true, Ordering::SeqCst);
            let retry = submit_force_stop(&f, task.task_id).await;
            let result = force_stop_result(&f, retry).await;
            assert_eq!(result.stage, CommandStage::Observed, "{result:?}");
            assert!(result.error.is_none());
            assert_writer(&f, None);
            let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
            assert!(!run.holds_writer);
            assert_eq!(
                run.state,
                if parent_ended {
                    RunState::Completed
                } else {
                    RunState::Interrupted
                }
            );
            assert_eq!(force_stop_fact(&f, task.task_id), FactStatus::Done);
            f.close().await;
        }
    }

    #[tokio::test]
    async fn force_stop_accepts_independent_exit_despite_termination_failure_or_timeout() {
        for timeout in [false, true] {
            let (f, task, id) = force_stop_fixture(true).await;
            let command = submit_force_stop(&f, task.task_id).await;
            // Record exit while terminate is held, before its error/timeout.
            f.hub
                .harness_event(
                    id,
                    HarnessEvent::Exited {
                        exit: RunExit::Interrupted,
                        outcome: None,
                    },
                )
                .await
                .unwrap();
            assert_writer(&f, None);
            if timeout {
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(8)).await;
            } else {
                f.harness.controls.terminate_gate.add_permits(1);
            }
            let result = force_stop_result(&f, command).await;
            if timeout {
                tokio::time::resume();
            }
            assert_eq!(result.stage, CommandStage::Observed, "{result:?}");
            assert!(result.error.is_none());
            assert_eq!(force_stop_fact(&f, task.task_id), FactStatus::Done);
            f.close().await;
        }
    }

    #[tokio::test]
    async fn unacknowledged_stop_waits_for_exit_or_keeps_force_stop_available() {
        for ends in [true, false] {
            let f = Fixture::new().await;
            let task = f.task(Some(TaskMode::Work)).await;
            let id = task.current_run.unwrap();
            f.dispatched(id).await;
            while !f
                .hub
                .read(|r| r.require::<Run>("run", id))
                .unwrap()
                .force_stoppable
            {
                tokio::task::yield_now().await;
            }
            f.harness
                .controls
                .hold_interrupt
                .store(true, Ordering::SeqCst);
            tokio::time::pause();
            let harness = f.harness.clone();
            let hub = f.hub.clone();
            let exit = tokio::spawn(async move {
                if ends {
                    tokio::time::sleep(Duration::from_secs(6)).await;
                    harness
                        .emit(
                            id,
                            HarnessEvent::Exited {
                                exit: ports::RunExit::Interrupted,
                                outcome: None,
                            },
                        )
                        .await;
                    // Recording the exit runs git in real time. Stay runnable
                    // until it is recorded, so paused time cannot auto-advance
                    // past Stop's exit wait while git runs under load. The
                    // bound is wall-clock time, which paused time cannot reach.
                    let deadline = std::time::Instant::now() + Duration::from_secs(60);
                    while !hub
                        .read(|r| {
                            Ok(r.require::<Run>("run", id)?.ended_at.is_some()
                                && r.get::<bool>("run_process_ended", id)?.unwrap_or(false))
                        })
                        .unwrap()
                    {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "the run's exit was not recorded within 60 s"
                        );
                        tokio::task::yield_now().await;
                    }
                }
            });
            let result = f.hub.stop_task(task.task_id, None, false).await;
            exit.await.unwrap();
            let receipt = timeline(&f, task.task_id)
                .into_iter()
                .find_map(|item| {
                    if let TimelineBody::Receipt { receipt } = item.item {
                        Some(receipt)
                    } else {
                        None
                    }
                })
                .unwrap();
            let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
            let mut labels = vec!["Agent confirmed Stop", "Stop requests"];
            if ends {
                labels.push("Agent process ended on the computer");
            }
            labels.extend([
                "Helpers",
                "Desktop access withdrawn from the agent",
                "Next up paused",
            ]);
            assert_eq!(
                receipt
                    .facts
                    .iter()
                    .map(|fact| fact.label.as_str())
                    .collect::<Vec<_>>(),
                labels
            );
            assert!(
                receipt
                    .facts
                    .iter()
                    .any(|fact| fact.label == "Agent confirmed Stop"
                        && fact.status == FactStatus::Unknown)
            );
            if ends {
                result.unwrap();
                assert_eq!(run.state, RunState::Interrupted);
                assert!(
                    receipt
                        .facts
                        .iter()
                        .any(|fact| fact.label == "Agent process ended on the computer"
                            && fact.status == FactStatus::Done)
                );
                assert!(!receipt.summary.contains("Check"));
            } else {
                assert_eq!(result.unwrap_err().code, ErrorCode::OutcomeUnknown);
                assert!(run.ended_at.is_none());
                assert!(run.force_stoppable);
                assert!(receipt.summary.contains("Check the task on the computer"));
                assert!(
                    !receipt
                        .facts
                        .iter()
                        .any(|fact| fact.label == "Agent process ended on the computer")
                );
            }
            tokio::time::resume();
            f.close().await;
        }
    }

    #[tokio::test]
    async fn force_stop_keeps_the_run_and_its_writer_until_the_process_is_seen_to_end() {
        // The computer refuses to confirm the process ended, or never answers.
        for held in [false, true] {
            let f = Fixture::new().await;
            let task = f.task(Some(TaskMode::Work)).await;
            let id = task.current_run.unwrap();
            f.dispatched(id).await;
            while !f
                .hub
                .read(|r| r.require::<Run>("run", id))
                .unwrap()
                .force_stoppable
            {
                tokio::task::yield_now().await;
            }
            f.harness
                .controls
                .hold_terminate
                .store(held, Ordering::SeqCst);
            let force_stop = || async {
                let command = CommandEnvelope::new(Command::StopTask {
                    task_id: task.task_id,
                    force: true,
                });
                let command_id = command.command_id;
                let receipt = f.hub.submit(&Caller::local_owner(), command).await;
                assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
                // Past the eight seconds a held termination is given.
                tokio::time::timeout(Duration::from_secs(20), async {
                    loop {
                        let stored = f.hub.read(|r| r.command(command_id)).unwrap().unwrap();
                        if !stored.pending {
                            return stored.error;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .expect("Force stop did not settle")
            };
            let error = force_stop()
                .await
                .expect("an unconfirmed end was reported as done");
            assert_eq!(error.code, ErrorCode::OutcomeUnknown, "{error:?}");
            let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
            assert!(run.ended_at.is_none(), "{run:?}");
            assert!(run.holds_writer && run.force_stoppable, "{run:?}");
            // Nothing else may write to the project beside a process that may still be.
            assert_writer(&f, Some(id));
            // And Force stop can be tried again.
            f.harness
                .controls
                .hold_terminate
                .store(false, Ordering::SeqCst);
            let again = force_stop().await.expect("a second unconfirmed end");
            assert_eq!(again.code, ErrorCode::OutcomeUnknown, "{again:?}");
            assert_eq!(f.harness.controls.terminates.load(Ordering::SeqCst), 2);
            // Once the process is seen to end, the run ends as stopped.
            f.harness
                .emit(
                    id,
                    HarnessEvent::Exited {
                        exit: RunExit::Failed {
                            reason: "killed".into(),
                        },
                        outcome: None,
                    },
                )
                .await;
            tokio::time::timeout(Duration::from_secs(15), async {
                while f
                    .hub
                    .read(|r| r.require::<Run>("run", id))
                    .unwrap()
                    .ended_at
                    .is_none()
                {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("the exit was not recorded");
            let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
            assert_eq!(run.state, RunState::Interrupted);
            assert_writer(&f, None);
            f.close().await;
        }
    }

    #[tokio::test]
    async fn a_stop_that_waits_reaches_only_the_run_it_was_accepted_for() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let task = f.task(Some(TaskMode::Work)).await;
        let first = task.current_run.unwrap();
        f.dispatched(first).await;
        let hub = &f.hub;
        let force_stoppable = |id| async move {
            while !hub
                .read(|r| r.require::<Run>("run", id))
                .unwrap()
                .force_stoppable
            {
                tokio::task::yield_now().await;
            }
        };
        force_stoppable(first).await;
        // The Stop is accepted on a runtime nothing drives yet, so its effect
        // waits there while the run ends and the task is resumed.
        let stop = CommandEnvelope::new(Command::StopTask {
            task_id: task.task_id,
            force: true,
        });
        let stop_id = stop.command_id;
        let (accepted, admission) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let hub = f.hub.clone();
        let parked = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _ = accepted.send(runtime.block_on(hub.submit(&Caller::local_owner(), stop)));
            let _ = released.blocking_recv();
            runtime.block_on(async {
                while hub.read(|r| r.command(stop_id)).unwrap().unwrap().pending {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });
        });
        let receipt = admission.await.unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        f.harness
            .emit(
                first,
                HarnessEvent::Exited {
                    exit: RunExit::Interrupted,
                    outcome: None,
                },
            )
            .await;
        tokio::time::timeout(Duration::from_secs(15), async {
            while f.hub.inner.runs.lock().await.contains_key(&first) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the exit was not recorded");
        let receipt = f
            .hub
            .submit(&owner, f.resume(task.task_id, Some("Next".into()), None))
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let second = f
            .hub
            .read(|r| r.require::<Task>("task", task.task_id))
            .unwrap()
            .current_run
            .unwrap();
        assert_ne!(second, first);
        f.dispatched(second).await;
        force_stoppable(second).await;
        release.send(()).unwrap();
        tokio::task::spawn_blocking(move || parked.join().unwrap())
            .await
            .unwrap();
        // Nothing reached the run that started after the Stop was accepted.
        assert_eq!(f.harness.controls.terminates.load(Ordering::SeqCst), 0);
        let run: Run = f.hub.read(|r| r.require("run", second)).unwrap();
        assert!(
            run.ended_at.is_none() && run.state != RunState::Stopping,
            "{run:?}"
        );
        assert_writer(&f, Some(second));
        // The Stop settles against its own run, whose process had ended.
        let stored = f.hub.read(|r| r.command(stop_id)).unwrap().unwrap();
        assert_eq!(stored.error, None);
        let receipts: Vec<_> = timeline(&f, task.task_id)
            .into_iter()
            .filter_map(|item| match item.item {
                TimelineBody::Receipt { receipt } => Some((item.run_id, receipt)),
                _ => None,
            })
            .collect();
        assert_eq!(receipts.len(), 1, "{receipts:?}");
        assert_eq!(receipts[0].0, Some(first));
        assert!(
            receipts[0]
                .1
                .facts
                .iter()
                .any(|fact| fact.label == "Agent process ended on the computer"
                    && fact.status == FactStatus::Done),
            "{:?}",
            receipts[0].1
        );
        f.close().await;
    }

    #[tokio::test]
    async fn attention_counts_only_blockers_across_native_events_finish_and_acknowledgement() {
        for (state, expected) in [
            (RunState::Completed, 0),
            (RunState::Failed, 0),
            (RunState::ReconciliationRequired, 1),
        ] {
            let f = Fixture::new().await;
            let task = f.task(None).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            let result_id = AttentionId::new();
            f.hub
                .write(|tx| {
                    for (id, kind) in [
                        (result_id, AttentionKind::Completed),
                        (AttentionId::new(), AttentionKind::Failure),
                    ] {
                        let item = AttentionItem {
                            child_id: None,
                            attention_id: id,
                            kind,
                            title: task.title.clone(),
                            body: None,
                            task_id: Some(task.task_id),
                            run_id: Some(run),
                            approval_id: None,
                            created_at: task.created_at,
                            resolved_at: None,
                            priority: 5,
                            topic: None,
                            quiet: false,
                            report_id: None,
                        };
                        tx.put("attention", id, &item)?;
                    }
                    Ok(())
                })
                .unwrap();
            let count = || {
                f.hub
                    .read(|r| r.require::<Task>("task", task.task_id))
                    .unwrap()
                    .attention_count
            };
            f.hub
                .harness_event(
                    run,
                    HarnessEvent::Warning {
                        text: "Still running".into(),
                    },
                )
                .await
                .unwrap();
            assert_eq!(count(), 0, "native events exclude review results");
            f.hub
                .harness_event(
                    run,
                    HarnessEvent::ApprovalRequested(ports::NativeApproval {
                        native_request_id: "review".into(),
                        kind: ApprovalKind::Command,
                        title: "Review command".into(),
                        detail: None,
                        detail_lines: Vec::new(),
                        questions: Vec::new(),
                        raw_request: None,
                        scope: "once".into(),
                        shortened: false,
                        tool_call: None,
                    }),
                )
                .await
                .unwrap();
            assert_eq!(count(), 1, "pending approvals count");
            f.hub.finish_run(run, state, None).unwrap();
            assert_eq!(
                count(),
                expected,
                "completion cancels approvals and counts only blockers"
            );
            let Backlog::Events(events) = f
                .hub
                .subscribe(&Caller::local_owner(), Some(0))
                .await
                .unwrap()
                .backlog
            else {
                panic!("gap")
            };
            let ended: Vec<_> = events
                .iter()
                .filter_map(|e| match &e.event {
                    Event::TaskUpserted { task: t }
                        if t.task_id == task.task_id
                            && matches!(
                                t.state,
                                TaskState::Completed
                                    | TaskState::Failed
                                    | TaskState::ReconciliationRequired
                            ) =>
                    {
                        Some(t.attention_count)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                ended,
                vec![expected],
                "completion notification has the final blocker count"
            );
            let receipt = f
                .hub
                .submit(
                    &Caller::local_owner(),
                    CommandEnvelope::new(Command::AcknowledgeAttention {
                        attention_id: result_id,
                    }),
                )
                .await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted);
            assert_eq!(
                count(),
                expected,
                "acknowledging a result cannot change blocker count"
            );
            f.close().await;
        }
    }

    #[tokio::test]
    async fn helper_exit_evidence_and_human_checks_are_separate() {
        for lifetime in [
            Some(ChildLifetime::InProcess),
            Some(ChildLifetime::SeparateProcess),
            None,
        ] {
            let mut f = Fixture::new().await;
            let task = f.task(None).await;
            let id = task.current_run.unwrap();
            f.dispatched(id).await;
            f.hub
                .harness_event(
                    id,
                    HarnessEvent::Child {
                        native_ref: "summary".into(),
                        label: "Summarize the README".into(),
                        state: ChildState::Active,
                        lifetime,
                    },
                )
                .await
                .unwrap();
            // A lost connection alone cannot establish an in-process helper ended.
            f.hub.child_control_closed(id).unwrap();
            let disconnected: Run = f.hub.read(|r| r.require("run", id)).unwrap();
            assert_eq!(disconnected.children[0].state, ChildState::Unknown);
            assert_ne!(disconnected.children[0].provenance, Provenance::Observed);
            f.hub
                .harness_event(
                    id,
                    HarnessEvent::Exited {
                        exit: RunExit::Completed,
                        outcome: None,
                    },
                )
                .await
                .unwrap();
            let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
            let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
            assert_eq!(run.state, RunState::Completed);
            if lifetime == Some(ChildLifetime::InProcess) {
                assert_eq!(run.children[0].state, ChildState::Stopped);
                assert_eq!(run.children[0].provenance, Provenance::Observed);
                assert!(run.children[0].ended_at.is_some());
                assert!(
                    matches!(&run.children[0].stop, Some(Availability::Unsupported { reason }) if reason == "Ended with the agent process")
                );
                assert_eq!(task.attention_count, 0);
            } else {
                assert_eq!(run.children[0].state, ChildState::Unknown);
                assert_eq!(task.attention_count, 1);
                let attention: Vec<AttentionItem> = f.hub.read(|r| r.list("attention")).unwrap();
                let helper = attention
                    .iter()
                    .find(|a| a.child_id.is_some() && a.resolved_at.is_none())
                    .unwrap();
                assert!(helper.title.contains(&task.title));
                assert!(
                    helper
                        .title
                        .contains("Helper 'Summarize the README' — state unknown")
                );
                assert!(helper.body.as_ref().unwrap().contains("conversation"));
                assert!(!helper.body.as_ref().unwrap().contains("terminal"));
                let mut check = CommandEnvelope::new(Command::CheckHelpers { run_id: id });
                check.expected_revision = Some(task.revision - 1);
                assert_eq!(
                    f.hub.submit(&Caller::local_owner(), check).await.status,
                    ReceiptStatus::Rejected
                );
                let mut check = CommandEnvelope::new(Command::CheckHelpers { run_id: id });
                check.expected_revision = Some(task.revision);
                let receipt = f.hub.submit(&Caller::local_owner(), check).await;
                assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
                let checked: Run = f.hub.read(|r| r.require("run", id)).unwrap();
                assert_eq!(
                    checked.children, run.children,
                    "A person's check never verifies helper facts"
                );
                assert_eq!(checked.state, run.state);
                assert!(checked.human_resolution.is_none());
                assert_eq!(
                    checked.helpers_checked.as_ref().unwrap().by,
                    Caller::local_owner().principal
                );
                assert_eq!(
                    f.hub
                        .read(|r| r.require::<Task>("task", task.task_id))
                        .unwrap()
                        .attention_count,
                    0
                );
                f.hub.child_control_closed(id).unwrap();
                assert_eq!(
                    f.hub
                        .read(|r| r.require::<Task>("task", task.task_id))
                        .unwrap()
                        .attention_count,
                    0
                );
                f.hub.shutdown().await.unwrap();
                drop(f.hub);
                f.hub = CompanionHub::builder()
                    .data_dir(&f.path)
                    .clock(f.clock.clone())
                    .harness(f.harness.clone())
                    .build()
                    .await
                    .unwrap();
                let restored: Run = f.hub.read(|r| r.require("run", id)).unwrap();
                assert_eq!(restored.helpers_checked, checked.helpers_checked);
                assert_eq!(restored.children, checked.children);
            }
            f.close().await;
        }
    }

    #[tokio::test]
    async fn a_run_lists_its_running_helpers_and_recently_finished_ones() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let id = task.current_run.unwrap();
        f.dispatched(id).await;
        let helper = |n: usize, state| HarnessEvent::Child {
            native_ref: format!("helper-{n}"),
            label: format!("Helper {n}"),
            state,
            lifetime: None,
        };
        for n in 0..150 {
            let state = if n % 10 == 0 {
                ChildState::Active
            } else {
                ChildState::Completed
            };
            f.clock.advance(Duration::from_millis(1));
            f.hub.harness_event(id, helper(n, state)).await.unwrap();
        }
        // A running helper that ends is the most recent to finish.
        f.clock.advance(Duration::from_millis(1));
        f.hub
            .harness_event(id, helper(0, ChildState::Completed))
            .await
            .unwrap();
        let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
        let listed = |state| {
            run.children
                .iter()
                .filter(|c| c.state == state)
                .map(|c| c.native_ref.clone().unwrap())
                .collect::<Vec<_>>()
        };
        fn names(helpers: impl Iterator<Item = usize>) -> Vec<String> {
            helpers.map(|n| format!("helper-{n}")).collect()
        }
        assert_eq!(listed(ChildState::Active), names((10..150).step_by(10)));
        let finished = std::iter::once(0).chain((0..150).filter(|n| n % 10 != 0).skip(36));
        assert_eq!(listed(ChildState::Completed), names(finished));
        assert_eq!(run.earlier_children, Some(36));
        let recorded = f
            .hub
            .read(|r| r.journal(0))
            .unwrap()
            .events
            .into_iter()
            .rev()
            .find_map(|e| match e.event {
                Event::RunUpserted { run } => Some(run),
                _ => None,
            })
            .unwrap();
        assert_eq!(*recorded, run);
        // An unlisted helper reported again is not a new one; one that runs
        // again is listed again.
        let read = || f.hub.read(|r| r.require::<Run>("run", id)).unwrap();
        f.hub
            .harness_event(id, helper(1, ChildState::Stopped))
            .await
            .unwrap();
        assert_eq!(read().children, run.children);
        assert_eq!(read().earlier_children, Some(36));
        f.hub
            .harness_event(id, helper(2, ChildState::Active))
            .await
            .unwrap();
        let again = read();
        assert_eq!(again.earlier_children, Some(35));
        assert_eq!(
            again.children.last().unwrap().native_ref.as_deref(),
            Some("helper-2")
        );
    }

    #[tokio::test]
    async fn stop_reports_a_helper_that_ended_and_left_the_list() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let id = task.current_run.unwrap();
        f.dispatched(id).await;
        let helper = |n: usize, state| HarnessEvent::Child {
            native_ref: format!("helper-{n}"),
            label: format!("Helper {n}"),
            state,
            lifetime: None,
        };
        f.hub
            .harness_event(id, helper(0, ChildState::Active))
            .await
            .unwrap();
        let before = f
            .hub
            .read(|r| r.require::<Run>("run", id))
            .unwrap()
            .children;
        // It ends, then enough others end after it to leave the list.
        for (n, state) in std::iter::once((0, ChildState::Completed))
            .chain((1..=100).map(|n| (n, ChildState::Completed)))
        {
            f.clock.advance(Duration::from_millis(1));
            f.hub.harness_event(id, helper(n, state)).await.unwrap();
        }
        let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
        assert!(
            !run.children
                .iter()
                .any(|c| c.child_id == before[0].child_id)
        );
        let facts = f.hub.stop_run_children(id, before, false).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].label, "Child: Helper 0");
        assert_eq!(facts[0].status, FactStatus::Done);
    }

    #[tokio::test]
    async fn a_run_applies_a_projects_own_settings_only_once_the_owner_chose_them() {
        let f = Fixture::new().await;
        let first = f.task(Some(TaskMode::Work)).await.current_run.unwrap();
        assert!(!f.dispatched(first).await.project_settings);
        f.close().await;
        let f = Fixture::new().await;
        let project = f.hub.set_project_settings(f.project, true).unwrap();
        assert_eq!(project.project_id, f.project);
        let chosen = f.task(Some(TaskMode::Work)).await.current_run.unwrap();
        assert!(f.dispatched(chosen).await.project_settings);
        assert_eq!(
            f.hub
                .set_project_settings(ProjectId::new(), true)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        f.close().await;
    }

    #[tokio::test]
    async fn process_exit_after_parent_completion_settles_only_in_process_helpers() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let id = task.current_run.unwrap();
        f.dispatched(id).await;
        f.hub
            .harness_event(
                id,
                HarnessEvent::Child {
                    native_ref: "helper".into(),
                    label: "Helper".into(),
                    state: ChildState::Active,
                    lifetime: Some(ChildLifetime::InProcess),
                },
            )
            .await
            .unwrap();
        f.hub
            .harness_event(
                id,
                HarnessEvent::ParentEnded {
                    exit: RunExit::Completed,
                },
            )
            .await
            .unwrap();
        let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
        assert_eq!(run.children[0].state, ChildState::Active);
        f.hub
            .harness_event(
                id,
                HarnessEvent::Exited {
                    exit: RunExit::Completed,
                    outcome: None,
                },
            )
            .await
            .unwrap();
        let run: Run = f.hub.read(|r| r.require("run", id)).unwrap();
        assert_eq!(run.children[0].state, ChildState::Stopped);
        assert_eq!(run.children[0].provenance, Provenance::Observed);
        assert_eq!(
            f.hub
                .read(|r| r.require::<Task>("task", task.task_id))
                .unwrap()
                .attention_count,
            0
        );
        f.close().await;
    }

    #[tokio::test]
    async fn recorded_failure_resolves_outcome_unknown_attention() {
        let mut f = Fixture::new().await;
        let task = f.task(None).await;
        let id = task.current_run.unwrap();
        f.dispatched(id).await;
        f.hub
            .finish_run(id, RunState::ReconciliationRequired, None)
            .unwrap();
        f.hub
            .write(|tx| {
                let mut run: Run = tx.read().require("run", id)?;
                run.children.push(ChildAgent {
                    lifetime: None,
                    child_id: ChildId::new(),
                    native_ref: None,
                    label: "Fixture child".into(),
                    state: ChildState::Unknown,
                    started_at: run.admitted_at,
                    ended_at: None,
                    provenance: Provenance::Native,
                    stop: None,
                });
                tx.put("run", id, &run)?;
                let attention = AttentionItem {
                    child_id: None,
                    attention_id: AttentionId::new(),
                    kind: AttentionKind::ReconciliationRequired,
                    task_id: Some(task.task_id),
                    run_id: None,
                    approval_id: None,
                    title: "Helper outcome unknown · Fixture child".into(),
                    body: None,
                    priority: 1,
                    topic: None,
                    created_at: run.admitted_at,
                    resolved_at: None,
                    quiet: false,
                    report_id: None,
                };
                tx.put("attention", attention.attention_id, &attention)?;
                recount_attention(tx, task.task_id)
            })
            .unwrap();
        let current: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(current.attention_count, 2);
        let mut command = CommandEnvelope::new(Command::ReconcileTask {
            task_id: task.task_id,
            resolution: Reconciliation::MarkFailed,
        });
        command.expected_revision = Some(current.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), command).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        assert!(
            !f.hub
                .read(|r| r.list::<AttentionItem>("attention"))
                .unwrap()
                .iter()
                .any(|a| a.task_id == Some(task.task_id)
                    && a.kind == AttentionKind::ReconciliationRequired
                    && a.resolved_at.is_none())
        );
        let recorded: Run = f.hub.read(|r| r.require("run", id)).unwrap();
        assert_eq!(recorded.state, RunState::Failed);
        assert_eq!(
            recorded.children[0].state,
            ChildState::Unknown,
            "A human decision does not fabricate a verified helper end"
        );
        assert!(!recorded.holds_writer);
        assert_eq!(
            recorded.human_resolution.as_ref().unwrap().resolution,
            Reconciliation::MarkFailed
        );
        let reconciled: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(reconciled.attention_count, 0);
        // A projection written by an older version is normalized on startup.
        f.hub
            .write(|tx| {
                tx.put(
                    "task",
                    task.task_id,
                    &Task {
                        attention_count: 99,
                        ..reconciled
                    },
                )
            })
            .unwrap();
        f.hub.shutdown().await.unwrap();
        drop(f.hub);
        f.hub = CompanionHub::builder()
            .data_dir(&f.path)
            .clock(f.clock.clone())
            .harness(f.harness.clone())
            .build()
            .await
            .unwrap();
        let restored: Run = f.hub.read(|r| r.require("run", id)).unwrap();
        assert_eq!(restored.human_resolution, recorded.human_resolution);
        assert_eq!(
            f.hub
                .read(|r| r.require::<Task>("task", task.task_id))
                .unwrap()
                .attention_count,
            0
        );
        f.close().await;
    }

    #[tokio::test]
    async fn unmatched_delivery_never_writes_a_timeline_note() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let before: Vec<TimelineItem> = f
            .hub
            .read(|r| r.list(&format!("timeline:{}", task.task_id)))
            .unwrap();
        f.hub
            .harness_event(
                run,
                HarnessEvent::MessageDelivery {
                    message_ref: MessageRef(CommandId::new()),
                    state: DeliveryState::NativeDelivered,
                },
            )
            .await
            .unwrap();
        let after: Vec<TimelineItem> = f
            .hub
            .read(|r| r.list(&format!("timeline:{}", task.task_id)))
            .unwrap();
        assert_eq!(
            after, before,
            "An unmatched native delivery must not add conversation rows"
        );
        f.close().await;
    }
    fn timeline(f: &Fixture, task: TaskId) -> Vec<TimelineItem> {
        f.hub.read(|r| r.list(&format!("timeline:{task}"))).unwrap()
    }

    fn deliveries(f: &Fixture, task: TaskId) -> Vec<(u64, DeliveryState)> {
        // These cases assert follow-up delivery; run-start prompts are tested below.
        let first = timeline(f, task)
            .into_iter()
            .find(|i| matches!(i.item, TimelineBody::UserMessage { .. }))
            .unwrap()
            .seq;
        timeline(f, task)
            .into_iter()
            .filter_map(|item| match item.item {
                TimelineBody::Delivery {
                    message_seq, state, ..
                } if message_seq != first => Some((message_seq, state)),
                TimelineBody::Note { .. } => panic!("Delivery must never write a note"),
                _ => None,
            })
            .collect()
    }

    fn admit_test_followup(hub: &CompanionHub, id: CommandId, task: TaskId, text: &str) {
        hub.write(|tx| {
            let task: Task = tx.read().require("task", task)?;
            hub.admit_followup(tx, id, &task, text, None)
        })
        .unwrap();
    }

    async fn followup(f: &Fixture, task: TaskId, state: DeliveryState) -> (u64, MessageRef) {
        followup_as(f, task, state, MessageDelivery::NextTurn).await
    }

    async fn followup_as(
        f: &Fixture,
        task: TaskId,
        state: DeliveryState,
        delivery: MessageDelivery,
    ) -> (u64, MessageRef) {
        let command_id = CommandId::new();
        *f.harness.controls.delivery.lock().unwrap() = state;
        let run = f
            .hub
            .read(|r| r.require::<Task>("task", task))
            .unwrap()
            .current_run
            .unwrap();
        let result = f
            .hub
            .send_message(
                &Caller::local_owner(),
                command_id,
                run,
                "follow-up",
                None,
                delivery,
            )
            .await;
        if state == DeliveryState::Unknown {
            assert_eq!(result.unwrap_err().code, ErrorCode::OutcomeUnknown);
        } else {
            result.unwrap();
        }
        assert_eq!(
            f.harness.controls.message_refs.lock().unwrap().last(),
            Some(&MessageRef(command_id))
        );
        (
            f.messages(task).await.last().unwrap().seq,
            MessageRef(command_id),
        )
    }

    async fn delivery(f: &Fixture, run: RunId, message_ref: MessageRef, state: DeliveryState) {
        f.hub
            .harness_event(run, HarnessEvent::MessageDelivery { message_ref, state })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn queued_message_advances_without_notes_or_duplicate_delivery() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let (seq, message_ref) = followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        delivery(&f, run, message_ref, DeliveryState::QueuedForBoundary).await;
        delivery(&f, run, message_ref, DeliveryState::HostAccepted).await;
        delivery(&f, run, message_ref, DeliveryState::HostAccepted).await;
        delivery(&f, run, message_ref, DeliveryState::NativeDelivered).await;
        delivery(&f, run, message_ref, DeliveryState::NativeDelivered).await;
        assert_eq!(
            deliveries(&f, task.task_id),
            vec![
                (seq, DeliveryState::QueuedForBoundary),
                (seq, DeliveryState::HostAccepted),
                (seq, DeliveryState::NativeDelivered)
            ]
        );
        let pending: Vec<InFlightMessage> =
            f.hub.read(|r| r.require("message_delivery", run)).unwrap();
        assert!(pending.is_empty());
        f.close().await;
    }

    #[tokio::test]
    async fn two_queued_messages_resolve_fifo_and_skip_states_already_reached() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let (a, a_ref) = followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        let (b, b_ref) = followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        delivery(&f, run, a_ref, DeliveryState::HostAccepted).await;
        delivery(&f, run, b_ref, DeliveryState::HostAccepted).await;
        delivery(&f, run, a_ref, DeliveryState::NativeDelivered).await;
        delivery(&f, run, b_ref, DeliveryState::NativeDelivered).await;
        assert_eq!(
            deliveries(&f, task.task_id),
            vec![
                (a, DeliveryState::QueuedForBoundary),
                (b, DeliveryState::QueuedForBoundary),
                (a, DeliveryState::HostAccepted),
                (b, DeliveryState::HostAccepted),
                (a, DeliveryState::NativeDelivered),
                (b, DeliveryState::NativeDelivered)
            ]
        );
        f.close().await;
    }

    #[tokio::test]
    async fn delivery_failure_and_unknown_update_only_the_referenced_message() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let (a, a_ref) = followup(&f, task.task_id, DeliveryState::HostAccepted).await;
        let (b, b_ref) = followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        delivery(&f, run, a_ref, DeliveryState::Failed).await;
        delivery(&f, run, b_ref, DeliveryState::Unknown).await;
        delivery(&f, run, a_ref, DeliveryState::NativeDelivered).await;
        assert_eq!(
            deliveries(&f, task.task_id),
            vec![
                (b, DeliveryState::QueuedForBoundary),
                (a, DeliveryState::Failed),
                (b, DeliveryState::Unknown)
            ]
        );
        f.close().await;
    }

    #[tokio::test]
    async fn a_message_that_overtakes_a_queued_one_confirms_only_itself() {
        // Claude Code writes "Steer now" at once while "After this turn" waits
        // in the adapter's queue, so the next native echo is the steer's.
        // "Stop the turn and send" jumps that queue the same way on both agents.
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let (queued, _queued_ref) =
            followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        let (steer, steer_ref) = followup_as(
            &f,
            task.task_id,
            DeliveryState::HostAccepted,
            MessageDelivery::Steer,
        )
        .await;
        delivery(&f, run, steer_ref, DeliveryState::NativeDelivered).await;
        assert_eq!(
            deliveries(&f, task.task_id),
            vec![
                (queued, DeliveryState::QueuedForBoundary),
                (steer, DeliveryState::NativeDelivered)
            ],
            "The steer's echo must not mark the still-queued message as delivered"
        );
        f.hub.stop_task(task.task_id, None, false).await.unwrap();
        f.hub
            .harness_event(
                run,
                HarnessEvent::Exited {
                    exit: RunExit::Interrupted,
                    outcome: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            deliveries(&f, task.task_id),
            vec![
                (queued, DeliveryState::QueuedForBoundary),
                (steer, DeliveryState::NativeDelivered),
                (queued, DeliveryState::Failed)
            ],
            "The queued message never reached the agent"
        );
        f.close().await;
    }

    #[tokio::test]
    async fn already_confirmed_steer_does_not_produce_a_duplicate_delivery_row() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let (seq, message_ref) = followup(&f, task.task_id, DeliveryState::NativeDelivered).await;
        delivery(&f, run, message_ref, DeliveryState::NativeDelivered).await;
        assert_eq!(
            deliveries(&f, task.task_id),
            vec![(seq, DeliveryState::NativeDelivered)]
        );
        f.close().await;
    }

    #[tokio::test]
    async fn interrupt_and_send_jumps_queue_without_confirming_the_older_message() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let (queued, queued_ref) =
            followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        let (urgent, urgent_ref) = followup_as(
            &f,
            task.task_id,
            DeliveryState::QueuedForBoundary,
            MessageDelivery::InterruptAndSend,
        )
        .await;
        delivery(&f, run, urgent_ref, DeliveryState::HostAccepted).await;
        delivery(&f, run, urgent_ref, DeliveryState::NativeDelivered).await;
        // A duplicate must not spill over onto the older queued follow-up.
        delivery(&f, run, urgent_ref, DeliveryState::NativeDelivered).await;
        assert_eq!(
            deliveries(&f, task.task_id),
            vec![
                (queued, DeliveryState::QueuedForBoundary),
                (urgent, DeliveryState::QueuedForBoundary),
                (urgent, DeliveryState::HostAccepted),
                (urgent, DeliveryState::NativeDelivered),
            ]
        );
        delivery(&f, run, queued_ref, DeliveryState::HostAccepted).await;
        delivery(&f, run, queued_ref, DeliveryState::NativeDelivered).await;
        assert_eq!(
            deliveries(&f, task.task_id),
            vec![
                (queued, DeliveryState::QueuedForBoundary),
                (urgent, DeliveryState::QueuedForBoundary),
                (urgent, DeliveryState::HostAccepted),
                (urgent, DeliveryState::NativeDelivered),
                (queued, DeliveryState::HostAccepted),
                (queued, DeliveryState::NativeDelivered),
            ]
        );
        f.close().await;
    }

    #[tokio::test]
    async fn timed_out_send_retains_its_reference_for_late_native_confirmation() {
        for host_accepted_first in [false, true] {
            let f = Fixture::new().await;
            let task = f.task(None).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            let (queued, _) = followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
            f.harness
                .controls
                .hold_message
                .store(true, Ordering::SeqCst);
            let command_id = CommandId::new();
            let message_ref = MessageRef(command_id);
            admit_test_followup(&f.hub, command_id, task.task_id, "delayed");
            // Exercise the production five-second timeout without a wall-clock wait.
            tokio::time::pause();
            let result = f
                .hub
                .send_message(
                    &Caller::local_owner(),
                    command_id,
                    run,
                    "delayed",
                    None,
                    MessageDelivery::Steer,
                )
                .await;
            tokio::time::resume();
            assert_eq!(result.unwrap_err().code, ErrorCode::OutcomeUnknown);
            let message = f.messages(task.task_id).await.pop().unwrap();
            assert!(matches!(
                message.item,
                TimelineBody::UserMessage {
                    delivery: DeliveryState::HostAccepted,
                    ..
                }
            ));
            let pending: Vec<InFlightMessage> =
                f.hub.read(|r| r.require("message_delivery", run)).unwrap();
            assert!(pending.iter().any(|m| m.seq == message.seq
                && m.message_ref == Some(message_ref)
                && m.state == DeliveryState::Unknown));
            let mut expected = vec![
                (queued, DeliveryState::QueuedForBoundary),
                (message.seq, DeliveryState::Unknown),
            ];
            if host_accepted_first {
                delivery(&f, run, message_ref, DeliveryState::HostAccepted).await;
                expected.push((message.seq, DeliveryState::HostAccepted));
            }
            delivery(&f, run, message_ref, DeliveryState::NativeDelivered).await;
            delivery(&f, run, message_ref, DeliveryState::NativeDelivered).await;
            expected.push((message.seq, DeliveryState::NativeDelivered));
            assert_eq!(deliveries(&f, task.task_id), expected);
            f.hub.finish_run(run, RunState::Interrupted, None).unwrap();
            expected.push((queued, DeliveryState::Failed));
            assert_eq!(deliveries(&f, task.task_id), expected);
            f.close().await;
        }
    }

    #[tokio::test]
    async fn unknown_delivery_only_fails_with_an_explicit_adapter_confirmation() {
        for confirm_failure in [false, true] {
            let f = Fixture::new().await;
            let task = f.task(None).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            let (seq, message_ref) = followup(&f, task.task_id, DeliveryState::Unknown).await;
            delivery(&f, run, message_ref, DeliveryState::Unknown).await;
            delivery(&f, run, message_ref, DeliveryState::QueuedForBoundary).await;
            assert_eq!(
                deliveries(&f, task.task_id),
                vec![(seq, DeliveryState::Unknown)]
            );
            if confirm_failure {
                delivery(&f, run, message_ref, DeliveryState::Failed).await;
            }
            f.hub.finish_run(run, RunState::Interrupted, None).unwrap();
            delivery(&f, run, message_ref, DeliveryState::NativeDelivered).await;
            assert_eq!(
                deliveries(&f, task.task_id),
                if confirm_failure {
                    vec![(seq, DeliveryState::Unknown), (seq, DeliveryState::Failed)]
                } else {
                    vec![(seq, DeliveryState::Unknown)]
                }
            );
            let pending: Option<Vec<InFlightMessage>> =
                f.hub.read(|r| r.get("message_delivery", run)).unwrap();
            assert!(pending.is_none());
            f.close().await;
        }
    }

    #[tokio::test]
    async fn unknown_reference_cannot_advance_any_pending_message() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        followup(&f, task.task_id, DeliveryState::Unknown).await;
        let before = timeline(&f, task.task_id);
        let pending: serde_json::Value =
            f.hub.read(|r| r.require("message_delivery", run)).unwrap();
        let unknown = MessageRef(CommandId::new());
        for state in [
            DeliveryState::HostAccepted,
            DeliveryState::NativeDelivered,
            DeliveryState::Failed,
            DeliveryState::Unknown,
        ] {
            delivery(&f, run, unknown, state).await;
        }
        assert_eq!(timeline(&f, task.task_id), before);
        assert_eq!(
            f.hub
                .read(|r| r.require::<serde_json::Value>("message_delivery", run))
                .unwrap(),
            pending
        );
        f.close().await;
    }

    #[tokio::test]
    async fn stop_withdraws_every_held_message() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let (accepted, accepted_ref) =
            followup(&f, task.task_id, DeliveryState::HostAccepted).await;
        let (reported, reported_ref) =
            followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        let (unreported, _) = followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        let stop = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::StopTask {
                    task_id: task.task_id,
                    force: false,
                }),
            )
            .await;
        assert_eq!(stop.status, ReceiptStatus::Accepted, "{stop:?}");
        // The adapter reports one withdrawal; the run's end settles the other.
        delivery(&f, run, reported_ref, DeliveryState::Cancelled).await;
        f.hub
            .harness_event(
                run,
                HarnessEvent::Exited {
                    exit: RunExit::Interrupted,
                    outcome: None,
                },
            )
            .await
            .unwrap();
        delivery(&f, run, accepted_ref, DeliveryState::NativeDelivered).await;
        let settled: Vec<_> = timeline(&f, task.task_id)
            .into_iter()
            .filter_map(|i| match i.item {
                TimelineBody::Delivery {
                    message_seq,
                    state,
                    withdrawn_by_stop,
                    ..
                } if [reported, unreported].contains(&message_seq) => {
                    Some((message_seq, state, withdrawn_by_stop))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            settled,
            vec![
                (reported, DeliveryState::QueuedForBoundary, false),
                (unreported, DeliveryState::QueuedForBoundary, false),
                (reported, DeliveryState::Cancelled, true),
                (unreported, DeliveryState::Cancelled, true)
            ]
        );
        assert!(matches!(
            timeline(&f, task.task_id)
                .iter()
                .find(|i| i.seq == accepted)
                .unwrap()
                .item,
            TimelineBody::UserMessage {
                delivery: DeliveryState::HostAccepted,
                ..
            }
        ));
        f.close().await;
    }

    #[tokio::test]
    async fn a_stop_raises_no_attention_but_a_failure_does() {
        let f = Fixture::new().await;
        let raised = |f: &Fixture, run: RunId| {
            f.hub
                .read(|r| r.list::<AttentionItem>("attention"))
                .unwrap()
                .into_iter()
                .filter(|a| a.run_id == Some(run))
                .map(|a| (a.kind, a.title))
                .collect::<Vec<_>>()
        };
        let stopped = f.task(None).await.current_run.unwrap();
        f.dispatched(stopped).await;
        f.hub
            .finish_run(stopped, RunState::Interrupted, None)
            .unwrap();
        assert_eq!(raised(&f, stopped), vec![]);
        let task = f.task(None).await;
        let failed = task.current_run.unwrap();
        f.dispatched(failed).await;
        f.hub.finish_run(failed, RunState::Failed, None).unwrap();
        assert_eq!(
            raised(&f, failed),
            vec![(AttentionKind::Failure, task.title)]
        );
        f.close().await;
    }

    #[tokio::test]
    async fn stop_before_dispatch_ends_the_run_and_nothing_can_start_it() {
        let f = Fixture::new().await;
        let mut create = f.create(Some(TaskMode::Work));
        if let Command::CreateTask { placement, .. } = &mut create.body {
            *placement = TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            };
        }
        let receipt = f.hub.submit(&Caller::local_owner(), create.clone()).await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            panic!("{receipt:?}")
        };
        // Admit a run without running its dispatcher: the window between a
        // start's admission and its dispatch.
        let run = f
            .hub
            .write(|tx| {
                let mut task: Task = tx.read().require("task", task_id)?;
                let run = f
                    .hub
                    .new_run(tx, &mut task, create.command_id, None, None, false)?;
                tx.put("task", task_id, &task)?;
                Ok(run)
            })
            .unwrap();
        assert_writer(&f, Some(run));
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        let mut stop = CommandEnvelope::new(Command::StopTask {
            task_id,
            force: false,
        });
        stop.expected_revision = Some(task.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), stop.clone()).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let command = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let command = f.hub.read(|r| r.command(stop.command_id)).unwrap().unwrap();
                if !command.pending {
                    return command;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("Stop did not settle");
        assert_eq!(command.stage, CommandStage::Observed, "{command:?}");
        // A dispatch arriving after the Stop has nothing left to start.
        f.hub.dispatch_task(task_id, run).await.unwrap();
        assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
        let ended: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_eq!(ended.state, RunState::Interrupted);
        assert!(ended.ended_at.is_some());
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        assert_eq!(task.state, TaskState::Interrupted);
        assert_writer(&f, None);
        let receipt = timeline(&f, task_id)
            .into_iter()
            .find_map(|item| match item.item {
                TimelineBody::Receipt { receipt } => Some(receipt),
                _ => None,
            })
            .unwrap();
        assert_eq!(receipt.facts[0].label, "Agent confirmed Stop");
        assert_eq!(receipt.facts[0].status, FactStatus::NotApplicable);
        assert_eq!(
            receipt.summary,
            "Stopped before the agent started. Nothing reached the agent."
        );
        f.close().await;
    }

    #[tokio::test]
    async fn stop_says_what_this_computer_could_not_end_with_the_run() {
        let owner = Caller::local_owner();
        for (containment, status, words) in [
            (
                Containment::ProcessGroup {
                    reason: "this computer doesn’t use cgroup v2".into(),
                },
                FactStatus::Unknown,
                "may still be running, because this computer doesn’t use cgroup v2",
            ),
            (
                Containment::Complete,
                FactStatus::Done,
                "including work it had moved into the background",
            ),
        ] {
            let f = Fixture::new().await;
            *f.harness.controls.containment.lock().unwrap() = Some(containment);
            let task = f.task(None).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            tokio::time::timeout(Duration::from_secs(15), async {
                while !f.hub.inner.runs.lock().await.contains_key(&run) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            let interrupts = f.harness.controls.interrupts.load(Ordering::SeqCst);
            let mut stop = CommandEnvelope::new(Command::StopTask {
                task_id: task.task_id,
                force: false,
            });
            stop.expected_revision = Some(task.revision);
            let receipt = f.hub.submit(&owner, stop).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            // As a real adapter does, report the exit once the run's
            // processes have ended.
            while f.harness.controls.interrupts.load(Ordering::SeqCst) == interrupts {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            f.harness
                .emit(
                    run,
                    HarnessEvent::Exited {
                        exit: RunExit::Interrupted,
                        outcome: None,
                    },
                )
                .await;
            let fact = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if let Some(fact) =
                        timeline(&f, task.task_id)
                            .into_iter()
                            .find_map(|item| match item.item {
                                TimelineBody::Receipt { receipt } => receipt
                                    .facts
                                    .into_iter()
                                    .find(|fact| fact.label == "Everything the agent started"),
                                _ => None,
                            })
                    {
                        return fact;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("no Stop receipt");
            assert_eq!(fact.status, status);
            assert!(fact.detail.unwrap().contains(words));
            f.close().await;
        }
    }

    #[tokio::test]
    async fn a_stop_whose_leftovers_are_unconfirmed_keeps_the_project_until_they_end() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        *f.harness.controls.containment.lock().unwrap() = Some(Containment::Complete);
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            while !f.hub.inner.runs.lock().await.contains_key(&run) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let interrupts = f.harness.controls.interrupts.load(Ordering::SeqCst);
        let mut stop = CommandEnvelope::new(Command::StopTask {
            task_id: task.task_id,
            force: false,
        });
        stop.expected_revision = Some(task.revision);
        let receipt = f.hub.submit(&owner, stop.clone()).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        while f.harness.controls.interrupts.load(Ordering::SeqCst) == interrupts {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        // As an adapter does when the agent ended but what it started could
        // not be confirmed ended: it keeps trying, and the run stays open.
        for event in [
            HarnessEvent::Warning {
                text: "Some of what the agent started may still be running on the computer.".into(),
            },
            HarnessEvent::CleanupPending,
        ] {
            f.harness.emit(run, event).await;
        }
        let fact = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(fact) =
                    timeline(&f, task.task_id)
                        .into_iter()
                        .find_map(|item| match item.item {
                            TimelineBody::Receipt { receipt } => receipt
                                .facts
                                .into_iter()
                                .find(|fact| fact.label == "Everything the agent started"),
                            _ => None,
                        })
                {
                    return fact;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("no Stop receipt");
        assert_eq!(fact.status, FactStatus::Unknown);
        assert!(fact.detail.unwrap().contains("hadn’t all ended"));
        // The agent confirmed Stop, but that alone doesn't say the work
        // stopped: the command's outcome stays unknown.
        let command = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let command = f.hub.read(|r| r.command(stop.command_id)).unwrap().unwrap();
                if !command.pending {
                    return command;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the Stop was never settled");
        assert_eq!(
            command.error.as_ref().map(|e| e.code),
            Some(ErrorCode::OutcomeUnknown),
            "{command:?}"
        );
        let open: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_eq!(open.state, RunState::Stopping);
        assert!(open.holds_writer);
        assert_writer(&f, Some(run));
        // Once they are confirmed ended, the project is free.
        f.harness
            .emit(
                run,
                HarnessEvent::Exited {
                    exit: RunExit::Interrupted,
                    outcome: None,
                },
            )
            .await;
        tokio::time::timeout(Duration::from_secs(15), async {
            while f
                .hub
                .read(|r| r.require::<Run>("run", run))
                .unwrap()
                .holds_writer
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the writer was kept after the run's processes ended");
        assert_writer(&f, None);
        f.close().await;
    }

    /// A run's processes, once the harness reports them unconfirmed ended.
    fn pending(f: &Fixture, run: RunId) -> bool {
        f.hub
            .read(|r| r.require::<Run>("run", run))
            .unwrap()
            .cleanup_pending
    }

    async fn stop(f: &Fixture, task: TaskId, force: bool) -> CommandReceipt {
        let current: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        let mut stop = CommandEnvelope::new(Command::StopTask {
            task_id: task,
            force,
        });
        stop.expected_revision = Some(current.revision);
        f.hub.submit(&Caller::local_owner(), stop).await
    }

    async fn receipts(f: &Fixture, task: TaskId, count: usize) {
        tokio::time::timeout(Duration::from_secs(20), async {
            while timeline(f, task)
                .iter()
                .filter(|item| matches!(item.item, TimelineBody::Receipt { .. }))
                .count()
                < count
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("no Stop receipt");
    }

    #[tokio::test]
    async fn a_force_stop_that_cannot_confirm_the_end_keeps_the_project_until_it_can() {
        for reported in [false, true] {
            let f = Fixture::new().await;
            let task = f.task(None).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            tokio::time::timeout(Duration::from_secs(15), async {
                while !f.hub.inner.runs.lock().await.contains_key(&run) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            if reported {
                f.hub
                    .harness_event(run, HarnessEvent::CleanupPending)
                    .await
                    .unwrap();
                let open: Run = f.hub.read(|r| r.require("run", run)).unwrap();
                assert!(!open.state.is_terminal(), "{open:?}");
                assert_writer(&f, Some(run));
            }
            // The synthetic adapter can't confirm it ended the agent.
            let receipt = stop(&f, task.task_id, true).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            receipts(&f, task.task_id, 1).await;
            let open: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            assert_eq!(open.state, RunState::Stopping, "reported={reported}");
            assert!(open.holds_writer);
            assert!(pending(&f, run));
            assert_writer(&f, Some(run));
            // It can still be stopped, and the project is free once the
            // adapter confirms everything ended.
            let receipt = stop(&f, task.task_id, true).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            receipts(&f, task.task_id, 2).await;
            f.harness
                .emit(
                    run,
                    HarnessEvent::Exited {
                        exit: RunExit::Interrupted,
                        outcome: None,
                    },
                )
                .await;
            tokio::time::timeout(Duration::from_secs(15), async {
                while f
                    .hub
                    .read(|r| r.require::<Run>("run", run))
                    .unwrap()
                    .holds_writer
                {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("the writer was kept after the run's processes ended");
            assert_writer(&f, None);
            assert!(!pending(&f, run));
            f.close().await;
        }
    }

    #[tokio::test]
    async fn what_an_ask_or_plan_turn_left_unconfirmed_stays_stoppable() {
        // Helpers still running are unconfirmed on their own, before the
        // adapter reports anything else it can't confirm ended.
        for (mode, reported) in [
            (TaskMode::Ask, false),
            (TaskMode::Ask, true),
            (TaskMode::Plan, false),
            (TaskMode::Plan, true),
        ] {
            let f = Fixture::new().await;
            let task = f.task(Some(mode)).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            tokio::time::timeout(Duration::from_secs(15), async {
                while !f.hub.inner.runs.lock().await.contains_key(&run) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            f.hub
                .harness_event(
                    run,
                    HarnessEvent::ParentEnded {
                        exit: RunExit::Completed,
                    },
                )
                .await
                .unwrap();
            if reported {
                f.hub
                    .harness_event(run, HarnessEvent::CleanupPending)
                    .await
                    .unwrap();
            }
            let ended: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            assert!(ended.state.is_terminal(), "{ended:?}");
            assert!(!ended.holds_writer);
            assert!(ended.cleanup_pending, "{mode:?} reported={reported}");
            let current: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
            let mut archive = CommandEnvelope::new(Command::ArchiveTask {
                task_id: task.task_id,
            });
            archive.expected_revision = Some(current.revision);
            let refused = f.hub.submit(&Caller::local_owner(), archive).await;
            assert_eq!(refused.status, ReceiptStatus::Rejected, "{mode:?}");
            for (count, force) in [(1, false), (2, true)] {
                let receipt = stop(&f, task.task_id, force).await;
                assert_eq!(
                    receipt.status,
                    ReceiptStatus::Accepted,
                    "{mode:?} {receipt:?}"
                );
                receipts(&f, task.task_id, count).await;
            }
            let said = timeline(&f, task.task_id)
                .into_iter()
                .find_map(|item| match item.item {
                    TimelineBody::Receipt { receipt } => receipt
                        .facts
                        .into_iter()
                        .find(|fact| fact.label == "Agent confirmed Stop"),
                    _ => None,
                })
                .unwrap();
            assert_eq!(said.status, FactStatus::NotApplicable, "{mode:?}");
            // Stopping what it left leaves the turn's answer as it was.
            let after: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            assert_eq!(after.state, RunState::Completed);
            assert!(pending(&f, run));
            f.close().await;
        }
    }

    #[tokio::test]
    async fn a_project_stays_while_what_an_ask_or_plan_turn_left_is_unconfirmed() {
        let waiting = "Something a task in this project started hasn’t ended yet. Stop it, or wait for it to end, then remove the project.";
        for mode in [TaskMode::Ask, TaskMode::Plan] {
            let f = Fixture::new().await;
            let owner = Caller::local_owner();
            let task = f.task(Some(mode)).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            tokio::time::timeout(Duration::from_secs(15), async {
                while !f.hub.inner.runs.lock().await.contains_key(&run) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            f.hub
                .harness_event(
                    run,
                    HarnessEvent::ParentEnded {
                        exit: RunExit::Completed,
                    },
                )
                .await
                .unwrap();
            let remove = || {
                CommandEnvelope::new(Command::RemoveProject {
                    project_id: f.project,
                })
            };
            let refused = f.hub.submit(&owner, remove()).await;
            assert_eq!(refused.error.unwrap().message, waiting, "{mode:?}");
            // Stop still reaches what the turn left, and the agent's
            // process then ends with it.
            let receipt = stop(&f, task.task_id, false).await;
            assert_eq!(
                receipt.status,
                ReceiptStatus::Accepted,
                "{mode:?} {receipt:?}"
            );
            f.hub
                .harness_event(
                    run,
                    HarnessEvent::Exited {
                        exit: RunExit::Completed,
                        outcome: None,
                    },
                )
                .await
                .unwrap();
            receipts(&f, task.task_id, 1).await;
            assert!(!pending(&f, run));
            let removed = f.hub.submit(&owner, remove()).await;
            assert_eq!(
                removed.status,
                ReceiptStatus::Accepted,
                "{mode:?} {removed:?}"
            );
            f.close().await;
        }
    }

    #[tokio::test]
    async fn a_stop_reaches_only_the_turn_it_was_accepted_for() {
        // Stop is accepted for what a finished turn left running. Before it
        // acts, that turn ends, with its event stream still open or already
        // closed, and the person starts the next turn.
        for (mode, closed) in [
            (TaskMode::Ask, false),
            (TaskMode::Ask, true),
            (TaskMode::Plan, false),
            (TaskMode::Plan, true),
        ] {
            let f = Fixture::new().await;
            let task = f.task(Some(mode)).await;
            let first = task.current_run.unwrap();
            f.dispatched(first).await;
            tokio::time::timeout(Duration::from_secs(15), async {
                while !f.hub.inner.runs.lock().await.contains_key(&first) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            f.hub
                .harness_event(
                    first,
                    HarnessEvent::ParentEnded {
                        exit: RunExit::Completed,
                    },
                )
                .await
                .unwrap();
            let stopped = stop(&f, task.task_id, false).await;
            assert_eq!(stopped.status, ReceiptStatus::Accepted, "{stopped:?}");
            // Nothing from here to the receipt gives Stop's effect a turn.
            f.hub
                .harness_event(
                    first,
                    HarnessEvent::Exited {
                        exit: RunExit::Completed,
                        outcome: None,
                    },
                )
                .await
                .unwrap();
            if closed {
                // As the run's event stream does once it has ended.
                f.hub.inner.runs.lock().await.remove(&first);
            }
            let resumed = f
                .hub
                .submit(
                    &Caller::local_owner(),
                    f.resume(task.task_id, Some("Go on".into()), None),
                )
                .await;
            assert_eq!(resumed.status, ReceiptStatus::Accepted, "{resumed:?}");
            let next = f
                .hub
                .read(|r| r.require::<Task>("task", task.task_id))
                .unwrap()
                .current_run
                .unwrap();
            assert_ne!(next, first);
            assert_eq!(f.harness.controls.interrupts.load(Ordering::SeqCst), 0);
            receipts(&f, task.task_id, 1).await;
            let turn: Run = f.hub.read(|r| r.require("run", next)).unwrap();
            assert!(
                !turn.state.is_terminal(),
                "{mode:?} closed={closed} {turn:?}"
            );
            f.dispatched(next).await;
            assert!(
                !f.hub.run_stopped(next).unwrap(),
                "{mode:?} closed={closed}"
            );
            let ended: Run = f.hub.read(|r| r.require("run", first)).unwrap();
            assert_eq!(ended.state, RunState::Completed);
            let (run_id, receipt) = timeline(&f, task.task_id)
                .into_iter()
                .find_map(|item| match item.item {
                    TimelineBody::Receipt { receipt } => Some((item.run_id, receipt)),
                    _ => None,
                })
                .unwrap();
            assert_eq!(run_id, Some(first));
            let said = receipt
                .facts
                .iter()
                .find(|fact| fact.label == "Agent confirmed Stop")
                .unwrap();
            if closed {
                // Nothing was left to stop, and it says so rather than
                // leaving Stop unconfirmed.
                assert_eq!(said.status, FactStatus::NotApplicable, "{mode:?}");
                assert_eq!(said.detail.as_deref(), Some("It had already ended."));
                assert_eq!(
                    receipt.summary,
                    "It had already ended before Stop reached it. Prior file changes remain."
                );
                let stage = tokio::time::timeout(Duration::from_secs(15), async {
                    loop {
                        let command = f
                            .hub
                            .read(|r| Ok(r.command(stopped.command_id)?.unwrap()))
                            .unwrap();
                        if !command.pending {
                            return command.stage;
                        }
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
                assert_eq!(stage, CommandStage::Observed, "{mode:?}");
            } else {
                assert_eq!(said.status, FactStatus::Done, "{mode:?}");
            }
            f.close().await;
        }
    }

    /// Starts `run` and waits until its agent is live and working.
    async fn working(f: &Fixture, task: TaskId, run: RunId) {
        f.dispatched(run).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            while !f.hub.inner.runs.lock().await.contains_key(&run) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("run did not start");
        f.hub
            .harness_event(
                run,
                HarnessEvent::SessionStarted {
                    native_session: run.to_string(),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await
            .unwrap();
        let task: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        assert_eq!(task.state, TaskState::Working);
    }

    /// Ends `run` as a finished turn does.
    async fn finish(f: &Fixture, run: RunId) {
        for event in [
            HarnessEvent::ParentEnded {
                exit: RunExit::Completed,
            },
            HarnessEvent::Exited {
                exit: RunExit::Completed,
                outcome: None,
            },
        ] {
            f.hub.harness_event(run, event).await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_late_start_reaches_only_the_run_it_admitted() {
        // Continue is accepted for turn A, and A is stopped before it starts.
        // The person continues again and turn B starts. Only then does A's
        // start arrive, as it does when its effect is delayed.
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let task = f.task(Some(TaskMode::Ask)).await;
        let first = task.current_run.unwrap();
        working(&f, task.task_id, first).await;
        finish(&f, first).await;
        let late = f.resume(task.task_id, Some("Go on".into()), None);
        let accepted = f.hub.submit(&owner, late.clone()).await;
        assert_eq!(accepted.status, ReceiptStatus::Accepted, "{accepted:?}");
        let a = f
            .hub
            .read(|r| r.require::<Task>("task", task.task_id))
            .unwrap()
            .current_run
            .unwrap();
        let stopped = stop(&f, task.task_id, false).await;
        assert_eq!(stopped.status, ReceiptStatus::Accepted, "{stopped:?}");
        tokio::time::timeout(Duration::from_secs(15), async {
            while !f
                .hub
                .read(|r| r.require::<Run>("run", a))
                .unwrap()
                .state
                .is_terminal()
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("A did not end");
        let again = f
            .hub
            .submit(&owner, f.resume(task.task_id, Some("Go on".into()), None))
            .await;
        assert_eq!(again.status, ReceiptStatus::Accepted, "{again:?}");
        let b = f
            .hub
            .read(|r| r.require::<Task>("task", task.task_id))
            .unwrap()
            .current_run
            .unwrap();
        assert_ne!(a, b);
        working(&f, task.task_id, b).await;
        let starts = f.harness.starts.load(Ordering::SeqCst);
        f.hub.execute(owner, late, None, accepted).await;
        assert_eq!(f.harness.starts.load(Ordering::SeqCst), starts);
        let turn: Run = f.hub.read(|r| r.require("run", b)).unwrap();
        assert_eq!(turn.state, RunState::Working, "{turn:?}");
        assert!(f.hub.inner.runs.lock().await.contains_key(&b));
        assert!(!f.hub.run_stopped(b).unwrap());
        let ended: Run = f.hub.read(|r| r.require("run", a)).unwrap();
        assert_eq!(ended.state, RunState::Interrupted);
        f.close().await;
    }

    #[tokio::test]
    async fn a_late_message_reaches_only_the_turn_it_was_accepted_for() {
        // A message that interrupts is accepted while turn A works. A ends,
        // the person continues and turn B starts. Only then does the
        // message's delivery arrive, as it does when its effect is delayed.
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let task = f.task(Some(TaskMode::Ask)).await;
        let a = task.current_run.unwrap();
        working(&f, task.task_id, a).await;
        let late = CommandEnvelope::new(Command::SendMessage {
            task_id: task.task_id,
            text: "For the turn that was working".into(),
            bundle: None,
            delivery: MessageDelivery::InterruptAndSend,
        });
        let accepted = f.hub.submit(&owner, late.clone()).await;
        assert_eq!(accepted.status, ReceiptStatus::Accepted, "{accepted:?}");
        // Nothing from here to the next submit gives its effect a turn.
        finish(&f, a).await;
        let again = f
            .hub
            .submit(&owner, f.resume(task.task_id, Some("Go on".into()), None))
            .await;
        assert_eq!(again.status, ReceiptStatus::Accepted, "{again:?}");
        let b = f
            .hub
            .read(|r| r.require::<Task>("task", task.task_id))
            .unwrap()
            .current_run
            .unwrap();
        assert_ne!(a, b);
        working(&f, task.task_id, b).await;
        let sent = f.harness.controls.messages.load(Ordering::SeqCst);
        f.hub.execute(owner, late.clone(), None, accepted).await;
        assert_eq!(f.harness.controls.messages.load(Ordering::SeqCst), sent);
        assert!(
            !f.harness
                .controls
                .message_refs
                .lock()
                .unwrap()
                .contains(&MessageRef(late.command_id))
        );
        assert!(
            f.messages(task.task_id).await.iter().all(|item| {
                item.run_id != Some(b)
                    || !matches!(
                        &item.item,
                        TimelineBody::UserMessage { text, .. }
                            if text == "For the turn that was working"
                    )
            }),
            "the message was recorded as sent to B"
        );
        let turn: Run = f.hub.read(|r| r.require("run", b)).unwrap();
        assert_eq!(turn.state, RunState::Working, "{turn:?}");
        assert!(!f.hub.run_stopped(b).unwrap());
        f.close().await;
    }

    #[tokio::test]
    async fn lost_events_leave_unconfirmed_leftovers_for_the_person_to_check() {
        // Without the report, events stopped between the agent's end and the
        // adapter saying it couldn't confirm what the helpers left ended.
        for reported in [false, true] {
            let f = Fixture::new().await;
            let (task, run) = helper_outlives_turn(&f).await;
            if reported {
                f.hub
                    .harness_event(run, HarnessEvent::CleanupPending)
                    .await
                    .unwrap();
            }
            // Shutting down closes the agent's control: nothing can confirm
            // the leftovers ended now.
            f.hub.shutdown().await.unwrap();
            let open: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            assert_eq!(open.state, RunState::ReconciliationRequired, "{reported}");
            assert!(open.holds_writer, "{reported}");
            assert_writer(&f, Some(run));
            let decide: Task = f.hub.read(|r| r.require("task", task)).unwrap();
            assert_eq!(decide.state, TaskState::ReconciliationRequired);
            assert!(timeline(&f, task).iter().any(|item| matches!(
                &item.item,
                TimelineBody::Note { text } if text.contains("can no longer confirm")
            )));
            f.close().await;
        }
    }

    #[tokio::test]
    async fn a_restart_leaves_unconfirmed_leftovers_for_the_person_to_check() {
        // Saved before helpers still running were recorded unconfirmed, the
        // helpers alone keep the project.
        for saved_unrecorded in [false, true] {
            let mut f = Fixture::new().await;
            let (task, run) = helper_outlives_turn(&f).await;
            if !saved_unrecorded {
                f.hub
                    .harness_event(run, HarnessEvent::CleanupPending)
                    .await
                    .unwrap();
            }
            f.hub.shutdown().await.unwrap();
            // As a crash would have left it, before anything noticed.
            f.hub
                .write(|tx| {
                    let mut kept: Run = tx.read().require("run", run)?;
                    kept.state = RunState::Completed;
                    if saved_unrecorded {
                        kept.cleanup_pending = false;
                    }
                    tx.put("run", run, &kept)?;
                    let mut left: Task = tx.read().require("task", task)?;
                    left.state = TaskState::Completed;
                    tx.put("task", task, &left)?;
                    for item in tx.read().list::<AttentionItem>("attention")? {
                        if item.kind == AttentionKind::ReconciliationRequired {
                            tx.delete("attention", item.attention_id)?;
                        }
                    }
                    Ok(())
                })
                .unwrap();
            assert!(helpers_finishing(
                &f.hub.read(|r| r.require::<Run>("run", run)).unwrap()
            ));
            drop(f.hub);
            f.hub = CompanionHub::builder()
                .data_dir(&f.path)
                .clock(f.clock.clone())
                .harness(f.harness.clone())
                .bridge(crate::bridge::BridgeConfig::new(
                    "synthetic-bridge",
                    "synthetic-socket",
                ))
                .build()
                .await
                .unwrap();
            assert_writer(&f, Some(run));
            let open: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            assert_eq!(
                open.state,
                RunState::ReconciliationRequired,
                "{saved_unrecorded}"
            );
            let decide: Task = f.hub.read(|r| r.require("task", task)).unwrap();
            assert_eq!(decide.state, TaskState::ReconciliationRequired);
            let raised = f
                .hub
                .read(|r| r.list::<AttentionItem>("attention"))
                .unwrap();
            assert!(raised.iter().any(|item| item.run_id == Some(run)
                && item.kind == AttentionKind::ReconciliationRequired
                && item.resolved_at.is_none()));
            // The person's decision frees the project.
            let mut reconcile = CommandEnvelope::new(Command::ReconcileTask {
                task_id: task,
                resolution: Reconciliation::MarkCompleted,
            });
            reconcile.expected_revision = Some(decide.revision);
            let receipt = f.hub.submit(&Caller::local_owner(), reconcile).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            assert_writer(&f, None);
            assert!(!pending(&f, run));
            f.close().await;
        }
    }

    #[tokio::test]
    async fn stop_reaches_the_run_the_person_saw_after_later_agent_activity() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let task = f.task(None).await;
        let first = task.current_run.unwrap();
        f.dispatched(first).await;
        f.hub.finish_run(first, RunState::Completed, None).unwrap();
        let before: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        f.hub
            .submit(
                &owner,
                f.resume(task.task_id, Some("Continue".into()), None),
            )
            .await;
        let run = f
            .hub
            .read(|r| r.require::<Task>("task", task.task_id))
            .unwrap()
            .current_run
            .unwrap();
        assert_ne!(run, first);
        f.dispatched(run).await;
        let child_id = ChildId::new();
        f.hub
            .write(|tx| {
                let mut current: Run = tx.read().require("run", run)?;
                current.children.push(ChildAgent {
                    lifetime: None,
                    child_id,
                    native_ref: Some("helper".into()),
                    label: "Helper".into(),
                    state: ChildState::Active,
                    started_at: current.admitted_at,
                    ended_at: None,
                    provenance: Provenance::Native,
                    stop: Some(Availability::Available),
                });
                tx.put("run", run, &current)
            })
            .unwrap();
        let seen: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        // The agent keeps working after the phone last heard from the computer.
        f.hub
            .harness_event(
                run,
                HarnessEvent::Activity {
                    key: "tool-1".into(),
                    tool: "Bash".into(),
                    summary: "Run the tests".into(),
                    status: ActivityStatus::Running,
                    detail: None,
                },
            )
            .await
            .unwrap();
        let stop = |revision| {
            let mut stop = CommandEnvelope::new(Command::StopTask {
                task_id: task.task_id,
                force: false,
            });
            stop.expected_revision = Some(revision);
            stop
        };
        let mut stop_child = CommandEnvelope::new(Command::StopChild {
            run_id: run,
            child_id,
        });
        stop_child.expected_revision = Some(seen.revision);
        let receipt = f.hub.submit(&owner, stop_child).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        // A revision from before this run began never stops it.
        let refused = f.hub.submit(&owner, stop(before.revision)).await;
        assert_eq!(refused.error.unwrap().code, ErrorCode::StaleRevision);
        let receipt = f.hub.submit(&owner, stop(seen.revision)).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let stopping: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_eq!(stopping.state, RunState::Stopping);
        f.close().await;
    }

    #[tokio::test]
    async fn a_helper_turn_after_the_parent_ended_does_not_reopen_the_run() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        f.hub
            .harness_event(run, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        f.hub
            .harness_event(
                run,
                HarnessEvent::ParentEnded {
                    exit: ports::RunExit::Completed,
                },
            )
            .await
            .unwrap();
        // A background helper keeps streaming model turns of its own.
        for event in [
            HarnessEvent::Child {
                lifetime: Some(ChildLifetime::InProcess),
                native_ref: "helper".into(),
                label: "Helper".into(),
                state: ChildState::Active,
            },
            HarnessEvent::TurnStarted,
            HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        ] {
            f.hub.harness_event(run, event).await.unwrap();
            let ended: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            assert_eq!(ended.state, RunState::Completed);
            let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
            assert_eq!(task.state, TaskState::Completed);
        }
        f.close().await;
    }

    #[tokio::test]
    async fn a_helper_turn_does_not_hide_a_pending_approval() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        f.hub
            .harness_event(
                run,
                HarnessEvent::ApprovalRequested(ports::NativeApproval {
                    native_request_id: "review".into(),
                    kind: ApprovalKind::Command,
                    title: "Review command".into(),
                    detail: None,
                    detail_lines: Vec::new(),
                    questions: Vec::new(),
                    raw_request: None,
                    scope: "once".into(),
                    shortened: false,
                    tool_call: None,
                }),
            )
            .await
            .unwrap();
        f.hub
            .harness_event(run, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        let waiting: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_eq!(waiting.state, RunState::AwaitingApproval);
        let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(task.state, TaskState::AwaitingDecision);
        f.close().await;
    }

    #[tokio::test]
    async fn a_failed_or_unknown_exit_says_why_in_the_conversation() {
        for (exit, state, note) in [
            (
                ports::RunExit::Failed {
                    reason: "You've hit your usage limit. Try again at 5 pm.".into(),
                },
                RunState::Failed,
                "The agent stopped with an error. You've hit your usage limit. Try again at 5 pm.",
            ),
            (
                ports::RunExit::Unknown {
                    reason: "native process exited without a completed turn".into(),
                },
                RunState::ReconciliationRequired,
                "The agent’s result couldn’t be confirmed. native process exited without a completed turn",
            ),
        ] {
            let f = Fixture::new().await;
            let task = f.task(None).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            f.hub
                .harness_event(
                    run,
                    HarnessEvent::Exited {
                        exit,
                        outcome: None,
                    },
                )
                .await
                .unwrap();
            let ended: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            assert_eq!(ended.state, state);
            let notes: Vec<String> = timeline(&f, task.task_id)
                .into_iter()
                .filter_map(|item| match item.item {
                    TimelineBody::Note { text } => Some(text),
                    _ => None,
                })
                .collect();
            assert_eq!(notes, vec![note.to_owned()]);
            f.close().await;
        }
    }

    #[tokio::test]
    async fn a_native_event_reads_only_its_own_task_attention() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        // Attention raised for other tasks over the months is not this
        // event's business; an event must not rescan it. A row no version of
        // the companion can read makes any such scan visible.
        f.hub
            .write(|tx| tx.put("attention", AttentionId::new(), &"unreadable"))
            .unwrap();
        f.hub
            .harness_event(
                run,
                HarnessEvent::Activity {
                    key: "tool-1".into(),
                    tool: "Bash".into(),
                    summary: "Run the tests".into(),
                    status: ActivityStatus::Running,
                    detail: None,
                },
            )
            .await
            .unwrap();
        f.close().await;
    }

    #[tokio::test]
    async fn delivery_echo_waits_for_the_message_record_and_does_not_guess() {
        for response in [
            DeliveryState::QueuedForBoundary,
            DeliveryState::HostAccepted,
            DeliveryState::NativeDelivered,
        ] {
            let f = Fixture::new().await;
            let task = f.task(None).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            let control = &f.harness.controls;
            *control.delivery.lock().unwrap() = response;
            control.hold_message.store(true, Ordering::SeqCst);
            let hub = f.hub.clone();
            let send = tokio::spawn(async move {
                hub.send_message(
                    &Caller::local_owner(),
                    CommandId::new(),
                    run,
                    "race",
                    None,
                    MessageDelivery::NextTurn,
                )
                .await
            });
            control.message_entered.acquire().await.unwrap().forget();
            {
                let event = f.hub.harness_event(
                    run,
                    HarnessEvent::MessageDelivery {
                        message_ref: *control.message_refs.lock().unwrap().last().unwrap(),
                        state: DeliveryState::NativeDelivered,
                    },
                );
                tokio::pin!(event);
                tokio::select! {
                    biased;
                    _ = &mut event => panic!("Echo raced past the unrecorded send"),
                    () = std::future::ready(()) => {}
                }
                control.message_gate.add_permits(1);
                send.await.unwrap().unwrap();
                event.await.unwrap();
            }
            let seq = f.messages(task.task_id).await.last().unwrap().seq;
            assert_eq!(
                deliveries(&f, task.task_id),
                if response == DeliveryState::QueuedForBoundary {
                    vec![
                        (seq, DeliveryState::QueuedForBoundary),
                        (seq, DeliveryState::NativeDelivered),
                    ]
                } else {
                    vec![(seq, DeliveryState::NativeDelivered)]
                }
            );
            f.close().await;
        }
    }

    #[tokio::test]
    async fn send_reply_after_run_end_cannot_leave_a_message_queued() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let control = &f.harness.controls;
        *control.delivery.lock().unwrap() = DeliveryState::QueuedForBoundary;
        control.hold_message.store(true, Ordering::SeqCst);
        let hub = f.hub.clone();
        let send = tokio::spawn(async move {
            hub.send_message(
                &Caller::local_owner(),
                CommandId::new(),
                run,
                "race",
                None,
                MessageDelivery::NextTurn,
            )
            .await
        });
        control.message_entered.acquire().await.unwrap().forget();
        f.hub.finish_run(run, RunState::Interrupted, None).unwrap();
        control.message_gate.add_permits(1);
        send.await.unwrap().unwrap();
        let seq = f.messages(task.task_id).await.last().unwrap().seq;
        assert_eq!(
            deliveries(&f, task.task_id),
            vec![(seq, DeliveryState::Failed)]
        );
        f.close().await;
    }

    #[tokio::test]
    async fn restart_preserves_delivery_tracking_until_explicit_reconciliation() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let (accepted, accepted_ref) =
            followup(&f, task.task_id, DeliveryState::HostAccepted).await;
        let (queued, queued_ref) =
            followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        let (unknown, unknown_ref) = followup(&f, task.task_id, DeliveryState::Unknown).await;
        // Abrupt cancellation models a crash without the shutdown path's
        // run-end events. No native tools or desktop resources exist here.
        let jobs = std::mem::take(&mut *f.hub.inner.jobs.lock().unwrap());
        for job in &jobs {
            job.abort();
        }
        for job in jobs {
            let _ = job.await;
        }
        drop(f.hub);
        let hub = CompanionHub::builder()
            .data_dir(&f.path)
            .clock(f.clock)
            .harness(f.harness)
            .build()
            .await
            .unwrap();
        let pending: Vec<InFlightMessage> =
            hub.read(|r| r.require("message_delivery", run)).unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|m| (m.seq, m.state, m.message_ref))
                .collect::<Vec<_>>(),
            vec![
                (accepted, DeliveryState::HostAccepted, Some(accepted_ref)),
                (queued, DeliveryState::QueuedForBoundary, Some(queued_ref)),
                (unknown, DeliveryState::Unknown, Some(unknown_ref))
            ]
        );
        let recovered: Task = hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(recovered.state, TaskState::ReconciliationRequired);
        let mut reconcile = CommandEnvelope::new(Command::ReconcileTask {
            task_id: task.task_id,
            resolution: Reconciliation::MarkFailed,
        });
        reconcile.expected_revision = Some(recovered.revision);
        assert_eq!(
            hub.submit(&Caller::local_owner(), reconcile).await.status,
            ReceiptStatus::Accepted
        );
        let history: Vec<TimelineItem> = hub
            .read(|r| r.list(&format!("timeline:{}", task.task_id)))
            .unwrap();
        let updates: Vec<_> = history
            .iter()
            .filter_map(|i| {
                if let TimelineBody::Delivery {
                    message_seq, state, ..
                } = i.item
                {
                    Some((message_seq, state))
                } else {
                    None
                }
            })
            .collect();
        let initial = history
            .iter()
            .find(|i| matches!(i.item, TimelineBody::UserMessage { .. }))
            .unwrap()
            .seq;
        assert_eq!(
            updates,
            vec![
                (queued, DeliveryState::QueuedForBoundary),
                (unknown, DeliveryState::Unknown),
                (initial, DeliveryState::Unknown),
                (queued, DeliveryState::Failed)
            ]
        );
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(f.path.with_extension("project")).unwrap();
        std::fs::remove_dir_all(f.path).unwrap();
    }

    #[tokio::test]
    async fn legacy_delivery_records_load_unmatched_and_resolve_only_at_run_end() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        let (accepted, accepted_ref) =
            followup(&f, task.task_id, DeliveryState::HostAccepted).await;
        let (queued, queued_ref) =
            followup(&f, task.task_id, DeliveryState::QueuedForBoundary).await;
        let (unknown, unknown_ref) = followup(&f, task.task_id, DeliveryState::Unknown).await;
        let legacy = serde_json::json!([
            {"seq": accepted, "state": "host_accepted"},
            {"seq": queued, "state": "queued_for_boundary"},
            {"seq": unknown, "state": "unknown"},
        ]);
        f.hub
            .write(|tx| tx.put("message_delivery", run, &legacy))
            .unwrap();
        let before = timeline(&f, task.task_id);
        let jobs = std::mem::take(&mut *f.hub.inner.jobs.lock().unwrap());
        for job in &jobs {
            job.abort();
        }
        for job in jobs {
            let _ = job.await;
        }
        drop(f.hub);
        let hub = CompanionHub::builder()
            .data_dir(&f.path)
            .clock(f.clock)
            .harness(f.harness)
            .build()
            .await
            .unwrap();
        let pending: Vec<InFlightMessage> =
            hub.read(|r| r.require("message_delivery", run)).unwrap();
        assert_eq!(pending.len(), 3);
        assert!(pending.iter().all(|m| m.message_ref.is_none()));
        for message_ref in [accepted_ref, queued_ref, unknown_ref] {
            hub.harness_event(
                run,
                HarnessEvent::MessageDelivery {
                    message_ref,
                    state: DeliveryState::NativeDelivered,
                },
            )
            .await
            .unwrap();
        }
        assert_eq!(
            hub.read(|r| r.require::<serde_json::Value>("message_delivery", run))
                .unwrap(),
            legacy
        );
        let after: Vec<TimelineItem> = hub
            .read(|r| r.list(&format!("timeline:{}", task.task_id)))
            .unwrap();
        assert_eq!(after, before);
        hub.finish_run(run, RunState::Failed, None).unwrap();
        let history: Vec<TimelineItem> = hub
            .read(|r| r.list(&format!("timeline:{}", task.task_id)))
            .unwrap();
        let updates: Vec<_> = history
            .iter()
            .filter_map(|i| match i.item {
                TimelineBody::Delivery {
                    message_seq, state, ..
                } => Some((message_seq, state)),
                _ => None,
            })
            .collect();
        let initial = history
            .iter()
            .find(|i| matches!(i.item, TimelineBody::UserMessage { .. }))
            .unwrap()
            .seq;
        assert_eq!(
            updates,
            vec![
                (queued, DeliveryState::QueuedForBoundary),
                (unknown, DeliveryState::Unknown),
                (initial, DeliveryState::Unknown),
                (queued, DeliveryState::Failed)
            ]
        );
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(f.path.with_extension("project")).unwrap();
        std::fs::remove_dir_all(f.path).unwrap();
    }

    fn start_deliveries(f: &Fixture, task: TaskId) -> Vec<(u64, DeliveryState)> {
        timeline(f, task)
            .into_iter()
            .filter_map(|item| match item.item {
                TimelineBody::Delivery {
                    message_seq, state, ..
                } => Some((message_seq, state)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn start_prompt_delivery_new_continued_and_legacy_runs() {
        let f = Fixture::new().await;
        let task = f.task(Some(TaskMode::Ask)).await;
        let first = task.current_run.unwrap();
        f.dispatched(first).await;
        let seq = timeline(&f, task.task_id)
            .iter()
            .find(|i| matches!(i.item, TimelineBody::UserMessage { .. }))
            .unwrap()
            .seq;
        f.hub
            .harness_event(first, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        f.hub
            .harness_event(first, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        assert_eq!(
            start_deliveries(&f, task.task_id),
            vec![(seq, DeliveryState::NativeDelivered)]
        );
        f.hub.finish_run(first, RunState::Completed, None).unwrap();
        f.hub
            .submit(
                &Caller::local_owner(),
                f.resume(task.task_id, Some("continue".into()), None),
            )
            .await;
        let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        let second = task.current_run.unwrap();
        f.dispatched(second).await;
        let seq2 = timeline(&f, task.task_id)
            .iter()
            .rev()
            .find(|i| matches!(i.item, TimelineBody::UserMessage { .. }))
            .unwrap()
            .seq;
        f.hub
            .harness_event(second, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        assert_eq!(
            start_deliveries(&f, task.task_id),
            vec![
                (seq, DeliveryState::NativeDelivered),
                (seq2, DeliveryState::NativeDelivered)
            ]
        );
        f.hub.finish_run(second, RunState::Completed, None).unwrap();
        // Model an older admitted run with no initial-message tracking, before
        // any native start evidence. Never misidentify a follow-up as its prompt.
        let legacy = f.task(Some(TaskMode::Ask)).await;
        let legacy_run = legacy.current_run.unwrap();
        f.dispatched(legacy_run).await;
        f.hub
            .write(|tx| tx.delete("start_message", legacy_run))
            .unwrap();
        f.hub
            .harness_event(legacy_run, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        f.hub
            .finish_run(legacy_run, RunState::Completed, None)
            .unwrap();
        assert!(start_deliveries(&f, legacy.task_id).is_empty());
        f.close().await;
    }

    #[tokio::test]
    async fn start_prompt_exit_without_evidence_is_unknown() {
        let f = Fixture::new().await;
        let task = f.task(Some(TaskMode::Ask)).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        f.hub
            .harness_event(
                run,
                HarnessEvent::Exited {
                    exit: RunExit::Failed {
                        reason: "early exit".into(),
                    },
                    outcome: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            start_deliveries(&f, task.task_id)
                .iter()
                .map(|d| d.1)
                .collect::<Vec<_>>(),
            vec![DeliveryState::Unknown]
        );
        f.close().await;
    }

    #[tokio::test]
    async fn start_prompt_spawn_failure_and_refusal_are_failed() {
        for spawn_failure in [true, false] {
            let f = Fixture::new().await;
            if spawn_failure {
                f.harness.fail_spawn.store(true, Ordering::SeqCst);
            } else {
                *f.harness.refuse_start.lock().unwrap() = Some("dispatch refused".into());
            }
            let task = f.task(Some(TaskMode::Ask)).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while start_deliveries(&f, task.task_id).is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                start_deliveries(&f, task.task_id)
                    .iter()
                    .map(|d| d.1)
                    .collect::<Vec<_>>(),
                vec![DeliveryState::Failed]
            );
            f.close().await;
        }
    }

    #[tokio::test]
    async fn start_prompt_codex_session_is_not_turn_evidence() {
        let f = Fixture::new().await;
        let task = f.task(Some(TaskMode::Ask)).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        f.hub
            .write(|tx| {
                let mut run: Run = tx.read().require("run", run)?;
                run.harness = HarnessKind::Codex;
                tx.put("run", run.run_id, &run)
            })
            .unwrap();
        f.hub
            .harness_event(
                run,
                HarnessEvent::SessionStarted {
                    native_session: "session".into(),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await
            .unwrap();
        assert!(start_deliveries(&f, task.task_id).is_empty());
        f.hub
            .harness_event(run, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        assert_eq!(
            start_deliveries(&f, task.task_id)[0].1,
            DeliveryState::NativeDelivered
        );
        f.close().await;
    }

    #[tokio::test]
    async fn queued_first_annotated_message_is_durably_associated_with_its_run() {
        let f = Fixture::new().await;
        let bundle = f.bundle(TaskId::new()).await;
        let mut create = f.create(Some(TaskMode::Ask));
        if let Command::CreateTask {
            placement,
            bundle: annotation,
            ..
        } = &mut create.body
        {
            *placement = TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            };
            *annotation = Some(bundle.clone());
        }
        let receipt = f.hub.submit(&Caller::local_owner(), create).await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            panic!("{receipt:?}")
        };
        let original = f.messages(task_id).await.remove(0);
        assert_eq!(original.run_id, None);
        f.hub
            .write(|tx| {
                let mut queue: QueueView = tx.read().require("queue", f.project)?;
                queue.paused = false;
                tx.put("queue", f.project, &queue)
            })
            .unwrap();
        f.hub
            .submit(&Caller::local_owner(), f.resume(task_id, None, None))
            .await;
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        let run = task.current_run.unwrap();
        assert_eq!(f.dispatched(run).await.prompt.images.len(), 1);
        assert!(
            matches!(&original.item, TimelineBody::UserMessage { bundle: Some(b), .. } if b == &bundle)
        );
        f.hub
            .harness_event(run, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        let history = timeline(&f, task_id);
        let delivery = history.iter().find(|i| matches!(i.item, TimelineBody::Delivery { message_seq, state: DeliveryState::NativeDelivered, .. } if message_seq == original.seq)).unwrap();
        assert_eq!(delivery.run_id, Some(run));
        assert_eq!(f.messages(task_id).await[0], original);
        f.close().await;
    }

    #[tokio::test]
    async fn start_prompt_next_up_tracks_the_edited_prompt() {
        let f = Fixture::new().await;
        let mut create = f.create(Some(TaskMode::Ask));
        if let Command::CreateTask { placement, .. } = &mut create.body {
            *placement = TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            };
        }
        let receipt = f.hub.submit(&Caller::local_owner(), create).await;
        let Some(CommandResult::TaskCreated {
            task_id,
            queue_item_id: Some(item),
            ..
        }) = receipt.result
        else {
            panic!("{receipt:?}")
        };
        f.hub
            .write(|tx| {
                let mut queue: QueueView = tx.read().require("queue", f.project)?;
                queue.paused = false;
                tx.put("queue", f.project, &queue)?;
                f.hub
                    .edit_queue_prompt(tx, item, "edited prompt", Some(queue.revision))
            })
            .unwrap();
        let seq = timeline(&f, task_id)
            .iter()
            .rev()
            .find(|i| matches!(i.item, TimelineBody::UserMessage { .. }))
            .unwrap()
            .seq;
        f.hub
            .submit(&Caller::local_owner(), f.resume(task_id, None, None))
            .await;
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        let run = task.current_run.unwrap();
        assert_eq!(f.dispatched(run).await.prompt.text, "edited prompt");
        f.hub
            .harness_event(run, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        assert_eq!(
            start_deliveries(&f, task_id),
            vec![(seq, DeliveryState::NativeDelivered)]
        );
        let history = timeline(&f, task_id);
        let prompt = history.iter().find(|i| i.seq == seq).unwrap();
        assert_eq!(prompt.run_id, None, "Queued message predates its run");
        let settlement = history.iter().find(|i| matches!(i.item, TimelineBody::Delivery { message_seq, .. } if message_seq == seq)).unwrap();
        assert_eq!(settlement.run_id, Some(run), "Durable receipt association");
        f.close().await;
    }

    impl Fixture {
        async fn new() -> Self {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/continuation-tests")
                .join(uuid::Uuid::new_v4().to_string());
            let clock = Arc::new(FakeClock::default());
            let harness = Arc::new(FakeHarness::default());
            let hub = CompanionHub::builder()
                .data_dir(&path)
                .clock(clock.clone())
                .harness(harness.clone())
                .bridge(crate::bridge::BridgeConfig::new(
                    "synthetic-bridge",
                    "synthetic-socket",
                ))
                .build()
                .await
                .unwrap();
            // Beside the data folder, which no project may overlap.
            let folder = path.with_extension("project");
            std::fs::create_dir_all(&folder).unwrap();
            let receipt = hub
                .submit(
                    &Caller::local_owner(),
                    CommandEnvelope::new(Command::AddProject {
                        host_path: folder.to_string_lossy().into_owned(),
                        name: Some("Test project".into()),
                    }),
                )
                .await;
            let Some(CommandResult::ProjectAdded {
                project_id: project,
            }) = receipt.result
            else {
                panic!("{receipt:?}")
            };
            hub.write(|tx| {
                let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
                infos[0].modes = [TaskMode::Ask, TaskMode::Plan, TaskMode::Work]
                    .into_iter()
                    .map(|mode| ModeSupport {
                        mode,
                        availability: Availability::Available,
                        enforcement: "Synthetic fixture restriction".into(),
                    })
                    .collect();
                tx.put("meta", "harnesses", &infos)
            })
            .unwrap();
            Self {
                hub,
                path,
                harness,
                clock,
                project,
            }
        }
        fn create(&self, mode: Option<TaskMode>) -> CommandEnvelope {
            CommandEnvelope::new(Command::CreateTask {
                github: None,
                agent_chosen: None,
                project_id: self.project,
                kind: TaskKind::Coding,
                mode,
                title: Some("Inspect".into()),
                prompt: "initial".into(),
                bundle: None,
                harness: HarnessKind::ClaudeCode,
                model: None,
                effort: None,
                launch_policy: None,
                branch: None,
                mission: None,
                target_id: None,
                placement: TaskPlacement::StartNow,
                start: None,
                place: None,
                browser: None,
                conversation: None,
                access: None,
                review: None,
            })
        }
        async fn task(&self, mode: Option<TaskMode>) -> Task {
            let receipt = self
                .hub
                .submit(&Caller::local_owner(), self.create(mode))
                .await;
            let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
                panic!("{receipt:?}")
            };
            self.hub.read(|r| r.require("task", task_id)).unwrap()
        }
        async fn dispatched(&self, run: RunId) -> ports::RunSpec {
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if let Some(spec) = self
                        .harness
                        .specs
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|s| s.run_id == run)
                        .cloned()
                    {
                        return spec;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("run was not dispatched")
        }
        fn resume(
            &self,
            task: TaskId,
            text: Option<String>,
            bundle: Option<Box<AnnotationBundle>>,
        ) -> CommandEnvelope {
            let task: Task = self.hub.read(|r| r.require("task", task)).unwrap();
            let mut envelope = CommandEnvelope::new(Command::ResumeTask {
                mode: None,
                task_id: task.task_id,
                text,
                bundle,
                carried_plan: None,
                read_only: false,
                model: None,
                effort: None,
            });
            envelope.expected_revision = Some(task.revision);
            envelope
        }
        async fn messages(&self, task: TaskId) -> Vec<TimelineItem> {
            let data = self
                .hub
                .query(
                    &Caller::local_owner(),
                    Query::TaskTimeline {
                        task_id: task,
                        before_seq: None,
                        limit: 100,
                    },
                )
                .await
                .unwrap();
            let QueryData::TaskTimeline { items, .. } = data else {
                panic!("timeline")
            };
            items
                .into_iter()
                .filter(|item| matches!(item.item, TimelineBody::UserMessage { .. }))
                .collect()
        }
        async fn bundle(&self, task: TaskId) -> Box<AnnotationBundle> {
            let artifact = self
                .hub
                .put_artifact(
                    b"synthetic-image",
                    "image/png",
                    Some(task),
                    protocol::auth::Principal::LocalOwner,
                )
                .await
                .unwrap();
            Box::new(AnnotationBundle {
                bundle_id: AnnotationBundleId::new(),
                revision: 1,
                sources: vec![AnnotationSource {
                    key: "S1".into(),
                    artifact_id: artifact.artifact_id,
                    sha256: artifact.sha256,
                    media_type: artifact.media_type,
                    width: 1,
                    height: 1,
                    origin: SourceOrigin::PhoneImport {
                        original_name: None,
                    },
                    captured_at: protocol::time::Timestamp(123),
                    derived_from: None,
                    derivation: None,
                }],
                marks: vec![],
                comments: vec![],
                steps: vec![],
                general_note: Some("Inspect this region".into()),
                speech: None,
            })
        }
        async fn close(self) {
            self.hub.shutdown().await.unwrap();
            drop(self.hub);
            std::fs::remove_dir_all(self.path.with_extension("project")).unwrap();
            std::fs::remove_dir_all(self.path).unwrap();
        }
    }

    fn assert_writer(f: &Fixture, writer: Option<RunId>) {
        let project: Project = f.hub.read(|r| r.require("project", f.project)).unwrap();
        let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        assert_eq!(project.active_writer, writer);
        assert_eq!(queue.writer, writer);
        for run in f.hub.read(|r| r.list::<Run>("run")).unwrap() {
            let task: Task = f.hub.read(|r| r.require("task", run.task_id)).unwrap();
            if run_mode(&run, &task) != TaskMode::Work {
                assert!(!run.holds_writer);
                assert_ne!(writer, Some(run.run_id));
            }
        }
    }

    #[tokio::test]
    async fn launch_policy_survives_admission_retry_and_plan_to_work_resume() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        for policy in [
            None,
            Some(LaunchPolicy::BypassPermissions),
            Some(LaunchPolicy::AcceptEdits),
            Some(LaunchPolicy::Standard),
        ] {
            let mut request = f.create(Some(TaskMode::Plan));
            if let Command::CreateTask { launch_policy, .. } = &mut request.body {
                *launch_policy = policy;
            }
            let receipt = f.hub.submit(&owner, request.clone()).await;
            let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
                panic!("{receipt:?}");
            };
            assert_eq!(
                f.hub.submit(&owner, request).await.status,
                ReceiptStatus::Duplicate
            );
            let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
            assert_eq!(task.launch_policy, policy);
            let run = task.current_run.unwrap();
            assert_eq!(f.dispatched(run).await.launch_policy, policy);
            assert_eq!(
                f.hub
                    .read(|r| r.require::<Run>("run", run))
                    .unwrap()
                    .launch_policy,
                policy
            );
            f.hub
                .harness_event(
                    run,
                    HarnessEvent::SessionStarted {
                        native_session: "saved-policy-session".into(),
                        actual_model: None,
                        actual_effort: None,
                        account_label: None,
                    },
                )
                .await
                .unwrap();
            f.hub.finish_run(run, RunState::Completed, None).unwrap();
            let mut resume = f.resume(task_id, Some("Carry out the plan".into()), None);
            if let Command::ResumeTask { mode, .. } = &mut resume.body {
                *mode = Some(TaskMode::Work);
            }
            let receipt = f.hub.submit(&owner, resume).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
            let run = task.current_run.unwrap();
            let spec = f.dispatched(run).await;
            assert_eq!(spec.launch_policy, policy);
            assert_eq!(spec.mode, TaskMode::Work);
            assert_eq!(spec.resume_session.as_deref(), Some("saved-policy-session"));
            f.hub.finish_run(run, RunState::Completed, None).unwrap();
        }
        let mut invalid = f.create(None);
        if let Command::CreateTask { launch_policy, .. } = &mut invalid.body {
            *launch_policy = Some(LaunchPolicy::Sandboxed);
        }
        assert_eq!(
            f.hub.submit(&owner, invalid).await.status,
            ReceiptStatus::Rejected
        );
        f.close().await;
    }

    #[tokio::test]
    async fn a_chosen_branch_reaches_each_run_and_names_git_refuses_are_refused() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        // A new branch stays new: the agent is told to make it before
        // changing any file, which a read-only Plan run never does.
        let chosen = protocol::model::TaskBranch {
            name: "feature/search".into(),
            new: true,
        };
        let mut request = f.create(Some(TaskMode::Plan));
        if let Command::CreateTask { branch, .. } = &mut request.body {
            *branch = Some(chosen.clone());
        }
        let receipt = f.hub.submit(&owner, request).await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            panic!("{receipt:?}");
        };
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        assert_eq!(task.branch.as_ref(), Some(&chosen));
        let run = task.current_run.unwrap();
        assert_eq!(f.dispatched(run).await.branch.as_ref(), Some(&chosen));
        f.hub.finish_run(run, RunState::Completed, None).unwrap();
        let mut resume = f.resume(task_id, Some("Carry out the plan".into()), None);
        if let Command::ResumeTask { mode, .. } = &mut resume.body {
            *mode = Some(TaskMode::Work);
        }
        let receipt = f.hub.submit(&owner, resume).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        let spec = f.dispatched(task.current_run.unwrap()).await;
        assert_eq!(spec.branch.as_ref(), Some(&chosen));
        f.hub
            .finish_run(task.current_run.unwrap(), RunState::Completed, None)
            .unwrap();
        // No branch chosen leaves it to the agent.
        let task = f.task(None).await;
        assert_eq!(task.branch, None);
        assert_eq!(f.dispatched(task.current_run.unwrap()).await.branch, None);
        for bad in ["--force", "a..b", "HEAD@{1}", "with space", ""] {
            let mut invalid = f.create(None);
            if let Command::CreateTask { branch, .. } = &mut invalid.body {
                *branch = Some(protocol::model::TaskBranch {
                    name: bad.into(),
                    new: true,
                });
            }
            assert_eq!(
                f.hub.submit(&owner, invalid).await.status,
                ReceiptStatus::Rejected,
                "{bad:?}"
            );
        }
        assert!(matches!(
            f.hub
                .query(
                    &owner,
                    protocol::Query::ProjectBranches {
                        project_id: f.project
                    }
                )
                .await,
            Ok(protocol::QueryData::ProjectBranches { .. })
        ));
        f.close().await;
    }

    #[tokio::test]
    async fn reviewed_plan_becomes_work_in_the_same_native_session() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let task = f.task(Some(TaskMode::Plan)).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        f.hub
            .harness_event(
                run,
                HarnessEvent::SessionStarted {
                    native_session: "plan-session".into(),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await
            .unwrap();
        f.hub.finish_run(run, RunState::Completed, None).unwrap();
        let current: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        let command: Command = serde_json::from_value(serde_json::json!({
            "command":"resume_task", "task_id":task.task_id, "text":"Carry out this plan: inspect and fix the parser", "mode":"work"
        })).unwrap();
        let writer = f.task(Some(TaskMode::Work)).await.current_run.unwrap();
        let mut request = CommandEnvelope::new(command.clone());
        request.expected_revision = Some(current.revision);
        let blocked = f.hub.submit(&owner, request).await;
        assert_eq!(blocked.error.unwrap().code, ErrorCode::Conflict);
        let unchanged: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(unchanged.mode, TaskMode::Plan);
        f.hub.finish_run(writer, RunState::Completed, None).unwrap();
        let mut stale = CommandEnvelope::new(command.clone());
        stale.expected_revision = Some(current.revision - 1);
        assert_eq!(
            f.hub.submit(&owner, stale).await.error.unwrap().code,
            ErrorCode::StaleRevision
        );
        let mut request = CommandEnvelope::new(command);
        request.expected_revision = Some(current.revision);
        let receipt = f.hub.submit(&owner, request).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let resumed: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        let spec = f.dispatched(resumed.current_run.unwrap()).await;
        assert_eq!(spec.mode, TaskMode::Work);
        assert_eq!(spec.resume_from_mode, Some(TaskMode::Plan));
        assert_eq!(spec.resume_session.as_deref(), Some("plan-session"));
        assert_eq!(resumed.task_id, task.task_id);
        assert_writer(&f, resumed.current_run);
        // The task is Work now; each run keeps the mode it was admitted in.
        let mode = |run: RunId| f.hub.read(|r| r.require::<Run>("run", run)).unwrap().mode;
        assert_eq!(mode(run), Some(TaskMode::Plan));
        assert_eq!(mode(resumed.current_run.unwrap()), Some(TaskMode::Work));
        f.close().await;
    }

    /// Run `task`'s current turn to completion in native session `session`.
    async fn finish_turn(f: &Fixture, task: TaskId, session: &str) -> ports::RunSpec {
        let current: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        let run = current.current_run.unwrap();
        let spec = f.dispatched(run).await;
        f.hub
            .harness_event(
                run,
                HarnessEvent::SessionStarted {
                    native_session: session.into(),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await
            .unwrap();
        f.hub.finish_run(run, RunState::Completed, None).unwrap();
        spec
    }

    /// A resume of `task` from `fields`, at its current revision.
    fn turn(f: &Fixture, task: TaskId, fields: serde_json::Value) -> CommandEnvelope {
        let current: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        let mut body = serde_json::json!({"command": "resume_task", "task_id": task});
        body.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let mut envelope = CommandEnvelope::new(serde_json::from_value(body).unwrap());
        envelope.expected_revision = Some(current.revision);
        envelope
    }

    fn conversation(f: &Fixture) -> CommandEnvelope {
        let mut create = f.create(Some(TaskMode::Ask));
        let Command::CreateTask { conversation, .. } = &mut create.body else {
            unreachable!()
        };
        *conversation = Some(true);
        create
    }

    #[tokio::test]
    async fn a_model_chosen_between_turns_reaches_that_turn_and_the_later_ones() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let task = f.task(Some(TaskMode::Ask)).await.task_id;
        finish_turn(&f, task, "talk-session").await;
        let models = |f: &Fixture| {
            let t: Task = f.hub.read(|r| r.require("task", task)).unwrap();
            (t.model, t.effort)
        };
        let some = |m: &str, e: Option<&str>| (Some(m.to_owned()), e.map(str::to_owned));
        for (fields, expected) in [
            (
                serde_json::json!({"text": "and now?", "model": "opus", "effort": "high"}),
                some("opus", Some("high")),
            ),
            // An effort alone keeps the model.
            (
                serde_json::json!({"text": "briefly", "effort": "low"}),
                some("opus", Some("low")),
            ),
            // A turn that chooses nothing keeps both.
            (
                serde_json::json!({"text": "go on"}),
                some("opus", Some("low")),
            ),
            // A model alone takes its own default effort.
            (
                serde_json::json!({"text": "again", "model": "sonnet"}),
                some("sonnet", None),
            ),
        ] {
            let receipt = f.hub.submit(&owner, turn(&f, task, fields)).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            assert_eq!(models(&f), expected);
            let spec = finish_turn(&f, task, "talk-session").await;
            assert_eq!((spec.model, spec.effort), expected);
            assert_eq!(spec.mode, TaskMode::Ask);
            assert_eq!(spec.resume_session.as_deref(), Some("talk-session"));
        }
        // A read-only turn keeps the task's model.
        let receipt = f
            .hub
            .submit(
                &owner,
                turn(
                    &f,
                    task,
                    serde_json::json!({"text": "sum up", "read_only": true, "model": "opus"}),
                ),
            )
            .await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        // Once the computer lists its models, an effort the model lacks is
        // refused and the task keeps what it had.
        f.hub
            .write(|tx| {
                let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
                infos[0].catalog = Some(CatalogObservation {
                    checked_at: protocol::time::Timestamp(1),
                    source: "Synthetic catalog".into(),
                    availability: Availability::Available,
                    default_model: Some("opus".into()),
                    default_effort: None,
                });
                infos[0].models = vec![ModelOption {
                    id: "opus".into(),
                    label: "Opus".into(),
                    suggested_for_computer_use: false,
                    is_default: true,
                    details: Some(ModelDetails {
                        alias: false,
                        efforts: vec![EffortOption {
                            id: "high".into(),
                            label: "High".into(),
                        }],
                        default_effort: Some("high".into()),
                    }),
                }];
                tx.put("meta", "harnesses", &infos)
            })
            .unwrap();
        let receipt = f
            .hub
            .submit(
                &owner,
                turn(
                    &f,
                    task,
                    serde_json::json!({"text": "hard", "model": "opus", "effort": "max"}),
                ),
            )
            .await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        assert_eq!(models(&f), some("sonnet", None));
        let receipt = f
            .hub
            .submit(
                &owner,
                turn(
                    &f,
                    task,
                    serde_json::json!({"text": "hard", "model": "opus", "effort": "high"}),
                ),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        assert_eq!(models(&f), some("opus", Some("high")));
        finish_turn(&f, task, "talk-session").await;
        assert_writer(&f, None);
        f.close().await;
    }

    #[tokio::test]
    async fn a_conversation_carried_on_in_work_becomes_a_task_in_its_session() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let receipt = f.hub.submit(&owner, conversation(&f)).await;
        let Some(CommandResult::TaskCreated { task_id: task, .. }) = receipt.result else {
            panic!("{receipt:?}")
        };
        let created: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        assert!(created.conversation);
        assert_eq!(created.mode, TaskMode::Ask);
        // A conversation needs no writer, so it goes on beside work.
        let writer = f.task(Some(TaskMode::Work)).await.current_run.unwrap();
        let spec = finish_turn(&f, task, "talk-session").await;
        assert_eq!(spec.mode, TaskMode::Ask);
        assert_writer(&f, Some(writer));
        f.hub.finish_run(writer, RunState::Completed, None).unwrap();
        // A plain Ask task can't be carried on in Work.
        let ask = f.task(Some(TaskMode::Ask)).await.task_id;
        finish_turn(&f, ask, "ask-session").await;
        let receipt = f
            .hub
            .submit(
                &owner,
                turn(
                    &f,
                    ask,
                    serde_json::json!({"text": "do it", "mode": "work"}),
                ),
            )
            .await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        let receipt = f
            .hub
            .submit(
                &owner,
                turn(
                    &f,
                    task,
                    serde_json::json!({"text": "Make that change", "mode": "work"}),
                ),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let carried: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        assert!(!carried.conversation);
        assert_eq!(carried.mode, TaskMode::Work);
        let spec = f.dispatched(carried.current_run.unwrap()).await;
        assert_eq!(spec.mode, TaskMode::Work);
        assert_eq!(spec.resume_from_mode, Some(TaskMode::Ask));
        assert_eq!(spec.resume_session.as_deref(), Some("talk-session"));
        assert_writer(&f, carried.current_run);
        f.close().await;
    }

    #[tokio::test]
    async fn a_conversation_is_with_a_coding_agent() {
        let f = Fixture::new().await;
        let mut create = conversation(&f);
        let Command::CreateTask { kind, mode, .. } = &mut create.body else {
            unreachable!()
        };
        (*kind, *mode) = (TaskKind::ComputerUse, Some(TaskMode::Work));
        let receipt = f.hub.submit(&Caller::local_owner(), create).await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        f.close().await;
    }

    #[tokio::test]
    async fn only_a_conversation_is_held_to_what_the_person_chose() {
        use protocol::access::{AccessBlocked, ConversationAccess};
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let (mine, other) = (f.project, ProjectId::new());
        f.hub
            .write(|tx| {
                let mut project: Project = tx.read().require("project", mine)?;
                project.project_id = other;
                tx.put("project", other, &project)
            })
            .unwrap();
        let chosen = ConversationAccess {
            read: vec![mine],
            write: vec![other],
            whole_read: false,
            whole_write: false,
            internet: false,
            local_network: false,
        };
        // A task follows its mode.
        let mut create = f.create(Some(TaskMode::Work));
        let Command::CreateTask { access, .. } = &mut create.body else {
            unreachable!()
        };
        *access = Some(chosen.clone());
        let receipt = f.hub.submit(&owner, create).await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        let task = f.task(Some(TaskMode::Work)).await.task_id;
        let set = |task_id, access: ConversationAccess| {
            CommandEnvelope::new(Command::SetConversationAccess { task_id, access })
        };
        let receipt = f.hub.submit(&owner, set(task, chosen.clone())).await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        // A conversation keeps what it may reach, as one list each.
        let mut create = conversation(&f);
        let Command::CreateTask { access, .. } = &mut create.body else {
            unreachable!()
        };
        *access = Some(chosen.clone());
        let receipt = f.hub.submit(&owner, create).await;
        let Some(CommandResult::TaskCreated { task_id: talk, .. }) = receipt.result else {
            panic!("{receipt:?}")
        };
        let stored = |f: &Fixture| f.hub.read(|r| r.require::<Task>("task", talk)).unwrap();
        let mut both = vec![mine, other];
        both.sort();
        assert_eq!(stored(&f).access.unwrap().read, both);
        // Mid-turn, a change lets what was kept from it be tried again.
        f.hub
            .write(|tx| {
                let mut t: Task = tx.read().require("task", talk)?;
                t.access_blocked = Some(Box::new(AccessBlocked {
                    path: Some("/srv/other".into()),
                    host: None,
                    local: false,
                    write: true,
                    project_id: Some(other),
                    at: ports::Clock::now(&*f.clock),
                }));
                tx.put("task", talk, &t)
            })
            .unwrap();
        let whole = ConversationAccess {
            whole_write: true,
            ..chosen
        };
        let receipt = f.hub.submit(&owner, set(talk, whole)).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let now = stored(&f);
        assert!(now.access.as_ref().unwrap().whole_read);
        assert_eq!(now.access_blocked, None);
        f.close().await;
    }

    #[tokio::test]
    async fn a_read_only_turn_answers_in_the_tasks_session_and_leaves_it_as_it_was() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let task = f.task(Some(TaskMode::Work)).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        f.hub
            .harness_event(
                run,
                HarnessEvent::SessionStarted {
                    native_session: "work-session".into(),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await
            .unwrap();
        let ask = |read_only: bool, mode: Option<TaskMode>| {
            let current: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
            let mut envelope = CommandEnvelope::new(Command::ResumeTask {
                task_id: task.task_id,
                mode,
                text: Some("What happened?".into()),
                bundle: None,
                carried_plan: None,
                read_only,
                model: None,
                effort: None,
            });
            envelope.expected_revision = Some(current.revision);
            envelope
        };
        // Not while the work is still going.
        let busy = f.hub.submit(&owner, ask(true, None)).await;
        assert_eq!(
            busy.error.as_ref().unwrap().code,
            ErrorCode::Conflict,
            "{busy:?}"
        );
        f.hub.finish_run(run, RunState::Failed, None).unwrap();
        // Another task now holds the project's writer; asking needs none.
        let writer = f.task(Some(TaskMode::Work)).await.current_run.unwrap();
        let continued = f.hub.submit(&owner, ask(false, None)).await;
        assert_eq!(continued.error.unwrap().code, ErrorCode::Conflict);
        let changing = f.hub.submit(&owner, ask(true, Some(TaskMode::Work))).await;
        assert_eq!(changing.error.unwrap().code, ErrorCode::InvalidRequest);
        let receipt = f.hub.submit(&owner, ask(true, None)).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let asking: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        let side = asking.current_run.unwrap();
        let spec = f.dispatched(side).await;
        assert_eq!(spec.mode, TaskMode::Ask);
        assert_eq!(spec.resume_session.as_deref(), Some("work-session"));
        assert_eq!(spec.resume_from_mode, Some(TaskMode::Work));
        assert_eq!(spec.model, task.model);
        assert!(spec.bridge.is_none());
        assert_eq!(asking.mode, TaskMode::Work);
        let recorded: Run = f.hub.read(|r| r.require("run", side)).unwrap();
        assert_eq!(recorded.mode, Some(TaskMode::Ask));
        assert!(!recorded.holds_writer);
        assert_writer(&f, Some(writer));
        // A completed native turn has acknowledged its resumed session.
        f.hub
            .harness_event(
                side,
                HarnessEvent::SessionStarted {
                    native_session: "work-session".into(),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await
            .unwrap();
        // What it answered doesn't change how the work ended.
        f.hub.finish_run(side, RunState::Completed, None).unwrap();
        let after: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(after.state, TaskState::Failed);
        assert_eq!(after.mode, TaskMode::Work);
        assert_writer(&f, Some(writer));
        // Continuing the work afterwards leaves the read-only turn behind.
        f.hub.finish_run(writer, RunState::Completed, None).unwrap();
        let receipt = f.hub.submit(&owner, ask(false, None)).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let resumed: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        let spec = f.dispatched(resumed.current_run.unwrap()).await;
        assert_eq!(spec.mode, TaskMode::Work);
        assert_eq!(spec.resume_from_mode, Some(TaskMode::Ask));
        assert_writer(&f, resumed.current_run);
        f.close().await;
    }

    #[tokio::test]
    async fn a_stopped_read_only_turn_puts_the_task_back_and_needs_a_session() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let ask = |task: &Task| {
            let current: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
            let mut envelope = CommandEnvelope::new(Command::ResumeTask {
                task_id: task.task_id,
                mode: None,
                text: Some("What happened?".into()),
                bundle: None,
                carried_plan: None,
                read_only: true,
                model: None,
                effort: None,
            });
            envelope.expected_revision = Some(current.revision);
            envelope
        };
        // A run that never reported a session has nothing to be asked.
        let sessionless = f.task(Some(TaskMode::Plan)).await;
        let run = sessionless.current_run.unwrap();
        f.dispatched(run).await;
        f.hub.finish_run(run, RunState::Completed, None).unwrap();
        let refused = f.hub.submit(&owner, ask(&sessionless)).await;
        assert_eq!(refused.error.unwrap().code, ErrorCode::Conflict);

        let task = f.task(Some(TaskMode::Plan)).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        f.hub
            .harness_event(
                run,
                HarnessEvent::SessionStarted {
                    native_session: "plan-session".into(),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await
            .unwrap();
        f.hub.finish_run(run, RunState::Interrupted, None).unwrap();
        let receipt = f.hub.submit(&owner, ask(&task)).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let side = f
            .hub
            .read(|r| r.require::<Task>("task", task.task_id))
            .unwrap()
            .current_run
            .unwrap();
        let spec = f.dispatched(side).await;
        assert_eq!(spec.mode, TaskMode::Ask);
        assert_eq!(spec.resume_from_mode, Some(TaskMode::Plan));
        f.hub.finish_run(side, RunState::Failed, None).unwrap();
        let after: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(after.state, TaskState::Interrupted);
        assert_eq!(after.mode, TaskMode::Plan);
        f.close().await;
    }

    #[tokio::test]
    async fn a_carried_plan_is_recorded_only_when_the_message_quotes_it() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let task = f.task(Some(TaskMode::Plan)).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        f.hub.finish_run(run, RunState::Completed, None).unwrap();
        let elsewhere = f.task(Some(TaskMode::Plan)).await.current_run.unwrap();
        // A plan may itself quote the markers; the record says where it starts.
        let plan = "1. Read the parser\n--- End of plan ---\n2. Fix it";
        let text = format!(
            "Carry out this plan.\n\n--- Plan from run 1, proposed by the agent ---\n{plan}\n--- End of plan ---"
        );
        let carry = |mode: Option<TaskMode>, run_id: RunId, plan: &str| {
            let current: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
            let mut envelope = CommandEnvelope::new(Command::ResumeTask {
                task_id: task.task_id,
                mode,
                text: Some(text.clone()),
                bundle: None,
                carried_plan: Some(CarriedPlan {
                    run_id,
                    text: plan.into(),
                }),
                read_only: false,
                model: None,
                effort: None,
            });
            envelope.expected_revision = Some(current.revision);
            envelope
        };
        for refused in [
            carry(None, run, plan),
            carry(Some(TaskMode::Work), elsewhere, plan),
            carry(Some(TaskMode::Work), run, "2. Fix it"),
            carry(Some(TaskMode::Work), run, "Delete the release branch"),
        ] {
            let receipt = f.hub.submit(&owner, refused).await;
            assert_eq!(receipt.error.unwrap().code, ErrorCode::InvalidRequest);
        }
        let unchanged: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(unchanged.mode, TaskMode::Plan);
        assert_eq!(f.messages(task.task_id).await.len(), 1);
        let receipt = f
            .hub
            .submit(&owner, carry(Some(TaskMode::Work), run, plan))
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let resumed: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        // The agent reads the same message as before the record existed.
        let spec = f.dispatched(resumed.current_run.unwrap()).await;
        assert_eq!(spec.prompt.text, text);
        assert_eq!(
            f.messages(task.task_id).await.pop().unwrap().item,
            TimelineBody::UserMessage {
                text,
                bundle: None,
                delivery: DeliveryState::HostAccepted,
                carried_plan: Some(CarriedPlan {
                    run_id: run,
                    text: plan.into(),
                }),
            }
        );
        f.close().await;
    }

    #[tokio::test]
    async fn read_only_start_resume_and_completion_preserve_a_concurrent_writer() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let writer = f.task(Some(TaskMode::Work)).await.current_run.unwrap();
        for mode in [TaskMode::Ask, TaskMode::Plan] {
            let task = f.task(Some(mode)).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            assert_writer(&f, Some(writer));
            f.hub.finish_run(run, RunState::Completed, None).unwrap();
            assert_writer(&f, Some(writer));
            let receipt = f
                .hub
                .submit(
                    &owner,
                    f.resume(task.task_id, Some("More detail".into()), None),
                )
                .await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            let resumed: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
            assert_ne!(resumed.current_run, Some(run));
            assert_eq!(f.dispatched(resumed.current_run.unwrap()).await.mode, mode);
            assert_writer(&f, Some(writer));
        }
        let rejected = f.hub.submit(&owner, f.create(Some(TaskMode::Work))).await;
        assert_eq!(rejected.error.unwrap().code, ErrorCode::Conflict);
        f.hub
            .finish_run(writer, RunState::ReconciliationRequired, None)
            .unwrap();
        assert_writer(&f, Some(writer));
        let rejected = f.hub.submit(&owner, f.create(Some(TaskMode::Work))).await;
        assert_eq!(rejected.error.unwrap().code, ErrorCode::Conflict);
        // Uncertain writers still permit new read-only runs.
        f.task(Some(TaskMode::Ask)).await;
        f.hub.finish_run(writer, RunState::Completed, None).unwrap();
        assert_writer(&f, None);
        let next = f.task(Some(TaskMode::Work)).await.current_run.unwrap();
        assert_writer(&f, Some(next));
        f.close().await;
    }

    #[tokio::test]
    async fn read_only_uncertainty_and_reconciliation_never_acquire_or_release_a_writer() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        for mode in [TaskMode::Ask, TaskMode::Plan] {
            let task = f.task(Some(mode)).await;
            f.hub
                .finish_run(
                    task.current_run.unwrap(),
                    RunState::ReconciliationRequired,
                    None,
                )
                .unwrap();
            assert_writer(&f, None);
            let writer = f.task(Some(TaskMode::Work)).await.current_run.unwrap();
            let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
            let mut command = CommandEnvelope::new(Command::ReconcileTask {
                task_id: task.task_id,
                resolution: Reconciliation::MarkCompleted,
            });
            command.expected_revision = Some(task.revision);
            let receipt = f.hub.submit(&owner, command).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            assert_writer(&f, Some(writer));
            f.hub.finish_run(writer, RunState::Completed, None).unwrap();
        }
        f.close().await;
    }

    #[tokio::test]
    async fn read_only_history_survives_project_removal_but_active_runs_prevent_removal() {
        for mode in [TaskMode::Ask, TaskMode::Plan] {
            let mut f = Fixture::new().await;
            let run = f.task(Some(mode)).await.current_run.unwrap();
            let remove = || {
                CommandEnvelope::new(Command::RemoveProject {
                    project_id: f.project,
                })
            };
            assert_eq!(
                f.hub
                    .submit(&Caller::local_owner(), remove())
                    .await
                    .error
                    .unwrap()
                    .code,
                ErrorCode::Conflict
            );
            f.hub
                .finish_run(run, RunState::ReconciliationRequired, None)
                .unwrap();
            assert_eq!(
                f.hub
                    .submit(&Caller::local_owner(), remove())
                    .await
                    .error
                    .unwrap()
                    .code,
                ErrorCode::Conflict
            );
            f.hub.finish_run(run, RunState::Completed, None).unwrap();
            assert_eq!(
                f.hub.submit(&Caller::local_owner(), remove()).await.status,
                ReceiptStatus::Accepted
            );
            f.hub.shutdown().await.unwrap();
            drop(f.hub);
            f.hub = CompanionHub::builder()
                .data_dir(&f.path)
                .clock(f.clock.clone())
                .harness(f.harness.clone())
                .build()
                .await
                .unwrap();
            let retained: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            assert_eq!(retained.state, RunState::Completed);
            assert!(!retained.holds_writer);
            f.close().await;
        }
    }

    #[tokio::test]
    async fn restart_read_only_runs_preserves_work_ownership_and_repairs_legacy_slots() {
        for mode in [TaskMode::Ask, TaskMode::Plan] {
            for with_writer in [false, true] {
                let mut f = Fixture::new().await;
                let reader = f.task(Some(mode)).await.current_run.unwrap();
                let writer = if with_writer {
                    Some(f.task(Some(TaskMode::Work)).await.current_run.unwrap())
                } else {
                    None
                };
                // Include old persisted rows that incorrectly held the writer.
                f.hub
                    .write(|tx| {
                        let mut run: Run = tx.read().require("run", reader)?;
                        run.holds_writer = true;
                        tx.put("run", reader, &run)?;
                        if !with_writer {
                            let mut project: Project = tx.read().require("project", f.project)?;
                            project.active_writer = Some(reader);
                            tx.put("project", f.project, &project)?;
                            let mut queue: QueueView = tx.read().require("queue", f.project)?;
                            queue.writer = Some(reader);
                            tx.put("queue", f.project, &queue)?;
                        }
                        Ok(())
                    })
                    .unwrap();
                f.hub.shutdown().await.unwrap();
                drop(f.hub);
                f.hub = CompanionHub::builder()
                    .data_dir(&f.path)
                    .clock(f.clock.clone())
                    .harness(f.harness.clone())
                    .build()
                    .await
                    .unwrap();
                assert_writer(&f, writer);
                let run: Run = f.hub.read(|r| r.require("run", reader)).unwrap();
                assert_eq!(run.state, RunState::ReconciliationRequired);
                assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
                f.close().await;
            }
        }
    }

    #[tokio::test]
    async fn queue_admits_read_only_items_and_continues_without_bypassing_authorization_or_order() {
        for with_writer in [false, true] {
            let f = Fixture::new().await;
            let owner = Caller::local_owner();
            let writer = if with_writer {
                Some(f.task(Some(TaskMode::Work)).await.current_run.unwrap())
            } else {
                None
            };
            let mut tasks = Vec::new();
            for mode in [TaskMode::Ask, TaskMode::Plan, TaskMode::Work, TaskMode::Ask] {
                let mut command = f.create(Some(mode));
                if let Command::CreateTask {
                    placement,
                    launch_policy,
                    ..
                } = &mut command.body
                {
                    *launch_policy = Some(LaunchPolicy::BypassPermissions);
                    *placement = TaskPlacement::Queue {
                        position: None,
                        authorization: QueueAuthorization::AskBeforeStart,
                    };
                }
                let receipt = f.hub.submit(&owner, command).await;
                let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
                    panic!("{receipt:?}")
                };
                tasks.push(task_id);
            }
            // Authorize the tail first: an unapproved head cannot be skipped.
            let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
            for item in queue.items.iter().rev().take(3) {
                let current: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
                let mut command = CommandEnvelope::new(Command::QueueSetAuthorization {
                    item_id: item.item_id,
                    authorization: QueueAuthorization::AutoStart,
                });
                command.expected_revision = Some(current.revision);
                let receipt = f.hub.submit(&owner, command).await;
                assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            }
            f.hub
                .write(|tx| {
                    let mut q: QueueView = tx.read().require("queue", f.project)?;
                    q.paused = false;
                    tx.put("queue", f.project, &q)
                })
                .unwrap();
            f.hub.advance_queues().await.unwrap();
            let q: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
            assert!(q.items.iter().all(QueueItem::is_pending));
            let mut command = CommandEnvelope::new(Command::QueueSetAuthorization {
                item_id: q.items[0].item_id,
                authorization: QueueAuthorization::AutoStart,
            });
            command.expected_revision = Some(q.revision);
            assert_eq!(
                f.hub.submit(&owner, command).await.status,
                ReceiptStatus::Accepted
            );
            f.hub.advance_queues().await.unwrap();
            let q: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
            assert!(!q.items[0].is_pending());
            assert!(!q.items[1].is_pending());
            if with_writer {
                assert_writer(&f, writer);
                assert!(q.items[2].is_pending());
                assert!(q.items[3].is_pending());
            } else {
                let work: Task = f.hub.read(|r| r.require("task", tasks[2])).unwrap();
                assert_writer(&f, work.current_run);
                assert!(q.items.iter().all(|item| !item.is_pending()));
            }
            for item in q.items.iter().filter(|item| !item.is_pending()) {
                let task: Task = f.hub.read(|r| r.require("task", item.task_id)).unwrap();
                assert_eq!(task.launch_policy, Some(LaunchPolicy::BypassPermissions));
                assert_eq!(
                    f.dispatched(task.current_run.unwrap()).await.launch_policy,
                    task.launch_policy
                );
                let permit: QueuePermit = f
                    .hub
                    .read(|r| r.require("queue_permit", item.item_id))
                    .unwrap();
                assert_eq!(permit.remaining, 0);
            }
            f.close().await;
        }
    }

    /// A finished task, its run and the message the person queues for it.
    async fn continuation(
        f: &Fixture,
        authorization: QueueAuthorization,
    ) -> (Task, RunId, CommandReceipt) {
        let task = f.task(Some(TaskMode::Ask)).await;
        let first = task.current_run.unwrap();
        f.dispatched(first).await;
        finish(f, first).await;
        let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(task.state, TaskState::Completed);
        let mut command = CommandEnvelope::new(Command::QueueContinuation {
            task_id: task.task_id,
            text: "Q: Which layout?\nA: The second".into(),
            bundle: None,
            authorization,
        });
        command.expected_revision = Some(task.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), command).await;
        (task, first, receipt)
    }

    #[tokio::test]
    async fn a_queued_continuation_starts_the_tasks_next_turn_with_its_message() {
        let f = Fixture::new().await;
        let (task, first, receipt) = continuation(&f, QueueAuthorization::AutoStart).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        // The next turn is a new run of the same task, started in the
        // background as Next up reaches it.
        let run = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
                if let Some(run) = task.current_run.filter(|run| *run != first) {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the continuation started");
        let spec = f.dispatched(run).await;
        assert_eq!(spec.prompt.text, "Q: Which layout?\nA: The second");
        let q: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        assert!(q.items.iter().all(|i| !i.is_pending()));
        let waiting: Option<String> = f
            .hub
            .read(|r| r.get(queue_completion::CONTINUATION, task.task_id))
            .unwrap();
        assert_eq!(waiting, None);
        // A second one waits only once the first has gone.
        let mut again = CommandEnvelope::new(Command::QueueContinuation {
            task_id: task.task_id,
            text: "more".into(),
            bundle: None,
            authorization: QueueAuthorization::AutoStart,
        });
        again.expected_revision = Some(task.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), again).await;
        assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
        f.close().await;
    }

    /// A finished task with markup alone waiting in Next up as its next message.
    async fn markup_continuation(
        f: &Fixture,
        authorization: QueueAuthorization,
    ) -> (Task, RunId, Box<AnnotationBundle>) {
        let task = f.task(Some(TaskMode::Ask)).await;
        let first = task.current_run.unwrap();
        f.dispatched(first).await;
        finish(f, first).await;
        let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        let bundle = f.bundle(task.task_id).await;
        let mut command = CommandEnvelope::new(Command::QueueContinuation {
            task_id: task.task_id,
            text: String::new(),
            bundle: Some(bundle.clone()),
            authorization,
        });
        command.expected_revision = Some(task.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), command).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        (task, first, bundle)
    }

    #[tokio::test]
    async fn markup_waits_in_next_up_and_reaches_the_agent_when_its_turn_comes() {
        let f = Fixture::new().await;
        let (task, first, bundle) = markup_continuation(&f, QueueAuthorization::AutoStart).await;
        assert!(f.messages(task.task_id).await.iter().any(|m| matches!(
            &m.item,
            TimelineBody::UserMessage { bundle: Some(b), .. } if b == &bundle
        )));
        let run = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
                if let Some(run) = task.current_run.filter(|run| *run != first) {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the continuation started");
        let spec = f.dispatched(run).await;
        assert_eq!(spec.prompt.images.len(), 1);
        assert!(
            spec.prompt.text.contains("Inspect this region"),
            "{}",
            spec.prompt.text
        );
        let waiting: Option<Box<AnnotationBundle>> = f
            .hub
            .read(|r| r.get(queue_completion::CONTINUATION_BUNDLE, task.task_id))
            .unwrap();
        assert_eq!(waiting, None);
        f.close().await;
    }

    #[tokio::test]
    async fn a_message_sent_to_the_task_takes_its_waiting_markup_along() {
        let f = Fixture::new().await;
        let (task, _, bundle) = markup_continuation(&f, QueueAuthorization::AskBeforeStart).await;
        let q: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        let item = q.items.iter().find(|i| i.task_id == task.task_id).unwrap();
        assert_eq!(
            item.request.as_ref().and_then(|r| r.bundle.as_deref()),
            Some(bundle.as_ref())
        );
        // One turn takes one piece of markup; the waiting one stays.
        let other = f.bundle(task.task_id).await;
        let refused = f
            .hub
            .submit(
                &Caller::local_owner(),
                f.resume(task.task_id, None, Some(other)),
            )
            .await;
        assert_eq!(refused.error.unwrap().code, ErrorCode::Conflict);
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                f.resume(task.task_id, Some("Also make it blue".into()), None),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        let spec = f.dispatched(task.current_run.unwrap()).await;
        assert_eq!(spec.prompt.images.len(), 1);
        assert!(
            spec.prompt.text.starts_with("Also make it blue"),
            "{}",
            spec.prompt.text
        );
        assert!(spec.prompt.text.contains("Inspect this region"));
        f.close().await;
    }

    #[tokio::test]
    async fn removing_waiting_markup_from_next_up_lets_it_go() {
        let f = Fixture::new().await;
        let (task, _, _) = markup_continuation(&f, QueueAuthorization::AskBeforeStart).await;
        let q: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        let item = q.items.iter().find(|i| i.is_pending()).unwrap();
        let mut remove = CommandEnvelope::new(Command::QueueRemove {
            item_id: item.item_id,
        });
        remove.expected_revision = Some(q.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), remove).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let waiting: Option<Box<AnnotationBundle>> = f
            .hub
            .read(|r| r.get(queue_completion::CONTINUATION_BUNDLE, task.task_id))
            .unwrap();
        assert_eq!(waiting, None);
        // The next message to the task goes without it.
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                f.resume(task.task_id, Some("Start over".into()), None),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        let spec = f.dispatched(task.current_run.unwrap()).await;
        assert!(spec.prompt.images.is_empty());
        assert_eq!(spec.prompt.text, "Start over");
        f.close().await;
    }

    #[tokio::test]
    async fn a_message_sent_to_the_task_carries_its_waiting_continuation_along() {
        let f = Fixture::new().await;
        // Another task waits ahead of it; a message from the person still
        // starts this one now.
        let mut ahead = f.create(Some(TaskMode::Ask));
        if let Command::CreateTask { placement, .. } = &mut ahead.body {
            *placement = TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            };
        }
        let receipt = f.hub.submit(&Caller::local_owner(), ahead).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let (task, _, receipt) = continuation(&f, QueueAuthorization::AskBeforeStart).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let q: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        let item = q.items.iter().find(|i| i.task_id == task.task_id).unwrap();
        assert_eq!(
            item.request.as_ref().map(|r| r.prompt.as_str()),
            Some("Q: Which layout?\nA: The second")
        );
        let receipt = f
            .hub
            .submit(
                &Caller::local_owner(),
                f.resume(task.task_id, Some("Also make it blue".into()), None),
            )
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let task: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        let run = task.current_run.unwrap();
        let spec = f.dispatched(run).await;
        assert_eq!(
            spec.prompt.text,
            "Q: Which layout?\nA: The second\n\nAlso make it blue"
        );
        let q: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        assert!(
            !q.items
                .iter()
                .any(|i| i.task_id == task.task_id && i.is_pending())
        );
        // Both messages are settled with the turn that took them.
        let messages: Vec<u64> = timeline(&f, task.task_id)
            .into_iter()
            .filter(|i| matches!(i.item, TimelineBody::UserMessage { .. }))
            .map(|i| i.seq)
            .skip(1)
            .collect();
        assert_eq!(messages.len(), 2);
        f.hub
            .harness_event(run, HarnessEvent::TurnStarted)
            .await
            .unwrap();
        let settled: Vec<(u64, DeliveryState)> = timeline(&f, task.task_id)
            .into_iter()
            .filter_map(|i| match i.item {
                TimelineBody::Delivery {
                    message_seq, state, ..
                } if messages.contains(&message_seq) => Some((message_seq, state)),
                _ => None,
            })
            .collect();
        assert_eq!(
            settled,
            messages
                .iter()
                .map(|m| (*m, DeliveryState::NativeDelivered))
                .collect::<Vec<_>>()
        );
        f.close().await;
    }

    #[tokio::test]
    async fn removing_a_continuation_withdraws_only_the_message() {
        let f = Fixture::new().await;
        let (task, _, receipt) = continuation(&f, QueueAuthorization::AskBeforeStart).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let q: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        let item = q.items.iter().find(|i| i.is_pending()).unwrap();
        let mut remove = CommandEnvelope::new(Command::QueueRemove {
            item_id: item.item_id,
        });
        remove.expected_revision = Some(q.revision);
        let receipt = f.hub.submit(&Caller::local_owner(), remove).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let after: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(after.state, TaskState::Completed);
        assert_eq!(after.current_run, task.current_run);
        let waiting: Option<String> = f
            .hub
            .read(|r| r.get(queue_completion::CONTINUATION, task.task_id))
            .unwrap();
        assert_eq!(waiting, None);
        let message = timeline(&f, task.task_id)
            .into_iter()
            .rfind(|i| matches!(i.item, TimelineBody::UserMessage { .. }))
            .unwrap()
            .seq;
        assert!(timeline(&f, task.task_id).iter().any(|i| matches!(
            i.item,
            TimelineBody::Delivery { message_seq, state: DeliveryState::Cancelled, .. }
                if message_seq == message
        )));
        f.close().await;
    }

    #[tokio::test]
    async fn a_refused_queued_start_does_not_strand_the_starts_admitted_with_it() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let mut command = f.create(Some(TaskMode::Ask));
            if let Command::CreateTask { placement, .. } = &mut command.body {
                *placement = TaskPlacement::Queue {
                    position: None,
                    authorization: QueueAuthorization::AskBeforeStart,
                };
            }
            let receipt = f.hub.submit(&owner, command).await;
            let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
                panic!("{receipt:?}")
            };
            tasks.push(task_id);
        }
        let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        for item in &queue.items {
            let current: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
            let mut command = CommandEnvelope::new(Command::QueueSetAuthorization {
                item_id: item.item_id,
                authorization: QueueAuthorization::AutoStart,
            });
            command.expected_revision = Some(current.revision);
            let receipt = f.hub.submit(&owner, command).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        }
        *f.harness.refuse_start.lock().unwrap() = Some("Sign in to the agent first.".into());
        f.hub
            .write(|tx| {
                let mut q: QueueView = tx.read().require("queue", f.project)?;
                q.paused = false;
                tx.put("queue", f.project, &q)
            })
            .unwrap();
        // Both read-only items start in one pass; the agent refuses the first.
        let refused = f.hub.advance_queues().await.unwrap_err();
        assert_eq!(refused.message, "Sign in to the agent first.");
        let first: Task = f.hub.read(|r| r.require("task", tasks[0])).unwrap();
        assert_eq!(first.state, TaskState::Failed);
        let second: Task = f.hub.read(|r| r.require("task", tasks[1])).unwrap();
        let run = second.current_run.expect("second item admitted");
        f.dispatched(run).await;
        assert_eq!(f.harness.starts.load(Ordering::SeqCst), 2);
        let run: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_eq!(run.state, RunState::Dispatching);
        f.close().await;
    }

    #[tokio::test]
    async fn a_dependent_starts_once_the_prerequisites_helpers_have_ended() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let mut command = f.create(Some(TaskMode::Ask));
            if let Command::CreateTask { placement, .. } = &mut command.body {
                *placement = TaskPlacement::Queue {
                    position: None,
                    authorization: QueueAuthorization::AskBeforeStart,
                };
            }
            let receipt = f.hub.submit(&owner, command).await;
            let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
                panic!("{receipt:?}")
            };
            tasks.push(task_id);
        }
        let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        let (first, second) = (queue.items[0].item_id, queue.items[1].item_id);
        let mut depend = CommandEnvelope::new(Command::QueueSetDependencies {
            item_id: second,
            depends_on: vec![first],
        });
        depend.expected_revision = Some(queue.revision);
        assert_eq!(
            f.hub.submit(&owner, depend).await.status,
            ReceiptStatus::Accepted
        );
        for item in [first, second] {
            let current: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
            let mut command = CommandEnvelope::new(Command::QueueSetAuthorization {
                item_id: item,
                authorization: QueueAuthorization::AutoStart,
            });
            command.expected_revision = Some(current.revision);
            let receipt = f.hub.submit(&owner, command).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        }
        f.hub
            .write(|tx| {
                let mut q: QueueView = tx.read().require("queue", f.project)?;
                q.paused = false;
                tx.put("queue", f.project, &q)
            })
            .unwrap();
        f.hub.advance_queues().await.unwrap();
        let run = f
            .hub
            .read(|r| r.require::<Task>("task", tasks[0]))
            .unwrap()
            .current_run
            .unwrap();
        f.dispatched(run).await;
        let mut subscription = f.hub.subscribe(&owner, None).await.unwrap();
        // The prerequisite's turn ends while a helper of its still runs.
        for event in [
            HarnessEvent::Child {
                lifetime: Some(ChildLifetime::InProcess),
                native_ref: "helper".into(),
                label: "Helper".into(),
                state: ChildState::Active,
            },
            HarnessEvent::ParentEnded {
                exit: ports::RunExit::Completed,
            },
        ] {
            f.hub.harness_event(run, event).await.unwrap();
        }
        // Next up looks again when the turn completes, and holds the dependent.
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let crate::api::Outbound::Event(event) = subscription.live.recv().await.unwrap()
                    && matches!(event.event, Event::QueueChanged { .. })
                {
                    return;
                }
            }
        })
        .await
        .expect("Next up did not look again");
        let waiting: Task = f.hub.read(|r| r.require("task", tasks[1])).unwrap();
        assert!(waiting.current_run.is_none(), "a live helper holds it back");
        // The agent process ends, and its in-process helper with it.
        f.hub
            .harness_event(
                run,
                HarnessEvent::Exited {
                    exit: ports::RunExit::Completed,
                    outcome: None,
                },
            )
            .await
            .unwrap();
        let started = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(run) = f
                    .hub
                    .read(|r| r.require::<Task>("task", tasks[1]))
                    .unwrap()
                    .current_run
                {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the dependent never started");
        f.dispatched(started).await;
        f.close().await;
    }

    #[tokio::test]
    async fn a_failed_work_run_pauses_next_up_and_names_the_task() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let run = f.task(Some(TaskMode::Work)).await.current_run.unwrap();
        f.dispatched(run).await;
        let mut command = f.create(Some(TaskMode::Work));
        if let Command::CreateTask { placement, .. } = &mut command.body {
            *placement = TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            };
        }
        let receipt = f.hub.submit(&owner, command).await;
        let Some(CommandResult::TaskCreated {
            task_id: waiting, ..
        }) = receipt.result
        else {
            panic!("{receipt:?}")
        };
        let resume = || async {
            let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
            let mut command = CommandEnvelope::new(Command::QueueResume {
                project_id: f.project,
            });
            command.expected_revision = Some(queue.revision);
            let receipt = f.hub.submit(&owner, command).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        };
        resume().await;
        f.hub
            .harness_event(
                run,
                HarnessEvent::Exited {
                    exit: ports::RunExit::Failed {
                        reason: "The model stopped responding.".into(),
                    },
                    outcome: None,
                },
            )
            .await
            .unwrap();
        assert_writer(&f, None);
        let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        assert!(queue.paused);
        assert_eq!(
            queue.pause_reason.as_deref(),
            Some(
                "Next up paused because “Inspect” failed, so nothing starts by itself. Check it, then resume when you’re ready."
            )
        );
        // Another look starts nothing while it is paused.
        f.hub.advance_queues().await.unwrap();
        let task: Task = f.hub.read(|r| r.require("task", waiting)).unwrap();
        assert!(task.current_run.is_none());
        // Resuming works as it does after any pause: the go-ahead given
        // when the task was queued still starts it.
        resume().await;
        let started = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(run) = f
                    .hub
                    .read(|r| r.require::<Task>("task", waiting))
                    .unwrap()
                    .current_run
                {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the waiting task did not start on resume");
        f.dispatched(started).await;
        f.close().await;
    }

    /// Whether the last run the phone was sent says its end is unconfirmed.
    fn published(f: &Fixture, id: RunId) -> bool {
        f.hub
            .read(|r| r.journal(0))
            .unwrap()
            .events
            .into_iter()
            .rev()
            .find_map(|e| match e.event {
                Event::RunUpserted { run } if run.run_id == id => Some(run.cleanup_pending),
                _ => None,
            })
            .unwrap()
    }

    /// A Work turn has left a helper running, and Force stop is waiting for
    /// the agent's process to end when Ophio stops following its events.
    /// Nothing has said the helper's end is unconfirmed before that. Force
    /// stop goes on waiting until `terminate_gate` lets it, and the returned
    /// handles finish once it has.
    async fn events_end_during_force_stop(
        f: &Fixture,
    ) -> (
        TaskId,
        RunId,
        tokio::task::JoinHandle<CommandReceipt>,
        tokio::task::JoinHandle<()>,
    ) {
        let (task, run) = helper_outlives_turn(f).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            while !f
                .hub
                .read(|r| r.require::<Run>("run", run))
                .unwrap()
                .force_stoppable
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        // The turn's end recorded the helpers it left as unconfirmed.
        assert!(pending(f, run));
        f.harness
            .controls
            .hold_terminate
            .store(true, Ordering::SeqCst);
        let current: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        let mut request = CommandEnvelope::new(Command::StopTask {
            task_id: task,
            force: true,
        });
        request.expected_revision = Some(current.revision);
        let hub = f.hub.clone();
        let stopping =
            tokio::spawn(async move { hub.submit(&Caller::local_owner(), request).await });
        f.harness
            .controls
            .terminate_entered
            .acquire()
            .await
            .unwrap()
            .forget();
        // Shutting down waits for Force stop, but stops following the
        // agent's events first.
        let hub = f.hub.clone();
        let shutting = tokio::spawn(async move { hub.shutdown().await.unwrap() });
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let run: Run = f.hub.read(|r| r.require("run", run)).unwrap();
                if run.state == RunState::ReconciliationRequired || !run.holds_writer {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the lost events were not recorded");
        (task, run, stopping, shutting)
    }

    #[tokio::test]
    async fn events_lost_while_force_stop_waits_keep_the_project_through_a_restart() {
        let mut f = Fixture::new().await;
        let (task, run, stopping, shutting) = events_end_during_force_stop(&f).await;
        let open: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_eq!(open.state, RunState::ReconciliationRequired);
        assert!(open.holds_writer);
        assert_writer(&f, Some(run));
        // The process's end stays unconfirmed.
        f.harness.controls.terminate_gate.add_permits(1);
        let receipt = stopping.await.unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        shutting.await.unwrap();
        assert!(pending(&f, run));
        assert_writer(&f, Some(run));
        drop(f.hub);
        f.hub = CompanionHub::builder()
            .data_dir(&f.path)
            .clock(f.clock.clone())
            .harness(f.harness.clone())
            .bridge(crate::bridge::BridgeConfig::new(
                "synthetic-bridge",
                "synthetic-socket",
            ))
            .build()
            .await
            .unwrap();
        assert_writer(&f, Some(run));
        let open: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_eq!(open.state, RunState::ReconciliationRequired);
        assert!(open.holds_writer);
        let decide: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        assert_eq!(decide.state, TaskState::ReconciliationRequired);
        let refused = f
            .hub
            .submit(&Caller::local_owner(), f.create(Some(TaskMode::Work)))
            .await;
        assert_eq!(refused.status, ReceiptStatus::Rejected, "{refused:?}");
        f.close().await;
    }

    #[tokio::test]
    async fn a_confirmed_end_settles_the_run_left_for_the_person_to_check() {
        let f = Fixture::new().await;
        let (task, run, stopping, shutting) = events_end_during_force_stop(&f).await;
        let unresolved = |child: bool| {
            f.hub
                .read(|r| r.list::<AttentionItem>("attention"))
                .unwrap()
                .into_iter()
                .filter(|item| {
                    item.run_id == Some(run)
                        && item.child_id.is_some() == child
                        && item.kind == AttentionKind::ReconciliationRequired
                        && item.resolved_at.is_none()
                })
                .map(|item| item.attention_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(unresolved(false).len(), 1);
        let helpers = unresolved(true);
        assert_eq!(helpers.len(), 1);
        // Force stop then confirms the agent's process ended.
        f.harness
            .controls
            .confirm_terminate
            .store(true, Ordering::SeqCst);
        f.harness.controls.terminate_gate.add_permits(1);
        let receipt = stopping.await.unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        shutting.await.unwrap();
        let ended: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_eq!(ended.state, RunState::Interrupted);
        assert!(!ended.holds_writer);
        assert!(!ended.cleanup_pending);
        assert_writer(&f, None);
        let settled: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        assert_eq!(settled.state, TaskState::Interrupted);
        assert!(unresolved(false).is_empty());
        // A helper whose state is unknown is still the person's to check.
        assert_eq!(unresolved(true), helpers);
        assert_eq!(settled.attention_count, 1);
        f.close().await;
    }

    #[tokio::test]
    async fn what_an_ask_or_plan_turn_left_unconfirmed_is_not_replaced_by_a_new_turn() {
        let waiting = "Something this task started earlier hasn’t ended yet. Stop it, or wait for it to end, then continue.";
        for mode in [TaskMode::Ask, TaskMode::Plan] {
            for lost in [false, true] {
                let f = Fixture::new().await;
                let owner = Caller::local_owner();
                let task = f.task(Some(mode)).await;
                let run = task.current_run.unwrap();
                f.dispatched(run).await;
                tokio::time::timeout(Duration::from_secs(15), async {
                    while !f.hub.inner.runs.lock().await.contains_key(&run) {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
                f.hub
                    .harness_event(
                        run,
                        HarnessEvent::ParentEnded {
                            exit: RunExit::Completed,
                        },
                    )
                    .await
                    .unwrap();
                // Its helpers alone are unconfirmed; the adapter may add more.
                assert!(published(&f, run), "{mode:?}");
                f.hub
                    .harness_event(run, HarnessEvent::CleanupPending)
                    .await
                    .unwrap();
                assert!(published(&f, run), "{mode:?}");
                let refused = f
                    .hub
                    .submit(&owner, f.resume(task.task_id, Some("Next".into()), None))
                    .await;
                assert_eq!(refused.error.unwrap().message, waiting, "{mode:?}");
                if mode == TaskMode::Plan {
                    let current: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
                    let command: Command = serde_json::from_value(serde_json::json!({
                        "command":"resume_task", "task_id":task.task_id, "text":"Carry out this plan", "mode":"work"
                    }))
                    .unwrap();
                    let mut request = CommandEnvelope::new(command);
                    request.expected_revision = Some(current.revision);
                    let refused = f.hub.submit(&owner, request).await;
                    assert_eq!(refused.error.unwrap().message, waiting);
                }
                let kept: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
                assert_eq!(kept.current_run, Some(run));
                assert_eq!(kept.mode, mode);
                if lost {
                    // Its Stop and the person's check stay with this task.
                    f.hub.shutdown().await.unwrap();
                    let decide: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
                    assert_eq!(decide.state, TaskState::ReconciliationRequired, "{mode:?}");
                    let open: Run = f.hub.read(|r| r.require("run", run)).unwrap();
                    assert_eq!(open.state, RunState::ReconciliationRequired);
                    let mut archive = CommandEnvelope::new(Command::ArchiveTask {
                        task_id: task.task_id,
                    });
                    archive.expected_revision = Some(decide.revision);
                    let refused = f.hub.submit(&owner, archive).await;
                    assert_eq!(refused.status, ReceiptStatus::Rejected, "{mode:?}");
                } else {
                    f.harness
                        .emit(
                            run,
                            HarnessEvent::Exited {
                                exit: RunExit::Completed,
                                outcome: None,
                            },
                        )
                        .await;
                    tokio::time::timeout(Duration::from_secs(15), async {
                        while pending(&f, run) {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                    })
                    .await
                    .expect("the leftovers' end was not recorded");
                    assert!(!published(&f, run));
                    let receipt = f
                        .hub
                        .submit(&owner, f.resume(task.task_id, Some("Next".into()), None))
                        .await;
                    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
                    let resumed: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
                    f.dispatched(resumed.current_run.unwrap()).await;
                }
                f.close().await;
            }
        }
    }

    /// Leaves a Work turn finished with one helper still running.
    async fn helper_outlives_turn(f: &Fixture) -> (TaskId, RunId) {
        let task = f.task(Some(TaskMode::Work)).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        for event in [
            HarnessEvent::Child {
                lifetime: Some(ChildLifetime::InProcess),
                native_ref: "assets".into(),
                label: "Build assets".into(),
                state: ChildState::Active,
            },
            HarnessEvent::ParentEnded {
                exit: RunExit::Completed,
            },
        ] {
            f.hub.harness_event(run, event).await.unwrap();
        }
        (task.task_id, run)
    }

    #[tokio::test]
    async fn a_turn_that_leaves_helpers_running_keeps_the_writer_until_they_end() {
        let f = Fixture::new().await;
        let owner = Caller::local_owner();
        let (task, run) = helper_outlives_turn(&f).await;
        let ended: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_eq!(ended.state, RunState::Completed);
        assert!(ended.holds_writer);
        assert_writer(&f, Some(run));
        let waiting = "Helpers from “Inspect” are still finishing in this project. This can start once they end, or stop them.";
        // Continue waits for them, as a second Work task does.
        let refused = f
            .hub
            .submit(&owner, f.resume(task, Some("Next".into()), None))
            .await;
        assert_eq!(refused.error.unwrap().message, waiting);
        let refused = f.hub.submit(&owner, f.create(Some(TaskMode::Work))).await;
        assert_eq!(refused.error.unwrap().message, waiting);
        let current: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        let mut archive = CommandEnvelope::new(Command::ArchiveTask { task_id: task });
        archive.expected_revision = Some(current.revision);
        let refused = f.hub.submit(&owner, archive).await;
        assert_eq!(
            refused.error.unwrap().message,
            "Active or uncertain tasks cannot be archived"
        );
        // Next up holds a Work task behind them too.
        let mut queued = f.create(Some(TaskMode::Work));
        if let Command::CreateTask { placement, .. } = &mut queued.body {
            *placement = TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            };
        }
        let receipt = f.hub.submit(&owner, queued).await;
        let Some(CommandResult::TaskCreated { task_id: next, .. }) = receipt.result else {
            panic!("{receipt:?}")
        };
        let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        let mut resume = CommandEnvelope::new(Command::QueueResume {
            project_id: f.project,
        });
        resume.expected_revision = Some(queue.revision);
        assert_eq!(
            f.hub.submit(&owner, resume).await.status,
            ReceiptStatus::Accepted
        );
        f.hub.advance_queues().await.unwrap();
        let held: Task = f.hub.read(|r| r.require("task", next)).unwrap();
        assert!(held.current_run.is_none());
        f.hub
            .harness_event(
                run,
                HarnessEvent::Exited {
                    exit: RunExit::Completed,
                    outcome: None,
                },
            )
            .await
            .unwrap();
        let settled: Run = f.hub.read(|r| r.require("run", run)).unwrap();
        assert_eq!(settled.state, RunState::Completed);
        assert!(!settled.holds_writer);
        let started = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(run) = f
                    .hub
                    .read(|r| r.require::<Task>("task", next))
                    .unwrap()
                    .current_run
                {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("Next up did not start the waiting task once the helpers ended");
        assert_writer(&f, Some(started));
        f.dispatched(started).await;
        f.close().await;
    }

    /// Stops each helper it is asked to, and counts the requests.
    #[derive(Debug, Default)]
    struct HelperControl {
        stops: std::sync::atomic::AtomicU64,
    }
    #[async_trait::async_trait]
    impl ports::RunControl for HelperControl {
        async fn send_message(
            &self,
            _: MessageRef,
            _: PromptInput,
            _: MessageDelivery,
        ) -> Result<DeliveryState, HarnessError> {
            Err(HarnessError::NotActive)
        }
        async fn interrupt(&self) -> Result<(), HarnessError> {
            Ok(())
        }
        fn close_admission(&self) {}
        async fn resolve_approval(
            &self,
            _: &str,
            _: &ApprovalDecision,
        ) -> Result<(), HarnessError> {
            Err(HarnessError::NotActive)
        }
        async fn shutdown(&self) -> Result<(), HarnessError> {
            Ok(())
        }
        fn child_stop_availability(&self, _: &str) -> Availability {
            Availability::Available
        }
        async fn stop_child(
            &self,
            _: &str,
        ) -> Result<ports::harness::ChildStopResult, HarnessError> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(ports::harness::ChildStopResult {
                state: ChildState::Stopped,
                detail: "The agent reported this helper stopped".into(),
            })
        }
    }

    #[tokio::test]
    async fn stop_reaches_the_helpers_a_finished_turn_left_and_keeps_its_outcome() {
        // The agent's process ends once its helpers have, or it doesn't.
        for ends in [true, false] {
            let f = Fixture::new().await;
            let owner = Caller::local_owner();
            let task = f.task(Some(TaskMode::Work)).await;
            let run = task.current_run.unwrap();
            f.dispatched(run).await;
            let control = Arc::new(HelperControl::default());
            f.hub.inner.runs.lock().await.insert(run, control.clone());
            for event in [
                HarnessEvent::Child {
                    lifetime: Some(ChildLifetime::SeparateProcess),
                    native_ref: "assets".into(),
                    label: "Build assets".into(),
                    state: ChildState::Active,
                },
                HarnessEvent::ParentEnded {
                    exit: RunExit::Completed,
                },
            ] {
                f.hub.harness_event(run, event).await.unwrap();
            }
            if ends {
                let seen: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
                let mut stop = CommandEnvelope::new(Command::StopTask {
                    task_id: task.task_id,
                    force: false,
                });
                stop.expected_revision = Some(seen.revision);
                let receipt = f.hub.submit(&owner, stop).await;
                assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
                let admitted: Run = f.hub.read(|r| r.require("run", run)).unwrap();
                assert_eq!(admitted.state, RunState::Completed);
                let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
                assert!(queue.paused);
                while control.stops.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                f.hub
                    .harness_event(
                        run,
                        HarnessEvent::Exited {
                            exit: RunExit::Completed,
                            outcome: None,
                        },
                    )
                    .await
                    .unwrap();
            } else {
                // Stop waits eight seconds for the process to end, and without
                // that its outcome stays unknown.
                tokio::time::pause();
                let stopped = f.hub.stop_task(task.task_id, None, false).await;
                tokio::time::resume();
                assert_eq!(stopped.unwrap_err().code, ErrorCode::OutcomeUnknown);
            }
            let receipt = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if let Some(receipt) =
                        timeline(&f, task.task_id)
                            .into_iter()
                            .find_map(|item| match item.item {
                                TimelineBody::Receipt { receipt } => Some(receipt),
                                _ => None,
                            })
                    {
                        return receipt;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("no Stop receipt");
            let fact = |label: &str| {
                receipt
                    .facts
                    .iter()
                    .find(|fact| fact.label == label)
                    .unwrap_or_else(|| panic!("{label}: {receipt:?}"))
                    .status
            };
            assert_eq!(fact("Child: Build assets"), FactStatus::Done);
            // Only the process ending confirms this Stop; otherwise the phone
            // goes on to offer Stop again and Force stop.
            assert_eq!(fact("Agent confirmed Stop"), FactStatus::NotApplicable);
            assert_eq!(
                fact("Agent process ended on the computer"),
                if ends {
                    FactStatus::Done
                } else {
                    FactStatus::Unknown
                }
            );
            assert_eq!(
                receipt.summary,
                if ends {
                    "The turn had already finished. Its helpers have ended, and so has the agent’s process."
                } else {
                    "The turn had already finished, so Stop went to the helpers it left running. The receipt says which have stopped."
                }
            );
            let stopped: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            assert_eq!(stopped.state, RunState::Completed);
            if ends {
                assert_writer(&f, None);
                let receipt = f
                    .hub
                    .submit(&owner, f.resume(task.task_id, Some("Next".into()), None))
                    .await;
                assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            } else {
                assert_writer(&f, Some(run));
            }
            f.close().await;
        }
    }

    #[tokio::test]
    async fn a_go_ahead_behind_helpers_nothing_can_follow_waits_for_the_person() {
        let mut f = Fixture::new().await;
        let owner = Caller::local_owner();
        let (task, run) = helper_outlives_turn(&f).await;
        assert_writer(&f, Some(run));
        // A Work task waits behind them in Next up, allowed to start by itself.
        let mut queued = f.create(Some(TaskMode::Work));
        if let Command::CreateTask { placement, .. } = &mut queued.body {
            *placement = TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            };
        }
        let receipt = f.hub.submit(&owner, queued).await;
        let Some(CommandResult::TaskCreated { task_id: next, .. }) = receipt.result else {
            panic!("{receipt:?}")
        };
        let queue: QueueView = f.hub.read(|r| r.require("queue", f.project)).unwrap();
        let mut resume = CommandEnvelope::new(Command::QueueResume {
            project_id: f.project,
        });
        resume.expected_revision = Some(queue.revision);
        assert_eq!(
            f.hub.submit(&owner, resume).await.status,
            ReceiptStatus::Accepted
        );
        // Shutting down closes the agent's control; its helpers can no
        // longer be followed or stopped from here, so nothing confirms they
        // ended, through a restart too.
        f.hub.shutdown().await.unwrap();
        assert_writer(&f, Some(run));
        drop(f.hub);
        f.hub = CompanionHub::builder()
            .data_dir(&f.path)
            .clock(f.clock.clone())
            .harness(f.harness.clone())
            .bridge(crate::bridge::BridgeConfig::new(
                "synthetic-bridge",
                "synthetic-socket",
            ))
            .build()
            .await
            .unwrap();
        assert_writer(&f, Some(run));
        f.hub.advance_queues().await.unwrap();
        let waiting: Task = f.hub.read(|r| r.require("task", next)).unwrap();
        assert_eq!(waiting.current_run, None);
        let decide: Task = f.hub.read(|r| r.require("task", task)).unwrap();
        assert_eq!(decide.state, TaskState::ReconciliationRequired);
        let mut reconcile = CommandEnvelope::new(Command::ReconcileTask {
            task_id: task,
            resolution: Reconciliation::MarkCompleted,
        });
        reconcile.expected_revision = Some(decide.revision);
        let receipt = f.hub.submit(&owner, reconcile).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        assert_writer(&f, None);
        // The go-ahead it was given still starts it.
        f.hub.advance_queues().await.unwrap();
        let started: Task = f.hub.read(|r| r.require("task", next)).unwrap();
        f.dispatched(started.current_run.expect("started once reconciled"))
            .await;
        f.close().await;
    }

    #[tokio::test]
    async fn create_rejects_unavailable_mode_with_reason_and_non_work_computer_use() {
        let f = Fixture::new().await;
        f.hub
            .write(|tx| {
                let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
                infos[0].modes[1].availability = Availability::Unsupported {
                    reason: "Native plan flag is absent".into(),
                };
                tx.put("meta", "harnesses", &infos)
            })
            .unwrap();
        let rejected = f
            .hub
            .submit(&Caller::local_owner(), f.create(Some(TaskMode::Plan)))
            .await;
        assert_eq!(rejected.status, ReceiptStatus::Rejected);
        let error = rejected.error.unwrap();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert_eq!(error.message, "Native plan flag is absent");
        for mode in [TaskMode::Ask, TaskMode::Plan] {
            let mut command = f.create(Some(mode));
            if let Command::CreateTask { kind, .. } = &mut command.body {
                *kind = TaskKind::ComputerUse;
            }
            let rejected = f.hub.submit(&Caller::local_owner(), command).await;
            assert_eq!(rejected.error.unwrap().code, ErrorCode::InvalidRequest);
        }
        assert_eq!(f.harness.starts.load(Ordering::SeqCst), 0);
        f.close().await;
    }

    #[tokio::test]
    async fn legacy_modes_are_work_only_and_resume_keeps_the_persisted_mode() {
        let f = Fixture::new().await;
        f.hub
            .write(|tx| {
                let mut infos: Vec<HarnessInfo> = tx.read().require("meta", "harnesses")?;
                infos[0].modes.clear();
                tx.put("meta", "harnesses", &infos)
            })
            .unwrap();
        let rejected = f
            .hub
            .submit(&Caller::local_owner(), f.create(Some(TaskMode::Ask)))
            .await;
        assert_eq!(rejected.error.unwrap().code, ErrorCode::Unsupported);
        let task = f.task(None).await;
        assert_eq!(task.mode, TaskMode::Work);
        let spec = f.dispatched(task.current_run.unwrap()).await;
        assert_eq!(spec.mode, TaskMode::Work);
        f.close().await;
    }

    #[tokio::test]
    async fn resume_markup_is_admitted_once_delivered_and_replayed_without_duplication() {
        let f = Fixture::new().await;
        let task = f.task(Some(TaskMode::Plan)).await;
        let first_run = task.current_run.unwrap();
        f.dispatched(first_run).await;
        f.hub
            .harness_event(
                first_run,
                HarnessEvent::SessionStarted {
                    native_session: "native-session".into(),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await
            .unwrap();
        f.hub
            .finish_run(first_run, RunState::Completed, None)
            .unwrap();
        let bundle = f.bundle(task.task_id).await;
        let command = f.resume(
            task.task_id,
            Some("Continue here".into()),
            Some(bundle.clone()),
        );
        let receipt = f.hub.submit(&Caller::local_owner(), command.clone()).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted);
        let messages = f.messages(task.task_id).await;
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[1].item,
            TimelineBody::UserMessage {
                text: "Continue here".into(),
                bundle: Some(bundle.clone()),
                delivery: DeliveryState::HostAccepted,
                carried_plan: None
            }
        );
        let second_run = messages[1].run_id.unwrap();
        let spec = f.dispatched(second_run).await;
        assert_eq!(spec.mode, TaskMode::Plan);
        assert_eq!(spec.resume_session.as_deref(), Some("native-session"));
        assert!(spec.bridge.is_none());
        assert!(spec.prompt.text.starts_with("Continue here"));
        assert!(spec.prompt.text.contains(&bundle.bundle_id.to_string()));
        assert_eq!(spec.prompt.images.len(), 1);
        assert_eq!(
            std::fs::read(&spec.prompt.images[0].path).unwrap(),
            b"synthetic-image"
        );
        assert_eq!(
            f.hub
                .submit(&Caller::local_owner(), command.clone())
                .await
                .status,
            ReceiptStatus::Duplicate
        );
        assert_eq!(f.messages(task.task_id).await.len(), 2);
        let backlog = f
            .hub
            .subscribe(&Caller::local_owner(), Some(0))
            .await
            .unwrap()
            .backlog;
        let Backlog::Events(events) = backlog else {
            panic!("journal gap")
        };
        assert_eq!(events.iter().filter(|e| matches!(&e.event, Event::TimelineAppended { item: TimelineItem { run_id: Some(id), item: TimelineBody::UserMessage { .. }, .. }, .. } if *id == second_run)).count(), 1);
        f.hub.shutdown().await.unwrap();
        drop(f.hub);
        let hub = CompanionHub::builder()
            .data_dir(&f.path)
            .clock(f.clock)
            .harness(f.harness.clone())
            .build()
            .await
            .unwrap();
        assert_eq!(
            hub.submit(&Caller::local_owner(), command).await.status,
            ReceiptStatus::Duplicate
        );
        let QueryData::TaskTimeline { items, .. } = hub
            .query(
                &Caller::local_owner(),
                Query::TaskTimeline {
                    task_id: task.task_id,
                    before_seq: None,
                    limit: 100,
                },
            )
            .await
            .unwrap()
        else {
            panic!("timeline")
        };
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(item.item, TimelineBody::UserMessage { .. }))
                .count(),
            2
        );
        assert_eq!(f.harness.starts.load(Ordering::SeqCst), 2);
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(f.path.with_extension("project")).unwrap();
        std::fs::remove_dir_all(f.path).unwrap();
    }

    #[tokio::test]
    async fn text_only_and_markup_only_resume_send_exactly_the_new_message() {
        let f = Fixture::new().await;
        let initial = f.task(Some(TaskMode::Ask)).await;
        let original_bundle = f.bundle(initial.task_id).await;
        // Persisted creation prompt is distinct from every continuation intent.
        f.hub
            .write(|tx| {
                tx.put(
                    "prompt",
                    initial.task_id,
                    &("original".to_owned(), Some(original_bundle.clone())),
                )
            })
            .unwrap();
        let mut previous_run = initial.current_run.unwrap();
        f.dispatched(previous_run).await;
        let new_bundle = f.bundle(initial.task_id).await;
        for (text, bundle) in [
            (Some("text only".to_owned()), None),
            (None, Some(new_bundle.clone())),
        ] {
            f.hub
                .finish_run(previous_run, RunState::Completed, None)
                .unwrap();
            let receipt = f
                .hub
                .submit(
                    &Caller::local_owner(),
                    f.resume(initial.task_id, text.clone(), bundle.clone()),
                )
                .await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted);
            let task: Task = f.hub.read(|r| r.require("task", initial.task_id)).unwrap();
            previous_run = task.current_run.unwrap();
            let spec = f.dispatched(previous_run).await;
            assert_eq!(spec.mode, TaskMode::Ask);
            assert!(
                !spec
                    .prompt
                    .text
                    .contains(&original_bundle.bundle_id.to_string())
            );
            if bundle.is_some() {
                assert!(spec.prompt.text.contains(&new_bundle.bundle_id.to_string()));
                assert_eq!(spec.prompt.images.len(), 1);
            } else {
                assert_eq!(spec.prompt.text, "text only");
                assert!(spec.prompt.images.is_empty());
            }
            let messages = f.messages(initial.task_id).await;
            assert_eq!(
                messages.last().unwrap().item,
                TimelineBody::UserMessage {
                    text: text.unwrap_or_default(),
                    bundle,
                    delivery: DeliveryState::HostAccepted,
                    carried_plan: None
                }
            );
        }
        assert_eq!(f.messages(initial.task_id).await.len(), 3);
        f.close().await;
    }

    #[tokio::test]
    async fn admitted_resume_before_dispatch_recovers_with_one_message_and_no_relaunch() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        f.dispatched(task.current_run.unwrap()).await;
        f.hub
            .finish_run(task.current_run.unwrap(), RunState::Completed, None)
            .unwrap();
        let command = f.resume(task.task_id, Some("Continue".into()), None);
        assert_eq!(
            f.hub
                .submit(&Caller::local_owner(), command.clone())
                .await
                .status,
            ReceiptStatus::Accepted
        );
        assert_eq!(f.harness.starts.load(Ordering::SeqCst), 1);
        f.hub.shutdown().await.unwrap();
        drop(f.hub);
        let hub = CompanionHub::builder()
            .data_dir(&f.path)
            .clock(f.clock)
            .harness(f.harness.clone())
            .build()
            .await
            .unwrap();
        let task: Task = hub.read(|r| r.require("task", task.task_id)).unwrap();
        assert_eq!(task.state, TaskState::ReconciliationRequired);
        assert_eq!(
            hub.submit(&Caller::local_owner(), command).await.status,
            ReceiptStatus::Duplicate
        );
        let QueryData::TaskTimeline { items, .. } = hub
            .query(
                &Caller::local_owner(),
                Query::TaskTimeline {
                    task_id: task.task_id,
                    before_seq: None,
                    limit: 100,
                },
            )
            .await
            .unwrap()
        else {
            panic!("timeline")
        };
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(item.item, TimelineBody::UserMessage { .. }))
                .count(),
            2
        );
        assert_eq!(f.harness.starts.load(Ordering::SeqCst), 1);
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(f.path.with_extension("project")).unwrap();
        std::fs::remove_dir_all(f.path).unwrap();
    }

    #[tokio::test]
    async fn resume_reuses_bundle_validation_and_revision_guards_without_appending() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        f.dispatched(task.current_run.unwrap()).await;
        f.hub
            .finish_run(task.current_run.unwrap(), RunState::Completed, None)
            .unwrap();
        let mut bundle = f.bundle(task.task_id).await;
        bundle.sources[0].sha256 = "0".repeat(64);
        let rejected = f
            .hub
            .submit(
                &Caller::local_owner(),
                f.resume(task.task_id, None, Some(bundle)),
            )
            .await;
        assert_eq!(rejected.error.unwrap().code, ErrorCode::StaleRevision);
        let mut stale = f.resume(task.task_id, Some("continue".into()), None);
        stale.expected_revision = Some(0);
        assert_eq!(
            f.hub
                .submit(&Caller::local_owner(), stale)
                .await
                .error
                .unwrap()
                .code,
            ErrorCode::StaleRevision
        );
        assert_eq!(f.messages(task.task_id).await.len(), 1);
        f.close().await;
    }

    #[tokio::test]
    async fn annotation_ownership_is_checked_for_create_send_and_resume() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let bundle = f.bundle(task.task_id).await;
        let caller = Caller {
            principal: protocol::auth::Principal::Device {
                device_id: DeviceId::new(),
            },
            grants: protocol::auth::Grants {
                tasks: true,
                ..Default::default()
            },
        };
        assert_eq!(
            f.hub
                .validate_bundle_for(&caller, &bundle)
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        f.hub
            .write(|tx| {
                tx.put(
                    "artifact_creator",
                    bundle.sources[0].artifact_id,
                    &caller.principal,
                )
            })
            .unwrap();
        f.hub.validate_bundle_for(&caller, &bundle).unwrap();
        f.close().await;
    }

    #[tokio::test]
    async fn what_was_said_over_a_video_reaches_the_agent_with_its_times() {
        use protocol::annotation::{Comment, CommentOrigin, VideoMoment};
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let mut bundle = f.bundle(task.task_id).await;
        let said = |text: &str, still: &str, start_ms, end_ms| Comment {
            comment_id: protocol::ids::CommentId::new(),
            text: text.into(),
            mark_ids: vec![],
            origin: CommentOrigin::Spoken {
                segment_ids: vec![],
                edited: false,
            },
            link_origins: vec![],
            moment: Some(VideoMoment {
                still: still.into(),
                start_ms,
                end_ms,
            }),
        };
        bundle.comments = vec![
            said("The menu jumps here", "S1", 12_400, 15_000),
            said("and stays wrong", "S1", 3_725_000, 3_726_000),
        ];
        let prompt = f.hub.prompt_input("Look", Some(&bundle)).unwrap();
        assert!(prompt.text.ends_with(
            "\n\nSaid while watching the video (times are positions in the video; each still is a frame from then):\n- 0:12–0:15, still S1: “The menu jumps here”\n- 1:02:05–1:02:06, still S1: “and stays wrong”"
        ), "{}", prompt.text);
        // A moment names one of the bundle's stills, and ends after it starts.
        for (still, start_ms, end_ms) in [("S9", 0, 1), ("S1", 2, 1)] {
            bundle.comments = vec![said("off", still, start_ms, end_ms)];
            assert_eq!(
                f.hub.prompt_input("Look", Some(&bundle)).unwrap_err().code,
                ErrorCode::InvalidRequest
            );
        }
        f.close().await;
    }

    #[tokio::test]
    async fn read_only_write_approval_is_rejected_before_claim_and_can_still_be_denied() {
        let f = Fixture::new().await;
        let task = f.task(Some(TaskMode::Ask)).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        f.hub
            .harness_event(
                run,
                HarnessEvent::ApprovalRequested(ports::NativeApproval {
                    native_request_id: "write".into(),
                    kind: ApprovalKind::FileEdit,
                    title: "Write file".into(),
                    detail: None,
                    detail_lines: Vec::new(),
                    questions: Vec::new(),
                    raw_request: None,
                    scope: "once".into(),
                    shortened: false,
                    tool_call: None,
                }),
            )
            .await
            .unwrap();
        let approval = f
            .hub
            .read(|r| Ok(r.list::<Approval>("approval")?.remove(0)))
            .unwrap();
        let mut allow = CommandEnvelope::new(Command::ResolveApproval {
            approval_id: approval.approval_id,
            decision: ApprovalDecision::Allow,
        });
        allow.expected_revision = Some(approval.revision.into());
        let receipt = f.hub.submit(&Caller::local_owner(), allow).await;
        assert_eq!(receipt.error.unwrap().code, ErrorCode::Unsupported);
        assert!(
            f.hub
                .read(|r| r.get::<CommandId>("approval_claim", approval.approval_id))
                .unwrap()
                .is_none()
        );
        let mut deny = CommandEnvelope::new(Command::ResolveApproval {
            approval_id: approval.approval_id,
            decision: ApprovalDecision::Deny { reason: None },
        });
        deny.expected_revision = Some(approval.revision.into());
        assert_eq!(
            f.hub.submit(&Caller::local_owner(), deny).await.status,
            ReceiptStatus::Accepted
        );
        f.close().await;
    }

    #[tokio::test]
    async fn native_approval_closure_persists_and_emits_only_reported_outcomes() {
        let f = Fixture::new().await;
        let task = f.task(None).await;
        let run = task.current_run.unwrap();
        f.dispatched(run).await;
        for outcome in [
            Some(ApprovalOutcome::Allowed),
            Some(ApprovalOutcome::Denied),
            None,
        ] {
            let native_request_id = uuid::Uuid::new_v4().to_string();
            f.hub
                .harness_event(
                    run,
                    HarnessEvent::ApprovalRequested(ports::NativeApproval {
                        native_request_id: native_request_id.clone(),
                        kind: ApprovalKind::Command,
                        title: "Review command".into(),
                        detail: None,
                        detail_lines: Vec::new(),
                        questions: Vec::new(),
                        raw_request: None,
                        scope: "once".into(),
                        shortened: false,
                        tool_call: None,
                    }),
                )
                .await
                .unwrap();
            f.hub
                .harness_event(
                    run,
                    HarnessEvent::ApprovalClosed {
                        native_request_id: native_request_id.clone(),
                        outcome,
                    },
                )
                .await
                .unwrap();
            let approval = f
                .hub
                .read(|r| {
                    Ok(r.list::<Approval>("approval")?
                        .into_iter()
                        .find(|a| a.native_request_id == native_request_id)
                        .unwrap())
                })
                .unwrap();
            assert_eq!(approval.state, ApprovalState::ResolvedElsewhere);
            let task_now: Task = f.hub.read(|r| r.require("task", task.task_id)).unwrap();
            let run_now: Run = f.hub.read(|r| r.require("run", run)).unwrap();
            assert_eq!(task_now.state, TaskState::Working);
            assert_eq!(task_now.attention_count, 0);
            assert_eq!(run_now.state, RunState::Working);
            assert_eq!(approval.outcome, outcome);
            assert!(approval.resolved_at.is_some());
            let Backlog::Events(events) = f
                .hub
                .subscribe(&Caller::local_owner(), Some(0))
                .await
                .unwrap()
                .backlog
            else {
                panic!("gap")
            };
            assert!(events.iter().any(
                |e| matches!(&e.event, Event::ApprovalUpserted { approval: a } if a == &approval)
            ));
            // A later cleanup does not erase an observed decision.
            f.hub
                .harness_event(
                    run,
                    HarnessEvent::ApprovalClosed {
                        native_request_id,
                        outcome: None,
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                f.hub
                    .read(|r| r.require::<Approval>("approval", approval.approval_id))
                    .unwrap()
                    .outcome,
                outcome
            );
        }
        f.close().await;
    }

    #[tokio::test]
    async fn a_start_the_agent_refuses_fails_once_and_says_why() {
        let f = Fixture::new().await;
        *f.harness.refuse_start.lock().unwrap() = Some("Update the agent to use Ask.".into());
        let command = f.create(Some(TaskMode::Ask));
        let receipt = f.hub.submit(&Caller::local_owner(), command.clone()).await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            panic!("{receipt:?}")
        };
        let status = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(c) = f.hub.read(|r| r.command(command.command_id)).unwrap()
                    && !c.pending
                {
                    return c;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("start did not settle");
        assert_eq!(status.stage, CommandStage::Failed);
        assert_eq!(
            status.error.unwrap().message,
            "Update the agent to use Ask."
        );
        let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
        assert_eq!(task.state, TaskState::Failed);
        let items: Vec<TimelineItem> = f
            .hub
            .read(|r| r.list(&format!("timeline:{task_id}")))
            .unwrap();
        let notes: Vec<&str> = items
            .iter()
            .filter_map(|i| match &i.item {
                TimelineBody::Note { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            notes,
            ["The agent didn’t start. Update the agent to use Ask."]
        );
        let ended = items
            .iter()
            .filter(|i| matches!(i.item, TimelineBody::RunEnded { .. }))
            .count();
        assert_eq!(ended, 1, "{items:?}");
        f.close().await;
    }
}
