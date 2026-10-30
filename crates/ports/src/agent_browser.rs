// SPDX-License-Identifier: Apache-2.0
//! Ophio's agent browser as the services reach it: tasks browse in their own
//! tabs, and a login's password goes in only through
//! [`AgentBrowsing::fill_login`], which checks the site and the field itself.
use async_trait::async_trait;
use protocol::{TaskId, ids::TargetId};
use serde::Serialize;
use std::{path::PathBuf, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    /// Ophio declined; the message tells the agent why.
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Failed(String),
}

/// What a task's browsing is allowed, as the task's logins decide it.
#[derive(Clone)]
pub struct TaskBrowsing {
    /// Sites the task may not open, nor any of their whole sites (see
    /// [`crate::agent_logins::whole_site`]). Asked again for
    /// every request, so a change to a login applies at once.
    pub blocked: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Where the task's downloads go.
    pub downloads: PathBuf,
}
impl std::fmt::Debug for TaskBrowsing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskBrowsing")
            .field("downloads", &self.downloads)
            .finish_non_exhaustive()
    }
}

/// A tab and the page in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageInfo {
    pub tab: String,
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TabInfo {
    pub tab: String,
    pub url: String,
    pub title: String,
    pub current: bool,
}

/// What the agent sees of the current page.
#[derive(Debug, Clone, Serialize)]
pub struct Observation {
    pub tab: String,
    pub url: String,
    pub title: String,
    /// A PNG of the visible page, base64, with filled fields painted over;
    /// empty when the page holds something Ophio typed into it.
    pub screenshot: String,
    /// Screenshot pixels per page pixel; control bounds are page pixels.
    pub screenshot_scale: f64,
    pub controls: Vec<Control>,
    /// Files that finished downloading since the last look.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub downloads: Vec<String>,
    /// Something the agent should know, such as a site the task may not open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// One control on the page, which the agent names by its `ref`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Control {
    #[serde(rename = "ref")]
    pub reference: String,
    pub role: String,
    /// As the page has it, up to [`RAW_TEXT`] characters, until
    /// [`Observation::tidy`] makes it what the agent is shown.
    pub name: String,
    /// Left out for passwords, filled fields and fields that look secret.
    /// Whole up to [`RAW_TEXT`] characters, as `name` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// x, y, width and height in page pixels.
    pub bounds: [i64; 4],
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
}

/// The most of a control's name or value the browser gives, followed by `…`
/// when there was more: enough to hold any password whole, so it can be
/// masked before the text is cut to what is shown.
pub const RAW_TEXT: usize = 2048;
/// The most of a control's name the agent is shown, and of its value.
pub const NAME_SHOWN: usize = 120;
pub const VALUE_SHOWN: usize = 500;

impl Observation {
    /// Every text in it, so each can be masked.
    pub fn visit_text(&mut self, f: &mut dyn FnMut(&mut String)) {
        f(&mut self.url);
        f(&mut self.title);
        for control in &mut self.controls {
            f(&mut control.name);
            if let Some(value) = &mut control.value {
                f(value);
            }
        }
        self.downloads.iter_mut().for_each(&mut *f);
        if let Some(notice) = &mut self.notice {
            f(notice);
        }
    }

    /// Names with their spaces closed up, and names and values cut to what
    /// the agent is shown. Done after masking, so neither can turn a secret
    /// into text that no longer matches it; mask again afterwards for the
    /// start of one left at a cut.
    pub fn tidy(&mut self) {
        for control in &mut self.controls {
            let name = control
                .name
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            control.name = shorten(name, NAME_SHOWN);
            if let Some(value) = control.value.take() {
                control.value = Some(shorten(value, VALUE_SHOWN));
            }
        }
    }
}

/// `text` cut to `most` characters, with `…` where it was cut.
fn shorten(text: String, most: usize) -> String {
    match text.char_indices().nth(most) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
}

/// Where a task was when the owner was asked for a code: its tab, the page
/// in it and the page's address. A code is typed only where it was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMark {
    pub tab: String,
    pub document: String,
    pub url: String,
}

/// The agent browser. A task's operations act on its current tab.
#[async_trait]
pub trait AgentBrowsing: Send + Sync + std::fmt::Debug + 'static {
    /// Lets `task` browse, within `browsing`.
    fn register(&self, task: TaskId, browsing: TaskBrowsing);
    /// The task's browsing has ended and its tabs close.
    async fn close_task(&self, task: TaskId);
    async fn open(&self, task: TaskId, url: &str, new_tab: bool) -> Result<PageInfo, AgentError>;
    async fn tabs(&self, task: TaskId) -> Result<Vec<TabInfo>, AgentError>;
    async fn switch_tab(&self, task: TaskId, tab: &str) -> Result<PageInfo, AgentError>;
    /// Closes `tab`, or the current tab.
    async fn close_tab(&self, task: TaskId, tab: Option<&str>) -> Result<(), AgentError>;
    async fn back(&self, task: TaskId) -> Result<PageInfo, AgentError>;
    /// The current tab and its page.
    async fn page(&self, task: TaskId) -> Result<PageInfo, AgentError>;
    async fn observe(&self, task: TaskId) -> Result<Observation, AgentError>;
    async fn click(&self, task: TaskId, reference: &str) -> Result<PageInfo, AgentError>;
    async fn type_text(
        &self,
        task: TaskId,
        reference: &str,
        text: &str,
        replace: bool,
    ) -> Result<(), AgentError>;
    async fn press(&self, task: TaskId, key: &str) -> Result<PageInfo, AgentError>;
    /// Scrolls the page, or the part of it holding a control.
    async fn scroll(
        &self,
        task: TaskId,
        dy: f64,
        reference: Option<&str>,
    ) -> Result<(), AgentError>;
    /// Puts a login into the sign-in form of the current page, which must be
    /// on one of `sites` over https.
    async fn fill_login(
        &self,
        task: TaskId,
        sites: &[String],
        username: &str,
        password: &str,
        submit: bool,
    ) -> Result<PageInfo, AgentError>;
    /// Where the task is now, for [`Self::fill_code`].
    async fn mark(&self, task: TaskId) -> Result<PageMark, AgentError>;
    /// Puts a sign-in code into the current page, with the same checks, and
    /// only if it is still the page `at` marks.
    async fn fill_code(
        &self,
        task: TaskId,
        sites: &[String],
        code: &str,
        submit: bool,
        at: Option<&PageMark>,
    ) -> Result<PageInfo, AgentError>;
    /// Signs the browser out of `sites` for every task.
    async fn sign_out(&self, sites: &[String]) -> Result<(), AgentError>;
    /// Empties each task's tab that shows a site the task may no longer use,
    /// after a login was paused or changed.
    async fn recheck_access(&self) {}
    /// Marks a tab the owner's while they use it, or gives it back. False if
    /// there is no such tab.
    fn set_taken_over(&self, id: TargetId, taken: bool) -> bool;
}
