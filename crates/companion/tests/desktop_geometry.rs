// SPDX-License-Identifier: Apache-2.0
#![cfg(target_os = "linux")]
use async_trait::async_trait;
use platform_linux::{
    LinuxDesktop,
    native::{Driver, NativeInput, Output},
};
use ports::{CapturedFrame, DesktopCapabilities, PlatformError};
use protocol::{
    Query, QueryData,
    capability::{Availability, InputScope},
};
use services::{
    CompanionHub,
    api::{Caller, Hub},
    testing::FakeClock,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Debug)]
struct RecordedOutput(Mutex<Output>);
#[async_trait]
impl Driver for RecordedOutput {
    async fn outputs(&self) -> Result<Vec<Output>, PlatformError> {
        Ok(vec![self.0.lock().unwrap().clone()])
    }
    async fn capture(&self, _: &Output) -> Result<CapturedFrame, PlatformError> {
        Err(PlatformError::Unsupported(
            "This fixture supplies output descriptions only".into(),
        ))
    }
    async fn input(&self) -> Result<Box<dyn NativeInput>, PlatformError> {
        Err(PlatformError::Unsupported(
            "This fixture supplies output descriptions only".into(),
        ))
    }
}

#[tokio::test(start_paused = true)]
async fn recorded_linux_output_sequences_reach_the_phone_without_an_input_command() {
    // Physical/logical output descriptions observed in the disposable compositor
    // guests. Replay the adapter boundary, then inspect the transport's Hub view.
    for (backend, initial, changed) in [
        ("hyprland", (1280, 720, 1280), (1600, 1200, 1280)),
        ("kwin", (1280, 800, 1280), (1920, 1080, 1536)),
        ("mutter", (1280, 720, 1280), (1280, 720, 1024)),
    ] {
        let output = |(width, height, logical_width)| Output {
            name: "recorded-output".into(),
            x: 0.0,
            y: 0.0,
            width,
            height,
            scale: width as f32 / logical_width as f32,
            rotation: 0,
        };
        let driver = Arc::new(RecordedOutput(Mutex::new(output(initial))));
        let clock = Arc::new(FakeClock::default());
        let desktop = Arc::new(
            LinuxDesktop::new(
                driver.clone(),
                DesktopCapabilities {
                    backend: backend.into(),
                    capture: Availability::Available,
                    input: Availability::Available,
                    input_scope: InputScope::OutputBound,
                    lock_session: Availability::Available,
                    unlock_session: Availability::Available,
                },
                None,
            )
            .with_clock(clock.clone()),
        );
        let directory = tempfile::tempdir().unwrap();
        let hub = CompanionHub::builder()
            .data_dir(directory.path())
            .clock(clock.clone())
            .build()
            .await
            .unwrap();
        hub.set_desktop(Some(desktop), Availability::Available)
            .await
            .unwrap();
        let QueryData::Snapshot(before) = hub
            .query(
                &Caller::local_owner(),
                Query::Snapshot {
                    reads_withheld: None,
                },
            )
            .await
            .unwrap()
        else {
            panic!("snapshot expected")
        };
        assert_eq!(before.targets[0].geometry.revision, 1);
        assert_eq!(before.control[0].target_id, before.targets[0].target_id);
        *driver.0.lock().unwrap() = output(changed);
        clock.advance(Duration::from_millis(300));
        tokio::time::advance(Duration::from_millis(300)).await;
        let mut published = None;
        for _ in 0..20 {
            tokio::task::yield_now().await;
            let QueryData::Snapshot(snapshot) = hub
                .query(
                    &Caller::local_owner(),
                    Query::Snapshot {
                        reads_withheld: None,
                    },
                )
                .await
                .unwrap()
            else {
                panic!("snapshot expected")
            };
            if snapshot.targets[0].geometry.revision == 2 {
                published = Some(snapshot);
                break;
            }
        }
        let after = published.expect("observer must publish the changed output");
        assert_eq!(after.targets[0].target_id, before.targets[0].target_id);
        assert_eq!(after.targets[0].geometry.width, changed.0);
        assert_eq!(after.targets[0].geometry.height, changed.1);
        assert_eq!(after.targets[0].geometry.scale, 1.25);
        let backlog = hub
            .subscribe(&Caller::local_owner(), Some(before.journal_head))
            .await
            .unwrap()
            .backlog;
        let services::api::Backlog::Events(events) = backlog else {
            panic!("events expected")
        };
        assert!(events.iter().any(|event| matches!(&event.event, protocol::Event::TargetsChanged { targets } if targets[0].geometry == after.targets[0].geometry)));
        hub.shutdown().await.unwrap();
    }
}
