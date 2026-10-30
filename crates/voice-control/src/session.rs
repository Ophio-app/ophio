// SPDX-License-Identifier: Apache-2.0
//! Per-session monotonic policy; no retry can repeat an utterance's action.
use crate::{
    Error, Snapshot,
    policy::{self, Decision},
    thresholds::*,
};
use jev::questions::VoiceAnswer;
use ports::Clock;
use protocol::voice::*;
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

pub struct Session {
    pub view: VoiceSessionView,
    pub snapshot: Option<Snapshot>,
    pub pending: Option<VoiceAction>,
    pub answer: Option<VoiceAnswer>,
    pub utterance: String,
    pub revision: u64,
    pub changed: Instant,
    clock: Arc<dyn Clock>,
    consumed: HashSet<String>,
    last_action: Option<Instant>,
    last_activity: Instant,
    stalls: usize,
}
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("phase", &self.view.phase)
            .finish_non_exhaustive()
    }
}
impl Session {
    pub fn new(view: VoiceSessionView, clock: Arc<dyn Clock>) -> Self {
        let now = clock.monotonic();
        Self {
            view,
            snapshot: None,
            pending: None,
            answer: None,
            utterance: String::new(),
            revision: 0,
            changed: now,
            clock,
            consumed: HashSet::new(),
            last_action: None,
            last_activity: now,
            stalls: 0,
        }
    }
    pub fn stop(&mut self, cause: VoiceStopCause) {
        if self.view.ended.is_none() {
            self.view.ended = Some(cause);
            self.view.phase = VoicePhase::Stopped;
            self.view.understood = match cause {
                VoiceStopCause::Stall=>"The controls did not change after three actions. Review the computer before continuing",
                VoiceStopCause::StepLimit=>"This voice session reached its receipt limit. Review before starting a new session",
                VoiceStopCause::Idle=>"No commands were given for 10 minutes",
                VoiceStopCause::Heartbeat=>"The computer stopped receiving this device’s heartbeat",
                VoiceStopCause::ScreenLock=>"The computer locked",
                VoiceStopCause::Revoked=>"This device’s control access was revoked",
                VoiceStopCause::CapabilityLost=>"Voice control availability changed. Check Settings",
                _=>"Voice control ended",
            }.into();
            self.pending = None;
            self.view.choices.clear();
        }
    }
    pub fn stamp(&self) -> (String, u64, u64) {
        (self.utterance.clone(), self.revision, self.view.prompt_id)
    }
    pub fn idle(&self) -> bool {
        self.clock
            .monotonic()
            .saturating_duration_since(self.last_activity)
            >= IDLE
    }
    pub fn begin(
        &mut self,
        utterance: &str,
        revision: u64,
        text: String,
        snapshot: Snapshot,
    ) -> Result<(), Error> {
        if self.view.ended.is_some() || self.idle() {
            return Err(Error::Stopped);
        }
        if self.view.phase == VoicePhase::Acting {
            return Err(Error::Superseded);
        }
        if self.consumed.contains(utterance)
            || (self.utterance == utterance && revision <= self.revision)
        {
            return Err(Error::Superseded);
        }
        if self.view.steps.len() >= STEP_LIMIT
            || serde_json::to_vec(&self.view.steps).map_or(true, |bytes| bytes.len() >= 256 * 1024)
        {
            self.stop(VoiceStopCause::StepLimit);
            return Err(Error::Stopped);
        }
        if utterance != self.utterance && !self.utterance.is_empty() {
            self.consumed.insert(self.utterance.clone());
        }
        self.utterance = utterance.into();
        self.revision = revision;
        self.changed = self.clock.monotonic();
        self.last_activity = self.changed;
        self.view.transcript = text;
        self.view.phase = VoicePhase::Assessing;
        self.view.understood = "Understanding…".into();
        self.snapshot = Some(snapshot);
        self.answer = None;
        self.pending = None;
        self.view.choices.clear();
        Ok(())
    }
    pub fn assessed(&mut self, answer: VoiceAnswer, final_segment: bool) -> Decision {
        self.answer = Some(answer);
        self.record(
            VoiceOutcome::Assessed,
            "Interpretation received; no input sent".into(),
            None,
        );
        self.evaluate(final_segment)
    }
    pub fn evaluate(&mut self, final_segment: bool) -> Decision {
        let decision = match (&self.answer, &self.snapshot) {
            (Some(a), Some(s)) => policy::evaluate(
                &self.view.transcript,
                s,
                &a.answers,
                final_segment,
                self.clock
                    .monotonic()
                    .saturating_duration_since(self.changed),
            ),
            _ => Decision::Wait,
        };
        self.present(&decision);
        decision
    }
    pub fn present(&mut self, decision: &Decision) {
        self.view.choices.clear();
        match decision {
            Decision::Ignore => {
                self.view.phase = VoicePhase::Ready;
                self.view.understood = "No command heard".into();
                self.record(VoiceOutcome::Ignored, "No command heard".into(), None);
            }
            Decision::Wait => {
                self.view.phase = VoicePhase::Waiting;
                self.view.understood = "Waiting for a complete, clear command".into();
            }
            Decision::Refuse(reason) => {
                self.view.phase = VoicePhase::Refused;
                self.view.understood = reason.clone();
                self.record(VoiceOutcome::Refused, reason.clone(), None);
            }
            Decision::Choose(choices, action) => {
                self.view.phase = VoicePhase::Disambiguation;
                self.view.choices = choices.clone();
                self.pending = Some(action.clone());
                self.view.prompt_id += 1;
                self.view.understood = "Say or tap 1–3".into();
            }
            Decision::Confirm(action) => {
                self.view.phase = VoicePhase::Confirm;
                self.pending = Some(action.clone());
                self.view.prompt_id += 1;
                self.view.understood = policy::describe(action);
            }
            Decision::Act(action) => {
                self.view.phase = VoicePhase::Ready;
                self.pending = Some(action.clone());
                self.view.understood = policy::describe(action);
            }
            Decision::Cancel => {
                self.view.phase = VoicePhase::Ready;
                self.pending = None;
                self.view.understood = "Command cancelled".into();
                self.record(VoiceOutcome::Cancelled, "Command cancelled".into(), None);
            }
        }
    }
    pub fn choose(&mut self, prompt: u64, id: &str) -> Result<Decision, Error> {
        if self.view.ended.is_some()
            || self.view.phase != VoicePhase::Disambiguation
            || self.view.prompt_id != prompt
        {
            return Err(Error::StaleTarget);
        }
        let target = self
            .view
            .choices
            .iter()
            .find(|e| e.id == id)
            .cloned()
            .ok_or(Error::StaleTarget)?;
        let mut action = self.pending.clone().ok_or(Error::StaleTarget)?;
        action.target = Some(target);
        let decision = policy::check_action(
            action,
            &self.answer.as_ref().ok_or(Error::StaleTarget)?.answers,
            false,
        );
        self.present(&decision);
        Ok(decision)
    }
    pub fn confirm(&mut self, prompt: u64) -> Result<Decision, Error> {
        if self.view.ended.is_some()
            || self.view.phase != VoicePhase::Confirm
            || self.view.prompt_id != prompt
        {
            return Err(Error::StaleTarget);
        }
        let decision = policy::check_action(
            self.pending.clone().ok_or(Error::StaleTarget)?,
            &self.answer.as_ref().ok_or(Error::StaleTarget)?.answers,
            true,
        );
        self.present(&decision);
        Ok(decision)
    }
    pub fn claim(&mut self, action: &VoiceAction, fresh: &Snapshot) -> Result<bool, Error> {
        if self.view.ended.is_some() || self.idle() {
            return Err(Error::Stopped);
        }
        if self.consumed.contains(&self.utterance) {
            return Err(Error::Superseded);
        }
        if self.last_action.is_some_and(|last| {
            self.clock.monotonic().saturating_duration_since(last) < ACTION_INTERVAL
        }) {
            self.view.phase = VoicePhase::Refused;
            self.view.understood = "Wait a moment, then give a new command".into();
            self.record(
                VoiceOutcome::Refused,
                self.view.understood.clone(),
                Some(action.clone()),
            );
            return Err(Error::Superseded);
        }
        if self
            .snapshot
            .as_ref()
            .is_none_or(|old| !fresh.still_matches(old, action))
        {
            self.view.phase = VoicePhase::Refused;
            self.view.understood = Error::StaleTarget.to_string();
            self.record(
                VoiceOutcome::StaleTarget,
                self.view.understood.clone(),
                Some(action.clone()),
            );
            self.snapshot = Some(fresh.clone());
            return Err(Error::StaleTarget);
        }
        self.consumed.insert(self.utterance.clone());
        self.last_action = Some(self.clock.monotonic());
        self.last_activity = self.clock.monotonic();
        self.view.phase = VoicePhase::Acting;
        // Record uncertainty before any effect. Crash recovery never retries a step.
        self.record(
            VoiceOutcome::Unknown,
            "The computer has not confirmed this action".into(),
            Some(action.clone()),
        );
        Ok(!self.view.dry_run)
    }
    pub fn complete(
        &mut self,
        action: &VoiceAction,
        outcome: Result<(), Error>,
        fresh: Option<Snapshot>,
    ) {
        let (result, detail) = match outcome {
            Ok(()) if self.view.dry_run => (
                VoiceOutcome::DryRun,
                "Preview only. No input sent".to_owned(),
            ),
            Ok(()) => (
                VoiceOutcome::Applied,
                "The computer confirmed the input".to_owned(),
            ),
            Err(Error::StaleTarget) => (VoiceOutcome::StaleTarget, Error::StaleTarget.to_string()),
            Err(Error::OutcomeUnknown) => {
                (VoiceOutcome::Unknown, Error::OutcomeUnknown.to_string())
            }
            Err(e) => (VoiceOutcome::Failed, e.to_string()),
        };
        if let Some(step) = self.view.steps.last_mut() {
            step.outcome = result;
            step.detail = detail.clone();
        }
        self.view.phase = match result {
            VoiceOutcome::Unknown => VoicePhase::OutcomeUnknown,
            VoiceOutcome::Failed | VoiceOutcome::StaleTarget => VoicePhase::Refused,
            _ => VoicePhase::Ready,
        };
        self.view.understood = format!("{} · {detail}", policy::describe(action));
        self.pending = None;
        self.view.choices.clear();
        if !self.view.dry_run
            && result == VoiceOutcome::Applied
            && fresh.as_ref() == self.snapshot.as_ref()
        {
            self.stalls += 1;
        } else {
            self.stalls = 0;
        }
        if self.stalls >= STALL_LIMIT {
            self.stop(VoiceStopCause::Stall);
        }
        if self.view.ended.is_some() {
            self.view.phase = VoicePhase::Stopped;
        }
        if result == VoiceOutcome::Unknown {
            self.view.ended = Some(VoiceStopCause::Stop);
        }
        if let Some(s) = fresh {
            self.snapshot = Some(s);
        }
    }
    pub fn fail(&mut self, e: &Error) {
        self.view.phase = VoicePhase::Refused;
        self.view.understood = e.to_string();
        self.record(VoiceOutcome::Failed, e.to_string(), None);
    }
    fn record(&mut self, outcome: VoiceOutcome, detail: String, action: Option<VoiceAction>) {
        if self.view.steps.last().is_some_and(|step| {
            outcome != VoiceOutcome::Assessed
                && step.utterance_id == self.utterance
                && step.outcome == VoiceOutcome::Assessed
        }) {
            self.view.steps.pop();
        }
        self.view.steps.push(VoiceStep {
            utterance_id: self.utterance.clone(),
            transcript: self.view.transcript.clone(),
            at: self.clock.now(),
            answers: self.answer.as_ref().map(|a| a.answers.clone()),
            action,
            outcome,
            detail,
            jev_usage: self.answer.as_ref().map(|a| a.usage.clone()),
        });
    }
    pub fn superseded(&mut self, utterance: &str, text: String, answer: Option<VoiceAnswer>) {
        self.view.steps.push(VoiceStep {
            utterance_id: utterance.into(),
            transcript: text,
            at: self.clock.now(),
            answers: answer.as_ref().map(|a| a.answers.clone()),
            action: None,
            outcome: VoiceOutcome::Superseded,
            detail: "Replaced by a newer command; no input sent".into(),
            jev_usage: answer.map(|a| a.usage),
        });
    }
    pub fn silence_remaining(&self) -> Duration {
        SILENCE.saturating_sub(
            self.clock
                .monotonic()
                .saturating_duration_since(self.changed),
        )
    }
}
