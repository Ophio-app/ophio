// SPDX-License-Identifier: Apache-2.0
//! What a chat's suggestion may start as. A task started from a suggestion
//! (`CreateTask.origin`) by the card's Start must be exactly what its card
//! showed. The phone works the same out to draw the card (`workCards.ts`);
//! the computer works it out again from the suggestion it keeps and refuses
//! any other, so neither the phone nor the agent can widen what the tap
//! approves.
//!
//! The suggestion's, and compared: the project, the kind of task (a plain
//! coding task, not a conversation), the title and the brief word for word,
//! the mode, the GitHub route (an issue's own number, a change's own pull
//! request or none) with no per-task GitHub setting the card didn't show, and
//! for a question the least access it needs. Nothing else rides along: no
//! branch, attachments, launcher, folder, stopping point, screen, review role
//! or browser setting.
//!
//! The person's, shown on the card and not compared: the agent, model,
//! effort and permission setting (their usual choice for that kind of work,
//! remembered on the phone only, or what they picked on the card), and when
//! it starts (now, or in Next up, whose own rules still hold it).
//!
//! A card the person opened in Review and started from there (`reviewed`)
//! isn't compared at all: they saw and could change every part of it. It is
//! still that card's task: started once, linked to the chat, and only by
//! the person.

use protocol::ProjectId;
use protocol::access::ConversationAccess;
use protocol::command::Command;
use protocol::github::{GithubLink, LaunchChoice};
use protocol::model::{NewWork, Project, TaskKind, TaskMode};

pub(crate) const DIFFERS: &str = "What would start differs from the suggestion you saw.";
/// A card's Start never resumes a Next up the person paused.
pub(crate) const PAUSED: &str = "Next up is paused, so this can't start by itself. Start it again from the card: it waits in Next up until you start it.";

/// The one project named `name`, as the card finds it: by name, ignoring
/// case, and only where exactly one project that isn't a one-off has it.
fn by_name(name: &str, projects: &[Project]) -> Option<ProjectId> {
    let name = name.trim().to_lowercase();
    let mut found = projects
        .iter()
        .filter(|p| !p.one_off && p.name.trim().to_lowercase() == name);
    match (found.next(), found.next()) {
        (Some(p), None) => Some(p.project_id),
        _ => None,
    }
}

/// The task mode a suggestion starts in: a question is Ask, a plan is Plan,
/// an issue is fixed in Work or planned first, anything else is Work.
pub(crate) fn mode_of(w: &NewWork) -> TaskMode {
    match (w.issue, w.mode) {
        (Some(_), TaskMode::Plan) => TaskMode::Plan,
        (Some(_), _) => TaskMode::Work,
        (None, m) => m,
    }
}

/// The least a question needs: its project, the others it names to read,
/// nothing to change, and the internet or the person's own network only when
/// it says so. Reaching the own network goes through the internet.
pub(crate) fn least_access(
    w: &NewWork,
    chat: ProjectId,
    projects: &[Project],
) -> Result<ConversationAccess, &'static str> {
    let needs = w.needs.clone().unwrap_or_default();
    let mut read = vec![chat];
    for name in &needs.read {
        read.push(by_name(name, projects).ok_or(DIFFERS)?);
    }
    let local = needs.local_network;
    Ok(ConversationAccess {
        read,
        write: Vec::new(),
        whole_read: needs.whole_computer,
        whole_write: false,
        internet: needs.internet || local,
        local_network: local,
    }
    .normalized())
}

/// Whether `create` starts exactly suggestion `w` from a chat in `chat`.
/// `reviewer` is the project's own stored reviewer, the only one a fix may
/// name. `Err` says why not, in the person's words.
pub(crate) fn check(
    w: &NewWork,
    chat: ProjectId,
    projects: &[Project],
    reviewer: Option<&LaunchChoice>,
    create: &Command,
) -> Result<(), &'static str> {
    // Every field is named, so one added later doesn't compile until it is
    // either compared or said to be the person's.
    let Command::CreateTask {
        project_id,
        kind,
        // The card itself, and turning a chat into a task, are the caller's.
        origin: _,
        from_chat: _,
        mode,
        title,
        prompt,
        bundle,
        // The person's: agent, model, effort and permission setting.
        harness: _,
        model: _,
        effort: _,
        launch_policy: _,
        agent_chosen: _,
        branch,
        start,
        place,
        browser,
        conversation,
        access,
        github,
        mission,
        target_id,
        // The person's: now or in Next up, as the card says; a paused Next up
        // still holds it (the caller's).
        placement: _,
        review,
    } = create
    else {
        return Err(DIFFERS);
    };
    // Fixing every issue starts as Run all, not as one task.
    if w.all_issues {
        return Err(DIFFERS);
    }
    // Why is on the card already.
    if w.refused.is_some() {
        return Err("This suggestion can’t start as written. Open it in Review instead.");
    }
    let needs = w.needs.clone().unwrap_or_default();
    // The desktop chooses its screen first, and another project starts from
    // Review: neither is one tap from a card.
    if needs.desktop {
        return Err(DIFFERS);
    }
    if let Some(name) = &needs.changes
        && by_name(name, projects) != Some(chat)
    {
        return Err(DIFFERS);
    }
    // Only the chat's own project, as a plain task of the suggestion's words.
    if *project_id != chat
        || *kind != TaskKind::Coding
        || conversation.unwrap_or(false)
        || bundle.is_some()
        || branch.is_some()
        || start.is_some()
        || place.is_some()
        || mission.is_some()
        || target_id.is_some()
        || review.is_some()
        || browser.is_some()
        || prompt != &w.brief
        || title.as_deref() != Some(w.title.as_str())
    {
        return Err(DIFFERS);
    }
    let want = mode_of(w);
    if mode.unwrap_or(TaskMode::Work) != want {
        return Err(DIFFERS);
    }
    // Its route: an issue's own branch and pull request, a change's own pull
    // request or none, and nothing for a question or a plan. A fix carries
    // no GitHub setting of its own the card didn't show: never whether it
    // merges by itself, and no reviewer but the project's own.
    let route_ok = match (w.issue, github.as_deref()) {
        (
            Some(n),
            Some(GithubLink::Issue {
                number,
                review_with,
                when_green,
            }),
        ) => {
            *number == n
                && when_green.is_none()
                && review_with.as_ref().is_none_or(|r| Some(r) == reviewer)
        }
        (Some(_), _) => false,
        (None, None) => true,
        (None, Some(GithubLink::NewPr)) => want == TaskMode::Work,
        (None, Some(_)) => false,
    };
    if !route_ok {
        return Err(DIFFERS);
    }
    // A question runs held to exactly the least it needs; a plan is held
    // read-only by its mode, and a change by the person's permission setting.
    match (want, access) {
        (TaskMode::Ask, Some(a)) if a.clone().normalized() == least_access(w, chat, projects)? => {
            Ok(())
        }
        (TaskMode::Plan | TaskMode::Work, None) => Ok(()),
        _ => Err(DIFFERS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::capability::HarnessKind;
    use protocol::command::TaskPlacement;
    use protocol::queue::QueueAuthorization;

    fn work(mode: TaskMode, issue: Option<u64>) -> NewWork {
        NewWork {
            title: "Fix it".into(),
            brief: "Fix the thing.".into(),
            mode,
            issue,
            needs: None,
            refused: None,
            after: None,
            all_issues: false,
        }
    }

    fn create(chat: ProjectId, w: &NewWork, mode: TaskMode, github: Option<GithubLink>) -> Command {
        Command::CreateTask {
            origin: None,
            from_chat: None,
            github: github.map(Box::new),
            agent_chosen: Some(true),
            mode: Some(mode),
            project_id: chat,
            kind: TaskKind::Coding,
            title: Some(w.title.clone()),
            prompt: w.brief.clone(),
            bundle: None,
            harness: HarnessKind::ClaudeCode,
            model: None,
            effort: None,
            launch_policy: None,
            branch: None,
            mission: None,
            target_id: None,
            placement: TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            },
            start: None,
            place: None,
            browser: None,
            conversation: None,
            access: None,
            review: None,
        }
    }

    /// A suggestion's route on GitHub is exactly the one its card showed.
    #[test]
    fn the_github_route_is_the_suggestions_own() {
        let chat = ProjectId::new();
        let issue = |number| {
            Some(GithubLink::Issue {
                number,
                review_with: None,
                when_green: None,
            })
        };
        let fix = work(TaskMode::Work, Some(12));
        let plan_fix = work(TaskMode::Plan, Some(12));
        let change = work(TaskMode::Work, None);
        let plan = work(TaskMode::Plan, None);
        let ask = work(TaskMode::Ask, None);
        let cases: Vec<(&str, &NewWork, TaskMode, Option<GithubLink>, bool)> = vec![
            (
                "an issue on its own issue",
                &fix,
                TaskMode::Work,
                issue(12),
                true,
            ),
            (
                "an issue planned first",
                &plan_fix,
                TaskMode::Plan,
                issue(12),
                true,
            ),
            (
                "an issue planned when it was to be fixed",
                &fix,
                TaskMode::Plan,
                issue(12),
                false,
            ),
            (
                "an issue on another issue",
                &fix,
                TaskMode::Work,
                issue(13),
                false,
            ),
            (
                "an issue without its issue",
                &fix,
                TaskMode::Work,
                None,
                false,
            ),
            (
                "an issue as a new pull request",
                &fix,
                TaskMode::Work,
                Some(GithubLink::NewPr),
                false,
            ),
            (
                "a change with its own pull request",
                &change,
                TaskMode::Work,
                Some(GithubLink::NewPr),
                true,
            ),
            ("a change without one", &change, TaskMode::Work, None, true),
            (
                "a change on an issue it didn't name",
                &change,
                TaskMode::Work,
                issue(99),
                false,
            ),
            (
                "a plan opening a pull request",
                &plan,
                TaskMode::Plan,
                Some(GithubLink::NewPr),
                false,
            ),
            (
                "a question opening a pull request",
                &ask,
                TaskMode::Ask,
                Some(GithubLink::NewPr),
                false,
            ),
        ];
        for (what, w, mode, github, starts) in cases {
            let access = (mode == TaskMode::Ask).then(|| ConversationAccess {
                read: vec![chat],
                write: vec![],
                whole_read: false,
                whole_write: false,
                internet: false,
                local_network: false,
            });
            let mut c = create(chat, w, mode, github);
            if let Command::CreateTask { access: a, .. } = &mut c {
                *a = access;
            }
            assert_eq!(check(w, chat, &[], None, &c).is_ok(), starts, "{what}");
        }
    }

    /// A fix started from its card carries no GitHub setting the card didn't
    /// show: not merging by itself, and no reviewer but the project's own.
    #[test]
    fn a_fix_carries_only_the_projects_own_github_settings() {
        let chat = ProjectId::new();
        let fix = work(TaskMode::Work, Some(12));
        let choice = |model: &str| LaunchChoice {
            harness: HarnessKind::Codex,
            model: Some(model.into()),
            effort: None,
            launch_policy: None,
        };
        let ours = choice("ours");
        let link = |review_with, when_green| {
            Some(GithubLink::Issue {
                number: 12,
                review_with,
                when_green,
            })
        };
        let cases: Vec<(&str, Option<GithubLink>, Option<&LaunchChoice>, bool)> = vec![
            ("no reviewer", link(None, None), Some(&ours), true),
            (
                "the project's reviewer",
                link(Some(ours.clone()), None),
                Some(&ours),
                true,
            ),
            (
                "another reviewer",
                link(Some(choice("theirs")), None),
                Some(&ours),
                false,
            ),
            (
                "a reviewer the project has none of",
                link(Some(ours.clone()), None),
                None,
                false,
            ),
            (
                "merging by itself",
                link(None, Some(protocol::github::WhenGreen::Merge)),
                Some(&ours),
                false,
            ),
            (
                "waiting for the person",
                link(None, Some(protocol::github::WhenGreen::Leave)),
                Some(&ours),
                false,
            ),
        ];
        for (what, github, reviewer, starts) in cases {
            let c = create(chat, &fix, TaskMode::Work, github);
            assert_eq!(
                check(&fix, chat, &[], reviewer, &c).is_ok(),
                starts,
                "{what}"
            );
        }
    }

    /// Nothing the card didn't show rides along, the browser setting included.
    #[test]
    fn nothing_else_rides_along() {
        let chat = ProjectId::new();
        let change = work(TaskMode::Work, None);
        let mut c = create(chat, &change, TaskMode::Work, None);
        assert!(check(&change, chat, &[], None, &c).is_ok());
        if let Command::CreateTask { browser, .. } = &mut c {
            *browser = Some(true);
        }
        assert_eq!(check(&change, chat, &[], None, &c), Err(DIFFERS));
    }
}
