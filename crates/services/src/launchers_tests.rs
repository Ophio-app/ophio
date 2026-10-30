// SPDX-License-Identifier: Apache-2.0
use super::*;
use protocol::launchers::*;
use sha2::{Digest, Sha256};

/// Folders for agents and skills beside the test's own, so nothing of the
/// person's own is read or written.
struct Folders {
    base: PathBuf,
}
impl Folders {
    fn new() -> Self {
        let base = std::path::absolute(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/service-tests")
                .join(format!("{}.launchers", uuid::Uuid::now_v7())),
        )
        .unwrap();
        for dir in ["claude/agents", "claude/skills", "codex/skills", "agents"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        Self { base }
    }
    fn dirs(&self) -> LauncherDirs {
        LauncherDirs {
            claude_home: Some(self.base.join("claude")),
            codex_home: Some(self.base.join("codex")),
            agents_home: Some(self.base.join("agents")),
            hermes_home: Some(self.base.join("hermes")),
            ophio_agents: Some(self.base.join("Ophio/Agents")),
            ophio_skills: Some(self.base.join("Ophio/Skills")),
            one_offs: Some(self.base.join("Ophio")),
            setup_skills: Some(self.base.join("data/setup-skills")),
        }
    }
    fn write(&self, rest: &str, text: impl AsRef<[u8]>) {
        let path = self.base.join(rest);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}
impl Drop for Folders {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

async fn with_folders(configure: impl FnOnce(HubBuilder) -> HubBuilder) -> (Fixture, Folders) {
    let folders = Folders::new();
    let dirs = folders.dirs();
    let f = Fixture::with(move |b| configure(b.launchers(dirs))).await;
    (f, folders)
}

/// What a command that finishes after its receipt came to.
async fn finished(
    f: &Fixture,
    caller: &Caller,
    command: CommandEnvelope,
) -> Result<CommandResult, ErrorCode> {
    let id = command.command_id;
    let r = f.hub.submit(caller, command).await;
    if let Some(e) = r.error {
        return Err(e.code);
    }
    settle(&f.hub, id).await;
    let stored = f.hub.read(|r| r.command(id)).unwrap().unwrap();
    match (stored.result, stored.error) {
        (_, Some(e)) => Err(e.code),
        (Some(result), None) => Ok(result),
        other => panic!("{other:?}"),
    }
}

/// A device given only what a new phone is, once another manages them.
async fn plain_device(f: &Fixture) -> Caller {
    f.device().await;
    pair(&f.hub, 2).await
}

async fn catalog(f: &Fixture, project_id: Option<ProjectId>) -> Catalog {
    match f
        .hub
        .query(&Caller::local_owner(), Query::Catalog { project_id })
        .await
        .unwrap()
    {
        QueryData::Catalog { catalog } => catalog,
        other => panic!("{other:?}"),
    }
}

fn entry<'a>(catalog: &'a Catalog, kind: CatalogKind, id: &str) -> &'a CatalogEntry {
    catalog
        .entries
        .iter()
        .find(|e| e.kind == kind && e.id == id)
        .unwrap_or_else(|| panic!("{id} in {catalog:?}"))
}

fn start_task(
    project_id: ProjectId,
    start: Option<TaskStart>,
    place: Option<TaskPlace>,
) -> Command {
    Command::CreateTask {
        github: None,
        agent_chosen: None,
        mode: Some(TaskMode::Work),
        project_id,
        kind: TaskKind::Coding,
        title: Some("Launch video".into()),
        prompt: "make it".into(),
        bundle: None,
        harness: HarnessKind::ClaudeCode,
        model: None,
        effort: None,
        launch_policy: None,
        branch: None,
        mission: None,
        target_id: None,
        placement: TaskPlacement::StartNow,
        start: start.map(Box::new),
        place,
        browser: None,
        conversation: None,
        access: None,
        review: None,
    }
}

#[tokio::test]
async fn the_catalog_lists_what_the_computer_has_and_a_task_starts_from_it_as_shown() {
    let (f, folders) = with_folders(|b| b).await;
    folders.write(
        "claude/agents/reviewer.md",
        "---\nname: reviewer\ndescription: Reads code closely\nmodel: opus\n---\n\nBe exacting.\n",
    );
    folders.write(
        "claude/skills/launch-video/SKILL.md",
        "---\nname: launch-video\ndescription: \"Makes a \\\"launch\\\" video\"\n---\nSteps.\n",
    );
    folders.write(
        "codex/skills/tidy/SKILL.md",
        "---\nname: tidy\n---\nTidy up.\n",
    );
    folders.write("claude/agents/broken.md", [0xff, 0xfe, 0]);
    let catalog = catalog(&f, None).await;
    let reviewer = entry(&catalog, CatalogKind::Agent, "reviewer");
    assert_eq!(reviewer.source, CatalogSource::ClaudeUser);
    assert_eq!(reviewer.description.as_deref(), Some("Reads code closely"));
    assert_eq!(reviewer.model.as_deref(), Some("opus"));
    assert_eq!(reviewer.harness, Some(HarnessKind::ClaudeCode));
    let video = entry(&catalog, CatalogKind::Skill, "launch-video");
    assert_eq!(
        video.description.as_deref(),
        Some("Makes a \"launch\" video")
    );
    assert_eq!(
        entry(&catalog, CatalogKind::Skill, "tidy").source,
        CatalogSource::CodexUser
    );
    assert!(catalog.entries.iter().all(|e| e.id != "broken"));
    assert!(
        catalog.problems.iter().any(|p| p.contains("broken.md")),
        "{catalog:?}"
    );
    // No one-off has been made, so its folder isn't there to be shown yet.
    assert!(catalog.agents_folder.is_none());

    // A task starts from the agent as it was shown, and is refused once the
    // file has changed since.
    let project_id = f.project().await;
    let shown = reviewer.digest.clone();
    folders.write(
        "claude/agents/reviewer.md",
        "---\nname: reviewer\n---\n\nSomething else.\n",
    );
    let stale = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(start_task(
                project_id,
                Some(TaskStart::Agent {
                    source: CatalogSource::ClaudeUser,
                    id: "reviewer".into(),
                    digest: shown,
                }),
                None,
            )),
        )
        .await;
    assert_eq!(stale.error.unwrap().code, ErrorCode::Conflict);
    let current = entry(
        &self::catalog(&f, None).await,
        CatalogKind::Agent,
        "reviewer",
    )
    .digest
    .clone();
    let r = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(start_task(
                project_id,
                Some(TaskStart::Agent {
                    source: CatalogSource::ClaudeUser,
                    id: "reviewer".into(),
                    digest: current,
                }),
                None,
            )),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}");
    };
    run_started(&f, task_id).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(
        task.start,
        Some(StartedFrom::Agent {
            name: "reviewer".into()
        })
    );
    let spec = f.harness.specs.lock().unwrap().last().cloned().unwrap();
    match spec.start {
        Some(ports::RunStart::Agent {
            instructions,
            native,
            ..
        }) => {
            assert_eq!(instructions, "Something else.");
            // Which agent broken.md is can't be told, so Claude isn't left
            // to find it by name.
            assert_eq!(native, None);
        }
        other => panic!("{other:?}"),
    }
    f.close().await;
}

#[tokio::test]
async fn setup_skills_are_built_in_and_written_out_for_the_task_that_uses_one() {
    let (f, folders) = with_folders(|b| b).await;
    let owner = Caller::local_owner();
    let catalog = catalog(&f, None).await;
    let built_in = |kind| {
        (catalog.entries.iter())
            .filter(|e| e.source == CatalogSource::OphioSetup && e.kind == kind)
            .count()
    };
    assert_eq!(built_in(CatalogKind::Skill), 17, "{catalog:?}");
    assert_eq!(built_in(CatalogKind::Agent), 16, "{catalog:?}");
    let checklist = entry(&catalog, CatalogKind::Skill, "checklist");
    assert_eq!(checklist.name, "Give the person a checklist");
    let phone = entry(&catalog, CatalogKind::Skill, "setup-phone");
    assert_eq!(phone.name, "Set up my phone");
    assert_eq!(phone.harness, None);
    assert!(phone.description.is_some());
    let written = folders.base.join("data/setup-skills/setup-phone/SKILL.md");
    assert_eq!(PathBuf::from(&phone.host_path), written);
    // Listing writes nothing.
    assert!(!folders.base.join("data").exists());

    let start = |id: &str, digest: &str| {
        CommandEnvelope::new(start_task(
            ProjectId::new(),
            Some(TaskStart::Skill {
                source: CatalogSource::OphioSetup,
                id: id.into(),
                digest: digest.into(),
            }),
            Some(TaskPlace::OneOff {
                name: "Set up my phone".into(),
                date: Some("2026-10-03".into()),
            }),
        ))
    };
    let r = f.hub.submit(&owner, start("setup-phone", "0")).await;
    assert_eq!(r.error.unwrap().code, ErrorCode::Conflict);
    let r = f
        .hub
        .submit(&owner, start("setup-unknown", &phone.digest))
        .await;
    assert_eq!(r.error.unwrap().code, ErrorCode::NotFound);
    assert!(!written.exists());

    // A file changed since is put back as it was built.
    folders.write("data/setup-skills/setup-phone/SKILL.md", "Pair a stranger.");
    let r = f
        .hub
        .submit(&owner, start("setup-phone", &phone.digest))
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}");
    };
    run_started(&f, task_id).await;
    let text = std::fs::read_to_string(&written).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&text)), phone.digest);
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(
        task.start,
        Some(StartedFrom::Skill {
            name: "Set up my phone".into()
        })
    );
    let spec = f.harness.specs.lock().unwrap().last().cloned().unwrap();
    match spec.start {
        Some(ports::RunStart::Skill {
            native,
            instructions: Some(instructions),
            skill_file,
            ..
        }) => {
            assert_eq!(native, None);
            assert_eq!(skill_file, written);
            assert!(instructions.starts_with("Help the person get Øphio onto"));
            assert!(instructions.contains("## How every Øphio setup skill works"));
        }
        other => panic!("{other:?}"),
    }
    f.close().await;
}

#[tokio::test]
async fn a_run_has_the_harness_find_the_entry_only_while_it_is_the_one_read() {
    let (f, folders) = with_folders(|b| b).await;
    folders.write(
        "claude/agents/reviewer.md",
        "---\nname: reviewer\n---\n\nBe exacting.\n",
    );
    // Each in a project of its own, as one task at a time writes in one.
    let project = folders.base.join("project");
    let made = std::sync::atomic::AtomicUsize::new(0);
    let digest = entry(&catalog(&f, None).await, CatalogKind::Agent, "reviewer")
        .digest
        .clone();
    let native = || async {
        let n = made.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let folder = folders.base.join(format!("work-{n}"));
        std::fs::create_dir_all(&folder).unwrap();
        // The project's own agents are the ones in `project`.
        if project.join(".claude").exists() {
            std::fs::rename(project.join(".claude"), folder.join(".claude")).unwrap();
        }
        let added = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(Command::AddProject {
                    host_path: folder.to_string_lossy().into_owned(),
                    name: Some(format!("Work {n}")),
                }),
            )
            .await;
        let Some(CommandResult::ProjectAdded { project_id }) = added.result else {
            panic!("{added:?}");
        };
        let r = f
            .hub
            .submit(
                &Caller::local_owner(),
                CommandEnvelope::new(start_task(
                    project_id,
                    Some(TaskStart::Agent {
                        source: CatalogSource::ClaudeUser,
                        id: "reviewer".into(),
                        digest: digest.clone(),
                    }),
                    None,
                )),
            )
            .await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
            panic!("{r:?}");
        };
        run_started(&f, task_id).await;
        match f
            .harness
            .specs
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap()
            .start
        {
            Some(ports::RunStart::Agent {
                instructions,
                native,
                ..
            }) => {
                assert_eq!(instructions, "Be exacting.");
                native.map(|n| n.name)
            }
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(native().await.as_deref(), Some("reviewer"));
    // Another agent Claude would know by the same name, in a folder within
    // its own or in the project's, could be the one it finds.
    for (rest, text) in [
        (
            "claude/agents/team/copy.md",
            "---\nname: Reviewer\n---\nOther.\n",
        ),
        ("claude/agents/team/reviewer.md", "Other.\n"),
    ] {
        folders.write(rest, text);
        assert_eq!(native().await, None, "{rest}");
        std::fs::remove_file(folders.base.join(rest)).unwrap();
    }
    let project_agent = project.join(".claude/agents/reviewer.md");
    std::fs::create_dir_all(project_agent.parent().unwrap()).unwrap();
    std::fs::write(&project_agent, "Other.\n").unwrap();
    assert_eq!(native().await, None);
    // A file that can't be read, or a folder too full to look through.
    folders.write("claude/agents/huge.md", vec![b'x'; 65 * 1024]);
    assert_eq!(native().await, None);
    std::fs::remove_file(folders.base.join("claude/agents/huge.md")).unwrap();
    assert_eq!(native().await.as_deref(), Some("reviewer"));
    f.close().await;
}

/// A project folder replaced by a link since it was added: what the harness
/// would find through it can't be told, so the run is given the saved copy.
#[cfg(unix)]
#[tokio::test]
async fn a_run_in_a_replaced_project_folder_is_given_the_saved_copy() {
    let (f, folders) = with_folders(|b| b).await;
    folders.write(
        "claude/agents/reviewer.md",
        "---\nname: reviewer\n---\n\nBe exacting.\n",
    );
    let digest = entry(&catalog(&f, None).await, CatalogKind::Agent, "reviewer")
        .digest
        .clone();
    let add = |name: &'static str| {
        let folder = folders.base.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        let f = &f;
        async move {
            let added = f
                .hub
                .submit(
                    &Caller::local_owner(),
                    CommandEnvelope::new(Command::AddProject {
                        host_path: folder.to_string_lossy().into_owned(),
                        name: Some(name.into()),
                    }),
                )
                .await;
            let Some(CommandResult::ProjectAdded { project_id }) = added.result else {
                panic!("{added:?}");
            };
            project_id
        }
    };
    let native = |project_id| {
        let (f, digest) = (&f, digest.clone());
        async move {
            let r = f
                .hub
                .submit(
                    &Caller::local_owner(),
                    CommandEnvelope::new(start_task(
                        project_id,
                        Some(TaskStart::Agent {
                            source: CatalogSource::ClaudeUser,
                            id: "reviewer".into(),
                            digest,
                        }),
                        None,
                    )),
                )
                .await;
            let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
                panic!("{r:?}");
            };
            run_started(f, task_id).await;
            let spec = f.harness.specs.lock().unwrap().last().cloned().unwrap();
            match spec.start {
                Some(ports::RunStart::Agent { native, .. }) => native.map(|n| n.name),
                other => panic!("{other:?}"),
            }
        }
    };
    let (kept, replaced) = (add("kept").await, add("work").await);
    assert_eq!(native(kept).await.as_deref(), Some("reviewer"));
    let folder = folders.base.join("work");
    let moved = folders.base.join("moved");
    std::fs::rename(&folder, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &folder).unwrap();
    assert_eq!(native(replaced).await, None);
    f.close().await;
}

#[tokio::test]
async fn a_task_on_its_own_gets_a_new_folder_kept_only_if_the_task_is() {
    let (f, folders) = with_folders(|b| b).await;
    let owner = Caller::local_owner();
    let place = || {
        Some(TaskPlace::OneOff {
            name: "Ophio launch video!".into(),
            date: Some("2026-10-03".into()),
        })
    };
    let one_offs = folders.base.join("Ophio");

    // Refused once the folder is made: it is removed again.
    let mut refused = start_task(ProjectId::new(), Some(TaskStart::Scratch), place());
    if let Command::CreateTask { prompt, .. } = &mut refused {
        *prompt = " ".into();
    }
    let r = f.hub.submit(&owner, CommandEnvelope::new(refused)).await;
    assert_eq!(r.status, ReceiptStatus::Rejected, "{r:?}");
    assert_eq!(std::fs::read_dir(&one_offs).unwrap().count(), 0);

    // Sent twice, one folder is kept.
    let project_id = ProjectId::new();
    let command = CommandEnvelope::new(start_task(project_id, Some(TaskStart::Scratch), place()));
    let first = f.hub.submit(&owner, command.clone()).await;
    let again = f.hub.submit(&owner, command).await;
    assert_eq!(first.status, ReceiptStatus::Accepted, "{first:?}");
    assert_eq!(again.result, first.result);
    let made: Vec<_> = std::fs::read_dir(&one_offs)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(made, ["2026-10-03-ophio-launch-video"]);
    let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    assert!(project.one_off);
    assert_eq!(project.name, "Ophio launch video!");
    assert_eq!(
        PathBuf::from(&project.host_path),
        ports::library::canonical(&one_offs.join(&made[0])).unwrap()
    );
    let Some(CommandResult::TaskCreated { task_id, .. }) = first.result else {
        panic!("{first:?}");
    };
    run_started(&f, task_id).await;
    assert_eq!(
        f.harness.specs.lock().unwrap().last().unwrap().start,
        Some(ports::RunStart::Scratch)
    );

    // The same name the same day gets the next number.
    let r = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(start_task(ProjectId::new(), None, place())),
        )
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    assert!(one_offs.join("2026-10-03-ophio-launch-video-2").is_dir());
    // A one-off waits in no Next up, and an existing project isn't one.
    let mut queued = start_task(ProjectId::new(), None, place());
    if let Command::CreateTask { placement, .. } = &mut queued {
        *placement = TaskPlacement::Queue {
            position: None,
            authorization: QueueAuthorization::AskBeforeStart,
        };
    }
    let r = f.hub.submit(&owner, CommandEnvelope::new(queued)).await;
    assert_eq!(r.error.unwrap().code, ErrorCode::InvalidRequest);
    let r = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(start_task(project_id, None, place())),
        )
        .await;
    assert_eq!(r.error.unwrap().code, ErrorCode::Conflict);

    // Promoted, it is an ordinary project; an ordinary one can't be.
    let device = plain_device(&f).await;
    let promote = Command::PromoteOneOff { project_id };
    let r = f
        .hub
        .submit(&device, CommandEnvelope::new(promote.clone()))
        .await;
    assert_eq!(r.error.unwrap().code, ErrorCode::PermissionDenied);
    let r = f
        .hub
        .submit(&owner, CommandEnvelope::new(promote.clone()))
        .await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    assert!(!project.one_off);
    let r = f.hub.submit(&owner, CommandEnvelope::new(promote)).await;
    assert_eq!(r.error.unwrap().code, ErrorCode::Conflict);
    f.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_one_off_is_deleted_in_full_and_only_its_own_folder_goes() {
    let (f, folders) = with_folders(|b| b).await;
    let owner = Caller::local_owner();
    let one_off = |name: &str| {
        Some(TaskPlace::OneOff {
            name: name.into(),
            date: Some("2026-10-05".into()),
        })
    };
    let delete = |project_id| CommandEnvelope::new(Command::DeleteOneOff { project_id });
    let project_id = ProjectId::new();
    let r = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(start_task(
                project_id,
                Some(TaskStart::Scratch),
                one_off("I need voice"),
            )),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}");
    };
    let run = run_started(&f, task_id).await;
    let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    let folder = PathBuf::from(&project.host_path);
    std::fs::write(folder.join("notes.md"), "kept until deleted").unwrap();
    let outside = folders.base.join("outside.md");
    std::fs::write(&outside, "not the one-off's").unwrap();
    std::os::unix::fs::symlink(&outside, folder.join("link.md")).unwrap();
    std::os::unix::fs::symlink(&folders.base, folder.join("base")).unwrap();

    // Not while its task runs, and not from a device that isn't an admin.
    let r = f.hub.submit(&owner, delete(project_id)).await;
    assert_eq!(r.error.unwrap().code, ErrorCode::Conflict);
    f.hub.finish_run(run, RunState::Completed, None).unwrap();
    let device = plain_device(&f).await;
    let r = f.hub.submit(&device, delete(project_id)).await;
    assert_eq!(r.error.unwrap().code, ErrorCode::PermissionDenied);
    assert!(folder.is_dir());

    let command = delete(project_id);
    let id = command.command_id;
    let r = f.hub.submit(&owner, command).await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    settle(&f.hub, id).await;
    let stored = f.hub.read(|r| r.command(id)).unwrap().unwrap();
    assert!(stored.error.is_none(), "{stored:?}");
    assert!(!folder.exists());
    assert!(outside.is_file() && folders.base.is_dir());
    assert!(
        f.hub
            .read(|r| r.get::<Project>("project", project_id))
            .unwrap()
            .is_none()
    );
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(task.state, TaskState::Archived);
    let r = f.hub.submit(&owner, delete(project_id)).await;
    assert_eq!(r.error.unwrap().code, ErrorCode::NotFound);

    // An ordinary project isn't one; its folder stays.
    let ordinary = f.project().await;
    let r = f.hub.submit(&owner, delete(ordinary)).await;
    assert_eq!(r.error.unwrap().code, ErrorCode::Conflict);
    assert!(f.project_dir().is_dir());

    // A one-off whose folder became a link, or was moved out of the one-offs
    // folder, is refused and kept.
    let mut kept = Vec::new();
    for name in ["Linked", "Moved"] {
        let project_id = ProjectId::new();
        let r = f
            .hub
            .submit(
                &owner,
                CommandEnvelope::new(start_task(project_id, None, one_off(name))),
            )
            .await;
        assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
        let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
        let folder = PathBuf::from(&project.host_path);
        let elsewhere = folders.base.join(name);
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("keep.md"), "keep").unwrap();
        if name == "Linked" {
            std::fs::remove_dir(&folder).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &folder).unwrap();
        } else {
            f.hub
                .write(|tx| {
                    let mut p: Project = tx.read().require("project", project_id)?;
                    p.host_path = elsewhere.display().to_string();
                    tx.put("project", project_id, &p)
                })
                .unwrap();
        }
        let r = f.hub.submit(&owner, delete(project_id)).await;
        assert_eq!(r.error.unwrap().code, ErrorCode::PathNotAllowed, "{name}");
        assert!(elsewhere.join("keep.md").is_file(), "{name}");
        kept.push(project_id);
    }
    for project_id in kept {
        let still: Option<Project> = f.hub.read(|r| r.get("project", project_id)).unwrap();
        assert!(still.is_some());
    }

    // One whose folder is already gone just leaves the lists.
    let project_id = ProjectId::new();
    let r = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(start_task(project_id, None, one_off("Gone"))),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}");
    };
    let run = run_started(&f, task_id).await;
    f.hub.finish_run(run, RunState::Completed, None).unwrap();
    let project: Project = f.hub.read(|r| r.require("project", project_id)).unwrap();
    std::fs::remove_dir(&project.host_path).unwrap();
    let r = f.hub.submit(&owner, delete(project_id)).await;
    assert_eq!(r.status, ReceiptStatus::Accepted, "{r:?}");
    let still: Option<Project> = f.hub.read(|r| r.get("project", project_id)).unwrap();
    assert!(still.is_none());
    f.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_one_offs_folder_under_a_link_into_ophios_own_is_refused_before_anything_is_made() {
    let folders = Folders::new();
    let own = folders.base.join("own");
    std::fs::create_dir_all(&own).unwrap();
    std::os::unix::fs::symlink(&own, folders.base.join("link")).unwrap();
    let mut dirs = folders.dirs();
    dirs.one_offs = Some(folders.base.join("link/new/Ophio"));
    let private = own.clone();
    let f = Fixture::with(move |b| b.launchers(dirs).private_dir(private)).await;
    let place = Some(TaskPlace::OneOff {
        name: "Launch video".into(),
        date: None,
    });
    let command = start_task(ProjectId::new(), Some(TaskStart::Scratch), place);
    let r = finished(&f, &Caller::local_owner(), CommandEnvelope::new(command)).await;
    assert_eq!(r.unwrap_err(), ErrorCode::PathNotAllowed);
    assert_eq!(std::fs::read_dir(&own).unwrap().count(), 0);
    f.close().await;
}

/// Library as the companion has it, over the test's folders, with its own
/// storage beside them.
async fn with_library(
    configure: impl FnOnce(HubBuilder, &std::path::Path) -> HubBuilder,
) -> (Fixture, Folders, Arc<::library::FsLibrary>) {
    let folders = Folders::new();
    let library = Arc::new(
        ::library::FsLibrary::new(
            ::library::LibraryConfig::new(folders.base.join("storage"), folders.base.join("home")),
            Arc::new(ports::SystemClock),
        )
        .unwrap(),
    );
    let dirs = folders.dirs();
    let base = folders.base.clone();
    let f = Fixture::with({
        let library = library.clone();
        move |b| configure(b.launchers(dirs).library(library), &base)
    })
    .await;
    (f, folders, library)
}

fn create_agent(name: &str, description: Option<&str>) -> CommandEnvelope {
    CommandEnvelope::new(Command::CreateAgent {
        name: name.into(),
        description: description.map(Into::into),
    })
}

#[tokio::test]
async fn a_new_agent_is_made_in_ophios_folder_and_opens_in_library() {
    let (f, folders, library) = with_library(|b, _| b).await;
    f.hub.set_library_roots(Vec::new()).await.unwrap();
    let owner = Caller::local_owner();
    let create = create_agent;
    let r = finished(
        &f,
        &owner,
        create("Video producer", Some("Makes \"short\" videos\nfast")),
    )
    .await;
    let Ok(CommandResult::AgentCreated {
        id,
        host_path,
        file,
    }) = r
    else {
        panic!("{r:?}");
    };
    assert_eq!(id, "video-producer");
    let folder = ports::library::canonical(&folders.base.join("Ophio/Agents")).unwrap();
    assert_eq!(PathBuf::from(&host_path), folder.join("video-producer.md"));
    // Made inside the one-offs folder, it joined Library through that, and
    // the file opens there.
    let file = file.unwrap();
    let roots = ports::LibraryFs::roots(&*library).await;
    let root = roots.iter().find(|r| r.root_id == file.root_id).unwrap();
    assert_eq!(PathBuf::from(&root.host_path), folder.parent().unwrap());
    assert_eq!(root.kind, protocol::library::RootKind::Notes);
    assert!(root.writable);
    // Beside the folders agent files are kept in, each reaching only those.
    let (agent, own): (Vec<_>, Vec<_>) = roots
        .iter()
        .partition(|r| r.kind == protocol::library::RootKind::AgentFiles);
    assert_eq!(own.len(), 1, "{roots:?}");
    assert!(agent.iter().all(|r| r.only.is_some()), "{agent:?}");
    assert_eq!(file.path, "Agents/video-producer.md");
    // It reads back as it was asked for, as an agent of Ophio's own.
    let catalog = catalog(&f, None).await;
    let made = entry(&catalog, CatalogKind::Agent, "video-producer");
    assert_eq!(made.source, CatalogSource::Ophio);
    assert_eq!(made.name, "video-producer");
    assert_eq!(
        made.description.as_deref(),
        Some("Makes \"short\" videos fast")
    );
    assert_eq!(
        catalog.agents_folder,
        Some(CatalogFile {
            root_id: file.root_id,
            path: "Agents".into(),
        })
    );

    // The same name again is refused, and the file is left as it was.
    let before = std::fs::read(&host_path).unwrap();
    let r = finished(&f, &owner, create("video  PRODUCER", None)).await;
    assert_eq!(r, Err(ErrorCode::Conflict));
    assert_eq!(std::fs::read(&host_path).unwrap(), before);
    for name in ["", "!!!", &"x".repeat(101)] {
        let r = finished(&f, &owner, create(name, None)).await;
        assert_eq!(r, Err(ErrorCode::InvalidRequest), "{name}");
    }
    // Making one needs Library's write permission.
    let device = plain_device(&f).await;
    let r = finished(&f, &device, create("Another", None)).await;
    assert_eq!(r, Err(ErrorCode::PermissionDenied));
    f.close().await;
}

fn copy_skill(id: &str) -> CommandEnvelope {
    CommandEnvelope::new(Command::CopySetupSkill { id: id.into() })
}

fn choose_version(id: &str, keep_mine: bool) -> CommandEnvelope {
    CommandEnvelope::new(Command::ChooseSetupSkillVersion {
        id: id.into(),
        keep_mine,
    })
}

/// Øphio is taken to have shipped a new version of `id` since it was copied.
fn shipped_since(f: &Fixture, id: &str) {
    f.hub
        .write(|tx| {
            let mut bases: crate::launchers::CopyBases = tx
                .read()
                .get("meta", crate::launchers::COPY_BASES)?
                .unwrap_or_default();
            bases.insert(id.into(), "an older version".into());
            tx.put("meta", crate::launchers::COPY_BASES, &bases)
        })
        .unwrap();
}

#[tokio::test]
async fn a_built_in_skill_is_copied_to_edit_and_the_copy_is_never_written_over() {
    let (f, folders, library) = with_library(|b, _| b).await;
    f.hub.set_library_roots(Vec::new()).await.unwrap();
    let owner = Caller::local_owner();
    let built_in = entry(&catalog(&f, None).await, CatalogKind::Skill, "setup-phone").clone();
    assert_eq!(
        (built_in.copy.as_ref(), built_in.library.as_ref()),
        (None, None)
    );

    let r = finished(&f, &owner, copy_skill("setup-phone")).await;
    let Ok(CommandResult::SetupSkillCopied {
        id,
        host_path,
        file: Some(file),
    }) = r
    else {
        panic!("{r:?}");
    };
    assert_eq!(id, "setup-phone");
    let folder = ports::library::canonical(&folders.base.join("Ophio/Skills")).unwrap();
    assert_eq!(
        PathBuf::from(&host_path),
        folder.join("setup-phone/SKILL.md")
    );
    // Made inside the one-offs folder, it opens in Library through that.
    assert_eq!(file.path, "Skills/setup-phone/SKILL.md");
    let read = f
        .hub
        .query(
            &owner,
            Query::LibraryRead {
                root_id: file.root_id,
                path: file.path.clone(),
            },
        )
        .await;
    assert!(read.is_ok(), "{read:?}");
    // Listed as the person's copy, the same as Øphio's until it is edited.
    let copied = entry(&catalog(&f, None).await, CatalogKind::Skill, "setup-phone").clone();
    assert_eq!(copied.source, CatalogSource::OphioSetup);
    assert_eq!(copied.copy, Some(SkillCopy { newer: false }));
    assert_eq!(copied.library.as_ref(), Some(&file));
    assert_eq!(copied.host_path, host_path);
    assert_eq!(copied.digest, built_in.digest);

    // Edited, it is theirs: copying again keeps it, and a task starts from it.
    let mine = "---\nname: Set up my phone\ndescription: My way\n---\nMy own steps.\n";
    std::fs::write(&host_path, mine).unwrap();
    let r = finished(&f, &owner, copy_skill("setup-phone")).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(std::fs::read_to_string(&host_path).unwrap(), mine);
    let edited = entry(&catalog(&f, None).await, CatalogKind::Skill, "setup-phone").clone();
    assert_eq!(edited.description.as_deref(), Some("My way"));
    assert_eq!(edited.copy, Some(SkillCopy { newer: false }));
    let start = CommandEnvelope::new(start_task(
        ProjectId::new(),
        Some(TaskStart::Skill {
            source: CatalogSource::OphioSetup,
            id: "setup-phone".into(),
            digest: edited.digest.clone(),
        }),
        Some(TaskPlace::OneOff {
            name: "Set up my phone".into(),
            date: Some("2026-10-10".into()),
        }),
    ));
    let r = f.hub.submit(&owner, start).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}");
    };
    run_started(&f, task_id).await;
    let spec = f.harness.specs.lock().unwrap().last().cloned().unwrap();
    match spec.start {
        Some(ports::RunStart::Skill {
            instructions: Some(instructions),
            skill_file,
            ..
        }) => {
            assert_eq!(skill_file, PathBuf::from(&host_path));
            assert_eq!(instructions, "My own steps.");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&host_path).unwrap(), mine);
    assert!(!folders.base.join("data/setup-skills/setup-phone").exists());

    // A newer version is offered beside theirs, and keeping theirs stops it.
    shipped_since(&f, "setup-phone");
    let offered = entry(&catalog(&f, None).await, CatalogKind::Skill, "setup-phone").clone();
    assert_eq!(offered.copy, Some(SkillCopy { newer: true }));
    assert_eq!(offered.digest, edited.digest);
    let r = finished(&f, &owner, choose_version("setup-phone", true)).await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(std::fs::read_to_string(&host_path).unwrap(), mine);
    let kept = entry(&catalog(&f, None).await, CatalogKind::Skill, "setup-phone").clone();
    assert_eq!(kept.copy, Some(SkillCopy { newer: false }));

    // Taking the new version puts theirs in recently deleted.
    shipped_since(&f, "setup-phone");
    let r = finished(&f, &owner, choose_version("setup-phone", false)).await;
    assert!(r.is_ok(), "{r:?}");
    let new = entry(&catalog(&f, None).await, CatalogKind::Skill, "setup-phone").clone();
    assert_eq!(new.digest, built_in.digest);
    assert_eq!(new.copy, Some(SkillCopy { newer: false }));
    let deleted = ports::LibraryFs::recoverable(&*library).await.unwrap();
    assert!(deleted.iter().any(|d| d.path == file.path), "{deleted:?}");

    let r = finished(&f, &owner, copy_skill("setup-unknown")).await;
    assert_eq!(r, Err(ErrorCode::NotFound));
    // Copying needs Library's write permission.
    let device = plain_device(&f).await;
    let r = finished(&f, &device, copy_skill("setup-voice")).await;
    assert_eq!(r, Err(ErrorCode::PermissionDenied));
    assert!(!folder.join("setup-voice").exists());
    f.close().await;
}

#[tokio::test]
async fn a_built_in_agent_starts_a_task_and_is_copied_to_edit() {
    let (f, folders, library) = with_library(|b, _| b).await;
    f.hub.set_library_roots(Vec::new()).await.unwrap();
    let owner = Caller::local_owner();
    let built_in = entry(&catalog(&f, None).await, CatalogKind::Agent, "bug-hunter").clone();
    assert_eq!(built_in.source, CatalogSource::OphioSetup);
    assert_eq!(built_in.name, "Bug hunter");
    assert_eq!(built_in.harness, None);
    assert_eq!(built_in.copy, None);
    let written = folders.base.join("data/setup-skills/bug-hunter.md");
    assert_eq!(PathBuf::from(&built_in.host_path), written);

    let start = |digest: &str| {
        CommandEnvelope::new(start_task(
            ProjectId::new(),
            Some(TaskStart::Agent {
                source: CatalogSource::OphioSetup,
                id: "bug-hunter".into(),
                digest: digest.into(),
            }),
            Some(TaskPlace::OneOff {
                name: "Hunt bugs".into(),
                date: Some("2026-10-11".into()),
            }),
        ))
    };
    let instructions = |f: &Fixture| match f.harness.specs.lock().unwrap().last().cloned() {
        Some(ports::RunSpec {
            start:
                Some(ports::RunStart::Agent {
                    instructions,
                    native,
                    file,
                    ..
                }),
            ..
        }) => {
            // Øphio's own: no harness knows it by name.
            assert_eq!(native, None);
            (instructions, file)
        }
        other => panic!("{other:?}"),
    };
    let r = f.hub.submit(&owner, start("0")).await;
    assert_eq!(r.error.unwrap().code, ErrorCode::Conflict);
    let r = f.hub.submit(&owner, start(&built_in.digest)).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}");
    };
    run_started(&f, task_id).await;
    let (said, read) = instructions(&f);
    assert_eq!(read, written);
    assert!(said.starts_with("Review the part of the project"), "{said}");
    assert!(said.contains("Hunt for bugs"));
    let text = std::fs::read_to_string(&written).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&text)), built_in.digest);
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(
        task.start,
        Some(StartedFrom::Agent {
            name: "Bug hunter".into()
        })
    );

    // A new agent of the person's own can't take its name.
    let r = finished(&f, &owner, create_agent("Bug  hunter", None)).await;
    assert_eq!(r, Err(ErrorCode::Conflict));

    // Copied to edit, it is kept in Ophio's agents folder and listed once,
    // as the copy.
    let r = finished(&f, &owner, copy_skill("bug-hunter")).await;
    let Ok(CommandResult::SetupSkillCopied {
        host_path,
        file: Some(file),
        ..
    }) = r
    else {
        panic!("{r:?}");
    };
    let folder = ports::library::canonical(&folders.base.join("Ophio/Agents")).unwrap();
    assert_eq!(PathBuf::from(&host_path), folder.join("bug-hunter.md"));
    assert_eq!(file.path, "Agents/bug-hunter.md");
    let listing = catalog(&f, None).await;
    let listed: Vec<_> = (listing.entries.iter())
        .filter(|e| e.id == "bug-hunter")
        .collect();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].source, CatalogSource::OphioSetup);
    assert_eq!(listed[0].copy, Some(SkillCopy { newer: false }));
    assert_eq!(listed[0].library.as_ref(), Some(&file));

    // Edited, a task starts from the copy, and it is never written over.
    let mine = "---\nname: Bug hunter\ndescription: Mine\n---\nHunt my way.\n";
    std::fs::write(&host_path, mine).unwrap();
    let edited = entry(&catalog(&f, None).await, CatalogKind::Agent, "bug-hunter").clone();
    let r = f.hub.submit(&owner, start(&edited.digest)).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}");
    };
    run_started(&f, task_id).await;
    let (said, read) = instructions(&f);
    assert_eq!(
        (said.as_str(), read),
        ("Hunt my way.", PathBuf::from(&host_path))
    );
    assert_eq!(std::fs::read_to_string(&host_path).unwrap(), mine);

    // Taking a newer version puts theirs in recently deleted.
    shipped_since(&f, "bug-hunter");
    let offered = entry(&catalog(&f, None).await, CatalogKind::Agent, "bug-hunter").clone();
    assert_eq!(offered.copy, Some(SkillCopy { newer: true }));
    let r = finished(&f, &owner, choose_version("bug-hunter", false)).await;
    assert!(r.is_ok(), "{r:?}");
    let new = entry(&catalog(&f, None).await, CatalogKind::Agent, "bug-hunter").clone();
    assert_eq!(new.digest, built_in.digest);
    let deleted = ports::LibraryFs::recoverable(&*library).await.unwrap();
    assert!(deleted.iter().any(|d| d.path == file.path), "{deleted:?}");
    f.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_skill_folder_linked_from_elsewhere_opens_where_it_leads() {
    let (f, folders, _library) = with_library(|b, _| b).await;
    folders.write(
        "elsewhere/browser/SKILL.md",
        "---\nname: browser\ndescription: Drives a browser\n---\nSteps.\n",
    );
    std::os::unix::fs::symlink(
        folders.base.join("elsewhere/browser"),
        folders.base.join("claude/skills/browser"),
    )
    .unwrap();
    f.hub.set_library_roots(Vec::new()).await.unwrap();
    let owner = Caller::local_owner();
    let browser = entry(&catalog(&f, None).await, CatalogKind::Skill, "browser").clone();
    let file = browser.library.expect("opens in Library");
    let read = f
        .hub
        .query(
            &owner,
            Query::LibraryRead {
                root_id: file.root_id,
                path: file.path.clone(),
            },
        )
        .await;
    assert!(read.is_ok(), "{file:?}: {read:?}");
    f.close().await;
}

#[tokio::test]
async fn agent_files_open_in_library_and_nothing_beside_them_does() {
    let (f, folders, library) = with_library(|b, _| b).await;
    folders.write("claude/CLAUDE.md", "# Everywhere\n");
    folders.write("claude/settings.json", "{}");
    folders.write("claude/.credentials.json", "{}");
    folders.write(
        "claude/skills/notes/SKILL.md",
        "---\nname: notes\ndescription: Keeps notes\n---\n",
    );
    folders.write("codex/AGENTS.md", "# Codex\n");
    folders.write("codex/auth.json", "{}");
    f.hub.set_library_roots(Vec::new()).await.unwrap();
    let owner = Caller::local_owner();
    let Ok(QueryData::AgentFiles { files, truncated }) =
        f.hub.query(&owner, Query::AgentFiles).await
    else {
        panic!("agent files");
    };
    assert!(!truncated);
    let shown: Vec<(&str, &str)> = files
        .iter()
        .map(|f| (f.scope.as_str(), f.shown.as_str()))
        .collect();
    assert_eq!(
        shown,
        [
            ("Claude Code", "CLAUDE.md"),
            ("Codex", "AGENTS.md"),
            ("Claude Code", "notes/SKILL.md"),
        ]
    );
    for file in &files {
        let read = f
            .hub
            .query(
                &owner,
                Query::LibraryRead {
                    root_id: file.root_id,
                    path: file.path.clone(),
                },
            )
            .await;
        assert!(read.is_ok(), "{file:?}");
    }
    let roots = ports::LibraryFs::roots(&*library).await;
    for (folder, beside) in [
        ("claude", "settings.json"),
        ("claude", ".credentials.json"),
        ("codex", "auth.json"),
    ] {
        let real = ports::library::canonical(&folders.base.join(folder)).unwrap();
        let root = roots
            .iter()
            .find(|r| Path::new(&r.host_path) == real)
            .unwrap();
        let read = ports::LibraryFs::read(&*library, root.root_id, beside).await;
        assert!(read.is_err(), "{folder}/{beside}");
        let listing = ports::LibraryFs::list(&*library, root.root_id, "")
            .await
            .unwrap();
        assert!(
            listing.entries.iter().all(|e| e.name != beside),
            "{folder}/{beside}"
        );
    }
    f.close().await;
}

#[tokio::test]
async fn a_new_agent_is_never_written_where_library_may_not_write() {
    // In a Library folder the person keeps read-only, the agents folder
    // isn't made, and an agent isn't made in it once it is there.
    let (f, folders, _library) = with_library(|b, _| b).await;
    let ophio = folders.base.join("Ophio");
    std::fs::create_dir_all(&ophio).unwrap();
    f.hub
        .set_library_roots(vec![ports::RootSpec {
            root_id: protocol::LibraryRootId::new(),
            label: "Ophio".into(),
            path: ophio.clone(),
            kind: protocol::library::RootKind::Notes,
            writable: false,
            project_id: None,
            harness: None,
            only: None,
        }])
        .await
        .unwrap();
    let owner = Caller::local_owner();
    let r = finished(&f, &owner, create_agent("Reviewer", None)).await;
    assert_eq!(r, Err(ErrorCode::PermissionDenied));
    assert!(!ophio.join("Agents").exists());
    std::fs::create_dir(ophio.join("Agents")).unwrap();
    let r = finished(&f, &owner, create_agent("Reviewer", None)).await;
    assert_eq!(r, Err(ErrorCode::PermissionDenied));
    assert_eq!(std::fs::read_dir(ophio.join("Agents")).unwrap().count(), 0);
    f.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_new_agent_is_never_written_through_a_link_into_ophios_own_folders() {
    let (f, folders, _library) = with_library(|b, base| {
        std::fs::create_dir_all(base.join("private")).unwrap();
        b.private_dir(base.join("private"))
    })
    .await;
    f.hub.set_library_roots(Vec::new()).await.unwrap();
    let private = folders.base.join("private");
    let ophio = folders.base.join("Ophio");
    std::fs::create_dir_all(&ophio).unwrap();
    let owner = Caller::local_owner();
    // The agents folder itself leads there.
    std::os::unix::fs::symlink(&private, ophio.join("Agents")).unwrap();
    let r = finished(&f, &owner, create_agent("Reviewer", None)).await;
    assert_eq!(r, Err(ErrorCode::PathNotAllowed));
    // Or the folder it would be made in does.
    std::fs::remove_file(ophio.join("Agents")).unwrap();
    std::fs::remove_dir(&ophio).unwrap();
    std::os::unix::fs::symlink(&private, &ophio).unwrap();
    let r = finished(&f, &owner, create_agent("Reviewer", None)).await;
    assert_eq!(r, Err(ErrorCode::PathNotAllowed));
    assert_eq!(std::fs::read_dir(&private).unwrap().count(), 0);
    f.close().await;
}

#[tokio::test]
async fn a_queued_task_starts_from_the_agent_as_it_was_when_queued() {
    let (f, folders) = with_folders(|b| b).await;
    folders.write(
        "claude/agents/reviewer.md",
        "---\nname: reviewer\n---\n\nBe exacting.\n",
    );
    let project_id = f.project().await;
    let digest = entry(&catalog(&f, None).await, CatalogKind::Agent, "reviewer")
        .digest
        .clone();
    let mut queued = start_task(
        project_id,
        Some(TaskStart::Agent {
            source: CatalogSource::ClaudeUser,
            id: "reviewer".into(),
            digest,
        }),
        None,
    );
    if let Command::CreateTask { placement, .. } = &mut queued {
        *placement = TaskPlacement::Queue {
            position: None,
            authorization: QueueAuthorization::AskBeforeStart,
        };
    }
    let r = f
        .hub
        .submit(&Caller::local_owner(), CommandEnvelope::new(queued))
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = r.result else {
        panic!("{r:?}");
    };
    // Changed while it waits: the task still starts from what was reviewed.
    folders.write(
        "claude/agents/reviewer.md",
        "---\nname: reviewer\n---\n\nSomething else.\n",
    );
    let queue = match f
        .hub
        .query(&Caller::local_owner(), Query::Queue { project_id })
        .await
        .unwrap()
    {
        QueryData::Queue { queue } => queue,
        other => panic!("{other:?}"),
    };
    let item = queue.items.iter().find(|i| i.task_id == task_id).unwrap();
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            edit(
                Command::QueueStart {
                    item_id: item.item_id,
                    task_revision: task.revision,
                },
                queue.revision,
            ),
        )
        .await;
    assert!(receipt.error.is_none(), "{receipt:?}");
    settle(&f.hub, receipt.command_id).await;
    run_started(&f, task_id).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(
        task.start,
        Some(StartedFrom::Agent {
            name: "reviewer".into()
        })
    );
    let spec = f.harness.specs.lock().unwrap().last().cloned().unwrap();
    match spec.start {
        Some(ports::RunStart::Agent {
            instructions,
            native,
            ..
        }) => {
            assert_eq!(instructions, "Be exacting.");
            // Claude would find the changed file by the agent's name.
            assert_eq!(native, None);
        }
        other => panic!("{other:?}"),
    }
    f.close().await;
}

#[tokio::test]
async fn a_next_up_task_starts_as_its_source_task_did() {
    let (f, folders) = with_folders(|b| b).await;
    folders.write(
        "claude/agents/reviewer.md",
        "---\nname: reviewer\n---\n\nBe exacting.\n",
    );
    let project_id = f.project().await;
    let digest = entry(&catalog(&f, None).await, CatalogKind::Agent, "reviewer")
        .digest
        .clone();
    let create = |project_id, start, queued: bool| {
        let mut command = start_task(project_id, start, None);
        if let Command::CreateTask { placement, .. } = &mut command
            && queued
        {
            *placement = TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AskBeforeStart,
            };
        }
        CommandEnvelope::new(command)
    };
    let owner = Caller::local_owner();
    let created = async |command| match f.hub.submit(&owner, command).await {
        CommandReceipt {
            result: Some(CommandResult::TaskCreated { task_id, .. }),
            ..
        } => task_id,
        other => panic!("{other:?}"),
    };
    let source = created(create(
        project_id,
        Some(TaskStart::Agent {
            source: CatalogSource::ClaudeUser,
            id: "reviewer".into(),
            digest,
        }),
        false,
    ))
    .await;
    // Changed since: Next up starts from what its source task was given,
    // not from the file as it is now.
    folders.write(
        "claude/agents/reviewer.md",
        "---\nname: reviewer\n---\n\nSomething else.\n",
    );
    let same = || Some(TaskStart::SameAs { task_id: source });
    let next = created(create(project_id, same(), true)).await;
    let saved = |task_id| {
        f.hub
            .read(|r| {
                Ok((
                    r.require::<Task>("task", task_id)?.start,
                    r.get::<ports::RunStart>("task_start", task_id)?,
                ))
            })
            .unwrap()
    };
    let (shown, run) = saved(next);
    assert_eq!(
        shown,
        Some(StartedFrom::Agent {
            name: "reviewer".into()
        })
    );
    assert_eq!(run, saved(source).1);
    assert!(matches!(
        run,
        Some(ports::RunStart::Agent { ref instructions, .. }) if instructions == "Be exacting."
    ));

    // Only in the source task's own project, and never on its own.
    let other = folders.base.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let added = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::AddProject {
                host_path: other.to_string_lossy().into_owned(),
                name: Some("Other".into()),
            }),
        )
        .await;
    let Some(CommandResult::ProjectAdded {
        project_id: other_id,
    }) = added.result
    else {
        panic!("{added:?}");
    };
    assert_eq!(
        finished(&f, &owner, create(other_id, same(), true)).await,
        Err(ErrorCode::NotFound)
    );
    let alone = CommandEnvelope::new(start_task(
        ProjectId::new(),
        same(),
        Some(TaskPlace::OneOff {
            name: "Alone".into(),
            date: None,
        }),
    ));
    assert_eq!(
        finished(&f, &owner, alone).await,
        Err(ErrorCode::InvalidRequest)
    );

    // A source that started from nothing gives an ordinary task.
    let plain = created(create(other_id, None, false)).await;
    let next = created(create(
        other_id,
        Some(TaskStart::SameAs { task_id: plain }),
        true,
    ))
    .await;
    assert_eq!(saved(next), (None, None));
    f.close().await;
}

async fn disabled_project_skill() -> (
    Fixture,
    Folders,
    Arc<::library::FsLibrary>,
    ProjectId,
    CatalogEntry,
) {
    let (f, folders, library) = with_library(|b, _| b).await;
    let path = f.project_dir().join(".claude/skills/quiet/SKILL.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "---\nname: quiet\ndescription: Stay quiet\n---\n\nQuiet instructions.\n",
    )
    .unwrap();
    std::fs::write(path.parent().unwrap().join("notes.md"), "Some notes.\n").unwrap();
    std::fs::write(f.project_dir().join("CLAUDE.md"), "# Instructions\n").unwrap();
    let project_id = f.project().await;
    f.hub.set_library_roots(Vec::new()).await.unwrap();
    let skill = entry(
        &catalog(&f, Some(project_id)).await,
        CatalogKind::Skill,
        "quiet",
    )
    .clone();
    (f, folders, library, project_id, skill)
}

async fn set_skill(
    f: &Fixture,
    kind: protocol::disabled::DisabledKind,
    path: &str,
    disabled: bool,
) -> Result<(), ErrorCode> {
    let command = CommandEnvelope::new(Command::SetDisabledInOphio {
        kind,
        host_path: path.into(),
        disabled,
    });
    let id = command.command_id;
    let receipt = f.hub.submit(&Caller::local_owner(), command).await;
    if let Some(error) = receipt.error {
        return Err(error.code);
    }
    settle(&f.hub, id).await;
    let stored = f.hub.read(|r| r.command(id)).unwrap().unwrap();
    stored.error.map_or(Ok(()), |error| Err(error.code))
}

#[tokio::test]
async fn a_disabled_project_skill_is_listed_for_the_person_but_never_started_from() {
    use protocol::disabled::{Disable, DisabledKind};
    let (f, _folders, _library, project_id, skill) = disabled_project_skill().await;
    let owner = Caller::local_owner();
    assert!(
        set_skill(&f, DisabledKind::Skill, &skill.host_path, true)
            .await
            .is_ok()
    );
    let QueryData::DisabledInOphio { items } =
        f.hub.query(&owner, Query::DisabledInOphio).await.unwrap()
    else {
        panic!("disabled items");
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, DisabledKind::Skill);
    assert!(items[0].found);
    assert!(items[0].library.is_some());
    assert!(matches!(
        entry(
            &catalog(&f, Some(project_id)).await,
            CatalogKind::Skill,
            "quiet"
        )
        .disable,
        Some(Disable::Can {
            kind: DisabledKind::Skill,
            disabled: true,
            ..
        })
    ));
    let QueryData::AgentFiles { files, .. } = f.hub.query(&owner, Query::AgentFiles).await.unwrap()
    else {
        panic!("agent files");
    };
    let listed = files
        .iter()
        .find(|file| file.host_path == skill.host_path)
        .expect("skill file");
    assert!(matches!(
        listed.disable,
        Some(Disable::Can {
            kind: DisabledKind::Skill,
            disabled: true,
            ..
        })
    ));
    let start = TaskStart::Skill {
        source: skill.source,
        id: skill.id.clone(),
        digest: skill.digest.clone(),
    };
    let refused = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(start_task(project_id, Some(start.clone()), None)),
        )
        .await;
    assert_eq!(
        refused.error.as_ref().map(|e| e.code),
        Some(ErrorCode::Conflict)
    );
    assert!(refused.error.unwrap().message.contains("disabled in Øphio"));
    assert!(
        set_skill(&f, DisabledKind::Skill, &skill.host_path, false)
            .await
            .is_ok()
    );
    assert!(matches!(
        entry(
            &catalog(&f, Some(project_id)).await,
            CatalogKind::Skill,
            "quiet"
        )
        .disable,
        Some(Disable::Can {
            kind: DisabledKind::Skill,
            disabled: false,
            ..
        })
    ));
    let started = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(start_task(project_id, Some(start), None)),
        )
        .await;
    assert!(started.error.is_none(), "{started:?}");
    f.close().await;
}

#[tokio::test]
async fn disabled_skills_are_carried_in_run_specs_and_hidden_from_agent_queries() {
    use protocol::disabled::DisabledKind;
    let (f, _folders, _library, project_id, skill) = disabled_project_skill().await;
    let owner = Caller::local_owner();
    set_skill(&f, DisabledKind::Skill, &skill.host_path, true)
        .await
        .unwrap();
    let QueryData::AgentFiles { files, .. } = f.hub.query(&owner, Query::AgentFiles).await.unwrap()
    else {
        panic!("files");
    };
    let listed = files
        .iter()
        .find(|file| file.host_path == skill.host_path)
        .unwrap();
    let root_id = listed.root_id;
    let path = listed.path.clone();
    let read = Query::LibraryRead {
        root_id,
        path: path.clone(),
    };
    assert!(f.hub.query_impl(&owner, read.clone()).await.is_ok());
    assert_eq!(
        f.hub.query_for_agent(&owner, read).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    let sibling = format!("{}notes.md", path.strip_suffix("SKILL.md").unwrap());
    assert_eq!(
        f.hub
            .query_for_agent(
                &owner,
                Query::LibraryRead {
                    root_id,
                    path: sibling
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let QueryData::Catalog { catalog: filtered } = f
        .hub
        .query_for_agent(
            &owner,
            Query::Catalog {
                project_id: Some(project_id),
            },
        )
        .await
        .unwrap()
    else {
        panic!("catalog");
    };
    assert!(
        filtered
            .entries
            .iter()
            .all(|entry| entry.host_path != skill.host_path)
    );
    let QueryData::AgentFiles {
        files: filtered, ..
    } = f
        .hub
        .query_for_agent(&owner, Query::AgentFiles)
        .await
        .unwrap()
    else {
        panic!("files");
    };
    assert!(
        filtered
            .iter()
            .all(|file| file.host_path != skill.host_path)
    );
    let save = Command::LibraryDelete {
        root_id,
        path: path.clone(),
        base_revision: String::new(),
    };
    assert_eq!(
        f.hub.refusal_for_agent(&save).await.unwrap().code,
        ErrorCode::PermissionDenied
    );
    let save = Command::LibrarySave {
        root_id,
        path: path.clone(),
        text: "edited".into(),
        base_revision: String::new(),
        newline: protocol::library::NewlineStyle::Lf,
        bom: false,
    };
    assert_eq!(
        f.hub.refusal_for_agent(&save).await.unwrap().code,
        ErrorCode::PermissionDenied
    );
    // Nor can it add a file to the skill's folder, or take the skill's file
    // from its project as a download.
    let create = Command::LibraryCreate {
        root_id,
        path: format!("{}extra.md", path.strip_suffix("SKILL.md").unwrap()),
        text: "added".into(),
    };
    assert_eq!(
        f.hub.refusal_for_agent(&create).await.unwrap().code,
        ErrorCode::PermissionDenied
    );
    let download = Command::CreateDownload {
        source: protocol::command::DownloadSource::ProjectFile {
            project_id,
            path: ".claude/skills/quiet/SKILL.md".into(),
        },
        shown_in_task: None,
    };
    assert_eq!(
        f.hub.refusal_for_agent(&download).await.unwrap().code,
        ErrorCode::PermissionDenied
    );
    let submitted = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(start_task(project_id, None, None)),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = submitted.result else {
        panic!("{submitted:?}");
    };
    run_started(&f, task_id).await;
    let spec = f.harness.specs.lock().unwrap().last().cloned().unwrap();
    assert!(
        spec.disabled
            .skills
            .iter()
            .any(|entry| entry.name == "quiet"
                && entry.path == Path::new(&skill.host_path)
                && entry.harness == Some(HarnessKind::ClaudeCode))
    );
    // Deleted, its copy in Recently deleted is kept from agents too.
    let QueryData::LibraryRead { file } = f
        .hub
        .query_impl(
            &owner,
            Query::LibraryRead {
                root_id,
                path: path.clone(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("read");
    };
    let deleted = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::LibraryDelete {
                root_id,
                path: path.clone(),
                base_revision: file.revision.clone(),
            }),
        )
        .await;
    settle(&f.hub, deleted.command_id).await;
    let trash = |data: QueryData| match data {
        QueryData::LibraryTrash { items } => items.iter().any(|i| i.path == path),
        other => panic!("{other:?}"),
    };
    assert!(trash(
        f.hub.query_impl(&owner, Query::LibraryTrash).await.unwrap()
    ));
    assert!(!trash(
        f.hub
            .query_for_agent(&owner, Query::LibraryTrash)
            .await
            .unwrap()
    ));
    f.close().await;
}

#[tokio::test]
async fn disabling_requires_a_listed_item_of_the_right_kind_and_refuses_builtins() {
    use protocol::disabled::DisabledKind;
    let (f, _folders, _library, project_id, skill) = disabled_project_skill().await;
    assert_eq!(
        set_skill(&f, DisabledKind::Agent, &skill.host_path, true).await,
        Err(ErrorCode::InvalidRequest)
    );
    assert_eq!(
        set_skill(&f, DisabledKind::Skill, "/tmp/not-listed/SKILL.md", true).await,
        Err(ErrorCode::NotFound)
    );
    let built_in = catalog(&f, Some(project_id))
        .await
        .entries
        .into_iter()
        .find(|e| e.source == CatalogSource::OphioSetup);
    let built_in = built_in.expect("built-in setup skill");
    assert_eq!(
        set_skill(&f, DisabledKind::Skill, &built_in.host_path, true).await,
        Err(ErrorCode::InvalidRequest)
    );
    f.close().await;
}

#[tokio::test]
async fn disabling_twice_keeps_the_first_time_and_enabling_removes_the_entry() {
    use protocol::disabled::DisabledKind;
    let (f, _folders, _library, _project_id, skill) = disabled_project_skill().await;
    let path = &skill.host_path;
    // Enabling a path that isn't kept is refused: nothing was enabled.
    assert_eq!(
        set_skill(&f, DisabledKind::Skill, "/tmp/never-listed/SKILL.md", false).await,
        Err(ErrorCode::NotFound)
    );
    set_skill(&f, DisabledKind::Skill, path, true)
        .await
        .unwrap();
    let first = f.hub.read(CompanionHub::disabled_stored).unwrap();
    f.clock.advance(Duration::from_secs(9));
    set_skill(&f, DisabledKind::Skill, path, true)
        .await
        .unwrap();
    let again = f.hub.read(CompanionHub::disabled_stored).unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].item.disabled_at, first[0].item.disabled_at);
    set_skill(&f, DisabledKind::Skill, path, false)
        .await
        .unwrap();
    assert!(
        f.hub
            .read(CompanionHub::disabled_stored)
            .unwrap()
            .is_empty()
    );
    f.close().await;
}

#[tokio::test]
async fn disabled_project_instructions_are_shown_as_not_applied() {
    use protocol::{disabled::DisabledKind, library::InstructionState};
    let (f, _folders, _library, project_id, _skill) = disabled_project_skill().await;
    let instruction = f.project_dir().join("CLAUDE.md");
    std::fs::write(&instruction, "# Instructions\n").unwrap();
    set_skill(&f, DisabledKind::File, instruction.to_str().unwrap(), true)
        .await
        .unwrap();
    let QueryData::Instructions { sources } = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::Instructions {
                project_id,
                task_id: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("instructions");
    };
    let source = sources
        .iter()
        .find(|source| {
            source.host_path.as_ref().is_some_and(|path| {
                crate::disabled::real(Path::new(path)) == crate::disabled::real(&instruction)
            })
        })
        .expect("project instructions");
    assert!(source.disabled);
    assert_eq!(source.state, InstructionState::NotApplied);
    f.close().await;
}

#[tokio::test]
async fn the_disabled_list_refuses_an_item_beyond_its_limit() {
    use protocol::disabled::DisabledKind;
    let (f, _folders, _library, _project_id, skill) = disabled_project_skill().await;
    let target = f
        .hub
        .disable_target(DisabledKind::Skill, &skill.host_path)
        .await
        .unwrap();
    let mut full = Vec::with_capacity(1000);
    for n in 0..1000 {
        let mut item = target.clone();
        item.item.host_path = format!("/tmp/disabled-{n}/SKILL.md");
        full.push(item);
    }
    f.hub
        .write(|tx| tx.put("meta", "disabled_in_ophio", &full))
        .unwrap();
    assert_eq!(
        set_skill(&f, DisabledKind::Skill, &skill.host_path, true).await,
        Err(ErrorCode::LimitExceeded)
    );
    assert_eq!(
        f.hub.read(CompanionHub::disabled_stored).unwrap().len(),
        1000
    );
    f.close().await;
}

#[tokio::test]
async fn an_agent_started_in_a_terminal_is_given_the_block() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let block = {
        let seen = seen.clone();
        ports::TerminalBlock(Arc::new(
            move |kind, _cwd: &std::path::Path, disabled: &ports::Disabled| {
                seen.lock().unwrap().push((kind, disabled.skills.len()));
                Ok(vec!["--settings".into(), "block.json".into()])
            },
        ))
    };
    let (f, _folders, _library) = with_library(move |b, _| b.terminal_block(block)).await;
    let path = f.project_dir().join(".claude/skills/quiet/SKILL.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "---\nname: quiet\ndescription: Stay quiet\n---\n\nQuiet.\n",
    )
    .unwrap();
    let project_id = f.project().await;
    let cwd = f.project_dir();
    // Nothing disabled: the terminal's agent starts as it is.
    assert!(
        f.hub
            .terminal_block(HarnessKind::ClaudeCode, &cwd)
            .unwrap()
            .is_empty()
    );
    let skill = entry(
        &catalog(&f, Some(project_id)).await,
        CatalogKind::Skill,
        "quiet",
    )
    .clone();
    set_skill(
        &f,
        protocol::disabled::DisabledKind::Skill,
        &skill.host_path,
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        f.hub.terminal_block(HarnessKind::ClaudeCode, &cwd).unwrap(),
        ["--settings", "block.json"]
    );
    // By each path it may be known by: as listed, and where it leads.
    let seen = seen.lock().unwrap().clone();
    assert!(matches!(seen.as_slice(), [(HarnessKind::ClaudeCode, n)] if *n >= 1));
}

#[tokio::test]
async fn without_a_block_an_agent_isn_t_started_in_a_terminal_while_something_applies() {
    let (f, _folders, _library, _project_id, skill) = disabled_project_skill().await;
    set_skill(
        &f,
        protocol::disabled::DisabledKind::Skill,
        &skill.host_path,
        true,
    )
    .await
    .unwrap();
    let refused = f
        .hub
        .terminal_block(HarnessKind::Codex, &f.project_dir())
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unsupported);
}

#[tokio::test]
async fn a_terminal_inside_a_project_gets_that_project_s_block_whatever_it_names() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let block = {
        let seen = seen.clone();
        ports::TerminalBlock(Arc::new(
            move |_kind, _cwd: &std::path::Path, disabled: &ports::Disabled| {
                seen.lock().unwrap().push(disabled.skills.len());
                Ok(vec!["--settings".into(), "block.json".into()])
            },
        ))
    };
    let (f, _folders, _library) = with_library(move |b, _| b.terminal_block(block)).await;
    let path = f.project_dir().join(".claude/skills/quiet/SKILL.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "---\nname: quiet\ndescription: Quiet\n---\n\nQuiet.\n",
    )
    .unwrap();
    let project_id = f.project().await;
    let skill = entry(
        &catalog(&f, Some(project_id)).await,
        CatalogKind::Skill,
        "quiet",
    )
    .clone();
    set_skill(
        &f,
        protocol::disabled::DisabledKind::Skill,
        &skill.host_path,
        true,
    )
    .await
    .unwrap();
    // A folder inside the project, opened with no project named, is the
    // project's: its own skill is kept from the terminal's agent.
    let inside = f.project_dir().join("src");
    std::fs::create_dir_all(&inside).unwrap();
    assert_eq!(f.hub.projects_at(&inside).unwrap(), [project_id]);
    // Its own .claude/skills isn't read from a subfolder, but the block is
    // still worked out for the project.
    let args = f
        .hub
        .terminal_block(HarnessKind::ClaudeCode, &f.project_dir())
        .unwrap();
    assert_eq!(args, ["--settings", "block.json"]);
    assert!(seen.lock().unwrap().iter().all(|n| *n >= 1));
    // Outside every project, nothing of the project's applies.
    let outside = tempfile::tempdir().unwrap();
    assert!(f.hub.projects_at(outside.path()).unwrap().is_empty());
    assert!(
        f.hub
            .terminal_block(HarnessKind::ClaudeCode, outside.path())
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_fallback_file_codex_reads_is_listed_and_can_be_disabled() {
    let (f, folders, _library) = with_library(|b, _| b).await;
    folders.write(
        "codex/config.toml",
        "project_doc_fallback_filenames = [\"NOTES.md\"]\n",
    );
    std::fs::create_dir_all(f.project_dir()).unwrap();
    std::fs::write(f.project_dir().join("NOTES.md"), "Team notes.\n").unwrap();
    let _project_id = f.project().await;
    f.hub.set_library_roots(Vec::new()).await.unwrap();
    let QueryData::AgentFiles { files, .. } = f
        .hub
        .query(&Caller::local_owner(), Query::AgentFiles)
        .await
        .unwrap()
    else {
        panic!("files");
    };
    let notes = files
        .iter()
        .find(|file| file.shown == "NOTES.md")
        .expect("the fallback file is listed");
    assert_eq!(notes.harness, Some(HarnessKind::Codex));
    assert!(matches!(
        notes.disable,
        Some(protocol::disabled::Disable::Can {
            kind: protocol::disabled::DisabledKind::File,
            ..
        })
    ));
    set_skill(
        &f,
        protocol::disabled::DisabledKind::File,
        &notes.host_path,
        true,
    )
    .await
    .unwrap();
    f.close().await;
}
