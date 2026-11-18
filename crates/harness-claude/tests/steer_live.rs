// SPDX-License-Identifier: Apache-2.0
//! "Steer now" against the installed Claude Code, through Ophio's own
//! adapter. Ignored: it uses the person's account. One run: a turn that
//! runs a short command, a steer sent while the command runs, and a
//! follow-up held for the turn's end. The steer must be read and answered in
//! that turn; the follow-up must go at once, with no wait for a further
//! turn; and the run must end promptly after the last reply.
//!
//! `OPHIO_LIVE_BINARY` is the Claude Code to run, `OPHIO_LIVE_PROJECT` a
//! throwaway folder to run it in, `OPHIO_LIVE_EVIDENCE` where the timed
//! event log goes, and `OPHIO_LIVE_MODEL` the model (cheapest by default).
use harness_claude::ClaudeAdapter;
use ports::{HarnessAdapter, HarnessEvent, MessageRef, PromptInput, RunEnv, RunExit, RunSpec};
use protocol::{
    command::MessageDelivery,
    model::{DeliveryState, LaunchPolicy, TaskMode},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn var(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set")))
}

fn input(text: &str) -> PromptInput {
    PromptInput {
        text: text.into(),
        images: vec![],
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed Claude Code with the person's account"]
async fn a_real_steer_is_read_in_its_turn_and_a_held_followup_goes_at_once() {
    let model = std::env::var("OPHIO_LIVE_MODEL").unwrap_or_else(|_| "claude-haiku-4-5".into());
    let spec = RunSpec {
        run_id: protocol::RunId::new(),
        task_id: protocol::TaskId::new(),
        mode: TaskMode::Work,
        cwd: var("OPHIO_LIVE_PROJECT"),
        prompt: input(
            "Run the shell command `sleep 8` exactly once, then reply with the single word DONE.",
        ),
        model: Some(model),
        effort: None,
        launch_policy: Some(LaunchPolicy::BypassPermissions),
        resume_session: None,
        resume_from_mode: None,
        mission: None,
        branch: None,
        start: None,
        bridge: None,
        project_settings: false,
        env: RunEnv::default(),
        github: None,
        scratch: None,
        attached: None,
        questions: true,
        sandbox: None,
        web: false,
        disabled: Default::default(),
    };
    let adapter = ClaudeAdapter::with_binary(var("OPHIO_LIVE_BINARY"));
    let start = Instant::now();
    let mut started = adapter.start_run(spec).await.expect("Claude Code started");
    let at = |start: Instant| start.elapsed().as_secs_f64();
    let mut log = String::new();
    let mut names: Vec<(MessageRef, &str)> = Vec::new();
    let mut deliveries: Vec<(&str, DeliveryState, f64)> = Vec::new();
    let mut replies: Vec<String> = vec![String::new()];
    let mut completed_at: Vec<f64> = Vec::new();
    let mut exit = None;
    let mut sent = false;
    let all = async {
        while let Some(event) = started.events.recv().await {
            let now = at(start);
            match event {
                HarnessEvent::Activity { tool, status, .. } => {
                    log.push_str(&format!("{now:7.2}s activity {tool} {status:?}\n"));
                    if !sent {
                        sent = true;
                        // The command is running: this steer arrives mid-turn.
                        let steer = MessageRef(protocol::CommandId::new());
                        names.push((steer, "steer"));
                        let state = started
                            .control
                            .send_message(
                                steer,
                                input("Also end your reply with the word STEERED."),
                                MessageDelivery::Steer,
                            )
                            .await
                            .expect("steer accepted");
                        log.push_str(&format!("{:7.2}s sent steer: {state:?}\n", at(start)));
                        let held = MessageRef(protocol::CommandId::new());
                        names.push((held, "follow-up"));
                        let state = started
                            .control
                            .send_message(
                                held,
                                input("Now reply with the single word TWO."),
                                MessageDelivery::NextTurn,
                            )
                            .await
                            .expect("follow-up accepted");
                        log.push_str(&format!("{:7.2}s sent follow-up: {state:?}\n", at(start)));
                    }
                }
                HarnessEvent::AgentMessage { text, .. } => {
                    log.push_str(&format!("{now:7.2}s message {text:?}\n"));
                    replies.last_mut().unwrap().push_str(&text);
                }
                HarnessEvent::MessageDelivery { message_ref, state } => {
                    let name = names
                        .iter()
                        .find(|(id, _)| *id == message_ref)
                        .map_or("unnamed", |(_, name)| *name);
                    log.push_str(&format!("{now:7.2}s delivery {name} {state:?}\n"));
                    deliveries.push((name, state, now));
                }
                HarnessEvent::TurnCompleted { .. } => {
                    log.push_str(&format!("{now:7.2}s turn completed\n"));
                    completed_at.push(now);
                    replies.push(String::new());
                }
                HarnessEvent::Exited { exit: ended, .. } => {
                    log.push_str(&format!("{now:7.2}s exited {ended:?}\n"));
                    exit = Some(ended);
                    break;
                }
                HarnessEvent::Warning { text } => {
                    log.push_str(&format!("{now:7.2}s warning {text}\n"))
                }
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(240), all)
        .await
        .expect("the run ended in time");
    std::fs::write(
        var("OPHIO_LIVE_EVIDENCE").join("steer-live.events.txt"),
        &log,
    )
    .unwrap();
    println!("{log}");
    assert_eq!(exit, Some(RunExit::Completed), "{log}");
    assert_eq!(completed_at.len(), 2, "{log}");
    let state = |name: &str, wanted: DeliveryState| {
        deliveries
            .iter()
            .find(|(n, s, _)| *n == name && *s == wanted)
            .map(|(_, _, t)| *t)
    };
    // The steer was read inside the first turn and answered there.
    let read = state("steer", DeliveryState::NativeDelivered).expect("steer read");
    assert!(read < completed_at[0], "{log}");
    assert!(replies[0].contains("STEERED"), "{log}");
    // The held follow-up was written at the turn's end, with no wait.
    let written = state("follow-up", DeliveryState::HostAccepted).expect("follow-up written");
    assert!(written - completed_at[0] < 1.0, "{log}");
    assert!(
        state("follow-up", DeliveryState::NativeDelivered).is_some(),
        "{log}"
    );
    assert!(
        !deliveries
            .iter()
            .any(|(_, s, _)| *s == DeliveryState::Unknown),
        "{log}"
    );
    // The run ends promptly after the last reply.
    let last = *completed_at.last().unwrap();
    let ended = at(start);
    assert!(ended - last < 5.0, "{log}");
}
