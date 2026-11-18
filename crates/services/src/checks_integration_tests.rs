// SPDX-License-Identifier: Apache-2.0
//! The existing task and phone paths must remain safe while checks hold a project.
use super::*;
use ports::Clock;
use protocol::checks::*;

fn approved_check() -> Check {
    Check {
        check_id: CheckId::new(),
        name: "App still works".into(),
        about: "Makes sure the app still works. A failure means it needs fixing.".into(),
        run: "printf 'approved check ran\\n'".into(),
        setup: Some("printf 'setup must not run'".into()),
        paths: vec![],
        when: CheckWhen::EveryTask,
        timeout_secs: 10,
        network: false,
        inputs: vec![],
        enabled: true,
        typical_secs: None,
    }
}

fn approve(f: &Fixture, project: ProjectId) {
    f.hub.inner.checks.paused.store(true, Ordering::Release);
    f.hub
        .write(|tx| {
            tx.change_checks(
                project,
                0,
                CheckEdit::Save {
                    check: approved_check(),
                },
                None,
                &f.project_dir(),
            )?;
            Ok(())
        })
        .unwrap();
}

async fn checking(f: &Fixture, task_id: TaskId) -> Task {
    tokio::time::timeout(WAIT, async {
        loop {
            let task: Task = f.hub.read(|r| r.require("task", task_id)).unwrap();
            if task.checking.is_some() {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("task did not start checking")
}

async fn changed_work(f: &Fixture, project: ProjectId) -> (TaskId, RunId) {
    let (task, _) = f
        .task(
            project,
            TaskPlacement::StartNow,
            &Caller::local_owner(),
            false,
        )
        .await;
    let run = run_started(f, task).await;
    std::fs::write(f.project_dir().join("app.txt"), "the agent changed this").unwrap();
    (task, run)
}

async fn queued_run_started(f: &Fixture, task_id: TaskId) -> RunId {
    tokio::time::timeout(WAIT, async {
        while f
            .hub
            .read(|r| Ok(r.require::<Task>("task", task_id)?.current_run.is_none()))
            .unwrap()
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("queued task was not admitted");
    run_started(f, task_id).await
}

async fn end(f: &Fixture, run: RunId) {
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
}

#[tokio::test]
async fn queued_work_cannot_start_between_agent_exit_and_check_completion() {
    let f = Fixture::new().await;
    let project = f.project().await;
    approve(&f, project);
    let (task, run) = changed_work(&f, project).await;
    let (next, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            },
            &Caller::local_owner(),
            false,
        )
        .await;
    f.hub
        .write(|tx| {
            let mut queue: QueueView = tx.read().require("queue", project)?;
            queue.paused = false;
            tx.put("queue", project, &queue)
        })
        .unwrap();
    let before = f.hub.read(|r| r.head()).unwrap();
    // Queue advancement actually races the agent's exit, not merely the final
    // state: releasing the writer before reserving the check would start next.
    let race = async {
        for _ in 0..100 {
            f.hub.advance_queues().await.unwrap();
            tokio::task::yield_now().await;
        }
    };
    tokio::join!(end(&f, run), race);
    let task = checking(&f, task).await;
    assert_eq!(task.state, TaskState::Working);
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 1);
    let job = task.checking.unwrap().job_id;
    let writer = crate::check_execution::job_writer(job);
    f.hub
        .read(|r| {
            let project: Project = r.require("project", project)?;
            let queue: QueueView = r.require("queue", project.project_id)?;
            assert_eq!(project.active_writer, Some(writer));
            assert_eq!(queue.writer, Some(writer));
            assert_eq!(r.require::<Task>("task", next)?.state, TaskState::Accepted);
            for event in r.journal(before)?.events {
                if let Event::ProjectUpserted { project } = event.event {
                    assert!(
                        project.active_writer.is_some(),
                        "the writer was released before checking"
                    );
                }
            }
            Ok(())
        })
        .unwrap();
    f.hub.finish_check_job_for_test(job, false).unwrap();
    f.hub.advance_queues().await.unwrap();
    run_started(&f, next).await;
    assert_eq!(f.harness.starts.load(Ordering::SeqCst), 2);
    f.close().await;
}

#[tokio::test]
async fn an_old_phone_lists_checking_as_working_gets_no_early_alert_and_can_stop_it() {
    let f = Fixture::new().await;
    let phone = f.device().await;
    let project = f.project().await;
    approve(&f, project);
    let (task_id, run) = changed_work(&f, project).await;
    end(&f, run).await;
    let task = checking(&f, task_id).await;
    // An old phone uses the ordinary snapshot request and ignores additive
    // task fields. It knows no checks requests, feature string or new state.
    let request: Query = serde_json::from_value(serde_json::json!({"kind":"snapshot"})).unwrap();
    let QueryData::Snapshot(snapshot) = f.hub.query(&phone, request).await.unwrap() else {
        panic!("snapshot");
    };
    let listed = snapshot
        .tasks
        .iter()
        .find(|task| task.task_id == task_id)
        .unwrap();
    let mut old_task = serde_json::to_value(listed).unwrap();
    old_task.as_object_mut().unwrap().remove("checking");
    assert_eq!(old_task["state"], "working");
    let listed_run = snapshot
        .runs
        .iter()
        .find(|item| item.run_id == run)
        .unwrap();
    // This is the old phone's isTaskRunning predicate for an ended run.
    // Working alone is insufficient once its native run record has arrived.
    assert!(listed_run.ended_at.is_some());
    assert!(listed_run.cleanup_pending);
    assert!(!listed_run.holds_writer);
    assert!(crate::runs::still_finishing(listed_run));
    assert!(!crate::runs::helpers_finishing(listed_run));
    // New phones receive a specific checking detail rather than helper work.
    assert_eq!(
        listed.checking.as_ref().unwrap().job_id,
        task.checking.as_ref().unwrap().job_id
    );
    assert!(
        f.hub
            .alerts_open(&phone, false)
            .unwrap()
            .0
            .iter()
            .all(|alert| alert.task_id != task_id)
    );
    let command: CommandEnvelope = serde_json::from_value(serde_json::json!({
        "command_id": CommandId::new(), "issued_at": f.clock.now(),
        "expected_revision": task.revision,
        "body": {"command":"stop_task", "task_id":task_id, "force":false}
    }))
    .unwrap();
    let id = command.command_id;
    let receipt = f.hub.submit(&phone, command).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    settle(&f.hub, id).await;
    f.hub
        .read(|r| {
            let task: Task = r.require("task", task_id)?;
            assert!(task.checking.is_none());
            assert_eq!(task.state, TaskState::Completed);
            let jobs = r.task_check_jobs(task_id)?;
            assert_eq!(jobs[0].state, CheckJobState::Finished);
            assert_eq!(jobs[0].message.as_deref(), Some("Not checked: stopped"));
            assert!(
                jobs[0]
                    .results
                    .iter()
                    .any(|r| r.outcome == CheckOutcome::Stopped)
            );
            assert_eq!(
                r.require::<Project>("project", project)?.active_writer,
                None
            );
            Ok(())
        })
        .unwrap();
    f.close().await;
}

#[tokio::test]
async fn restart_between_check_completion_and_task_completion_replays_the_end_once() {
    let f = Fixture::new().await;
    let project = f.project().await;
    approve(&f, project);
    let (task_id, run) = changed_work(&f, project).await;
    end(&f, run).await;
    let task = checking(&f, task_id).await;
    let job_id = task.checking.unwrap().job_id;
    // Persist the handback but deliberately do not replay end_run. This is
    // the exact crash boundary between the two completion transactions.
    f.hub
        .write(|tx| {
            let mut job: CheckJob = tx.read().require(store::checks::JOBS, job_id)?;
            job.state = CheckJobState::Finished;
            job.finished_at = Some(f.clock.now());
            tx.put_check_job(&job)?;
            assert!(f.hub.advance_task_checks(tx, job_id)?.is_some());
            assert_eq!(
                tx.read()
                    .require::<Project>("project", project)?
                    .active_writer,
                Some(run)
            );
            Ok(())
        })
        .unwrap();
    let f = f.reopened(|builder| builder).await;
    f.hub
        .read(|r| {
            let task: Task = r.require("task", task_id)?;
            assert_eq!(task.state, TaskState::Completed);
            assert!(task.checking.is_none());
            assert_eq!(
                r.require::<Project>("project", project)?.active_writer,
                None
            );
            assert_eq!(
                r.task_attention::<AttentionItem>(task_id)?
                    .iter()
                    .filter(|item| item.kind == AttentionKind::Completed)
                    .count(),
                1
            );
            Ok(())
        })
        .unwrap();
    let f = f.reopened(|builder| builder).await;
    assert_eq!(
        f.hub
            .read(|r| Ok(r
                .task_attention::<AttentionItem>(task_id)?
                .iter()
                .filter(|item| item.kind == AttentionKind::Completed)
                .count()))
            .unwrap(),
        1
    );
    f.close().await;
}

#[tokio::test]
async fn chat_starts_read_only_while_standalone_or_task_checks_keep_the_project() {
    for standalone in [true, false] {
        let sandbox = Arc::new(FakeSandboxes::default());
        let f = Fixture::with(|builder| builder.sandboxes(sandbox.clone())).await;
        let project = f.project().await;
        more_access_tests::all_modes(&f);
        approve(&f, project);
        let job_id = if standalone {
            let check_id = f
                .hub
                .read(|r| Ok(r.project_checks(project)?.unwrap().checks[0].check_id))
                .unwrap();
            let receipt = f
                .hub
                .submit(
                    &Caller::local_owner(),
                    CommandEnvelope::new(Command::ChecksRunNow {
                        project_id: project,
                        check_ids: vec![check_id],
                    }),
                )
                .await;
            let Some(CommandResult::CheckJob { job }) = receipt.result else {
                panic!("{receipt:?}");
            };
            assert!(job.task_id.is_none());
            // Freeze the scheduler's reserved-job boundary without starting
            // a native check process. A merely queued Run now owns no writer.
            f.hub
                .write(|tx| {
                    let mut job = job.clone();
                    job.state = CheckJobState::Running;
                    job.started_at = Some(f.clock.now());
                    tx.put_check_job(&job)?;
                    let writer = Some(crate::check_execution::job_writer(job.id));
                    let mut project: Project = tx.read().require("project", project)?;
                    project.active_writer = writer;
                    let mut queue: QueueView = tx.read().require("queue", project.project_id)?;
                    queue.writer = writer;
                    tx.put("project", project.project_id, &project)?;
                    tx.put("queue", queue.project_id, &queue)
                })
                .unwrap();
            job.id
        } else {
            let (task, run) = changed_work(&f, project).await;
            end(&f, run).await;
            checking(&f, task).await.checking.unwrap().job_id
        };
        let command = conversation_access::create(
            project,
            protocol::access::ConversationAccess {
                write: vec![project],
                ..Default::default()
            },
        );
        let command_id = command.command_id;
        let receipt = f.hub.submit(&Caller::local_owner(), command).await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            panic!("{receipt:?}");
        };
        let spec = conversation_access::dispatched(&f, task_id).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if f.hub
                    .read(|r| Ok(r.require::<Run>("run", spec.run_id)?.force_stoppable))
                    .unwrap()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("Chat launch-status transaction failed while checks held the project");
        f.harness
            .emit(
                spec.run_id,
                ports::HarnessEvent::SessionStarted {
                    native_session: format!("chat-{}", spec.run_id),
                    actual_model: None,
                    actual_effort: None,
                    account_label: None,
                },
            )
            .await;
        settle(&f.hub, command_id).await;
        f.hub.read(|r| {
            let command = r.command(command_id)?.unwrap();
            assert!(command.error.is_none(), "Chat launch failed: {:?}", command.error);
            let run: Run = r.require("run", spec.run_id)?;
            assert!(run.force_stoppable);
            assert!(!run.holds_writer);
            assert_eq!(r.require::<Project>("project", project)?.active_writer, Some(crate::check_execution::job_writer(job_id)));
            assert_eq!(r.require::<QueueView>("queue", project)?.writer, Some(crate::check_execution::job_writer(job_id)));
            assert!(r.timeline(task_id, None, 100)?.0.iter().any(|item| matches!(&item.item, TimelineBody::Note { text } if text.contains("checking this project"))));
            Ok(())
        }).unwrap();
        let reach = sandbox.specs.lock().unwrap().last().unwrap().reach.clone();
        assert!(reach.write.is_empty());
        assert!(!reach.read.is_empty());
        f.hub
            .finish_run(spec.run_id, RunState::Completed, None)
            .unwrap();
        f.hub
            .read(|r| {
                let writer = Some(crate::check_execution::job_writer(job_id));
                assert_eq!(
                    r.require::<Project>("project", project)?.active_writer,
                    writer
                );
                assert_eq!(r.require::<QueueView>("queue", project)?.writer, writer);
                Ok(())
            })
            .unwrap();
        f.close().await;
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "Needs host user namespaces and a systemd user manager"]
async fn linux_work_exit_runs_a_real_sandboxed_check_through_the_service_layer() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let secret = f.path.join("private-token");
    std::fs::write(&secret, "must stay private").unwrap();
    let mut check = approved_check();
    check.run = format!(
        "test ! -r '{}' && printf 'approved check ran\\n' && printf verified > check-proof.txt",
        secret.display()
    );
    f.hub
        .write(|tx| {
            tx.change_checks(
                project,
                0,
                CheckEdit::Save { check },
                None,
                &f.project_dir(),
            )?;
            Ok(())
        })
        .unwrap();
    let (task, run) = changed_work(&f, project).await;
    end(&f, run).await;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if f.hub
                .read(|r| Ok(r.require::<Task>("task", task)?.state == TaskState::Completed))
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real sandboxed check did not finish");
    f.hub
        .read(|r| {
            let jobs = r.task_check_jobs(task)?;
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].state, CheckJobState::Finished, "{:?}", jobs[0]);
            assert_eq!(jobs[0].sandbox, Some(CheckSandboxLevel::Contained));
            assert_eq!(jobs[0].results.len(), 1);
            assert_eq!(
                jobs[0].results[0].outcome,
                CheckOutcome::Passed,
                "{:?}",
                jobs[0].results
            );
            assert!(jobs[0].results[0].output.contains("approved check ran"));
            assert!(!jobs[0].results[0].output.contains("setup must not run"));
            assert!(
                r.project_checks(project)?.unwrap().checks[0]
                    .typical_secs
                    .is_some()
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(f.project_dir().join("check-proof.txt")).unwrap(),
        "verified"
    );
    f.close().await;
}

#[tokio::test]
async fn a_person_continuing_a_checking_task_stops_checks_before_starting_the_next_run() {
    let f = Fixture::new().await;
    let project = f.project().await;
    approve(&f, project);
    let (task_id, run) = changed_work(&f, project).await;
    end(&f, run).await;
    let task = checking(&f, task_id).await;
    let (queued, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            },
            &Caller::local_owner(),
            false,
        )
        .await;
    f.hub
        .write(|tx| {
            let mut queue: QueueView = tx.read().require("queue", project)?;
            queue.paused = false;
            tx.put("queue", project, &queue)
        })
        .unwrap();
    let command: CommandEnvelope = serde_json::from_value(serde_json::json!({
        "command_id":CommandId::new(), "issued_at":f.clock.now(),
        "expected_revision":task.revision,
        "body":{"command":"resume_task", "task_id":task_id, "text":"Continue this work"}
    }))
    .unwrap();
    let race = async {
        for _ in 0..100 {
            f.hub.advance_queues().await.unwrap();
            tokio::task::yield_now().await;
        }
    };
    let owner = Caller::local_owner();
    let (receipt, ()) = tokio::join!(f.hub.submit(&owner, command.clone()), race);
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let next = run_started(&f, task_id).await;
    assert_ne!(run, next);
    let duplicate = f.hub.submit(&Caller::local_owner(), command).await;
    assert_eq!(duplicate.status, ReceiptStatus::Duplicate, "{duplicate:?}");
    f.hub
        .read(|r| {
            let jobs = r.task_check_jobs(task_id)?;
            assert_eq!(jobs[0].state, CheckJobState::Finished);
            assert_eq!(jobs[0].message.as_deref(), Some("Not checked: stopped"));
            assert_eq!(
                r.require::<Task>("task", queued)?.state,
                TaskState::Accepted
            );
            assert_eq!(r.require::<Task>("task", queued)?.current_run, None);
            assert_eq!(
                r.require::<Project>("project", project)?.active_writer,
                Some(next)
            );
            Ok(())
        })
        .unwrap();
    end(&f, next).await;
    queued_run_started(&f, queued).await;
    assert!(
        f.hub
            .inner
            .checking_continuations
            .lock()
            .unwrap()
            .is_empty()
    );
    f.close().await;
}

#[tokio::test]
async fn failed_continuation_releases_its_claim_and_queued_work_can_start_after_the_normal_pause() {
    let f = Fixture::new().await;
    let project = f.project().await;
    approve(&f, project);
    let (task_id, run) = changed_work(&f, project).await;
    end(&f, run).await;
    let task = checking(&f, task_id).await;
    let (queued, _) = f
        .task(
            project,
            TaskPlacement::Queue {
                position: None,
                authorization: QueueAuthorization::AutoStart,
            },
            &Caller::local_owner(),
            false,
        )
        .await;
    *f.harness.refuse_start.lock().unwrap() = Some("Synthetic launch refusal".into());
    let command: CommandEnvelope = serde_json::from_value(serde_json::json!({
        "command_id":CommandId::new(), "issued_at":f.clock.now(),
        "expected_revision":task.revision,
        "body":{"command":"resume_task", "task_id":task_id, "text":"Continue this work"}
    }))
    .unwrap();
    let id = command.command_id;
    let receipt = f.hub.submit(&Caller::local_owner(), command).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    settle(&f.hub, id).await;
    assert!(
        f.hub
            .inner
            .checking_continuations
            .lock()
            .unwrap()
            .is_empty()
    );
    let queue: QueueView = f.hub.read(|r| r.require("queue", project)).unwrap();
    f.hub
        .read(|r| {
            let task: Task = r.require("task", task_id)?;
            assert_eq!(task.state, TaskState::Failed);
            assert!(
                task.checking.is_none(),
                "a refused launch never ran an agent to check"
            );
            assert_eq!(
                r.task_check_jobs(task_id)?.len(),
                1,
                "the refused continuation must not create another check job"
            );
            Ok(())
        })
        .unwrap();
    assert!(
        queue.paused,
        "A failed Work run still uses the existing safety pause"
    );
    let mut resume = CommandEnvelope::new(Command::QueueResume {
        project_id: project,
    });
    resume.expected_revision = Some(queue.revision);
    let receipt = f.hub.submit(&Caller::local_owner(), resume).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    queued_run_started(&f, queued).await;
    assert_ne!(
        f.hub
            .read(|r| r.require::<Task>("task", queued))
            .unwrap()
            .state,
        TaskState::Failed
    );
    f.close().await;
}

#[tokio::test]
async fn the_transaction_boundary_rejects_an_unlisted_writer_path_during_continue() {
    let f = Fixture::new().await;
    let project = f.project().await;
    approve(&f, project);
    let (task, run) = changed_work(&f, project).await;
    end(&f, run).await;
    let job = checking(&f, task).await.checking.unwrap().job_id;
    f.hub.inner.checking_continuations.lock().unwrap().insert(
        project,
        crate::commands::CheckingContinuation {
            run_id: run,
            command_id: CommandId::new(),
        },
    );
    let other = RunId::new();
    let result = f.hub.write(|tx| {
        let mut p: Project = tx.read().require("project", project)?;
        p.active_writer = Some(other);
        tx.put("project", project, &p)?;
        let mut q: QueueView = tx.read().require("queue", project)?;
        q.writer = Some(other);
        tx.put("queue", project, &q)
    });
    assert!(matches!(result, Err(ref cause) if cause.code == ErrorCode::Conflict));
    assert_eq!(
        f.hub
            .read(|r| Ok(r.require::<Project>("project", project)?.active_writer))
            .unwrap(),
        Some(crate::check_execution::job_writer(job))
    );
    f.hub.inner.checking_continuations.lock().unwrap().clear();
    f.close().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "Needs host user namespaces and a systemd user manager"]
async fn linux_standalone_run_now_reads_fake_toolchain_but_cannot_read_private_files() {
    let temporary_home = tempfile::tempdir().unwrap();
    let home = temporary_home.path().canonicalize().unwrap();
    let toolchain = home.join(".cargo/bin");
    std::fs::create_dir_all(&toolchain).unwrap();
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(toolchain.join("check-fixture"), "toolchain-readable").unwrap();
    let token = home.join(".cargo/credentials.toml");
    let private = home.join(".ssh/private");
    std::fs::write(&token, "must stay private").unwrap();
    std::fs::write(&private, "must stay private").unwrap();
    let f = Fixture::with(|builder| builder.home_dir(home)).await;
    let project = f.project().await;
    let check = Check {
        run: format!(
            "test \"$(cat '{}')\" = toolchain-readable && test ! -r '{}' && test ! -r '{}' && printf 'standalone toolchain verified\\n'",
            toolchain.join("check-fixture").display(),
            token.display(),
            private.display()
        ),
        ..approved_check()
    };
    let check_id = check.check_id;
    f.hub
        .write(|tx| {
            tx.change_checks(
                project,
                0,
                CheckEdit::Save { check },
                None,
                &f.project_dir(),
            )?;
            Ok(())
        })
        .unwrap();
    let receipt = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::ChecksRunNow {
                project_id: project,
                check_ids: vec![check_id],
            }),
        )
        .await;
    let Some(CommandResult::CheckJob { job }) = receipt.result else {
        panic!("{receipt:?}");
    };
    assert!(job.task_id.is_none());
    let finished = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let current = f.hub.read(|r| Ok(r.check_job(job.id)?.unwrap())).unwrap();
            if matches!(
                current.state,
                CheckJobState::Finished | CheckJobState::Uncertain
            ) {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("standalone sandboxed check did not finish");
    assert_eq!(finished.state, CheckJobState::Finished, "{finished:?}");
    assert_eq!(finished.sandbox, Some(CheckSandboxLevel::Contained));
    assert_eq!(
        finished.results[0].outcome,
        CheckOutcome::Passed,
        "{finished:?}"
    );
    assert!(
        finished.results[0]
            .output
            .contains("standalone toolchain verified")
    );
    f.hub
        .read(|r| {
            assert!(
                r.require::<Project>("project", project)?
                    .active_writer
                    .is_none()
            );
            assert!(r.require::<QueueView>("queue", project)?.writer.is_none());
            Ok(())
        })
        .unwrap();
    f.close().await;
}
