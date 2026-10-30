// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::Clock;
use ports::system::{SystemMonitor, SystemReading};

#[derive(Debug)]
struct Idle;
impl SystemMonitor for Idle {
    fn read(&self) -> SystemReading {
        SystemReading {
            uptime_s: Some(42),
            ..SystemReading::default()
        }
    }
}

#[tokio::test]
async fn a_computer_that_cant_read_itself_says_so() {
    let f = Fixture::new().await;
    let features = f.hub.host_capabilities().await.features;
    assert!(!features.contains(&"system_stats_v1".to_string()));
    let refused = f
        .hub
        .query(&Caller::local_owner(), Query::SystemStats)
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unsupported);
}

#[tokio::test]
async fn whoever_may_watch_the_computer_may_see_how_busy_it_is() {
    let f = Fixture::with(|b| b.system_monitor(Arc::new(Idle))).await;
    let features = f.hub.host_capabilities().await.features;
    assert!(features.contains(&"system_stats_v1".to_string()));
    let watcher = artifact_access::device_with(
        &f,
        2,
        Grants {
            view: true,
            ..Grants::default()
        },
    )
    .await;
    let QueryData::SystemStats { stats } = f.hub.query(&watcher, Query::SystemStats).await.unwrap()
    else {
        panic!("system stats")
    };
    assert_eq!(stats.uptime_s, Some(42));
    assert_eq!(stats.at, f.clock.now());
    assert_eq!(stats.history.len(), 1);
    let blind = artifact_access::device_with(
        &f,
        3,
        Grants {
            tasks: true,
            ..Grants::default()
        },
    )
    .await;
    let refused = f.hub.query(&blind, Query::SystemStats).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
}
