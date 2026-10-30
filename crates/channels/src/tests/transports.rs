// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::{Message, person_name};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as Ws;

struct FakeTokens;
#[async_trait]
impl TokenSource for FakeTokens {
    async fn token(&self, _: ChannelKind) -> Result<Option<BotToken>> {
        Ok(Some(BotToken::new("123:synthetic".into())?))
    }
}
struct NoTokens;
#[async_trait]
impl TokenSource for NoTokens {
    async fn token(&self, _: ChannelKind) -> Result<Option<BotToken>> {
        Ok(None)
    }
}

#[tokio::test]
async fn telegram_conflict_stops_polling_without_deleting_webhook() {
    for webhook in [false, true] {
        let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
        if webhook {
            f.server.mock.script(
                "/bot123:synthetic/getWebhookInfo",
                vec![(
                    200,
                    json!({"ok":true,"result":{"url":"https://another.example.invalid/webhook"}}),
                )],
            );
        } else {
            f.server.mock.script(
                "/bot123:synthetic/getUpdates",
                vec![(
                    409,
                    json!({"ok":false,"error_code":409,"description":"Conflict: another poller"}),
                )],
            );
        }
        let mut cfg = Config::default();
        cfg.telegram = f.core.cfg.clone();
        cfg.telegram.enabled = true;
        cfg.telegram.endpoints.telegram_api = f.server.base.clone();
        let handle = spawn_with_clock(
            f.hub.clone(),
            cfg,
            Arc::new(FakeTokens),
            f.dir.path().join("runner"),
            CancellationToken::new(),
            f.clock.clone(),
        )
        .await
        .unwrap();
        until(|| handle.status()[0].state == ChannelState::Conflict).await;
        let requests = f.server.mock.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|(p, _, _)| p.ends_with("getUpdates"))
                .count(),
            usize::from(!webhook)
        );
        assert!(
            !requests
                .iter()
                .any(|(p, _, _)| p.contains("deleteWebhook") || p.contains("setWebhook"))
        );
        assert!(
            handle.status()[0]
                .remedy
                .as_ref()
                .unwrap()
                .contains("dedicated bot")
        );
        handle.shutdown().await;
    }
}
#[tokio::test]
async fn telegram_offset_replay_and_two_stage_ack_through_mock_api() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    f.hub.link(ChannelKind::Telegram, "7", "42");
    let update = json!({"update_id":8,"message":{"message_id":3,"chat":{"id":42,"type":"private"},"from":{"id":7,"is_bot":false},"text":"/new Use the mapped defaults"}});
    f.server
        .mock
        .inner
        .updates
        .lock()
        .unwrap()
        .extend([json!([update.clone(), update])]);
    let mut cfg = Config::default();
    cfg.telegram = f.core.cfg.clone();
    cfg.telegram.enabled = true;
    cfg.telegram.endpoints.telegram_api = f.server.base.clone();
    let handle = spawn_with_clock(
        f.hub.clone(),
        cfg.clone(),
        Arc::new(FakeTokens),
        f.dir.path().join("runner"),
        CancellationToken::new(),
        f.clock.clone(),
    )
    .await
    .unwrap();
    until(|| {
        f.server
            .mock
            .requests()
            .iter()
            .any(|(p, v, _)| p.ends_with("getUpdates") && v["offset"] == 9)
    })
    .await;
    assert_eq!(f.hub.submitted.lock().unwrap().len(), 1);
    assert_eq!(f.hub.snapshot.lock().unwrap().tasks.len(), 1);
    let text = f.server.mock.texts();
    assert!(text.find("Received").unwrap() < text.find("Accepted by the computer").unwrap());
    handle.shutdown().await;
    f.server.mock.inner.requests.lock().unwrap().clear();
    let handle = spawn_with_clock(
        f.hub.clone(),
        cfg,
        Arc::new(FakeTokens),
        f.dir.path().join("runner"),
        CancellationToken::new(),
        f.clock.clone(),
    )
    .await
    .unwrap();
    until(|| {
        f.server
            .mock
            .requests()
            .iter()
            .any(|(p, _, _)| p.ends_with("getUpdates"))
    })
    .await;
    let offset = f
        .server
        .mock
        .requests()
        .into_iter()
        .find(|(p, _, _)| p.ends_with("getUpdates"))
        .unwrap()
        .1["offset"]
        .clone();
    assert_eq!(offset, 9);
    handle.shutdown().await;
}
#[tokio::test]
async fn one_refused_update_does_not_stop_the_telegram_channel() {
    // A button tapped while the companion was off can no longer be answered, and
    // a chat the bot may not write to refuses the acknowledgement. Neither may
    // stop the channel or pin the offset on the same update after a restart.
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    f.hub.link(ChannelKind::Telegram, "7", "42");
    let stale_tap = json!({"update_id":8,"callback_query":{"id":"cb-old","from":{"id":7,"is_bot":false},"message":{"message_id":3,"chat":{"id":42,"type":"private"}},"data":"x"}});
    let refused_chat = json!({"update_id":9,"message":{"message_id":4,"chat":{"id":43,"type":"group"},"from":{"id":7,"is_bot":false},"text":"hello"}});
    let good = json!({"update_id":10,"message":{"message_id":5,"chat":{"id":42,"type":"private"},"from":{"id":7,"is_bot":false},"text":"/new Use the mapped defaults"}});
    f.server.mock.script(
        "/bot123:synthetic/answerCallbackQuery",
        vec![(
            400,
            json!({"ok":false,"error_code":400,"description":"Bad Request: query is too old and response timeout expired or query ID is invalid"}),
        )],
    );
    f.server.mock.script(
        "/bot123:synthetic/sendMessage",
        vec![
            (200, json!({"ok":true,"result":{"message_id":90}})),
            (
                403,
                json!({"ok":false,"error_code":403,"description":"Forbidden: bot is not a member of the group chat"}),
            ),
        ],
    );
    f.server
        .mock
        .inner
        .updates
        .lock()
        .unwrap()
        .extend([json!([stale_tap, refused_chat, good])]);
    let mut cfg = Config::default();
    cfg.telegram = f.core.cfg.clone();
    cfg.telegram.enabled = true;
    cfg.telegram.endpoints.telegram_api = f.server.base.clone();
    let handle = spawn_with_clock(
        f.hub.clone(),
        cfg.clone(),
        Arc::new(FakeTokens),
        f.dir.path().join("runner"),
        CancellationToken::new(),
        f.clock.clone(),
    )
    .await
    .unwrap();
    until(|| {
        f.server
            .mock
            .requests()
            .iter()
            .any(|(p, v, _)| p.ends_with("getUpdates") && v["offset"] == 11)
    })
    .await;
    assert_eq!(f.hub.submitted.lock().unwrap().len(), 1);
    assert_eq!(handle.status()[0].state, ChannelState::Connected);
    handle.shutdown().await;
    f.server.mock.inner.requests.lock().unwrap().clear();
    let handle = spawn_with_clock(
        f.hub.clone(),
        cfg,
        Arc::new(FakeTokens),
        f.dir.path().join("runner"),
        CancellationToken::new(),
        f.clock.clone(),
    )
    .await
    .unwrap();
    until(|| {
        f.server
            .mock
            .requests()
            .iter()
            .any(|(p, _, _)| p.ends_with("getUpdates"))
    })
    .await;
    let requests = f.server.mock.requests();
    assert_eq!(
        requests
            .iter()
            .find(|(p, _, _)| p.ends_with("getUpdates"))
            .unwrap()
            .1["offset"],
        11
    );
    handle.shutdown().await;
}
// Telegram's error code is a number in the reply. One outside the range of
// HTTP statuses is not another status: 65945 must not read as 409, which
// stops the connector as if another program owned the bot.
#[tokio::test]
async fn telegram_error_code_out_of_range_is_not_another_status() {
    let server = Server::new().await;
    server.mock.script(
        "/bot123:synthetic/test",
        vec![(400, json!({"ok":false,"error_code":65945}))],
    );
    let http = Http::new(
        server.base.clone(),
        BotToken::new("123:synthetic".into()).unwrap(),
        true,
        CancellationToken::new(),
    )
    .unwrap();
    let error = http
        .request(
            reqwest::Method::POST,
            "test",
            "test",
            Some(json!({})),
            Pace::Wait,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Http(400)), "{error:?}");
}
// Discord's rate-limit headers arrive on replies whose request succeeded.
// A reset time out of range is no reason to report the request as failed:
// the message was posted, and failing it leaves a reply half done.
#[tokio::test]
async fn discord_success_with_an_unusable_reset_time_still_succeeds() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut request = vec![0; 4096];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                      x-ratelimit-remaining: 0\r\nx-ratelimit-reset-after: 1e9\r\n\
                      content-length: 11\r\nconnection: close\r\n\r\n{\"id\":\"42\"}",
                )
                .await;
        }
    });
    let http = Http::new(
        base,
        BotToken::new("123:synthetic".into()).unwrap(),
        false,
        CancellationToken::new(),
    )
    .unwrap();
    let reply = http
        .request(
            reqwest::Method::POST,
            "channels/1/messages",
            "send",
            Some(json!({})),
            Pace::Wait,
        )
        .await
        .unwrap();
    assert_eq!(reply["id"], "42");
}
#[tokio::test]
async fn approval_buttons_stay_withdrawn_once_a_connector_starts_with_minimal_exposure() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Titles).await;
    let identity = f.create().await;
    let state = f.dir.path().join("runner");
    let core = |store, exposure| {
        let cfg = ChannelConfig {
            exposure,
            ..f.core.cfg.clone()
        };
        Core::new(f.hub.clone(), cfg, store, f.clock.clone(), f.kind)
    };
    let titles = core(Store::open(state.clone()).await.unwrap(), Exposure::Titles);
    let approval = f.hub.approval(ApprovalKind::Command);
    let event = f.hub.event(Event::ApprovalUpserted { approval });
    titles
        .notify(f.io.as_ref(), &f.route("7"), &identity, &event)
        .await
        .unwrap();
    let buttons = f.server.mock.last_buttons();
    assert_eq!(buttons.len(), 2);
    let mut released = titles.store.released();
    drop(titles);
    while released.changed().await.is_ok() {}
    // The owner switches to minimal exposure and the connector starts with it.
    let mut cfg = Config::default();
    cfg.telegram = f.core.cfg.clone();
    cfg.telegram.exposure = Exposure::Minimal;
    cfg.telegram.enabled = true;
    cfg.telegram.endpoints.telegram_api = f.server.base.clone();
    let handle = spawn_with_clock(
        f.hub.clone(),
        cfg,
        Arc::new(FakeTokens),
        state.clone(),
        CancellationToken::new(),
        f.clock.clone(),
    )
    .await
    .unwrap();
    until(|| handle.status()[0].state == ChannelState::Connected).await;
    handle.shutdown().await;
    // Switching back to titles well within the buttons' ten minutes doesn't
    // bring them back.
    let titles = core(Store::open(state).await.unwrap(), Exposure::Titles);
    let mut press = f.input("allow", "");
    press.action = Some(buttons[0].clone());
    titles.handle(f.io.as_ref(), &mut press).await.unwrap();
    assert_eq!(f.hub.submitted.lock().unwrap().len(), 1);
    assert!(f.server.mock.texts().contains("Request refused (Expired)"));
}
#[tokio::test]
async fn both_platforms_honor_retry_after() {
    for telegram in [true, false] {
        let server = Server::new().await;
        let path = if telegram {
            "/bot123:synthetic/test"
        } else {
            "/test"
        };
        server.mock.script(
            path,
            vec![(
                429,
                if telegram {
                    json!({"ok":false,"error_code":429,"parameters":{"retry_after":0.12}})
                } else {
                    json!({"retry_after":0.12,"global":true})
                },
            )],
        );
        let http = Http::new(
            server.base.clone(),
            BotToken::new("123:synthetic".into()).unwrap(),
            telegram,
            CancellationToken::new(),
        )
        .unwrap();
        http.request(
            reqwest::Method::POST,
            "test",
            "test",
            Some(json!({})),
            Pace::Wait,
        )
        .await
        .unwrap();
        let requests = server.mock.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].2.duration_since(requests[0].2) >= Duration::from_millis(120));
    }
}
#[tokio::test(start_paused = true)]
async fn a_telegram_rate_limit_holds_up_only_its_chat_unless_it_is_global() {
    // Loopback I/O must progress without Tokio auto-advancing to a deadline.
    // This wall-clock timeout is only a failure guard, not a timing assertion.
    async fn drive<T>(work: impl std::future::Future<Output = T>) -> T {
        let timeout = std::time::Instant::now() + Duration::from_secs(10);
        tokio::pin!(work);
        loop {
            tokio::select! {
                biased;
                result = &mut work => return result,
                _ = tokio::task::yield_now() => {
                    assert!(std::time::Instant::now() < timeout, "mock operation timed out");
                }
            }
        }
    }

    for global in [false, true] {
        let server = Server::new().await;
        let mut limit = json!({"ok":false,"error_code":429,"parameters":{"retry_after":1}});
        if global {
            limit["global"] = json!(true);
        }
        server
            .mock
            .inner
            .chat_replies
            .lock()
            .unwrap()
            .insert("42".into(), vec![(429, limit)].into());
        let http = Http::new(
            server.base.clone(),
            BotToken::new("123:synthetic".into()).unwrap(),
            true,
            CancellationToken::new(),
        )
        .unwrap();
        let io = Arc::new(Telegram::new(http.clone()));
        let sent = |chat: &str| {
            let chat = chat.to_owned();
            server
                .mock
                .requests()
                .into_iter()
                .filter(|(p, v, _)| p.ends_with("sendMessage") && v["chat_id"] == chat)
                .map(|(_, _, at)| at)
                .collect::<Vec<_>>()
        };
        let limited = tokio::spawn({
            let io = io.clone();
            async move { io.send("42", Message::text("first")).await }
        });
        // A logged request is too early: the client must have read the 429
        // and installed its limit before the second chat starts.
        let deadline = drive(async {
            loop {
                let (chat, all) = http.rate_limit_deadlines("sendMessage:42").await;
                if let Some(deadline) = if global { all } else { chat } {
                    assert_eq!(if global { chat } else { all }, None);
                    break deadline;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(
            deadline - tokio::time::Instant::now(),
            Duration::from_secs(1)
        );
        assert_eq!(sent("42").len(), 1);
        // Poll the same pacing future used by request(), before socket I/O can
        // hide a missing wait by returning Pending for an unrelated reason.
        let other = http.wait_for_rate_limit("sendMessage", "sendMessage:43", Pace::Yield);
        tokio::pin!(other);
        if global {
            assert!(futures_util::poll!(&mut other).is_pending());
        } else {
            assert!(matches!(
                futures_util::poll!(&mut other),
                std::task::Poll::Ready(Ok(()))
            ));
            drive(io.send("43", Message::text("second"))).await.unwrap();
            assert_eq!(sent("43"), vec![deadline - Duration::from_secs(1)]);
        }
        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(!limited.is_finished());
        assert_eq!(sent("42").len(), 1);
        if global {
            assert!(futures_util::poll!(&mut other).is_pending());
            assert!(sent("43").is_empty());
        }
        // Cross the deadline and Tokio's millisecond timer rounding boundary.
        tokio::time::advance(Duration::from_millis(2)).await;
        if global {
            assert!(matches!(
                futures_util::poll!(&mut other),
                std::task::Poll::Ready(Ok(()))
            ));
            drive(io.send("43", Message::text("second"))).await.unwrap();
            assert_eq!(sent("43").len(), 1);
            assert!(sent("43")[0] >= deadline);
        }
        drive(limited).await.unwrap().unwrap();
        // The limited chat itself waits out retry_after before trying again.
        let retries = sent("42");
        assert_eq!(retries.len(), 2);
        assert!(retries[1] >= deadline);
    }
}
#[tokio::test]
async fn a_long_chat_limit_is_returned_rather_than_waited_out() {
    let server = Server::new().await;
    let limit = json!({"ok":false,"error_code":429,"parameters":{"retry_after":30}});
    server
        .mock
        .inner
        .chat_replies
        .lock()
        .unwrap()
        .insert("42".into(), vec![(429, limit)].into());
    let io = server.io(ChannelKind::Telegram);
    let sends = |chat: &str| {
        server
            .mock
            .requests()
            .iter()
            .filter(|(p, v, _)| p.ends_with("sendMessage") && v["chat_id"] == chat)
            .count()
    };
    let started = tokio::time::Instant::now();
    for _ in 0..2 {
        let Err(Error::RateLimited(wait)) = io.send("42", Message::text("first")).await else {
            panic!("a long limit is returned to the caller");
        };
        assert!(wait > Duration::from_secs(25), "{wait:?}");
    }
    // The second attempt knew the limit and didn't call the platform again.
    assert_eq!(sends("42"), 1);
    io.send("43", Message::text("second")).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    // A global limit still holds up every chat, however long it is.
    let server = Server::new().await;
    server.mock.inner.chat_replies.lock().unwrap().insert(
        "42".into(),
        vec![(
            429,
            json!({"ok":false,"error_code":429,"global":true,"parameters":{"retry_after":1.5}}),
        )]
        .into(),
    );
    let io = server.io(ChannelKind::Telegram);
    let started = tokio::time::Instant::now();
    io.send("42", Message::text("first")).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(1500));
}
#[tokio::test]
async fn a_discord_interaction_rate_limit_holds_up_only_its_chat() {
    let server = Server::new().await;
    server.mock.script(
        "/interactions/111/token-a/callback",
        vec![(429, json!({"retry_after":1.0,"global":false}))],
    );
    let discord = Arc::new(Discord::new(
        Http::new(
            server.base.clone(),
            BotToken::new("123:synthetic".into()).unwrap(),
            false,
            CancellationToken::new(),
        )
        .unwrap(),
    ));
    let interaction = |id: &str, token: &str, chat: &str| json!({"id":id,"token":token,"application_id":"123","channel_id":chat});
    let limited = tokio::spawn({
        let discord = discord.clone();
        let event = interaction("111", "token-a", "42");
        async move { discord.acknowledge(&event).await }
    });
    until(|| {
        server
            .mock
            .requests()
            .iter()
            .any(|(p, _, _)| p == "/interactions/111/token-a/callback")
    })
    .await;
    let other = tokio::time::Instant::now();
    discord
        .acknowledge(&interaction("222", "token-b", "43"))
        .await
        .unwrap();
    assert!(other.elapsed() < Duration::from_millis(500));
    limited.await.unwrap().unwrap();
    assert!(other.elapsed() >= Duration::from_millis(900));
}
#[tokio::test]
async fn discord_gateway_resumes_and_slash_commands_reach_the_same_authority() {
    let f = Fixture::new(ChannelKind::Discord, Exposure::Minimal).await;
    f.hub.link(ChannelKind::Discord, "7", "42");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = format!("ws://{}", listener.local_addr().unwrap());
    let (resumed_tx, resumed_rx) = tokio::sync::oneshot::channel();
    let server_base = gateway.clone();
    let hub = f.hub.clone();
    let gateway_worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Ws::Text(
                json!({"op":10,"d":{"heartbeat_interval":1000}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let identify: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(identify["op"], 2);
        assert_eq!(identify["d"]["intents"], 4096);
        socket.send(Ws::Text(json!({"op":0,"s":1,"t":"READY","d":{"session_id":"synthetic-session","resume_gateway_url":server_base}}).to_string().into())).await.unwrap();
        let event = json!({"id":"999","application_id":"123","token":"synthetic-interaction","type":2,"channel_id":"42","user":{"id":"7"},"data":{"name":"new","options":[{"name":"prompt","value":"Start via slash command"}]}});
        socket
            .send(Ws::Text(
                json!({"op":0,"s":2,"t":"INTERACTION_CREATE","d":event})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        until(|| !hub.submitted.lock().unwrap().is_empty()).await;
        socket
            .send(Ws::Text(json!({"op":7,"d":null}).to_string().into()))
            .await
            .unwrap();
        drop(socket);
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Ws::Text(
                json!({"op":10,"d":{"heartbeat_interval":1000}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let resume: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(resume["op"], 6);
        assert_eq!(resume["d"]["session_id"], "synthetic-session");
        assert_eq!(resume["d"]["seq"], 2);
        socket
            .send(Ws::Text(
                json!({"op":0,"s":3,"t":"RESUMED","d":{}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        resumed_tx.send(()).unwrap();
        while let Some(Ok(frame)) = socket.next().await {
            if frame.is_text() {
                let v: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                if v["op"] == 1 {
                    let _ = socket
                        .send(Ws::Text(json!({"op":11,"d":null}).to_string().into()))
                        .await;
                }
            }
        }
    });
    let mut cfg = Config::default();
    cfg.discord = f.core.cfg.clone();
    cfg.discord.enabled = true;
    cfg.discord.endpoints.discord_api = f.server.base.clone();
    cfg.discord.endpoints.discord_gateway = gateway;
    let handle = spawn_with_clock(
        f.hub.clone(),
        cfg,
        Arc::new(FakeTokens),
        f.dir.path().join("runner"),
        CancellationToken::new(),
        f.clock.clone(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(8), resumed_rx)
        .await
        .unwrap()
        .unwrap();
    until(|| f.server.mock.texts().contains("Accepted by the computer")).await;
    assert_eq!(f.hub.submitted.lock().unwrap().len(), 1);
    let requests = f.server.mock.requests();
    assert!(
        requests
            .iter()
            .any(|(p, v, _)| p.ends_with("/callback") && v["data"]["content"] == "Received")
    );
    assert_eq!(
        requests
            .iter()
            .filter(|(p, _, _)| p.ends_with("/commands"))
            .count(),
        7
    );
    handle.shutdown().await;
    gateway_worker.abort();
}
// The gateway is on the network. A dispatch whose body is not an object is
// not an event; it is skipped and the connection goes on.
#[tokio::test]
async fn discord_dispatch_without_an_object_body_is_skipped() {
    let f = Fixture::new(ChannelKind::Discord, Exposure::Minimal).await;
    f.hub.link(ChannelKind::Discord, "7", "42");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = format!("ws://{}", listener.local_addr().unwrap());
    let hub = f.hub.clone();
    let gateway_worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Ws::Text(
                json!({"op":10,"d":{"heartbeat_interval":1000}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        let mut frames =
            vec![json!({"op":0,"s":1,"t":"READY","d":{"session_id":"synthetic-session"}})];
        for (s, d) in [
            json!(1),
            json!("text"),
            json!([1]),
            json!(true),
            Value::Null,
        ]
        .into_iter()
        .enumerate()
        {
            frames.push(json!({"op":0,"s":s + 2,"t":"MESSAGE_CREATE","d":d}));
        }
        let event = json!({"id":"999","application_id":"123","token":"synthetic-interaction","type":2,"channel_id":"42","user":{"id":"7"},"data":{"name":"new","options":[{"name":"prompt","value":"Start after odd frames"}]}});
        frames.push(json!({"op":0,"s":9,"t":"INTERACTION_CREATE","d":event}));
        for frame in frames {
            socket
                .send(Ws::Text(frame.to_string().into()))
                .await
                .unwrap();
        }
        until(|| !hub.submitted.lock().unwrap().is_empty()).await;
        while let Some(Ok(frame)) = socket.next().await {
            if frame.is_text() {
                let v: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                if v["op"] == 1 {
                    let _ = socket
                        .send(Ws::Text(json!({"op":11,"d":null}).to_string().into()))
                        .await;
                }
            }
        }
    });
    let mut cfg = Config::default();
    cfg.discord = f.core.cfg.clone();
    cfg.discord.enabled = true;
    cfg.discord.endpoints.discord_api = f.server.base.clone();
    cfg.discord.endpoints.discord_gateway = gateway;
    let handle = spawn_with_clock(
        f.hub.clone(),
        cfg,
        Arc::new(FakeTokens),
        f.dir.path().join("runner"),
        CancellationToken::new(),
        f.clock.clone(),
    )
    .await
    .unwrap();
    until(|| !f.hub.submitted.lock().unwrap().is_empty()).await;
    handle.shutdown().await;
    gateway_worker.abort();
}
#[tokio::test]
async fn subscriptions_notify_linked_routes_and_suppress_revoked_routes() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Titles).await;
    let identity = f.create().await;
    let stop = CancellationToken::new();
    let core = f.core.clone();
    let io = f.io.clone();
    let shutdown = stop.clone();
    let worker = tokio::spawn(async move {
        core.notifications(
            io,
            "123".into(),
            shutdown,
            &crate::recovery::Status::new(&|_| {}),
        )
        .await
    });
    until(|| f.hub.events.receiver_count() > 0).await;
    let mut task = f.hub.task();
    task.state = TaskState::Failed;
    let event = f.hub.event(Event::TaskUpserted { task });
    f.hub.events.send(Outbound::Event(Arc::new(event))).unwrap();
    until(|| f.server.mock.texts().contains("failed")).await;
    f.hub
        .identities
        .lock()
        .unwrap()
        .iter_mut()
        .find(|i| i.id == identity.id)
        .unwrap()
        .revoked_at = Some(f.clock.now());
    let a = f.hub.approval(ApprovalKind::Command);
    let event = f.hub.event(Event::ApprovalUpserted { approval: a });
    f.hub.events.send(Outbound::Event(Arc::new(event))).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(f.server.mock.last_buttons().is_empty());
    stop.cancel();
    worker.await.unwrap().unwrap();
}
#[tokio::test]
async fn disabled_and_missing_tokens_never_connect() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    let mut cfg = Config::default();
    cfg.telegram.enabled = true;
    let handle = spawn(
        f.hub.clone(),
        cfg,
        Arc::new(NoTokens),
        f.dir.path().join("runner"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(handle.status()[0].state, ChannelState::TokenMissing);
    assert_eq!(handle.status()[1].state, ChannelState::Disabled);
    handle.shutdown().await;
}
#[test]
fn decoding_never_infers_group_identity_or_privileged_content() {
    assert!(telegram::decode(&json!({"update_id":1,"message":{"message_id":1,"chat":{"id":42},"sender_chat":{"id":42},"from":{"id":7},"text":"/new forbidden"}}),"123", "companion_bot").is_none());
    assert!(discord::decode(&json!({"id":"1","guild_id":"2","channel_id":"42","author":{"id":"7"},"content":"secret"}),"123").is_none());
    let input=discord::decode(&json!({"_interaction":true,"id":"1","type":3,"channel_id":"42","member":{"user":{"id":"7"}},"data":{"custom_id":"opaque:allow"}}),"123").unwrap();
    assert_eq!(input.route.user, "7");
    assert_eq!(input.action.as_deref(), Some("opaque:allow"));
    let input=discord::decode(&json!({"_interaction":true,"id":"1","type":2,"channel_id":"42","member":{"user":{"id":"7"}},"data":{"name":"answer","options":[{"name":"message","value":"456"},{"name":"text","value":"Answer text"}]}}),"123").unwrap();
    assert_eq!(input.reply.as_deref(), Some("456"));
    assert_eq!(input.text, "Answer text");
}

#[test]
fn senders_are_named_as_their_platform_shows_them() {
    let telegram = |from: Value| {
        telegram::decode(&json!({"update_id":1,"message":{"message_id":1,"chat":{"id":42},"from":from,"text":"hi"}}), "123", "companion_bot").unwrap().name
    };
    assert_eq!(
        telegram(json!({"id":7,"first_name":"Sam","last_name":"Rivers","username":"sam"}))
            .as_deref(),
        Some("Sam Rivers")
    );
    assert_eq!(
        telegram(json!({"id":7,"first_name":"Sam"})).as_deref(),
        Some("Sam")
    );
    assert_eq!(
        telegram(json!({"id":7,"first_name":" ","username":"sam_r"})).as_deref(),
        Some("@sam_r")
    );
    assert_eq!(telegram(json!({"id":7})), None);
    let callback = telegram::decode(&json!({"update_id":2,"callback_query":{"id":"c","from":{"id":7,"first_name":"Sam"},"data":"x","message":{"message_id":1,"chat":{"id":42}}}}), "123", "companion_bot").unwrap();
    assert_eq!(callback.name.as_deref(), Some("Sam"));

    let discord = |user: Value| {
        discord::decode(&json!({"_interaction":true,"id":"1","type":3,"channel_id":"42","member":{"user":user},"data":{"custom_id":"x"}}), "123").unwrap().name
    };
    assert_eq!(
        discord(json!({"id":"7","global_name":"Sam Rivers","username":"samr"})).as_deref(),
        Some("Sam Rivers")
    );
    assert_eq!(
        discord(json!({"id":"7","global_name":null,"username":"samr"})).as_deref(),
        Some("samr")
    );
    assert_eq!(discord(json!({"id":"7"})), None);
    let dm = discord::decode(
        &json!({"id":"1","channel_id":"42","author":{"id":"7","username":"samr"},"content":"hi"}),
        "123",
    )
    .unwrap();
    assert_eq!(dm.name.as_deref(), Some("samr"));
}
#[test]
fn sender_names_are_cleaned_and_bounded() {
    assert_eq!(
        person_name("Sam\n\tRivers\u{7}").as_deref(),
        Some("Sam Rivers")
    );
    assert_eq!(
        person_name("\u{202E}sreviR maS").as_deref(),
        Some("sreviR maS")
    );
    assert_eq!(person_name(" \u{200F}\n "), None);
    let long = person_name(&"é".repeat(300)).unwrap();
    assert_eq!(long.chars().count(), 64);
    assert!(long.ends_with('…'));
    assert_eq!(person_name(&"a".repeat(64)).unwrap(), "a".repeat(64));
}
#[test]
fn sender_names_fit_the_hub_in_bytes_as_well_as_characters() {
    // Fifty four-byte letters are exactly 200 bytes and are kept whole.
    let exact = "𝓢".repeat(50);
    assert_eq!(person_name(&exact).unwrap(), exact);
    // One byte more is shortened to exactly 200 bytes, ellipsis included.
    let over = person_name(&format!("a{exact}")).unwrap();
    assert_eq!(over, format!("a{}…", "𝓢".repeat(49)));
    assert_eq!(over.len(), 200);
    // Room for the ellipsis ends inside a four-byte letter; that letter goes.
    let straddle = person_name(&format!("ab{}", "𝓢".repeat(60))).unwrap();
    assert_eq!(straddle, format!("ab{}…", "𝓢".repeat(48)));
    assert_eq!(straddle.len(), 197);
}

#[test]
fn a_telegram_group_command_for_another_bot_is_not_this_bots() {
    let text = |text: &str| {
        telegram::decode(
            &json!({"update_id":1,"message":{"message_id":1,"chat":{"id":-42},"from":{"id":7},"text":text}}),
            "123",
            "companion_bot",
        )
        .map(|input| input.text)
    };
    assert_eq!(text("/new@OtherBot Delete the branch"), None);
    assert_eq!(
        text("/new@Companion_Bot Fix the tests").as_deref(),
        Some("/new Fix the tests")
    );
    assert_eq!(text("/tasks@companion_bot").as_deref(), Some("/tasks"));
    assert_eq!(text("/tasks").as_deref(), Some("/tasks"));
    assert_eq!(text("Ask a@b.example").as_deref(), Some("Ask a@b.example"));
}
#[test]
fn a_forwarded_telegram_message_is_ordinary_text_and_its_buttons_are_not_pressed() {
    let text = "/new@OtherBot Delete the branch";
    for forward in [
        json!({"forward_origin":{"type":"hidden_user","date":1,"sender_user_name":"Ana"}}),
        json!({"forward_date":1}),
    ] {
        let mut message = json!({"message_id":1,"chat":{"id":-42},"from":{"id":7},"text":text});
        message
            .as_object_mut()
            .unwrap()
            .extend(forward.as_object().unwrap().clone());
        let input = telegram::decode(
            &json!({"update_id":1,"message":message.clone()}),
            "123",
            "companion_bot",
        )
        .unwrap();
        assert!(input.forwarded);
        assert_eq!(input.text, text);
        let pressed = telegram::decode(
            &json!({"update_id":2,"callback_query":{"id":"c","from":{"id":7},"data":"reference:allow","message":message}}),
            "123",
            "companion_bot",
        );
        assert!(pressed.is_none());
    }
    let typed = telegram::decode(
        &json!({"update_id":3,"message":{"message_id":1,"chat":{"id":-42},"from":{"id":7},"text":"/tasks"}}),
        "123",
        "companion_bot",
    )
    .unwrap();
    assert!(!typed.forwarded);
}
#[test]
fn sender_names_drop_invisible_characters() {
    assert_eq!(
        person_name("Sam\u{200B} Riv\u{00AD}ers\u{FEFF}\u{E0041}").as_deref(),
        Some("Sam Rivers")
    );
    assert_eq!(
        person_name("\u{3164}\u{200B}\u{115F}\u{1160}\u{FFA0}\u{2060}"),
        None
    );
    // A name with nothing left to show falls back as a missing one does.
    let telegram = |from: Value| {
        telegram::decode(&json!({"update_id":1,"message":{"message_id":1,"chat":{"id":42},"from":from,"text":"hi"}}), "123", "companion_bot").unwrap().name
    };
    assert_eq!(
        telegram(json!({"id":7,"first_name":"\u{3164}","username":"sam_r"})).as_deref(),
        Some("@sam_r")
    );
    assert_eq!(
        telegram(json!({"id":7,"first_name":"\u{200B}\u{3164}"})),
        None
    );
}
// A Telegram name written in a decorative script is four bytes a letter.
// Shortened by characters alone it stayed over the hub's 200 bytes, so its
// chat was refused a link code on every message and could never be linked.
#[tokio::test]
async fn a_kept_sender_name_can_still_request_a_link() {
    let script = "𝓢𝓪𝓶𝓪𝓷𝓽𝓱𝓪𝓡𝓲𝓿𝓮𝓻𝓼𝓢".repeat(2);
    let name = telegram::decode(
        &json!({"update_id":1,"message":{"message_id":1,"chat":{"id":42},"from":{"id":7,"first_name":script,"last_name":script},"text":"hi"}}),
        "123",
        "companion_bot",
    )
    .unwrap()
    .name
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let hub = services::CompanionHub::builder()
        .data_dir(dir.path().join("data"))
        .build()
        .await
        .unwrap();
    let result = hub
        .request_channel_link(ChannelLinkRequest {
            channel: ChannelKind::Telegram,
            external_user: "7".into(),
            external_chat: "42".into(),
            display_name: name.clone(),
        })
        .await;
    hub.shutdown().await.unwrap();
    assert!(
        result.is_ok(),
        "{} characters, {} bytes: {:?}",
        name.chars().count(),
        name.len(),
        result.err().map(|e| e.code)
    );
}

// U+061C ARABIC LETTER MARK is a bidi control like U+200F. Kept, it
// would let a name change how its neighbours read in the app.
#[test]
fn sender_names_drop_every_bidi_control() {
    assert_eq!(
        person_name("Sam\u{061C} Rivers").as_deref(),
        Some("Sam Rivers")
    );
}

#[tokio::test]
async fn discord_rest_observes_bucket_reset_headers() {
    let times = Arc::new(Mutex::new(Vec::<tokio::time::Instant>::new()));
    let shared = times.clone();
    let app = Router::new().route(
        "/bucket",
        any(move || {
            let times = shared.clone();
            async move {
                times.lock().unwrap().push(tokio::time::Instant::now());
                (
                    [
                        ("x-ratelimit-bucket", "test-bucket"),
                        ("x-ratelimit-remaining", "0"),
                        ("x-ratelimit-reset-after", "0.12"),
                    ],
                    axum::Json(json!({"id":"1"})),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let worker = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let http = Http::new(
        base,
        BotToken::new("synthetic".into()).unwrap(),
        false,
        CancellationToken::new(),
    )
    .unwrap();
    for _ in 0..2 {
        http.request(
            reqwest::Method::POST,
            "bucket",
            "same-major-route",
            Some(json!({})),
            Pace::Wait,
        )
        .await
        .unwrap();
    }
    let times = times.lock().unwrap();
    assert!(times[1].duration_since(times[0]) >= Duration::from_millis(120));
    worker.abort();
}

#[tokio::test]
async fn telegram_conflict_is_terminal_even_without_a_json_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(any(|| async {
        (axum::http::StatusCode::CONFLICT, "another poller")
    }));
    let worker = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let http = Http::new(
        base,
        BotToken::new("synthetic".into()).unwrap(),
        true,
        CancellationToken::new(),
    )
    .unwrap();
    assert!(matches!(
        http.request(
            reqwest::Method::POST,
            "getUpdates",
            "poll",
            Some(json!({})),
            Pace::Wait,
        )
        .await,
        Err(Error::Conflict)
    ));
    worker.abort();
}
