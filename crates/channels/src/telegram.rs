// SPDX-License-Identifier: Apache-2.0
use crate::{
    BotToken, ChannelState, Endpoints, Error, Result,
    core::{ChatIo, Core, Incoming, Message, Route, escape_html, person_name, split_message},
    http::{Http, Pace, backoff},
    recovery::{Failure, Scope, Status, skip_refused},
};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(crate) struct Telegram {
    http: Http,
}
impl Telegram {
    pub(crate) fn new(http: Http) -> Self {
        Self { http }
    }
    /// Telegram limits a bot per chat, so a chat's calls wait only for that
    /// chat's limit. Calls about the bot itself have no chat.
    async fn call(
        &self,
        method: &str,
        chat: Option<&str>,
        pace: Pace,
        body: Value,
    ) -> Result<Value> {
        let route = match chat {
            Some(chat) => format!("{method}:{chat}"),
            None => method.to_owned(),
        };
        self.http
            .request(Method::POST, method, &route, Some(body), pace)
            .await
    }
    async fn deliver(&self, chat: &str, id: Option<&str>, message: Message) -> Result<String> {
        let chunks = split_message(&message.text, 4096);
        let mut first = None;
        for (index, chunk) in chunks.iter().enumerate() {
            let mut body = json!({"chat_id":chat,"text":escape_html(chunk),"parse_mode":"HTML","link_preview_options":{"is_disabled":true}});
            if let Some(thread) = &message.thread {
                body["message_thread_id"] = json!(thread);
            }
            // Put controls on the first message, whose ID is persisted. Effects
            // are not truncated and all chunks must send before state advances.
            body["reply_markup"] = json!({"inline_keyboard": if index==0 {vec![message.buttons.iter().map(|b|json!({"text":b.label,"callback_data":b.data})).collect::<Vec<_>>()]}else{Vec::new()}});
            // The rest of a message already partly posted follows it rather
            // than give way, so a retry never posts its first part twice.
            let pace = if index == 0 { Pace::Yield } else { Pace::Wait };
            let result = if index == 0 && id.is_some() {
                body["message_id"] = json!(id);
                self.call("editMessageText", Some(chat), pace, body).await?
            } else {
                self.call("sendMessage", Some(chat), pace, body).await?
            };
            if index == 0 {
                first = Some(
                    id.map(str::to_owned)
                        .or_else(|| result["message_id"].as_i64().map(|id| id.to_string()))
                        .ok_or(Error::PlatformProtocol)?,
                );
            }
        }
        first.ok_or(Error::PlatformProtocol)
    }
}
#[async_trait]
impl ChatIo for Telegram {
    async fn send(&self, chat: &str, message: Message) -> Result<String> {
        self.deliver(chat, None, message).await
    }
    async fn edit(&self, chat: &str, id: &str, message: Message) -> Result<()> {
        self.deliver(chat, Some(id), message).await.map(|_| ())
    }
}
pub(crate) async fn run(
    core: Arc<Core>,
    endpoints: Endpoints,
    token: Arc<BotToken>,
    stop: CancellationToken,
    status: &(dyn Fn(ChannelState) + Send + Sync),
) -> Result<()> {
    match run_inner(core, endpoints, token, stop, status).await {
        Err(Error::Shutdown) => Ok(()),
        result => result,
    }
}
async fn run_inner(
    core: Arc<Core>,
    endpoints: Endpoints,
    token: Arc<BotToken>,
    stop: CancellationToken,
    status: &(dyn Fn(ChannelState) + Send + Sync),
) -> Result<()> {
    let io = Arc::new(Telegram::new(Http::new(
        endpoints.telegram_api,
        token,
        true,
        stop.clone(),
    )?));
    let status = Status::new(status);
    let me = io.call("getMe", None, Pace::Wait, json!({})).await?;
    let bot = me["id"]
        .as_i64()
        .ok_or(Error::PlatformProtocol)?
        .to_string();
    let username = me["username"].as_str().unwrap_or_default().to_owned();
    // Read-only preflight. Never delete or replace an existing webhook.
    let webhook = io
        .call("getWebhookInfo", None, Pace::Wait, json!({}))
        .await?;
    if webhook["url"].as_str().is_some_and(|s| !s.is_empty()) {
        return Err(Error::Conflict);
    }
    let notifications = core
        .clone()
        .notifications(io.clone(), bot.clone(), stop.clone(), &status);
    let polling = poll(core.clone(), io, bot, username, stop.clone(), &status);
    tokio::select! {_=stop.cancelled()=>Ok(()),result=notifications=>result,result=polling=>result}
}
pub(crate) async fn poll(
    core: Arc<Core>,
    io: Arc<Telegram>,
    bot: String,
    username: String,
    stop: CancellationToken,
    status: &Status<'_>,
) -> Result<()> {
    let offset_key = format!("telegram:{bot}");
    let mut offset = core
        .store
        .get::<i64>("offsets", &offset_key)
        .await?
        .unwrap_or(0);
    let mut delay = 1;
    let mut pending: Option<Incoming> = None;
    // Updates from a chat the platform is limiting wait here, with when to try
    // them again, while other chats' updates go ahead. The offset stays at the
    // first one waiting, so updates after it that were handled are remembered.
    let mut waiting: BTreeMap<i64, (Incoming, Instant)> = BTreeMap::new();
    let mut handled: BTreeSet<i64> = BTreeSet::new();
    'poll: loop {
        // Unconfirmed updates return at once, so don't long-poll while one waits.
        let timeout = if waiting.is_empty() { 30 } else { 0 };
        let updates = match io.call("getUpdates", None, Pace::Wait, json!({"offset":offset,"timeout":timeout,"limit":100,"allowed_updates":["message","callback_query"]})).await {
            Ok(updates) => updates,
            Err(error) => match error.failure(Scope::Channel) {
                Failure::Fatal => return Err(error),
                Failure::Shutdown => return Ok(()),
                _ => {
                    status.retrying("transport");
                    backoff(&stop, delay).await?;
                    delay = (delay * 2).min(60);
                    continue;
                }
            },
        };
        for update in updates.as_array().ok_or(Error::PlatformProtocol)? {
            let Some(id) = update["update_id"].as_i64().filter(|id| *id < i64::MAX) else {
                skip_refused(Err(Error::PlatformProtocol))?;
                continue;
            };
            if id < offset {
                continue;
            }
            if !handled.contains(&id) {
                let input = match waiting.remove(&id) {
                    Some((input, at)) if at > Instant::now() => {
                        waiting.insert(id, (input, at));
                        continue;
                    }
                    Some((input, _)) => Some(input),
                    None => pending
                        .take()
                        .filter(|input| input.event == format!("update:{id}"))
                        .or_else(|| decode(update, &bot, &username)),
                };
                if let Some(mut input) = input {
                    // A chat's updates keep their order behind one that waits.
                    if let Some(at) = waiting
                        .range(..id)
                        .find(|(_, (earlier, _))| earlier.route.chat == input.route.chat)
                        .map(|(_, (_, at))| *at)
                    {
                        waiting.insert(id, (input, at));
                        continue;
                    }
                    if !input.rate_checked
                        && let Some(callback) = update["callback_query"]["id"].as_str()
                        && let Err(error) = io
                            .call(
                                "answerCallbackQuery",
                                Some(&input.route.chat),
                                Pace::Yield,
                                json!({"callback_query_id":callback,"text":"Received"}),
                            )
                            .await
                    {
                        if matches!(error, Error::Shutdown) {
                            return Ok(());
                        }
                        tracing::warn!(error = %error, "callback acknowledgement failed");
                    }
                    match skip_refused(core.handle(io.as_ref(), &mut input).await) {
                        Ok(()) => {}
                        Err(Error::RateLimited(wait)) => {
                            waiting.insert(id, (input, Instant::now() + wait));
                            continue;
                        }
                        Err(error) => match error.failure(Scope::Item) {
                            Failure::Transient => {
                                pending = Some(input);
                                status.retrying("transport");
                                backoff(&stop, delay).await?;
                                delay = (delay * 2).min(60);
                                continue 'poll;
                            }
                            Failure::Shutdown => return Ok(()),
                            _ => return Err(error),
                        },
                    }
                }
                handled.insert(id);
            }
            // The offset passes an update once it and every one before it are handled.
            if waiting.keys().next().is_none_or(|first| *first > id) {
                offset = id + 1;
                handled.retain(|later| *later >= offset);
                core.store.put("offsets", &offset_key, &offset).await?;
            }
            delay = 1;
            status.connected();
        }
        if pending.is_none() {
            delay = 1;
            status.connected();
        }
        // Real long polling waits remotely; avoid busy loops on empty proxies.
        // While an update waits, look for other chats' updates each second.
        let pause = waiting
            .values()
            .map(|(_, at)| at.saturating_duration_since(Instant::now()))
            .min()
            .map_or(Duration::from_millis(50), |wait| {
                wait.clamp(Duration::from_millis(50), Duration::from_secs(1))
            });
        tokio::select! {_=stop.cancelled()=>return Ok(()),_=tokio::time::sleep(pause)=>{}}
    }
}

/// "First Last", or "@username" when the account has no name to show.
fn sender_name(user: &Value) -> Option<String> {
    let full: Vec<&str> = [&user["first_name"], &user["last_name"]]
        .into_iter()
        .filter_map(Value::as_str)
        .collect();
    person_name(&full.join(" ")).or_else(|| {
        user["username"]
            .as_str()
            .and_then(|u| person_name(&format!("@{u}")))
    })
}
/// A group command names the bot it is for, as `/new@name`. One for another
/// bot isn't for this one; this bot's own name is dropped from its command.
fn addressed(text: &str, username: &str) -> Option<String> {
    let Some(command) = text.strip_prefix('/') else {
        return Some(text.to_owned());
    };
    let end = command.find(char::is_whitespace).unwrap_or(command.len());
    match command[..end].split_once('@') {
        None => Some(text.to_owned()),
        // Telegram usernames are not case-sensitive.
        Some((verb, name)) if name.eq_ignore_ascii_case(username) => {
            Some(format!("/{verb}{}", &command[end..]))
        }
        Some(_) => None,
    }
}
pub(crate) fn decode(update: &Value, bot: &str, username: &str) -> Option<Incoming> {
    let callback = update.get("callback_query");
    let message = callback
        .map(|c| &c["message"])
        .unwrap_or(&update["message"]);
    let user = callback.map(|c| &c["from"]).unwrap_or(&message["from"]);
    if user["is_bot"].as_bool() == Some(true) {
        return None;
    }
    // Anonymous administrators / channel posts do not identify an individual.
    if message.get("sender_chat").is_some() && callback.is_none() {
        return None;
    }
    let chat = message["chat"]["id"].as_i64()?.to_string();
    // A forwarded message repeats what someone wrote, perhaps elsewhere. Its
    // text is ordinary text, and its buttons are not pressed here.
    let forwarded =
        message.get("forward_origin").is_some() || message.get("forward_date").is_some();
    if forwarded && callback.is_some() {
        return None;
    }
    let action = callback.and_then(|c| c["data"].as_str()).map(str::to_owned);
    let text = if callback.is_some() {
        String::new()
    } else if forwarded {
        message["text"].as_str()?.to_owned()
    } else {
        addressed(message["text"].as_str()?, username)?
    };
    Some(Incoming {
        route: Route {
            bot: bot.into(),
            user: user["id"].as_i64()?.to_string(),
            chat,
        },
        name: sender_name(user),
        event: format!("update:{}", update["update_id"].as_i64()?),
        text,
        reply: message["reply_to_message"]["message_id"]
            .as_i64()
            .map(|id| id.to_string()),
        thread: message["message_thread_id"]
            .as_i64()
            .map(|id| id.to_string()),
        action,
        explicit_answer: false,
        forwarded,
        acknowledgement: None,
        rate_checked: false,
    })
}
