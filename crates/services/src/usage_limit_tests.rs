// SPDX-License-Identifier: Apache-2.0
use super::*;
use ports::{Clock, HarnessEvent};
use protocol::capability::{HarnessInfo, UsageLimit};

async fn limit(f: &Fixture) -> Option<UsageLimit> {
    match f
        .hub
        .query(&Caller::local_owner(), Query::Harnesses)
        .await
        .unwrap()
    {
        QueryData::Harnesses { harnesses } => {
            harnesses
                .into_iter()
                .find(|h: &HarnessInfo| h.kind == HarnessKind::ClaudeCode)
                .unwrap()
                .usage_limit
        }
        _ => panic!("harnesses expected"),
    }
}

async fn settle(f: &Fixture, expected: Option<UsageLimit>) {
    let mut more = waiting();
    while more() && limit(f).await != expected {
        tokio::task::yield_now().await;
    }
    assert_eq!(limit(f).await, expected);
}

#[tokio::test]
async fn a_usage_limit_lasts_until_a_reply_or_its_reset_time() {
    let f = Fixture::new().await;
    let project = f.project().await;
    f.task(
        project,
        TaskPlacement::StartNow,
        &Caller::local_owner(),
        false,
    )
    .await;
    let mut more = waiting();
    while more() && f.harness.runs().is_empty() {
        tokio::task::yield_now().await;
    }
    let run = f.harness.runs()[0];
    assert_eq!(limit(&f).await, None);

    let reached_at = f.clock.now();
    let resets_at = Some(Timestamp(reached_at.0 + 3_600_000));
    f.harness
        .emit(run, HarnessEvent::UsageLimited { resets_at })
        .await;
    let recorded = Some(UsageLimit {
        reached_at,
        resets_at,
    });
    settle(&f, recorded.clone()).await;
    // Starting stays allowed: the limit may lift before anyone says so.
    assert!(
        f.hub
            .read(|r| {
                let harnesses: Vec<HarnessInfo> = r.get("meta", "harnesses")?.unwrap_or_default();
                Ok(harnesses[0].capabilities.start_run.is_available())
            })
            .unwrap()
    );
    // A fresh probe knows nothing of usage; the limit is carried until it resets.
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(limit(&f).await, recorded);
    f.clock.advance(Duration::from_secs(2 * 60 * 60));
    f.hub.refresh_catalog().await.unwrap();
    assert_eq!(limit(&f).await, None);

    // A reply proves usage returned, whatever the reset time said.
    f.harness
        .emit(run, HarnessEvent::UsageLimited { resets_at: None })
        .await;
    settle(
        &f,
        Some(UsageLimit {
            reached_at: f.clock.now(),
            resets_at: None,
        }),
    )
    .await;
    f.harness
        .emit(
            run,
            HarnessEvent::AgentMessage {
                message_key: "m".into(),
                text: "Done".into(),
            },
        )
        .await;
    settle(&f, None).await;
    f.close().await;
}
