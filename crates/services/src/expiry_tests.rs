// SPDX-License-Identifier: Apache-2.0
//! What an agent made goes a week after the person last looked at it; what
//! the person kept or sent, and the pages they added, stay.
use super::shown::{PNG, bound, person};
use super::*;
use crate::expiry::SHOWN_FOR;
use crate::expiry::page_seen;
use ports::bridge::PersonCall;
use protocol::{
    auth::Principal,
    model::{ArtifactLink, ArtifactRole},
};

async fn finished(f: &Fixture, task: TaskId, run: RunId) {
    f.harness
        .emit(
            run,
            ports::HarnessEvent::Exited {
                exit: ports::RunExit::Completed,
                outcome: None,
            },
        )
        .await;
    let mut wait = waiting();
    loop {
        let (task, run): (Task, Run) = f
            .hub
            .read(|r| Ok((r.require("task", task)?, r.require("run", run)?)))
            .unwrap();
        if task.state == TaskState::Completed && !crate::runs::still_finishing(&run) {
            break;
        }
        assert!(wait(), "the run did not finish");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn linked(task: TaskId, run: RunId, role: ArtifactRole) -> Option<ArtifactLink> {
    Some(ArtifactLink {
        task_id: task,
        run_id: Some(run),
        annotation: None,
        role,
    })
}

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

#[tokio::test]
async fn what_the_agent_made_goes_a_week_after_it_was_last_looked_at() {
    let f = Fixture::new().await;
    let (task, run, binding) = bound(&f).await;
    let scratch = f.hub.scratch_dir(task);
    let mut shown = Vec::new();
    for name in ["home.png", "drawn-on.png"] {
        std::fs::write(scratch.join(name), [PNG, name.as_bytes()].concat()).unwrap();
        let path = scratch.join(name).to_str().unwrap().to_owned();
        person(
            &f,
            &binding,
            PersonCall::Show {
                path,
                caption: None,
                invite: None,
            },
        )
        .await
        .unwrap();
        let meta = (f
            .hub
            .read(|r| r.list::<ArtifactMeta>("artifact"))
            .unwrap()
            .into_iter())
        .find(|a| a.label.as_deref() == Some(name))
        .unwrap();
        shown.push(meta);
    }
    let [shown, drawn_on] = <[_; 2]>::try_from(shown).unwrap();
    // The person drew on one and sent it.
    f.hub
        .write(|tx| {
            let mut meta: ArtifactMeta = tx.read().require("artifact", drawn_on.artifact_id)?;
            meta.links.extend(linked(task, run, ArtifactRole::Source));
            tx.put("artifact", meta.artifact_id, &meta)
        })
        .unwrap();
    let put = |bytes: &'static [u8], role, by| {
        f.hub
            .store_artifact(bytes, "image/png", linked(task, run, role), by, None, None)
    };
    let screenshot = put(
        b"agent screenshot",
        ArtifactRole::Observation,
        Principal::System,
    )
    .await
    .unwrap();
    let after = put(b"after evidence", ArtifactRole::After, Principal::System)
        .await
        .unwrap();
    let captured = put(
        b"phone capture",
        ArtifactRole::Observation,
        Principal::LocalOwner,
    )
    .await
    .unwrap();
    let present = |meta: &ArtifactMeta| {
        f.hub
            .read(|r| r.get::<ArtifactMeta>("artifact", meta.artifact_id))
            .unwrap()
            .is_some()
    };
    let listed = || async {
        let project = f
            .hub
            .read(|r| r.require::<Task>("task", task))
            .unwrap()
            .project_id;
        let listed = f
            .hub
            .query(
                &Caller::local_owner(),
                Query::ProjectShown {
                    project_id: project,
                    before: None,
                    limit: 10,
                },
            )
            .await
            .unwrap();
        let QueryData::ProjectShown { history } = listed else {
            panic!("{listed:?}")
        };
        let mut names: Vec<_> = (history.artifacts.into_iter())
            .map(|a| a.label.unwrap())
            .collect();
        names.sort();
        names
    };

    // However long it runs.
    f.clock.advance(SHOWN_FOR * 2);
    assert_eq!(f.hub.expire_shown().await.unwrap(), 0);
    finished(&f, task, run).await;
    assert_eq!(f.hub.expire_shown().await.unwrap(), 0);
    // Opened on the phone the day before its week is up, it stays a week
    // more; the screenshot nobody opened goes.
    f.clock.advance(SHOWN_FOR - DAY);
    let opened = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::CreateDownload {
                shown_in_task: None,
                source: DownloadSource::Artifact {
                    artifact_id: shown.artifact_id,
                },
            }),
        )
        .await;
    assert_eq!(opened.status, ReceiptStatus::Accepted, "{opened:?}");
    f.clock.advance(DAY);
    assert_eq!(f.hub.expire_shown().await.unwrap(), 1);
    for (meta, kept) in [
        (&shown, true),
        (&screenshot, false),
        (&drawn_on, true),
        (&after, true),
        (&captured, true),
    ] {
        assert_eq!(present(meta), kept, "{:?}", meta.label);
    }
    // The project lists it while it stays, however long ago its task ended.
    assert_eq!(listed().await, ["drawn-on.png", "home.png"]);
    f.clock.advance(SHOWN_FOR - DAY - Duration::from_secs(1));
    assert_eq!(f.hub.expire_shown().await.unwrap(), 0);
    f.clock.advance(Duration::from_secs(1));
    assert_eq!(f.hub.expire_shown().await.unwrap(), 1);
    assert!(!present(&shown));
    assert!(present(&drawn_on));
    assert_eq!(listed().await, ["drawn-on.png"]);

    // What still names it hears why it's gone.
    let Err(gone) = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::Artifact {
                artifact_id: shown.artifact_id,
            },
        )
        .await
    else {
        panic!("an expired artifact")
    };
    assert_eq!(gone.code, ErrorCode::Expired, "{gone:?}");
    assert!(gone.message.contains("after a week unopened"));
    // Its bytes go with the next collection, the rest stay.
    f.hub.collect_transfers().await.unwrap();
    for (meta, kept) in [
        (&shown, false),
        (&screenshot, false),
        (&drawn_on, true),
        (&after, true),
    ] {
        assert_eq!(
            f.hub.artifact_path(&meta.sha256).unwrap().exists(),
            kept,
            "{:?}",
            meta.label
        );
    }
    assert_eq!(f.hub.expire_shown().await.unwrap(), 0);
    let seen: Vec<(String, protocol::time::Timestamp)> =
        f.hub.read(|r| r.entries("artifact_seen")).unwrap();
    assert!(seen.is_empty(), "{seen:?}");
    f.close().await;
}

#[tokio::test]
async fn a_queued_continuation_keeps_the_tasks_pictures_until_removed() {
    let f = Fixture::new().await;
    let (task, run, _) = bound(&f).await;
    let picture = f
        .hub
        .store_artifact(
            b"shown picture",
            "image/png",
            linked(task, run, ArtifactRole::Shown),
            Principal::System,
            None,
            None,
        )
        .await
        .unwrap();
    finished(&f, task, run).await;
    let project_id = f
        .hub
        .read(|r| Ok(r.require::<Task>("task", task)?.project_id))
        .unwrap();
    for command in [
        Command::QueuePause { project_id },
        Command::QueueContinuation {
            task_id: task,
            text: "Look again at the picture you showed me".into(),
            bundle: None,
            authorization: QueueAuthorization::AskBeforeStart,
        },
    ] {
        let revision = f
            .hub
            .read(|r| match command {
                Command::QueuePause { .. } => {
                    Ok(r.require::<QueueView>("queue", project_id)?.revision)
                }
                _ => Ok(r.require::<Task>("task", task)?.revision),
            })
            .unwrap();
        let mut command = CommandEnvelope::new(command);
        command.expected_revision = Some(revision);
        let receipt = f.hub.submit(&Caller::local_owner(), command).await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    }
    let queue: QueueView = f.hub.read(|r| r.require("queue", project_id)).unwrap();
    assert!(queue.paused);
    let item = queue
        .items
        .iter()
        .find(|i| i.task_id == task && i.is_pending())
        .unwrap();
    f.clock.advance(SHOWN_FOR * 2);
    assert_eq!(f.hub.expire_shown().await.unwrap(), 0);
    assert!(
        f.hub
            .read(|r| r.get::<ArtifactMeta>("artifact", picture.artifact_id))
            .unwrap()
            .is_some()
    );

    let mut command = CommandEnvelope::new(Command::QueueRemove {
        item_id: item.item_id,
    });
    command.expected_revision = Some(queue.revision);
    let receipt = f.hub.submit(&Caller::local_owner(), command).await;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    // Removing the continuation starts a fresh week for the stopped task.
    assert_eq!(f.hub.expire_shown().await.unwrap(), 0);
    f.clock.advance(SHOWN_FOR);
    assert_eq!(f.hub.expire_shown().await.unwrap(), 1);
    f.close().await;
}

#[tokio::test]
async fn pending_queue_items_keep_artifacts_for_every_attached_task() {
    let f = Fixture::new().await;
    let (task, run, _) = bound(&f).await;
    let sample = f
        .hub
        .store_artifact(
            b"screenshot",
            "image/png",
            linked(task, run, ArtifactRole::Observation),
            Principal::System,
            None,
            None,
        )
        .await
        .unwrap();
    finished(&f, task, run).await;
    let mut queued_task = Task {
        task_id: TaskId::new(),
        current_run: None,
        ..f.hub.read(|r| r.require("task", task)).unwrap()
    };
    let mut queue: QueueView = f
        .hub
        .read(|r| r.require("queue", queued_task.project_id))
        .unwrap();
    queue.items = vec![QueueItem {
        item_id: QueueItemId::new(),
        task_id: queued_task.task_id,
        position: 0,
        depends_on: vec![],
        authorization: QueueAuthorization::AskBeforeStart,
        state: QueueItemState::Waiting,
        added_by: Principal::LocalOwner,
        added_at: sample.created_at,
        request: None,
        authorization_expires_at: None,
    }];
    let mut shared = ArtifactMeta {
        artifact_id: ArtifactId::new(),
        ..sample.clone()
    };
    shared.links.push(ArtifactLink {
        task_id: queued_task.task_id,
        run_id: None,
        annotation: None,
        role: ArtifactRole::Shown,
    });
    let primary = ArtifactMeta {
        artifact_id: ArtifactId::new(),
        task_id: Some(queued_task.task_id),
        media_type: "video/webm".into(),
        ..sample.clone()
    };
    f.hub
        .write(|tx| {
            for meta in [&shared, &primary] {
                tx.put("artifact", meta.artifact_id, meta)?;
                tx.put("artifact_creator", meta.artifact_id, &Principal::System)?;
            }
            Ok(())
        })
        .unwrap();
    f.clock.advance(SHOWN_FOR * 2);
    let mut expected_expired = 1; // The unrelated task's screenshot still expires.
    for state in [
        TaskState::Completed,
        TaskState::Failed,
        TaskState::Interrupted,
    ] {
        queued_task.state = state;
        for pending in [
            QueueItemState::Waiting,
            QueueItemState::AwaitingStart,
            QueueItemState::Blocked {
                reason: "A dependency failed".into(),
            },
        ] {
            queue.paused = pending == QueueItemState::Waiting;
            queue.items[0].state = pending;
            f.hub
                .write(|tx| {
                    tx.put("task", queued_task.task_id, &queued_task)?;
                    tx.put("queue", queue.project_id, &queue)
                })
                .unwrap();
            assert_eq!(f.hub.expire_shown().await.unwrap(), expected_expired);
            expected_expired = 0;
            for meta in [&shared, &primary] {
                assert!(
                    f.hub
                        .read(|r| r.get::<ArtifactMeta>("artifact", meta.artifact_id))
                        .unwrap()
                        .is_some(),
                    "{state:?}, {:?}",
                    queue.items[0].state
                );
            }
        }
    }
    // A started item kept as queue history doesn't keep a stopped task alive.
    queue.items[0].state = QueueItemState::Started { run_id: run };
    f.hub
        .write(|tx| tx.put("queue", queue.project_id, &queue))
        .unwrap();
    assert_eq!(f.hub.expire_shown().await.unwrap(), 2);
    f.close().await;
}

/// Every page is stopped.
#[derive(Debug)]
struct Stopped;
#[async_trait::async_trait]
impl crate::preview::PreviewChecker for Stopped {
    async fn check(&self, _: &url::Url) -> Result<bool, ProtocolError> {
        Ok(false)
    }
}

#[tokio::test]
async fn a_page_an_agent_showed_goes_a_week_after_it_was_last_opened() {
    let f = Fixture::with(|b| b.preview_checker(Arc::new(Stopped))).await;
    let (task, _, binding) = bound(&f).await;
    let project_id = f
        .hub
        .read(|r| r.require::<Task>("task", task))
        .unwrap()
        .project_id;
    let pages = || {
        let mut labels: Vec<String> = (f.hub.read(|r| r.list::<PreviewEndpoint>("preview")))
            .unwrap()
            .into_iter()
            .map(|p| p.label)
            .collect();
        labels.sort();
        labels
    };
    let show = |url: &str, label: &str| PersonCall::Preview {
        url: url.into(),
        label: Some(label.into()),
        note: None,
        start: Some("npm run dev".into()),
        questions: Vec::new(),
    };
    person(&f, &binding, show("http://127.0.0.1:5179/", "Shown"))
        .await
        .unwrap();
    person(&f, &binding, show("http://127.0.0.1:5180/", "Shown again"))
        .await
        .unwrap();
    let added = f
        .hub
        .submit(
            &Caller::local_owner(),
            CommandEnvelope::new(Command::RegisterPreview {
                project_id,
                label: "Added".into(),
                url: "http://127.0.0.1:5181/".into(),
            }),
        )
        .await;
    assert_eq!(added.status, ReceiptStatus::Accepted, "{added:?}");
    let shown = (f.hub.read(|r| r.list::<PreviewEndpoint>("preview")))
        .unwrap()
        .into_iter()
        .find(|p| p.label == "Shown")
        .unwrap();
    // One an agent showed before pages were counted, and one found running.
    f.hub
        .write(|tx| {
            for (label, automatic) in [("Shown before", false), ("Found", true)] {
                let p = PreviewEndpoint {
                    preview_id: PreviewId::new(),
                    label: label.into(),
                    automatic,
                    start: (!automatic).then(|| "npm run dev".into()),
                    ..shown.clone()
                };
                tx.put("preview", p.preview_id, &p)?;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(f.hub.expire_shown().await.unwrap(), 0);

    // Opened on the phone, or shown again, a page stays a week more.
    f.clock.advance(SHOWN_FOR - DAY);
    let command = CommandEnvelope::new(Command::CheckPreview {
        preview_id: shown.preview_id,
        start: false,
    });
    let id = command.command_id;
    f.hub.submit(&Caller::local_owner(), command).await;
    settle(&f.hub, id).await;
    person(&f, &binding, show("http://127.0.0.1:5180/", "Shown again"))
        .await
        .unwrap();
    f.clock.advance(DAY);
    assert_eq!(f.hub.expire_shown().await.unwrap(), 1);
    assert_eq!(pages(), ["Added", "Found", "Shown", "Shown again"]);
    f.clock.advance(SHOWN_FOR - DAY);
    assert_eq!(f.hub.expire_shown().await.unwrap(), 2);
    // What the person added stays, and one found running is the finder's.
    f.clock.advance(SHOWN_FOR * 10);
    assert_eq!(f.hub.expire_shown().await.unwrap(), 0);
    assert_eq!(pages(), ["Added", "Found"]);
    // Opening one the person added doesn't start a week for it.
    f.hub
        .write(|tx| {
            for p in tx.read().list::<PreviewEndpoint>("preview")? {
                page_seen(tx, p.preview_id, false, ports::Clock::now(&*f.clock))?;
            }
            Ok(())
        })
        .unwrap();
    let seen: Vec<(String, protocol::time::Timestamp)> =
        f.hub.read(|r| r.entries("preview_seen")).unwrap();
    assert!(seen.is_empty(), "{seen:?}");
    f.close().await;
}

// Current-thread tests keep the synchronous sweep and its SQL trace together.
thread_local! {
    static REFERENCE_SCANS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

fn count_reference_scans(event: rusqlite::trace::TraceEvent<'_>) {
    let rusqlite::trace::TraceEvent::Stmt(statement, _) = event else {
        return;
    };
    let Some(sql) = statement.expanded_sql() else {
        return;
    };
    REFERENCE_SCANS.with(|counts| {
        let (mut prompts, mut queues) = counts.get();
        if sql == "SELECT json FROM documents WHERE kind='prompt' ORDER BY id" {
            prompts += 1;
        }
        if sql == "SELECT json FROM documents WHERE kind='queue' ORDER BY id" {
            queues += 1;
        }
        counts.set((prompts, queues));
    });
}

#[tokio::test]
async fn expiry_scans_prompt_and_queue_collections_at_most_once() {
    let f = Fixture::new().await;
    let (task, run, _) = bound(&f).await;
    finished(&f, task, run).await;
    let sample = f
        .hub
        .store_artifact(
            b"screenshot",
            "image/png",
            linked(task, run, ArtifactRole::Observation),
            Principal::System,
            None,
            None,
        )
        .await
        .unwrap();
    f.hub
        .write(|tx| {
            for i in 0..1000 {
                let meta = ArtifactMeta {
                    artifact_id: ArtifactId::new(),
                    ..sample.clone()
                };
                tx.put("artifact", meta.artifact_id, &meta)?;
                tx.put("artifact_creator", meta.artifact_id, &Principal::System)?;
                tx.put(
                    "prompt",
                    format!("saved-{i}"),
                    &(
                        "x".repeat(8192),
                        None::<Box<protocol::annotation::AnnotationBundle>>,
                    ),
                )?;
            }
            Ok(())
        })
        .unwrap();
    // Bundles protect artifacts even when no Source link was persisted.
    let prompt_source = ArtifactId::new();
    let queue_source = ArtifactId::new();
    let bundle = |artifact_id| {
        use protocol::annotation::{AnnotationBundle, AnnotationSource, SourceOrigin};
        Some(Box::new(AnnotationBundle {
            bundle_id: AnnotationBundleId::new(),
            revision: 1,
            sources: vec![AnnotationSource {
                key: "S1".into(),
                artifact_id,
                sha256: sample.sha256.clone(),
                media_type: sample.media_type.clone(),
                width: 1,
                height: 1,
                origin: SourceOrigin::Shown { artifact_id },
                captured_at: sample.created_at,
                derived_from: None,
                derivation: None,
            }],
            marks: vec![],
            comments: vec![],
            steps: vec![],
            general_note: None,
            speech: None,
        }))
    };
    let project_id = f
        .hub
        .read(|r| Ok(r.require::<Task>("task", task)?.project_id))
        .unwrap();
    f.hub
        .write(|tx| {
            for artifact_id in [prompt_source, queue_source] {
                let meta = ArtifactMeta {
                    artifact_id,
                    ..sample.clone()
                };
                tx.put("artifact", artifact_id, &meta)?;
                tx.put("artifact_creator", artifact_id, &Principal::System)?;
            }
            tx.put(
                "prompt",
                "saved-0",
                &("x".repeat(8192), bundle(prompt_source)),
            )?;
            tx.put(
                "queue",
                project_id,
                &QueueView {
                    project_id,
                    revision: 1,
                    paused: true,
                    pause_reason: None,
                    writer: None,
                    items: vec![QueueItem {
                        item_id: QueueItemId::new(),
                        // A different task's queued message keeps its sources,
                        // without keeping this stopped task's other pictures.
                        task_id: TaskId::new(),
                        position: 0,
                        depends_on: vec![],
                        authorization: QueueAuthorization::AskBeforeStart,
                        state: QueueItemState::Waiting,
                        added_by: Principal::LocalOwner,
                        added_at: sample.created_at,
                        request: Some(QueuedRequest {
                            prompt: "queued".into(),
                            bundle: bundle(queue_source),
                        }),
                        authorization_expires_at: None,
                    }],
                },
            )
        })
        .unwrap();
    f.hub
        .inner
        .db
        .lock()
        .unwrap()
        .trace(Some(count_reference_scans));
    REFERENCE_SCANS.with(|counts| counts.set((0, 0)));
    assert_eq!(f.hub.expire_shown().await.unwrap(), 0);
    assert_eq!(
        REFERENCE_SCANS.with(|counts| counts.get()),
        (0, 0),
        "no scans before anything is old"
    );
    f.clock.advance(SHOWN_FOR);
    REFERENCE_SCANS.with(|counts| counts.set((0, 0)));
    let expired = f.hub.expire_shown().await.unwrap();
    f.hub.inner.db.lock().unwrap().trace(None);
    assert_eq!(expired, 1001);
    let scans = REFERENCE_SCANS.with(|counts| counts.get());
    for id in [prompt_source, queue_source] {
        assert!(
            f.hub
                .read(|r| r.get::<ArtifactMeta>("artifact", id))
                .unwrap()
                .is_some()
        );
    }
    // Removing references lets a later sweep expire them: no cross-sweep cache.
    f.hub
        .write(|tx| {
            tx.delete("prompt", "saved-0")?;
            tx.delete("queue", project_id)
        })
        .unwrap();
    assert_eq!(f.hub.expire_shown().await.unwrap(), 2);
    f.close().await;
    assert_eq!(
        scans,
        (1, 1),
        "each collection should be scanned once per sweep"
    );
}
