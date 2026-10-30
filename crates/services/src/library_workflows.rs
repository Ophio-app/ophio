// SPDX-License-Identifier: Apache-2.0
use crate::{CompanionHub, api::Caller, error, plumbing::library_error};
use protocol::{library::*, model::*, *};
use std::path::Path;

#[path = "library_formats.rs"]
mod formats;

impl CompanionHub {
    pub(crate) fn reusable_formats(
        &self,
        id: TaskId,
        harness: protocol::capability::HarnessKind,
    ) -> Result<QueryData, ProtocolError> {
        let text = self.reusable_text(id)?;
        let infos: Vec<protocol::capability::HarnessInfo> =
            self.read(|r| Ok(r.get("meta", "harnesses")?.unwrap_or_default()))?;
        let info = infos.iter().find(|i| i.kind == harness);
        let data = QueryData::ReusableFormats {
            native_version: info.and_then(|i| i.native_version.clone()),
            formats: formats::templates(info, &text),
        };
        // A catalog repeats the prompt across formats. Refuse an oversized
        // reply explicitly rather than letting the control transport drop it.
        let size = serde_json::to_vec(&data)
            .map_err(|_| error(ErrorCode::Internal, "Could not encode draft formats"))?
            .len();
        if size > 900 * 1024 {
            return Err(error(
                ErrorCode::LimitExceeded,
                "This task is too large to preview all reusable formats. Create a smaller task draft first.",
            ));
        }
        Ok(data)
    }

    pub(crate) async fn instruction_baseline(
        &self,
        run: RunId,
        project: &Project,
        task: &Task,
    ) -> Result<(), ProtocolError> {
        if let Some(library) = &self.inner.library {
            let settings = self.read(|r| Self::project_settings(r, project.project_id))?;
            let sources = library
                .instruction_sources(Path::new(&project.host_path), task.harness, &[], settings)
                .await;
            self.write(|tx| tx.put("instruction_baseline", run, &sources))?;
        }
        Ok(())
    }

    pub(crate) async fn instructions(
        &self,
        caller: &Caller,
        project_id: ProjectId,
        task_id: Option<TaskId>,
    ) -> Result<QueryData, ProtocolError> {
        let (project, harness, baseline, settings) = self.read(|r| {
            let project: Project = r.require("project", project_id)?;
            let settings = Self::project_settings(r, project_id)?;
            let mut baseline = Vec::<InstructionSource>::new();
            let harness = if let Some(id) = task_id {
                let task: Task = r.require("task", id)?;
                if task.project_id != project_id {
                    return Err(error(
                        ErrorCode::InvalidRequest,
                        "Task belongs to a different project",
                    )
                    .into());
                }
                if let Some(run) = task.current_run {
                    baseline = r.get("instruction_baseline", run)?.unwrap_or_default();
                }
                task.harness
            } else {
                project
                    .default_harness
                    .unwrap_or(protocol::capability::HarnessKind::ClaudeCode)
            };
            Ok((project, harness, baseline, settings))
        })?;
        // Current native adapters do not report loaded instruction paths.
        let mut sources = self
            .library()?
            .instruction_sources(Path::new(&project.host_path), harness, &[], settings)
            .await;
        apply_baseline(&mut sources, &baseline);
        // Disabled in Ophio, a file isn't given to the project's runs.
        let disabled = self.disabled_paths()?;
        for source in &mut sources {
            if source
                .host_path
                .as_deref()
                .is_some_and(|p| disabled.contains(&crate::disabled::real(Path::new(p))))
            {
                source.disabled = true;
                if source.state != InstructionState::Missing {
                    source.state = InstructionState::NotApplied;
                }
            }
        }
        // A Library folder's files are the caller's to read. Of a source
        // anywhere else, such as the harness's own folder, the inspector shows
        // where it is and what the computer concluded about it, never its size
        // or digests, and only to a caller who directs this project's agents.
        sources.retain_mut(|source| {
            if source.root_id.is_some() {
                return true;
            }
            source.size = None;
            source.revision_at_start = None;
            source.current_revision = None;
            caller.grants.tasks
        });
        Ok(QueryData::Instructions { sources })
    }

    pub(crate) fn reusable_text(&self, id: TaskId) -> Result<String, ProtocolError> {
        self.read(|r| {
            let task: Task = r.require("task", id)?;
            let prompt: Option<(String, Option<Box<protocol::annotation::AnnotationBundle>>)> = r.get("prompt", id)?;
            let mut text = format!("# {}\n\nReusable task draft — review before use.\n\n## Fill in before starting\n\n- Project and working folder: [fill in]\n- Inputs and desired result: [fill in]\n- Allowed changes and exclusions: [fill in]\n\n## Prompt\n\n{}\n\n## Boundaries\n\n", task.title, prompt.map(|p| p.0).unwrap_or_else(|| task.title.clone()));
            if let Some(mission) = &task.mission {
                text.push_str(&format!("Goal: {}\n\n", mission.goal));
                for bound in &mission.bounds { text.push_str(&format!("- {}\n", bound.text)); }
                if let Some(stop) = &mission.stop_point { text.push_str(&format!("Stop when: {stop}\n")); }
                text.push_str("\n## Checks before calling it complete\n\n");
                for check in &mission.completion_checks { text.push_str(&format!("- [ ] {check}\n")); }
            } else { text.push_str("[Specify what may change, what must stay untouched, and when to stop.]\n\n## Checks before calling it complete\n\n"); }
            text.push_str("- [ ] [Add a concrete verification command or observable result.]\n- [ ] Review remaining unknowns and report them.\n\nThis document is a draft. Saving it does not activate instructions or start a task.\n");
            Ok(text)
        })
    }

    pub(crate) async fn create_reusable(
        &self,
        task_id: TaskId,
        root_id: LibraryRootId,
        path: &str,
        text: &str,
    ) -> Result<Option<protocol::command::CommandResult>, ProtocolError> {
        let _: Task = self.read(|r| r.require("task", task_id))?;
        crate::transfers::relative(path, false)?;
        validate_draft_name(path)?;
        let library = self.library()?;
        if !library
            .roots()
            .await
            .iter()
            .any(|r| r.root_id == root_id && r.writable && r.kind == RootKind::Drafts)
        {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Choose a writable Drafts root; active instruction folders are not draft destinations",
            ));
        }
        let revision = library
            .create(root_id, path, text)
            .await
            .map_err(library_error)?;
        self.library_changed(root_id, path, Some(revision))?;
        Ok(Some(protocol::command::CommandResult::DraftCreated {
            root_id,
            path: path.into(),
        }))
    }
}

fn validate_draft_name(path: &str) -> Result<(), ProtocolError> {
    let name = path.to_ascii_lowercase();
    if path.contains(['/', '\\'])
        || !name.ends_with(".md")
        || matches!(
            name.as_str(),
            "agents.md" | "claude.md" | "skill.md" | "gemini.md"
        )
    {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Choose a single draft .md file name, not an active instruction file or a subfolder",
        ));
    }
    Ok(())
}

fn apply_baseline(sources: &mut Vec<InstructionSource>, baseline: &[InstructionSource]) {
    for source in sources.iter_mut() {
        if let Some(base) = baseline.iter().find(|b| b.host_path == source.host_path) {
            source.revision_at_start = base.current_revision.clone();
            if source.current_revision != base.current_revision
                && (source.current_revision.is_some() || base.current_revision.is_some())
            {
                source.state = InstructionState::Changed;
            }
        }
    }
    for base in baseline {
        if !sources.iter().any(|s| s.host_path == base.host_path) {
            let mut source = base.clone();
            source.revision_at_start = source.current_revision.take();
            source.state = InstructionState::Unknown;
            source.loaded_reported = false;
            source.size = None;
            sources.push(source);
        }
    }
}

#[cfg(test)]
mod recovery_authority_tests {
    use crate::{api::*, testing::*, *};
    use protocol::{auth::*, library::*, *};
    use std::sync::Arc;

    #[test]
    fn draft_names_cannot_create_native_discovery_files() {
        for name in [
            "AGENTS.md",
            "ClAuDe.md",
            "SKILL.md",
            ".claude/agents/reviewer.md",
            "agent.toml",
        ] {
            assert!(super::validate_draft_name(name).is_err());
        }
        for name in ["skill-draft.md", "reviewed-task.md", "agent-draft.toml.md"] {
            assert!(super::validate_draft_name(name).is_ok());
        }
    }

    #[tokio::test]
    async fn recovery_queries_and_discard_require_current_library_grants() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/library-authority-tests")
            .join(uuid::Uuid::now_v7().to_string());
        let hub = CompanionHub::builder()
            .data_dir(&dir)
            .library(Arc::new(FakeLibrary::default()))
            .clock(Arc::new(FakeClock::default()))
            .build()
            .await
            .unwrap();
        let owner = Caller::local_owner();
        let ticket = hub.open_pairing(&owner).await.unwrap();
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let paired = crate::testing::pair_confirmed(
            &hub,
            PairRequest {
                pairing_secret: ticket.secret,
                device_name: "Reader".into(),
                device_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
                device_platform: "test".into(),
                client_version: "test".into(),
                protocol: ProtocolVersion::CURRENT,
                shows_code: true,
                replaces: None,
            },
        )
        .await
        .unwrap();
        let caller = Caller {
            principal: Principal::Device {
                device_id: paired.device_id,
            },
            grants: Grants::owner(),
        };
        for (read, write) in [(false, false), (false, true), (true, false)] {
            hub.write(|tx| {
                let mut device: crate::identity::Device =
                    tx.read().require("device", paired.device_id)?;
                device.record.grants.library_read = read;
                device.record.grants.library_write = write;
                tx.put("device", paired.device_id, &device)
            })
            .unwrap();
            for q in [
                Query::LibraryRecoveryList,
                Query::LibraryRecoveryRead {
                    root_id: LibraryRootId::new(),
                    recovery_id: "snapshot".into(),
                },
            ] {
                let error = hub.query(&caller, q).await.unwrap_err();
                assert_eq!(
                    error.code,
                    if read {
                        ErrorCode::NotFound
                    } else {
                        ErrorCode::PermissionDenied
                    }
                );
            }
            let result = hub
                .submit(
                    &caller,
                    CommandEnvelope::new(Command::LibraryRecoveryDiscard {
                        root_id: LibraryRootId::new(),
                        recovery_id: "snapshot".into(),
                    }),
                )
                .await;
            assert_eq!(result.error.unwrap().code, ErrorCode::PermissionDenied);
        }
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn sources_outside_library_folders_show_no_digests_and_only_to_task_holders() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/library-authority-tests")
            .join(uuid::Uuid::now_v7().to_string());
        let library = Arc::new(FakeLibrary::default());
        let hub = CompanionHub::builder()
            .data_dir(&dir)
            .library(library.clone())
            .clock(Arc::new(FakeClock::default()))
            .build()
            .await
            .unwrap();
        let folder = dir.with_extension("project");
        std::fs::create_dir_all(&folder).unwrap();
        let owner = Caller::local_owner();
        let Some(protocol::command::CommandResult::ProjectAdded { project_id }) = hub
            .submit(
                &owner,
                CommandEnvelope::new(Command::AddProject {
                    host_path: folder.to_string_lossy().into_owned(),
                    name: None,
                }),
            )
            .await
            .result
        else {
            panic!("project not added");
        };
        let source = |root_id, host_path: &str| InstructionSource {
            label: "Instructions".into(),
            root_id,
            path: root_id.map(|_| "CLAUDE.md".into()),
            host_path: Some(host_path.into()),
            harness: protocol::capability::HarnessKind::ClaudeCode,
            scope: InstructionScope::User,
            state: InstructionState::Discovered,
            revision_at_start: None,
            current_revision: Some("a".repeat(64)),
            size: Some(12),
            loaded_reported: false,
            disabled: false,
        };
        library.set_instruction_sources(vec![
            source(Some(LibraryRootId::new()), "/shared/CLAUDE.md"),
            source(None, "/home/sam/.claude/CLAUDE.md"),
        ]);
        let query = || Query::Instructions {
            project_id,
            task_id: None,
        };
        let QueryData::Instructions { sources } = hub.query(&owner, query()).await.unwrap() else {
            panic!("expected instructions");
        };
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].size, Some(12));
        assert!(sources[0].current_revision.is_some());
        assert_eq!(
            sources[1].host_path.as_deref(),
            Some("/home/sam/.claude/CLAUDE.md")
        );
        assert_eq!(sources[1].state, InstructionState::Discovered);
        assert_eq!(sources[1].size, None);
        assert_eq!(sources[1].current_revision, None);
        let ticket = hub.open_pairing(&owner).await.unwrap();
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let reader = crate::testing::pair_confirmed(
            &hub,
            PairRequest {
                pairing_secret: ticket.secret,
                device_name: "Reader".into(),
                device_public_key: URL_SAFE_NO_PAD.encode([9; 32]),
                device_platform: "test".into(),
                client_version: "test".into(),
                protocol: ProtocolVersion::CURRENT,
                shows_code: true,
                replaces: None,
            },
        )
        .await
        .unwrap();
        hub.write(|tx| {
            let mut device: crate::identity::Device =
                tx.read().require("device", reader.device_id)?;
            device.record.grants.library_read = true;
            device.record.grants.tasks = false;
            tx.put("device", reader.device_id, &device)
        })
        .unwrap();
        let reader = Caller {
            principal: Principal::Device {
                device_id: reader.device_id,
            },
            grants: Grants::owner(),
        };
        let QueryData::Instructions { sources } = hub.query(&reader, query()).await.unwrap() else {
            panic!("expected instructions");
        };
        assert_eq!(sources.len(), 1);
        assert!(sources[0].root_id.is_some());
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(folder).unwrap();
    }
}
