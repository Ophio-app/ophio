// SPDX-License-Identifier: Apache-2.0
//! Central, exhaustive authority table. Adding a wire variant requires a policy decision here.
use protocol::{
    Command, Event, Query,
    auth::{Grants, Principal},
    model::TaskKind,
};

pub fn command(g: &Grants, c: &Command) -> bool {
    use Command::*;
    match c {
        AddProject { .. }
        | ReviewContribution { .. }
        | CancelContribution { .. }
        | UpdateProjectDefaults { .. }
        | RemoveProject { .. }
        | DeleteOneOff { .. }
        | PromoteOneOff { .. }
        | RegisterPreview { .. }
        | RemovePreview { .. }
        | RenameDevice { .. }
        | RevokeDevice { .. }
        | SetDeviceGrants { .. }
        | ChangeDeviceGrants { .. }
        | ApproveChannelLink { .. }
        | RefuseChannelLink { .. }
        | RevokeChannelIdentity { .. }
        | SetChannelProject { .. }
        | RunReadinessCheck
        | SetAgentUpdates { .. }
        | SetAgentPreferences { .. }
        | SetLaunchDefaults { .. }
        | SetFileDelivery { .. } => g.admin,
        // Which alerts reach a phone is the person's to choose wherever they
        // run tasks, not only on a phone that manages the computer.
        SetNotificationSettings { .. } => g.tasks || g.admin,
        // Any paired phone can report a problem it met and follow it up: the
        // bundle holds nothing the phone couldn't already read. Whether the
        // computer sends reports at all is the owner's choice.
        SubmitReport { .. } | AnswerReportQuestion { .. } | WithdrawReport { .. } => g.view,
        // Where reports go, and what their reporters hear, are the owner's.
        SetReportSettings { .. } | SetReportVerdict { .. } | LookIntoReport { .. } => g.admin,
        PrepareContribution { .. } | SetContributionRoot { .. } => g.admin && g.library_write,
        CreateTask { kind, .. } => g.tasks && (*kind != TaskKind::ComputerUse || g.computer_use),
        SendMessage { .. }
        | StopTask { .. }
        | ResumeTask { .. }
        | RenameTask { .. }
        | ArchiveTask { .. }
        | UnarchiveTask { .. }
        // Tasks already run with the whole computer; a conversation is held to less.
        | SetConversationAccess { .. }
        | ReconcileTask { .. }
        | CheckHelpers { .. }
        | AcknowledgeAttention { .. }
        | QueueMove { .. }
        | QueueRemove { .. }
        | QueueSetDependencies { .. }
        | QueueSetAuthorization { .. }
        | QueuePause { .. }
        | QueueResume { .. }
        | CheckPreview { start: false, .. }
        | ChecklistTick { .. }
        | ChecklistAnswer { .. }
        | ChecklistAsk { .. }
        | ChecklistDeliver { .. }
        | RemoveChecklist { .. } => g.tasks,
        // Starting the server runs its command on the computer, as a terminal does.
        CheckPreview { start: true, .. } => g.tasks && g.terminal,
        ResolveApproval { .. } => g.approvals,
        RequestHumanLease { .. }
        | ReleaseLease { .. }
        | PauseInput { .. }
        | TakeOver { .. }
        | StartPrivateStep { .. }
        | EndPrivateStep { .. }
        | WithdrawObservation { .. }
        | LockDesktop => g.control_input,
        // The password opens the whole computer, as Manage does.
        UnlockDesktop { .. } => g.control_input && g.admin,
        ReturnToAgent { .. } => g.control_input && g.computer_use,
        CaptureTarget { evidence, .. } => g.view && (evidence.is_none() || g.tasks),
        // A save conflict answers with the file's current text.
        LibrarySave { .. } => g.library_read && g.library_write,
        LibraryCreate { .. }
        | LibraryRename { .. }
        | LibraryDelete { .. }
        | CreateAgent { .. }
        | CopySetupSkill { .. }
        | ChooseSetupSkillVersion { .. }
        // What agents are kept from is as much the Library's as its files.
        | SetDisabledInOphio { .. } => g.library_write,
        MakeReusable { .. } => g.tasks && g.library_write,
        OpenTerminal { .. } | CloseTerminal { .. } => g.terminal,
        CreateDownload { source, .. } => {
            g.transfers
                && match source {
                    protocol::command::DownloadSource::LibraryFile { .. } => g.library_read,
                    protocol::command::DownloadSource::ProjectFile { .. }
                    | protocol::command::DownloadSource::ChecklistFile { .. } => g.tasks,
                    protocol::command::DownloadSource::Artifact { .. } => true,
                    // The computer's own app, which holds nothing of the owner's.
                    protocol::command::DownloadSource::AppUpdate { .. } => true,
                }
        }
        CreateUpload {
            destination,
            evidence,
            ..
        } => {
            g.transfers
                && (evidence.is_none() || g.tasks)
                && (!matches!(
                    destination,
                    protocol::transfer::TransferDestination::ProjectDir { .. }
                ) || g.tasks)
        }
        // Each artifact is checked as a download of it is.
        CancelTransfer { .. } | DeleteArtifact { .. } => g.transfers,
        LibraryRestore { .. } => g.library_read && g.library_write,
        CreateReusableDraft { .. } => g.tasks && g.library_write,
        CreateUploadReviewed {
            destination,
            collision,
            task_id,
            ..
        } => {
            g.transfers
                && (task_id.is_none() || g.tasks)
                && (!matches!(
                    destination,
                    protocol::transfer::TransferDestination::ProjectDir { .. }
                ) || g.tasks)
                && (!matches!(
                    collision,
                    protocol::transfer::UploadCollision::Replace { .. }
                ) || (g.library_read && g.library_write))
        }
        CancelMessage { .. }
        | QueueEditPrompt { .. }
        | QueueStart { .. }
        | QueueContinuation { .. } => g.tasks,
        Voice { .. } => g.view && g.control_input,
        SetVoice { .. } => g.admin,
        WindowAction { .. } => g.control_input,
        StopChild { .. } => g.tasks,
        StopObservedSession { .. } => g.tasks && g.terminal,
        LibraryRecoveryDiscard { .. } => g.library_read && g.library_write,
        SetProjectStage { .. } => g.admin,
        // A command the owner describes starts a program, as a terminal can.
        Account { action } => g.accounts && (!action.runs_commands() || g.terminal),
        // A code a site sent the owner answers a task's request, as an
        // approval does; the rest manages the logins.
        AgentLogin {
            action: protocol::agent_logins::AgentLoginAction::AnswerCode { .. },
        } => g.approvals,
        AgentLogin { .. } => g.accounts,
        // Merging is the person's sign-off, as an approval is; the rest
        // changes how the project's agents work with GitHub. Run all and
        // sorting by hand start and order tasks, as Next up does. Stopping
        // Merge together merges nothing more.
        Github {
            action:
                protocol::github::GithubAction::MarkPrReady { .. }
                | protocol::github::GithubAction::FileIssue { .. }
                | protocol::github::GithubAction::CloseIssue { .. }
                | protocol::github::GithubAction::RunIssues { .. }
                | protocol::github::GithubAction::SetGroup { .. }
                | protocol::github::GithubAction::StopTogether { .. },
        } => g.tasks,
        Github {
            action:
                protocol::github::GithubAction::MergePr { .. }
                | protocol::github::GithubAction::MergeTogether { .. },
        } => g.tasks && g.approvals,
        Github { .. } => g.admin,
    }
}

pub fn query(g: &Grants, q: &Query) -> bool {
    match q {
        Query::ContributionDelivery { .. }
        | Query::ContributionCapabilities
        | Query::ContributionOperation { .. }
        | Query::ContributionPolicy { .. } => g.admin,
        Query::Snapshot { .. } | Query::CommandStatus { .. } => true,
        Query::TaskTimeline { .. }
        | Query::Harnesses
        | Query::Previews { .. }
        | Query::ProjectBranches { .. }
        | Query::Findings { .. }
        | Query::Catalog { .. }
        | Query::Queue { .. }
        | Query::Checklists
        | Query::Checklist { .. } => g.tasks,
        // Where files go is the owner's to set, and theirs to use.
        Query::FileDelivery => g.tasks || g.admin,
        Query::AgentUpdates | Query::AgentPreferences | Query::NotificationSettings => g.view,
        Query::Reports | Query::Report { .. } | Query::ReportPreview { .. } => g.view,
        Query::InboxReportCode { .. } => g.admin,
        Query::ObservedSessions => g.tasks,
        Query::ProjectStages { .. } => g.tasks || g.admin,
        Query::TaskHistory { .. } | Query::Task { .. } | Query::Run { .. } => g.tasks,
        Query::Approval { .. } => g.approvals,
        Query::ControlTargets => g.view,
        Query::Voice { .. } => g.view && g.control_input,
        Query::Windows { .. } => g.view,
        Query::SystemStats => g.view,
        Query::LibraryList { .. }
        | Query::LibraryRead { .. }
        | Query::LibrarySearch { .. }
        | Query::Instructions { .. } => g.library_read,
        Query::Terminals => g.terminal,
        Query::Readiness
        | Query::Devices
        | Query::ChannelLink { .. }
        | Query::ProjectFolders { .. } => g.admin,
        Query::TaskArtifacts { .. } | Query::ProjectShown { .. } => g.tasks && g.transfers,
        Query::Transfers | Query::Artifact { .. } => g.transfers,
        Query::Accounts | Query::AgentLogins | Query::AgentLoginPassword { .. } => g.accounts,
        Query::GithubProject { .. } | Query::GithubItems { .. } | Query::GithubPr { .. } => g.tasks,
        Query::LibraryTrash
        | Query::LibraryTrashRead { .. }
        | Query::AgentFiles
        | Query::DisabledInOphio => g.library_read,
        Query::LibraryRecoveryList | Query::LibraryRecoveryRead { .. } => g.library_read,
        Query::ReusableDraft { .. } | Query::ReusableFormats { .. } => g.tasks && g.library_read,
        Query::UploadDestination { destination, .. } => {
            g.transfers
                && (!matches!(
                    destination,
                    protocol::transfer::TransferDestination::ProjectDir { .. }
                ) || g.tasks)
        }
    }
}

/// Channels are never a desktop, terminal or private-file disclosure route.
pub fn effective(mut g: Grants, p: Principal) -> Grants {
    if matches!(p, Principal::Channel { .. }) {
        g.view = false;
        g.control_input = false;
        g.terminal = false;
        g.computer_use = false;
        g.library_read = false;
        g.library_write = false;
        g.transfers = false;
        g.admin = false;
        g.accounts = false;
    }
    g
}

pub fn event(g: &Grants, e: &Event) -> bool {
    use Event::*;
    match e {
        HostUpdated { .. } | CapabilitiesChanged { .. } | BootEpochChanged { .. } => true,
        HarnessesChanged { .. }
        | ProjectUpserted { .. }
        | ProjectRemoved { .. }
        | TaskUpserted { .. }
        | RunUpserted { .. }
        | TimelineAppended { .. }
        | AttentionUpserted { .. }
        | AttentionResolved { .. }
        | QueueChanged { .. }
        | PreviewsChanged { .. }
        | ChecklistChanged { .. } => g.tasks,
        CommandProgress { .. } => true, // Caller identity is checked against the command table as well.
        ApprovalUpserted { .. } => g.approvals,
        TargetsChanged { .. } | ControlChanged { .. } => g.view,
        InterventionRecorded { target_id, .. } => {
            if target_id.is_some() {
                g.view
            } else {
                g.tasks
            }
        }
        LibraryRootsChanged { .. } | LibraryFileChanged { .. } => g.library_read,
        TerminalsChanged { .. } => g.terminal,
        TransferUpserted { .. } | ArtifactCreated { .. } | ArtifactDeleted { .. } => g.transfers,
        DevicesChanged { .. } | ChannelIdentitiesChanged { .. } | ReadinessReported { .. } => {
            g.admin
        }
        AccountsChanged {} | AgentLoginsChanged {} | AgentLoginUsed { .. } => g.accounts,
        NotificationSettingsChanged {} => g.view,
        ReportsChanged {} => g.view,
        GithubChanged { .. } | PrChanged { .. } => g.tasks,
    }
}

pub fn device_defaults() -> Grants {
    Grants {
        view: true,
        tasks: true,
        approvals: true,
        library_read: true,
        ..Grants::default()
    }
}
/// Everything a device may be given from another device: Accounts is turned on only at the computer.
pub fn manager_defaults() -> Grants {
    Grants {
        accounts: false,
        ..Grants::owner()
    }
}
pub fn channel_defaults() -> Grants {
    Grants {
        tasks: true,
        approvals: true,
        ..Grants::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{
        command::*, library::NewlineStyle, queue::QueueAuthorization,
        transfer::TransferDestination, *,
    };
    #[test]
    fn accounts_need_their_own_grant_and_never_reach_a_chat() {
        use protocol::accounts::{AccountAction, ServiceKind, ServiceProfile};
        let manager = Grants {
            accounts: false,
            ..Grants::owner()
        };
        let pause = Command::Account {
            action: AccountAction::PauseAll,
        };
        assert!(!command(&manager, &pause));
        assert!(!query(&manager, &Query::Accounts));
        assert!(!event(&manager, &Event::AccountsChanged {}));
        let accounts = Grants {
            accounts: true,
            ..Grants::default()
        };
        assert!(command(&accounts, &pause));
        let cli = Command::Account {
            action: AccountAction::AddService {
                service: ServiceProfile {
                    name: "Cloud".into(),
                    service: ServiceKind::Cli {
                        sign_in: "gcloud auth login".into(),
                        status: "gcloud auth list".into(),
                        sign_out: None,
                        config_variable: "CLOUDSDK_CONFIG".into(),
                        manage_url: None,
                    },
                },
            },
        };
        assert!(!command(&accounts, &cli));
        assert!(command(
            &Grants {
                terminal: true,
                ..accounts.clone()
            },
            &cli
        ));
        let chat = effective(
            Grants::owner(),
            Principal::Channel {
                identity_id: protocol::ChannelIdentityId::new(),
            },
        );
        assert!(!chat.accounts);
    }
    #[test]
    fn unlocking_the_computer_takes_control_and_manage() {
        let unlock = Command::UnlockDesktop {
            password: protocol::accounts::Secret::new("x"),
        };
        let control = Grants {
            control_input: true,
            ..Grants::default()
        };
        let manage = Grants {
            admin: true,
            ..Grants::default()
        };
        assert!(command(&control, &Command::LockDesktop));
        assert!(!command(&control, &unlock));
        assert!(!command(&manage, &unlock));
        assert!(command(
            &Grants {
                admin: true,
                ..control
            },
            &unlock
        ));
        let chat = effective(
            Grants::owner(),
            Principal::Channel {
                identity_id: protocol::ChannelIdentityId::new(),
            },
        );
        assert!(!command(&chat, &unlock));
    }
    #[test]
    fn agent_logins_are_managed_with_accounts_and_codes_answered_with_approvals() {
        use protocol::accounts::Secret;
        use protocol::agent_logins::AgentLoginAction;
        let pause = Command::AgentLogin {
            action: AgentLoginAction::PauseAll,
        };
        let code = Command::AgentLogin {
            action: AgentLoginAction::AnswerCode {
                approval_id: ApprovalId::new(),
                code: Secret::new("482913"),
            },
        };
        let approver = Grants {
            approvals: true,
            ..Grants::default()
        };
        assert!(!command(&approver, &pause));
        assert!(command(&approver, &code));
        let accounts = Grants {
            accounts: true,
            ..Grants::default()
        };
        assert!(command(&accounts, &pause));
        assert!(!command(&accounts, &code));
        let reveal = Query::AgentLoginPassword {
            login_id: "l1".into(),
        };
        assert!(query(&accounts, &reveal) && !query(&approver, &reveal));
        assert!(!event(&approver, &Event::AgentLoginsChanged {}));
    }
    #[test]
    fn merging_is_a_sign_off_and_settings_are_the_owners() {
        use protocol::github::{GithubAction, GithubSettings, MergeMethod};
        let p = ProjectId::new();
        let tasks = Grants {
            tasks: true,
            ..Grants::default()
        };
        let merge = Command::Github {
            action: GithubAction::MergePr {
                project_id: p,
                repo: "octo/app".into(),
                number: 7,
                expected_head: "abc".into(),
                method: MergeMethod::Squash,
                approval_shown: None,
            },
        };
        let ready = Command::Github {
            action: GithubAction::MarkPrReady {
                project_id: p,
                repo: "octo/app".into(),
                number: 7,
            },
        };
        let settings = Command::Github {
            action: GithubAction::SetSettings {
                project_id: p,
                settings: GithubSettings::default(),
            },
        };
        let together = Command::Github {
            action: GithubAction::MergeTogether {
                project_id: p,
                repo: "octo/app".into(),
                prs: vec![protocol::github::ChosenPr {
                    number: 7,
                    head: "abc".into(),
                    approval_shown: None,
                }],
            },
        };
        let stop = Command::Github {
            action: GithubAction::StopTogether { project_id: p },
        };
        assert!(command(&tasks, &ready) && !command(&tasks, &merge));
        assert!(!command(&tasks, &together) && command(&tasks, &stop));
        assert!(command(&channel_defaults(), &merge));
        assert!(!command(&channel_defaults(), &settings));
        assert!(command(&Grants::owner(), &settings));
    }
    #[test]
    fn saving_over_a_file_needs_permission_to_read_it() {
        let save = Command::LibrarySave {
            root_id: LibraryRootId::new(),
            path: "notes.md".into(),
            text: "x".into(),
            base_revision: "unknown".into(),
            newline: NewlineStyle::Lf,
            bom: false,
        };
        // A conflict answers with the file's current text.
        let write_only = Grants {
            library_write: true,
            ..Grants::default()
        };
        assert!(!command(&write_only, &save));
        assert!(command(
            &Grants {
                library_read: true,
                ..write_only
            },
            &save
        ));
    }
    #[test]
    fn starting_a_previews_server_takes_the_terminal() {
        let check = |start| Command::CheckPreview {
            preview_id: PreviewId::new(),
            start,
        };
        let tasks = Grants {
            tasks: true,
            ..Grants::default()
        };
        assert!(command(&tasks, &check(false)) && !command(&tasks, &check(true)));
        let terminal = Grants {
            terminal: true,
            ..tasks
        };
        assert!(command(&terminal, &check(true)));
        assert!(!command(
            &Grants {
                tasks: false,
                ..terminal
            },
            &check(true)
        ));
    }
    #[test]
    fn every_command_requires_authority() {
        let p = ProjectId::new();
        let t = TaskId::new();
        let a = ApprovalId::new();
        let q = QueueItemId::new();
        let target = TargetId::new();
        let run = RunId::new();
        let root = LibraryRootId::new();
        let device = DeviceId::new();
        let commands = vec![
            Command::SetContributionRoot { root_id: root },
            Command::ReviewContribution {
                input: Box::new(protocol::contribution::ContributionInput {
                    packet_json: "{}".into(),
                    handoff_json: "{}".into(),
                    host_id: HostId::new(),
                    root_id: root,
                }),
            },
            Command::PrepareContribution {
                workspace_choice: None,
                operation_id: "fixture".into(),
                approval: Box::new(protocol::contribution::ContributionApproval {
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
            },
            Command::CancelContribution {
                operation_id: "fixture".into(),
            },
            Command::SetProjectStage {
                project_id: p,
                stage: protocol::project::ProjectStage::Planning,
                note: None,
            },
            Command::StopObservedSession {
                force: false,
                observed_id: "fixture".into(),
                confirmed: true,
            },
            Command::StopChild {
                run_id: run,
                child_id: ChildId::new(),
            },
            Command::AddProject {
                host_path: "x".into(),
                name: None,
            },
            Command::UpdateProjectDefaults {
                project_id: p,
                harness: None,
                model: None,
                effort: None,
            },
            Command::RemoveProject { project_id: p },
            Command::DeleteOneOff { project_id: p },
            Command::PromoteOneOff { project_id: p },
            Command::CreateTask {
                github: None,
                agent_chosen: None,
                mode: None,
                project_id: p,
                kind: TaskKind::Coding,
                title: None,
                prompt: "x".into(),
                bundle: None,
                harness: protocol::capability::HarnessKind::ClaudeCode,
                model: None,
                effort: None,
                launch_policy: None,
                branch: None,
                mission: None,
                target_id: None,
                placement: TaskPlacement::StartNow,
                start: None,
                place: None,
                browser: None,
                conversation: None,
                access: None,
                review: None,
            },
            Command::SendMessage {
                task_id: t,
                text: "x".into(),
                bundle: None,
                delivery: MessageDelivery::Steer,
            },
            Command::StopTask {
                task_id: t,
                force: false,
            },
            Command::ResumeTask {
                mode: None,
                bundle: None,
                task_id: t,
                text: None,
                carried_plan: None,
                read_only: false,
                model: None,
                effort: None,
            },
            Command::RenameTask {
                task_id: t,
                title: "x".into(),
                expected_title: None,
            },
            Command::ArchiveTask { task_id: t },
            Command::UnarchiveTask { task_id: t },
            Command::SetConversationAccess {
                task_id: t,
                access: Default::default(),
            },
            Command::CheckHelpers { run_id: run },
            Command::ReconcileTask {
                task_id: t,
                resolution: Reconciliation::MarkFailed,
            },
            Command::ResolveApproval {
                approval_id: a,
                decision: ApprovalDecision::Allow,
            },
            Command::AcknowledgeAttention {
                attention_id: AttentionId::new(),
            },
            Command::QueueMove {
                item_id: q,
                position: 0,
            },
            Command::QueueRemove { item_id: q },
            Command::QueueSetDependencies {
                item_id: q,
                depends_on: Vec::new(),
            },
            Command::QueueSetAuthorization {
                item_id: q,
                authorization: QueueAuthorization::AutoStart,
            },
            Command::QueuePause { project_id: p },
            Command::QueueResume { project_id: p },
            Command::RequestHumanLease {
                target_id: target,
                geometry_revision: 1,
            },
            Command::ReleaseLease {
                lease_id: LeaseId::new(),
            },
            Command::PauseInput { target_id: target },
            Command::TakeOver {
                target_id: target,
                geometry_revision: 1,
            },
            Command::ReturnToAgent {
                target_id: target,
                run_id: run,
            },
            Command::StartPrivateStep { target_id: target },
            Command::EndPrivateStep { target_id: target },
            Command::WithdrawObservation {
                run_id: run,
                target_id: target,
            },
            Command::LockDesktop,
            Command::UnlockDesktop {
                password: protocol::accounts::Secret::new("x"),
            },
            Command::CaptureTarget {
                target_id: target,
                evidence: None,
            },
            Command::LibrarySave {
                root_id: root,
                path: "x".into(),
                text: "x".into(),
                base_revision: "x".into(),
                newline: NewlineStyle::Lf,
                bom: false,
            },
            Command::LibraryCreate {
                root_id: root,
                path: "x".into(),
                text: "x".into(),
            },
            Command::CreateAgent {
                name: "x".into(),
                description: None,
            },
            Command::CopySetupSkill { id: "x".into() },
            Command::ChooseSetupSkillVersion {
                id: "x".into(),
                keep_mine: true,
            },
            Command::SetDisabledInOphio {
                kind: protocol::disabled::DisabledKind::Skill,
                host_path: "x".into(),
                disabled: true,
            },
            Command::LibraryRename {
                root_id: root,
                from: "x".into(),
                to: "y".into(),
                base_revision: "x".into(),
            },
            Command::LibraryDelete {
                root_id: root,
                path: "x".into(),
                base_revision: "x".into(),
            },
            Command::MakeReusable {
                task_id: t,
                kind: ReusableKind::Note,
                name: "x".into(),
            },
            Command::OpenTerminal {
                cwd: None,
                project_id: Some(p),
                harness: None,
                cols: 80,
                rows: 24,
            },
            Command::CloseTerminal {
                terminal_id: TerminalId::new(),
            },
            Command::CreateUpload {
                evidence: None,
                name: "x".into(),
                size: 1,
                sha256: "x".into(),
                destination: TransferDestination::Inbox,
            },
            Command::CreateDownload {
                shown_in_task: None,
                source: DownloadSource::Artifact {
                    artifact_id: ArtifactId::new(),
                },
            },
            Command::CancelTransfer {
                transfer_id: TransferId::new(),
            },
            Command::DeleteArtifact {
                artifact_id: ArtifactId::new(),
            },
            Command::RegisterPreview {
                project_id: p,
                label: "x".into(),
                url: "x".into(),
            },
            Command::RemovePreview {
                preview_id: PreviewId::new(),
            },
            Command::CheckPreview {
                preview_id: PreviewId::new(),
                start: false,
            },
            Command::RenameDevice {
                device_id: device,
                name: "x".into(),
            },
            Command::RevokeDevice { device_id: device },
            Command::SetDeviceGrants {
                expected: Some(Grants::owner()),
                device_id: device,
                grants: Grants::owner(),
            },
            Command::ChangeDeviceGrants {
                device_id: device,
                mask: Grants::owner(),
                allowed: false,
            },
            Command::ApproveChannelLink {
                link_code: "x".into(),
            },
            Command::RefuseChannelLink {
                link_code: "x".into(),
            },
            Command::RevokeChannelIdentity {
                identity_id: ChannelIdentityId::new(),
            },
            Command::SetChannelProject {
                identity_id: ChannelIdentityId::new(),
                project_id: Some(p),
            },
            Command::RunReadinessCheck,
            Command::LibraryRestore {
                trash_id: "x".into(),
                base_revision: "x".into(),
                copy_path: None,
            },
            Command::CreateReusableDraft {
                task_id: t,
                root_id: root,
                path: "draft.md".into(),
                text: "draft".into(),
            },
            Command::CreateUploadReviewed {
                task_id: None,
                name: "x".into(),
                size: 1,
                sha256: "x".into(),
                destination: TransferDestination::Inbox,
                collision: protocol::transfer::UploadCollision::KeepBoth {
                    name: "x (1)".into(),
                },
            },
            Command::CancelMessage {
                task_id: t,
                message_seq: 1,
            },
            Command::QueueEditPrompt {
                item_id: q,
                prompt: "inspect".into(),
            },
            Command::QueueStart {
                item_id: q,
                task_revision: 1,
            },
            Command::QueueContinuation {
                task_id: t,
                text: "go on".into(),
                bundle: None,
                authorization: QueueAuthorization::AutoStart,
            },
            Command::Voice {
                operation: protocol::voice::VoiceOperation::Stop {
                    session_id: "test".into(),
                },
            },
            Command::SetVoice {
                browser: true,
                desktop: true,
            },
            Command::SetAgentUpdates { enabled: true },
            Command::SetAgentPreferences { questions: false },
            Command::SetLaunchDefaults {
                defaults: Vec::new(),
            },
            Command::SetNotificationSettings {
                off: Vec::new(),
                bundle: true,
                projects: Vec::new(),
            },
            Command::ChecklistTick {
                project_id: p,
                item_id: "a".into(),
                step_id: None,
                done: true,
            },
            Command::ChecklistAnswer {
                project_id: p,
                item_id: "a".into(),
                answer: "yes".into(),
            },
            Command::ChecklistAsk {
                project_id: p,
                item_id: None,
                text: "why?".into(),
            },
            Command::ChecklistDeliver {
                project_id: p,
                sha256: "x".into(),
            },
            Command::RemoveChecklist { project_id: p },
            Command::SetFileDelivery {
                delivery: protocol::checklist::FileDelivery::Phone,
            },
        ];
        let mut commands = commands;
        commands.push(Command::WindowAction {
            control: protocol::windows::WindowControl {
                target_id: target,
                lease_id: LeaseId::new(),
                boot_epoch: BootEpoch::new(),
                generation: 1,
                geometry_revision: 1,
            },
            action: protocol::windows::WindowAction::Focus {
                window_id: "fixture".into(),
            },
        });
        let report_id = protocol::ReportId::new();
        commands.push(Command::SubmitReport {
            bundle: Box::new(
                serde_json::from_value(serde_json::json!({
                    "schema": 1,
                    "report_id": report_id,
                    "created_at": 0,
                    "origin": {"type": "settings"},
                    "signature": {"code": "none", "screen": "/", "build": "test"},
                    "versions": {},
                    "context": {"screen": "/"},
                }))
                .unwrap(),
            ),
        });
        commands.push(Command::AnswerReportQuestion {
            report_id,
            question_id: protocol::ReportQuestionId::new(),
            answer: protocol::reports::ReportAnswer {
                choice: protocol::reports::QuestionChoice::Yes,
                words: None,
                at: protocol::time::Timestamp(0),
            },
        });
        commands.push(Command::WithdrawReport { report_id });
        commands.push(Command::SetReportSettings {
            sending: true,
            ring: protocol::reports::ReleaseRing::Everyone,
        });
        commands.push(Command::SetReportVerdict {
            report_id,
            verdict: protocol::reports::ReportVerdict::Reproduced,
        });
        commands.push(Command::LookIntoReport { report_id });
        commands.push(Command::LibraryRecoveryDiscard {
            root_id: root,
            recovery_id: "snapshot".into(),
        });
        commands.push(Command::Account {
            action: protocol::accounts::AccountAction::PauseAll,
        });
        commands.push(Command::AgentLogin {
            action: protocol::agent_logins::AgentLoginAction::PauseAll,
        });
        commands.push(Command::Github {
            action: protocol::github::GithubAction::ProtectBranch { project_id: p },
        });
        // Keep the list tied to the wire schema so a new variant cannot silently
        // escape this table-driven permission test.
        let schema = serde_json::to_value(schemars::schema_for!(Command)).unwrap();
        assert_eq!(commands.len(), schema["oneOf"].as_array().unwrap().len());
        // A checklist's files come down like a task's.
        commands.push(Command::CreateDownload {
            shown_in_task: None,
            source: DownloadSource::ChecklistFile {
                project_id: p,
                sha256: "x".into(),
            },
        });
        for c in commands {
            assert!(!command(&Grants::default(), &c), "{c:?}");
            assert!(command(&Grants::owner(), &c), "{c:?}");
            if matches!(
                c,
                Command::PrepareContribution { .. } | Command::SetContributionRoot { .. }
            ) {
                assert!(!command(
                    &Grants {
                        library_write: false,
                        ..Grants::owner()
                    },
                    &c
                ));
            }
        }
        let qs = vec![
            Query::ContributionDelivery {
                packet_json: "{}".into(),
                handoff_url: "https://example.com/handoff.json".into(),
            },
            Query::ContributionCapabilities,
            Query::ContributionOperation {
                operation_id: "fixture".into(),
            },
            Query::ContributionPolicy {
                operation_id: "fixture".into(),
                path: "AGENTS.md".into(),
            },
            Query::ProjectFolders {
                path: None,
                after: None,
                show_hidden: false,
                search: String::new(),
            },
            Query::ProjectStages {
                project_id: p,
                before_revision: None,
                limit: 50,
            },
            Query::Voice { session_id: None },
            Query::ObservedSessions,
            Query::Snapshot {
                reads_withheld: None,
            },
            Query::CommandStatus {
                command_id: CommandId::new(),
            },
            Query::TaskTimeline {
                task_id: t,
                before_seq: None,
                limit: 1,
            },
            Query::Harnesses,
            Query::AgentUpdates,
            Query::AgentPreferences,
            Query::NotificationSettings,
            Query::Checklists,
            Query::Checklist { project_id: p },
            Query::FileDelivery,
            Query::ControlTargets,
            Query::SystemStats,
            Query::LibraryList {
                root_id: root,
                dir: "".into(),
            },
            Query::LibraryRead {
                root_id: root,
                path: "x".into(),
            },
            Query::LibrarySearch {
                root_id: None,
                text: "x".into(),
                limit: 1,
            },
            Query::Instructions {
                project_id: p,
                task_id: None,
            },
            Query::Terminals,
            Query::Previews { project_id: p },
            Query::ProjectBranches { project_id: p },
            Query::Findings { project_id: p },
            Query::Catalog { project_id: None },
            Query::Readiness,
            Query::Devices,
            Query::ChannelLink {
                link_code: "x".into(),
            },
            Query::Transfers,
            Query::Queue { project_id: p },
            Query::Artifact {
                artifact_id: ArtifactId::new(),
            },
            Query::LibraryTrash,
            Query::AgentFiles,
            Query::DisabledInOphio,
            Query::LibraryTrashRead {
                trash_id: "x".into(),
            },
            Query::ReusableDraft { task_id: t },
            Query::UploadDestination {
                name: "x".into(),
                destination: TransferDestination::Inbox,
            },
        ];
        let mut qs = qs;
        qs.push(Query::Windows {
            search: String::new(),
        });
        qs.push(Query::TaskArtifacts {
            task_id: t,
            before: None,
            limit: 10,
        });
        qs.push(Query::ProjectShown {
            project_id: p,
            before: None,
            limit: 10,
        });
        qs.extend([
            Query::TaskHistory {
                project_id: p,
                from: protocol::model::TaskCursor {
                    updated_at: protocol::time::Timestamp(0),
                    task_id: t,
                },
                limit: 50,
            },
            Query::Task { task_id: t },
            Query::Run {
                run_id: RunId::new(),
            },
            Query::Approval {
                approval_id: ApprovalId::new(),
            },
            Query::Accounts,
            Query::AgentLogins,
            Query::AgentLoginPassword {
                login_id: "l1".into(),
            },
            Query::GithubProject { project_id: p },
            Query::GithubItems {
                project_id: p,
                list: protocol::github::GithubItemKind::Issues,
                filter: None,
            },
            Query::GithubPr {
                project_id: p,
                number: 7,
            },
            Query::LibraryRecoveryList,
            Query::LibraryRecoveryRead {
                root_id: root,
                recovery_id: "snapshot".into(),
            },
            Query::ReusableFormats {
                task_id: t,
                harness: protocol::capability::HarnessKind::ClaudeCode,
            },
            Query::Reports,
            Query::InboxReportCode {
                report_id: protocol::ReportId::new(),
            },
            Query::Report {
                report_id: protocol::ReportId::new(),
            },
            Query::ReportPreview {
                salt: "00".into(),
                extras: Vec::new(),
            },
        ]);
        let schema = serde_json::to_value(schemars::schema_for!(Query)).unwrap();
        assert_eq!(qs.len(), schema["oneOf"].as_array().unwrap().len());
        for q in qs {
            assert!(query(&Grants::owner(), &q));
            if !matches!(q, Query::Snapshot { .. } | Query::CommandStatus { .. }) {
                assert!(!query(&Grants::default(), &q), "{q:?}");
            }
        }
    }
}
