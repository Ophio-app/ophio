// SPDX-License-Identifier: Apache-2.0
//! An agent asks for more than it has with `request_access`, and the person
//! answers one card. In a conversation (or a task carried on from one) Allow
//! turns on exactly what it asked for, as the access bar would: within the
//! same turn when the sandbox can widen as it runs (the network, a project
//! it may now change), otherwise in a fresh turn of the same session. In a
//! Plan or Ask task the way forward is Switch to Work ([`crate::switch_work`]).
//! "Not now" tells the agent to finish without it, and it isn't asked again
//! in that turn.
use crate::{CompanionHub, api::Caller, error};
use protocol::{
    access::ConversationAccess,
    command::ApprovalDecision,
    model::{
        AccessNeed, AccessRequest, Approval, ApprovalFact, ApprovalKind, ApprovalState, Project,
        Run, Task, TaskMode,
    },
    *,
};

/// What each run's person declined, by run: the kind and exact request of
/// each, so an identical request in the same turn is declined at once.
pub(crate) const DECLINED: &str = "run_declined";

/// One request the person declined in a run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Declined {
    pub kind: ApprovalKind,
    /// An access request: asking for any of its needs is asking again.
    #[serde(default)]
    pub access: bool,
    /// [`Declined::request_of`]: what it asked, with the tool it asked of.
    pub request: String,
    /// What an access request asked for.
    #[serde(default)]
    pub needs: Vec<AccessNeed>,
}

impl Declined {
    /// What makes two requests the same: the tool (which the title names)
    /// and the exact input.
    pub(crate) fn request_of(title: &str, raw: Option<&str>) -> String {
        format!("{title}\n{}", raw.unwrap_or_default())
    }

    pub(crate) fn of(a: &Approval) -> Self {
        Self {
            kind: a.kind,
            access: a.access_request.is_some(),
            request: Self::request_of(&a.title, a.raw_request.as_deref()),
            needs: (a.access_request.as_ref())
                .map(|r| r.needs.clone())
                .unwrap_or_default(),
        }
    }
}

pub(crate) use ports::harness::{DECLINED_TEXT, declined_text};
/// What the agent is told when it asks again for what was declined.
pub(crate) const ASKED_AGAIN: &str = "The person already declined this in this turn, so Ophio declined it again without asking them. Don't ask for it again in this turn: finish what you can without it, then say plainly what's left and what it needs.";

/// Whether `request` was already declined in its run.
pub(crate) fn declined_before(
    r: &store::Read<'_>,
    run: RunId,
    request: &Declined,
) -> store::Result<bool> {
    let declined: Vec<Declined> = r.get(DECLINED, run)?.unwrap_or_default();
    Ok(declined.iter().any(|d| {
        d.kind == request.kind
            && d.access == request.access
            && match request.access {
                // Asking again for some of what was declined is asking again.
                true => request.needs.iter().all(|n| d.needs.contains(n)),
                false => d.request == request.request,
            }
    }))
}

/// Records that the person declined `a` in its run.
pub(crate) fn note_declined(tx: &mut store::Write<'_>, a: &Approval) -> store::Result<()> {
    let mut declined: Vec<Declined> = tx.read().get(DECLINED, a.run_id)?.unwrap_or_default();
    let this = Declined::of(a);
    if !declined.contains(&this) && declined.len() < 64 {
        declined.push(this);
        tx.put(DECLINED, a.run_id, &declined)?;
    }
    Ok(())
}

/// Which of `needs` `access` doesn't give a run in `project`.
pub(crate) fn missing(
    access: &ConversationAccess,
    project: ProjectId,
    needs: &[AccessNeed],
) -> Vec<AccessNeed> {
    let access = access.clone().normalized();
    let mut missing: Vec<AccessNeed> = (needs.iter().copied())
        .filter(|need| match need {
            AccessNeed::ChangeFiles => !(access.whole_write || access.write.contains(&project)),
            AccessNeed::Internet => !access.internet,
            AccessNeed::LocalNetwork => {
                !(access.internet && (access.local_network || access.unrestricted()))
            }
            AccessNeed::WholeComputer => !(access.whole_read && access.whole_write),
        })
        .collect();
    // The person's own network is reached only with the internet: asking
    // for it asks for both.
    if missing.contains(&AccessNeed::LocalNetwork) && !access.internet {
        missing.push(AccessNeed::Internet);
    }
    missing.sort();
    missing.dedup();
    missing
}

/// How a card's warning that Allow lifts every limit begins.
const LIFTS: &str = "lifts this ";

/// Why an Allow is refused once the access has changed under its card.
pub(crate) const ACCESS_CHANGED: &str = "The access has changed since the agent asked, so Allow would no longer do just what this card says. Nothing was changed, and the request was withdrawn. If it's still needed, ask the agent to request it again.";

/// What the agent hears when its request is withdrawn for that.
pub(crate) const WITHDRAWN: &str = "Ophio withdrew this request without an answer: the access changed while you waited, so its card no longer said what Allow would do. If you still need it, ask again.";

/// The access an Allow of `a` gives from `access`, if that is no more than
/// its card showed: nothing it didn't list, and every limit lifted only if
/// the card warned of that. The access may have changed since it was asked.
pub(crate) fn as_shown(
    a: &Approval,
    access: &ConversationAccess,
    project: ProjectId,
) -> Option<ConversationAccess> {
    let shown = (a.access_request.as_ref())
        .map(|r| r.needs.as_slice())
        .unwrap_or_default();
    let turns_on = missing(access, project, shown);
    if !turns_on.iter().all(|need| shown.contains(need)) {
        return None;
    }
    let widened = widen(access, project, &turns_on);
    let warned = (a.facts.iter()).any(|f| f.risky && f.text.starts_with(LIFTS));
    if !access.unrestricted() && widened.unrestricted() && !warned {
        return None;
    }
    Some(widened)
}

/// `access` with exactly `needs` turned on for a run in `project`, and
/// nothing else.
pub(crate) fn widen(
    access: &ConversationAccess,
    project: ProjectId,
    needs: &[AccessNeed],
) -> ConversationAccess {
    let mut access = access.clone();
    for need in needs {
        match need {
            AccessNeed::ChangeFiles => access.write.push(project),
            AccessNeed::Internet => access.internet = true,
            AccessNeed::LocalNetwork => {
                access.internet = true;
                access.local_network = true;
            }
            AccessNeed::WholeComputer => {
                access.whole_read = true;
                access.whole_write = true;
            }
        }
    }
    access.normalized()
}

/// `needs` in words, as in "change files in ophio and reach the internet".
pub(crate) fn needs_text(needs: &[AccessNeed], project: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    // The local network includes the internet: say it once.
    let local = needs.contains(&AccessNeed::LocalNetwork);
    for need in needs {
        parts.push(match need {
            AccessNeed::ChangeFiles => format!("change files in {project}"),
            AccessNeed::Internet if local => continue,
            AccessNeed::Internet => "reach the internet".into(),
            AccessNeed::LocalNetwork if needs.contains(&AccessNeed::Internet) => {
                "reach the internet and your own network".into()
            }
            AccessNeed::LocalNetwork => "reach your own network".into(),
            AccessNeed::WholeComputer => "read and change anything on this computer".into(),
        });
    }
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The card's one line: "Claude Code needs to change files in ophio and
/// reach your own network to build the Mac app."
pub(crate) fn title(agent: &str, needs: &[AccessNeed], project: &str, reason: &str) -> String {
    let reason = reason.trim().trim_end_matches(['.', '!']);
    let lower = reason.to_lowercase();
    let joined = if ["to ", "so ", "for ", "because ", "in order "]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        format!(" {reason}")
    } else if reason.is_empty() {
        String::new()
    } else {
        format!(": {reason}")
    };
    format!("{agent} needs to {}{joined}.", needs_text(needs, project))
}

/// The agent's needs and reason, as asked.
fn tidy(needs: Vec<AccessNeed>) -> Vec<AccessNeed> {
    let mut needs = needs;
    needs.sort();
    needs.dedup();
    needs
}

impl CompanionHub {
    /// The agent of `run_id` asks for `needs`, for `reason`: what it hears
    /// back.
    pub(crate) async fn access_request(
        &self,
        run_id: RunId,
        task: &Task,
        needs: Vec<AccessNeed>,
        reason: String,
    ) -> Result<String, ProtocolError> {
        let needs = tidy(needs);
        // The agent's words read as written: a direction control shows as
        // its escape, so it can't reorder the card around it.
        let reason = ports::tool_approval::visible(&reason);
        let (run, project): (Run, Project) = self.read(|r| {
            Ok((
                r.require("run", run_id)?,
                r.require("project", task.project_id)?,
            ))
        })?;
        let mode = crate::runs::run_mode(&run, task);
        let missing = match task.access.as_ref() {
            Some(access) => missing(access, task.project_id, &needs),
            None if mode == TaskMode::Work => Vec::new(),
            None if !self.read(|r| crate::approval_rule::switchable_in(r, task, mode))? => {
                return Ok("This task can't be given more from here: it only reads, and its plan is reviewed before anything is changed. Finish what you can by reading, then say plainly what's left and what it needs.".into());
            }
            // A read-only task gets all of it by switching to Work.
            None => needs.clone(),
        };
        if missing.is_empty() {
            if needs
                .iter()
                .any(|need| matches!(need, AccessNeed::ChangeFiles | AccessNeed::WholeComputer))
                && let Some((note, _)) = self.writer_note(task, run_id, false)?
            {
                return Ok(note);
            }
            return Ok(match task.access {
                Some(_) => format!(
                    "You already have that: you may {}. If something is still refused, say exactly what failed and why.",
                    needs_text(&needs, &project.name)
                ),
                None => "You're in Work mode, which already has that, as far as your own permissions allow. If something is still refused, say exactly what failed and why.".into(),
            });
        }
        let asked = Declined {
            kind: ApprovalKind::Other,
            access: true,
            request: String::new(),
            needs: missing.clone(),
        };
        if self.read(|r| declined_before(r, run_id, &asked))? {
            return Ok(ASKED_AGAIN.into());
        }
        let agent = crate::agent_browser::harness_name(task.harness);
        let held = task.access.is_some();
        let what = match task.conversation {
            true => "this chat",
            false => "this task",
        };
        // With the whole computer and the internet, nothing holds it in: it
        // reaches what the computer reaches, the person's network included.
        let unheld = (task.access.as_ref()).is_some_and(|a| {
            !a.unrestricted() && widen(a, task.project_id, &missing).unrestricted()
        });
        let scope = match (held, unheld) {
            (true, true) => format!(
                "Lifts this {}'s limits: it can read and change anything on this computer and reach whatever the computer reaches, your own network included.",
                if task.conversation { "chat" } else { "task" }
            ),
            (true, false) => format!(
                "Turns on, for {what}: {}. Nothing else changes.",
                capitalized(&needs_text(&missing, &project.name))
            ),
            (false, _) => format!(
                "Switches this task to Work mode, in the same session: {agent} can then change files and run commands in {}, as far as its own permissions allow.",
                project.name
            ),
        };
        // What the card says on its face, beside the agent's line: what it
        // can't say itself, and what deserves a second look.
        let fact = |text: &str, risky: bool| ApprovalFact {
            text: text.into(),
            risky,
        };
        let facts = match (held, unheld) {
            (true, true) => vec![fact(
                &format!(
                    "{LIFTS}{}'s limits, so it reaches whatever this computer reaches, your own network included",
                    if task.conversation { "chat" } else { "task" }
                ),
                true,
            )],
            (true, false) if missing.contains(&AccessNeed::WholeComputer) => {
                vec![fact(
                    "lets it read and change anything on this computer",
                    true,
                )]
            }
            (true, false) => Vec::new(),
            (false, _) => vec![fact(
                "switches this task to Work mode, in the same session",
                false,
            )],
        };
        let id = ApprovalId::new();
        let now = self.inner.clock.now();
        let approval = Approval {
            approval_id: id,
            run_id,
            task_id: task.task_id,
            project_id: task.project_id,
            native_request_id: format!("bridge:{id}"),
            kind: ApprovalKind::Other,
            title: title(agent, &missing, &project.name, &reason),
            headline: None,
            facts,
            detail: None,
            detail_lines: Vec::new(),
            questions: Vec::new(),
            raw_request: None,
            scope,
            shortened: false,
            revision: 1,
            state: ApprovalState::Pending,
            access_request: Some(AccessRequest {
                needs: missing.clone(),
                reason,
            }),
            actions: None,
            delivery: None,
            outcome: None,
            requested_at: now,
            // No deadline of its own: unanswered, it goes back to the agent
            // like any request that carries on, and stays open after.
            expires_at: None,
            resolved_by: None,
            resolved_at: None,
        };
        let decision = match self.bridge_wait_on(approval, None).await {
            Ok(decision) => decision,
            Err(e) if e.code == ErrorCode::Expired => {
                return Ok("Nobody answered in time. Don't ask again in this turn: finish what you can without it, then say plainly what's left and what it needs.".into());
            }
            Err(e) => return Err(e),
        };
        if matches!(decision, ApprovalDecision::Allow) && held {
            let current: Task = self.read(|r| r.require("task", task.task_id))?;
            if let Some((note, _)) = self.writer_note(&current, run_id, false)? {
                return Ok(format!(
                    "The person allowed the access you asked for, but this reply keeps its temporary limits. Newly allowed project changes must wait for a later reply.\n\n{note}"
                ));
            }
        }
        Ok(match decision {
            ApprovalDecision::Allow => match held {
                true => format!(
                    "The person allowed it: you may now {}. Carry on.",
                    needs_text(&missing, &project.name)
                ),
                // Switching to Work ends this turn; it is never answered
                // Allow while it lasts.
                false => "The person agreed. Ophio is switching this task to Work mode: stop here, and your next turn carries on in this session.".into(),
            },
            // Nobody has answered yet: the answer comes as its next message.
            // Or its card no longer said what Allow would do.
            ApprovalDecision::Deny { reason }
                if matches!(reason.as_deref(), Some(crate::carry_on::GUIDANCE | WITHDRAWN)) =>
            {
                reason.unwrap_or_default()
            }
            ApprovalDecision::Deny { reason } => match reason.as_deref().map(str::trim) {
                Some(reason) if !reason.is_empty() => {
                    format!("The person said not now: {reason}. {DECLINED_TEXT}")
                }
                _ => format!("The person said not now. {DECLINED_TEXT}"),
            },
            ApprovalDecision::Answer { .. } => {
                return Err(error(
                    ErrorCode::InvalidRequest,
                    "An access request is allowed or declined",
                ));
            }
        })
    }

    /// The person's decision on an access request, once admitted: a chat's
    /// Allow widens its sandbox first, so the agent finds what it was told
    /// it has, and a switch to Work carries on in a new turn.
    pub(crate) async fn resolve_access(
        &self,
        caller: &Caller,
        id: ApprovalId,
        decision: &ApprovalDecision,
        command: CommandId,
    ) -> Result<(), ProtocolError> {
        if self
            .read(|r| r.get::<crate::switch_work::Switch>(crate::switch_work::SWITCH, command))?
            .is_some()
        {
            return self.switch_effect(command).await;
        }
        let a: Approval = self.read(|r| r.require("approval", id))?;
        if !matches!(decision, ApprovalDecision::Allow) {
            return self.resolve_approval(caller, id, decision).await;
        }
        let task: Task = self.read(|r| r.require("task", a.task_id))?;
        if task.access.is_none() {
            return self.resolve_approval(caller, id, decision).await;
        }
        let needs = (a.access_request.as_ref())
            .map(|r| r.needs.clone())
            .unwrap_or_default();
        let project: Project = self.read(|r| r.require("project", task.project_id))?;
        let text = format!(
            "Ophio stopped your previous turn to give you what you asked for: you may now {}. Commands you started in it have ended: check what finished before starting any again, then carry on.",
            needs_text(&needs, &project.name)
        );
        match self
            .apply_access(a.task_id, command, Some((caller, id, text)))
            .await?
        {
            crate::access::AccessApplied::Restarted => Ok(()),
            crate::access::AccessApplied::Live => self.resolve_approval(caller, id, decision).await,
        }
    }
}

impl CompanionHub {
    /// Withdraws access request `id` if Allow would no longer do just what
    /// its card says ([`as_shown`]), so it doesn't sit there unusable. Its
    /// agent, if still waiting, is told; otherwise its turn has moved on.
    pub(crate) async fn withdraw_if_changed(&self, id: ApprovalId) {
        let now = self.inner.clock.now();
        let withdrawn = self.write(|tx| {
            let Some(mut a) = tx.read().get::<Approval>("approval", id)? else {
                return Ok(false);
            };
            let task: Task = tx.read().require("task", a.task_id)?;
            let changed = a.state == ApprovalState::Pending
                && a.access_request.is_some()
                && (tx.read().get::<CommandId>("approval_claim", id)?).is_none()
                && (task.access.as_ref())
                    .is_some_and(|access| as_shown(&a, access, task.project_id).is_none());
            if !changed {
                return Ok(false);
            }
            tx.delete(crate::carry_on::CARRIED_ON, id)?;
            a.state = ApprovalState::Cancelled;
            a.revision += 1;
            a.resolved_at = Some(now);
            tx.put("approval", id, &a)?;
            tx.emit(Event::ApprovalUpserted {
                approval: a.clone(),
            })?;
            crate::runs::resolve_attention(tx, id, now)?;
            crate::carry_on::back_to_work(tx, a.run_id, now)?;
            crate::carry_on::note(
                tx,
                &a,
                now,
                format!(
                    "The access changed after the agent asked “{}”, so the request was withdrawn and nothing was changed.",
                    a.title
                ),
            )?;
            Ok(true)
        });
        if withdrawn.unwrap_or(false)
            && let Some(wait) = self.inner.waits.lock().await.remove(&id)
        {
            let _ = wait.send(ApprovalDecision::Deny {
                reason: Some(WITHDRAWN.into()),
            });
        }
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectId {
        ProjectId::new()
    }

    /// Allow gives exactly what was asked, and nothing more.
    #[test]
    fn allow_turns_on_exactly_what_was_asked() {
        let here = project();
        let other = project();
        let base = ConversationAccess {
            read: vec![other],
            internet: false,
            ..ConversationAccess::default()
        };
        let cases: &[(&[AccessNeed], ConversationAccess)] = &[
            (
                &[AccessNeed::ChangeFiles],
                ConversationAccess {
                    write: vec![here],
                    ..base.clone()
                },
            ),
            (
                &[AccessNeed::Internet],
                ConversationAccess {
                    internet: true,
                    ..base.clone()
                },
            ),
            (
                &[AccessNeed::LocalNetwork],
                ConversationAccess {
                    internet: true,
                    local_network: true,
                    ..base.clone()
                },
            ),
            (
                &[AccessNeed::WholeComputer],
                ConversationAccess {
                    whole_read: true,
                    whole_write: true,
                    ..base.clone()
                },
            ),
            (
                &[AccessNeed::ChangeFiles, AccessNeed::LocalNetwork],
                ConversationAccess {
                    write: vec![here],
                    internet: true,
                    local_network: true,
                    ..base.clone()
                },
            ),
        ];
        for (needs, expected) in cases {
            let widened = widen(&base, here, needs);
            assert_eq!(widened, expected.clone().normalized(), "{needs:?}");
            // Nothing it asked for is missing afterwards.
            assert!(missing(&widened, here, needs).is_empty(), "{needs:?}");
            // Another project is never made writable.
            assert!(!widened.write.contains(&other), "{needs:?}");
        }
    }

    #[test]
    fn only_what_is_missing_is_asked() {
        let here = project();
        let access = ConversationAccess {
            write: vec![here],
            ..ConversationAccess::default()
        };
        assert_eq!(
            missing(
                &access,
                here,
                &[
                    AccessNeed::ChangeFiles,
                    AccessNeed::Internet,
                    AccessNeed::LocalNetwork
                ]
            ),
            [AccessNeed::LocalNetwork]
        );
        assert!(
            missing(
                &widen(&access, here, &[AccessNeed::WholeComputer]),
                here,
                &[AccessNeed::WholeComputer]
            )
            .is_empty()
        );
    }

    #[test]
    fn the_card_says_it_in_one_line() {
        assert_eq!(
            title(
                "Claude Code",
                &[AccessNeed::ChangeFiles, AccessNeed::LocalNetwork],
                "ophio",
                "to build the Mac app on the MacBook."
            ),
            "Claude Code needs to change files in ophio and reach your own network to build the Mac app on the MacBook."
        );
        assert_eq!(
            title(
                "Codex",
                &[AccessNeed::Internet, AccessNeed::LocalNetwork],
                "site",
                "Fetch the release notes"
            ),
            "Codex needs to reach the internet and your own network: Fetch the release notes."
        );
        assert_eq!(
            needs_text(
                &[
                    AccessNeed::ChangeFiles,
                    AccessNeed::Internet,
                    AccessNeed::WholeComputer
                ],
                "app"
            ),
            "change files in app, reach the internet and read and change anything on this computer"
        );
    }

    #[test]
    fn a_declined_access_request_covers_asking_for_less() {
        let declined = Declined {
            kind: ApprovalKind::Other,
            access: true,
            request: String::new(),
            needs: vec![AccessNeed::ChangeFiles, AccessNeed::Internet],
        };
        let fewer = Declined {
            needs: vec![AccessNeed::Internet],
            ..declined.clone()
        };
        let more = Declined {
            needs: vec![AccessNeed::Internet, AccessNeed::WholeComputer],
            ..declined.clone()
        };
        let covers = |asked: &Declined| asked.needs.iter().all(|n| declined.needs.contains(n));
        assert!(covers(&fewer));
        assert!(!covers(&more));
    }
}
