// SPDX-License-Identifier: Apache-2.0
//! Real composition without the owner's desktop, agents, accounts or services.
//! This deliberately does not open a default terminal: that would start the
//! owner's login shell, whose startup files are outside ComposeOptions' control.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use companion::{
    compose::{ComposeOptions, DesktopFactory, DesktopSession, Runtime},
    config::{Config, Paths},
    identity::{FileSecretStore, Identity},
    local::{OwnerRequest, OwnerResponse, OwnerRuntime},
};
use protocol::{auth::Grants, capability::*, command::*, model::*, *};
use services::{Caller, Hub, api::Outbound};
use std::{path::PathBuf, sync::Arc, time::Duration};

const DEADLINE: Duration = Duration::from_secs(30);

struct ClosedDesktop;
#[async_trait::async_trait]
impl DesktopFactory for ClosedDesktop {
    async fn open(&self, _: &Paths, _: &ComposeOptions) -> companion::Result<DesktopSession> {
        panic!("a composition fixture must never open the native desktop")
    }
}

struct FixtureIndicator;
#[async_trait::async_trait]
impl ports::desktop::LocalIndicator for FixtureIndicator {
    async fn show(&self, _: &str) -> Result<(), ports::PlatformError> {
        panic!("a composition fixture must never acquire computer control")
    }
    async fn hide(&self) -> Result<(), ports::PlatformError> {
        Ok(())
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    paths: Paths,
    identity: Identity,
    options: ComposeOptions,
    project: PathBuf,
    notes: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = test_support::private_tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        let identity = Identity::load_or_create(
            &FileSecretStore::new(paths.identity()),
            vec!["localhost".into()],
        )
        .unwrap();
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_owned();
        let copy = |crate_name: &str, filename: &str| {
            let destination = temp.path().join(filename);
            test_support::executable(
                &destination,
                std::fs::read(fixtures.join(crate_name).join("tests").join(filename)).unwrap(),
            )
            .unwrap();
            destination
        };
        let project = temp.path().join("project");
        let notes = temp.path().join("notes");
        let home = temp.path().join("home");
        for path in [&project, &notes, &home] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(notes.join("fixture.md"), "original\n").unwrap();
        let options = ComposeOptions {
            environment: [("HOME".into(), home.to_string_lossy().into_owned())]
                .into_iter()
                .collect(),
            executable_dir: temp.path().join("no-tools"),
            clock: Arc::new(ports::SystemClock),
            claude_binary: Some(copy("harness-claude", "fake_claude.py")),
            codex_binary: Some(copy("harness-codex", "fake_codex.py")),
            desktop_factory: Some(Arc::new(ClosedDesktop)),
            speech_factory: Arc::new(companion::frontdoors::LocalSpeech),
            accounts: false,
            release_check: false,
        };
        Self {
            _temp: temp,
            paths,
            identity,
            options,
            project,
            notes,
        }
    }

    async fn start(&self) -> Arc<Runtime> {
        let config = if self.paths.config_file().exists() {
            Config::load(&self.paths.config_file()).unwrap()
        } else {
            let config = Config::default();
            config.save(&self.paths.config_file()).unwrap();
            config
        };
        assert!(!config.desktop_enabled);
        let runtime = tokio::time::timeout(
            DEADLINE,
            Runtime::build(
                self.paths.clone(),
                config,
                &self.identity,
                self.options.clone(),
            ),
        )
        .await
        .expect("composition startup timed out")
        .unwrap();
        // As in composition.rs, replace local protection with a fixture. On
        // macOS connect() re-executes this integration-test binary, not Ophio's
        // main: libtest rejects __local-stop-indicator, so no AppKit UI starts.
        // Its child is kill-on-drop; Runtime::shutdown joins the retry task.
        runtime
            .hub
            .set_local_indicator(Some(Arc::new(FixtureIndicator)))
            .await;
        runtime
    }
}

/// Subscribe before admission so even an immediately finished effect is seen.
async fn effect(runtime: &Runtime, command: Command) -> Option<CommandResult> {
    tokio::time::timeout(DEADLINE, async {
        let caller = Caller::local_owner();
        let mut events = runtime
            .hub
            .subscribe(&caller, Some(runtime.hub.journal_head()))
            .await
            .unwrap();
        let receipt = runtime
            .hub
            .submit(&caller, CommandEnvelope::new(command))
            .await;
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        if receipt.result.is_some() {
            return receipt.result;
        }
        loop {
            if let Outbound::Event(envelope) = events.live.recv().await.unwrap()
                && let Event::CommandProgress {
                    command_id,
                    stage,
                    result,
                    error,
                    ..
                } = &envelope.event
                && *command_id == receipt.command_id
                && *stage != CommandStage::HostAccepted
            {
                assert_eq!(*stage, CommandStage::Observed, "{envelope:?}");
                assert!(error.is_none(), "{error:?}");
                return result.clone();
            }
        }
    })
    .await
    .expect("command effect timed out")
}

async fn saved_reply(runtime: &Runtime, task_id: TaskId) {
    let QueryData::TaskTimeline { items, .. } = runtime
        .hub
        .query(
            &Caller::local_owner(),
            Query::TaskTimeline {
                task_id,
                before_seq: None,
                limit: 100,
            },
        )
        .await
        .unwrap()
    else {
        panic!("expected task timeline")
    };
    assert!(
        items.iter().any(|item| matches!(
            &item.item, TimelineBody::AgentMessage { text, .. } if text.contains("hello")
        )),
        "the fake agent's reply was not saved: {items:?}"
    );
    assert!(
        items
            .iter()
            .any(|item| matches!(&item.item, TimelineBody::RunEnded { .. }))
    );
}

#[tokio::test]
async fn native_runtime_saves_fake_agent_work_and_enforces_library_grants_across_restart() {
    // One overall deadline includes startup, commands, event waits and restart.
    tokio::time::timeout(Duration::from_secs(90), async {
        let fixture = Fixture::new();
        let runtime = fixture.start().await;
        let caller = Caller::local_owner();
        let first_boot = runtime.hub.boot_epoch();
        let QueryData::Snapshot(snapshot) = runtime
            .hub
            .query(
                &caller,
                Query::Snapshot {
                    reads_withheld: None,
                    account_items: false,
                },
            )
            .await
            .unwrap()
        else {
            panic!("expected startup snapshot")
        };
        assert_eq!(snapshot.host.host_id, fixture.identity.host_id);
        assert!(matches!(
            snapshot.host.capabilities.capture,
            Availability::PermissionMissing { .. }
        ));
        assert!(matches!(
            runtime.hub.query(&caller, Query::Readiness).await.unwrap(),
            QueryData::Readiness { .. }
        ));

        let Some(CommandResult::ProjectAdded { project_id }) = effect(
            &runtime,
            Command::AddProject {
                host_path: fixture.project.to_string_lossy().into_owned(),
                name: Some("Native fixture".into()),
            },
        )
        .await
        else {
            panic!("expected project")
        };
        let mut events = runtime
            .hub
            .subscribe(&caller, Some(runtime.hub.journal_head()))
            .await
            .unwrap();
        // A normal coding task uses the authored Codex process, not a sandboxed
        // Claude conversation (which can copy the real Mac login keychain).
        // No production permissions are changed to run this fixture.
        let Some(CommandResult::TaskCreated { task_id, .. }) = effect(
            &runtime,
            Command::CreateTask {
                origin: None,
                from_chat: None,
                github: None,
                agent_chosen: None,
                mode: Some(TaskMode::Work),
                project_id,
                harness: HarnessKind::Codex,
                prompt: "complete".into(),
                kind: TaskKind::Coding,
                title: None,
                bundle: None,
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
                conversation: None,
                access: None,
                review: None,
            },
        )
        .await
        else {
            panic!("expected task")
        };
        tokio::time::timeout(DEADLINE, async {
            loop {
                if let Outbound::Event(envelope) = events.live.recv().await.unwrap()
                    && let Event::RunUpserted { run } = &envelope.event
                    && run.task_id == task_id
                    && matches!(
                        run.state,
                        RunState::Completed | RunState::Failed | RunState::Interrupted
                    )
                {
                    assert_eq!(run.state, RunState::Completed, "{run:?}");
                    break;
                }
            }
        })
        .await
        .expect("fake agent did not finish");
        saved_reply(&runtime, task_id).await;

        let OwnerResponse::Settings {
            config,
            applied_live,
            ..
        } = runtime
            .request(OwnerRequest::RootsAdd {
                path: fixture.notes.clone(),
                label: Some("Fixture notes".into()),
                writable: true,
            })
            .await
            .unwrap()
        else {
            panic!("expected Library settings")
        };
        assert!(applied_live);
        let root_id = config
            .roots
            .iter()
            .find(|root| root.path == fixture.notes)
            .unwrap()
            .root_id;
        assert!(matches!(
            runtime
                .hub
                .query(
                    &caller,
                    Query::LibraryList {
                        root_id,
                        dir: "".into()
                    }
                )
                .await
                .unwrap(),
            QueryData::LibraryList { .. }
        ));
        let QueryData::LibraryRead { file } = runtime
            .hub
            .query(
                &caller,
                Query::LibraryRead {
                    root_id,
                    path: "fixture.md".into(),
                },
            )
            .await
            .unwrap()
        else {
            panic!("expected fixture file")
        };
        let save = |text: &str| Command::LibrarySave {
            root_id,
            path: "fixture.md".into(),
            text: text.into(),
            base_revision: file.revision.clone(),
            newline: file.newline,
            bom: file.has_bom,
        };
        let mut limited = caller.clone();
        limited.grants = Grants {
            library_read: false,
            library_write: false,
            ..Grants::owner()
        };
        let error = runtime
            .hub
            .query(
                &limited,
                Query::LibraryRead {
                    root_id,
                    path: "fixture.md".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied);
        let refused = runtime
            .hub
            .submit(
                &limited,
                CommandEnvelope::new(save("must not be written\n")),
            )
            .await;
        assert_eq!(refused.status, ReceiptStatus::Rejected);
        assert_eq!(refused.error.unwrap().code, ErrorCode::PermissionDenied);
        assert_eq!(
            std::fs::read_to_string(fixture.notes.join("fixture.md")).unwrap(),
            "original\n"
        );
        assert!(matches!(
            effect(&runtime, save("saved\n")).await,
            Some(CommandResult::Saved {
                outcome: protocol::library::SaveOutcome::Saved { .. }
            })
        ));
        assert!(matches!(
            effect(&runtime, save("stale\n")).await,
            Some(CommandResult::Saved {
                outcome: protocol::library::SaveOutcome::Conflict { .. }
            })
        ));

        tokio::time::timeout(DEADLINE, runtime.shutdown())
            .await
            .expect("shutdown timed out")
            .unwrap();
        drop(events);
        let hub = runtime.hub.as_ref().clone();
        drop(runtime);
        services::testing::drop_hub_and_wait(hub, DEADLINE).await;
        let restarted = fixture.start().await;
        assert_eq!(restarted.hub.host_id(), fixture.identity.host_id);
        assert_ne!(restarted.hub.boot_epoch(), first_boot);
        let QueryData::Snapshot(snapshot) = restarted
            .hub
            .query(
                &caller,
                Query::Snapshot {
                    reads_withheld: None,
                    account_items: false,
                },
            )
            .await
            .unwrap()
        else {
            panic!("expected restored snapshot")
        };
        assert!(
            snapshot
                .projects
                .iter()
                .any(|project| project.project_id == project_id)
        );
        assert!(snapshot.tasks.iter().any(|task| task.task_id == task_id));
        saved_reply(&restarted, task_id).await;
        assert!(matches!(
            restarted
                .hub
                .query(
                    &caller,
                    Query::LibraryRead {
                        root_id,
                        path: "fixture.md".into()
                    }
                )
                .await
                .unwrap(),
            QueryData::LibraryRead { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(fixture.notes.join("fixture.md")).unwrap(),
            "saved\n"
        );
        tokio::time::timeout(DEADLINE, restarted.shutdown())
            .await
            .expect("restart shutdown timed out")
            .unwrap();
    })
    .await
    .expect("native composition workflow timed out");
}
