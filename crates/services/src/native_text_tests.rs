// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::api::Outbound;
use ports::HarnessEvent;
use protocol::model::{ActivityStatus, ChildState};

/// The companion's limit for one control frame of JSON.
const FRAME: usize = 1024 * 1024;

// A tool's output and the agent's own words are outside the owner's
// control; a line of either larger than a frame must still reach the phone.
#[tokio::test]
async fn agent_text_of_any_size_is_recorded_in_events_that_fit_one_frame() {
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
    let mut live = f
        .hub
        .subscribe(&Caller::local_owner(), None)
        .await
        .unwrap()
        .live;
    // Quotes and control characters take two and six bytes once encoded.
    let quotes = "\"".repeat(2 * FRAME);
    let controls = "\u{1}".repeat(2 * FRAME);
    for event in [
        HarnessEvent::AgentTextDelta {
            message_key: "m".into(),
            text: quotes.clone(),
        },
        HarnessEvent::AgentMessage {
            message_key: "m".into(),
            text: quotes.clone(),
        },
        HarnessEvent::Activity {
            key: "read".into(),
            tool: "Read".into(),
            summary: "Read finished".into(),
            status: ActivityStatus::Succeeded,
            detail: Some(controls.clone()),
        },
        HarnessEvent::Child {
            lifetime: None,
            native_ref: "helper".into(),
            label: quotes.clone(),
            state: ChildState::Active,
        },
        HarnessEvent::Warning {
            text: controls.clone(),
        },
    ] {
        f.harness.emit(run, event).await;
    }
    let mut notes = 0;
    while notes < 3 {
        let frame = match tokio::time::timeout(WAIT, live.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Outbound::Event(event) => {
                if matches!(
                    &event.event,
                    Event::TimelineAppended { item, .. } if matches!(
                        item.item,
                        TimelineBody::AgentMessage { .. }
                            | TimelineBody::Activity { .. }
                            | TimelineBody::Note { .. }
                    )
                ) {
                    notes += 1;
                }
                ServerFrame::Event((*event).clone())
            }
            Outbound::Live(event) => ServerFrame::Live((*event).clone()),
        };
        let size = serde_json::to_vec(&frame).unwrap().len();
        assert!(size <= FRAME, "{size} bytes");
    }
    let QueryData::TaskTimeline { items, .. } = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::TaskTimeline {
                task_id: task,
                before_seq: None,
                limit: 10,
            },
        )
        .await
        .unwrap()
    else {
        panic!("timeline")
    };
    let cut = |text: &str| text.ends_with("(too long to show in full)");
    assert!(items.iter().any(|i| matches!(&i.item,
        TimelineBody::AgentMessage { text, .. } if cut(text))));
    assert!(items.iter().any(|i| matches!(&i.item,
        TimelineBody::Activity { detail: Some(detail), .. } if cut(detail))));
    assert!(items.iter().any(|i| matches!(&i.item,
        TimelineBody::Note { text } if cut(text))));
    let run: Run = f.hub.read(|r| r.require("run", run)).unwrap();
    assert!(cut(&run.children[0].label));
    f.close().await;
}

// Typical tool output is well under its own budget, yet a page of it can
// exceed a frame. Every page fits, and paging back still reaches every item.
#[tokio::test]
async fn timeline_pages_fit_one_frame_and_together_hold_every_item() {
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
    for n in 0..80 {
        f.harness
            .emit(
                run,
                HarnessEvent::Activity {
                    key: format!("read-{n}"),
                    tool: "Read".into(),
                    summary: "Read finished".into(),
                    status: ActivityStatus::Succeeded,
                    detail: Some("\n".repeat(16 * 1024)),
                },
            )
            .await;
    }
    let hub = &f.hub;
    let page = move |before_seq| async move {
        hub.query(
            &Caller::local_owner(),
            Query::TaskTimeline {
                task_id: task,
                before_seq,
                limit: 80,
            },
        )
        .await
        .unwrap()
    };
    let reads = |data: &QueryData| match data {
        QueryData::TaskTimeline { items, .. } => items
            .iter()
            .filter(|i| matches!(i.item, TimelineBody::Activity { .. }))
            .count(),
        _ => panic!("timeline"),
    };
    let mut more = waiting();
    while more() && reads(&page(None).await) < 1 {
        tokio::task::yield_now().await;
    }
    let (mut before, mut seen) = (None, 0);
    loop {
        let data = page(before).await;
        let size = serde_json::to_vec(&ServerFrame::QueryResult(QueryResult {
            query_id: QueryId::new(),
            result: Ok(data.clone()),
        }))
        .unwrap()
        .len();
        assert!(size <= FRAME, "{size} bytes");
        seen += reads(&data);
        let QueryData::TaskTimeline { items, has_more } = data else {
            unreachable!()
        };
        if !has_more {
            break;
        }
        before = Some(items[0].seq);
    }
    assert_eq!(seen, 80);
    f.close().await;
}

// A request the phone cannot show whole cannot be decided there: its record
// would not fit a frame, and a cut preview offers no exact request to check.
// The agent is told no, the transcript says why, and nothing oversized is
// recorded.
#[tokio::test]
async fn a_native_request_too_large_to_show_is_declined_and_not_recorded() {
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
    let run = run_started(&f, task).await;
    let mut live = f
        .hub
        .subscribe(&Caller::local_owner(), None)
        .await
        .unwrap()
        .live;
    let raw = serde_json::json!({ "command": "\"".repeat(FRAME / 2) }).to_string();
    f.harness
        .emit(
            run,
            HarnessEvent::ApprovalRequested(ports::NativeApproval {
                native_request_id: "large".into(),
                kind: ApprovalKind::Command,
                title: "Approve native command".into(),
                detail: Some("\"\"\"…".into()),
                detail_lines: Vec::new(),
                questions: Vec::new(),
                raw_request: Some(raw),
                scope: "this command once".into(),
                shortened: true,
                tool_call: None,
            }),
        )
        .await;
    f.harness
        .emit(
            run,
            HarnessEvent::Warning {
                text: "after".into(),
            },
        )
        .await;
    loop {
        let frame = match tokio::time::timeout(WAIT, live.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Outbound::Event(event) => ServerFrame::Event((*event).clone()),
            Outbound::Live(event) => ServerFrame::Live((*event).clone()),
        };
        let size = serde_json::to_vec(&frame).unwrap().len();
        assert!(size <= FRAME, "{size} bytes");
        if matches!(&frame, ServerFrame::Event(e) if matches!(&e.event,
            Event::TimelineAppended { item, .. } if matches!(&item.item, TimelineBody::Note { text } if text == "after")))
        {
            break;
        }
    }
    let approvals: Vec<Approval> = f.hub.read(|r| r.list("approval")).unwrap();
    assert!(approvals.iter().all(|a| a.native_request_id != "large"));
    assert_eq!(
        f.harness
            .controls
            .resolutions
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let QueryData::TaskTimeline { items, .. } = f
        .hub
        .query(
            &Caller::local_owner(),
            Query::TaskTimeline {
                task_id: task,
                before_seq: None,
                limit: 10,
            },
        )
        .await
        .unwrap()
    else {
        panic!("timeline")
    };
    assert!(items.iter().any(|i| matches!(&i.item,
        TimelineBody::Note { text } if text.contains("too large to show"))));
    f.close().await;
}

// The same holds for a permission request through the tool bridge: a file the
// agent wants to write whose content would not fit is refused at once, as a
// request over the bridge's own size limit already was.
#[tokio::test]
async fn a_bridge_request_too_large_to_show_is_refused_at_once() {
    use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse};
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
    let run = run_started(&f, task).await;
    let binding = BridgeBinding {
        run_id: run,
        target_id: None,
        computer_use: false,
        browser: false,
        github: false,
        checklist_answers: false,
    };
    // Under the bridge's 1 MiB limit as text, over a frame once escaped.
    let content = "\"".repeat(300 * 1024);
    let response = tokio::time::timeout(
        WAIT,
        f.hub.handle(
            &binding,
            BridgeRequest::PermissionPrompt {
                tool_name: "Write".into(),
                input: serde_json::json!({"file_path": "data.json", "content": content}),
                tool_use_id: None,
                native_request_id: Some("large-write".into()),
            },
        ),
    )
    .await
    .expect("a request that cannot be shown must not wait for a decision");
    assert!(
        matches!(&response, BridgeResponse::Refused { reason } if reason.contains("too large")),
        "{response:?}"
    );
    let approvals: Vec<Approval> = f.hub.read(|r| r.list("approval")).unwrap();
    assert!(
        approvals
            .iter()
            .all(|a| a.native_request_id != "large-write")
    );
    f.close().await;
}
