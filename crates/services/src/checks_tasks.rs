// SPDX-License-Identifier: Apache-2.0
//! Requests to set up or change checks become ordinary project tasks. The
//! caller admits the returned command with the original command id, so normal
//! retry protection, launcher validation and run dispatch still apply.
use crate::{CompanionHub, error};
use protocol::{
    ErrorCode, ProjectId, ProtocolError,
    capability::{HarnessInfo, HarnessKind},
    command::{Command, TaskPlacement},
    launchers::{CatalogKind, CatalogSource, TaskStart},
    model::{Project, TaskKind, TaskMode},
};

impl CompanionHub {
    /// Build the task command before normal command preparation and admission.
    /// `request` carries the person's exact words and any selected-check context
    /// supplied by the caller. `checks_empty` means there are no approved checks,
    /// not merely that none are enabled. This does not admit a second command.
    pub(crate) async fn prepare_checks_task(
        &self,
        project_id: ProjectId,
        request: &str,
        checks_empty: bool,
    ) -> Result<Command, ProtocolError> {
        let (project, agents) = self.read(|r| {
            Ok((
                r.require::<Project>("project", project_id)?,
                r.get::<Vec<HarnessInfo>>("meta", "harnesses")?
                    .unwrap_or_default(),
            ))
        })?;
        // Do not silently replace a project's chosen agent when it is signed
        // out or unavailable. Ordinary admission explains why it cannot start.
        let harness = checks_agent(
            project.default_harness,
            agents
                .iter()
                .filter(|h| {
                    self.inner.harnesses.iter().any(|a| a.kind() == h.kind)
                        && crate::work_tools::ready(h).is_ok()
                        && domain::task_mode(h, TaskMode::Work).is_ok()
                })
                .map(|h| h.kind),
        )?;
        let (id, _) = checks_skill(request, checks_empty);
        let entry = self
            .catalog(Some(project_id))
            .await?
            .entries
            .into_iter()
            .find(|e| {
                e.kind == CatalogKind::Skill && e.source == CatalogSource::OphioSetup && e.id == id
            })
            .ok_or_else(|| {
                error(
                    ErrorCode::TemporarilyUnavailable,
                    "The skill for changing checks isn't available on this computer. Check Skills in the Library.",
                )
            })?;
        let start = TaskStart::Skill {
            source: entry.source,
            id: entry.id,
            digest: entry.digest,
        };
        Ok(checks_task(
            project_id,
            harness,
            start,
            request,
            checks_empty,
        ))
    }
}

fn checks_agent(
    default: Option<HarnessKind>,
    mut ready: impl Iterator<Item = HarnessKind>,
) -> Result<HarnessKind, ProtocolError> {
    default.or_else(|| ready.next()).ok_or_else(|| {
        error(
            ErrorCode::TemporarilyUnavailable,
            "No agent on this computer can start work now. Check Agents in Settings.",
        )
    })
}

fn checks_skill(request: &str, checks_empty: bool) -> (&'static str, &'static str) {
    if request.trim().is_empty() && checks_empty {
        ("set-up-checks", "Set up checks")
    } else {
        ("change-checks", "Change checks")
    }
}

fn checks_task(
    project_id: ProjectId,
    harness: HarnessKind,
    start: TaskStart,
    request: &str,
    checks_empty: bool,
) -> Command {
    let (_, title) = checks_skill(request, checks_empty);
    let prompt = if request.trim().is_empty() {
        if checks_empty {
            "Find a small, useful set of checks for this project and suggest them for me to approve."
        } else {
            "Read this project's checks and ask me what I would like changed. Do not draft changes until I have said what I want."
        }
        .to_owned()
    } else {
        // Keep all the person's words, including whitespace, rather than
        // summarizing them or losing which check the caller identified.
        format!(
            "Suggest changes to this project's checks for me to approve.\n\nRequest:\n{request}"
        )
    };
    Command::CreateTask {
        project_id,
        kind: TaskKind::Coding,
        origin: None,
        from_chat: None,
        mode: Some(TaskMode::Work),
        title: Some(title.to_owned()),
        prompt,
        bundle: None,
        harness,
        // Ordinary task admission fills in this project's model and effort.
        model: None,
        effort: None,
        launch_policy: None,
        branch: None,
        start: Some(Box::new(start)),
        place: None,
        browser: None,
        conversation: None,
        access: None,
        github: None,
        mission: None,
        target_id: None,
        placement: TaskPlacement::StartNow,
        agent_chosen: None,
        review: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(request: &str, checks_empty: bool) -> Command {
        let (id, _) = checks_skill(request, checks_empty);
        checks_task(
            ProjectId::new(),
            HarnessKind::Codex,
            TaskStart::Skill {
                source: CatalogSource::OphioSetup,
                id: id.to_owned(),
                digest: "current catalog digest".to_owned(),
            },
            request,
            checks_empty,
        )
    }

    #[test]
    fn both_skills_are_registered_with_plain_names_and_resolve_for_tasks() {
        let folder = tempfile::tempdir().unwrap();
        let dirs = crate::LauncherDirs {
            setup_skills: Some(folder.path().join("skills")),
            ..Default::default()
        };
        let catalog = crate::launchers::scan(&dirs, None, &[], &Default::default());
        for (id, name) in [
            ("set-up-checks", "Set up checks"),
            ("change-checks", "Change checks"),
        ] {
            let entry = catalog
                .entries
                .iter()
                .find(|e| e.kind == CatalogKind::Skill && e.id == id)
                .expect("The built-in checks skill must be in the catalog");
            assert_eq!(entry.name, name);
            assert_eq!(entry.source, CatalogSource::OphioSetup);
            let start = TaskStart::Skill {
                source: entry.source,
                id: entry.id.clone(),
                digest: entry.digest.clone(),
            };
            assert!(crate::launchers::resolve(&dirs, None, &start).is_ok());
        }
    }

    #[test]
    fn only_an_empty_request_without_checks_uses_setup() {
        for (request, empty, expected) in [
            ("", true, "set-up-checks"),
            (" \n\t", true, "set-up-checks"),
            ("", false, "change-checks"),
            ("Also check the release build", true, "change-checks"),
            ("Also check the release build", false, "change-checks"),
        ] {
            let Command::CreateTask { start, title, .. } = task(request, empty) else {
                panic!("Expected an ordinary task");
            };
            let TaskStart::Skill { source, id, digest } = *start.unwrap() else {
                panic!("Expected a skill");
            };
            assert_eq!(source, CatalogSource::OphioSetup);
            assert_eq!(id, expected);
            assert_eq!(digest, "current catalog digest");
            assert_eq!(title.as_deref(), Some(checks_skill(request, empty).1));
        }
    }

    #[test]
    fn the_brief_keeps_the_exact_request_and_selected_check() {
        let request = "Selected check: App builds (check-123)\n\nPerson's request:\n  Also check the release build.\nKeep the old test.\n";
        let Command::CreateTask { prompt, .. } = task(request, false) else {
            panic!("Expected an ordinary task");
        };
        assert!(prompt.ends_with(request));
        assert!(prompt.contains("for me to approve"));
    }

    #[test]
    fn checks_tasks_use_work_and_skip_review_without_overriding_project_defaults() {
        let Command::CreateTask {
            harness,
            kind,
            mode,
            model,
            effort,
            agent_chosen,
            review,
            placement,
            conversation,
            ..
        } = task("", true)
        else {
            panic!("Expected an ordinary task");
        };
        assert_eq!(harness, HarnessKind::Codex);
        assert_eq!(kind, TaskKind::Coding);
        assert_eq!(mode, Some(TaskMode::Work));
        assert!(model.is_none());
        assert!(effort.is_none());
        assert_ne!(agent_chosen, Some(true));
        assert!(review.is_none());
        assert!(matches!(placement, TaskPlacement::StartNow));
        assert_ne!(conversation, Some(true));
    }

    #[test]
    fn the_project_default_is_not_replaced_even_if_another_agent_is_ready() {
        assert_eq!(
            checks_agent(
                Some(HarnessKind::Codex),
                [HarnessKind::ClaudeCode].into_iter()
            )
            .unwrap(),
            HarnessKind::Codex
        );
        assert_eq!(
            checks_agent(None, [HarnessKind::ClaudeCode].into_iter()).unwrap(),
            HarnessKind::ClaudeCode
        );
        assert!(checks_agent(None, std::iter::empty()).is_err());
    }

    #[test]
    fn an_empty_change_request_asks_instead_of_inventing_changes() {
        let Command::CreateTask { prompt, .. } = task("", false) else {
            panic!("Expected an ordinary task");
        };
        assert!(prompt.contains("ask me what I would like changed"));
        assert!(prompt.contains("Do not draft changes"));
    }
}
