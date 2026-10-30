// SPDX-License-Identifier: Apache-2.0
mod support;
use companion::server::Limits;
use futures_util::StreamExt;
use protocol::{
    ErrorCode, ServerFrame,
    alerts::{Alert, AlertClientFrame, AlertServerFrame, MAX_OPEN_ALERTS},
    auth::ConnectionKind,
    capability::HarnessKind,
    event::Event,
    ids::*,
    model::{AttentionItem, AttentionKind},
    time::Timestamp,
};
use std::time::Duration;
use support::*;
use tokio_tungstenite::tungstenite::Message;

fn alert(kind: AttentionKind, at: i64) -> Alert {
    Alert {
        attention_id: AttentionId::new(),
        kind,
        project_id: ProjectId::new(),
        project: "Site".into(),
        task_id: TaskId::new(),
        task: "Fix the header".into(),
        harness: HarnessKind::ClaudeCode,
        created_at: Timestamp(at),
        headline: None,
        question: None,
        topic: None,
        quiet: false,
        bundle: false,
        report: None,
    }
}
fn item(alert: &Alert, resolved: bool) -> AttentionItem {
    AttentionItem {
        child_id: None,
        attention_id: alert.attention_id,
        kind: alert.kind,
        title: "Done".into(),
        body: None,
        task_id: Some(alert.task_id),
        run_id: None,
        approval_id: None,
        created_at: alert.created_at,
        resolved_at: resolved.then_some(Timestamp(alert.created_at.0 + 1)),
        priority: 1,
        topic: None,
        quiet: false,
        report_id: None,
    }
}
async fn watching(fixture: &Fixture) -> support::Client {
    watching_quietly(fixture, true).await
}
async fn watching_quietly(fixture: &Fixture, quiet: bool) -> support::Client {
    let mut client = fixture.authenticated(ConnectionKind::Alerts).await;
    client
        .send(&AlertClientFrame::Watch {
            showing: Vec::new(),
            quiet,
            reports: false,
        })
        .await;
    client
}

#[tokio::test]
async fn open_list_then_raised_and_resolved_once_each() {
    let fixture = Fixture::new().await;
    let waiting = alert(AttentionKind::Question, 10);
    let answered = alert(AttentionKind::Approval, 11);
    fixture
        .hub
        .alerts
        .lock()
        .unwrap()
        .extend([(waiting.clone(), true), (answered.clone(), false)]);
    fixture.hub.emit(1);
    let mut client = watching(&fixture).await;
    let AlertServerFrame::Open { alerts, .. } = client.receive().await else {
        panic!("open list first")
    };
    assert_eq!(alerts, vec![waiting.clone()]);

    // Events at or before the list's head are already in it.
    fixture.hub.emit(1);
    let finished = alert(AttentionKind::Completed, 20);
    fixture
        .hub
        .alerts
        .lock()
        .unwrap()
        .push((finished.clone(), true));
    fixture.hub.publish(Event::AttentionUpserted {
        item: item(&finished, false),
    });
    assert_eq!(
        client.receive::<AlertServerFrame>().await,
        AlertServerFrame::Raised {
            alert: finished.clone()
        }
    );
    // An update to an alert already sent is not a second alert.
    fixture.hub.publish(Event::AttentionUpserted {
        item: item(&finished, false),
    });
    // One the device may not see, or already closed, raises nothing.
    let hidden = alert(AttentionKind::Question, 21);
    fixture.hub.publish(Event::AttentionUpserted {
        item: item(&hidden, false),
    });
    fixture.hub.publish(Event::AttentionResolved {
        attention_id: waiting.attention_id,
    });
    assert_eq!(
        client.receive::<AlertServerFrame>().await,
        AlertServerFrame::Resolved {
            attention_id: waiting.attention_id
        }
    );
    // Resolved twice, or never sent: nothing.
    fixture.hub.publish(Event::AttentionResolved {
        attention_id: waiting.attention_id,
    });
    fixture.hub.publish(Event::AttentionResolved {
        attention_id: hidden.attention_id,
    });
    fixture.hub.publish(Event::AttentionUpserted {
        item: item(&finished, true),
    });
    assert_eq!(
        client.receive::<AlertServerFrame>().await,
        AlertServerFrame::Resolved {
            attention_id: finished.attention_id
        }
    );
    fixture.stop().await;
}

#[tokio::test]
async fn a_full_list_still_clears_the_alerts_it_left_out_and_lists_again_when_one_leaves() {
    let fixture = Fixture::new().await;
    let alerts: Vec<Alert> = (0..MAX_OPEN_ALERTS as i64 + 2)
        .map(|i| alert(AttentionKind::Completed, 10 + i))
        .collect();
    (fixture.hub.alerts.lock().unwrap()).extend(alerts.iter().map(|a| (a.clone(), true)));
    let mut client = watching(&fixture).await;
    let AlertServerFrame::Open { alerts: listed, .. } = client.receive().await else {
        panic!("open list first")
    };
    assert_eq!(listed, alerts[2..]);
    let answer = |a: &Alert| {
        let mut known = fixture.hub.alerts.lock().unwrap();
        known.iter_mut().find(|(k, _)| k == a).unwrap().1 = false;
        drop(known);
        fixture.hub.publish(Event::AttentionResolved {
            attention_id: a.attention_id,
        });
    };
    // The oldest, left out, is answered: the phone may show it from before.
    answer(&alerts[0]);
    assert_eq!(
        client.receive::<AlertServerFrame>().await,
        AlertServerFrame::Resolved {
            attention_id: alerts[0].attention_id
        }
    );
    // Two listed are answered together: the list comes again once, now with
    // the one left out.
    for a in &alerts[100..102] {
        answer(a);
        assert_eq!(
            client.receive::<AlertServerFrame>().await,
            AlertServerFrame::Resolved {
                attention_id: a.attention_id
            }
        );
    }
    let AlertServerFrame::Open { alerts: listed, .. } = client.receive().await else {
        panic!("the list again")
    };
    let open: Vec<_> = (alerts[1..100].iter().chain(&alerts[102..]).cloned()).collect();
    assert_eq!(listed, open);
    // Not full now: one never listed, answered, sends nothing.
    let hidden = alert(AttentionKind::Question, 1);
    fixture.hub.publish(Event::AttentionResolved {
        attention_id: hidden.attention_id,
    });
    answer(&alerts[1]);
    assert_eq!(
        client.receive::<AlertServerFrame>().await,
        AlertServerFrame::Resolved {
            attention_id: alerts[1].attention_id
        }
    );
    fixture.stop().await;
}

#[tokio::test]
async fn an_answer_journaled_as_the_list_comes_again_still_clears_the_alert() {
    let fixture = Fixture::new().await;
    let alerts: Vec<Alert> = (0..MAX_OPEN_ALERTS as i64 + 2)
        .map(|i| alert(AttentionKind::Completed, 10 + i))
        .collect();
    (fixture.hub.alerts.lock().unwrap()).extend(alerts.iter().map(|a| (a.clone(), true)));
    let mut client = watching(&fixture).await;
    let AlertServerFrame::Open { .. } = client.receive().await else {
        panic!("open list first")
    };
    // The oldest, left out of the list, is answered just before the list is
    // read again: its answer is still on its way when the list is sent.
    fixture.hub.alerts.lock().unwrap()[0].1 = false;
    fixture
        .hub
        .before_open
        .lock()
        .unwrap()
        .push(Event::AttentionResolved {
            attention_id: alerts[0].attention_id,
        });
    fixture.hub.alerts.lock().unwrap()[100].1 = false;
    fixture.hub.publish(Event::AttentionResolved {
        attention_id: alerts[100].attention_id,
    });
    let _: AlertServerFrame = client.receive().await;
    let AlertServerFrame::Open { alerts: listed, .. } = client.receive().await else {
        panic!("the list again")
    };
    assert_eq!(listed.len(), MAX_OPEN_ALERTS);
    assert!(!listed.contains(&alerts[0]));
    assert_eq!(
        client.receive::<AlertServerFrame>().await,
        AlertServerFrame::Resolved {
            attention_id: alerts[0].attention_id
        }
    );
    fixture.stop().await;
}

#[tokio::test]
async fn a_reconnect_starts_from_the_open_list() {
    let fixture = Fixture::new().await;
    let first = alert(AttentionKind::Completed, 10);
    fixture
        .hub
        .alerts
        .lock()
        .unwrap()
        .push((first.clone(), true));
    let mut client = watching(&fixture).await;
    let _: AlertServerFrame = client.receive().await;
    drop(client);
    // Raised and answered while the phone was away; one more still open.
    let gone = alert(AttentionKind::Question, 11);
    let missed = alert(AttentionKind::Failure, 12);
    fixture
        .hub
        .alerts
        .lock()
        .unwrap()
        .extend([(gone, false), (missed.clone(), true)]);
    let mut client = watching(&fixture).await;
    let AlertServerFrame::Open { alerts, .. } = client.receive().await else {
        panic!("open list first")
    };
    assert_eq!(alerts, vec![first, missed]);
    fixture.stop().await;
}

#[tokio::test]
async fn alerts_turned_off_reach_only_a_phone_that_holds_them_quietly() {
    let fixture = Fixture::new().await;
    let loud = alert(AttentionKind::Question, 10);
    let quiet = Alert {
        quiet: true,
        ..alert(AttentionKind::Completed, 11)
    };
    (fixture.hub.alerts.lock().unwrap()).extend([(loud.clone(), true), (quiet.clone(), true)]);
    let mut new = watching(&fixture).await;
    let AlertServerFrame::Open { alerts, .. } = new.receive().await else {
        panic!("open list first")
    };
    assert_eq!(alerts, vec![loud.clone(), quiet.clone()]);
    let mut old = watching_quietly(&fixture, false).await;
    let AlertServerFrame::Open { alerts, .. } = old.receive().await else {
        panic!("open list first")
    };
    assert_eq!(alerts, vec![loud.clone()]);

    // Raised live, the same.
    let later = Alert {
        quiet: true,
        ..alert(AttentionKind::Completed, 12)
    };
    let next = alert(AttentionKind::Failure, 13);
    (fixture.hub.alerts.lock().unwrap()).extend([(later.clone(), true), (next.clone(), true)]);
    fixture.hub.publish(Event::AttentionUpserted {
        item: item(&later, false),
    });
    fixture.hub.publish(Event::AttentionUpserted {
        item: item(&next, false),
    });
    let raised = |alert: &Alert| AlertServerFrame::Raised {
        alert: alert.clone(),
    };
    assert_eq!(new.receive::<AlertServerFrame>().await, raised(&later));
    assert_eq!(new.receive::<AlertServerFrame>().await, raised(&next));
    assert_eq!(old.receive::<AlertServerFrame>().await, raised(&next));

    // The person turns them on again: both phones are sent the list anew.
    for (a, _) in fixture.hub.alerts.lock().unwrap().iter_mut() {
        a.quiet = false;
    }
    fixture.hub.publish(Event::NotificationSettingsChanged {});
    for client in [&mut new, &mut old] {
        let AlertServerFrame::Open { alerts, gone, .. } = client.receive().await else {
            panic!("a fresh list")
        };
        assert_eq!(alerts.len(), 4);
        assert!(alerts.iter().all(|a| !a.quiet) && gone.is_empty());
    }
    fixture.stop().await;
}

#[tokio::test]
async fn a_reconnect_behind_a_full_list_names_what_the_phone_shows_that_closed_meanwhile() {
    let fixture = Fixture::new().await;
    let shown = alert(AttentionKind::Approval, 1);
    let left_out = alert(AttentionKind::Question, 2);
    let alerts: Vec<Alert> = (0..MAX_OPEN_ALERTS as i64)
        .map(|i| alert(AttentionKind::Completed, 10 + i))
        .collect();
    {
        let mut known = fixture.hub.alerts.lock().unwrap();
        // Answered while the phone was away; the other still open, both
        // older than the full list.
        known.extend([(shown.clone(), false), (left_out.clone(), true)]);
        known.extend(alerts.iter().map(|a| (a.clone(), true)));
    }
    let never = AttentionId::new();
    let mut client = fixture.authenticated(ConnectionKind::Alerts).await;
    client
        .send(&AlertClientFrame::Watch {
            quiet: true,
            showing: vec![
                shown.attention_id,
                left_out.attention_id,
                alerts[5].attention_id,
                never,
                shown.attention_id,
            ],
            reports: false,
        })
        .await;
    let AlertServerFrame::Open {
        alerts: listed,
        gone,
        ..
    } = client.receive().await
    else {
        panic!("open list first")
    };
    assert_eq!(listed, alerts);
    // In the list itself, so the phone settles both before raising any it
    // kept; the one left out is still open.
    assert_eq!(gone, vec![shown.attention_id, never]);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), client.socket.next())
            .await
            .is_err()
    );
    fixture.stop().await;
}

#[tokio::test]
async fn a_watcher_too_far_behind_is_let_go_to_name_what_it_shows_again() {
    let fixture = Fixture::new().await;
    let mut client = watching(&fixture).await;
    let _: AlertServerFrame = client.receive().await;
    // More than the connection's queue holds, before it reads any.
    for _ in 0..40 {
        fixture.hub.publish(Event::AttentionResolved {
            attention_id: AttentionId::new(),
        });
    }
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match client.socket.next().await {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(Message::Text(text))) => {
                    panic!("nothing more is sent: {text}")
                }
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(ended.is_ok());
    fixture.stop().await;
}

#[tokio::test]
async fn watch_names_a_bounded_list() {
    let fixture = Fixture::new().await;
    let mut client = fixture.authenticated(ConnectionKind::Alerts).await;
    client
        .send(&AlertClientFrame::Watch {
            quiet: true,
            showing: (0..=protocol::alerts::MAX_SHOWING)
                .map(|_| AttentionId::new())
                .collect(),
            reports: false,
        })
        .await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(e) if e.code == ErrorCode::InvalidRequest)
    );
    fixture.stop().await;
}

#[tokio::test]
async fn watch_must_come_first() {
    let fixture = Fixture::new().await;
    let mut client = fixture.authenticated(ConnectionKind::Alerts).await;
    client
        .send(&serde_json::json!({"type": "ping", "nonce": 1}))
        .await;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(e) if e.code == ErrorCode::InvalidRequest)
    );
    fixture.stop().await;
}

#[tokio::test]
async fn changed_grants_end_the_connection() {
    let fixture = Fixture::new().await;
    let mut client = watching(&fixture).await;
    let _: AlertServerFrame = client.receive().await;
    fixture
        .hub
        .credentials
        .lock()
        .unwrap()
        .get_mut(&fixture.hub.device)
        .unwrap()
        .grants
        .tasks = false;
    assert!(
        matches!(client.receive::<ServerFrame>().await, ServerFrame::Fatal(e) if e.code == ErrorCode::PermissionDenied)
    );
    fixture.stop().await;
}

#[tokio::test]
async fn the_computer_pings_and_drops_a_phone_that_stops_answering() {
    let fixture = Fixture::with_limits(Limits {
        alert_keepalive: Duration::from_millis(150),
        ..Limits::default()
    })
    .await;
    let mut client = watching(&fixture).await;
    let _: AlertServerFrame = client.receive().await;
    // Reading answers each ping, and the connection stays.
    for _ in 0..4 {
        assert!(matches!(client.message().await, Message::Ping(_)));
    }
    // A phone that stops answering is let go after the next ping.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut ended = false;
    for _ in 0..8 {
        let next = tokio::time::timeout(Duration::from_secs(5), client.socket.next()).await;
        match next.unwrap().map(|m| m.ok()) {
            Some(Some(message)) => match message {
                Message::Ping(_) => continue,
                Message::Text(text) => {
                    let frame: ServerFrame = serde_json::from_str(&text).unwrap();
                    assert!(matches!(frame, ServerFrame::Fatal(e) if e.code == ErrorCode::Expired));
                }
                _ => {
                    ended = true;
                    break;
                }
            },
            _ => {
                ended = true;
                break;
            }
        }
    }
    assert!(ended);
    fixture.stop().await;
}

#[tokio::test]
async fn report_alerts_require_the_watch_capability_in_lists_and_live_events() {
    use protocol::reports::{ReportAlert, ReportProgress, report_reference};
    let fixture = Fixture::new().await;
    let mut report = alert(AttentionKind::Completed, 10);
    let id = ReportId::new();
    report.project_id = "00000000-0000-0000-0000-000000000000".parse().unwrap();
    report.task_id = "00000000-0000-0000-0000-000000000000".parse().unwrap();
    report.project.clear();
    report.task.clear();
    report.report = Some(Box::new(ReportAlert {
        report_id: id,
        reference: report_reference(&id),
        status: ReportProgress::Reproduced,
        question: None,
    }));
    fixture
        .hub
        .alerts
        .lock()
        .unwrap()
        .push((report.clone(), true));
    let mut old = watching(&fixture).await;
    let AlertServerFrame::Open { alerts, .. } = old.receive().await else {
        panic!("open")
    };
    assert!(alerts.is_empty());
    let mut supported = fixture.authenticated(ConnectionKind::Alerts).await;
    supported
        .send(&AlertClientFrame::Watch {
            showing: Vec::new(),
            quiet: true,
            reports: true,
        })
        .await;
    let AlertServerFrame::Open { alerts, .. } = supported.receive().await else {
        panic!("open")
    };
    assert_eq!(alerts, vec![report.clone()]);
    let mut next = report.clone();
    next.attention_id = AttentionId::new();
    fixture
        .hub
        .alerts
        .lock()
        .unwrap()
        .push((next.clone(), true));
    fixture.hub.publish(Event::AttentionUpserted {
        item: item(&next, false),
    });
    assert_eq!(
        supported.receive::<AlertServerFrame>().await,
        AlertServerFrame::Raised { alert: next }
    );
    // A normal task alert follows on both connections. On the older phone
    // it must be the very next frame, with no report ahead of it.
    let normal = alert(AttentionKind::Completed, 20);
    fixture
        .hub
        .alerts
        .lock()
        .unwrap()
        .push((normal.clone(), true));
    fixture.hub.publish(Event::AttentionUpserted {
        item: item(&normal, false),
    });
    assert_eq!(
        old.receive::<AlertServerFrame>().await,
        AlertServerFrame::Raised {
            alert: normal.clone()
        }
    );
    assert_eq!(
        supported.receive::<AlertServerFrame>().await,
        AlertServerFrame::Raised { alert: normal }
    );
    fixture.stop().await;
}
