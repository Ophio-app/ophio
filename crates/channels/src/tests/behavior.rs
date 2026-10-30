// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::Message;

#[tokio::test]
async fn unlinked_users_only_receive_link_instructions_on_both_platforms() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Minimal).await;
        f.handle(f.input("a", "/new forbidden")).await;
        assert!(f.hub.submitted.lock().unwrap().is_empty());
        assert_eq!(f.hub.links.lock().unwrap()[0].external_user, "7");
        assert!(f.server.mock.texts().contains("Received"));
        assert!(
            f.server
                .mock
                .texts()
                .contains("Approve this code in the app: Connections › Chat")
        );
        assert!(!f.server.mock.texts().contains("forbidden"));
    }
}
#[tokio::test]
async fn a_link_request_carries_the_senders_name_or_names_the_platform() {
    for (kind, fallback) in [
        (ChannelKind::Telegram, "Telegram chat"),
        (ChannelKind::Discord, "Discord chat"),
    ] {
        let f = Fixture::new(kind, Exposure::Minimal).await;
        f.handle(Incoming {
            name: Some("Sam Rivers".into()),
            ..f.input("a", "hello")
        })
        .await;
        f.handle(f.input("b", "hello again")).await;
        let links = f.hub.links.lock().unwrap();
        assert_eq!(links[0].display_name, "Sam Rivers");
        assert_eq!(links[1].display_name, fallback);
    }
}
#[tokio::test]
async fn link_codes_say_when_they_expire_and_how_to_get_a_new_one() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Minimal).await;
        f.handle(f.input("a", "hello")).await;
        assert!(f.server.mock.texts().contains(&format!(
            "Code: TEST42\nApprove this code in the app: Connections › Chat\n\
             It expires in {} minutes. For a new code, send any message here again.",
            services::api::LINK_CODE_MINUTES
        )));
        // Asking again too soon says when to ask, not to check an app this chat cannot use.
        f.hub
            .link_errors
            .lock()
            .unwrap()
            .push_back(ErrorCode::LimitExceeded);
        f.handle(f.input("b", "hello again")).await;
        let texts = f.server.mock.texts();
        assert!(texts.contains("Wait a minute, then send any message here again for a new code."));
        assert!(!texts.contains("Check the app"));
    }
}
#[tokio::test]
async fn the_person_who_asked_for_a_code_is_told_whether_it_was_approved_or_refused() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = &Fixture::new(kind, Exposure::Minimal).await;
        let stop = CancellationToken::new();
        let (core, io, shutdown) = (f.core.clone(), f.io.clone(), stop.clone());
        let worker = tokio::spawn(async move {
            core.notifications(
                io,
                "123".into(),
                shutdown,
                &crate::recovery::Status::new(&|_| {}),
            )
            .await
        });
        let decide = |user: &str, event: &str, outcome| {
            let mut input = f.input(event, "hello");
            input.route.user = user.into();
            input.thread = Some("5".into());
            async move {
                let told = || f.server.mock.texts().matches("Code TEST42 was").count();
                let before = told();
                f.handle(input).await;
                // Nothing is said while the code waits for the owner.
                tokio::time::sleep(Duration::from_millis(1200)).await;
                assert_eq!(told(), before);
                f.hub
                    .link_outcomes
                    .lock()
                    .unwrap()
                    .insert("TEST42".into(), outcome);
            }
        };
        let approved =
            "Code TEST42 was approved in the app. This chat is now linked to the computer.";
        decide("7", "a", ChannelLinkOutcome::Approved).await;
        until(|| f.server.mock.texts().contains(approved)).await;
        let refused = "Code TEST42 was refused in the app, so this chat isn’t linked.";
        decide("8", "b", ChannelLinkOutcome::Refused).await;
        until(|| f.server.mock.texts().contains(refused)).await;
        // Each person is told once, in the topic they asked from.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let requests = f.server.mock.requests();
        for text in [approved, refused] {
            let sent: Vec<_> = requests
                .iter()
                .filter(|(_, v, _)| v["text"] == text || v["content"] == text)
                .collect();
            assert_eq!(sent.len(), 1, "{text}");
            if kind == ChannelKind::Telegram {
                assert_eq!(sent[0].1["message_thread_id"], "5");
            }
        }

        // A code that runs out is forgotten without a message.
        f.handle(f.input("c", "hello")).await;
        f.hub.link_outcomes.lock().unwrap().remove("TEST42");
        f.clock
            .advance(Duration::from_secs(services::api::LINK_CODE_MINUTES * 60));
        let store = f.core.store.clone();
        let mut forgotten = false;
        for _ in 0..50 {
            forgotten = store
                .get::<Value>("link_codes", "TEST42")
                .await
                .unwrap()
                .is_none();
            if forgotten {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(forgotten);
        stop.cancel();
        worker.await.unwrap().unwrap();
        assert_eq!(f.server.mock.texts().matches("Code TEST42 was").count(), 2);
    }
}
#[tokio::test]
async fn strangers_cannot_fill_the_route_table_and_linked_routes_are_never_evicted() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    // The owner's route is remembered first, so it is the oldest.
    f.hub.link(f.kind, "7", "42");
    f.handle(f.input("owner", "/tasks")).await;
    // Strangers who asked for codes fill the rest of the table.
    let now = f.clock.now().0;
    let strangers = (0..1023)
        .map(|i| {
            let route = Route {
                user: format!("s{i}"),
                ..f.route("7")
            };
            let value = json!({"bot": route.bot, "user": route.user, "chat": route.chat, "at": now + 1 + i});
            ("telegram_routes", route.key(f.kind), value)
        })
        .collect();
    f.core.store.put_batch(strangers).await.unwrap();
    let users = || async {
        let routes = f.core.store.list::<Route>("telegram_routes").await.unwrap();
        routes.into_iter().map(|r| r.user).collect::<Vec<_>>()
    };
    let ask = |user: &str| {
        let mut input = f.input(&format!("ask:{user}"), "hello");
        input.route.user = user.into();
        f.handle(input)
    };

    // A new person still gets a code; the oldest stranger makes room.
    ask("8").await;
    assert_eq!(
        f.hub.links.lock().unwrap().last().unwrap().external_user,
        "8"
    );
    let remembered = users().await;
    assert_eq!(remembered.len(), 1024);
    for (user, kept) in [("7", true), ("8", true), ("s0", false), ("s1", true)] {
        assert_eq!(remembered.contains(&user.to_owned()), kept, "{user}");
    }

    // With every route linked, nothing is evicted to make room.
    for user in remembered.iter().filter(|u| *u != "7") {
        f.hub.link(f.kind, user, "42");
    }
    ask("9").await;
    let remembered = users().await;
    assert_eq!(remembered.len(), 1024);
    assert!(!remembered.contains(&"9".to_owned()));
    assert!(
        f.server
            .mock
            .texts()
            .contains("Request refused (LimitExceeded)")
    );
}
#[tokio::test]
async fn a_route_that_never_completed_a_link_is_forgotten_when_its_code_runs_out() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    let ask = |user: &str, event: &str| {
        let mut input = f.input(event, "hello");
        input.route.user = user.into();
        f.handle(input)
    };
    let users = || async {
        let routes = f.core.store.list::<Route>("telegram_routes").await.unwrap();
        let mut users: Vec<_> = routes.into_iter().map(|r| r.user).collect();
        users.sort();
        users
    };
    let half = Duration::from_secs(services::api::LINK_CODE_MINUTES * 30);
    // The owner is linked; three others ask for codes.
    f.hub.link(f.kind, "7", "42");
    f.handle(f.input("owner", "/tasks")).await;
    ask("8", "a").await;
    ask("9", "b").await;
    ask("10", "c").await;
    // One code is approved, and one person asks for a new code.
    f.clock.advance(half);
    f.hub.link(f.kind, "8", "42");
    ask("10", "d").await;
    // Once the first codes run out, a new person's request clears the route
    // whose code was never approved. Linked routes stay however old.
    f.clock.advance(half);
    ask("11", "e").await;
    assert_eq!(users().await, ["10", "11", "7", "8"]);
    f.clock.advance(half);
    ask("12", "f").await;
    assert_eq!(users().await, ["11", "12", "7", "8"]);
}
#[tokio::test]
async fn an_unmapped_chat_is_told_where_to_choose_its_project() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Minimal).await;
        f.hub.link(kind, "7", "43");
        let mut input = f.input("a", "/new Build the requested feature");
        input.route.chat = "43".into();
        f.handle(input).await;
        assert!(f.hub.submitted.lock().unwrap().is_empty());
        assert!(
            f.server
                .mock
                .texts()
                .contains("Choose a project for this chat in the app: Connections › Chat")
        );
    }
}
#[tokio::test]
async fn project_defaults_grants_and_replay_survive_restart() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Minimal).await;
        let identity = f.create().await;
        let (caller, envelope) = f.hub.submitted.lock().unwrap()[0].clone();
        assert_eq!(
            caller.principal,
            Principal::Channel {
                identity_id: identity.id
            }
        );
        assert_eq!(
            caller.grants,
            Grants {
                tasks: true,
                approvals: true,
                ..Grants::default()
            }
        );
        assert_eq!(envelope.command_id, command_id(kind, "123", "new:1"));
        assert!(
            matches!(envelope.body,Command::CreateTask {project_id,harness:HarnessKind::Codex,model:Some(ref m),effort:Some(ref e),placement:TaskPlacement::StartNow,..} if project_id==f.hub.project() && m=="chosen-model" && e=="chosen-effort")
        );
        f.hub.snapshot.lock().unwrap().projects[0].default_model = Some("changed".into());
        f.handle(f.input("new:1", "/new Build the requested feature"))
            .await;
        assert_eq!(f.hub.snapshot.lock().unwrap().tasks.len(), 1);
        let again = f.hub.submitted.lock().unwrap()[1].1.clone();
        assert_eq!(envelope, again);
        assert!(f.server.mock.texts().contains("Accepted by the computer"));
        assert!(!f.server.mock.texts().contains("Done"));
        let Fixture {
            hub,
            clock,
            dir,
            core,
            server,
            io,
            kind,
        } = f;
        let config = core.cfg.clone();
        drop(core);
        let store = Store::open(dir.path().join("state")).await.unwrap();
        let core = Core::new(hub.clone(), config, store, clock, kind);
        core.handle(
            io.as_ref(),
            &mut Incoming {
                route: Route {
                    bot: "123".into(),
                    user: "7".into(),
                    chat: "42".into(),
                },
                name: None,
                event: "new:1".into(),
                text: "/new Build the requested feature".into(),
                reply: None,
                thread: None,
                action: None,
                explicit_answer: false,
                forwarded: false,
                acknowledgement: None,
                rate_checked: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(hub.snapshot.lock().unwrap().tasks.len(), 1);
        assert_eq!(hub.submitted.lock().unwrap()[2].1, envelope);
        drop(server);
    }
}
#[tokio::test]
async fn stop_requires_bound_confirmation_and_original_revision() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Minimal).await;
        f.create().await;
        let task = f.hub.task();
        f.handle(f.input("stop", "/stop")).await;
        assert_eq!(f.hub.submitted.lock().unwrap().len(), 1);
        f.handle(f.input("stop2", &format!("/stop {}", task.task_id)))
            .await;
        assert_eq!(f.hub.submitted.lock().unwrap().len(), 1);
        let data = f.server.mock.last_buttons()[0].clone();
        f.hub.snapshot.lock().unwrap().tasks[0].revision += 1;
        let mut input = f.input("confirm", "");
        input.action = Some(data);
        f.handle(input).await;
        assert_eq!(f.hub.task().state, TaskState::Working);
        assert!(f.server.mock.texts().contains("StaleRevision"));
        assert!(!f.server.mock.texts().contains("private error body"));
        f.handle(f.input("stop3", &format!("/stop {}", task.task_id)))
            .await;
        let mut input = f.input("confirm2", "");
        input.action = Some(f.server.mock.last_buttons()[0].clone());
        f.handle(input).await;
        assert_eq!(f.hub.task().state, TaskState::Interrupted);
    }
}
#[tokio::test]
async fn approval_buttons_bind_user_chat_revision_and_grants() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Titles).await;
        let identity = f.create().await;
        let a = f.hub.approval(ApprovalKind::Command);
        f.notice(&identity, a.clone()).await;
        let buttons = f.server.mock.last_buttons();
        assert_eq!(buttons.len(), 2);
        assert!(f.server.mock.texts().contains("cargo test"));
        f.hub.link(kind, "8", "42");
        let mut attack = f.input("attack", "");
        attack.route.user = "8".into();
        attack.action = Some(buttons[0].clone());
        f.handle(attack).await;
        assert_eq!(f.hub.submitted.lock().unwrap().len(), 1);
        assert!(f.server.mock.texts().contains("PermissionDenied"));
        let mut answer = f.input("allow", "");
        answer.action = Some(buttons[0].clone());
        f.handle(answer).await;
        let submitted = f.hub.submitted.lock().unwrap().clone();
        assert_eq!(submitted.len(), 2);
        assert!(
            matches!(submitted[1].1.body,Command::ResolveApproval {approval_id,decision:ApprovalDecision::Allow} if approval_id==a.approval_id)
        );
        assert_eq!(submitted[1].1.expected_revision, Some(1));
        drop(submitted);
        let mut replay = f.input("allow-again", "");
        replay.action = Some(buttons[0].clone());
        f.handle(replay).await;
        assert_eq!(f.hub.submitted.lock().unwrap().len(), 2);
    }
}
#[tokio::test]
async fn a_command_addressed_to_another_bot_is_not_run() {
    let f = Fixture::new(ChannelKind::Discord, Exposure::Minimal).await;
    f.hub.link(f.kind, "7", "42");
    f.handle(f.input("other", "/new@OtherBot Delete the branch"))
        .await;
    assert!(f.hub.submitted.lock().unwrap().is_empty());
    assert!(
        f.server
            .mock
            .texts()
            .contains("Use /new, /tasks, /status, /stop or /steer.")
    );
}
#[tokio::test]
async fn a_forwarded_message_is_never_a_command_or_an_answer() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Titles).await;
    let identity = f.create().await;
    let task = f.hub.task().task_id;
    let acknowledgement = f.message_id();
    let forwarded = |event: &str, text: &str, reply: &str| Incoming {
        forwarded: true,
        reply: Some(reply.into()),
        ..f.input(event, text)
    };
    // Forwarded commands in reply to a task are ordinary text for that task.
    for (event, text) in [("new", "/new Delete the branch"), ("stop", "/stop")] {
        f.handle(forwarded(event, text, &acknowledgement)).await;
    }
    let messages: Vec<_> = f
        .hub
        .submitted
        .lock()
        .unwrap()
        .iter()
        .skip(1)
        .map(|(_, c)| match &c.body {
            Command::SendMessage {
                task_id,
                text,
                delivery: MessageDelivery::NextTurn,
                ..
            } if *task_id == task => text.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(messages, ["/new Delete the branch", "/stop"]);
    assert_eq!(f.hub.snapshot.lock().unwrap().tasks.len(), 1);
    // A forwarded reply to a question is not its answer.
    f.notice(&identity, f.hub.approval(ApprovalKind::Question))
        .await;
    let question = (f.server.mock.inner.next.load(Ordering::SeqCst) - 1).to_string();
    f.handle(forwarded("answer", "Yes, delete it", &question))
        .await;
    assert_eq!(f.hub.submitted.lock().unwrap().len(), 3);
    assert!(
        f.server
            .mock
            .texts()
            .contains("Request refused (InvalidRequest)")
    );
}
#[tokio::test]
async fn resolution_elsewhere_edits_buttons_and_records_actor() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Titles).await;
        let identity = f.create().await;
        let mut a = f.hub.approval(ApprovalKind::Tool);
        f.notice(&identity, a.clone()).await;
        a.state = ApprovalState::Denied;
        a.revision += 1;
        a.resolved_by = Some(Principal::Device {
            device_id: DeviceId::new(),
        });
        f.hub.snapshot.lock().unwrap().approvals[0] = a.clone();
        f.notice(&identity, a).await;
        assert!(
            f.server
                .mock
                .texts()
                .contains("approval denied. Recorded by device")
        );
        let last = f.server.mock.requests().pop().unwrap().1;
        if kind == ChannelKind::Telegram {
            assert!(
                last["reply_markup"]["inline_keyboard"][0]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        } else {
            assert!(last["components"].as_array().unwrap().is_empty());
        }
    }
}
#[tokio::test]
async fn question_reply_unsupported_kind_and_reference_expiry_are_honest() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Titles).await;
    let identity = f.create().await;
    let a = f.hub.approval(ApprovalKind::Question);
    f.notice(&identity, a.clone()).await;
    assert!(f.server.mock.last_buttons().is_empty());
    let id = (f.server.mock.inner.next.load(Ordering::SeqCst) - 1).to_string();
    let mut answer = f.input("question-answer", "A precise answer");
    answer.reply = Some(id);
    f.handle(answer).await;
    assert!(
        matches!(&f.hub.submitted.lock().unwrap()[1].1.body,Command::ResolveApproval {decision:ApprovalDecision::Answer {text,..},..} if text=="A precise answer")
    );
    let a = f.hub.approval(ApprovalKind::ComputerUse);
    f.notice(&identity, a).await;
    assert!(f.server.mock.last_buttons().is_empty());
    assert!(f.server.mock.texts().contains("requires the app"));
    // Logins and posts on GitHub are allowed on the phone, with its unlock.
    for kind in [
        ApprovalKind::LoginUse,
        ApprovalKind::LoginCode,
        ApprovalKind::GithubPost,
    ] {
        let a = f.hub.approval(kind);
        f.notice(&identity, a).await;
        assert!(f.server.mock.last_buttons().is_empty());
    }
    let a = f.hub.approval(ApprovalKind::Command);
    f.notice(&identity, a).await;
    let action = f.server.mock.last_buttons()[0].clone();
    f.clock.advance(Duration::from_secs(601));
    let mut input = f.input("expired", "");
    input.action = Some(action);
    f.handle(input).await;
    assert_eq!(f.hub.submitted.lock().unwrap().len(), 2);
    assert!(f.server.mock.texts().contains("Expired"));
}
#[tokio::test]
async fn splitting_html_and_discord_disable_active_content() {
    let server = Server::new().await;
    let input = format!(
        "{}<&\" @everyone **markdown** https://example.invalid",
        "😀".repeat(4100)
    );
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        server
            .io(kind)
            .send("42", Message::text(&input))
            .await
            .unwrap();
    }
    for (_, v, _) in server.mock.requests() {
        if let Some(text) = v["text"].as_str() {
            assert!(text.encode_utf16().count() <= 4200);
            assert_eq!(v["parse_mode"], "HTML");
            assert!(!text.contains("<&"));
        } else if let Some(text) = v["content"].as_str() {
            assert!(text.encode_utf16().count() <= 2000);
            assert_eq!(v["allowed_mentions"]["parse"], json!([]));
            assert_eq!(v["flags"], 4);
        }
    }
    let pieces = split_message(&input, 4096);
    assert_eq!(pieces.concat(), input);
    assert!(pieces.iter().all(|s| s.encode_utf16().count() <= 4096));
    assert_eq!(escape_html("<&>\""), "&lt;&amp;&gt;&quot;");
}
#[tokio::test]
async fn exposure_mapping_and_revocation_are_checked_per_message() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Minimal).await;
        let identity = f.create().await;
        f.handle(f.input("status", "/status")).await;
        let a = f.hub.approval(ApprovalKind::Command);
        f.notice(&identity, a).await;
        assert!(f.server.mock.last_buttons().is_empty());
        assert!(!f.server.mock.texts().contains("Sensitive task title"));
        assert!(!f.server.mock.texts().contains("cargo test"));
        let mut task = f.hub.task();
        task.state = TaskState::Completed;
        let event = f.hub.event(Event::TaskUpserted { task: task.clone() });
        f.core
            .notify(f.io.as_ref(), &f.route("7"), &identity, &event)
            .await
            .unwrap();
        assert!(f.server.mock.texts().contains("finished"));
        assert!(!f.server.mock.texts().contains("PRIVATE"));
        let mut other = task.clone();
        other.project_id = ProjectId::new();
        let event = f.hub.event(Event::TaskUpserted { task: other });
        let before = f.server.mock.requests().len();
        f.core
            .notify(f.io.as_ref(), &f.route("7"), &identity, &event)
            .await
            .unwrap();
        assert_eq!(f.server.mock.requests().len(), before);
        f.hub.identities.lock().unwrap()[0].revoked_at = Some(f.clock.now());
        f.handle(f.input("revoked", "/new Do not start")).await;
        assert_eq!(f.hub.submitted.lock().unwrap().len(), 1);
        let event = f.hub.event(Event::TaskUpserted { task });
        let before = f.server.mock.requests().len();
        f.core
            .notify(f.io.as_ref(), &f.route("7"), &identity, &event)
            .await
            .unwrap();
        assert_eq!(f.server.mock.requests().len(), before);
    }
}
#[tokio::test]
async fn next_turn_steer_titles_and_uncertain_receipts() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Titles).await;
    f.create().await;
    let reply = f.message_id();
    let mut input = f.input("followup", "Continue carefully");
    input.reply = Some(reply.clone());
    f.handle(input).await;
    let mut input = f.input("steer", "/steer Change the direction");
    input.reply = Some(reply);
    f.handle(input).await;
    let submitted = f.hub.submitted.lock().unwrap().clone();
    assert!(matches!(
        submitted[1].1.body,
        Command::SendMessage {
            delivery: MessageDelivery::NextTurn,
            ..
        }
    ));
    assert!(matches!(
        submitted[2].1.body,
        Command::SendMessage {
            delivery: MessageDelivery::Steer,
            ..
        }
    ));
    drop(submitted);
    f.handle(f.input("tasks", "/tasks")).await;
    assert!(f.server.mock.texts().contains("Sensitive task title"));
    *f.hub.stage.lock().unwrap() = Some(CommandStage::ReconciliationRequired);
    f.handle(f.input("uncertain", "/new Test an unknown outcome"))
        .await;
    assert!(
        f.server
            .mock
            .texts()
            .contains("Outcome unknown — check the app")
    );
}
#[tokio::test]
async fn limits_use_injected_monotonic_clock_and_state_has_one_owner() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    assert!(matches!(
        Store::open(f.dir.path().join("state")).await,
        Err(Error::StateLocked)
    ));
    for n in 0..25 {
        f.handle(f.input(&format!("{n}"), "/new cannot run")).await;
    }
    assert_eq!(f.hub.links.lock().unwrap().len(), 20);
    f.clock.advance(Duration::from_secs(61));
    f.handle(f.input("after", "/new cannot run")).await;
    assert_eq!(f.hub.links.lock().unwrap().len(), 21);
}
#[test]
fn deterministic_ids_namespace_and_configuration() {
    assert_eq!(
        command_id(ChannelKind::Telegram, "1", "2"),
        command_id(ChannelKind::Telegram, "1", "2")
    );
    assert_ne!(
        command_id(ChannelKind::Telegram, "12", "3"),
        command_id(ChannelKind::Telegram, "1", "23")
    );
    assert_ne!(
        command_id(ChannelKind::Telegram, "1", "2"),
        command_id(ChannelKind::Discord, "1", "2")
    );
    let cfg: Config = serde_json::from_str("{}").unwrap();
    assert_eq!(cfg.telegram.exposure, Exposure::Minimal);
    assert!(!cfg.discord.enabled);
    assert!(config::validate_endpoint("http://example.invalid", false).is_err());
    assert!(config::validate_endpoint("http://127.0.0.1:80", false).is_ok());
    assert_eq!(
        format!("{:?}", BotToken::new("test.fake".into()).unwrap()),
        "BotToken([redacted])"
    );
}

#[tokio::test]
async fn telegram_topic_followups_are_bound_per_sender_and_ambiguity_fails_closed() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    f.hub.link(ChannelKind::Telegram, "7", "42");
    let mut input = f.input("topic-new", "/new First task");
    input.thread = Some("77".into());
    f.handle(input).await;
    assert!(
        f.server
            .mock
            .requests()
            .iter()
            .any(|(_, v, _)| v["message_thread_id"] == "77")
    );
    let mut input = f.input("topic-followup", "Continue the task");
    input.thread = Some("77".into());
    f.handle(input).await;
    assert!(matches!(
        f.hub.submitted.lock().unwrap()[1].1.body,
        Command::SendMessage {
            delivery: MessageDelivery::NextTurn,
            ..
        }
    ));
    f.hub.link(ChannelKind::Telegram, "8", "42");
    let mut input = f.input("other-sender", "Cannot inherit a sender's task binding");
    input.route.user = "8".into();
    input.thread = Some("77".into());
    f.handle(input).await;
    assert_eq!(f.hub.submitted.lock().unwrap().len(), 2);
    let mut input = f.input("second-task", "/new Second task");
    input.thread = Some("77".into());
    f.handle(input).await;
    let mut input = f.input("ambiguous", "Which task?");
    input.thread = Some("77".into());
    f.handle(input).await;
    assert_eq!(f.hub.submitted.lock().unwrap().len(), 3);
    assert!(f.server.mock.texts().contains("InvalidRequest"));
}
#[tokio::test]
async fn long_escaped_approval_effects_never_get_partial_allow_buttons() {
    let f = Fixture::new(ChannelKind::Discord, Exposure::Titles).await;
    let identity = f.create().await;
    let mut a = f.hub.approval(ApprovalKind::Command);
    a.raw_request = Some("*".repeat(1000));
    f.hub.snapshot.lock().unwrap().approvals[0] = a.clone();
    f.notice(&identity, a).await;
    assert!(f.server.mock.last_buttons().is_empty());
    assert!(f.server.mock.texts().contains("requires the app"));
    let server = Server::new().await;
    server
        .io(ChannelKind::Discord)
        .send("42", Message::text(format!("{}*end", "x".repeat(1999))))
        .await
        .unwrap();
    let texts: Vec<_> = server
        .mock
        .requests()
        .into_iter()
        .map(|(_, v, _)| v["content"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(texts[0].len(), 1999);
    assert_eq!(texts[1], "\\*end");
}
#[tokio::test]
async fn completion_summaries_are_opt_in_and_live_conversations_are_never_mirrored() {
    for (kind, exposure) in [
        (ChannelKind::Telegram, Exposure::Minimal),
        (ChannelKind::Telegram, Exposure::Titles),
        (ChannelKind::Discord, Exposure::Minimal),
        (ChannelKind::Discord, Exposure::Titles),
    ] {
        let f = Fixture::new(kind, exposure).await;
        let identity = f.create().await;
        let mut task = f.hub.task();
        let run_id = RunId::new();
        task.current_run = Some(run_id);
        task.state = TaskState::Completed;
        let run = Run {
            human_resolution: None,
            helpers_checked: None,
            force_stoppable: false,
            cleanup_pending: false,
            run_id,
            task_id: task.task_id,
            harness: task.harness,
            requested_model: None,
            actual_model: None,
            requested_effort: None,
            launch_policy: None,
            actual_effort: None,
            account_label: None,
            native_session: None,
            state: RunState::Completed,
            admitted_at: f.clock.now(),
            dispatched_at: None,
            acknowledged_at: None,
            ended_at: Some(f.clock.now()),
            children: vec![],
            earlier_children: None,
            outcome: Some(RunOutcome {
                summary: Some("Reviewed outcome summary".into()),
                changed_files: vec![],
                checks: vec![],
                still_active: vec![],
                unknowns: vec![],
                usage: None,
                report: None,
                verification: None,
            }),
            holds_writer: false,
            mode: None,
            revisions: None,
            follow_up: None,
        };
        {
            let mut s = f.hub.snapshot.lock().unwrap();
            s.tasks[0] = task.clone();
            s.runs.push(run.clone());
        }
        f.ignored_notification(&identity, Event::RunUpserted { run: Box::new(run) })
            .await;
        let event = f.hub.event(Event::TaskUpserted { task: task.clone() });
        f.core
            .notify(f.io.as_ref(), &f.route("7"), &identity, &event)
            .await
            .unwrap();
        assert_eq!(
            f.server.mock.texts().contains("Reviewed outcome summary"),
            exposure == Exposure::Titles
        );
        let count = f.server.mock.requests().len();
        let event = f.hub.event(Event::TimelineAppended {
            task_id: task.task_id,
            item: TimelineItem {
                seq: 9,
                at: f.clock.now(),
                run_id: Some(run_id),
                item: TimelineBody::AgentMessage {
                    text: "PRIVATE CONVERSATION".into(),
                    partial: false,
                },
            },
        });
        f.core
            .notify(f.io.as_ref(), &f.route("7"), &identity, &event)
            .await
            .unwrap();
        assert_eq!(f.server.mock.requests().len(), count);
    }
}

#[tokio::test]
async fn discord_explicit_answers_cannot_be_reinterpreted_as_new_commands() {
    let f = Fixture::new(ChannelKind::Discord, Exposure::Titles).await;
    let identity = f.create().await;
    let a = f.hub.approval(ApprovalKind::Question);
    f.notice(&identity, a).await;
    let message = (f.server.mock.inner.next.load(Ordering::SeqCst) - 1).to_string();
    let input=discord::decode(&json!({"_interaction":true,"id":"456","type":2,"channel_id":"42","user":{"id":"7"},"data":{"name":"answer","options":[{"name":"message","value":message},{"name":"text","value":"/new is the command I mean"}]}}),"123").unwrap();
    f.handle(input).await;
    assert_eq!(f.hub.snapshot.lock().unwrap().tasks.len(), 1);
    assert!(
        matches!(&f.hub.submitted.lock().unwrap()[1].1.body,Command::ResolveApproval {decision:ApprovalDecision::Answer {text,..},..} if text=="/new is the command I mean")
    );
}

#[tokio::test]
async fn completed_result_does_not_make_telegram_say_needs_attention() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Titles).await;
    let identity = f.create().await;
    {
        let mut snapshot = f.hub.snapshot.lock().unwrap();
        snapshot.tasks[0].state = TaskState::Completed;
        let task = snapshot.tasks[0].clone();
        assert_eq!(task.attention_count, 0);
        snapshot.attention.push(AttentionItem {
            child_id: None,
            attention_id: AttentionId::new(),
            kind: AttentionKind::Completed,
            title: task.title,
            body: None,
            task_id: Some(task.task_id),
            run_id: task.current_run,
            approval_id: None,
            created_at: f.clock.now(),
            resolved_at: None,
            priority: 5,
            topic: None,
            quiet: false,
            report_id: None,
        });
    }
    let snapshot = f.hub.snapshot.lock().unwrap().clone();
    for event in [
        Event::TaskUpserted {
            task: snapshot.tasks[0].clone(),
        },
        Event::AttentionUpserted {
            item: snapshot.attention[0].clone(),
        },
    ] {
        f.core
            .notify(f.io.as_ref(), &f.route("7"), &identity, &f.hub.event(event))
            .await
            .unwrap();
    }
    f.handle(f.input("completed-status", "/tasks")).await;
    let text = f.server.mock.texts();
    assert!(text.contains("finished"), "{text}");
    assert!(!text.to_lowercase().contains("needs attention"), "{text}");
    // A real blocker still receives the suffix, even alongside a result.
    f.hub.snapshot.lock().unwrap().tasks[0].attention_count = 1;
    f.handle(f.input("blocked-status", "/tasks")).await;
    assert!(f.server.mock.texts().contains("needs attention"));
}
#[tokio::test]
async fn state_is_released_only_after_a_cancelled_callers_blocking_write_ends() {
    // A cancelled connector can leave a write queued on the blocking pool. Its
    // replacement must wait for that write, not find the directory still owned.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    let store = Store::open(path.clone()).await.unwrap();
    let mut closed = store.released();
    let released = async move { while closed.changed().await.is_ok() {} };
    let (finish, finished) = std::sync::mpsc::channel::<()>();
    let held = store.clone();
    let write = tokio::task::spawn_blocking(move || {
        finished.recv().unwrap();
        drop(held);
    });
    drop(store);
    tokio::pin!(released);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut released)
            .await
            .is_err()
    );
    assert!(matches!(
        Store::open(path.clone()).await,
        Err(Error::StateLocked)
    ));
    finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), released)
        .await
        .unwrap();
    write.await.unwrap();
    Store::open(path).await.unwrap();
}

#[tokio::test]
async fn ignored_notifications_do_not_query_snapshots() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Titles).await;
        let identity = f.create().await;
        let task = f.hub.task();
        let mut unrelated = task.clone();
        unrelated.project_id = ProjectId::new();
        unrelated.state = TaskState::Completed;
        let mut delegated = task.clone();
        delegated.state = TaskState::Completed;
        delegated.asked_by = Some(TaskId::new());
        let mut approval = f.hub.approval(ApprovalKind::Command);
        approval.project_id = ProjectId::new();
        let attention = AttentionItem {
            child_id: None,
            attention_id: AttentionId::new(),
            kind: AttentionKind::WaitingForYou,
            title: "Needs a decision".into(),
            body: None,
            task_id: Some(task.task_id),
            run_id: task.current_run,
            approval_id: None,
            created_at: f.clock.now(),
            resolved_at: None,
            priority: 5,
            topic: None,
            quiet: false,
            report_id: None,
        };
        let events = [
            Event::TimelineAppended {
                task_id: task.task_id,
                item: TimelineItem {
                    seq: 9,
                    at: f.clock.now(),
                    run_id: task.current_run,
                    item: TimelineBody::AgentMessage {
                        text: "Tool progress".into(),
                        partial: false,
                    },
                },
            },
            Event::TaskUpserted { task },
            Event::TaskUpserted { task: unrelated },
            Event::TaskUpserted { task: delegated },
            Event::ApprovalUpserted { approval },
            Event::AttentionResolved {
                attention_id: attention.attention_id,
            },
            Event::AttentionUpserted {
                item: AttentionItem {
                    approval_id: Some(ApprovalId::new()),
                    ..attention.clone()
                },
            },
            Event::AttentionUpserted {
                item: AttentionItem {
                    kind: AttentionKind::Completed,
                    ..attention.clone()
                },
            },
            Event::AttentionUpserted {
                item: AttentionItem {
                    kind: AttentionKind::Failure,
                    ..attention.clone()
                },
            },
            Event::AttentionUpserted {
                item: AttentionItem {
                    task_id: None,
                    ..attention.clone()
                },
            },
            Event::AttentionUpserted {
                item: AttentionItem {
                    quiet: true,
                    ..attention.clone()
                },
            },
            Event::AttentionUpserted {
                item: AttentionItem {
                    resolved_at: Some(f.clock.now()),
                    ..attention
                },
            },
        ];
        for event in events {
            f.ignored_notification(&identity, event).await;
        }
    }
}

#[tokio::test]
async fn notifications_recheck_identity_and_grants_before_loading_context() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Titles).await;
        let identity = f.create().await;
        let approval = f.hub.approval(ApprovalKind::Command);
        f.hub.identities.lock().unwrap()[0].grants.approvals = false;
        f.ignored_notification(&identity, Event::ApprovalUpserted { approval })
            .await;

        let mut task = f.hub.task();
        task.state = TaskState::Completed;
        let event = Event::TaskUpserted { task };
        let mut no_tasks = identity.clone();
        no_tasks.grants.tasks = false;
        let mut revoked = identity.clone();
        revoked.revoked_at = Some(f.clock.now());
        let mut relinked = identity.clone();
        relinked.id = ChannelIdentityId::new();
        // Check each kind of lost authority using the original cached identity.
        for current in [no_tasks, revoked, relinked] {
            f.hub.identities.lock().unwrap()[0] = current;
            f.ignored_notification(&identity, event.clone()).await;
        }
    }
}

#[tokio::test]
async fn attention_notifications_recheck_latest_context_and_project() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Titles).await;
        let identity = f.create().await;
        let task = f.hub.task();
        let item = AttentionItem {
            child_id: None,
            attention_id: AttentionId::new(),
            kind: AttentionKind::WaitingForYou,
            title: "Old title".into(),
            body: None,
            task_id: Some(task.task_id),
            run_id: task.current_run,
            approval_id: None,
            created_at: f.clock.now(),
            resolved_at: None,
            priority: 5,
            topic: None,
            quiet: false,
            report_id: None,
        };
        let mut latest = item.clone();
        latest.title = "Latest title".into();
        f.hub.snapshot.lock().unwrap().attention.push(latest);
        let before = f.server.mock.requests().len();
        let queries = f.hub.snapshot_queries.load(Ordering::SeqCst);
        // A stale open event cannot send a resolved or another project's notice.
        for (resolved, project) in [
            (Some(f.clock.now()), task.project_id),
            (None, ProjectId::new()),
            (None, task.project_id),
            (None, task.project_id),
        ] {
            {
                let mut snapshot = f.hub.snapshot.lock().unwrap();
                snapshot.attention[0].resolved_at = resolved;
                snapshot.tasks[0].project_id = project;
            }
            f.core
                .notify(
                    f.io.as_ref(),
                    &f.route("7"),
                    &identity,
                    &f.hub.event(Event::AttentionUpserted { item: item.clone() }),
                )
                .await
                .unwrap();
            let expected = usize::from(resolved.is_none() && project == task.project_id);
            assert_eq!(f.server.mock.requests().len(), before + expected);
        }
        assert_eq!(f.hub.snapshot_queries.load(Ordering::SeqCst), queries + 4);
        assert!(f.server.mock.texts().contains("Latest title"));
        assert!(!f.server.mock.texts().contains("Old title"));
    }
}
