// SPDX-License-Identifier: Apache-2.0
//! Starting the agent browser and following it. Every tab, frame and worker
//! is held when it appears until its requests pass through Ophio, and each
//! request is let through or refused by whose tab it comes from.
use super::download::{deliver, safe_name};
use super::{AgentError, Shared, TaskBrowsing, blocked_by, on_whole_site, origin_host, plain_host};
use crate::pipe::Pipe;
use protocol::{TaskId, ids::TargetId};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

/// How long the browser stays open after its last task ends.
pub(super) const IDLE_CLOSE: Duration = Duration::from_secs(60);
/// How long a new browser waits for the last one's processes to exit.
pub(super) const CLOSING_WAIT: Duration = Duration::from_secs(30);
/// How long a new browser has to answer.
const START_WAIT: Duration = Duration::from_secs(10);
/// A tab nobody claims within this long, such as one the browser opened by
/// itself, is closed.
const UNCLAIMED_CLOSE: Duration = Duration::from_secs(5);
/// The largest download a task may receive.
const DOWNLOAD_LIMIT: f64 = 500. * 1024. * 1024.;
/// The size of every tab's page.
pub(super) const VIEWPORT: (u32, u32) = (1280, 800);
/// The origins the browser has sent requests to, one a line, in its profile.
const ORIGINS_FILE: &str = "ophio-origins";
/// The most origins remembered; the longest unseen are forgotten first.
const ORIGINS_KEPT: usize = 20_000;

pub(super) struct Running {
    pub(super) pipe: Arc<Pipe>,
    pub(super) state: Mutex<State>,
    /// Held while input, tab activation or capture goes to the browser, so
    /// another task can't put a capture's tab in the background. Also fences
    /// the owner's input from agents' input: see [`Running::fenced`].
    pub(super) input: tokio::sync::Mutex<()>,
    shared: Weak<Shared>,
    ended: AtomicBool,
}

#[derive(Default)]
pub(super) struct State {
    /// Tabs by the browser's target id.
    pub(super) tabs: HashMap<String, Tab>,
    sessions: HashMap<String, Session>,
    /// Tabs a task opened that the browser hasn't announced yet.
    claims: HashMap<String, TaskId>,
    /// The tabs the browser started with, which keep it open and load nothing.
    keepers: HashSet<String>,
    /// Each frame's tab, and the site of its page.
    frames: HashMap<String, Frame>,
    pub(super) current: HashMap<TaskId, String>,
    pub(super) origins: Origins,
    downloads: HashMap<String, Download>,
    /// Files each task received since it last looked.
    pub(super) arrived: HashMap<TaskId, Vec<String>>,
    /// Delivery failures each task has not seen yet, including recovery paths.
    pub(super) download_notices: HashMap<TaskId, Vec<String>>,
    next_tab: u64,
    /// Bumped whenever tabs come, go or change, for the desktop's list.
    pub(super) revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Owner {
    Task(TaskId),
    Keeper,
    Unclaimed,
}

/// Whether a tab's requests pass through Ophio yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Setup {
    Pending,
    Ready,
    Failed,
}

pub(super) struct Tab {
    pub(super) id: TargetId,
    /// The name agents use, such as "t2".
    pub(super) name: String,
    pub(super) owner: Owner,
    pub(super) session: String,
    pub(super) url: String,
    /// The host of the origin of the page the tab shows, as the browser
    /// reports it, which a page keeps when it moves to a `blob:` address or
    /// a tab it opens is written into: empty when it has none, as an emptied
    /// tab or a page the browser couldn't open has.
    pub(super) site: String,
    pub(super) title: String,
    pub(super) ready: watch::Sender<Setup>,
    /// The last site refused in this tab, for telling the agent.
    pub(super) refused: Option<String>,
    /// A download from the tab wasn't kept, for telling the agent.
    pub(super) dropped_download: bool,
    /// The owner is using this tab, so agents can't.
    pub(super) taken_over: bool,
    /// What each control reference from the last look points at.
    pub(super) refs: HashMap<String, super::page::Handle>,
    pub(super) order: u64,
    /// The page the tab shows, by its document's loader as the browser last
    /// announced it: empty before the first.
    pub(super) document: String,
    /// Pages in this tab that Ophio typed a password or code into.
    pub(super) typed: Vec<String>,
    /// Pages in this tab that were open while Ophio typed into a page in
    /// another of its task's tabs, and could have been passed what it typed.
    pub(super) beside: Vec<String>,
}

struct Frame {
    tab: String,
    /// The host of the origin of the frame's page, as for a tab's: empty
    /// before it shows one, or when it has none, as a page the browser
    /// couldn't open in it has.
    site: String,
}

/// How many pages of a tab are remembered as typed into, or beside one.
const MARKED: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Page,
    Frame,
    Worker,
    /// Shared and service workers, which belong to no one tab.
    Shared,
    /// The browser's own pages.
    Chrome,
}

#[derive(Debug, Clone)]
pub(super) struct Session {
    pub(super) kind: Kind,
    pub(super) target: String,
    pub(super) tab: Option<String>,
    pub(super) parent: Option<String>,
}

struct Download {
    task: TaskId,
    name: String,
}

/// What may happen to one request.
enum Verdict {
    Allow,
    /// Refused, naming the site when it was a blocked one.
    Refuse(Option<String>),
}

impl Running {
    pub(super) async fn launch(shared: &Arc<Shared>) -> Result<Arc<Self>, AgentError> {
        let unavailable = |_| AgentError::Failed("The agent browser could not start".into());
        prepare_profile(&shared.profile, &shared.staging)
            .await
            .map_err(unavailable)?;
        let mut extra: Vec<String> = vec![format!("--window-size={},{}", VIEWPORT.0, VIEWPORT.1)];
        extra.extend(shared.extra.iter().cloned());
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        let pipe = Pipe::launch(&shared.executable, &shared.profile, true, true, &extra)
            .await
            .map_err(|_| AgentError::Failed("The agent browser could not start".into()))?;
        let file = shared.profile.join(ORIGINS_FILE);
        let origins = tokio::task::spawn_blocking(move || Origins::load(file))
            .await
            .map_err(|_| AgentError::Failed("The agent browser could not start".into()))?;
        let running = Arc::new(Self {
            pipe: pipe.clone(),
            state: Mutex::new(State {
                origins,
                ..State::default()
            }),
            input: tokio::sync::Mutex::default(),
            shared: Arc::downgrade(shared),
            ended: AtomicBool::new(false),
        });
        // Nothing the browser says from here on is missed.
        tokio::spawn(follow(running.clone(), pipe.events()));
        if let Err(e) = running.start(&shared.staging).await {
            pipe.close().await;
            return Err(e);
        }
        Ok(running)
    }
    async fn start(&self, staging: &Path) -> Result<(), AgentError> {
        let failed = |_| AgentError::Failed("The agent browser did not start properly".into());
        let deadline = tokio::time::Instant::now() + START_WAIT;
        // The tab the browser opens itself. Some builds answer before it
        // exists, so look again until it does.
        let keepers = loop {
            let waited = tokio::time::Instant::now() >= deadline || self.ended();
            match self.pipe.call(None, "Target.getTargets", json!({})).await {
                Ok(targets) => {
                    let pages: HashSet<String> = targets["targetInfos"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|t| t["type"] == "page")
                        .filter_map(|t| t["targetId"].as_str().map(str::to_owned))
                        .collect();
                    if !pages.is_empty() || waited {
                        break pages;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(voice_control::Error::OutcomeUnknown) if !waited => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Err(failed(e)),
            }
        };
        if let Ok(mut state) = self.state.lock() {
            state.keepers = keepers;
        }
        self.pipe
            .call(
                None,
                "Browser.setDownloadBehavior",
                json!({"behavior":"allowAndName","downloadPath":staging,"eventsEnabled":true}),
            )
            .await
            .map_err(failed)?;
        self.pipe
            .call(None, "Target.setDiscoverTargets", json!({"discover":true}))
            .await
            .map_err(failed)?;
        // Everything that starts from now on waits for Ophio.
        self.pipe
            .call(
                None,
                "Target.setAutoAttach",
                json!({"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true}),
            )
            .await
            .map_err(failed)?;
        Ok(())
    }
    /// Whether the browser has gone, or is closing.
    pub(super) fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }
    /// Closes the browser and waits for it to exit. Nothing uses it from the
    /// moment it starts closing.
    pub(super) async fn close(&self) {
        self.ended.store(true, Ordering::Release);
        self.pipe.close().await;
    }
    fn shared(&self) -> Option<Arc<Shared>> {
        self.shared.upgrade()
    }
    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn handle(self: &Arc<Self>, event: Value) {
        let parent = event["sessionId"].as_str();
        let params = &event["params"];
        match event["method"].as_str().unwrap_or_default() {
            "Target.attachedToTarget" => self.attached(parent, params),
            "Target.detachedFromTarget" => self.detached(params),
            "Target.targetInfoChanged" => self.info_changed(&params["targetInfo"]),
            "Fetch.requestPaused" => self.paused(parent, params),
            "Page.frameAttached" => {
                if let (Some(frame), Some(session)) = (params["frameId"].as_str(), parent) {
                    let mut state = self.lock();
                    if let Some(tab) = state.sessions.get(session).and_then(|s| s.tab.clone()) {
                        state.frame(frame, tab);
                    }
                }
            }
            "Page.frameNavigated" => self.navigated(parent, &params["frame"]),
            "Page.frameDetached" if params["reason"] == "remove" => {
                if let Some(frame) = params["frameId"].as_str() {
                    self.lock().frames.remove(frame);
                }
            }
            "Browser.downloadWillBegin" => self.download_begins(params),
            "Browser.downloadProgress" => self.download_progress(params),
            _ => {}
        }
    }

    fn attached(self: &Arc<Self>, parent: Option<&str>, params: &Value) {
        let (Some(session), Some(target)) = (
            params["sessionId"].as_str(),
            params["targetInfo"]["targetId"].as_str(),
        ) else {
            return;
        };
        let info = &params["targetInfo"];
        let mut state = self.lock();
        let parent_tab = parent
            .and_then(|p| state.sessions.get(p))
            .and_then(|s| s.tab.clone());
        let kind = match info["type"].as_str().unwrap_or_default() {
            "page" if parent.is_none() => Kind::Page,
            "page" | "iframe" if parent_tab.is_some() => Kind::Frame,
            "worker" if parent_tab.is_some() => Kind::Worker,
            "shared_worker" | "service_worker" => Kind::Shared,
            _ => Kind::Chrome,
        };
        let mut tab = parent_tab;
        let mut owner = None;
        if kind == Kind::Page {
            let opener = info["openerId"]
                .as_str()
                .and_then(|o| state.tab_of(o))
                .and_then(|o| state.tabs.get(&o));
            // A tab a page opens starts with the origin of the page, or the
            // frame, that opened it, which can write into it before the
            // browser announces anything.
            let site = (info["openerFrameId"].as_str())
                .and_then(|f| state.frames.get(f))
                .map(|f| f.site.clone())
                .filter(|site| !site.is_empty())
                .or_else(|| opener.map(|o| o.site.clone()))
                .unwrap_or_default();
            let opener = opener.map(|o| o.owner);
            let claimed = state.claims.remove(target).map(Owner::Task);
            let decided = claimed
                .or(opener.filter(|o| matches!(o, Owner::Task(_))))
                .unwrap_or(if state.keepers.contains(target) {
                    Owner::Keeper
                } else {
                    Owner::Unclaimed
                });
            state.next_tab += 1;
            let order = state.next_tab;
            // A tab that opens while a typed-into page is open could be
            // handed what was typed before the browser announces its page.
            let beside = if state.typed_open(decided, None) {
                vec![String::new()]
            } else {
                Vec::new()
            };
            state.tabs.insert(
                target.into(),
                Tab {
                    id: TargetId::new(),
                    name: format!("t{order}"),
                    owner: decided,
                    session: session.into(),
                    url: info["url"].as_str().unwrap_or_default().into(),
                    title: info["title"].as_str().unwrap_or_default().into(),
                    ready: watch::channel(Setup::Pending).0,
                    refused: None,
                    dropped_download: false,
                    site,
                    taken_over: false,
                    refs: HashMap::new(),
                    order,
                    document: String::new(),
                    typed: Vec::new(),
                    beside,
                },
            );
            state.revision += 1;
            // A tab a page opens comes to the front, as it would for a person.
            if let Owner::Task(task) = decided {
                state.current.insert(task, target.into());
            }
            tab = Some(target.into());
            owner = Some(decided);
        }
        if kind == Kind::Frame
            && let Some(tab) = &tab
        {
            state.frame(target, tab.clone());
        }
        state.sessions.insert(
            session.into(),
            Session {
                kind,
                target: target.into(),
                tab,
                parent: parent.map(str::to_owned),
            },
        );
        drop(state);
        if owner == Some(Owner::Unclaimed) {
            let running = Arc::downgrade(self);
            let target = target.to_owned();
            tokio::spawn(async move {
                tokio::time::sleep(UNCLAIMED_CLOSE).await;
                if let Some(running) = running.upgrade()
                    && running
                        .lock()
                        .tabs
                        .get(&target)
                        .is_some_and(|t| t.owner == Owner::Unclaimed)
                {
                    let _ = running
                        .pipe
                        .call(None, "Target.closeTarget", json!({"targetId":target}))
                        .await;
                }
            });
        }
        let running = self.clone();
        let session = session.to_owned();
        let target = target.to_owned();
        tokio::spawn(async move {
            let ready = running.set_up(&session, kind, owner).await;
            if kind != Kind::Page {
                return;
            }
            if let Some(tab) = running.lock().tabs.get(&target) {
                tab.ready
                    .send_replace(if ready { Setup::Ready } else { Setup::Failed });
            }
            running.enforce(&target);
            // A tab whose requests can't pass through Ophio never loads.
            if !ready {
                let _ = running
                    .pipe
                    .call(None, "Target.closeTarget", json!({"targetId":target}))
                    .await;
            }
        });
    }

    /// Puts a new target's requests through Ophio, then lets it run. One that
    /// can't be is left waiting, so it loads nothing.
    async fn set_up(&self, session: &str, kind: Kind, owner: Option<Owner>) -> bool {
        let call = |method: &'static str, params: Value| {
            let pipe = self.pipe.clone();
            let session = session.to_owned();
            async move { pipe.call(Some(&session), method, params).await.is_ok() }
        };
        let intercept = || call("Fetch.enable", json!({"patterns":[{"urlPattern":"*"}]}));
        let attach = || {
            call(
                "Target.setAutoAttach",
                json!({"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true}),
            )
        };
        let resume = || call("Runtime.runIfWaitingForDebugger", json!({}));
        match kind {
            Kind::Page => {
                // Pages go to the network themselves rather than through a
                // service worker, whose own requests are checked separately.
                let ready = intercept().await
                    && call("Network.setBypassServiceWorker", json!({"bypass":true})).await
                    && attach().await
                    && resume().await;
                if ready {
                    // Every page is the same size, and behaves as if it had
                    // the keyboard though no window does.
                    call(
                        "Emulation.setDeviceMetricsOverride",
                        json!({"width":VIEWPORT.0,"height":VIEWPORT.1,"deviceScaleFactor":1,"mobile":false}),
                    )
                    .await;
                    call(
                        "Emulation.setFocusEmulationEnabled",
                        json!({"enabled":true}),
                    )
                    .await;
                }
                // The page's frames are followed only once it runs.
                ready
                    && call("Page.enable", json!({})).await
                    && (owner != Some(Owner::Keeper) || call("Network.enable", json!({})).await)
            }
            Kind::Frame => {
                intercept().await
                    && attach().await
                    && call("Page.enable", json!({})).await
                    && resume().await
            }
            // A dedicated worker's requests wait on its page's session.
            Kind::Worker => attach().await && resume().await,
            Kind::Shared | Kind::Chrome => intercept().await && resume().await,
        }
    }

    fn detached(&self, params: &Value) {
        let Some(session) = params["sessionId"].as_str() else {
            return;
        };
        let mut state = self.lock();
        let Some(gone) = state.sessions.remove(session) else {
            return;
        };
        match gone.kind {
            Kind::Page => {
                let Some(tab) = state.tabs.remove(&gone.target) else {
                    return;
                };
                state.revision += 1;
                state.frames.retain(|_, f| f.tab != gone.target);
                state
                    .sessions
                    .retain(|_, s| s.tab.as_deref() != Some(gone.target.as_str()));
                if let Owner::Task(task) = tab.owner
                    && state.current.get(&task) == Some(&gone.target)
                {
                    let next = state
                        .tabs
                        .iter()
                        .filter(|(_, t)| t.owner == Owner::Task(task))
                        .max_by_key(|(_, t)| t.order)
                        .map(|(id, _)| id.clone());
                    match next {
                        Some(next) => state.current.insert(task, next),
                        None => state.current.remove(&task),
                    };
                }
            }
            Kind::Frame => {
                state.frames.remove(&gone.target);
            }
            _ => {}
        }
    }

    /// A tab, or one of its frames, shows a new page.
    fn navigated(self: &Arc<Self>, session: Option<&str>, frame: &Value) {
        let (Some(session), Some(document)) = (session, frame["loaderId"].as_str()) else {
            return;
        };
        let mut state = self.lock();
        let Some(Session {
            kind, target, tab, ..
        }) = state.sessions.get(session)
        else {
            return;
        };
        let (kind, target, tab) = (*kind, target.clone(), tab.clone());
        if frame["parentId"].is_string() {
            let (Some(id), Some(tab)) = (frame["id"].as_str(), tab) else {
                return;
            };
            // A frame's site is its origin's alone: a page the browser
            // couldn't open in a frame has none.
            let site = frame["securityOrigin"].as_str().map(origin_host);
            state.frame(id, tab.clone()).site = site.unwrap_or_default();
            drop(state);
            self.enforce(&tab);
            return;
        }
        if kind != Kind::Page {
            return;
        }
        state.shown(&target, document);
        if let Some(tab) = state.tabs.get_mut(&target) {
            tab.site = frame["securityOrigin"]
                .as_str()
                .map(origin_host)
                .filter(|site| !site.is_empty())
                .unwrap_or_else(|| origin_host(frame["url"].as_str().unwrap_or_default()));
        }
        // The last page's frames are going with it.
        for frame in state.frames.values_mut().filter(|f| f.tab == target) {
            frame.site.clear();
        }
        drop(state);
        self.enforce(&target);
    }

    /// Empties a tab that shows a page, or holds a frame, its task may not
    /// use: the browser opens some pages, such as those at an address a page
    /// made, without a request Ophio could refuse.
    fn enforce(self: &Arc<Self>, target: &str) {
        let Some(site) = self.kept_from(target) else {
            return;
        };
        let running = self.clone();
        let target = target.to_owned();
        tokio::spawn(async move { running.empty(&target, &[site]).await });
    }

    pub(super) fn info_changed(&self, info: &Value) {
        let Some(target) = info["targetId"].as_str() else {
            return;
        };
        let mut state = self.lock();
        if let Some(tab) = state.tabs.get_mut(target) {
            let url = info["url"].as_str().unwrap_or_default();
            let title = info["title"].as_str().unwrap_or_default();
            if tab.url != url || tab.title != title {
                tab.url = url.into();
                tab.title = title.into();
                state.revision += 1;
            }
        }
    }

    fn paused(&self, session: Option<&str>, params: &Value) {
        let Some(request) = params["requestId"].as_str() else {
            return;
        };
        let url = params["request"]["url"].as_str().unwrap_or_default();
        let verdict = self.verdict(session, url);
        if matches!(verdict, Verdict::Allow)
            && let Ok(parsed) = url::Url::parse(url)
            && matches!(parsed.scheme(), "http" | "https")
        {
            self.lock()
                .origins
                .saw(&parsed.origin().ascii_serialization());
        }
        if let Verdict::Refuse(Some(site)) = &verdict
            && params["resourceType"] == "Document"
            && let Some(session) = session
        {
            let mut state = self.lock();
            if let Some(tab) = state.sessions.get(session).and_then(|s| s.tab.clone())
                && let Some(tab) = state.tabs.get_mut(&tab)
            {
                tab.refused = Some(site.clone());
            }
        }
        let pipe = self.pipe.clone();
        let session = session.map(str::to_owned);
        let request = request.to_owned();
        tokio::spawn(async move {
            let (method, params) = match verdict {
                Verdict::Allow => ("Fetch.continueRequest", json!({"requestId":request})),
                Verdict::Refuse(_) => (
                    "Fetch.failRequest",
                    json!({"requestId":request,"errorReason":"BlockedByClient"}),
                ),
            };
            let _ = pipe.call(session.as_deref(), method, params).await;
        });
    }

    /// Whether a request for `url` from `session` may go out.
    fn verdict(&self, session: Option<&str>, url: &str) -> Verdict {
        let Ok(parsed) = url::Url::parse(url) else {
            return Verdict::Refuse(None);
        };
        match parsed.scheme() {
            "data" | "blob" => return Verdict::Allow,
            "http" | "https" => {}
            _ => return Verdict::Refuse(None),
        }
        let Some(host) = parsed.host_str().map(plain_host) else {
            return Verdict::Refuse(None);
        };
        let who = {
            let state = self.lock();
            let session = session.and_then(|s| state.sessions.get(s));
            match session {
                Some(s) if s.kind == Kind::Shared => None,
                Some(s) if s.kind == Kind::Chrome => return Verdict::Refuse(None),
                Some(s) => match s.tab.as_ref().and_then(|t| state.tabs.get(t)) {
                    Some(Tab {
                        owner: Owner::Task(task),
                        ..
                    }) => Some(*task),
                    _ => return Verdict::Refuse(None),
                },
                None => return Verdict::Refuse(None),
            }
        };
        let Some(shared) = self.shared() else {
            return Verdict::Refuse(None);
        };
        let browsing: Vec<TaskBrowsing> = match who {
            Some(task) => match shared.browsing(task) {
                Some(browsing) => vec![browsing],
                None => return Verdict::Refuse(None),
            },
            // A worker serves every tab of its site, so it may reach a site
            // only when no task is kept from it.
            None => {
                let all = shared.all_browsing();
                if all.is_empty() {
                    return Verdict::Refuse(None);
                }
                all
            }
        };
        for browsing in browsing {
            if let Some(site) = blocked_by(&host, &(browsing.blocked)()) {
                return Verdict::Refuse(Some(site));
            }
        }
        Verdict::Allow
    }

    /// The site of the page `target` shows, when its task may no longer
    /// open it: a login was paused or changed after the page loaded.
    pub(super) fn kept_from(&self, target: &str) -> Option<String> {
        let (owner, sites) = {
            let state = self.lock();
            (state.tabs.get(target)?.owner, state.sites_of(target))
        };
        let Owner::Task(task) = owner else {
            return None;
        };
        if sites.is_empty() {
            return None;
        }
        let blocked = (self.shared()?.browsing(task)?.blocked)();
        (sites.iter()).find_map(|site| blocked_by(site, &blocked))
    }

    fn download_begins(&self, params: &Value) {
        let (Some(guid), Some(frame)) = (params["guid"].as_str(), params["frameId"].as_str())
        else {
            return;
        };
        let mut state = self.lock();
        let tab = state.tab_of(frame);
        let found = tab
            .as_ref()
            .and_then(|t| state.tabs.get(t))
            .map(|t| (t.owner, t.document.clone()));
        if let (Some(tab), Some((Owner::Task(task), document))) = (tab, found) {
            // A page that may hold what Ophio typed could write it into a
            // file, which the agent would read unmasked.
            if state.withheld(&tab, &document).is_some() {
                if let Some(tab) = state.tabs.get_mut(&tab) {
                    tab.dropped_download = true;
                }
            } else {
                let name = params["suggestedFilename"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                state.downloads.insert(guid.into(), Download { task, name });
                return;
            }
        }
        drop(state);
        self.cancel_download(guid);
    }

    fn download_progress(self: &Arc<Self>, params: &Value) {
        let Some(guid) = params["guid"].as_str() else {
            return;
        };
        let staged = self.shared().map(|s| s.staging.join(guid));
        match params["state"].as_str() {
            Some("inProgress") => {
                let size = params["receivedBytes"]
                    .as_f64()
                    .unwrap_or(0.)
                    .max(params["totalBytes"].as_f64().unwrap_or(0.));
                if size > DOWNLOAD_LIMIT {
                    self.lock().downloads.remove(guid);
                    self.cancel_download(guid);
                }
            }
            Some("completed") => {
                let download = self.lock().downloads.remove(guid);
                let shared = self.shared();
                let Some(staged) = staged else { return };
                let destination = download.as_ref().and_then(|d| {
                    shared
                        .as_ref()
                        .and_then(|s| s.browsing(d.task))
                        .map(|b| (d.task, d.name.clone(), b.downloads))
                });
                let running = Arc::downgrade(self);
                tokio::spawn(async move {
                    match destination {
                        Some((task, name, folder)) => {
                            let result = deliver(&staged, &folder, &name).await;
                            if let Some(running) = running.upgrade() {
                                let mut state = running.lock();
                                match result {
                                    Ok(name) => state.arrived.entry(task).or_default().push(name),
                                    Err(error) => {
                                        state.download_notices.entry(task).or_default().push(
                                            format!(
                                                "Download {:?} could not be saved to {}: {error}. \
                                             The complete file is kept at {}. After fixing the \
                                             destination, copy or move that file there",
                                                safe_name(&name),
                                                folder.display(),
                                                staged.display()
                                            ),
                                        )
                                    }
                                }
                            }
                        }
                        None => {
                            let _ = tokio::fs::remove_file(&staged).await;
                        }
                    }
                });
            }
            Some("canceled") => {
                self.lock().downloads.remove(guid);
                if let Some(staged) = staged {
                    tokio::spawn(async move {
                        let _ = tokio::fs::remove_file(&staged).await;
                    });
                }
            }
            _ => {}
        }
    }

    fn cancel_download(&self, guid: &str) {
        let pipe = self.pipe.clone();
        let guid = guid.to_owned();
        tokio::spawn(async move {
            let _ = pipe
                .call(None, "Browser.cancelDownload", json!({"guid":guid}))
                .await;
        });
    }

    /// Closes every tab `task` has.
    pub(super) async fn close_tabs_of(&self, task: TaskId) {
        let _input = self.input.lock().await;
        let tabs: Vec<String> = {
            let mut state = self.lock();
            state.current.remove(&task);
            state.arrived.remove(&task);
            state.download_notices.remove(&task);
            state
                .tabs
                .iter()
                .filter(|(_, t)| t.owner == Owner::Task(task))
                .map(|(id, _)| id.clone())
                .collect()
        };
        for tab in tabs {
            let _ = self
                .pipe
                .call(None, "Target.closeTarget", json!({"targetId":tab}))
                .await;
        }
    }

    /// The page session of a tab of the browser's own, kept for operations
    /// that need one, such as deleting cookies. A browser started just now
    /// may not have it ready yet.
    pub(super) async fn keeper_session(&self) -> Option<String> {
        let deadline = tokio::time::Instant::now() + START_WAIT;
        loop {
            let keeper = self
                .lock()
                .tabs
                .values()
                .find(|t| t.owner == Owner::Keeper && *t.ready.borrow() == Setup::Ready)
                .map(|t| t.session.clone());
            if keeper.is_some() || tokio::time::Instant::now() >= deadline || self.ended() {
                return keeper;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Claims a tab `task` just created, wherever its announcement is.
    pub(super) fn claim(&self, target: &str, task: TaskId) {
        let mut state = self.lock();
        match state.tabs.get_mut(target) {
            Some(tab) => {
                if tab.owner == Owner::Unclaimed {
                    tab.owner = Owner::Task(task);
                }
            }
            None => {
                state.claims.insert(target.into(), task);
            }
        }
        state.current.insert(task, target.into());
        state.revision += 1;
    }

    /// Waits until a tab's requests pass through Ophio.
    pub(super) async fn ready(&self, target: &str) -> Result<(), AgentError> {
        let deadline = tokio::time::Instant::now() + START_WAIT;
        loop {
            let receiver = self.lock().tabs.get(target).map(|t| t.ready.subscribe());
            match receiver {
                Some(mut receiver) => {
                    let outcome = tokio::time::timeout_at(
                        deadline,
                        receiver.wait_for(|s| *s != Setup::Pending),
                    )
                    .await;
                    return match outcome {
                        Ok(Ok(setup)) if *setup == Setup::Ready => Ok(()),
                        _ => Err(AgentError::Failed("The new tab could not be opened".into())),
                    };
                }
                // The browser hasn't announced it yet.
                None if tokio::time::Instant::now() < deadline && !self.ended() => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                None => return Err(AgentError::Failed("The new tab could not be opened".into())),
            }
        }
    }

    /// Every session of a tab: its page's first, then its frames', each after
    /// its parent.
    pub(super) fn sessions_of(&self, target: &str) -> Vec<(String, Session)> {
        let state = self.lock();
        let mut out: Vec<(String, Session)> = Vec::new();
        let mut rest: Vec<(String, Session)> = state
            .sessions
            .iter()
            .filter(|(_, s)| {
                s.tab.as_deref() == Some(target) && matches!(s.kind, Kind::Page | Kind::Frame)
            })
            .map(|(id, s)| (id.clone(), s.clone()))
            .collect();
        loop {
            let before = rest.len();
            rest.retain(|(id, s)| {
                let placed = s.kind == Kind::Page
                    || s.parent
                        .as_ref()
                        .is_some_and(|p| out.iter().any(|(o, _)| o == p));
                if placed {
                    out.push((id.clone(), s.clone()));
                }
                !placed
            });
            if rest.is_empty() || rest.len() == before {
                return out;
            }
        }
    }
}

impl State {
    /// The tab a target or frame belongs to.
    fn tab_of(&self, id: &str) -> Option<String> {
        if self.tabs.contains_key(id) {
            return Some(id.into());
        }
        self.frames.get(id).map(|f| f.tab.clone())
    }

    /// A frame of `tab`'s, which keeps the site it has.
    fn frame(&mut self, id: &str, tab: String) -> &mut Frame {
        (self.frames.entry(id.into())).or_insert_with(|| Frame {
            tab,
            site: String::new(),
        })
    }

    /// The sites of the pages a tab and its frames show.
    pub(super) fn sites_of(&self, target: &str) -> Vec<String> {
        let tab = self.tabs.get(target).map(|t| &t.site);
        let frames = (self.frames.values()).filter(|f| f.tab == target);
        (tab.into_iter().chain(frames.map(|f| &f.site)))
            .filter(|site| !site.is_empty())
            .cloned()
            .collect()
    }

    /// Whether one of `owner`'s tabs, other than `except`, shows a page Ophio
    /// typed into.
    pub(super) fn typed_open(&self, owner: Owner, except: Option<&str>) -> bool {
        matches!(owner, Owner::Task(_))
            && self.tabs.iter().any(|(target, tab)| {
                tab.owner == owner
                    && Some(target.as_str()) != except
                    && tab.typed.contains(&tab.document)
            })
    }

    /// The tab now shows `document`, which one of its task's typed-into
    /// pages, open in another tab, could pass what was typed to.
    fn shown(&mut self, target: &str, document: &str) {
        let Some(owner) = self.tabs.get(target).map(|t| t.owner) else {
            return;
        };
        let beside = self.typed_open(owner, Some(target));
        let tab = self.tabs.get_mut(target).expect("found above");
        tab.document = document.to_owned();
        if beside {
            remember(&mut tab.beside, document);
        }
    }

    /// Ophio is typing into `document`, the page the tab shows: what the
    /// task's other tabs show could be passed what it types.
    pub(super) fn typing(&mut self, target: &str, document: &str) {
        let Some(tab) = self.tabs.get_mut(target) else {
            return;
        };
        let owner = tab.owner;
        remember(&mut tab.typed, document);
        for (other, tab) in &mut self.tabs {
            if tab.owner == owner && other != target {
                let shown = tab.document.clone();
                remember(&mut tab.beside, &shown);
            }
        }
    }

    /// Why a picture of `live`, the page the tab shows now, isn't shown, if
    /// it isn't: it may show what Ophio typed.
    pub(super) fn withheld(&self, target: &str, live: &str) -> Option<Withheld> {
        let Some(tab) = self.tabs.get(target) else {
            return Some(Withheld::Typed);
        };
        if tab.typed.iter().any(|d| d == live) {
            return Some(Withheld::Typed);
        }
        if tab.beside.iter().any(|d| d == live) {
            return Some(Withheld::Beside);
        }
        if tab.document == live {
            return None;
        }
        // The browser hasn't announced this page yet: decide as it will be
        // when it does, or as the page the tab opened with.
        (tab.document.is_empty() && tab.beside.iter().any(String::is_empty)
            || self.typed_open(tab.owner, Some(target)))
        .then_some(Withheld::Beside)
    }
}

/// Why a picture of a page isn't given to an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Withheld {
    /// Ophio typed a password or code into it.
    Typed,
    /// It was open while Ophio typed into another of the task's pages.
    Beside,
    /// It changed while the picture was taken.
    Changed,
}

/// Adds a page to a tab's marked ones, forgetting the oldest beyond
/// [`MARKED`].
fn remember(marked: &mut Vec<String>, document: &str) {
    if marked.iter().any(|d| d == document) {
        return;
    }
    if marked.len() == MARKED {
        marked.remove(0);
    }
    marked.push(document.to_owned());
}

async fn follow(running: Arc<Running>, mut events: mpsc::UnboundedReceiver<Value>) {
    while let Some(event) = events.recv().await {
        running.handle(event);
    }
    running.ended.store(true, Ordering::Release);
}

/// Makes the profile private to the owner's user and sets what Ophio needs
/// before the browser reads it: no third-party cookies, so a page can't send
/// a request carrying another site's session, and no saving or filling of
/// passwords or addresses by the browser itself.
/// Every http and https origin the browser has sent a request to, kept in
/// its profile across starts: anything a page stored, it stored under one of
/// them, so signing out of a site can clear them all.
#[derive(Default)]
pub(super) struct Origins {
    file: Option<PathBuf>,
    seen: HashSet<String>,
    /// Oldest first.
    order: VecDeque<String>,
}

impl Origins {
    /// The origins kept in `file`, which is rewritten without repeats.
    fn load(file: PathBuf) -> Self {
        let mut origins = Self::default();
        for line in std::fs::read_to_string(&file).unwrap_or_default().lines() {
            origins.remember(line.trim());
        }
        let kept: String = origins.order.iter().map(|o| format!("{o}\n")).collect();
        let _ = std::fs::write(&file, kept);
        origins.file = Some(file);
        origins
    }

    fn saw(&mut self, origin: &str) {
        if self.seen.contains(origin) {
            return;
        }
        self.remember(origin);
        if let Some(file) = &self.file
            && let Ok(mut out) = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(file)
        {
            let _ = writeln!(out, "{origin}");
        }
    }

    fn remember(&mut self, origin: &str) {
        if origin.is_empty() || !self.seen.insert(origin.to_owned()) {
            return;
        }
        self.order.push_back(origin.to_owned());
        while self.order.len() > ORIGINS_KEPT {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
    }

    /// The origins whose host is on the whole site of one of `sites`.
    pub(super) fn on(&self, sites: &[String]) -> Vec<String> {
        (self.order.iter())
            .filter(|origin| {
                url::Url::parse(origin)
                    .ok()
                    .and_then(|u| u.host_str().map(plain_host))
                    .is_some_and(|host| sites.iter().any(|site| on_whole_site(&host, site)))
            })
            .cloned()
            .collect()
    }
}

async fn prepare_profile(profile: &Path, staging: &Path) -> std::io::Result<()> {
    let profile = profile.to_owned();
    let staging = staging.to_owned();
    tokio::task::spawn_blocking(move || {
        private_directory(&profile)?;
        private_directory(&staging)?;
        // Chromium adds .crdownload while writing. Completed downloads may
        // be waiting for recovery after a failed delivery; keep their bytes.
        for entry in std::fs::read_dir(&staging)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "crdownload") {
                std::fs::remove_file(path)?;
            }
        }
        let default = profile.join("Default");
        private_directory(&default)?;
        // The last browser's tabs. Some builds reopen them at start whatever
        // the preferences say, before Ophio governs what they load, and a page
        // from before a sign-out could then sign in again.
        match std::fs::remove_dir_all(default.join("Sessions")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let path = default.join("Preferences");
        let mut preferences: Value = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        for (path, value) in [
            ("profile.cookie_controls_mode", json!(1)),
            ("profile.block_third_party_cookies", json!(true)),
            ("credentials_enable_service", json!(false)),
            ("credentials_enable_autosignin", json!(false)),
            ("profile.password_manager_enabled", json!(false)),
            ("autofill.profile_enabled", json!(false)),
            ("autofill.credit_card_enabled", json!(false)),
            ("download.prompt_for_download", json!(false)),
        ] {
            set(&mut preferences, path, value);
        }
        std::fs::write(&path, serde_json::to_vec(&preferences)?)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn set(root: &mut Value, path: &str, value: Value) {
    let mut node = root;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        if !node.is_object() {
            *node = json!({});
        }
        let Some(object) = node.as_object_mut() else {
            return;
        };
        if parts.peek().is_none() {
            object.insert(part.into(), value);
            return;
        }
        node = object.entry(part).or_insert_with(|| json!({}));
    }
}

fn private_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restarting_keeps_completed_downloads_and_removes_only_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("downloading");
        let profile = dir.path().join("profile");
        private_directory(&staging).unwrap();
        std::fs::write(staging.join("completed-guid"), b"complete").unwrap();
        std::fs::write(staging.join("partial-guid.crdownload"), b"partial").unwrap();
        prepare_profile(&profile, &staging).await.unwrap();
        assert_eq!(
            std::fs::read(staging.join("completed-guid")).unwrap(),
            b"complete"
        );
        assert!(!staging.join("partial-guid.crdownload").exists());
    }
}
