// SPDX-License-Identifier: Apache-2.0
//! Line protocol between a per-run MCP bridge process and the companion.
//!
//! A managed run gets a stdio MCP server process (see
//! [`crate::harness::ToolBridge`]). That process connects to the companion's
//! local bridge socket (Unix socket / Windows named pipe, owner-only) and
//! sends one JSON object per line. The first line must be `hello` with the
//! run token the companion placed in the process environment; every later
//! request is attributed to that run by the companion. Lease, target and
//! approval identity are never taken from the model's arguments.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use protocol::control::{PointerButton, TargetPoint};
use protocol::ids::{RunId, TargetId};

/// Environment variable carrying the run token into the bridge process.
pub const RUN_TOKEN_ENV: &str = "OPHIO_BRIDGE_TOKEN";
/// Environment variable carrying the bridge socket path / pipe name.
pub const SOCKET_ENV: &str = "OPHIO_BRIDGE_SOCKET";
/// The bridge process's argument naming a file that holds the run token and
/// socket instead, for a harness that won't pass the environment through.
/// The process reads the file and deletes it.
pub const BRIDGE_FILE_ARG: &str = "--bridge-file";
/// The tools of the agent browser and the task's logins.
pub const BROWSER_TOOLS: &[&str] = &[
    "browser_open",
    "browser_back",
    "browser_tabs",
    "browser_switch_tab",
    "browser_close_tab",
    "browser_observe",
    "browser_click",
    "browser_type",
    "browser_press",
    "browser_scroll",
    "list_logins",
    "fill_login",
    "fill_code",
    "ask_owner_for_code",
];
/// The tool asking the owner to take a step, which browser runs share with
/// the computer-use specialist.
pub const HUMAN_TOOL: &str = "request_human";
/// The computer-use specialist's tools, and the only tools it has: it sees
/// and acts on its bound target, opens installed apps and asks the owner,
/// nothing else.
pub const COMPUTER_TOOLS: [&str; 7] = [
    "observe_target",
    "pointer_action",
    "key_action",
    "type_text",
    "scroll",
    OPEN_APP_TOOL,
    HUMAN_TOOL,
];
/// The tool the computer-use specialist opens an installed app with, by
/// name or kind ("Firefox", "a terminal"), the way voice control does. It
/// is held to the same agent lease as its input.
pub const OPEN_APP_TOOL: &str = "open_app";
/// The tools a task linked to GitHub posts through. The agent's own `gh`
/// reads; everything it writes on GitHub goes through these.
pub const GITHUB_TOOLS: &[&str] = &[
    "github_pr",
    "github_comment",
    "github_issue",
    "github_ready",
    "github_merge",
    "github_cannot_reproduce",
];
/// The tools an agent puts things in front of the person with, in Talk,
/// asks to be woken with rather than waiting in its turn, and checks with
/// whether it may ask them anything now. Every run with the bridge has them
/// except the computer-use specialist's.
pub const PERSON_TOOLS: &[&str] = &[
    "show_person",
    "show_preview",
    "invite_person",
    WAKE_TOOL,
    QUESTIONS_TOOL,
    "checklist_read",
    "checklist_add",
    "checklist_remove",
    "checklist_reply",
];
/// All a read-only turn answering the person's question about the checklist
/// may call, in place of every other tool: it reads the checklist and answers
/// under it, and changes nothing.
pub const ANSWER_TOOLS: &[&str] = &["checklist_read", "checklist_reply"];
/// The tool an agent asks to be woken later with, so it ends its turn
/// instead of waiting in it.
pub const WAKE_TOOL: &str = "wake_me";
/// The tool an agent checks with, before it asks the person anything,
/// whether they have questions on: they can change it while it works.
pub const QUESTIONS_TOOL: &str = "can_ask";

/// The tools an agent plans the person's work with: the agents and models
/// it may choose, another agent it asks for help, issues it files on the
/// project's repository and work it starts on them. Every run with the
/// bridge has them except the computer-use specialist's; a run working on
/// GitHub has only [`ASK_TOOLS`] of them. A read-only run (Plan, Ask) has
/// [`PROPOSE_TOOL`] and [`ACCESS_TOOL`] instead of the other work tools.
pub const WORK_TOOLS: &[&str] = &[
    "ophio_agents",
    PROPOSE_TOOL,
    ASK_TOOL,
    "github_new_issue",
    "github_start_issue",
    "github_start_review",
];
/// The tool an agent asks another agent with (Codex from Claude Code, say):
/// Ophio runs it read-only and brings its answer back.
pub const ASK_TOOL: &str = "ask_agent";
/// The tool an agent uses to propose work as a card in this chat.
pub const PROPOSE_TOOL: &str = "ophio_propose";
/// What every run with [`WORK_TOOLS`] has, working on GitHub too: the
/// agents there are, and asking one of them.
pub const ASK_TOOLS: &[&str] = &["ophio_agents", ASK_TOOL];
/// The tool an agent asks for more than it has with: changing files in its
/// project, the internet, the person's own network or the whole computer.
/// Ophio puts it to the person as one card. Every run with the bridge has
/// it except the computer-use specialist's and a turn answering on the
/// checklist; a read-only run (Plan, Ask) has it and [`PROPOSE_TOOL`].
pub const ACCESS_TOOL: &str = "request_access";
/// What a read-only run (Plan, Ask) may call: ask for more, or propose work,
/// which starts nothing until the person does. Each agent's program is
/// allowed exactly these on such a turn.
pub const READ_ONLY_TOOLS: &[&str] = &[ACCESS_TOOL, PROPOSE_TOOL];

/// Read the approved checks of the project bound to an ordinary run.
pub const CHECKS_READ_TOOL: &str = "checks_read";
/// Replace the task's open checks draft, without changing approved checks.
/// Only an ordinary Work run may call it.
pub const CHECKS_PROPOSE_TOOL: &str = "checks_propose";

/// What [`BRIDGE_FILE_ARG`]'s file holds.
#[derive(Clone, Serialize, Deserialize)]
pub struct BridgeFile {
    pub token: String,
    pub socket: String,
}
impl std::fmt::Debug for BridgeFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeFile")
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BridgeRequest {
    Hello {
        run_token: String,
    },
    /// Claude Code `--permission-prompt-tool` call, forwarded verbatim.
    PermissionPrompt {
        tool_name: String,
        input: serde_json::Value,
        tool_use_id: Option<String>,
        /// Native MCP JSON-RPC request id, JSON-encoded to preserve its type.
        /// Present even when the permission tool arguments omit tool_use_id.
        /// Correlate within the authenticated bridge connection/run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_request_id: Option<String>,
    },
    /// Capture the run's bound target for the model.
    ObserveTarget {
        max_dim: Option<u32>,
    },
    /// Pointer action at a normalized point of the observation named by
    /// `observation_revision` (its geometry revision). Stale → rejected.
    Pointer {
        action: BridgePointer,
        at: TargetPoint,
        observation_revision: u64,
    },
    Key {
        keys: Vec<String>,
    },
    TypeText {
        text: String,
    },
    Scroll {
        at: TargetPoint,
        dx: f64,
        dy: f64,
        observation_revision: u64,
    },
    /// Open an installed app named, or of the kind named, by the model.
    /// The name only chooses among the computer's own app list; it is never
    /// run.
    OpenApp {
        name: String,
    },
    /// The agent asks a person to take a step (login, 2FA, judgement).
    RequestHuman {
        reason: String,
    },
    /// Capture after-evidence for reviewed marks. The run/target remain socket-bound.
    ObserveEvidence {
        max_dim: Option<u32>,
        annotation: protocol::model::ArtifactAnnotation,
    },
    /// Something in the run's tabs of Ophio's agent browser.
    Browser {
        action: BrowserAction,
    },
    /// Something the run posts on its task's GitHub repository.
    Github {
        action: GithubCall,
    },
    /// Something the run shows the person, or asks them to do, in Talk.
    Person {
        action: PersonCall,
    },
    /// Work the run plans for the person: what it may choose from, issues
    /// it files and tasks it starts. The project comes from the run's task.
    Work {
        action: WorkCall,
    },
    /// Read the run's project's approved checks, or draft changes for its task.
    /// The connection supplies project and task identity, never model arguments.
    Checks {
        action: ChecksCall,
    },
    /// The run asks the person for more than it has ([`ACCESS_TOOL`]).
    RequestAccess {
        needs: Vec<protocol::model::AccessNeed>,
        reason: String,
    },
}

/// Checks the run reads or proposes. Drafts replace the task's open draft;
/// they never change approved checks until the person accepts them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum ChecksCall {
    Read,
    Propose {
        base_version: u64,
        items: Vec<protocol::checks::CheckDraftItem>,
    },
}

/// An agent, model and effort as an agent names them for work it starts.
/// Omitted parts are the project's defaults, then the agent's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentChoice {
    pub agent: protocol::capability::HarnessKind,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

/// What an agent plans for the person. The repository is the run's
/// project's `origin`, never one the agent names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum WorkCall {
    /// The agents, models and efforts there are, the person's defaults for
    /// the project, and its GitHub setup.
    Agents,
    /// File an issue on the project's repository.
    NewIssue {
        title: String,
        body: String,
        #[serde(default)]
        labels: Vec<String>,
        /// Issues Øphio said may say this already, which the agent read
        /// and found to be about something else.
        #[serde(default)]
        distinct_from: Vec<u64>,
    },
    /// Queue an agent to fix an open issue, in Next up.
    StartIssue {
        number: u64,
        #[serde(default)]
        agent: Option<AgentChoice>,
        /// Starts by itself once the project is free; otherwise it waits for
        /// the person to start it.
        #[serde(default)]
        start: bool,
        /// More for the agent than the issue says.
        #[serde(default)]
        note: Option<String>,
        /// Another agent checks the change before it merges.
        #[serde(default)]
        reviewer: Option<AgentChoice>,
    },
    /// Ask another agent, read-only, and have its answer come back as a
    /// message once it is done.
    Ask {
        agent: AgentChoice,
        prompt: String,
        #[serde(default)]
        title: Option<String>,
    },
    /// Start a review that files the issues it finds, and has them fixed.
    StartReview {
        #[serde(default)]
        profile: Option<protocol::github::FindFocus>,
        /// A folder of the project to look at, rather than all of it.
        #[serde(default)]
        folder: Option<String>,
        #[serde(default)]
        reviewer: Option<AgentChoice>,
        #[serde(default)]
        fixer: Option<AgentChoice>,
        #[serde(default)]
        max_issues: Option<u32>,
        #[serde(default)]
        then: Option<protocol::github::FixFindings>,
    },
    /// Put work in front of the person as a card: one task, an ordered set
    /// of them, or fixing every open issue. Each task is a suggestion in
    /// the run report's own shape (`title`, `brief`, `mode`, `issue`,
    /// `needs`), checked as one.
    Propose {
        #[serde(default)]
        tasks: Vec<serde_json::Value>,
        /// Each task waits for the one before it to finish.
        #[serde(default)]
        in_order: bool,
        /// Fix every issue Agent can take, as Run all does; `tasks` empty.
        #[serde(default)]
        all_issues: bool,
    },
}

/// What an agent puts in front of the person. Files come from the task's
/// project folder; the task and its run come from the connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum PersonCall {
    /// Show a picture or video from the project folder.
    Show {
        path: String,
        #[serde(default)]
        caption: Option<String>,
        #[serde(default)]
        invite: Option<protocol::model::MediaInvite>,
    },
    /// Open a web page the agent serves on the person's phone.
    Preview {
        url: String,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        note: Option<String>,
        /// The command line that starts the page's server in the project's
        /// folder, so the person can have it started again after the run.
        #[serde(default)]
        start: Option<String>,
        /// What the agent asks about the page, answered in the preview.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        questions: Vec<protocol::model::ApprovalQuestion>,
    },
    /// Suggest something the person can do in Ophio, with a button for it.
    Invite {
        action: protocol::model::PersonAction,
        note: String,
        /// For computer use, the goal to fill in.
        goal: Option<String>,
    },
    /// Start the task's next turn in `minutes`, for what it waits on. The
    /// agent ends its turn meanwhile, and the project is free.
    Wake {
        minutes: u32,
        reason: String,
        /// A short, plain sentence for the person, separate from the agent reminder.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        person_reason: Option<String>,
    },
    /// Whether the person has questions on now, and what to do if not.
    CanAsk,
    /// The project's checklist as the agent reads it, with every id.
    ChecklistRead,
    /// Add items to a section of the project's checklist, making the
    /// checklist if there is none, or rewrite the items with the same ids.
    /// With `title`, `summary` or `theme`, those change too.
    ChecklistAdd {
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
    },
    /// Take items off the checklist.
    ChecklistRemove { items: Vec<String> },
    /// Answer the person under an item, or with no item under the whole
    /// checklist.
    ChecklistReply {
        #[serde(default)]
        item: Option<String>,
        text: String,
    },
}

/// What an agent asks to post on GitHub. The repository and the issue or
/// pull request come from the run's task. A replacement branch is explicit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum GithubCall {
    /// Open the task's pull request from its branch, or edit the one open.
    Pr {
        title: String,
        body: String,
        /// Deliberately replace the task's branch with this pushed branch.
        /// Remembered only after the pull request is accepted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    /// Comment on the task's pull request or issue, or reply in one of the
    /// pull request's review threads.
    Comment {
        body: String,
        #[serde(default)]
        thread: Option<String>,
        /// Resolve the thread once replied in it: only one a bot started.
        #[serde(default)]
        resolve: bool,
    },
    /// File an issue for a finding. A security finding goes to the
    /// repository's private vulnerability reporting.
    Issue {
        title: String,
        body: String,
        #[serde(default)]
        security: bool,
        /// Issues Øphio said may say this already, which the agent read
        /// and found to be about something else.
        #[serde(default)]
        distinct_from: Vec<u64>,
    },
    /// Mark the task's draft pull request ready for review.
    Ready,
    /// Merge the task's pull request at the head the agent last checked.
    Merge { expected_head: String },
    /// Say on the task's issue that it could not be reproduced: what was
    /// tried, and whether the code fixes it already.
    CannotReproduce {
        tried: String,
        #[serde(default)]
        already_fixed: bool,
    },
}

/// What an agent asks of its tabs in the agent browser. Controls are named by
/// the `ref` the last look at the page gave them; logins by their id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum BrowserAction {
    Open {
        url: String,
        #[serde(default)]
        new_tab: bool,
    },
    Back,
    Tabs,
    SwitchTab {
        tab: String,
    },
    CloseTab {
        #[serde(default)]
        tab: Option<String>,
    },
    Observe,
    Click {
        control: String,
    },
    Type {
        control: String,
        text: String,
        #[serde(default)]
        replace: bool,
    },
    Press {
        key: String,
    },
    Scroll {
        dy: f64,
        #[serde(default)]
        control: Option<String>,
    },
    /// The logins the task may use, without their passwords.
    ListLogins,
    FillLogin {
        login: String,
        #[serde(default)]
        submit: bool,
    },
    /// A code from the login's setup key.
    FillCode {
        login: String,
        #[serde(default)]
        submit: bool,
    },
    /// A code the owner reads from their phone, email or text and types in.
    AskOwnerForCode {
        login: String,
        why: String,
        #[serde(default)]
        submit: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BridgePointer {
    Move,
    Click { button: PointerButton, count: u8 },
    Down { button: PointerButton },
    Up { button: PointerButton },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum BridgeResponse {
    Welcome {
        run_id: RunId,
        target_id: Option<TargetId>,
        computer_use: bool,
        /// The run may use the agent browser and its logins.
        #[serde(default)]
        browser: bool,
        /// The run may post on its task's GitHub repository.
        #[serde(default)]
        github: bool,
        /// The run only reads the checklist and answers on it
        /// (`ANSWER_TOOLS`).
        #[serde(default)]
        checklist_answers: bool,
        /// The run only reads (Plan, Ask): its tools are [`ACCESS_TOOL`]
        /// and [`PROPOSE_TOOL`].
        #[serde(default)]
        read_only: bool,
        /// The ordinary run has a project whose approved checks it may read.
        /// Work runs may also draft changes, even when no checks exist yet.
        #[serde(default)]
        checks: bool,
    },
    /// Claude permission-prompt-tool answer, already in the shape Claude Code
    /// expects (`{"behavior":"allow","updatedInput":…}` or
    /// `{"behavior":"deny","message":…}`).
    Permission {
        decision: serde_json::Value,
    },
    Observation {
        /// Base64 image bytes.
        image_base64: String,
        media_type: String,
        width: u32,
        height: u32,
        geometry_revision: u64,
    },
    Done,
    /// The person paused the agent while an action of its waited, then
    /// handed the screen back: the action wasn't taken, and this is the
    /// screen now, for the model to act again from.
    Resumed {
        text: String,
        /// Base64 image bytes.
        image_base64: String,
        media_type: String,
        width: u32,
        height: u32,
        geometry_revision: u64,
    },
    /// The person took the step or declined; text is shown to the model.
    HumanResult {
        text: String,
    },
    /// What a browser action did, for the model, and a PNG of the page when
    /// it looked.
    BrowserResult {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_base64: Option<String>,
    },
    /// What a GitHub post did, for the model.
    GithubResult {
        text: String,
    },
    /// What the person now has in Talk, for the model.
    PersonResult {
        text: String,
    },
    /// What planning the work did, for the model.
    WorkResult {
        text: String,
    },
    /// What came of asking for more access, for the model.
    AccessResult {
        text: String,
    },
    /// Approved check definitions, or the outcome of replacing the task's draft.
    /// Validation mistakes use Error so the model can correct the same call.
    ChecksResult {
        text: String,
    },
    /// Which app opened, for the model.
    AppOpened {
        text: String,
    },
    /// Refused: no lease, stale observation, stopped, observation withdrawn…
    Refused {
        reason: String,
    },
    Error {
        message: String,
    },
}

/// Ophio's side: handles requests from an authenticated bridge connection.
#[async_trait]
pub trait BridgeHandler: Send + Sync + 'static {
    /// Validate a run token; returns the binding or `None` to close.
    async fn bind(&self, run_token: &str) -> Option<BridgeBinding>;
    async fn handle(&self, binding: &BridgeBinding, request: BridgeRequest) -> BridgeResponse;
    /// The bridge connection closed.
    async fn unbound(&self, binding: &BridgeBinding);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeBinding {
    pub run_id: RunId,
    pub target_id: Option<TargetId>,
    pub computer_use: bool,
    /// The run may use the agent browser and its logins.
    pub browser: bool,
    /// The run may post on its task's GitHub repository.
    pub github: bool,
    /// The run only reads the checklist and answers on it
    /// (`ANSWER_TOOLS`): it answers the person's question for an agent that
    /// was put away.
    pub checklist_answers: bool,
    /// The run only reads (Plan, Ask): it may call [`ACCESS_TOOL`] and
    /// [`PROPOSE_TOOL`].
    pub read_only: bool,
    /// The ordinary run has a project. This allows reading approved checks,
    /// including an empty list, and proposing drafts in Work runs only.
    pub checks: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_bridge_welcome_does_not_enable_checks() {
        let value = serde_json::json!({
            "result": "welcome", "run_id": RunId::new(), "target_id": null,
            "computer_use": false,
        });
        assert!(matches!(
            serde_json::from_value::<BridgeResponse>(value).unwrap(),
            BridgeResponse::Welcome { checks: false, .. }
        ));
    }
}
