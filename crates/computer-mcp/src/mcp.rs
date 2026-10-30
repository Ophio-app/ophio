// SPDX-License-Identifier: Apache-2.0
use crate::{BINARY_NAME, BridgeError, PERMISSION_TOOL, framing, socket};
use ports::bridge::{
    ANSWER_TOOLS, ASK_TOOL, ASK_TOOLS, BROWSER_TOOLS, BridgeFile, BridgePointer, BridgeRequest,
    BridgeResponse, BrowserAction, GITHUB_TOOLS, GithubCall, HUMAN_TOOL, PERSON_TOOLS, PersonCall,
    QUESTIONS_TOOL, RUN_TOKEN_ENV, SOCKET_ENV, WAKE_TOOL, WORK_TOOLS, WorkCall,
};
use protocol::control::TargetPoint;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};

const MODERN: &str = "2026-07-28";
const LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const LIST_TTL_MS: u64 = 60 * 60 * 1000;

/// Run the per-run MCP process. The environment, or the bridge file when one
/// is named, is read only here; its values never enter diagnostics, argv, tool
/// schemas, or debug representations.
pub async fn run_stdio(bridge_file: Option<&Path>) -> Result<(), BridgeError> {
    let (token, path) = match bridge_file {
        Some(file) => {
            let read = std::fs::read(file);
            // The token is good for one connection; nothing else should find it.
            let _ = std::fs::remove_file(file);
            let details: BridgeFile = serde_json::from_slice(
                &read.map_err(|_| BridgeError::Environment(ports::bridge::BRIDGE_FILE_ARG))?,
            )
            .map_err(|_| BridgeError::Environment(ports::bridge::BRIDGE_FILE_ARG))?;
            (details.token, PathBuf::from(details.socket))
        }
        None => (
            std::env::var(RUN_TOKEN_ENV).map_err(|_| BridgeError::Environment(RUN_TOKEN_ENV))?,
            PathBuf::from(
                std::env::var_os(SOCKET_ENV).ok_or(BridgeError::Environment(SOCKET_ENV))?,
            ),
        ),
    };
    let stream = socket::connect(&path).await?;
    let (read, mut write) = tokio::io::split(stream);
    let mut read = BufReader::new(read);
    let handshake = async {
        framing::write(&mut write, &BridgeRequest::Hello { run_token: token }).await?;
        let bytes = framing::line(&mut read)
            .await?
            .ok_or(BridgeError::Disconnected)?;
        match serde_json::from_slice::<BridgeResponse>(&bytes).map_err(|_| BridgeError::Protocol)? {
            BridgeResponse::Welcome {
                computer_use,
                browser,
                github,
                checklist_answers,
                ..
            } => Ok((computer_use, browser, github, checklist_answers)),
            _ => Err(BridgeError::Authentication),
        }
    };
    let (computer_use, browser, github, checklist_answers) =
        tokio::time::timeout(Duration::from_secs(5), handshake)
            .await
            .map_err(|_| BridgeError::Timeout)??;
    let client = Client {
        read,
        write,
        connected: true,
        computer_use,
        browser,
        github,
        checklist_answers,
        observation: None,
    };
    serve(tokio::io::stdin(), tokio::io::stdout(), client).await
}

struct Client<R, W> {
    read: BufReader<R>,
    write: W,
    connected: bool,
    computer_use: bool,
    /// The run may use the agent browser and its logins.
    browser: bool,
    /// The run may post on its task's GitHub repository.
    github: bool,
    /// The run only reads the checklist and answers on it.
    checklist_answers: bool,
    observation: Option<(u32, u32, u64)>,
}

/// Why a run may not call `name`, if it may not.
fn refusal(name: &str, computer_use: bool, browser: bool, github: bool) -> Option<&'static str> {
    let browsing = BROWSER_TOOLS.contains(&name);
    let posting = GITHUB_TOOLS.contains(&name);
    let showing = PERSON_TOOLS.contains(&name);
    let planning = WORK_TOOLS.contains(&name);
    let asking = ASK_TOOLS.contains(&name);
    if computer_use {
        return if name == PERMISSION_TOOL {
            Some("permission escalation is not available to the computer specialist")
        } else if browsing {
            Some("the agent browser is not available to the computer specialist")
        } else if posting {
            Some("GitHub is not available to the computer specialist")
        } else if showing {
            Some("showing the person things is not available to the computer specialist")
        } else if planning {
            Some("planning work is not available to the computer specialist")
        } else {
            None
        };
    }
    if name == PERMISSION_TOOL
        || showing
        || (planning && (!github || asking))
        || (browser && (browsing || name == HUMAN_TOOL))
        || (github && posting)
    {
        None
    } else if browsing {
        Some("the agent browser is not enabled for this task")
    } else if posting {
        Some("this task is not linked to GitHub")
    } else if planning {
        Some("a task working a GitHub issue doesn't plan other work")
    } else {
        Some("computer use is not enabled for this run")
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Client<R, W> {
    async fn call(&mut self, id: &Value, name: &str, args: Value) -> Value {
        if !self.connected {
            return tool_error(&BridgeError::Disconnected.to_string());
        }
        if self.checklist_answers && !ANSWER_TOOLS.contains(&name) {
            return tool_error(
                "this turn only reads the checklist and answers the person with checklist_reply",
            );
        }
        if let Some(reason) = refusal(name, self.computer_use, self.browser, self.github) {
            return tool_error(reason);
        }
        // A checklist is long; the agent is told which part is wrong.
        let kept = name.starts_with("checklist_").then(|| args.clone());
        let mut request = match tool_request(name, args, self.observation) {
            Ok(request) => request,
            Err(reason) => {
                let why = kept.and_then(|args| checklist_mistake(name, args));
                return tool_error(why.as_deref().unwrap_or(reason));
            }
        };
        if let BridgeRequest::PermissionPrompt {
            native_request_id, ..
        } = &mut request
        {
            *native_request_id = Some(id.to_string());
        }
        let permission = matches!(request, BridgeRequest::PermissionPrompt { .. });
        let observe = matches!(
            request,
            BridgeRequest::ObserveTarget { .. } | BridgeRequest::ObserveEvidence { .. }
        );
        let browsing = matches!(request, BridgeRequest::Browser { .. });
        let posting = matches!(request, BridgeRequest::Github { .. });
        let showing = matches!(request, BridgeRequest::Person { .. });
        let planning = matches!(request, BridgeRequest::Work { .. });
        let looking = matches!(
            request,
            BridgeRequest::Browser {
                action: BrowserAction::Observe
            }
        );
        let exchange = async {
            framing::write(&mut self.write, &request).await?;
            let bytes = framing::line(&mut self.read)
                .await?
                .ok_or(BridgeError::Disconnected)?;
            serde_json::from_slice::<BridgeResponse>(&bytes).map_err(|_| BridgeError::Protocol)
        };
        // Human decisions can take time, but a dead broker must never hang forever.
        let response = match tokio::time::timeout(Duration::from_secs(3600), exchange).await {
            Ok(Ok(response)) => response,
            result => {
                self.connected = false; // Never retry an action whose outcome is unknown.
                let error = match result {
                    Ok(Err(e)) => e,
                    _ => BridgeError::Timeout,
                };
                return tool_error(&error.to_string());
            }
        };
        match response {
            BridgeResponse::Observation {
                image_base64,
                media_type,
                width,
                height,
                geometry_revision,
            } if observe && width > 0 && height > 0 => {
                self.observation = Some((width, height, geometry_revision));
                json!({"content":[{"type":"image","data":image_base64,"mimeType":media_type},
                    {"type":"text","text":format!("Image size: {width} x {height}; observation_revision: {geometry_revision}. Coordinates are image pixels.")}], "isError":false})
            }
            // Not a refusal: the action wasn't taken, and the agent sees the
            // screen it acts again from.
            BridgeResponse::Resumed {
                text,
                image_base64,
                media_type,
                width,
                height,
                geometry_revision,
            } if !permission
                && !observe
                && !browsing
                && !posting
                && !showing
                && !planning
                && width > 0
                && height > 0 =>
            {
                self.observation = Some((width, height, geometry_revision));
                json!({"content":[{"type":"text","text":text},
                    {"type":"image","data":image_base64,"mimeType":media_type},
                    {"type":"text","text":format!("Image size: {width} x {height}; observation_revision: {geometry_revision}. Coordinates are image pixels.")}], "isError":false})
            }
            BridgeResponse::Permission { decision } if permission => {
                text_result(&decision.to_string())
            }
            BridgeResponse::BrowserResult {
                text,
                image_base64: Some(image),
            } if looking => {
                json!({"content":[{"type":"image","data":image,"mimeType":"image/png"},
                    {"type":"text","text":text}], "isError":false})
            }
            BridgeResponse::BrowserResult {
                text,
                image_base64: None,
            } if browsing => text_result(&text),
            BridgeResponse::GithubResult { text } if posting => text_result(&text),
            BridgeResponse::PersonResult { text } if showing => text_result(&text),
            BridgeResponse::WorkResult { text } if planning => text_result(&text),
            BridgeResponse::Done
                if !permission && !observe && !browsing && !posting && !showing && !planning =>
            {
                text_result("Done")
            }
            BridgeResponse::HumanResult { text }
                if !permission && !observe && !browsing && !posting && !showing && !planning =>
            {
                text_result(&text)
            }
            BridgeResponse::Refused { reason } => tool_error(&reason),
            BridgeResponse::Error { message } => tool_error(&message),
            _ => {
                self.connected = false;
                tool_error("unexpected bridge response; reconnect requires a new run binding")
            }
        }
    }
}

async fn serve<I, O, R, W>(
    input: I,
    mut output: O,
    mut bridge: Client<R, W>,
) -> Result<(), BridgeError>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut input = BufReader::new(input);
    let mut initialized = false;
    let mut ready = false;
    while let Some(bytes) = framing::line(&mut input).await? {
        let request: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                framing::write(&mut output, &rpc_error(Value::Null, -32700, "invalid JSON"))
                    .await?;
                continue;
            }
        };
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str);
        if request["jsonrpc"] != "2.0"
            || method.is_none()
            || id
                .as_ref()
                .is_some_and(|v| !v.is_string() && !v.is_i64() && !v.is_u64())
        {
            framing::write(
                &mut output,
                &rpc_error(Value::Null, -32600, "invalid request"),
            )
            .await?;
            continue;
        }
        let method = method.unwrap_or_default();
        let Some(id) = id else {
            if method == "notifications/initialized" && initialized {
                ready = true;
            }
            continue;
        };
        let params = &request["params"];
        let version = params["_meta"][VERSION_KEY].as_str();
        let modern = version.is_some();
        let response = if modern && version != Some(MODERN) {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32022,"message":"Unsupported protocol version","data":{"supported":[MODERN,"2025-11-25"],"requested":version}}})
        } else if modern
            && !params["_meta"]["io.modelcontextprotocol/clientCapabilities"].is_object()
        {
            rpc_error(id, -32602, "clientCapabilities metadata is required")
        } else {
            let result = match method {
                "initialize" if !initialized && !modern => {
                    match params["protocolVersion"].as_str() {
                        Some(version) => {
                            initialized = true;
                            let selected = if LEGACY.contains(&version) {
                                version
                            } else {
                                LEGACY[0]
                            };
                            Ok(
                                json!({"protocolVersion":selected,"capabilities":{"tools":{}},"serverInfo":server_info()}),
                            )
                        }
                        None => Err((-32602, "protocolVersion is required")),
                    }
                }
                "ping" => Ok(json!({})),
                "server/discover" if modern => Ok(
                    json!({"supportedVersions":[MODERN,"2025-11-25","2025-06-18","2025-03-26","2024-11-05"],"capabilities":{"tools":{}}}),
                ),
                _ if !modern && !ready => Err((
                    -32600,
                    "initialize and notifications/initialized are required",
                )),
                "tools/list" => {
                    if params.get("cursor").is_some_and(|v| !v.is_null()) {
                        Err((-32602, "unknown cursor"))
                    } else {
                        let tools = match bridge.checklist_answers {
                            true => answer_tools(),
                            false => tools(bridge.computer_use, bridge.browser, bridge.github),
                        };
                        let mut list = json!({ "tools": tools });
                        // 2026-07-28 requires cache hints on a list. This
                        // run's list never changes while it lives and belongs
                        // to this run alone.
                        if modern {
                            list["ttlMs"] = json!(LIST_TTL_MS);
                            list["cacheScope"] = json!("private");
                        }
                        Ok(list)
                    }
                }
                "tools/call" => match params["name"].as_str() {
                    Some(name) => Ok(bridge
                        .call(
                            &id,
                            name,
                            params.get("arguments").cloned().unwrap_or(json!({})),
                        )
                        .await),
                    None => Err((-32602, "tool name is required")),
                },
                _ => Err((-32601, "method not supported")),
            };
            match result {
                Ok(mut result) => {
                    if modern {
                        result["resultType"] = json!("complete");
                        result["_meta"] =
                            json!({"io.modelcontextprotocol/serverInfo":server_info()});
                    }
                    json!({"jsonrpc":"2.0","id":id,"result":result})
                }
                Err((code, text)) => rpc_error(id, code, text),
            }
        };
        framing::write(&mut output, &response).await?;
    }
    Ok(())
}

fn server_info() -> Value {
    json!({"name":BINARY_NAME,"version":env!("CARGO_PKG_VERSION")})
}
fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn text_result(text: &str) -> Value {
    json!({"content":[{"type":"text","text":text}],"isError":false})
}
fn tool_error(text: &str) -> Value {
    json!({"content":[{"type":"text","text":text}],"isError":true})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Permission {
    tool_name: String,
    input: Value,
    #[serde(default)]
    tool_use_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observe {
    #[serde(default)]
    annotation: Option<protocol::model::ArtifactAnnotation>,
    #[serde(default)]
    max_dim: Option<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    action: BridgePointer,
    x: f64,
    y: f64,
    observation_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scroll {
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
    observation_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    keys: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Text {
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Human {
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Nothing {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    url: String,
    #[serde(default)]
    new_tab: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tab {
    tab: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MaybeTab {
    #[serde(default)]
    tab: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Click {
    control: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Type {
    control: String,
    text: String,
    #[serde(default)]
    replace: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Press {
    key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageScroll {
    dy: f64,
    #[serde(default)]
    control: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    login: String,
    #[serde(default)]
    submit: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskCode {
    login: String,
    why: String,
    #[serde(default)]
    submit: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pr {
    title: String,
    body: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Comment {
    body: String,
    #[serde(default)]
    thread: Option<String>,
    #[serde(default)]
    resolve: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Issue {
    title: String,
    body: String,
    #[serde(default)]
    security: bool,
    #[serde(default)]
    distinct_from: Vec<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewIssue {
    title: String,
    body: String,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    distinct_from: Vec<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartIssue {
    number: u64,
    #[serde(default)]
    agent: Option<Agent>,
    #[serde(default)]
    start: Option<bool>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    reviewer: Option<Agent>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartReview {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    reviewer: Option<Agent>,
    #[serde(default)]
    fixer: Option<Agent>,
    #[serde(default)]
    max_issues: Option<u32>,
    #[serde(default)]
    then: Option<protocol::github::FixFindings>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Agent {
    agent: protocol::capability::HarnessKind,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ask {
    agent: Agent,
    prompt: String,
    #[serde(default)]
    title: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Merge {
    expected_head: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CannotReproduce {
    tried: String,
    #[serde(default)]
    already_fixed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Show {
    path: String,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    invite: Option<protocol::model::MediaInvite>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    url: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    questions: Vec<Question>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Question {
    question: String,
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    options: Vec<QuestionOption>,
    #[serde(default)]
    multi_select: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionOption {
    label: String,
    #[serde(default)]
    description: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invite {
    action: protocol::model::PersonAction,
    note: String,
    #[serde(default)]
    goal: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wake {
    minutes: u32,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChecklistAdd {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    theme: Option<protocol::checklist::ThemeDraft>,
    #[serde(default)]
    section: Option<String>,
    #[serde(default)]
    items: Vec<protocol::checklist::ItemDraft>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChecklistRemove {
    items: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChecklistReply {
    #[serde(default)]
    item: Option<String>,
    text: String,
}

/// The longest caption or note shown in Talk.
const MAX_NOTE: usize = 1_000;
/// The longest an agent may ask to sleep: a day.
const MAX_WAKE_MINUTES: u32 = 24 * 60;
/// The longest preview name, and the longest answer a question offers.
const MAX_LABEL: usize = 80;
/// The most questions a preview asks, and the most answers each offers.
const MAX_QUESTIONS: usize = 6;
const MAX_OPTIONS: usize = 8;
/// The longest short label over a question, like "Layout".
const MAX_HEADER: usize = 40;

/// A preview's questions as the person sees them: each with its own words
/// and, when it has options, distinct short ones to choose from.
fn questions(asked: Vec<Question>) -> Result<Vec<protocol::model::ApprovalQuestion>, &'static str> {
    const QUESTIONS: &str = "questions are up to 6, each a question of up to 1000 characters with up to 8 options of distinct one-line labels up to 80 characters";
    if asked.len() > MAX_QUESTIONS {
        return Err(QUESTIONS);
    }
    let line = |text: Option<String>, max| match words(text, max, QUESTIONS)? {
        Some(t) if t.contains('\n') => Err(QUESTIONS),
        t => Ok(t),
    };
    asked
        .into_iter()
        .map(|q| {
            if q.options.len() > MAX_OPTIONS {
                return Err(QUESTIONS);
            }
            let mut options: Vec<protocol::model::ApprovalQuestionOption> = Vec::new();
            for o in q.options {
                let label = line(Some(o.label), MAX_LABEL)?.ok_or(QUESTIONS)?;
                if options.iter().any(|known| known.label == label) {
                    return Err(QUESTIONS);
                }
                options.push(protocol::model::ApprovalQuestionOption {
                    label,
                    description: words(o.description, MAX_NOTE, QUESTIONS)?,
                });
            }
            Ok(protocol::model::ApprovalQuestion {
                id: None,
                header: line(q.header, MAX_HEADER)?,
                question: words(Some(q.question), MAX_NOTE, QUESTIONS)?.ok_or(QUESTIONS)?,
                multi_select: q.multi_select && !options.is_empty(),
                options,
                secret: false,
            })
        })
        .collect()
}

/// Trimmed text of at most `max` characters; empty is none.
fn words(
    text: Option<String>,
    max: usize,
    what: &'static str,
) -> Result<Option<String>, &'static str> {
    let text = text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
    if text
        .as_ref()
        .is_some_and(|t| t.chars().count() > max || t.chars().any(|c| c.is_control() && c != '\n'))
    {
        return Err(what);
    }
    Ok(text)
}

/// What is wrong with a checklist tool's arguments, as serde says it.
fn checklist_mistake(name: &str, args: Value) -> Option<String> {
    let wrong = match name {
        "checklist_add" => serde_json::from_value::<ChecklistAdd>(args).err(),
        "checklist_remove" => serde_json::from_value::<ChecklistRemove>(args).err(),
        "checklist_reply" => serde_json::from_value::<ChecklistReply>(args).err(),
        _ => None,
    }?;
    Some(format!("invalid {name} arguments: {wrong}"))
}

/// What a person tool puts in front of the person.
fn person_call(name: &str, args: Value) -> Result<PersonCall, &'static str> {
    const NOTE: &str = "a caption or note is up to 1000 characters";
    const GOAL: &str = "a goal is up to 1000 characters";
    Ok(match name {
        "show_person" => {
            let p: Show = parse(args)?;
            if p.path.trim().is_empty() || p.path.len() > 4096 || p.path.contains('\0') {
                return Err("path is a file in the project folder");
            }
            PersonCall::Show {
                path: p.path,
                caption: words(p.caption, MAX_NOTE, NOTE)?,
                invite: p.invite,
            }
        }
        "show_preview" => {
            let p: Preview = parse(args)?;
            const LABEL: &str = "a label is up to 80 characters on one line";
            let label = words(p.label, MAX_LABEL, LABEL)?;
            if label.as_ref().is_some_and(|l| l.contains('\n')) {
                return Err(LABEL);
            }
            const START: &str = "start is one command line, up to 1000 characters";
            let start = words(p.start, MAX_NOTE, START)?;
            if start.as_ref().is_some_and(|s| s.contains('\n')) {
                return Err(START);
            }
            PersonCall::Preview {
                url: p.url.trim().to_owned(),
                label,
                note: words(p.note, MAX_NOTE, NOTE)?,
                start,
                questions: questions(p.questions)?,
            }
        }
        "invite_person" => {
            let p: Invite = parse(args)?;
            if p.goal.is_some() && p.action != protocol::model::PersonAction::ComputerUse {
                return Err("goal is only for computer_use");
            }
            PersonCall::Invite {
                action: p.action,
                note: words(Some(p.note), MAX_NOTE, NOTE)?.ok_or("say why in note")?,
                goal: words(p.goal, MAX_NOTE, GOAL)?,
            }
        }
        WAKE_TOOL => {
            let p: Wake = parse(args)?;
            if !(1..=MAX_WAKE_MINUTES).contains(&p.minutes) {
                return Err("minutes is 1 to 1440");
            }
            const REASON: &str = "say in reason what you're waiting for, up to 1000 characters";
            PersonCall::Wake {
                minutes: p.minutes,
                reason: words(Some(p.reason), MAX_NOTE, REASON)?.ok_or(REASON)?,
            }
        }
        QUESTIONS_TOOL => PersonCall::CanAsk,
        // Ophio checks what a checklist holds as it keeps it, and says
        // what is wrong.
        "checklist_read" => PersonCall::ChecklistRead,
        "checklist_add" => {
            let p: ChecklistAdd = parse(args)?;
            PersonCall::ChecklistAdd {
                title: p.title,
                summary: p.summary,
                theme: p.theme,
                section: p.section,
                items: p.items,
            }
        }
        "checklist_remove" => {
            let p: ChecklistRemove = parse(args)?;
            if p.items.is_empty() || p.items.len() > protocol::checklist::MAX_ITEMS {
                return Err("name the ids of the items to take off in items");
            }
            PersonCall::ChecklistRemove { items: p.items }
        }
        "checklist_reply" => {
            let p: ChecklistReply = parse(args)?;
            PersonCall::ChecklistReply {
                item: p.item,
                text: p.text,
            }
        }
        _ => return Err("unknown tool"),
    })
}

/// The longest title GitHub takes.
const MAX_TITLE: usize = 256;
/// The longest body GitHub takes, less room for Øphio's disclosure line.
const MAX_GITHUB_BODY: usize = 60_000;

fn title(title: String) -> Result<String, &'static str> {
    let title = title.trim().to_owned();
    if title.is_empty() || title.chars().count() > MAX_TITLE {
        return Err("a title is 1 to 256 characters");
    }
    Ok(title)
}
fn body(body: String) -> Result<String, &'static str> {
    if body.trim().is_empty() || body.chars().count() > MAX_GITHUB_BODY {
        return Err("a body is 1 to 60000 characters");
    }
    Ok(body)
}

/// What a GitHub tool asks to post.
fn github_call(name: &str, args: Value) -> Result<GithubCall, &'static str> {
    Ok(match name {
        "github_pr" => {
            let p: Pr = parse(args)?;
            GithubCall::Pr {
                title: title(p.title)?,
                body: body(p.body)?,
            }
        }
        "github_comment" => {
            let p: Comment = parse(args)?;
            if p.thread
                .as_deref()
                .is_some_and(|t| t.is_empty() || t.len() > 128)
            {
                return Err("thread is a review thread's id");
            }
            if p.resolve && p.thread.is_none() {
                return Err("resolve needs the thread it resolves");
            }
            GithubCall::Comment {
                body: body(p.body)?,
                thread: p.thread,
                resolve: p.resolve,
            }
        }
        "github_issue" => {
            let p: Issue = parse(args)?;
            GithubCall::Issue {
                title: title(p.title)?,
                body: body(p.body)?,
                security: p.security,
                distinct_from: distinct_from(p.distinct_from)?,
            }
        }
        "github_ready" => parse::<Nothing>(args).map(|_| GithubCall::Ready)?,
        "github_merge" => {
            let p: Merge = parse(args)?;
            let head = p.expected_head.trim().to_ascii_lowercase();
            if !matches!(head.len(), 40 | 64) || !head.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err("expected_head is the full commit id you checked");
            }
            GithubCall::Merge {
                expected_head: head,
            }
        }
        "github_cannot_reproduce" => {
            let p: CannotReproduce = parse(args)?;
            GithubCall::CannotReproduce {
                tried: body(p.tried)?,
                already_fixed: p.already_fixed,
            }
        }
        _ => return Err("unknown tool"),
    })
}

/// The most labels an agent puts on an issue it files.
const MAX_LABELS: usize = 10;
/// The most issues an agent says one it files is not, more than Øphio
/// shows it at once.
const MAX_DISTINCT_FROM: usize = 20;

/// The issues an agent read and says the one it files is not.
fn distinct_from(mut numbers: Vec<u64>) -> Result<Vec<u64>, &'static str> {
    numbers.sort_unstable();
    numbers.dedup();
    match numbers.len() > MAX_DISTINCT_FROM || numbers.contains(&0) {
        true => Err("distinct_from is up to 20 issue numbers"),
        false => Ok(numbers),
    }
}
/// The longest label GitHub takes.
const MAX_LABEL_NAME: usize = 50;

/// An agent named for work, its model and effort as ophio_agents lists them.
fn agent_choice(agent: Option<Agent>) -> Result<Option<ports::bridge::AgentChoice>, &'static str> {
    const NAME: &str = "a model or effort is a name from ophio_agents";
    let Some(a) = agent else { return Ok(None) };
    let name = |n: Option<String>| -> Result<Option<String>, &'static str> {
        let n = words(n, 128, NAME)?;
        if n.as_ref().is_some_and(|n| n.contains('\n')) {
            return Err(NAME);
        }
        Ok(n)
    };
    Ok(Some(ports::bridge::AgentChoice {
        agent: a.agent,
        model: name(a.model)?,
        effort: name(a.effort)?,
    }))
}

/// The longest question an agent asks another, with what it shows it.
const MAX_ASK: usize = 60_000;
/// The longest title of a question an agent asks another.
const MAX_ASK_TITLE: usize = 80;

/// What a planning tool asks for.
fn work_call(name: &str, args: Value) -> Result<WorkCall, &'static str> {
    const NOTE: &str = "a note is up to 1000 characters";
    Ok(match name {
        "ophio_agents" => parse::<Nothing>(args).map(|_| WorkCall::Agents)?,
        ASK_TOOL => {
            let p: Ask = parse(args)?;
            const PROMPT: &str = "prompt is what you ask, 1 to 60000 characters";
            if p.prompt.trim().is_empty()
                || p.prompt.chars().count() > MAX_ASK
                || p.prompt.contains('\0')
            {
                return Err(PROMPT);
            }
            const TITLE: &str = "a title is up to 80 characters on one line";
            let title = words(p.title, MAX_ASK_TITLE, TITLE)?;
            if title.as_ref().is_some_and(|t| t.contains('\n')) {
                return Err(TITLE);
            }
            WorkCall::Ask {
                agent: agent_choice(Some(p.agent))?.ok_or("agent is required")?,
                prompt: p.prompt,
                title,
            }
        }
        "github_new_issue" => {
            let p: NewIssue = parse(args)?;
            const LABELS: &str = "labels are up to 10 names of 1 to 50 characters";
            let labels: Vec<String> = p.labels.iter().map(|l| l.trim().to_owned()).collect();
            if labels.len() > MAX_LABELS
                || labels.iter().any(|l| {
                    l.is_empty()
                        || l.chars().count() > MAX_LABEL_NAME
                        || l.chars().any(char::is_control)
                })
            {
                return Err(LABELS);
            }
            WorkCall::NewIssue {
                title: title(p.title)?,
                body: body(p.body)?,
                labels,
                distinct_from: distinct_from(p.distinct_from)?,
            }
        }
        "github_start_issue" => {
            let p: StartIssue = parse(args)?;
            if p.number == 0 {
                return Err("number is the issue's number");
            }
            WorkCall::StartIssue {
                number: p.number,
                agent: agent_choice(p.agent)?,
                // The person asked for the work to start.
                start: p.start.unwrap_or(true),
                note: words(p.note, MAX_NOTE, NOTE)?,
                reviewer: agent_choice(p.reviewer)?,
            }
        }
        "github_start_review" => {
            let p: StartReview = parse(args)?;
            let profile = match p.profile.as_deref().map(str::trim) {
                None | Some("" | "default") => None,
                Some(name) => Some(
                    serde_json::from_value::<protocol::github::FindFocus>(json!(name))
                        .map_err(|_| "profile is one of those github_start_review lists")?,
                ),
            };
            if profile.is_some_and(protocol::github::FindFocus::uses_it) {
                return Err(
                    "a reviewer that uses the app works on a screen the person chooses, so they start it from Tasks › Find/Fix › New review",
                );
            }
            let folder = p
                .folder
                .map(|f| f.trim().to_owned())
                .filter(|f| !f.is_empty());
            if folder
                .as_ref()
                .is_some_and(|f| f.len() > 4096 || f.chars().any(char::is_control))
            {
                return Err("folder is a folder in the project");
            }
            if p.max_issues
                .is_some_and(|n| !(1..=protocol::github::MAX_FINDINGS_PER_RUN).contains(&n))
            {
                return Err("max_issues is 1 to 20");
            }
            WorkCall::StartReview {
                profile,
                folder,
                reviewer: agent_choice(p.reviewer)?,
                fixer: agent_choice(p.fixer)?,
                max_issues: p.max_issues,
                then: p.then,
            }
        }
        _ => return Err("unknown tool"),
    })
}

/// What a browser or login tool asks of the agent browser.
fn browser_action(name: &str, args: Value) -> Result<BrowserAction, &'static str> {
    Ok(match name {
        "browser_open" => {
            let p: Open = parse(args)?;
            BrowserAction::Open {
                url: p.url,
                new_tab: p.new_tab,
            }
        }
        "browser_back" => parse::<Nothing>(args).map(|_| BrowserAction::Back)?,
        "browser_tabs" => parse::<Nothing>(args).map(|_| BrowserAction::Tabs)?,
        "browser_switch_tab" => BrowserAction::SwitchTab {
            tab: parse::<Tab>(args)?.tab,
        },
        "browser_close_tab" => BrowserAction::CloseTab {
            tab: parse::<MaybeTab>(args)?.tab,
        },
        "browser_observe" => parse::<Nothing>(args).map(|_| BrowserAction::Observe)?,
        "browser_click" => BrowserAction::Click {
            control: parse::<Click>(args)?.control,
        },
        "browser_type" => {
            let p: Type = parse(args)?;
            BrowserAction::Type {
                control: p.control,
                text: p.text,
                replace: p.replace,
            }
        }
        "browser_press" => BrowserAction::Press {
            key: parse::<Press>(args)?.key,
        },
        "browser_scroll" => {
            let p: PageScroll = parse(args)?;
            if !p.dy.is_finite() {
                return Err("scroll deltas must be finite");
            }
            BrowserAction::Scroll {
                dy: p.dy,
                control: p.control,
            }
        }
        "list_logins" => parse::<Nothing>(args).map(|_| BrowserAction::ListLogins)?,
        "fill_login" => {
            let p: Login = parse(args)?;
            BrowserAction::FillLogin {
                login: p.login,
                submit: p.submit,
            }
        }
        "fill_code" => {
            let p: Login = parse(args)?;
            BrowserAction::FillCode {
                login: p.login,
                submit: p.submit,
            }
        }
        "ask_owner_for_code" => {
            let p: AskCode = parse(args)?;
            BrowserAction::AskOwnerForCode {
                login: p.login,
                why: p.why,
                submit: p.submit,
            }
        }
        _ => return Err("unknown tool"),
    })
}

fn parse<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, &'static str> {
    serde_json::from_value(args).map_err(|_| "invalid tool arguments")
}
fn point(
    x: f64,
    y: f64,
    revision: u64,
    observed: Option<(u32, u32, u64)>,
) -> Result<TargetPoint, &'static str> {
    let (w, h, known) = observed.ok_or("observe the target before using image coordinates")?;
    if revision != known {
        return Err("stale observation_revision; observe again before acting");
    }
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x >= f64::from(w)
        || y >= f64::from(h)
    {
        return Err("coordinates are outside the observation image");
    }
    Ok(TargetPoint {
        x: x / f64::from(w),
        y: y / f64::from(h),
    })
}
fn tool_request(
    name: &str,
    args: Value,
    observed: Option<(u32, u32, u64)>,
) -> Result<BridgeRequest, &'static str> {
    Ok(match name {
        PERMISSION_TOOL => {
            let p: Permission = parse(args)?;
            if p.tool_name.is_empty() || !p.input.is_object() {
                return Err("permission prompt requires a tool name and input object");
            }
            BridgeRequest::PermissionPrompt {
                tool_name: p.tool_name,
                input: p.input,
                tool_use_id: p.tool_use_id,
                native_request_id: None,
            }
        }
        "observe_target" | "observe" => {
            let p: Observe = parse(args)?;
            if p.max_dim.is_some_and(|v| v == 0 || v > 16384) {
                return Err("max_dim must be between 1 and 16384");
            }
            match p.annotation {
                Some(annotation) => BridgeRequest::ObserveEvidence {
                    max_dim: p.max_dim,
                    annotation,
                },
                None => BridgeRequest::ObserveTarget { max_dim: p.max_dim },
            }
        }
        "pointer_action" | "pointer" => {
            let p: Pointer = parse(args)?;
            if matches!(p.action, BridgePointer::Click { count, .. } if !(1..=3).contains(&count)) {
                return Err("click count must be between 1 and 3");
            }
            BridgeRequest::Pointer {
                action: p.action,
                at: point(p.x, p.y, p.observation_revision, observed)?,
                observation_revision: p.observation_revision,
            }
        }
        "scroll" => {
            let p: Scroll = parse(args)?;
            if !p.dx.is_finite() || !p.dy.is_finite() {
                return Err("scroll deltas must be finite");
            }
            BridgeRequest::Scroll {
                at: point(p.x, p.y, p.observation_revision, observed)?,
                dx: p.dx,
                dy: p.dy,
                observation_revision: p.observation_revision,
            }
        }
        "key_action" | "key" => {
            let p: Key = parse(args)?;
            if p.keys.is_empty()
                || p.keys.len() > 16
                || p.keys.iter().any(|k| k.is_empty() || k.len() > 64)
            {
                return Err("keys must contain 1 to 16 key names");
            }
            BridgeRequest::Key { keys: p.keys }
        }
        "type_text" => {
            let p: Text = parse(args)?;
            BridgeRequest::TypeText { text: p.text }
        }
        HUMAN_TOOL => {
            let p: Human = parse(args)?;
            BridgeRequest::RequestHuman { reason: p.reason }
        }
        _ if BROWSER_TOOLS.contains(&name) => BridgeRequest::Browser {
            action: browser_action(name, args)?,
        },
        _ if GITHUB_TOOLS.contains(&name) => BridgeRequest::Github {
            action: github_call(name, args)?,
        },
        _ if PERSON_TOOLS.contains(&name) => BridgeRequest::Person {
            action: person_call(name, args)?,
        },
        _ if WORK_TOOLS.contains(&name) => BridgeRequest::Work {
            action: work_call(name, args)?,
        },
        _ => return Err("unknown tool"),
    })
}

/// The issues Øphio listed as reading alike, which the agent read and
/// found to be about something else.
fn distinct_from_schema() -> Value {
    json!({"type":"array","maxItems":MAX_DISTINCT_FROM,"items":{"type":"integer","minimum":1},"description":"Only after Øphio listed issues that read alike: the numbers of those you read and found to be about a different problem."})
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}
/// All a turn answering the person's question about the checklist sees: how
/// to read the checklist and how to answer under it.
fn answer_tools() -> Vec<Value> {
    (person_tools().into_iter())
        .filter(|t| ANSWER_TOOLS.contains(&t["name"].as_str().unwrap_or("")))
        .collect()
}

fn tools(computer_use: bool, browser: bool, github: bool) -> Vec<Value> {
    let mut result = vec![tool(
        PERMISSION_TOOL,
        "Resolve a native Claude Code permission request through Øphio.",
        json!({"tool_name":{"type":"string"},"input":{"type":"object"},"tool_use_id":{"type":"string"}}),
        &["tool_name", "input"],
    )];
    if !computer_use {
        if browser {
            result.extend(browser_tools());
        }
        if github {
            result.extend(github_tools());
        }
        result.extend(person_tools());
        result.extend(
            work_tools()
                .into_iter()
                .filter(|t| !github || ASK_TOOLS.contains(&t["name"].as_str().unwrap_or(""))),
        );
        return result;
    }
    result.clear();
    result.push(tool("observe_target", "Observe the run's bound target. Use the returned image dimensions and observation_revision for pointer and scroll actions.", json!({"max_dim":{"type":"integer","minimum":1,"maximum":16384},"annotation":{"type":"object","description":"After-evidence for reviewed drawings; use their exact bundle, revision and mark IDs.","additionalProperties":false,"properties":{"bundle_id":{"type":"string"},"revision":{"type":"integer","minimum":0},"mark_ids":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":256}},"required":["bundle_id","revision","mark_ids"]}}), &[]));
    let button = json!({"type":"string","enum":["left","right","middle"]});
    let action = json!({"oneOf":[
        {"type":"object","properties":{"kind":{"const":"move"}},"required":["kind"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"const":"click"},"button":button,"count":{"type":"integer","minimum":1,"maximum":3}},"required":["kind","button","count"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"enum":["down","up"]},"button":button},"required":["kind","button"],"additionalProperties":false}
    ]});
    result.push(tool("pointer_action", "Pointer action in observation image pixels, fenced by observation_revision.", json!({"action":action,"x":{"type":"number","minimum":0},"y":{"type":"number","minimum":0},"observation_revision":{"type":"integer","minimum":0}}), &["action","x","y","observation_revision"]));
    result.push(tool(
        "key_action",
        "Press the named key combination on the bound target.",
        json!({"keys":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":16}}),
        &["keys"],
    ));
    result.push(tool(
        "type_text",
        "Type text on the bound target.",
        json!({"text":{"type":"string"}}),
        &["text"],
    ));
    result.push(tool("scroll", "Scroll at an observation image pixel position, fenced by observation_revision.", json!({"x":{"type":"number","minimum":0},"y":{"type":"number","minimum":0},"dx":{"type":"number"},"dy":{"type":"number"},"observation_revision":{"type":"integer","minimum":0}}), &["x","y","dx","dy","observation_revision"]));
    result.push(tool(
        HUMAN_TOOL,
        "Ask the person to perform a step. The broker controls the handoff.",
        json!({"reason":{"type":"string"}}),
        &["reason"],
    ));
    result
}

fn browser_tools() -> Vec<Value> {
    let none = json!({});
    let control =
        json!({"type":"string","description":"The control's ref from the last browser_observe."});
    let login = json!({"type":"string","description":"The login's id from list_logins."});
    let submit = json!({"type":"boolean","description":"Press Enter afterwards."});
    vec![
        tool(
            "browser_open",
            "Open a web address (http or https) in this task's tab of Øphio's agent browser, or in a new tab. The browser stays signed in to sites between tasks.",
            json!({"url":{"type":"string"},"new_tab":{"type":"boolean"}}),
            &["url"],
        ),
        tool(
            "browser_back",
            "Go back a page in the current tab.",
            none.clone(),
            &[],
        ),
        tool("browser_tabs", "List this task's tabs.", none.clone(), &[]),
        tool(
            "browser_switch_tab",
            "Make one of this task's tabs the current one.",
            json!({"tab":{"type":"string"}}),
            &["tab"],
        ),
        tool(
            "browser_close_tab",
            "Close a tab, or the current tab.",
            json!({"tab":{"type":"string"}}),
            &[],
        ),
        tool(
            "browser_observe",
            "Look at the current page: a screenshot, its real address and title, and its controls, each with a ref for browser_click, browser_type and browser_scroll. Look again after the page changes; refs from an earlier look may be gone. Password fields and fields Øphio filled never show their values.",
            none.clone(),
            &[],
        ),
        tool(
            "browser_click",
            "Click a control.",
            json!({"control":control}),
            &["control"],
        ),
        tool(
            "browser_type",
            "Type text into a control, after what it holds or in its place with replace. Never type a password; use fill_login.",
            json!({"control":control,"text":{"type":"string"},"replace":{"type":"boolean"}}),
            &["control", "text"],
        ),
        tool(
            "browser_press",
            "Press a key on the current page.",
            json!({"key":{"type":"string","enum":["Enter","Tab","Escape","Backspace","Delete","ArrowLeft","ArrowUp","ArrowRight","ArrowDown","Home","End","PageUp","PageDown","Space"]}}),
            &["key"],
        ),
        tool(
            "browser_scroll",
            "Scroll the page, or the part of it holding a control, by dy pixels; negative scrolls up.",
            json!({"dy":{"type":"number"},"control":control}),
            &["dy"],
        ),
        tool(
            "list_logins",
            "The website logins the owner lets this task use: id, name, sites, username, whether a 2FA setup key is stored, and the owner's notes for you. Passwords are never shown.",
            none,
            &[],
        ),
        tool(
            "fill_login",
            "Fill a login's username and password into the sign-in form on the current page, which must be on one of the login's sites over https. Answers only whether it filled. The owner may be asked to approve first.",
            json!({"login":login,"submit":submit}),
            &["login"],
        ),
        tool(
            "fill_code",
            "Fill the current authenticator code into the current page, for a login with a 2FA setup key stored.",
            json!({"login":login,"submit":submit}),
            &["login"],
        ),
        tool(
            "ask_owner_for_code",
            "Ask the owner for a sign-in code the site sent them by text or email. Say in why where they will find it. Øphio fills the code they type into the current page; you never see it.",
            json!({"login":login,"why":{"type":"string"},"submit":submit}),
            &["login", "why"],
        ),
        tool(
            HUMAN_TOOL,
            "Ask the owner to take a step themselves, such as a CAPTCHA, an \"is this you?\" check or a passkey. They can watch the agent browser and take it over from their phone.",
            json!({"reason":{"type":"string"}}),
            &["reason"],
        ),
    ]
}

fn github_tools() -> Vec<Value> {
    let title = json!({"type":"string","minLength":1,"maxLength":MAX_TITLE});
    let body = json!({"type":"string","minLength":1,"maxLength":MAX_GITHUB_BODY});
    vec![
        tool(
            "github_pr",
            "Open this task's pull request from the branch you pushed, or rewrite the title and description of the one already open. Push first. Øphio adds the line saying an agent made it, the agent label and the link to this task, and opens it as a draft where the project asks for drafts.",
            json!({"title":title,"body":body}),
            &["title", "body"],
        ),
        tool(
            "github_comment",
            "Comment on this task's pull request, or on its issue if there is no pull request yet; or, with thread, reply in one of the pull request's review threads, such as a bot's finding you checked. Øphio puts a first line saying an agent wrote it. You can't approve or request changes.",
            json!({"body":body,"thread":{"type":"string","description":"The review thread's id, from the pull request's review threads."},"resolve":{"type":"boolean","description":"Resolve the thread after replying, once its finding is fixed or answered. Only for a thread a bot started; a person's thread is theirs to resolve."}}),
            &["body"],
        ),
        tool(
            "github_issue",
            "File an issue for one finding. Look first for an issue that already says it (gh issue list --state all --search \"<a few words>\"), and don't file one that does. Give a plain-language title; then what is wrong, where (file and line links at the commit you read), how to see it (a test that fails, where you can), how serious it is and how sure you are. Set security for a vulnerability: on a public repository it goes to the repository's private vulnerability reporting, never a public issue. Øphio refuses one titled like an open or recently closed issue, and lists any that read alike instead of filing: read them, and file only if yours is a different problem, with distinct_from naming the ones you read.",
            json!({"title":title,"body":body,"security":{"type":"boolean"},"distinct_from":distinct_from_schema()}),
            &["title", "body"],
        ),
        tool(
            "github_ready",
            "Mark this task's draft pull request ready for review, once every required check passes on its latest commit and no review thread is unresolved. Only where the project lets the agent merge.",
            json!({}),
            &[],
        ),
        tool(
            "github_merge",
            "Squash-merge this task's pull request at the commit you checked. Only where the project lets the agent merge, once it's ready, every required check passes and no review thread is unresolved. Refused if the pull request has moved past expected_head.",
            json!({"expected_head":{"type":"string","description":"The full id of the head commit you checked."}}),
            &["expected_head"],
        ),
        tool(
            "github_cannot_reproduce",
            "Say on this task's issue that you couldn't reproduce it, with what you tried, and stop without changing code. The owner decides what happens to the issue.",
            json!({"tried":body,"already_fixed":{"type":"boolean","description":"Fixed already; cite the commit/PR in tried."}}),
            &["tried"],
        ),
    ]
}

/// The tools that put things in front of the person in Talk. What each
/// says is how an agent learns what Ophio can show and what comes back.
fn person_tools() -> Vec<Value> {
    let note = json!({"type":"string","minLength":1,"maxLength":MAX_NOTE});
    vec![
        tool(
            "show_person",
            "Show the person a picture or video from this project's folder or your scratch folder in Chat: a screenshot of something you built (a page in a headless browser), a diagram, a chart, a screen recording or a render. PNG, JPEG, WebP, GIF, MP4, WebM or MOV, up to 20 MiB. Caption it with what they're looking at. invite \"draw\" asks them to draw on it: their marks come back in their next message as labelled drawings (A, B…) at normalized coordinates, each with their comments. invite \"talk\" on a video asks them to talk over it: each comment comes back with the span it was said over (\"0:12–0:15\") and a still from that moment.",
            json!({"path":{"type":"string","description":"The file, relative to the project folder, or an absolute path inside it or your scratch folder."},"caption":note,"invite":{"type":"string","enum":["draw","talk"],"description":"What to ask them to do with it: draw on a picture, or talk over a video."}}),
            &["path"],
        ),
        tool(
            "show_preview",
            "Put a page you serve on this computer, such as the dev server of what you're building, on the person's phone as a preview, carried over their private network. Give the address you reach it at here: http://127.0.0.1:PORT/ or http://localhost:PORT/, with a path if you like. A server started in the project's folder usually joins its previews in Control by itself; this brings it up in Chat with a button to open it, adding it to Control if it isn't there. Your server stops when your turn ends, so give start, the command that starts it from the project's folder: if nothing answers when they open the preview, Ophio offers to run it there until the page answers. They can take a still from the preview and draw on it, which comes back like a drawn-on picture. Ask what you want to know about the page in questions rather than in your reply: they answer beside the page as they try it and send the answers as their next message, so end your turn instead of waiting.",
            json!({"url":{"type":"string","description":"The page's address on this computer: loopback only."},"questions":{"type":"array","maxItems":MAX_QUESTIONS,"description":"What you'd like them to tell you about the page, asked beside it in the preview. Leave out options for a question they answer in their own words.","items":{"type":"object","properties":{
                "question":{"type":"string","minLength":1,"maxLength":MAX_NOTE},
                "header":{"type":"string","maxLength":MAX_HEADER,"description":"A short label for it, like \"Layout\"."},
                "options":{"type":"array","maxItems":MAX_OPTIONS,"description":"Answers to choose from; they can also write their own.","items":{"type":"object","properties":{"label":{"type":"string","minLength":1,"maxLength":MAX_LABEL},"description":{"type":"string","maxLength":MAX_NOTE}},"required":["label"],"additionalProperties":false}},
                "multi_select":{"type":"boolean","description":"Several options may be chosen together."}
            },"required":["question"],"additionalProperties":false}},"label":{"type":"string","maxLength":MAX_LABEL,"description":"A short name for the preview, like \"Site\" or \"Storybook\"."},"note":note,"start":{"type":"string","maxLength":MAX_NOTE,"description":"The command that starts the page's server from the project's folder, on one line as you'd type it in a shell there, like \"npx vite --port 5179\". Keep the port the url uses."}}),
            &["url"],
        ),
        tool(
            "invite_person",
            "Offer the person a button in Chat that does something in Ophio. \"capture\": screenshot this computer's screen and draw on it, to point at something there. \"watch_desktop\": watch the screen live from their phone, as while you change something visual. \"send_files\": send you files or photos from their phone into this project's folder; look there for new files, or ask where they put them. \"computer_use\": have an agent work in an app or a browser on one of this computer's screens while they watch and can take over, for what only a person at the screen could do, like trying a desktop app or clicking through a site as a user would; give the goal to fill in. \"voice_control\": control the browser or the focused app on this computer by speaking, with Jev, as for walking through a flow hands-free. Say in note what you'd like and why. Nothing waits for it: what they do comes back as their next message.",
            json!({"action":{"type":"string","enum":["capture","watch_desktop","send_files","computer_use","voice_control"]},"note":note,"goal":{"type":"string","maxLength":MAX_NOTE,"description":"For computer_use only: what the agent at the screen should do, as the person would ask it, like \"Open the app, sign up with a new account and tell me where it's confusing\"."}}),
            &["action", "note"],
        ),
        tool(
            WAKE_TOOL,
            "Have Ophio wake you later instead of waiting in your turn, for CI, a deploy, a reply or anything else. Give how many minutes and what for, then end your turn at once with a short summary: the project is free meanwhile, and the person sees the task as watching. Ophio starts your next turn with your reason; if it still isn't ready, call this again. A newer call replaces an earlier one.",
            json!({"minutes":{"type":"integer","minimum":1,"maximum":MAX_WAKE_MINUTES,"description":"How long from now to wake you. Pick about how long the thing takes: 5 to 15 for CI checks, longer for slow work."},"reason":{"type":"string","minLength":1,"maxLength":MAX_NOTE,"description":"What you're waiting for and what to check when you wake, like \"CI on PR #42; merge if it's green\"."}}),
            &["minutes", "reason"],
        ),
        tool(
            QUESTIONS_TOOL,
            "Whether the person wants questions now. Call it every time before you ask them anything, with AskUserQuestion, request_user_input or any other way you'd stop for an answer. When questions are off, it says what to do with yours instead.",
            json!({}),
            &[],
        ),
        tool(
            "checklist_read",
            "Read the project's checklist of what the person has to do: every id, their ticks, answers and questions, and how to write items. Read it before checklist_add.",
            json!({}),
            &[],
        ),
        tool("checklist_add", CHECKLIST_ADD, checklist_add_schema(), &[]),
        tool(
            "checklist_remove",
            "Take items off the project's checklist by id. Leave what they ticked off.",
            json!({"items":{"type":"array","minItems":1,"items":{"type":"string"}}}),
            &["items"],
        ),
        tool(
            "checklist_reply",
            "Answer a question the person asked on the checklist: give item for one asked under an item.",
            json!({"item":{"type":"string"},"text":{"type":"string","minLength":1}}),
            &["text"],
        ),
    ]
}

/// When an agent gives the person something to do. How to write it well
/// comes with `checklist_read`, and Ophio checks the fields itself, so the
/// list every turn carries stays short.
const CHECKLIST_ADD: &str = "Give the person what they must do themselves on the project's checklist, never as directions in your reply: whenever they have to act outside this chat, such as sign up or in, grant access, make a key, pay, approve, choose, install or check something only they can. Read checklist_read first; it says how to write items. Each step is complete on its own, with the link (url), text to paste (copy) or file it needs. Never point them elsewhere or include a secret. Give waiting on an item you're blocked on and end your turn if nothing else can go on: Ophio starts your next turn the moment they finish it. Only what can't be done now goes on it, never work you're about to do: your own work blocked on theirs is owner agent, after them, and you mark it done.";

fn checklist_add_schema() -> Value {
    let text = json!({"type":"string"});
    let list = |items: Value| json!({"type":"array","items":items});
    let step = json!({"type":"object","properties":{"text":text,"url":text,"copy":text,"file":{"type":"string","description":"In the project or your scratch folder."}},"required":["text"]});
    let option =
        json!({"type":"object","properties":{"label":text,"detail":text},"required":["label"]});
    let item = json!({"type":"object","properties":{
        "id":{"type":"string","pattern":"^[a-z0-9-]+$","description":"The same id rewrites it."},
        "title":text,"why":text,
        "minutes":{"type":"integer"},
        "priority":{"enum":["low","medium","high","blocker"],"description":"blocker: important work is stopped on it."},
        "due":{"type":"string","description":"A suggested day, YYYY-MM-DD."},
        "after":list(text.clone()),
        "steps":list(step),
        "ask":{"type":"object","properties":{"question":text,"options":list(option)},"required":["question"]},
        "waiting":{"type":"string","description":"What you'll do once it's done."},
        "reopen":{"type":"boolean"},
        "owner":{"enum":["person","agent"]},
        "done":{"type":"boolean","description":"Your own items only."}
    },"required":["id","title"]});
    json!({
        "title":text,"summary":text,
        "section":{"type":"string","description":"By title or id; made if new."},
        "items":list(item),
        "theme":{"type":"object","description":"The project's brand, if it has one: accent and surface (#rrggbb), font (sans, serif, rounded or mono), name, logo (an image in the project)."}
    })
}

/// The tools an agent plans the person's GitHub work with. What each says
/// is how an agent learns how Øphio runs that work and when to start it.
fn work_tools() -> Vec<Value> {
    let title = json!({"type":"string","minLength":1,"maxLength":MAX_TITLE});
    let body = json!({"type":"string","minLength":1,"maxLength":MAX_GITHUB_BODY});
    let note = json!({"type":"string","minLength":1,"maxLength":MAX_NOTE});
    let agent = |what: &str| {
        json!({"type":"object","description":what,"properties":{
            "agent":{"type":"string","enum":["claude_code","codex","hermes","open_claw","pi"]},
            "model":{"type":"string","description":"A model id from ophio_agents; leave out for the default."},
            "effort":{"type":"string","description":"An effort from ophio_agents; leave out for the default."}
        },"required":["agent"],"additionalProperties":false})
    };
    vec![
        tool(
            "ophio_agents",
            "What you can choose when you plan work in Øphio: the agents on this computer, whether each is signed in, their models and efforts; the person's defaults for this project; whether another task is working in it now; and, for a task that plans GitHub work, the project's GitHub setup (workflow on or off, repository, merge when green or drafts, review profiles). Call it before naming an agent or model in ask_agent, github_start_issue or github_start_review.",
            json!({}),
            &[],
        ),
        tool(
            ASK_TOOL,
            "Ask another agent on this computer to review or help with your work: Codex from Claude Code, Claude Code from Codex, or a model your own subagent tool can't run. Øphio runs it as a task of its own beside yours: it reads the project and changes nothing, and can't see this conversation or, for some agents, run commands, so put everything it needs in prompt: what you did and why, the commits or the diff, and which files to read. Returns at once; its final message comes back to you from Øphio. Keep working meanwhile, or end your turn if you need the answer first: Øphio starts your next turn with it. Don't wait or poll for it.",
            json!({
                "agent":agent("Who to ask, from ophio_agents. Name the model you want when it isn't the agent's default."),
                "prompt":{"type":"string","minLength":1,"maxLength":MAX_ASK,"description":"What to ask, with everything it needs to answer: it can't see this conversation."},
                "title":{"type":"string","minLength":1,"maxLength":MAX_ASK_TITLE,"description":"A few words for its task in Øphio, like \"Review the login fix\"."}
            }),
            &["agent", "prompt"],
        ),
        tool(
            "github_new_issue",
            "File an issue on this project's GitHub repository for work the person wants done: one bug, feature or chore each, in plain language, for an agent that never saw this conversation: what should change and why, where (file paths), and how to tell it's done. Look first for one that already asks for it (gh issue list --state all --search \"<a few words>\") and point the person to that instead. Øphio refuses one titled like an open or recently closed issue and lists any that read alike instead of filing: read them, and file only if yours is different, with distinct_from naming them. It asks the person first if the repository isn't theirs, and adds the line saying an agent filed it. Filing starts nothing. Returns the issue's number.",
            json!({"title":title,"body":body,"labels":{"type":"array","maxItems":MAX_LABELS,"items":{"type":"string","minLength":1,"maxLength":MAX_LABEL_NAME},"description":"Labels the repository already has, like \"bug\" or \"enhancement\"."},"distinct_from":distinct_from_schema()}),
            &["title", "body"],
        ),
        tool(
            "github_start_issue",
            "Put an agent on an open issue of this project's repository, only when the person asked for the work to start. Øphio adds a task to Next up that checks the issue is real, works it on its own branch and opens a pull request that closes it. It starts once the project is free, so after this conversation; with start false it waits for the person to start it. Leave agent out for the person's default. With reviewer, a second agent checks the change before it merges.",
            json!({"number":{"type":"integer","minimum":1},"agent":agent("Who works it, from ophio_agents."),"start":{"type":"boolean","default":true,"description":"Start by itself once the project is free (the default). false leaves it in Next up for the person to start."},"note":note,"reviewer":agent("Who checks the change before it merges, from ophio_agents, when the person wants a second agent to review it.")}),
            &["number"],
        ),
        tool(
            "github_start_review",
            "Start a review of this project, only when the person asks for one. A reviewer agent reads the code without changing it and files one issue per real problem, most serious first, up to max_issues; each goes to the fixer agent, which checks it's real before fixing it. profile default reviews as a careful senior engineer, for anything that matters; the others focus on one area (security findings on a public repository go to private vulnerability reporting). then: by_itself starts the fixes once the project is free, ask_first queues them in Next up for the person, file_only only files. Leave anything out for the project's own choice.",
            json!({
                "profile":{"type":"string","enum":["default","bugs","security","tests","accessibility","ui_ux","performance","reliability","maintainability","dependencies","privacy","api","docs"]},
                "folder":{"type":"string","description":"Review only this folder of the project, relative to its root."},
                "reviewer":agent("Who reviews, from ophio_agents."),
                "fixer":agent("Who fixes what the review files, from ophio_agents."),
                "max_issues":{"type":"integer","minimum":1,"maximum":protocol::github::MAX_FINDINGS_PER_RUN},
                "then":{"type":"string","enum":["by_itself","ask_first","file_only"]}
            }),
            &[],
        ),
    ]
}

#[cfg(test)]
mod isolation_tests {
    use super::*;
    #[test]
    fn specialist_advertises_exact_tools_and_model_identity_cannot_override_binding() {
        let names: Vec<_> = tools(true, true, true)
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "observe_target",
                "pointer_action",
                "key_action",
                "type_text",
                "scroll",
                "request_human"
            ]
        );
        for field in ["run_id", "target_id", "lease_id", "generation"] {
            let mut arguments = json!({});
            arguments[field] = json!("model-supplied");
            assert!(tool_request("observe_target", arguments, None).is_err());
        }
        assert_eq!(
            tools(false, false, false).len(),
            1 + PERSON_TOOLS.len() + WORK_TOOLS.len()
        );
        assert!(refusal("browser_open", true, true, true).is_some());
        assert!(refusal("github_pr", true, true, true).is_some());
        assert!(refusal(PERMISSION_TOOL, true, true, true).is_some());
        for name in PERSON_TOOLS.iter().chain(WORK_TOOLS) {
            assert!(refusal(name, true, true, true).is_some(), "{name}");
        }
    }

    #[test]
    fn the_readme_names_every_listed_tool() {
        let readme = include_str!("../README.md");
        // Between them these list every tool, all the work tools included.
        for (computer_use, browser, github) in [
            (true, true, true),
            (false, true, true),
            (false, false, false),
        ] {
            for tool in tools(computer_use, browser, github) {
                let name = tool["name"].as_str().unwrap();
                assert!(readme.contains(&format!("`{name}`")), "{name}");
            }
        }
    }

    #[test]
    fn the_agent_asks_to_be_woken_rather_than_waiting() {
        assert_eq!(
            tool_request(
                WAKE_TOOL,
                json!({"minutes":20,"reason":"  the checks on PR #12  "}),
                None
            )
            .unwrap(),
            BridgeRequest::Person {
                action: PersonCall::Wake {
                    minutes: 20,
                    reason: "the checks on PR #12".into(),
                }
            }
        );
        for minutes in [0, 1441] {
            assert_eq!(
                tool_request(WAKE_TOOL, json!({"minutes":minutes,"reason":"CI"}), None),
                Err("minutes is 1 to 1440"),
                "{minutes}"
            );
        }
        for reason in [json!(" "), json!("x".repeat(1001))] {
            assert_eq!(
                tool_request(WAKE_TOOL, json!({"minutes":5,"reason":reason}), None),
                Err("say in reason what you're waiting for, up to 1000 characters")
            );
        }
        assert!(tool_request(WAKE_TOOL, json!({"minutes":5}), None).is_err());
        // The computer specialist ends when its task does.
        assert!(refusal(WAKE_TOOL, true, true, true).is_some());
    }

    #[test]
    fn the_agent_gives_the_person_what_to_do_on_the_checklist() {
        let add = json!({
            "section": "Accounts",
            "items": [{
                "id": "api",
                "title": "Grant API access",
                "steps": [{"text": "Open the console", "url": "https://example.com"}],
                "ask": {"question": "Which region?", "options": [{"label": "EU", "description": "Frankfurt"}]},
                "waiting": "deploy the service"
            }]
        });
        let Ok(BridgeRequest::Person {
            action: PersonCall::ChecklistAdd { section, items, .. },
        }) = tool_request("checklist_add", add, None)
        else {
            panic!("not a checklist addition");
        };
        assert_eq!(section.as_deref(), Some("Accounts"));
        assert_eq!(items[0].waiting.as_deref(), Some("deploy the service"));
        // Agents often call an option's detail its description.
        let ask = items[0].ask.as_ref().unwrap();
        assert_eq!(ask.options[0].detail.as_deref(), Some("Frankfurt"));
        // An agent's own item, which it marks done itself.
        let own = json!({"items": [{"id": "deploy", "title": "Deploy", "owner": "agent", "after": ["api"], "done": true}]});
        let Ok(BridgeRequest::Person {
            action: PersonCall::ChecklistAdd { items, .. },
        }) = tool_request("checklist_add", own, None)
        else {
            panic!("not a checklist addition");
        };
        assert_eq!(
            items[0].owner,
            Some(protocol::checklist::ChecklistOwner::Agent)
        );
        assert_eq!(items[0].done, Some(true));
        // A field Ophio doesn't know is named back, not dropped.
        let wrong = json!({"items": [{"id": "api", "title": "Grant", "link": "https://x"}]});
        assert!(tool_request("checklist_add", wrong.clone(), None).is_err());
        let why = checklist_mistake("checklist_add", wrong).unwrap();
        assert!(
            why.starts_with("invalid checklist_add arguments: "),
            "{why}"
        );
        assert!(why.contains("`link`"), "{why}");
        assert_eq!(
            tool_request("checklist_remove", json!({"items": []}), None),
            Err("name the ids of the items to take off in items")
        );
        assert_eq!(
            tool_request(
                "checklist_reply",
                json!({"item": "api", "text": "Yes"}),
                None
            )
            .unwrap(),
            BridgeRequest::Person {
                action: PersonCall::ChecklistReply {
                    item: Some("api".into()),
                    text: "Yes".into(),
                }
            }
        );
        assert_eq!(
            tool_request("checklist_read", json!({}), None).unwrap(),
            BridgeRequest::Person {
                action: PersonCall::ChecklistRead
            }
        );
        // The computer specialist has nothing to give the person.
        assert!(refusal("checklist_add", true, true, true).is_some());
    }

    #[test]
    fn the_agent_checks_whether_it_may_ask() {
        assert_eq!(
            tool_request(QUESTIONS_TOOL, json!({}), None).unwrap(),
            BridgeRequest::Person {
                action: PersonCall::CanAsk
            }
        );
        assert_eq!(refusal(QUESTIONS_TOOL, false, false, true), None);
        // The computer specialist asks through request_human.
        assert!(refusal(QUESTIONS_TOOL, true, true, true).is_some());
    }

    #[test]
    fn every_other_run_can_show_the_person_things() {
        let names: Vec<_> = tools(false, false, false)
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        let mut expected = vec![PERMISSION_TOOL];
        expected.extend(PERSON_TOOLS);
        expected.extend(WORK_TOOLS);
        assert_eq!(names, expected);
        for name in PERSON_TOOLS {
            assert_eq!(refusal(name, false, false, false), None, "{name}");
            assert_ne!(
                tool_request(name, json!({}), None),
                Err("unknown tool"),
                "{name}"
            );
        }
        assert_eq!(
            tool_request(
                "show_person",
                json!({"path":"shots/home.png","caption":"  The home page  ","invite":"draw"}),
                None
            )
            .unwrap(),
            BridgeRequest::Person {
                action: PersonCall::Show {
                    path: "shots/home.png".into(),
                    caption: Some("The home page".into()),
                    invite: Some(protocol::model::MediaInvite::Draw),
                }
            }
        );
        assert_eq!(
            tool_request("show_person", json!({"path":"a.mp4","caption":" "}), None).unwrap(),
            BridgeRequest::Person {
                action: PersonCall::Show {
                    path: "a.mp4".into(),
                    caption: None,
                    invite: None,
                }
            }
        );
        assert_eq!(
            tool_request(
                "show_preview",
                json!({"url":" http://127.0.0.1:5173/ ","label":"Site"}),
                None
            )
            .unwrap(),
            BridgeRequest::Person {
                action: PersonCall::Preview {
                    url: "http://127.0.0.1:5173/".into(),
                    label: Some("Site".into()),
                    note: None,
                    start: None,
                    questions: Vec::new(),
                }
            }
        );
        // How the server starts again once the run that started it ends.
        assert_eq!(
            tool_request(
                "show_preview",
                json!({"url":"http://127.0.0.1:5179/","start":" npx vite --port 5179 "}),
                None
            )
            .unwrap(),
            BridgeRequest::Person {
                action: PersonCall::Preview {
                    url: "http://127.0.0.1:5179/".into(),
                    label: None,
                    note: None,
                    start: Some("npx vite --port 5179".into()),
                    questions: Vec::new(),
                }
            }
        );
        // What it asks about the page: options to choose, or their own words.
        assert_eq!(
            tool_request(
                "show_preview",
                json!({"url":"http://127.0.0.1:5179/","questions":[
                    {"header":"Layout","question":"Which header?","options":[{"label":" A "},{"label":"B","description":"The bold one"}],"multi_select":true},
                    {"question":"Anything else?","multi_select":true}
                ]}),
                None
            )
            .unwrap(),
            BridgeRequest::Person {
                action: PersonCall::Preview {
                    url: "http://127.0.0.1:5179/".into(),
                    label: None,
                    note: None,
                    start: None,
                    questions: vec![
                        protocol::model::ApprovalQuestion {
                            id: None,
                            header: Some("Layout".into()),
                            question: "Which header?".into(),
                            options: vec![
                                protocol::model::ApprovalQuestionOption {
                                    label: "A".into(),
                                    description: None,
                                },
                                protocol::model::ApprovalQuestionOption {
                                    label: "B".into(),
                                    description: Some("The bold one".into()),
                                },
                            ],
                            multi_select: true,
                            secret: false,
                        },
                        protocol::model::ApprovalQuestion {
                            id: None,
                            header: None,
                            question: "Anything else?".into(),
                            options: Vec::new(),
                            multi_select: false,
                            secret: false,
                        },
                    ],
                }
            }
        );
        let many = vec![json!({"question":"Q"}); MAX_QUESTIONS + 1];
        let options = vec![json!({"label":"x"}); 2];
        for questions in [
            json!(many),
            json!([{"question":" "}]),
            json!([{"question":"Q","options":options}]),
            json!([{"question":"Q","options":[{"label":"a\nb"}]}]),
            json!([{"question":"Q","header":"x".repeat(MAX_HEADER + 1)}]),
            json!([{"question":"Q","secret":true}]),
        ] {
            assert!(
                tool_request(
                    "show_preview",
                    json!({"url":"http://127.0.0.1:5179/","questions":questions}),
                    None
                )
                .is_err(),
                "{questions}"
            );
        }
        for start in [
            "npm i\nnpx vite",
            "npx vite\u{7}",
            &"x".repeat(MAX_NOTE + 1),
        ] {
            assert!(
                tool_request(
                    "show_preview",
                    json!({"url":"http://127.0.0.1:5179/","start":start}),
                    None
                )
                .is_err(),
                "{start}"
            );
        }
        assert_eq!(
            tool_request(
                "invite_person",
                json!({"action":"capture","note":"Show me where it overlaps"}),
                None
            )
            .unwrap(),
            BridgeRequest::Person {
                action: PersonCall::Invite {
                    action: protocol::model::PersonAction::Capture,
                    note: "Show me where it overlaps".into(),
                    goal: None,
                }
            }
        );
        assert_eq!(
            tool_request(
                "invite_person",
                json!({"action":"computer_use","note":"Try it as a user would","goal":"Sign up in the app"}),
                None
            )
            .unwrap(),
            BridgeRequest::Person {
                action: PersonCall::Invite {
                    action: protocol::model::PersonAction::ComputerUse,
                    note: "Try it as a user would".into(),
                    goal: Some("Sign up in the app".into()),
                }
            }
        );
        // A goal is only for computer use.
        assert!(
            tool_request(
                "invite_person",
                json!({"action":"voice_control","note":"x","goal":"y"}),
                None
            )
            .is_err()
        );
        // The task, the run and the project are the connection's.
        for field in ["task_id", "run_id", "project_id", "artifact_id"] {
            let mut arguments = json!({"path":"a.png"});
            arguments[field] = json!("model-supplied");
            assert!(
                tool_request("show_person", arguments, None).is_err(),
                "{field}"
            );
        }
        assert!(tool_request("show_person", json!({"path":" "}), None).is_err());
        assert!(tool_request("show_person", json!({"path":"a\u{0}b"}), None).is_err());
        assert!(
            tool_request(
                "show_person",
                json!({"path":"a.png","invite":"paint"}),
                None
            )
            .is_err()
        );
        assert!(
            tool_request(
                "show_person",
                json!({"path":"a.png","caption":"x".repeat(1001)}),
                None
            )
            .is_err()
        );
        assert!(
            tool_request(
                "show_person",
                json!({"path":"a.png","caption":"a\u{1b}[2J"}),
                None
            )
            .is_err()
        );
        assert!(
            tool_request(
                "show_preview",
                json!({"url":"http://x","label":"two\nlines"}),
                None
            )
            .is_err()
        );
        assert!(
            tool_request(
                "invite_person",
                json!({"action":"capture","note":"  "}),
                None
            )
            .is_err()
        );
        assert!(
            tool_request(
                "invite_person",
                json!({"action":"take_over","note":"x"}),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn planning_tools_turn_into_work_requests_and_check_their_arguments() {
        for name in WORK_TOOLS {
            assert_eq!(refusal(name, false, false, false), None, "{name}");
        }
        assert_eq!(
            tool_request("ophio_agents", json!({}), None).unwrap(),
            BridgeRequest::Work {
                action: WorkCall::Agents
            }
        );
        assert_eq!(
            tool_request(
                "github_new_issue",
                json!({"title":"  Sign-in loops  ","body":"Steps…","labels":[" bug "],"distinct_from":[9, 4, 9]}),
                None
            )
            .unwrap(),
            BridgeRequest::Work {
                action: WorkCall::NewIssue {
                    title: "Sign-in loops".into(),
                    body: "Steps…".into(),
                    labels: vec!["bug".into()],
                    distinct_from: vec![4, 9],
                }
            }
        );
        for distinct in [json!([0]), json!((1..=21).collect::<Vec<_>>())] {
            let args = json!({"title":"t","body":"b","distinct_from":distinct});
            assert!(tool_request("github_new_issue", args, None).is_err());
        }
        let too_many: Vec<_> = (0..11).map(|i| format!("l{i}")).collect();
        assert!(
            tool_request(
                "github_new_issue",
                json!({"title":"t","body":"b","labels":too_many}),
                None
            )
            .is_err()
        );
        assert!(tool_request("github_new_issue", json!({"title":" ","body":"b"}), None).is_err());
        assert_eq!(
            tool_request(
                "github_start_issue",
                json!({"number":12,"agent":{"agent":"codex","model":"gpt-5.5"},"start":true}),
                None
            )
            .unwrap(),
            BridgeRequest::Work {
                action: WorkCall::StartIssue {
                    number: 12,
                    agent: Some(ports::bridge::AgentChoice {
                        agent: protocol::capability::HarnessKind::Codex,
                        model: Some("gpt-5.5".into()),
                        effort: None,
                    }),
                    start: true,
                    note: None,
                    reviewer: None,
                }
            }
        );
        // Asked to start it, it starts by itself unless told to wait; a
        // second agent can review.
        assert_eq!(
            tool_request(
                "github_start_issue",
                json!({"number":12,"reviewer":{"agent":"claude_code"}}),
                None
            )
            .unwrap(),
            BridgeRequest::Work {
                action: WorkCall::StartIssue {
                    number: 12,
                    agent: None,
                    start: true,
                    note: None,
                    reviewer: Some(ports::bridge::AgentChoice {
                        agent: protocol::capability::HarnessKind::ClaudeCode,
                        model: None,
                        effort: None,
                    }),
                }
            }
        );
        assert!(tool_request("github_start_issue", json!({"number":0}), None).is_err());
        assert!(
            tool_request(
                "github_start_issue",
                json!({"number":3,"agent":{"agent":"someone"}}),
                None
            )
            .is_err()
        );
        assert_eq!(
            tool_request(
                "github_start_review",
                json!({"profile":"ui_ux","max_issues":5,"then":"file_only","folder":" client "}),
                None
            )
            .unwrap(),
            BridgeRequest::Work {
                action: WorkCall::StartReview {
                    profile: Some(protocol::github::FindFocus::UiUx),
                    folder: Some("client".into()),
                    reviewer: None,
                    fixer: None,
                    max_issues: Some(5),
                    then: Some(protocol::github::FixFindings::FileOnly),
                }
            }
        );
        assert!(matches!(
            tool_request("github_start_review", json!({"profile":"default"}), None),
            Ok(BridgeRequest::Work {
                action: WorkCall::StartReview { profile: None, .. }
            })
        ));
        for bad in [
            json!({"profile":"vibes"}),
            json!({"profile":"novice_user"}),
            json!({"max_issues":0}),
            json!({"max_issues":21}),
            json!({"repo":"someone/else"}),
        ] {
            assert!(
                tool_request("github_start_review", bad.clone(), None).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn asking_another_agent_takes_the_whole_question() {
        let question = "Review a1b2c3:\n\tfn main() {}\n";
        assert_eq!(
            tool_request(
                ASK_TOOL,
                json!({"agent":{"agent":"codex","model":"gpt-6-sol"},"prompt":question,"title":"  Review the fix  "}),
                None
            )
            .unwrap(),
            BridgeRequest::Work {
                action: WorkCall::Ask {
                    agent: ports::bridge::AgentChoice {
                        agent: protocol::capability::HarnessKind::Codex,
                        model: Some("gpt-6-sol".into()),
                        effort: None,
                    },
                    prompt: question.into(),
                    title: Some("Review the fix".into()),
                }
            }
        );
        assert!(matches!(
            tool_request(
                ASK_TOOL,
                json!({"agent":{"agent":"claude_code"},"prompt":"Why?"}),
                None
            ),
            Ok(BridgeRequest::Work {
                action: WorkCall::Ask { title: None, .. }
            })
        ));
        for bad in [
            json!({"prompt":"Why?"}),
            json!({"agent":{"agent":"codex"}}),
            json!({"agent":{"agent":"codex"},"prompt":"  \n "}),
            json!({"agent":{"agent":"codex"},"prompt":"a\u{0}b"}),
            json!({"agent":{"agent":"codex"},"prompt":"x".repeat(MAX_ASK + 1)}),
            json!({"agent":{"agent":"codex"},"prompt":"Why?","title":"two\nlines"}),
            json!({"agent":{"agent":"codex"},"prompt":"Why?","title":"t".repeat(81)}),
            json!({"agent":{"agent":"someone"},"prompt":"Why?"}),
            json!({"agent":{"agent":"codex"},"prompt":"Why?","project":"elsewhere"}),
        ] {
            assert!(tool_request(ASK_TOOL, bad.clone(), None).is_err(), "{bad}");
        }
        assert!(
            tool_request(
                ASK_TOOL,
                json!({"agent":{"agent":"codex"},"prompt":"x".repeat(MAX_ASK)}),
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn a_browser_run_advertises_and_may_call_only_its_tools() {
        let names: Vec<_> = tools(false, true, false)
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        let mut expected = vec![PERMISSION_TOOL];
        expected.extend(BROWSER_TOOLS);
        expected.push(HUMAN_TOOL);
        expected.extend(PERSON_TOOLS);
        expected.extend(WORK_TOOLS);
        assert_eq!(names, expected);
        for name in &names {
            assert_eq!(refusal(name, false, true, false), None, "{name}");
        }
        for name in ["observe_target", "pointer_action", "type_text", "scroll"] {
            assert!(refusal(name, false, true, false).is_some(), "{name}");
        }
        for name in BROWSER_TOOLS.iter().chain([&HUMAN_TOOL]) {
            assert!(refusal(name, false, false, true).is_some(), "{name}");
        }
        for name in GITHUB_TOOLS {
            assert!(refusal(name, false, true, false).is_some(), "{name}");
        }
        // Every advertised tool turns into a request, and unknown arguments
        // are refused.
        assert_eq!(
            tool_request("fill_login", json!({"login":"l1","submit":true}), None).unwrap(),
            BridgeRequest::Browser {
                action: BrowserAction::FillLogin {
                    login: "l1".into(),
                    submit: true
                }
            }
        );
        assert!(tool_request("fill_login", json!({"login":"l1","password":"x"}), None).is_err());
        assert!(tool_request("browser_observe", json!({"run_id":"x"}), None).is_err());
        assert!(tool_request("browser_scroll", json!({"dy":f64::NAN}), None).is_err());
        for name in BROWSER_TOOLS {
            assert_ne!(
                tool_request(name, json!({}), None),
                Err("unknown tool"),
                "{name}"
            );
        }
    }

    #[test]
    fn a_github_run_advertises_and_may_call_only_its_tools() {
        let names: Vec<_> = tools(false, false, true)
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        let mut expected = vec![PERMISSION_TOOL];
        expected.extend(GITHUB_TOOLS);
        expected.extend(PERSON_TOOLS);
        expected.extend(ASK_TOOLS);
        assert_eq!(names, expected);
        for name in &names {
            assert_eq!(refusal(name, false, false, true), None, "{name}");
        }
        // A task working an issue doesn't plan other work, but may ask another agent.
        for name in WORK_TOOLS.iter().filter(|n| !ASK_TOOLS.contains(n)) {
            assert!(refusal(name, false, false, true).is_some(), "{name}");
        }
        for name in BROWSER_TOOLS.iter().chain([&HUMAN_TOOL, &"observe_target"]) {
            assert!(refusal(name, false, false, true).is_some(), "{name}");
        }
        // A run with both gets both.
        assert_eq!(
            tools(false, true, true).len(),
            1 + BROWSER_TOOLS.len() + 1 + GITHUB_TOOLS.len() + PERSON_TOOLS.len() + ASK_TOOLS.len()
        );
        assert_eq!(
            tool_request(
                "github_pr",
                json!({"title":" Fix the thing ","body":"Problem…"}),
                None
            )
            .unwrap(),
            BridgeRequest::Github {
                action: GithubCall::Pr {
                    title: "Fix the thing".into(),
                    body: "Problem…".into()
                }
            }
        );
        // The repository, the pull request and the branch are the task's.
        for field in ["repo", "owner", "number", "branch", "draft", "head"] {
            let mut arguments = json!({"title":"t","body":"b"});
            arguments[field] = json!("model-supplied");
            assert!(
                tool_request("github_pr", arguments, None).is_err(),
                "{field}"
            );
        }
        assert!(tool_request("github_pr", json!({"title":"","body":"b"}), None).is_err());
        assert!(tool_request("github_pr", json!({"title":"t","body":"  "}), None).is_err());
        assert!(
            tool_request(
                "github_pr",
                json!({"title":"x".repeat(257),"body":"b"}),
                None
            )
            .is_err()
        );
        assert!(tool_request("github_comment", json!({"body":"b","thread":""}), None).is_err());
        assert_eq!(
            tool_request(
                "github_issue",
                json!({"title":"Export drops rows","body":"b","distinct_from":[18]}),
                None
            )
            .unwrap(),
            BridgeRequest::Github {
                action: GithubCall::Issue {
                    title: "Export drops rows".into(),
                    body: "b".into(),
                    security: false,
                    distinct_from: vec![18],
                }
            }
        );
        assert!(
            tool_request(
                "github_issue",
                json!({"title":"t","body":"b","distinct_from":[0]}),
                None
            )
            .is_err()
        );
        assert!(tool_request("github_ready", json!({"number":3}), None).is_err());
        assert!(
            tool_request("github_merge", json!({"expected_head":"abc1234"}), None).is_err(),
            "a short id could name another commit"
        );
        assert_eq!(
            tool_request(
                "github_merge",
                json!({"expected_head":"ABCDEF0123456789abcdef0123456789abcdef01"}),
                None
            )
            .unwrap(),
            BridgeRequest::Github {
                action: GithubCall::Merge {
                    expected_head: "abcdef0123456789abcdef0123456789abcdef01".into()
                }
            }
        );
        assert!(
            tool_request(
                "github_merge",
                json!({"expected_head":"g".repeat(40)}),
                None
            )
            .is_err()
        );
        for name in GITHUB_TOOLS {
            assert_ne!(
                tool_request(name, json!({}), None),
                Err("unknown tool"),
                "{name}"
            );
        }
    }

    /// Every request of a run carries the whole tool list, so what grows
    /// here is paid again on every turn of every task. The checklist's
    /// tools took it from about 13.9k to 16.8k: what the person has to do
    /// is in every run's reach, and how to write it comes with
    /// `checklist_read` rather than here. An agent's own items, and that
    /// work it is about to do stays off the checklist, took them to 17.2k
    /// and 15.75k.
    #[test]
    fn the_tool_list_stays_within_its_budget() {
        for (github, budget) in [(false, 17_200), (true, 15_750)] {
            let size = serde_json::to_string(&tools(false, false, github))
                .unwrap()
                .len();
            assert!(size <= budget, "github {github}: {size} characters");
        }
    }
}
