// SPDX-License-Identifier: Apache-2.0
//! `ophio setup`: one guided pass through everything Øphio needs, which can
//! be left at any step and taken up again where it was left.
//!
//! The steps are in [`flow`], written against two seams: [`flow::Host`] does
//! things on this computer and asks the running Øphio, and [`flow::Ui`]
//! shows each screen and returns the answer. Tests drive both with fakes, so
//! nothing real is installed, signed in to, started or stopped by them.
use crate::{Result, config::Paths};
use protocol::capability::HarnessKind;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

mod flow;
mod host;
mod plain;
#[cfg(test)]
mod tests;
mod tui;

#[derive(Debug, Clone, Default, clap::Args)]
pub struct SetupArgs {
    /// Show setup as plain text, one screen after another, as a screen
    /// reader or a basic terminal reads best.
    #[arg(long)]
    pub plain: bool,
    /// Show no animation.
    #[arg(long)]
    pub still: bool,
    /// Set up only a computer that has not run Øphio yet, as an installer
    /// does when it finishes; otherwise do nothing.
    #[arg(long, hide = true)]
    pub first_run: bool,
}

/// What setup has done, kept in `setup.json` in the data directory so that a
/// setup left part way carries on where it was.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Progress {
    /// None while setup has neither been finished nor skipped.
    pub finished: Option<Finished>,
    pub steps: BTreeMap<Step, Outcome>,
    /// The phone's answer to whether it gets alerts, once asked.
    pub alerts: Option<bool>,
    /// The agent new projects start with.
    pub agent: Option<HarnessKind>,
    /// The answer to whether Øphio keeps the agents up to date, once asked.
    pub updates: Option<bool>,
    /// Whether where a checklist's files go has been offered: until it is,
    /// Extras offers it ticked.
    pub files: bool,
    /// Extras chosen and then left for later. One no longer offered is
    /// dropped, rather than the rest of the progress with it.
    #[serde(deserialize_with = "offered")]
    pub left: Vec<Pick>,
}

fn offered<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<Pick>, D::Error> {
    let left = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(left
        .into_iter()
        .filter_map(|pick| serde_json::from_value(pick).ok())
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Finished {
    Done,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Step {
    Welcome,
    Background,
    Reach,
    Agent,
    Phone,
    Options,
    Readiness,
    Done,
}
impl Step {
    /// The steps counted as "Step N of 6".
    pub const NUMBERED: [Step; 6] = [
        Step::Background,
        Step::Reach,
        Step::Agent,
        Step::Phone,
        Step::Options,
        Step::Readiness,
    ];
    pub fn number(self) -> Option<usize> {
        Self::NUMBERED
            .iter()
            .position(|s| *s == self)
            .map(|i| i + 1)
    }
    /// Its short name, as the list of steps shows it.
    pub fn name(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome",
            Step::Background => "Keep running",
            Step::Reach => "Reach",
            Step::Agent => "Agents",
            Step::Phone => "Phone",
            Step::Options => "Extras",
            Step::Readiness => "Check",
            Step::Done => "Done",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Done,
    Later,
}

/// The extras setup offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Pick {
    Projects,
    Files,
    Desktop,
    Voice,
    Telegram,
    Discord,
    Accounts,
    Github,
    AgentLogins,
    VoiceControl,
}

/// Where setup starts from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Start {
    /// A computer that has not run Øphio yet.
    First(Progress),
    /// A setup that was left part way.
    Resume(Progress),
    /// Setup was finished or skipped, or Øphio was in use before it existed:
    /// every step is shown again.
    Rerun(Progress),
}
impl Start {
    /// Whether setup still has to be gone through, so that `ophio` alone
    /// starts it.
    pub fn pending(&self) -> bool {
        matches!(self, Start::First(_) | Start::Resume(_))
    }
}

const PROGRESS: &str = "setup.json";

/// Where setup starts on this computer. `others` are the directories of the
/// program's earlier name, which count when deciding whether Øphio was ever
/// used here.
pub(crate) fn start(current: &Paths, others: &[Paths]) -> Start {
    match std::fs::read(current.data.join(PROGRESS)) {
        Ok(bytes) => {
            // A file that can't be read is a setup that has begun.
            let progress: Progress = serde_json::from_slice(&bytes).unwrap_or_default();
            if progress.finished.is_none() {
                Start::Resume(progress)
            } else {
                Start::Rerun(progress)
            }
        }
        Err(_) => {
            let fresh = std::iter::once(current)
                .chain(others)
                .all(|paths| unused(&paths.config) && unused(&paths.data));
            if fresh {
                Start::First(Progress::default())
            } else {
                Start::Rerun(Progress {
                    finished: Some(Finished::Skipped),
                    ..Progress::default()
                })
            }
        }
    }
}

/// Whether `ophio` with nothing after it should start setup: on a computer
/// that has not run Øphio yet, or whose setup was left part way.
pub fn pending_first_run() -> bool {
    use crate::config::{APP_NAME, EARLIER_APP_NAME};
    let (Ok(current), Ok(earlier)) = (Paths::named(APP_NAME), Paths::named(EARLIER_APP_NAME))
    else {
        return false;
    };
    start(&current, &[earlier]).pending()
}

/// Files Øphio may leave before anything is set up: the instance lock that a
/// start takes, and setup's own progress.
const UNUSED_FILES: [&str; 2] = ["instance.lock", PROGRESS];
/// How far into a directory to look before calling it used.
const LOOK: usize = 64;

/// Whether a directory is missing, or holds nothing but empty directories and
/// [`UNUSED_FILES`].
fn unused(directory: &Path) -> bool {
    let mut looked = 0;
    let mut pending = vec![directory.to_owned()];
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return false,
        };
        for entry in entries {
            looked += 1;
            let Ok(entry) = entry else { return false };
            if looked > LOOK {
                return false;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(entry.path()),
                Ok(kind)
                    if kind.is_file()
                        && UNUSED_FILES.iter().any(|name| entry.file_name() == *name) => {}
                _ => return false,
            }
        }
    }
    true
}

pub(crate) fn save(paths: &Paths, progress: &Progress) -> Result<()> {
    crate::identity::private_dir(&paths.data)?;
    let bytes = serde_json::to_vec_pretty(progress)
        .map_err(|_| crate::Error::Config("cannot encode setup progress".into()))?;
    crate::identity::atomic_write(&paths.data.join(PROGRESS), &bytes, true)
}

/// Setup's agent pane against a running Øphio, for the composition tests:
/// the request it shows for a task, and its decisions and replies, sent as
/// the pane sends them.
#[doc(hidden)]
pub mod probe {
    use super::{flow::Host, host::Real};
    use crate::{Result, config::Paths};
    use protocol::{
        command::ApprovalDecision,
        ids::{ApprovalId, TaskId},
    };

    /// The request shown for `task`, and the revision it was shown at.
    pub async fn shown(paths: &Paths, task: TaskId) -> Result<Option<(ApprovalId, u32)>> {
        let view = Real::new(paths.clone())?.task_view(task).await?;
        Ok(view.asks.map(|asked| (asked.id, asked.revision)))
    }

    pub async fn decide(
        paths: &Paths,
        approval: ApprovalId,
        revision: u32,
        decision: ApprovalDecision,
    ) -> Result<()> {
        Real::new(paths.clone())?
            .decide(approval, revision, decision)
            .await
    }

    pub async fn tell(paths: &Paths, task: TaskId, text: &str) -> Result<()> {
        Real::new(paths.clone())?.tell_task(task, text).await
    }
}

pub async fn execute(paths: &Paths, args: SetupArgs) -> Result<()> {
    use crate::config::EARLIER_APP_NAME;
    use std::io::IsTerminal;
    let earlier = Paths::named(EARLIER_APP_NAME)?;
    let start = start(paths, std::slice::from_ref(&earlier));
    let terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if args.first_run {
        if !start.pending() {
            return Ok(());
        }
        if !terminal {
            println!(
                "Set Øphio up with `{} setup`. It starts Øphio, connects your phone and checks what works.",
                ports::COMMAND
            );
            return Ok(());
        }
    }
    if let Start::First(progress) = &start {
        // A setup interrupted from here on carries on where it was.
        save(paths, progress)?;
    }
    let mut host = host::Real::new(paths.clone())?;
    let basic = std::env::var("TERM").is_ok_and(|term| term == "dumb");
    let full = !args.plain && terminal && !basic && tui::fits();
    // A terminal that can't take the full screen gets plain lines instead.
    let tui = full.then(|| tui::Tui::terminal(args.still).ok()).flatten();
    let ending = match tui {
        // Dropped before the ending is printed, on the terminal as it was.
        Some(mut ui) => flow::run(&mut host, &mut ui, start).await,
        None => flow::run(&mut host, &mut plain::Plain::terminal(), start).await,
    };
    if let Ok(ending) = &ending {
        println!("\n{}", ending.message());
    }
    #[cfg(windows)]
    if platform_windows::background::own_console() {
        // Opened from Explorer, the window would close before this is read.
        if let Err(error) = &ending {
            eprintln!("\n{error}");
        }
        plain::Plain::terminal().wait_for_enter().await;
    }
    ending.map(drop)
}
