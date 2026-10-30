// SPDX-License-Identifier: Apache-2.0
use crate::{
    Caller, CompanionHub, Hub,
    testing::{FakeClock, FakeDesktop, FakeMedia},
};
use ports::Clock;
use protocol::{Command, Query, QueryData, command::CommandResult};
use std::sync::Arc;

#[tokio::test]
async fn artifact_uses_frame_geometry_instead_of_cached_target() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/capture-tests")
        .join(uuid::Uuid::now_v7().to_string());
    let desktop = Arc::new(FakeDesktop::default());
    let clock = Arc::new(FakeClock::default());
    let hub = CompanionHub::builder()
        .data_dir(&path)
        .clock(clock.clone())
        .desktop(desktop.clone())
        .media(Arc::new(FakeMedia))
        .build()
        .await
        .unwrap();
    let old = desktop.target.lock().unwrap().clone();
    let mut geometry = old.geometry;
    geometry.revision += 1;
    geometry.width += 20;
    geometry.scale = 1.5;
    geometry.rotation_degrees = 90;
    desktop.target.lock().unwrap().geometry = geometry;
    let result = hub
        .control_command(
            &Caller::local_owner(),
            &Command::CaptureTarget {
                target_id: old.target_id,
                evidence: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    let CommandResult::ArtifactCaptured { artifact } = result else {
        panic!("capture")
    };
    let capture = artifact.capture.as_ref().unwrap();
    assert_eq!(capture.geometry, geometry);
    assert_ne!(capture.geometry.revision, old.geometry.revision);
    assert_eq!(capture.target_id, old.target_id);
    assert_eq!(capture.captured_at, clock.now());
    let QueryData::Artifact { artifact: stored } = hub
        .query(
            &Caller::local_owner(),
            Query::Artifact {
                artifact_id: artifact.artifact_id,
            },
        )
        .await
        .unwrap()
    else {
        panic!("artifact")
    };
    assert_eq!(stored, artifact);
    hub.shutdown().await.unwrap();
    drop(hub);
    std::fs::remove_dir_all(path).unwrap();
}
