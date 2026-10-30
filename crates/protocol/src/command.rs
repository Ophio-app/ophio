// SPDX-License-Identifier: Apache-2.0
//! Effectful commands and their receipts.
//!
//! Every command carries a client-generated [`CommandId`]. The companion
//! records the id before acting and answers a retry of the same id with the
//! original receipt (`duplicate`), so a phone that lost its connection after
//! sending can safely resend. A receipt means *admitted by the host*; what the
//! native harness or desktop later did is reported through journal events.

use serde::{Deserialize, Serialize};

use crate::annotation::AnnotationBundle;
use crate::auth::Grants;
use crate::capability::HarnessKind;
use crate::control::LeaseView;
use crate::error::ProtocolError;
use crate::ids::*;
use crate::library::{NewlineStyle, SaveOutcome};
use crate::model::{ArtifactMeta, CarriedPlan, Mission, TaskKind, TaskMode};
use crate::queue::QueueAuthorization;
use crate::terminal::TerminalInfo;
use crate::time::Timestamp;
use crate::transfer::{TransferDestination, TransferRecord};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CommandEnvelope {
    pub command_id: CommandId,
    /// Client wall-clock time, informational only.
    pub issued_at: Timestamp,
    /// Optimistic-concurrency guard for commands that edit a revisioned
    /// object (task, queue, approval). A mismatch is rejected with
    /// `stale_revision` and nothing changes.
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub expected_revision: Option<u64>,
    /// After this instant the host must not start acting on the command.
    /// It is on the sender's clock, so a host that reads `expires_in_ms`
    /// ignores it; a sender gives both to keep a host that doesn't.
    pub expires_at: Option<Timestamp>,
    /// How long after the host receives the command it may still start
    /// acting on it. The host sets the deadline on its own clock, so it means
    /// the same whether or not the sender's clock agrees with the host's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub expires_in_ms: Option<u64>,
    pub body: Command,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "command", rename_all = "snake_case")]
#[ts(export)]
// Keep the command wire API direct; boxing changes every producer for a small memory saving.
#[allow(clippy::large_enum_variant)]
pub enum Command {
    /// Store inert records for local review. Does not fetch or register a project.
    ReviewContribution {
        input: Box<crate::contribution::ContributionInput>,
    },
    /// Explicit local trust, also required afresh on resume/reopen.
    PrepareContribution {
        operation_id: String,
        approval: Box<crate::contribution::ContributionApproval>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        workspace_choice: Option<crate::contribution::ContributionWorkspaceChoice>,
    },
    /// Revoke pending acquisition/trust; preserve every completed project and file.
    CancelContribution {
        operation_id: String,
    },
    /// Choose a whole writable Library root for future contribution workspaces.
    SetContributionRoot {
        root_id: LibraryRootId,
    },
    // --- Projects -------------------------------------------------------
    AddProject {
        host_path: String,
        name: Option<String>,
    },
    UpdateProjectDefaults {
        project_id: ProjectId,
        harness: Option<HarnessKind>,
        model: Option<String>,
        effort: Option<String>,
    },
    /// Make a one-off folder an ordinary project, listed with the others.
    PromoteOneOff {
        project_id: ProjectId,
    },
    /// Make a new agent in Ophio's agents folder, ready to edit in Library.
    CreateAgent {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        description: Option<String>,
    },
    /// Make the person's own copy of the built-in skill `id` in Ophio's
    /// skills folder, ready to edit in Library; tasks start from it from then
    /// on. A copy already made is the one kept.
    CopySetupSkill {
        id: String,
    },
    /// For a copied built-in skill Øphio has a newer version of: keep the
    /// copy as it is and stop offering this version, or move the copy to
    /// Library's recently deleted and copy the new version in its place.
    ChooseSetupSkillVersion {
        id: String,
        keep_mine: bool,
    },
    /// Disable a skill, agent or agent file in Ophio, or enable it again:
    /// while disabled, no agent Ophio runs is given it. `host_path` is the
    /// path it was listed with; it must be listed as one that can be, of
    /// `kind`. Enabling one that is no longer there is allowed. The file
    /// itself is never changed. Advertised as
    /// [`crate::disabled::FEATURE`].
    SetDisabledInOphio {
        kind: crate::disabled::DisabledKind,
        host_path: String,
        disabled: bool,
    },
    /// Forget the project in the companion. Never deletes files.
    RemoveProject {
        project_id: ProjectId,
    },
    /// Forget a one-off and delete its folder, with everything in it, from
    /// the computer. Its tasks are archived.
    DeleteOneOff {
        project_id: ProjectId,
    },

    // --- Tasks and runs -------------------------------------------------
    CreateTask {
        project_id: ProjectId,
        kind: TaskKind,
        /// Omission preserves the existing Work behavior.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        mode: Option<TaskMode>,
        title: Option<String>,
        prompt: String,
        bundle: Option<Box<AnnotationBundle>>,
        harness: HarnessKind,
        model: Option<String>,
        effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        launch_policy: Option<crate::model::LaunchPolicy>,
        /// Git branch to tell a coding agent to work on, or to make. Ophio
        /// never creates or checks it out. Omission leaves it to the agent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        branch: Option<crate::model::TaskBranch>,
        /// An agent, skill or scratch pad to start from. Omission is an
        /// ordinary task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        start: Option<Box<crate::launchers::TaskStart>>,
        /// A one-off folder to work in instead of the existing project
        /// `project_id` names. Omission works in that project.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        place: Option<crate::launchers::TaskPlace>,
        /// Whether the task's Work runs may use Ophio's agent browser and the
        /// logins the owner lets them. Omission gives a coding task the
        /// browser.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        browser: Option<bool>,
        /// Whether this is a conversation for Talk rather than a piece of
        /// work. Omission is a task. Advertised as `conversations_v1`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        conversation: Option<bool>,
        /// What a conversation's agent may read, change and reach. Omission
        /// is its own project, read-only, and the internet. Advertised as
        /// `conversation_access_v1`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        access: Option<crate::access::ConversationAccess>,
        /// The GitHub work the task is for, in a project with the GitHub
        /// workflow on. Omission links nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        github: Option<Box<crate::github::GithubLink>>,
        mission: Option<Mission>,
        /// Required for computer-use tasks.
        target_id: Option<TargetId>,
        placement: TaskPlacement,
        /// Whether the person chose the agent, model and effort themselves,
        /// taken as chosen. Omission lets the project's own model and effort
        /// fill in for its usual agent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_chosen: Option<bool>,
        /// What the task is for in a review, which gives it the project's
        /// trusted review rules. Omission is an ordinary task, which never
        /// gets them. Advertised as `review_rules_v1`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        review: Option<crate::review::ReviewRole>,
    },
    /// Send a follow-up or steering message to a task.
    SendMessage {
        task_id: TaskId,
        text: String,
        bundle: Option<Box<AnnotationBundle>>,
        delivery: MessageDelivery,
    },
    /// Stop the task's active run (native interrupt). Never queued behind
    /// media or other traffic. The outcome arrives as an intervention receipt.
    StopTask {
        task_id: TaskId,
        #[serde(default)]
        #[ts(as = "Option<bool>", optional)]
        force: bool,
    },
    /// Start a new run continuing an interrupted/completed task.
    ResumeTask {
        task_id: TaskId,
        /// Explicit reviewed transition from a completed Plan to Work, or
        /// from a conversation to Work, which makes it a task. Omit to keep
        /// the mode.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        mode: Option<TaskMode>,
        /// The model for this turn and the task's later ones, chosen
        /// between turns. Omission keeps the task's model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        model: Option<String>,
        /// The effort for this turn and later ones. With `model`, omission
        /// is that model's own default; alone, it keeps the task's model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        effort: Option<String>,
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        bundle: Option<Box<AnnotationBundle>>,
        /// The earlier run's plan that `text` ends with, when this resume
        /// carries a reviewed Plan into Work. Needs `mode: work`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        carried_plan: Option<CarriedPlan>,
        /// Run this one turn in Ask, with the task's own session, model and
        /// effort, while the task keeps its mode: the agent can read and
        /// answer but change nothing, and no writer is needed. The task's
        /// state is put back when the turn ends. Not with `mode`,
        /// `carried_plan`, `model` or `effort`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        read_only: bool,
    },
    RenameTask {
        task_id: TaskId,
        title: String,
        /// The title this rename replaces. When given, the rename is refused
        /// if the task is no longer called that, and the task's revision,
        /// which every agent step moves on, is not checked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        expected_title: Option<String>,
    },
    ArchiveTask {
        task_id: TaskId,
    },
    /// Bring an archived task back to the state it was archived from, as
    /// the Undo just after archiving it. Taken without the task's revision:
    /// only an archived task comes back. What archiving let go of, such as
    /// a wait in Next up, stays let go. Advertised as `unarchive_task_v1`.
    UnarchiveTask {
        task_id: TaskId,
    },
    /// Change what a conversation's agent may read, change and reach, at
    /// any time, mid-turn included. Taken without the task's revision, which
    /// every agent step moves on. Advertised as `conversation_access_v1`.
    SetConversationAccess {
        task_id: TaskId,
        access: crate::access::ConversationAccess,
    },
    /// Resolve a task whose outcome is uncertain, after the person has looked.
    ReconcileTask {
        task_id: TaskId,
        resolution: Reconciliation,
    },
    /// Record a person's check of this run's helper unknowns only.
    CheckHelpers {
        run_id: RunId,
    },
    ResolveApproval {
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    },
    AcknowledgeAttention {
        attention_id: AttentionId,
    },

    // --- Next up (per-project queue) ------------------------------------
    QueueMove {
        item_id: QueueItemId,
        position: u32,
    },
    QueueRemove {
        item_id: QueueItemId,
    },
    QueueSetDependencies {
        item_id: QueueItemId,
        depends_on: Vec<QueueItemId>,
    },
    QueueSetAuthorization {
        item_id: QueueItemId,
        authorization: QueueAuthorization,
    },
    QueuePause {
        project_id: ProjectId,
    },
    QueueResume {
        project_id: ProjectId,
    },

    // --- Desktop control ------------------------------------------------
    /// Ask for a human input lease on one target at the geometry revision the
    /// phone is displaying.
    RequestHumanLease {
        target_id: TargetId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        geometry_revision: u64,
    },
    ReleaseLease {
        lease_id: LeaseId,
    },
    /// Stop all managed input on the target (human or agent) without stopping
    /// the task. Watching continues.
    PauseInput {
        target_id: TargetId,
    },
    /// Revoke an agent lease and grant the requesting human one, atomically
    /// from the worker's point of view.
    TakeOver {
        target_id: TargetId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        geometry_revision: u64,
    },
    /// Give managed input back to a computer-use run after a takeover/pause.
    ReturnToAgent {
        target_id: TargetId,
        run_id: RunId,
    },
    /// Private human step: stop agent input and managed model observation,
    /// report coverage, then (only if coverage allows) let the person act.
    StartPrivateStep {
        target_id: TargetId,
    },
    EndPrivateStep {
        target_id: TargetId,
    },
    WithdrawObservation {
        run_id: RunId,
        target_id: TargetId,
    },
    LockDesktop,
    /// Unlock the computer with its account's password, which the computer
    /// checks. It is never kept: a stored copy of this command holds it
    /// withheld.
    UnlockDesktop {
        password: crate::accounts::Secret,
    },
    /// Capture a still of a target as an artifact (for annotation).
    CaptureTarget {
        target_id: TargetId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        evidence: Option<crate::model::ArtifactLink>,
    },

    // --- Library --------------------------------------------------------
    LibrarySave {
        root_id: LibraryRootId,
        path: String,
        text: String,
        /// Revision the edit was based on (from `FileContent.revision`).
        base_revision: String,
        /// Preserve the file's original newline style and BOM on save.
        newline: NewlineStyle,
        bom: bool,
    },
    LibraryCreate {
        root_id: LibraryRootId,
        path: String,
        text: String,
    },
    LibraryRename {
        root_id: LibraryRootId,
        from: String,
        to: String,
        base_revision: String,
    },
    /// Moves to the companion's recoverable trash, not a hard delete.
    LibraryDelete {
        root_id: LibraryRootId,
        path: String,
        base_revision: String,
    },
    /// Turn a task into a reusable draft in the Drafts root. Never writes into
    /// an active skills/agents folder.
    MakeReusable {
        task_id: TaskId,
        kind: ReusableKind,
        name: String,
    },

    // --- Terminals ------------------------------------------------------
    OpenTerminal {
        project_id: Option<ProjectId>,
        /// Optional absolute working directory. Older companions use the project directory.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        cwd: Option<String>,
        /// Start a harness CLI interactively instead of a shell.
        harness: Option<HarnessKind>,
        cols: u16,
        rows: u16,
    },
    CloseTerminal {
        terminal_id: TerminalId,
    },

    // --- Transfers ------------------------------------------------------
    /// Declare an upload; bytes then flow on a transfer connection.
    CreateUpload {
        name: String,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        size: u64,
        sha256: String,
        destination: TransferDestination,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        evidence: Option<crate::model::ArtifactLink>,
    },
    /// Offer a host file or artifact for download to the phone.
    CreateDownload {
        source: DownloadSource,
        /// Internal fetch to show an attachment in a task, not a requested download.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        shown_in_task: Option<bool>,
    },
    /// Cancel staging. For reconciliation-required uploads, acknowledges that
    /// the person checked the destination and clears the record without deleting
    /// or changing the possibly published destination file or its evidence.
    CancelTransfer {
        transfer_id: TransferId,
    },
    /// Delete a picture or video kept on the computer. Refused while a
    /// message waiting to be delivered still needs it.
    DeleteArtifact {
        artifact_id: ArtifactId,
    },

    // --- Previews -------------------------------------------------------
    RegisterPreview {
        project_id: ProjectId,
        label: String,
        url: String,
    },
    RemovePreview {
        preview_id: PreviewId,
    },
    CheckPreview {
        preview_id: PreviewId,
        /// Start the preview's server with its command when nothing answers,
        /// as a terminal in the project's folder, and wait for it to answer.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        start: bool,
    },

    // --- Devices and chat identities -----------------------------------
    RenameDevice {
        device_id: DeviceId,
        name: String,
    },
    RevokeDevice {
        device_id: DeviceId,
    },
    /// Replace what a paired device may do. Its connections close so it
    /// reconnects under the new grants.
    SetDeviceGrants {
        device_id: DeviceId,
        /// Grants the editor last saw; compared inside the service transaction.
        /// Absent only in legacy stored commands; live requests must supply it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(as = "Grants", optional = false)]
        expected: Option<Grants>,
        grants: Grants,
    },
    /// Apply only the selected permissions to the current stored grants.
    ChangeDeviceGrants {
        device_id: DeviceId,
        mask: Grants,
        allowed: bool,
    },
    /// Approve a pending chat link code shown by the bot in Telegram/Discord.
    ApproveChannelLink {
        link_code: String,
    },
    /// Refuse a pending chat link code. The bot tells the chat it was refused.
    RefuseChannelLink {
        link_code: String,
    },
    RevokeChannelIdentity {
        identity_id: ChannelIdentityId,
    },
    /// Choose the project a linked chat works in, or none. The setting belongs
    /// to the chat, so every identity linked in that chat shares it.
    SetChannelProject {
        identity_id: ChannelIdentityId,
        project_id: Option<ProjectId>,
    },

    // --- Host -----------------------------------------------------------
    RunReadinessCheck,
    LibraryRestore {
        trash_id: String,
        base_revision: String,
        // None restores the original name. A copy stays in the original root.
        copy_path: Option<String>,
    },
    CreateReusableDraft {
        task_id: TaskId,
        root_id: LibraryRootId,
        path: String,
        text: String,
    },
    CreateUploadReviewed {
        name: String,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        size: u64,
        sha256: String,
        destination: TransferDestination,
        collision: crate::transfer::UploadCollision,
        /// Associate an artifact upload with the task that supplied it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        task_id: Option<TaskId>,
    },
    /// Withdraw only a follow-up still held by the adapter, before native write.
    CancelMessage {
        task_id: TaskId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        message_seq: u64,
    },
    /// Edit a pending item's persisted prompt, quoting the queue revision.
    QueueEditPrompt {
        item_id: QueueItemId,
        prompt: String,
    },
    /// Explicitly start only the reviewed item; quotes both queue and task revisions.
    /// Allowed while Next up is paused; leaves automatic dispatch paused.
    QueueStart {
        item_id: QueueItemId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        task_revision: u64,
    },
    /// Put the next message for a task that isn't running in Next up, to go
    /// to its agent as a new turn when its turn there comes. One per task;
    /// a message sent to the task meanwhile carries it along.
    QueueContinuation {
        task_id: TaskId,
        text: String,
        /// Markup to go with the message. Advertised as `queue_markup_v1`;
        /// omission sends only the words.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        bundle: Option<Box<AnnotationBundle>>,
        authorization: QueueAuthorization,
    },
    /// Keep Øphio's own copies of the agents up to date, or go back to the
    /// programs installed on the computer. Advertised as `agent_updates_v1`.
    SetAgentUpdates {
        enabled: bool,
    },
    /// How the person likes agents to work with them, in every task from the
    /// next run on. Advertised as `agent_preferences_v1`.
    SetAgentPreferences {
        questions: bool,
    },
    /// The launch flags work starts with when nobody chose them, replacing
    /// every earlier default. Advertised as `launch_defaults_v1`.
    SetLaunchDefaults {
        defaults: Vec<crate::agent_preferences::LaunchDefault>,
    },
    /// Which alerts reach the person's phones, replacing every earlier
    /// choice. Advertised as `notification_settings_v1`.
    SetNotificationSettings {
        off: Vec<crate::notifications::AlertTopic>,
        bundle: bool,
        projects: Vec<crate::notifications::ProjectNotifications>,
    },
    // --- Checklist ------------------------------------------------------
    /// Tick a step of a checklist item off, or the whole item, or untick it.
    /// Ticking an item ticks its steps; the last step ticked, with the
    /// item's question answered, ticks the item. An agent's own item is
    /// refused: the agent keeps it. Advertised as `checklist_v1`.
    ChecklistTick {
        project_id: ProjectId,
        item_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        step_id: Option<String>,
        done: bool,
    },
    /// Answer what a checklist item asks. A new answer replaces the old one.
    ChecklistAnswer {
        project_id: ProjectId,
        item_id: String,
        answer: String,
    },
    /// Ask about a checklist item, or with no item about the whole checklist.
    /// The agent that wrote it answers under it. On an agent's own item it is
    /// the person's feedback, for the agent that keeps it.
    ChecklistAsk {
        project_id: ProjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        item_id: Option<String>,
        text: String,
    },
    /// Put a checklist file where the person keeps files: by Taildrop, into
    /// a folder or onto a drive, as `Query::FileDelivery` says. To the phone
    /// it goes with `CreateDownload` instead.
    ChecklistDeliver {
        project_id: ProjectId,
        sha256: String,
    },
    /// Put a project's checklist away, with its files.
    RemoveChecklist {
        project_id: ProjectId,
    },
    /// Where checklist files go from now on.
    SetFileDelivery {
        delivery: crate::checklist::FileDelivery,
    },
    /// Keep a problem report and send it to the intake, now or once the
    /// intake is reachable. The bundle is whole: the phone's part and the
    /// computer's part from `Query::ReportPreview`, as the person saw them.
    /// Sending one already kept changes nothing. Advertised as `reports_v1`.
    SubmitReport {
        bundle: Box<crate::reports::ReportBundle>,
    },
    /// Answer a question about a report.
    AnswerReportQuestion {
        report_id: ReportId,
        question_id: ReportQuestionId,
        answer: crate::reports::ReportAnswer,
    },
    /// Take a report back: the computer forgets it and asks the intake to.
    WithdrawReport {
        report_id: ReportId,
    },
    /// The owner's word on a report in their inbox, for everyone who sent
    /// one like it. Only on a computer with an inbox. Advertised as
    /// `report_inbox_v1`.
    SetReportVerdict {
        report_id: ReportId,
        verdict: crate::reports::ReportVerdict,
    },
    /// The owner chooses a report in their inbox for the fixer, which takes
    /// nothing else. Only on a computer whose fixer runs. Advertised as
    /// `report_look_into_v1`.
    LookIntoReport {
        report_id: ReportId,
    },
    /// Whether the computer sends reports, and how early to hear of fixes.
    SetReportSettings {
        sending: bool,
        ring: crate::reports::ReleaseRing,
    },
    Voice {
        operation: crate::voice::VoiceOperation,
    },
    SetVoice {
        browser: bool,
        desktop: bool,
    },
    WindowAction {
        control: crate::windows::WindowControl,
        action: crate::windows::WindowAction,
    },
    /// Stop exactly one native child, including one whose parent turn ended.
    /// Requires the current parent task revision in the command envelope.
    StopChild {
        run_id: RunId,
        child_id: ChildId,
    },
    /// Requires confirmation of the broader process-group scope and the current
    /// observation revision. An observed process is never adopted as a managed run.
    StopObservedSession {
        #[serde(default)]
        #[ts(as = "Option<bool>", optional)]
        force: bool,
        observed_id: String,
        confirmed: bool,
    },
    LibraryRecoveryDiscard {
        root_id: LibraryRootId,
        recovery_id: String,
    },
    /// Records a human decision only. Requires the current stage revision in
    /// expected_revision (zero when unset); grants no execution authority.
    SetProjectStage {
        project_id: ProjectId,
        stage: crate::project::ProjectStage,
        note: Option<String>,
    },
    /// Manage the computer's accounts. Advertised as `accounts_v1`.
    Account {
        action: crate::accounts::AccountAction,
    },
    /// Manage the computer's agent logins. Advertised as `agent_logins_v1`.
    AgentLogin {
        action: crate::agent_logins::AgentLoginAction,
    },
    /// A project's GitHub workflow. Advertised as `github_v1`.
    Github {
        action: crate::github::GithubAction,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(tag = "mode", rename_all = "snake_case")]
#[ts(export)]
pub enum TaskPlacement {
    /// Start now if the project's writer slot is free; otherwise rejected with
    /// `conflict` (the client offers Next up instead).
    StartNow,
    /// Append to Next up (or insert at `position`).
    Queue {
        position: Option<u32>,
        authorization: QueueAuthorization,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MessageDelivery {
    /// Deliver into the running turn if the native harness supports steering;
    /// otherwise hold until the next safe boundary (reported as such).
    Steer,
    /// Deliver as the next turn after the current one ends.
    NextTurn,
    /// Interrupt the current turn, then deliver.
    InterruptAndSend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "decision", rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalDecision {
    /// Allow exactly the requested effect once.
    Allow,
    Deny {
        reason: Option<String>,
    },
    /// Answer a question-type request.
    Answer {
        text: String,
        /// One entry per question when the request asked structured
        /// `questions`; `text` then reads them back in words.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(as = "Option<Vec<QuestionAnswer>>", optional)]
        answers: Vec<QuestionAnswer>,
    },
}

impl ApprovalDecision {
    /// Secrets belong only in the native hand-off. Withhold the entire answer
    /// to a secret-bearing request, including its duplicate free-text summary.
    /// Missing authority (e.g. a rejected unknown approval) fails closed.
    pub fn without_secrets(&self, approval: Option<&crate::model::Approval>) -> Self {
        if matches!(self, Self::Answer { .. })
            && approval.is_none_or(|a| a.questions.iter().any(|q| q.secret))
        {
            Self::Answer {
                text: String::new(),
                answers: Vec::new(),
            }
        } else {
            self.clone()
        }
    }
}

/// The person's answer to one of an approval's `questions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct QuestionAnswer {
    /// The opaque identity from the approval's question, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    /// The question, exactly as asked.
    pub question: String,
    /// Chosen option labels, then the person's own words if they wrote any.
    pub choices: Vec<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Reconciliation {
    MarkCompleted,
    MarkFailed,
    /// Leave the uncertain run as-is and start a fresh run.
    RetryAsNewRun,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReusableKind {
    Skill,
    Agent,
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum DownloadSource {
    Artifact {
        artifact_id: ArtifactId,
    },
    LibraryFile {
        root_id: LibraryRootId,
        path: String,
    },
    ProjectFile {
        project_id: ProjectId,
        path: String,
    },
    /// A phone app the computer offers in its capabilities' `app_updates`.
    AppUpdate {
        sha256: String,
    },
    /// A file kept with a project's checklist. Advertised as `checklist_v1`.
    ChecklistFile {
        project_id: ProjectId,
        sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CommandReceipt {
    pub command_id: CommandId,
    pub status: ReceiptStatus,
    /// Journal sequence of the admission event, when one was written.
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub seq: Option<u64>,
    pub result: Option<CommandResult>,
    pub error: Option<ProtocolError>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReceiptStatus {
    /// Admitted and recorded. Effects continue asynchronously.
    Accepted,
    /// Already seen this command id; this is the original receipt's content.
    Duplicate,
    /// Not admitted; nothing changed. See `error`.
    Rejected,
}

/// Immediate, synchronous results some commands return with their receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum CommandResult {
    Contribution {
        operation: crate::contribution::ContributionView,
    },
    /// Where a checklist file went, as the person reads it.
    Delivered {
        to: String,
    },
    ProjectAdded {
        project_id: ProjectId,
    },
    TaskCreated {
        task_id: TaskId,
        queue_item_id: Option<QueueItemId>,
    },
    LeaseGranted {
        lease: LeaseView,
    },
    Saved {
        outcome: SaveOutcome,
    },
    Created {
        path: String,
        revision: String,
    },
    TerminalOpened {
        terminal: TerminalInfo,
    },
    Transfer {
        transfer: TransferRecord,
    },
    ArtifactCaptured {
        artifact: ArtifactMeta,
    },
    PreviewRegistered {
        preview_id: PreviewId,
    },
    DraftCreated {
        root_id: LibraryRootId,
        path: String,
    },
    /// `file` is where Library opens it, absent when the agents folder
    /// isn't in Library.
    AgentCreated {
        id: String,
        host_path: String,
        file: Option<crate::launchers::CatalogFile>,
    },
    /// The person's copy of a built-in skill. `file` is where Library opens
    /// it, absent when the skills folder isn't in Library.
    SetupSkillCopied {
        id: String,
        host_path: String,
        file: Option<crate::launchers::CatalogFile>,
    },
    LibraryDeleted {
        item: crate::library::LibraryTrashItem,
    },
    LibraryRestored {
        root_id: LibraryRootId,
        path: String,
        revision: String,
    },
    VoiceSession {
        session: Box<crate::voice::VoiceSessionView>,
    },
    WindowAction {
        receipt: crate::windows::WindowActionReceipt,
    },
    Intervention {
        receipt: Box<crate::model::InterventionReceipt>,
    },
    LibraryRecoveryDiscarded {
        recovery_id: String,
    },
    ProjectStageSet {
        project_id: ProjectId,
        transition: crate::project::ProjectStageTransition,
    },
    IssueFiled {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
        url: String,
    },
}

/// Later stages of a command after admission, reported via journal events.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CommandStage {
    HostAccepted,
    /// Delivered to the native harness/platform, which acknowledged it.
    NativeAcknowledged,
    /// The effect was observed (e.g. process stopped, file saved).
    Observed,
    /// Delivery or effect cannot be established.
    Uncertain,
    ReconciliationRequired,
    Failed,
}

impl CommandEnvelope {
    /// Identity of a retry. The sender's informational issue time may change;
    /// the requested action, concurrency guard and expiry must not.
    pub fn matches_request(&self, other: &Self) -> bool {
        self.command_id == other.command_id
            && self.body == other.body
            && self.expected_revision == other.expected_revision
            && self.expires_at == other.expires_at
            && self.expires_in_ms == other.expires_in_ms
    }

    pub fn new(body: Command) -> Self {
        Self {
            command_id: CommandId::new(),
            issued_at: Timestamp::now(),
            expected_revision: None,
            expires_at: None,
            expires_in_ms: None,
            body,
        }
    }

    /// The envelope as it may be kept: any key it carries is withheld.
    pub fn without_secrets(&self) -> Self {
        self.without_secrets_for(None)
    }

    pub fn without_secrets_for(&self, approval: Option<&crate::model::Approval>) -> Self {
        match &self.body {
            Command::ResolveApproval {
                approval_id,
                decision,
            } => Self {
                body: Command::ResolveApproval {
                    approval_id: *approval_id,
                    decision: decision.without_secrets(approval),
                },
                ..self.clone()
            },
            Command::Account { action } => Self {
                body: Command::Account {
                    action: action.without_secrets(),
                },
                ..self.clone()
            },
            Command::AgentLogin { action } => Self {
                body: Command::AgentLogin {
                    action: action.without_secrets(),
                },
                ..self.clone()
            },
            Command::UnlockDesktop { .. } => Self {
                body: Command::UnlockDesktop {
                    password: crate::accounts::Secret::withheld(),
                },
                ..self.clone()
            },
            _ => self.clone(),
        }
    }

    /// The instant, on the host's clock, after which the host must not start
    /// acting on a command it received at `received`.
    pub fn deadline(&self, received: Timestamp) -> Option<Timestamp> {
        match self.expires_in_ms {
            Some(ms) => Some(Timestamp(
                received
                    .0
                    .saturating_add(i64::try_from(ms).unwrap_or(i64::MAX)),
            )),
            None => self.expires_at,
        }
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_relative_deadline_is_optional_and_set_on_the_hosts_clock() {
        let value = json!({"command_id":CommandId::new(),"issued_at":1_000,
            "expected_revision":null,"expires_at":11_000,"body":{"command":"lock_desktop"}});
        let legacy: CommandEnvelope = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(legacy.expires_in_ms, None);
        assert_eq!(legacy.deadline(Timestamp(50_000)), Some(Timestamp(11_000)));
        assert!(
            !serde_json::to_value(&legacy)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("expires_in_ms")
        );
        let mut current = value;
        current["expires_in_ms"] = json!(10_000);
        let current: CommandEnvelope = serde_json::from_value(current).unwrap();
        assert_eq!(current.deadline(Timestamp(50_000)), Some(Timestamp(60_000)));
    }

    #[test]
    fn an_unlock_password_is_sent_but_never_kept_or_printed() {
        let envelope = CommandEnvelope::new(Command::UnlockDesktop {
            password: crate::accounts::Secret::new("correct horse"),
        });
        assert!(
            serde_json::to_string(&envelope)
                .unwrap()
                .contains("correct horse")
        );
        assert!(!format!("{envelope:?}").contains("correct horse"));
        let kept = envelope.without_secrets();
        assert!(
            !serde_json::to_string(&kept)
                .unwrap()
                .contains("correct horse")
        );
        assert_eq!(kept.command_id, envelope.command_id);
    }

    #[test]
    fn task_mode_and_resume_bundle_are_optional_on_the_wire() {
        let id = ProjectId::new();
        let mut value = json!({"command":"create_task","project_id":id,"kind":"coding",
            "title":null,"prompt":"inspect","bundle":null,"harness":"codex","model":null,
            "effort":null,"mission":null,"target_id":null,"placement":{"mode":"start_now"}});
        let legacy: Command = serde_json::from_value(value.clone()).unwrap();
        assert!(matches!(legacy, Command::CreateTask { mode: None, .. }));
        value["mode"] = json!("plan");
        assert!(matches!(
            serde_json::from_value::<Command>(value).unwrap(),
            Command::CreateTask {
                mode: Some(TaskMode::Plan),
                ..
            }
        ));
        let resume: Command = serde_json::from_value(
            json!({"command":"resume_task","task_id":TaskId::new(),"text":"continue"}),
        )
        .unwrap();
        assert!(matches!(
            resume,
            Command::ResumeTask {
                bundle: None,
                mode: None,
                model: None,
                effort: None,
                carried_plan: None,
                read_only: false,
                ..
            }
        ));
        let switched: Command = serde_json::from_value(json!({"command":"resume_task",
            "task_id":TaskId::new(),"text":"and now?","model":"opus","effort":"high"}))
        .unwrap();
        assert!(matches!(
            &switched,
            Command::ResumeTask { model: Some(m), effort: Some(e), .. } if m == "opus" && e == "high"
        ));
        assert!(!serde_json::to_string(&resume).unwrap().contains("model"));
        let side: Command = serde_json::from_value(
            json!({"command":"resume_task","task_id":TaskId::new(),"text":"sum up","read_only":true}),
        )
        .unwrap();
        assert!(matches!(
            side,
            Command::ResumeTask {
                read_only: true,
                ..
            }
        ));
        let plain = serde_json::to_value(&resume).unwrap();
        assert!(
            plain.get("read_only").is_none(),
            "an ordinary resume reads as before"
        );
    }
}
