// SPDX-License-Identifier: Apache-2.0
//! An agent whose screen lapsed waits for the person to hand it back.
use super::*;
use crate::hand_back::HAND_BACK;
use ports::bridge::{BridgeBinding, BridgeHandler, BridgeRequest, BridgeResponse};
use protocol::notifications::AlertTopic;

struct Watched {
    f: Fixture,
    phone: Caller,
    task: TaskId,
    run: RunId,
    target: TargetId,
    binding: BridgeBinding,
}

/// A computer-use run that has looked at its screen, whose lease then
/// lapsed: the person stopped watching.
async fn lapsed() -> Watched {
    let f = Fixture::new().await;
    let phone = f.device().await;
    let project = f.project().await;
    let (task, _) = f.task(project, TaskPlacement::StartNow, &phone, true).await;
    let run = run_started(&f, task).await;
    let target = f.desktop.target.lock().unwrap().target_id;
    let binding = BridgeBinding {
        run_id: run,
        target_id: Some(target),
        computer_use: true,
        browser: false,
        github: false,
        checklist_answers: false,
    };
    assert!(matches!(
        f.hub
            .handle(&binding, BridgeRequest::ObserveTarget { max_dim: None })
            .await,
        BridgeResponse::Observation { .. }
    ));
    f.clock.advance(Duration::from_secs(4));
    Watched {
        f,
        phone,
        task,
        run,
        target,
        binding,
    }
}

impl Watched {
    fn types(&self) -> tokio::task::JoinHandle<BridgeResponse> {
        let (hub, binding) = (self.f.hub.clone(), self.binding.clone());
        tokio::spawn(async move {
            hub.handle(
                &binding,
                BridgeRequest::TypeText {
                    text: "synthetic".into(),
                },
            )
            .await
        })
    }
    fn paused(&self) -> Option<AttentionItem> {
        (self.f.hub)
            .read(|r| r.task_attention::<AttentionItem>(self.task))
            .unwrap()
            .into_iter()
            .find(|i| i.run_id == Some(self.run) && i.title == "Agent paused")
    }
    /// What asks the person to hand the screen back, once it is raised.
    async fn asked(&self) -> AttentionItem {
        let mut wait = waiting();
        loop {
            if let Some(item) = self.paused() {
                return item;
            }
            assert!(wait(), "the person was not asked");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    fn injected(&self) -> u64 {
        self.f.desktop.input.injected.load(Ordering::SeqCst)
    }
}

fn refusal(response: BridgeResponse) -> String {
    match response {
        BridgeResponse::Refused { reason } => reason,
        other => panic!("refused expected: {other:?}"),
    }
}

#[tokio::test]
async fn an_agent_whose_screen_lapsed_waits_for_the_person_to_hand_it_back() {
    let w = lapsed().await;
    let typing = w.types();
    let asked = w.asked().await;
    assert_eq!(asked.kind, AttentionKind::WaitingForYou);
    assert_eq!(asked.topic, Some(AlertTopic::NeedsYou));
    assert!(asked.resolved_at.is_none());
    assert!(
        asked.body.as_deref().unwrap().contains("tap Resume agent"),
        "{asked:?}"
    );
    // Another action of the run waits under the same question.
    let again = w.types();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!typing.is_finished() && !again.is_finished());
    let open = (w.f.hub.read(|r| r.task_attention::<AttentionItem>(w.task)))
        .unwrap()
        .into_iter()
        .filter(|i| i.title == "Agent paused" && i.resolved_at.is_none())
        .count();
    assert_eq!(open, 1);
    assert_eq!(w.injected(), 0);

    let back = CommandEnvelope::new(Command::ReturnToAgent {
        target_id: w.target,
        run_id: w.run,
    });
    let id = back.command_id;
    w.f.hub.submit(&w.phone, back).await;
    settle(&w.f.hub, id).await;
    // Not sent, since the screen may have changed meanwhile, but shown it
    // as it is now to act again from, rather than refused.
    for waited in [typing, again] {
        match waited.await.unwrap() {
            BridgeResponse::Resumed {
                text, image_base64, ..
            } => {
                assert!(text.starts_with("Not sent, and nothing is wrong"), "{text}");
                assert!(!image_base64.is_empty());
            }
            other => panic!("resumed expected: {other:?}"),
        }
    }
    assert!(w.paused().unwrap().resolved_at.is_some());
    assert_eq!(w.injected(), 0);
    assert!(matches!(w.types().await.unwrap(), BridgeResponse::Done));
    assert_eq!(w.injected(), 1);
    w.f.close().await;
}

#[tokio::test]
async fn an_agent_nobody_hands_the_screen_back_to_is_told_to_stop_trying() {
    let w = lapsed().await;
    let typing = w.types();
    w.asked().await;
    w.f.clock.advance(HAND_BACK);
    let told = refusal(typing.await.unwrap());
    assert!(told.contains("Nobody handed the screen back"), "{told}");
    assert!(w.paused().unwrap().resolved_at.is_some());
    assert_eq!(w.injected(), 0);
    w.f.close().await;
}

#[tokio::test]
async fn stopping_an_agent_that_waits_for_its_screen_ends_the_wait() {
    let w = lapsed().await;
    let typing = w.types();
    w.asked().await;
    let stop = CommandEnvelope::new(Command::StopTask {
        task_id: w.task,
        force: false,
    });
    let id = stop.command_id;
    w.f.hub.submit(&w.phone, stop).await;
    settle(&w.f.hub, id).await;
    let told = refusal(typing.await.unwrap());
    assert!(told.contains("no longer has control"), "{told}");
    assert!(w.paused().unwrap().resolved_at.is_some());
    assert_eq!(w.injected(), 0);
    // Nobody waits for the screen any more: it reads as free, not paused.
    let view = (w.f.hub)
        .read(|r| r.get::<TargetControlView>("control", w.target))
        .unwrap()
        .unwrap();
    assert_eq!(view.input_state, InputState::Ready);
    assert!(view.lease.is_none());
    w.f.close().await;
}
