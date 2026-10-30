// SPDX-License-Identifier: Apache-2.0
use crate::{
    BotToken, ChannelState, Endpoints, Error, Result,
    config::validate_endpoint,
    core::{ChatIo, Core, Incoming, Message, Route, person_name},
    http::{Http, Pace, backoff},
    recovery::{Failure, Scope, Status, skip_refused},
};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    sync::{Mutex, mpsc},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message as WsMessage};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub(crate) struct Discord {
    http: Http,
    interactions: Mutex<HashMap<String, (String, String, tokio::time::Instant)>>,
}
impl Discord {
    pub(crate) fn new(http: Http) -> Self {
        Self {
            http,
            interactions: Mutex::new(HashMap::new()),
        }
    }
    async fn deliver(&self, chat: &str, id: Option<&str>, message: Message) -> Result<String> {
        let chunks = discord_chunks(&message.text);
        let mut first = None;
        for (index, chunk) in chunks.iter().enumerate() {
            let body = json!({"content":chunk,"allowed_mentions":{"parse":[]},"flags":4,"components":if index==0 && !message.buttons.is_empty() {vec![json!({"type":1,"components":message.buttons.iter().map(|b|json!({"type":2,"style":2,"label":b.label,"custom_id":b.data})).collect::<Vec<_>>()})]}else{Vec::new()}});
            let interaction = if let Some(id) = id {
                let mut map = self.interactions.lock().await;
                map.retain(|_, (_, _, at)| at.elapsed() < Duration::from_secs(14 * 60));
                map.get(id).cloned()
            } else {
                None
            };
            let (method, path) = if index == 0 && id.is_some() {
                (
                    Method::PATCH,
                    match interaction {
                        Some((app, token, _)) => {
                            format!("webhooks/{app}/{token}/messages/@original")
                        }
                        None => format!("channels/{chat}/messages/{}", id.unwrap_or_default()),
                    },
                )
            } else {
                (Method::POST, format!("channels/{chat}/messages"))
            };
            // The rest of a message already partly posted follows it rather
            // than give way, so a retry never posts its first part twice.
            let pace = if index == 0 { Pace::Yield } else { Pace::Wait };
            let value = self
                .http
                .request(method, &path, &format!("messages:{chat}"), Some(body), pace)
                .await?;
            if index == 0 {
                first = Some(
                    id.map(str::to_owned)
                        .or_else(|| value["id"].as_str().map(str::to_owned))
                        .ok_or(Error::PlatformProtocol)?,
                );
            }
        }
        first.ok_or(Error::PlatformProtocol)
    }
    pub(crate) async fn acknowledge(&self, event: &Value) -> Result<String> {
        let id = event["id"].as_str().ok_or(Error::PlatformProtocol)?;
        let token = event["token"]
            .as_str()
            .filter(|s| valid_segment(s))
            .ok_or(Error::PlatformProtocol)?;
        let app = event["application_id"]
            .as_str()
            .filter(|s| snowflake(s))
            .ok_or(Error::PlatformProtocol)?;
        let chat = event["channel_id"]
            .as_str()
            .filter(|s| snowflake(s))
            .ok_or(Error::PlatformProtocol)?;
        if !snowflake(id) {
            return Err(Error::PlatformProtocol);
        }
        let acknowledgement = self
            .http
            .request(
                Method::POST,
                &format!("interactions/{id}/{token}/callback"),
                // Kept per chat, like messages, so one chat's limit holds up only it.
                &format!("interaction:{chat}"),
                Some(
                    json!({"type":4,"data":{"content":"Received","allowed_mentions":{"parse":[]}}}),
                ),
                Pace::Yield,
            )
            .await;
        // Replayed interactions may already have an initial response. Recover
        // it through the authenticated original-response route before admission.
        if let Err(error) = acknowledgement
            && !matches!(error, Error::Http(400))
        {
            return Err(error);
        }
        let message = self
            .http
            .request(
                Method::GET,
                &format!("webhooks/{app}/{token}/messages/@original"),
                &format!("interaction-original:{chat}"),
                None,
                Pace::Yield,
            )
            .await?;
        let id = message["id"]
            .as_str()
            .ok_or(Error::PlatformProtocol)?
            .to_owned();
        self.interactions.lock().await.insert(
            id.clone(),
            (app.into(), token.into(), tokio::time::Instant::now()),
        );
        Ok(id)
    }
}
#[async_trait]
impl ChatIo for Discord {
    async fn send(&self, chat: &str, message: Message) -> Result<String> {
        self.deliver(chat, None, message).await
    }
    async fn edit(&self, chat: &str, id: &str, message: Message) -> Result<()> {
        self.deliver(chat, Some(id), message).await.map(|_| ())
    }
}
fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 512
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn snowflake(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit())
}
/// Prevent mentions, links, markdown formatting and embeds from turning untrusted
/// harness titles/effect descriptions into active platform content.
pub(crate) fn discord_text(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\`*_~|>[]()#".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
fn discord_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut units = 0;
    for c in text.chars() {
        let escaped = discord_text(&c.to_string());
        let length = escaped.encode_utf16().count();
        if units + length > 2000 {
            chunks.push(std::mem::take(&mut chunk));
            units = 0;
        }
        chunk.push_str(&escaped);
        units += length;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    if chunks.is_empty() {
        chunks.push(" ".into());
    }
    chunks
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
    let status = Status::new(status);
    let io = Arc::new(Discord::new(Http::new(
        endpoints.discord_api,
        token,
        false,
        stop.clone(),
    )?));
    let me = io
        .http
        .request(Method::GET, "users/@me", "me", None, Pace::Wait)
        .await?;
    let bot = me["id"]
        .as_str()
        .filter(|s| snowflake(s))
        .ok_or(Error::PlatformProtocol)?
        .to_owned();
    let app = io
        .http
        .request(
            Method::GET,
            "oauth2/applications/@me",
            "application",
            None,
            Pace::Wait,
        )
        .await?;
    let app = app["id"]
        .as_str()
        .filter(|s| snowflake(s))
        .ok_or(Error::PlatformProtocol)?;
    register(io.as_ref(), app).await?;
    let gateway_info = io
        .http
        .request(Method::GET, "gateway/bot", "gateway", None, Pace::Wait)
        .await?;
    if gateway_info["session_start_limit"]["remaining"].as_u64() == Some(0) {
        return Err(Error::Http(429));
    }
    let (tx, mut rx) = mpsc::channel::<Value>(128);
    let processor = {
        let core = core.clone();
        let io = io.clone();
        let bot = bot.clone();
        let stop = stop.clone();
        let status = &status;
        async move {
            let mut handlers = futures_util::stream::FuturesUnordered::new();
            loop {
                tokio::select! {
                    Some(result)=handlers.next(), if !handlers.is_empty()=>{ result?; },
                    event=rx.recv(), if handlers.len()<32=>{
                        let Some(event)=event else {break;};
                        if let Some(mut input)=decode(&event,&bot) {
                            let io=io.clone(); let core=core.clone(); let stop=stop.clone();
                            handlers.push(async move {
                                let key = format!("handler:{}", input.event);
                                let mut delay = 1;
                                loop {
                                    let result = async {
                                        if input.acknowledgement.is_none() && event["_interaction"].as_bool()==Some(true) {
                                            input.acknowledgement=Some(io.acknowledge(&event).await?);
                                        }
                                        core.handle(io.as_ref(), &mut input).await
                                    }.await;
                                    match skip_refused(result) {
                                        Ok(()) => { status.recovered(&key); return Ok(()); }
                                        // Only this chat waits out its limit; other handlers go ahead.
                                        Err(Error::RateLimited(wait)) => {
                                            tokio::select! {_=stop.cancelled()=>return Err(Error::Shutdown),_=tokio::time::sleep(wait)=>{}}
                                        }
                                        Err(error) if error.failure(Scope::Item) == Failure::Transient => {
                                            status.retrying(&key);
                                            backoff(&stop, delay).await?;
                                            delay = (delay * 2).min(60);
                                        }
                                        Err(error) => return Err(error),
                                    }
                                }
                            });
                        }
                    },
                }
            }
            Ok(())
        }
    };
    let notifications = core
        .clone()
        .notifications(io.clone(), bot, stop.clone(), &status);
    let gateway = gateway(io, endpoints.discord_gateway, tx, stop.clone(), &status);
    tokio::select! {_=stop.cancelled()=>Ok(()),r=processor=>r,r=notifications=>r,r=gateway=>r}
}
async fn register(io: &Discord, app: &str) -> Result<()> {
    for (name, description, options) in [
        (
            "new",
            "Start a task in the mapped project",
            vec![
                json!({"type":3,"name":"prompt","description":"What should the agent work on?","required":true,"max_length":4000}),
            ],
        ),
        ("tasks", "List tasks in the mapped project", vec![]),
        ("status", "Show active tasks and attention", vec![]),
        (
            "stop",
            "Request confirmation to stop a task",
            vec![json!({"type":3,"name":"task","description":"Task UUID","required":true})],
        ),
        (
            "steer",
            "Steer a task during its current turn",
            task_options(),
        ),
        ("reply", "Send a message for the next turn", task_options()),
        (
            "answer",
            "Answer an agent question",
            vec![
                json!({"type":3,"name":"message","description":"Question message ID (Copy Message ID)","required":true}),
                json!({"type":3,"name":"text","description":"Your answer","required":true,"max_length":4000}),
            ],
        ),
    ] {
        io.http.request(Method::POST,&format!("applications/{app}/commands"),"commands",Some(json!({"name":name,"description":description,"type":1,"options":options,"dm_permission":true})),Pace::Wait).await?;
    }
    Ok(())
}
fn task_options() -> Vec<Value> {
    vec![
        json!({"type":3,"name":"task","description":"Task UUID","required":true}),
        json!({"type":3,"name":"text","description":"Message to the agent","required":true,"max_length":4000}),
    ]
}

pub(crate) fn decode(event: &Value, bot: &str) -> Option<Incoming> {
    let interaction = event["_interaction"].as_bool() == Some(true);
    let sender = if interaction {
        event
            .get("member")
            .map(|m| &m["user"])
            .unwrap_or(&event["user"])
    } else {
        &event["author"]
    };
    if sender["bot"].as_bool() == Some(true) {
        return None;
    }
    let user = sender["id"].as_str().filter(|s| snowflake(s))?.to_owned();
    // The display name when set, otherwise the account name.
    let name = sender["global_name"]
        .as_str()
        .and_then(person_name)
        .or_else(|| sender["username"].as_str().and_then(person_name));
    let chat = event["channel_id"]
        .as_str()
        .filter(|s| snowflake(s))?
        .to_owned();
    let id = event["id"].as_str().filter(|s| snowflake(s))?;
    let mut action = None;
    let mut reply = event["message_reference"]["message_id"]
        .as_str()
        .map(str::to_owned);
    let text = if interaction {
        match event["type"].as_u64()? {
            3 => {
                action = Some(event["data"]["custom_id"].as_str()?.to_owned());
                String::new()
            }
            2 => {
                let data = &event["data"];
                let name = data["name"].as_str()?;
                let opt = |name: &str| -> &str {
                    data["options"]
                        .as_array()
                        .and_then(|o| o.iter().find(|o| o["name"] == name))
                        .and_then(|o| o["value"].as_str())
                        .unwrap_or("")
                };
                match name {
                    "answer" => {
                        reply = Some(opt("message").into());
                        opt("text").into()
                    }
                    "new" => format!("/new {}", opt("prompt")),
                    "status" | "tasks" => format!("/{name}"),
                    "stop" => format!("/stop {}", opt("task")),
                    "steer" | "reply" => format!("/{name} {} {}", opt("task"), opt("text")),
                    _ => return None,
                }
            }
            _ => return None,
        }
    } else {
        // Only DIRECT_MESSAGES is requested. Guild MESSAGE_CREATE content is
        // deliberately ignored even if another installation enabled that intent.
        if event.get("guild_id").is_some() {
            return None;
        }
        event["content"].as_str()?.to_owned()
    };
    Some(Incoming {
        route: Route {
            bot: bot.into(),
            user,
            chat,
        },
        name,
        event: format!(
            "{}:{id}",
            if interaction {
                "interaction"
            } else {
                "message"
            }
        ),
        text,
        reply,
        thread: None,
        action,
        explicit_answer: interaction && event["data"]["name"] == "answer",
        forwarded: false,
        acknowledgement: None,
        rate_checked: false,
    })
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
#[derive(Default)]
struct Session {
    id: Option<String>,
    sequence: Option<u64>,
    resume: Option<String>,
}
async fn gateway(
    io: Arc<Discord>,
    base: String,
    tx: mpsc::Sender<Value>,
    stop: CancellationToken,
    status: &Status<'_>,
) -> Result<()> {
    let mut session = Session::default();
    let mut delay = 1;
    let mut last_identify: Option<tokio::time::Instant> = None;
    loop {
        if session.id.is_none() {
            if let Some(last) = last_identify {
                tokio::select! { _=stop.cancelled()=>return Ok(()),_=tokio::time::sleep_until(last+Duration::from_secs(5))=>{} }
            }
            last_identify = Some(tokio::time::Instant::now());
        }
        let endpoint = session.resume.as_deref().unwrap_or(&base);
        let connect = tokio::time::timeout(
            Duration::from_secs(15),
            tokio_tungstenite::connect_async(format!("{endpoint}/?v=10&encoding=json")),
        );
        let connection = tokio::select! {_=stop.cancelled()=>return Ok(()),c=connect=>c};
        let result = match connection {
            Ok(Ok((mut socket, _))) => {
                connected(&mut socket, &io, &base, &mut session, &tx, &stop, status).await
            }
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
                Err(Error::Http(response.status().as_u16()))
            }
            _ => Err(Error::Connection),
        };
        match result {
            Ok(()) => delay = 1,
            Err(error) => match error.failure(Scope::Channel) {
                Failure::Shutdown => return Ok(()),
                Failure::Fatal => return Err(error),
                _ => {}
            },
        }
        status.retrying("transport");
        backoff(&stop, delay).await?;
        delay = (delay * 2).min(60);
    }
}
async fn send(socket: &mut Socket, value: Value) -> Result<()> {
    socket
        .send(WsMessage::Text(value.to_string().into()))
        .await
        .map_err(|_| Error::Connection)
}
#[derive(Serialize)]
struct GatewayAuth<'a> {
    op: u8,
    d: GatewayAuthData<'a>,
}
#[derive(Serialize)]
#[serde(untagged)]
enum GatewayAuthData<'a> {
    Resume {
        token: &'a str,
        session_id: &'a str,
        seq: u64,
    },
    Identify {
        token: &'a str,
        intents: u32,
        properties: GatewayProperties,
    },
}
#[derive(Serialize)]
struct GatewayProperties {
    os: &'static str,
    browser: &'static str,
    device: &'static str,
}
impl<'a> GatewayAuth<'a> {
    fn new(token: &'a str, session: &'a Session) -> Self {
        if let (Some(id), Some(seq)) = (&session.id, session.sequence) {
            Self {
                op: 6,
                d: GatewayAuthData::Resume {
                    token,
                    session_id: id,
                    seq,
                },
            }
        } else {
            Self {
                op: 2,
                d: GatewayAuthData::Identify {
                    token,
                    intents: 4096,
                    properties: GatewayProperties {
                        os: std::env::consts::OS,
                        browser: ports::PROGRAM_NAME,
                        device: ports::PROGRAM_NAME,
                    },
                },
            }
        }
    }
    fn encode(&self) -> Result<Zeroizing<String>> {
        // Borrow the token during serialization; no intermediate JSON Value copy.
        let mut bytes = Zeroizing::new(Vec::with_capacity(1024));
        serde_json::to_writer(&mut *bytes, self).map_err(|_| Error::PlatformProtocol)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| Error::PlatformProtocol)?;
        Ok(Zeroizing::new(text.to_owned()))
    }
}
async fn authenticate(socket: &mut Socket, token: &str, session: &Session) -> Result<()> {
    let text = GatewayAuth::new(token, session).encode()?;
    // tungstenite owns its immutable frame and write buffers; they expose no wipe API.
    socket
        .send(WsMessage::Text(text.as_str().into()))
        .await
        .map_err(|_| Error::Connection)
}

#[allow(clippy::too_many_arguments)]
async fn connected(
    socket: &mut Socket,
    io: &Discord,
    base: &str,
    session: &mut Session,
    tx: &mpsc::Sender<Value>,
    stop: &CancellationToken,
    status: &Status<'_>,
) -> Result<()> {
    let hello = tokio::time::timeout(Duration::from_secs(15), socket.next())
        .await
        .map_err(|_| Error::Connection)?
        .ok_or(Error::Connection)?
        .map_err(|_| Error::Connection)?;
    let hello: Value = serde_json::from_str(hello.to_text().map_err(|_| Error::PlatformProtocol)?)
        .map_err(|_| Error::PlatformProtocol)?;
    if hello["op"] != 10 {
        return Err(Error::PlatformProtocol);
    }
    let interval = hello["d"]["heartbeat_interval"]
        .as_u64()
        .filter(|i| *i >= 100 && *i <= 120_000)
        .ok_or(Error::PlatformProtocol)?;
    authenticate(socket, io.http.token(), session).await?;
    let jitter = (uuid::Uuid::new_v4().as_u128() % u128::from(interval)) as u64;
    let mut beat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_millis(jitter),
        Duration::from_millis(interval),
    );
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut awaiting_ack = false;
    loop {
        tokio::select! {
            _=stop.cancelled()=>return Err(Error::Shutdown),
            _=beat.tick()=>{
                if awaiting_ack {return Err(Error::Connection);}
                send(socket,json!({"op":1,"d":session.sequence})).await?;awaiting_ack=true;
            },
            frame=socket.next()=>{
                let frame=frame.ok_or(Error::Connection)?.map_err(|_|Error::Connection)?;
                match frame {
                    WsMessage::Close(close)=>{
                        let code=close.map(|c|u16::from(c.code)).unwrap_or(1000);
                        match code {
                            4004|4010|4011|4012|4013|4014=>return Err(Error::Http(401)),
                            4007|4009=>*session=Session::default(),
                            4005=>return Err(Error::Conflict),
                            _=>{},
                        }
                        return Err(Error::Connection);
                    },
                    WsMessage::Ping(bytes)=>{socket.send(WsMessage::Pong(bytes)).await.map_err(|_|Error::Connection)?;continue;},
                    WsMessage::Text(text)=>{
                        if text.len()>1024*1024 {return Err(Error::PlatformProtocol);}
                        let value:Value=serde_json::from_str(&text).map_err(|_|Error::PlatformProtocol)?;
                        match value["op"].as_u64() {
                            Some(0)=>{
                                let mut event=value["d"].clone();
                                match value["t"].as_str() {
                                    Some("READY")=>{
                                        session.id=Some(event["session_id"].as_str().ok_or(Error::PlatformProtocol)?.into());
                                        if let Some(url)=event["resume_gateway_url"].as_str() {validate_resume(base,url)?;session.resume=Some(url.trim_end_matches('/').into());}
                                        status.connected();
                                    },
                                    Some("RESUMED")=>status.connected(),
                                    Some("INTERACTION_CREATE")|Some("MESSAGE_CREATE")=>{
                                        // A body that is not an object is not an event; skip it.
                                        if let Some(fields)=event.as_object_mut() {
                                            fields.insert("_interaction".into(),json!(value["t"]=="INTERACTION_CREATE"));
                                            // Backpressure disconnects without advancing this event's
                                            // sequence; Resume requests it again rather than dropping it.
                                            tx.try_send(event).map_err(|_|Error::Connection)?;
                                        }
                                    },_=>{},
                                }
                                session.sequence=value["s"].as_u64().or(session.sequence);
                            },
                            Some(1)=>{send(socket,json!({"op":1,"d":session.sequence})).await?;awaiting_ack=true;},
                            Some(7)=>return Ok(()),
                            Some(9)=>{if value["d"]!=true {*session=Session::default();}backoff(stop,5).await?;return Ok(());},
                            Some(11)=>awaiting_ack=false,
                            _=>{},
                        }
                    },_=>{},
                }
            }
        }
    }
}
fn validate_resume(base: &str, resume: &str) -> Result<()> {
    validate_endpoint(resume, true)?;
    let a = reqwest::Url::parse(base).map_err(|_| Error::PlatformProtocol)?;
    let b = reqwest::Url::parse(resume).map_err(|_| Error::PlatformProtocol)?;
    let same = a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default();
    let official = a.host_str() == Some("gateway.discord.gg")
        && b.scheme() == "wss"
        && b.host_str()
            .is_some_and(|h| h == "gateway.discord.gg" || h.ends_with(".discord.gg"));
    if same || official {
        Ok(())
    } else {
        Err(Error::PlatformProtocol)
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn identify_and_resume_borrow_tokens_and_serialize_into_zeroizing_buffers() {
        let token = BotToken::new("synthetic-token".into()).unwrap();
        for session in [
            Session::default(),
            Session {
                id: Some("synthetic-session".into()),
                sequence: Some(42),
                resume: None,
            },
        ] {
            let encoded: Zeroizing<String> =
                GatewayAuth::new(token.expose(), &session).encode().unwrap();
            let payload: Value = serde_json::from_str(&encoded).unwrap();
            if session.id.is_some() {
                assert_eq!(
                    payload,
                    json!({"op":6,"d":{"token":"synthetic-token","session_id":"synthetic-session","seq":42}})
                );
            } else {
                assert_eq!(
                    payload,
                    json!({"op":2,"d":{"token":"synthetic-token","intents":4096,"properties":{"os":std::env::consts::OS,"browser":"ophio","device":"ophio"}}})
                );
            }
        }
    }
}
