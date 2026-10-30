// SPDX-License-Identifier: Apache-2.0
//! What only the person at the computer may do through the owner's controls.
//!
//! A program Øphio started, such as an agent's task or a shell opened for a
//! device, may use the owner's controls for everything else: a setup skill
//! running in a task installs voice, sets up a chat or adds a project. It may
//! not consent for the person: pair a device or change what one may do, answer
//! an approval or a chat's request to link, start or instruct a task, loosen
//! what tasks are given, reveal a secret, or end Øphio. Every match is
//! exhaustive, so a new request is decided here before it builds.

use crate::local::OwnerRequest;
use protocol::{
    Command, ErrorCode, ProtocolError, Query, accounts::AccountAction,
    agent_logins::AgentLoginAction, github::GithubAction,
};

/// The refusal a started program is given instead of `what`.
pub(crate) fn refusal(what: &str) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::PermissionDenied,
        format!(
            "{} started this program, so it can't {what}; only the person at this computer can, in `ophio setup` or a terminal they opened themselves.",
            ports::PRODUCT_NAME
        ),
    )
}

pub(crate) fn owner_request(request: &OwnerRequest) -> Option<&'static str> {
    use OwnerRequest::*;
    match request {
        AnswerPairing { confirm: true, .. } => Some("confirm a pairing"),
        // Whether tasks apply the settings committed in a project's folder.
        ProjectSettings { .. } => Some("change what tasks in a project are allowed"),
        Quit => Some("quit Øphio"),
        // A task renews only an account it was given, with its own credential.
        AccountToken { run, .. } => match run {
            Some(run) if !run.expose().is_empty() => None,
            _ => Some("read an account's key"),
        },
        OpenPairing
        | WaitingPairing
        | AnswerPairing { confirm: false, .. }
        | Jev { .. }
        | Voice { .. }
        | Channels { .. }
        | TokenSet { .. }
        | TokenClear { .. }
        | FrontdoorStatus
        | Desktop { .. }
        | RootsAdd { .. }
        | RootsRemove { .. }
        | RootsList
        | InboxSet { .. }
        | RootsSet { .. }
        | RetentionSet { .. }
        // What the person has to do, as an agent's checklist_add puts it.
        | Checklist { .. }
        | Stop
        | StopTask { .. } => None,
    }
}

pub(crate) fn command(command: &Command) -> Option<&'static str> {
    use Command::*;
    match command {
        ResolveApproval { .. } | PrepareContribution { .. } | SetContributionRoot { .. } => {
            Some("answer an approval")
        }
        // An agent can't widen what holds it.
        SetConversationAccess { .. } => Some("change what a conversation may reach"),
        // Files the person may still want are theirs to delete.
        DeleteOneOff { .. } => Some("delete a one-off's folder"),
        // A task can't hand another task more than it was given.
        CreateTask { .. }
        | SendMessage { .. }
        | ResumeTask { .. }
        | QueueStart { .. }
        | QueueEditPrompt { .. }
        | QueueContinuation { .. } => Some("start or instruct a task"),
        QueueSetAuthorization { .. } => Some("let queued tasks start without asking"),
        UnlockDesktop { .. } => Some("unlock the desktop"),
        RevokeDevice { .. } => Some("end a device's access"),
        SetDeviceGrants { .. } | ChangeDeviceGrants { .. } => Some("change what a device may do"),
        ApproveChannelLink { .. } | RefuseChannelLink { .. } | RevokeChannelIdentity { .. } => {
            Some("answer who may use a chat")
        }
        Account { action } => match action {
            AccountAction::SignIn { .. } | AccountAction::Resume { .. } => {
                Some("give tasks an account")
            }
            AccountAction::CancelSignIn
            | AccountAction::Pause { .. }
            | AccountAction::PauseAll
            | AccountAction::SignOut { .. }
            | AccountAction::Delete { .. }
            | AccountAction::SetKey { .. }
            | AccountAction::AddService { .. }
            | AccountAction::Check { .. }
            | AccountAction::SetUpPasswordManager { .. }
            | AccountAction::PasswordManagerDone => None,
        },
        AgentLogin { action } => match action {
            AgentLoginAction::Accept { .. } => Some("turn agent logins on"),
            AgentLoginAction::Resume { .. } | AgentLoginAction::ResumeAll => {
                Some("give tasks a login")
            }
            AgentLoginAction::AnswerCode { .. } => Some("answer a sign-in code"),
            // An edit keeps the password, so it could take it to other
            // sites and tasks, or stop the phone being asked first.
            AgentLoginAction::Edit { .. } => Some("edit a login"),
            AgentLoginAction::Add { .. }
            | AgentLoginAction::SetSecret { .. }
            | AgentLoginAction::Pause { .. }
            | AgentLoginAction::PauseAll
            | AgentLoginAction::SignOut { .. }
            | AgentLoginAction::Delete { .. } => None,
        },
        Github { action } => match action {
            GithubAction::SetSettings { .. } | GithubAction::AnswerWarning { .. } => {
                Some("change how a project's work reaches GitHub")
            }
            GithubAction::MergePr { .. } => Some("merge a pull request"),
            GithubAction::MergeTogether { .. } => Some("merge pull requests"),
            // Stopping leaves what's picked unmerged, a choice that is the
            // person's as the picking was.
            GithubAction::StopTogether { .. } => Some("stop merging pull requests together"),
            // An agent files issues with its own tool, which says who wrote
            // them; this one posts in the person's name alone.
            GithubAction::FileIssue { .. } => Some("file a GitHub issue in your name"),
            GithubAction::CloseIssue { .. } => Some("close a GitHub issue in your name"),
            // Run all starts tasks with the project's fixer, and what is
            // listed under Agent can take it is what it starts.
            GithubAction::RunIssues { .. } => Some("start or instruct a task"),
            GithubAction::SetGroup { .. } => Some("choose which issues agents fix"),
            GithubAction::ProtectBranch { .. } | GithubAction::MarkPrReady { .. } => None,
        },
        SetAgentPreferences { .. } => Some("change how agents work with you"),
        // An agent could otherwise give itself what the person kept from it.
        SetDisabledInOphio { .. } => Some("change what agents in Øphio are kept from"),
        SetLaunchDefaults { .. } => Some("change the launch flags work starts with"),
        SetNotificationSettings { .. } => Some("change which alerts reach you"),
        // What the person did on a checklist starts its agent again, so
        // only the person does it.
        ChecklistTick { .. } => Some("tick off your checklist"),
        ChecklistAnswer { .. } => Some("answer your checklist"),
        ChecklistAsk { .. } => Some("ask about your checklist"),
        ChecklistDeliver { .. } => Some("send a checklist file to you"),
        RemoveChecklist { .. } => Some("put away a checklist"),
        SetFileDelivery { .. } => Some("change where your files go"),
        // A report leaves the computer, so only the person sends one or
        // speaks for it; an agent can't use it to carry anything out.
        SubmitReport { .. } => Some("send a problem report"),
        AnswerReportQuestion { .. } => Some("answer a question about your report"),
        WithdrawReport { .. } => Some("withdraw a problem report"),
        SetReportSettings { .. } => Some("change where problem reports go"),
        SetReportVerdict { .. } => Some("tell reporters where their problem stands"),
        LookIntoReport { .. } => Some("choose a problem report for the fixer"),
        AcknowledgeAttention { .. } => Some("close a request for the person"),
        AddProject { .. }
        | ReviewContribution { .. }
        | CancelContribution { .. }
        | UpdateProjectDefaults { .. }
        | PromoteOneOff { .. }
        | CreateAgent { .. }
        | CopySetupSkill { .. }
        | ChooseSetupSkillVersion { .. }
        | RemoveProject { .. }
        | StopTask { .. }
        | RenameTask { .. }
        | ArchiveTask { .. }
        | UnarchiveTask { .. }
        | ReconcileTask { .. }
        | CheckHelpers { .. }
        | QueueMove { .. }
        | QueueRemove { .. }
        | QueueSetDependencies { .. }
        | QueuePause { .. }
        | QueueResume { .. }
        | RequestHumanLease { .. }
        | ReleaseLease { .. }
        | PauseInput { .. }
        | TakeOver { .. }
        | ReturnToAgent { .. }
        | StartPrivateStep { .. }
        | EndPrivateStep { .. }
        | WithdrawObservation { .. }
        | LockDesktop
        | CaptureTarget { .. }
        | LibrarySave { .. }
        | LibraryCreate { .. }
        | LibraryRename { .. }
        | LibraryDelete { .. }
        | MakeReusable { .. }
        | OpenTerminal { .. }
        | CloseTerminal { .. }
        | CreateUpload { .. }
        | CreateDownload { .. }
        | CancelTransfer { .. }
        | DeleteArtifact { .. }
        | RegisterPreview { .. }
        | RemovePreview { .. }
        | CheckPreview { .. }
        | RenameDevice { .. }
        | SetChannelProject { .. }
        | RunReadinessCheck
        | LibraryRestore { .. }
        | CreateReusableDraft { .. }
        | CreateUploadReviewed { .. }
        | CancelMessage { .. }
        | SetAgentUpdates { .. }
        | Voice { .. }
        | SetVoice { .. }
        | WindowAction { .. }
        | StopChild { .. }
        | StopObservedSession { .. }
        | LibraryRecoveryDiscard { .. }
        | SetProjectStage { .. } => None,
    }
}

pub(crate) fn query(query: &Query) -> Option<&'static str> {
    use Query::*;
    match query {
        AgentLoginPassword { .. } => Some("read a login's password"),
        InboxReportCode { .. } => Some("read untrusted report code"),
        // Its names are what agents are kept from.
        DisabledInOphio => Some("read what is disabled in Øphio"),
        Snapshot { .. }
        | ContributionDelivery { .. }
        | ContributionCapabilities
        | ContributionOperation { .. }
        | ContributionPolicy { .. }
        | CommandStatus { .. }
        | TaskTimeline { .. }
        | ProjectFolders { .. }
        | Harnesses
        | ControlTargets
        | LibraryList { .. }
        | LibraryRead { .. }
        | LibrarySearch { .. }
        | Instructions { .. }
        | Terminals
        | Previews { .. }
        | ProjectBranches { .. }
        | Findings { .. }
        | Catalog { .. }
        | Readiness
        | SystemStats
        | Devices
        | ChannelLink { .. }
        | Transfers
        | Queue { .. }
        | Artifact { .. }
        | LibraryTrash
        | LibraryTrashRead { .. }
        | AgentFiles
        | ReusableDraft { .. }
        | UploadDestination { .. }
        | TaskArtifacts { .. }
        | ProjectShown { .. }
        | AgentUpdates
        | AgentPreferences
        | NotificationSettings
        | Checklists
        | Checklist { .. }
        | FileDelivery
        | Reports
        | Report { .. }
        | ReportPreview { .. }
        | Voice { .. }
        | Windows { .. }
        | ObservedSessions
        | ReusableFormats { .. }
        | LibraryRecoveryList
        | LibraryRecoveryRead { .. }
        | ProjectStages { .. }
        | TaskHistory { .. }
        | Task { .. }
        | Run { .. }
        | Approval { .. }
        | Accounts
        | AgentLogins
        | GithubProject { .. }
        | GithubItems { .. }
        | GithubPr { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::contribution::{ContributionApproval, ContributionInput};

    #[test]
    fn contribution_trust_requires_the_person_but_delivery_review_and_cancel_do_not() {
        assert!(
            command(&Command::SetContributionRoot {
                root_id: protocol::LibraryRootId::new()
            })
            .is_some()
        );
        assert!(
            command(&Command::PrepareContribution {
                workspace_choice: None,
                operation_id: "fixture".into(),
                approval: Box::new(ContributionApproval {
                    display_name: None,
                    publisher_id: "fixture".into(),
                    packet_sha256: "a".repeat(64),
                    handoff_sha256: "b".repeat(64),
                    game_id: "fixture".into(),
                    release_id: "one".into(),
                    repository_url: "https://source.example/fixture".into(),
                    commit: "c".repeat(40),
                    checked_at: 1,
                    valid_until: 2,
                }),
            })
            .is_some()
        );
        assert!(
            command(&Command::ReviewContribution {
                input: Box::new(ContributionInput {
                    packet_json: "{}".into(),
                    handoff_json: "{}".into(),
                    host_id: protocol::HostId::new(),
                    root_id: protocol::LibraryRootId::new(),
                }),
            })
            .is_none()
        );
        assert!(
            command(&Command::CancelContribution {
                operation_id: "fixture".into()
            })
            .is_none()
        );
        assert!(query(&Query::ContributionCapabilities).is_none());
        // Delivery only reads digest-bound public bytes. It creates no trust
        // or execution authority, like the review allowed above. The service
        // still checks Manage permission and its network/freshness limits.
        assert!(
            query(&Query::ContributionDelivery {
                packet_json: "{}".into(),
                handoff_url: "https://catalog.example.com/handoff.json".into(),
            })
            .is_none()
        );
        assert!(
            query(&Query::ContributionOperation {
                operation_id: "fixture".into()
            })
            .is_none()
        );
    }
}
