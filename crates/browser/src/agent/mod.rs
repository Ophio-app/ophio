// SPDX-License-Identifier: Apache-2.0
//! Ophio's agent browser: one headless browser with a persistent profile,
//! shared by tasks, each in its own tabs. Agents reach it only through these
//! operations. There is no way to run a script in a page or to read a field's
//! value, and a password goes in only through [`AgentBrowser::fill_login`],
//! which checks the site and the field itself.
mod desktop;
mod download;
mod page;
mod paint;
mod run;
#[cfg(all(test, target_os = "linux"))]
mod tests;

pub use desktop::WithAgentBrowser;
pub use ports::agent_browser::{AgentError, Control, Observation, PageInfo, TabInfo, TaskBrowsing};
use ports::agent_logins::whole_site;
use protocol::TaskId;
use run::Running;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;

/// One task's place in the browser.
struct TaskEntry {
    browsing: TaskBrowsing,
    /// A task's operations run one at a time.
    serial: AsyncMutex<()>,
}

struct Shared {
    executable: PathBuf,
    profile: PathBuf,
    staging: PathBuf,
    extra: Vec<String>,
    tasks: Mutex<HashMap<TaskId, Arc<TaskEntry>>>,
    launching: AsyncMutex<()>,
    running: Mutex<Option<Arc<Running>>>,
}

#[derive(Clone)]
pub struct AgentBrowser {
    shared: Arc<Shared>,
}
impl std::fmt::Debug for AgentBrowser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AgentBrowser")
    }
}

/// The browser's folder in Ophio's data folder.
pub fn directory(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-browser")
}

impl AgentBrowser {
    /// The browser starts when a task first needs it.
    pub fn new(executable: PathBuf, data_dir: &Path) -> Self {
        Self::with_arguments(executable, data_dir, Vec::new())
    }
    fn with_arguments(executable: PathBuf, data_dir: &Path, extra: Vec<String>) -> Self {
        let directory = directory(data_dir);
        Self {
            shared: Arc::new(Shared {
                executable,
                profile: directory.join("profile"),
                staging: directory.join("downloading"),
                extra,
                tasks: Mutex::default(),
                launching: AsyncMutex::new(()),
                running: Mutex::default(),
            }),
        }
    }
    /// Lets `task` browse, within `browsing`.
    pub fn register(&self, task: TaskId, browsing: TaskBrowsing) {
        if let Ok(mut tasks) = self.shared.tasks.lock() {
            tasks.insert(
                task,
                Arc::new(TaskEntry {
                    browsing,
                    serial: AsyncMutex::new(()),
                }),
            );
        }
    }
    /// The task has ended: its tabs close, and the browser does too a minute
    /// after the last task.
    pub async fn close_task(&self, task: TaskId) {
        let remaining = match self.shared.tasks.lock() {
            Ok(mut tasks) => {
                tasks.remove(&task);
                tasks.len()
            }
            Err(_) => return,
        };
        if let Some(running) = self.shared.current() {
            running.close_tabs_of(task).await;
        }
        if remaining == 0 {
            let shared = Arc::downgrade(&self.shared);
            tokio::spawn(async move {
                tokio::time::sleep(run::IDLE_CLOSE).await;
                if let Some(shared) = shared.upgrade() {
                    shared.close_if_idle().await;
                }
            });
        }
    }
    /// Closes the browser now, whatever is using it.
    pub async fn shut_down(&self) {
        let _launching = self.shared.launching.lock().await;
        let running = self.shared.running.lock().ok().and_then(|mut r| r.take());
        if let Some(running) = running {
            running.pipe.close().await;
        }
    }
    fn task(&self, task: TaskId) -> Result<Arc<TaskEntry>, AgentError> {
        self.shared
            .tasks
            .lock()
            .ok()
            .and_then(|tasks| tasks.get(&task).cloned())
            .ok_or_else(|| AgentError::Refused("This task has no browser".into()))
    }
}

impl Shared {
    fn current(&self) -> Option<Arc<Running>> {
        self.running
            .lock()
            .ok()
            .and_then(|r| r.clone())
            .filter(|r| !r.ended())
    }
    /// The running browser, started if it isn't.
    async fn running(self: &Arc<Self>) -> Result<Arc<Running>, AgentError> {
        let _launching = self.launching.lock().await;
        let previous = self.running.lock().ok().and_then(|r| r.clone());
        if let Some(previous) = &previous
            && !previous.ended()
        {
            return Ok(previous.clone());
        }
        let running = self.launch_after(previous.as_deref()).await?;
        if let Ok(mut slot) = self.running.lock() {
            *slot = Some(running.clone());
        }
        Ok(running)
    }
    /// Starts a browser once `previous`, which has closed, has wholly exited:
    /// a new one can't use the profile while the last one's processes still
    /// hold it, and nothing they do can follow what the new one does.
    async fn launch_after(
        self: &Arc<Self>,
        previous: Option<&Running>,
    ) -> Result<Arc<Running>, AgentError> {
        if let Some(previous) = previous {
            let deadline = tokio::time::Instant::now() + run::CLOSING_WAIT;
            while !previous.pipe.is_closed() {
                if tokio::time::Instant::now() >= deadline {
                    return Err(AgentError::Failed(
                        "The agent browser is still closing; try again shortly".into(),
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
        Running::launch(self).await
    }
    /// Signs the browser out of `sites`. A running browser closes first:
    /// only that stops every request it has already sent, any of which could
    /// still answer with a session after the clearing, and every page, at
    /// whatever address, that could store one again. Every tab closes with
    /// it. A new browser is cleared before anything else can use it.
    async fn signed_out(self: &Arc<Self>, sites: &[String]) -> Result<(), AgentError> {
        let _launching = self.launching.lock().await;
        let previous = self.running.lock().ok().and_then(|r| r.clone());
        if let Some(previous) = &previous {
            previous.close().await;
        }
        let running = self.launch_after(previous.as_deref()).await?;
        let outcome = running.sign_out(sites).await;
        if outcome.is_err() {
            running.close().await;
        }
        // Even closed, it is the one the next start waits for.
        if let Ok(mut slot) = self.running.lock() {
            *slot = Some(running);
        }
        outcome
    }
    async fn close_if_idle(&self) {
        let _launching = self.launching.lock().await;
        if self.tasks.lock().map(|t| !t.is_empty()).unwrap_or(true) {
            return;
        }
        let running = self.running.lock().ok().and_then(|mut r| r.take());
        if let Some(running) = running {
            running.pipe.close().await;
        }
    }
    fn browsing(&self, task: TaskId) -> Option<TaskBrowsing> {
        self.tasks
            .lock()
            .ok()?
            .get(&task)
            .map(|entry| entry.browsing.clone())
    }
    fn all_browsing(&self) -> Vec<TaskBrowsing> {
        self.tasks
            .lock()
            .map(|tasks| tasks.values().map(|e| e.browsing.clone()).collect())
            .unwrap_or_default()
    }
}

/// The host of the origin of `address`, as [`plain_host`] keeps it: for a
/// `blob:` address, the origin of the page that made it. Empty for an origin
/// with no host, such as an emptied tab's.
fn origin_host(address: &str) -> String {
    match url::Url::parse(address).map(|u| u.origin()) {
        Ok(url::Origin::Tuple(_, host, _)) => plain_host(&host.to_string()),
        _ => String::new(),
    }
}

/// `host`, lowercased and without a trailing dot, as sites are kept.
fn plain_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Whether `host` is `site` or one of its subdomains.
fn on_site(host: &str, site: &str) -> bool {
    let site = plain_host(site);
    let site = site.strip_prefix("www.").unwrap_or(&site);
    !site.is_empty()
        && (host == site
            || host
                .strip_suffix(site)
                .is_some_and(|rest| rest.ends_with('.')))
}

/// Whether `host` is on the whole site `site` is part of.
fn on_whole_site(host: &str, site: &str) -> bool {
    on_site(host, whole_site(&plain_host(site)))
}

/// `host`, when it is on the whole site of a blocked site. A page on one of
/// a site's subdomains can open a WebSocket to another, which neither `Fetch`
/// nor the browser's URL blocking sees, carrying that site's cookies; so a
/// task kept from a login's site is kept from all of its site.
fn blocked_by(host: &str, blocked: &[String]) -> Option<String> {
    let host = plain_host(host);
    blocked
        .iter()
        .find(|site| on_whole_site(&host, site))
        .map(|_| host)
}

#[async_trait::async_trait]
impl ports::agent_browser::AgentBrowsing for AgentBrowser {
    fn register(&self, task: TaskId, browsing: TaskBrowsing) {
        AgentBrowser::register(self, task, browsing);
    }
    async fn close_task(&self, task: TaskId) {
        AgentBrowser::close_task(self, task).await;
    }
    async fn open(&self, task: TaskId, url: &str, new_tab: bool) -> Result<PageInfo, AgentError> {
        AgentBrowser::open(self, task, url, new_tab).await
    }
    async fn tabs(&self, task: TaskId) -> Result<Vec<TabInfo>, AgentError> {
        AgentBrowser::tabs(self, task).await
    }
    async fn switch_tab(&self, task: TaskId, tab: &str) -> Result<PageInfo, AgentError> {
        AgentBrowser::switch_tab(self, task, tab).await
    }
    async fn close_tab(&self, task: TaskId, tab: Option<&str>) -> Result<(), AgentError> {
        AgentBrowser::close_tab(self, task, tab).await
    }
    async fn back(&self, task: TaskId) -> Result<PageInfo, AgentError> {
        AgentBrowser::back(self, task).await
    }
    async fn page(&self, task: TaskId) -> Result<PageInfo, AgentError> {
        AgentBrowser::page(self, task).await
    }
    async fn observe(&self, task: TaskId) -> Result<Observation, AgentError> {
        AgentBrowser::observe(self, task).await
    }
    async fn click(&self, task: TaskId, reference: &str) -> Result<PageInfo, AgentError> {
        AgentBrowser::click(self, task, reference).await
    }
    async fn type_text(
        &self,
        task: TaskId,
        reference: &str,
        text: &str,
        replace: bool,
    ) -> Result<(), AgentError> {
        AgentBrowser::type_text(self, task, reference, text, replace).await
    }
    async fn press(&self, task: TaskId, key: &str) -> Result<PageInfo, AgentError> {
        AgentBrowser::press(self, task, key).await
    }
    async fn scroll(
        &self,
        task: TaskId,
        dy: f64,
        reference: Option<&str>,
    ) -> Result<(), AgentError> {
        AgentBrowser::scroll(self, task, dy, reference).await
    }
    async fn fill_login(
        &self,
        task: TaskId,
        sites: &[String],
        username: &str,
        password: &str,
        submit: bool,
    ) -> Result<PageInfo, AgentError> {
        AgentBrowser::fill_login(self, task, sites, username, password, submit).await
    }
    async fn mark(&self, task: TaskId) -> Result<ports::agent_browser::PageMark, AgentError> {
        AgentBrowser::mark(self, task).await
    }
    async fn fill_code(
        &self,
        task: TaskId,
        sites: &[String],
        code: &str,
        submit: bool,
        at: Option<&ports::agent_browser::PageMark>,
    ) -> Result<PageInfo, AgentError> {
        AgentBrowser::fill_code(self, task, sites, code, submit, at).await
    }
    async fn sign_out(&self, sites: &[String]) -> Result<(), AgentError> {
        AgentBrowser::sign_out(self, sites).await
    }
    async fn recheck_access(&self) {
        AgentBrowser::recheck_access(self).await;
    }
    fn set_taken_over(&self, id: protocol::ids::TargetId, taken: bool) -> bool {
        AgentBrowser::set_taken_over(self, id, taken)
    }
}
