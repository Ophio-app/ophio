// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::recovery::{Failure, Scope, Status};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as Ws;

struct SyntheticTokens;
#[async_trait]
impl TokenSource for SyntheticTokens {
    async fn token(&self, _: ChannelKind) -> Result<Option<BotToken>> {
        Ok(Some(BotToken::new("123:synthetic".into())?))
    }
}

// Keep Tokio from auto-advancing while loopback I/O or SQLite runs on another
// thread. Only explicit advance calls move deadlines; the wall deadline below
// is a failure guard, not part of the behavior under test.
struct FrozenTime(tokio::task::JoinHandle<()>);
impl FrozenTime {
    fn new() -> Self {
        tokio::time::pause();
        Self(tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        }))
    }
    async fn advance(&self, clock: &FakeClock, duration: Duration) {
        let duration = duration + Duration::from_millis(1);
        clock.advance(duration);
        tokio::time::advance(duration).await;
    }
}
impl Drop for FrozenTime {
    fn drop(&mut self) {
        self.0.abort();
        tokio::time::resume();
    }
}
async fn ready(mut check: impl FnMut() -> bool) {
    // Tokio rounds timer deadlines up to the next millisecond. Let newly
    // scheduled immediate ticks register and cross that rounding boundary.
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(
            std::time::Instant::now() < deadline,
            "mock condition timed out"
        );
        tokio::task::yield_now().await;
    }
}
fn count(mock: &Mock, suffix: &str) -> usize {
    mock.requests()
        .iter()
        .filter(|(p, _, _)| p.ends_with(suffix))
        .count()
}
fn notices(mock: &Mock, chat: &str) -> usize {
    mock.requests()
        .iter()
        .filter(|(p, body, _)| {
            p.ends_with("sendMessage")
                && body["chat_id"] == chat
                && body["text"].as_str().is_some_and(|t| t.contains("failed"))
        })
        .count()
}
fn update(id: i64) -> Value {
    json!({"update_id":id,"message":{"message_id":id,"chat":{"id":42},"from":{"id":7},"text":"/new Synthetic request"}})
}
async fn telegram_worker(f: &Fixture) -> ChannelsHandle {
    let mut cfg = Config::default();
    cfg.telegram = f.core.cfg.clone();
    cfg.telegram.enabled = true;
    cfg.telegram.endpoints.telegram_api = f.server.base.clone();
    spawn_with_clock(
        f.hub.clone(),
        cfg,
        Arc::new(SyntheticTokens),
        f.dir.path().join("runner"),
        CancellationToken::new(),
        f.clock.clone(),
    )
    .await
    .unwrap()
}

#[test]
fn error_policy_distinguishes_authentication_from_item_refusal() {
    for error in [
        Error::Conflict,
        Error::State,
        Error::StateLocked,
        Error::Configuration("synthetic"),
        Error::TokenStore,
    ] {
        assert_eq!(error.failure(Scope::Channel), Failure::Fatal);
        assert_eq!(error.failure(Scope::Item), Failure::Fatal);
    }
    for error in [
        Error::Connection,
        Error::Http(500),
        Error::Http(502),
        Error::Http(599),
        Error::Http(429),
    ] {
        assert_eq!(error.failure(Scope::Item), Failure::Transient);
        assert_eq!(error.failure(Scope::Channel), Failure::Transient);
    }
    for code in [401, 403] {
        assert_eq!(Error::Http(code).failure(Scope::Channel), Failure::Fatal);
        assert_eq!(Error::Http(code).failure(Scope::Item), Failure::Refused);
    }
    for error in [
        Error::Http(400),
        Error::Http(404),
        Error::PlatformProtocol,
        Error::Hub(ErrorCode::PermissionDenied),
    ] {
        assert_eq!(error.failure(Scope::Item), Failure::Refused);
    }
    assert_eq!(Error::Shutdown.failure(Scope::Item), Failure::Shutdown);
}

#[tokio::test]
async fn telegram_transient_ack_retries_saved_offset_once_and_recovers() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    f.hub.link(ChannelKind::Telegram, "7", "42");
    let time = FrozenTime::new();
    f.server
        .mock
        .script("/bot123:synthetic/sendMessage", vec![(502, json!({}))]);
    f.server
        .mock
        .inner
        .updates
        .lock()
        .unwrap()
        .extend([json!([update(8)]), json!([update(8)])]);
    let handle = telegram_worker(&f).await;
    ready(|| handle.status()[0].state == ChannelState::PlatformError).await;
    assert_eq!(count(&f.server.mock, "getUpdates"), 1);
    assert!(f.hub.submitted.lock().unwrap().is_empty());
    time.advance(&f.clock, Duration::from_secs(1)).await;
    ready(|| handle.status()[0].state == ChannelState::Connected).await;
    assert_eq!(f.hub.submitted.lock().unwrap().len(), 1);
    assert_eq!(f.hub.snapshot.lock().unwrap().tasks.len(), 1);
    let polls: Vec<_> = f
        .server
        .mock
        .requests()
        .into_iter()
        .filter(|(p, _, _)| p.ends_with("getUpdates"))
        .collect();
    assert_eq!(polls[0].1["offset"], polls[1].1["offset"]);
    assert!(polls[1].2.duration_since(polls[0].2) >= Duration::from_secs(1));
    handle.shutdown().await;
}

#[tokio::test]
async fn telegram_reply_retries_reuse_ack_and_backoff_does_not_reset_on_poll_success() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    f.hub.link(ChannelKind::Telegram, "7", "42");
    let time = FrozenTime::new();
    f.server.mock.script(
        "/bot123:synthetic/editMessageText",
        vec![(502, json!({})), (502, json!({}))],
    );
    f.server.mock.inner.updates.lock().unwrap().extend([
        json!([update(8), update(9)]),
        json!([update(8), update(9)]),
        json!([update(8), update(9)]),
    ]);
    let states = Arc::new(Mutex::new(vec![]));
    let stop = CancellationToken::new();
    let io = Arc::new(Telegram::new(
        Http::new(
            f.server.base.clone(),
            BotToken::new("123:synthetic".into()).unwrap(),
            true,
            stop.clone(),
        )
        .unwrap(),
    ));
    let core = f.core.clone();
    let shutdown = stop.clone();
    let recorded = states.clone();
    let worker = tokio::spawn(async move {
        let update = |state| recorded.lock().unwrap().push(state);
        telegram::poll(
            core,
            io,
            "123".into(),
            "companion_bot".into(),
            shutdown,
            &Status::new(&update),
        )
        .await
    });
    ready(|| states.lock().unwrap().len() == 1).await;
    assert_eq!(count(&f.server.mock, "sendMessage"), 1);
    time.advance(&f.clock, Duration::from_secs(1)).await;
    ready(|| states.lock().unwrap().len() == 2).await;
    // The second reply failure schedules two seconds despite a healthy getUpdates.
    time.advance(&f.clock, Duration::from_secs(2)).await;
    ready(|| {
        states.lock().unwrap().last() == Some(&ChannelState::Connected)
            && f.hub.commands.lock().unwrap().len() == 2
    })
    .await;
    assert_eq!(count(&f.server.mock, "sendMessage"), 2);
    let submits = f.hub.submitted.lock().unwrap().clone();
    assert_eq!(submits.len(), 4);
    assert!(
        submits[..3]
            .iter()
            .all(|(_, c)| c.command_id == submits[0].1.command_id)
    );
    let edits: Vec<_> = f
        .server
        .mock
        .requests()
        .into_iter()
        .filter(|(p, _, _)| p.ends_with("editMessageText"))
        .collect();
    assert!(edits[2].2.duration_since(edits[1].2) >= Duration::from_secs(2));
    assert_eq!(edits[0].1["message_id"], edits[2].1["message_id"]);
    stop.cancel();
    worker.await.unwrap().unwrap();
}

fn prompts(hub: &FakeHub) -> Vec<String> {
    hub.submitted
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, c)| match &c.body {
            Command::CreateTask { prompt, .. } => Some(prompt.clone()),
            _ => None,
        })
        .collect()
}
fn chat_sends(mock: &Mock, chat: &str) -> usize {
    mock.requests()
        .iter()
        .filter(|(p, v, _)| p.ends_with("sendMessage") && v["chat_id"] == chat)
        .count()
}

#[tokio::test]
async fn a_telegram_chat_under_a_long_limit_waits_while_other_chats_go_ahead() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    let (core, _) = notification_core(&f).await;
    let message = |id: i64, chat: i64, text: &str| json!({"update_id":id,"message":{"message_id":id,"chat":{"id":chat},"from":{"id":7},"text":text}});
    let batch = json!([
        message(8, 42, "/new First in the limited chat"),
        message(9, 43, "/new In another chat"),
        message(10, 42, "/new Second in the limited chat"),
    ]);
    // Telegram delivers unconfirmed updates again on every poll.
    f.server
        .mock
        .inner
        .updates
        .lock()
        .unwrap()
        .extend(std::iter::repeat_n(batch, 10));
    f.server.mock.inner.chat_replies.lock().unwrap().insert(
        "42".into(),
        vec![(
            429,
            json!({"ok":false,"error_code":429,"parameters":{"retry_after":2}}),
        )]
        .into(),
    );
    let stop = CancellationToken::new();
    let io = Arc::new(Telegram::new(
        Http::new(
            f.server.base.clone(),
            BotToken::new("123:synthetic".into()).unwrap(),
            true,
            stop.clone(),
        )
        .unwrap(),
    ));
    let offset = || async {
        core.store
            .get::<i64>("offsets", "telegram:123")
            .await
            .unwrap()
    };
    let worker = tokio::spawn({
        let (core, shutdown) = (core.clone(), stop.clone());
        async move {
            let update = |_| {};
            telegram::poll(
                core,
                io,
                "123".into(),
                "companion_bot".into(),
                shutdown,
                &Status::new(&update),
            )
            .await
        }
    });
    until(|| prompts(&f.hub) == ["In another chat"]).await;
    // The limited chat's later update waits behind its first, and neither is
    // confirmed to Telegram yet.
    assert_eq!(chat_sends(&f.server.mock, "42"), 1);
    assert_eq!(offset().await, None);
    until(|| prompts(&f.hub).len() == 3).await;
    assert_eq!(
        prompts(&f.hub)[1..],
        ["First in the limited chat", "Second in the limited chat"]
    );
    let sends: Vec<_> = f
        .server
        .mock
        .requests()
        .into_iter()
        .filter(|(p, v, _)| p.ends_with("sendMessage") && v["chat_id"] != "43")
        .map(|(_, _, at)| at)
        .collect();
    assert!(sends[1].duration_since(sends[0]) >= Duration::from_secs(2));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while offset().await != Some(11) {
        assert!(std::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    stop.cancel();
    worker.await.unwrap().unwrap();
}

#[tokio::test]
async fn preflight_transient_failure_is_supervised_and_auth_refusal_is_terminal() {
    for (path, code) in [("getWebhookInfo", 502), ("getMe", 401), ("getUpdates", 403)] {
        let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
        let time = FrozenTime::new();
        f.server.mock.script(
            &format!("/bot123:synthetic/{path}"),
            vec![(code, json!({}))],
        );
        let handle = telegram_worker(&f).await;
        ready(|| handle.status()[0].state == ChannelState::PlatformError).await;
        time.advance(&f.clock, Duration::from_secs(1)).await;
        if code == 502 {
            ready(|| handle.status()[0].state == ChannelState::Connected).await;
            assert_eq!(count(&f.server.mock, path), 2);
            assert!(count(&f.server.mock, "getUpdates") > 0);
        } else {
            assert_eq!(count(&f.server.mock, path), 1);
            assert_eq!(handle.status()[0].state, ChannelState::PlatformError);
        }
        handle.shutdown().await;
    }
}

#[tokio::test]
async fn rate_limit_without_usable_retry_after_remains_transient() {
    let server = Server::new().await;
    for body in [json!({}), json!({"parameters":{"retry_after":-1}})] {
        server
            .mock
            .script("/bot123:synthetic/test", vec![(429, body)]);
        let http = Http::new(
            server.base.clone(),
            BotToken::new("123:synthetic".into()).unwrap(),
            true,
            CancellationToken::new(),
        )
        .unwrap();
        assert!(matches!(
            http.request(reqwest::Method::POST, "test", "test", None, Pace::Wait)
                .await,
            Err(Error::Http(429))
        ));
    }
}

async fn notification_core(f: &Fixture) -> (Arc<Core>, Vec<(Route, ChannelIdentity)>) {
    let mut cfg = f.core.cfg.clone();
    cfg.projects.insert("43".into(), f.hub.project());
    let core = Arc::new(Core::new(
        f.hub.clone(),
        cfg,
        f.core.store.clone(),
        f.clock.clone(),
        f.kind,
    ));
    let mut routes = vec![];
    for chat in ["42", "43"] {
        let route = Route {
            bot: "123".into(),
            user: "7".into(),
            chat: chat.into(),
        };
        let identity = f
            .hub
            .channel_identity(f.kind, "7", chat)
            .await
            .unwrap_or_else(|| f.hub.link(f.kind, "7", chat));
        core.store
            .put("telegram_routes", &route.key(f.kind), &route)
            .await
            .unwrap();
        core.store
            .put(
                "cursors",
                &format!("{}:{}", route.key(f.kind), identity.id),
                &0_u64,
            )
            .await
            .unwrap();
        routes.push((route, identity));
    }
    (core, routes)
}
fn notification_worker(
    core: Arc<Core>,
    io: Arc<dyn ChatIo>,
    stop: CancellationToken,
    states: Arc<Mutex<Vec<ChannelState>>>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let update = |state| states.lock().unwrap().push(state);
        let status = Status::new(&update);
        status.connected();
        core.notifications(io, "123".into(), stop, &status).await
    })
}
async fn cursor(core: &Core, route: &Route, identity: &ChannelIdentity) -> u64 {
    core.store
        .get(
            "cursors",
            &format!("{}:{}", route.key(core.kind), identity.id),
        )
        .await
        .unwrap()
        .unwrap_or(0)
}

#[tokio::test]
async fn refused_notification_advances_only_its_cursor_and_other_route_still_delivers() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    f.create().await;
    let (core, routes) = notification_core(&f).await;
    let time = FrozenTime::new();
    let mut task = f.hub.task();
    task.state = TaskState::Failed;
    let event = f.hub.event(Event::TaskUpserted { task });
    f.server
        .mock
        .inner
        .chat_replies
        .lock()
        .unwrap()
        .insert("43".into(), vec![(403, json!({})); 20].into());
    let stop = CancellationToken::new();
    let states = Arc::new(Mutex::new(vec![]));
    let worker = notification_worker(core.clone(), f.io.clone(), stop.clone(), states.clone());
    ready(|| notices(&f.server.mock, "42") == 1 && notices(&f.server.mock, "43") == 1).await;
    // Waiting for the cursor also establishes that the refusal was consumed.
    for (route, identity) in &routes {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while cursor(&core, route, identity).await < event.seq {
            assert!(std::time::Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
    }
    time.advance(&f.clock, Duration::from_secs(10)).await;
    ready(|| f.hub.events.receiver_count() == 2).await;
    assert_eq!(notices(&f.server.mock, "43"), 1);
    assert!(
        !states
            .lock()
            .unwrap()
            .contains(&ChannelState::PlatformError)
    );
    assert!(!worker.is_finished());
    stop.cancel();
    worker.await.unwrap().unwrap();
}

#[tokio::test]
async fn transient_notification_replays_from_cursor_with_per_route_backoff() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    f.create().await;
    let (core, routes) = notification_core(&f).await;
    let time = FrozenTime::new();
    let stop = CancellationToken::new();
    let states = Arc::new(Mutex::new(vec![]));
    let worker = notification_worker(core.clone(), f.io.clone(), stop.clone(), states.clone());
    ready(|| f.hub.events.receiver_count() == 2).await;
    let mut task = f.hub.task();
    task.state = TaskState::Failed;
    let event = f.hub.event(Event::TaskUpserted { task });
    f.server
        .mock
        .inner
        .chat_replies
        .lock()
        .unwrap()
        .insert("43".into(), vec![(502, json!({})); 2].into());
    f.hub
        .events
        .send(Outbound::Event(Arc::new(event.clone())))
        .unwrap();
    time.advance(&f.clock, Duration::from_secs(1)).await;
    ready(|| {
        notices(&f.server.mock, "43") == 1
            && notices(&f.server.mock, "42") == 1
            && states.lock().unwrap().last() == Some(&ChannelState::PlatformError)
    })
    .await;
    assert_eq!(cursor(&core, &routes[1].0, &routes[1].1).await, 0);
    time.advance(&f.clock, Duration::from_secs(1)).await;
    ready(|| notices(&f.server.mock, "43") == 2 && f.hub.events.receiver_count() == 1).await;
    time.advance(&f.clock, Duration::from_secs(1)).await;
    // A healthy route is processed during the other route's two-second delay.
    let mut task = f.hub.task();
    task.state = TaskState::Completed;
    f.hub
        .events
        .send(Outbound::Event(Arc::new(
            f.hub.event(Event::TaskUpserted { task }),
        )))
        .unwrap();
    time.advance(&f.clock, Duration::from_secs(1)).await;
    ready(|| {
        notices(&f.server.mock, "43") == 3
            && states.lock().unwrap().last() == Some(&ChannelState::Connected)
    })
    .await;
    assert!(cursor(&core, &routes[1].0, &routes[1].1).await >= event.seq);
    assert_eq!(notices(&f.server.mock, "42"), 1);
    let attempts: Vec<_> = f
        .server
        .mock
        .requests()
        .into_iter()
        .filter(|(p, body, _)| {
            p.ends_with("sendMessage")
                && body["chat_id"] == "43"
                && body["text"].as_str().is_some_and(|t| t.contains("failed"))
        })
        .collect();
    assert!(attempts[2].2.duration_since(attempts[1].2) >= Duration::from_secs(2));
    assert!(
        f.hub
            .subscriptions
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| *id == routes[1].1.id)
            .all(|(_, after)| *after == Some(0))
    );
    stop.cancel();
    worker.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_notification_to_a_limited_chat_waits_for_the_limit_without_holding_up_others() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    f.create().await;
    let (core, _) = notification_core(&f).await;
    let time = FrozenTime::new();
    let stop = CancellationToken::new();
    let states = Arc::new(Mutex::new(vec![]));
    let worker = notification_worker(core.clone(), f.io.clone(), stop.clone(), states.clone());
    ready(|| f.hub.events.receiver_count() == 2).await;
    // Chat 43's route is visited first, so its wait would hold up chat 42's.
    f.server.mock.inner.chat_replies.lock().unwrap().insert(
        "43".into(),
        vec![(
            429,
            json!({"ok":false,"error_code":429,"parameters":{"retry_after":2}}),
        )]
        .into(),
    );
    let mut task = f.hub.task();
    task.state = TaskState::Failed;
    f.hub
        .events
        .send(Outbound::Event(Arc::new(
            f.hub.event(Event::TaskUpserted { task }),
        )))
        .unwrap();
    time.advance(&f.clock, Duration::from_secs(1)).await;
    // The other chat is told while the limited one waits.
    ready(|| notices(&f.server.mock, "43") == 1 && notices(&f.server.mock, "42") == 1).await;
    // The loop checks its routes each second; the limit ends two seconds later.
    for _ in 0..4 {
        time.advance(&f.clock, Duration::from_secs(1)).await;
    }
    ready(|| notices(&f.server.mock, "43") == 2).await;
    let attempts: Vec<_> = f
        .server
        .mock
        .requests()
        .into_iter()
        .filter(|(p, body, _)| {
            p.ends_with("sendMessage")
                && body["chat_id"] == "43"
                && body["text"].as_str().is_some_and(|t| t.contains("failed"))
        })
        .collect();
    assert!(attempts[1].2.duration_since(attempts[0].2) >= Duration::from_secs(2));
    // Waiting for a platform's limit isn't reported as a connector error.
    assert!(
        !states
            .lock()
            .unwrap()
            .contains(&ChannelState::PlatformError)
    );
    stop.cancel();
    worker.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_discord_chat_under_a_long_limit_does_not_hold_up_another_chats_command() {
    let f = Fixture::new(ChannelKind::Discord, Exposure::Minimal).await;
    f.hub.link(ChannelKind::Discord, "7", "42");
    f.hub.link(ChannelKind::Discord, "7", "43");
    let (gateway, events, gateway_worker) = mock_gateway().await;
    let limited = "/webhooks/123/synthetic-999/messages/@original";
    let other = "/webhooks/123/synthetic-1000/messages/@original";
    // The limited chat's reply is refused for a while after its acknowledgement.
    f.server.mock.script(
        limited,
        vec![
            (200, json!({"id":"900"})),
            (429, json!({"retry_after":2.0,"global":false})),
        ],
    );
    let mut cfg = Config::default();
    cfg.discord = f.core.cfg.clone();
    cfg.discord.projects.insert("43".into(), f.hub.project());
    cfg.discord.enabled = true;
    cfg.discord.endpoints.discord_api = f.server.base.clone();
    cfg.discord.endpoints.discord_gateway = gateway;
    let handle = spawn_with_clock(
        f.hub.clone(),
        cfg,
        Arc::new(SyntheticTokens),
        f.dir.path().join("runner"),
        CancellationToken::new(),
        f.clock.clone(),
    )
    .await
    .unwrap();
    until(|| handle.status()[1].state == ChannelState::Connected).await;
    events.send(interaction("999")).await.unwrap();
    until(|| count(&f.server.mock, limited) == 2).await;
    let mut event = interaction("1000");
    event["channel_id"] = json!("43");
    events.send(event).await.unwrap();
    until(|| count(&f.server.mock, limited) == 3).await;
    let times = |path: &str| -> Vec<_> {
        f.server
            .mock
            .requests()
            .into_iter()
            .filter(|(p, _, _)| p == path)
            .map(|(_, _, at)| at)
            .collect()
    };
    let (limited, other) = (times(limited), times(other));
    // The other chat's reply went out while the limited chat waited.
    assert_eq!(other.len(), 2);
    assert!(other[1] < limited[2]);
    assert!(limited[2].duration_since(limited[1]) >= Duration::from_secs(2));
    assert_eq!(handle.status()[1].state, ChannelState::Connected);
    handle.shutdown().await;
    gateway_worker.abort();
}

#[tokio::test]
async fn a_link_reply_still_failing_when_its_code_expires_stops_reporting_an_error() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    f.handle(f.input("a", "hello")).await;
    f.server
        .mock
        .inner
        .chat_replies
        .lock()
        .unwrap()
        .insert("42".into(), vec![(502, json!({})); 100].into());
    f.hub
        .link_outcomes
        .lock()
        .unwrap()
        .insert("TEST42".into(), ChannelLinkOutcome::Approved);
    let stop = CancellationToken::new();
    let states = Arc::new(Mutex::new(vec![]));
    let worker = notification_worker(f.core.clone(), f.io.clone(), stop.clone(), states.clone());
    until(|| states.lock().unwrap().last() == Some(&ChannelState::PlatformError)).await;
    // The code runs out, and the computer forgets it, before the reply gets through.
    f.hub.link_outcomes.lock().unwrap().remove("TEST42");
    f.clock
        .advance(Duration::from_secs(services::api::LINK_CODE_MINUTES * 60));
    until(|| states.lock().unwrap().last() == Some(&ChannelState::Connected)).await;
    stop.cancel();
    worker.await.unwrap().unwrap();
}

#[tokio::test]
async fn gap_and_snapshot_repair_failures_are_isolated() {
    // Each row fails a different part of attachment: gap notice, approval
    // repair, or snapshot query. Refusals consume the gap; a 502 retries it.
    for (gap_code, approval_code, snapshot_refusal) in [
        (403, 200, false),
        (200, 403, false),
        (200, 200, true),
        (502, 200, false),
    ] {
        let f = Fixture::new(ChannelKind::Telegram, Exposure::Titles).await;
        let identity = f.create().await;
        let approval = f.hub.approval(ApprovalKind::Command);
        let event = f.hub.event(Event::ApprovalUpserted { approval });
        f.hub.gap.store(true, Ordering::SeqCst);
        let route = f.route("7");
        f.core
            .store
            .put(
                "cursors",
                &format!("{}:{}", route.key(f.kind), identity.id),
                &0_u64,
            )
            .await
            .unwrap();
        f.server.mock.script(
            "/bot123:synthetic/sendMessage",
            vec![
                (gap_code, json!({"ok":true,"result":{"message_id":900}})),
                (
                    approval_code,
                    json!({"ok":true,"result":{"message_id":901}}),
                ),
            ],
        );
        if snapshot_refusal {
            f.hub
                .snapshot_errors
                .lock()
                .unwrap()
                .push_back(ErrorCode::PermissionDenied);
        }
        let time = FrozenTime::new();
        let stop = CancellationToken::new();
        let states = Arc::new(Mutex::new(vec![]));
        let worker =
            notification_worker(f.core.clone(), f.io.clone(), stop.clone(), states.clone());
        if gap_code == 502 {
            ready(|| states.lock().unwrap().last() == Some(&ChannelState::PlatformError)).await;
            assert_eq!(cursor(&f.core, &route, &identity).await, 0);
            time.advance(&f.clock, Duration::from_secs(1)).await;
        }
        ready(|| !f.hub.subscriptions.lock().unwrap().is_empty()).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while cursor(&f.core, &route, &identity).await != event.seq {
            assert!(std::time::Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
        ready(|| states.lock().unwrap().last() == Some(&ChannelState::Connected)).await;
        let attempts = count(&f.server.mock, "sendMessage");
        time.advance(&f.clock, Duration::from_secs(10)).await;
        assert_eq!(count(&f.server.mock, "sendMessage"), attempts);
        assert!(!worker.is_finished());
        if gap_code != 502 {
            assert!(
                !states
                    .lock()
                    .unwrap()
                    .contains(&ChannelState::PlatformError)
            );
        }
        stop.cancel();
        worker.await.unwrap().unwrap();
    }
}

async fn mock_gateway() -> (
    String,
    tokio::sync::mpsc::Sender<Value>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}", listener.local_addr().unwrap());
    let (events, mut rx) = tokio::sync::mpsc::channel::<Value>(8);
    let worker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Ws::Text(
                json!({"op":10,"d":{"heartbeat_interval":120000}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let _identify = socket.next().await.unwrap().unwrap();
        socket
            .send(Ws::Text(
                json!({"op":0,"s":1,"t":"READY","d":{"session_id":"synthetic"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let mut seq = 1;
        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    seq += 1;
                    socket.send(Ws::Text(json!({"op":0,"s":seq,"t":"INTERACTION_CREATE","d":event}).to_string().into())).await.unwrap();
                }
                frame = socket.next() => {
                    let Some(Ok(frame)) = frame else { break; };
                    if let Ws::Text(text) = frame {
                        let value: Value = serde_json::from_str(&text).unwrap();
                        if value["op"] == 1 {
                            socket.send(Ws::Text(json!({"op":11,"d":null}).to_string().into())).await.unwrap();
                        }
                    }
                }
            }
        }
    });
    (base, events, worker)
}
fn interaction(id: &str) -> Value {
    json!({"id":id,"application_id":"123","token":format!("synthetic-{id}"),"type":2,"channel_id":"42","user":{"id":"7"},"data":{"name":"new","options":[{"name":"prompt","value":"Synthetic request"}]}})
}

#[tokio::test]
async fn discord_refused_ack_or_reply_does_not_poison_following_handler() {
    for (ack_failure, code) in [(true, 403), (false, 403), (false, 502)] {
        let f = Fixture::new(ChannelKind::Discord, Exposure::Minimal).await;
        f.hub.link(ChannelKind::Discord, "7", "42");
        let (gateway, events, gateway_worker) = mock_gateway().await;
        let time = FrozenTime::new();
        let path = if ack_failure {
            "/interactions/999/synthetic-999/callback"
        } else {
            "/webhooks/123/synthetic-999/messages/@original"
        };
        let replies = if ack_failure {
            vec![(code, json!({}))]
        } else {
            // The first GET recovers the acknowledged response; then PATCH fails.
            vec![(200, json!({"id":"900"})), (code, json!({}))]
        };
        f.server.mock.script(path, replies);
        let mut cfg = Config::default();
        cfg.discord = f.core.cfg.clone();
        cfg.discord.enabled = true;
        cfg.discord.endpoints.discord_api = f.server.base.clone();
        cfg.discord.endpoints.discord_gateway = gateway;
        let handle = spawn_with_clock(
            f.hub.clone(),
            cfg,
            Arc::new(SyntheticTokens),
            f.dir.path().join("runner"),
            CancellationToken::new(),
            f.clock.clone(),
        )
        .await
        .unwrap();
        ready(|| handle.status()[1].state == ChannelState::Connected).await;
        events.send(interaction("999")).await.unwrap();
        ready(|| count(&f.server.mock, path) >= if ack_failure { 1 } else { 2 }).await;
        if code == 502 {
            ready(|| handle.status()[1].state == ChannelState::PlatformError).await;
            time.advance(&f.clock, Duration::from_secs(1)).await;
            ready(|| handle.status()[1].state == ChannelState::Connected).await;
            assert_eq!(
                count(&f.server.mock, "/interactions/999/synthetic-999/callback"),
                1
            );
        }
        events.send(interaction("1000")).await.unwrap();
        ready(|| {
            count(
                &f.server.mock,
                "/webhooks/123/synthetic-1000/messages/@original",
            ) == 2
        })
        .await;
        ready(|| handle.status()[1].state == ChannelState::Connected).await;
        assert_eq!(
            f.hub.commands.lock().unwrap().len(),
            if ack_failure { 1 } else { 2 }
        );
        assert_eq!(
            count(&f.server.mock, "users/@me"),
            1,
            "handler failure must not restart the gateway"
        );
        handle.shutdown().await;
        gateway_worker.abort();
    }
}

#[tokio::test]
async fn shutdown_is_success_even_during_preflight() {
    for kind in [ChannelKind::Telegram, ChannelKind::Discord] {
        let f = Fixture::new(kind, Exposure::Minimal).await;
        let stop = CancellationToken::new();
        stop.cancel();
        let endpoints = Endpoints {
            telegram_api: f.server.base.clone(),
            discord_api: f.server.base.clone(),
            ..Endpoints::default()
        };
        let token = Arc::new(BotToken::new("123:synthetic".into()).unwrap());
        let result = match kind {
            ChannelKind::Telegram => telegram::run(f.core, endpoints, token, stop, &|_| {}).await,
            ChannelKind::Discord => discord::run(f.core, endpoints, token, stop, &|_| {}).await,
        };
        result.unwrap();
        assert!(f.server.mock.requests().is_empty());
    }
}

#[tokio::test]
async fn corrupt_channel_store_is_not_converted_to_a_chat_reply() {
    let f = Fixture::new(ChannelKind::Telegram, Exposure::Minimal).await;
    f.hub.link(f.kind, "7", "42");
    let mut input = f.input("synthetic-event", "/new Synthetic request");
    let id = command_id(f.kind, &input.route.bot, &input.event);
    f.core
        .store
        .put("commands", &id.to_string(), &false)
        .await
        .unwrap();
    assert!(matches!(
        f.core.handle(f.io.as_ref(), &mut input).await,
        Err(Error::State)
    ));
    assert_eq!(count(&f.server.mock, "sendMessage"), 1);
    assert_eq!(count(&f.server.mock, "editMessageText"), 0);
    assert!(f.hub.submitted.lock().unwrap().is_empty());
}
