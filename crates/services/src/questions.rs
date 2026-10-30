// SPDX-License-Identifier: Apache-2.0
//! Questions, as the person has them now. An agent checks with `can_ask`
//! before it asks anything, since the person can turn questions off or on
//! while it works. One that asks while they're off gets its question
//! straight back, with word to answer it itself or hold it for the summary
//! that ends its turn, and the person isn't interrupted.

use crate::{CompanionHub, carry_on::CARRY_ON_AFTER};
use ports::harness::HOLD_QUESTIONS;
use protocol::{
    Event, ProtocolError, RunId, TaskId,
    model::{TimelineBody, TimelineItem},
};

impl CompanionHub {
    /// Whether the person has questions on now.
    pub(crate) fn questions_on(&self) -> Result<bool, ProtocolError> {
        Ok(self.read(Self::agent_preferences)?.questions)
    }

    /// What `can_ask` tells the agent.
    pub(crate) fn can_ask(&self) -> Result<String, ProtocolError> {
        if !self.questions_on()? {
            return Ok(HOLD_QUESTIONS.into());
        }
        Ok(format!(
            "Questions are on: ask the person when you need to. If nobody answers in {} minutes, the question comes back to you; then finish what doesn't depend on it and end your turn.",
            CARRY_ON_AFTER.as_secs() / 60
        ))
    }

    /// Notes in the timeline that `run_id`'s question, `title`, went back to
    /// its agent unasked.
    pub(crate) fn held_question(
        &self,
        task_id: TaskId,
        run_id: RunId,
        title: &str,
    ) -> Result<(), ProtocolError> {
        let now = self.inner.clock.now();
        self.write(|tx| {
            tx.emit(Event::TimelineAppended {
                task_id,
                item: TimelineItem {
                    seq: 0,
                    at: now,
                    run_id: Some(run_id),
                    item: TimelineBody::Note {
                        text: format!(
                            "Questions are off, so Ophio gave the agent its question back, to answer itself or keep for the end of its turn: “{}”",
                            title.trim()
                        ),
                    },
                },
            })?;
            Ok(())
        })
    }
}
