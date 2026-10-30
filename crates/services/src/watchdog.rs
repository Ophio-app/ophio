// SPDX-License-Identifier: Apache-2.0
//! A turn that holds the project shows it is working, or lets go. The guard
//! reads each running turn's events: one that goes quiet, or spends its time
//! sleeping, watching or polling, is told to end its turn, and is stopped if
//! it doesn't, with Next up left to go on. Waiting is what `wake_me` is for:
//! the agent ends its turn, the project is free, and Ophio wakes it.
//!
//! A turn waiting on a person isn't held to this; what it asked goes back to
//! it unanswered after a while instead (see `carry_on`).

use crate::{
    CompanionHub,
    github_watch::{as_creator, headline_item, raise_headline, raise_item},
};
use ports::{HarnessEvent, MessageRef, PromptInput};
use protocol::{
    CommandId, ErrorCode, Event, ProtocolError, RunId,
    command::{Command, CommandEnvelope, MessageDelivery},
    model::{
        ActivityStatus, AttentionKind, ChildState, Project, Run, RunState, Task, TaskKind,
        TimelineBody, TimelineItem,
    },
};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

/// Quiet this long, with nothing running, the agent is reminded.
pub(crate) const QUIET_NUDGE: Duration = Duration::from_secs(20 * 60);
/// Quiet this long, with nothing running, it is stopped.
pub(crate) const QUIET_STOP: Duration = Duration::from_secs(30 * 60);
/// Spent this long in all sleeping, watching or polling, it is reminded.
pub(crate) const WAIT_NUDGE: Duration = Duration::from_secs(15 * 60);
/// And stopped after this long of it.
pub(crate) const WAIT_STOP: Duration = Duration::from_secs(30 * 60);
/// One step running this long, the person is told.
pub(crate) const STEP_ALERT: Duration = Duration::from_secs(45 * 60);
/// One step running this long, it is stopped.
pub(crate) const STEP_STOP: Duration = Duration::from_secs(90 * 60);
/// Still running this long after the guard stopped it, it is force stopped.
const FORCE_AFTER: Duration = Duration::from_secs(2 * 60);

/// Stops the guard asked for, by command: Next up isn't paused for one.
pub(crate) const GUARD_STOP: &str = "guard_stop";

const QUIET_TEXT: &str = "Ophio hasn't seen any sign of work from you in 20 minutes, and you hold the project, so nobody else can work on it. If you're waiting on something, stop: call the wake_me tool with how many minutes to wait and what for, then end your turn. If you're blocked, end your turn with a short summary of exactly what you need. A turn that shows no sign of work for 30 minutes is stopped.";
const WAIT_TEXT: &str = "You've spent 15 minutes of this turn sleeping, watching or polling while you hold the project, so nobody else can work on it. Don't wait in your turn: call the wake_me tool with how many minutes to wait and what for, then end your turn. The project is free meanwhile, and Ophio wakes you. A turn that keeps waiting is stopped after 30 minutes of it.";

/// What one running turn was last seen doing.
#[derive(Debug)]
pub(crate) struct Liveness {
    last: Instant,
    steps: HashMap<String, Step>,
    helpers: HashSet<String>,
    /// Time its finished waits took.
    waited: Duration,
    /// While it waits on a person, its clocks stand still.
    held: Option<Instant>,
    told: Option<Why>,
    alerted: bool,
    stopped: Option<Instant>,
}

#[derive(Debug)]
struct Step {
    since: Instant,
    tool: String,
    kind: StepKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Work,
    Wait,
    /// A helper agent: it shows its own work.
    Helper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Why {
    Quiet,
    Waiting,
    LongStep,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Fine,
    Remind(Why),
    /// One step has run long: the person is told which.
    Alert(String),
    Stop(Why),
    /// Stopped and still running.
    Force,
}

impl Liveness {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last: now,
            steps: HashMap::new(),
            helpers: HashSet::new(),
            waited: Duration::ZERO,
            held: None,
            told: None,
            alerted: false,
            stopped: None,
        }
    }

    pub(crate) fn saw(&mut self, event: &HarnessEvent, now: Instant) {
        match event {
            // What Ophio itself sent being delivered is no sign of work.
            HarnessEvent::MessageDelivery { .. } => return,
            HarnessEvent::Activity {
                key,
                tool,
                summary,
                status: ActivityStatus::Running,
                detail,
            } => {
                let kind = if matches!(tool.as_str(), "Task" | "Agent")
                    || summary == "collabAgentToolCall"
                {
                    StepKind::Helper
                } else if waits(detail.as_deref()) {
                    StepKind::Wait
                } else {
                    StepKind::Work
                };
                self.steps.entry(key.clone()).or_insert(Step {
                    since: now,
                    tool: tool.clone(),
                    kind,
                });
            }
            HarnessEvent::Activity { key, .. } => {
                if let Some(step) = self.steps.remove(key)
                    && step.kind == StepKind::Wait
                {
                    self.waited += now.saturating_duration_since(step.since);
                }
            }
            HarnessEvent::Child {
                native_ref, state, ..
            } => {
                if *state == ChildState::Active {
                    self.helpers.insert(native_ref.clone());
                } else {
                    self.helpers.remove(native_ref);
                }
            }
            _ => {}
        }
        self.last = now;
    }

    /// Stops its clocks while it waits on a person.
    pub(crate) fn hold(&mut self, now: Instant) {
        self.held.get_or_insert(now);
    }

    /// Starts them again, as they were. What it was seen doing meanwhile
    /// already counts from when it was seen.
    pub(crate) fn release(&mut self, now: Instant) {
        let Some(held) = self.held.take() else {
            return;
        };
        let paused = now.saturating_duration_since(held);
        if self.last < held {
            self.last += paused;
        }
        for step in self.steps.values_mut() {
            if step.since < held {
                step.since += paused;
            }
        }
    }

    /// What to do about it now. `stopping` when it has been asked to stop,
    /// by the guard or anyone.
    pub(crate) fn verdict(&self, now: Instant, stopping: bool) -> Verdict {
        if let Some(stopped) = self.stopped {
            return match now.saturating_duration_since(stopped) >= FORCE_AFTER {
                true => Verdict::Force,
                false => Verdict::Fine,
            };
        }
        if stopping {
            return Verdict::Fine;
        }
        let quiet = now.saturating_duration_since(self.last);
        let mut waiting = self.waited;
        let mut longest: Option<(&Step, Duration)> = None;
        let mut busy = !self.helpers.is_empty();
        for step in self.steps.values() {
            let age = now.saturating_duration_since(step.since);
            match step.kind {
                StepKind::Wait => waiting += age,
                StepKind::Helper => busy = true,
                StepKind::Work => {
                    busy = true;
                    if longest.is_none_or(|(_, l)| age > l) {
                        longest = Some((step, age));
                    }
                }
            }
        }
        let long = longest.map(|(_, age)| age).unwrap_or_default();
        if waiting >= WAIT_STOP {
            return Verdict::Stop(Why::Waiting);
        }
        if (!busy && quiet >= QUIET_STOP) || long >= STEP_STOP || quiet >= STEP_STOP {
            return Verdict::Stop(if busy { Why::LongStep } else { Why::Quiet });
        }
        if waiting >= WAIT_NUDGE && self.told != Some(Why::Waiting) {
            return Verdict::Remind(Why::Waiting);
        }
        if !busy && quiet >= QUIET_NUDGE && self.told.is_none() {
            return Verdict::Remind(Why::Quiet);
        }
        if let Some((step, age)) = longest
            && age >= STEP_ALERT
            && !self.alerted
        {
            return Verdict::Alert(step.tool.clone());
        }
        Verdict::Fine
    }
}

/// Whether a command or tool input waits: sleeps half a minute or more,
/// sleeps in a loop, or watches something until it changes.
pub(crate) fn waits(detail: Option<&str>) -> bool {
    let Some(detail) = detail else {
        return false;
    };
    let text = detail.to_lowercase();
    const WATCHES: [&str; 6] = [
        "gh run watch",
        "--watch",
        "watch -n",
        "tail -f",
        "inotifywait",
        "--follow",
    ];
    if WATCHES.iter().any(|w| text.contains(w)) {
        return true;
    }
    let mut sleeps = text.match_indices("sleep ").map(|(at, _)| &text[at + 6..]);
    let looped = ["while ", "until ", "for "]
        .iter()
        .any(|l| text.contains(l));
    sleeps.any(|rest| looped || seconds(rest).is_some_and(|s| s >= 30.0))
}

/// The length `sleep` was given at the start of `rest`, in seconds.
fn seconds(rest: &str) -> Option<f64> {
    let arg: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
        .collect();
    let split = arg.find(|c: char| !(c.is_ascii_digit() || c == '.'));
    let (number, unit) = arg.split_at(split.unwrap_or(arg.len()));
    let number: f64 = number.parse().ok()?;
    let scale = match unit {
        "" | "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        _ => return None,
    };
    Some(number * scale)
}

/// How often the guard looks while anything runs or is still to pass on.
pub(crate) const GUARD_EVERY: Duration = Duration::from_secs(30);

impl CompanionHub {
    /// One look over running turns and what is still to pass on to their
    /// agents. Whether there is anything to look at again.
    pub(crate) async fn guard_once(&self) -> bool {
        let _one = self.inner.guarding.lock().await;
        if let Err(e) = self.carry_on_unanswered().await {
            tracing::error!(code = ?e.code, "giving unanswered requests back failed");
        }
        if let Err(e) = self.guard_liveness().await {
            tracing::error!(code = ?e.code, "guarding running turns failed");
        }
        if let Err(e) = self.pass_on_followups().await {
            tracing::error!(code = ?e.code, "passing follow-ups on failed");
        }
        !self.inner.runs.lock().await.is_empty() || self.read(crate::followups::any).unwrap_or(true)
    }

    /// Starts watching a turn that has just started.
    pub(crate) fn guard_run(&self, run: RunId) {
        let now = self.inner.clock.monotonic();
        (self.inner.liveness.lock().expect("liveness lock")).insert(run, Liveness::new(now));
        self.wake_guard();
    }

    /// Notes an event from a running turn.
    pub(crate) fn saw(&self, run: RunId, event: &HarnessEvent) {
        let now = self.inner.clock.monotonic();
        if let Some(l) = (self.inner.liveness.lock().expect("liveness lock")).get_mut(&run) {
            l.saw(event, now);
        }
    }

    pub(crate) fn unguard_run(&self, run: RunId) {
        (self.inner.liveness.lock().expect("liveness lock")).remove(&run);
    }

    /// Reminds, or stops, each running turn that holds the project with no
    /// sign of work.
    pub(crate) async fn guard_liveness(&self) -> Result<(), ProtocolError> {
        let live: Vec<RunId> = self.inner.runs.lock().await.keys().copied().collect();
        for run_id in live {
            let guarded = self.read(|r| guarded(r, run_id))?;
            let now = self.inner.clock.monotonic();
            let verdict = {
                let mut liveness = self.inner.liveness.lock().expect("liveness lock");
                let Some(l) = liveness.get_mut(&run_id) else {
                    continue;
                };
                match &guarded {
                    Some((_, stopping)) => {
                        l.release(now);
                        let verdict = l.verdict(now, *stopping);
                        // Marked as it is decided, so it is acted on once.
                        match &verdict {
                            Verdict::Remind(why) => l.told = Some(*why),
                            Verdict::Alert(_) => l.alerted = true,
                            _ => {}
                        }
                        verdict
                    }
                    None => {
                        l.hold(now);
                        Verdict::Fine
                    }
                }
            };
            let Some((task, _)) = guarded else { continue };
            let acted = match verdict {
                Verdict::Fine => Ok(()),
                Verdict::Remind(why) => self.remind(&task, run_id, why).await,
                Verdict::Alert(tool) => self.alert_step(&task, run_id, &tool),
                Verdict::Stop(why) => self.guard_stop(&task, run_id, why, false).await,
                Verdict::Force => self.guard_stop(&task, run_id, Why::Quiet, true).await,
            };
            if let Err(e) = acted {
                tracing::warn!(code = ?e.code, "guarding a run failed");
            }
        }
        Ok(())
    }

    fn mark(&self, run_id: RunId, change: impl FnOnce(&mut Liveness)) {
        if let Some(l) = (self.inner.liveness.lock().expect("liveness lock")).get_mut(&run_id) {
            change(l);
        }
    }

    async fn remind(&self, task: &Task, run_id: RunId, why: Why) -> Result<(), ProtocolError> {
        let Some(control) = self.inner.runs.lock().await.get(&run_id).cloned() else {
            return Ok(());
        };
        let text = match why {
            Why::Waiting => WAIT_TEXT,
            _ => QUIET_TEXT,
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            control.send_message(
                MessageRef(CommandId::new()),
                PromptInput {
                    text: text.into(),
                    images: Vec::new(),
                },
                MessageDelivery::Steer,
            ),
        )
        .await;
        let note = match why {
            Why::Waiting => {
                "The agent has spent 15 minutes waiting while it holds the project, so Ophio told it to end its turn and ask to be woken instead."
            }
            _ => {
                "No sign of work from the agent in 20 minutes, so Ophio told it to end its turn if it's waiting or blocked."
            }
        };
        self.note(task, Some(run_id), note)
    }

    pub(crate) fn alert_step(
        &self,
        task: &Task,
        run_id: RunId,
        tool: &str,
    ) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        let body = format!(
            "One step ({tool}) has been running for {} minutes while it holds the project. Ophio stops it at {} minutes; stop it sooner if it's stuck.",
            STEP_ALERT.as_secs() / 60,
            STEP_STOP.as_secs() / 60
        );
        let mut item = headline_item(
            task.task_id,
            AttentionKind::WaitingForYou,
            protocol::notifications::AlertTopic::SlowWork,
            "A step is taking long",
            Some(body),
            now,
        );
        // About this turn's step, so it ends with the turn.
        item.run_id = Some(run_id);
        self.write(|tx| raise_item(tx, item))
    }

    async fn guard_stop(
        &self,
        task: &Task,
        run_id: RunId,
        why: Why,
        force: bool,
    ) -> Result<(), ProtocolError> {
        let mut envelope = CommandEnvelope::new(Command::StopTask {
            task_id: task.task_id,
            force,
        });
        if force && !self.read(|r| Ok(r.require::<Run>("run", run_id)?.force_stoppable))? {
            return Ok(());
        }
        let command_id = envelope.command_id;
        self.write(|tx| tx.put(GUARD_STOP, command_id, &true))?;
        envelope.expected_revision = None;
        let receipt = self.admit(&as_creator(task), envelope).await;
        if let Some(e) = receipt.error {
            self.write(|tx| tx.delete(GUARD_STOP, command_id))?;
            if e.code != ErrorCode::AlreadyResolved {
                return Err(e);
            }
            return Ok(());
        }
        let now = self.inner.clock.monotonic();
        self.mark(run_id, |l| l.stopped = Some(now));
        if force {
            return Ok(());
        }
        let (headline, body) = match why {
            Why::Waiting => (
                "Stopped: it was waiting",
                "It spent 30 minutes waiting while it held the project, so Ophio stopped it and freed the project. Continue picks it up where it left off.",
            ),
            Why::LongStep => (
                "Stopped: a step ran too long",
                "One step ran for 90 minutes while it held the project, so Ophio stopped it and freed the project. Continue picks it up where it left off.",
            ),
            Why::Quiet => (
                "Stopped: no sign of work",
                "It showed no sign of work for 30 minutes while it held the project, so Ophio stopped it and freed the project. Continue picks it up where it left off.",
            ),
        };
        let at = self.inner.clock.now();
        self.write(|tx| {
            raise_headline(
                tx,
                task.task_id,
                AttentionKind::WaitingForYou,
                protocol::notifications::AlertTopic::SlowWork,
                headline,
                Some(body.into()),
                at,
            )
        })?;
        self.note(task, Some(run_id), body)
    }

    fn note(&self, task: &Task, run_id: Option<RunId>, text: &str) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            tx.emit(Event::TimelineAppended {
                task_id: task.task_id,
                item: TimelineItem {
                    seq: 0,
                    at: now,
                    run_id,
                    item: TimelineBody::Note { text: text.into() },
                },
            })?;
            Ok(())
        })
    }
}

/// The task, when `run_id` is a turn the guard holds to account: working,
/// or asked to stop and not yet stopped, holding its project, and not
/// waiting on a person. The computer specialist's turns have their own
/// limits. With it, whether it has been asked to stop.
fn guarded(r: &store::Read<'_>, run_id: RunId) -> store::Result<Option<(Task, bool)>> {
    let Some(run) = r.get::<Run>("run", run_id)? else {
        return Ok(None);
    };
    let stopping = r.get::<bool>("stopped", run_id)?.unwrap_or(false);
    if !(run.state == RunState::Working || stopping && !run.state.is_terminal())
        || !run.holds_writer
    {
        return Ok(None);
    }
    let Some(task) = r.get::<Task>("task", run.task_id)? else {
        return Ok(None);
    };
    let project: Option<Project> = r.get("project", task.project_id)?;
    if task.kind == TaskKind::ComputerUse
        || project.is_none_or(|p| p.active_writer != Some(run_id))
        || crate::carry_on::run_blocked(r, run_id)?
    {
        return Ok(None);
    }
    Ok(Some((task, stopping)))
}
