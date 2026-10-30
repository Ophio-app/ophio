// SPDX-License-Identifier: Apache-2.0
//! Wire contract shared by the phone client, chat channel adapters and the host
//! companion.
//!
//! The companion is authoritative for admitted commands, tasks, runs, grants,
//! approvals and the event journal. The phone is authoritative only for unsent
//! drafts. Every effectful request therefore travels as a [`CommandEnvelope`]
//! with a client-generated [`CommandId`]; the companion deduplicates on that
//! identity and answers with a [`CommandReceipt`]. Durable facts flow back as
//! [`EventEnvelope`]s carrying a monotonic journal sequence.
//!
//! Transport separation is part of the contract: control/events, terminal
//! bytes, media frames and file transfers use separate connections so that a
//! congested video stream can never queue a Stop behind frames.
//!
//! Types derive `schemars::JsonSchema` (published schema) and `ts_rs::TS`
//! (generated TypeScript bindings for the client UI).

/// Largest integer exactly representable by the phone's JavaScript `number`.
/// All wire integers must stay within this bound; their JSON encoding is numeric.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub mod access;
pub mod accounts;
pub mod agent_logins;
pub mod agent_preferences;
pub mod agent_updates;
pub mod alerts;
pub mod annotation;
pub mod auth;
pub mod capability;
pub mod checklist;
pub mod command;
pub mod contribution;
pub mod control;
pub mod disabled;
pub mod error;
pub mod event;
pub mod github;
pub mod ids;
pub mod integer;
pub mod launchers;
pub mod library;
pub mod media;
pub mod model;
pub mod notifications;
pub mod observed;
pub mod preview;
pub mod project_folders;
pub mod queue;
pub mod reports;
pub mod review;
pub mod system;
pub mod terminal;
pub mod time;
pub mod transfer;
pub mod version;
pub mod windows;

pub use command::{Command, CommandEnvelope, CommandReceipt, ReceiptStatus};
pub use error::{ErrorCode, ProtocolError};
pub use event::{Event, EventEnvelope};
pub use ids::*;
pub use version::{PROTOCOL_MAJOR, PROTOCOL_MINOR, ProtocolVersion};

/// Messages the client sends on the control connection.
#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ClientFrame {
    /// First frame on every connection, before authentication completes.
    Hello(auth::Hello),
    /// Proof of possession of the enrolled device key.
    Authenticate(auth::Authenticate),
    /// Submit an effectful command. The companion answers with `receipt`.
    Command(Box<CommandEnvelope>),
    /// Read-only query; answered with `query_result`.
    Query(QueryEnvelope),
    /// Subscribe to the durable journal from a known position.
    Subscribe(Subscribe),
    /// Liveness probe for the control session.
    Ping {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        nonce: u64,
    },
    /// Human input while holding a lease. Not journaled; acknowledged with
    /// `input_ack`. Rejected if the lease, generation or geometry revision is
    /// stale.
    Input(control::InputEnvelope),
    /// Renew a human lease. Sent only while the control view is foregrounded
    /// and supervised; if renewals stop, the input worker closes the lease on
    /// its own clock.
    RenewLease {
        lease_id: LeaseId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        generation: u64,
    },
}

/// Messages the companion sends on the control connection.
#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ServerFrame {
    Challenge(auth::Challenge),
    Authenticated(auth::Authenticated),
    Receipt(CommandReceipt),
    QueryResult(QueryResult),
    Event(EventEnvelope),
    /// The requested journal position is no longer retained, or an event after
    /// it is too large to deliver; the client must load a fresh snapshot and
    /// show an explicit observation gap.
    JournalGap {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        requested_from: u64,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        oldest_retained: u64,
    },
    /// This subscription's bounded transport queue overflowed. The control
    /// connection remains usable for commands. Resubscribe after this last
    /// delivered sequence; a subsequent `journal_gap` requires a snapshot.
    /// Unlike `journal_gap`, this does not claim history was compacted.
    SubscriptionClosed {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        after_seq: u64,
        reason: ProtocolError,
    },
    /// Live tail reached; subsequent events are real time.
    CaughtUp {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        head: u64,
    },
    Pong {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        nonce: u64,
    },
    InputAck(control::InputAck),
    /// Non-durable live update (streaming text); never replayed.
    Live(event::LiveEvent),
    /// Connection-level failure; the connection closes after this frame.
    Fatal(ProtocolError),
}

#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct Subscribe {
    /// Last journal sequence the client has applied, or `None` for a fresh
    /// snapshot followed by the live tail.
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub after_seq: Option<u64>,
}

#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct QueryEnvelope {
    pub query_id: QueryId,
    pub query: Query,
}

/// Read-only requests. Queries never change host state and never require
/// idempotency handling.
#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum Query {
    /// Resolve a user-opened contribution reference through the paired host.
    /// Neither record grants trust or execution authority.
    ContributionDelivery {
        packet_json: String,
        handoff_url: String,
    },
    ContributionCapabilities,
    ContributionOperation {
        operation_id: String,
    },
    /// Read a pinned rule as inert text before granting it trust.
    ContributionPolicy {
        operation_id: String,
        path: String,
    },
    /// Current projection (host, projects, tasks, runs, approvals, control,
    /// attention) within one frame, plus the journal head it reflects.
    Snapshot {
        /// The client reads each pending approval the snapshot names in
        /// `withheld_approvals` with `Query::Approval`. A client that leaves
        /// this out gets every pending approval whole, or, when they do not
        /// fit one frame, `IncompatibleVersion` rather than a snapshot
        /// missing one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reads_withheld: Option<bool>,
    },
    /// Look up what happened to a command the client may have sent.
    CommandStatus {
        command_id: CommandId,
    },
    /// Conversation/activity items for a task, newest last.
    TaskTimeline {
        task_id: TaskId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        before_seq: Option<u64>,
        limit: u32,
    },
    /// Folder names for project registration. Requires Manage access; never
    /// reads files or registers anything. Advertised as `project_folders`.
    ProjectFolders {
        path: Option<String>,
        after: Option<String>,
        show_hidden: bool,
        search: String,
    },
    /// Harness catalog: installed adapters, readiness, models and efforts.
    Harnesses,
    /// Capture targets (outputs/windows) available on the host session.
    ControlTargets,
    /// Library roots and directory listing.
    LibraryList {
        root_id: LibraryRootId,
        dir: String,
    },
    LibraryRead {
        root_id: LibraryRootId,
        path: String,
    },
    LibrarySearch {
        root_id: Option<LibraryRootId>,
        text: String,
        limit: u32,
    },
    /// Known instruction sources for a task or project (instruction inspector).
    Instructions {
        project_id: ProjectId,
        task_id: Option<TaskId>,
    },
    /// Terminal sessions the device may attach to.
    Terminals,
    /// Registered preview endpoints for a project.
    Previews {
        project_id: ProjectId,
    },
    /// Every project's checklist at a glance. Advertised as `checklist_v1`.
    Checklists,
    /// A project's checklist whole.
    Checklist {
        project_id: ProjectId,
    },
    /// Where the computer puts checklist files for the person, and where it
    /// could.
    FileDelivery,
    /// The project's local Git branches, read now.
    ProjectBranches {
        project_id: ProjectId,
    },
    /// The project's `FINDINGS.md`, where a review without GitHub writes
    /// what it found and its fixers tick it off, read now. Advertised as
    /// `findings_v1`.
    Findings {
        project_id: ProjectId,
    },
    /// Agents and skills a task can start from, read from the computer now:
    /// the person's own and, with `project_id`, the project's.
    Catalog {
        project_id: Option<ProjectId>,
    },
    /// Readiness checks (network, terminal, graphics, accounts).
    Readiness,
    /// How busy the computer is now, with the recent history. Advertised
    /// as `system_stats_v1`.
    SystemStats,
    /// Paired devices and channel identities with their grants.
    Devices,
    /// Who asked for a pending chat link code, and from which chat, before the
    /// owner approves or refuses it.
    ChannelLink {
        link_code: String,
    },
    /// Transfer records.
    Transfers,
    /// Next-up queue for a project.
    Queue {
        project_id: ProjectId,
    },
    /// Artifact metadata.
    Artifact {
        artifact_id: ArtifactId,
    },
    LibraryTrash,
    LibraryTrashRead {
        trash_id: String,
    },
    /// Every Markdown file agents are given on the computer, sorted by what
    /// it is for and where it applies.
    AgentFiles,
    /// The skills, agents and files disabled in Ophio, newest first.
    /// Advertised as [`disabled::FEATURE`].
    DisabledInOphio,
    ReusableDraft {
        task_id: TaskId,
    },
    UploadDestination {
        name: String,
        destination: transfer::TransferDestination,
    },
    TaskArtifacts {
        task_id: TaskId,
        before: Option<ArtifactId>,
        limit: u32,
    },
    /// The pictures and videos agents showed in a project's tasks, newest
    /// first, however long ago those tasks ended. Each stays on the computer
    /// a week after the person last opened it. Advertised as
    /// `project_shown_v1`.
    ProjectShown {
        project_id: ProjectId,
        before: Option<ArtifactId>,
        limit: u32,
    },
    /// Whether Øphio keeps its own copies of the agents up to date. Advertised
    /// as `agent_updates_v1`.
    AgentUpdates,
    /// How the person likes agents to work with them. Advertised as
    /// `agent_preferences_v1`.
    AgentPreferences,
    /// Which alerts reach the person's phones. Advertised as
    /// `notification_settings_v1`.
    NotificationSettings,
    /// The person's problem reports, their settings and how the intake is
    /// reached. Advertised as `reports_v1`.
    Reports,
    /// One report with exactly what was sent.
    Report {
        report_id: ReportId,
    },
    /// Untrusted reporter-supplied code in the owner's inbox, if present.
    /// Advertised as `report_inbox_code_v1`.
    InboxReportCode {
        report_id: ReportId,
    },
    /// The computer's part of a report about to be sent, exactly as it would
    /// go, for "See all". `salt` names ids as the phone's part does (see
    /// [`reports::pseudonym`]); it never leaves the person's devices.
    ReportPreview {
        salt: String,
        #[serde(default)]
        extras: Vec<reports::ReportExtra>,
    },
    Voice {
        session_id: Option<String>,
    },
    Windows {
        search: String,
    },
    ObservedSessions,
    ReusableFormats {
        task_id: TaskId,
        harness: capability::HarnessKind,
    },
    LibraryRecoveryList,
    LibraryRecoveryRead {
        root_id: LibraryRootId,
        recovery_id: String,
    },
    ProjectStages {
        project_id: ProjectId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        before_revision: Option<u64>,
        limit: u32,
    },
    /// A project's tasks from `from` on, newest first, as a snapshot carries
    /// them: at most `limit`, and as many as fit one frame.
    TaskHistory {
        project_id: ProjectId,
        from: model::TaskCursor,
        limit: u32,
    },
    /// One task as a snapshot carries it, whether or not the snapshot does.
    Task {
        task_id: TaskId,
    },
    /// One run whole, with the diffs its agent reported, and the approvals
    /// and attention about it.
    Run {
        run_id: RunId,
    },
    /// One approval whole: one a snapshot, task, page or run named as
    /// withheld to stay within its frame.
    Approval {
        approval_id: ApprovalId,
    },
    /// The computer's accounts, the sign-in in progress and their history.
    /// Advertised as `accounts_v1`.
    Accounts,
    /// The computer's agent logins and their history, without secrets.
    /// Advertised as `agent_logins_v1`.
    AgentLogins,
    /// One login's password, for the owner to read after unlocking the
    /// phone. Each answer is recorded in the history.
    AgentLoginPassword {
        login_id: String,
    },
    /// A project's GitHub settings and what the computer reads about its
    /// repository. Advertised as `github_v1`.
    GithubProject {
        project_id: ProjectId,
    },
    /// A project's open issues or pull requests. `filter` matches titles.
    GithubItems {
        project_id: ProjectId,
        list: github::GithubItemKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        filter: Option<String>,
    },
    /// One pull request with its checks and review threads.
    GithubPr {
        project_id: ProjectId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
    },
}

#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct QueryResult {
    pub query_id: QueryId,
    pub result: Result<QueryData, ProtocolError>,
}

#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
// Receipts intentionally remain inline for Rust API compatibility. Transfer
// recovery evidence adds two strings; this bounded control response is not a
// high-rate queue item and does not warrant reshaping existing receipt fields.
#[allow(clippy::large_enum_variant)]
pub enum QueryData {
    ContributionDelivery {
        handoff_json: String,
    },
    ContributionCapabilities {
        capabilities: contribution::ContributionCapabilities,
    },
    ContributionOperation {
        operation: contribution::ContributionView,
    },
    ContributionPolicy {
        document: contribution::ContributionPolicyDocument,
    },
    Snapshot(Box<model::Snapshot>),
    CommandStatus {
        receipt: Option<CommandReceipt>,
        /// Latest durable execution stage. The receipt remains the original
        /// admission result, so command retries cannot change its meaning.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        stage: Option<command::CommandStage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        result: Option<Box<command::CommandResult>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        error: Option<Box<ProtocolError>>,
    },
    TaskTimeline {
        items: Vec<model::TimelineItem>,
        has_more: bool,
    },
    ProjectFolders {
        listing: project_folders::ProjectFolderListing,
    },
    Harnesses {
        harnesses: Vec<capability::HarnessInfo>,
    },
    ControlTargets {
        targets: Vec<control::ControlTarget>,
    },
    LibraryList {
        listing: library::DirListing,
    },
    LibraryRead {
        file: library::FileContent,
    },
    LibrarySearch {
        hits: Vec<library::SearchHit>,
    },
    Instructions {
        sources: Vec<library::InstructionSource>,
    },
    Terminals {
        terminals: Vec<terminal::TerminalInfo>,
    },
    Previews {
        previews: Vec<model::PreviewEndpoint>,
    },
    Checklists {
        checklists: Vec<checklist::ChecklistSummary>,
    },
    /// None when the project has no checklist.
    Checklist {
        checklist: Option<Box<checklist::Checklist>>,
    },
    FileDelivery {
        view: checklist::FileDeliveryView,
    },
    /// None when the project folder is not a Git repository Ophio can read.
    ProjectBranches {
        listing: Option<model::BranchListing>,
    },
    /// None when the project has no `FINDINGS.md`.
    Findings {
        text: Option<String>,
    },
    Catalog {
        catalog: launchers::Catalog,
    },
    Readiness {
        report: model::ReadinessReport,
    },
    SystemStats {
        stats: Box<system::SystemStats>,
    },
    Devices {
        devices: Vec<auth::DeviceRecord>,
        identities: Vec<auth::ChannelIdentity>,
    },
    ChannelLink {
        link: auth::PendingChannelLink,
    },
    Transfers {
        transfers: Vec<transfer::TransferRecord>,
    },
    Queue {
        queue: queue::QueueView,
    },
    Artifact {
        artifact: model::ArtifactMeta,
    },
    LibraryTrash {
        items: Vec<library::LibraryTrashItem>,
    },
    AgentFiles {
        files: Vec<library::AgentFile>,
        /// More were kept than are listed.
        truncated: bool,
    },
    DisabledInOphio {
        items: Vec<disabled::DisabledItem>,
    },
    LibraryTrashRead {
        file: library::FileContent,
    },
    ReusableDraft {
        text: String,
    },
    UploadDestination {
        check: transfer::UploadDestinationCheck,
    },
    TaskArtifacts {
        history: model::TaskArtifactHistory,
    },
    ProjectShown {
        history: model::TaskArtifactHistory,
    },
    AgentUpdates {
        view: agent_updates::AgentUpdatesView,
    },
    AgentPreferences {
        view: agent_preferences::AgentPreferences,
    },
    NotificationSettings {
        view: notifications::NotificationSettings,
    },
    Reports {
        view: reports::ReportsView,
    },
    /// None when the computer holds no such report.
    Report {
        detail: Option<Box<reports::ReportDetail>>,
    },
    InboxReportCode {
        code: Option<reports::InboxReportCode>,
    },
    ReportPreview {
        part: Box<reports::ComputerPart>,
    },
    Voice {
        view: Box<voice::VoiceView>,
    },
    Windows {
        catalog: windows::WindowCatalog,
    },
    ObservedSessions {
        view: observed::ObservedSessions,
    },
    ReusableFormats {
        native_version: Option<String>,
        formats: Vec<library::ReusableFormat>,
    },
    LibraryRecoveryList {
        items: Vec<library::LibraryRecoveryItem>,
    },
    LibraryRecoveryRead {
        recovery: Box<library::LibraryRecovery>,
    },
    ProjectStages {
        history: project::ProjectStageHistory,
    },
    TaskHistory {
        records: model::TaskRecords,
        /// Where the next page begins; absent after the oldest task.
        next: Option<model::TaskCursor>,
    },
    Task {
        records: model::TaskRecords,
    },
    Run {
        run: Box<model::Run>,
        approvals: Vec<model::Approval>,
        attention: Vec<model::AttentionItem>,
        /// Pending approvals left out to stay within one frame, each read
        /// alone with `Query::Approval`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(as = "Option<Vec<ApprovalId>>", optional)]
        withheld_approvals: Vec<ApprovalId>,
    },
    Approval {
        approval: Box<model::Approval>,
    },
    Accounts {
        view: Box<accounts::AccountsView>,
    },
    AgentLogins {
        view: Box<agent_logins::AgentLoginsView>,
    },
    AgentLoginPassword {
        password: accounts::Secret,
    },
    GithubProject {
        view: Box<github::GithubProjectView>,
    },
    GithubItems {
        items: github::GithubItems,
    },
    GithubPr {
        pr: Box<github::GithubPrView>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::LiveEvent;
    use crate::ids::{RunId, TaskId};

    #[test]
    fn live_frame_keeps_both_tags() {
        let frame = ServerFrame::Live(LiveEvent::AgentTextDelta {
            task_id: TaskId::new(),
            run_id: RunId::new(),
            message_key: "m1".into(),
            text: "Hel".into(),
        });
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "live");
        assert_eq!(json["kind"], "agent_text_delta");
        let back: ServerFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }
}

pub mod voice;

pub mod project;
