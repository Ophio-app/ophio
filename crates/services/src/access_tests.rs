// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::sandbox::*;
use protocol::{access::ConversationAccess, capability::Availability};
use std::sync::Mutex;

#[derive(Debug, Default)]
pub(crate) struct Backend {
    pub(crate) specs: Mutex<Vec<SandboxSpec>>,
    sandbox: Arc<Live>,
}
#[derive(Debug, Default)]
pub(crate) struct Live {
    changes: Mutex<Vec<Reach>>,
    restart: std::sync::atomic::AtomicBool,
    /// Where a server inside answers, and the addresses Ophio asked for.
    inside: Mutex<Option<std::net::SocketAddr>>,
    asked: Mutex<Vec<std::net::SocketAddr>>,
}
impl Backend {
    /// Each change the running sandbox was asked to make, in order.
    pub(crate) fn sandbox_changes(&self) -> Vec<Reach> {
        self.sandbox.changes.lock().unwrap().clone()
    }
    /// The running sandbox can't change as it runs: its turn must end.
    pub(crate) fn needs_restart(&self) {
        self.sandbox.restart.store(true, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl Sandboxes for Backend {
    fn availability(&self) -> Availability {
        Availability::Available
    }
    fn create(&self, spec: SandboxSpec) -> Result<Arc<dyn Sandbox>, ports::HarnessError> {
        self.specs.lock().unwrap().push(spec);
        Ok(self.sandbox.clone())
    }
}
#[async_trait::async_trait]
impl Sandbox for Live {
    fn wrap(&self, _: WrapRequest<'_>) -> Result<Wrapped, ports::HarnessError> {
        panic!("fake harness does not spawn")
    }
    fn prepare(&self, _: &mut std::process::Command) -> std::io::Result<()> {
        Ok(())
    }
    async fn started(&self) -> Result<(), ports::HarnessError> {
        Ok(())
    }
    async fn update(&self, reach: Reach) -> Applied {
        self.changes.lock().unwrap().push(reach);
        if self.restart.load(Ordering::SeqCst) {
            Applied::NeedsRestart("fixture read revocation".into())
        } else {
            Applied::Applied
        }
    }
    async fn connect(
        &self,
        addresses: &[std::net::SocketAddr],
    ) -> Result<std::net::TcpStream, ports::HarnessError> {
        self.asked.lock().unwrap().extend(addresses);
        let inside = *self.inside.lock().unwrap();
        inside
            .and_then(|at| std::net::TcpStream::connect(at).ok())
            .ok_or_else(|| ports::HarnessError::Unsupported("nothing answers inside".into()))
    }
}
pub(crate) fn allow_ask(f: &Fixture) {
    f.hub
        .write(|tx| {
            let mut infos: Vec<protocol::capability::HarnessInfo> =
                tx.read().require("meta", "harnesses")?;
            infos[0].modes.push(protocol::capability::ModeSupport {
                mode: TaskMode::Ask,
                availability: Availability::Available,
                enforcement: "fixture".into(),
            });
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
}
pub(crate) fn create(project: ProjectId, access: ConversationAccess) -> CommandEnvelope {
    CommandEnvelope::new(Command::CreateTask {
        origin: None,
        from_chat: None,
        github: None,
        agent_chosen: None,
        project_id: project,
        kind: TaskKind::Coding,
        mode: Some(TaskMode::Ask),
        title: Some("Conversation".into()),
        prompt: "hello".into(),
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
        conversation: Some(true),
        access: Some(access),
        review: None,
    })
}
pub(crate) async fn dispatched(f: &Fixture, task: TaskId) -> ports::RunSpec {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(spec) = f
                .harness
                .specs
                .lock()
                .unwrap()
                .iter()
                .find(|s| s.task_id == task)
                .cloned()
            {
                return spec;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn conversation_launch_and_live_grant_use_the_same_sandbox_and_publish_blocks() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    allow_ask(&f);
    let owner = Caller::local_owner();
    let access = ConversationAccess {
        read: vec![project],
        write: vec![project],
        internet: false,
        ..Default::default()
    };
    let receipt = f.hub.submit(&owner, create(project, access.clone())).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let spec = dispatched(&f, task_id).await;
    assert!(spec.sandbox.is_some());
    // Ophio's tools come along, through the bridge's socket and program.
    assert!(
        spec.bridge
            .as_ref()
            .is_some_and(|b| !b.github && !b.computer_use)
    );
    assert_eq!(spec.mode, TaskMode::Work); // The outer sandbox owns live policy.
    {
        let specs = backend.specs.lock().unwrap();
        assert_eq!(specs.len(), 1);
        assert!(specs[0].always.contains(&PathBuf::from("synthetic-socket")));
        assert_eq!(specs[0].programs, vec![PathBuf::from("synthetic-bridge")]);
    }
    let path = f.project_dir().join("file").to_string_lossy().into_owned();
    f.hub
        .conversation_block_event(
            spec.run_id,
            &ports::HarnessEvent::Activity {
                key: "write".into(),
                tool: "shell".into(),
                summary: "write".into(),
                status: ActivityStatus::Failed,
                detail: Some(format!("/bin/sh: line 1: {path}: Read-only file system")),
            },
        )
        .unwrap();
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let block = task.access_blocked.unwrap();
    assert_eq!(block.path.as_deref(), Some(path.as_str()));
    assert_eq!(block.project_id, Some(project));
    assert!(block.write);
    let callback = backend.specs.lock().unwrap()[0].on_blocked.clone().unwrap();
    callback(Blocked::Host("example.com".into()));
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(
        task.access_blocked.unwrap().host.as_deref(),
        Some("example.com")
    );
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetConversationAccess {
                task_id,
                access: ConversationAccess {
                    write: vec![project],
                    internet: true,
                    ..access
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    tokio::time::timeout(Duration::from_secs(5), async {
        while backend.sandbox.changes.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let changes = backend.sandbox.changes.lock().unwrap().clone();
    assert_eq!(changes.len(), 1);
    assert!(changes[0].internet);
    assert_eq!(changes[0].write.len(), 1);
    // Internal sandbox grants may retain Windows' extended spelling.
    crate::testing::assert_path_eq(
        &changes[0].write[0],
        f.project_dir().canonicalize().unwrap(),
        false,
    );
    assert_eq!(f.harness.specs.lock().unwrap().len(), 1);
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert!(task.access_blocked.is_none());
    f.close().await;
}
#[tokio::test]
async fn granting_project_write_restarts_before_giving_a_read_only_turn_the_writer() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    allow_ask(&f);
    f.harness
        .controls
        .confirm_terminate
        .store(true, Ordering::SeqCst);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            create(project, ConversationAccess::default()),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let first = dispatched(&f, task_id).await;
    f.harness
        .emit(
            first.run_id,
            ports::HarnessEvent::SessionStarted {
                native_session: "read-to-write".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetConversationAccess {
                task_id,
                access: ConversationAccess {
                    write: vec![project],
                    ..Default::default()
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    tokio::time::timeout(
        Duration::from_secs(5),
        f.harness.controls.terminate_entered.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    assert!(
        backend.sandbox_changes().is_empty(),
        "write must not reach the old sandbox"
    );
    assert_eq!(f.harness.specs.lock().unwrap().len(), 1);
    f.harness
        .emit(
            first.run_id,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Interrupted,
                outcome: None,
            },
        )
        .await;
    let second = launched(&f, 1).await;
    assert_eq!(second.resume_session.as_deref(), Some("read-to-write"));
    let (run, host): (Run, Project) = f
        .hub
        .read(|r| {
            Ok((
                r.require("run", second.run_id)?,
                r.require("project", project)?,
            ))
        })
        .unwrap();
    assert!(run.holds_writer);
    assert_eq!(host.active_writer, Some(second.run_id));
    assert_eq!(
        backend.specs.lock().unwrap()[1].reach.write,
        vec![PathBuf::from(host.host_path)]
    );
    f.close().await;
}

#[tokio::test]
async fn granting_project_write_beside_work_restarts_with_the_project_still_read_only() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    more_access_tests::all_modes(&f);
    f.harness
        .controls
        .confirm_terminate
        .store(true, Ordering::SeqCst);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            create(project, ConversationAccess::default()),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let first = dispatched(&f, task_id).await;
    let (work, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let work_spec = dispatched(&f, work).await;
    f.harness
        .emit(
            first.run_id,
            ports::HarnessEvent::SessionStarted {
                native_session: "read-to-write".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetConversationAccess {
                task_id,
                access: ConversationAccess {
                    write: vec![project],
                    ..Default::default()
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    tokio::time::timeout(
        Duration::from_secs(5),
        f.harness.controls.terminate_entered.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    assert!(
        backend.sandbox_changes().is_empty(),
        "write must not reach the old sandbox"
    );
    assert_eq!(f.harness.specs.lock().unwrap().len(), 2);
    f.harness
        .emit(
            first.run_id,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Interrupted,
                outcome: None,
            },
        )
        .await;
    let second = launched(&f, 2).await;
    assert_eq!(second.resume_session.as_deref(), Some("read-to-write"));
    let (run, host): (Run, Project) = f
        .hub
        .read(|r| {
            Ok((
                r.require("run", second.run_id)?,
                r.require("project", project)?,
            ))
        })
        .unwrap();
    assert!(!run.holds_writer);
    assert_eq!(host.active_writer, Some(work_spec.run_id));
    assert!(backend.specs.lock().unwrap()[1].reach.write.is_empty());
    assert!(
        second
            .prompt
            .text
            .contains("Read them, but do not change them this turn.")
    );
    assert_eq!(
        f.hub
            .read(|r| r.require::<Option<TaskId>>(crate::access::CHAT_WRITER_HOLD, second.run_id))
            .unwrap(),
        Some(work)
    );
    f.close().await;
}

#[tokio::test]
async fn blocked_chat_keeps_overlapping_grants_read_only_on_launch_and_live_updates() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    allow_ask(&f);
    let owner = Caller::local_owner();
    let receipt = f
        .hub
        .submit(
            &owner,
            create(
                project,
                ConversationAccess {
                    write: vec![project],
                    ..Default::default()
                },
            ),
        )
        .await;
    let Some(CommandResult::TaskCreated {
        task_id: writer, ..
    }) = receipt.result
    else {
        panic!("{receipt:?}")
    };
    let writer_spec = dispatched(&f, writer).await;
    let (parent, child, other) = f
        .hub
        .write(|tx| {
            let host: Project = tx.read().require("project", project)?;
            let path = PathBuf::from(&host.host_path);
            let mut registered = Vec::new();
            for path in [
                path.parent().unwrap().to_owned(),
                path.join("nested"),
                path.with_extension("other"),
            ] {
                let mut alias = host.clone();
                alias.project_id = ProjectId::new();
                alias.host_path = path.to_string_lossy().into_owned();
                alias.active_writer = None;
                tx.put("project", alias.project_id, &alias)?;
                registered.push(alias.project_id);
            }
            Ok((registered[0], registered[1], registered[2]))
        })
        .unwrap();
    let access = ConversationAccess {
        write: vec![parent, child, project, other],
        ..Default::default()
    };
    let receipt = f.hub.submit(&owner, create(project, access.clone())).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let blocked = dispatched(&f, task_id).await;
    let other_path: Project = f.hub.read(|r| r.require("project", other)).unwrap();
    let check = |reach: &Reach| {
        assert_eq!(reach.write, vec![PathBuf::from(&other_path.host_path)]);
        assert!(!reach.whole_write);
        for id in [parent, child, project] {
            let host: Project = f.hub.read(|r| r.require("project", id)).unwrap();
            assert!(reach.read.contains(&PathBuf::from(host.host_path)));
        }
    };
    check(&backend.specs.lock().unwrap()[1].reach);
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(
        task.access,
        Some(access.clone().normalized()),
        "the task's grant is not rewritten"
    );
    let hold: Option<TaskId> = f
        .hub
        .read(|r| r.require(crate::access::CHAT_WRITER_HOLD, blocked.run_id))
        .unwrap();
    assert_eq!(hold, Some(writer));
    assert!(
        blocked
            .prompt
            .text
            .contains("Read them, but do not change them this turn.")
    );
    // Ending the holder does not silently upgrade an already-running turn.
    finish(&f, writer, writer_spec.run_id).await;
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetConversationAccess {
                task_id,
                access: ConversationAccess {
                    internet: false,
                    ..access
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    tokio::time::timeout(Duration::from_secs(5), async {
        while backend.sandbox_changes().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let changed = backend.sandbox_changes();
    check(changed.last().unwrap());
    assert!(!changed.last().unwrap().internet);
    assert_eq!(f.harness.specs.lock().unwrap().len(), 2);
    f.close().await;
}

#[tokio::test]
async fn writer_detection_and_effective_reach_cover_parent_child_and_whole_computer_grants() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    allow_ask(&f);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            create(project, ConversationAccess::default()),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let spec = dispatched(&f, task_id).await;
    let mut task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let host: Project = f.hub.read(|r| r.require("project", project)).unwrap();
    let own = PathBuf::from(&host.host_path);
    let mut alias = host.clone();
    alias.project_id = ProjectId::new();
    for (path, writes) in [
        (own.clone(), true),
        (own.parent().unwrap().to_owned(), true),
        (own.join("nested"), true),
        (own.with_extension("other"), false),
    ] {
        alias.host_path = path.to_string_lossy().into_owned();
        f.hub
            .write(|tx| tx.put("project", alias.project_id, &alias))
            .unwrap();
        task.access = Some(ConversationAccess {
            write: vec![alias.project_id],
            ..Default::default()
        });
        assert_eq!(
            f.hub
                .read(|r| crate::access::conversation_writes_project(r, &task))
                .unwrap(),
            writes,
            "{}",
            path.display()
        );
    }
    f.hub
        .write(|tx| {
            tx.put(
                crate::access::CHAT_WRITER_HOLD,
                spec.run_id,
                &Some(TaskId::new()),
            )
        })
        .unwrap();
    let offline = ConversationAccess {
        whole_read: true,
        whole_write: true,
        internet: false,
        ..Default::default()
    };
    task.access = Some(offline.clone());
    assert!(
        f.hub
            .read(|r| crate::access::conversation_writes_project(r, &task))
            .unwrap()
    );
    let reach = f
        .hub
        .read(|r| f.hub.conversation_reach(r, &task, spec.run_id, &offline))
        .unwrap();
    assert!(reach.whole_read);
    assert!(!reach.whole_write);
    assert!(reach.write.is_empty());
    let unrestricted = ConversationAccess {
        internet: true,
        ..offline
    };
    task.access = Some(unrestricted.clone());
    let reach = f
        .hub
        .read(|r| {
            f.hub
                .conversation_reach(r, &task, spec.run_id, &unrestricted)
        })
        .unwrap();
    assert!(
        reach.whole_write,
        "unrestricted access is deliberately advisory"
    );
    assert!(
        f.hub
            .conversation_sandbox(&task, spec.run_id, &host, None, Vec::new(), false)
            .unwrap()
            .is_none()
    );
    task.access = Some(ConversationAccess::default());
    assert!(
        !f.hub
            .read(|r| crate::access::conversation_writes_project(r, &task))
            .unwrap()
    );
    task.conversation = false;
    task.access = Some(unrestricted);
    assert!(
        !f.hub
            .read(|r| crate::access::conversation_writes_project(r, &task))
            .unwrap()
    );
    f.close().await;
}

#[tokio::test]
async fn writer_notes_name_the_holder_and_only_report_changed_holds() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend)).await;
    let project = f.project().await;
    allow_ask(&f);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            create(project, ConversationAccess::default()),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let spec = dispatched(&f, task_id).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert!(
        f.hub
            .writer_note(&task, spec.run_id, false)
            .unwrap()
            .is_none()
    );
    let mut holder = task.clone();
    holder.task_id = TaskId::new();
    holder.title = "Update the homepage".into();
    f.hub
        .write(|tx| {
            tx.put("task", holder.task_id, &holder)?;
            tx.put(
                crate::access::CHAT_WRITER_HOLD,
                spec.run_id,
                &Some(holder.task_id),
            )
        })
        .unwrap();
    let (text, hold) = f
        .hub
        .writer_note(&task, spec.run_id, true)
        .unwrap()
        .unwrap();
    assert_eq!(hold, Some(holder.task_id));
    assert!(text.contains("“Update the homepage” is changing"));
    assert!(text.contains("Read them, but do not change them this turn."));
    assert!(text.contains("suggest them as work using Ophio’s tools"));
    assert!(text.contains(
        "tell the person you can make them in a later reply once “Update the homepage” finishes"
    ));
    assert!(!text.contains("unrestricted access"));
    assert!(!text.contains("whole-computer access is read-only"));
    assert!(!text.contains("the person changed"));
    let mut unrestricted = task.clone();
    unrestricted.access = Some(ConversationAccess {
        whole_read: true,
        whole_write: true,
        ..Default::default()
    });
    let unrestricted_text = f
        .hub
        .writer_note(&unrestricted, spec.run_id, false)
        .unwrap()
        .unwrap()
        .0;
    assert!(
        unrestricted_text
            .contains("With unrestricted access, the computer cannot enforce this instruction.")
    );
    assert!(!unrestricted_text.contains("whole-computer access is read-only"));
    unrestricted.access.as_mut().unwrap().internet = false;
    let sandboxed_text = f
        .hub
        .writer_note(&unrestricted, spec.run_id, false)
        .unwrap()
        .unwrap()
        .0;
    assert!(sandboxed_text.contains("For this reply, whole-computer access is read-only."));
    assert!(
        sandboxed_text
            .contains("Only separately allowed folders that do not overlap this project, and your scratch folder if available, remain writable.")
    );
    assert!(!sandboxed_text.contains("unrestricted access"));
    assert!(!sandboxed_text.contains("the person changed"));
    f.hub
        .write(|tx| tx.put(crate::access::WRITER_TOLD, task_id, &hold))
        .unwrap();
    assert!(
        f.hub
            .writer_note(&task, spec.run_id, true)
            .unwrap()
            .is_none()
    );
    assert!(
        f.hub
            .writer_note(&task, spec.run_id, false)
            .unwrap()
            .is_some(),
        "a fresh native session must hear the hold"
    );
    holder.task_id = TaskId::new();
    holder.title = "Fix the menu".into();
    f.hub
        .write(|tx| {
            tx.put("task", holder.task_id, &holder)?;
            tx.put(
                crate::access::CHAT_WRITER_HOLD,
                spec.run_id,
                &Some(holder.task_id),
            )
        })
        .unwrap();
    assert!(
        f.hub
            .writer_note(&task, spec.run_id, true)
            .unwrap()
            .unwrap()
            .0
            .contains("Fix the menu")
    );
    f.hub
        .write(|tx| {
            tx.put(
                crate::access::CHAT_WRITER_HOLD,
                spec.run_id,
                &None::<TaskId>,
            )
        })
        .unwrap();
    let (text, hold) = f
        .hub
        .writer_note(&task, spec.run_id, true)
        .unwrap()
        .unwrap();
    assert_eq!(hold, None);
    assert!(text.contains("the pause on changing this project has ended"));
    assert!(!text.contains("the person changed"));
    f.hub
        .write(|tx| tx.put(crate::access::WRITER_TOLD, task_id, &hold))
        .unwrap();
    assert!(
        f.hub
            .writer_note(&task, spec.run_id, true)
            .unwrap()
            .is_none()
    );
    f.close().await;
}

#[tokio::test]
async fn restricted_conversation_fails_closed_without_a_backend() {
    let f = Fixture::new().await;
    let project = f.project().await;
    allow_ask(&f);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            create(project, ConversationAccess::default()),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
            if task.state == TaskState::Failed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(f.harness.specs.lock().unwrap().is_empty());
    f.close().await;
}

#[tokio::test]
async fn restart_waits_for_containment_then_resumes_the_same_native_session() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    allow_ask(&f);
    f.harness
        .controls
        .confirm_terminate
        .store(true, Ordering::SeqCst);
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            create(project, ConversationAccess::default()),
        )
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let first = dispatched(&f, task_id).await;
    f.harness
        .emit(
            first.run_id,
            ports::HarnessEvent::SessionStarted {
                native_session: "same-session".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    backend.sandbox.restart.store(true, Ordering::SeqCst);
    f.hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::SetConversationAccess {
                task_id,
                access: ConversationAccess {
                    internet: false,
                    ..Default::default()
                },
            }),
        )
        .await;
    tokio::time::timeout(
        Duration::from_secs(5),
        f.harness.controls.terminate_entered.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    assert_eq!(f.harness.specs.lock().unwrap().len(), 1);
    f.harness
        .emit(
            first.run_id,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Interrupted,
                outcome: None,
            },
        )
        .await;
    let second = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(spec) = f.harness.specs.lock().unwrap().get(1).cloned() {
                break spec;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(second.resume_session.as_deref(), Some("same-session"));
    assert_ne!(first.run_id, second.run_id);
    let stored: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert_eq!(stored.current_run, Some(second.run_id));
    assert_eq!(stored.state, TaskState::Dispatching);
    // The first turn heard its access; this one hears what it is now.
    assert!(
        first
            .prompt
            .text
            .starts_with("Ophio holds this conversation")
    );
    assert!(
        second
            .prompt
            .text
            .starts_with("Ophio: the person changed this conversation's access."),
        "{}",
        second.prompt.text
    );
    assert!(second.prompt.text.contains("The internet is off"));
    assert!(
        second
            .prompt
            .text
            .contains("Ophio stopped your previous turn")
    );
    assert_eq!(backend.specs.lock().unwrap().len(), 2);
    assert!(!backend.specs.lock().unwrap()[1].reach.internet);
    f.close().await;
}

#[tokio::test]
async fn later_turns_hear_about_access_only_when_it_changed() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    allow_ask(&f);
    let owner = Caller::local_owner();
    let receipt = f
        .hub
        .submit(&owner, create(project, ConversationAccess::default()))
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let turn = |n: usize| {
        let f = &f;
        async move {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(spec) = f.harness.specs.lock().unwrap().get(n).cloned() {
                        break spec;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap()
        }
    };
    let finish = |spec: ports::RunSpec| {
        let f = &f;
        async move {
            f.harness
                .emit(
                    spec.run_id,
                    ports::HarnessEvent::SessionStarted {
                        native_session: "session".into(),
                        actual_model: None,
                        actual_effort: None,
                        account_label: None,
                    },
                )
                .await;
            f.harness
                .emit(
                    spec.run_id,
                    ports::HarnessEvent::Exited {
                        exit: ports::RunExit::Completed,
                        outcome: None,
                    },
                )
                .await;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let (task, run): (Task, Run) = f
                        .hub
                        .read(|r| Ok((r.require("task", task_id)?, r.require("run", spec.run_id)?)))
                        .unwrap();
                    if task.state == TaskState::Completed && !crate::runs::still_finishing(&run) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
    };
    let say = |text: &str| {
        let f = &f;
        let owner = &owner;
        let text = text.to_owned();
        async move {
            let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
            let mut envelope = CommandEnvelope::new(Command::ResumeTask {
                observed_reply: None,
                harness: None,
                mode: None,
                task_id,
                text: Some(text),
                bundle: None,
                carried_plan: None,
                read_only: false,
                model: None,
                effort: None,
            });
            envelope.expected_revision = Some(task.revision);
            let receipt = f.hub.submit(owner, envelope).await;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        }
    };
    let first = turn(0).await;
    assert!(
        first
            .prompt
            .text
            .starts_with("Ophio holds this conversation")
    );
    assert!(first.prompt.text.ends_with("\n\nhello"));
    finish(first).await;
    say("and now?").await;
    let second = turn(1).await;
    assert_eq!(second.resume_session.as_deref(), Some("session"));
    assert_eq!(second.prompt.text, "and now?");
    finish(second).await;
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetConversationAccess {
                task_id,
                access: ConversationAccess {
                    write: vec![project],
                    ..Default::default()
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    say("go ahead").await;
    let third = turn(2).await;
    let host: Project = f.hub.read(|r| r.require("project", project)).unwrap();
    crate::testing::assert_path_eq(
        &host.host_path,
        f.project_dir().canonicalize().unwrap(),
        true,
    );
    let changed = format!(
        "Ophio: the person changed this conversation's access. You may change `{}`.",
        host.host_path
    );
    assert!(
        third.prompt.text.starts_with(&changed),
        "{}",
        third.prompt.text
    );
    assert!(third.prompt.text.ends_with("\n\ngo ahead"));
    finish(third).await;
    say("thanks").await;
    assert_eq!(turn(3).await.prompt.text, "thanks");
    f.close().await;
}
/// Ends `run`, the turn of `task`, as a native session that started.
async fn finish(f: &Fixture, task_id: TaskId, run: RunId) {
    f.harness
        .emit(
            run,
            ports::HarnessEvent::SessionStarted {
                native_session: "talk-session".into(),
                actual_model: None,
                actual_effort: None,
                account_label: None,
            },
        )
        .await;
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (task, ended): (Task, Run) = f
                .hub
                .read(|r| Ok((r.require("task", task_id)?, r.require("run", run)?)))
                .unwrap();
            if task.state == TaskState::Completed && !crate::runs::still_finishing(&ended) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

/// The `n`th run the harness was asked to start, once it is.
async fn launched(f: &Fixture, n: usize) -> ports::RunSpec {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(spec) = f.harness.specs.lock().unwrap().get(n).cloned() {
                break spec;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_conversation_carried_on_as_a_task_keeps_its_access() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    allow_ask(&f);
    f.hub
        .write(|tx| {
            let mut infos: Vec<protocol::capability::HarnessInfo> =
                tx.read().require("meta", "harnesses")?;
            infos[0].modes.push(protocol::capability::ModeSupport {
                mode: TaskMode::Work,
                availability: Availability::Available,
                enforcement: "fixture".into(),
            });
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
    let owner = Caller::local_owner();
    let offline = ConversationAccess {
        internet: false,
        ..Default::default()
    };
    let receipt = f.hub.submit(&owner, create(project, offline)).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let first = dispatched(&f, task_id).await;
    assert!(first.sandbox.is_some());
    finish(&f, task_id, first.run_id).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(
                Command::ResumeTask {
                    observed_reply: None,
                    harness: None,
                    task_id,
                    mode: Some(TaskMode::Work),
                    text: Some("Add a README".into()),
                    bundle: None,
                    carried_plan: None,
                    read_only: false,
                    model: None,
                    effort: None,
                },
                task.revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    // The same session, still held: the internet stays off and its own
    // project, which it was carried on to change, is now writable.
    let second = launched(&f, 1).await;
    assert!(second.sandbox.is_some());
    assert_eq!(second.resume_session.as_deref(), Some("talk-session"));
    assert!(!second.web);
    // Ophio's own tools come along, and check that access as they're used:
    // nothing posts to GitHub or starts the app.
    assert!(
        second
            .bridge
            .as_ref()
            .is_some_and(|b| !b.github && !b.computer_use)
    );
    let reach = backend.specs.lock().unwrap()[1].reach.clone();
    assert!(!reach.internet);
    assert!(!reach.whole_read);
    let host: Project = f.hub.read(|r| r.require("project", project)).unwrap();
    assert_eq!(reach.write.len(), 1);
    // Internal sandbox grants may retain Windows' extended spelling.
    crate::testing::assert_path_eq(&reach.write[0], &host.host_path, false);
    crate::testing::assert_path_eq(
        &host.host_path,
        f.project_dir().canonicalize().unwrap(),
        true,
    );
    let changed = format!(
        "Ophio: the person changed this task's access. You may change `{}`.",
        host.host_path
    );
    assert!(
        second.prompt.text.starts_with(&changed),
        "{}",
        second.prompt.text
    );
    assert!(second.prompt.text.contains("The internet is off"));
    let stored: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert!(!stored.conversation);
    assert_eq!(
        stored.access,
        Some(ConversationAccess {
            read: vec![project],
            write: vec![project],
            internet: false,
            ..Default::default()
        })
    );
    // Its access bar follows it to Work.
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetConversationAccess {
                task_id,
                access: ConversationAccess {
                    write: vec![project],
                    ..Default::default()
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let changes = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(reach) = backend.sandbox.changes.lock().unwrap().last().cloned() {
                break reach;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(changes.internet);
    f.close().await;
}

#[tokio::test]
async fn a_side_turn_only_reads_what_its_access_may_change() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    allow_ask(&f);
    let owner = Caller::local_owner();
    let writes = ConversationAccess {
        read: vec![project],
        write: vec![project],
        internet: false,
        ..Default::default()
    };
    let receipt = f.hub.submit(&owner, create(project, writes)).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let first = dispatched(&f, task_id).await;
    let host: Project = f.hub.read(|r| r.require("project", project)).unwrap();
    assert_eq!(
        backend.specs.lock().unwrap()[0].reach.write,
        vec![PathBuf::from(&host.host_path)]
    );
    finish(&f, task_id, first.run_id).await;
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    let receipt = f
        .hub
        .submit(
            &owner,
            edit(
                Command::ResumeTask {
                    observed_reply: None,
                    harness: None,
                    task_id,
                    mode: None,
                    text: Some("Sum up".into()),
                    bundle: None,
                    carried_plan: None,
                    read_only: true,
                    model: None,
                    effort: None,
                },
                task.revision,
            ),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    // A summary takes no writer, so it changes nothing, whatever the
    // conversation may change.
    let side = launched(&f, 1).await;
    assert!(side.sandbox.is_some());
    let reach = backend.specs.lock().unwrap()[1].reach.clone();
    assert!(reach.write.is_empty() && !reach.whole_write);
    assert_eq!(reach.read, vec![PathBuf::from(&host.host_path)]);
    // Nor does a change of access made meanwhile let it.
    let receipt = f
        .hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::SetConversationAccess {
                task_id,
                access: ConversationAccess {
                    write: vec![project],
                    ..Default::default()
                },
            }),
        )
        .await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let changed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(reach) = backend.sandbox.changes.lock().unwrap().last().cloned() {
                break reach;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(changed.internet);
    assert!(changed.write.is_empty() && !changed.whole_write);
    f.close().await;
}

/// A preview checker that finds nothing on the computer's own loopback.
#[derive(Debug)]
struct Silent;
#[async_trait::async_trait]
impl crate::preview::PreviewChecker for Silent {
    async fn check(&self, _: &url::Url) -> Result<bool, ProtocolError> {
        Ok(false)
    }
}

#[tokio::test]
async fn a_chat_shows_a_page_it_serves_inside_its_sandbox_while_its_turn_lasts() {
    let backend = Arc::new(Backend::default());
    let inside = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    *backend.sandbox.inside.lock().unwrap() = Some(inside.local_addr().unwrap());
    let f = Fixture::with(|b| {
        b.sandboxes(backend.clone())
            .preview_checker(Arc::new(Silent))
    })
    .await;
    let project = f.project().await;
    allow_ask(&f);
    let owner = Caller::local_owner();
    let access = ConversationAccess {
        read: vec![project],
        ..Default::default()
    };
    let receipt = f.hub.submit(&owner, create(project, access)).await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let spec = dispatched(&f, task_id).await;
    let bridge = spec.bridge.clone().expect("a bridge");
    let token = (bridge.env.iter())
        .find(|(key, _)| key == ports::bridge::RUN_TOKEN_ENV)
        .map(|(_, token)| token.clone())
        .unwrap();
    let binding = ports::bridge::BridgeHandler::bind(&f.hub, &token)
        .await
        .expect("bridge binding");
    let said = crate::tests::shown::person(
        &f,
        &binding,
        ports::bridge::PersonCall::Preview {
            url: "http://127.0.0.1:5179/".into(),
            label: None,
            note: None,
            start: None,
            questions: Vec::new(),
        },
    )
    .await
    .unwrap();
    assert!(said.contains("inside its sandbox"), "{said}");
    let check = || async {
        let previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
        let command = CommandEnvelope::new(Command::CheckPreview {
            preview_id: previews[0].preview_id,
            start: false,
        });
        let id = command.command_id;
        f.hub.submit(&owner, command).await;
        settle(&f.hub, id).await;
        let previews: Vec<PreviewEndpoint> = f.hub.read(|r| r.list("preview")).unwrap();
        previews[0].reachable
    };
    // The computer's loopback has nothing there; the sandbox's has.
    assert_eq!(check().await, Some(true));
    assert!(
        (backend.sandbox.asked.lock().unwrap().iter()).any(|a| a.port() == 5179),
        "{:?}",
        backend.sandbox.asked
    );
    // Once the turn ends, so does its server, and its sandbox isn't asked.
    finish(&f, task_id, spec.run_id).await;
    backend.sandbox.asked.lock().unwrap().clear();
    assert_eq!(check().await, Some(false));
    assert!(backend.sandbox.asked.lock().unwrap().is_empty());
    f.close().await;
}

/// Raises `kind` from `run` as its agent would, and returns the card.
async fn raised(f: &Fixture, run: RunId, kind: ApprovalKind) -> Approval {
    let native_request_id = uuid::Uuid::new_v4().to_string();
    f.harness
        .emit(
            run,
            ports::HarnessEvent::ApprovalRequested(ports::NativeApproval {
                headline: None,
                subject: Default::default(),
                native_request_id: native_request_id.clone(),
                kind,
                title: format!("{kind:?}"),
                detail: None,
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: None,
                scope: "once".into(),
                shortened: false,
                tool_call: None,
            }),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let found = (f.hub.read(|r| r.list::<Approval>("approval")).unwrap())
                .into_iter()
                .find(|a| a.native_request_id == native_request_id);
            if let Some(approval) = found {
                return approval;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("approval was not raised")
}
async fn decide(f: &Fixture, approval: &Approval, decision: ApprovalDecision) -> CommandReceipt {
    let mut envelope = CommandEnvelope::new(Command::ResolveApproval {
        approval_id: approval.approval_id,
        decision,
    });
    envelope.expected_revision = Some(approval.revision.into());
    f.hub.submit(&Caller::local_owner(), envelope).await
}
/// Allow is refused before anything is claimed, and Deny still goes.
async fn only_declined(f: &Fixture, approval: &Approval) {
    let receipt = decide(f, approval, ApprovalDecision::Allow).await;
    assert_eq!(
        receipt.error.as_ref().map(|e| e.code),
        Some(ErrorCode::Unsupported),
        "{:?}: {receipt:?}",
        approval.kind
    );
    assert!(
        f.hub
            .read(|r| r.get::<CommandId>("approval_claim", approval.approval_id))
            .unwrap()
            .is_none()
    );
    let receipt = decide(f, approval, ApprovalDecision::Deny { reason: None }).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
}

/// A chat is an Ask task whose turns start as Work inside the sandbox its
/// access holds. Claude Code still asks about some commands there, and the
/// person's Allow lets them run inside that sandbox. What would act outside
/// it can only be declined, as in any Ask task.
#[tokio::test]
async fn a_chat_allows_what_stays_inside_its_sandbox() {
    let backend = Arc::new(Backend::default());
    let f = Fixture::with(|b| b.sandboxes(backend.clone())).await;
    let project = f.project().await;
    allow_ask(&f);
    let access = ConversationAccess {
        read: vec![project],
        write: vec![project],
        internet: false,
        ..Default::default()
    };
    let receipt = f
        .hub
        .submit(&Caller::local_owner(), create(project, access))
        .await;
    let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
        panic!("{receipt:?}")
    };
    let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
    assert!(task.conversation);
    assert_eq!(task.mode, TaskMode::Ask);
    let spec = dispatched(&f, task_id).await;
    run_started(&f, task_id).await;
    assert!(spec.sandbox.is_some());
    assert_eq!(spec.mode, TaskMode::Work);
    for kind in [ApprovalKind::Command, ApprovalKind::FileEdit] {
        let approval = raised(&f, spec.run_id, kind).await;
        let receipt = decide(&f, &approval, ApprovalDecision::Allow).await;
        assert_eq!(
            receipt.status,
            ReceiptStatus::Accepted,
            "{kind:?}: {receipt:?}"
        );
        assert!(
            f.hub
                .read(|r| r.get::<CommandId>("approval_claim", approval.approval_id))
                .unwrap()
                .is_some()
        );
    }
    // Ophio's own tools, logins and posts act on the computer itself, and
    // hosted search and fetches don't pass through the sandbox.
    for kind in [
        ApprovalKind::Tool,
        ApprovalKind::Network,
        ApprovalKind::LoginUse,
        ApprovalKind::GithubPost,
        ApprovalKind::ComputerUse,
        ApprovalKind::Other,
    ] {
        let approval = raised(&f, spec.run_id, kind).await;
        only_declined(&f, &approval).await;
    }
    f.close().await;
}

/// An Ask or Plan task that isn't held to any access starts read-only, so
/// nothing it asks for can be allowed, whatever kind it is.
#[tokio::test]
async fn ask_and_plan_tasks_outside_a_chat_still_refuse_allow() {
    let f = Fixture::new().await;
    let project = f.project().await;
    allow_ask(&f);
    f.hub
        .write(|tx| {
            let mut infos: Vec<protocol::capability::HarnessInfo> =
                tx.read().require("meta", "harnesses")?;
            infos[0].modes.push(protocol::capability::ModeSupport {
                mode: TaskMode::Plan,
                availability: Availability::Available,
                enforcement: "fixture".into(),
            });
            tx.put("meta", "harnesses", &infos)
        })
        .unwrap();
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        let mut envelope = create(project, ConversationAccess::default());
        if let Command::CreateTask {
            mode: m,
            conversation,
            access,
            ..
        } = &mut envelope.body
        {
            *m = Some(mode);
            *conversation = None;
            *access = None;
        }
        let receipt = f.hub.submit(&Caller::local_owner(), envelope).await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            panic!("{receipt:?}")
        };
        let spec = dispatched(&f, task_id).await;
        run_started(&f, task_id).await;
        assert_eq!(spec.mode, mode);
        for kind in [ApprovalKind::Command, ApprovalKind::FileEdit] {
            let approval = raised(&f, spec.run_id, kind).await;
            only_declined(&f, &approval).await;
        }
        f.hub
            .finish_run(spec.run_id, RunState::Completed, None)
            .unwrap();
    }
    f.close().await;
}
