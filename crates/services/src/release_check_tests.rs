// SPDX-License-Identifier: Apache-2.0
//! A newer Øphio out for this computer: one notice per version, never again,
//! cleared once the computer runs it; the check follows the build's default
//! until the owner chooses. The source is faked; no test asks GitHub.
use super::*;
use crate::api::Hub;
use ports::releases::{LatestRelease, ReleaseSource};
use protocol::{
    model::{AttentionItem, AttentionKind},
    notifications::AlertType,
    releases::{RELEASE_CHECK_FEATURE, ReleaseCheckView},
};
use std::sync::Mutex;

/// Answers each question with what it is set to say, counting them.
#[derive(Debug, Default)]
struct Releases {
    answer: Mutex<Option<Result<Option<LatestRelease>, String>>>,
    asked: Mutex<usize>,
    /// When set, each answer waits for a permit.
    gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}
impl Releases {
    fn says(&self, tag: Option<&str>) {
        *self.answer.lock().unwrap() = Some(Ok(tag.map(|tag| LatestRelease {
            tag: tag.into(),
            url: format!("https://github.com/Ophio-app/ophio/releases/tag/{tag}"),
        })));
    }
    fn fails(&self) {
        *self.answer.lock().unwrap() = Some(Err("offline".into()));
    }
    fn asked(&self) -> usize {
        *self.asked.lock().unwrap()
    }
}
#[async_trait::async_trait]
impl ReleaseSource for Releases {
    async fn latest(&self) -> Result<Option<LatestRelease>, String> {
        *self.asked.lock().unwrap() += 1;
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.acquire().await.unwrap().forget();
        }
        self.answer.lock().unwrap().clone().unwrap_or(Ok(None))
    }
}

fn checking(
    source: &Arc<Releases>,
    on: bool,
    installed: &str,
) -> impl FnOnce(HubBuilder) -> HubBuilder {
    let check = crate::ReleaseCheck {
        source: source.clone(),
        on_by_default: on,
        installed: installed.into(),
    };
    move |b| b.release_check(check)
}

async fn view(f: &Fixture, caller: &Caller) -> ReleaseCheckView {
    let QueryData::ReleaseCheck { view } = f.hub.query(caller, Query::ReleaseCheck).await.unwrap()
    else {
        panic!("release check")
    };
    view
}
async fn set(f: &Fixture, caller: &Caller, enabled: bool, revision: u64) -> CommandReceipt {
    let mut command = CommandEnvelope::new(Command::SetReleaseCheck { enabled });
    command.expected_revision = Some(revision);
    f.hub.submit(caller, command).await
}
/// The notices about newer versions, oldest first.
fn notices(f: &Fixture) -> Vec<AttentionItem> {
    let mut items: Vec<AttentionItem> = (f.hub.read(|r| r.list::<AttentionItem>("attention")))
        .unwrap()
        .into_iter()
        .filter(|a| a.new_version.is_some())
        .collect();
    items.sort_by_key(|a| (a.created_at, a.attention_id));
    items
}
fn open(f: &Fixture) -> Vec<AttentionItem> {
    let mut open = notices(f);
    open.retain(|a| a.resolved_at.is_none());
    open
}

#[tokio::test]
async fn a_public_build_checks_and_a_personal_one_doesnt_until_the_owner_chooses() {
    let source = Arc::new(Releases::default());
    source.says(Some("v0.3.0"));
    let personal = Fixture::with(checking(&source, false, "0.2.0")).await;
    let off = view(&personal, &Caller::local_owner()).await;
    assert!(!off.enabled);
    assert_eq!(off.installed, "0.2.0");
    personal.hub.check_release().await.unwrap();
    assert_eq!(source.asked(), 0, "a personal build never asks by itself");
    assert!(notices(&personal).is_empty());
    // Turned on, it asks soon by itself, as any build does.
    let receipt = set(&personal, &Caller::local_owner(), true, 0).await;
    assert_eq!(receipt.error, None);
    let mut wait = waiting();
    while open(&personal).is_empty() {
        assert!(wait(), "turning the check on didn't check");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(source.asked(), 1);
    personal.close().await;

    let source = Arc::new(Releases::default());
    source.says(Some("v0.3.0"));
    let public = Fixture::with(checking(&source, true, "0.2.0")).await;
    assert!(view(&public, &Caller::local_owner()).await.enabled);
    public.hub.check_release().await.unwrap();
    assert_eq!(source.asked(), 1);
    let seen = view(&public, &Caller::local_owner()).await;
    let newer = seen.newer.unwrap();
    assert_eq!(newer.version, "0.3.0");
    assert!(newer.url.ends_with("/releases/tag/v0.3.0"));
    assert!(seen.checked_at.is_some());
    assert!(!seen.how_to_update.is_empty());
    public.close().await;
}

#[tokio::test]
async fn the_feature_is_advertised_only_where_the_computer_checks() {
    let f = Fixture::new().await;
    assert!(!(f.hub.capabilities().await.features.iter()).any(|x| x == RELEASE_CHECK_FEATURE));
    let refused = f
        .hub
        .query(&Caller::local_owner(), Query::ReleaseCheck)
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unsupported);
    let receipt = set(&f, &Caller::local_owner(), true, 0).await;
    assert_eq!(receipt.error.unwrap().code, ErrorCode::Unsupported);
    f.close().await;

    let source = Arc::new(Releases::default());
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    assert!((f.hub.capabilities().await.features.iter()).any(|x| x == RELEASE_CHECK_FEATURE));
    f.close().await;
}

#[tokio::test]
async fn each_version_raises_one_notice_that_never_comes_back() {
    let source = Arc::new(Releases::default());
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    let phone = f.device().await;
    // Nothing newer: nothing to tell.
    for tag in [
        None,
        Some("v0.2.0"),
        Some("v0.1.1"),
        Some("v0.3.0-rc.1"),
        Some("nightly"),
    ] {
        source.says(tag);
        f.hub.check_release().await.unwrap();
        assert!(notices(&f).is_empty(), "{tag:?}");
        assert_eq!(view(&f, &phone).await.newer, None);
    }

    source.says(Some("v0.3.0"));
    f.hub.check_release().await.unwrap();
    let [notice] = open(&f).try_into().unwrap();
    assert_eq!(notice.new_version.as_deref(), Some("0.3.0"));
    assert_eq!(notice.kind, AttentionKind::Completed);
    assert_eq!(notice.task_id, None);
    assert_eq!(
        notice.alert, None,
        "an older Øphio could read it after going back"
    );
    assert_eq!(notice.title, "Øphio 0.3.0 is available");
    assert_eq!(AlertType::of(&notice, false), Some(AlertType::NewVersion));
    // The phone's background watcher is sent it, in no project.
    let (alerts, _) = f.hub.alerts_open(&phone, true).unwrap();
    let [alert] = alerts.try_into().unwrap();
    assert_eq!(alert.alert_type, Some(AlertType::NewVersion));
    assert_eq!(alert.new_version.unwrap().version, "0.3.0");
    assert_eq!(alert.project_tier, None);

    // Asked again: the same version raises nothing more.
    f.hub.check_release().await.unwrap();
    assert_eq!(notices(&f).len(), 1);
    // Swiped away on the phone: it goes from every device, stays open in
    // Versions, and a later check doesn't raise it again.
    f.hub
        .mark_alerts_handled(&phone, &[notice.attention_id])
        .unwrap();
    let [handled] = open(&f).try_into().unwrap();
    assert!(handled.alert_handled_at.is_some());
    let (alerts, _) = f.hub.alerts_open(&phone, true).unwrap();
    assert!(alerts[0].quiet, "sent as handled, so it shows nowhere");
    f.hub.check_release().await.unwrap();
    assert_eq!(notices(&f).len(), 1);

    // Off, the notice goes; on again, the version shows in Versions but
    // isn't raised again.
    assert_eq!(set(&f, &Caller::local_owner(), false, 0).await.error, None);
    assert!(open(&f).is_empty());
    let off = view(&f, &phone).await;
    assert!(!off.enabled && off.newer.is_none());
    assert_eq!(set(&f, &Caller::local_owner(), true, 1).await.error, None);
    f.hub.check_release().await.unwrap();
    assert_eq!(view(&f, &phone).await.newer.unwrap().version, "0.3.0");
    assert_eq!(notices(&f).len(), 1);
    assert!(open(&f).is_empty());

    // A newer version still is told, once.
    source.says(Some("v0.4.0"));
    f.hub.check_release().await.unwrap();
    f.hub.check_release().await.unwrap();
    let [newest] = open(&f).try_into().unwrap();
    assert_eq!(newest.new_version.as_deref(), Some("0.4.0"));
    assert_eq!(notices(&f).len(), 2);

    // A check that gets no answer says nothing and changes nothing.
    source.fails();
    f.hub.check_release().await.unwrap();
    assert_eq!(open(&f), std::slice::from_ref(&newest));
    assert_eq!(view(&f, &phone).await.newer.unwrap().version, "0.4.0");
    f.close().await;
}

#[tokio::test]
async fn a_newer_version_replaces_the_notice_for_an_older_one() {
    let source = Arc::new(Releases::default());
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    source.says(Some("v0.3.0"));
    f.hub.check_release().await.unwrap();
    let [older] = open(&f).try_into().unwrap();
    source.says(Some("v0.4.0"));
    f.hub.check_release().await.unwrap();
    let [newer] = open(&f).try_into().unwrap();
    assert_ne!(newer.attention_id, older.attention_id);
    assert_eq!(newer.new_version.as_deref(), Some("0.4.0"));
    // The latest pulled back to one no newer than this computer's: no
    // notice stays up for it.
    source.says(Some("v0.2.0"));
    f.hub.check_release().await.unwrap();
    assert!(open(&f).is_empty());
    assert_eq!(view(&f, &Caller::local_owner()).await.newer, None);
    f.close().await;
}

#[tokio::test]
async fn the_notice_clears_once_the_computer_runs_that_version() {
    let source = Arc::new(Releases::default());
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    source.says(Some("v0.3.0"));
    f.hub.check_release().await.unwrap();
    assert_eq!(open(&f).len(), 1);
    // Restarted on an older build than the release: still newer, still up.
    let f = f.reopened(checking(&source, true, "0.2.1")).await;
    assert_eq!(open(&f).len(), 1);
    // Updated to it: the notice clears before any check, for good.
    let f = f.reopened(checking(&source, true, "0.3.0")).await;
    assert!(open(&f).is_empty());
    assert_eq!(view(&f, &Caller::local_owner()).await.newer, None);
    f.hub.check_release().await.unwrap();
    assert!(open(&f).is_empty());
    assert_eq!(notices(&f).len(), 1);
    f.close().await;
}

#[tokio::test]
async fn only_a_manager_changes_the_setting_and_only_at_the_current_revision() {
    let source = Arc::new(Releases::default());
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    let manager = f.device().await;
    let phone = pair(&f.hub, 2).await;
    assert!(!phone.grants.admin);
    // Every paired phone may read it.
    assert!(view(&f, &phone).await.enabled);
    let refused = set(&f, &phone, false, 0).await;
    assert_eq!(refused.error.unwrap().code, ErrorCode::PermissionDenied);
    assert!(view(&f, &manager).await.enabled);
    let stale = set(&f, &manager, false, 7).await;
    assert_eq!(stale.error.unwrap().code, ErrorCode::StaleRevision);
    let receipt = set(&f, &manager, false, 0).await;
    assert_eq!(receipt.error, None);
    let now = view(&f, &phone).await;
    assert!(!now.enabled);
    assert_eq!(now.revision, 1);
    // The choice holds over the build's default after a restart.
    let f = f.reopened(checking(&source, true, "0.2.0")).await;
    assert!(!view(&f, &Caller::local_owner()).await.enabled);
    f.close().await;
}

/// What the host record says of the check, as every phone sees it.
async fn host_view(f: &Fixture, caller: &Caller) -> Option<ReleaseCheckView> {
    let QueryData::Snapshot(snapshot) = f
        .hub
        .query(
            caller,
            Query::Snapshot {
                reads_withheld: None,
                account_items: false,
            },
        )
        .await
        .unwrap()
    else {
        panic!("snapshot")
    };
    snapshot.host.release_check
}
/// The kept setting and findings, as stored.
fn stored(f: &Fixture) -> serde_json::Value {
    (f.hub
        .read(|r| r.get::<serde_json::Value>("meta", "release_check")))
    .unwrap()
    .unwrap_or_default()
}

#[tokio::test]
async fn an_answer_that_names_no_stable_version_keeps_what_was_known() {
    let source = Arc::new(Releases::default());
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    source.says(Some("v0.4.0"));
    f.hub.check_release().await.unwrap();
    let [notice] = open(&f).try_into().unwrap();
    // No release (a 404), a tag that isn't a version, a pre-release not
    // marked as one, a failure: none of them says 0.4.0 went away.
    for answer in [None, Some("nightly"), Some("v0.5.0-rc.1"), Some("v0.5.0+")] {
        source.says(answer);
        f.hub.check_release().await.unwrap();
        assert_eq!(open(&f), std::slice::from_ref(&notice), "{answer:?}");
        let seen = view(&f, &Caller::local_owner()).await;
        assert_eq!(seen.newer.unwrap().version, "0.4.0", "{answer:?}");
    }
    source.fails();
    f.hub.check_release().await.unwrap();
    assert_eq!(open(&f), std::slice::from_ref(&notice));
    f.close().await;
}

#[tokio::test]
async fn a_personal_build_on_a_public_builds_data_clears_its_notice() {
    let source = Arc::new(Releases::default());
    source.says(Some("v0.3.0"));
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    f.hub.check_release().await.unwrap();
    assert_eq!(open(&f).len(), 1);
    // Opened by a personal build, which doesn't check unless asked to.
    let f = f.reopened(checking(&source, false, "0.2.0")).await;
    assert!(open(&f).is_empty());
    let seen = view(&f, &Caller::local_owner()).await;
    assert!(!seen.enabled && seen.newer.is_none());
    assert_eq!(host_view(&f, &Caller::local_owner()).await, Some(seen));
    f.close().await;
}

#[tokio::test]
async fn the_attempt_is_kept_before_asking_and_shown_as_checking() {
    let source = Arc::new(Releases::default());
    source.says(Some("v0.3.0"));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *source.gate.lock().unwrap() = Some(gate.clone());
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    let phone = f.device().await;
    let hub = f.hub.clone();
    let asking = tokio::spawn(async move { hub.check_release().await });
    let mut wait = waiting();
    while source.asked() == 0 {
        assert!(wait(), "it never asked");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A restart now would wait as after a check that got no answer.
    let kept = stored(&f);
    assert!(kept["tried_at"].is_number());
    assert!(kept["checked_at"].is_null());
    assert!(host_view(&f, &phone).await.unwrap().checking);
    gate.add_permits(1);
    asking.await.unwrap().unwrap();
    let kept = stored(&f);
    assert_eq!(kept["checked_at"], kept["tried_at"]);
    let shown = host_view(&f, &phone).await.unwrap();
    assert!(!shown.checking);
    assert_eq!(shown.newer.unwrap().version, "0.3.0");
    f.close().await;
}

#[tokio::test]
async fn every_change_shows_on_the_host_record_at_once() {
    let source = Arc::new(Releases::default());
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    let phone = f.device().await;
    let mut events = f
        .hub
        .subscribe(&phone, Some(f.hub.journal_head()))
        .await
        .unwrap();
    // A check that finds nothing new still says when it was checked.
    source.says(Some("v0.2.0"));
    f.hub.check_release().await.unwrap();
    let after = host_view(&f, &phone).await.unwrap();
    assert!(after.checked_at.is_some() && after.newer.is_none());
    // Another device turning it off.
    assert_eq!(set(&f, &Caller::local_owner(), false, 0).await.error, None);
    let published = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let crate::api::Outbound::Event(e) = events.live.recv().await.unwrap()
                && let Event::HostUpdated { host } = &e.event
                && host.release_check.as_ref().is_some_and(|v| !v.enabled)
            {
                return host.release_check.clone().unwrap();
            }
        }
    })
    .await
    .expect("the change was published");
    assert_eq!(published, view(&f, &phone).await);
    f.close().await;
}

#[tokio::test]
async fn only_turning_it_on_from_off_asks_at_once() {
    let source = Arc::new(Releases::default());
    source.says(Some("v0.2.0"));
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    f.hub.check_release().await.unwrap();
    assert_eq!(source.asked(), 1);
    // On already: saying so again, however often, asks nothing.
    for revision in 0..3 {
        assert_eq!(
            set(&f, &Caller::local_owner(), true, revision).await.error,
            None
        );
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(source.asked(), 1);
    // Off, then on: that asks.
    assert_eq!(set(&f, &Caller::local_owner(), false, 3).await.error, None);
    assert_eq!(set(&f, &Caller::local_owner(), true, 4).await.error, None);
    let mut wait = waiting();
    while source.asked() < 2 {
        assert!(wait(), "turning it back on didn't ask");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(source.asked(), 2);
    f.close().await;
}

/// A wall clock the schedule tests move, forwards as time passes and, to
/// play a clock put right, backwards. Deadlines follow only time passing.
#[derive(Debug)]
struct Wall {
    millis: std::sync::atomic::AtomicI64,
    passed: std::sync::atomic::AtomicU64,
    base: std::time::Instant,
}
impl Wall {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            millis: 1_700_000_000_000.into(),
            passed: 0.into(),
            base: std::time::Instant::now(),
        })
    }
    fn at(&self) -> i64 {
        self.millis.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn pass(&self, d: Duration) {
        use std::sync::atomic::Ordering::SeqCst;
        self.millis.fetch_add(d.as_millis() as i64, SeqCst);
        self.passed.fetch_add(d.as_millis() as u64, SeqCst);
    }
    fn set_by(&self, millis: i64) {
        self.millis
            .fetch_add(millis, std::sync::atomic::Ordering::SeqCst);
    }
}
impl ports::Clock for Wall {
    fn now(&self) -> protocol::time::Timestamp {
        protocol::time::Timestamp(self.at())
    }
    fn monotonic(&self) -> std::time::Instant {
        self.base + Duration::from_millis(self.passed.load(std::sync::atomic::Ordering::SeqCst))
    }
}

/// Answers as scripted, by the order of asking, noting when each was asked
/// by the wall clock: `true` answers, `false` fails as offline would.
#[derive(Debug)]
struct Script {
    wall: Arc<Wall>,
    answers: Vec<bool>,
    asked: Mutex<Vec<i64>>,
}
#[async_trait::async_trait]
impl ReleaseSource for Script {
    async fn latest(&self) -> Result<Option<LatestRelease>, String> {
        let mut asked = self.asked.lock().unwrap();
        asked.push(self.wall.at());
        match self.answers.get(asked.len() - 1).copied().unwrap_or(true) {
            true => Ok(Some(LatestRelease {
                tag: "v0.2.0".into(),
                url: "https://github.com/Ophio-app/ophio/releases/tag/v0.2.0".into(),
            })),
            false => Err("offline".into()),
        }
    }
}

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// Runs the real schedule against `script`, its naps passing time on the
/// wall clock at once, until `days` have passed; `each_nap` may move the
/// clock besides. Returns when each check asked, from the start.
async fn run_schedule(
    answers: Vec<bool>,
    days: i64,
    each_nap: impl Fn(&Wall, &[i64], &std::path::Path) + Send + Sync + 'static,
) -> Vec<i64> {
    let wall = Wall::new();
    let script = Arc::new(Script {
        wall: wall.clone(),
        answers,
        asked: Mutex::default(),
    });
    let check = crate::ReleaseCheck {
        source: script.clone(),
        on_by_default: true,
        installed: "0.2.0".into(),
    };
    let clock = wall.clone();
    let f = Fixture::with(move |b| b.clock(clock).release_check(check)).await;
    let start = wall.at();
    let end = start + days * DAY;
    let (done, finished) = tokio::sync::watch::channel(false);
    let database = Arc::new(f.path.join("companion.sqlite"));
    each_nap(&wall, &[], &database);
    let napping = (
        wall.clone(),
        script.clone(),
        Arc::new(each_nap),
        Arc::new(done),
        database,
    );
    let nap: crate::release_check::Nap = Arc::new(move |d| {
        let (wall, script, each_nap, done, database) = napping.clone();
        Box::pin(async move {
            wall.pass(d);
            let asked = script.asked.lock().unwrap().clone();
            each_nap(&wall, &asked, &database);
            if wall.at() >= end {
                let _ = done.send(true);
                std::future::pending::<()>().await;
            }
            tokio::task::yield_now().await;
        })
    });
    let schedule = tokio::spawn(crate::release_check::release_loop(
        Arc::downgrade(&f.hub.inner),
        f.hub.inner.release_wake.clone(),
        f.hub.inner.shutdown.subscribe(),
        nap,
    ));
    let mut finished = finished;
    tokio::time::timeout(Duration::from_secs(60), finished.wait_for(|d| *d))
        .await
        .expect("the schedule stopped moving")
        .unwrap();
    schedule.abort();
    let asked = script.asked.lock().unwrap().clone();
    f.close().await;
    asked.into_iter().map(|at| at - start).collect()
}

fn gaps(asked: &[i64]) -> Vec<i64> {
    asked.windows(2).map(|w| w[1] - w[0]).collect()
}

#[tokio::test]
async fn the_schedule_checks_once_a_day_for_days_on_end() {
    let asked = run_schedule(Vec::new(), 7, |_, _, _| {}).await;
    // First 2 to 20 minutes after the start, then a day apart, each up to
    // 20 minutes late.
    assert!((2 * MINUTE..=20 * MINUTE).contains(&asked[0]), "{asked:?}");
    assert!(
        asked.len() >= 7,
        "only {} checks in a week: {asked:?}",
        asked.len()
    );
    for gap in gaps(&asked) {
        assert!(
            (DAY..=DAY + 20 * MINUTE).contains(&gap),
            "{gap} in {asked:?}"
        );
    }
}

#[tokio::test]
async fn the_schedule_retries_six_hours_after_no_answer() {
    let asked = run_schedule(vec![false, false, true, false, true], 4, |_, _, _| {}).await;
    let gaps = gaps(&asked);
    let retry = 6 * HOUR..=6 * HOUR + 20 * MINUTE;
    let day = DAY..=DAY + 20 * MINUTE;
    assert!(
        retry.contains(&gaps[0]) && retry.contains(&gaps[1]),
        "{asked:?}"
    );
    assert!(day.contains(&gaps[2]), "{asked:?}");
    assert!(retry.contains(&gaps[3]), "{asked:?}");
    assert!(day.contains(&gaps[4]), "{asked:?}");
}

/// A clock three days fast asks at its time; put right, the next check
/// comes a day after that, not three days and a day later.
#[tokio::test]
async fn a_clock_put_right_doesnt_hold_the_schedule_back() {
    let moved = Arc::new(Mutex::new((false, false, 0)));
    let seen = moved.clone();
    let asked = run_schedule(Vec::new(), 4, move |wall, asked, _| {
        let mut moved = seen.lock().unwrap();
        // After the first check, the clock jumps three days ahead.
        if asked.len() == 1 && !moved.0 {
            moved.0 = true;
            wall.set_by(3 * DAY);
        }
        // Once a check asked at the fast time, it is put right.
        if asked.len() == 2 && !moved.1 {
            moved.1 = true;
            wall.set_by(-3 * DAY);
            moved.2 = wall.at();
        }
    })
    .await;
    let (_, put_right, at) = *moved.lock().unwrap();
    assert!(put_right, "{asked:?}");
    assert!(
        asked.len() >= 3,
        "no check after the clock was put right: {asked:?}"
    );
    // In the test's own time: the third check came about a day after the
    // clock was put right.
    // Asks are counted from the start, at 1_700_000_000_000 on the clock.
    let since = asked[2] - (at - 1_700_000_000_000);
    assert!(
        (DAY..=DAY + 20 * MINUTE + HOUR).contains(&since),
        "{since} in {asked:?}"
    );
}

#[tokio::test]
async fn turned_on_again_nothing_claims_to_be_newest_until_a_check_answers() {
    let source = Arc::new(Releases::default());
    source.says(Some("v0.2.0"));
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    f.hub.check_release().await.unwrap();
    assert!(view(&f, &Caller::local_owner()).await.checked_at.is_some());
    assert_eq!(set(&f, &Caller::local_owner(), false, 0).await.error, None);
    source.fails();
    assert_eq!(set(&f, &Caller::local_owner(), true, 1).await.error, None);
    f.hub.check_release().await.unwrap();
    // GitHub out of reach: what was answered before it was turned off says
    // nothing of now.
    let seen = view(&f, &Caller::local_owner()).await;
    assert!(seen.enabled && seen.checked_at.is_none() && seen.newer.is_none());
    f.close().await;
}

/// Asks and, while asking, makes the final save fail: the computer's name
/// is kept in a form it can't read.
#[derive(Debug, Default)]
struct Breaks(std::sync::OnceLock<std::sync::Weak<crate::hub::Inner>>);
#[async_trait::async_trait]
impl ReleaseSource for Breaks {
    async fn latest(&self) -> Result<Option<LatestRelease>, String> {
        // The hub owns this fake source, so the fake must not own the hub.
        let hub = CompanionHub {
            inner: self.0.get().unwrap().upgrade().unwrap(),
        };
        hub.write(|tx| tx.put("meta", "computer_name", &5)).unwrap();
        Ok(Some(LatestRelease {
            tag: "v0.3.0".into(),
            url: "https://github.com/Ophio-app/ophio/releases/tag/v0.3.0".into(),
        }))
    }
}

#[tokio::test]
async fn a_check_whose_result_cant_be_saved_still_stops_saying_checking() {
    let source = Arc::new(Breaks::default());
    let check = crate::ReleaseCheck {
        source: source.clone(),
        on_by_default: true,
        installed: "0.2.0".into(),
    };
    let f = Fixture::with(move |b| b.release_check(check)).await;
    source.0.set(Arc::downgrade(&f.hub.inner)).unwrap();
    assert!(f.hub.check_release().await.is_err());
    f.hub
        .write(|tx| tx.delete("meta", "computer_name"))
        .unwrap();
    let shown = host_view(&f, &Caller::local_owner()).await.unwrap();
    assert!(!shown.checking);
    // The attempt was kept, so the next waits as after no answer.
    assert!(stored(&f)["tried_at"].is_number());
    f.close().await;
}

#[tokio::test]
async fn an_unreadable_record_reads_as_none_and_the_computer_still_starts() {
    let source = Arc::new(Releases::default());
    let f = Fixture::with(checking(&source, true, "0.2.0")).await;
    // As a newer Øphio might have kept them, in forms this one can't read.
    f.hub
        .write(|tx| {
            tx.put(
                "meta",
                "release_check",
                &serde_json::json!({ "enabled": "maybe", "told": 3 }),
            )?;
            let mut host: serde_json::Value = tx.read().get("meta", "host")?.unwrap();
            host["release_check"] = serde_json::json!({ "enabled": [1, 2] });
            tx.put("meta", "host", &host)
        })
        .unwrap();
    let f = f.reopened(checking(&source, true, "0.2.0")).await;
    let seen = view(&f, &Caller::local_owner()).await;
    assert!(seen.enabled && seen.newer.is_none() && seen.checked_at.is_none());
    assert_eq!(host_view(&f, &Caller::local_owner()).await, Some(seen));
    f.close().await;
}

/// The disk is full when the first check would keep its attempt, before any
/// was ever kept; once it has room again, a check still comes, about six
/// hours on, and then daily.
#[tokio::test]
async fn a_first_check_that_cant_be_saved_is_tried_again() {
    let freed = Arc::new(Mutex::new(None::<i64>));
    let noted = freed.clone();
    let asked = run_schedule(Vec::new(), 3, move |wall, _, database| {
        let db = rusqlite::Connection::open(database).unwrap();
        let mut freed = noted.lock().unwrap();
        if wall.at() == 1_700_000_000_000 {
            // Before the schedule starts: nothing kept may be saved.
            for event in ["INSERT", "UPDATE"] {
                db.execute_batch(&format!(
                    "CREATE TRIGGER full_{event} BEFORE {event} ON documents \
                     WHEN NEW.kind = 'meta' AND NEW.id = 'release_check' \
                     BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;"
                ))
                .unwrap();
            }
        } else if freed.is_none() && wall.at() >= 1_700_000_000_000 + HOUR {
            // An hour on, after the first check failed to save: room again.
            db.execute_batch("DROP TRIGGER full_INSERT; DROP TRIGGER full_UPDATE;")
                .unwrap();
            *freed = Some(wall.at() - 1_700_000_000_000);
        }
    })
    .await;
    assert!(freed.lock().unwrap().is_some());
    assert!(
        !asked.is_empty(),
        "no check ever came after the failed save"
    );
    // The failed attempt came 2 to 20 minutes after the start, and asked
    // nothing; the next comes six hours after it, then daily.
    assert!(
        (6 * HOUR + 2 * MINUTE..=6 * HOUR + 21 * MINUTE).contains(&asked[0]),
        "{asked:?}"
    );
    for gap in gaps(&asked) {
        assert!(
            (DAY..=DAY + 20 * MINUTE).contains(&gap),
            "{gap} in {asked:?}"
        );
    }
}
