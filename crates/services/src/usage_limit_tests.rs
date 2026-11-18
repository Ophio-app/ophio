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

#[tokio::test]
async fn usage_notices_are_once_per_wording_per_conversation_per_utc_day_and_survive_restart() {
    let f = Fixture::new().await;
    let first = TaskId::new();
    let second = TaskId::new();
    let near = "Claude Code reports you are close to the weekly usage limit";
    let reached = "Claude Code reports the weekly usage limit is reached";
    let other_limit = "Claude Code reports you are close to the five-hour usage limit";
    let codex = "Codex reports the usage limit is reached";
    let record = |f: &Fixture, task, text, now| {
        f.hub
            .write(|tx| crate::usage_limits::notice_once(tx, task, text, Timestamp(now)))
            .unwrap()
    };
    assert!(record(&f, first, near, 86_400_000));
    assert!(!record(&f, first, near, 86_400_001));
    assert!(record(&f, second, near, 86_400_001));
    assert!(record(&f, first, reached, 86_400_002));
    assert!(record(&f, first, other_limit, 86_400_003));
    assert!(record(&f, first, codex, 86_400_004));
    // Alternating wording must not make a previously shown warning repeat.
    assert!(!record(&f, first, near, 86_400_005));
    let f = f.reopened(|b| b).await;
    assert!(!record(&f, first, near, 172_799_999));
    assert!(!record(&f, first, codex, 172_799_999));
    assert!(record(&f, first, near, 172_800_000));
    assert!(record(&f, first, codex, 172_800_001));
    f.close().await;
}

#[tokio::test]
async fn repeated_usage_notices_stay_out_of_replies_and_other_warnings_still_repeat() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let (task, _) = f
        .task(
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
    let near = "Claude Code reports you are close to the weekly usage limit";
    for _ in 0..2 {
        f.harness
            .emit(run, HarnessEvent::UsageNotice { text: near.into() })
            .await;
        f.harness
            .emit(
                run,
                HarnessEvent::Warning {
                    text: "An ordinary warning".into(),
                },
            )
            .await;
    }
    f.harness
        .emit(
            run,
            HarnessEvent::AgentMessage {
                message_key: "reply".into(),
                text: "The reply alone".into(),
            },
        )
        .await;
    let mut more = waiting();
    let items = loop {
        let QueryData::TaskTimeline { items, .. } = f
            .hub
            .query(
                &Caller::local_owner(),
                Query::TaskTimeline {
                    task_id: task,
                    before_seq: None,
                    limit: 100,
                },
            )
            .await
            .unwrap()
        else {
            panic!("timeline expected")
        };
        if items.iter().any(|i| matches!(&i.item, TimelineBody::AgentMessage { text, .. } if text == "The reply alone")) {
            break items;
        }
        assert!(more(), "reply not stored");
        tokio::task::yield_now().await;
    };
    let notes: Vec<_> = items
        .iter()
        .filter_map(|i| match &i.item {
            TimelineBody::Note { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(notes.iter().filter(|n| **n == near).count(), 1);
    assert_eq!(
        notes
            .iter()
            .filter(|n| **n == "An ordinary warning")
            .count(),
        2
    );
    assert!(items.iter().all(
        |i| !matches!(&i.item, TimelineBody::AgentMessage { text, .. } if text.contains(near))
    ));
    f.close().await;
}
