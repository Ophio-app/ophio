// SPDX-License-Identifier: Apache-2.0
//! Which ways forward a pending approval has. `ResolveApproval` and
//! `SwitchToWork` admit a decision by [`Asked::refusal`] and
//! [`Asked::switch_refusal`], and the phone's buttons are
//! [`Asked::actions`], drawn from the same two, so the phone never offers a
//! button the computer would refuse.
use crate::error;
use protocol::{
    ErrorCode, ProtocolError,
    command::ApprovalDecision,
    model::{Approval, ApprovalAction, ApprovalKind, Run, Task, TaskKind, TaskMode},
};

/// What decides an approval's ways forward: what it asks for, and how its
/// run and task work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Asked {
    pub kind: ApprovalKind,
    /// The agent asks for more access (`Approval::access_request`).
    pub access: bool,
    /// Ophio's own gate, such as starting an outsider's task without
    /// project limits: never allowed by switching, and never again unasked.
    pub gate: bool,
    /// The mode its run works in ([`crate::runs::run_mode`]).
    pub mode: TaskMode,
    /// The mode its run's agent was really started in
    /// ([`crate::runs::launched_mode`]).
    pub launched: TaskMode,
    /// Its task is held to an access bar (a conversation, or a task carried
    /// on from one).
    pub held: bool,
    pub conversation: bool,
    /// The task can carry on in Work in this session: a conversation, or
    /// a Plan or Ask task whose run is its own turn (not a read-only side
    /// turn of a Work task), that isn't a finder or check that only reports.
    pub switchable: bool,
}

/// The message a refusal for a read-only task gives.
pub(crate) const READ_ONLY: &str =
    "This task only reads, so it can’t allow this. Switch it to Work to allow it.";
/// The message a refusal for a conversation gives.
pub(crate) const CHAT: &str = "A chat can’t allow this, because it may act outside the chat’s access. Turn the chat into a task to allow it.";

/// Whether `task` can carry on in Work in the same session, as of a run in
/// `mode`. Never one an outsider started (it began in Plan so its plan is
/// reviewed first, and carries on in Work only that way), nor one holding
/// outside content.
pub(crate) fn switchable(task: &Task, mode: TaskMode) -> bool {
    task.kind == TaskKind::Coding
        && !crate::github::writes_nothing(task)
        && !task.github.as_ref().is_some_and(|g| g.from_stranger)
        && (task.conversation
            || (matches!(task.mode, TaskMode::Plan | TaskMode::Ask) && mode == task.mode))
}

/// [`switchable`], and not holding content from outside the project.
pub(crate) fn switchable_in(
    r: &store::Read<'_>,
    task: &Task,
    mode: TaskMode,
) -> store::Result<bool> {
    Ok(switchable(task, mode) && !outsider(r, task)?)
}

/// Whether `task` read content from someone outside the project.
pub(crate) fn outsider(r: &store::Read<'_>, task: &Task) -> store::Result<bool> {
    Ok(task.github.as_ref().is_some_and(|g| g.from_stranger)
        || r.get::<bool>(crate::github_access::UNTRUSTED, task.task_id)?
            .unwrap_or(false))
}

/// Whether `a` is Ophio's own gate rather than the agent's request.
pub(crate) fn gate(r: &store::Read<'_>, a: &Approval) -> store::Result<bool> {
    Ok(
        r.get::<protocol::ApprovalId>(crate::github_access::START_APPROVAL, a.run_id)?
            == Some(a.approval_id),
    )
}

impl Asked {
    pub(crate) fn of(r: &store::Read<'_>, a: &Approval) -> store::Result<Self> {
        let task: Task = r.require("task", a.task_id)?;
        let (mode, launched) = match r.get::<Run>("run", a.run_id)? {
            Some(run) => (
                crate::runs::run_mode(&run, &task),
                crate::runs::launched_mode(r, &run, &task)?,
            ),
            None => (task.mode, task.mode),
        };
        Ok(Self {
            kind: a.kind,
            access: a.access_request.is_some(),
            gate: gate(r, a)?,
            mode,
            launched,
            held: task.access.is_some(),
            conversation: task.conversation,
            switchable: switchable_in(r, &task, mode)?,
        })
    }

    /// Why `decision` can't be taken, if it can't. `channel`: it comes from
    /// a chat app such as Telegram rather than the person's phone.
    pub(crate) fn refusal(
        &self,
        decision: &ApprovalDecision,
        channel: bool,
    ) -> Option<ProtocolError> {
        let deny = matches!(decision, ApprovalDecision::Deny { .. });
        let answer = matches!(decision, ApprovalDecision::Answer { .. });
        if deny {
            return None;
        }
        if self.access {
            if answer {
                return Some(error(
                    ErrorCode::InvalidRequest,
                    "Allow or decline this request for access.",
                ));
            }
            // Widening what holds the agent, or switching it to Work, is the
            // person's to do on their phone.
            if channel {
                return Some(error(
                    ErrorCode::PermissionDenied,
                    "Allow more access on your phone, not in a chat.",
                ));
            }
            // Held to an access bar, Allow turns on what it asked for; in
            // Work it has it all already. In a read-only task, Allow from a
            // phone that predates Switch to Work switches.
            return match self.held || self.mode == TaskMode::Work || self.switchable {
                true => None,
                false => Some(error(
                    ErrorCode::Unsupported,
                    "This task can’t be given more access.",
                )),
            };
        }
        match self.kind {
            // A code goes into the code prompt, which fills it.
            ApprovalKind::LoginCode => {
                return Some(error(
                    ErrorCode::InvalidRequest,
                    "Type the code in the code prompt.",
                ));
            }
            // A login, and a post on GitHub as the person, are allowed from
            // the phone.
            ApprovalKind::LoginUse | ApprovalKind::GithubPost => {
                let what = match self.kind {
                    ApprovalKind::LoginUse => "a login",
                    _ => "a post on GitHub",
                };
                if answer {
                    return Some(error(
                        ErrorCode::InvalidRequest,
                        format!("Allow or decline {what}."),
                    ));
                }
                if channel {
                    return Some(error(
                        ErrorCode::PermissionDenied,
                        format!("Allow {what} on your phone, not in a chat."),
                    ));
                }
            }
            _ => {}
        }
        // A run held to its access (a conversation's, say) starts as Work,
        // whatever its mode, held to that access: by a sandbox, or by
        // nothing when the access is the whole computer. A command it runs,
        // or a file it edits, stays within that access, so allowing it
        // gives no more than the access shows. Anything else may act
        // outside it: Ophio's own tools, logins and posts run on the
        // computer itself, and hosted web search doesn't pass through the
        // sandbox (a sandboxed run reaches the network with curl instead).
        let contained = self.launched == TaskMode::Work
            && matches!(self.kind, ApprovalKind::Command | ApprovalKind::FileEdit);
        let answering = self.kind == ApprovalKind::Question && answer;
        if self.mode != TaskMode::Work && !contained && !answering {
            return Some(error(
                ErrorCode::Unsupported,
                if self.conversation { CHAT } else { READ_ONLY },
            ));
        }
        None
    }

    /// Why `SwitchToWork` can't take this approval along, if it can't: only
    /// a request Allow can't take, from a task that can switch, goes with
    /// it. An access request from a read-only task is what it is for.
    pub(crate) fn switch_refusal(&self) -> Option<ProtocolError> {
        let wanted = match self.kind {
            // Ophio's own gates are answered as they are.
            _ if self.gate => false,
            _ if self.access => !self.held && self.mode != TaskMode::Work,
            ApprovalKind::LoginCode | ApprovalKind::Question => false,
            _ => self.refusal(&ApprovalDecision::Allow, false).is_some(),
        };
        match wanted && self.switchable {
            true => None,
            false => Some(error(
                ErrorCode::InvalidRequest,
                "This request doesn’t need the task switched to Work. Allow or decline it.",
            )),
        }
    }

    /// The ways forward to offer on the person's phone, in order.
    pub(crate) fn actions(&self) -> Vec<ApprovalAction> {
        let mut actions = Vec::new();
        match self.kind {
            ApprovalKind::Question => {
                let answer = ApprovalDecision::Answer {
                    text: String::new(),
                    answers: Vec::new(),
                };
                if self.refusal(&answer, false).is_none() {
                    actions.push(ApprovalAction::Answer);
                }
            }
            ApprovalKind::LoginCode => {}
            _ => {
                if self.switch_refusal().is_none() {
                    actions.push(match self.conversation {
                        true => ApprovalAction::TurnIntoTask,
                        false => ApprovalAction::SwitchToWork,
                    });
                } else if self.refusal(&ApprovalDecision::Allow, false).is_none() {
                    actions.push(ApprovalAction::Allow);
                }
            }
        }
        actions.push(ApprovalAction::Deny);
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ApprovalKind::*;
    use TaskMode::*;

    fn asked(
        kind: ApprovalKind,
        mode: TaskMode,
        launched: TaskMode,
        held: bool,
        conversation: bool,
        switchable: bool,
    ) -> Asked {
        Asked {
            kind,
            access: false,
            gate: false,
            mode,
            launched,
            held,
            conversation,
            switchable,
        }
    }

    /// An agent's request for more access, as stored: kind other.
    fn access(
        mode: TaskMode,
        launched: TaskMode,
        held: bool,
        conversation: bool,
        switchable: bool,
    ) -> Asked {
        Asked {
            access: true,
            ..asked(Other, mode, launched, held, conversation, switchable)
        }
    }

    const KINDS: [ApprovalKind; 10] = [
        Command,
        FileEdit,
        Network,
        Tool,
        Question,
        ComputerUse,
        LoginUse,
        LoginCode,
        GithubPost,
        Other,
    ];

    /// Every button offered is one the computer takes, and every decision
    /// it takes beyond Deny and an answer has its button.
    #[test]
    fn the_buttons_are_exactly_what_the_rule_admits() {
        for kind in KINDS {
            for mode in [Ask, Plan, Work] {
                for launched in [Ask, Plan, Work] {
                    for held in [false, true] {
                        for conversation in [false, true] {
                            for (switchable, access, gate) in
                                [false, true].into_iter().flat_map(|s| {
                                    [(s, false, false), (s, true, false), (s, false, true)]
                                })
                            {
                                if access && kind != Other {
                                    continue;
                                }
                                let a = Asked {
                                    access,
                                    gate,
                                    ..asked(kind, mode, launched, held, conversation, switchable)
                                };
                                let actions = a.actions();
                                let allows = a.refusal(&ApprovalDecision::Allow, false).is_none();
                                let switches = a.switch_refusal().is_none();
                                assert!(actions.contains(&ApprovalAction::Deny), "{a:?}");
                                assert_eq!(
                                    actions.contains(&ApprovalAction::SwitchToWork)
                                        || actions.contains(&ApprovalAction::TurnIntoTask),
                                    switches && kind != Question && kind != LoginCode,
                                    "{a:?}"
                                );
                                // Ophio's own gate is never switched past.
                                if gate {
                                    assert!(!switches, "{a:?}");
                                }
                                if actions.contains(&ApprovalAction::Allow) {
                                    assert!(allows, "{a:?}");
                                }
                                // Allow is shown wherever it works, except where
                                // switching is its meaning (an access request
                                // from a read-only task) or it's a question.
                                if allows && !switches && kind != Question {
                                    assert!(actions.contains(&ApprovalAction::Allow), "{a:?}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// A read-only run never has a change allowed without switching.
    #[test]
    fn a_read_only_run_cannot_allow_a_change() {
        for kind in [Command, FileEdit, Network, Tool, Other, ComputerUse] {
            for mode in [Ask, Plan] {
                let a = asked(kind, mode, mode, false, false, true);
                assert!(
                    a.refusal(&ApprovalDecision::Allow, false).is_some(),
                    "{a:?}"
                );
                assert_eq!(
                    a.actions(),
                    [ApprovalAction::SwitchToWork, ApprovalAction::Deny]
                );
                // A side turn of a Work task can't switch either: Deny only.
                let side = asked(kind, mode, mode, false, false, false);
                assert_eq!(side.actions(), [ApprovalAction::Deny]);
            }
        }
    }

    #[test]
    fn a_chat_allows_what_its_sandbox_holds_and_turns_into_a_task_for_the_rest() {
        let chat = |kind| asked(kind, Ask, Work, true, true, true);
        assert_eq!(
            chat(Command).actions(),
            [ApprovalAction::Allow, ApprovalAction::Deny]
        );
        assert_eq!(
            chat(FileEdit).actions(),
            [ApprovalAction::Allow, ApprovalAction::Deny]
        );
        assert_eq!(
            access(Ask, Work, true, true, true).actions(),
            [ApprovalAction::Allow, ApprovalAction::Deny]
        );
        for kind in [LoginUse, GithubPost, Network, Tool, Other] {
            assert_eq!(
                chat(kind).actions(),
                [ApprovalAction::TurnIntoTask, ApprovalAction::Deny],
                "{kind:?}"
            );
            assert!(
                chat(kind)
                    .refusal(&ApprovalDecision::Allow, false)
                    .is_some()
            );
        }
        assert_eq!(chat(LoginCode).actions(), [ApprovalAction::Deny]);
        // Access is never widened from a chat app.
        assert!(
            access(Ask, Work, true, true, true)
                .refusal(&ApprovalDecision::Allow, true)
                .is_some()
        );
    }

    #[test]
    fn an_access_request_in_a_read_only_task_switches_to_work() {
        let a = access(Plan, Plan, false, false, true);
        assert_eq!(
            a.actions(),
            [ApprovalAction::SwitchToWork, ApprovalAction::Deny]
        );
        let finder = access(Ask, Ask, false, false, false);
        assert_eq!(finder.actions(), [ApprovalAction::Deny]);
        assert!(finder.refusal(&ApprovalDecision::Allow, false).is_some());
    }

    /// Ophio's own gate on an outsider's Plan task (start without project
    /// limits) is answered as it is: declined, or allowed in Work, never
    /// switched past.
    #[test]
    fn ophio_s_own_gate_is_never_switched_past() {
        let gate = Asked {
            gate: true,
            ..asked(Other, Plan, Plan, false, false, true)
        };
        assert!(gate.switch_refusal().is_some());
        assert_eq!(gate.actions(), [ApprovalAction::Deny]);
    }
}
