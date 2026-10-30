// SPDX-License-Identifier: Apache-2.0
//! Alerts while the app is closed.
//!
//! A watcher keeps its own small connection to each paired computer
//! (`/v1/alerts`, see [`protocol::alerts`]) and turns what arrives into
//! notifications. Android's background service starts it on its own thread,
//! not the app's screen, so swiping the app away or Android reclaiming the
//! screen does not stop it.
//!
//! `alerts.json` keeps the person's choice and which alerts each computer has
//! already raised here, so a restart neither repeats an alert nor loses one
//! raised while the phone was away. Restarting the phone or updating the app
//! clears Android's shade, so the alerts still open are raised again.
//!
//! Alerts the person turned off, everywhere or in one project, are counted
//! as raised and never shown. Where the person chose one alert per project,
//! a project's second alert folds the two into one that counts them.
//!
//! A question the agent asked comes with its words, and its notification
//! takes the person's answer. [`answer`] sends it on a short control
//! connection of its own, as the app would send it.
use crate::{
    error::{Error, Result},
    keys::KeyVault,
    links::Links,
    network,
    profiles::{self, Profiles, Record},
};
use futures_util::StreamExt;
use protocol::{
    ApprovalId, AttentionId, ClientFrame, Command, CommandEnvelope, CommandId, ErrorCode,
    ProjectId, Query, QueryData, QueryEnvelope, QueryId, ReceiptStatus, ServerFrame,
    alerts::{Alert, AlertClientFrame, AlertServerFrame, MAX_OPEN_ALERTS, MAX_SHOWING},
    auth::ConnectionKind,
    capability::{AppOffer, HarnessKind},
    command::{ApprovalDecision, QuestionAnswer},
    model::AttentionKind,
    notifications::AlertTopic,
    time::Timestamp,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::watch, task::JoinSet};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "android")]
mod android;

/// The computer pings every four minutes, so this much silence means the
/// connection is gone without having said so.
const SILENCE: Duration = Duration::from_secs(10 * 60);
/// The wait before reconnecting after each failure in a row.
const RETRY: [Duration; 5] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
];
/// The wait when only an update or a new pairing can help. The app opening
/// or a network change ends it sooner.
const IDLE: Duration = Duration::from_secs(60 * 60);
/// How often the watcher looks whether the app has left the screen, while
/// alerts that arrived as it showed them wait.
const LOOK: Duration = Duration::from_secs(2);
/// How often the watcher looks for room in the shade while alerts wait for
/// it, as the person clears alerts.
const ROOM: Duration = Duration::from_secs(15);
const FILE: &str = "alerts.json";
/// The most alerts the watcher keeps in Android's shade. Android silently
/// drops an app's notifications beyond about 50, its summaries among them,
/// so the rest wait for room. The device plugin's `Notifications.MAX_SHOWN`
/// refuses the app's own beyond it.
const MAX_SHOWN: usize = 24;
/// The tag of the note that says how many alerts wait for room. It names no
/// computer, so no computer's list clears it.
const MORE: &str = "more";
/// How long an answer from a notification may take, signing in included.
const ANSWERING: Duration = Duration::from_secs(20);

/// How much of a task an alert shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    Minimal,
    TaskTitle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Alerts when the app is closed.
    pub enabled: bool,
    pub detail: Detail,
}

/// How the watcher's connection to one computer is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Connecting,
    Watching,
    /// Øphio on the computer predates background alerts.
    UpdateNeeded,
    Unreachable,
    /// The computer no longer accepts this phone.
    NotAllowed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    NeedsYou,
    Finished,
}

/// One notification as Android shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    /// `<profile>/<attention>`, as the app's own alerts are tagged, so
    /// either replaces the other.
    pub tag: String,
    pub category: Category,
    pub title: String,
    pub body: String,
    /// Finished tasks from one computer stack under its name.
    pub group: Option<String>,
    pub group_title: Option<String>,
    pub route: serde_json::Value,
    /// A question the person can answer from the notification itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<Asking>,
}

/// A question as its notification asks it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Asking {
    pub text: String,
    /// Answers to offer, in the order offered. The person may always write
    /// their own instead.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Where the answer goes. The notification's actions carry it back.
    pub reply: Reply,
}

/// Where an answer from a notification goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub profile_id: String,
    pub approval_id: ApprovalId,
    /// The approval's revision when asked: one answered or changed since
    /// refuses the answer.
    pub revision: u32,
    /// The question exactly as asked; none when it was asked in words only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<String>,
}

/// What became of an alert posted to the shade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posted {
    Shown,
    /// The shade filled meanwhile, as with alerts the app raised itself.
    NoRoom,
    /// The app cleared it, as answered, since its alert was learned of.
    Cleared,
}

/// Where alerts are shown: Android's notification shade.
pub trait Shade: Send + Sync {
    /// Shows `note`. Given `since`, the shade's count of clears when its
    /// alert was learned of, shows nothing if it was cleared after then.
    fn post(&self, note: &Note, since: Option<i64>) -> Result<Posted>;
    fn cancel(&self, tag: &str);
    /// How many alerts the app and the watcher have cleared, by the shade's count.
    fn clears(&self) -> i64;
    /// The tags of the alerts showing now.
    fn shown(&self) -> Vec<String>;
    /// Whether the app's screen is in front and unlocked.
    fn on_screen(&self) -> bool;
    /// The computer `name` offers these phone apps: the shade says once that
    /// an update is ready if one is for this app.
    fn app_updates(&self, _profile_id: &str, _name: &str, _offers: &[AppOffer]) {}
}

#[derive(Serialize, Deserialize)]
struct Book {
    version: u32,
    /// None until the person has answered.
    settings: Option<Settings>,
    /// Which of Android's shades the alerts raised here are in, as of the
    /// watcher's last start.
    #[serde(default)]
    shade: Option<String>,
    #[serde(default)]
    computers: BTreeMap<String, Seen>,
    /// How many alerts the note of those waiting counts.
    #[serde(skip)]
    more: usize,
    /// The shade's count of clears as the update under way began: what the
    /// computer says is open is learned of at it.
    #[serde(skip)]
    clears: i64,
}
impl Default for Book {
    fn default() -> Self {
        Self {
            version: 1,
            settings: None,
            shade: None,
            computers: BTreeMap::new(),
            more: 0,
            clears: 0,
        }
    }
}
impl Book {
    fn off(&self) -> bool {
        self.settings.is_some_and(|s| !s.enabled)
    }
    fn detail(&self) -> Detail {
        self.settings.map_or(Detail::Minimal, |s| s.detail)
    }
    /// The watcher starts with Android's shade as `shade`. When that changed,
    /// which cleared every alert it had raised, the next open list raises
    /// again those still open.
    fn started(&mut self, shade: &str) {
        if self.shade.as_ref().is_some_and(|s| s != shade) {
            for seen in self.computers.values_mut() {
                for id in std::mem::take(&mut seen.raised) {
                    seen.notified.remove(&id);
                }
                seen.folds.clear();
            }
        }
        self.shade = Some(shade.to_owned());
    }
    /// A watcher starts anew, as in a new process: what waited, for room or
    /// for the app, rises again from each computer's next open list, once
    /// that says what was answered while none watched.
    fn restarted(&mut self) {
        for seen in self.computers.values_mut() {
            seen.waiting.clear();
            seen.held.clear();
            seen.live = false;
        }
        self.more = 0;
    }
    /// The watcher began watching a computer at `at`, by this phone's clock:
    /// what it asks from then on is new, even before it is first reached.
    fn begin(&mut self, profile_id: &str, at: Timestamp) {
        if !self.off() {
            self.computers
                .entry(profile_id.to_owned())
                .or_insert_with(|| Seen::began(at));
        }
    }
    /// The app showed these alerts on its screen, so they are not raised.
    fn seen_in_app(&mut self, profile_id: &str, ids: &[AttentionId]) {
        if let Some(seen) = self.computers.get_mut(profile_id) {
            for id in ids {
                seen.waiting.remove(id);
                seen.notified.insert(*id);
            }
            seen.held.retain(|r| !ids.contains(&r.alert.attention_id));
        }
    }
    /// The app raised this alert itself, as the watcher would have.
    fn raised_by_app(&mut self, profile_id: &str, id: AttentionId) {
        if let Some(seen) = self.computers.get_mut(profile_id) {
            seen.waiting.remove(&id);
            seen.held.retain(|r| r.alert.attention_id != id);
            seen.notified.insert(id);
            seen.raised.insert(id);
        }
    }
    /// The alerts from a computer this phone shows, or keeps to show or
    /// for the app: those `shown` first, then those waiting for room or for
    /// the app to leave the screen. The computer's next open list names each
    /// no longer open. One waiting that the list has no room to name is
    /// forgotten, as the computer could not say it was answered: the open
    /// list raises it again if it is still open.
    fn showing(&mut self, profile_id: &str, shown: &[String]) -> Vec<AttentionId> {
        let prefix = format!("{profile_id}/");
        let mut ids: Vec<AttentionId> = (shown.iter())
            .filter_map(|t| t.strip_prefix(&prefix)?.parse().ok())
            .collect();
        let seen = self.computers.get_mut(profile_id);
        if let Some(seen) = &seen {
            ids.extend(seen.held.iter().map(|r| r.alert.attention_id));
            ids.extend(seen.waiting.keys().copied());
            ids.extend(seen.notified.iter().copied());
        }
        let mut named = BTreeSet::new();
        ids.retain(|id| named.insert(*id));
        ids.truncate(MAX_SHOWING);
        if let Some(seen) = seen {
            let named: BTreeSet<_> = ids.iter().collect();
            seen.held.retain(|r| named.contains(&r.alert.attention_id));
            seen.waiting.retain(|id, _| named.contains(id));
        }
        ids
    }
    /// The connection to a computer ended: what it keeps waits until the
    /// next one says what changed meanwhile.
    fn ended(&mut self, profile_id: &str) {
        if let Some(seen) = self.computers.get_mut(profile_id) {
            seen.live = false;
        }
    }
    /// Android did not show these: the next open list raises them again.
    fn unraise(&mut self, tags: &[String]) {
        if tags.is_empty() {
            return;
        }
        for (profile_id, seen) in &mut self.computers {
            let kept = |id: &AttentionId| !tags.contains(&tag(profile_id, id));
            seen.notified.retain(kept);
            seen.raised.retain(kept);
        }
    }
    fn waiting(&self, profile_id: &str) -> bool {
        self.computers
            .get(profile_id)
            .is_some_and(|s| !s.waiting.is_empty())
    }
    /// See [`configure`].
    fn configure(&mut self, settings: Settings) -> Vec<String> {
        let mut raised = Vec::new();
        if !settings.enabled {
            // The note may be left from before a restart, which counts none.
            self.more = 0;
            raised.push(MORE.to_owned());
            for (profile_id, seen) in std::mem::take(&mut self.computers) {
                raised.extend(seen.raised.iter().map(|id| tag(&profile_id, id)));
                raised.extend(
                    (seen.folds.iter())
                        .filter(|(_, f)| f.folded)
                        .map(|(project_id, _)| folded_tag(&profile_id, project_id)),
                );
            }
        }
        self.settings = Some(settings);
        raised
    }
    /// Whether alerts wait for room that may rise once there is some.
    fn holding(&self) -> bool {
        self.computers
            .values()
            .any(|s| s.live && !s.held.is_empty())
    }
    /// Of `raises`, those with room in the shade, which shows `shown`: what
    /// waited for room first, then the rest in order. Those without room
    /// wait, unrecorded, so a restart raises them again from the open list.
    /// What waits rises only from a computer connected and caught up, as
    /// one answered meanwhile would otherwise rise before the computer says
    /// so, and not while the app's screen shows that computer, `in_app`: the
    /// app shows it.
    fn fit(
        &mut self,
        raises: Vec<Raise>,
        shown: &[String],
        in_app: &BTreeSet<String>,
    ) -> Vec<Raise> {
        let showing = |tag: &str| shown.iter().any(|t| t == tag);
        let mut room = MAX_SHOWN.saturating_sub(shown.iter().filter(|t| *t != MORE).count());
        let mut post = Vec::new();
        for (profile_id, seen) in &mut self.computers {
            if !seen.live || in_app.contains(profile_id) {
                continue;
            }
            let ready = room.min(seen.held.len());
            for raise in seen.held.drain(..ready) {
                seen.notified.insert(raise.alert.attention_id);
                seen.raised.insert(raise.alert.attention_id);
                post.push(raise);
            }
            room -= ready;
        }
        let mut refused = Vec::new();
        for raise in raises {
            if showing(&raise.tag()) {
                post.push(raise);
            } else if room > 0 {
                room -= 1;
                post.push(raise);
            } else {
                refused.push(raise);
            }
        }
        self.hold(refused, false);
        post
    }
    /// Keeps `raises`, unrecorded, until there is room: those Android had
    /// no room for after all `first`, as they waited longest.
    fn hold(&mut self, raises: Vec<Raise>, first: bool) {
        let mut at = 0;
        for raise in raises {
            let Some(seen) = self.computers.get_mut(&raise.computer.profile_id) else {
                continue;
            };
            let id = raise.alert.attention_id;
            seen.notified.remove(&id);
            seen.raised.remove(&id);
            if seen.held.iter().any(|r| r.alert.attention_id == id) {
                continue;
            }
            if first {
                seen.held.insert(at.min(seen.held.len()), raise);
                at += 1;
            } else {
                seen.held.push(raise);
            }
        }
    }
    /// The note of the alerts waiting for room, when their count changed;
    /// `Err` with the tag when none wait.
    fn more(&mut self) -> Option<std::result::Result<Note, &'static str>> {
        let held: Vec<&Raise> = (self.computers.values())
            .flat_map(|s| s.held.iter())
            .collect();
        if held.len() == self.more {
            return None;
        }
        self.more = held.len();
        if held.is_empty() {
            return Some(Err(MORE));
        }
        let needs_you = (held.iter())
            .any(|r| heading(r.alert.kind).is_some_and(|(c, _)| c == Category::NeedsYou));
        let count = match held.len() {
            1 => "1 more alert".to_owned(),
            n => format!("{n} more alerts"),
        };
        Some(Ok(Note {
            tag: MORE.into(),
            category: if needs_you {
                Category::NeedsYou
            } else {
                Category::Finished
            },
            title: count,
            body: "They show here as you clear these. Open Øphio to see them all.".into(),
            group: None,
            group_title: None,
            route: serde_json::Value::Null,
            question: None,
        }))
    }
}

/// What one computer has raised on this phone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Seen {
    /// The computer's time from which an open alert is new here: one opened
    /// since, and neither raised nor seen in the app, was missed. Older ones
    /// were the app's to show. None until the computer first lists them.
    since: Option<Timestamp>,
    /// This phone's time when it began watching, which the computer's first
    /// list turns into `since`.
    #[serde(default)]
    began: Option<Timestamp>,
    /// Open alerts already raised here, or seen in the app.
    notified: BTreeSet<AttentionId>,
    /// Those of them raised here.
    #[serde(default)]
    raised: BTreeSet<AttentionId>,
    /// New alerts that arrived while the app's screen showed the computer,
    /// raised if the app leaves the screen before reporting them seen, each
    /// with the count of clears when it was learned of.
    #[serde(skip)]
    waiting: BTreeMap<AttentionId, (Alert, i64)>,
    /// Alerts waiting, oldest first, for room in Android's shade. They are
    /// written out as they rise, with the detail chosen then.
    #[serde(skip)]
    held: Vec<Raise>,
    /// Connected, and told what changed while it was not: only then may
    /// what it keeps rise.
    #[serde(skip)]
    live: bool,
    /// Projects whose alerts show as one, by project.
    #[serde(default)]
    folds: BTreeMap<ProjectId, Fold>,
}
impl Seen {
    fn began(at: Timestamp) -> Self {
        Self {
            since: None,
            began: Some(at),
            notified: BTreeSet::new(),
            raised: BTreeSet::new(),
            waiting: BTreeMap::new(),
            held: Vec::new(),
            live: false,
            folds: BTreeMap::new(),
        }
    }
    fn knows(&self, id: &AttentionId) -> bool {
        self.notified.contains(id)
            || self.waiting.contains_key(id)
            || self.held.iter().any(|r| r.alert.attention_id == *id)
    }
    /// The alert is no longer open.
    fn forget(&mut self, id: &AttentionId) {
        self.notified.remove(id);
        self.raised.remove(id);
        self.waiting.remove(id);
        self.held.retain(|r| r.alert.attention_id != *id);
    }
    /// The open list, received at `at` by this phone's clock and `now` by
    /// the computer's. Returns the alerts missed meanwhile, oldest first, and
    /// forgets the ones no longer open.
    fn reopen<'a>(&mut self, alerts: &'a [Alert], now: Timestamp, at: Timestamp) -> Vec<&'a Alert> {
        // Only the time elapsed here carries over: the two clocks can differ.
        let began = self.began;
        let since = *self.since.get_or_insert_with(|| {
            let elapsed = began.map_or(0, |b| (at.millis() - b.millis()).max(0));
            Timestamp(now.millis().saturating_sub(elapsed))
        });
        // A full list leaves the oldest out, so one missing may still be open.
        if alerts.len() < MAX_OPEN_ALERTS {
            let open: BTreeSet<_> = alerts.iter().map(|a| a.attention_id).collect();
            self.notified.retain(|id| open.contains(id));
            self.raised.retain(|id| open.contains(id));
            self.waiting.retain(|id, _| open.contains(id));
            self.held.retain(|r| open.contains(&r.alert.attention_id));
        }
        let mut missed: Vec<&Alert> = alerts
            .iter()
            .filter(|a| a.created_at >= since && !self.knows(&a.attention_id))
            .collect();
        missed.sort_by_key(|a| a.created_at);
        missed
    }
    /// The person turned this alert off since it was learned of: whatever
    /// waits to show of it goes, and it counts as raised. Returns whether it
    /// showed.
    fn hush(&mut self, id: AttentionId) -> bool {
        self.waiting.remove(&id);
        self.held.retain(|r| r.alert.attention_id != id);
        self.notified.insert(id);
        self.raised.remove(&id)
    }
    /// The computer says `alerts` are open, as of the count of clears
    /// `since`: those kept to raise were learned of again then.
    fn heard(&mut self, alerts: &[Alert], since: i64) {
        let open = |id: &AttentionId| alerts.iter().any(|a| a.attention_id == *id);
        for raise in &mut self.held {
            if open(&raise.alert.attention_id) {
                raise.since = since;
            }
        }
        for (id, (_, learned)) in &mut self.waiting {
            if open(id) {
                *learned = since;
            }
        }
    }
    /// Raises `alert`, learned of at the count of clears `since`, or keeps
    /// it waiting while the app shows it.
    fn raise(
        &mut self,
        computer: &Computer,
        alert: &Alert,
        in_app: bool,
        since: i64,
    ) -> Option<Raise> {
        // Turned off: counted as raised, so turning it on raises nothing.
        if in_app && !alert.quiet {
            self.waiting
                .insert(alert.attention_id, (alert.clone(), since));
            return None;
        }
        self.waiting.remove(&alert.attention_id);
        self.notified.insert(alert.attention_id);
        if alert.quiet {
            return None;
        }
        heading(alert.kind)?;
        self.raised.insert(alert.attention_id);
        Some(Raise {
            computer: computer.clone(),
            alert: alert.clone(),
            since,
        })
    }
}

/// A paired computer as its alerts name it.
#[derive(Debug, Clone, PartialEq)]
struct Computer {
    profile_id: String,
    name: String,
}

/// A project's alerts shown as one: the first on its own, then, from the
/// second, one note that counts them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Fold {
    project: String,
    computer: String,
    /// The alerts it counts, each with whether it needs the person.
    members: BTreeMap<AttentionId, bool>,
    /// The note that counts them shows; before, the first shows on its own.
    folded: bool,
    /// How many the note shows.
    counted: usize,
}
impl Fold {
    fn note(&self, profile_id: &str, project_id: &ProjectId) -> Note {
        let needs_you = self.members.values().filter(|n| **n).count();
        let other = self.members.len() - needs_you;
        let mut body = self.computer.clone();
        let mut parts = Vec::new();
        if needs_you > 0 {
            parts.push(format!("{needs_you} need you"));
        }
        if other > 0 {
            parts.push(format!("{other} to look at"));
        }
        if !parts.is_empty() {
            body.push_str(" · ");
            body.push_str(&parts.join(", "));
        }
        Note {
            tag: folded_tag(profile_id, project_id),
            category: if needs_you > 0 {
                Category::NeedsYou
            } else {
                Category::Finished
            },
            title: match self.members.len() {
                1 => format!("{} has 1 notification", self.project),
                n => format!("{} has {n} notifications", self.project),
            },
            body,
            group: None,
            group_title: None,
            route: serde_json::json!({
                "kind": "needs_you",
                "profileId": profile_id,
                "projectId": project_id,
            }),
            question: None,
        }
    }
}

/// An alert to raise. It is written out as it rises, so one that waited
/// for room shows the detail chosen by then.
#[derive(Debug, Clone, PartialEq)]
struct Raise {
    computer: Computer,
    alert: Alert,
    /// The shade's count of clears when the computer last said it was open:
    /// if the app clears it after, as answered, it does not rise.
    since: i64,
}
impl Raise {
    fn tag(&self) -> String {
        tag(&self.computer.profile_id, &self.alert.attention_id)
    }
    fn note(&self, detail: Detail) -> Option<Note> {
        note(&self.computer, &self.alert, detail)
    }
}

#[derive(Debug, Default, PartialEq)]
struct Changes {
    post: Vec<Raise>,
    cancel: Vec<String>,
}

/// What a frame from `computer`, received at `at` by this phone's clock,
/// changes. `in_app` is true when the app's screen is showing that computer,
/// which then shows its alerts itself. `shown` is the tags showing now.
fn receive(
    book: &mut Book,
    computer: &Computer,
    frame: AlertServerFrame,
    in_app: bool,
    shown: &[String],
    at: Timestamp,
) -> Changes {
    // Turned off while a frame was on its way.
    if book.off() {
        return Changes::default();
    }
    let id = &computer.profile_id;
    let clears = book.clears;
    let mut changes = Changes::default();
    match frame {
        AlertServerFrame::Open { alerts, now, gone } => {
            let seen = (book.computers.entry(id.clone())).or_insert_with(|| Seen::began(at));
            // Those answered while away go first, so none kept rises.
            for attention_id in gone.iter().take(MAX_SHOWING) {
                seen.forget(attention_id);
                changes.cancel.push(tag(id, attention_id));
            }
            seen.heard(&alerts, clears);
            // Those the person turned off since they were raised go.
            for alert in alerts.iter().filter(|a| a.quiet) {
                if seen.knows(&alert.attention_id) && seen.hush(alert.attention_id) {
                    changes.cancel.push(tag(id, &alert.attention_id));
                }
            }
            for alert in seen.reopen(&alerts, now, at) {
                changes
                    .post
                    .extend(seen.raise(computer, alert, in_app, clears));
            }
            seen.live = true;
            // A full list says nothing of the alerts it leaves out.
            if alerts.len() < MAX_OPEN_ALERTS {
                let open: BTreeSet<String> =
                    alerts.iter().map(|a| tag(id, &a.attention_id)).collect();
                let prefix = format!("{id}/");
                for t in shown {
                    // A project's note goes once it counts none.
                    let folded = t
                        .strip_prefix(&prefix)
                        .is_some_and(|rest| rest.starts_with(FOLDED));
                    if t.starts_with(&prefix)
                        && !folded
                        && !open.contains(t)
                        && !changes.cancel.contains(t)
                    {
                        changes.cancel.push(t.clone());
                    }
                }
            }
        }
        AlertServerFrame::Raised { alert } => {
            let seen = (book.computers.entry(id.clone())).or_insert_with(|| Seen::began(at));
            if seen.knows(&alert.attention_id) {
                seen.heard(std::slice::from_ref(&alert), clears);
            } else {
                changes
                    .post
                    .extend(seen.raise(computer, &alert, in_app, clears));
            }
        }
        AlertServerFrame::Resolved { attention_id } => {
            if let Some(seen) = book.computers.get_mut(id) {
                seen.forget(&attention_id);
            }
            changes.cancel.push(tag(id, &attention_id));
        }
    }
    changes
}

/// The app's screen no longer shows `computer`: what arrived while it did,
/// and the app has not reported seen, is raised.
fn unwatched(book: &mut Book, computer: &Computer) -> Changes {
    let mut changes = Changes::default();
    if book.off() {
        return changes;
    }
    // One answered while not connected rises only once the computer says.
    if let Some(seen) = book.computers.get_mut(&computer.profile_id)
        && seen.live
    {
        let mut waiting: Vec<(Alert, i64)> =
            std::mem::take(&mut seen.waiting).into_values().collect();
        waiting.sort_by_key(|(a, _)| a.created_at);
        for (alert, since) in &waiting {
            changes
                .post
                .extend(seen.raise(computer, alert, false, *since));
        }
    }
    changes
}

fn tag(profile_id: &str, attention_id: &AttentionId) -> String {
    format!("{profile_id}/{}", attention_id.as_uuid())
}

/// After a computer's prefix, the note that counts a project's alerts.
const FOLDED: &str = "project:";

/// The note that counts a project's alerts, under its computer's prefix so
/// it goes with the computer's alerts; the app tags its own the same way.
fn folded_tag(profile_id: &str, project_id: &ProjectId) -> String {
    format!("{profile_id}/{FOLDED}{}", project_id.as_uuid())
}

/// The category and title of an alert of `kind`; none for a kind that
/// raises no alert.
fn heading(kind: AttentionKind) -> Option<(Category, &'static str)> {
    Some(match kind {
        AttentionKind::Question => (Category::NeedsYou, "Question from"),
        AttentionKind::Approval => (Category::NeedsYou, "Approval waiting"),
        AttentionKind::WaitingForYou
        | AttentionKind::Conflict
        | AttentionKind::ReconciliationRequired => (Category::NeedsYou, "Task needs you"),
        AttentionKind::Completed => (Category::Finished, "Task finished"),
        AttentionKind::Failure => (Category::Finished, "Task stopped with a problem"),
        AttentionKind::ConnectionIssue => return None,
    })
}

fn note(computer: &Computer, alert: &Alert, detail: Detail) -> Option<Note> {
    if let Some(report) = &alert.report {
        return Some(Note {
            tag: tag(&computer.profile_id, &alert.attention_id),
            category: if report.question.is_some() {
                Category::NeedsYou
            } else {
                Category::Finished
            },
            title: format!("Report {}", report.reference),
            body: "There’s an update on your problem report.".into(),
            group: None,
            group_title: None,
            question: None,
            route: serde_json::json!({ "kind": "report", "profileId": computer.profile_id, "reportId": report.report_id }),
        });
    }
    let (category, title) = heading(alert.kind)?;
    let title = match (alert.kind, &alert.headline) {
        (_, Some(headline)) => headline.clone(),
        (AttentionKind::Question, None) => format!("{title} {}", agent(alert.harness)),
        _ => title.to_owned(),
    };
    let mut body = format!("{} · {}", computer.name, alert.project);
    if detail == Detail::TaskTitle && !alert.task.is_empty() {
        body.push_str(" · ");
        body.push_str(&alert.task);
    }
    let group =
        (category == Category::Finished).then(|| format!("{}:finished", computer.profile_id));
    Some(Note {
        tag: tag(&computer.profile_id, &alert.attention_id),
        category,
        title,
        body,
        group_title: group.as_ref().map(|_| computer.name.clone()),
        group,
        route: serde_json::json!({
            "kind": "attention",
            "profileId": computer.profile_id,
            "attentionId": alert.attention_id,
        }),
        question: (alert.question.as_ref())
            .filter(|_| alert.kind == AttentionKind::Question)
            .map(|q| Asking {
                text: q.text.clone(),
                choices: q.choices.clone(),
                reply: Reply {
                    profile_id: computer.profile_id.clone(),
                    approval_id: q.approval_id,
                    revision: q.revision,
                    asked: q.asked.clone(),
                },
            }),
    })
}

fn agent(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::ClaudeCode => "Claude Code",
        HarnessKind::Codex => "Codex",
        HarnessKind::Hermes => "Hermes",
        HarnessKind::OpenClaw => "OpenClaw",
        HarnessKind::Pi => "Pi",
    }
}

/// Sends the person's answer, `text`, to the question `reply` names, from
/// its notification. It goes on a control connection of its own, as the
/// app's `ResolveApproval`, so the computer applies all its usual checks.
/// Done once the computer took it; the question's alerts then close as any
/// answered elsewhere do.
pub async fn answer(data: &Path, vault: &dyn KeyVault, reply: &Reply, text: &str) -> Result<()> {
    let envelope = answering(reply, text)?;
    let id = envelope.command_id;
    let record = Profiles::open(data.to_owned())?.get(&reply.profile_id)?;
    tokio::time::timeout(ANSWERING, async {
        let (mut ws, _, _) =
            network::authenticate(&record, ConnectionKind::Control, vault, || {}).await?;
        network::send_json(&mut ws, &ClientFrame::Command(Box::new(envelope))).await?;
        let taken = loop {
            let message = ws.next().await.ok_or(Error::Closed)?;
            match message.map_err(|_| Error::Closed)? {
                Message::Text(text) => {
                    if let Some(taken) = receipt(&text, id) {
                        break taken;
                    }
                }
                Message::Close(_) => return Err(Error::Closed),
                _ => {}
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
        taken
    })
    .await
    .map_err(|_| Error::Timeout)?
}

/// The command that answers `reply`'s question with `text`.
fn answering(reply: &Reply, text: &str) -> Result<CommandEnvelope> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::Invalid("Write an answer first."));
    }
    let answers = (reply.asked.iter())
        .map(|question| QuestionAnswer {
            id: None,
            question: question.clone(),
            choices: vec![text.to_owned()],
        })
        .collect();
    Ok(CommandEnvelope {
        expected_revision: Some(reply.revision.into()),
        ..CommandEnvelope::new(Command::ResolveApproval {
            approval_id: reply.approval_id,
            decision: ApprovalDecision::Answer {
                text: text.to_owned(),
                answers,
            },
        })
    })
}

/// What the computer said of command `id`, if `text` says it.
fn receipt(text: &str, id: CommandId) -> Option<Result<()>> {
    match serde_json::from_str::<ServerFrame>(text).ok()? {
        ServerFrame::Receipt(receipt) if receipt.command_id == id => Some(match receipt.status {
            ReceiptStatus::Accepted | ReceiptStatus::Duplicate => Ok(()),
            ReceiptStatus::Rejected => Err(receipt.error.map_or(
                Error::Invalid("The computer didn’t take the answer."),
                Error::Remote,
            )),
        }),
        ServerFrame::Fatal(error) => Some(Err(Error::Remote(error))),
        _ => None,
    }
}

/// Why an answer was not sent, as its notification says.
pub fn unsent(error: &Error) -> String {
    match error.remote_code() {
        Some(
            ErrorCode::StaleRevision
            | ErrorCode::NotFound
            | ErrorCode::Conflict
            | ErrorCode::AlreadyResolved,
        ) => "It was answered or changed elsewhere.".into(),
        Some(ErrorCode::Expired) => "The agent stopped waiting for it.".into(),
        _ => error.connection_message(),
    }
}

struct Ledger {
    path: PathBuf,
    book: Book,
    /// What the file holds, so an unchanged book is not written again.
    saved: Vec<u8>,
}
static LEDGER: Mutex<Option<Ledger>> = Mutex::new(None);
static STATUS: Mutex<BTreeMap<String, Status>> = Mutex::new(BTreeMap::new());
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static APP: OnceLock<Weak<Links>> = OnceLock::new();
/// Which start of the watcher is current. One stopped finishes its
/// connections on its own time, and must not touch the book or the shade
/// meanwhile, as a replacement may already watch.
static WATCHER: AtomicU64 = AtomicU64::new(0);

fn next_watcher() -> u64 {
    WATCHER.fetch_add(1, Ordering::SeqCst).wrapping_add(1)
}
fn current(watcher: u64) -> bool {
    WATCHER.load(Ordering::SeqCst) == watcher
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Reads the book from the app's data directory. The app and Android's
/// background service each call this; the first wins.
pub fn init(data: &Path) {
    let mut ledger = lock(&LEDGER);
    if ledger.is_some() {
        return;
    }
    let path = data.join(FILE);
    let saved = match fs::read(&path) {
        Ok(bytes) if bytes.len() <= 4 * 1024 * 1024 => bytes,
        _ => Vec::new(),
    };
    let book = serde_json::from_slice::<Book>(&saved)
        .ok()
        .filter(|b| b.version == 1)
        .unwrap_or_default();
    *ledger = Some(Ledger { path, book, saved });
}

/// Changes the book and saves it. On a failed save the change stays in
/// memory and the next one tries again.
fn with_book<T>(f: impl FnOnce(&mut Book) -> T) -> Result<T> {
    let mut guard = lock(&LEDGER);
    let ledger = guard.as_mut().ok_or(Error::Storage)?;
    let out = f(&mut ledger.book);
    ledger.save()?;
    Ok(out)
}
/// As [`with_book`], unless `watcher` was stopped, which changes nothing:
/// a replacement may already watch. The check holds while `f` runs, so `f`
/// clears from the shade too.
fn with_watched_book<T>(watcher: u64, f: impl FnOnce(&mut Book) -> T) -> Result<Option<T>> {
    with_book(|book| current(watcher).then(|| f(book)))
}
impl Ledger {
    fn save(&mut self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.book).map_err(|_| Error::Storage)?;
        if bytes != self.saved {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir).map_err(|_| Error::Storage)?;
            }
            profiles::write_private(&self.path, &bytes)?;
            self.saved = bytes;
        }
        Ok(())
    }
}

/// The person's choice, or None if they have not been asked.
pub fn settings() -> Result<Option<Settings>> {
    with_book(|book| book.settings)
}

/// Saves the person's choice. Turning alerts off forgets what each computer
/// raised, so turning them on again starts afresh rather than raising
/// everything that happened meanwhile. Returns the tags of the alerts the
/// watcher raised, which nothing clears once it stops.
pub fn configure(settings: Settings) -> Result<Vec<String>> {
    with_book(|book| book.configure(settings))
}

/// The app showed these alerts from a computer on its screen, so the watcher
/// does not raise them.
pub fn seen(profile_id: &str, ids: &[AttentionId]) -> Result<()> {
    with_book(|book| book.seen_in_app(profile_id, ids))
}

/// The app raised this alert, tagged `<profile>/<attention>`, itself. It is
/// recorded as the watcher records its own, so a restart of the phone raises
/// it again and turning alerts off clears it.
pub fn raised_by_app(tag: &str) -> Result<()> {
    let Some((profile_id, id)) = tag.split_once('/') else {
        return Ok(());
    };
    let Ok(id) = id.parse::<AttentionId>() else {
        return Ok(());
    };
    with_book(|book| book.raised_by_app(profile_id, id))
}

/// Each paired computer's connection, while the watcher runs.
pub fn statuses() -> BTreeMap<String, Status> {
    lock(&STATUS).clone()
}

/// Lets the watcher see which computers the app's screen is connected to.
pub fn attach(links: &Arc<Links>) {
    let _ = APP.set(Arc::downgrade(links));
}

struct Running {
    stop: CancellationToken,
    reload: watch::Sender<u64>,
    nudge: watch::Sender<u64>,
    retry: watch::Sender<u64>,
}

/// Starts watching every computer paired in `data`, unless already watching.
/// `cleared` changes whenever Android clears every alert the app raised: a
/// restart of the phone or an update of the app.
pub fn start(
    data: &Path,
    cleared: &str,
    shade: Arc<dyn Shade>,
    vault: Arc<dyn KeyVault>,
) -> Result<()> {
    init(data);
    with_book(|book| book.started(cleared))?;
    let mut running = lock(&RUNNING);
    if running.is_some() {
        return Ok(());
    }
    // A note of alerts waiting for room outlives the watcher that counted
    // them: none wait now.
    with_book(|book| {
        book.restarted();
        shade.cancel(MORE);
    })?;
    let unavailable = || Error::Native("The phone could not start watching for alerts.".into());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| unavailable())?;
    let stop = CancellationToken::new();
    let (reload, reloaded) = watch::channel(0);
    let (nudge, nudged) = watch::channel(0);
    let (retry, retried) = watch::channel(0);
    let wake = Wake {
        stop: stop.clone(),
        nudge: nudged,
        retry: retried,
        watcher: next_watcher(),
    };
    let data = data.to_owned();
    std::thread::Builder::new()
        .name("ophio-alerts".into())
        .spawn(move || runtime.block_on(supervise(data, shade, vault, reloaded, wake)))
        .map_err(|_| unavailable())?;
    *running = Some(Running {
        stop,
        reload,
        nudge,
        retry,
    });
    Ok(())
}

pub fn stop() {
    // Cancelled under the status lock, so no computer's status outlives it.
    let mut status = lock(&STATUS);
    if let Some(running) = lock(&RUNNING).take() {
        next_watcher();
        running.stop.cancel();
    }
    status.clear();
}

pub fn running() -> bool {
    lock(&RUNNING).is_some()
}

fn signal(pick: impl FnOnce(&Running) -> &watch::Sender<u64>) {
    if let Some(running) = lock(&RUNNING).as_ref() {
        pick(running).send_modify(|n| *n = n.wrapping_add(1));
    }
}
/// The paired computers changed.
pub fn reload() {
    signal(|r| &r.reload);
}
/// The network changed: every connection starts again on the new one.
pub fn nudge() {
    signal(|r| &r.nudge);
}
/// The app opened: a computer waiting to be tried again is tried now.
pub fn retry() {
    signal(|r| &r.retry);
}

#[derive(Clone)]
struct Wake {
    stop: CancellationToken,
    nudge: watch::Receiver<u64>,
    retry: watch::Receiver<u64>,
    watcher: u64,
}

fn set_status(stop: &CancellationToken, profile_id: &str, status: Status) {
    let mut statuses = lock(&STATUS);
    if !stop.is_cancelled() {
        statuses.insert(profile_id.to_owned(), status);
    }
}

async fn supervise(
    data: PathBuf,
    shade: Arc<dyn Shade>,
    vault: Arc<dyn KeyVault>,
    mut reload: watch::Receiver<u64>,
    wake: Wake,
) {
    let stop = wake.stop.clone();
    loop {
        reload.borrow_and_update();
        let records: Vec<Record> = Profiles::open(data.clone())
            .map(|p| {
                p.list()
                    .iter()
                    .filter_map(|h| p.get(&h.profile_id).ok())
                    .collect()
            })
            .unwrap_or_default();
        let ids: BTreeSet<String> = records
            .iter()
            .map(|r| r.profile.profile_id.clone())
            .collect();
        forget_others(&ids, shade.as_ref(), wake.watcher);
        {
            let mut statuses = lock(&STATUS);
            if !stop.is_cancelled() {
                statuses.retain(|id, _| ids.contains(id));
            }
        }
        let round = stop.child_token();
        let mut tasks = JoinSet::new();
        for record in records {
            let wake = Wake {
                stop: round.clone(),
                ..wake.clone()
            };
            tasks.spawn(watch_computer(record, shade.clone(), vault.clone(), wake));
        }
        let changed = tokio::select! {
            _ = stop.cancelled() => false,
            changed = reload.changed() => changed.is_ok(),
        };
        round.cancel();
        while tasks.join_next().await.is_some() {}
        if !changed || stop.is_cancelled() {
            return;
        }
    }
}

/// Drops what the book and the shade hold for computers no longer paired.
fn forget_others(ids: &BTreeSet<String>, shade: &dyn Shade, watcher: u64) {
    let forgot = with_watched_book(watcher, |book| {
        book.computers.retain(|id, _| ids.contains(id));
        for tag in shade.shown() {
            if let Some((profile, _)) = tag.split_once('/')
                && !ids.contains(profile)
            {
                shade.cancel(&tag);
            }
        }
    });
    if let Err(error) = forgot {
        tracing::warn!(%error, "alerts: could not forget unpaired computers");
    }
}

/// The computer no longer accepts this phone, so it cannot say when its
/// alerts are answered: they go, and their task titles with them.
fn refused(profile_id: &str, shade: &dyn Shade, watcher: u64) {
    let forgot = with_watched_book(watcher, |book| {
        book.computers.remove(profile_id);
        let prefix = format!("{profile_id}/");
        for tag in shade.shown() {
            if tag.starts_with(&prefix) {
                shade.cancel(&tag);
            }
        }
    });
    if let Err(error) = forgot {
        tracing::warn!(%error, "alerts: could not forget a computer that refused this phone");
    }
}

enum Ended {
    Closed,
    /// The network changed: connect again now.
    Moved,
}

async fn watch_computer(
    record: Record,
    shade: Arc<dyn Shade>,
    vault: Arc<dyn KeyVault>,
    mut wake: Wake,
) {
    let computer = Computer {
        profile_id: record.profile.profile_id.clone(),
        name: record.profile.name.clone(),
    };
    let stop = wake.stop.clone();
    set_status(&stop, &computer.profile_id, Status::Connecting);
    let began = with_watched_book(wake.watcher, |book| {
        book.begin(&computer.profile_id, Timestamp::now());
    });
    if let Err(error) = began {
        tracing::warn!(%error, "alerts: could not save when watching began");
    }
    let mut failures = 0usize;
    loop {
        let ended = session(
            &record,
            &computer,
            shade.as_ref(),
            vault.as_ref(),
            &mut wake,
            &mut failures,
        )
        .await;
        if let Err(error) = connection_ended(&computer.profile_id, wake.watcher) {
            tracing::warn!(%error, "alerts: could not note a connection ended");
        }
        if stop.is_cancelled() {
            return;
        }
        let Some((status, wait)) = after(&ended, &mut failures) else {
            continue;
        };
        if status == Status::NotAllowed {
            refused(&computer.profile_id, shade.as_ref(), wake.watcher);
        }
        set_status(&stop, &computer.profile_id, status);
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = wake.nudge.changed() => failures = 0,
            _ = wake.retry.changed() => failures = 0,
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// What follows the end of a connection: the status to show and the wait
/// before trying again, or none to connect again now.
fn after(ended: &Result<Ended>, failures: &mut usize) -> Option<(Status, Duration)> {
    Some(match ended {
        // A new network starts the waits again, whether the move ended a
        // connection or an attempt to sign in.
        Ok(Ended::Moved) => {
            *failures = 0;
            return None;
        }
        Err(Error::Incompatible) => (Status::UpdateNeeded, IDLE),
        Err(Error::Remote(e)) if e.code == ErrorCode::IncompatibleVersion => {
            (Status::UpdateNeeded, IDLE)
        }
        Err(Error::Unauthenticated) => (Status::NotAllowed, IDLE),
        Err(Error::Remote(e)) if e.code == ErrorCode::Unauthenticated => (Status::NotAllowed, IDLE),
        // The phone's grants changed: sign in again with the new ones.
        Err(Error::Remote(e)) if e.code == ErrorCode::PermissionDenied => {
            (Status::Connecting, RETRY[0])
        }
        Ok(Ended::Closed) | Err(_) => {
            let wait = RETRY[(*failures).min(RETRY.len() - 1)];
            *failures += 1;
            (Status::Unreachable, wait)
        }
    })
}

/// The connection to a computer ended, unless the watcher that made it
/// was stopped meanwhile: its replacement's may be live.
fn connection_ended(profile_id: &str, watcher: u64) -> Result<()> {
    with_book(|book| {
        if current(watcher) {
            book.ended(profile_id);
        }
    })
}

/// One connection: watch until it ends. `failures` is cleared once the
/// computer lists the open alerts.
async fn session(
    record: &Record,
    computer: &Computer,
    shade: &dyn Shade,
    vault: &dyn KeyVault,
    wake: &mut Wake,
    failures: &mut usize,
) -> Result<Ended> {
    let (mut ws, _, signed_in) = tokio::select! {
        _ = wake.stop.cancelled() => return Ok(Ended::Closed),
        _ = wake.nudge.changed() => return Ok(Ended::Moved),
        signed_in = network::authenticate(record, ConnectionKind::Alerts, vault, || {}) => signed_in?,
    };
    if let Some(offers) = &signed_in.capabilities.app_updates {
        shade.app_updates(&computer.profile_id, &computer.name, offers);
    }
    let showing = with_watched_book(wake.watcher, |book| {
        book.showing(&computer.profile_id, &shade.shown())
    })?;
    let Some(showing) = showing else {
        return Ok(Ended::Closed);
    };
    // Alerts the person turned off still come, marked, so they count as
    // raised here and turning them on later raises none of them.
    let reports_supported = signed_in
        .capabilities
        .features
        .iter()
        .any(|f| f == "reports_v1");
    let reports_on = reports_supported && report_alerts_on(record, vault).await;
    let watch = AlertClientFrame::Watch {
        reports: reports_on,
        showing,
        quiet: true,
    };
    network::send_json(&mut ws, &watch).await?;
    let heard = network::heard(&ws);
    let mut again = None;
    // The first list answers this session's Watch, whose report setting was
    // just read; a later one follows a settings change.
    let mut listed = false;
    loop {
        let quiet = SILENCE.saturating_sub(heard.silent_for());
        if quiet.is_zero() {
            return Err(Error::Timeout);
        }
        let message = tokio::select! {
            _ = wake.stop.cancelled() => {
                let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
                return Ok(Ended::Closed);
            }
            _ = wake.nudge.changed() => return Ok(Ended::Moved),
            () = tokio::time::sleep(again.unwrap_or_default()), if again.is_some() => {
                again = look(computer, shade, wake.watcher).await;
                continue;
            }
            next = tokio::time::timeout(quiet, ws.next()) => match next {
                Err(_) => continue,
                Ok(None) => return Ok(Ended::Closed),
                Ok(Some(message)) => message.map_err(|_| Error::Closed)?,
            },
        };
        // Pings are answered by the socket itself on its next read.
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => return Ok(Ended::Closed),
            _ => continue,
        };
        let Some(frame) = parse(&text)? else { continue };
        let opened = matches!(frame, AlertServerFrame::Open { .. });
        if opened
            && std::mem::replace(&mut listed, true)
            && reports_supported
            && report_alerts_on(record, vault).await != reports_on
        {
            return Ok(Ended::Moved);
        }
        again = deliver(computer, frame, shade, wake.watcher).await;
        if opened {
            *failures = 0;
            set_status(&wake.stop, &computer.profile_id, Status::Watching);
        }
    }
}

fn report_alerts_enabled(features: &[String], off: &[AlertTopic]) -> bool {
    features.iter().any(|f| f == "reports_v1") && !off.contains(&AlertTopic::Reports)
}

// The watcher has no screen store. Read the computer’s saved topic before
// opting in, and again when its settings cause a fresh open list.
async fn report_alerts_on(record: &Record, vault: &dyn KeyVault) -> bool {
    tokio::time::timeout(Duration::from_secs(8), async {
        let (mut ws, _, signed_in) =
            network::authenticate(record, ConnectionKind::Control, vault, || {}).await?;
        if !signed_in
            .capabilities
            .features
            .iter()
            .any(|f| f == "notification_settings_v1")
        {
            return Ok::<bool, Error>(true);
        }
        let query_id = QueryId::new();
        network::send_json(
            &mut ws,
            &ClientFrame::Query(QueryEnvelope {
                query_id,
                query: Query::NotificationSettings,
            }),
        )
        .await?;
        let on = loop {
            let message = ws
                .next()
                .await
                .ok_or(Error::Closed)?
                .map_err(|_| Error::Closed)?;
            if let Message::Text(text) = message {
                if text.len() > network::MAX_TEXT {
                    return Err(Error::Invalid("The computer sent oversized settings."));
                }
                match serde_json::from_str::<ServerFrame>(&text) {
                    Ok(ServerFrame::QueryResult(result)) if result.query_id == query_id => {
                        match result.result {
                            Ok(QueryData::NotificationSettings { view }) => {
                                break report_alerts_enabled(
                                    &signed_in.capabilities.features,
                                    &view.off,
                                );
                            }
                            _ => {
                                return Err(Error::Invalid(
                                    "Report alert settings couldn’t be read.",
                                ));
                            }
                        }
                    }
                    Ok(ServerFrame::Fatal(error)) => return Err(Error::Remote(error)),
                    _ => {}
                }
            } else if matches!(message, Message::Close(_)) {
                return Err(Error::Closed);
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
        Ok(on)
    })
    .await
    .ok()
    .and_then(std::result::Result::ok)
    .unwrap_or(false)
}

/// None for a frame this app does not know, from a newer computer.
fn parse(text: &str) -> Result<Option<AlertServerFrame>> {
    if text.len() > network::MAX_TEXT {
        return Err(Error::Invalid("The computer sent an oversized alert."));
    }
    if let Ok(frame) = serde_json::from_str(text) {
        return Ok(Some(frame));
    }
    match serde_json::from_str(text) {
        Ok(ServerFrame::Fatal(error)) => Err(Error::Remote(error)),
        _ => Ok(None),
    }
}

/// Raises and clears what a frame changes. Returns when to look again, if
/// alerts wait for the app to leave the screen or for room in the shade.
async fn deliver(
    computer: &Computer,
    frame: AlertServerFrame,
    shade: &dyn Shade,
    watcher: u64,
) -> Option<Duration> {
    let in_app = in_app(shade).await;
    let here = in_app.contains(&computer.profile_id);
    let at = Timestamp::now();
    let changes =
        |book: &mut Book, shown: &[String]| receive(book, computer, frame, here, shown, at);
    update(watcher, shade, &in_app, changes, |book| {
        look_again(book, computer)
    })
    .flatten()
}

/// Raises what waits once the app's screen no longer shows the computer,
/// and what waits for room once there is some. Returns when to look again.
async fn look(computer: &Computer, shade: &dyn Shade, watcher: u64) -> Option<Duration> {
    let in_app = in_app(shade).await;
    let changes = |book: &mut Book, _: &[String]| {
        if in_app.contains(&computer.profile_id) {
            Changes::default()
        } else {
            unwatched(book, computer)
        }
    };
    update(watcher, shade, &in_app, changes, |book| {
        look_again(book, computer)
    })
    .flatten()
}

fn look_again(book: &Book, computer: &Computer) -> Option<Duration> {
    if book.waiting(&computer.profile_id) {
        Some(LOOK)
    } else {
        book.holding().then_some(ROOM)
    }
}

/// Changes the book with `f`, given the tags showing now, then clears and
/// raises what it returns and saves the book, all under the ledger's lock:
/// a save or a change of settings in between would otherwise record an
/// alert never raised, or leave one raised after alerts were turned off.
/// An alert Android did not show is forgotten, so the next open list raises
/// it again, as is one whose saving the app's end cut short, or one the app
/// cleared, as answered, since the computer last said it was open; one it
/// had no room for after all waits for room. Each is written out now, with
/// the detail chosen now. `in_app` are the computers the app's screen shows.
/// Returns what `then` reads from the book after, or None if `watcher` was
/// stopped, which changes nothing.
fn update<T>(
    watcher: u64,
    shade: &dyn Shade,
    in_app: &BTreeSet<String>,
    f: impl FnOnce(&mut Book, &[String]) -> Changes,
    then: impl FnOnce(&Book) -> T,
) -> Option<T> {
    let mut guard = lock(&LEDGER);
    let ledger = guard.as_mut()?;
    if !current(watcher) {
        return None;
    }
    // The app clears alerts on its own, as answered: one it cleared after
    // the computer last said it was open is not raised.
    ledger.book.clears = shade.clears();
    let mut shown = shade.shown();
    let changes = f(&mut ledger.book, &shown);
    for tag in &changes.cancel {
        shade.cancel(tag);
    }
    shown.retain(|t| !changes.cancel.contains(t));
    let post = ledger.book.fit(changes.post, &shown, in_app);
    let detail = ledger.book.detail();
    let mut unshown = Vec::new();
    let mut no_room = Vec::new();
    for raise in post {
        let raise = match fold(&mut ledger.book, shade, raise, &mut shown) {
            Folding::Alone(raise) => raise,
            Folding::Shown => continue,
            Folding::NoRoom(raise) => {
                no_room.push(raise);
                continue;
            }
            Folding::Failed(raise) => {
                unshown.push(raise.tag());
                continue;
            }
        };
        let Some(note) = raise.note(detail) else {
            continue;
        };
        match shade.post(&note, Some(raise.since)) {
            Ok(Posted::Shown) => shown.push(note.tag),
            Ok(Posted::NoRoom) => no_room.push(raise),
            Ok(Posted::Cleared) => unshown.push(note.tag),
            Err(error) => {
                tracing::warn!(%error, "alerts: Android did not show an alert");
                unshown.push(note.tag);
            }
        }
    }
    ledger.book.unraise(&unshown);
    ledger.book.hold(no_room, true);
    recount(&mut ledger.book, shade, &shown);
    match ledger.book.more() {
        // The note is the watcher's own; the app never clears it.
        Some(Ok(note)) => {
            if let Err(error) = shade.post(&note, None) {
                tracing::warn!(%error, "alerts: Android did not show how many alerts wait");
            }
        }
        Some(Err(tag)) => shade.cancel(tag),
        None => {}
    }
    if let Err(error) = ledger.save() {
        tracing::warn!(%error, "alerts: could not save which alerts were shown");
    }
    Some(then(&ledger.book))
}

enum Folding {
    /// It shows on its own.
    Alone(Raise),
    /// Its project's note counts it.
    Shown,
    NoRoom(Raise),
    Failed(Raise),
}

/// Where its project's alerts show as one, `raise` shows on its own if it
/// is the first; from the second, one note counts them in place of each.
/// The note alerts again for one that needs the person, and otherwise
/// changes its count quietly. One the person swiped away counts no longer.
/// `shown` is the tags showing, kept as they change.
fn fold(book: &mut Book, shade: &dyn Shade, raise: Raise, shown: &mut Vec<String>) -> Folding {
    if !raise.alert.bundle {
        return Folding::Alone(raise);
    }
    let profile_id = raise.computer.profile_id.clone();
    let Some(seen) = book.computers.get_mut(&profile_id) else {
        return Folding::Alone(raise);
    };
    let (id, project_id) = (raise.alert.attention_id, raise.alert.project_id);
    let needs_you = heading(raise.alert.kind).is_some_and(|(c, _)| c == Category::NeedsYou);
    let note_tag = folded_tag(&profile_id, &project_id);
    let fold = seen.folds.entry(project_id).or_insert_with(|| Fold {
        project: String::new(),
        computer: String::new(),
        members: BTreeMap::new(),
        folded: false,
        counted: 0,
    });
    fold.project.clone_from(&raise.alert.project);
    fold.computer.clone_from(&raise.computer.name);
    if fold.folded && !shown.contains(&note_tag) {
        fold.members.clear();
        fold.folded = false;
    }
    let folded = fold.folded;
    let raised = &seen.raised;
    fold.members.retain(|m, _| {
        *m != id && raised.contains(m) && (folded || shown.contains(&tag(&profile_id, m)))
    });
    if fold.members.is_empty() {
        fold.folded = false;
        fold.members.insert(id, needs_you);
        return Folding::Alone(raise);
    }
    fold.members.insert(id, needs_you);
    let note = fold.note(&profile_id, &project_id);
    // Posted again, a note changes quietly: one that needs the person
    // alerts as a new one would.
    if folded && needs_you {
        shade.cancel(&note_tag);
        shown.retain(|t| *t != note_tag);
    }
    match shade.post(&note, None) {
        Ok(Posted::Shown) => {
            if !folded {
                for m in fold.members.keys().filter(|m| **m != id) {
                    let alone = tag(&profile_id, m);
                    shade.cancel(&alone);
                    shown.retain(|t| *t != alone);
                }
            }
            fold.folded = true;
            fold.counted = fold.members.len();
            if !shown.contains(&note_tag) {
                shown.push(note_tag);
            }
            Folding::Shown
        }
        Ok(Posted::NoRoom) => {
            fold.members.remove(&id);
            Folding::NoRoom(raise)
        }
        Ok(Posted::Cleared) | Err(_) => {
            fold.members.remove(&id);
            Folding::Failed(raise)
        }
    }
}

/// Each project's note counts what is still raised here: one answered or
/// turned off leaves the count, quietly, and the note goes with the last.
/// A note the person swiped away, or the first alert's own, counts none
/// from then on.
fn recount(book: &mut Book, shade: &dyn Shade, shown: &[String]) {
    for (profile_id, seen) in &mut book.computers {
        let raised = &seen.raised;
        seen.folds.retain(|project_id, fold| {
            fold.members.retain(|m, _| raised.contains(m));
            let note_tag = folded_tag(profile_id, project_id);
            if fold.members.is_empty() {
                if fold.folded {
                    shade.cancel(&note_tag);
                }
                return false;
            }
            if !fold.folded {
                return (fold.members.keys()).any(|m| shown.contains(&tag(profile_id, m)));
            }
            if !shown.contains(&note_tag) {
                return false;
            }
            if fold.counted != fold.members.len() {
                fold.counted = fold.members.len();
                if let Err(error) = shade.post(&fold.note(profile_id, project_id), None) {
                    tracing::warn!(%error, "alerts: Android did not show a project's new count");
                }
            }
            true
        });
    }
}

/// The computer whose alerts the app's screen shows, while it shows them.
static VIEWING: Mutex<Option<String>> = Mutex::new(None);

/// The app's screen began, `on`, or stopped showing the alerts of a
/// computer. A connection the app keeps to another computer, as for an
/// unfinished hand-off, shows none of that one's alerts.
pub fn viewing(profile_id: &str, on: bool) {
    let mut viewing = lock(&VIEWING);
    if on {
        *viewing = Some(profile_id.to_owned());
    } else if viewing.as_deref() == Some(profile_id) {
        *viewing = None;
    }
}

/// The computer the app's screen is in front, unlocked, showing and
/// connected to, and so shows the alerts of itself.
async fn in_app(shade: &dyn Shade) -> BTreeSet<String> {
    let mut showing = BTreeSet::new();
    if !shade.on_screen() {
        return showing;
    }
    let Some(id) = lock(&VIEWING).clone() else {
        return showing;
    };
    if app_connected(&id).await {
        showing.insert(id);
    }
    showing
}

async fn app_connected(profile_id: &str) -> bool {
    match APP.get().and_then(Weak::upgrade) {
        Some(links) => links.control_online(profile_id).await,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{ProjectId, TaskId};

    fn computer() -> Computer {
        Computer {
            profile_id: "p1".into(),
            name: "Studio".into(),
        }
    }
    fn alert(kind: AttentionKind, at: i64) -> Alert {
        Alert {
            report: None,
            attention_id: AttentionId::new(),
            kind,
            project_id: ProjectId::new(),
            project: "Site".into(),
            task_id: TaskId::new(),
            task: "Fix the header".into(),
            harness: HarnessKind::Codex,
            created_at: Timestamp(at),
            headline: None,
            question: None,
            topic: None,
            quiet: false,
            bundle: false,
        }
    }
    fn open(alerts: &[Alert], now: i64) -> AlertServerFrame {
        open_gone(alerts, now, Vec::new())
    }
    /// The open list on a reconnect, naming those shown that closed meanwhile.
    fn open_gone(alerts: &[Alert], now: i64, gone: Vec<AttentionId>) -> AlertServerFrame {
        AlertServerFrame::Open {
            alerts: alerts.to_vec(),
            now: Timestamp(now),
            gone,
        }
    }
    fn tags(changes: &Changes) -> Vec<String> {
        changes.post.iter().map(Raise::tag).collect()
    }
    /// A frame received when this phone's clock reads 0.
    fn rx(book: &mut Book, frame: AlertServerFrame, in_app: bool, shown: &[String]) -> Changes {
        receive(book, &computer(), frame, in_app, shown, Timestamp(0))
    }
    fn current_watcher() -> u64 {
        WATCHER.load(Ordering::SeqCst)
    }
    /// Held by each test that uses the shared ledger.
    static ONE_LEDGER: Mutex<()> = Mutex::new(());
    fn ledger(dir: &Path) -> MutexGuard<'static, ()> {
        let one = lock(&ONE_LEDGER);
        *lock(&LEDGER) = Some(Ledger {
            path: dir.join(FILE),
            book: Book::default(),
            saved: Vec::new(),
        });
        one
    }
    const ON: Settings = Settings {
        enabled: true,
        detail: Detail::Minimal,
    };
    /// Android's shade, refusing alerts while `refuse` is set and without
    /// room for more while `full` is, as when the app raised its own.
    #[derive(Default)]
    struct FakeShade {
        shown: Mutex<Vec<String>>,
        posted: Mutex<Vec<Note>>,
        refuse: std::sync::atomic::AtomicBool,
        full: std::sync::atomic::AtomicBool,
        /// Each tag cleared, in turn: its count is its place, from 1.
        cleared: Mutex<Vec<String>>,
        /// A tag the app clears as the watcher posts it.
        racing: Mutex<Option<String>>,
    }
    impl Shade for FakeShade {
        fn post(&self, note: &Note, since: Option<i64>) -> Result<Posted> {
            // Nothing saves the book, or turns alerts off, as one is raised.
            assert!(LEDGER.try_lock().is_err());
            if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(Error::Native("blocked".into()));
            }
            if lock(&self.racing).as_ref() == Some(&note.tag) {
                self.cancel(&note.tag);
            }
            if let Some(since) = since
                && (lock(&self.cleared).iter().enumerate())
                    .any(|(n, t)| *t == note.tag && n as i64 + 1 > since)
            {
                return Ok(Posted::Cleared);
            }
            let mut shown = lock(&self.shown);
            if note.tag != MORE
                && !shown.contains(&note.tag)
                && self.full.load(std::sync::atomic::Ordering::SeqCst)
            {
                return Ok(Posted::NoRoom);
            }
            if !shown.contains(&note.tag) {
                shown.push(note.tag.clone());
            }
            lock(&self.posted).push(note.clone());
            Ok(Posted::Shown)
        }
        fn cancel(&self, tag: &str) {
            lock(&self.cleared).push(tag.to_owned());
            lock(&self.shown).retain(|t| t != tag);
        }
        fn clears(&self) -> i64 {
            lock(&self.cleared).len() as i64
        }
        fn shown(&self) -> Vec<String> {
            lock(&self.shown).clone()
        }
        fn on_screen(&self) -> bool {
            false
        }
    }
    fn saved(dir: &Path) -> Book {
        serde_json::from_slice(&fs::read(dir.join(FILE)).unwrap()).unwrap()
    }

    #[test]
    fn watch_reports_only_when_supported_and_the_topic_is_on() {
        assert!(!report_alerts_enabled(&[], &[]));
        let features = vec!["reports_v1".into()];
        assert!(report_alerts_enabled(&features, &[]));
        assert!(!report_alerts_enabled(&features, &[AlertTopic::Reports]));
        assert!(report_alerts_enabled(&features, &[AlertTopic::Finished]));
    }

    #[test]
    fn report_alerts_open_the_report_without_task_content_or_reply_actions() {
        let computer = Computer {
            profile_id: "phone".into(),
            name: "Desk".into(),
        };
        let mut alert = alert(AttentionKind::Completed, 1);
        let report_id = protocol::ReportId::new();
        alert.report = Some(Box::new(protocol::reports::ReportAlert {
            report_id,
            reference: "A7K3QX".into(),
            status: protocol::reports::ReportProgress::Reproduced,
            question: None,
        }));
        let note = note(&computer, &alert, Detail::TaskTitle).unwrap();
        assert_eq!(note.title, "Report A7K3QX");
        assert_eq!(
            note.route,
            serde_json::json!({ "kind": "report", "profileId": "phone", "reportId": report_id })
        );
        assert!(note.question.is_none());
        assert!(!note.body.contains(&alert.task));
    }

    #[test]
    fn an_alert_about_no_task_names_none() {
        let mut alert = alert(AttentionKind::WaitingForYou, 1);
        alert.project = "Problem reports".into();
        alert.task = String::new();
        alert.headline = Some("New problem report A7K3QX".into());
        let note = note(&computer(), &alert, Detail::TaskTitle).unwrap();
        assert_eq!(note.title, "New problem report A7K3QX");
        assert!(note.body.ends_with("Problem reports"), "{}", note.body);
    }

    #[test]
    fn an_alert_is_raised_and_saved_under_one_lock_so_turning_off_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let frame = |frame: AlertServerFrame| {
            let none = BTreeSet::new();
            update(
                current_watcher(),
                &shade,
                &none,
                |book, shown| rx(book, frame, false, shown),
                |_| (),
            )
            .unwrap();
        };
        frame(open(&[], 1));
        let (shown, refused) = (
            alert(AttentionKind::Question, 2),
            alert(AttentionKind::Approval, 3),
        );
        frame(AlertServerFrame::Raised {
            alert: shown.clone(),
        });
        shade
            .refuse
            .store(true, std::sync::atomic::Ordering::SeqCst);
        frame(AlertServerFrame::Raised {
            alert: refused.clone(),
        });
        // Saved as raised only once Android showed it.
        let book = saved(dir.path());
        assert!(book.computers["p1"].raised.contains(&shown.attention_id));
        assert!(
            !book.computers["p1"]
                .notified
                .contains(&refused.attention_id)
        );
        let off = configure(Settings {
            enabled: false,
            ..ON
        })
        .unwrap();
        assert_eq!(off, vec![MORE.to_owned(), tag("p1", &shown.attention_id)]);
        // Off, a frame on its way raises nothing.
        shade
            .refuse
            .store(false, std::sync::atomic::Ordering::SeqCst);
        frame(AlertServerFrame::Raised {
            alert: alert(AttentionKind::Question, 4),
        });
        assert_eq!(shade.shown(), vec![tag("p1", &shown.attention_id)]);
        lock(&LEDGER).take();
    }

    #[test]
    fn alerts_turned_off_count_as_raised_and_never_show() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let frame = |frame: AlertServerFrame, in_app: bool| {
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, frame, in_app, shown),
                |_| (),
            )
            .unwrap();
        };
        let quiet = |a: &Alert| Alert {
            quiet: true,
            ..a.clone()
        };
        let (merged, question) = (
            alert(AttentionKind::Completed, 2),
            alert(AttentionKind::Question, 3),
        );
        frame(open(&[quiet(&merged)], 1), false);
        frame(
            AlertServerFrame::Raised {
                alert: question.clone(),
            },
            false,
        );
        assert_eq!(shade.shown(), vec![tag("p1", &question.attention_id)]);
        let seen = || lock(&LEDGER).as_ref().unwrap().book.computers["p1"].clone();
        assert!(seen().notified.contains(&merged.attention_id));
        assert!(!seen().raised.contains(&merged.attention_id));
        // Questions turned off too: the one showing goes, as does one that
        // waited for the app to leave the screen.
        let waiting = alert(AttentionKind::Question, 4);
        frame(
            AlertServerFrame::Raised {
                alert: waiting.clone(),
            },
            true,
        );
        frame(
            open(&[quiet(&merged), quiet(&question), quiet(&waiting)], 5),
            false,
        );
        assert!(shade.shown().is_empty());
        assert!(seen().waiting.is_empty());
        assert!(seen().raised.is_empty());
        // Turned on again, those it held back stay unraised.
        frame(open(&[merged.clone(), question, waiting], 6), false);
        assert!(shade.shown().is_empty());
        assert!(lock(&shade.posted).len() == 1);
        // One turned off while the app shows the computer never waits.
        let later = quiet(&alert(AttentionKind::Approval, 7));
        frame(AlertServerFrame::Raised { alert: later }, true);
        assert!(seen().waiting.is_empty());
        lock(&LEDGER).take();
    }

    #[test]
    fn a_projects_alerts_fold_into_one_note_that_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, frame, false, shown),
                |_| (),
            )
            .unwrap();
        };
        let project = ProjectId::new();
        let folds = |kind, at| Alert {
            project_id: project,
            bundle: true,
            ..alert(kind, at)
        };
        let raise = |a: &Alert| {
            frame(AlertServerFrame::Raised { alert: a.clone() });
        };
        let note_tag = folded_tag("p1", &project);
        let last = || lock(&shade.posted).last().unwrap().clone();
        let cleared = |t: &str| lock(&shade.cleared).iter().filter(|c| *c == t).count();
        frame(open(&[], 1));
        // The first shows on its own.
        let first = folds(AttentionKind::Completed, 2);
        raise(&first);
        assert_eq!(shade.shown(), vec![tag("p1", &first.attention_id)]);
        // The second folds both into one note, in place of the first.
        let second = folds(AttentionKind::Failure, 3);
        raise(&second);
        assert_eq!(shade.shown(), vec![note_tag.clone()]);
        let note = last();
        assert_eq!(note.title, "Site has 2 notifications");
        assert_eq!(note.body, "Studio · 2 to look at");
        assert_eq!(note.category, Category::Finished);
        assert_eq!(
            note.route,
            serde_json::json!({ "kind": "needs_you", "profileId": "p1", "projectId": project })
        );
        assert_eq!(cleared(&tag("p1", &first.attention_id)), 1);
        // One that needs the person alerts again; another finished counts quietly.
        let question = folds(AttentionKind::Question, 4);
        raise(&question);
        assert_eq!(cleared(&note_tag), 1);
        assert_eq!(last().title, "Site has 3 notifications");
        assert_eq!(last().body, "Studio · 1 need you, 2 to look at");
        assert_eq!(last().category, Category::NeedsYou);
        let fourth = folds(AttentionKind::Completed, 5);
        raise(&fourth);
        assert_eq!(cleared(&note_tag), 1);
        assert_eq!(last().title, "Site has 4 notifications");
        // Another project's alert, or one not folded, shows on its own.
        let elsewhere = alert(AttentionKind::Completed, 6);
        raise(&elsewhere);
        assert_eq!(
            shade.shown(),
            vec![note_tag.clone(), tag("p1", &elsewhere.attention_id)]
        );
        // Answered, they leave the count, and the note goes with the last.
        frame(AlertServerFrame::Resolved {
            attention_id: question.attention_id,
        });
        assert_eq!(last().title, "Site has 3 notifications");
        assert_eq!(last().category, Category::Finished);
        let posts = lock(&shade.posted).len();
        frame(open_gone(
            &[elsewhere.clone(), fourth.clone()],
            7,
            vec![first.attention_id, second.attention_id],
        ));
        assert_eq!(last().title, "Site has 1 notification");
        frame(AlertServerFrame::Resolved {
            attention_id: fourth.attention_id,
        });
        assert_eq!(lock(&shade.posted).len(), posts + 1);
        assert_eq!(shade.shown(), vec![tag("p1", &elsewhere.attention_id)]);
        // Swiped away, a note counts none from then on.
        let (a, b, c) = (
            folds(AttentionKind::Completed, 8),
            folds(AttentionKind::Completed, 9),
            folds(AttentionKind::Completed, 10),
        );
        raise(&a);
        raise(&b);
        assert!(shade.shown().contains(&note_tag));
        shade.cancel(&note_tag);
        raise(&c);
        assert!(shade.shown().contains(&tag("p1", &c.attention_id)));
        assert!(!shade.shown().contains(&note_tag));
        // Off, the note goes with every alert raised.
        raise(&folds(AttentionKind::Completed, 11));
        let off = configure(Settings {
            enabled: false,
            ..ON
        })
        .unwrap();
        assert!(off.contains(&note_tag));
        lock(&LEDGER).take();
    }

    #[test]
    fn a_stopped_watcher_leaves_the_book_and_shade_to_its_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let frame = |watcher: u64, frame: AlertServerFrame| {
            update(
                watcher,
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, frame, false, shown),
                |_| (),
            )
        };
        // Alerts are switched off and on: the first watcher's connection
        // is still closing as the second's lists what is open.
        let old = next_watcher();
        let new = next_watcher();
        assert!(frame(new, open(&[], 1)).is_some());
        let live = || lock(&LEDGER).as_ref().unwrap().book.computers["p1"].live;
        assert!(live());
        connection_ended("p1", old).unwrap();
        assert!(live());
        // Nor does what the old one still reads raise anything.
        let late = alert(AttentionKind::Question, 2);
        assert!(frame(old, AlertServerFrame::Raised { alert: late }).is_none());
        assert!(shade.shown().is_empty());
        connection_ended("p1", new).unwrap();
        assert!(!live());
        lock(&LEDGER).take();
    }

    #[test]
    fn alerts_beyond_the_shades_room_wait_unrecorded_and_rise_as_it_frees() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, frame, false, shown),
                |b| b.holding(),
            )
            .unwrap()
        };
        frame(open(&[], 1));
        let alerts: Vec<Alert> = (0..MAX_SHOWN as i64 + 3)
            .map(|i| alert(AttentionKind::Completed, 10 + i))
            .collect();
        let tags_of = |alerts: &[Alert]| -> Vec<String> {
            alerts.iter().map(|a| tag("p1", &a.attention_id)).collect()
        };
        assert!(frame(open(&alerts, 100)));
        // The oldest fill the shade; a note counts the rest.
        let mut expected = tags_of(&alerts[..MAX_SHOWN]);
        expected.push(MORE.into());
        assert_eq!(shade.shown(), expected);
        assert_eq!(saved(dir.path()).computers["p1"].notified.len(), MAX_SHOWN);
        // The list again raises none of them twice.
        assert!(frame(open(&alerts, 110)));
        assert_eq!(shade.shown(), expected);
        // Two answered make room for the two waiting longest.
        for a in &alerts[..2] {
            frame(AlertServerFrame::Resolved {
                attention_id: a.attention_id,
            });
        }
        let mut expected = tags_of(&alerts[2..MAX_SHOWN + 2]);
        expected.insert(MAX_SHOWN - 2, MORE.into());
        assert_eq!(shade.shown(), expected);
        // A restart forgets the last one waiting, which the next list raises.
        let restarted = saved(dir.path());
        assert!(!restarted.computers["p1"].knows(&alerts[MAX_SHOWN + 2].attention_id));
        // Answered while it waits, it goes, and the note with it.
        let still_open = [&alerts[2..MAX_SHOWN + 2]].concat();
        assert!(!frame(open(&still_open, 120)));
        expected.retain(|t| t != MORE);
        assert_eq!(shade.shown(), expected);
        lock(&LEDGER).take();
    }

    #[test]
    fn the_note_of_those_waiting_counts_them_and_alerts_when_one_needs_you() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let full: Vec<String> = (0..MAX_SHOWN).map(|i| format!("p1/{i}")).collect();
        let waiting = [
            alert(AttentionKind::Completed, 2),
            alert(AttentionKind::Approval, 3),
        ];
        let mut posted = Vec::new();
        for a in &waiting {
            let frame = AlertServerFrame::Raised { alert: a.clone() };
            let changes = rx(&mut book, frame, false, &full);
            posted.extend(book.fit(changes.post, &full, &BTreeSet::new()));
        }
        assert!(posted.is_empty());
        let Some(Ok(note)) = book.more() else {
            panic!("a note of those waiting")
        };
        assert_eq!(note.title, "2 more alerts");
        assert_eq!(note.category, Category::NeedsYou);
        assert_eq!(note.route, serde_json::Value::Null);
        // Unchanged, it is not posted again.
        assert!(book.more().is_none());
        // Turned off, it is cleared with the rest.
        let off = book.configure(Settings {
            enabled: false,
            ..ON
        });
        assert_eq!(off, vec![MORE.to_owned()]);
        // And so even when this process counted none.
        let mut fresh = Book::default();
        assert_eq!(
            fresh.configure(Settings {
                enabled: false,
                ..ON
            }),
            vec![MORE.to_owned()]
        );
    }

    #[test]
    fn what_waits_for_room_waits_while_the_app_shows_it_and_goes_once_it_reports_it_seen() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let full: Vec<String> = (0..MAX_SHOWN).map(|i| format!("p9/{i}")).collect();
        let (question, done) = (
            alert(AttentionKind::Question, 2),
            alert(AttentionKind::Completed, 3),
        );
        for a in [&question, &done] {
            let changes = rx(
                &mut book,
                AlertServerFrame::Raised { alert: a.clone() },
                false,
                &full,
            );
            assert!(book.fit(changes.post, &full, &BTreeSet::new()).is_empty());
        }
        // Room frees while the app shows the computer: nothing rises.
        let in_app = BTreeSet::from(["p1".to_owned()]);
        assert!(book.fit(Vec::new(), &[], &in_app).is_empty());
        // The app shows the question; once it leaves, only the other rises.
        book.seen_in_app("p1", &[question.attention_id]);
        let posted = book.fit(Vec::new(), &[], &BTreeSet::new());
        assert_eq!(
            posted.iter().map(Raise::tag).collect::<Vec<_>>(),
            vec![tag("p1", &done.attention_id)]
        );
        assert!(!book.holding());
    }

    #[test]
    fn the_watch_names_what_the_phone_shows_or_keeps_from_that_computer() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let raised = alert(AttentionKind::Question, 2);
        rx(
            &mut book,
            AlertServerFrame::Raised {
                alert: raised.clone(),
            },
            false,
            &[],
        );
        let full: Vec<String> = (0..MAX_SHOWN).map(|i| format!("p9/{i}")).collect();
        let held = alert(AttentionKind::Completed, 3);
        let changes = rx(
            &mut book,
            AlertServerFrame::Raised {
                alert: held.clone(),
            },
            false,
            &full,
        );
        book.fit(changes.post, &full, &BTreeSet::new());
        let by_app = AttentionId::new();
        let shown = vec![
            tag("p1", &by_app),
            tag("p1", &raised.attention_id),
            tag("p2", &AttentionId::new()),
            MORE.to_owned(),
        ];
        assert_eq!(
            book.showing("p1", &shown),
            vec![by_app, raised.attention_id, held.attention_id]
        );
        // The app's own alert, raised while the watcher was away, is
        // recorded as the watcher's: a cleared shade raises it again.
        book.raised_by_app("p1", by_app);
        assert!(book.computers["p1"].raised.contains(&by_app));
        book.started("a");
        book.started("b");
        assert!(!book.computers["p1"].knows(&by_app));
        // At most so many.
        let many: Vec<String> = (0..MAX_SHOWING + 5)
            .map(|_| tag("p1", &AttentionId::new()))
            .collect();
        assert_eq!(book.showing("p1", &many).len(), MAX_SHOWING);
    }

    #[test]
    fn what_waits_for_room_rises_only_once_the_computer_says_what_was_answered_while_away() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let none = BTreeSet::new();
        let full: Vec<String> = (0..MAX_SHOWN).map(|i| format!("p9/{i}")).collect();
        let approval = alert(AttentionKind::Approval, 2);
        let frame = AlertServerFrame::Raised {
            alert: approval.clone(),
        };
        let changes = rx(&mut book, frame, false, &full);
        assert!(book.fit(changes.post, &full, &none).is_empty());
        book.ended("p1");
        // Room frees, as another computer's alerts are answered, while this
        // one is away: what it keeps waits.
        assert!(book.fit(Vec::new(), &[], &none).is_empty());
        assert!(!book.holding());
        assert_eq!(book.showing("p1", &[]), vec![approval.attention_id]);
        // Back, behind a full list, the computer says it was answered: it
        // goes before anything rises.
        let others: Vec<Alert> = (0..MAX_OPEN_ALERTS as i64)
            .map(|i| alert(AttentionKind::Completed, 100 + i))
            .collect();
        let frame = open_gone(&others, 400, vec![approval.attention_id]);
        let changes = rx(&mut book, frame, false, &[]);
        assert_eq!(changes.cancel, vec![tag("p1", &approval.attention_id)]);
        let posted = book.fit(changes.post, &[], &none);
        assert_eq!(posted.len(), MAX_SHOWN);
        assert!(
            !posted
                .iter()
                .any(|r| r.alert.attention_id == approval.attention_id)
        );
        assert!(!book.computers["p1"].knows(&approval.attention_id));
    }

    #[test]
    fn what_waits_for_the_app_is_named_on_a_reconnect_and_rises_only_once_caught_up() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let question = alert(AttentionKind::Question, 2);
        let frame = AlertServerFrame::Raised {
            alert: question.clone(),
        };
        assert!(rx(&mut book, frame, true, &[]).post.is_empty());
        assert_eq!(book.showing("p1", &[]), vec![question.attention_id]);
        book.ended("p1");
        // Away, the app leaving the screen raises nothing that may be answered.
        assert!(unwatched(&mut book, &computer()).post.is_empty());
        assert!(book.waiting("p1"));
        // Answered meanwhile: the computer says so on return.
        let frame = open_gone(&[], 5, vec![question.attention_id]);
        assert_eq!(
            rx(&mut book, frame, false, &[]).cancel,
            vec![tag("p1", &question.attention_id)]
        );
        assert!(!book.waiting("p1"));
        // One the list has no room to name is forgotten: the open list
        // raises it again if it is still open.
        let other = alert(AttentionKind::Question, 6);
        let frame = AlertServerFrame::Raised {
            alert: other.clone(),
        };
        rx(&mut book, frame, true, &[]);
        let many: Vec<String> = (0..MAX_SHOWING)
            .map(|_| tag("p1", &AttentionId::new()))
            .collect();
        assert_eq!(book.showing("p1", &many).len(), MAX_SHOWING);
        assert!(!book.computers["p1"].knows(&other.attention_id));
        let again = rx(&mut book, open(std::slice::from_ref(&other), 7), false, &[]);
        assert_eq!(tags(&again), vec![tag("p1", &other.attention_id)]);
    }

    #[test]
    fn only_the_computer_the_app_shows_waits_for_it() {
        let _one = lock(&ONE_LEDGER);
        viewing("a", true);
        // Leaving one no longer shown leaves the one shown now.
        viewing("b", true);
        viewing("a", false);
        assert_eq!(lock(&VIEWING).as_deref(), Some("b"));
        viewing("b", false);
        assert_eq!(*lock(&VIEWING), None);
    }

    #[test]
    fn an_alert_that_waited_shows_the_detail_chosen_when_it_rises() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(Settings {
            enabled: true,
            detail: Detail::TaskTitle,
        })
        .unwrap();
        let shade = FakeShade::default();
        *lock(&shade.shown) = (0..MAX_SHOWN).map(|i| format!("p9/{i}")).collect();
        let none = BTreeSet::new();
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &none,
                |book, shown| rx(book, frame, false, shown),
                |_| (),
            )
            .unwrap();
        };
        frame(open(&[], 1));
        let done = alert(AttentionKind::Completed, 2);
        frame(AlertServerFrame::Raised {
            alert: done.clone(),
        });
        configure(ON).unwrap();
        lock(&shade.shown).clear();
        update(
            current_watcher(),
            &shade,
            &none,
            |_, _| Changes::default(),
            |_| (),
        )
        .unwrap();
        let posted = lock(&shade.posted);
        let note = posted
            .iter()
            .find(|n| n.tag == tag("p1", &done.attention_id));
        assert_eq!(note.map(|n| n.body.as_str()), Some("Studio · Site"));
        drop(posted);
        lock(&LEDGER).take();
    }

    #[test]
    fn an_alert_android_had_no_room_for_after_all_waits_for_room() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let none = BTreeSet::new();
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &none,
                |book, shown| rx(book, frame, false, shown),
                |b| b.holding(),
            )
            .unwrap()
        };
        frame(open(&[], 1));
        // The app's own alerts filled the shade meanwhile.
        shade.full.store(true, std::sync::atomic::Ordering::SeqCst);
        let question = alert(AttentionKind::Question, 2);
        assert!(frame(AlertServerFrame::Raised {
            alert: question.clone(),
        }));
        assert_eq!(shade.shown(), vec![MORE.to_owned()]);
        assert!(!saved(dir.path()).computers["p1"].knows(&question.attention_id));
        // Room frees: it rises, and the note goes.
        shade.full.store(false, std::sync::atomic::Ordering::SeqCst);
        let holding = update(
            current_watcher(),
            &shade,
            &none,
            |_, _| Changes::default(),
            |b| b.holding(),
        )
        .unwrap();
        assert!(!holding);
        assert_eq!(shade.shown(), vec![tag("p1", &question.attention_id)]);
        assert!(
            saved(dir.path()).computers["p1"]
                .raised
                .contains(&question.attention_id)
        );
        lock(&LEDGER).take();
    }

    #[test]
    fn an_alert_the_app_clears_as_the_watcher_raises_it_is_forgotten_not_shown() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let none = BTreeSet::new();
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &none,
                |book, shown| rx(book, frame, false, shown),
                |b| b.holding(),
            )
            .unwrap()
        };
        frame(open(&[], 1));
        // The app hears it answered and clears it as the watcher posts it.
        let question = alert(AttentionKind::Question, 2);
        *lock(&shade.racing) = Some(tag("p1", &question.attention_id));
        assert!(!frame(AlertServerFrame::Raised {
            alert: question.clone(),
        }));
        assert!(shade.shown().is_empty());
        // Neither shown nor waiting for room: the next open list says.
        assert!(!saved(dir.path()).computers["p1"].knows(&question.attention_id));
        *lock(&shade.racing) = None;
        frame(open(std::slice::from_ref(&question), 3));
        assert_eq!(shade.shown(), vec![tag("p1", &question.attention_id)]);
        lock(&LEDGER).take();
    }

    #[test]
    fn an_alert_the_app_clears_while_it_waits_for_room_does_not_rise() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let none = BTreeSet::new();
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &none,
                |book, shown| rx(book, frame, false, shown),
                |b| b.holding(),
            )
            .unwrap()
        };
        frame(open(&[], 1));
        shade.full.store(true, std::sync::atomic::Ordering::SeqCst);
        let question = alert(AttentionKind::Question, 2);
        assert!(frame(AlertServerFrame::Raised {
            alert: question.clone(),
        }));
        // The app hears it answered first, on its own connection.
        shade.cancel(&tag("p1", &question.attention_id));
        shade.full.store(false, std::sync::atomic::Ordering::SeqCst);
        let room = update(
            current_watcher(),
            &shade,
            &none,
            |book, _| unwatched(book, &computer()),
            |b| b.holding(),
        );
        assert_eq!(room, Some(false));
        let raised = tag("p1", &question.attention_id);
        assert!(!lock(&shade.posted).iter().any(|n| n.tag == raised));
        assert!(shade.shown().is_empty());
        lock(&LEDGER).take();
    }

    #[test]
    fn an_alert_the_app_clears_before_it_leaves_the_screen_does_not_rise() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let none = BTreeSet::new();
        let frame = |frame: AlertServerFrame, in_app: bool| {
            update(
                current_watcher(),
                &shade,
                &none,
                |book, shown| rx(book, frame, in_app, shown),
                |_| (),
            )
            .unwrap()
        };
        frame(open(&[], 1), false);
        let question = alert(AttentionKind::Question, 2);
        frame(
            AlertServerFrame::Raised {
                alert: question.clone(),
            },
            true,
        );
        // The app hears it answered on its own connection, then leaves.
        shade.cancel(&tag("p1", &question.attention_id));
        update(
            current_watcher(),
            &shade,
            &none,
            |book, _| unwatched(book, &computer()),
            |_| (),
        );
        assert!(shade.shown().is_empty());
        lock(&LEDGER).take();
    }

    #[test]
    fn an_alert_the_computer_opens_again_after_the_app_cleared_it_rises() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let none = BTreeSet::new();
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &none,
                |book, shown| rx(book, frame, false, shown),
                |_| (),
            )
            .unwrap()
        };
        frame(open(&[], 1));
        let approval = alert(AttentionKind::Approval, 2);
        let raised = AlertServerFrame::Raised {
            alert: approval.clone(),
        };
        frame(raised.clone());
        let shown = vec![tag("p1", &approval.attention_id)];
        assert_eq!(shade.shown(), shown);
        // Answered, and cleared by the app first; then its answer could
        // not be handed to the agent, so it is open again.
        shade.cancel(&shown[0]);
        frame(AlertServerFrame::Resolved {
            attention_id: approval.attention_id,
        });
        frame(raised);
        assert_eq!(shade.shown(), shown);
        lock(&LEDGER).take();
    }

    #[test]
    fn a_watcher_started_anew_raises_what_waited_only_once_its_computer_says() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let none = BTreeSet::new();
        let laptop = Computer {
            profile_id: "p2".into(),
            name: "Laptop".into(),
        };
        let frame = |computer: &Computer, frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &none,
                |book, shown| receive(book, computer, frame, false, shown, Timestamp(0)),
                |b| b.holding(),
            )
            .unwrap()
        };
        frame(&computer(), open(&[], 1));
        shade.full.store(true, std::sync::atomic::Ordering::SeqCst);
        let question = alert(AttentionKind::Question, 2);
        assert!(frame(
            &computer(),
            AlertServerFrame::Raised {
                alert: question.clone(),
            }
        ));
        // Watching stops and starts again in this process, and the question
        // is answered at the computer meanwhile. Its old connection's end
        // is its stopped watcher's, which changes nothing.
        next_watcher();
        with_book(|book| {
            book.restarted();
            shade.cancel(MORE);
        })
        .unwrap();
        shade.full.store(false, std::sync::atomic::Ordering::SeqCst);
        // The other computer is reached first: nothing of the first rises.
        frame(&laptop, open(&[], 3));
        assert!(shade.shown().is_empty());
        // The first's list leaves the question out, as answered.
        assert!(!frame(&computer(), open(&[], 4)));
        assert!(shade.shown().is_empty());
        let raised = tag("p1", &question.attention_id);
        assert!(!lock(&shade.posted).iter().any(|n| n.tag == raised));
        lock(&LEDGER).take();
    }

    #[test]
    fn a_stopped_watcher_forgets_nothing_its_replacement_shows() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let old = next_watcher();
        let new = next_watcher();
        let question = alert(AttentionKind::Question, 2);
        for f in [
            open(&[], 1),
            AlertServerFrame::Raised {
                alert: question.clone(),
            },
        ] {
            update(
                new,
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, f, false, shown),
                |_| (),
            );
        }
        let shown = vec![tag("p1", &question.attention_id)];
        assert_eq!(shade.shown(), shown);
        // The old one listed what was paired before this computer was, and
        // was then refused by it.
        forget_others(&BTreeSet::new(), &shade, old);
        refused("p1", &shade, old);
        assert_eq!(shade.shown(), shown);
        assert!(saved(dir.path()).computers["p1"].knows(&question.attention_id));
        refused("p1", &shade, new);
        assert!(shade.shown().is_empty());
        assert!(!saved(dir.path()).computers.contains_key("p1"));
        lock(&LEDGER).take();
    }

    #[test]
    fn the_first_list_is_the_apps_and_later_ones_raise_what_was_missed_once() {
        let mut book = Book::default();
        let old = alert(AttentionKind::Question, 5);
        let first = rx(&mut book, open(std::slice::from_ref(&old), 10), false, &[]);
        assert_eq!(first, Changes::default());

        // Away: one raised and answered, two raised and still open.
        let later = alert(AttentionKind::Completed, 30);
        let missed = alert(AttentionKind::Failure, 20);
        let all = [old.clone(), later.clone(), missed.clone()];
        let again = rx(&mut book, open(&all, 40), false, &[]);
        assert_eq!(
            tags(&again),
            vec![
                tag("p1", &missed.attention_id),
                tag("p1", &later.attention_id)
            ]
        );
        assert!(rx(&mut book, open(&all, 50), false, &[]).post.is_empty());
    }

    #[test]
    fn alerts_asked_after_watching_began_are_raised_when_the_computer_is_first_reached() {
        let mut book = Book::default();
        // Turned on at 1000 by the phone's clock, with the computer away.
        book.begin("p1", Timestamp(1_000));
        let before = alert(AttentionKind::Approval, 9_000);
        let after = alert(AttentionKind::Question, 9_500);
        // Reached 300 later; the computer's clock reads 9_700 by then.
        let frame = open(&[before, after.clone()], 9_700);
        let first = receive(&mut book, &computer(), frame, false, &[], Timestamp(1_300));
        assert_eq!(tags(&first), vec![tag("p1", &after.attention_id)]);
        assert_eq!(book.computers["p1"].since, Some(Timestamp(9_400)));
        // Turned off, nothing begins.
        let mut off = Book {
            settings: Some(Settings {
                enabled: false,
                detail: Detail::Minimal,
            }),
            ..Book::default()
        };
        off.begin("p1", Timestamp(1));
        assert!(off.computers.is_empty());
    }

    #[test]
    fn raised_once_cleared_when_resolved() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let question = alert(AttentionKind::Question, 2);
        let raised = AlertServerFrame::Raised {
            alert: question.clone(),
        };
        let first = rx(&mut book, raised.clone(), false, &[]);
        assert_eq!(first.post.len(), 1);
        let note = first.post[0].note(book.detail()).unwrap();
        assert_eq!(note.title, "Question from Codex");
        assert_eq!(note.category, Category::NeedsYou);
        assert_eq!(note.body, "Studio · Site");
        assert_eq!(note.group, None);
        assert!(rx(&mut book, raised, false, &[]).post.is_empty());

        let resolved = rx(
            &mut book,
            AlertServerFrame::Resolved {
                attention_id: question.attention_id,
            },
            false,
            &[],
        );
        assert_eq!(resolved.cancel, vec![tag("p1", &question.attention_id)]);
        assert!(book.computers["p1"].notified.is_empty());
    }

    #[test]
    fn an_alert_the_app_showed_waits_until_it_leaves_the_screen_unless_seen() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let (seen, missed) = (
            alert(AttentionKind::Completed, 3),
            alert(AttentionKind::Approval, 4),
        );
        for a in [&seen, &missed] {
            let frame = AlertServerFrame::Raised { alert: a.clone() };
            assert!(rx(&mut book, frame, true, &[]).post.is_empty());
        }
        assert!(book.waiting("p1"));
        // The app reports one seen, then leaves the screen before the other.
        book.seen_in_app("p1", &[seen.attention_id]);
        let left = unwatched(&mut book, &computer());
        assert_eq!(tags(&left), vec![tag("p1", &missed.attention_id)]);
        assert!(!book.waiting("p1"));
        assert!(unwatched(&mut book, &computer()).post.is_empty());
        // Neither comes back on a reconnect.
        assert!(
            rx(&mut book, open(&[seen, missed], 5), false, &[])
                .post
                .is_empty()
        );
    }

    #[test]
    fn an_alert_android_did_not_show_is_raised_again() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let approval = alert(AttentionKind::Approval, 2);
        let frame = AlertServerFrame::Raised {
            alert: approval.clone(),
        };
        let posted = rx(&mut book, frame, false, &[]);
        book.unraise(&tags(&posted));
        assert!(book.computers["p1"].notified.is_empty());
        let again = rx(
            &mut book,
            open(std::slice::from_ref(&approval), 3),
            false,
            &[],
        );
        assert_eq!(tags(&again), vec![tag("p1", &approval.attention_id)]);
    }

    #[test]
    fn a_reconnect_clears_alerts_answered_meanwhile_from_this_computer_only() {
        let mut book = Book::default();
        let open_one = alert(AttentionKind::Approval, 1);
        let answered = alert(AttentionKind::Approval, 2);
        rx(
            &mut book,
            open(&[open_one.clone(), answered.clone()], 3),
            false,
            &[],
        );
        book.seen_in_app("p1", &[open_one.attention_id, answered.attention_id]);
        let shown = vec![
            tag("p1", &open_one.attention_id),
            tag("p1", &answered.attention_id),
            tag("p2", &answered.attention_id),
        ];
        let changes = rx(&mut book, open(&[open_one], 4), false, &shown);
        assert_eq!(changes.cancel, vec![tag("p1", &answered.attention_id)]);
        assert_eq!(book.computers["p1"].notified.len(), 1);
    }

    #[test]
    fn a_full_list_clears_nothing_and_an_alert_it_left_out_is_raised_when_it_returns() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 50), false, &[]);
        let old = alert(AttentionKind::Approval, 70);
        let raised = rx(&mut book, open(std::slice::from_ref(&old), 60), false, &[]);
        assert_eq!(tags(&raised), vec![tag("p1", &old.attention_id)]);
        // Away: more opened than one list holds, the raised one left out.
        let alerts: Vec<Alert> = (0..MAX_OPEN_ALERTS as i64)
            .map(|i| alert(AttentionKind::Completed, 100 + i))
            .collect();
        let shown = [tag("p1", &old.attention_id)];
        let full = rx(&mut book, open(&alerts, 400), false, &shown);
        assert_eq!(full.post.len(), MAX_OPEN_ALERTS);
        assert!(full.cancel.is_empty());
        assert!(book.computers["p1"].notified.contains(&old.attention_id));
        // One answered makes room for it again: still raised once, not twice.
        let mut next = alerts[1..].to_vec();
        next.insert(0, old);
        assert!(rx(&mut book, open(&next, 500), false, &[]).post.is_empty());
        // One never shown, left out while the list was full, is new.
        let unseen = alert(AttentionKind::Question, 80);
        next[0] = unseen.clone();
        let changes = rx(&mut book, open(&next, 600), false, &[]);
        assert_eq!(tags(&changes), vec![tag("p1", &unseen.attention_id)]);
    }

    #[test]
    fn a_cleared_shade_raises_again_what_was_raised_and_still_waits() {
        let mut book = Book::default();
        book.started("4:1");
        rx(&mut book, open(&[], 1), false, &[]);
        let (raised, seen, answered) = (
            alert(AttentionKind::Approval, 2),
            alert(AttentionKind::Question, 3),
            alert(AttentionKind::Completed, 4),
        );
        for (alert, in_app) in [(&raised, false), (&seen, true), (&answered, false)] {
            let frame = AlertServerFrame::Raised {
                alert: alert.clone(),
            };
            rx(&mut book, frame, in_app, &[]);
        }
        book.seen_in_app("p1", &[seen.attention_id]);
        let reopened = |book: &mut Book| {
            let frame = open(&[raised.clone(), seen.clone()], 5);
            tags(&rx(book, frame, false, &[]))
        };
        // The watcher starting again, with the shade as it was, repeats nothing.
        book.started("4:1");
        assert_eq!(reopened(&mut book), Vec::<String>::new());
        // After a restart or an update, only what was raised here and is
        // still open comes back.
        book.started("5:1");
        assert_eq!(reopened(&mut book), vec![tag("p1", &raised.attention_id)]);
        assert_eq!(reopened(&mut book), Vec::<String>::new());
    }

    #[test]
    fn titles_categories_detail_and_grouping() {
        let mut book = Book {
            settings: Some(Settings {
                enabled: true,
                detail: Detail::TaskTitle,
            }),
            ..Book::default()
        };
        rx(&mut book, open(&[], 0), false, &[]);
        let cases = [
            (
                AttentionKind::Approval,
                "Approval waiting",
                Category::NeedsYou,
            ),
            (
                AttentionKind::WaitingForYou,
                "Task needs you",
                Category::NeedsYou,
            ),
            (
                AttentionKind::Conflict,
                "Task needs you",
                Category::NeedsYou,
            ),
            (
                AttentionKind::ReconciliationRequired,
                "Task needs you",
                Category::NeedsYou,
            ),
            (
                AttentionKind::Completed,
                "Task finished",
                Category::Finished,
            ),
            (
                AttentionKind::Failure,
                "Task stopped with a problem",
                Category::Finished,
            ),
        ];
        for (kind, title, category) in cases {
            let changes = rx(
                &mut book,
                AlertServerFrame::Raised {
                    alert: alert(kind, 1),
                },
                false,
                &[],
            );
            let note = changes.post[0].note(book.detail()).unwrap();
            assert_eq!((note.title.as_str(), note.category), (title, category));
            assert_eq!(note.body, "Studio · Site · Fix the header");
            let grouped = category == Category::Finished;
            assert_eq!(note.group.is_some(), grouped);
            assert_eq!(note.group_title.as_deref(), grouped.then_some("Studio"));
        }
        let none = rx(
            &mut book,
            AlertServerFrame::Raised {
                alert: alert(AttentionKind::ConnectionIssue, 1),
            },
            false,
            &[],
        );
        assert!(none.post.is_empty());
    }

    #[test]
    fn a_headline_replaces_the_heading() {
        let mut a = alert(AttentionKind::Completed, 1);
        a.headline = Some("PR #12 merged".into());
        let note = note(&computer(), &a, Detail::Minimal).unwrap();
        assert_eq!(
            (note.title.as_str(), note.category),
            ("PR #12 merged", Category::Finished)
        );
        assert_eq!(note.body, "Studio · Site");
    }

    #[test]
    fn the_route_opens_the_alert() {
        let a = alert(AttentionKind::Question, 1);
        let note = note(&computer(), &a, Detail::Minimal).unwrap();
        assert_eq!(
            note.route,
            serde_json::json!({"kind": "attention", "profileId": "p1", "attentionId": a.attention_id})
        );
        assert_eq!(note.tag, format!("p1/{}", a.attention_id.as_uuid()));
    }

    fn asking(asked: Option<&str>) -> Reply {
        Reply {
            profile_id: "p1".into(),
            approval_id: ApprovalId::new(),
            revision: 3,
            asked: asked.map(Into::into),
        }
    }

    #[test]
    fn a_question_comes_with_its_words_choices_and_where_to_answer() {
        let mut a = alert(AttentionKind::Question, 1);
        let approval_id = ApprovalId::new();
        a.question = Some(protocol::alerts::AlertQuestion {
            approval_id,
            revision: 4,
            text: "Create the repo as private?".into(),
            asked: Some("Create the repo as private?".into()),
            choices: vec!["Yes".into(), "No".into()],
        });
        let note = note(&computer(), &a, Detail::Minimal).unwrap();
        assert_eq!(note.body, "Studio · Site");
        assert_eq!(
            serde_json::to_value(&note).unwrap()["question"],
            serde_json::json!({
                "text": "Create the repo as private?",
                "choices": ["Yes", "No"],
                "reply": {
                    "profileId": "p1",
                    "approvalId": approval_id,
                    "revision": 4,
                    "asked": "Create the repo as private?",
                },
            })
        );

        // Asked in words: no choices and nothing to answer by.
        let q = a.question.as_mut().unwrap();
        (q.asked, q.choices) = (None, Vec::new());
        let json = serde_json::to_value(note_of(&a)).unwrap();
        assert_eq!(json["question"]["text"], "Create the repo as private?");
        assert!(json["question"].get("choices").is_none());
        assert!(json["question"]["reply"].get("asked").is_none());

        // Only a question asks one, and an alert without one asks nothing.
        a.kind = AttentionKind::Approval;
        assert!(note_of(&a).question.is_none());
        let plain = serde_json::to_value(note_of(&alert(AttentionKind::Question, 1))).unwrap();
        assert!(plain.get("question").is_none());
    }

    fn note_of(a: &Alert) -> Note {
        note(&computer(), a, Detail::Minimal).unwrap()
    }

    #[test]
    fn an_answer_is_the_apps_command_at_the_revision_asked() {
        let reply = asking(Some("Create the repo as private?"));
        let envelope = answering(&reply, "  Yes \n").unwrap();
        assert_eq!(envelope.expected_revision, Some(3));
        assert_eq!(
            envelope.body,
            Command::ResolveApproval {
                approval_id: reply.approval_id,
                decision: ApprovalDecision::Answer {
                    text: "Yes".into(),
                    answers: vec![QuestionAnswer {
                        id: None,
                        question: "Create the repo as private?".into(),
                        choices: vec!["Yes".into()],
                    }],
                },
            }
        );

        // A question asked in words is answered in words.
        let words = answering(&asking(None), "Use the org account").unwrap();
        assert!(matches!(
            words.body,
            Command::ResolveApproval {
                decision: ApprovalDecision::Answer { ref text, ref answers },
                ..
            } if text == "Use the org account" && answers.is_empty()
        ));

        assert!(matches!(
            answering(&reply, " \n "),
            Err(Error::Invalid("Write an answer first."))
        ));
    }

    #[test]
    fn only_the_answers_own_receipt_settles_it() {
        let id = CommandId::new();
        let frame = |status, error| {
            serde_json::to_string(&ServerFrame::Receipt(protocol::CommandReceipt {
                command_id: id,
                status,
                seq: None,
                result: None,
                error,
            }))
            .unwrap()
        };
        assert!(matches!(
            receipt(&frame(ReceiptStatus::Accepted, None), id),
            Some(Ok(()))
        ));
        assert!(matches!(
            receipt(&frame(ReceiptStatus::Duplicate, None), id),
            Some(Ok(()))
        ));
        let stale = protocol::ProtocolError::new(ErrorCode::StaleRevision, "Changed");
        assert!(matches!(
            receipt(&frame(ReceiptStatus::Rejected, Some(stale)), id),
            Some(Err(Error::Remote(e))) if e.code == ErrorCode::StaleRevision
        ));
        assert!(matches!(
            receipt(&frame(ReceiptStatus::Rejected, None), id),
            Some(Err(Error::Invalid(_)))
        ));
        let fatal = protocol::ProtocolError::new(ErrorCode::PermissionDenied, "No");
        let fatal = serde_json::to_string(&ServerFrame::Fatal(fatal)).unwrap();
        assert!(matches!(
            receipt(&fatal, id),
            Some(Err(Error::Remote(e))) if e.code == ErrorCode::PermissionDenied
        ));

        // Another command's receipt, other frames and noise wait on.
        let other = frame(ReceiptStatus::Accepted, None);
        assert!(receipt(&other, CommandId::new()).is_none());
        let pong = serde_json::to_string(&ServerFrame::Pong { nonce: 1 }).unwrap();
        assert!(receipt(&pong, id).is_none());
        assert!(receipt("not json", id).is_none());
    }

    #[test]
    fn an_unsent_answer_says_why_in_the_persons_words() {
        for code in [
            ErrorCode::StaleRevision,
            ErrorCode::NotFound,
            ErrorCode::Conflict,
            ErrorCode::AlreadyResolved,
        ] {
            let error = Error::Remote(protocol::ProtocolError::new(code, "internal words"));
            assert_eq!(unsent(&error), "It was answered or changed elsewhere.");
        }
        let late = protocol::ProtocolError::new(ErrorCode::Expired, "Approval expired");
        assert_eq!(
            unsent(&Error::Remote(late)),
            "The agent stopped waiting for it."
        );
        let denied =
            protocol::ProtocolError::new(ErrorCode::PermissionDenied, "This phone may not answer");
        assert_eq!(unsent(&Error::Remote(denied)), "This phone may not answer");
        assert_eq!(unsent(&Error::Unreachable), Error::Unreachable.to_string());
    }

    #[test]
    fn a_network_change_starts_the_waits_after_failing_again() {
        let mut failures = 0;
        for wait in RETRY.into_iter().chain([RETRY[4]]) {
            assert_eq!(
                after(&Err(Error::Unreachable), &mut failures),
                Some((Status::Unreachable, wait))
            );
        }
        // As when the network changes while the watcher is signing in.
        assert_eq!(after(&Ok(Ended::Moved), &mut failures), None);
        assert_eq!(
            after(&Ok(Ended::Closed), &mut failures),
            Some((Status::Unreachable, RETRY[0]))
        );
        assert_eq!(
            after(&Err(Error::Unauthenticated), &mut failures),
            Some((Status::NotAllowed, IDLE))
        );
    }

    #[test]
    fn unknown_frames_are_skipped_and_fatal_ones_end_the_session() {
        assert!(parse(r#"{"type":"something_new"}"#).unwrap().is_none());
        let fatal = serde_json::to_string(&ServerFrame::Fatal(protocol::ProtocolError::new(
            ErrorCode::PermissionDenied,
            "grants changed",
        )))
        .unwrap();
        assert!(
            matches!(parse(&fatal), Err(Error::Remote(e)) if e.code == ErrorCode::PermissionDenied)
        );
        assert!(parse(&"x".repeat(network::MAX_TEXT + 1)).is_err());
    }

    #[test]
    fn the_book_survives_a_restart_and_off_forgets_what_was_raised() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let _one = lock(&ONE_LEDGER);
        *lock(&LEDGER) = Some(Ledger {
            path: path.clone(),
            book: Book::default(),
            saved: Vec::new(),
        });
        let on = Settings {
            enabled: true,
            detail: Detail::Minimal,
        };
        configure(on).unwrap();
        let question = alert(AttentionKind::Question, 2);
        with_book(|book| {
            rx(book, open(&[], 1), false, &[]);
            let frame = AlertServerFrame::Raised {
                alert: question.clone(),
            };
            rx(book, frame, false, &[]);
        })
        .unwrap();
        assert!(with_book(|b| b.computers.contains_key("p1")).unwrap());
        // Off: what the watcher raised is cleared, since nothing else would.
        let raised = configure(Settings {
            enabled: false,
            ..on
        })
        .unwrap();
        assert_eq!(
            raised,
            vec![MORE.to_owned(), tag("p1", &question.attention_id)]
        );
        let saved: Book = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(saved.computers.is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        lock(&LEDGER).take();
        init(dir.path());
        assert_eq!(settings().unwrap().map(|s| s.enabled), Some(false));
        lock(&LEDGER).take();
    }
}
