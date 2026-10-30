// SPDX-License-Identifier: Apache-2.0
use super::*;

#[tokio::test]
async fn ask_and_plan_admission_do_not_require_an_installed_permission_bridge() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/service-tests")
        .join(uuid::Uuid::now_v7().to_string());
    let harness = Arc::new(FakeHarness::default());
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .clock(Arc::new(FakeClock::default()))
        .harness(harness)
        .build()
        .await
        .unwrap();
    // Advertise the same read-only modes as ManagedClaude's real adapter;
    // retain refresh_catalog's start_run capability exactly as published.
    hub.write(|tx| {
        let mut infos: Vec<protocol::capability::HarnessInfo> =
            tx.read().require("meta", "harnesses")?;
        infos[0]
            .modes
            .extend([TaskMode::Ask, TaskMode::Plan].into_iter().map(|mode| {
                protocol::capability::ModeSupport {
                    mode,
                    availability: protocol::capability::Availability::Available,
                    enforcement: "Synthetic read-only fixture".into(),
                }
            }));
        tx.put("meta", "harnesses", &infos)
    })
    .unwrap();
    let owner = Caller::local_owner();
    let folder = path.with_extension("project");
    std::fs::create_dir_all(&folder).unwrap();
    let added = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::AddProject {
                host_path: folder.to_string_lossy().into_owned(),
                name: Some("Read-only fixture".into()),
            }),
        )
        .await;
    let Some(CommandResult::ProjectAdded { project_id }) = added.result else {
        panic!("project admission failed: {added:?}");
    };
    let mut receipts = Vec::new();
    for mode in [TaskMode::Ask, TaskMode::Plan] {
        receipts.push(
            hub.submit(
                &owner,
                CommandEnvelope::new(Command::CreateTask {
                    github: None,
                    agent_chosen: None,
                    mode: Some(mode),
                    project_id,
                    kind: TaskKind::Coding,
                    title: Some("Read-only request".into()),
                    prompt: "List the files".into(),
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
                    conversation: None,
                    access: None,
                    review: None,
                }),
            )
            .await,
        );
    }
    let work = hub
        .submit(
            &owner,
            CommandEnvelope::new(Command::CreateTask {
                github: None,
                agent_chosen: None,
                mode: Some(TaskMode::Work),
                project_id,
                kind: TaskKind::Coding,
                title: None,
                prompt: "Edit files".into(),
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
                conversation: None,
                access: None,
                review: None,
            }),
        )
        .await;
    assert_eq!(work.error.unwrap().code, ErrorCode::OsPermissionMissing);
    hub.refresh_readiness().await.unwrap();
    let QueryData::Readiness { report } = hub.query(&owner, Query::Readiness).await.unwrap() else {
        panic!("readiness")
    };
    for label in ["Ask", "Plan"] {
        assert_eq!(
            report
                .checks
                .iter()
                .find(|c| c.label.ends_with(label))
                .unwrap()
                .status,
            CheckStatus::Passed
        );
    }
    assert_eq!(
        report
            .checks
            .iter()
            .find(|c| c.label.ends_with("Work"))
            .unwrap()
            .status,
        CheckStatus::NotRun
    );
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(folder).unwrap();
    std::fs::remove_dir_all(path).unwrap();
    for receipt in receipts {
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    }
}

#[derive(Debug)]
struct Sandboxes(protocol::capability::Availability);
#[async_trait::async_trait]
impl ports::sandbox::Sandboxes for Sandboxes {
    fn availability(&self) -> protocol::capability::Availability {
        self.0.clone()
    }
    fn create(
        &self,
        _: ports::sandbox::SandboxSpec,
    ) -> Result<Arc<dyn ports::sandbox::Sandbox>, ports::HarnessError> {
        panic!("readiness must not create a sandbox")
    }
}

#[tokio::test]
async fn the_check_says_whether_agents_can_be_held_to_what_they_may_reach() {
    use protocol::capability::Availability;
    let checked = async |availability| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/service-tests")
            .join(uuid::Uuid::now_v7().to_string());
        let hub = CompanionHub::builder()
            .data_dir(&path)
            .clock(Arc::new(FakeClock::default()))
            .sandboxes(Arc::new(Sandboxes(availability)))
            .build()
            .await
            .unwrap();
        hub.refresh_readiness().await.unwrap();
        let QueryData::Readiness { report } = hub
            .query(&Caller::local_owner(), Query::Readiness)
            .await
            .unwrap()
        else {
            panic!("readiness")
        };
        hub.shutdown().await.unwrap();
        drop(hub);
        std::fs::remove_dir_all(path).unwrap();
        report
            .checks
            .into_iter()
            .find(|c| c.label == "Agent limits")
    };
    let held = checked(Availability::Available).await.unwrap();
    assert_eq!(
        (held.area, held.status, held.summary.as_deref()),
        (
            ReadinessArea::Accounts,
            CheckStatus::Passed,
            Some("Agents can be held to what you let them reach")
        )
    );
    let missing = checked(Availability::Unsupported {
        reason: "Install bubblewrap 0.13 at /usr/bin/bwrap".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        (missing.status, missing.detail.as_deref()),
        (
            CheckStatus::NotRun,
            Some("Install bubblewrap 0.13 at /usr/bin/bwrap")
        )
    );
}
