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
//! raised while the phone was away. An alert raised once is never raised
//! again for the same item: not after a swipe, a reconnect, a restart of the
//! phone or an update of the app, though the last two clear Android's shade.
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
    agent_updates::{AgentUpdateNoticeKind, FEATURE as AGENT_UPDATE_FEATURE},
    alerts::{Alert, AlertClientFrame, AlertServerFrame, MAX_OPEN_ALERTS, MAX_SHOWING},
    auth::ConnectionKind,
    capability::{AppOffer, HarnessKind},
    command::{ApprovalDecision, QuestionAnswer},
    model::AttentionKind,
    notifications::{AlertTopic, AlertType, Tier},
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
const FILE: &str = "alerts.json";
/// The book as this version writes it. Version 1 raised again, after an
/// update or a restart of the phone, every alert still open; reading one
/// counts every alert open then as raised (see [`Book::migrate`]).
const VERSION: u32 = 2;
/// How far apart the saved times a computer was heard from are, at least.
const HEARD_STEP_MS: i64 = 60_000;
/// The most alerts the watcher keeps in Android's shade. Android silently
/// drops an app's notifications beyond about 50, its summaries among them,
/// so the rest wait for room. The device plugin's `Notifications.MAX_SHOWN`
/// refuses the app's own beyond it.
const MAX_SHOWN: usize = 24;
/// The tag of the note that says how many alerts had no room. It names no
/// computer, so no computer's list clears it.
const MORE: &str = "more";
/// The tag of the note, posted once after the update to version 2, that
/// says alerts no longer come back. It names no computer either.
const CALM: &str = "calm";
/// How long an answer from a notification may take, signing in included.
const ANSWERING: Duration = Duration::from_secs(20);
/// After one notification from a computer makes a sound, the others from it
/// this soon after show silently: a burst makes one sound, not one each.
const BURST_MS: i64 = 60_000;
/// How long a raised alert may be missing from the shade, unexplained, before
/// it is put back silently. A swipe tells the watcher through Android's
/// delete intent, which arrives well within this; Android removes
/// notifications for reasons that aren't the person, such as a force stop or
/// a category replaced, and those must not count as handled.
const MISSING_MS: i64 = 10_000;

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
    /// Øphio's own word about its notifications, which never sounds.
    Notice,
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
    /// Shown without sound, vibration or a pop-up.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub silent: bool,
    /// What it is about: Android shows it under this type's category, as
    /// the person chose for the type on this phone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alert_type: Option<AlertType>,
    /// A choice over the phone's own for the type: the project's, or
    /// `Alert` for a task the person asked to hear about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub want: Option<Tier>,
    /// The tags a swipe of this note handles, as it shows them: a
    /// project's note names the alerts it counts as it is posted, so one
    /// counted after the person swiped isn't handled with them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub handles: Vec<String>,
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
    /// Shown with a sound or a pop-up.
    Sounded,
    /// The shade filled meanwhile, as with alerts the app raised itself.
    NoRoom,
    /// The app cleared it, as answered, since its alert was learned of.
    Cleared,
    /// Not shown: the person turned its type off on this phone, or its
    /// project or task did. It counts as handled.
    Off,
}

/// Where alerts are shown: Android's notification shade.
pub trait Shade: Send + Sync {
    /// Shows `note`. Given `since`, the shade's count of clears when its
    /// alert was learned of, shows nothing if it was cleared after then.
    fn post(&self, note: &Note, since: Option<i64>) -> Result<Posted>;
    /// An individual may be posted as its project's summary. Check clears of
    /// both identities, reuse visible members' places, and remove individuals
    /// only after a successful post. An urgent replacement may alert in place.
    fn post_member(&self, note: &Note, since: i64, member: &str, realert: bool) -> Result<Posted>;
    fn cancel(&self, tag: &str);
    /// How many alerts the app and the watcher have cleared, by the shade's count.
    fn clears(&self) -> i64;
    /// The tags of the alerts showing now.
    fn shown(&self) -> Vec<String>;
    /// Whether the app's screen is in front and unlocked.
    fn on_screen(&self) -> bool;
    /// How `note` would show now, by this phone's choice for its type and
    /// any choice over it, without posting it.
    fn effective(&self, _note: &Note) -> Tier {
        Tier::Alert
    }
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
    /// When this phone last heard from each computer, in milliseconds by its
    /// own clock. The app's screen shows it while its own connection is
    /// closed, as it is whenever the app is not on screen.
    #[serde(default)]
    heard: BTreeMap<String, i64>,
    /// The note that alerts no longer come back, still to be told or
    /// finished telling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    calm: Option<Calm>,
    /// How many alerts the note of those without room counts.
    #[serde(skip)]
    more: usize,
    /// How many of them need the person, as last told.
    #[serde(skip)]
    more_needs_you: usize,
    /// When a notification from each computer last made a sound, in ms.
    #[serde(skip)]
    sounded: BTreeMap<String, i64>,
    /// The tags of the alerts showing silently, oldest first: when the shade
    /// is full, a new one that alerts takes the oldest one's place.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    quiet: Vec<String>,
    /// The shade's count of clears as the update under way began: what the
    /// computer says is open is learned of at it.
    #[serde(skip)]
    clears: i64,
}
impl Default for Book {
    fn default() -> Self {
        Self {
            version: VERSION,
            settings: None,
            shade: None,
            computers: BTreeMap::new(),
            heard: BTreeMap::new(),
            calm: None,
            more: 0,
            more_needs_you: 0,
            sounded: BTreeMap::new(),
            quiet: Vec::new(),
            clears: 0,
        }
    }
}
impl Book {
    /// Notes that the computer was heard from at `at`. The computer pings
    /// every few minutes, so a minute's steps keep the file from being
    /// written at each; a clock set back is believed at once.
    fn heard_from(&mut self, profile_id: &str, at: Timestamp) {
        let last = self.heard.get(profile_id).copied();
        if last.is_none_or(|last| at.millis() < last || at.millis() - last >= HEARD_STEP_MS) {
            self.heard.insert(profile_id.to_owned(), at.millis());
        }
    }
    fn off(&self) -> bool {
        self.settings.is_some_and(|s| !s.enabled)
    }
    fn detail(&self) -> Detail {
        self.settings.map_or(Detail::Minimal, |s| s.detail)
    }
    /// The watcher starts with Android's shade as `shade`. When that changed,
    /// as after a restart of the phone or an update of the app, Android
    /// cleared every alert raised here. What was still showing then, never
    /// swiped away, opened or seen, is put back silently once its computer
    /// says it is still open; nothing else is raised again.
    fn started(&mut self, shade: &str) {
        if self.shade.as_ref().is_some_and(|s| s != shade) {
            for seen in self.computers.values_mut() {
                // What no longer showed by then isn't put back, nor is it
                // counted as handled: nobody said it was.
                let raised = std::mem::take(&mut seen.raised);
                let showed = std::mem::take(&mut seen.shown_last);
                seen.restore.extend(raised.intersection(&showed).copied());
                seen.missing.clear();
                seen.folds.clear();
            }
            self.quiet.clear();
        }
        self.shade = Some(shade.to_owned());
    }
    /// The person handled these alerts from `profile_id`: swiped them away,
    /// opened them or saw them in the app, here or, when `told`, on another
    /// of their devices. They are never raised again; the tags to clear are
    /// returned. Those handled here are kept to tell the computer.
    fn handle(&mut self, profile_id: &str, ids: &[AttentionId], told: bool) -> Vec<String> {
        let Some(seen) = self.computers.get_mut(profile_id) else {
            return Vec::new();
        };
        let mut tags = Vec::new();
        for id in ids {
            seen.notified.insert(*id);
            seen.raised.remove(id);
            seen.restore.remove(id);
            seen.missing.remove(id);
            seen.shown_last.remove(id);
            seen.waiting.remove(id);
            seen.held.retain(|r| r.alert.attention_id != *id);
            if !told {
                seen.unsent.insert(*id);
            }
            tags.push(tag(profile_id, id));
        }
        tags
    }
    /// Those raised here, not handled, that have been missing from the shade
    /// for [`MISSING_MS`] by `now`: put back silently. Android removed them
    /// for a reason that wasn't the person, or the person's swipe would have
    /// handled them. A project's alerts that its note counts in their place
    /// still show, through it.
    fn missing(&mut self, shown: &[String], now: i64, since: i64) -> Vec<Raise> {
        // Each put back is stamped with the shade's count of clears when it
        // was last shown, so one the app cleared since, as answered or seen,
        // is refused by the shade and stays gone.
        let showing = |t: &str| shown.iter().any(|s| s == t);
        let mut back = Vec::new();
        for (profile_id, seen) in &mut self.computers {
            let counted: BTreeSet<AttentionId> = (seen.folds.iter())
                .filter(|(project_id, f)| f.folded && showing(&folded_tag(profile_id, project_id)))
                .flat_map(|(_, f)| f.members.keys().copied())
                .collect();
            let raised: Vec<AttentionId> = seen.raised.iter().copied().collect();
            for id in raised {
                if counted.contains(&id) || showing(&tag(profile_id, &id)) {
                    seen.missing.remove(&id);
                    continue;
                }
                let Some(alert) = seen.alerts.get(&id) else {
                    continue;
                };
                let from = *seen.missing.entry(id).or_insert(now);
                if now - from >= MISSING_MS {
                    seen.missing.remove(&id);
                    back.push(Raise {
                        computer: Computer {
                            profile_id: profile_id.clone(),
                            name: seen.name.clone(),
                        },
                        alert: alert.clone(),
                        since: seen.posted.get(&id).copied().unwrap_or(since),
                        silent: true,
                    });
                }
            }
        }
        back
    }
    /// How long until the soonest missing alert is due to go back, if any.
    fn next_missing(&self, now: i64) -> Option<Duration> {
        (self.computers.values())
            .flat_map(|s| s.missing.values())
            .map(|from| from + MISSING_MS - now)
            .min()
            .map(|ms| Duration::from_millis(ms.clamp(500, MISSING_MS) as u64 + 100))
    }
    /// The app cleared these, tagged `<profile>/<attention>`: answered, or
    /// seen on its screen. None is put back; whether it is open is the
    /// computer's to say.
    fn cleared_by_app(&mut self, tags: &[String]) {
        for t in tags {
            self.quiet.retain(|q| q != t);
            let Some((profile_id, id)) = t.split_once('/') else {
                continue;
            };
            let Ok(id) = id.parse::<AttentionId>() else {
                continue;
            };
            let seen = self
                .computers
                .entry(profile_id.to_owned())
                .or_insert_with(|| Seen::began(Timestamp::now()));
            // Also blocks an app post still waiting for the ledger. Grouping
            // cancellations happen inside the posting transaction, not here.
            seen.notified.insert(id);
            seen.waiting.remove(&id);
            seen.held.retain(|r| r.alert.attention_id != id);
            seen.restore.remove(&id);
            seen.raised.remove(&id);
            seen.missing.remove(&id);
            seen.shown_last.remove(&id);
            seen.posted.remove(&id);
        }
    }
    /// A book written by version 1, which raised again everything still
    /// open after an update. Every alert open now counts as raised, as the
    /// next list from each computer says which: nothing from before this
    /// update shows again. One silent note says so, and how many things
    /// wait for the person.
    fn migrate(&mut self) {
        if self.version >= VERSION {
            return;
        }
        self.version = VERSION;
        for seen in self.computers.values_mut() {
            seen.rebase = true;
            // Version 1 never learned of swipes, so what it raised can't be
            // told from what was swiped: none of it is put back.
            seen.raised.clear();
            seen.restore.clear();
            seen.shown_last.clear();
        }
        if !self.off() && !self.computers.is_empty() {
            self.calm = Some(Calm {
                pending: self.computers.keys().cloned().collect(),
                needs_you: 0,
                told: None,
            });
        }
    }
    /// The note that alerts no longer come back, when it is to be posted
    /// or posted again with a new count; it is posted again only while it
    /// shows, so one swiped away stays gone. Done once every computer said
    /// what was open.
    fn calm_note(&mut self, shown: &[String]) -> Option<Note> {
        let calm = self.calm.as_mut()?;
        let note = match calm.told {
            Some(told) if told == calm.needs_you => None,
            Some(_) if !shown.iter().any(|t| t == CALM) => None,
            _ => {
                calm.told = Some(calm.needs_you);
                Some(calm.note())
            }
        };
        if calm.pending.is_empty() && calm.told.is_some() {
            self.calm = None;
        }
        note
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
        self.more_needs_you = 0;
    }
    /// The watcher began watching a computer at `at`, by this phone's clock:
    /// what it asks from then on is new, even before it is first reached.
    fn begin(&mut self, profile_id: &str, at: Timestamp) {
        self.computers
            .entry(profile_id.to_owned())
            .or_insert_with(|| Seen::began(at));
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
            if let Some((alert, _)) = seen.waiting.remove(&id) {
                seen.alerts.insert(id, alert);
            }
            seen.held.retain(|r| r.alert.attention_id != id);
            seen.notified.insert(id);
            seen.raised.insert(id);
        }
    }
    /// Account only for an actually shown post, whichever producer made it.
    fn posted(&mut self, note: &Note, outcome: Posted, since: i64, now: i64) {
        match outcome {
            Posted::Shown => {
                if !self.quiet.contains(&note.tag) {
                    self.quiet.push(note.tag.clone());
                }
            }
            Posted::Sounded => {
                self.quiet.retain(|t| *t != note.tag);
                if let Some((profile, _)) = note.tag.split_once('/') {
                    self.sounded.insert(profile.to_owned(), now);
                }
            }
            _ => return,
        }
        let tags = if note.handles.is_empty() {
            std::slice::from_ref(&note.tag)
        } else {
            &note.handles
        };
        for t in tags {
            if let Some((profile, id)) = t.split_once('/')
                && let Ok(id) = id.parse::<AttentionId>()
                && let Some(seen) = self.computers.get_mut(profile)
                && seen.raised.contains(&id)
            {
                seen.posted.insert(id, since);
                seen.missing.remove(&id);
            }
        }
    }
    /// Snapshot the real shade after posting, cancellation and recount. A
    /// fold counts only while its successfully posted summary actually shows.
    fn snapshot(&mut self, shown: &[String]) {
        for (profile, seen) in &mut self.computers {
            let counted: BTreeSet<_> = seen
                .folds
                .iter()
                .filter(|(project, f)| f.folded && shown.contains(&folded_tag(profile, project)))
                .flat_map(|(_, f)| f.members.keys().copied())
                .collect();
            seen.shown_last = seen
                .raised
                .iter()
                .filter(|id| counted.contains(id) || shown.contains(&tag(profile, id)))
                .copied()
                .collect();
        }
        self.quiet.retain(|t| shown.contains(t));
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
    fn configure(&mut self, settings: Settings) {
        self.settings = Some(settings);
    }
    /// Whether alerts had no room, as the note of them counts.
    #[cfg(test)]
    fn holding(&self) -> bool {
        self.computers
            .values()
            .any(|s| s.live && !s.held.is_empty())
    }
    /// Of `raises`, those with room in the shade, which shows `shown`, in
    /// order. Those without room are counted as raised and never shown: a
    /// note counts them, and they wait in the app. They never rise later as
    /// the person clears the shade, which would bring older ones back as
    /// fast as they are swiped away.
    fn fit(
        &mut self,
        raises: Vec<Raise>,
        shown: &[String],
        _in_app: &BTreeSet<String>,
    ) -> (Vec<Raise>, Vec<Raise>) {
        let showing = |tag: &str| shown.iter().any(|t| t == tag);
        let mut room =
            MAX_SHOWN.saturating_sub(shown.iter().filter(|t| *t != MORE && *t != CALM).count());
        let mut post = Vec::new();
        let mut refused = Vec::new();
        let mut projects = BTreeSet::new();
        for raise in raises {
            let profile = &raise.computer.profile_id;
            let project = raise.alert.project_id;
            // Folding replaces the project's visible members, not another
            // place. Include a project already admitted in this same batch.
            let reuses_project = raise.alert.bundle
                && (projects.contains(&(profile.clone(), project))
                    || self.computers.get(profile).is_some_and(|s| {
                        s.folds.get(&project).is_some_and(|f| {
                            f.members.keys().any(|id| {
                                s.raised.contains(id)
                                    && ((f.folded && showing(&folded_tag(profile, &project)))
                                        || showing(&tag(profile, id)))
                            })
                        })
                    }));
            if showing(&raise.tag()) || reuses_project || room > 0 {
                if !showing(&raise.tag()) && !reuses_project {
                    room -= 1;
                }
                if raise.alert.bundle {
                    projects.insert((profile.clone(), project));
                }
                post.push(raise);
            } else {
                refused.push(raise);
            }
        }
        (post, refused)
    }
    /// The oldest alert showing silently, to make room for one that alerts.
    fn oldest_quiet(&self, shown: &[String]) -> Option<String> {
        (self.quiet.iter()).find(|t| shown.contains(t)).cloned()
    }
    /// Clears `tag` to make room: not handled, and not put back.
    fn make_room(&mut self, tag: &str) {
        self.quiet.retain(|t| t != tag);
        let Some((profile_id, id)) = tag.split_once('/') else {
            return;
        };
        let Some(seen) = self.computers.get_mut(profile_id) else {
            return;
        };
        if let Some(project) = id
            .strip_prefix(FOLDED)
            .and_then(|p| p.parse::<ProjectId>().ok())
        {
            // A quiet summary occupies one place but represents every member.
            // Making room is not handling them, and must not restore them later.
            if let Some(fold) = seen.folds.remove(&project) {
                for member in fold.members.keys() {
                    seen.raised.remove(member);
                    seen.missing.remove(member);
                    seen.shown_last.remove(member);
                    seen.posted.remove(member);
                }
            }
        } else if let Ok(id) = id.parse::<AttentionId>() {
            seen.raised.remove(&id);
            seen.missing.remove(&id);
            seen.shown_last.remove(&id);
            seen.posted.remove(&id);
        }
    }
    /// Counts `raises` as raised without showing them, for the note of
    /// those without room.
    fn hold(&mut self, raises: Vec<Raise>) {
        for raise in raises {
            let Some(seen) = self.computers.get_mut(&raise.computer.profile_id) else {
                continue;
            };
            let id = raise.alert.attention_id;
            seen.notified.insert(id);
            seen.raised.remove(&id);
            if !seen.held.iter().any(|r| r.alert.attention_id == id) {
                seen.held.push(raise);
            }
        }
    }
    /// The note of the alerts that had no room, when their count changed;
    /// `Err` with the tag when none is left open. It changes silently,
    /// alerting only when one more needs the person.
    /// It is posted again only as it grows, or while it shows: one swiped
    /// away stays gone until there is one more.
    fn more(&mut self, shown: &[String]) -> Option<std::result::Result<Note, &'static str>> {
        let held: Vec<&Raise> = (self.computers.values())
            .flat_map(|s| s.held.iter())
            .collect();
        if held.len() == self.more {
            return None;
        }
        let grew = held.len() > self.more;
        self.more = held.len();
        let needs_you = (held.iter())
            .filter(|r| category(&r.alert) == Some(Category::NeedsYou))
            .count();
        let more_need_you = needs_you > self.more_needs_you;
        self.more_needs_you = needs_you;
        if held.is_empty() {
            return Some(Err(MORE));
        }
        if !grew && !shown.iter().any(|t| t == MORE) {
            return None;
        }
        let count = match held.len() {
            1 => "1 more alert".to_owned(),
            n => format!("{n} more alerts"),
        };
        // Øphio's own note, silent; it alerts as the one more that needs
        // the person would, under that one's type.
        let loud = (held.iter().rev())
            .find(|r| more_need_you && category(&r.alert) == Some(Category::NeedsYou));
        Some(Ok(Note {
            tag: MORE.into(),
            category: if loud.is_some() {
                Category::NeedsYou
            } else {
                Category::Notice
            },
            title: count,
            body: "Open Øphio to see them.".into(),
            group: None,
            group_title: None,
            route: serde_json::Value::Null,
            question: None,
            silent: loud.is_none(),
            alert_type: loud.map(|r| alert_type(&r.alert)),
            want: loud.and_then(|r| want(&r.alert, alert_type(&r.alert))),
            handles: Vec::new(),
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
    /// The next open list counts all it lists as raised, and the
    /// computer's time it was sent as when new alerts begin: set once, as
    /// the book written by version 1 is read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    rebase: bool,
    /// Handled here, and not yet told to the computer, which tells the
    /// person's other devices.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    unsent: BTreeSet<AttentionId>,
    /// Showing when Android cleared the shade, after an update or a restart
    /// of the phone: put back silently if still open.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    restore: BTreeSet<AttentionId>,
    /// The computer takes the phone's word that an alert was handled.
    #[serde(skip)]
    takes_handled: bool,
    /// The raised alerts as last heard, to put back one Android removed.
    #[serde(skip)]
    alerts: BTreeMap<AttentionId, Alert>,
    /// The computer's name, as its alerts show it.
    #[serde(skip)]
    name: String,
    /// When each raised alert was first seen missing from the shade, in ms.
    #[serde(skip)]
    missing: BTreeMap<AttentionId, i64>,
    /// The shade's count of clears when each raised alert was last shown.
    #[serde(skip)]
    posted: BTreeMap<AttentionId, i64>,
    /// The raised alerts showing at the last look: after Android clears the
    /// shade, only these are put back.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    shown_last: BTreeSet<AttentionId>,
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
            rebase: false,
            unsent: BTreeSet::new(),
            restore: BTreeSet::new(),
            takes_handled: false,
            alerts: BTreeMap::new(),
            name: String::new(),
            missing: BTreeMap::new(),
            posted: BTreeMap::new(),
            shown_last: BTreeSet::new(),
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
        self.restore.remove(id);
        self.alerts.remove(id);
        self.missing.remove(id);
        self.shown_last.remove(id);
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
            self.restore.retain(|id| open.contains(id));
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
        self.missing.remove(&id);
        self.shown_last.remove(&id);
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
        self.alerts.insert(alert.attention_id, alert.clone());
        Some(Raise {
            computer: computer.clone(),
            alert: alert.clone(),
            since,
            silent: false,
        })
    }
}

/// The one note, after the update that stopped alerts coming back, that
/// says so and how many things wait for the person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Calm {
    /// Computers whose first open list since the update is still to come.
    pending: BTreeSet<String>,
    /// What those lists said waits for the person.
    needs_you: usize,
    /// The count the note showed, once posted.
    told: Option<usize>,
}
impl Calm {
    fn note(&self) -> Note {
        let mut body = "Notifications you swipe away stay away, even after an update.".to_owned();
        match self.needs_you {
            0 => {}
            1 => body.push_str(" 1 thing waits for you in Needs you."),
            n => body.push_str(&format!(" {n} things wait for you in Needs you.")),
        }
        Note {
            tag: CALM.into(),
            category: Category::Notice,
            title: "Notifications are calmer now".into(),
            body,
            group: None,
            group_title: None,
            route: serde_json::Value::Null,
            question: None,
            silent: true,
            alert_type: None,
            want: None,
            handles: Vec::new(),
        }
    }
}

/// Whether an alert's notification needs the person or is to look at;
/// none for one that raises no notification.
fn category(alert: &Alert) -> Option<Category> {
    if alert.agent_update.is_some() {
        return Some(Category::Finished);
    }
    if alert.new_version.is_some() {
        return Some(Category::Finished);
    }
    if alert.sign_in.is_some() {
        return Some(Category::NeedsYou);
    }
    if let Some(report) = &alert.report {
        return Some(if report.question.is_some() {
            Category::NeedsYou
        } else {
            Category::Finished
        });
    }
    heading(alert.kind).map(|(c, _)| c)
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
    /// Each member's type and any choice over the phone's, for the
    /// category the note shows in.
    #[serde(default)]
    types: BTreeMap<AttentionId, (AlertType, Option<Tier>)>,
}
impl Fold {
    fn note(&self, shade: &dyn Shade, profile_id: &str, project_id: &ProjectId) -> Note {
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
        let mut note = Note {
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
            silent: false,
            alert_type: None,
            want: None,
            // A swipe handles what the note counted as the person saw it.
            handles: (self.members.keys()).map(|m| tag(profile_id, m)).collect(),
        };
        // Compare actual device routing, including blocked channels and raw
        // project/task overrides, before applying the shared quiet budget.
        // Resolve each distinct choice once, not once per member.
        let choices: BTreeSet<_> = self
            .members
            .keys()
            .filter_map(|id| self.types.get(id).copied())
            .collect();
        let lead = choices.into_iter().max_by_key(|(kind, want)| {
            note.alert_type = Some(*kind);
            note.want = *want;
            (shade.effective(&note), std::cmp::Reverse(*kind))
        });
        note.alert_type = lead.map(|(kind, _)| kind);
        note.want = lead.and_then(|(_, want)| want);
        note
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
    /// Put back after Android cleared the shade, so without a sound.
    silent: bool,
}
impl Raise {
    fn tag(&self) -> String {
        tag(&self.computer.profile_id, &self.alert.attention_id)
    }
    fn note(&self, detail: Detail) -> Option<Note> {
        let mut note = note(&self.computer, &self.alert, detail)?;
        note.silent |= self.silent;
        Some(note)
    }
}

#[derive(Debug, Default, PartialEq)]
struct Changes {
    post: Vec<Raise>,
    cancel: Vec<String>,
}

/// A computer that labels each alert's type sent one without a type this
/// phone knows: a newer type, shown silently whatever this phone chose for
/// the type its kind suggests. Notes, from the open list, that the
/// computer takes this phone's word on what was handled, as only such a
/// computer labels types.
fn future_types(book: &mut Book, computer: &Computer, frame: AlertServerFrame) -> AlertServerFrame {
    let typed =
        |book: &Book| (book.computers.get(&computer.profile_id)).is_some_and(|s| s.takes_handled);
    let future = |alert: &mut Alert| {
        if alert.alert_type.is_none()
            && alert.sign_in.is_none()
            && alert.new_version.is_none()
            && alert.agent_update.is_none()
            && alert.report.is_none()
        {
            alert.project_tier = alert.project_tier.or(Some(Tier::Silent));
        }
    };
    match frame {
        AlertServerFrame::Open {
            mut alerts,
            now,
            gone,
            handled,
        } => {
            if handled {
                alerts.iter_mut().for_each(future);
            }
            AlertServerFrame::Open {
                alerts,
                now,
                gone,
                handled,
            }
        }
        AlertServerFrame::Raised { mut alert } => {
            if typed(book) {
                future(&mut alert);
            }
            AlertServerFrame::Raised { alert }
        }
        other => other,
    }
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
    let id = &computer.profile_id;
    let clears = book.clears;
    let mut changes = Changes::default();
    let frame = future_types(book, computer, frame);
    match frame {
        AlertServerFrame::Open {
            alerts,
            now,
            gone,
            handled,
        } => {
            let seen = (book.computers.entry(id.clone())).or_insert_with(|| Seen::began(at));
            // The first list since the update to version 2: what is open
            // now was raised before it, or was the app's to show.
            if std::mem::take(&mut seen.rebase) {
                seen.since = Some(now);
                seen.notified.extend(alerts.iter().map(|a| a.attention_id));
                if let Some(calm) = &mut book.calm
                    && calm.pending.remove(id)
                {
                    calm.needs_you += (alerts.iter())
                        .filter(|a| !a.quiet && category(a) == Some(Category::NeedsYou))
                        .count();
                }
            }
            let seen = book.computers.get_mut(id).expect("just entered");
            seen.name.clone_from(&computer.name);
            seen.takes_handled = handled;
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
            // A project muted, or its type turned off there, since it was
            // raised: it goes from this phone, and isn't counted as handled.
            for alert in &alerts {
                let off = want(alert, alert_type(alert)) == Some(Tier::Off);
                if off && seen.raised.remove(&alert.attention_id) {
                    seen.missing.remove(&alert.attention_id);
                    seen.shown_last.remove(&alert.attention_id);
                    changes.cancel.push(tag(id, &alert.attention_id));
                }
            }
            // The latest word on what is raised, to put it back if Android
            // removes it.
            for alert in &alerts {
                if seen.raised.contains(&alert.attention_id) {
                    seen.alerts.insert(alert.attention_id, alert.clone());
                }
            }
            for alert in seen.reopen(&alerts, now, at) {
                changes
                    .post
                    .extend(seen.raise(computer, alert, in_app, clears));
            }
            // What showed before Android cleared the shade goes back,
            // silently, unless it was answered, turned off or handled since.
            for id in std::mem::take(&mut seen.restore) {
                if let Some(alert) = alerts.iter().find(|a| a.attention_id == id && !a.quiet) {
                    seen.raised.insert(id);
                    seen.alerts.insert(id, alert.clone());
                    changes.post.push(Raise {
                        computer: computer.clone(),
                        alert: alert.clone(),
                        since: clears,
                        silent: true,
                    });
                }
            }
            // An older computer can't be told; nor can it tell the others.
            if !handled {
                seen.unsent.clear();
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
                seen.heard(std::slice::from_ref(alert.as_ref()), clears);
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
        // Handled on another of the person's devices, or this one's word
        // coming back.
        AlertServerFrame::Handled { attention_id } => {
            changes
                .cancel
                .extend(book.handle(id, &[attention_id], true));
        }
    }
    changes
}

/// The app's screen no longer shows `computer`: what arrived while it did,
/// and the app has not reported seen, is raised.
fn unwatched(book: &mut Book, computer: &Computer) -> Changes {
    let mut changes = Changes::default();
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
    // An agent that isn't signed in: tapping it starts signing in again, on
    // this computer, in the phone's own browser.
    if let Some(sign_in) = &alert.sign_in {
        return Some(Note {
            tag: tag(&computer.profile_id, &alert.attention_id),
            category: Category::NeedsYou,
            title: format!("{} isn't signed in on {}", sign_in.service, computer.name),
            body: "Tap to sign in again.".into(),
            group: None,
            group_title: None,
            question: None,
            silent: false,
            alert_type: Some(AlertType::SignIn),
            want: want(alert, AlertType::SignIn),
            handles: Vec::new(),
            route: serde_json::json!({ "kind": "sign_in", "profileId": computer.profile_id, "accountId": sign_in.account_id }),
        });
    }
    if let Some(update) = &alert.agent_update {
        let name = agent(update.harness);
        let version = update
            .version
            .as_deref()
            .map(safe_update_label)
            .filter(|s| !s.is_empty());
        let models = named_models(&update.models);
        let single = if update.models.len() == 1 {
            models.as_deref()
        } else {
            None
        };
        let title = match (update.kind, single) {
            (AgentUpdateNoticeKind::Behind, _) => format!("{name} has an update"),
            (AgentUpdateNoticeKind::UpdateForModels, Some(model)) => {
                format!("{model} needs newer {name}")
            }
            (AgentUpdateNoticeKind::NewModelsAvailable, Some(model)) => {
                format!("{model} is now available on {}", computer.name)
            }
            (AgentUpdateNoticeKind::UpdateForModels, _) => format!("Update {name} for new models"),
            (AgentUpdateNoticeKind::NewModelsAvailable, _) => format!("New models in {name}"),
        };
        let detail = match update.kind {
            AgentUpdateNoticeKind::Behind => "An update is available".to_owned(),
            AgentUpdateNoticeKind::UpdateForModels => format!(
                "An update announces {}. Availability depends on your account",
                models.unwrap_or_else(|| model_count_label(update.models.len()))
            ),
            AgentUpdateNoticeKind::NewModelsAvailable => format!(
                "The updated agent now lists {} for your account",
                models.unwrap_or_else(|| model_count_label(update.models.len()))
            ),
        };
        let detail = version.map_or(detail.clone(), |v| format!("Version {v} · {detail}"));
        return Some(Note {
            tag: tag(&computer.profile_id, &alert.attention_id),
            category: Category::Finished,
            title,
            body: format!("{} · {detail}. Tap to see agent updates.", computer.name),
            group: None,
            group_title: None,
            question: None,
            silent: false,
            alert_type: Some(AlertType::AgentUpdate),
            want: want(alert, AlertType::AgentUpdate),
            handles: Vec::new(),
            route: serde_json::json!({ "kind": "agent_updates", "profileId": computer.profile_id }),
        });
    }
    if let Some(version) = &alert.new_version {
        return Some(Note {
            tag: tag(&computer.profile_id, &alert.attention_id),
            category: Category::Finished,
            title: format!("Øphio {} is available", version.version),
            body: format!(
                "{} · Tap to see what’s new and how to update.",
                computer.name
            ),
            group: None,
            group_title: None,
            question: None,
            silent: false,
            alert_type: Some(AlertType::NewVersion),
            want: want(alert, AlertType::NewVersion),
            handles: Vec::new(),
            route: serde_json::json!({ "kind": "new_version", "profileId": computer.profile_id }),
        });
    }
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
            silent: false,
            alert_type: Some(alert_type(alert)),
            want: want(alert, alert_type(alert)),
            handles: Vec::new(),
            route: serde_json::json!({ "kind": "report", "profileId": computer.profile_id, "reportId": report.report_id }),
        });
    }
    let (category, title) = heading(alert.kind)?;
    let title = match (alert.kind, &alert.headline) {
        (_, Some(headline)) => headline.clone(),
        (AttentionKind::Question, None) => format!("{title} {}", agent(alert.harness)),
        // An agent's answer in a chat, as the app names it.
        _ if alert_type(alert) == AlertType::Reply => "New reply".to_owned(),
        _ => title.to_owned(),
    };
    let mut body = format!("{} · {}", computer.name, alert.project);
    if detail == Detail::TaskTitle && !alert.task.is_empty() {
        body.push_str(" · ");
        body.push_str(&alert.task);
    }
    // A project's notifications from one computer stack together, so a
    // burst from one project takes one place in the shade.
    let group = Some(format!(
        "{}/{}",
        computer.profile_id,
        alert.project_id.as_uuid()
    ));
    let group_title = Some(format!("{} · {}", alert.project, computer.name));
    let alert_type = alert_type(alert);
    Some(Note {
        tag: tag(&computer.profile_id, &alert.attention_id),
        category,
        title,
        body,
        group_title,
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
        silent: false,
        alert_type: Some(alert_type),
        want: want(alert, alert_type),
        handles: Vec::new(),
    })
}

/// Keep external version identifiers short and free of control characters in the shade.
fn safe_update_label(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        .take(40)
        .collect()
}

/// Name at most three model ids from structured host data, never release prose.
/// Unknown or unexpectedly long ids fall back to a count rather than being
/// echoed into the phone's notification shade.
fn named_models(ids: &[String]) -> Option<String> {
    if ids.is_empty()
        || ids.iter().any(|id| {
            id.is_empty()
                || id.len() > 72
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._@[]/".contains(&b))
        })
    {
        return None;
    }
    let names: Vec<String> = ids
        .iter()
        .take(3)
        .map(|id| ports::model_name::model_name(id))
        .filter(|name| name.len() <= 60)
        .collect();
    if names.len() != ids.len().min(3) {
        return None;
    }
    let mut text = names.join(", ");
    if ids.len() > 3 {
        text.push_str(&format!(" and {} more", ids.len() - 3));
    }
    Some(text)
}

fn model_count_label(count: usize) -> String {
    if count == 1 {
        "1 new model".into()
    } else {
        format!("{count} new models")
    }
}

/// What an alert is about: as the computer labels it, or, from an older
/// computer, as its kind and topic say.
fn alert_type(alert: &Alert) -> AlertType {
    if let Some(t) = alert.alert_type {
        return t;
    }
    if alert.agent_update.is_some() {
        return AlertType::AgentUpdate;
    }
    if alert.new_version.is_some() {
        return AlertType::NewVersion;
    }
    if alert.sign_in.is_some() {
        return AlertType::SignIn;
    }
    if let Some(report) = &alert.report {
        return if report.question.is_some() {
            AlertType::ReportQuestion
        } else {
            AlertType::Reports
        };
    }
    let item = protocol::model::AttentionItem {
        child_id: None,
        attention_id: alert.attention_id,
        kind: alert.kind,
        title: String::new(),
        body: None,
        task_id: None,
        run_id: None,
        approval_id: None,
        created_at: alert.created_at,
        resolved_at: None,
        priority: 0,
        topic: alert.topic,
        quiet: alert.quiet,
        report_id: None,
        account_id: None,
        alert: None,
        alert_handled_at: None,
        new_version: None,
        agent_update: None,
    };
    AlertType::of(&item, false).unwrap_or(AlertType::Decision)
}

/// A choice over the phone's own for this alert's type: the task the
/// person asked to hear about alerts; otherwise the project's own choice.
fn want(alert: &Alert, alert_type: AlertType) -> Option<Tier> {
    if alert.follow && alert_type.follows_task() {
        return Some(Tier::Alert);
    }
    alert.project_tier
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
        .filter(|b| (1..=VERSION).contains(&b.version))
        .map(|mut b| {
            b.migrate();
            b
        })
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

/// Saves the person's choice: whether alerts keep coming once the app is
/// closed, and how much they show. While the app runs, the watcher runs
/// either way, so nothing it raised needs clearing.
pub fn configure(settings: Settings) -> Result<()> {
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

/// Optional local IPC metadata: Android, not the page, owns this project's fold.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppFold {
    project_id: ProjectId,
    project: String,
    computer: String,
}

/// Compatibility posts have no native project metadata.
#[cfg(test)]
fn post_from_app_with(
    shade: &dyn Shade,
    note: Note,
    since: i64,
    now: impl FnOnce() -> i64,
) -> Result<()> {
    post_from_app_fold_with(shade, note, None, since, now)
}

#[cfg(any(target_os = "android", test))]
fn post_from_app_fold_with(
    shade: &dyn Shade,
    mut note: Note,
    project: Option<AppFold>,
    since: i64,
    now: impl FnOnce() -> i64,
) -> Result<()> {
    let mut guard = lock(&LEDGER);
    let ledger = guard.as_mut().ok_or(Error::Storage)?;
    if note.handles.len() > MAX_SHOWING
        || note.handles.iter().any(|tag| {
            let Some((profile, id)) = tag.split_once('/') else {
                return true;
            };
            tag.len() > 256
                || !note.tag.starts_with(&format!("{profile}/"))
                || id.parse::<AttentionId>().is_err()
        })
    {
        return Err(Error::Invalid("Invalid notification members."));
    }
    let now = now();
    let source = note.tag.clone();
    let profile_id = note.tag.split_once('/').map(|(p, _)| p.to_owned());
    let mut adopted = None;
    let mut replace_to_alert = false;
    // A settings refresh is not new work, with or without project grouping.
    // Keep non-attention/legacy tags compatible, and accept genuinely new IDs.
    if let Some((profile, id)) = source.split_once('/')
        && let Ok(id) = id.parse::<AttentionId>()
        && ledger
            .book
            .computers
            .get(profile)
            .is_some_and(|seen| seen.notified.contains(&id) && !seen.raised.contains(&id))
    {
        return Ok(());
    }
    if let Some(project) = project {
        let Some((profile, id)) = note.tag.split_once('/') else {
            return Err(Error::Invalid("Invalid notification member."));
        };
        let id = id
            .parse::<AttentionId>()
            .map_err(|_| Error::Invalid("Invalid notification member."))?;
        if note.tag.len() > 256
            || project.project.len() > 256
            || project.computer.len() > 256
            || !note.handles.is_empty()
        {
            return Err(Error::Invalid("Invalid notification project."));
        }
        let seen = ledger
            .book
            .computers
            .entry(profile.to_owned())
            .or_insert_with(|| Seen::began(Timestamp(now)));
        if seen
            .alerts
            .get(&id)
            .or_else(|| seen.waiting.get(&id).map(|(a, _)| a))
            .is_some_and(|a| a.project_id != project.project_id)
        {
            return Err(Error::Invalid("Notification belongs to another project."));
        }
        // An Off incoming member must not borrow an active member's channel
        // and enter its count. Leave other visible members intact.
        let choice = Note {
            silent: false,
            ..note.clone()
        };
        if shade.effective(&choice) == Tier::Off {
            seen.hush(id);
            if shade.shown().contains(&source) {
                shade.cancel(&source);
            }
            recount(&mut ledger.book, shade, &shade.shown());
            ledger.book.snapshot(&shade.shown());
            if let Err(error) = ledger.save() {
                tracing::warn!(%error, "alerts: could not record an alert turned off");
            }
            return Ok(());
        }
        seen.name.clone_from(&project.computer);
        let shown = shade.shown();
        let summary = folded_tag(profile, &project.project_id);
        let mut fold = seen
            .folds
            .get(&project.project_id)
            .cloned()
            .unwrap_or_else(|| Fold {
                project: project.project.clone(),
                computer: project.computer.clone(),
                members: BTreeMap::new(),
                folded: false,
                counted: 0,
                types: BTreeMap::new(),
            });
        let folded = fold.folded && shown.contains(&summary);
        fold.members
            .retain(|m, _| seen.raised.contains(m) && (folded || shown.contains(&tag(profile, m))));
        fold.types.retain(|m, _| fold.members.contains_key(m));
        fold.project = project.project;
        fold.computer = project.computer;
        fold.members.insert(id, note.category == Category::NeedsYou);
        if fold.members.len() > MAX_SHOWING {
            return Err(Error::Invalid("Too many notification members."));
        }
        if let Some(kind) = note.alert_type {
            fold.types.insert(id, (kind, note.want));
        }
        fold.folded = folded || fold.members.len() > 1;
        if fold.folded {
            replace_to_alert = folded && note.category == Category::NeedsYou;
            let silent = note.silent || (folded && note.category != Category::NeedsYou);
            note = fold.note(shade, profile, &project.project_id);
            note.silent = silent;
        }
        adopted = Some((project.project_id, fold));
    }
    if let Some(profile) = &profile_id {
        note.silent |= burst_quiet(&ledger.book, profile, now);
    }
    let realert = replace_to_alert && !note.silent && shade.effective(&note) == Tier::Alert;
    // Check the original member and summary before deliberately replacing a
    // summary to alert. A refused stale source must leave the old summary alone.
    let outcome = shade.post_member(&note, since, &source, realert)?;
    match outcome {
        Posted::NoRoom => {
            return Err(Error::Native(
                "There is no room for another notification.".into(),
            ));
        }
        Posted::Cleared => return Ok(()),
        Posted::Off => {
            // Off is known, but never recorded as successfully shown.
            for t in if note.handles.is_empty() {
                std::slice::from_ref(&note.tag)
            } else {
                &note.handles
            } {
                if let Some((profile, id)) = t.split_once('/')
                    && let Ok(id) = id.parse::<AttentionId>()
                {
                    ledger.book.begin(profile, Timestamp(now));
                    ledger.book.computers.get_mut(profile).unwrap().hush(id);
                }
            }
        }
        Posted::Shown | Posted::Sounded => {
            if let Some(profile) = &profile_id {
                ledger.book.begin(profile, Timestamp(now));
            }
            if let Some((project, mut fold)) = adopted {
                let profile = profile_id.as_ref().expect("validated member");
                for id in fold.members.keys() {
                    ledger.book.raised_by_app(profile, *id);
                }
                fold.counted = fold.members.len();
                ledger
                    .book
                    .computers
                    .get_mut(profile)
                    .unwrap()
                    .folds
                    .insert(project, fold);
            } else {
                record_app_note(&mut ledger.book, &note);
            }
            ledger.book.posted(&note, outcome, shade.clears(), now);
        }
    }
    ledger.book.snapshot(&shade.shown());
    if let Err(error) = ledger.save() {
        tracing::warn!(%error, "alerts: could not record an alert the app raised");
    }
    Ok(())
}

#[cfg(any(target_os = "android", test))]
fn record_app_note(book: &mut Book, note: &Note) {
    let Some((profile, id)) = note.tag.split_once('/') else {
        return;
    };
    if let Ok(id) = id.parse::<AttentionId>() {
        book.raised_by_app(profile, id);
        return;
    }
    let Some(project) = id
        .strip_prefix(FOLDED)
        .and_then(|id| id.parse::<ProjectId>().ok())
    else {
        return;
    };
    // Older pages have no membership to adopt; leave their existing behavior.
    if note.handles.is_empty() {
        return;
    }
    let ids: BTreeSet<AttentionId> = note
        .handles
        .iter()
        .filter_map(|tag| {
            let (p, id) = tag.split_once('/')?;
            (p == profile).then(|| id.parse().ok()).flatten()
        })
        .collect();
    for id in &ids {
        book.raised_by_app(profile, *id);
    }
    let Some(seen) = book.computers.get_mut(profile) else {
        return;
    };
    // The producer's title has this exact suffix. Metadata normally comes
    // from waiting alerts; this fallback also covers the UI arriving first.
    let suffix = format!(
        " has {} notification{}",
        ids.len(),
        if ids.len() == 1 { "" } else { "s" }
    );
    let mut fold = Fold {
        project: note
            .title
            .strip_suffix(&suffix)
            .unwrap_or(&note.title)
            .to_owned(),
        computer: seen.name.clone(),
        members: BTreeMap::new(),
        folded: true,
        counted: ids.len(),
        types: BTreeMap::new(),
    };
    for id in ids {
        let alert = seen.alerts.get(&id);
        if let Some(alert) = alert {
            if alert.project_id != project {
                continue;
            }
            fold.project.clone_from(&alert.project);
        }
        let kind = alert.map(alert_type).or(note.alert_type);
        let needs_you = alert.and_then(category).unwrap_or(note.category) == Category::NeedsYou;
        fold.members.insert(id, needs_you);
        if let Some(kind) = kind {
            fold.types.insert(
                id,
                (kind, alert.map(|a| want(a, kind)).unwrap_or(note.want)),
            );
        }
    }
    seen.folds.insert(project, fold);
}

fn burst_quiet(book: &Book, profile: &str, now: i64) -> bool {
    book.sounded
        .get(profile)
        .is_some_and(|at| (0..BURST_MS).contains(&(now - at)))
}

#[cfg(target_os = "android")]
pub use android::{clear_from_app, handle_from_app, post_from_app};

/// Each paired computer's connection, while the watcher runs.
pub fn statuses() -> BTreeMap<String, Status> {
    lock(&STATUS).clone()
}

/// When the watcher last heard from each paired computer, in milliseconds.
pub fn heard() -> BTreeMap<String, i64> {
    lock(&LEDGER)
        .as_ref()
        .map(|ledger| ledger.book.heard.clone())
        .unwrap_or_default()
}

/// Notes the computer was heard from now. A failed save is retried at the
/// next, so it does not end the connection.
fn note_heard(profile_id: &str, watcher: u64) {
    if let Err(error) = with_watched_book(watcher, |book| {
        book.heard_from(profile_id, Timestamp::now())
    }) {
        tracing::warn!(%error, "alerts: could not save when a computer was heard from");
    }
}

/// This phone's choices from before notifications had types, as Android's
/// three old categories kept them, by id and importance: the types that
/// used to show under each take the old choice, where it is quieter than
/// the type's recommended one. Never louder.
pub fn imported_tiers(old: &BTreeMap<String, i32>) -> BTreeMap<AlertType, Tier> {
    use AlertType::*;
    let under = |category: &str| -> &'static [AlertType] {
        match category {
            // Everything that waited on the person, and the slow steps and
            // issue calls that showed with them.
            "needs-you" => &[
                Question,
                Approval,
                SignIn,
                Decision,
                PrNeedsYou,
                ReportQuestion,
                GoAhead,
                IssueCall,
                Slow,
            ],
            "finished" => &[
                Finished, Failed, Reply, Merged, Issues, PrReady, Batch, Reports,
            ],
            "updates" => &[AppUpdate],
            _ => &[],
        }
    };
    let mut tiers = BTreeMap::new();
    for (category, importance) in old {
        // Android's importance: none, min, low, default, high.
        let tier = match importance {
            0 => Tier::Off,
            1 | 2 => Tier::Silent,
            _ => continue,
        };
        for t in under(category) {
            if tier < t.default_tier() {
                tiers.insert(*t, tier);
            }
        }
    }
    tiers
}

/// Rings each computer's connection when alerts were handled here, so it
/// tells its computer, which tells the person's other devices.
static TOLD: OnceLock<watch::Sender<u64>> = OnceLock::new();
fn told() -> &'static watch::Sender<u64> {
    TOLD.get_or_init(|| watch::channel(0).0)
}
fn tell() {
    told().send_modify(|n| *n = n.wrapping_add(1));
}

/// The person handled these alerts in the app: opened them, or saw their
/// items on screen. Their notifications go, here and on every other device
/// of theirs, and none is raised again. Returns the tags to clear.
pub fn handled(profile_id: &str, ids: &[AttentionId]) -> Result<Vec<String>> {
    let tags = with_book(|book| book.handle(profile_id, ids, false))?;
    if !tags.is_empty() {
        tell();
    }
    Ok(tags)
}

/// The person swiped away, or opened, a notification: `tags` are the alerts
/// it stood for, one per line, as it named them when posted (a project's
/// note names each it counted). Each is handled; any copy put back since
/// goes too.
pub fn dismissed(data: &Path, tags: &str, shade: &dyn Shade) -> Result<()> {
    init(data);
    let mut by_computer: BTreeMap<&str, Vec<AttentionId>> = BTreeMap::new();
    for t in tags.lines() {
        let Some((profile_id, id)) = t.split_once('/') else {
            continue;
        };
        if let Ok(id) = id.parse::<AttentionId>() {
            by_computer.entry(profile_id).or_default().push(id);
        }
    }
    with_book(|book| {
        for (profile_id, ids) in by_computer {
            for t in book.handle(profile_id, &ids, false) {
                shade.cancel(&t);
            }
        }
        recount(book, shade, &shade.shown());
        book.snapshot(&shade.shown());
    })?;
    tell();
    Ok(())
}

/// Android cancellations and recounts are one ledger/shade transaction too.
#[cfg(any(target_os = "android", test))]
fn clear_from_app_with(shade: &dyn Shade, tags: &[String]) -> Result<()> {
    with_book(|book| {
        book.cleared_by_app(tags);
        for tag in tags {
            shade.cancel(tag);
        }
        recount(book, shade, &shade.shown());
        book.snapshot(&shade.shown());
    })
}

#[cfg(any(target_os = "android", test))]
fn handle_from_app_with(shade: &dyn Shade, profile: &str, ids: &[AttentionId]) -> Result<()> {
    with_book(|book| {
        book.begin(profile, Timestamp::now());
        for tag in book.handle(profile, ids, false) {
            shade.cancel(&tag);
        }
        recount(book, shade, &shade.shown());
        book.snapshot(&shade.shown());
    })?;
    tell();
    Ok(())
}

/// The app cleared these notifications, by tag: answered, or seen on its
/// screen. None is put back.
pub fn cleared_by_app(tags: &[String]) -> Result<()> {
    with_book(|book| book.cleared_by_app(tags))
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
        book.heard.retain(|id, _| ids.contains(id));
        // A computer unpaired before it said what was open says nothing.
        if let Some(calm) = &mut book.calm {
            calm.pending.retain(|id| ids.contains(id));
            if calm.pending.is_empty() && calm.told.is_some() {
                book.calm = None;
            }
        }
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
        if let Some(calm) = &mut book.calm {
            calm.pending.remove(profile_id);
        }
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

fn supports_agent_updates(features: &[String]) -> bool {
    features.iter().any(|f| f == AGENT_UPDATE_FEATURE)
}

fn supports_new_version(features: &[String]) -> bool {
    features
        .iter()
        .any(|f| f == protocol::releases::RELEASE_CHECK_FEATURE)
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
    note_heard(&computer.profile_id, wake.watcher);
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
    // A computer that labels each alert's type leaves every choice of
    // which reach the person to the phone.
    let types = (signed_in.capabilities.features.iter())
        .any(|f| f == protocol::notifications::ALERT_TYPES_FEATURE);
    let reports_on = reports_supported && (types || report_alerts_on(record, vault).await);
    // Only a computer that can sign its agents in from the phone says when
    // one isn't signed in; an older one would refuse the field anyway.
    let sign_in = signed_in
        .capabilities
        .features
        .iter()
        .any(|f| f == protocol::accounts::SIGN_IN_ON_PHONE_FEATURE);
    // Only a computer that checks releases knows when a new version is available;
    // an older one would refuse the field anyway.
    let new_version = supports_new_version(&signed_in.capabilities.features);
    // Rung from here on, so nothing handled meanwhile is missed.
    let mut rung = told().subscribe();
    // What was handled here while the computer couldn't be told.
    let backlog = with_watched_book(wake.watcher, |book| untold(book, &computer.profile_id))?
        .unwrap_or_default();
    let watch = AlertClientFrame::Watch {
        reports: reports_on,
        showing,
        quiet: true,
        sign_in,
        new_version,
        agent_updates: supports_agent_updates(&signed_in.capabilities.features),
        types: true,
        handled: backlog.clone(),
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
            _ = rung.changed() => {
                tell_computer(&mut ws, &computer.profile_id, wake.watcher).await?;
                continue;
            }
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
        note_heard(&computer.profile_id, wake.watcher);
        // Pings are answered by the socket itself on its next read.
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => return Ok(Ended::Closed),
            _ => continue,
        };
        let Some(frame) = parse(&text)? else { continue };
        let opened = matches!(frame, AlertServerFrame::Open { .. });
        let first = opened && !std::mem::replace(&mut listed, true);
        if opened
            && !first
            && !types
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
        // The Watch told the first list's computer what was handled while
        // away; what was handled since is told now.
        if first {
            with_watched_book(wake.watcher, |book| {
                if let Some(seen) = book.computers.get_mut(&computer.profile_id) {
                    for id in &backlog {
                        seen.unsent.remove(id);
                    }
                }
            })?;
            tell_computer(&mut ws, &computer.profile_id, wake.watcher).await?;
        }
    }
}

/// What was handled here and not yet told to the computer, at most as many
/// as one frame names.
fn untold(book: &Book, profile_id: &str) -> Vec<AttentionId> {
    (book.computers.get(profile_id))
        .map(|s| s.unsent.iter().take(MAX_SHOWING).copied().collect())
        .unwrap_or_default()
}

/// Tells the computer which alerts were handled here, if it takes them,
/// and forgets them once sent.
async fn tell_computer(ws: &mut network::Socket, profile_id: &str, watcher: u64) -> Result<()> {
    loop {
        let Some((ids, takes)) = with_watched_book(watcher, |book| {
            let takes = book
                .computers
                .get(profile_id)
                .is_some_and(|s| s.takes_handled);
            (untold(book, profile_id), takes)
        })?
        else {
            return Ok(());
        };
        if ids.is_empty() || !takes {
            return Ok(());
        }
        let frame = AlertClientFrame::Handled {
            attention_ids: ids.clone(),
        };
        network::send_json(ws, &frame).await?;
        with_watched_book(watcher, |book| {
            if let Some(seen) = book.computers.get_mut(profile_id) {
                for id in &ids {
                    seen.unsent.remove(id);
                }
            }
        })?;
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
    let missing = book.next_missing(Timestamp::now().millis());
    match (book.waiting(&computer.profile_id).then_some(LOOK), missing) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
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
    let now = Timestamp::now().millis();
    // Only the person's own word handles an alert: one missing for another
    // reason goes back, silently.
    let book_clears = ledger.book.clears;
    let back = ledger.book.missing(&shown, now, book_clears);
    let mut changes = f(&mut ledger.book, &shown);
    changes.post.extend(back);
    for tag in &changes.cancel {
        shade.cancel(tag);
    }
    shown.retain(|t| !changes.cancel.contains(t));
    let detail = ledger.book.detail();
    let mut unshown = Vec::new();
    let mut no_room = Vec::new();
    let mut off = Vec::new();
    // Only notes already quiet before this batch may be displaced. Do not
    // rotate newly posted members of the same burst out of the shade.
    let mut replaceable = ledger.book.quiet.clone();
    for raise in changes.post {
        replaceable.retain(|t| shown.contains(t));
        // Admit and post one item at a time. A later eviction may remove a
        // project summary, so its place cannot be promised to a pending item.
        let (mut post, mut refused) = ledger.book.fit(vec![raise], &shown, in_app);
        let raise = if let Some(raise) = post.pop() {
            raise
        } else {
            let raise = refused.pop().expect("one candidate was fitted");
            let alerts = raise
                .note(detail)
                .is_some_and(|n| !n.silent && shade.effective(&n) == Tier::Alert);
            // A new alert takes the oldest silent notification's place:
            // making room and consuming it stay together, under this lock.
            let Some(victim) = ledger.book.oldest_quiet(&replaceable).filter(|_| alerts) else {
                no_room.push(raise);
                continue;
            };
            shade.cancel(&victim);
            shown.retain(|t| *t != victim);
            replaceable.retain(|t| *t != victim);
            ledger.book.make_room(&victim);
            raise
        };
        // One sound for a burst from a computer: the rest show silently.
        let hush = burst_quiet(&ledger.book, &raise.computer.profile_id, now);
        // A first summary inherits a pre-existing quiet individual's place,
        // but a wholly new quiet notification is not a victim in this batch.
        let quiet_replacement = ledger
            .book
            .computers
            .get(&raise.computer.profile_id)
            .and_then(|s| s.folds.get(&raise.alert.project_id))
            .filter(|f| {
                raise.alert.bundle
                    && f.members.keys().any(|id| {
                        let member = tag(&raise.computer.profile_id, id);
                        replaceable.contains(&member) && ledger.book.quiet.contains(&member)
                    })
            })
            .map(|_| folded_tag(&raise.computer.profile_id, &raise.alert.project_id));
        let raise = match fold(&mut ledger.book, shade, raise, &mut shown, hush) {
            Folding::Alone(raise) => raise,
            Folding::Shown => {
                if let Some(summary) = quiet_replacement
                    && ledger.book.quiet.contains(&summary)
                    && !replaceable.contains(&summary)
                {
                    replaceable.push(summary);
                }
                continue;
            }
            Folding::NoRoom(raise) => {
                no_room.push(raise);
                continue;
            }
            Folding::Failed(raise) => {
                unshown.push(raise.tag());
                continue;
            }
            Folding::Off(raise) => {
                off.push(raise);
                continue;
            }
        };
        let Some(mut note) = raise.note(detail) else {
            continue;
        };
        note.silent |= hush;
        match shade.post(&note, Some(raise.since)) {
            Ok(outcome @ (Posted::Shown | Posted::Sounded)) => {
                ledger.book.posted(&note, outcome, book_clears, now);
                shown = shade.shown();
            }
            // Put back, but cleared by the app since it last showed: it
            // stays gone, and whether it is open is the computer's to say.
            Ok(Posted::Cleared) if raise.silent => {
                ledger.book.cleared_by_app(std::slice::from_ref(&note.tag));
            }
            Ok(Posted::NoRoom) => no_room.push(raise),
            Ok(Posted::Cleared) => unshown.push(note.tag),
            Ok(Posted::Off) => off.push(raise),
            Err(error) => {
                tracing::warn!(%error, "alerts: Android did not show an alert");
                unshown.push(note.tag);
            }
        }
    }
    // Turned off here: counted as raised, never shown, and not told.
    for raise in &off {
        if let Some(seen) = ledger.book.computers.get_mut(&raise.computer.profile_id) {
            seen.raised.remove(&raise.alert.attention_id);
            seen.missing.remove(&raise.alert.attention_id);
        }
    }
    ledger.book.unraise(&unshown);
    ledger.book.hold(no_room);
    recount(&mut ledger.book, shade, &shown);
    if let Some(note) = ledger.book.calm_note(&shown)
        && let Err(error) = shade.post(&note, None)
    {
        tracing::warn!(%error, "alerts: Android did not show that alerts are calmer");
    }
    match ledger.book.more(&shown) {
        // The note is the watcher's own; the app never clears it.
        Some(Ok(note)) => {
            if let Err(error) = shade.post(&note, None) {
                tracing::warn!(%error, "alerts: Android did not show how many alerts wait");
            }
        }
        Some(Err(tag)) => shade.cancel(tag),
        None => {}
    }
    ledger.book.snapshot(&shade.shown());
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
    /// Its type is off here.
    Off(Raise),
}

/// Where its project's alerts show as one, `raise` shows on its own if it
/// is the first; from the second, one note counts them in place of each.
/// The note alerts again for one that needs the person, and otherwise
/// changes its count quietly. One the person swiped away counts no longer.
/// `shown` is the tags showing, kept as they change.
fn fold(
    book: &mut Book,
    shade: &dyn Shade,
    raise: Raise,
    shown: &mut Vec<String>,
    hush: bool,
) -> Folding {
    if !raise.alert.bundle {
        return Folding::Alone(raise);
    }
    let profile_id = raise.computer.profile_id.clone();
    let Some(seen) = book.computers.get_mut(&profile_id) else {
        return Folding::Alone(raise);
    };
    if raise.note(Detail::Minimal).is_some_and(|mut n| {
        n.silent = false;
        shade.effective(&n) == Tier::Off
    }) {
        return Folding::Off(raise);
    }
    let (id, project_id) = (raise.alert.attention_id, raise.alert.project_id);
    let needs_you = category(&raise.alert) == Some(Category::NeedsYou);
    let note_tag = folded_tag(&profile_id, &project_id);
    // Keep the last accepted membership until posting accepts this candidate.
    let mut fold = seen
        .folds
        .get(&project_id)
        .cloned()
        .unwrap_or_else(|| Fold {
            project: String::new(),
            computer: String::new(),
            members: BTreeMap::new(),
            folded: false,
            counted: 0,
            types: BTreeMap::new(),
        });
    let kind = alert_type(&raise.alert);
    fold.project.clone_from(&raise.alert.project);
    fold.computer.clone_from(&raise.computer.name);
    let folded = fold.folded && shown.contains(&note_tag);
    fold.members.retain(|m, _| {
        *m != id && seen.raised.contains(m) && (folded || shown.contains(&tag(&profile_id, m)))
    });
    fold.types.retain(|m, _| fold.members.contains_key(m));
    fold.types.insert(id, (kind, want(&raise.alert, kind)));
    fold.members.insert(id, needs_you);
    if fold.members.len() > MAX_SHOWING {
        return Folding::NoRoom(raise);
    }
    if !folded && fold.members.len() == 1 {
        fold.folded = false;
        seen.folds.insert(project_id, fold);
        return Folding::Alone(raise);
    }
    let mut note = fold.note(shade, &profile_id, &project_id);
    note.silent = raise.silent || hush || (folded && !needs_you);
    let realert = folded && needs_you && !note.silent && shade.effective(&note) == Tier::Alert;
    match shade.post_member(&note, raise.since, &raise.tag(), realert) {
        Ok(outcome @ (Posted::Shown | Posted::Sounded)) => {
            fold.folded = true;
            fold.counted = fold.members.len();
            book.computers
                .get_mut(&profile_id)
                .unwrap()
                .folds
                .insert(project_id, fold);
            *shown = shade.shown();
            book.posted(&note, outcome, shade.clears(), Timestamp::now().millis());
            Folding::Shown
        }
        Ok(Posted::NoRoom) => Folding::NoRoom(raise),
        Ok(Posted::Off) => Folding::Off(raise),
        Ok(Posted::Cleared) | Err(_) => Folding::Failed(raise),
    }
}

/// Each project's note counts what is still raised here: one answered or
/// turned off leaves the count, quietly, and the note goes with the last.
/// A note the person swiped away, or the first alert's own, counts none
/// from then on.
fn recount(book: &mut Book, shade: &dyn Shade, shown: &[String]) {
    let mut posts = Vec::new();
    for (profile_id, seen) in &mut book.computers {
        let raised = &seen.raised;
        seen.folds.retain(|project_id, fold| {
            fold.members.retain(|m, _| raised.contains(m));
            fold.types.retain(|m, _| fold.members.contains_key(m));
            let note_tag = folded_tag(profile_id, project_id);
            if fold.members.is_empty() {
                if fold.folded {
                    shade.cancel(&note_tag);
                }
                return false;
            }
            if !fold.folded {
                return fold
                    .members
                    .keys()
                    .any(|m| shown.contains(&tag(profile_id, m)));
            }
            if !shown.contains(&note_tag) {
                return false;
            }
            if fold.counted != fold.members.len() {
                let mut note = fold.note(shade, profile_id, project_id);
                note.silent = true;
                posts.push((profile_id.clone(), *project_id, note));
            }
            true
        });
    }
    for (profile, project, note) in posts {
        match shade.post(&note, None) {
            Ok(outcome @ (Posted::Shown | Posted::Sounded)) => {
                if let Some(fold) = book
                    .computers
                    .get_mut(&profile)
                    .and_then(|s| s.folds.get_mut(&project))
                {
                    fold.counted = fold.members.len();
                }
                book.posted(&note, outcome, shade.clears(), Timestamp::now().millis());
            }
            Ok(Posted::Off) => {
                shade.cancel(&note.tag);
                if let Some(seen) = book.computers.get_mut(&profile)
                    && let Some(fold) = seen.folds.remove(&project)
                {
                    for id in fold.members.keys() {
                        seen.hush(*id);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "alerts: Android did not show a project's new count")
            }
            _ => {}
        }
    }
}

/// The computers whose alerts the app's screen shows, while it shows them:
/// the one on screen, or, while the app shows All, every one it is
/// connected to.
static VIEWING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// The app's screen began, `on`, or stopped showing the alerts of a
/// computer. A connection the app keeps to another computer, as for an
/// unfinished hand-off, shows none of that one's alerts unless the app
/// says so. The app and the watcher raise an alert under one tag, so a
/// change on the way never shows one twice.
pub fn viewing(profile_id: &str, on: bool) {
    let mut viewing = lock(&VIEWING);
    if on {
        viewing.insert(profile_id.to_owned());
    } else {
        viewing.remove(profile_id);
    }
}

/// The computers the app's screen is in front, unlocked, showing and
/// connected to, and so shows the alerts of itself.
async fn in_app(shade: &dyn Shade) -> BTreeSet<String> {
    let mut showing = BTreeSet::new();
    if !shade.on_screen() {
        return showing;
    }
    let ids = lock(&VIEWING).clone();
    for id in ids {
        if app_connected(&id).await {
            showing.insert(id);
        }
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
            sign_in: None,
            new_version: None,
            agent_update: None,
            alert_type: typed(kind),
            project_tier: None,
            follow: false,
        }
    }
    /// The type a computer labels an alert of `kind` with.
    fn typed(kind: AttentionKind) -> Option<AlertType> {
        Some(match kind {
            AttentionKind::Question => AlertType::Question,
            AttentionKind::Approval => AlertType::Approval,
            AttentionKind::Completed => AlertType::Finished,
            AttentionKind::Failure => AlertType::Failed,
            AttentionKind::ConnectionIssue => return None,
            _ => AlertType::Decision,
        })
    }
    /// Fits `raises` in the shade, counting those without room as the
    /// watcher does.
    fn fit_hold(
        book: &mut Book,
        raises: Vec<Raise>,
        shown: &[String],
        in_app: &BTreeSet<String>,
    ) -> Vec<Raise> {
        let (post, refused) = book.fit(raises, shown, in_app);
        book.hold(refused);
        post
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
            handled: true,
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
        /// This phone's choice per type; every type alerts unless set.
        tiers: Mutex<BTreeMap<AlertType, Tier>>,
        /// How many posts made a sound.
        sounds: std::sync::atomic::AtomicUsize,
        /// Each showing alert's group.
        groups: Mutex<BTreeMap<String, String>>,
        /// The groups with a summary showing.
        summaries: Mutex<BTreeSet<String>>,
        /// Clears a group's summary once fewer than two of its alerts are
        /// left, as the app once did; Android then clears the rest with it.
        old_summaries: std::sync::atomic::AtomicBool,
    }
    impl FakeShade {
        fn post_note(
            &self,
            note: &Note,
            since: Option<i64>,
            replacements: &[String],
            realert: bool,
        ) -> Result<Posted> {
            assert!(LEDGER.try_lock().is_err());
            if self.refuse.load(Ordering::SeqCst) {
                return Err(Error::Native("blocked".into()));
            }
            if lock(&self.racing).as_ref() == Some(&note.tag) {
                self.cancel(&note.tag);
            }
            if let Some(since) = since
                && lock(&self.cleared)
                    .iter()
                    .enumerate()
                    .any(|(n, t)| *t == note.tag && n as i64 + 1 > since)
            {
                return Ok(Posted::Cleared);
            }
            let (wanted, loud) = self.tier_of(note);
            if wanted == Tier::Off {
                return Ok(Posted::Off);
            }
            let mut shown = lock(&self.shown);
            if note.tag != MORE
                && !shown.contains(&note.tag)
                && (self.full.load(Ordering::SeqCst)
                    || shown
                        .iter()
                        .filter(|t| *t != MORE && !replacements.contains(t))
                        .count()
                        >= MAX_SHOWN)
            {
                return Ok(Posted::NoRoom);
            }
            let replacing = shown.contains(&note.tag);
            if !replacing {
                shown.push(note.tag.clone());
            }
            if let Some(group) = &note.group {
                let mut groups = lock(&self.groups);
                groups.insert(note.tag.clone(), group.clone());
                if groups.values().filter(|g| *g == group).count() >= 2 {
                    lock(&self.summaries).insert(group.clone());
                }
            }
            lock(&self.posted).push(note.clone());
            if loud && wanted == Tier::Alert && !note.silent && (!replacing || realert) {
                self.sounds.fetch_add(1, Ordering::SeqCst);
                return Ok(Posted::Sounded);
            }
            Ok(Posted::Shown)
        }
        fn tier(&self, alert_type: AlertType, tier: Tier) {
            lock(&self.tiers).insert(alert_type, tier);
        }
        fn sounded(&self) -> usize {
            self.sounds.load(std::sync::atomic::Ordering::SeqCst)
        }
        /// The person swipes it away: it goes, with no clear of the app's.
        fn swipe(&self, tag: &str) {
            lock(&self.shown).retain(|t| t != tag);
        }
        /// Android removes it for a reason of its own, not the person's.
        fn drop_quietly(&self, tag: &str) {
            lock(&self.shown).retain(|t| t != tag);
        }
        /// As Android's side: a note with no type shows under the type its
        /// category suggests, as the app's older alerts did, at that type's
        /// recommended choice unless set; Øphio's own notes, silently.
        fn tier_of(&self, note: &Note) -> (Tier, bool) {
            let device =
                |t: AlertType, unset: Tier| lock(&self.tiers).get(&t).copied().unwrap_or(unset);
            match (note.alert_type, note.category) {
                (Some(t), _) => (note.want.unwrap_or(device(t, Tier::Alert)), true),
                (None, Category::Notice) => (Tier::Silent, false),
                (None, Category::Finished) => (
                    device(AlertType::Finished, AlertType::Finished.default_tier()),
                    true,
                ),
                (None, Category::NeedsYou) => (device(AlertType::Decision, Tier::Alert), true),
            }
        }
    }
    impl Shade for FakeShade {
        fn post_member(
            &self,
            note: &Note,
            since: i64,
            member: &str,
            realert: bool,
        ) -> Result<Posted> {
            if lock(&self.racing)
                .as_deref()
                .is_some_and(|t| t == member || t == note.tag)
            {
                self.cancel(lock(&self.racing).as_deref().unwrap());
            }
            if lock(&self.cleared)
                .iter()
                .enumerate()
                .any(|(n, t)| (t == member || *t == note.tag) && n as i64 + 1 > since)
            {
                return Ok(Posted::Cleared);
            }
            let replacements = if note.tag != member
                && member.split_once('/').is_some_and(|(profile, id)| {
                    id.parse::<AttentionId>().is_ok()
                        && note.tag.starts_with(&format!("{profile}/{FOLDED}"))
                        && note.handles.iter().any(|t| t == member)
                }) {
                note.handles.as_slice()
            } else {
                &[]
            };
            let posted = self.post_note(note, Some(self.clears()), replacements, realert)?;
            if matches!(posted, Posted::Shown | Posted::Sounded) {
                let shown = self.shown();
                for tag in replacements.iter().filter(|t| shown.contains(t)) {
                    self.cancel(tag);
                }
            }
            Ok(posted)
        }
        fn post(&self, note: &Note, since: Option<i64>) -> Result<Posted> {
            self.post_note(note, since, &[], false)
        }
        fn cancel(&self, tag: &str) {
            lock(&self.cleared).push(tag.to_owned());
            lock(&self.shown).retain(|t| t != tag);
            // The app's summary of the group goes when none of it is left,
            // or, as it once did, when fewer than two are; Android clears
            // what is left of a group with its summary.
            let Some(group) = lock(&self.groups).remove(tag) else {
                return;
            };
            let left: Vec<String> = (lock(&self.groups).iter())
                .filter(|(_, g)| **g == group)
                .map(|(t, _)| t.clone())
                .collect();
            let old = self.old_summaries.load(std::sync::atomic::Ordering::SeqCst);
            if (left.is_empty() || (old && left.len() < 2)) && lock(&self.summaries).remove(&group)
            {
                for t in &left {
                    lock(&self.groups).remove(t);
                    lock(&self.shown).retain(|s| s != t);
                }
            }
        }
        fn effective(&self, note: &Note) -> Tier {
            match self.tier_of(note).0 {
                Tier::Off => Tier::Off,
                _ if note.silent => Tier::Silent,
                tier => tier,
            }
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
    fn a_sign_in_alert_names_the_agent_and_computer_and_opens_its_sign_in() {
        let computer = Computer {
            profile_id: "phone".into(),
            name: "Desk".into(),
        };
        let mut alert = alert(AttentionKind::WaitingForYou, 1);
        alert.project = String::new();
        alert.task = String::new();
        alert.headline = Some("Claude isn't signed in on studio".into());
        alert.sign_in = Some(Box::new(protocol::alerts::SignInAlert {
            account_id: "claude".into(),
            service: "Claude".into(),
        }));
        let note = note(&computer, &alert, Detail::TaskTitle).unwrap();
        // The phone's own name for the computer, as every other alert has.
        assert_eq!(note.title, "Claude isn't signed in on Desk");
        assert_eq!(note.body, "Tap to sign in again.");
        assert_eq!(note.category, Category::NeedsYou);
        assert_eq!(
            note.route,
            serde_json::json!({ "kind": "sign_in", "profileId": "phone", "accountId": "claude" })
        );
        assert!(note.question.is_none());
    }

    #[test]
    fn a_new_version_alert_names_the_version_and_computer_and_opens_versions() {
        let computer = Computer {
            profile_id: "phone".into(),
            name: "Desk".into(),
        };
        let mut alert = alert(AttentionKind::Completed, 1);
        alert.project_id = "00000000-0000-0000-0000-000000000000".parse().unwrap();
        alert.task_id = "00000000-0000-0000-0000-000000000000".parse().unwrap();
        alert.project.clear();
        alert.task.clear();
        alert.alert_type = None;
        alert.new_version = Some(Box::new(protocol::alerts::NewVersionAlert {
            version: "0.2.0".into(),
        }));
        let note = note(&computer, &alert, Detail::TaskTitle).unwrap();
        assert_eq!(note.title, "Øphio 0.2.0 is available");
        assert_eq!(note.body, "Desk · Tap to see what’s new and how to update.");
        assert_eq!(note.category, Category::Finished);
        assert_eq!(category(&alert), Some(Category::Finished));
        assert_eq!(note.alert_type, Some(AlertType::NewVersion));
        assert_eq!(alert_type(&alert), AlertType::NewVersion);
        assert_eq!(
            note.route,
            serde_json::json!({ "kind": "new_version", "profileId": "phone" })
        );
        assert_eq!(note.tag, tag("phone", &alert.attention_id));
        assert!(note.group.is_none());
        assert!(note.group_title.is_none());
        assert!(note.question.is_none());
        assert!(note.handles.is_empty());
        assert!(!note.silent);
    }

    #[test]
    fn the_watcher_asks_for_new_versions_only_when_the_computer_checks_releases() {
        assert!(!supports_new_version(&[]));
        assert!(!supports_new_version(&["reports_v1".into()]));
        assert!(supports_new_version(&[
            protocol::releases::RELEASE_CHECK_FEATURE.into()
        ]));
    }

    #[test]
    fn agent_update_watch_is_gated_by_the_host_feature() {
        assert!(!supports_agent_updates(&[]));
        assert!(!supports_agent_updates(&["reports_v1".into()]));
        assert!(supports_agent_updates(&[AGENT_UPDATE_FEATURE.into()]));
    }

    fn agent_update(kind: AgentUpdateNoticeKind) -> Alert {
        let mut alert = alert(AttentionKind::Completed, 1);
        alert.project.clear();
        alert.task.clear();
        alert.alert_type = None;
        alert.agent_update = Some(Box::new(protocol::agent_updates::AgentUpdateNotice {
            harness: HarnessKind::ClaudeCode,
            kind,
            version: Some("2.1.3".into()),
            models: vec!["claude-fable-5-2".into()],
        }));
        alert
    }

    #[test]
    fn agent_news_opens_accounts_without_task_details_or_release_text() {
        for (kind, title, body) in [
            (
                AgentUpdateNoticeKind::Behind,
                "Claude Code has an update",
                "Version 2.1.3 · An update is available",
            ),
            (
                AgentUpdateNoticeKind::UpdateForModels,
                "Claude Fable 5.2 needs newer Claude Code",
                "Version 2.1.3 · An update announces Claude Fable 5.2. Availability depends on your account",
            ),
            (
                AgentUpdateNoticeKind::NewModelsAvailable,
                "Claude Fable 5.2 is now available on Studio",
                "Version 2.1.3 · The updated agent now lists Claude Fable 5.2 for your account",
            ),
        ] {
            let mut alert = agent_update(kind);
            alert.headline = Some("untrusted release text".into());
            alert.task = "private task title".into();
            let note = note(&computer(), &alert, Detail::TaskTitle).unwrap();
            assert_eq!(note.title, title);
            assert!(note.body.contains(body), "{}", note.body);
            assert!(!note.body.contains("private task"));
            assert!(!note.body.contains("untrusted"));
            assert!(!note.body.contains("claude-fable-5-2"));
            assert_eq!(note.alert_type, Some(AlertType::AgentUpdate));
            assert_eq!(alert_type(&alert), AlertType::AgentUpdate);
            assert_eq!(category(&alert), Some(Category::Finished));
            assert_eq!(
                note.route,
                serde_json::json!({ "kind": "agent_updates", "profileId": "p1" })
            );
            assert!(note.question.is_none());
            assert!(note.group.is_none());
            assert!(note.handles.is_empty());
        }
    }

    #[test]
    fn model_names_are_bounded_and_unknown_identifiers_are_not_echoed() {
        let mut alert = agent_update(AgentUpdateNoticeKind::NewModelsAvailable);
        alert.agent_update.as_mut().unwrap().models = vec![
            "claude-fable-5-2".into(),
            "claude-sonnet-5-5".into(),
            "claude-opus-5-5".into(),
            "claude-haiku-5-5".into(),
        ];
        let named = note(&computer(), &alert, Detail::Minimal).unwrap();
        assert_eq!(named.title, "New models in Claude Code");
        assert!(
            named
                .body
                .contains("Claude Fable 5.2, Claude Sonnet 5.5, Claude Opus 5.5 and 1 more")
        );
        assert!(!named.body.contains("Claude Haiku"));
        alert.agent_update.as_mut().unwrap().models = vec!["unexpected\nrelease text".into()];
        let note = note(&computer(), &alert, Detail::Minimal).unwrap();
        assert_eq!(note.title, "New models in Claude Code");
        assert!(note.body.contains("1 new model"));
        assert!(!note.body.contains("release text"));
    }

    #[test]
    fn agent_news_is_not_treated_as_an_unknown_future_type_and_is_removed_when_resolved() {
        let alert = agent_update(AgentUpdateNoticeKind::Behind);
        let id = alert.attention_id;
        let mut book = Book::default();
        book.begin("p1", Timestamp(0));
        let first = rx(&mut book, open(&[alert], 1), false, &[]);
        assert_eq!(first.post.len(), 1);
        assert_eq!(first.post[0].alert.project_tier, None);
        let gone = rx(
            &mut book,
            open_gone(&[], 2, vec![id]),
            false,
            &[tag("p1", &id)],
        );
        assert_eq!(gone.cancel, vec![tag("p1", &id)]);
        assert!(gone.post.is_empty());
    }

    #[test]
    fn agent_update_version_is_bounded_for_native_notifications() {
        let mut alert = agent_update(AgentUpdateNoticeKind::Behind);
        alert.agent_update.as_mut().unwrap().version =
            Some(format!("{}\nprivate", "X".repeat(200)));
        let note = note(&computer(), &alert, Detail::TaskTitle).unwrap();
        assert!(note.body.contains(&"X".repeat(40)));
        assert!(!note.body.contains(&"X".repeat(41)));
        assert!(!note.body.contains("private"));
    }

    #[test]
    fn a_new_version_is_not_treated_as_an_unknown_future_type() {
        let mut alert = alert(AttentionKind::Completed, 1);
        alert.alert_type = None;
        alert.new_version = Some(Box::new(protocol::alerts::NewVersionAlert {
            version: "0.2.0".into(),
        }));
        let frame = future_types(&mut Book::default(), &computer(), open(&[alert], 1));
        let AlertServerFrame::Open { alerts, .. } = frame else {
            panic!("expected the open alerts");
        };
        assert_eq!(alerts[0].project_tier, None);
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
    fn an_alert_is_raised_and_saved_under_one_lock_and_turning_off_keeps_it() {
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
            alert: Box::new(shown.clone()),
        });
        shade
            .refuse
            .store(true, std::sync::atomic::Ordering::SeqCst);
        frame(AlertServerFrame::Raised {
            alert: Box::new(refused.clone()),
        });
        // Saved as raised only once Android showed it.
        let book = saved(dir.path());
        assert!(book.computers["p1"].raised.contains(&shown.attention_id));
        assert!(
            !book.computers["p1"]
                .notified
                .contains(&refused.attention_id)
        );
        // Turning off alerts once the app is closed keeps what shows, and
        // the watcher still raises alerts while the app runs.
        configure(Settings {
            enabled: false,
            ..ON
        })
        .unwrap();
        shade
            .refuse
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let later = alert(AttentionKind::Question, 4);
        frame(AlertServerFrame::Raised {
            alert: Box::new(later.clone()),
        });
        assert_eq!(
            shade.shown(),
            vec![
                tag("p1", &shown.attention_id),
                tag("p1", &later.attention_id)
            ]
        );
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
                alert: Box::new(question.clone()),
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
                alert: Box::new(waiting.clone()),
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
        frame(
            AlertServerFrame::Raised {
                alert: Box::new(later),
            },
            true,
        );
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
            frame(AlertServerFrame::Raised {
                alert: Box::new(a.clone()),
            });
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
        // Urgent members alert in place, rather than clearing the summary.
        assert_eq!(cleared(&note_tag), 0);
        assert_eq!(last().title, "Site has 3 notifications");
        assert_eq!(last().body, "Studio · 1 need you, 2 to look at");
        assert_eq!(last().category, Category::NeedsYou);
        let fourth = folds(AttentionKind::Completed, 5);
        raise(&fourth);
        assert_eq!(cleared(&note_tag), 0);
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
        assert!(
            frame(
                old,
                AlertServerFrame::Raised {
                    alert: Box::new(late)
                }
            )
            .is_none()
        );
        assert!(shade.shown().is_empty());
        connection_ended("p1", new).unwrap();
        assert!(!live());
        lock(&LEDGER).take();
    }

    #[test]
    fn alerts_beyond_the_shades_room_are_counted_and_never_rise_as_it_frees() {
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
        // The oldest fill the shade; a note counts the rest, which count
        // as raised.
        let mut expected = tags_of(&alerts[..MAX_SHOWN]);
        expected.push(MORE.into());
        assert_eq!(shade.shown(), expected);
        assert_eq!(
            saved(dir.path()).computers["p1"].notified.len(),
            alerts.len()
        );
        // The list again raises none of them twice.
        assert!(frame(open(&alerts, 110)));
        assert_eq!(shade.shown(), expected);
        // The person swipes two away: none of those without room rises in
        // their place, now or once the list comes again.
        for t in &expected[..2] {
            shade.cancel(t);
        }
        let posted = lock(&shade.posted).len();
        assert!(frame(open(&alerts, 120)));
        assert_eq!(lock(&shade.posted).len(), posted);
        // Nor after a restart, which forgets the count.
        with_book(|book| book.restarted()).unwrap();
        frame(open(&alerts, 130));
        assert_eq!(lock(&shade.posted).len(), posted);
        for a in &alerts[MAX_SHOWN..] {
            assert!(saved(dir.path()).computers["p1"].knows(&a.attention_id));
        }
        lock(&LEDGER).take();
    }

    #[test]
    fn the_note_of_those_without_room_goes_once_they_are_answered() {
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
            .unwrap()
        };
        frame(open(&[], 1));
        let alerts: Vec<Alert> = (0..MAX_SHOWN as i64 + 1)
            .map(|i| alert(AttentionKind::Completed, 10 + i))
            .collect();
        frame(open(&alerts, 100));
        assert!(shade.shown().contains(&MORE.to_owned()));
        // The one without room is answered: the note goes, and nothing rises.
        frame(AlertServerFrame::Resolved {
            attention_id: alerts[MAX_SHOWN].attention_id,
        });
        assert!(!shade.shown().contains(&MORE.to_owned()));
        assert_eq!(shade.shown().len(), MAX_SHOWN);
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
            let frame = AlertServerFrame::Raised {
                alert: Box::new(a.clone()),
            };
            let changes = rx(&mut book, frame, false, &full);
            posted.extend(fit_hold(&mut book, changes.post, &full, &BTreeSet::new()));
        }
        assert!(posted.is_empty());
        let Some(Ok(note)) = book.more(&full) else {
            panic!("a note of those waiting")
        };
        assert_eq!(note.title, "2 more alerts");
        assert_eq!(note.category, Category::NeedsYou);
        assert_eq!(note.route, serde_json::Value::Null);
        // One more needs the person, so it alerts.
        assert!(!note.silent);
        // Unchanged, it is not posted again.
        assert!(book.more(&full).is_none());
        // One more to look at changes it silently.
        let changes = rx(
            &mut book,
            AlertServerFrame::Raised {
                alert: Box::new(alert(AttentionKind::Completed, 4)),
            },
            false,
            &full,
        );
        assert!(fit_hold(&mut book, changes.post, &full, &BTreeSet::new()).is_empty());
        let Some(Ok(note)) = book.more(&full) else {
            panic!("a new count")
        };
        assert!(note.silent);
        // Swiped away, it stays gone as the count falls.
        let answered = waiting[0].attention_id;
        rx(
            &mut book,
            AlertServerFrame::Resolved {
                attention_id: answered,
            },
            false,
            &full,
        );
        assert!(book.more(&full).is_none());
    }

    #[test]
    fn what_had_no_room_never_rises_and_leaves_the_count_once_seen() {
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
                AlertServerFrame::Raised {
                    alert: Box::new(a.clone()),
                },
                false,
                &full,
            );
            assert!(fit_hold(&mut book, changes.post, &full, &BTreeSet::new()).is_empty());
        }
        // Room frees while the app shows the computer: nothing rises.
        let in_app = BTreeSet::from(["p1".to_owned()]);
        assert!(fit_hold(&mut book, Vec::new(), &[], &in_app).is_empty());
        // The app shows the question, and the count drops it; once the app
        // leaves, nothing rises either: both count as raised.
        book.seen_in_app("p1", &[question.attention_id]);
        assert_eq!(book.computers["p1"].held.len(), 1);
        assert!(fit_hold(&mut book, Vec::new(), &[], &BTreeSet::new()).is_empty());
        assert!(book.computers["p1"].knows(&done.attention_id));
    }

    #[test]
    fn the_watch_names_what_the_phone_shows_or_keeps_from_that_computer() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let raised = alert(AttentionKind::Question, 2);
        rx(
            &mut book,
            AlertServerFrame::Raised {
                alert: Box::new(raised.clone()),
            },
            false,
            &[],
        );
        let full: Vec<String> = (0..MAX_SHOWN).map(|i| format!("p9/{i}")).collect();
        let held = alert(AttentionKind::Completed, 3);
        let changes = rx(
            &mut book,
            AlertServerFrame::Raised {
                alert: Box::new(held.clone()),
            },
            false,
            &full,
        );
        fit_hold(&mut book, changes.post, &full, &BTreeSet::new());
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
        // recorded as the watcher's: a cleared shade doesn't raise it again.
        book.raised_by_app("p1", by_app);
        assert!(book.computers["p1"].raised.contains(&by_app));
        book.started("a");
        book.started("b");
        assert!(book.computers["p1"].knows(&by_app));
        assert!(book.computers["p1"].raised.is_empty());
        // At most so many.
        let many: Vec<String> = (0..MAX_SHOWING + 5)
            .map(|_| tag("p1", &AttentionId::new()))
            .collect();
        assert_eq!(book.showing("p1", &many).len(), MAX_SHOWING);
    }

    #[test]
    fn what_had_no_room_leaves_once_the_computer_says_it_was_answered_while_away() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let none = BTreeSet::new();
        let full: Vec<String> = (0..MAX_SHOWN).map(|i| format!("p9/{i}")).collect();
        let approval = alert(AttentionKind::Approval, 2);
        let frame = AlertServerFrame::Raised {
            alert: Box::new(approval.clone()),
        };
        let changes = rx(&mut book, frame, false, &full);
        assert!(fit_hold(&mut book, changes.post, &full, &none).is_empty());
        book.ended("p1");
        // Room frees, as another computer's alerts are answered, while this
        // one is away: nothing rises.
        assert!(fit_hold(&mut book, Vec::new(), &[], &none).is_empty());
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
        let posted = fit_hold(&mut book, changes.post, &[], &none);
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
            alert: Box::new(question.clone()),
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
            alert: Box::new(other.clone()),
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
    fn only_the_computers_the_app_shows_wait_for_it() {
        let _one = lock(&ONE_LEDGER);
        let shown = || lock(&VIEWING).iter().cloned().collect::<Vec<_>>();
        viewing("a", true);
        // Switching: the one shown now stays when the one left goes.
        viewing("b", true);
        viewing("a", false);
        assert_eq!(shown(), ["b"]);
        // All: every computer the app shows waits for it, each until it goes.
        viewing("a", true);
        viewing("c", true);
        assert_eq!(shown(), ["a", "b", "c"]);
        viewing("b", false);
        viewing("b", false);
        assert_eq!(shown(), ["a", "c"]);
        viewing("a", false);
        viewing("c", false);
        assert!(shown().is_empty());
    }

    #[test]
    fn an_alert_android_had_no_room_for_after_all_is_counted_and_never_rises() {
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
            alert: Box::new(question.clone()),
        }));
        assert_eq!(shade.shown(), vec![MORE.to_owned()]);
        assert!(saved(dir.path()).computers["p1"].knows(&question.attention_id));
        // Room frees: it doesn't rise, here or from the next list.
        shade.full.store(false, std::sync::atomic::Ordering::SeqCst);
        update(
            current_watcher(),
            &shade,
            &none,
            |_, _| Changes::default(),
            |_| (),
        )
        .unwrap();
        frame(open(std::slice::from_ref(&question), 3));
        assert_eq!(shade.shown(), vec![MORE.to_owned()]);
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
            alert: Box::new(question.clone()),
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
            alert: Box::new(question.clone()),
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
        assert!(room.is_some());
        let raised = tag("p1", &question.attention_id);
        assert!(!lock(&shade.posted).iter().any(|n| n.tag == raised));
        assert!(!shade.shown().contains(&raised));
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
                alert: Box::new(question.clone()),
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
            alert: Box::new(approval.clone()),
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
                alert: Box::new(question.clone()),
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
                alert: Box::new(question.clone()),
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
        // With alerts off once the app closes, watching still begins: the
        // app running raises them.
        let mut off = Book {
            settings: Some(Settings {
                enabled: false,
                detail: Detail::Minimal,
            }),
            ..Book::default()
        };
        off.begin("p1", Timestamp(1));
        assert!(off.computers.contains_key("p1"));
    }

    #[test]
    fn raised_once_cleared_when_resolved() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let question = alert(AttentionKind::Question, 2);
        let raised = AlertServerFrame::Raised {
            alert: Box::new(question.clone()),
        };
        let first = rx(&mut book, raised.clone(), false, &[]);
        assert_eq!(first.post.len(), 1);
        let note = first.post[0].note(book.detail()).unwrap();
        assert_eq!(note.title, "Question from Codex");
        assert_eq!(note.category, Category::NeedsYou);
        assert_eq!(note.body, "Studio · Site");
        // A project's notifications stack together.
        let project = question.project_id.as_uuid();
        assert_eq!(note.group, Some(format!("p1/{project}")));
        assert_eq!(note.alert_type, Some(AlertType::Question));
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
            let frame = AlertServerFrame::Raised {
                alert: Box::new(a.clone()),
            };
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
            alert: Box::new(approval.clone()),
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
    fn a_swiped_alert_never_comes_back_and_one_still_showing_returns_silently() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        with_book(|book| book.started("4:1")).unwrap();
        let shade = FakeShade::default();
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, frame, false, shown),
                |_| (),
            )
            .unwrap()
        };
        frame(open(&[], 1));
        let (kept, swiped, seen) = (
            alert(AttentionKind::Approval, 2),
            alert(AttentionKind::Question, 3),
            alert(AttentionKind::WaitingForYou, 4),
        );
        for a in [&kept, &swiped, &seen] {
            frame(AlertServerFrame::Raised {
                alert: Box::new(a.clone()),
            });
        }
        assert_eq!(shade.shown().len(), 3);
        // The person swipes one away, which Android tells the app, and sees
        // another in the app.
        shade.swipe(&tag("p1", &swiped.attention_id));
        dismissed(dir.path(), &tag("p1", &swiped.attention_id), &shade).unwrap();
        let cleared = handled("p1", &[seen.attention_id]).unwrap();
        for t in &cleared {
            shade.cancel(t);
        }
        let still_open = [kept.clone(), swiped.clone(), seen.clone()];
        let posted = |shade: &FakeShade| lock(&shade.posted).len();
        // A reconnect brings nothing back, and records the swipe.
        let before = posted(&shade);
        frame(open(&still_open, 5));
        assert_eq!(posted(&shade), before);
        let book = saved(dir.path());
        assert!(book.computers["p1"].unsent.contains(&swiped.attention_id));
        assert!(book.computers["p1"].unsent.contains(&seen.attention_id));
        // A restart of the app's process, with the shade as it was: nothing.
        with_book(|book| {
            book.restarted();
            book.started("4:1");
        })
        .unwrap();
        frame(open(&still_open, 6));
        assert_eq!(posted(&shade), before);
        // An update clears the shade. Read again from storage, as the new
        // app's process does, only the one still showing comes back, and
        // silently; the swiped and the seen ones stay gone.
        lock(&shade.shown).clear();
        let mut book: Book = saved(dir.path());
        book.restarted();
        book.started("4:2");
        with_book(|b| *b = book).unwrap();
        frame(open(&still_open, 7));
        let back: Vec<Note> = lock(&shade.posted)[before..].to_vec();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].tag, tag("p1", &kept.attention_id));
        assert!(back[0].silent);
        // Again: nothing more.
        frame(open(&still_open, 8));
        assert_eq!(posted(&shade), before + 1);
        // A restart of the phone, after the person swiped the one put back:
        // nothing at all.
        shade.swipe(&tag("p1", &kept.attention_id));
        dismissed(dir.path(), &tag("p1", &kept.attention_id), &shade).unwrap();
        frame(open(&still_open, 9));
        with_book(|book| {
            book.restarted();
            book.started("5:2");
        })
        .unwrap();
        lock(&shade.shown).clear();
        frame(open(&still_open, 10));
        assert_eq!(posted(&shade), before + 1);
        // Something new still alerts.
        let new = alert(AttentionKind::Question, 11);
        frame(AlertServerFrame::Raised {
            alert: Box::new(new.clone()),
        });
        assert_eq!(shade.shown(), vec![tag("p1", &new.attention_id)]);
        lock(&LEDGER).take();
    }

    /// As `update` would, at `now` ms on this phone's clock.
    fn missing_at(now: i64) -> Changes {
        with_book(|book| {
            let shown = Vec::new();
            let since = book.clears;
            Changes {
                post: book.missing(&shown, now, since),
                cancel: Vec::new(),
            }
        })
        .unwrap()
    }

    #[test]
    fn only_the_persons_word_handles_an_alert_and_one_android_removed_comes_back() {
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
            .unwrap()
        };
        frame(open(&[], 1));
        let question = alert(AttentionKind::Question, 2);
        frame(AlertServerFrame::Raised {
            alert: Box::new(question.clone()),
        });
        let t = tag("p1", &question.attention_id);
        // Android removes it, as for a force stop or a category replaced:
        // no delete intent, so it isn't handled and isn't told.
        shade.drop_quietly(&t);
        frame(open(std::slice::from_ref(&question), 3));
        let book = saved(dir.path());
        assert!(book.computers["p1"].unsent.is_empty());
        assert!(book.computers["p1"].raised.contains(&question.attention_id));
        // Missing a while, it goes back, silently.
        with_book(|book| {
            let seen = book.computers.get_mut("p1").unwrap();
            for at in seen.missing.values_mut() {
                *at -= MISSING_MS;
            }
        })
        .unwrap();
        frame(open(std::slice::from_ref(&question), 4));
        assert_eq!(shade.shown(), vec![t.clone()]);
        assert!(lock(&shade.posted).last().unwrap().silent);
        assert_eq!(shade.sounded(), 1);
        // Missing only a moment, as a swipe whose delete intent is on its
        // way: nothing yet.
        shade.drop_quietly(&t);
        let now = Timestamp::now().millis();
        assert!(missing_at(now).post.is_empty());
        assert!(missing_at(now + MISSING_MS / 2).post.is_empty());
        dismissed(dir.path(), &t, &shade).unwrap();
        assert!(missing_at(now + MISSING_MS * 2).post.is_empty());
        assert!(
            saved(dir.path()).computers["p1"]
                .unsent
                .contains(&question.attention_id)
        );
        lock(&LEDGER).take();
    }

    #[test]
    fn answering_one_of_a_projects_two_alerts_leaves_the_other_showing_and_unhandled() {
        for old in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let _one = ledger(dir.path());
            configure(ON).unwrap();
            let shade = FakeShade::default();
            shade
                .old_summaries
                .store(old, std::sync::atomic::Ordering::SeqCst);
            let frame = |frame: AlertServerFrame| {
                update(
                    current_watcher(),
                    &shade,
                    &BTreeSet::new(),
                    |book, shown| rx(book, frame, false, shown),
                    |_| (),
                )
                .unwrap()
            };
            frame(open(&[], 1));
            let first = alert(AttentionKind::Approval, 2);
            let mut second = alert(AttentionKind::Approval, 3);
            second.project_id = first.project_id;
            for a in [&first, &second] {
                frame(AlertServerFrame::Raised {
                    alert: Box::new(a.clone()),
                });
            }
            assert_eq!(lock(&shade.summaries).len(), 1);
            // The first is answered at the computer.
            frame(AlertServerFrame::Resolved {
                attention_id: first.attention_id,
            });
            let other = tag("p1", &second.attention_id);
            // With the summary kept while one is left, the other shows. As
            // the app once did, Android drops it with the summary: even
            // then it isn't handled, and it comes back.
            assert_eq!(shade.shown().contains(&other), !old, "old: {old}");
            let book = saved(dir.path());
            assert!(book.computers["p1"].unsent.is_empty());
            assert!(book.computers["p1"].raised.contains(&second.attention_id));
            if old {
                with_book(|book| {
                    let seen = book.computers.get_mut("p1").unwrap();
                    seen.missing.insert(second.attention_id, 0);
                })
                .unwrap();
                frame(open(std::slice::from_ref(&second), 4));
                assert!(shade.shown().contains(&other));
            }
            lock(&LEDGER).take();
        }
    }

    #[test]
    fn muting_a_project_clears_what_shows_here_without_handling_it() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let (loud, followed) = (
            alert(AttentionKind::Question, 2),
            Alert {
                follow: true,
                ..alert(AttentionKind::Completed, 3)
            },
        );
        for a in [&loud, &followed] {
            rx(
                &mut book,
                AlertServerFrame::Raised {
                    alert: Box::new(a.clone()),
                },
                false,
                &[],
            );
        }
        let muted = |a: &Alert| Alert {
            project_tier: Some(Tier::Off),
            ..a.clone()
        };
        let changes = rx(
            &mut book,
            open(&[muted(&loud), muted(&followed)], 4),
            false,
            &[],
        );
        // The muted project's goes; the task the person follows stays.
        assert_eq!(changes.cancel, vec![tag("p1", &loud.attention_id)]);
        assert!(!book.computers["p1"].raised.contains(&loud.attention_id));
        assert!(book.computers["p1"].raised.contains(&followed.attention_id));
        // Local only: nothing to tell the computer, and nothing raised again.
        assert!(book.computers["p1"].unsent.is_empty());
        assert!(
            rx(&mut book, open(std::slice::from_ref(&loud), 5), false, &[])
                .post
                .is_empty()
        );
    }

    #[test]
    fn a_full_shade_makes_room_for_one_that_alerts() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        shade.tier(AlertType::Finished, Tier::Silent);
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, frame, false, shown),
                |_| (),
            )
            .unwrap()
        };
        frame(open(&[], 1));
        let quiet: Vec<Alert> = (0..MAX_SHOWN as i64)
            .map(|i| alert(AttentionKind::Completed, 10 + i))
            .collect();
        for a in &quiet {
            frame(AlertServerFrame::Raised {
                alert: Box::new(a.clone()),
            });
        }
        assert_eq!(shade.shown().len(), MAX_SHOWN);
        // One more to look at waits behind the note.
        let another = alert(AttentionKind::Completed, 50);
        frame(AlertServerFrame::Raised {
            alert: Box::new(another.clone()),
        });
        assert!(!shade.shown().contains(&tag("p1", &another.attention_id)));
        // A question takes the oldest quiet one's place.
        let question = alert(AttentionKind::Question, 60);
        frame(AlertServerFrame::Raised {
            alert: Box::new(question.clone()),
        });
        let shown = shade.shown();
        assert!(shown.contains(&tag("p1", &question.attention_id)));
        assert!(!shown.contains(&tag("p1", &quiet[0].attention_id)));
        // The one cleared to make room isn't handled, nor put back.
        let book = saved(dir.path());
        assert!(book.computers["p1"].unsent.is_empty());
        assert!(!book.computers["p1"].raised.contains(&quiet[0].attention_id));
        lock(&LEDGER).take();
    }

    #[test]
    fn a_type_this_phone_doesnt_know_shows_silently() {
        let mut book = Book::default();
        let mut future = alert(AttentionKind::Question, 2);
        future.alert_type = None;
        // From a computer that labels types, a missing one is newer than
        // this phone: silent.
        let changes = rx(
            &mut book,
            open(std::slice::from_ref(&future), 3),
            false,
            &[],
        );
        let _ = changes;
        let changes = rx(
            &mut book,
            AlertServerFrame::Raised {
                alert: Box::new(Alert {
                    attention_id: AttentionId::new(),
                    ..future.clone()
                }),
            },
            false,
            &[],
        );
        let note = changes.post[0].note(Detail::Minimal).unwrap();
        assert_eq!(note.want, Some(Tier::Silent));
        // From an older computer, which labels none, it goes by its kind.
        let mut older = Book::default();
        let frame = AlertServerFrame::Open {
            alerts: vec![],
            now: Timestamp(1),
            gone: vec![],
            handled: false,
        };
        rx(&mut older, frame, false, &[]);
        let changes = rx(
            &mut older,
            AlertServerFrame::Raised {
                alert: Box::new(future),
            },
            false,
            &[],
        );
        let note = changes.post[0].note(Detail::Minimal).unwrap();
        assert_eq!(note.want, None);
        assert_eq!(note.alert_type, Some(AlertType::Question));
    }

    #[test]
    fn choices_from_the_old_categories_carry_over_only_when_quieter() {
        let old = |pairs: &[(&str, i32)]| -> BTreeMap<String, i32> {
            pairs.iter().map(|(c, i)| ((*c).to_owned(), *i)).collect()
        };
        // Untouched old categories change nothing.
        assert!(
            imported_tiers(&old(&[("needs-you", 4), ("finished", 3), ("updates", 3)])).is_empty()
        );
        // Finished tasks made silent: what showed there, and alerts by
        // default, becomes silent; what is already silent or off stays.
        let silent = imported_tiers(&old(&[("finished", 2)]));
        assert_eq!(silent.get(&AlertType::Failed), Some(&Tier::Silent));
        assert_eq!(silent.get(&AlertType::Finished), None);
        assert_eq!(silent.get(&AlertType::Reply), Some(&Tier::Silent));
        assert_eq!(silent.get(&AlertType::Question), None);
        // Needs you switched off: every type under it is off.
        let off = imported_tiers(&old(&[("needs-you", 0), ("updates", 0)]));
        for t in [
            AlertType::Question,
            AlertType::Approval,
            AlertType::GoAhead,
            AlertType::AppUpdate,
        ] {
            assert_eq!(off.get(&t), Some(&Tier::Off), "{t:?}");
        }
        // Already off by default: nothing to say.
        assert_eq!(off.get(&AlertType::Slow), None);
    }

    /// Ages what is missing from the shade, as ten seconds would.
    fn age_missing() {
        with_book(|book| {
            for seen in book.computers.values_mut() {
                for at in seen.missing.values_mut() {
                    *at -= MISSING_MS;
                }
            }
        })
        .unwrap();
    }

    #[test]
    fn an_answered_alert_isnt_put_back_while_its_computer_is_silent() {
        for told in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let _one = ledger(dir.path());
            configure(ON).unwrap();
            let shade = FakeShade::default();
            let laptop = Computer {
                profile_id: "p2".into(),
                name: "Laptop".into(),
            };
            let frame = |computer: &Computer, frame: AlertServerFrame| {
                update(
                    current_watcher(),
                    &shade,
                    &BTreeSet::new(),
                    |book, shown| receive(book, computer, frame, false, shown, Timestamp(0)),
                    |_| (),
                )
                .unwrap()
            };
            frame(&computer(), open(&[], 1));
            frame(&laptop, open(&[], 1));
            let question = alert(AttentionKind::Question, 2);
            frame(
                &computer(),
                AlertServerFrame::Raised {
                    alert: Box::new(question.clone()),
                },
            );
            let t = tag("p1", &question.attention_id);
            assert_eq!(shade.shown(), vec![t.clone()]);
            // The first computer's connection is dead. The person answers in
            // the app, which clears it: told to the watcher, or not.
            shade.cancel(&t);
            if told {
                cleared_by_app(std::slice::from_ref(&t)).unwrap();
            }
            // The other computer speaks twice, ten seconds apart.
            frame(
                &laptop,
                AlertServerFrame::Raised {
                    alert: Box::new(alert(AttentionKind::Completed, 3)),
                },
            );
            age_missing();
            frame(
                &laptop,
                AlertServerFrame::Raised {
                    alert: Box::new(alert(AttentionKind::Completed, 4)),
                },
            );
            assert!(!shade.shown().contains(&t), "told: {told}");
            // Nor is it handled: the computer says whether it is open.
            let book = saved(dir.path());
            assert!(book.computers["p1"].unsent.is_empty());
            assert!(!book.computers["p1"].raised.contains(&question.attention_id));
            lock(&LEDGER).take();
        }
    }

    #[test]
    fn a_swiped_project_note_handles_only_what_it_counted() {
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
            .unwrap()
        };
        frame(open(&[], 1));
        let bundled = |kind, at| Alert {
            bundle: true,
            ..alert(kind, at)
        };
        let a = bundled(AttentionKind::Question, 2);
        let b = Alert {
            project_id: a.project_id,
            ..bundled(AttentionKind::Approval, 3)
        };
        let c = Alert {
            project_id: a.project_id,
            ..bundled(AttentionKind::Approval, 4)
        };
        frame(AlertServerFrame::Raised {
            alert: Box::new(a.clone()),
        });
        frame(AlertServerFrame::Raised {
            alert: Box::new(b.clone()),
        });
        let note_tag = folded_tag("p1", &a.project_id);
        let swiped = (lock(&shade.posted).iter().rev())
            .find(|n| n.tag == note_tag)
            .unwrap()
            .handles
            .clone();
        assert_eq!(swiped.len(), 2);
        // The person swipes the note; before Android says so, C comes in.
        shade.swipe(&note_tag);
        frame(AlertServerFrame::Raised {
            alert: Box::new(c.clone()),
        });
        dismissed(dir.path(), &swiped.join("\n"), &shade).unwrap();
        let book = saved(dir.path());
        let seen = &book.computers["p1"];
        assert!(seen.unsent.contains(&a.attention_id) && seen.unsent.contains(&b.attention_id));
        assert!(!seen.unsent.contains(&c.attention_id));
        assert!(seen.raised.contains(&c.attention_id));
        lock(&LEDGER).take();
    }

    #[test]
    fn a_project_note_put_back_or_in_a_burst_is_silent_and_its_count_too() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        with_book(|book| book.started("4:1")).unwrap();
        let shade = FakeShade::default();
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, frame, false, shown),
                |_| (),
            )
            .unwrap()
        };
        frame(open(&[], 1));
        let first = Alert {
            bundle: true,
            ..alert(AttentionKind::Question, 2)
        };
        let second = Alert {
            project_id: first.project_id,
            ..first.clone()
        };
        let second = Alert {
            attention_id: AttentionId::new(),
            created_at: Timestamp(3),
            ..second
        };
        frame(AlertServerFrame::Raised {
            alert: Box::new(first.clone()),
        });
        frame(AlertServerFrame::Raised {
            alert: Box::new(second.clone()),
        });
        // The second folds the two into one note, in the same burst: silent.
        let note_tag = folded_tag("p1", &first.project_id);
        let note = |shade: &FakeShade| {
            (lock(&shade.posted).iter().rev())
                .find(|n| n.tag == note_tag)
                .cloned()
                .unwrap()
        };
        assert!(note(&shade).silent);
        assert_eq!(note(&shade).alert_type, Some(AlertType::Question));
        assert_eq!(shade.sounded(), 1);
        // An update clears the shade: what showed goes back, silently.
        lock(&shade.shown).clear();
        let mut book: Book = saved(dir.path());
        book.restarted();
        book.started("4:2");
        with_book(|b| *b = book).unwrap();
        frame(open(&[first.clone(), second.clone()], 5));
        assert!(lock(&shade.posted).iter().skip(2).all(|n| n.silent));
        assert_eq!(shade.sounded(), 1);
        lock(&LEDGER).take();
    }

    #[test]
    fn the_note_of_those_without_room_shows_as_oephios_own() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        // Finished tasks off, as recommended: the note doesn't depend on it.
        shade.tier(AlertType::Finished, Tier::Off);
        shade.full.store(true, std::sync::atomic::Ordering::SeqCst);
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, frame, false, shown),
                |_| (),
            )
            .unwrap()
        };
        frame(open(&[], 1));
        frame(AlertServerFrame::Raised {
            alert: Box::new(Alert {
                alert_type: Some(AlertType::PrReady),
                ..alert(AttentionKind::Completed, 2)
            }),
        });
        assert_eq!(shade.shown(), vec![MORE.to_owned()]);
        let note = lock(&shade.posted).last().cloned().unwrap();
        assert_eq!(note.category, Category::Notice);
        assert!(note.silent);
        lock(&LEDGER).take();
    }

    #[test]
    fn something_missing_is_looked_at_again_on_its_own_and_a_late_swipe_clears_the_copy() {
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
                |b| look_again(b, &computer()),
            )
            .unwrap()
        };
        assert_eq!(frame(open(&[], 1)), None);
        let question = alert(AttentionKind::Question, 2);
        frame(AlertServerFrame::Raised {
            alert: Box::new(question.clone()),
        });
        let t = tag("p1", &question.attention_id);
        // Gone, as for a force stop: the watcher asks to look again soon,
        // without waiting for the computer.
        shade.drop_quietly(&t);
        let again = frame(open(std::slice::from_ref(&question), 3)).expect("a look again");
        assert!(again <= Duration::from_millis(MISSING_MS as u64 + 100));
        age_missing();
        let again = update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, _| unwatched(book, &computer()),
            |b| look_again(b, &computer()),
        )
        .unwrap();
        assert_eq!(shade.shown(), vec![t.clone()]);
        assert_eq!(again, None);
        // The swipe that removed it is told late: the copy put back goes.
        dismissed(dir.path(), &t, &shade).unwrap();
        assert!(shade.shown().is_empty());
        lock(&LEDGER).take();
    }

    #[test]
    fn handled_on_another_device_clears_here_and_never_rises() {
        let mut book = Book::default();
        rx(&mut book, open(&[], 1), false, &[]);
        let (showing, coming) = (
            alert(AttentionKind::Approval, 2),
            alert(AttentionKind::Question, 3),
        );
        rx(
            &mut book,
            AlertServerFrame::Raised {
                alert: Box::new(showing.clone()),
            },
            false,
            &[],
        );
        let gone = rx(
            &mut book,
            AlertServerFrame::Handled {
                attention_id: showing.attention_id,
            },
            false,
            &[],
        );
        assert_eq!(gone.cancel, vec![tag("p1", &showing.attention_id)]);
        // Not told back to the computer, which told this phone.
        assert!(book.computers["p1"].unsent.is_empty());
        // Handled elsewhere before it came here: the computer lists it quiet.
        let mut quiet = coming.clone();
        quiet.quiet = true;
        let listed = rx(&mut book, open(&[showing.clone(), quiet], 4), false, &[]);
        assert!(listed.post.is_empty());
        assert!(book.computers["p1"].knows(&coming.attention_id));
    }

    #[test]
    fn a_choice_of_tier_takes_effect_on_the_next_alert() {
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
            .unwrap()
        };
        frame(open(&[], 1));
        let last = |shade: &FakeShade| lock(&shade.posted).last().cloned();
        // Off on this phone: not shown, counted as handled, never raised
        // later when turned on again.
        shade.tier(AlertType::Finished, Tier::Off);
        let done = alert(AttentionKind::Completed, 2);
        frame(AlertServerFrame::Raised {
            alert: Box::new(done.clone()),
        });
        assert!(shade.shown().is_empty());
        shade.tier(AlertType::Finished, Tier::Alert);
        frame(open(std::slice::from_ref(&done), 3));
        assert!(shade.shown().is_empty());
        // Silent: shown without a sound.
        shade.tier(AlertType::Finished, Tier::Silent);
        let quiet = alert(AttentionKind::Completed, 4);
        frame(AlertServerFrame::Raised {
            alert: Box::new(quiet.clone()),
        });
        assert_eq!(shade.sounded(), 0);
        assert_eq!(last(&shade).unwrap().tag, tag("p1", &quiet.attention_id));
        // A project's own choice over the phone's: off there.
        let mut muted = alert(AttentionKind::Question, 5);
        muted.project_tier = Some(Tier::Off);
        frame(AlertServerFrame::Raised {
            alert: Box::new(muted.clone()),
        });
        assert!(!shade.shown().contains(&tag("p1", &muted.attention_id)));
        // A task the person asked to hear about alerts, whatever the phone's
        // choice for finished tasks.
        shade.tier(AlertType::Finished, Tier::Off);
        let mut followed = alert(AttentionKind::Completed, 6);
        followed.follow = true;
        frame(AlertServerFrame::Raised {
            alert: Box::new(followed.clone()),
        });
        let note = last(&shade).unwrap();
        assert_eq!(note.tag, tag("p1", &followed.attention_id));
        assert_eq!(note.want, Some(Tier::Alert));
        assert_eq!(shade.sounded(), 1);
        lock(&LEDGER).take();
    }

    fn app_reply(profile: &str) -> Note {
        Note {
            tag: tag(profile, &AttentionId::new()),
            category: Category::Finished,
            title: "A reply".into(),
            body: "An agent replied".into(),
            group: None,
            group_title: None,
            route: serde_json::Value::Null,
            question: None,
            silent: false,
            alert_type: Some(AlertType::Reply),
            want: None,
            handles: Vec::new(),
        }
    }

    #[test]
    fn foreground_sound_budget_respects_computers_minute_and_quiet_posts() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        shade.tier(AlertType::Finished, Tier::Off);
        shade.tier(AlertType::Reply, AlertType::Reply.default_tier());
        let post = |note, now| post_from_app_with(&shade, note, shade.clears(), || now).unwrap();
        let mut quiet = app_reply("p1");
        quiet.silent = true;
        post(quiet, 100);
        let mut silent = app_reply("p1");
        silent.want = Some(Tier::Silent);
        post(silent, 200);
        let mut off = app_reply("p1");
        off.want = Some(Tier::Off);
        post(off, 300);
        assert_eq!(shade.sounded(), 0);
        assert!(lock(&LEDGER).as_ref().unwrap().book.sounded.is_empty());
        post(app_reply("p1"), 1_000);
        post(app_reply("p1"), 1_001);
        assert_eq!(shade.sounded(), 1);
        post(app_reply("p2"), 1_002);
        assert_eq!(shade.sounded(), 2);
        post(app_reply("p1"), 60_999);
        assert_eq!(shade.sounded(), 2);
        post(app_reply("p1"), 61_000);
        assert_eq!(shade.sounded(), 3);
        assert_eq!(lock(&LEDGER).as_ref().unwrap().book.sounded["p1"], 61_000);
        lock(&LEDGER).take();
    }

    #[test]
    fn foreground_members_are_bounded_same_computer_and_optional() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        for handles in [
            vec![tag("p2", &AttentionId::new())],
            vec!["p1/not-an-id".into()],
            vec![tag("p1", &AttentionId::new()); MAX_SHOWING + 1],
        ] {
            let mut summary = app_reply("p1");
            summary.tag = folded_tag("p1", &ProjectId::new());
            summary.handles = handles;
            assert!(post_from_app_with(&shade, summary, shade.clears(), || 100).is_err());
        }
        assert!(shade.shown().is_empty());
        // Legacy producers omit handles; neither individual nor summary posts fail.
        post_from_app_with(&shade, app_reply("p1"), shade.clears(), || 100).unwrap();
        let mut legacy = app_reply("p1");
        legacy.tag = folded_tag("p1", &ProjectId::new());
        post_from_app_with(&shade, legacy, shade.clears(), || 101).unwrap();
        assert_eq!(shade.shown().len(), 2);
        lock(&LEDGER).take();
    }

    #[test]
    fn foreground_fold_handoff_keeps_members_and_swipe_handles_only_that_summary() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let shade = FakeShade::default();
        let mut first = alert(AttentionKind::Completed, 2);
        first.alert_type = Some(AlertType::Reply);
        first.bundle = true;
        let mut second = first.clone();
        second.attention_id = AttentionId::new();
        second.created_at = Timestamp(3);
        with_book(|book| {
            rx(book, open(&[], 1), true, &[]);
            for alert in [&first, &second] {
                rx(
                    book,
                    AlertServerFrame::Raised {
                        alert: Box::new(alert.clone()),
                    },
                    true,
                    &[],
                );
            }
            assert_eq!(book.computers["p1"].waiting.len(), 2);
        })
        .unwrap();
        let single = note(&computer(), &first, Detail::Minimal).unwrap();
        post_from_app_with(&shade, single.clone(), shade.clears(), || 1_000).unwrap();
        // The frontend removes individual notes before posting the summary.
        cleared_by_app(std::slice::from_ref(&single.tag)).unwrap();
        shade.cancel(&single.tag);
        let mut summary = single;
        summary.tag = folded_tag("p1", &first.project_id);
        summary.title = "Site has 2 notifications".into();
        summary.handles = vec![
            tag("p1", &first.attention_id),
            tag("p1", &second.attention_id),
        ];
        post_from_app_with(&shade, summary.clone(), shade.clears(), || 1_001).unwrap();
        let sounds = shade.sounded();
        with_book(|book| {
            let seen = &book.computers["p1"];
            assert!(seen.waiting.is_empty());
            let fold = &seen.folds[&first.project_id];
            assert!(fold.folded);
            assert_eq!(
                fold.members.keys().copied().collect::<BTreeSet<_>>(),
                BTreeSet::from([first.attention_id, second.attention_id])
            );
            assert!(unwatched(book, &computer()).post.is_empty());
            assert!(
                book.missing(&shade.shown(), 10_000, shade.clears())
                    .is_empty()
            );
            assert!(
                book.missing(&shade.shown(), 10_000 + MISSING_MS, shade.clears())
                    .is_empty()
            );
        })
        .unwrap();
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, _| unwatched(book, &computer()),
            |_| (),
        )
        .unwrap();
        assert_eq!(shade.shown(), vec![summary.tag.clone()]);
        assert_eq!(shade.sounded(), sounds);
        // A newer reply not represented by this posted summary is not handled.
        let mut third = second.clone();
        third.attention_id = AttentionId::new();
        with_book(|book| {
            rx(
                book,
                AlertServerFrame::Raised {
                    alert: Box::new(third.clone()),
                },
                true,
                &shade.shown(),
            );
        })
        .unwrap();
        shade.swipe(&summary.tag);
        dismissed(dir.path(), &summary.handles.join("\n"), &shade).unwrap();
        with_book(|book| {
            let seen = &book.computers["p1"];
            for id in [first.attention_id, second.attention_id] {
                assert!(seen.unsent.contains(&id));
                assert!(!seen.raised.contains(&id));
            }
            assert!(!seen.unsent.contains(&third.attention_id));
            assert!(seen.waiting.contains_key(&third.attention_id));
        })
        .unwrap();
        lock(&LEDGER).take();
    }

    #[test]
    fn foreground_background_handoffs_share_one_sound_budget() {
        for app_first in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let _one = ledger(dir.path());
            configure(ON).unwrap();
            let shade = FakeShade::default();
            let frame = |frame| {
                update(
                    current_watcher(),
                    &shade,
                    &BTreeSet::new(),
                    |book, shown| rx(book, frame, false, shown),
                    |_| (),
                )
                .unwrap();
            };
            frame(open(&[], 1));
            let foreground = || {
                post_from_app_with(&shade, app_reply("p1"), shade.clears(), || {
                    Timestamp::now().0
                })
                .unwrap();
            };
            let background = || {
                frame(AlertServerFrame::Raised {
                    alert: Box::new(alert(AttentionKind::Approval, 2)),
                })
            };
            if app_first {
                foreground();
                background();
            } else {
                background();
                foreground();
            }
            assert_eq!(shade.sounded(), 1, "app_first={app_first}");
            assert_eq!(shade.shown().len(), 2);
            lock(&LEDGER).take();
        }
    }

    #[test]
    fn foreground_cleared_full_or_failed_posts_do_not_consume_sound_budget() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        let note = app_reply("p1");
        *lock(&shade.racing) = Some(note.tag.clone());
        post_from_app_with(&shade, note, 0, || 100).unwrap();
        shade.full.store(true, Ordering::SeqCst);
        assert!(post_from_app_with(&shade, app_reply("p1"), shade.clears(), || 200).is_err());
        shade.refuse.store(true, Ordering::SeqCst);
        assert!(post_from_app_with(&shade, app_reply("p1"), shade.clears(), || 300).is_err());
        assert!(lock(&LEDGER).as_ref().unwrap().book.sounded.is_empty());
        lock(&LEDGER).take();
    }

    fn round4_capacity_batch(
        folded: bool,
        urgent_first: bool,
        other_quiet: bool,
        urgent_count: usize,
        shared_project: bool,
        refused_reply: bool,
    ) {
        let label = format!(
            "folded={folded}, urgent_first={urgent_first}, other_quiet={other_quiet}, \
             urgent_count={urgent_count}, shared_project={shared_project}, refused_reply={refused_reply}"
        );
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        with_book(|book| {
            book.started("shade");
            rx(book, open(&[], 1), false, &[]);
        })
        .unwrap();
        let first = Alert {
            bundle: true,
            alert_type: Some(AlertType::Reply),
            project_tier: Some(Tier::Silent),
            ..alert(AttentionKind::Completed, 2)
        };
        let mut all = vec![first.clone()];
        if folded {
            all.push(Alert {
                attention_id: AttentionId::new(),
                created_at: Timestamp(3),
                ..first.clone()
            });
        }
        for a in &all {
            post_from_app_fold_with(
                &shade,
                note(&computer(), a, Detail::Minimal).unwrap(),
                Some(app_fold_for(a)),
                shade.clears(),
                || 1_000,
            )
            .unwrap();
        }
        let summary = folded_tag("p1", &first.project_id);
        for i in 1..MAX_SHOWN {
            // Separate computers keep loud fillers outside each other's budget.
            let mut n = app_reply(&format!("other-{i}"));
            n.want = Some(if other_quiet {
                Tier::Silent
            } else {
                Tier::Alert
            });
            post_from_app_with(&shade, n, shade.clears(), || 1_000).unwrap();
        }
        assert_eq!(shade.shown().len(), MAX_SHOWN);
        assert_eq!(
            saved(dir.path()).quiet.first(),
            Some(&if folded {
                summary.clone()
            } else {
                tag("p1", &first.attention_id)
            })
        );
        let reply = Alert {
            attention_id: AttentionId::new(),
            created_at: Timestamp(if urgent_first { 20 } else { 4 }),
            ..first.clone()
        };
        let project = ProjectId::new();
        let urgent: Vec<_> = (0..urgent_count)
            .map(|i| Alert {
                project_id: if shared_project {
                    project
                } else {
                    ProjectId::new()
                },
                bundle: shared_project,
                ..alert(AttentionKind::Approval, 5 + i as i64)
            })
            .collect();
        all.push(reply.clone());
        all.extend(urgent.iter().cloned());
        let sounds = shade.sounded();
        if refused_reply {
            *lock(&shade.racing) = Some(tag("p1", &reply.attention_id));
        }
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, shown| rx(book, open(&all, 30), false, shown),
            |_| (),
        )
        .unwrap();
        let showing = shade.shown();
        let mut book = saved(dir.path());
        let seen = &book.computers["p1"];
        for a in &urgent {
            let represented = showing.contains(&tag("p1", &a.attention_id))
                || seen.folds.get(&a.project_id).is_some_and(|f| {
                    f.members.contains_key(&a.attention_id)
                        && showing.contains(&folded_tag("p1", &a.project_id))
                });
            assert!(
                represented,
                "a reused project must not take an urgent item's place: {label}"
            );
            assert!(seen.raised.contains(&a.attention_id), "{label}");
            assert!(seen.shown_last.contains(&a.attention_id), "{label}");
        }
        assert_eq!(
            showing.iter().filter(|t| *t != MORE).count(),
            MAX_SHOWN,
            "{label}"
        );
        assert!(
            seen.held
                .iter()
                .all(|r| r.alert.attention_id == reply.attention_id),
            "{label}"
        );
        assert_eq!(shade.sounded(), sounds + 1, "{label}");
        // Displacing a summary is final, not handling or a restart candidate.
        let displaced: Vec<_> = all
            .iter()
            .filter(|a| {
                a.project_id == first.project_id
                    && !showing.contains(&tag("p1", &a.attention_id))
                    && !seen.folds.get(&first.project_id).is_some_and(|f| {
                        f.members.contains_key(&a.attention_id) && showing.contains(&summary)
                    })
            })
            .map(|a| a.attention_id)
            .collect();
        for id in &displaced {
            assert!(!seen.raised.contains(id), "{label}");
            assert!(!seen.shown_last.contains(id), "{label}");
            assert!(!seen.unsent.contains(id), "{label}");
        }
        book.started("after");
        assert!(
            displaced
                .iter()
                .all(|id| !book.computers["p1"].restore.contains(id)),
            "{label}"
        );
        lock(&LEDGER).take();
    }

    #[test]
    fn round4_project_reuse_cannot_take_an_urgent_items_place_after_eviction() {
        round4_capacity_batch(true, false, true, 1, false, false);
    }

    #[test]
    fn round4_capacity_ordering_keeps_each_urgent_project_visible() {
        for folded in [false, true] {
            for urgent_first in [false, true] {
                for shared in [false, true] {
                    round4_capacity_batch(folded, urgent_first, true, 2, shared, false);
                }
            }
        }
    }

    #[test]
    fn round4_a_quiet_projects_only_place_remains_available_to_urgent_work() {
        for folded in [false, true] {
            for urgent_first in [false, true] {
                round4_capacity_batch(folded, urgent_first, false, 1, false, false);
                round4_capacity_batch(folded, urgent_first, false, 2, true, false);
            }
        }
    }

    #[test]
    fn round4_refused_project_reuse_never_spends_the_urgent_items_budget_or_place() {
        for folded in [false, true] {
            for urgent_first in [false, true] {
                round4_capacity_batch(folded, urgent_first, true, 1, false, true);
            }
        }
    }

    #[test]
    fn round4_an_individual_refresh_does_not_consume_another_place() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        with_book(|book| {
            rx(book, open(&[], 1), false, &[]);
        })
        .unwrap();
        let existing = Alert {
            alert_type: Some(AlertType::Reply),
            project_tier: Some(Tier::Silent),
            ..alert(AttentionKind::Completed, 2)
        };
        post_from_app_with(
            &shade,
            note(&computer(), &existing, Detail::Minimal).unwrap(),
            shade.clears(),
            || 1_000,
        )
        .unwrap();
        for _ in 1..MAX_SHOWN - 1 {
            let mut n = app_reply("p2");
            n.want = Some(Tier::Silent);
            post_from_app_with(&shade, n, shade.clears(), || 1_000).unwrap();
        }
        let urgent = alert(AttentionKind::Approval, 3);
        let since = shade.clears();
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, shown| {
                let mut changes = rx(
                    book,
                    AlertServerFrame::Raised {
                        alert: Box::new(urgent.clone()),
                    },
                    false,
                    shown,
                );
                // A queued update can find the identity already posted by
                // the foreground producer. It uses no additional place.
                changes.post.insert(
                    0,
                    Raise {
                        computer: computer(),
                        alert: existing.clone(),
                        since,
                        silent: true,
                    },
                );
                changes
            },
            |_| (),
        )
        .unwrap();
        assert_eq!(shade.shown().len(), MAX_SHOWN);
        assert!(shade.shown().contains(&tag("p1", &urgent.attention_id)));
        assert!(
            lock(&shade.cleared).is_empty(),
            "the free place needed no eviction"
        );
        assert!(saved(dir.path()).computers["p1"].held.is_empty());
        assert_eq!(shade.sounded(), 1);
        lock(&LEDGER).take();
    }

    #[test]
    fn round3_first_fold_reuses_the_individual_slot_at_actual_capacity() {
        for background in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let _one = ledger(dir.path());
            let shade = FakeShade::default();
            let first = Alert {
                bundle: true,
                alert_type: Some(AlertType::Reply),
                ..alert(AttentionKind::Completed, 2)
            };
            let second = Alert {
                attention_id: AttentionId::new(),
                created_at: Timestamp(3),
                ..first.clone()
            };
            post_from_app_fold_with(
                &shade,
                note(&computer(), &first, Detail::Minimal).unwrap(),
                Some(app_fold_for(&first)),
                shade.clears(),
                || 1_000,
            )
            .unwrap();
            for _ in 1..MAX_SHOWN {
                post_from_app_with(&shade, app_reply("p2"), shade.clears(), || 1_000).unwrap();
            }
            assert_eq!(shade.shown().len(), MAX_SHOWN);
            if background {
                update(
                    current_watcher(),
                    &shade,
                    &BTreeSet::new(),
                    |book, shown| {
                        rx(
                            book,
                            AlertServerFrame::Raised {
                                alert: Box::new(second.clone()),
                            },
                            false,
                            shown,
                        )
                    },
                    |_| (),
                )
                .unwrap();
            } else {
                post_from_app_fold_with(
                    &shade,
                    note(&computer(), &second, Detail::Minimal).unwrap(),
                    Some(app_fold_for(&second)),
                    shade.clears(),
                    || 1_001,
                )
                .expect("the summary replaces A rather than requiring a 25th place");
            }
            assert_eq!(shade.shown().len(), MAX_SHOWN, "background={background}");
            assert!(!shade.shown().contains(&tag("p1", &first.attention_id)));
            assert!(
                shade.shown().contains(&folded_tag("p1", &first.project_id)),
                "background={background}"
            );
            let book = saved(dir.path());
            assert_eq!(
                book.computers["p1"].folds[&first.project_id].members.len(),
                2
            );
            assert!(book.computers["p1"].raised.contains(&second.attention_id));
            lock(&LEDGER).take();
        }
    }

    #[test]
    fn round3_mixed_producer_fold_uses_actual_device_tiers_and_shared_budget() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        shade.tier(AlertType::Question, Tier::Silent);
        shade.tier(AlertType::Reply, Tier::Alert);
        let question = Alert {
            bundle: true,
            ..alert(AttentionKind::Question, 2)
        };
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, shown| {
                rx(
                    book,
                    AlertServerFrame::Raised {
                        alert: Box::new(question.clone()),
                    },
                    false,
                    shown,
                )
            },
            |_| (),
        )
        .unwrap();
        assert_eq!(shade.sounded(), 0);
        let reply = Alert {
            attention_id: AttentionId::new(),
            kind: AttentionKind::Completed,
            alert_type: Some(AlertType::Reply),
            ..question.clone()
        };
        let mut outgoing = note(&computer(), &reply, Detail::Minimal).unwrap();
        outgoing.want = Some(Tier::Alert);
        post_from_app_fold_with(
            &shade,
            outgoing,
            Some(app_fold_for(&reply)),
            shade.clears(),
            || Timestamp::now().0,
        )
        .unwrap();
        assert_eq!(
            shade.sounded(),
            1,
            "the Alert reply must not use the Silent Question channel"
        );
        let summary = lock(&shade.posted).last().unwrap().clone();
        assert_eq!(summary.alert_type, Some(AlertType::Reply));
        assert_eq!(summary.handles.len(), 2);
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, shown| {
                rx(
                    book,
                    AlertServerFrame::Raised {
                        alert: Box::new(alert(AttentionKind::Approval, 4)),
                    },
                    false,
                    shown,
                )
            },
            |_| (),
        )
        .unwrap();
        assert_eq!(
            shade.sounded(),
            1,
            "background work shares the foreground sound budget"
        );
        lock(&LEDGER).take();
    }

    #[test]
    fn round3_ungrouped_settings_refresh_never_restores_displaced_or_handled_work() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        with_book(|book| {
            book.started("before");
            rx(book, open(&[], 1), true, &[]);
        })
        .unwrap();
        let replies: Vec<_> = (0..MAX_SHOWN)
            .map(|i| Alert {
                alert_type: Some(AlertType::Reply),
                ..alert(AttentionKind::Completed, 2 + i as i64)
            })
            .collect();
        for a in &replies {
            post_from_app_with(
                &shade,
                note(&computer(), a, Detail::Minimal).unwrap(),
                shade.clears(),
                || 1_000,
            )
            .unwrap();
        }
        let urgent = alert(AttentionKind::Approval, 100);
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, shown| {
                rx(
                    book,
                    AlertServerFrame::Raised {
                        alert: Box::new(urgent.clone()),
                    },
                    false,
                    shown,
                )
            },
            |_| (),
        )
        .unwrap();
        let displaced = tag("p1", &replies[1].attention_id);
        let handled = tag("p1", &replies[2].attention_id);
        assert!(!shade.shown().contains(&displaced));
        handle_from_app_with(&shade, "p1", &[replies[2].attention_id]).unwrap();
        assert_eq!(shade.shown().len(), MAX_SHOWN - 1);
        // This is the silent, no-project routing refresh emitted by the page
        // when grouping is off and Replies changes from Alert to Silent.
        for a in [&replies[1], &replies[2], &replies[3]] {
            let mut refresh = note(&computer(), a, Detail::Minimal).unwrap();
            refresh.silent = true;
            refresh.want = Some(Tier::Silent);
            post_from_app_with(&shade, refresh, shade.clears(), || 2_000).unwrap();
            assert!(
                !shade.shown().contains(&displaced),
                "settings must not summon displaced work"
            );
        }
        assert!(
            !shade.shown().contains(&displaced),
            "settings must not summon displaced work"
        );
        assert!(!shade.shown().contains(&handled));
        let mut book = saved(dir.path());
        assert!(
            book.computers["p1"]
                .notified
                .contains(&replies[1].attention_id)
        );
        assert!(
            !book.computers["p1"]
                .unsent
                .contains(&replies[1].attention_id)
        );
        book.started("after");
        assert!(
            !book.computers["p1"]
                .restore
                .contains(&replies[1].attention_id)
        );
        assert!(
            !book.computers["p1"]
                .restore
                .contains(&replies[2].attention_id)
        );
        lock(&shade.shown).clear();
        with_book(|b| *b = book).unwrap();
        let mut all = replies.clone();
        all.push(urgent);
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, shown| rx(book, open(&all, 101), false, shown),
            |_| (),
        )
        .unwrap();
        assert!(!shade.shown().contains(&displaced));
        assert!(!shade.shown().contains(&handled));
        lock(&LEDGER).take();
    }

    #[test]
    fn round3_refused_replacements_preserve_the_last_accepted_individual_or_summary() {
        for background in [false, true] {
            for folded in [false, true] {
                for mode in ["cleared", "failed", "off", "full"] {
                    // Updating an already-visible tag needs no capacity, so
                    // FULL only applies to a new summary tag.
                    if folded && mode == "full" {
                        continue;
                    }
                    let dir = tempfile::tempdir().unwrap();
                    let _one = ledger(dir.path());
                    let shade = FakeShade::default();
                    with_book(|b| b.started("before")).unwrap();
                    let first = Alert {
                        bundle: true,
                        alert_type: Some(AlertType::Reply),
                        ..alert(AttentionKind::Completed, 2)
                    };
                    for _ in 0..if folded { 2 } else { 1 } {
                        let a = if lock(&shade.posted).is_empty() {
                            first.clone()
                        } else {
                            Alert {
                                attention_id: AttentionId::new(),
                                ..first.clone()
                            }
                        };
                        post_from_app_fold_with(
                            &shade,
                            note(&computer(), &a, Detail::Minimal).unwrap(),
                            Some(app_fold_for(&a)),
                            shade.clears(),
                            || 1_000,
                        )
                        .unwrap();
                    }
                    for _ in 1..MAX_SHOWN {
                        post_from_app_with(&shade, app_reply("p2"), shade.clears(), || 1_000)
                            .unwrap();
                    }
                    let previous = shade.shown();
                    let accepted =
                        saved(dir.path()).computers["p1"].folds[&first.project_id].clone();
                    let mut candidate = Alert {
                        project_id: first.project_id,
                        bundle: true,
                        ..alert(AttentionKind::Approval, 3)
                    };
                    match mode {
                        "cleared" => {
                            *lock(&shade.racing) = Some(tag("p1", &candidate.attention_id))
                        }
                        "failed" => shade.refuse.store(true, Ordering::SeqCst),
                        "off" => candidate.project_tier = Some(Tier::Off),
                        _ => shade.full.store(true, Ordering::SeqCst),
                    }
                    if background {
                        update(
                            current_watcher(),
                            &shade,
                            &BTreeSet::new(),
                            |book, shown| {
                                rx(
                                    book,
                                    AlertServerFrame::Raised {
                                        alert: Box::new(candidate.clone()),
                                    },
                                    false,
                                    shown,
                                )
                            },
                            |_| (),
                        )
                        .unwrap();
                    } else {
                        let _ = post_from_app_fold_with(
                            &shade,
                            note(&computer(), &candidate, Detail::Minimal).unwrap(),
                            Some(app_fold_for(&candidate)),
                            shade.clears(),
                            || 61_000,
                        );
                    }
                    let label = format!("background={background}, folded={folded}, mode={mode}");
                    assert_eq!(
                        shade
                            .shown()
                            .into_iter()
                            .filter(|t| t != MORE)
                            .collect::<Vec<_>>(),
                        previous,
                        "{label}"
                    );
                    let mut book = saved(dir.path());
                    let seen = &book.computers["p1"];
                    assert_eq!(seen.folds[&first.project_id], accepted, "{label}");
                    assert!(!seen.raised.contains(&candidate.attention_id), "{label}");
                    for id in accepted.members.keys() {
                        assert!(seen.raised.contains(id), "{label}");
                        assert!(seen.shown_last.contains(id), "{label}");
                    }
                    book.started("after");
                    assert!(
                        !book.computers["p1"]
                            .restore
                            .contains(&candidate.attention_id),
                        "{label}"
                    );
                    assert!(
                        accepted
                            .members
                            .keys()
                            .all(|id| book.computers["p1"].restore.contains(id)),
                        "{label}"
                    );
                    lock(&LEDGER).take();
                }
            }
        }
    }

    #[test]
    fn round3_fold_lead_resolves_off_and_overrides_without_freezing_device_choices() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        let q = AttentionId::new();
        let r = AttentionId::new();
        let project = ProjectId::new();
        for (question, q_want, reply, r_want, lead, tier) in [
            (
                Tier::Silent,
                None,
                Tier::Alert,
                Some(Tier::Alert),
                AlertType::Reply,
                Tier::Alert,
            ),
            (
                Tier::Off,
                None,
                Tier::Silent,
                None,
                AlertType::Reply,
                Tier::Silent,
            ),
            (
                Tier::Alert,
                Some(Tier::Off),
                Tier::Silent,
                None,
                AlertType::Reply,
                Tier::Silent,
            ),
            (
                Tier::Alert,
                Some(Tier::Silent),
                Tier::Alert,
                None,
                AlertType::Reply,
                Tier::Alert,
            ),
            (
                Tier::Off,
                Some(Tier::Alert),
                Tier::Silent,
                None,
                AlertType::Question,
                Tier::Alert,
            ),
            (
                Tier::Off,
                None,
                Tier::Off,
                None,
                AlertType::Question,
                Tier::Off,
            ),
        ] {
            shade.tier(AlertType::Question, question);
            shade.tier(AlertType::Reply, reply);
            let fold = Fold {
                project: "Site".into(),
                computer: "Studio".into(),
                members: BTreeMap::from([(q, true), (r, false)]),
                folded: true,
                counted: 2,
                types: BTreeMap::from([
                    (q, (AlertType::Question, q_want)),
                    (r, (AlertType::Reply, r_want)),
                ]),
            };
            let note = fold.note(&shade, "p1", &project);
            assert_eq!(note.alert_type, Some(lead));
            assert_eq!(shade.effective(&note), tier);
            assert_eq!(
                note.want,
                if lead == AlertType::Question {
                    q_want
                } else {
                    r_want
                }
            );
        }
        lock(&LEDGER).take();
    }

    #[test]
    fn round3_off_incoming_members_do_not_borrow_a_visible_members_channel() {
        for background in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let _one = ledger(dir.path());
            let shade = FakeShade::default();
            let first = Alert {
                bundle: true,
                alert_type: Some(AlertType::Reply),
                ..alert(AttentionKind::Completed, 2)
            };
            post_from_app_fold_with(
                &shade,
                note(&computer(), &first, Detail::Minimal).unwrap(),
                Some(app_fold_for(&first)),
                shade.clears(),
                || 1_000,
            )
            .unwrap();
            let off = Alert {
                project_id: first.project_id,
                bundle: true,
                ..alert(AttentionKind::Question, 3)
            };
            shade.tier(AlertType::Question, Tier::Off);
            if background {
                update(
                    current_watcher(),
                    &shade,
                    &BTreeSet::new(),
                    |b, shown| {
                        rx(
                            b,
                            AlertServerFrame::Raised {
                                alert: Box::new(off.clone()),
                            },
                            false,
                            shown,
                        )
                    },
                    |_| (),
                )
                .unwrap();
            } else {
                post_from_app_fold_with(
                    &shade,
                    note(&computer(), &off, Detail::Minimal).unwrap(),
                    Some(app_fold_for(&off)),
                    shade.clears(),
                    || 61_000,
                )
                .unwrap();
            }
            assert_eq!(shade.shown(), vec![tag("p1", &first.attention_id)]);
            let book = saved(dir.path());
            assert!(book.computers["p1"].notified.contains(&off.attention_id));
            assert!(!book.computers["p1"].raised.contains(&off.attention_id));
            assert_eq!(
                book.computers["p1"].folds[&first.project_id].members.len(),
                1
            );
            lock(&LEDGER).take();
        }
    }

    #[test]
    fn round3_background_batch_reuses_one_project_slot_and_stays_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        with_book(|b| {
            rx(b, open(&[], 1), false, &[]);
        })
        .unwrap();
        for _ in 1..MAX_SHOWN {
            post_from_app_with(&shade, app_reply("p2"), shade.clears(), || 1_000).unwrap();
        }
        let first = Alert {
            bundle: true,
            ..alert(AttentionKind::Question, 2)
        };
        let batch: Vec<_> = (0..MAX_SHOWING)
            .map(|i| Alert {
                attention_id: AttentionId::new(),
                created_at: Timestamp(2 + i as i64),
                ..first.clone()
            })
            .collect();
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |b, shown| rx(b, open(&batch, 300), false, shown),
            |_| (),
        )
        .unwrap();
        assert_eq!(shade.shown().len(), MAX_SHOWN);
        let summary = folded_tag("p1", &first.project_id);
        assert!(shade.shown().contains(&summary));
        assert_eq!(
            saved(dir.path()).computers["p1"].folds[&first.project_id]
                .members
                .len(),
            MAX_SHOWING
        );
        let extra = Alert {
            attention_id: AttentionId::new(),
            ..first
        };
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |b, shown| {
                rx(
                    b,
                    AlertServerFrame::Raised {
                        alert: Box::new(extra.clone()),
                    },
                    false,
                    shown,
                )
            },
            |_| (),
        )
        .unwrap();
        let book = saved(dir.path());
        assert_eq!(
            book.computers["p1"].folds[&extra.project_id].members.len(),
            MAX_SHOWING
        );
        assert!(!book.computers["p1"].raised.contains(&extra.attention_id));
        let captured = lock(&shade.posted)
            .iter()
            .rev()
            .find(|n| n.tag == summary)
            .unwrap()
            .handles
            .clone();
        assert_eq!(captured.len(), MAX_SHOWING);
        lock(&LEDGER).take();
    }

    #[test]
    fn round2_foreground_individual_and_fold_restore_without_a_native_update() {
        for bundled in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let _one = ledger(dir.path());
            let shade = FakeShade::default();
            let first = Alert {
                bundle: bundled,
                alert_type: Some(AlertType::Reply),
                ..alert(AttentionKind::Completed, 2)
            };
            let second = Alert {
                attention_id: AttentionId::new(),
                created_at: Timestamp(3),
                ..first.clone()
            };
            let alerts = if bundled {
                vec![first.clone(), second]
            } else {
                vec![first.clone()]
            };
            with_book(|book| {
                book.started("before-update");
                rx(book, open(&[], 1), true, &[]);
                for a in &alerts {
                    rx(
                        book,
                        AlertServerFrame::Raised {
                            alert: Box::new(a.clone()),
                        },
                        true,
                        &[],
                    );
                }
            })
            .unwrap();
            let mut note = note(&computer(), &first, Detail::Minimal).unwrap();
            if bundled {
                note.tag = folded_tag("p1", &first.project_id);
                note.title = "Site has 2 notifications".into();
                note.handles = alerts.iter().map(|a| tag("p1", &a.attention_id)).collect();
            }
            post_from_app_with(&shade, note.clone(), shade.clears(), || 1_000).unwrap();
            let mut book = saved(dir.path());
            assert_eq!(
                book.computers["p1"].shown_last.len(),
                alerts.len(),
                "bundled={bundled}"
            );
            lock(&shade.shown).clear();
            book.restarted();
            book.started("after-update");
            with_book(|b| *b = book).unwrap();
            let before = lock(&shade.posted).len();
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, open(&alerts, 4), false, shown),
                |_| (),
            )
            .unwrap();
            let restored = lock(&shade.posted)[before..].to_vec();
            assert!(!restored.is_empty(), "bundled={bundled}");
            assert!(restored.iter().all(|n| n.silent));
            assert_eq!(shade.shown(), vec![note.tag]);
            assert_eq!(
                saved(dir.path()).computers["p1"].shown_last.len(),
                alerts.len()
            );
            lock(&LEDGER).take();
        }
    }

    #[test]
    fn round2_foreground_silent_replies_make_room_for_background_urgent_alerts() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        with_book(|book| {
            book.started("shade");
            rx(book, open(&[], 1), true, &[]);
        })
        .unwrap();
        let replies: Vec<_> = (0..MAX_SHOWN)
            .map(|i| Alert {
                alert_type: Some(AlertType::Reply),
                ..alert(AttentionKind::Completed, 2 + i as i64)
            })
            .collect();
        for (i, reply) in replies.iter().enumerate() {
            let note = note(&computer(), reply, Detail::Minimal).unwrap();
            post_from_app_with(&shade, note, shade.clears(), || 1_000 + i as i64).unwrap();
        }
        assert_eq!(shade.shown().len(), MAX_SHOWN);
        assert_eq!(shade.sounded(), 1);
        // Actual capacity refusal, not the fixture's force-full switch.
        assert!(post_from_app_with(&shade, app_reply("p1"), shade.clears(), || 2_000).is_err());
        // Native receipt of the same replies must neither repost nor lose their quiet order.
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, shown| rx(book, open(&replies, 100), false, shown),
            |_| (),
        )
        .unwrap();
        let urgent = alert(AttentionKind::Approval, 101);
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, shown| {
                rx(
                    book,
                    AlertServerFrame::Raised {
                        alert: Box::new(urgent.clone()),
                    },
                    false,
                    shown,
                )
            },
            |_| (),
        )
        .unwrap();
        let shown = shade.shown();
        assert!(shown.contains(&tag("p1", &urgent.attention_id)));
        assert!(shown.contains(&tag("p1", &replies[0].attention_id)));
        assert!(!shown.contains(&tag("p1", &replies[1].attention_id)));
        assert_eq!(shown.len(), MAX_SHOWN);
        let book = saved(dir.path());
        assert!(book.computers["p1"].unsent.is_empty());
        assert!(
            !book.computers["p1"]
                .raised
                .contains(&replies[1].attention_id)
        );
        lock(&LEDGER).take();
    }

    #[test]
    fn round2_foreground_quiet_folds_make_room_without_restoring_their_members() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        with_book(|book| {
            book.started("shade");
            rx(book, open(&[], 1), true, &[]);
        })
        .unwrap();
        let mut replies = Vec::new();
        for i in 0..MAX_SHOWN - 1 {
            let first = Alert {
                project_id: ProjectId::new(),
                bundle: true,
                alert_type: Some(AlertType::Reply),
                ..alert(AttentionKind::Completed, 2 + i as i64 * 2)
            };
            let second = Alert {
                attention_id: AttentionId::new(),
                created_at: Timestamp(first.created_at.0 + 1),
                ..first.clone()
            };
            for a in [first, second] {
                with_book(|book| {
                    rx(
                        book,
                        AlertServerFrame::Raised {
                            alert: Box::new(a.clone()),
                        },
                        true,
                        &shade.shown(),
                    );
                })
                .unwrap();
                post_from_app_fold_with(
                    &shade,
                    note(&computer(), &a, Detail::Minimal).unwrap(),
                    Some(app_fold_for(&a)),
                    shade.clears(),
                    || 1_000,
                )
                .unwrap();
                replies.push(a);
            }
        }
        post_from_app_with(&shade, app_reply("p2"), shade.clears(), || 1_000).unwrap();
        assert_eq!(shade.shown().len(), MAX_SHOWN);
        let victim = folded_tag("p1", &replies[0].project_id);
        let urgent = alert(AttentionKind::Approval, 101);
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, shown| {
                rx(
                    book,
                    AlertServerFrame::Raised {
                        alert: Box::new(urgent.clone()),
                    },
                    false,
                    shown,
                )
            },
            |_| (),
        )
        .unwrap();
        assert!(shade.shown().contains(&tag("p1", &urgent.attention_id)));
        assert!(!shade.shown().contains(&victim));
        let mut book = saved(dir.path());
        let seen = &book.computers["p1"];
        assert!(seen.unsent.is_empty());
        for a in &replies[..2] {
            assert!(!seen.raised.contains(&a.attention_id));
        }
        assert!(!seen.folds.contains_key(&replies[0].project_id));
        assert!(
            book.missing(&shade.shown(), 100_000, shade.clears())
                .is_empty()
        );
        assert!(
            book.missing(&shade.shown(), 120_000, shade.clears())
                .is_empty()
        );
        lock(&LEDGER).take();
    }

    fn app_fold_for(alert: &Alert) -> AppFold {
        AppFold {
            project_id: alert.project_id,
            project: alert.project.clone(),
            computer: computer().name,
        }
    }

    #[test]
    fn round2_reverse_handoff_recounts_native_members_and_swipes_exactly_them() {
        for handle in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let _one = ledger(dir.path());
            let shade = FakeShade::default();
            let first = Alert {
                bundle: true,
                alert_type: Some(AlertType::Reply),
                ..alert(AttentionKind::Completed, 2)
            };
            let second = Alert {
                attention_id: AttentionId::new(),
                created_at: Timestamp(3),
                ..first.clone()
            };
            let third = Alert {
                attention_id: AttentionId::new(),
                created_at: Timestamp(4),
                ..first.clone()
            };
            with_book(|book| {
                book.started("shade");
                rx(book, open(&[], 1), true, &[]);
            })
            .unwrap();
            for a in [&first, &second] {
                with_book(|book| {
                    rx(
                        book,
                        AlertServerFrame::Raised {
                            alert: Box::new(a.clone()),
                        },
                        true,
                        &shade.shown(),
                    );
                })
                .unwrap();
                post_from_app_fold_with(
                    &shade,
                    note(&computer(), a, Detail::Minimal).unwrap(),
                    Some(app_fold_for(a)),
                    shade.clears(),
                    || 1_000,
                )
                .unwrap();
            }
            let summary = folded_tag("p1", &first.project_id);
            assert_eq!(shade.shown(), vec![summary.clone()]);
            // Hidden JavaScript skips C; the native producer extends the same fold.
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| {
                    rx(
                        book,
                        AlertServerFrame::Raised {
                            alert: Box::new(third.clone()),
                        },
                        false,
                        shown,
                    )
                },
                |_| (),
            )
            .unwrap();
            let before = shade.sounded();
            if handle {
                handle_from_app_with(&shade, "p1", &[first.attention_id]).unwrap();
            } else {
                clear_from_app_with(&shade, &[tag("p1", &first.attention_id)]).unwrap();
            }
            let current = lock(&shade.posted).last().unwrap().clone();
            assert_eq!(current.title, "Site has 2 notifications");
            assert!(current.silent);
            assert_eq!(
                current.handles.iter().cloned().collect::<BTreeSet<_>>(),
                BTreeSet::from([
                    tag("p1", &second.attention_id),
                    tag("p1", &third.attention_id)
                ])
            );
            assert_eq!(shade.sounded(), before);
            // A stale queued individual, not just a stale aggregate, cannot resurrect A.
            let posts = lock(&shade.posted).len();
            post_from_app_fold_with(
                &shade,
                note(&computer(), &first, Detail::Minimal).unwrap(),
                Some(app_fold_for(&first)),
                shade.clears(),
                || 2_000,
            )
            .unwrap();
            assert_eq!(lock(&shade.posted).len(), posts);
            // Swipe membership is captured before a newer D arrives.
            shade.swipe(&summary);
            let fourth = Alert {
                attention_id: AttentionId::new(),
                created_at: Timestamp(5),
                ..first.clone()
            };
            post_from_app_fold_with(
                &shade,
                note(&computer(), &fourth, Detail::Minimal).unwrap(),
                Some(app_fold_for(&fourth)),
                shade.clears(),
                || 2_001,
            )
            .unwrap();
            dismissed(dir.path(), &current.handles.join("\n"), &shade).unwrap();
            let book = saved(dir.path());
            let seen = &book.computers["p1"];
            assert!(!seen.raised.contains(&first.attention_id));
            assert!(seen.unsent.contains(&second.attention_id));
            assert!(seen.unsent.contains(&third.attention_id));
            assert!(!seen.unsent.contains(&fourth.attention_id));
            assert!(seen.raised.contains(&fourth.attention_id));
            assert_eq!(shade.shown(), vec![tag("p1", &fourth.attention_id)]);
            lock(&LEDGER).take();
        }
    }

    #[test]
    fn round2_native_managed_foreground_fold_restores_and_checks_original_member_clears() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        let first = Alert {
            bundle: true,
            alert_type: Some(AlertType::Reply),
            ..alert(AttentionKind::Completed, 2)
        };
        let second = Alert {
            attention_id: AttentionId::new(),
            ..first.clone()
        };
        with_book(|book| {
            book.started("before");
            rx(book, open(&[], 1), true, &[]);
        })
        .unwrap();
        for a in [&first, &second] {
            with_book(|book| {
                rx(
                    book,
                    AlertServerFrame::Raised {
                        alert: Box::new(a.clone()),
                    },
                    true,
                    &shade.shown(),
                );
            })
            .unwrap();
            post_from_app_fold_with(
                &shade,
                note(&computer(), a, Detail::Minimal).unwrap(),
                Some(app_fold_for(a)),
                shade.clears(),
                || 1_000,
            )
            .unwrap();
        }
        let third = Alert {
            attention_id: AttentionId::new(),
            ..first.clone()
        };
        let since = shade.clears();
        *lock(&shade.racing) = Some(tag("p1", &third.attention_id));
        post_from_app_fold_with(
            &shade,
            note(&computer(), &third, Detail::Minimal).unwrap(),
            Some(app_fold_for(&third)),
            since,
            || 1_001,
        )
        .unwrap();
        *lock(&shade.racing) = None;
        let mut book = saved(dir.path());
        assert_eq!(
            book.computers["p1"].shown_last,
            BTreeSet::from([first.attention_id, second.attention_id])
        );
        assert!(!book.computers["p1"].raised.contains(&third.attention_id));
        book.restarted();
        book.started("after");
        lock(&shade.shown).clear();
        with_book(|b| *b = book).unwrap();
        update(
            current_watcher(),
            &shade,
            &BTreeSet::new(),
            |book, shown| {
                rx(
                    book,
                    open(&[first.clone(), second.clone()], 5),
                    false,
                    shown,
                )
            },
            |_| (),
        )
        .unwrap();
        let restored = lock(&shade.posted).last().unwrap().clone();
        assert_eq!(restored.tag, folded_tag("p1", &first.project_id));
        assert_eq!(restored.handles.len(), 2);
        assert!(restored.silent);
        lock(&LEDGER).take();
    }

    #[test]
    fn round2_urgent_foreground_member_checks_clears_before_realerting_the_summary() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        let first = Alert {
            bundle: true,
            alert_type: Some(AlertType::Reply),
            ..alert(AttentionKind::Completed, 2)
        };
        let second = Alert {
            attention_id: AttentionId::new(),
            ..first.clone()
        };
        for a in [&first, &second] {
            post_from_app_fold_with(
                &shade,
                note(&computer(), a, Detail::Minimal).unwrap(),
                Some(app_fold_for(a)),
                shade.clears(),
                || 1_000,
            )
            .unwrap();
        }
        let summary = folded_tag("p1", &first.project_id);
        let urgent = Alert {
            project_id: first.project_id,
            bundle: true,
            ..alert(AttentionKind::Approval, 4)
        };
        let stamp = shade.clears();
        *lock(&shade.racing) = Some(tag("p1", &urgent.attention_id));
        post_from_app_fold_with(
            &shade,
            note(&computer(), &urgent, Detail::Minimal).unwrap(),
            Some(app_fold_for(&urgent)),
            stamp,
            || 61_000,
        )
        .unwrap();
        assert_eq!(shade.shown(), vec![summary.clone()]);
        assert_eq!(shade.sounded(), 1);
        assert_eq!(
            saved(dir.path()).computers["p1"].folds[&first.project_id]
                .members
                .len(),
            2
        );
        *lock(&shade.racing) = None;
        // A new call after the clear can show the still-open item, and one
        // urgent replacement after the minute may alert, not every replacement.
        post_from_app_fold_with(
            &shade,
            note(&computer(), &urgent, Detail::Minimal).unwrap(),
            Some(app_fold_for(&urgent)),
            shade.clears(),
            || 61_001,
        )
        .unwrap();
        assert_eq!(shade.sounded(), 2);
        let next = Alert {
            attention_id: AttentionId::new(),
            ..urgent.clone()
        };
        post_from_app_fold_with(
            &shade,
            note(&computer(), &next, Detail::Minimal).unwrap(),
            Some(app_fold_for(&next)),
            shade.clears(),
            || 61_002,
        )
        .unwrap();
        assert_eq!(shade.sounded(), 2);
        assert_eq!(shade.shown(), vec![summary]);
        let posted = lock(&shade.posted).last().unwrap().clone();
        assert!(posted.silent);
        assert_eq!(posted.handles.len(), 4);
        lock(&LEDGER).take();
    }

    #[test]
    fn round2_native_fold_validates_metadata_and_early_handling_blocks_queued_posts() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        let first = alert(AttentionKind::Completed, 2);
        handle_from_app_with(&shade, "p1", &[first.attention_id]).unwrap();
        post_from_app_fold_with(
            &shade,
            note(&computer(), &first, Detail::Minimal).unwrap(),
            Some(app_fold_for(&first)),
            shade.clears(),
            || 1_000,
        )
        .unwrap();
        assert!(shade.shown().is_empty());
        let second = alert(AttentionKind::Completed, 3);
        with_book(|book| {
            rx(
                book,
                AlertServerFrame::Raised {
                    alert: Box::new(second.clone()),
                },
                true,
                &[],
            );
        })
        .unwrap();
        let original = note(&computer(), &second, Detail::Minimal).unwrap();
        for (note, fold) in [
            (
                original.clone(),
                AppFold {
                    project_id: ProjectId::new(),
                    ..app_fold_for(&second)
                },
            ),
            (
                original.clone(),
                AppFold {
                    project: "x".repeat(257),
                    ..app_fold_for(&second)
                },
            ),
            (
                original.clone(),
                AppFold {
                    computer: "x".repeat(257),
                    ..app_fold_for(&second)
                },
            ),
            (
                Note {
                    tag: "p1/not-an-id".into(),
                    ..original.clone()
                },
                app_fold_for(&second),
            ),
            (
                Note {
                    handles: vec![tag("p2", &first.attention_id)],
                    ..original.clone()
                },
                app_fold_for(&second),
            ),
        ] {
            assert!(
                post_from_app_fold_with(&shade, note, Some(fold), shade.clears(), || 1_000)
                    .is_err()
            );
        }
        assert!(shade.shown().is_empty());
        lock(&LEDGER).take();
    }

    #[test]
    fn round2_native_fold_members_stay_bounded_and_separate_by_computer() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let shade = FakeShade::default();
        let first = Alert {
            bundle: true,
            alert_type: Some(AlertType::Reply),
            ..alert(AttentionKind::Completed, 2)
        };
        for _ in 0..MAX_SHOWING {
            let a = Alert {
                attention_id: AttentionId::new(),
                ..first.clone()
            };
            post_from_app_fold_with(
                &shade,
                note(&computer(), &a, Detail::Minimal).unwrap(),
                Some(app_fold_for(&a)),
                shade.clears(),
                || 1_000,
            )
            .unwrap();
        }
        assert!(
            post_from_app_fold_with(
                &shade,
                note(&computer(), &first, Detail::Minimal).unwrap(),
                Some(app_fold_for(&first)),
                shade.clears(),
                || 1_000
            )
            .is_err()
        );
        let p1 = lock(&shade.posted).last().unwrap().clone();
        assert_eq!(p1.handles.len(), MAX_SHOWING);
        assert!(p1.handles.iter().all(|tag| tag.starts_with("p1/")));
        let other = Computer {
            profile_id: "p2".into(),
            name: "Laptop".into(),
        };
        post_from_app_fold_with(
            &shade,
            note(&other, &first, Detail::Minimal).unwrap(),
            Some(AppFold {
                computer: other.name.clone(),
                ..app_fold_for(&first)
            }),
            shade.clears(),
            || 1_000,
        )
        .unwrap();
        assert_eq!(shade.shown().len(), 2);
        let book = saved(dir.path());
        assert_eq!(
            book.computers["p1"].folds[&first.project_id].members.len(),
            MAX_SHOWING
        );
        assert_eq!(
            book.computers["p2"].folds[&first.project_id].members.len(),
            1
        );
        assert_eq!(shade.sounded(), 2);
        lock(&LEDGER).take();
    }

    #[test]
    fn round2_failed_native_fold_candidates_never_commit_new_members() {
        for mode in ["off", "cleared", "full", "failed"] {
            let dir = tempfile::tempdir().unwrap();
            let _one = ledger(dir.path());
            let shade = FakeShade::default();
            with_book(|book| book.started("before")).unwrap();
            let first = Alert {
                bundle: true,
                alert_type: Some(AlertType::Reply),
                ..alert(AttentionKind::Completed, 2)
            };
            let second = Alert {
                attention_id: AttentionId::new(),
                ..first.clone()
            };
            post_from_app_fold_with(
                &shade,
                note(&computer(), &first, Detail::Minimal).unwrap(),
                Some(app_fold_for(&first)),
                shade.clears(),
                || 1_000,
            )
            .unwrap();
            match mode {
                "off" => shade.tier(AlertType::Reply, Tier::Off),
                "cleared" => *lock(&shade.racing) = Some(tag("p1", &second.attention_id)),
                "full" => shade.full.store(true, Ordering::SeqCst),
                _ => shade.refuse.store(true, Ordering::SeqCst),
            }
            let _ = post_from_app_fold_with(
                &shade,
                note(&computer(), &second, Detail::Minimal).unwrap(),
                Some(app_fold_for(&second)),
                shade.clears(),
                || 1_001,
            );
            let mut book = saved(dir.path());
            let seen = &book.computers["p1"];
            assert!(!seen.raised.contains(&second.attention_id), "{mode}");
            assert!(!seen.shown_last.contains(&second.attention_id), "{mode}");
            assert!(
                !seen.folds[&first.project_id]
                    .members
                    .contains_key(&second.attention_id),
                "{mode}"
            );
            book.started("after");
            assert!(
                !book.computers["p1"].restore.contains(&second.attention_id),
                "{mode}"
            );
            assert_eq!(shade.sounded(), 1);
            lock(&LEDGER).take();
        }
    }

    #[test]
    fn round2_unsuccessful_foreground_posts_are_not_restorable() {
        for mode in ["off", "cleared", "full", "failed"] {
            let dir = tempfile::tempdir().unwrap();
            let _one = ledger(dir.path());
            let shade = FakeShade::default();
            let a = Alert {
                alert_type: Some(AlertType::Reply),
                ..alert(AttentionKind::Completed, 2)
            };
            with_book(|book| {
                book.started("before");
                rx(book, open(&[], 1), true, &[]);
                rx(
                    book,
                    AlertServerFrame::Raised {
                        alert: Box::new(a.clone()),
                    },
                    true,
                    &[],
                );
            })
            .unwrap();
            match mode {
                "off" => shade.tier(AlertType::Reply, Tier::Off),
                "cleared" => *lock(&shade.racing) = Some(tag("p1", &a.attention_id)),
                "full" => shade.full.store(true, Ordering::SeqCst),
                _ => shade.refuse.store(true, Ordering::SeqCst),
            }
            let _ = post_from_app_with(
                &shade,
                note(&computer(), &a, Detail::Minimal).unwrap(),
                shade.clears(),
                || 1_000,
            );
            let mut book = saved(dir.path());
            assert!(book.computers["p1"].raised.is_empty(), "{mode}");
            assert!(book.computers["p1"].shown_last.is_empty(), "{mode}");
            book.started("after");
            assert!(book.computers["p1"].restore.is_empty(), "{mode}");
            if mode == "off" {
                assert!(book.computers["p1"].notified.contains(&a.attention_id));
            }
            lock(&LEDGER).take();
        }
    }

    #[test]
    fn a_burst_from_one_computer_makes_one_sound() {
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
            .unwrap()
        };
        frame(open(&[], 1));
        for i in 0..10 {
            frame(AlertServerFrame::Raised {
                alert: Box::new(alert(AttentionKind::Approval, 2 + i)),
            });
        }
        assert_eq!(shade.shown().len(), 10);
        assert_eq!(shade.sounded(), 1);
        // Another computer's first still sounds.
        let laptop = Computer {
            profile_id: "p2".into(),
            name: "Laptop".into(),
        };
        let other = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| receive(book, &laptop, frame, false, shown, Timestamp(0)),
                |_| (),
            )
            .unwrap()
        };
        other(open(&[], 1));
        other(AlertServerFrame::Raised {
            alert: Box::new(alert(AttentionKind::Approval, 50)),
        });
        assert_eq!(shade.sounded(), 2);
        lock(&LEDGER).take();
    }

    #[test]
    fn a_dismissal_from_android_is_recorded_without_the_watcher() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        configure(ON).unwrap();
        let question = alert(AttentionKind::Question, 2);
        with_book(|book| {
            rx(book, open(&[], 1), false, &[]);
            rx(
                book,
                AlertServerFrame::Raised {
                    alert: Box::new(question.clone()),
                },
                false,
                &[],
            );
        })
        .unwrap();
        let shade = FakeShade::default();
        dismissed(dir.path(), &tag("p1", &question.attention_id), &shade).unwrap();
        // Not a computer's alert: nothing.
        dismissed(dir.path(), "more", &shade).unwrap();
        let book = saved(dir.path());
        assert!(book.computers["p1"].unsent.contains(&question.attention_id));
        assert!(!book.computers["p1"].raised.contains(&question.attention_id));
        // The next list doesn't raise it.
        let mut book = book;
        assert!(
            rx(
                &mut book,
                open(std::slice::from_ref(&question), 3),
                false,
                &[]
            )
            .post
            .is_empty()
        );
        lock(&LEDGER).take();
    }

    #[test]
    fn the_update_to_version_2_counts_all_open_as_raised_and_says_so_once() {
        // A book version 1 wrote: one alert raised and swiped away, and one
        // the app showed, both still open.
        let (swiped, unseen) = (
            alert(AttentionKind::Completed, 2),
            alert(AttentionKind::Approval, 3),
        );
        let mut old = Book {
            version: 1,
            settings: Some(ON),
            ..Book::default()
        };
        old.started("4:1");
        rx(&mut old, open(&[], 1), false, &[]);
        rx(
            &mut old,
            AlertServerFrame::Raised {
                alert: Box::new(swiped.clone()),
            },
            false,
            &[],
        );
        old.computers.insert("p2".into(), Seen::began(Timestamp(0)));
        let mut text = serde_json::to_value(&old).unwrap();
        text["version"] = 1.into();
        // Version 1 re-raised these after an update; reading it changes that.
        let mut book: Book = serde_json::from_value(text).unwrap();
        book.migrate();
        assert_eq!(book.version, VERSION);
        book.started("4:2");
        // An approval, a question and an answered alert are open, one never
        // raised here and made after the phone began watching.
        let question = alert(AttentionKind::Question, 4);
        let listed = [swiped.clone(), unseen.clone(), question.clone()];
        let changes = rx(&mut book, open(&listed, 50), false, &[]);
        assert!(changes.post.is_empty());
        assert!(
            listed
                .iter()
                .all(|a| book.computers["p1"].knows(&a.attention_id))
        );
        // One silent note says so, with what waits for the person.
        let note = book.calm_note(&[]).expect("the note");
        assert_eq!(note.tag, CALM);
        assert!(note.silent);
        assert_eq!(note.category, Category::Notice);
        assert_eq!(note.title, "Notifications are calmer now");
        assert!(
            note.body.ends_with(" 2 things wait for you in Needs you."),
            "{}",
            note.body
        );
        // Unchanged, it isn't posted again.
        assert!(book.calm_note(&[CALM.to_owned()]).is_none());
        // The other computer adds its own count, silently, while it shows.
        let laptop = Computer {
            profile_id: "p2".into(),
            name: "Laptop".into(),
        };
        let waiting = alert(AttentionKind::WaitingForYou, 5);
        receive(
            &mut book,
            &laptop,
            open(std::slice::from_ref(&waiting), 60),
            false,
            &[],
            Timestamp(0),
        );
        let note = book.calm_note(&[CALM.to_owned()]).expect("a new count");
        assert!(note.body.ends_with(" 3 things wait for you in Needs you."));
        // Every computer has said: it is done, and never comes back.
        assert!(book.calm.is_none());
        book.migrate();
        assert!(book.calm.is_none());
        // The next list from each raises nothing from before.
        assert!(rx(&mut book, open(&listed, 70), false, &[]).post.is_empty());
        // Swiped away before the second computer answers, it stays gone.
        let mut other: Book = serde_json::from_value({
            let mut t = serde_json::to_value(&old).unwrap();
            t["version"] = 1.into();
            t
        })
        .unwrap();
        other.migrate();
        rx(&mut other, open(&listed, 50), false, &[]);
        assert!(other.calm_note(&[]).is_some());
        receive(
            &mut other,
            &laptop,
            open(std::slice::from_ref(&waiting), 60),
            false,
            &[],
            Timestamp(0),
        );
        assert!(other.calm_note(&[]).is_none());
        assert!(other.calm.is_none());
    }

    #[test]
    fn the_calm_note_reaches_the_shade_once() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        with_book(|book| {
            *book = Book {
                version: 1,
                settings: Some(ON),
                ..Book::default()
            };
            book.computers
                .insert("p1".into(), Seen::began(Timestamp(0)));
            book.migrate();
        })
        .unwrap();
        let shade = FakeShade::default();
        let frame = |frame: AlertServerFrame| {
            update(
                current_watcher(),
                &shade,
                &BTreeSet::new(),
                |book, shown| rx(book, frame, false, shown),
                |_| (),
            )
            .unwrap()
        };
        frame(open(&[alert(AttentionKind::Completed, 2)], 10));
        assert_eq!(shade.shown(), vec![CALM.to_owned()]);
        let posted = lock(&shade.posted).clone();
        assert_eq!(posted.len(), 1);
        assert_eq!(
            posted[0].body,
            "Notifications you swipe away stay away, even after an update."
        );
        assert!(saved(dir.path()).calm.is_none());
        frame(open(&[], 20));
        assert_eq!(lock(&shade.posted).len(), 1);
        lock(&LEDGER).take();
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
                    alert: Box::new(alert(kind, 1)),
                },
                false,
                &[],
            );
            let note = changes.post[0].note(book.detail()).unwrap();
            assert_eq!((note.title.as_str(), note.category), (title, category));
            assert_eq!(note.body, "Studio · Site · Fix the header");
            assert!(note.group.is_some());
            assert_eq!(note.group_title.as_deref(), Some("Site · Studio"));
        }
        let none = rx(
            &mut book,
            AlertServerFrame::Raised {
                alert: Box::new(alert(AttentionKind::ConnectionIssue, 1)),
            },
            false,
            &[],
        );
        assert!(none.post.is_empty());
    }

    #[test]
    fn a_chat_reply_is_a_new_reply() {
        let mut a = alert(AttentionKind::Completed, 1);
        a.alert_type = Some(AlertType::Reply);
        let note = note(&computer(), &a, Detail::Minimal).unwrap();
        assert_eq!(note.title, "New reply");
        assert_eq!(note.alert_type, Some(AlertType::Reply));
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
    fn the_book_survives_a_restart_and_off_keeps_what_was_raised() {
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
                alert: Box::new(question.clone()),
            };
            rx(book, frame, false, &[]);
        })
        .unwrap();
        assert!(with_book(|b| b.computers.contains_key("p1")).unwrap());
        // Off only stops alerts once the app is closed: what was raised is
        // still known, so nothing comes back when it is on again.
        configure(Settings {
            enabled: false,
            ..on
        })
        .unwrap();
        let saved: Book = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(saved.computers["p1"].knows(&question.attention_id));
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

    #[test]
    fn when_a_computer_was_heard_from_is_saved_in_minute_steps() {
        let mut book = Book::default();
        book.heard_from("p1", Timestamp(1_000));
        assert_eq!(book.heard["p1"], 1_000);
        // A ping half a minute on is not worth a write.
        book.heard_from("p1", Timestamp(31_000));
        assert_eq!(book.heard["p1"], 1_000);
        book.heard_from("p1", Timestamp(61_000));
        assert_eq!(book.heard["p1"], 61_000);
        // The phone's clock was set back.
        book.heard_from("p1", Timestamp(5_000));
        assert_eq!(book.heard["p1"], 5_000);
        book.heard_from("p2", Timestamp(7_000));
        assert_eq!(book.heard.len(), 2);
    }

    #[test]
    fn when_a_computer_was_heard_from_is_kept_until_it_is_unpaired() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        let watcher = next_watcher();
        note_heard("p1", watcher);
        note_heard("p2", watcher);
        assert_eq!(heard().keys().collect::<Vec<_>>(), ["p1", "p2"]);
        assert!(saved(dir.path()).heard.contains_key("p1"));
        // A stopped watcher notes nothing: its replacement may already watch.
        let stale = watcher;
        let watcher = next_watcher();
        lock(&LEDGER).as_mut().unwrap().book.heard.clear();
        note_heard("p1", stale);
        assert!(heard().is_empty());
        note_heard("p1", watcher);
        note_heard("p2", watcher);
        // Turning alerts off, or a computer refusing this phone, does not
        // change when it was last heard from.
        configure(Settings {
            enabled: false,
            detail: Detail::Minimal,
        })
        .unwrap();
        refused("p1", &FakeShade::default(), watcher);
        assert_eq!(heard().len(), 2);
        forget_others(
            &BTreeSet::from(["p2".to_owned()]),
            &FakeShade::default(),
            watcher,
        );
        assert_eq!(heard().keys().collect::<Vec<_>>(), ["p2"]);
        assert_eq!(saved(dir.path()).heard.keys().collect::<Vec<_>>(), ["p2"]);
        lock(&LEDGER).take();
    }

    #[test]
    fn a_book_saved_before_heard_was_kept_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let _one = ledger(dir.path());
        lock(&LEDGER).take();
        fs::write(
            dir.path().join(FILE),
            br#"{"version":1,"settings":{"enabled":true,"detail":"minimal"},"computers":{}}"#,
        )
        .unwrap();
        init(dir.path());
        assert_eq!(settings().unwrap().map(|s| s.enabled), Some(true));
        assert!(heard().is_empty());
        lock(&LEDGER).take();
    }
}
