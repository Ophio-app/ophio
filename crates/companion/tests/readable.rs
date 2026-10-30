// SPDX-License-Identifier: Apache-2.0
//! What owner commands print for people.
mod support;

use channels::{ChannelState, ChannelStatus};
use companion::{
    cli::InboxStatus,
    config::LibraryRootConfig,
    frontdoors::{FrontdoorStatus, VoiceStatus},
    readable::{self, Style},
};
use ports::HarnessAdapter;
use protocol::{
    auth::ChannelKind,
    capability::{AccountReadiness, Availability, HarnessInfo, HarnessKind},
    ids::{BootEpoch, HostId, LibraryRootId, ProjectId},
    library::RootKind,
    model::{
        CheckStatus, HostInfo, Project, ReadinessArea, ReadinessCheck, ReadinessReport, Snapshot,
    },
    time::Timestamp,
};
use services::testing::FakeHarness;

const ACCOUNT: &str = "owner@example.com";

struct Computer {
    snapshot: Snapshot,
    harnesses: Vec<HarnessInfo>,
    readiness: ReadinessReport,
    frontdoors: FrontdoorStatus,
    inbox: InboxStatus,
}
impl Computer {
    async fn well() -> Self {
        let mut harness = FakeHarness::default().probe().await;
        harness.display_name = "Claude Code".into();
        harness.account = AccountReadiness::SignedIn {
            account_label: Some(ACCOUNT.into()),
            plan: Some("Max".into()),
        };
        let passed = |area, label: &str| check(area, label, CheckStatus::Passed, None, None);
        Self {
            snapshot: Snapshot {
                journal_head: 0,
                host: HostInfo {
                    inbox_path: Some("/data/Received".into()),
                    host_id: HostId::new(),
                    name: "studio".into(),
                    os_label: "Linux".into(),
                    companion_version: "0.1.0".into(),
                    boot_epoch: BootEpoch::new(),
                    started_at: Timestamp(1_700_000_000_000),
                    capabilities: support::capabilities(),
                },
                projects: Vec::new(),
                tasks: Vec::new(),
                runs: Vec::new(),
                approvals: Vec::new(),
                attention: Vec::new(),
                targets: Vec::new(),
                control: Vec::new(),
                library_roots: Vec::new(),
                older_tasks: None,
                withheld_approvals: Vec::new(),
            },
            harnesses: vec![harness],
            readiness: ReadinessReport {
                checked_at: Timestamp(1_700_000_000_000),
                checks: vec![
                    passed(ReadinessArea::Terminal, "Terminal"),
                    passed(ReadinessArea::Graphics, "Capture"),
                    passed(ReadinessArea::Graphics, "Input"),
                    passed(ReadinessArea::Graphics, "Lock session"),
                    passed(ReadinessArea::Graphics, "Computer use"),
                    passed(ReadinessArea::Speech, "Voice"),
                    passed(ReadinessArea::Storage, "Library"),
                    passed(ReadinessArea::Storage, "Transfers"),
                    passed(ReadinessArea::Channels, "Telegram"),
                    passed(ReadinessArea::Channels, "Discord"),
                    check(
                        ReadinessArea::Network,
                        "Private network reachability",
                        CheckStatus::Unknown,
                        Some(
                            "Reachability depends on the device and address. Check the connection on the device.",
                        ),
                        None,
                    ),
                    passed(ReadinessArea::Accounts, "Claude Code installation"),
                    passed(ReadinessArea::Accounts, "Claude Code start run"),
                    passed(ReadinessArea::Accounts, "Claude Code Work"),
                    passed(ReadinessArea::Accounts, "Claude Code sign-in"),
                ],
            },
            frontdoors: FrontdoorStatus {
                voice: VoiceStatus {
                    model: "tiny.en".into(),
                    availability: Availability::Available,
                    remedy: None,
                },
                channels: vec![
                    channel(ChannelKind::Telegram, ChannelState::Connected, None),
                    channel(ChannelKind::Discord, ChannelState::Connected, None),
                ],
                applied_live: true,
                note: String::new(),
            },
            inbox: InboxStatus {
                folder: Some("/data/Received".into()),
                remedy: None,
                legacy_folder: None,
            },
        }
    }
    fn check_mut(&mut self, label: &str) -> &mut ReadinessCheck {
        self.readiness
            .checks
            .iter_mut()
            .find(|c| c.label == label)
            .unwrap()
    }
    fn status(&self) -> String {
        readable::status(
            &self.snapshot,
            &self.harnesses,
            &self.frontdoors,
            &self.inbox,
            Style::PLAIN,
        )
    }
    fn doctor(&self) -> String {
        readable::doctor(&self.readiness, &self.frontdoors, &self.inbox, Style::PLAIN)
    }
}
fn check(
    area: ReadinessArea,
    label: &str,
    status: CheckStatus,
    detail: Option<&str>,
    remedy: Option<&str>,
) -> ReadinessCheck {
    ReadinessCheck {
        area,
        label: label.into(),
        status,
        detail: detail.map(Into::into),
        remedy: remedy.map(Into::into),
        summary: None,
    }
}
fn channel(kind: ChannelKind, state: ChannelState, remedy: Option<&str>) -> ChannelStatus {
    ChannelStatus {
        channel: kind,
        state,
        remedy: remedy.map(Into::into),
    }
}

/// `text` with the command typed as it is on this computer.
fn typed(text: &str) -> String {
    text.replace("ophio ", &format!("{} ", ports::COMMAND))
}

#[tokio::test]
async fn all_well() {
    let computer = Computer::well().await;
    assert_eq!(
        computer.status(),
        typed(
            "\
Øphio 0.1.0 is running on studio (Linux).

Desktop view     ready
Desktop control  ready
Terminal         ready
Voice            ready, using tiny.en
Claude Code      signed in
Telegram         connected
Discord          connected
Projects         none yet
                 Run `ophio projects add <PATH>`.
Library folders  none shared
Received files   /data/Received

Run `ophio doctor` for every check and what to do about it.
"
        )
    );
    assert_eq!(
        computer.doctor(),
        typed(
            "\
Computer
  Terminal                      ready
  Capture                       ready
  Input                         ready
  Lock session                  ready
  Computer use                  ready
  Voice                         ready, using tiny.en
  Library                       ready
  Transfers                     ready
  Private network reachability  not known
                                Reachability depends on the device and address. Check the connection on the device.
  Received files                /data/Received

Agents
  Claude Code installation  ready
  Claude Code sign-in       ready

Chat connectors
  Telegram  connected
  Discord   connected

Run `ophio doctor --json` for every check, including those not shown.
Nothing needs attention.
"
        )
    );
    assert!(!computer.status().contains(ACCOUNT) && !computer.doctor().contains(ACCOUNT));
}

#[tokio::test]
async fn signed_out_agent() {
    let mut computer = Computer::well().await;
    let remedy = "Run claude auth login on the computer";
    computer.harnesses[0].account = AccountReadiness::SignedOut {
        remedy: remedy.into(),
    };
    *computer.check_mut("Claude Code sign-in") = check(
        ReadinessArea::Accounts,
        "Claude Code sign-in",
        CheckStatus::Failed,
        None,
        Some(remedy),
    );
    assert!(computer.status().contains(
        "\
Claude Code      signed out
                 Run claude auth login on the computer
Telegram         connected
"
    ));
    assert!(computer.doctor().ends_with(&typed(
        "\
Agents
  Claude Code installation  ready
  Claude Code sign-in       not ready
                            Run claude auth login on the computer

Chat connectors
  Telegram  connected
  Discord   connected

Run `ophio doctor --json` for every check, including those not shown.
1 thing needs attention: Claude Code sign-in.
"
    )));
}

#[tokio::test]
async fn disabled_channel() {
    let mut computer = Computer::well().await;
    computer.frontdoors.channels[1] = channel(
        ChannelKind::Discord,
        ChannelState::Disabled,
        Some(&typed("ophio channels enable discord")),
    );
    let off = typed("Discord   off\n          Run `ophio channels enable discord`.\n");
    assert_eq!(
        readable::channels(&computer.frontdoors, Style::PLAIN),
        format!("Telegram  connected\n{off}")
    );
    assert!(computer.status().contains(&typed(
        "\
Discord          off
                 Run `ophio channels enable discord`.
Projects         none yet
"
    )));
    let doctor = computer.doctor();
    assert!(doctor.contains(&typed(
        "\
Chat connectors
  Telegram  connected
  Discord   off
            Run `ophio channels enable discord`.
"
    )));
    assert!(doctor.ends_with("Nothing needs attention.\n"), "{doctor}");
}

#[tokio::test]
async fn agent_limits_needing_setup_are_named_in_the_summary() {
    let mut computer = Computer::well().await;
    let remedy = "Install the bwrap AppArmor profile";
    computer.readiness.checks.push(check(
        ReadinessArea::Accounts,
        "Agent limits",
        CheckStatus::NotRun,
        Some("Needs setup."),
        Some(remedy),
    ));
    let doctor = computer.doctor();
    assert!(
        doctor.contains("  Agent limits              needs setup\n"),
        "{doctor}"
    );
    assert!(
        doctor.ends_with("Nothing is broken.\nStill to set up: Agent limits.\n"),
        "{doctor}"
    );
    *computer.check_mut("Claude Code sign-in") = check(
        ReadinessArea::Accounts,
        "Claude Code sign-in",
        CheckStatus::Failed,
        None,
        Some("Run claude auth login on the computer"),
    );
    let doctor = computer.doctor();
    assert!(
        doctor.ends_with(
            "1 thing needs attention: Claude Code sign-in.\nStill to set up: Agent limits.\n"
        ),
        "{doctor}"
    );
}

#[tokio::test]
async fn locked_desktop() {
    let mut computer = Computer::well().await;
    let locked = "Computer is locked";
    computer.snapshot.host.capabilities.input = Availability::TemporarilyUnavailable {
        reason: locked.into(),
    };
    *computer.check_mut("Input") = check(
        ReadinessArea::Graphics,
        "Input",
        CheckStatus::Failed,
        Some(locked),
        None,
    );
    assert!(computer.status().contains(
        "\
Desktop view     ready
Desktop control  not ready
                 Computer is locked
Terminal         ready
"
    ));
    let doctor = computer.doctor();
    assert!(doctor.starts_with(
        "\
Computer
  Terminal                      ready
  Capture                       ready
  Input                         not ready
                                Computer is locked
  Lock session                  ready
"
    ));
    assert!(
        doctor.ends_with("1 thing needs attention: Input.\n"),
        "{doctor}"
    );
}

#[test]
fn settings_commands() {
    let root = LibraryRootConfig {
        root_id: LibraryRootId::new(),
        label: "Web docs".into(),
        path: "/code/web/docs".into(),
        writable: true,
        kind: RootKind::Project,
        project_id: Some(ProjectId::new()),
        harness: Some(HarnessKind::ClaudeCode),
    };
    assert_eq!(
        readable::roots(std::slice::from_ref(&root)),
        format!(
            "Web docs: /code/web/docs\n  Writable, kind project, project {}, agent claude_code.\n  ID {}\n",
            root.project_id.unwrap(),
            root.root_id
        )
    );
    assert_eq!(
        readable::roots(&[]),
        typed("No folders are shared. Run `ophio roots add <PATH>` to share one.\n")
    );
    let unset = InboxStatus {
        folder: None,
        remedy: Some("Set one with `ophio inbox set <PATH>`"),
        legacy_folder: None,
    };
    assert_eq!(
        readable::inbox(&unset, Style::PLAIN),
        "Received files  no folder\n                Set one with `ophio inbox set <PATH>`\n"
    );
    let no_session = "No supported graphical session. Run ophio desktop enable from the computer's graphical login session after fixing it";
    assert_eq!(
        readable::desktop(
            true,
            &Availability::PermissionMissing {
                remedy: no_session.into()
            },
            Style::PLAIN
        ),
        format!("Desktop access  on\nDesktop view    needs setup\n                {no_session}\n")
    );
    assert_eq!(
        readable::desktop(false, &Availability::Available, Style::PLAIN),
        "Desktop access  off\n"
    );
    assert_eq!(readable::retention(1), "Event history is kept for 1 day.\n");
    assert_eq!(
        readable::voice(
            &VoiceStatus {
                model: "base.en".into(),
                availability: Availability::PermissionMissing {
                    remedy: typed("ophio voice install base.en")
                },
                remedy: Some(typed("ophio voice install base.en")),
            },
            Style::PLAIN
        ),
        typed("Voice  needs setup\n       Run `ophio voice install base.en`.\n")
    );
}

#[test]
fn projects() {
    let project = Project {
        project_id: ProjectId::new(),
        name: "Website".into(),
        host_path: "/code/website".into(),
        default_harness: None,
        default_model: None,
        default_effort: None,
        active_writer: None,
        vcs: None,
        created_at: Timestamp(1_700_000_000_000),
        stage: None,
        one_off: false,
        home: false,
    };
    let id = project.project_id;
    assert_eq!(
        readable::projects(std::slice::from_ref(&project)),
        format!("Website: /code/website\n  ID {id}\n")
    );
    assert_eq!(
        readable::project_added(&project),
        format!("Added Website at /code/website.\nID {id}\n")
    );
    assert_eq!(
        readable::projects(&[]),
        typed("No projects yet. Run `ophio projects add <PATH>` to add one.\n")
    );
}

#[test]
fn jev() {
    let now = Timestamp(1_700_000_000_000);
    let mut access = ::jev::Access {
        connection: ::jev::Connection::Connected,
        checked_at: Some(Timestamp(now.0 - 5 * 60_000)),
    };
    assert_eq!(
        readable::jev(&access, now, Style::PLAIN),
        "Jev access  connected\nLast check  5 minutes ago\n"
    );
    access.connection = ::jev::Connection::LastCheckFailed;
    assert!(readable::jev(&access, now, Style::PLAIN).contains(&typed(
        "Jev access  last check failed\n            Check your TypeSafe access, or run `ophio jev connect` to replace the key.\n"
    )));
    assert_eq!(
        readable::jev(&::jev::Access::default(), now, Style::PLAIN),
        typed("Jev access  not connected\n            Run `ophio jev connect`.\n")
    );
}
