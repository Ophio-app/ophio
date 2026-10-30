// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result,
    config::{APP_NAME, Config, PAIR_SCHEME, Paths},
    identity::{FileSecretStore, Identity},
    local::{OwnerClient, OwnerRequest, OwnerResponse},
    network::Discovery,
    readable::{self, Style, shown},
    server::{Server, ServerDeps},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::{Parser, Subcommand};
use ports::{COMMAND, PRODUCT_NAME};
use protocol::{
    ClientFrame, Command, CommandEnvelope, Query, QueryData, QueryEnvelope, ServerFrame,
};
use services::api::PairingPrompt;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Debug, Parser)]
#[command(
    name = APP_NAME,
    version,
    about = "Connect your phone or tablet to your own computer",
    after_help = "Get started:
  ophio setup             Set Øphio up, one step at a time
  ophio service install   Start Øphio now and whenever you log in
  ophio pair              Pair your phone or tablet
  ophio doctor            Check what works and what needs attention"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Action,
}
impl Cli {
    /// Reads this process's arguments. Help and usage name the program as it
    /// is typed on this computer.
    pub fn read() -> Self {
        use clap::{CommandFactory, FromArgMatches};
        use std::io::IsTerminal;
        // `ophio` alone, on a computer not set up yet, sets it up.
        if std::env::args_os().len() == 1
            && std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && crate::setup::pending_first_run()
        {
            return Cli {
                command: Action::Setup(crate::setup::SetupArgs::default()),
            };
        }
        let mut command = retype(Cli::command(), COMMAND);
        if COMMAND != APP_NAME {
            command = command.bin_name(COMMAND);
        }
        let matches = command.get_matches();
        Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
    }
}

/// Help is written for `ophio` typed as is. Elsewhere, as on Windows, its
/// commands and examples are rewritten to say what to type instead.
fn retype(mut command: clap::Command, program: &str) -> clap::Command {
    if program == APP_NAME {
        return command;
    }
    let (example, quoted) = (format!("  {APP_NAME} "), format!("`{APP_NAME} "));
    let swap = |text: Option<&clap::builder::StyledStr>| {
        let lines: Vec<_> = (text?.to_string().split('\n'))
            .map(|line| match line.strip_prefix(&example) {
                Some(rest) => format!("  {program} {rest}"),
                None => line.to_owned(),
            })
            .collect();
        Some(lines.join("\n").replace(&quoted, &format!("`{program} ")))
    };
    if let Some(text) = swap(command.get_about()) {
        command = command.about(text);
    }
    if let Some(text) = swap(command.get_long_about()) {
        command = command.long_about(text);
    }
    if let Some(text) = swap(command.get_after_help()) {
        command = command.after_help(text);
    }
    command
        .mut_args(|mut arg| {
            if let Some(text) = swap(arg.get_help()) {
                arg = arg.help(text);
            }
            if let Some(text) = swap(arg.get_long_help()) {
                arg = arg.long_help(text);
            }
            arg
        })
        .mut_subcommands(|sub| retype(sub, program))
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Set Øphio up, one step at a time.
    ///
    /// Starts Øphio now and at every login, works out how your phone reaches
    /// this computer, connects Claude Code and Codex, pairs your phone and
    /// checks what works. Any step can be left for later; run it again to
    /// carry on, or to go through it all again once it is done.
    Setup(crate::setup::SetupArgs),
    /// Start Øphio in this terminal.
    ///
    /// It runs until you press Ctrl-C. Only one copy of Øphio runs at a time,
    /// so if the service is running, use it instead. To start Øphio whenever
    /// you log in, run `ophio service install`.
    Run,
    /// Link Jev, which tells voice control what you meant.
    ///
    /// Øphio must be running. `ophio jev connect --help` says what voice
    /// control sends and to whom.
    Jev {
        #[command(subcommand)]
        action: crate::jev::JevCommand,
        /// Print the full result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Install and choose the speech model for voice input.
    ///
    /// Speech is recognised on this computer; audio is never sent anywhere
    /// else. The models are tiny.en (the default), base.en and small.en.
    /// With Øphio stopped, changes are saved and the next start applies them.
    Voice {
        #[command(subcommand)]
        action: crate::frontdoors::VoiceCommand,
        /// Print the full result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Connect your own Telegram or Discord bot to your projects.
    ///
    /// Store the bot's token, turn the connector on, then map each chat to a
    /// project. Each person who writes to the bot must be approved once in the
    /// app, under Connections › Chat. With Øphio stopped, changes are saved
    /// and the next start applies them.
    #[command(after_help = "Examples:
  ophio channels token set telegram                         Store the bot's token
  ophio channels enable telegram                            Turn the connector on
  ophio projects list                                       Find the project's ID
  ophio channels map telegram -1001234567890 <PROJECT-ID>   Send a group's messages to it
  ophio channels status                                     Check that the bot connected")]
    Channels {
        #[command(subcommand)]
        action: crate::frontdoors::ChannelsCli,
        /// Print the full result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Pair a new phone or tablet.
    ///
    /// Øphio must be running. Shows a QR code and a link for the app, and
    /// a fingerprint to compare with the one the app shows. When a device
    /// asks to pair, it shows the device's name and a six-digit code. The
    /// device can connect only after you confirm here that it shows the same
    /// code. In a terminal, Øphio asks you. Without one, as from a script,
    /// confirm with `ophio pair confirm CODE`.
    #[command(after_help = "Examples:
  ophio pair                             Show a pairing code, then scan it in the app
  ophio pair confirm 123456              Pair the device that shows the code 123 456
  ophio devices                          Check the new device and what it may do
  ophio devices allow Tablet control     Let the device named Tablet use mouse and keyboard")]
    Pair {
        #[command(subcommand)]
        action: Option<PairCommand>,
    },
    /// Put the Øphio app on an Android phone over Wireless debugging.
    ///
    /// Uses Google's adb, from Android's SDK Platform-Tools. Each step is
    /// a command of its own; `ophio setup` takes them in turn.
    #[command(after_help = "Examples:
  ophio phone pair                       Pair with the phone's Wireless debugging
  ophio phone install                    Install the app that came with this download
  ophio phone link --alerts on           Open the app with this computer's pairing link
  ophio phone off                        Turn Wireless debugging off again")]
    Phone {
        #[command(subcommand)]
        action: crate::phone::PhoneCommand,
        /// The phone to use, by the serial `ophio phone find` lists, when
        /// more than one is connected.
        #[arg(long, global = true, value_name = "SERIAL")]
        device: Option<String>,
        /// Print the result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Show whether Øphio is running and what needs attention.
    ///
    /// Covers the desktop, agents, voice, chat connectors and the folder for
    /// received files. Works whether or not Øphio is running.
    Status {
        /// Print the full records as JSON, for scripts.
        #[arg(long)]
        json: bool,
    },
    /// Show or change the folder for files received from your devices.
    ///
    /// With no subcommand, shows the folder in use.
    Inbox {
        #[command(subcommand)]
        action: Option<InboxCommand>,
        /// Print the full result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// List paired devices, or change what one may do.
    ///
    /// Øphio must be running. With no subcommand, lists each device with
    /// its permissions and when it was last seen.
    Devices {
        #[command(subcommand)]
        action: Option<DeviceCommand>,
        /// Print the full device records, including revoked devices, as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List this computer's accounts, pause them all, or give one its OAuth client.
    ///
    /// Øphio must be running. Sign-ins, keys and new services are added from
    /// the app, under Settings › Accounts. With no subcommand, lists each
    /// account and who it is signed in as; keys are never shown.
    #[command(after_help = "Examples:
  ophio accounts                                  List accounts and their state
  ophio accounts pause-all                        Pause every account for tasks started from now
  ophio accounts set-client google client.json    Give Google its own OAuth client")]
    Accounts {
        #[command(subcommand)]
        action: Option<AccountsCommand>,
        /// Print the full result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// List this computer's agent logins, turn them on, or pause them all.
    ///
    /// Øphio must be running. Logins are added from the app, under Settings ›
    /// Agent logins; passwords are never shown here.
    #[command(after_help = "Examples:
  ophio agent-logins                              List agent logins and their state
  ophio agent-logins enable                       Read the notice and turn agent logins on
  ophio agent-logins pause-all                    Pause every login and close its sites to agents")]
    AgentLogins {
        #[command(subcommand)]
        action: Option<AgentLoginsCommand>,
        /// Print the full result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Print an account's key or a fresh access token.
    ///
    /// For tasks that need to renew the token they were given when they
    /// started. Øphio must be running, and the account must be active.
    Token {
        /// The account's name or ID, as `ophio accounts` shows it.
        account: String,
    },
    /// Add or list the project folders agents work in.
    ///
    /// Øphio must be running.
    Projects {
        #[command(subcommand)]
        action: ProjectCommand,
        /// Print the full result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Show a project's checklist of what you have to do, or change it.
    ///
    /// For agents working outside Øphio: what they need you to do goes on the
    /// checklist, as checklist_add does for agents inside it. Øphio must be
    /// running.
    #[command(after_help = "Examples:
  ophio checklist show .                     The checklist of the project in this folder
  ophio checklist put . items.json           Put the items in items.json on it
  ophio checklist put <PROJECT-ID> - < x.json  Read them from standard input
  ophio checklist remove . make-key          Take an item off")]
    Checklist {
        #[command(subcommand)]
        action: ChecklistCommand,
    },
    /// Share folders with your devices through the Library.
    ///
    /// Library folders are read-only unless added with --writable. Works
    /// whether or not Øphio is running; when it is stopped, changes are saved
    /// and the next start applies them.
    Roots {
        #[command(subcommand)]
        action: RootCommand,
        /// Print the full result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Check everything Øphio needs and say what to fix.
    ///
    /// Øphio must be running. Checks the desktop, terminal, storage,
    /// voice, chat connectors, and each agent's installation and sign-in. Each
    /// problem comes with what to do about it.
    #[command(after_help = "Examples:
  ophio doctor            Check everything and list what needs attention
  ophio doctor --json     Print every check, including each agent's features
  ophio run               Start Øphio first; doctor needs it running")]
    Doctor {
        /// Print every check as JSON, for scripts.
        #[arg(long)]
        json: bool,
    },
    /// Turn access to this computer's screen, mouse and keyboard on or off.
    ///
    /// Off until you turn it on. Works whether or not Øphio is running; when
    /// it is stopped, changes are saved and the next start applies them.
    Desktop {
        #[command(subcommand)]
        action: DesktopCommand,
        /// Print the full result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Choose how many days of event history Øphio keeps.
    Retention {
        #[command(subcommand)]
        action: RetentionCommand,
        /// Print the full result as JSON, for scripts.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Start Øphio automatically when you log in.
    Service {
        #[command(subcommand)]
        action: crate::install::ServiceCommand,
    },
    /// Install Øphio on this computer from an extracted download.
    ///
    /// Run it as the download's own program: `./ophio install`. It puts
    /// Øphio in ~/.local/opt/ophio and the ophio command in ~/.local/bin.
    /// Where Øphio is installed already, this updates it, as `ophio update`
    /// does.
    Install(crate::update::InstallArgs),
    /// Update Øphio from a download, go back, or show what is installed.
    ///
    /// An update waits until no task is running and no approval is waiting,
    /// then restarts Øphio on the new version and checks that it works,
    /// going back to the previous version if it doesn't. It keeps only that
    /// one previous version, and removes everything it staged.
    Update(crate::update::UpdateArgs),
    /// Replace this computer's security certificate.
    ///
    /// Stop Øphio first. Afterwards, every paired device must confirm
    /// the new fingerprint before it reconnects, or pair again.
    RotateCertificate {
        /// Confirm that every device will need to check the new fingerprint.
        #[arg(long)]
        confirm_repin: bool,
    },
    /// Delete Øphio's settings, paired devices and data.
    ///
    /// Stop and uninstall the service first. Accounts signed in through
    /// Øphio are signed out where Øphio can and their keys removed from the
    /// keyring. Received files, project folders, Library folders and agent
    /// logins are kept. Without --yes, it only names the folders it would
    /// delete from.
    UninstallData {
        /// Delete without asking again.
        #[arg(long)]
        yes: bool,
    },
    /// Stop every phone input session and computer-use task on this computer.
    ///
    /// Øphio must be running.
    Stop,
    /// Stop one task.
    ///
    /// Øphio must be running. The agent is asked to stop; --force ends its
    /// process instead. File changes it already made remain.
    StopTask {
        /// The task's ID, as the app shows it.
        task_id: protocol::TaskId,
        /// End the agent's process instead of asking it to stop.
        #[arg(long)]
        force: bool,
    },
}
#[derive(Debug, Subcommand)]
pub enum InboxCommand {
    /// Save files received from your devices to a different folder.
    ///
    /// The folder does not need to exist yet, but it must be outside Øphio's
    /// own folders.
    Set {
        /// The folder for received files.
        path: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum PairCommand {
    /// Pair the device that is asking, once it shows this code.
    ///
    /// This is how to confirm when `ophio pair` runs without a terminal, as
    /// from a script. Check the code on the device's screen first.
    Confirm {
        /// The six digits the device shows, with or without the space.
        #[arg(required = true, num_args = 1..=2, value_name = "CODE")]
        code: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccountsCommand {
    /// Pause every account, so no task started from now receives any of them.
    ///
    /// Tasks already running aren't stopped: they keep the keys and tokens
    /// they were given, though `ophio token` renews none of them. To end a
    /// task, stop it in the app or with `ophio stop-task`. Programs you run
    /// yourself aren't affected. Resume each account in the app.
    PauseAll,
    /// Give an OAuth service its own client, a Desktop app you created.
    ///
    /// The file is the client JSON the provider lets you download. It is kept
    /// in the computer's keyring; the file itself can be deleted afterwards.
    SetClient {
        /// The account's name or ID, as `ophio accounts` shows it.
        account: String,
        /// The client JSON file.
        file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum AgentLoginsCommand {
    /// Show the notice and, once you type yes, turn agent logins on.
    Enable,
    /// Pause every agent login, so no agent signs in with any of them.
    ///
    /// Agents can't use any login or open its sites, but tasks aren't
    /// stopped. To end a task, stop it in the app or with `ophio stop-task`.
    /// Resume the logins in the app.
    PauseAll,
}

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    /// End a device's access. It must pair again to reconnect.
    Revoke {
        /// The device's name or ID, as `ophio devices` shows it.
        device: String,
    },
    /// Let a paired device do more.
    ///
    /// A connected device reconnects with the new permissions.
    Allow {
        /// The device's name or ID, as `ophio devices` shows it.
        device: String,
        /// One or more permissions to add.
        #[arg(required = true, value_enum)]
        permissions: Vec<Permission>,
    },
    /// Stop a paired device doing something.
    ///
    /// A connected device reconnects with the new permissions.
    Deny {
        /// The device's name or ID, as `ophio devices` shows it.
        device: String,
        /// One or more permissions to remove.
        #[arg(required = true, value_enum)]
        permissions: Vec<Permission>,
    },
}
/// What a paired device may do. Control needs view, and editing Library
/// files needs reading them, so allowing one also allows the other and
/// denying the first also denies the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Permission {
    /// Watch the desktop.
    View,
    /// Use the mouse and keyboard when the person takes control.
    Control,
    /// Open terminals.
    Terminal,
    /// Start, steer and stop agent tasks.
    Tasks,
    /// Answer agent requests.
    Approvals,
    /// Let an agent use the desktop.
    ComputerUse,
    /// Read Library folders.
    LibraryRead,
    /// Edit Library files.
    LibraryWrite,
    /// Send and receive files.
    Transfers,
    /// Manage paired devices, chat links and settings.
    Manage,
    /// Sign this computer's accounts in and out, pause them and set keys.
    /// Only the computer can allow it.
    Accounts,
    /// Everything above.
    All,
}
fn permission_mask(permissions: &[Permission]) -> protocol::auth::Grants {
    let mut grants = protocol::auth::Grants::default();
    for permission in permissions {
        match permission {
            Permission::View => grants.view = true,
            Permission::Control => grants.control_input = true,
            Permission::Terminal => grants.terminal = true,
            Permission::Tasks => grants.tasks = true,
            Permission::Approvals => grants.approvals = true,
            Permission::ComputerUse => grants.computer_use = true,
            Permission::LibraryRead => grants.library_read = true,
            Permission::LibraryWrite => grants.library_write = true,
            Permission::Transfers => grants.transfers = true,
            Permission::Manage => grants.admin = true,
            Permission::Accounts => grants.accounts = true,
            Permission::All => grants = protocol::auth::Grants::owner(),
        }
    }
    grants
}
async fn change_permissions(
    client: &mut OwnerClient,
    device: &str,
    permissions: &[Permission],
    allowed: bool,
) -> Result<()> {
    let record = find_device(client, device).await?;
    run_command(
        client,
        Command::ChangeDeviceGrants {
            device_id: record.device_id,
            mask: permission_mask(permissions),
            allowed,
        },
    )
    .await?;
    let current = find_device(client, &record.device_id.to_string()).await?;
    println!(
        "{} {}. If it is connected, it reconnects with the new permissions.",
        shown(&current.name),
        may(&current.grants)
    );
    Ok(())
}
/// "may watch the desktop, …" or "may not do anything".
pub(crate) fn may(g: &protocol::auth::Grants) -> String {
    let allowed: Vec<&str> = [
        (g.view, "watch the desktop"),
        (g.control_input, "control input"),
        (g.terminal, "open terminals"),
        (g.tasks, "run agent tasks"),
        (g.approvals, "answer agent requests"),
        (g.computer_use, "let an agent use the desktop"),
        (g.library_read, "read the Library"),
        (g.library_write, "edit the Library"),
        (g.transfers, "transfer files"),
        (g.admin, "manage access"),
        (g.accounts, "manage accounts"),
    ]
    .into_iter()
    .filter_map(|(on, word)| on.then_some(word))
    .collect();
    if allowed.is_empty() {
        "may not do anything".into()
    } else {
        format!("may {}", allowed.join(", "))
    }
}
/// "just now", "5 minutes ago", "3 days ago".
pub(crate) fn ago(then: protocol::time::Timestamp, now: protocol::time::Timestamp) -> String {
    let minutes = now.0.saturating_sub(then.0).max(0) / 60_000;
    let (count, unit) = match minutes {
        0 => return "just now".into(),
        1..60 => (minutes, "minute"),
        60..1440 => (minutes / 60, "hour"),
        _ => (minutes / 1440, "day"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}
fn describe_devices(
    devices: &[protocol::auth::DeviceRecord],
    now: protocol::time::Timestamp,
) -> String {
    let active: Vec<_> = devices.iter().filter(|d| d.revoked_at.is_none()).collect();
    let revoked = devices.len() - active.len();
    let mut out = String::new();
    if active.is_empty() {
        out.push_str(&format!(
            "No paired devices. Run `{COMMAND} pair` to add one.\n"
        ));
    }
    for device in &active {
        let seen = device.last_seen_at.map_or("never connected".into(), |at| {
            format!("last seen {}", ago(at, now))
        });
        out.push_str(&format!(
            "{} ({}), paired {}, {seen}\n  It {}.\n  ID {}\n",
            shown(&device.name),
            shown(&device.platform),
            ago(device.paired_at, now),
            may(&device.grants),
            device.device_id
        ));
        if let Some(since) = services::device_activity::not_seen_since(device, devices, now) {
            out.push_str(&format!(
                "  Not seen for {} days, and another device has paired since. It still has access; \
                 if it is no longer used, run `{COMMAND} devices revoke {}`.\n",
                now.0.saturating_sub(since.0) / 86_400_000,
                device.device_id
            ));
        }
    }
    if revoked > 0 {
        out.push_str(&format!(
            "{revoked} revoked {} not shown; `--json` lists {}.\n",
            if revoked == 1 { "device" } else { "devices" },
            if revoked == 1 { "it" } else { "them" }
        ));
    }
    out
}
fn describe_accounts(view: &protocol::accounts::AccountsView) -> String {
    use protocol::accounts::AccountState;
    let mut out = String::new();
    for a in &view.accounts {
        let state = match &a.state {
            AccountState::Active => "active".to_owned(),
            AccountState::Paused => "paused".to_owned(),
            AccountState::SignedOut => "signed out".to_owned(),
            AccountState::NeedsAttention { reason } => {
                format!("needs attention: {}", shown(reason))
            }
        };
        out.push_str(&format!(
            "{}  ({})  {state}",
            shown(&a.service),
            a.account_id
        ));
        if let Some(identity) = &a.identity {
            out.push_str(&format!(" as {}", shown(identity)));
        }
        match a.needs {
            Some(protocol::accounts::AccountNeed::Client) => out.push_str(&format!(
                ". Needs its OAuth client: `{COMMAND} accounts set-client {} FILE`",
                a.account_id
            )),
            Some(protocol::accounts::AccountNeed::Key) => out.push_str(". Set its key in the app"),
            None => {}
        }
        out.push('\n');
    }
    out.push_str(&format!("Keys are kept in {}.\n", view.store));
    out
}
fn describe_agent_logins(view: &protocol::agent_logins::AgentLoginsView) -> String {
    use protocol::agent_logins::LoginStatus;
    if !view.enabled {
        return format!(
            "Agent logins are off on this computer. Turn them on with `{COMMAND} agent-logins enable`.\n"
        );
    }
    let mut out = String::new();
    if view.paused_all {
        out.push_str("Every agent login is paused.\n");
    }
    if view.logins.is_empty() {
        out.push_str("No agent logins yet. Add them in the app, under Settings › Agent logins.\n");
    }
    for login in &view.logins {
        let state = match &login.status {
            LoginStatus::Ready => "ready".to_owned(),
            LoginStatus::Paused => "paused".to_owned(),
            LoginStatus::NeedsAttention { reason } => {
                format!("needs attention: {}", shown(reason))
            }
        };
        out.push_str(&format!(
            "{}  {} as {}  {state}\n",
            shown(&login.name),
            login
                .sites
                .iter()
                .map(|s| shown(s))
                .collect::<Vec<_>>()
                .join(", "),
            shown(&login.username),
        ));
    }
    out.push_str(&format!("Passwords are kept in {}.\n", shown(&view.store)));
    out
}
async fn find_account(client: &mut OwnerClient, name: &str) -> Result<protocol::accounts::Account> {
    let QueryData::Accounts { view } = query(client, Query::Accounts).await? else {
        return Err(crate::error::invalid("unexpected accounts response"));
    };
    let wanted = name.trim();
    view.accounts
        .into_iter()
        .find(|a| a.account_id == wanted || a.service.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| {
            Error::Ended(format!(
                "There is no account called {}. `{COMMAND} accounts` lists them.",
                shown(wanted)
            ))
        })
}
/// Reads an OAuth client file, which holds the client's secret.
fn read_client(file: &std::path::Path) -> Result<zeroize::Zeroizing<String>> {
    use std::io::Read;
    let mut text = zeroize::Zeroizing::new(String::new());
    std::fs::File::open(file)?
        .take(64 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|_| Error::Ended(format!("{} is not a text file.", file.display())))?;
    if text.len() > 64 * 1024 {
        return Err(Error::Ended(format!(
            "{} is too large to be an OAuth client file.",
            file.display()
        )));
    }
    Ok(text)
}
async fn find_device(
    client: &mut OwnerClient,
    device: &str,
) -> Result<protocol::auth::DeviceRecord> {
    let QueryData::Devices { devices, .. } = query(client, Query::Devices).await? else {
        return Err(crate::error::invalid("unexpected devices response"));
    };
    let mut found = devices.into_iter().filter(|d| {
        d.revoked_at.is_none()
            && (d.device_id.to_string() == device || d.name.eq_ignore_ascii_case(device))
    });
    match (found.next(), found.next()) {
        (Some(record), None) => Ok(record),
        (None, _) => Err(Error::Config(format!(
            "no paired device is named {device}; run `{COMMAND} devices` to list them"
        ))),
        (Some(_), Some(_)) => Err(Error::Config(format!(
            "more than one paired device is named {device}; use its ID from `{COMMAND} devices`"
        ))),
    }
}
#[derive(Debug, Subcommand)]
pub enum ChecklistCommand {
    /// Print the checklist with every item's id, ticks, answers and questions,
    /// and which items are an agent's own work, done or not.
    Show {
        /// The project's ID, as `ophio projects list` shows it, or its folder.
        project: String,
    },
    /// Put items on the checklist, or rewrite the ones with the same ids.
    ///
    /// Takes the JSON checklist_add takes: title, summary, theme, section and
    /// items. A step's file is a path in the project's folder. Your own work
    /// that waits on the person is an item with owner "agent" and after
    /// naming their open items; mark it done with done: true.
    Put {
        /// The project's ID, as `ophio projects list` shows it, or its folder.
        project: String,
        /// The JSON file, or - for standard input.
        file: PathBuf,
    },
    /// Take items off the checklist.
    Remove {
        /// The project's ID, as `ophio projects list` shows it, or its folder.
        project: String,
        /// The ids of the items, as `ophio checklist show` prints them.
        #[arg(required = true)]
        ids: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Register a folder as a project, so devices can start agent tasks in it.
    #[command(after_help = "Examples:
  ophio projects add ~/code/website                   Name the project after its folder
  ophio projects add ~/code/website --name \"Web site\"  Choose the name the app shows")]
    Add {
        /// The project's folder.
        path: PathBuf,
        /// The name the app shows. Defaults to the folder's name.
        #[arg(long)]
        name: Option<String>,
    },
    /// List registered projects with their IDs.
    List,
    /// Choose whether Claude Code uses the settings in a project's folder.
    ///
    /// A project's .claude/settings.json and .claude/settings.local.json can
    /// let the agent use tools without asking and run commands whenever it
    /// works. Work runs started from a device ignore them until you turn them
    /// on here for that project. Its CLAUDE.md instructions apply either way.
    #[command(after_help = "Examples:
  ophio projects list                       Find the project's ID
  ophio projects settings <PROJECT-ID> on   Use the settings in its folder
  ophio projects settings <PROJECT-ID> off  Ignore them again")]
    Settings {
        /// The project's ID, as `ophio projects list` shows it.
        project: protocol::ProjectId,
        /// on to use the settings, off to ignore them.
        #[arg(value_enum)]
        state: Switch,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Switch {
    On,
    Off,
}
#[derive(Debug, Subcommand)]
pub enum DesktopCommand {
    /// Let paired devices view and control the desktop.
    ///
    /// Each device also needs the view or control permission; see
    /// `ophio devices allow`. On Linux, the desktop may ask you to approve
    /// screen sharing.
    Enable,
    /// End all desktop viewing and control now.
    ///
    /// On Linux, this also forgets the saved screen-sharing approval.
    Disable,
    /// Show whether desktop access is on and available.
    Status,
}
#[derive(Debug, Subcommand)]
pub enum RetentionCommand {
    /// Keep this many days of event history; older events are deleted.
    ///
    /// Projects, tasks and files are kept. Works whether or not Øphio is
    /// running; when it is stopped, the next start applies the change.
    Set {
        /// Number of days, 1 or more. The default is 30.
        days: u16,
    },
}
#[derive(Debug, Subcommand)]
pub enum RootCommand {
    /// Stop sharing a Library folder. The folder and its files are not changed.
    Remove {
        /// The folder's ID, from `ophio roots list`.
        id: protocol::LibraryRootId,
    },
    /// Share a folder in the Library.
    #[command(after_help = "Examples:
  ophio roots add ~/Notes --label Notes     Share a folder read-only
  ophio roots add ~/Drafts --writable       Let devices edit the files in it
  ophio roots list                          See the folders and their IDs")]
    Add {
        /// The folder to share. It must exist and be outside Øphio's own
        /// folders.
        path: PathBuf,
        /// The name the app shows. Defaults to the folder's name.
        #[arg(long)]
        label: Option<String>,
        /// Let devices with the library-write permission change its files.
        #[arg(long)]
        writable: bool,
    },
    /// Change a Library folder's settings. Its ID and location stay the same.
    Set {
        /// The folder's ID, from `ophio roots list`.
        id: protocol::LibraryRootId,
        /// What the folder holds: notes, drafts, skills, agents, instructions
        /// or project.
        #[arg(long, value_parser = parse_root_kind)]
        kind: Option<protocol::library::RootKind>,
        /// Link the folder to a project, by its ID from `ophio projects list`.
        #[arg(long, conflicts_with = "clear_project")]
        project_id: Option<protocol::ProjectId>,
        /// Remove the folder's project link.
        #[arg(long)]
        clear_project: bool,
        /// true lets devices with the library-write permission change its
        /// files; false makes it read-only.
        #[arg(long, action = clap::ArgAction::Set)]
        writable: Option<bool>,
        /// Link the folder to one agent: claude_code, codex, hermes, open_claw
        /// or pi.
        #[arg(long, value_parser = parse_root_harness, conflicts_with = "clear_harness")]
        harness: Option<protocol::capability::HarnessKind>,
        /// Remove the folder's agent link.
        #[arg(long)]
        clear_harness: bool,
    },
    /// List Library folders with their IDs.
    List,
}

/// Application composition seam. The services crate supplies this factory;
/// transports never construct an alternative state authority.
#[async_trait::async_trait]
pub trait HubFactory: Send + Sync {
    async fn build(
        &self,
        paths: &Paths,
        config: &Config,
        identity: &Identity,
    ) -> Result<crate::compose::Application>;
}

/// Opened with no command in a console of its own, as a double-click in
/// Explorer does, `ophio.exe` would show its usage in a window that closes at
/// once. It says how to start Øphio instead, and keeps the window until it has
/// been read. True when it has.
#[cfg(windows)]
pub fn opened_from_explorer() -> bool {
    if std::env::args_os().len() > 1
        || !platform_windows::background::own_console()
        || crate::setup::pending_first_run()
    {
        // Setup starts instead, and keeps the window until it is read.
        return false;
    }
    println!(
        "{PRODUCT_NAME} runs in the background, and is set up from a terminal.\n\n\
         To go through its setup again, type powershell in this folder's address\n\
         bar, press Enter, and run:\n\n    .\\ophio.exe setup\n\n\
         To pair another phone, run:\n\n    .\\ophio.exe pair\n\nPress Enter to close this window."
    );
    let _ = std::io::stdin().read_line(&mut String::new());
    true
}

/// What only the person at the computer does, refused when an agent's task
/// asks: pairing a device, changing what one may do, setup itself, and the
/// commands that end every pairing. This doesn't stop a determined agent
/// running as the person; it stops mistakes, and a page talking an agent into
/// pairing a stranger.
fn refused_in_task(action: &Action) -> Option<String> {
    let what = match action {
        Action::Setup(_) => "set Øphio up",
        Action::Pair { action: None } => "show a pairing code",
        Action::Pair {
            action: Some(PairCommand::Confirm { .. }),
        } => "confirm a pairing",
        Action::Devices {
            action: Some(DeviceCommand::Allow { .. } | DeviceCommand::Deny { .. }),
            ..
        } => "change what a device may do",
        Action::Devices {
            action: Some(DeviceCommand::Revoke { .. }),
            ..
        } => "end a device's access",
        // The phone's own screen shows the other half, so a task's output,
        // read only once the command ends, can't be scanned or typed in time.
        Action::Phone {
            action: crate::phone::PhoneCommand::Pair { .. },
            ..
        } => "pair with a phone's Wireless debugging",
        Action::RotateCertificate { .. } => "replace this computer's certificate",
        Action::UninstallData { .. } => "delete Øphio's data",
        Action::Install(_) => "install or update Øphio",
        Action::Update(args) if args.changes() => "install or update Øphio",
        Action::AgentLogins {
            action: Some(AgentLoginsCommand::Enable),
            ..
        } => "turn agent logins on",
        _ => return None,
    };
    Some(format!(
        "An agent's task can't {what}; only the person at this computer can. Ask them to run this command in their own terminal."
    ))
}

/// Said under a device asking to pair when an agent's task is waiting for
/// it: the person confirms, never the task.
fn person_confirms() -> String {
    format!(
        "Only the person at this computer can pair it: ask them to compare the code and confirm it in `{COMMAND} setup`, or with `{COMMAND} pair confirm` in their own terminal."
    )
}

pub async fn execute(cli: Cli, factory: &dyn HubFactory) -> Result<()> {
    if std::env::var_os(ports::TASK_ENV).is_some()
        && let Some(refusal) = refused_in_task(&cli.command)
    {
        return Err(Error::Ended(refusal));
    }
    // These work on the install, whichever data folders it uses.
    match cli.command {
        Action::Service { action } => return crate::install::service(action).await,
        Action::Install(args) => return crate::update::install(args).await,
        Action::Update(args) => return crate::update::update(args).await,
        _ => {}
    }
    let paths = Paths::discover()?;
    #[cfg(windows)]
    if let Action::Run = cli.command
        && platform_windows::background::own_console()
    {
        // Windows opened a console just for this run, at login or from
        // Explorer, and would end Øphio when it is closed. Øphio carries on
        // without one; if it is running already, there is nothing to start.
        if OwnerClient::connect(&paths.socket()).await.is_ok() {
            return Ok(());
        }
        if crate::install::start_in_background(&std::env::current_exe()?).is_ok() {
            return Ok(());
        }
    }
    match cli.command {
        Action::Setup(args) => crate::setup::execute(&paths, args).await,
        Action::Run => run(paths, factory).await,
        Action::Jev { action, json } => crate::jev::execute(&paths, action, json).await,
        Action::Pair { action: None } => pair(&paths, None).await,
        Action::Phone {
            action,
            device,
            json,
        } => crate::phone::execute(&paths, action, device, json).await,
        Action::Pair {
            action: Some(PairCommand::Confirm { code }),
        } => confirm_pairing(&paths, &code.concat()).await,
        Action::StopTask { task_id, force } => {
            let mut client = OwnerClient::connect(&paths.socket()).await?;
            let response: OwnerResponse = client
                .request(&OwnerRequest::StopTask { task_id, force })
                .await?;
            let OwnerResponse::TaskStop(receipt) = response else {
                return Err(Error::Config("Unexpected task Stop response".into()));
            };
            if let Some(error) = receipt.error {
                return Err(error.into());
            }
            println!(
                "{} requested. Check the task receipt for confirmed process and child outcomes.",
                if force { "Force stop" } else { "Stop" }
            );
            Ok(())
        }
        Action::Stop => {
            let mut client = OwnerClient::connect(&paths.socket()).await?;
            let response: OwnerResponse = client.request(&OwnerRequest::Stop).await?;
            let OwnerResponse::ControlStopped { tasks_requested } = response else {
                return Err(Error::Closed);
            };
            println!(
                "Computer input stopped. Stop requested for {tasks_requested} computer-use tasks; native exit is confirmed separately in the app."
            );
            Ok(())
        }
        Action::Voice { action, json } => {
            match frontdoor_settings(&paths, OwnerRequest::Voice { action }).await? {
                response if json => print_json(&response),
                OwnerResponse::Frontdoors(status) => {
                    print!("{}", readable::voice(&status.voice, Style::stdout()));
                    Ok(())
                }
                _ => Err(Error::Closed),
            }
        }
        Action::Channels { action, json } => {
            use crate::frontdoors::{ChannelsCli, TokenCommand, TokenInput};
            let request = match action {
                ChannelsCli::Settings(action) => OwnerRequest::Channels { action },
                ChannelsCli::Token {
                    action: TokenCommand::Clear { connector },
                } => OwnerRequest::TokenClear { connector },
                ChannelsCli::Token {
                    action: TokenCommand::Set { connector },
                } => {
                    let token = tokio::task::spawn_blocking(TokenInput::read_stdin)
                        .await
                        .map_err(|_| Error::Config("Token input failed".into()))??;
                    OwnerRequest::TokenSet { connector, token }
                }
            };
            match frontdoor_settings(&paths, request).await? {
                response if json => print_json(&response),
                OwnerResponse::Frontdoors(status) => {
                    print!("{}", readable::channels(&status, Style::stdout()));
                    Ok(())
                }
                _ => Err(Error::Closed),
            }
        }
        Action::Inbox { action, json } => {
            if let Some(InboxCommand::Set { path }) = action {
                let response = settings(&paths, OwnerRequest::InboxSet { path }).await?;
                if json {
                    return print_json(&response);
                }
            }
            let inbox = inbox_status(&paths)?;
            if json {
                return print_json(&serde_json::to_value(inbox).map_err(|_| Error::Closed)?);
            }
            print!("{}", readable::inbox(&inbox, Style::stdout()));
            Ok(())
        }
        Action::Status { json } => {
            let inbox = inbox_status(&paths)?;
            let mut client = match OwnerClient::connect(&paths.socket()).await {
                Ok(client) => client,
                Err(Error::NotRunning) => {
                    if json {
                        return print_json(&serde_json::json!({"running": false, "inbox": inbox}));
                    }
                    print!("{}", readable::stopped(&inbox, Style::stdout()));
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            let snapshot = query(
                &mut client,
                Query::Snapshot {
                    reads_withheld: Some(true),
                },
            )
            .await?;
            let harnesses = query(&mut client, Query::Harnesses).await?;
            let readiness = query(&mut client, Query::Readiness).await?;
            let frontdoors: OwnerResponse = client.request(&OwnerRequest::FrontdoorStatus).await?;
            let OwnerResponse::Frontdoors(frontdoors) = frontdoors else {
                return Err(Error::Closed);
            };
            if json {
                return print_json(
                    &serde_json::json!({"running": true, "inbox": inbox, "snapshot": snapshot, "harnesses": harnesses, "readiness": readiness, "voice": frontdoors.voice, "channels": frontdoors.channels}),
                );
            }
            let (QueryData::Snapshot(snapshot), QueryData::Harnesses { harnesses }) =
                (&snapshot, &harnesses)
            else {
                return Err(Error::Closed);
            };
            print!(
                "{}",
                readable::status(snapshot, harnesses, &frontdoors, &inbox, Style::stdout())
            );
            Ok(())
        }
        Action::Doctor { json } => {
            let mut client = OwnerClient::connect(&paths.socket()).await?;
            let readiness = query(&mut client, Query::Readiness).await?;
            let OwnerResponse::Frontdoors(frontdoors) =
                client.request(&OwnerRequest::FrontdoorStatus).await?
            else {
                return Err(Error::Closed);
            };
            let inbox = inbox_status(&paths)?;
            if json {
                let mut readiness = serde_json::to_value(readiness).map_err(|_| Error::Closed)?;
                readiness["inbox"] = serde_json::to_value(inbox).map_err(|_| Error::Closed)?;
                readiness["voice"] =
                    serde_json::to_value(frontdoors.voice).map_err(|_| Error::Closed)?;
                readiness["channels"] =
                    serde_json::to_value(frontdoors.channels).map_err(|_| Error::Closed)?;
                return print_json(&readiness);
            }
            let QueryData::Readiness { report } = &readiness else {
                return Err(Error::Closed);
            };
            print!(
                "{}",
                readable::doctor(report, &frontdoors, &inbox, Style::stdout())
            );
            Ok(())
        }
        Action::Devices { action, json } => {
            let mut client = OwnerClient::connect(&paths.socket()).await?;
            match action {
                Some(DeviceCommand::Revoke { device }) => {
                    let record = find_device(&mut client, &device).await?;
                    run_command(
                        &mut client,
                        Command::RevokeDevice {
                            device_id: record.device_id,
                        },
                    )
                    .await?;
                    println!(
                        "{} can no longer connect. To use it again, run `{COMMAND} pair` and pair it again.",
                        shown(&record.name)
                    );
                    Ok(())
                }
                Some(DeviceCommand::Allow {
                    device,
                    permissions,
                }) => change_permissions(&mut client, &device, &permissions, true).await,
                Some(DeviceCommand::Deny {
                    device,
                    permissions,
                }) => change_permissions(&mut client, &device, &permissions, false).await,
                None => match query(&mut client, Query::Devices).await? {
                    data if json => print_json(&data),
                    QueryData::Devices { devices, .. } => {
                        print!(
                            "{}",
                            describe_devices(&devices, protocol::time::Timestamp::now())
                        );
                        Ok(())
                    }
                    _ => Err(crate::error::invalid("unexpected devices response")),
                },
            }
        }
        Action::Accounts { action, json } => {
            let mut client = OwnerClient::connect(&paths.socket()).await?;
            match action {
                None => match query(&mut client, Query::Accounts).await? {
                    data if json => print_json(&data),
                    QueryData::Accounts { view } => {
                        print!("{}", describe_accounts(&view));
                        Ok(())
                    }
                    _ => Err(crate::error::invalid("unexpected accounts response")),
                },
                Some(AccountsCommand::PauseAll) => {
                    let done = finish_command(
                        &mut client,
                        Command::Account {
                            action: protocol::accounts::AccountAction::PauseAll,
                        },
                    )
                    .await?;
                    if json {
                        return print_json(&done);
                    }
                    println!(
                        "Every account is paused. Tasks {PRODUCT_NAME} starts get none of them until you resume each one in the app. Tasks already running keep what they were given; to end one, stop it in the app or run `{COMMAND} stop-task`."
                    );
                    Ok(())
                }
                Some(AccountsCommand::SetClient { account, file }) => {
                    let account = find_account(&mut client, &account).await?;
                    let client_json = read_client(&file)?;
                    let done = finish_command(
                        &mut client,
                        Command::Account {
                            action: protocol::accounts::AccountAction::SetKey {
                                account_id: account.account_id.clone(),
                                key: protocol::accounts::Secret::new(client_json.as_str()),
                            },
                        },
                    )
                    .await?;
                    if json {
                        return print_json(&done);
                    }
                    println!(
                        "{} has its OAuth client. Sign in to it from the app.",
                        shown(&account.service)
                    );
                    Ok(())
                }
            }
        }
        Action::AgentLogins { action, json } => {
            let mut client = OwnerClient::connect(&paths.socket()).await?;
            let QueryData::AgentLogins { view } = query(&mut client, Query::AgentLogins).await?
            else {
                return Err(crate::error::invalid("unexpected agent logins response"));
            };
            match action {
                None if json => print_json(&QueryData::AgentLogins { view }),
                None => {
                    print!("{}", describe_agent_logins(&view));
                    Ok(())
                }
                Some(AgentLoginsCommand::Enable) => {
                    if view.enabled {
                        println!("Agent logins are already on.");
                        return Ok(());
                    }
                    use std::io::{IsTerminal, Write};
                    if !std::io::stdin().is_terminal() {
                        return Err(Error::Ended(
                            "Turning agent logins on needs a typed yes; run this in a terminal."
                                .into(),
                        ));
                    }
                    println!("{}\n", shown(&view.disclaimer_title));
                    for paragraph in &view.disclaimer {
                        println!("{}\n", shown(paragraph));
                    }
                    print!(
                        "{}\nType yes to turn agent logins on: ",
                        shown(&view.disclaimer_confirm)
                    );
                    std::io::stdout().flush()?;
                    let mut answer = String::new();
                    std::io::stdin().read_line(&mut answer)?;
                    if !answer.trim().eq_ignore_ascii_case("yes") {
                        return Err(Error::Ended("Agent logins stay off.".into()));
                    }
                    let done = finish_command(
                        &mut client,
                        Command::AgentLogin {
                            action: protocol::agent_logins::AgentLoginAction::Accept {
                                version: view.disclaimer_version,
                            },
                        },
                    )
                    .await?;
                    if json {
                        return print_json(&done);
                    }
                    println!(
                        "Agent logins are on. Add logins in the app, under Settings › Agent logins."
                    );
                    Ok(())
                }
                Some(AgentLoginsCommand::PauseAll) => {
                    let done = finish_command(
                        &mut client,
                        Command::AgentLogin {
                            action: protocol::agent_logins::AgentLoginAction::PauseAll,
                        },
                    )
                    .await?;
                    if json {
                        return print_json(&done);
                    }
                    println!(
                        "Every agent login is paused. Tasks keep running; to end one, stop it in the app or run `{COMMAND} stop-task`. Resume the logins in the app."
                    );
                    Ok(())
                }
            }
        }
        Action::Token { account } => {
            let mut client = OwnerClient::connect(&paths.socket()).await?;
            // Inside a task Ophio started, the task's own credential asks
            // for what it was given; the owner's controls refuse it.
            let run = std::env::var(ports::accounts::RENEW_ENV)
                .ok()
                .filter(|v| !v.is_empty())
                .map(protocol::accounts::Secret::new);
            let OwnerResponse::AccountToken(token) = client
                .request(&OwnerRequest::AccountToken { account, run })
                .await?
            else {
                return Err(Error::Closed);
            };
            println!("{token}");
            Ok(())
        }
        Action::Checklist { action } => {
            let mut client = OwnerClient::connect(&paths.socket()).await?;
            let (project, put, remove) = match action {
                ChecklistCommand::Show { project } => (project, None, Vec::new()),
                ChecklistCommand::Put { project, file } => {
                    let text = if file.as_os_str() == "-" {
                        std::io::read_to_string(std::io::stdin())?
                    } else {
                        std::fs::read_to_string(&file)?
                    };
                    let put = serde_json::from_str(&text).map_err(|e| {
                        Error::Config(format!(
                            "{} is not a checklist: {e}. `ophio checklist put --help` says what it takes.",
                            file.display()
                        ))
                    })?;
                    (project, Some(put), Vec::new())
                }
                ChecklistCommand::Remove { project, ids } => (project, None, ids),
            };
            let project_id = checklist_project(&mut client, &project).await?;
            let OwnerResponse::Checklist(said) = client
                .request(&OwnerRequest::Checklist {
                    project_id,
                    put,
                    remove,
                })
                .await?
            else {
                return Err(Error::Closed);
            };
            print!("{said}");
            Ok(())
        }
        Action::Projects {
            action: ProjectCommand::Settings { project, state },
            json,
        } => {
            let mut client = OwnerClient::connect(&paths.socket()).await?;
            let response: OwnerResponse = client
                .request(&OwnerRequest::ProjectSettings {
                    project_id: project,
                    apply: state == Switch::On,
                })
                .await?;
            let OwnerResponse::ProjectSettings { project, apply } = response else {
                return Err(Error::Closed);
            };
            if json {
                return print_json(&serde_json::json!({ "project": project, "apply": apply }));
            }
            print!("{}", readable::project_settings(&project, apply));
            Ok(())
        }
        Action::Projects { action, json } => {
            let mut client = OwnerClient::connect(&paths.socket()).await?;
            let added = match action {
                ProjectCommand::Add { path, name } => {
                    let receipt = run_command(
                        &mut client,
                        Command::AddProject {
                            host_path: ports::library::canonical(&path)?
                                .to_str()
                                .ok_or_else(|| Error::Config("project path must be UTF-8".into()))?
                                .into(),
                            name,
                        },
                    )
                    .await?;
                    if json {
                        return print_json(&receipt);
                    }
                    let Some(protocol::command::CommandResult::ProjectAdded { project_id }) =
                        receipt.result
                    else {
                        return Err(Error::Closed);
                    };
                    Some(project_id)
                }
                ProjectCommand::List => None,
                ProjectCommand::Settings { .. } => unreachable!("handled above"),
            };
            let QueryData::Snapshot(snapshot) = query(
                &mut client,
                Query::Snapshot {
                    reads_withheld: Some(true),
                },
            )
            .await?
            else {
                return Err(Error::Closed);
            };
            match added {
                _ if json => print_json(&snapshot.projects),
                Some(id) => {
                    let project = snapshot
                        .projects
                        .iter()
                        .find(|p| p.project_id == id)
                        .ok_or(Error::Closed)?;
                    print!("{}", readable::project_added(project));
                    Ok(())
                }
                None => {
                    print!("{}", readable::projects(&snapshot.projects));
                    Ok(())
                }
            }
        }
        Action::Roots { action, json } => {
            let request = match action {
                RootCommand::Add {
                    path,
                    label,
                    writable,
                } => OwnerRequest::RootsAdd {
                    path,
                    label,
                    writable,
                },
                RootCommand::Remove { id } => OwnerRequest::RootsRemove { root_id: id },
                RootCommand::List => OwnerRequest::RootsList,
                RootCommand::Set {
                    id,
                    kind,
                    project_id,
                    clear_project,
                    writable,
                    harness,
                    clear_harness,
                } => OwnerRequest::RootsSet {
                    root_id: id,
                    kind,
                    project_id,
                    clear_project,
                    writable,
                    harness,
                    clear_harness,
                },
            };
            match settings(&paths, request).await? {
                response if json => print_json(&response),
                OwnerResponse::Settings { config, .. } => {
                    print!("{}", readable::roots(&config.roots));
                    Ok(())
                }
                _ => Err(Error::Closed),
            }
        }
        Action::Desktop { action, json } => {
            let enabled = match action {
                DesktopCommand::Enable => Some(true),
                DesktopCommand::Disable => Some(false),
                DesktopCommand::Status => None,
            };
            match settings(&paths, OwnerRequest::Desktop { enabled }).await? {
                response if json => print_json(&response),
                OwnerResponse::Settings {
                    config, desktop, ..
                } => {
                    print!(
                        "{}",
                        readable::desktop(config.desktop_enabled, &desktop, Style::stdout())
                    );
                    Ok(())
                }
                _ => Err(Error::Closed),
            }
        }
        Action::Retention {
            action: RetentionCommand::Set { days },
            json,
        } => match settings(&paths, OwnerRequest::RetentionSet { days }).await? {
            response if json => print_json(&response),
            OwnerResponse::Settings { config, .. } => {
                print!("{}", readable::retention(config.retention_days));
                Ok(())
            }
            _ => Err(Error::Closed),
        },
        Action::RotateCertificate { confirm_repin } => {
            if !confirm_repin {
                return Err(Error::Config("rotation requires --confirm-repin; all devices must verify the new fingerprint before reconnecting".into()));
            }
            paths.initialize()?;
            let _lock = paths.lock()?;
            let config = Config::load(&paths.config_file())?;
            let discovery = Discovery::detect().await;
            let identity = Identity::rotate(
                &FileSecretStore::new(paths.identity()),
                discovery.certificate_names(&config),
            )?;
            println!(
                "Certificate rotated. Every device must re-pin before reconnecting.\nSHA-256: {}",
                identity.fingerprint()
            );
            Ok(())
        }
        Action::UninstallData { yes } => {
            uninstall_data(&paths, yes, accounts::Config::computer).await
        }
        Action::Service { .. } | Action::Install(_) | Action::Update(_) => {
            unreachable!("handled before path discovery")
        }
    }
}

pub async fn run(paths: Paths, factory: &dyn HubFactory) -> Result<()> {
    paths.initialize()?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("service")
        .max_log_files(7)
        .build(paths.data.join("logs"))
        .map_err(|_| Error::Config(format!("cannot open the {PRODUCT_NAME} log")))?;
    let (writer, _log_guard) = tracing_appender::non_blocking(appender);
    let _ = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_writer(writer)
        .try_init();
    let result = serve(&paths, factory).await;
    if let Err(error) = &result {
        // Run in the background, Øphio has no terminal to say this in.
        tracing::error!(error = %shown(&error.to_string()), "{PRODUCT_NAME} stopped");
    }
    result
}

async fn serve(paths: &Paths, factory: &dyn HubFactory) -> Result<()> {
    let _lock = paths.lock()?;
    // Before anything is started, so that everything started afterwards counts.
    let started = crate::started::Started::claim()?;
    let config = Config::load(&paths.config_file())?;
    if !paths.config_file().exists() {
        config.save(&paths.config_file())?;
    }
    let discovery = Discovery::detect().await;
    let identity = Arc::new(Identity::load_or_create(
        &FileSecretStore::new(paths.identity()),
        discovery.certificate_names(&config),
    )?);
    let application = factory.build(paths, &config, &identity).await?;
    let serving = async {
        let mut deps = ServerDeps::new(application.hub.clone(), identity)?;
        deps.owner = Some(application.owner.clone());
        deps.addresses = discovery.offer_addresses(&config)?.into();
        let mut server = Server::bind(deps.clone(), &discovery.listen_addresses(&config)?).await?;
        let (listening, later) = (server.local_addresses()?, server.later());
        let local = crate::local::LocalServer::bind(&paths.socket(), started).await?;
        tracing::info!("{PRODUCT_NAME} listening");
        // Register synchronously before serving. Tokio keeps the native close
        // handler alive while we drain; Windows still imposes its own deadline.
        #[cfg(windows)]
        let (mut ctrl_c, mut ctrl_break, mut ctrl_close) = (
            tokio::signal::windows::ctrl_c()?,
            tokio::signal::windows::ctrl_break()?,
            tokio::signal::windows::ctrl_close()?,
        );
        let shutdown = tokio_util::sync::CancellationToken::new();
        let signal_token = shutdown.clone();
        let health_owner = application.owner.clone();
        let health_stop = shutdown.clone();
        let health = tokio::spawn(async move {
            tokio::select! { _ = health_stop.cancelled() => {}, _ = health_owner.failed() => health_stop.cancel() }
        });
        let signal = tokio::spawn(async move {
            #[cfg(unix)]
            {
                if let Ok(mut term) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
                } else {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
            #[cfg(windows)]
            {
                tokio::select! {
                    _ = ctrl_c.recv() => {},
                    _ = ctrl_break.recv() => {},
                    _ = ctrl_close.recv() => {},
                }
            }
            #[cfg(not(any(unix, windows)))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
            signal_token.cancel();
        });
        let tailscale = tokio::spawn({
            let (config, offered, stop) = (config.clone(), deps.addresses.clone(), shutdown.clone());
            async move {
                crate::network::follow(&config, Discovery::detect, listening, later, offered, stop)
                    .await;
            }
        });
        let network_token = shutdown.clone();
        let local_token = shutdown.clone();
        let serving = async { tokio::join!(
            async {
                let result = server.run(network_token).await;
                shutdown.cancel();
                result
            },
            async {
                let result = local.run(deps, local_token).await;
                shutdown.cancel();
                result
            },
        ) };
        #[cfg(windows)]
        let ((network_result, local_result), stopped) = tokio::join!(serving, async {
            shutdown.cancelled().await;
            // Begin ordered cleanup immediately, even if a network peer is
            // slow to close. The native console handler cannot extend the OS deadline.
            application.shutdown().await
        });
        #[cfg(not(windows))]
        let (network_result, local_result) = serving.await;
        signal.abort();
        let _ = signal.await;
        let _ = health.await;
        let _ = tailscale.await;
        #[cfg(windows)]
        stopped?;
        network_result?;
        local_result
    }
    .await;
    let stopped = application.shutdown().await;
    serving?;
    stopped
}
pub(crate) async fn query(client: &mut OwnerClient, query: Query) -> Result<QueryData> {
    let query_id = protocol::QueryId::new();
    match client
        .request::<_, ServerFrame>(&ClientFrame::Query(QueryEnvelope { query_id, query }))
        .await?
    {
        ServerFrame::QueryResult(result) if result.query_id == query_id => {
            result.result.map_err(Error::from)
        }
        _ => Err(crate::error::invalid("unexpected query response")),
    }
}
/// The project `named` is, by its ID or its folder, or the folder of a
/// project it is in.
async fn checklist_project(client: &mut OwnerClient, named: &str) -> Result<protocol::ProjectId> {
    if let Ok(id) = named.parse::<protocol::ProjectId>() {
        return Ok(id);
    }
    let QueryData::Snapshot(snapshot) = query(
        client,
        Query::Snapshot {
            reads_withheld: Some(true),
        },
    )
    .await?
    else {
        return Err(Error::Closed);
    };
    let folder = ports::library::canonical(Path::new(named)).map_err(|_| {
        Error::Config(format!(
            "No project is {named}. `ophio projects list` shows each project's ID and folder."
        ))
    })?;
    (snapshot.projects.iter())
        .filter(|p| folder.starts_with(&p.host_path))
        .max_by_key(|p| p.host_path.len())
        .map(|p| p.project_id)
        .ok_or_else(|| {
            Error::Config(format!(
                "{} is not in a project. `ophio projects list` shows each project's ID and folder.",
                folder.display()
            ))
        })
}

pub(crate) async fn run_command(
    client: &mut OwnerClient,
    command: Command,
) -> Result<protocol::CommandReceipt> {
    run_command_at(client, command, None).await
}
/// Run a command that is admitted only at the revision it was decided on:
/// a stale one is refused, never applied to what has changed since.
pub(crate) async fn run_command_at(
    client: &mut OwnerClient,
    command: Command,
    revision: Option<u64>,
) -> Result<protocol::CommandReceipt> {
    let mut envelope = CommandEnvelope::new(command);
    envelope.expected_revision = revision;
    let id = envelope.command_id;
    match client
        .request::<_, ServerFrame>(&ClientFrame::Command(Box::new(envelope)))
        .await?
    {
        ServerFrame::Receipt(receipt) if receipt.command_id == id => match receipt.error {
            Some(error) => Err(error.into()),
            None => Ok(receipt),
        },
        _ => Err(crate::error::invalid("unexpected command receipt")),
    }
}
/// Run a command and wait for what it came to: its receipt says only that
/// it was taken. Its status, once it has ended well, or its error.
pub(crate) async fn finish_command(
    client: &mut OwnerClient,
    command: Command,
) -> Result<QueryData> {
    use protocol::command::CommandStage;
    let receipt = run_command(client, command).await?;
    let started = std::time::Instant::now();
    loop {
        let status = query(
            client,
            Query::CommandStatus {
                command_id: receipt.command_id,
            },
        )
        .await?;
        let QueryData::CommandStatus { stage, error, .. } = &status else {
            return Err(crate::error::invalid("unexpected command status"));
        };
        match stage {
            Some(CommandStage::Observed) => return Ok(status),
            Some(
                CommandStage::Failed
                | CommandStage::Uncertain
                | CommandStage::ReconciliationRequired,
            ) => {
                return Err(match error {
                    Some(error) => (**error).clone().into(),
                    None => Error::Ended(format!(
                        "{PRODUCT_NAME} couldn't say whether it was done. Check in the app."
                    )),
                });
            }
            _ => {}
        }
        if started.elapsed() > Duration::from_secs(60) {
            return Err(Error::Ended(format!(
                "{PRODUCT_NAME} hasn't finished it after a minute. Check in the app."
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|_| crate::error::invalid("cannot render result"))?
    );
    Ok(())
}
pub fn pairing_link(offer: &protocol::auth::PairingOffer) -> Result<String> {
    Ok(format!(
        "{PAIR_SCHEME}://pair?d={}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(offer).map_err(|_| Error::Identity)?)
    ))
}
/// The same offer packed for the QR code, so the code fits a default terminal
/// window: `<scheme>://pair?c=<base32>`, in about half the characters of
/// [`pairing_link`]. `None` when a field does not fit the packed form; the QR
/// code then carries the full link.
///
/// The bytes are format 1, the host id (16), the certificate hash (32), the
/// pairing secret (32), the expiry in milliseconds (i64), the protocol major
/// and minor (u16 each), all big-endian; then the host name, an address count
/// and each address, every text a length byte and UTF-8. Base32 (RFC 4648,
/// upper case, no padding) keeps to the characters a QR code stores in its
/// compact alphanumeric mode. `client/src/protocol/pairing.ts` reads it.
pub fn pairing_qr_link(offer: &protocol::auth::PairingOffer) -> Option<String> {
    fn text(bytes: &mut Vec<u8>, text: &str) -> Option<()> {
        bytes.push(u8::try_from(text.len()).ok()?);
        bytes.extend_from_slice(text.as_bytes());
        Some(())
    }
    let mut bytes = vec![1];
    bytes.extend_from_slice(offer.host_id.as_uuid().as_bytes());
    let cert = offer.cert_sha256.as_bytes();
    if cert.len() != 64 || !cert.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    for pair in cert.chunks(2) {
        bytes.push(u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?);
    }
    let secret = URL_SAFE_NO_PAD.decode(&offer.pairing_secret).ok()?;
    if secret.len() != 32 || URL_SAFE_NO_PAD.encode(&secret) != offer.pairing_secret {
        return None;
    }
    bytes.extend_from_slice(&secret);
    bytes.extend_from_slice(&offer.expires_at.0.to_be_bytes());
    bytes.extend_from_slice(&offer.protocol.major.to_be_bytes());
    bytes.extend_from_slice(&offer.protocol.minor.to_be_bytes());
    text(&mut bytes, &offer.host_name)?;
    if offer.addresses.is_empty() {
        return None;
    }
    bytes.push(u8::try_from(offer.addresses.len()).ok()?);
    for address in &offer.addresses {
        text(&mut bytes, address)?;
    }
    Some(format!("{PAIR_SCHEME}://pair?c={}", base32(&bytes)))
}
fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let (mut buffer, mut bits) = (0u16, 0u16);
    for &byte in bytes {
        buffer = (buffer << 8) | u16::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[usize::from((buffer >> bits) & 31)]));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(
            ALPHABET[usize::from((buffer << (5 - bits)) & 31)],
        ));
    }
    out
}
/// The QR code as terminal text, two modules to a character cell, with a
/// margin of two modules. The terminal's own background continues the margin
/// on either colour scheme, so a wider one would only push the code's top out
/// of a default-sized window.
pub(crate) fn pairing_qr(link: &str) -> Result<String> {
    use qrcode::{EcLevel, QrCode, render::unicode};
    let code = QrCode::with_error_correction_level(link.as_bytes(), EcLevel::L)
        .map_err(|_| Error::Config("pairing offer is too large for a QR code".into()))?;
    let rows = code.render::<unicode::Dense1x2>().quiet_zone(false).build();
    let mut text = String::from("\n");
    for row in rows.lines() {
        text.push_str("  ");
        text.push_str(row);
        text.push('\n');
    }
    Ok(text)
}
/// The part of the certificate fingerprint a person compares by eye: its
/// first 128 bits, as eight groups of four hex digits. The phone shows the
/// same form before pairing so the person can compare the two screens.
pub fn fingerprint_words(hex: &str) -> String {
    hex.chars()
        .take(32)
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>().to_uppercase())
        .collect::<Vec<_>>()
        .join(" ")
}
/// Opens pairing and waits for a device to ask, then for you to confirm it.
/// With `phone`, the link goes to that phone in place of a QR code.
pub(crate) async fn pair(paths: &Paths, phone: Option<crate::phone::LinkTo>) -> Result<()> {
    use std::io::{IsTerminal, Write};
    let mut client = OwnerClient::connect(&paths.socket()).await?;
    let previous: Vec<_> = paired_devices(&mut client)
        .await?
        .into_iter()
        .map(|d| d.device_id)
        .collect();
    let OwnerResponse::PairingOffer(offer) = client.request(&OwnerRequest::OpenPairing).await?
    else {
        return Err(Error::Closed);
    };
    let link = pairing_link(&offer)?;
    let expires =
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(offer.expires_at.0) * 1_000_000)
            .map_err(|_| Error::Config("Invalid pairing expiry time".into()))?;
    let offset = time::UtcOffset::local_offset_at(expires).ok();
    let fingerprint = fingerprint_words(&offer.cert_sha256);
    let expiry = pairing_expiry(expires, protocol::time::Timestamp::now(), offset);
    if let Some(phone) = phone {
        let name = phone.send(&link).await?;
        println!(
            "Opened {PRODUCT_NAME} on {name} with this computer's pairing link.\nCheck that it shows this fingerprint: {fingerprint}\n{expiry}"
        );
    } else {
        let code = pairing_qr(pairing_qr_link(&offer).as_deref().unwrap_or(&link))?;
        // The code goes last, nearest the cursor, where a window too short for
        // everything still shows all of it; the link is above it for pasting.
        println!("{link}\nFingerprint: {fingerprint}\n{expiry}\n{code}");
    }
    if let Some(note) = unreachable_offer(&offer.addresses, &paths.config_file(), local_address()) {
        eprintln!("\n{note}\n");
    }
    // Without a terminal nobody can answer here; `ophio pair confirm` does.
    // An agent's task never answers, even in a terminal of its own.
    let from_task = std::env::var_os(ports::TASK_ENV).is_some();
    let mut answers = (std::io::stdin().is_terminal() && !from_task).then(terminal_lines);
    let mut asking: Option<PairingPrompt> = None;
    loop {
        let remaining = offer
            .expires_at
            .0
            .saturating_sub(protocol::time::Timestamp::now().0);
        if remaining <= 0 {
            if asking.is_some() {
                eprintln!();
            }
            return Err(Error::Ended(format!(
                "This pairing code expired. Nothing was paired. Run `{COMMAND} pair` again to pair a device."
            )));
        }
        let OwnerResponse::PairingWaiting(waiting) =
            client.request(&OwnerRequest::WaitingPairing).await?
        else {
            return Err(Error::Closed);
        };
        match (waiting, asking.take()) {
            (Some(prompt), Some(shown)) if prompt == shown => asking = Some(shown),
            (Some(prompt), _) if !prompt.shows_code => {
                // The computer never pairs it; refusing lets it stop waiting.
                eprintln!();
                println!("{}", pairing_too_old(&prompt));
                let refuse = OwnerRequest::AnswerPairing {
                    code: prompt.code.clone(),
                    confirm: false,
                };
                match client.request(&refuse).await {
                    Ok(OwnerResponse::PairingAnswered(_)) => {}
                    Err(Error::Protocol(error)) if error.code == protocol::ErrorCode::NotFound => {}
                    Err(error) => return Err(error),
                    Ok(_) => return Err(Error::Closed),
                }
            }
            (Some(prompt), _) => {
                eprintln!();
                let now = protocol::time::Timestamp::now();
                match from_task {
                    true => println!(
                        "{}\n{}",
                        pairing_request(&prompt, true, now),
                        person_confirms()
                    ),
                    false => println!("{}", pairing_request(&prompt, answers.is_some(), now)),
                }
                if let Some(lines) = &mut answers {
                    while lines.try_recv().is_ok() {}
                    print!("{} [y/N] ", pairing_question(&prompt));
                    std::io::stdout().flush()?;
                }
                asking = Some(prompt);
            }
            (None, shown) => {
                // `ophio pair confirm` answered, or the device stopped asking.
                if let Some(device) = paired_devices(&mut client)
                    .await?
                    .into_iter()
                    .find(|d| !previous.contains(&d.device_id) && d.revoked_at.is_none())
                {
                    println!("\n{}", pairing_complete(&device.name));
                    return Ok(());
                }
                if let Some(shown) = shown {
                    println!("\n{}", pairing_withdrawn(&shown.device_name));
                }
                eprint!(
                    "\rWaiting for a device — {}   ",
                    seconds_remaining((remaining + 999) / 1000)
                );
                std::io::stderr().flush()?;
            }
        }
        let tick = tokio::time::sleep(Duration::from_secs(1));
        let (Some(prompt), Some(lines)) = (&asking, &mut answers) else {
            tick.await;
            continue;
        };
        tokio::select! {
            line = lines.recv() => {
                let confirm = line.is_some_and(|l| matches!(l.trim(), "y" | "Y" | "yes" | "Yes" | "YES"));
                let answer = OwnerRequest::AnswerPairing { code: prompt.code.clone(), confirm };
                match client.request(&answer).await {
                    Ok(OwnerResponse::PairingAnswered(Some(device))) => {
                        println!("{}", pairing_complete(&device.name));
                        return Ok(());
                    }
                    Ok(OwnerResponse::PairingAnswered(None)) => {
                        return Err(Error::Ended(pairing_refused(&prompt.device_name)));
                    }
                    Err(Error::Protocol(error)) if error.code == protocol::ErrorCode::NotFound => {
                        println!("{}", pairing_withdrawn(&prompt.device_name));
                        asking = None;
                    }
                    Err(error) => return Err(error),
                    Ok(_) => return Err(Error::Closed),
                }
            }
            () = tick => {}
        }
    }
}
async fn confirm_pairing(paths: &Paths, code: &str) -> Result<()> {
    let mut client = OwnerClient::connect(&paths.socket()).await?;
    let answer = OwnerRequest::AnswerPairing {
        code: code.into(),
        confirm: true,
    };
    match client.request(&answer).await {
        Ok(OwnerResponse::PairingAnswered(Some(device))) => {
            println!("{}", pairing_complete(&device.name));
            Ok(())
        }
        Err(Error::Protocol(error)) if error.code == protocol::ErrorCode::NotFound => {
            Err(Error::Ended(format!(
                "No device is asking to pair with the code {code}. Check the code that `{COMMAND} pair` shows."
            )))
        }
        Err(error) => Err(error),
        Ok(_) => Err(Error::Closed),
    }
}
pub(crate) async fn paired_devices(
    client: &mut OwnerClient,
) -> Result<Vec<protocol::auth::DeviceRecord>> {
    match query(client, Query::Devices).await? {
        QueryData::Devices { devices, .. } => Ok(devices),
        _ => Err(Error::Closed),
    }
}
/// Lines typed at the terminal. A thread of their own reads them, so waiting
/// for an answer never stops Ophio noticing that the device stopped asking.
fn terminal_lines() -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (lines, receiver) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            if line.map(|line| lines.send(line)).is_err() {
                break;
            }
        }
    });
    receiver
}
fn asking_to_pair(prompt: &PairingPrompt) -> String {
    let name = shown(&prompt.device_name);
    if prompt.device_platform.is_empty() {
        format!("{name} is asking to pair.")
    } else {
        format!(
            "{name} ({}) is asking to pair.",
            shown(&prompt.device_platform)
        )
    }
}
/// What the owner is told about a device asking to pair.
pub(crate) fn pairing_request(
    prompt: &PairingPrompt,
    terminal: bool,
    now: protocol::time::Timestamp,
) -> String {
    let mut asking = asking_to_pair(prompt);
    if let Some(earlier) = &prompt.replaces {
        asking.push_str(&format!(
            "\nIt replaces {} ({}), paired {}. Confirming removes that device and gives \
             this one its permissions: it {}.",
            shown(&earlier.name),
            shown(&earlier.platform),
            ago(earlier.paired_at, now),
            may(&earlier.grants)
        ));
    }
    let check = format!(
        "Pair it only if it shows the code {}.",
        code_words(&prompt.code)
    );
    if terminal {
        format!("{asking}\n{check}")
    } else {
        format!(
            "{asking}\n{check} To pair it, run:\n  {COMMAND} pair confirm {}",
            prompt.code
        )
    }
}
/// This computer's address on its local network: the one it would send from
/// to the internet, if that is a private one. Nothing is sent.
pub(crate) fn local_address() -> Option<std::net::IpAddr> {
    use std::net::{IpAddr, Ipv4Addr, UdpSocket};
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if ip.is_private() => Some(IpAddr::V4(ip)),
        _ => None,
    }
}
/// What to do when a pairing code offers only this computer's own address,
/// which a phone cannot reach. `local` is this computer's address on its
/// network, when it has one.
pub(crate) fn unreachable_offer(
    addresses: &[String],
    config: &std::path::Path,
    local: Option<std::net::IpAddr>,
) -> Option<String> {
    let reachable = addresses.iter().any(|address| {
        let host = address
            .rsplit_once(':')
            .map_or(address.as_str(), |(host, _)| host);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host != "localhost"
            && !host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    (!reachable).then(|| {
        let (address, which) = match local {
            Some(ip) => (ip.to_string(), ""),
            None => (
                "ADDRESS".to_owned(),
                "Replace ADDRESS with this computer's address on your network, as your router or network settings show it.\n",
            ),
        };
        format!(
            "Your phone can't reach this computer yet: this code offers only this computer's own address.\n\
             Run Tailscale on this computer and on your phone, or, on a network you trust, add this computer's local address to {}:\n  \
             allow_lan = true\n  \
             listen = [\"{address}\"]\n\
             {which}\
             Then restart Øphio and run `{COMMAND} pair` again.",
            config.display()
        )
    })
}
/// The question the owner answers at the terminal.
pub(crate) fn pairing_question(prompt: &PairingPrompt) -> String {
    match &prompt.replaces {
        Some(earlier) => format!(
            "Pair {} in place of {}?",
            shown(&prompt.device_name),
            shown(&earlier.name)
        ),
        None => format!("Pair {}?", shown(&prompt.device_name)),
    }
}
/// The code as both screens show it: two groups of three digits.
pub(crate) fn code_words(code: &str) -> String {
    match code.split_at_checked(3) {
        Some((first, rest)) => format!("{first} {rest}"),
        None => code.into(),
    }
}
/// A device whose app shows no code, which the computer refuses.
pub(crate) fn pairing_too_old(prompt: &PairingPrompt) -> String {
    format!(
        "{}\nThis device's app is too old to pair safely. Update Øphio on the device and try again.",
        asking_to_pair(prompt)
    )
}
pub(crate) fn pairing_withdrawn(name: &str) -> String {
    format!(
        "{} stopped asking. Nothing was paired. Waiting for another request.",
        shown(name)
    )
}
fn pairing_refused(name: &str) -> String {
    format!(
        "Refused {}. Nothing was paired, and this pairing code no longer works. Run `{COMMAND} pair` again to pair a device.",
        shown(name)
    )
}
pub(crate) fn pairing_expiry(
    expires: time::OffsetDateTime,
    now: protocol::time::Timestamp,
    offset: Option<time::UtcOffset>,
) -> String {
    let remaining = (expires.unix_timestamp_nanos() / 1_000_000 - i128::from(now.0)).max(0);
    if remaining == 0 {
        return "This pairing code has expired.".into();
    }
    let minutes = (remaining + 59_999) / 60_000;
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    match offset {
        Some(offset) => {
            let local = expires.to_offset(offset);
            format!(
                "Expires at {:02}:{:02}, in {minutes} {unit}.",
                local.hour(),
                local.minute()
            )
        }
        None => format!("Expires in {minutes} {unit}."),
    }
}

fn seconds_remaining(seconds: i64) -> String {
    let unit = if seconds == 1 { "second" } else { "seconds" };
    format!("{seconds} {unit} remaining")
}

fn pairing_complete(name: &str) -> String {
    format!("Paired with {}. You can continue in the app.", shown(name))
}

async fn settings(paths: &Paths, request: OwnerRequest) -> Result<OwnerResponse> {
    let unreached = match OwnerClient::connect(&paths.socket()).await {
        Ok(mut client) => return client.request(&request).await,
        Err(error) => error,
    };
    let turn_off = matches!(
        &request,
        OwnerRequest::Desktop {
            enabled: Some(false)
        }
    );
    let stopped = matches!(unreached, Error::NotRunning);
    if !stopped && !turn_off {
        return Err(unreached);
    }
    // Turning sharing off needs only the storage that holds the instance
    // lock and consent, so nothing else can keep it on. Whatever became of
    // the socket, holding that lock is what shows Ophio is stopped.
    if turn_off {
        crate::identity::private_dir(&paths.data)?;
    } else {
        paths.initialize()?;
    }
    let _lock = match paths.lock() {
        Ok(lock) => lock,
        Err(Error::Running) if !stopped => return Err(unreached),
        Err(error) => return Err(error),
    };
    let (config, saved) = if turn_off {
        (turn_desktop_off_offline(paths)?, true)
    } else {
        let mut config = Config::load(&paths.config_file())?;
        let changed = config.apply(request, paths)?;
        if changed {
            config.save(&paths.config_file())?;
        }
        (config, changed)
    };
    if saved {
        eprintln!("Configuration saved. {PRODUCT_NAME} applies it the next time it starts.");
    }
    Ok(OwnerResponse::Settings {
        config,
        applied_live: false,
        desktop: protocol::capability::Availability::Unknown {
            reason: format!("Start {COMMAND} run to check desktop availability"),
        },
    })
}
/// Turns sharing off while Ophio is stopped, with its instance lock held.
/// Consent is forgotten whatever becomes of the settings, so settings that
/// cannot be read or saved never keep sharing able to resume without asking.
/// Settings that cannot be read are left as they are, and every failure is
/// reported together.
fn turn_desktop_off_offline(paths: &Paths) -> Result<Config> {
    let mut problems = Vec::new();
    #[cfg(target_os = "linux")]
    crate::compose::forget_for_turn_off(paths, &mut problems);
    let loaded = match crate::identity::private_dir(&paths.config) {
        Ok(()) => Config::load(&paths.config_file()),
        Err(_) => Err(Error::Config(format!(
            "{} must be private to you",
            paths.config.display()
        ))),
    };
    if let Err(error) = &loaded {
        problems.push(format!(
            "the settings could not be read, so they were left as they are and it may turn on again when {PRODUCT_NAME} next starts ({error})"
        ));
    }
    let config = loaded.ok().map(|mut config| {
        config.desktop_enabled = false;
        if let Err(error) = config.save(&paths.config_file()) {
            problems.push(crate::compose::unsaved_turn_off(&error));
        }
        config
    });
    // The companion is stopped, so nothing holds the desktop's input.
    crate::compose::turned_off(problems, false)?;
    config.ok_or_else(|| Error::Config("settings could not be read".into()))
}
/// Where received files go, and older ones from before the folder was configurable.
#[derive(Debug, serde::Serialize)]
pub struct InboxStatus {
    pub folder: Option<PathBuf>,
    pub remedy: Option<&'static str>,
    pub legacy_folder: Option<PathBuf>,
}
fn inbox_status(paths: &Paths) -> Result<InboxStatus> {
    let config = Config::load(&paths.config_file())?;
    let old = paths.database().join("inbox");
    let legacy = std::fs::read_dir(&old).is_ok_and(|mut entries| entries.next().is_some());
    let folder = config.resolved_inbox_directory(paths)?;
    let remedy = folder.is_none().then_some(if cfg!(windows) {
        "Set one with `.\\ophio.exe inbox set <PATH>`"
    } else {
        "Set one with `ophio inbox set <PATH>`"
    });
    Ok(InboxStatus {
        folder,
        remedy,
        legacy_folder: legacy.then_some(old),
    })
}

/// `accounts` opens this computer's accounts, given the data folder, once
/// nothing else can be using them.
async fn uninstall_data(
    paths: &Paths,
    confirmed: bool,
    accounts: impl AsyncFnOnce(&Path) -> accounts::Config,
) -> Result<()> {
    if !confirmed {
        return Err(Error::Ended(format!(
            "Nothing was deleted. `{COMMAND} uninstall-data --yes` deletes {PRODUCT_NAME}'s settings, paired devices and data from {} and {}. Stop {PRODUCT_NAME} first. If it starts when you log in, `{COMMAND} service uninstall` stops it and turns that off.",
            shown(&paths.data.display().to_string()),
            shown(&paths.config.display().to_string())
        )));
    }
    paths.initialize()?;
    let lock = paths.lock()?;
    let inbox = Config::load(&paths.config_file())?.resolved_inbox_directory(paths)?;
    let old = paths.database().join("inbox");
    // First what Øphio holds outside its folders: sessions it can end, and
    // the keys it keeps in the keyring.
    // Account startup cleans its saved sign-ins before forget_all runs. Remove
    // a linked accounts entry first so that cleanup cannot reach outside data.
    let account_dir = paths.data.join("accounts");
    match std::fs::symlink_metadata(&account_dir) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(&account_dir)?,
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let config = accounts(&paths.data).await;
    let unopened = cfg!(target_os = "linux") && config.vault.note().is_some();
    let (still_open, emptied) = accounts::ComputerAccounts::new(config).forget_all().await;
    // Only known owned entries, never registered roots or paths from config.
    for name in [
        "identity.json",
        "accounts",
        "voice",
        "channels",
        "secrets",
        "db",
        "db-wal",
        "db-shm",
        "artifacts",
        "staging",
        "logs",
        "library",
        "desktop-consent",
        "voice-browser",
        "agent-browser",
        "setup.json",
        "setup-skills",
        "agents",
        "reporter.key",
        "report-trail.json",
        "report-crashes.json",
        "run",
    ] {
        let path = paths.data.join(name);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        if name == "db" && meta.is_dir() && old.is_dir() {
            // Old received files stay in place; uninstall must not erase them.
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                if entry.file_name() == "inbox" {
                    continue;
                }
                if entry.file_type()?.is_dir() {
                    std::fs::remove_dir_all(entry.path())?;
                } else {
                    std::fs::remove_file(entry.path())?;
                }
            }
            continue;
        }
        if meta.is_dir() {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
    }
    // A killed process can leave an atomic write before its rename. Only the
    // writers' temporary-file names inside our own folders are removed.
    for (folder, reports) in [(&paths.data, true), (&paths.config, false)] {
        for entry in std::fs::read_dir(folder)? {
            let entry = entry?;
            if owned_write_temporary(&entry.file_name(), reports) && !entry.file_type()?.is_dir() {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    match std::fs::remove_file(paths.config_file()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let remove_empty = |folder: &Path| -> std::io::Result<()> {
        match std::fs::remove_dir(folder) {
            Ok(()) => Ok(()),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    };
    // Remove config while this lock still excludes new writers. After unlink,
    // a new start may create its replacement lock and begin saving settings.
    remove_empty(&paths.config)?;
    // Then the lock and its empty data folder. A replacement lock makes that
    // folder nonempty, so removing it cannot race a new lock holder's writes.
    // Windows cannot delete a file that is open, so there the lock is let go
    // first. An Øphio that starts in between holds the file open, so removing
    // it then fails instead of taking the lock from under it. Elsewhere, a
    // start that opened the file before it went refuses the lock it then gets
    // (`Paths::lock`).
    #[cfg(windows)]
    drop(lock);
    std::fs::remove_file(paths.data.join("instance.lock"))?;
    #[cfg(not(windows))]
    drop(lock);
    remove_empty(&paths.data)?;
    // Named only when something was received; a new install has no folder.
    if let Some(inbox) = inbox.filter(|inbox| inbox.is_dir()) {
        println!(
            "Received files kept at {}.",
            shown(&inbox.display().to_string())
        );
    }
    if old.is_dir() {
        println!(
            "Legacy received files kept at {}.",
            shown(&old.display().to_string())
        );
    }
    if let Err(e) = emptied {
        println!(
            "{PRODUCT_NAME}'s keys could not all be removed from the keyring: {e}. Remove the items named \"Ophio: …\" there."
        );
    } else if unopened {
        println!(
            "{PRODUCT_NAME} couldn't open this computer's keyring, where it can keep passwords and keys, to check it. If {PRODUCT_NAME} saved keys there before, remove the items named \"Ophio: …\" with your desktop's password manager."
        );
    }
    if !still_open.is_empty() {
        println!("These may still work until ended at the service:");
        for (name, page) in &still_open {
            println!("  {name}: {page}");
        }
    }
    println!(
        "{PRODUCT_NAME}'s settings and data removed. Project files, agent logins and network configuration were preserved."
    );
    Ok(())
}

/// Temporary siblings from the report and identity/config atomic writers.
fn owned_write_temporary(name: &std::ffi::OsStr, reports: bool) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    // identity::atomic_write uses NamedTempFile's default on Unix.
    if let Some(random) = name.strip_prefix(".tmp") {
        return random.len() == 6 && random.bytes().all(|byte| byte.is_ascii_alphanumeric());
    }
    // Windows' identity writer and the report writer use UUIDs.
    let id = name.strip_prefix(".identity-").or_else(|| {
        reports
            .then(|| {
                name.strip_prefix("report-trail.")
                    .or_else(|| name.strip_prefix("report-crashes."))
            })
            .flatten()
    });
    let Some(id) = id.and_then(|rest| rest.strip_suffix(".tmp")) else {
        return false;
    };
    id.len() == 36
        && id.bytes().enumerate().all(|(i, byte)| match i {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

async fn frontdoor_settings(paths: &Paths, request: OwnerRequest) -> Result<OwnerResponse> {
    let operation = async {
        match OwnerClient::connect(&paths.socket()).await {
            Ok(mut client) => {
                client
                    .request_with_progress::<_, OwnerResponse>(&request, |p| {
                        eprintln!(
                            "{}: {} {}/{} bytes",
                            shown(&p.model),
                            shown(&p.file),
                            p.downloaded_bytes,
                            p.total_bytes
                        );
                    })
                    .await
            }
            Err(Error::NotRunning) => {
                paths.initialize()?;
                let _lock = paths.lock()?;
                let mut config = Config::load(&paths.config_file())?;
                let (progress, mut updates) = tokio::sync::mpsc::channel(16);
                let reporter = tokio::spawn(async move {
                    while let Some(OwnerResponse::VoiceProgress(p)) = updates.recv().await {
                        eprintln!(
                            "{}: {} {}/{} bytes",
                            shown(&p.model),
                            shown(&p.file),
                            p.downloaded_bytes,
                            p.total_bytes
                        );
                    }
                });
                // Aborting this future drops the install staging directory too.
                let result = crate::frontdoors::apply_files(
                    paths,
                    &request,
                    &crate::frontdoors::LocalSpeech,
                    progress,
                    tokio_util::sync::CancellationToken::new(),
                )
                .await;
                let _ = reporter.await;
                result?;
                if config.apply(request, paths)? {
                    config.save(&paths.config_file())?;
                }
                let (_, availability) = crate::frontdoors::open_speech(
                    paths,
                    &config,
                    Arc::new(crate::frontdoors::LocalSpeech),
                )
                .await?;
                let channels = crate::frontdoors::offline_channels(paths, &config).await?;
                eprintln!("{PRODUCT_NAME} applies the changes the next time it starts.");
                Ok(OwnerResponse::Frontdoors(crate::frontdoors::status(
                    &config,
                    availability,
                    channels,
                    false,
                )))
            }
            Err(e) => Err(e),
        }
    };
    tokio::select! { result = operation => result, _ = tokio::signal::ctrl_c() => Err(Error::Config("Owner operation cancelled".into())) }
}

#[cfg(test)]
mod fingerprint_tests {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    #[test]
    fn pairing_output_uses_local_clock_and_plain_confirmation() {
        use protocol::time::Timestamp;
        let expires = time::OffsetDateTime::from_unix_timestamp(19_920).unwrap();
        let offset = time::UtcOffset::from_hms(-4, 0, 0).unwrap();
        assert_eq!(
            super::pairing_expiry(expires, Timestamp(19_320_001), Some(offset)),
            "Expires at 01:32, in 10 minutes."
        );
        assert_eq!(
            super::pairing_expiry(expires, Timestamp(19_919_999), Some(offset)),
            "Expires at 01:32, in 1 minute."
        );
        assert_eq!(
            super::pairing_expiry(expires, Timestamp(19_920_000), Some(offset)),
            "This pairing code has expired."
        );
        assert_eq!(
            super::pairing_expiry(expires, Timestamp(19_320_000), None),
            "Expires in 10 minutes."
        );
        assert_eq!(
            super::pairing_complete("Tablet"),
            "Paired with Tablet. You can continue in the app."
        );
    }

    fn offer() -> protocol::auth::PairingOffer {
        protocol::auth::PairingOffer {
            host_id: "0f8b6c1e-2d3a-4b5c-8d9e-0a1b2c3d4e5f".parse().unwrap(),
            host_name: "Büro-PC".into(),
            addresses: vec![
                "192.168.1.20:47100".into(),
                "desk.example.ts.net:47100".into(),
            ],
            cert_sha256: "5f1c9a0e7d3b2a4c6e8f0a1b3c5d7e9f2a4b6c8d0e1f3a5b7c9d1e3f5a7b9c0d".into(),
            pairing_secret: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8".into(),
            expires_at: protocol::time::Timestamp(1_900_000_000_000),
            protocol: protocol::ProtocolVersion { major: 1, minor: 2 },
        }
    }

    /// Reads the packed form back the way the phone does.
    fn unpack(link: &str) -> protocol::auth::PairingOffer {
        fn text(bytes: &[u8], at: &mut usize) -> String {
            let len = usize::from(bytes[*at]);
            *at += 1 + len;
            String::from_utf8(bytes[*at - len..*at].to_vec()).unwrap()
        }
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let code = link.strip_prefix("ophio://pair?c=").unwrap();
        let mut bytes = Vec::new();
        let (mut buffer, mut bits) = (0u32, 0);
        for c in code.bytes() {
            let value = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
                .iter()
                .position(|&a| a == c)
                .unwrap();
            buffer = ((buffer << 5) | u32::try_from(value).unwrap()) & 0xfff;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                bytes.push(u8::try_from((buffer >> bits) & 0xff).unwrap());
            }
        }
        assert!(
            bits < 5 && buffer & ((1 << bits) - 1) == 0,
            "unused bits are zero"
        );
        assert_eq!(bytes[0], 1);
        let id = hex(&bytes[1..17]);
        let mut at = 93;
        let host_name = text(&bytes, &mut at);
        let count = bytes[at];
        at += 1;
        let addresses = (0..count).map(|_| text(&bytes, &mut at)).collect();
        assert_eq!(at, bytes.len(), "nothing follows the last address");
        protocol::auth::PairingOffer {
            host_id: format!(
                "{}-{}-{}-{}-{}",
                &id[..8],
                &id[8..12],
                &id[12..16],
                &id[16..20],
                &id[20..]
            )
            .parse()
            .unwrap(),
            host_name,
            addresses,
            cert_sha256: hex(&bytes[17..49]),
            pairing_secret: URL_SAFE_NO_PAD.encode(&bytes[49..81]),
            expires_at: protocol::time::Timestamp(i64::from_be_bytes(
                bytes[81..89].try_into().unwrap(),
            )),
            protocol: protocol::ProtocolVersion {
                major: u16::from_be_bytes([bytes[89], bytes[90]]),
                minor: u16::from_be_bytes([bytes[91], bytes[92]]),
            },
        }
    }

    #[test]
    fn the_qr_code_carries_the_whole_offer_in_the_packed_form() {
        let offer = offer();
        let packed = super::pairing_qr_link(&offer).unwrap();
        // client/src/protocol/pairing.test.ts reads this exact text.
        assert_eq!(
            packed,
            "ophio://pair?c=AEHYW3A6FU5EWXENTYFBWLB5JZPV6HE2BZ6TWKSMN2HQUGZ4LV7J6KSLNSGQ4HZ2LN6J2HR7LJ5ZYDIAAEBAGBAFAYDQQCIKBMGA2DQPCAIREEYUCULBOGAZDINRYHI6D4AAAAN2MDJTQAAAAEAAECCCYO6HE3ZNKBBQEERRHEZC4MJWHAXDCLRSGA5DINZRGAYBSZDFONVS4ZLYMFWXA3DFFZ2HGLTOMV2DUNBXGEYDA"
        );
        assert_eq!(unpack(&packed), offer);
        let full = super::pairing_link(&offer).unwrap();
        assert!(
            packed.len() * 2 < full.len() + 60,
            "{} vs {}",
            packed.len(),
            full.len()
        );
        // Varied offers, including the longest texts the form takes.
        let longest = "x".repeat(255);
        for (name, addresses, secret_byte, expires) in [
            ("", 1, 0u8, 0i64),
            ("工作站 😀", 3, 255, -1),
            (longest.as_str(), 255, 7, i64::MAX),
        ] {
            let offer = protocol::auth::PairingOffer {
                host_name: name.into(),
                addresses: (0..addresses)
                    .map(|i| format!("h{i}.example:{}", 47100 + i))
                    .collect(),
                pairing_secret: URL_SAFE_NO_PAD.encode([secret_byte; 32]),
                expires_at: protocol::time::Timestamp(expires),
                ..offer.clone()
            };
            assert_eq!(unpack(&super::pairing_qr_link(&offer).unwrap()), offer);
        }
    }

    #[test]
    fn an_offer_the_packed_form_cannot_hold_keeps_the_full_link_in_its_code() {
        let base = offer();
        for offer in [
            protocol::auth::PairingOffer {
                host_name: "x".repeat(256),
                ..base.clone()
            },
            protocol::auth::PairingOffer {
                addresses: vec![],
                ..base.clone()
            },
            protocol::auth::PairingOffer {
                addresses: vec!["h.example:1".into(); 256],
                ..base.clone()
            },
            protocol::auth::PairingOffer {
                addresses: vec!["h".repeat(256)],
                ..base.clone()
            },
            protocol::auth::PairingOffer {
                cert_sha256: base.cert_sha256.to_uppercase(),
                ..base.clone()
            },
            protocol::auth::PairingOffer {
                cert_sha256: "ab".repeat(31),
                ..base.clone()
            },
            protocol::auth::PairingOffer {
                pairing_secret: "c2VjcmV0".into(),
                ..base.clone()
            },
            // Padding or other spare bits would not come back the same.
            protocol::auth::PairingOffer {
                pairing_secret: format!("{}=", base.pairing_secret),
                ..base.clone()
            },
            protocol::auth::PairingOffer {
                pairing_secret: format!("{}9", &base.pairing_secret[..42]),
                ..base.clone()
            },
        ] {
            assert_eq!(super::pairing_qr_link(&offer), None, "{offer:?}");
        }
    }

    #[test]
    fn the_pairing_code_fits_a_default_terminal_window() {
        // A typical offer: one tailnet name, its address and a LAN address.
        let offer = protocol::auth::PairingOffer {
            host_name: "DESKTOP-4F7Q2KD".into(),
            addresses: vec![
                "desktop-4f7q2kd.tail1234ab.ts.net:47100".into(),
                "100.101.102.103:47100".into(),
                "192.168.1.20:47100".into(),
            ],
            ..offer()
        };
        let code = super::pairing_qr(&super::pairing_qr_link(&offer).unwrap()).unwrap();
        let rows: Vec<&str> = code.lines().collect();
        // A blank line first; then the code with two columns of margin.
        assert_eq!(rows[0], "");
        assert!(rows[1..].iter().all(|r| r.starts_with("  ")));
        let width = rows[1].chars().count();
        // Windows Terminal and the console open at 120 by 30; the code, a
        // blank line and the waiting line must fit.
        assert!(rows.len() + 2 <= 30, "{} rows", rows.len());
        assert!(width <= 80, "{width} columns");
    }

    #[test]
    fn a_code_offering_only_this_computer_says_how_a_phone_can_reach_it() {
        let config = std::path::Path::new("settings/config.toml");
        let offer = |addresses: &[&str]| {
            let addresses: Vec<String> = addresses.iter().map(|a| (*a).to_owned()).collect();
            super::unreachable_offer(&addresses, config, None)
        };
        for local in [
            &["127.0.0.1:47100"][..],
            &["[::1]:47100"],
            &["localhost:47100"],
            &["127.0.0.1:47100", "[::1]:47100"],
        ] {
            let note = offer(local).expect("a local-only code needs a note");
            assert!(note.contains("only this computer's own address"), "{note}");
            assert!(note.contains("settings/config.toml"), "{note}");
            assert!(note.contains("allow_lan = true"), "{note}");
            assert!(!note.contains("192.168.1.20"), "a made-up address: {note}");
            assert!(
                note.contains("listen = [\"ADDRESS\"]\nReplace ADDRESS"),
                "{note}"
            );
            // With the computer's own address known, the advice shows it.
            let addresses: Vec<String> = local.iter().map(|a| (*a).to_owned()).collect();
            let lan = "10.0.2.15".parse().ok();
            let note = super::unreachable_offer(&addresses, config, lan).unwrap();
            assert!(
                note.contains("listen = [\"10.0.2.15\"]\nThen restart"),
                "{note}"
            );
            assert!(!note.contains("ADDRESS"), "{note}");
        }
        for reachable in [
            &["desk.example.ts.net:47100", "100.64.0.1:47100"][..],
            &["[fd7a:115c:a1e0::1]:47100"],
            &["192.168.1.20:47100"],
        ] {
            assert_eq!(offer(reachable), None, "{reachable:?}");
        }
    }

    #[test]
    fn a_device_asking_to_pair_is_named_with_the_code_to_compare() {
        let mut prompt = services::api::PairingPrompt {
            device_name: "Pixel 9".into(),
            device_platform: "android".into(),
            code: "042917".into(),
            shows_code: true,
            replaces: None,
            expires_at: protocol::time::Timestamp(0),
        };
        let now = protocol::time::Timestamp(3 * 86_400_000);
        assert_eq!(
            super::pairing_request(&prompt, true, now),
            "Pixel 9 (android) is asking to pair.\nPair it only if it shows the code 042 917."
        );
        assert_eq!(super::pairing_question(&prompt), "Pair Pixel 9?");
        // Without a terminal, the one way to confirm is spelled out.
        assert_eq!(
            super::pairing_request(&prompt, false, now),
            format!(
                "Pixel 9 (android) is asking to pair.\nPair it only if it shows the code 042 917. \
                 To pair it, run:\n  {} pair confirm 042917",
                super::COMMAND
            )
        );
        // Pairing again in place of an earlier device says what confirming does.
        let earlier = protocol::auth::DeviceRecord {
            device_id: protocol::DeviceId::new(),
            name: "Old Pixel".into(),
            platform: "android".into(),
            paired_at: protocol::time::Timestamp(0),
            last_seen_at: None,
            revoked_at: None,
            grants: protocol::auth::Grants {
                view: true,
                admin: true,
                ..protocol::auth::Grants::default()
            },
            is_current: false,
        };
        prompt.replaces = Some(earlier);
        assert_eq!(
            super::pairing_request(&prompt, true, now),
            "Pixel 9 (android) is asking to pair.\nIt replaces Old Pixel (android), paired 3 days ago. \
             Confirming removes that device and gives this one its permissions: it may watch the \
             desktop, manage access.\nPair it only if it shows the code 042 917."
        );
        assert_eq!(
            super::pairing_question(&prompt),
            "Pair Pixel 9 in place of Old Pixel?"
        );
        prompt.replaces = None;
        prompt.shows_code = false;
        prompt.device_platform.clear();
        assert_eq!(
            super::pairing_too_old(&prompt),
            "Pixel 9 is asking to pair.\nThis device's app is too old to pair safely. \
             Update Øphio on the device and try again."
        );
        assert_eq!(
            super::pairing_refused("Pixel 9"),
            format!(
                "Refused Pixel 9. Nothing was paired, and this pairing code no longer works. \
                 Run `{} pair` again to pair a device.",
                super::COMMAND
            )
        );
        assert_eq!(
            super::pairing_withdrawn("Pixel 9"),
            "Pixel 9 stopped asking. Nothing was paired. Waiting for another request."
        );
        // A name stored before control characters were refused cannot hide or
        // forge what the owner reads.
        prompt.device_name = "Pixel\x1b[8m".into();
        prompt.device_platform = "android\n".into();
        prompt.replaces = Some(protocol::auth::DeviceRecord {
            device_id: protocol::DeviceId::new(),
            name: "Old\rPixel".into(),
            platform: "android\u{202e}".into(),
            paired_at: protocol::time::Timestamp(0),
            last_seen_at: None,
            revoked_at: None,
            grants: protocol::auth::Grants::default(),
            is_current: false,
        });
        let texts = [
            super::pairing_request(&prompt, true, now),
            super::pairing_question(&prompt),
            super::pairing_complete(&prompt.device_name),
            super::pairing_refused(&prompt.device_name),
            super::pairing_withdrawn(&prompt.device_name),
        ];
        assert!(texts[0].starts_with(
            "Pixel\\u{1b}[8m (android\\n) is asking to pair.\nIt replaces Old\\rPixel (android\\u{202e}),"
        ));
        assert_eq!(texts[1], "Pair Pixel\\u{1b}[8m in place of Old\\rPixel?");
        for text in &texts[2..] {
            assert!(text.contains("Pixel\\u{1b}[8m"), "{text}");
        }
        for text in &texts {
            assert!(!text.contains(['\x1b', '\r', '\u{202e}']), "{text}");
        }
    }

    #[test]
    fn pair_confirm_takes_the_code_with_or_without_its_space() {
        use clap::Parser;
        for args in [
            &["ophio", "pair", "confirm", "042917"][..],
            &["ophio", "pair", "confirm", "042", "917"],
        ] {
            let super::Action::Pair {
                action: Some(super::PairCommand::Confirm { code }),
            } = super::Cli::parse_from(args).command
            else {
                panic!("not a confirmation");
            };
            assert_eq!(code.concat(), "042917");
        }
    }

    #[test]
    fn fingerprint_matches_the_phone_format() {
        let fingerprint = "5f1c9a0e7d3b2a4c6e8f0a1b3c5d7e9f2a4b6c8d0e1f3a5b7c9d1e3f5a7b9c0d";
        let words = super::fingerprint_words(fingerprint);
        assert_eq!(words, "5F1C 9A0E 7D3B 2A4C 6E8F 0A1B 3C5D 7E9F");
        // The phone's own test renders the same fingerprint the same way, so
        // changing the grouping on one side fails a test on the other.
        let phone = include_str!("../../../client/src/protocol/pairing.test.ts");
        assert!(
            phone.contains(&format!(
                "fingerprintWords(\"{fingerprint}\")).toBe(\"{words}\")"
            )),
            "client/src/protocol/pairing.test.ts must expect {words}"
        );
    }

    #[test]
    fn permissions_keep_control_with_view_and_writing_with_reading() {
        use super::{Permission::*, permission_mask};
        let mut grants = protocol::auth::Grants {
            view: true,
            tasks: true,
            ..Default::default()
        };
        grants.apply_delta(&permission_mask(&[Control, LibraryWrite]), true);
        assert!(grants.control_input && grants.library_write && grants.view);
        grants.apply_delta(&permission_mask(&[View, LibraryRead]), false);
        assert!(!grants.control_input && !grants.library_write && grants.tasks);
        grants.apply_delta(&permission_mask(&[All]), true);
        assert_eq!(grants, protocol::auth::Grants::owner());
        grants.apply_delta(&permission_mask(&[Manage]), false);
        assert!(!grants.admin && grants.terminal);
    }

    #[test]
    fn device_list_reads_as_sentences_and_hides_revoked_devices() {
        use protocol::{auth::DeviceRecord, time::Timestamp};
        let record = |name: &str, seen: Option<i64>, revoked: Option<i64>| DeviceRecord {
            device_id: protocol::DeviceId::new(),
            name: name.into(),
            platform: "android".into(),
            paired_at: Timestamp(0),
            last_seen_at: seen.map(Timestamp),
            revoked_at: revoked.map(Timestamp),
            grants: protocol::auth::Grants {
                view: true,
                tasks: true,
                ..Default::default()
            },
            is_current: false,
        };
        let now = Timestamp(3 * 86_400_000);
        let text = super::describe_devices(
            &[
                record("Tablet", Some(now.0 - 5 * 60_000), None),
                record("Old phone", None, Some(1)),
            ],
            now,
        );
        assert!(text.starts_with("Tablet (android), paired 3 days ago, last seen 5 minutes ago\n  It may watch the desktop, run agent tasks.\n"));
        assert!(text.ends_with("1 revoked device not shown; `--json` lists it.\n"));
        assert!(!text.contains("Not seen for"));
        // A tablet paired minutes ago says nothing about the phone.
        let mut newer = record("Tablet", Some(now.0 - 60_000), None);
        newer.paired_at = Timestamp(now.0 - 5 * 60_000);
        let phone = record("Phone", Some(now.0 - 10 * 60_000), None);
        let text = super::describe_devices(&[phone, newer.clone()], now);
        assert!(!text.contains("Not seen for"));
        let now = Timestamp(20 * 86_400_000);
        let stale = record("Tablet", Some(60_000), None);
        let text = super::describe_devices(&[stale.clone(), newer], now);
        assert!(text.contains(&format!(
            "  Not seen for 19 days, and another device has paired since. It still has access; \
             if it is no longer used, run `{} devices revoke {}`.\n",
            super::COMMAND,
            stale.device_id
        )));
        assert_eq!(text.matches("Not seen for").count(), 1);
        assert!(!text.contains("Old phone"));
        let text =
            super::describe_devices(&[record("Phone\x1b[8m\nTablet (android)", None, None)], now);
        assert!(
            text.starts_with("Phone\\u{1b}[8m\\nTablet (android) (android), paired"),
            "{text}"
        );
        assert_eq!(
            super::may(&protocol::auth::Grants::default()),
            "may not do anything"
        );
    }

    #[test]
    fn account_list_names_state_and_what_each_needs_never_a_key() {
        use protocol::accounts::{Account, AccountKind, AccountNeed, AccountState, AccountsView};
        let account = |id: &str, service: &str, state, identity: Option<&str>, needs| Account {
            account_id: id.into(),
            service: service.into(),
            kind: AccountKind::ApiKey,
            builtin: true,
            identity: identity.map(Into::into),
            state,
            checked_at: None,
            variables: vec![],
            manage_url: None,
            needs,
            browser_sign_in: false,
            note: None,
            used_by: vec![],
        };
        let view = AccountsView {
            accounts: vec![
                account(
                    "github",
                    "GitHub",
                    AccountState::Active,
                    Some("sam-builds"),
                    None,
                ),
                account(
                    "google",
                    "Google (Gmail)",
                    AccountState::SignedOut,
                    None,
                    Some(AccountNeed::Client),
                ),
                account(
                    "openrouter",
                    "OpenRouter",
                    AccountState::NeedsAttention {
                        reason: "The key\x1b[8m was refused.".into(),
                    },
                    None,
                    None,
                ),
                account(
                    "groq",
                    "Groq",
                    AccountState::Paused,
                    None,
                    Some(AccountNeed::Key),
                ),
            ],
            sign_in: None,
            history: vec![],
            store: "Secret Service".into(),
            store_note: None,
            browser: None,
            password_manager_done: false,
        };
        let text = super::describe_accounts(&view);
        assert_eq!(
            text,
            "GitHub  (github)  active as sam-builds\n\
             Google (Gmail)  (google)  signed out. Needs its OAuth client: `ophio accounts set-client google FILE`\n\
             OpenRouter  (openrouter)  needs attention: The key\\u{1b}[8m was refused.\n\
             Groq  (groq)  paused. Set its key in the app\n\
             Keys are kept in Secret Service.\n"
        );
    }
}

#[cfg(test)]
mod help_tests {
    use clap::CommandFactory;

    #[test]
    fn every_command_and_argument_says_what_it_does() {
        fn walk(command: &clap::Command, path: &str, missing: &mut Vec<String>) {
            let path = format!("{path} {}", command.get_name()).trim().to_owned();
            if command.get_about().is_none() {
                missing.push(path.clone());
            }
            for arg in command.get_arguments() {
                if arg.get_help().is_none() {
                    missing.push(format!("{path} <{}>", arg.get_id()));
                }
            }
            for sub in command.get_subcommands() {
                walk(sub, &path, missing);
            }
        }
        let mut command = super::Cli::command();
        command.build();
        let mut missing = Vec::new();
        walk(&command, "", &mut missing);
        assert!(missing.is_empty(), "no help text: {missing:#?}");
        command.debug_assert();
    }

    #[test]
    fn help_says_what_to_type_where_the_program_is_not_on_path() {
        fn walk(command: &mut clap::Command, path: &str, said: &mut Vec<String>) {
            let path = format!("{path} {}", command.get_name());
            let help = command.render_long_help().to_string();
            for line in help.lines() {
                if line.contains("`ophio ") || line.starts_with("  ophio ") {
                    said.push(format!("{path}: {line}"));
                }
            }
            for sub in command.get_subcommands_mut() {
                walk(sub, &path, said);
            }
        }
        let mut here = super::retype(super::Cli::command(), "ophio");
        let mut said = Vec::new();
        walk(&mut here, "", &mut said);
        assert!(said.len() > 20, "the examples moved: {said:#?}");
        let mut windows = super::retype(super::Cli::command(), ".\\ophio.exe");
        let help = windows.render_long_help().to_string();
        assert!(help.contains("\n  .\\ophio.exe service install   Start "));
        let mut said = Vec::new();
        walk(&mut windows, "", &mut said);
        assert!(said.is_empty(), "still says ophio: {said:#?}");
    }

    #[test]
    fn pause_all_help_does_not_promise_to_stop_tasks() {
        let mut command = super::Cli::command();
        for parent in ["accounts", "agent-logins"] {
            let parent = command.find_subcommand_mut(parent).unwrap();
            let examples = parent.render_long_help().to_string();
            let example = (examples.lines())
                .find(|line| line.starts_with("  ophio ") && line.contains(" pause-all "))
                .unwrap();
            assert!(example.contains("Pause every"), "{example}");
            assert!(!example.contains("Stop"), "{example}");
            let pause_all = parent.find_subcommand_mut("pause-all").unwrap();
            let help = pause_all.render_long_help().to_string();
            assert!(help.contains("aren't stopped"), "{help}");
            assert!(help.contains("`ophio stop-task`"), "{help}");
        }
    }
}

#[cfg(test)]
mod task_guard_tests {
    use clap::Parser;

    fn refused(args: &[&str]) -> Option<String> {
        let cli = super::Cli::try_parse_from(std::iter::once("ophio").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{args:?}: {e}"));
        super::refused_in_task(&cli.command)
    }

    #[test]
    fn a_task_cannot_pair_change_grants_or_run_setup() {
        for args in [
            &["setup"][..],
            &["pair"],
            &["pair", "confirm", "123", "456"],
            &["devices", "allow", "phone", "control"],
            &["devices", "deny", "phone", "control"],
            &["devices", "revoke", "phone"],
            &["phone", "pair"],
            &["phone", "pair", "--code"],
            &["rotate-certificate"],
            &["uninstall-data", "--yes"],
            &["agent-logins", "enable"],
        ] {
            let refusal = refused(args).unwrap_or_else(|| panic!("allowed: {args:?}"));
            assert!(refusal.starts_with("An agent's task can't "), "{refusal}");
            assert!(
                refusal.contains("only the person at this computer"),
                "{refusal}"
            );
        }
    }

    #[test]
    fn a_task_can_do_what_setup_skills_ask_of_it() {
        for args in [
            &["status"][..],
            &["doctor"],
            &["devices"],
            &["phone", "find"],
            &["phone", "install"],
            &["phone", "link"],
            &["projects", "list"],
            &["channels", "status"],
            &["accounts"],
            &["token", "github"],
            &["agent-logins"],
            &["agent-logins", "pause-all"],
            &["checklist", "show", "."],
            &["checklist", "put", ".", "-"],
            &["checklist", "remove", ".", "make-key"],
        ] {
            assert_eq!(refused(args), None, "{args:?}");
        }
    }
}

#[cfg(test)]
mod uninstall_tests {
    use super::*;
    use accounts::vault::Vault as _;

    #[tokio::test]
    async fn uninstall_data_leaves_nothing_behind() {
        // Setup finished, and setup left part way: either way the next
        // install is set up as a new one.
        for progress in [
            r#"{"finished":"done","steps":{"background":"done","phone":"done"}}"#,
            r#"{"steps":{"background":"done","phone":"done"}}"#,
        ] {
            uninstall_data_leaves_nothing_behind_after(progress).await;
        }
    }

    #[tokio::test]
    async fn uninstall_data_without_yes_deletes_nothing_and_says_so() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        std::fs::write(paths.data.join("report-trail.json"), "[]").unwrap();
        std::fs::write(paths.config_file(), "# keep my settings").unwrap();
        let error = uninstall_data(&paths, false, async |_: &Path| -> accounts::Config {
            unreachable!("a preview opens no accounts")
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("Nothing was deleted."), "{error}");
        assert!(error.contains("uninstall-data --yes"), "{error}");
        assert!(error.contains("service uninstall"), "{error}");
        assert!(error.contains("when you log in"), "{error}");
        assert!(!error.contains("configuration error"), "{error}");
        assert_eq!(
            std::fs::read(paths.data.join("report-trail.json")).unwrap(),
            b"[]"
        );
        assert_eq!(
            std::fs::read(paths.config_file()).unwrap(),
            b"# keep my settings"
        );
    }

    async fn uninstall_with_empty_accounts(paths: &Paths) {
        uninstall_data(paths, true, async |dir: &Path| accounts::Config {
            dir: dir.join("accounts"),
            vault: Arc::new(accounts::vault::MemoryVault::default()),
            opener: Arc::new(accounts::desktop::Recorder::default()),
            tools: accounts::tool::Tools {
                env: vec![("PATH".into(), dir.join("missing-tools").into())],
            },
            http: accounts::http(),
            sign_in_limit: Duration::from_secs(1),
        })
        .await
        .unwrap();
    }

    fn test_paths(root: &Path) -> Paths {
        let paths = Paths {
            config: root.join("config"),
            data: root.join("data"),
            runtime: root.join("run"),
        };
        paths.initialize().unwrap();
        Config::default().save(&paths.config_file()).unwrap();
        paths
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn uninstall_does_not_follow_database_symlink_with_legacy_inbox() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let external = temp.path().join("external");
        std::fs::create_dir_all(external.join("inbox")).unwrap();
        std::fs::create_dir_all(external.join("project")).unwrap();
        std::fs::write(external.join("project/keep.txt"), "keep").unwrap();
        std::fs::write(external.join("keep.txt"), "keep").unwrap();
        std::os::unix::fs::symlink(&external, paths.database()).unwrap();

        uninstall_with_empty_accounts(&paths).await;

        assert!(std::fs::symlink_metadata(paths.database()).is_err());
        assert_eq!(std::fs::read(external.join("keep.txt")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(external.join("project/keep.txt")).unwrap(),
            b"keep"
        );
        assert!(external.join("inbox").is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_start_that_opened_the_lock_before_uninstall_does_not_hold_it_after() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let lock = paths.data.join("instance.lock");
        let opened = crate::identity::private_open(&lock).unwrap();

        uninstall_with_empty_accounts(&paths).await;
        assert!(!paths.data.exists());

        // A later start makes a new lock file; the early one's is not it.
        paths.initialize().unwrap();
        let _successor = paths.lock().unwrap();
        assert!(matches!(
            crate::config::lock_if_current(&lock, opened),
            Ok(None)
        ));
        let again = crate::identity::private_open(&lock).unwrap();
        assert!(matches!(
            crate::config::lock_if_current(&lock, again),
            Err(Error::Running)
        ));
    }

    #[tokio::test]
    async fn uninstall_removes_only_owned_report_temporaries() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let id = "019a1de0-0000-7000-8000-000000000001";
        let owned = [
            format!("report-trail.{id}.tmp"),
            format!("report-crashes.{id}.tmp"),
        ];
        let kept = [
            "report-trail.notes.tmp".to_owned(),
            format!("unrelated.{id}.tmp"),
            format!("report-crashes.{id}.tmp.backup"),
        ];
        for name in owned.iter().chain(&kept) {
            std::fs::write(paths.data.join(name), "synthetic").unwrap();
        }
        let directory = paths
            .data
            .join("report-trail.019a1de0-0000-7000-8000-000000000002.tmp");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("keep.txt"), "keep").unwrap();

        uninstall_with_empty_accounts(&paths).await;

        for name in owned {
            assert!(!paths.data.join(name).exists());
        }
        for name in kept {
            assert!(paths.data.join(name).exists());
        }
        assert!(directory.join("keep.txt").exists());
    }

    #[tokio::test]
    async fn uninstall_removes_runtime_when_stored_under_data() {
        let temp = tempfile::tempdir().unwrap();
        let mut paths = test_paths(temp.path());
        paths.runtime = paths.data.join("run");
        paths.initialize().unwrap();
        std::fs::write(paths.runtime.join("owner.sock"), "stale socket").unwrap();

        uninstall_with_empty_accounts(&paths).await;

        assert!(!paths.runtime.exists());
    }

    #[tokio::test]
    async fn uninstall_removes_interrupted_identity_and_config_writes() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        for parent in [&paths.data, &paths.config] {
            // Keeping a NamedTempFile simulates a process killed before persist.
            let file = tempfile::NamedTempFile::new_in(parent).unwrap();
            std::fs::write(file.path(), "private settings").unwrap();
            let (file, _) = file.keep().unwrap();
            drop(file);
            std::fs::write(
                parent.join(".identity-019a1de0-0000-7000-8000-000000000001.tmp"),
                "private settings",
            )
            .unwrap();
        }

        uninstall_with_empty_accounts(&paths).await;

        assert!(!paths.config.exists());
        assert!(!paths.data.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn uninstall_does_not_follow_accounts_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let external = temp.path().join("external");
        std::fs::create_dir_all(external.join("runs")).unwrap();
        std::fs::write(external.join("runs/keep.txt"), "keep").unwrap();
        std::os::unix::fs::symlink(&external, paths.data.join("accounts")).unwrap();

        uninstall_with_empty_accounts(&paths).await;

        assert_eq!(
            std::fs::read(external.join("runs/keep.txt")).unwrap(),
            b"keep"
        );
        assert!(std::fs::symlink_metadata(paths.data.join("accounts")).is_err());
    }

    async fn uninstall_data_leaves_nothing_behind_after(progress: &str) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        Config::default().save(&paths.config_file()).unwrap();
        // Everything a running companion keeps under its data directory,
        // including deleted notes, edit snapshots, desktop consent and the
        // separate voice browser's profiles.
        for file in [
            "identity.json",
            "secrets/telegram.token",
            "db/companion.sqlite",
            "artifacts/ab/synthetic",
            "staging/synthetic",
            "logs/service.synthetic",
            "voice/tiny.en/config.json",
            "channels/state",
            "library/trash/synthetic/content",
            "library/recovery/synthetic/base",
            "desktop-consent/desktop-restore-token",
            "voice-browser/synthetic/Default/Cookies",
            "agent-browser/profile/Default/Cookies",
            "accounts/config/github/hosts.yml",
            "accounts/vault/openrouter.key",
            "setup-skills/ophio-setup/SKILL.md",
            "agents/claude/package.json",
            "reporter.key",
            "report-trail.json",
            "report-crashes.json",
        ] {
            let path = paths.data.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "synthetic").unwrap();
        }
        std::fs::write(paths.data.join("setup.json"), progress).unwrap();
        assert!(!matches!(
            crate::setup::start(&paths, &[]),
            crate::setup::Start::First(_)
        ));
        // A stand-in for the keyring; no tool is found to run.
        let vault = Arc::new(accounts::vault::MemoryVault::default());
        vault
            .set("google", accounts::vault::Item::Refresh, "synthetic")
            .await
            .unwrap();
        let empty = temp.path().join("bin");
        std::fs::create_dir(&empty).unwrap();
        let kept = vault.clone();
        uninstall_data(&paths, true, async move |dir: &Path| accounts::Config {
            dir: dir.join("accounts"),
            vault: kept,
            opener: Arc::new(accounts::desktop::Recorder::default()),
            tools: accounts::tool::Tools {
                env: vec![("PATH".into(), empty.into())],
            },
            http: accounts::http(),
            sign_in_limit: Duration::from_secs(1),
        })
        .await
        .unwrap();
        assert!(vault.is_empty());
        assert!(!paths.data.exists());
        assert!(!paths.config.exists());
        assert!(matches!(
            crate::setup::start(&paths, &[]),
            crate::setup::Start::First(_)
        ));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod desktop_tests {
    use super::*;
    use platform_linux::portal::{FileTokenStore, TokenStore};

    #[tokio::test]
    async fn offline_disable_forgets_only_the_consent_token() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        let consent = paths.data.join("desktop-consent");
        std::fs::create_dir(&consent).unwrap();
        let tokens = FileTokenStore::new(consent.clone());
        tokens.save("synthetic-restore-token").await.unwrap();
        std::fs::write(consent.join("unrelated"), "keep").unwrap();
        settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(true),
            },
        )
        .await
        .unwrap();
        assert!(tokens.load().await.unwrap().is_some());
        for _ in 0..2 {
            settings(
                &paths,
                OwnerRequest::Desktop {
                    enabled: Some(false),
                },
            )
            .await
            .unwrap();
            assert!(tokens.load().await.unwrap().is_none());
        }
        settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(true),
            },
        )
        .await
        .unwrap();
        assert!(tokens.load().await.unwrap().is_none());
        assert_eq!(
            std::fs::read_to_string(consent.join("unrelated")).unwrap(),
            "keep"
        );

        // An unexpected directory in the token's place is an error, never a
        // reason to recursively delete data or report that consent was forgotten.
        std::fs::create_dir(consent.join("desktop-restore-token")).unwrap();
        assert!(
            settings(
                &paths,
                OwnerRequest::Desktop {
                    enabled: Some(false)
                }
            )
            .await
            .is_err()
        );
        assert!(consent.join("desktop-restore-token").is_dir());
    }

    #[tokio::test]
    async fn offline_disable_does_not_wait_on_a_library_folder_that_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        let folder = temp.path().join("notes");
        std::fs::create_dir(&folder).unwrap();
        for request in [
            OwnerRequest::RootsAdd {
                path: folder.clone(),
                label: None,
                writable: false,
            },
            OwnerRequest::Desktop {
                enabled: Some(true),
            },
        ] {
            settings(&paths, request).await.unwrap();
        }
        let consent = paths.data.join("desktop-consent");
        std::fs::create_dir(&consent).unwrap();
        let tokens = FileTokenStore::new(consent.clone());
        tokens.save("synthetic-restore-token").await.unwrap();
        std::fs::remove_dir(&folder).unwrap();
        settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(false),
            },
        )
        .await
        .expect("a removed Library folder kept sharing on");
        assert!(tokens.load().await.unwrap().is_none());
        let saved = Config::load(&paths.config_file()).unwrap();
        assert!(!saved.desktop_enabled);
        assert_eq!(saved.roots.len(), 1, "the Library folder was dropped");
    }

    #[tokio::test]
    async fn offline_disable_forgets_consent_even_when_the_setting_cannot_be_saved() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(true),
            },
        )
        .await
        .unwrap();
        let consent = paths.data.join("desktop-consent");
        std::fs::create_dir(&consent).unwrap();
        let tokens = FileTokenStore::new(consent.clone());
        tokens.save("synthetic-restore-token").await.unwrap();
        let mode = std::fs::metadata(&paths.config).unwrap().permissions();
        std::fs::set_permissions(&paths.config, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(false),
            },
        )
        .await;
        std::fs::set_permissions(&paths.config, mode).unwrap();
        let error = result.expect_err("an unsaved change reported success");
        assert!(
            tokens.load().await.unwrap().is_none(),
            "consent outlived the turn-off: {error}"
        );
        assert!(
            error.to_string().contains("could not be saved"),
            "the person is not told the setting is unsaved: {error}"
        );
        // Turning it off again once settings can be saved records it off.
        settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(false),
            },
        )
        .await
        .unwrap();
        assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
    }

    #[tokio::test]
    async fn offline_disable_forgets_consent_even_when_the_settings_cannot_be_read() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(true),
            },
        )
        .await
        .unwrap();
        let consent = paths.data.join("desktop-consent");
        std::fs::create_dir(&consent).unwrap();
        let tokens = FileTokenStore::new(consent.clone());
        tokens.save("synthetic-restore-token").await.unwrap();
        // A setting unrelated to sharing is edited by hand into one Ophio
        // does not accept, in otherwise well-formed settings.
        let file = paths.config_file();
        let good = std::fs::read_to_string(&file).unwrap();
        let mut table: toml::Table = good.parse().unwrap();
        table.insert("voice_model".into(), "no-such-model".into());
        let bad = table.to_string();
        std::fs::write(&file, &bad).unwrap();
        assert!(Config::load(&file).is_err());
        let error = settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(false),
            },
        )
        .await
        .expect_err("unreadable settings reported the turn-off as saved");
        assert!(
            tokens.load().await.unwrap().is_none(),
            "consent outlived the turn-off: {error}"
        );
        assert!(
            error.to_string().contains("could not be read"),
            "the person is not told the settings are unreadable: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            bad,
            "unreadable settings were overwritten"
        );
        // Once the settings can be read again, turning it off records it off.
        std::fs::write(&file, &good).unwrap();
        settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(false),
            },
        )
        .await
        .unwrap();
        assert!(!Config::load(&file).unwrap().desktop_enabled);
    }

    #[tokio::test]
    async fn offline_disable_forgets_consent_even_when_the_settings_folder_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(true),
            },
        )
        .await
        .unwrap();
        let consent = paths.data.join("desktop-consent");
        std::fs::create_dir(&consent).unwrap();
        let tokens = FileTokenStore::new(consent.clone());
        tokens.save("synthetic-restore-token").await.unwrap();
        // The settings folder, and another Ophio does not need to turn
        // sharing off, are opened up to other people.
        for dir in [&paths.config, &paths.runtime] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let error = settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(false),
            },
        )
        .await
        .expect_err("a settings folder open to others reported the turn-off as saved");
        assert!(
            tokens.load().await.unwrap().is_none(),
            "consent outlived the turn-off: {error}"
        );
        assert!(
            error.to_string().contains("settings"),
            "the person is not told about the settings: {error}"
        );
        std::fs::set_permissions(&paths.config, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            Config::load(&paths.config_file()).unwrap().desktop_enabled,
            "settings in a folder open to others were saved"
        );
    }

    #[tokio::test]
    async fn offline_disable_forgets_consent_when_the_runtime_folder_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(true),
            },
        )
        .await
        .unwrap();
        let consent = paths.data.join("desktop-consent");
        std::fs::create_dir(&consent).unwrap();
        let tokens = FileTokenStore::new(consent.clone());
        tokens.save("synthetic-restore-token").await.unwrap();
        // With Ophio stopped, the folder its socket lives in cannot be looked into.
        std::fs::set_permissions(&paths.runtime, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(false),
            },
        )
        .await;
        std::fs::set_permissions(&paths.runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            tokens.load().await.unwrap().is_none(),
            "consent outlived the turn-off: {:?}",
            result.err()
        );
        assert!(!Config::load(&paths.config_file()).unwrap().desktop_enabled);
    }

    #[tokio::test]
    async fn disable_leaves_a_running_ophio_alone_when_its_socket_cannot_be_reached() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(true),
            },
        )
        .await
        .unwrap();
        let consent = paths.data.join("desktop-consent");
        std::fs::create_dir(&consent).unwrap();
        let tokens = FileTokenStore::new(consent.clone());
        tokens.save("synthetic-restore-token").await.unwrap();
        // Another Ophio holds the instance lock, so it owns its consent.
        let running = paths.lock().unwrap();
        std::fs::set_permissions(&paths.runtime, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = settings(
            &paths,
            OwnerRequest::Desktop {
                enabled: Some(false),
            },
        )
        .await;
        std::fs::set_permissions(&paths.runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(running);
        let error = result.expect_err("a running Øphio's settings were changed from outside");
        assert!(
            matches!(error, Error::Io(ref e) if e.kind() == std::io::ErrorKind::PermissionDenied),
            "the socket's own failure is not what is reported: {error}"
        );
        assert!(tokens.load().await.unwrap().is_some());
        assert!(Config::load(&paths.config_file()).unwrap().desktop_enabled);
    }

    #[test]
    fn consent_cleanup_accepts_absence_and_rejects_a_redirected_directory() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: temp.path().join("config"),
            data: temp.path().join("data"),
            runtime: temp.path().join("run"),
        };
        paths.initialize().unwrap();
        crate::compose::forget_desktop_consent(&paths).unwrap();
        let other = temp.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("desktop-restore-token"), "synthetic").unwrap();
        std::os::unix::fs::symlink(&other, paths.data.join("desktop-consent")).unwrap();
        assert!(crate::compose::forget_desktop_consent(&paths).is_err());
        assert!(other.join("desktop-restore-token").is_file());
    }
}

fn parse_root_kind(value: &str) -> std::result::Result<protocol::library::RootKind, String> {
    serde_json::from_value(serde_json::Value::String(value.into()))
        .map_err(|_| "Choose notes, drafts, skills, agents, instructions, or project".into())
}
fn parse_root_harness(
    value: &str,
) -> std::result::Result<protocol::capability::HarnessKind, String> {
    serde_json::from_value(serde_json::Value::String(value.into()))
        .map_err(|_| "Unknown agent kind".into())
}
