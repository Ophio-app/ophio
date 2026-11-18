// SPDX-License-Identifier: Apache-2.0
//! Telling the person when a newer Øphio is out for this computer.
//!
//! Once a day the computer asks the public repository for its latest
//! release ([`ports::releases::ReleaseSource`]), without signing in. A
//! stable version newer than the one installed raises one attention item,
//! "Øphio 0.3.0 is available", which the person's phones show under New
//! Øphio versions and Settings › Versions explains. Nothing is downloaded.
//!
//! - Each version is raised once on this computer, ever: a notice handled
//!   on one phone goes from all of them as any other does, and turning the
//!   check off and on, or a restart, never raises it again.
//! - A newer version still replaces the notice for an older one.
//! - Once the computer runs that version or a later one, the notice clears.
//! - A check that gets no usable answer says nothing, keeps what was known
//!   and is tried again later.
//! - Each change shows on every phone at once: the view rides in the host
//!   record ([`protocol::model::HostInfo::release_check`]), which a
//!   `HostUpdated` carries, as older phones and computers already read.
//!
//! The item has no task and no `alert`: an older Øphio, after going back,
//! reads it as a finished item about no task, which it never shows.
use crate::CompanionHub;
use ports::releases::ReleaseSource;
use protocol::{
    AttentionId, ErrorCode, Event, ProtocolError,
    model::{AttentionItem, AttentionKind, HostInfo},
    releases::{NewerRelease, ReleaseCheckView, Version, newer_than},
    time::Timestamp,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

/// The check's setting and findings, under `meta`.
const KEY: &str = "release_check";
/// Between checks that got an answer.
pub(crate) const EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// After a check that got none, such as while the computer was offline.
pub(crate) const RETRY: Duration = Duration::from_secs(6 * 60 * 60);
/// The least wait after Øphio starts, so a start never asks at once; up to
/// [`FIRST_SPREAD`] more is added, so the first check comes 2 to 20 minutes
/// after a start.
const FIRST: Duration = Duration::from_secs(2 * 60);
const FIRST_SPREAD: Duration = Duration::from_secs(18 * 60);
/// The least wait after a check, so nothing spins.
const AGAIN: Duration = Duration::from_secs(60);
/// Up to this much more is added to each later check's wait, so computers
/// started together don't all ask at the same moment.
const SPREAD: Duration = Duration::from_secs(20 * 60);
/// The longest sleep: the wall clock is read again after each, as a
/// computer asleep meanwhile doesn't count that time in a sleep.
const LOOK_AGAIN: Duration = Duration::from_secs(60 * 60);
/// How many versions are remembered as told.
const TOLD_KEPT: usize = 20;
/// Below a finished task's news: nothing waits on it.
const PRIORITY: u8 = 1;

/// How the computer learns of new releases, and what it has.
#[derive(Debug, Clone)]
pub struct ReleaseCheck {
    pub source: Arc<dyn ReleaseSource>,
    /// Whether it checks until the person chooses: on in public builds, off
    /// in personal ones, which carry everything already.
    pub on_by_default: bool,
    /// The version of Øphio running here.
    pub installed: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Stored {
    /// The person's choice; none follows the build's default.
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    revision: u64,
    /// When a check last got an answer.
    #[serde(default)]
    checked_at: Option<Timestamp>,
    /// When a check last asked, answered or not.
    #[serde(default)]
    tried_at: Option<Timestamp>,
    /// The newer release the last answer named.
    #[serde(default)]
    newer: Option<NewerRelease>,
    /// The notice up for `newer`, until it clears.
    #[serde(default)]
    notice: Option<AttentionId>,
    /// The versions a notice was raised for, newest last: never again.
    #[serde(default)]
    told: Vec<String>,
}

/// The kept record; none when there is none, or it can't be read, as when
/// a newer Øphio wrote it and the computer went back to this one. Read
/// strictly, that would stop the computer starting.
fn load(r: &store::Read<'_>) -> store::Result<Stored> {
    let Some(value) = r.get::<serde_json::Value>("meta", KEY)? else {
        return Ok(Stored::default());
    };
    Ok(serde_json::from_value(value).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "the version check's record can't be read; starting afresh");
        Stored::default()
    }))
}

impl Stored {
    fn enabled(&self, config: &ReleaseCheck) -> bool {
        self.enabled.unwrap_or(config.on_by_default)
    }
}

/// How to update a computer of this system to `version`, for the person.
/// Linux and Macs accept an archive; Windows needs the extracted folder.
fn how_to_update(version: Option<&str>) -> String {
    if cfg!(windows) {
        return windows_how_to_update().into();
    }
    // Downloads are named for their version and system, as
    // ophio-0.2.0-x86_64-linux.tar.gz.
    let system = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    match version {
        Some(v) => format!(
            "Download it from the release page, then on the computer run ophio update with the download, such as ophio update ophio-{v}-{}-{system}.tar.gz.",
            std::env::consts::ARCH
        ),
        None => "Download it from the release page, then on the computer run ophio update with the download.".into(),
    }
}

/// Kept separate so the Windows guidance can also be tested on other systems.
fn windows_how_to_update() -> &'static str {
    r#"Download the Windows ZIP from the release page, unblock it, and extract it into a new folder outside the installation and Øphio's Library. In ordinary, non-administrator PowerShell in %LOCALAPPDATA%\Programs\Ophio, run .\ophio.exe update "C:\path\to\extracted-folder". For an older copy without an updater, run .\ophio.exe install from the new extracted folder instead. Updating waits for one idle minute and keeps your choice of whether Øphio starts at login."#
}

/// The view, as the person's choice and the last answer leave it. What a
/// check found is only shown while the check is on, and while it is still
/// newer than what runs here.
fn view(config: &ReleaseCheck, stored: &Stored, checking: bool) -> ReleaseCheckView {
    let enabled = stored.enabled(config);
    let newer = (stored.newer.clone())
        .filter(|n| enabled && newer_than(&n.version, &config.installed).is_some());
    ReleaseCheckView {
        enabled,
        revision: stored.revision,
        installed: config.installed.clone(),
        checked_at: stored.checked_at,
        how_to_update: how_to_update(newer.as_ref().map(|n| n.version.as_str())),
        newer,
        checking: checking && enabled,
    }
}

/// What a check found is forgotten, and its notice cleared, while the check
/// is effectively off: the person turned it off, or this build's default is
/// off and they never chose.
fn forget_while_off(
    tx: &mut store::Write<'_>,
    config: &ReleaseCheck,
    stored: &mut Stored,
    now: Timestamp,
) -> store::Result<()> {
    if stored.enabled(config) {
        return Ok(());
    }
    // A check answered before it was turned off says nothing of now: turned
    // on again, nothing claims to be the newest until a check answers.
    stored.newer = None;
    stored.checked_at = None;
    if let Some(id) = stored.notice.take() {
        resolve(tx, id, now)?;
    }
    Ok(())
}

/// Keeps what an answer said is newest: the notice for an older version, or
/// for one no longer newer, goes; a version never told raises its own.
fn found(
    tx: &mut store::Write<'_>,
    stored: &mut Stored,
    newer: Option<NewerRelease>,
    computer: &str,
    now: Timestamp,
) -> store::Result<()> {
    let same = |a: &Option<NewerRelease>, b: &Option<NewerRelease>| {
        a.as_ref().map(|n| &n.version) == b.as_ref().map(|n| &n.version)
    };
    if !same(&stored.newer, &newer)
        && let Some(id) = stored.notice.take()
    {
        resolve(tx, id, now)?;
    }
    stored.newer = newer;
    let Some(newer) = &stored.newer else {
        return Ok(());
    };
    if stored.notice.is_some() || stored.told.contains(&newer.version) {
        return Ok(());
    }
    let item = AttentionItem {
        child_id: None,
        attention_id: AttentionId::new(),
        kind: AttentionKind::Completed,
        title: format!("Øphio {} is available", newer.version),
        body: Some(format!("{computer} can be updated to it.")),
        task_id: None,
        run_id: None,
        approval_id: None,
        created_at: now,
        resolved_at: None,
        priority: PRIORITY,
        topic: None,
        quiet: false,
        report_id: None,
        account_id: None,
        alert: None,
        alert_handled_at: None,
        agent_update: None,
        new_version: Some(newer.version.clone()),
    };
    stored.notice = Some(item.attention_id);
    stored.told.push(newer.version.clone());
    let extra = stored.told.len().saturating_sub(TOLD_KEPT);
    stored.told.drain(..extra);
    tx.put("attention", item.attention_id, &item)?;
    tx.emit(Event::AttentionUpserted { item })?;
    Ok(())
}

/// Clears a notice: it goes from every phone.
fn resolve(tx: &mut store::Write<'_>, id: AttentionId, now: Timestamp) -> store::Result<()> {
    let Some(mut item) = tx.read().get::<AttentionItem>("attention", id)? else {
        return Ok(());
    };
    if item.resolved_at.is_some() {
        return Ok(());
    }
    item.resolved_at = Some(now);
    tx.put("attention", id, &item)?;
    tx.emit(Event::AttentionResolved { attention_id: id })?;
    Ok(())
}

/// When the next check is due: a day after an attempt that was answered,
/// six hours after one that wasn't; none before any attempt was kept, when
/// the round's own earliest time decides.
fn due_at(stored: &Stored) -> Option<Timestamp> {
    let tried = stored.tried_at?;
    let answered = stored.checked_at.is_some_and(|c| c >= tried);
    Some(later(tried, if answered { EVERY } else { RETRY }))
}

/// Some time from none up to `most`, different each time.
fn spread(most: Duration) -> Duration {
    let millis = most.as_millis() as u64;
    if millis == 0 {
        return Duration::ZERO;
    }
    Duration::from_millis(getrandom::u64().unwrap_or(0) % millis)
}

fn not_configured() -> ProtocolError {
    crate::error(
        ErrorCode::Unsupported,
        "This computer doesn't check for new versions of Øphio.",
    )
}

impl CompanionHub {
    fn release_config(&self) -> Result<&ReleaseCheck, ProtocolError> {
        self.inner.release_check.as_ref().ok_or_else(not_configured)
    }
    fn release_busy(&self) -> bool {
        self.inner.release_busy.load(Ordering::SeqCst)
    }
    pub(crate) fn release_check_view(&self) -> Result<ReleaseCheckView, ProtocolError> {
        let config = self.release_config()?;
        let stored: Stored = self.read(load)?;
        Ok(view(config, &stored, self.release_busy()))
    }
    /// The view for the host record; none where the computer doesn't check.
    pub(crate) fn release_check_for_host(
        &self,
        r: &store::Read<'_>,
    ) -> store::Result<Option<ReleaseCheckView>> {
        let Some(config) = &self.inner.release_check else {
            return Ok(None);
        };
        let stored: Stored = load(r)?;
        Ok(Some(view(config, &stored, self.release_busy())))
    }
    /// Keeps `stored`, and shows its view on every phone when it changed.
    fn keep_release(&self, tx: &mut store::Write<'_>, stored: &Stored) -> store::Result<()> {
        tx.put("meta", KEY, stored)?;
        self.publish_release(tx, stored)
    }
    /// Shows the view of `stored` on every phone, when it changed.
    fn publish_release(&self, tx: &mut store::Write<'_>, stored: &Stored) -> store::Result<()> {
        let Some(config) = &self.inner.release_check else {
            return Ok(());
        };
        let now = view(config, stored, self.release_busy());
        // The host is written at every start, before any of this runs. One
        // that can't be read is left to that.
        if let Some(mut host) = tx.read().get::<HostInfo>("meta", "host").ok().flatten()
            && host.release_check.as_ref() != Some(&now)
        {
            host.release_check = Some(now);
            tx.put("meta", "host", &host)?;
            tx.emit(Event::HostUpdated {
                host: Box::new(host),
            })?;
        }
        Ok(())
    }
    /// Turns the daily check on or off. Off, the notice for a newer version
    /// goes too: the person asked not to hear of them. Turned on from off,
    /// a check is due at once; on already, nothing changes when it asks.
    pub(crate) fn set_release_check(
        &self,
        tx: &mut store::Write<'_>,
        enabled: bool,
        revision: Option<u64>,
    ) -> store::Result<()> {
        let Some(config) = &self.inner.release_check else {
            return Err(not_configured().into());
        };
        let mut stored: Stored = load(&tx.read())?;
        domain::revision(revision, stored.revision)?;
        let was = stored.enabled(config);
        stored.enabled = Some(enabled);
        stored.revision += 1;
        if enabled && !was {
            stored.tried_at = None;
        }
        forget_while_off(tx, config, &mut stored, self.inner.clock.now())?;
        self.keep_release(tx, &stored)
    }
    /// After a change of the setting: the schedule is looked at again, so a
    /// check turned on asks at once.
    pub(crate) fn release_check_changed(&self) {
        self.inner.release_wake.notify_one();
    }
    /// As Øphio starts: a notice for a version no newer than the one now
    /// running clears, as the computer was updated; so does one kept while
    /// the check is effectively off, as when a personal build opens a
    /// public build's data.
    pub(crate) fn release_check_started(&self) -> Result<(), ProtocolError> {
        let config = self.release_config()?.clone();
        let now = self.inner.clock.now();
        self.write(|tx| {
            let mut stored: Stored = load(&tx.read())?;
            forget_while_off(tx, &config, &mut stored, now)?;
            let current = (stored.newer.as_ref())
                .is_some_and(|n| newer_than(&n.version, &config.installed).is_some());
            if !current {
                stored.newer = None;
                if let Some(id) = stored.notice.take() {
                    resolve(tx, id, now)?;
                }
            }
            self.keep_release(tx, &stored)
        })
    }
    /// Asks for the latest release once, while the check is on, and keeps
    /// what it says. The attempt is kept before asking, so a restart or a
    /// failed save meanwhile waits as after a check that got no answer. An
    /// answer that names no stable version, or none at all, is no usable
    /// answer: what was known stays.
    pub(crate) async fn check_release(&self) -> Result<(), ProtocolError> {
        let config = self.release_config()?.clone();
        let _one = self.inner.release_checking.lock().await;
        let asked_at = self.inner.clock.now();
        let busy = Busy::start(self);
        let asking = self.write(|tx| {
            let mut stored: Stored = load(&tx.read())?;
            if !stored.enabled(&config) {
                return Ok(false);
            }
            stored.tried_at = Some(asked_at);
            self.keep_release(tx, &stored)?;
            Ok(true)
        });
        if !asking? {
            return Ok(());
        }
        let answer = config.source.latest().await;
        let now = self.inner.clock.now();
        let usable = match answer {
            Ok(Some(latest)) => match Version::parse(&latest.tag).filter(Version::stable) {
                Some(version) => Some((version, latest.url)),
                None => {
                    tracing::debug!(tag = %latest.tag, "the latest release names no stable version");
                    None
                }
            },
            Ok(None) => None,
            Err(reason) => {
                tracing::debug!(%reason, "checking for a newer Øphio got no answer");
                None
            }
        };
        drop(busy);
        let kept = self.write(|tx| {
            let mut stored: Stored = load(&tx.read())?;
            // Turned off meanwhile: what was found isn't for the person.
            if let Some((version, url)) = usable.filter(|_| stored.enabled(&config)) {
                stored.checked_at = Some(now);
                let installed = Version::parse(&config.installed);
                let newer = installed
                    .is_some_and(|i| version > i)
                    .then(|| NewerRelease {
                        version: version.to_string(),
                        url,
                    });
                let computer = self.computer_name(&tx.read())?;
                found(tx, &mut stored, newer, &computer, now)?;
            }
            self.keep_release(tx, &stored)
        });
        // Whatever kept it from being saved, the phones aren't left saying a
        // check is asking.
        if kept.is_err()
            && let Err(e) = self.write(|tx| {
                let stored = load(&tx.read())?;
                self.publish_release(tx, &stored)
            })
        {
            tracing::warn!(code = ?e.code, "saying the version check ended failed");
        }
        kept
    }
    /// A kept attempt later than `now`: the clock was fast and has been put
    /// right. The attempt counts as made now, keeping whether it was
    /// answered, so the schedule isn't held back by however far ahead the
    /// clock was.
    fn release_clock_put_right(&self, now: Timestamp) -> Result<(), ProtocolError> {
        // Read first: nearly always there is nothing to put right.
        if self.read(load)?.tried_at.is_none_or(|t| t <= now) {
            return Ok(());
        }
        self.write(|tx| {
            let mut stored = load(&tx.read())?;
            let Some(tried) = stored.tried_at.filter(|t| *t > now) else {
                return Ok(());
            };
            let answered = stored.checked_at.is_some_and(|c| c >= tried);
            stored.tried_at = Some(now);
            if answered {
                stored.checked_at = Some(now);
            } else {
                stored.checked_at = stored.checked_at.map(|c| Timestamp(c.0.min(now.0 - 1)));
            }
            tx.put("meta", KEY, &stored)
        })
    }
    /// Checks while the hub runs: first 2 to 20 minutes after it starts,
    /// then as each check falls due by the wall clock, and at once when the
    /// check is turned on.
    pub(crate) fn start_release_checks(&self) {
        if self.inner.release_check.is_none() {
            return;
        }
        if let Err(e) = self.release_check_started() {
            tracing::error!(code = ?e.code, "clearing an earlier new-version notice failed");
        }
        let weak = Arc::downgrade(&self.inner);
        let wake = self.inner.release_wake.clone();
        let shutdown = self.inner.shutdown.subscribe();
        let nap: Nap = Arc::new(|d| Box::pin(tokio::time::sleep(d)));
        self.spawn(release_loop(weak, wake, shutdown, nap));
    }
}

/// How the schedule waits: a real sleep, or a step of a fake clock in tests.
pub(crate) type Nap = Arc<
    dyn Fn(Duration) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// The schedule: the first check 2 to 20 minutes after a start, then each
/// as it falls due by the wall clock, and at once when the check is turned
/// on. Each round's time is fixed once, as an absolute time with its own
/// spread, and the check starts when the clock reaches it.
pub(crate) async fn release_loop(
    weak: std::sync::Weak<crate::hub::Inner>,
    wake: Arc<tokio::sync::Notify>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    nap: Nap,
) {
    let Some(started) = weak.upgrade().map(|inner| inner.clock.now()) else {
        return;
    };
    let mut not_before = later(started, FIRST + spread(FIRST_SPREAD));
    let mut jitter = Duration::ZERO;
    // The clock as last read: one that went back since was put right, and
    // what was set by the fast one moves back with it.
    let mut seen = started;
    loop {
        let (now, due) = {
            let Some(inner) = weak.upgrade() else { break };
            let hub = CompanionHub { inner };
            let Some(config) = hub.inner.release_check.clone() else {
                break;
            };
            let now = hub.inner.clock.now();
            if now.0 < seen.0 {
                not_before = Timestamp(not_before.0 - (seen.0 - now.0));
            }
            seen = now;
            if let Err(e) = hub.release_clock_put_right(now) {
                tracing::warn!(code = ?e.code, "putting the version check's schedule right failed");
            }
            let stored = hub.read(load).unwrap_or_default();
            // A fixed time either way, never one worked out from now: a
            // time that moves with the clock is never reached.
            let due = stored.enabled(&config).then(|| match due_at(&stored) {
                None => not_before,
                Some(at) => {
                    let at = later(at, jitter);
                    if at.0 < not_before.0 { not_before } else { at }
                }
            });
            (now, due)
        };
        if due.is_some_and(|at| at.0 <= now.0) {
            let Some(inner) = weak.upgrade() else { break };
            let hub = CompanionHub { inner };
            let checked = tokio::select! {
                _ = shutdown.changed() => break,
                checked = hub.check_release() => checked,
            };
            let now = hub.inner.clock.now();
            // A check that couldn't even be saved waits as one that got no
            // answer, so a failing disk isn't asked again every minute.
            not_before = later(now, if checked.is_err() { RETRY } else { AGAIN });
            if let Err(e) = checked {
                tracing::error!(code = ?e.code, "checking for a newer Øphio failed");
            }
            jitter = spread(SPREAD);
            continue;
        }
        let wait = due.map(|at| until(now, at));
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = nap(nap_for(wait)) => {}
            _ = wake.notified() => {
                // Turned on, or off: a check due now asks at once.
                let Some(inner) = weak.upgrade() else { break };
                not_before = inner.clock.now();
                jitter = Duration::ZERO;
            }
        }
    }
}

/// How long to sleep towards a check `wait` away: at most an hour, after
/// which the wall clock is read again. A sleep doesn't count the time the
/// computer was suspended, so a laptop asleep most of the day would
/// otherwise check days late.
fn nap_for(wait: Option<Duration>) -> Duration {
    wait.unwrap_or(LOOK_AGAIN).min(LOOK_AGAIN)
}

/// `at` moved on by `by`.
fn later(at: Timestamp, by: Duration) -> Timestamp {
    Timestamp(at.0.saturating_add(by.as_millis() as i64))
}

/// How long from `now` until `at`; none once it has passed.
fn until(now: Timestamp, at: Timestamp) -> Duration {
    Duration::from_millis(at.0.saturating_sub(now.0).max(0) as u64)
}

/// Marks a check under way while it lives, for the view's `checking`.
struct Busy<'a>(&'a CompanionHub);
impl<'a> Busy<'a> {
    fn start(hub: &'a CompanionHub) -> Self {
        hub.inner.release_busy.store(true, Ordering::SeqCst);
        Self(hub)
    }
}
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.inner.release_busy.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_falls_due_a_day_after_an_answer_and_sooner_after_none() {
        let hour = 60 * 60 * 1000;
        let at = |h: i64| Timestamp(h * hour);
        // Never asked: the round's earliest time decides.
        assert_eq!(due_at(&Stored::default()), None);
        let answered = Stored {
            tried_at: Some(at(10)),
            checked_at: Some(at(10)),
            ..Stored::default()
        };
        assert_eq!(due_at(&answered), Some(at(34)));
        let unanswered = Stored {
            tried_at: Some(at(10)),
            checked_at: Some(at(2)),
            ..Stored::default()
        };
        assert_eq!(due_at(&unanswered), Some(at(16)));
        assert!(spread(SPREAD) < SPREAD);
        assert_eq!(spread(Duration::ZERO), Duration::ZERO);
        // A sleep is never longer than an hour, so the wall clock is read
        // again after a suspension.
        assert_eq!(nap_for(Some(EVERY)), LOOK_AGAIN);
        assert_eq!(
            nap_for(Some(Duration::from_secs(120))),
            Duration::from_secs(120)
        );
        assert_eq!(nap_for(None), LOOK_AGAIN);
        // The first check after a start comes 2 to 20 minutes later.
        assert_eq!(FIRST + FIRST_SPREAD, Duration::from_secs(20 * 60));
    }

    #[test]
    fn the_way_to_update_fits_the_computer_and_the_version() {
        let words = how_to_update(Some("0.3.0"));
        if cfg!(windows) {
            assert_eq!(words, windows_how_to_update());
            assert_eq!(how_to_update(None), windows_how_to_update());
        } else {
            let file = format!("ophio update ophio-0.3.0-{}-", std::env::consts::ARCH);
            assert!(words.contains(&file), "{words}");
            assert!(!how_to_update(None).contains("0."));
        }
    }

    #[test]
    fn windows_update_guidance_uses_the_extracted_download_and_stable_launcher() {
        let words = windows_how_to_update();
        for expected in [
            "Windows ZIP",
            "unblock it",
            "extract it into a new folder outside the installation and Øphio's Library",
            "ordinary, non-administrator PowerShell",
            r"%LOCALAPPDATA%\Programs\Ophio",
            r#".\ophio.exe update "C:\path\to\extracted-folder""#,
            "waits for one idle minute",
        ] {
            assert!(words.contains(expected), "missing {expected:?}: {words}");
        }
    }

    #[test]
    fn windows_update_guidance_migrates_older_copies_without_manual_replacement() {
        let words = windows_how_to_update();
        assert!(
            words.contains(
                r"For an older copy without an updater, run .\ophio.exe install from the new extracted folder instead."
            ),
            "{words}"
        );
        assert!(
            words.contains("keeps your choice of whether Øphio starts at login"),
            "{words}"
        );
        assert!(words.chars().count() < 500, "{words}");
        assert!(!words.contains("--now"), "{words}");
        assert!(!words.contains("check progress"), "{words}");
        assert!(!words.contains("service uninstall"), "{words}");
        assert!(!words.contains("service install"), "{words}");
        assert!(!words.contains("replace ophio.exe"), "{words}");
    }
}
