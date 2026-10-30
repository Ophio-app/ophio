// SPDX-License-Identifier: Apache-2.0
//! What agents do in their tabs, and how a login's details go into a page.
//! Pages are read and changed only through `agent.js`, in a world of Ophio's
//! own in each frame, so page scripts can't see or alter what it does.
use super::{
    AgentBrowser, AgentError, Control, Observation, PageInfo, TabInfo, blocked_by, on_site,
    on_whole_site, origin_host,
    paint::{self, Rect},
    plain_host,
    run::{Kind, Owner, Running, VIEWPORT, Withheld},
};
use base64::Engine;
use ports::agent_browser::PageMark;
use protocol::{TaskId, ids::TargetId};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::time::Instant;

const AGENT: &str = include_str!("agent.js");
const WORLD: &str = "ophio-agent";
const NAVIGATE_WAIT: Duration = Duration::from_secs(30);
/// How long a page has to finish loading after it opens.
const LOAD_WAIT: Duration = Duration::from_secs(15);
/// How long a page has to settle after a click or key.
const ACTION_WAIT: Duration = Duration::from_secs(5);
/// How long a page has to go once Ophio empties its tab.
const LEAVE_WAIT: Duration = Duration::from_secs(2);
/// The largest screenshot an agent is sent.
const PICTURE_LIMIT: usize = 1024 * 1024;
/// Keys an agent may press: name, key, code and text. None of them copies,
/// selects or moves text, so a filled field can't be carried elsewhere.
const KEYS: &[(&str, &str, i64, &str)] = &[
    ("Enter", "Enter", 13, "\r"),
    ("Tab", "Tab", 9, ""),
    ("Escape", "Escape", 27, ""),
    ("Backspace", "Backspace", 8, ""),
    ("Delete", "Delete", 46, ""),
    ("ArrowLeft", "ArrowLeft", 37, ""),
    ("ArrowUp", "ArrowUp", 38, ""),
    ("ArrowRight", "ArrowRight", 39, ""),
    ("ArrowDown", "ArrowDown", 40, ""),
    ("Home", "Home", 36, ""),
    ("End", "End", 35, ""),
    ("PageUp", "PageUp", 33, ""),
    ("PageDown", "PageDown", 34, ""),
    ("Space", " ", 32, " "),
];

/// What a control reference from a look points at.
#[derive(Debug, Clone)]
pub(super) struct Handle {
    session: String,
    frame: String,
    id: u64,
}

/// A frame, where its view sits on the page, and the part of the page it
/// shows.
#[derive(Debug, Clone)]
pub(super) struct Frame {
    pub session: String,
    pub id: String,
    pub base: (f64, f64),
    pub clip: [f64; 4],
}
impl Frame {
    fn shows(&self, (x, y): (f64, f64)) -> bool {
        x >= self.clip[0] && y >= self.clip[1] && x < self.clip[2] && y < self.clip[3]
    }
}

/// A tab the owner may watch and use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OwnerTab {
    pub id: TargetId,
    pub title: String,
    pub url: String,
}

fn lost() -> AgentError {
    AgentError::Failed("The page stopped responding".into())
}
fn changed() -> AgentError {
    AgentError::Failed("The page changed; look at it again".into())
}
fn refused(message: impl Into<String>) -> AgentError {
    AgentError::Refused(message.into())
}
fn may_not_open(site: &str) -> AgentError {
    refused(format!(
        "This task may not open {site}: it is on the site of a login the owner keeps from this task"
    ))
}

impl AgentBrowser {
    /// Opens `url` in the task's current tab, or a new tab.
    pub async fn open(
        &self,
        task: TaskId,
        url: &str,
        new_tab: bool,
    ) -> Result<PageInfo, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let address = url::Url::parse(url).map_err(|_| refused("That is not a web address"))?;
        if !matches!(address.scheme(), "http" | "https") {
            return Err(refused("Only http and https addresses can be opened"));
        }
        let host = address
            .host_str()
            .map(plain_host)
            .ok_or_else(|| refused("That address has no site"))?;
        if let Some(site) = blocked_by(&host, &(entry.browsing.blocked)()) {
            return Err(may_not_open(&site));
        }
        let running = self.shared.running().await?;
        let existing = if new_tab {
            None
        } else {
            running.lock().current.get(&task).cloned()
        };
        let target = match existing {
            Some(target) => target,
            None => running.new_tab(task).await?,
        };
        running.ready(&target).await?;
        let session = running.usable(&target)?;
        if let Some(tab) = running.lock().tabs.get_mut(&target) {
            tab.refused = None;
        }
        let opened = running
            .pipe
            .call_within(
                Some(&session),
                "Page.navigate",
                json!({"url":address.as_str()}),
                NAVIGATE_WAIT,
            )
            .await
            .map_err(|_| AgentError::Failed("The page did not open in time".into()))?;
        if let Some(error) = opened["errorText"].as_str() {
            return Err(match running.take_refused(&target) {
                Some(site) => may_not_open(&site),
                None => AgentError::Failed(format!("The page could not be opened ({error})")),
            });
        }
        running.settle(&target, LOAD_WAIT).await;
        // A redirect to a blocked site ends on the browser's error page.
        if running.top_url(&target).await.starts_with("chrome-error:")
            && let Some(site) = running.take_refused(&target)
        {
            return Err(may_not_open(&site));
        }
        running.page_info(&target)
    }

    /// The task's tabs, oldest first.
    pub async fn tabs(&self, task: TaskId) -> Result<Vec<TabInfo>, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let Some(running) = self.shared.current() else {
            return Ok(Vec::new());
        };
        let state = running.lock();
        let current = state.current.get(&task);
        let mut tabs: Vec<_> = state
            .tabs
            .iter()
            .filter(|(_, t)| t.owner == Owner::Task(task))
            .collect();
        tabs.sort_by_key(|(_, t)| t.order);
        Ok(tabs
            .into_iter()
            .map(|(id, t)| TabInfo {
                tab: t.name.clone(),
                url: t.url.clone(),
                title: t.title.clone(),
                current: current == Some(id),
            })
            .collect())
    }

    pub async fn switch_tab(&self, task: TaskId, tab: &str) -> Result<PageInfo, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.named(task, tab)?;
        let _input = running.input.lock().await;
        running.lock().current.insert(task, target.clone());
        running.activate(&target).await?;
        running.page_info(&target)
    }

    /// Closes a tab, the current one if none is named.
    pub async fn close_tab(&self, task: TaskId, tab: Option<&str>) -> Result<(), AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = match tab {
            Some(tab) => running.named(task, tab)?,
            None => running.current_tab(task)?,
        };
        let _input = running.input.lock().await;
        running
            .pipe
            .call(None, "Target.closeTarget", json!({"targetId":target}))
            .await
            .map_err(|_| AgentError::Failed("The tab could not be closed".into()))?;
        // Its going is reported separately; the agent's next step sees it gone.
        let deadline = Instant::now() + Duration::from_secs(2);
        while running.lock().tabs.contains_key(&target) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }

    pub async fn back(&self, task: TaskId) -> Result<PageInfo, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.current_tab(task)?;
        let session = running.usable(&target)?;
        running.ready(&target).await?;
        let history = running
            .send(&session, "Page.getNavigationHistory", json!({}))
            .await?;
        let entry = history["currentIndex"]
            .as_u64()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| history["entries"].get(i as usize))
            .and_then(|e| e["id"].as_i64())
            .ok_or_else(|| refused("There is no earlier page in this tab"))?;
        running
            .send(
                &session,
                "Page.navigateToHistoryEntry",
                json!({"entryId":entry}),
            )
            .await?;
        running.settle(&target, LOAD_WAIT).await;
        running.page_info(&target)
    }

    /// The task's current page.
    pub async fn page(&self, task: TaskId) -> Result<PageInfo, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.current_tab(task)?;
        running.page_info(&target)
    }

    /// A picture of the current page and the controls on it. Control
    /// references are good until the next look.
    pub async fn observe(&self, task: TaskId) -> Result<Observation, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.current_tab(task)?;
        running.usable(&target)?;
        running.still_allowed(&target).await?;
        running.ready(&target).await?;
        running.refresh(&target).await;
        let frames = running.frames(&target).await;
        // No picture of a page that may show what Ophio typed: painting over
        // its fields can't cover a copy the page made elsewhere.
        let document = running.document(&target).await?;
        let mut withheld = running.lock().withheld(&target, &document);
        let (mut screenshot, mut screenshot_scale) = (String::new(), 1.);
        if withheld.is_none() {
            (screenshot, screenshot_scale) = running.picture(&target, &frames).await?;
            let now = running.document(&target).await?;
            if now != document {
                withheld = Some(
                    running
                        .lock()
                        .withheld(&target, &now)
                        .unwrap_or(Withheld::Changed),
                );
            }
            if withheld.is_some() {
                (screenshot, screenshot_scale) = (String::new(), 1.);
            }
        }
        let mut controls = Vec::new();
        let mut refs = HashMap::new();
        for frame in &frames {
            let Ok(found) = running
                .script(&frame.session, &frame.id, &[json!("observe")])
                .await
            else {
                continue;
            };
            for item in found.as_array().into_iter().flatten() {
                let number = |key: &str| item[key].as_f64();
                let (Some(id), Some(x), Some(y), Some(w), Some(h)) = (
                    item["id"].as_u64(),
                    number("x"),
                    number("y"),
                    number("w"),
                    number("h"),
                ) else {
                    continue;
                };
                let (x, y) = (frame.base.0 + x, frame.base.1 + y);
                if !frame.shows((x + w / 2., y + h / 2.)) {
                    continue;
                }
                let reference = format!("e{}", refs.len() + 1);
                refs.insert(
                    reference.clone(),
                    Handle {
                        session: frame.session.clone(),
                        frame: frame.id.clone(),
                        id,
                    },
                );
                let text = |key: &str| item[key].as_str().unwrap_or_default().to_owned();
                controls.push(Control {
                    reference,
                    role: text("role"),
                    name: text("name"),
                    value: item["value"].as_str().map(str::to_owned),
                    bounds: [x, y, w, h].map(|n| n.round() as i64),
                    disabled: item["disabled"] == true,
                    focused: item["focused"] == true,
                    checked: item["checked"].as_bool(),
                });
            }
        }
        let mut state = running.lock();
        let downloads = state.arrived.remove(&task).unwrap_or_default();
        let download_notices = state.download_notices.remove(&task).unwrap_or_default();
        let tab = state.tabs.get_mut(&target).ok_or_else(changed)?;
        tab.refs = refs;
        let notices: Vec<String> = [
            tab.refused.take().map(|site| {
                format!(
                    "Something on this page tried to reach {site}, which this task may not open, and was stopped"
                )
            }),
            std::mem::take(&mut tab.dropped_download).then(|| {
                "A download from this tab wasn't kept: its page may hold a password or code \
                 Ophio typed"
                    .to_owned()
            }),
            withheld.map(|why| match why {
                Withheld::Typed => "No screenshot: Ophio typed a password or code into this page. \
                                    Screenshots come back when the tab opens another page"
                    .to_owned(),
                Withheld::Beside => "No screenshot: this page was open while Ophio typed a password \
                                     or code into another of this task's pages. Screenshots come \
                                     back when the tab opens another page"
                    .to_owned(),
                Withheld::Changed => "No screenshot: the page changed while it was being taken; \
                                      look again"
                    .to_owned(),
            }),
        ]
        .into_iter()
        .flatten()
        .chain(download_notices)
        .collect();
        let notice = (!notices.is_empty()).then(|| notices.join(". "));
        Ok(Observation {
            tab: tab.name.clone(),
            url: tab.url.clone(),
            title: tab.title.clone(),
            screenshot,
            screenshot_scale,
            controls,
            downloads,
            notice,
        })
    }

    pub async fn click(&self, task: TaskId, reference: &str) -> Result<PageInfo, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.current_tab(task)?;
        let session = running.usable(&target)?;
        let (x, y) = running.point_of(&target, reference).await?;
        (running.fenced(&target, running.click_at(&session, x, y))).await?;
        // A click may open a tab, which becomes the current one.
        let now = running.current_tab(task).unwrap_or(target);
        running.settle(&now, ACTION_WAIT).await;
        running.page_info(&now)
    }

    /// Types `text` into a control, after what it holds or in its place.
    pub async fn type_text(
        &self,
        task: TaskId,
        reference: &str,
        text: &str,
        replace: bool,
    ) -> Result<(), AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.current_tab(task)?;
        let session = running.usable(&target)?;
        let (x, y) = running.point_of(&target, reference).await?;
        (running.fenced(&target, running.click_at(&session, x, y))).await?;
        let handle = running.control_of(&target, reference)?;
        let prepare = [json!("prepare"), json!(handle.id), json!(replace)];
        let ready = (running.fenced(
            &target,
            running.script(&handle.session, &handle.frame, &prepare),
        ))
        .await?;
        if ready != true {
            return Err(refused("That control did not take the keyboard"));
        }
        let typing = running.send(&session, "Input.insertText", json!({"text":text}));
        running.fenced(&target, typing).await?;
        Ok(())
    }

    pub async fn press(&self, task: TaskId, key: &str) -> Result<PageInfo, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.current_tab(task)?;
        let session = running.usable(&target)?;
        running
            .fenced(&target, running.press(&session, key))
            .await?;
        let now = running.current_tab(task).unwrap_or(target);
        running.settle(&now, ACTION_WAIT).await;
        running.page_info(&now)
    }

    /// Scrolls the page, or the part of it holding a control, by `dy` page
    /// pixels.
    pub async fn scroll(
        &self,
        task: TaskId,
        dy: f64,
        reference: Option<&str>,
    ) -> Result<(), AgentError> {
        if !dy.is_finite() || dy.abs() > 10_000. {
            return Err(refused("Scroll by at most 10000 pixels at a time"));
        }
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.current_tab(task)?;
        let session = running.usable(&target)?;
        let (x, y) = match reference {
            Some(reference) => running.point_of(&target, reference).await?,
            None => (f64::from(VIEWPORT.0) / 2., f64::from(VIEWPORT.1) / 2.),
        };
        let wheel = json!({"type":"mouseWheel","x":x,"y":y,"deltaX":0,"deltaY":dy});
        let scrolling = running.send(&session, "Input.dispatchMouseEvent", wheel);
        running.fenced(&target, scrolling).await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        Ok(())
    }

    /// Puts a login's username and password into the sign-in form on the
    /// task's current page. The page, and the frame holding the form, must be
    /// on one of `sites` over https, and each field must be one a person could
    /// see and type in.
    pub async fn fill_login(
        &self,
        task: TaskId,
        sites: &[String],
        username: &str,
        password: &str,
        submit: bool,
    ) -> Result<PageInfo, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.current_tab(task)?;
        let session = running.usable(&target)?;
        running.ready(&target).await?;
        running.check_top(&target, sites).await?;
        let (frame, found) = running
            .find(
                &target,
                |found| found["password"].is_u64(),
                &[json!("findLogin"), json!(sites)],
            )
            .await?;
        if !found["password"].is_u64() && username.is_empty() {
            return Err(refused(
                "This page asks for a username, and the login has none",
            ));
        }
        (running.fenced(&target, running.focus_frame(&session, &frame, &found))).await?;
        let details = json!({
            "username": (!username.is_empty()).then_some(username),
            "password": password,
        });
        let document = running.typing(&target).await?;
        let fill = [
            json!("fillLogin"),
            json!(sites),
            found["username"].clone(),
            found["password"].clone(),
            details,
        ];
        let filled =
            (running.fenced(&target, running.script(&frame.session, &frame.id, &fill))).await;
        running.typed(&target, &document).await;
        let filled = filled?;
        if let Some(error) = filled["error"].as_str() {
            return Err(refused(error));
        }
        if submit {
            running
                .fenced(&target, running.press(&session, "Enter"))
                .await?;
        }
        let now = running.current_tab(task).unwrap_or(target);
        running.settle(&now, ACTION_WAIT).await;
        running.page_info(&now)
    }

    /// Puts a sign-in code into the code field, or the row of one-character
    /// boxes, on the task's current page, with the same checks as
    /// [`Self::fill_login`].
    pub async fn fill_code(
        &self,
        task: TaskId,
        sites: &[String],
        code: &str,
        submit: bool,
        at: Option<&PageMark>,
    ) -> Result<PageInfo, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.current_tab(task)?;
        let session = running.usable(&target)?;
        running.ready(&target).await?;
        running.check_top(&target, sites).await?;
        if let Some(at) = at
            && running.mark(&target).await? != *at
        {
            return Err(refused(
                "The page changed while the owner was asked for the code, so it wasn't typed; \
                 ask again on the page that wants it",
            ));
        }
        let length = code.chars().count();
        let (frame, found) = running
            .find(
                &target,
                |_| true,
                &[json!("findCode"), json!(sites), json!(length)],
            )
            .await?;
        (running.fenced(&target, running.focus_frame(&session, &frame, &found))).await?;
        let document = running.typing(&target).await?;
        // The field was found on the marked page, if it is still the one
        // shown: a page's loader changes with every navigation, and the
        // field's reference dies with its page.
        if let Some(at) = at
            && document != at.document
        {
            return Err(refused(
                "The page changed while the owner was asked for the code, so it wasn't typed; \
                 ask again on the page that wants it",
            ));
        }
        let fill = [
            json!("fillCode"),
            json!(sites),
            found["fields"].clone(),
            json!(code),
        ];
        let filled =
            (running.fenced(&target, running.script(&frame.session, &frame.id, &fill))).await;
        running.typed(&target, &document).await;
        let filled = filled?;
        if let Some(error) = filled["error"].as_str() {
            return Err(refused(error));
        }
        if submit {
            running
                .fenced(&target, running.press(&session, "Enter"))
                .await?;
        }
        let now = running.current_tab(task).unwrap_or(target);
        running.settle(&now, ACTION_WAIT).await;
        running.page_info(&now)
    }

    /// Where the task is now: its tab, the page in it and its address.
    pub async fn mark(&self, task: TaskId) -> Result<PageMark, AgentError> {
        let entry = self.task(task)?;
        let _serial = entry.serial.lock().await;
        let running = self.running_now()?;
        let target = running.current_tab(task)?;
        running.mark(&target).await
    }

    /// Signs the agent browser out of `sites`: their cookies and stored data
    /// go. A running browser restarts for it, so every tab closes.
    pub async fn sign_out(&self, sites: &[String]) -> Result<(), AgentError> {
        let outcome = self.shared.signed_out(sites).await;
        // Started only for this, it closes again.
        self.shared.close_if_idle().await;
        outcome
    }

    /// Empties each task's tab that shows a site the task may no longer use,
    /// after a login was paused or changed.
    pub async fn recheck_access(&self) {
        if let Some(running) = self.shared.current() {
            running.recheck_access().await;
        }
    }

    /// The running browser, for a task that has opened a page.
    fn running_now(&self) -> Result<Arc<Running>, AgentError> {
        self.shared
            .current()
            .ok_or_else(|| refused("This task has no open tab; open a page first"))
    }

    /// Tabs of tasks, for the owner to watch.
    pub(super) fn owner_tabs(&self) -> (u64, Vec<OwnerTab>) {
        let Some(running) = self.shared.current() else {
            return (0, Vec::new());
        };
        let state = running.lock();
        let mut tabs: Vec<_> = state
            .tabs
            .values()
            .filter(|t| matches!(t.owner, Owner::Task(_)))
            .collect();
        tabs.sort_by_key(|t| t.order);
        (
            state.revision,
            tabs.into_iter()
                .map(|t| OwnerTab {
                    id: t.id,
                    title: t.title.clone(),
                    url: t.url.clone(),
                })
                .collect(),
        )
    }

    /// A picture of a task's tab for the owner, filled fields painted over.
    pub(super) async fn capture(&self, id: TargetId) -> Result<paint::Rgba, AgentError> {
        let running = self.shared.current().ok_or_else(changed)?;
        let target = running.by_id(id).ok_or_else(changed)?;
        running.refresh(&target).await;
        let frames = running.frames(&target).await;
        let (png, cover) = running.capture_png(&target, &frames).await?;
        let mut picture = paint::decode(&png).ok_or_else(lost)?;
        let scale = f64::from(picture.width) / f64::from(VIEWPORT.0);
        paint::cover(&mut picture, &cover, scale);
        Ok(picture)
    }

    /// Sends the owner's input to a task's tab.
    pub(super) async fn owner_input(
        &self,
        id: TargetId,
        events: &[(&str, Value)],
    ) -> Result<(), AgentError> {
        let running = self.shared.current().ok_or_else(changed)?;
        let target = running.by_id(id).ok_or_else(changed)?;
        let session = running.session(&target).ok_or_else(changed)?;
        let _input = running.input.lock().await;
        for (method, params) in events {
            running.send(&session, method, params.clone()).await?;
        }
        Ok(())
    }

    /// Marks a tab as the owner's while they use it, so agents can't, or
    /// gives it back. False if there is no such tab.
    pub fn set_taken_over(&self, id: TargetId, taken: bool) -> bool {
        let Some(running) = self.shared.current() else {
            return false;
        };
        let mut state = running.lock();
        match state.tabs.values_mut().find(|t| t.id == id) {
            Some(tab) => {
                tab.taken_over = taken;
                true
            }
            None => false,
        }
    }
}

impl Running {
    pub(super) async fn send(
        &self,
        session: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, AgentError> {
        self.pipe
            .call(Some(session), method, params)
            .await
            .map_err(|_| lost())
    }

    /// Runs one of `agent.js`'s operations in a frame.
    pub(super) async fn script(
        &self,
        session: &str,
        frame: &str,
        args: &[Value],
    ) -> Result<Value, AgentError> {
        let world = self
            .send(
                session,
                "Page.createIsolatedWorld",
                json!({"frameId":frame,"worldName":WORLD}),
            )
            .await?;
        let context = world["executionContextId"].as_i64().ok_or_else(lost)?;
        let arguments: Vec<Value> = args.iter().map(|a| json!({"value":a})).collect();
        let result = self
            .send(
                session,
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": AGENT,
                    "executionContextId": context,
                    "arguments": arguments,
                    "returnByValue": true,
                }),
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err(changed());
        }
        Ok(result["result"]["value"].clone())
    }

    async fn new_tab(&self, task: TaskId) -> Result<String, AgentError> {
        let _input = self.input.lock().await;
        let created = self
            .pipe
            .call(None, "Target.createTarget", json!({"url":"about:blank"}))
            .await
            .map_err(|_| AgentError::Failed("A new tab could not be opened".into()))?;
        let target = created["targetId"]
            .as_str()
            .ok_or_else(|| AgentError::Failed("A new tab could not be opened".into()))?
            .to_owned();
        self.claim(&target, task);
        self.activate(&target).await?;
        Ok(target)
    }

    /// The caller holds `input` until the operation needing this tab ends.
    async fn activate(&self, target: &str) -> Result<(), AgentError> {
        self.pipe
            .call(None, "Target.activateTarget", json!({"targetId":target}))
            .await
            .map_err(|_| lost())?;
        Ok(())
    }

    fn current_tab(&self, task: TaskId) -> Result<String, AgentError> {
        self.lock()
            .current
            .get(&task)
            .cloned()
            .ok_or_else(|| refused("This task has no open tab; open a page first"))
    }

    /// Runs `act`, which sends an agent's input to a tab or types or moves
    /// focus in its page, if agents may still use the tab. One at a time with
    /// the owner's input, so once the owner has taken the tab over nothing
    /// more of the agent's reaches it, and what was already on its way
    /// arrives before anything of theirs.
    async fn fenced<T>(
        &self,
        target: &str,
        act: impl Future<Output = Result<T, AgentError>>,
    ) -> Result<T, AgentError> {
        let _input = self.input.lock().await;
        self.usable(target)?;
        self.still_allowed(target).await?;
        act.await
    }

    /// Refuses, emptying the tab, when its task may no longer use the page
    /// it shows: a login it used was paused or changed since it opened.
    async fn still_allowed(&self, target: &str) -> Result<(), AgentError> {
        let Some(site) = self.kept_from(target) else {
            return Ok(());
        };
        self.empty(target, std::slice::from_ref(&site)).await;
        Err(refused(format!(
            "This task may no longer use {site}, so its page was closed; open another page"
        )))
    }

    pub(super) async fn recheck_access(&self) {
        let targets: Vec<String> = (self.lock().tabs.iter())
            .filter(|(_, t)| matches!(t.owner, Owner::Task(_)))
            .map(|(target, _)| target.clone())
            .collect();
        for target in targets {
            if let Some(site) = self.kept_from(&target) {
                self.empty(&target, std::slice::from_ref(&site)).await;
            }
        }
    }

    /// Stops the page a tab shows: it is emptied, or the tab closed when its
    /// page won't go. Whether the tab has gone or is off `sites`.
    pub(super) async fn empty(&self, target: &str, sites: &[String]) -> bool {
        if let Some(session) = self.session(target) {
            let _ = (self.send(&session, "Page.navigate", json!({"url":"about:blank"}))).await;
        }
        if self.left(target, sites).await {
            return true;
        }
        let _ = (self.pipe)
            .call(None, "Target.closeTarget", json!({"targetId":target}))
            .await;
        self.left(target, sites).await
    }

    /// Whether, within a moment, a tab has gone or shows no page on `sites`:
    /// neither its address nor the origin its page or a frame of it has is
    /// on them.
    async fn left(&self, target: &str, sites: &[String]) -> bool {
        let until = Instant::now() + LEAVE_WAIT;
        let on = |host: &str| !host.is_empty() && blocked_by(host, sites).is_some();
        loop {
            let Ok(info) = (self.pipe)
                .call(None, "Target.getTargetInfo", json!({"targetId":target}))
                .await
            else {
                return true;
            };
            let shows = origin_host(info["targetInfo"]["url"].as_str().unwrap_or_default());
            let has = self.lock().sites_of(target);
            if !on(&shows) && !has.iter().any(|site| on(site)) {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A tab's page session, if agents may use the tab now.
    fn usable(&self, target: &str) -> Result<String, AgentError> {
        let state = self.lock();
        let tab = state.tabs.get(target).ok_or_else(changed)?;
        if tab.taken_over {
            return Err(refused(
                "The owner is using this tab; wait until they hand it back",
            ));
        }
        Ok(tab.session.clone())
    }

    fn session(&self, target: &str) -> Option<String> {
        self.lock().tabs.get(target).map(|t| t.session.clone())
    }

    fn by_id(&self, id: TargetId) -> Option<String> {
        self.lock()
            .tabs
            .iter()
            .find(|(_, t)| t.id == id && matches!(t.owner, Owner::Task(_)))
            .map(|(target, _)| target.clone())
    }

    fn named(&self, task: TaskId, name: &str) -> Result<String, AgentError> {
        self.lock()
            .tabs
            .iter()
            .find(|(_, t)| t.owner == Owner::Task(task) && t.name == name)
            .map(|(target, _)| target.clone())
            .ok_or_else(|| refused(format!("This task has no tab {name}")))
    }

    fn control_of(&self, target: &str, reference: &str) -> Result<Handle, AgentError> {
        self.lock()
            .tabs
            .get(target)
            .and_then(|t| t.refs.get(reference).cloned())
            .ok_or_else(|| {
                refused(format!(
                    "There is no control {reference}; look at the page again"
                ))
            })
    }

    fn take_refused(&self, target: &str) -> Option<String> {
        self.lock().tabs.get_mut(target)?.refused.take()
    }

    fn page_info(&self, target: &str) -> Result<PageInfo, AgentError> {
        let state = self.lock();
        let tab = state
            .tabs
            .get(target)
            .ok_or_else(|| AgentError::Failed("The tab closed".into()))?;
        Ok(PageInfo {
            tab: tab.name.clone(),
            url: tab.url.clone(),
            title: tab.title.clone(),
        })
    }

    /// The page a tab shows now, by its document's loader.
    async fn document(&self, target: &str) -> Result<String, AgentError> {
        let session = self.session(target).ok_or_else(changed)?;
        let tree = self.send(&session, "Page.getFrameTree", json!({})).await?;
        tree["frameTree"]["frame"]["loaderId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(changed)
    }

    async fn mark(&self, target: &str) -> Result<PageMark, AgentError> {
        let document = self.document(target).await?;
        Ok(PageMark {
            tab: target.to_owned(),
            document,
            url: self.top_url(target).await,
        })
    }

    /// Marks the page a tab shows as typed into, before Ophio types: if it
    /// can't be told which page that is, nothing is typed.
    async fn typing(&self, target: &str) -> Result<String, AgentError> {
        let document = self.document(target).await?;
        self.lock().typing(target, &document);
        Ok(document)
    }

    /// Marks the page the tab shows after Ophio typed, if it isn't the one
    /// marked before: what was typed may have gone into it.
    async fn typed(&self, target: &str, before: &str) {
        if let Ok(now) = self.document(target).await
            && now != before
        {
            self.lock().typing(target, &now);
        }
    }

    async fn top_url(&self, target: &str) -> String {
        self.pipe
            .call(None, "Target.getTargetInfo", json!({"targetId":target}))
            .await
            .ok()
            .and_then(|info| info["targetInfo"]["url"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Brings the tab's address and title up to date; the browser doesn't
    /// announce a page's title once it has loaded.
    async fn refresh(&self, target: &str) {
        if let Ok(info) = self
            .pipe
            .call(None, "Target.getTargetInfo", json!({"targetId":target}))
            .await
        {
            self.info_changed(&info["targetInfo"]);
        }
    }

    /// Waits a while for the tab's page to finish loading.
    async fn settle(&self, target: &str, wait: Duration) {
        let deadline = Instant::now() + wait;
        tokio::time::sleep(Duration::from_millis(150)).await;
        while Instant::now() < deadline {
            let Some(session) = self.session(target) else {
                return;
            };
            if let Ok(state) = self.script(&session, target, &[json!("ready")]).await
                && state == "complete"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.refresh(target).await;
    }

    /// The tab's frames, its page's first. A frame that isn't laid out, such
    /// as one hidden, is left out, and so is what it holds.
    pub(super) async fn frames(&self, target: &str) -> Vec<Frame> {
        let mut out = Vec::new();
        let mut roots: HashMap<String, ((f64, f64), [f64; 4])> = HashMap::new();
        for (session, info) in self.sessions_of(target) {
            let (base, clip) = if info.kind == Kind::Page {
                (
                    (0., 0.),
                    [0., 0., f64::from(VIEWPORT.0), f64::from(VIEWPORT.1)],
                )
            } else {
                let Some((parent, (parent_base, parent_clip))) = info
                    .parent
                    .as_ref()
                    .and_then(|p| roots.get(p).map(|r| (p.clone(), *r)))
                else {
                    continue;
                };
                let Some(owner) = self.owner_box(&parent, &info.target).await else {
                    continue;
                };
                place(parent_base, parent_clip, owner)
            };
            roots.insert(session.clone(), (base, clip));
            let Ok(tree) = self.send(&session, "Page.getFrameTree", json!({})).await else {
                continue;
            };
            let mut stack = vec![(tree["frameTree"].clone(), base, clip)];
            while let Some((node, frame_base, frame_clip)) = stack.pop() {
                let Some(id) = node["frame"]["id"].as_str() else {
                    continue;
                };
                out.push(Frame {
                    session: session.clone(),
                    id: id.into(),
                    base: frame_base,
                    clip: frame_clip,
                });
                for child in node["childFrames"].as_array().into_iter().flatten() {
                    let Some(child_id) = child["frame"]["id"].as_str() else {
                        continue;
                    };
                    // A session's positions are all relative to its root's view.
                    if let Some(owner) = self.owner_box(&session, child_id).await {
                        let (child_base, child_clip) = place(base, frame_clip, owner);
                        stack.push((child.clone(), child_base, child_clip));
                    }
                }
            }
        }
        out
    }

    /// Where the element holding a frame shows its content, relative to the
    /// view of `session`'s root.
    async fn owner_box(&self, session: &str, frame: &str) -> Option<[f64; 4]> {
        let owner = self
            .send(session, "DOM.getFrameOwner", json!({"frameId":frame}))
            .await
            .ok()?;
        let node = owner["backendNodeId"].as_i64()?;
        let model = self
            .send(session, "DOM.getBoxModel", json!({"backendNodeId":node}))
            .await
            .ok()?;
        let quad: Vec<f64> = model["model"]["content"]
            .as_array()?
            .iter()
            .filter_map(Value::as_f64)
            .collect();
        if quad.len() != 8 {
            return None;
        }
        let xs = [quad[0], quad[2], quad[4], quad[6]];
        let ys = [quad[1], quad[3], quad[5], quad[7]];
        let low = |v: [f64; 4]| v.into_iter().fold(f64::INFINITY, f64::min);
        let high = |v: [f64; 4]| v.into_iter().fold(f64::NEG_INFINITY, f64::max);
        Some([low(xs), low(ys), high(xs), high(ys)])
    }

    /// Where filled fields are on the page.
    async fn filled(&self, frames: &[Frame]) -> Vec<Rect> {
        let mut out = Vec::new();
        for frame in frames {
            let Ok(rects) = self
                .script(&frame.session, &frame.id, &[json!("filledRects")])
                .await
            else {
                continue;
            };
            for r in rects.as_array().into_iter().flatten() {
                if let (Some(x), Some(y), Some(w), Some(h)) = (
                    r["x"].as_f64(),
                    r["y"].as_f64(),
                    r["w"].as_f64(),
                    r["h"].as_f64(),
                ) {
                    out.push(Rect {
                        x: frame.base.0 + x,
                        y: frame.base.1 + y,
                        w,
                        h,
                    });
                }
            }
        }
        out
    }

    /// A screenshot for an agent, with filled fields painted over wherever
    /// they were just before or just after it was taken.
    async fn picture(&self, target: &str, frames: &[Frame]) -> Result<(String, f64), AgentError> {
        let (png, cover) = self.capture_png(target, frames).await?;
        let (png, scale) =
            paint::picture(&png, &cover, VIEWPORT.0, PICTURE_LIMIT).ok_or_else(lost)?;
        Ok((base64::engine::general_purpose::STANDARD.encode(png), scale))
    }

    async fn capture_png(
        &self,
        target: &str,
        frames: &[Frame],
    ) -> Result<(Vec<u8>, Vec<Rect>), AgentError> {
        // Chromium may stop painting a background tab even with focus
        // emulation. Keep it in front until capture and the field checks end.
        let _input = self.input.lock().await;
        self.activate(target).await?;
        let session = self.session(target).ok_or_else(changed)?;
        let mut cover = self.filled(frames).await;
        let shot = self
            .send(
                &session,
                "Page.captureScreenshot",
                json!({"format":"png","captureBeyondViewport":false}),
            )
            .await?;
        cover.extend(self.filled(frames).await);
        let png = base64::engine::general_purpose::STANDARD
            .decode(shot["data"].as_str().unwrap_or_default())
            .map_err(|_| lost())?;
        Ok((png, cover))
    }

    /// Where on the page to click a control from the last look, bringing it
    /// into view first.
    async fn point_of(&self, target: &str, reference: &str) -> Result<(f64, f64), AgentError> {
        let handle = self.control_of(target, reference)?;
        let rect = self
            .script(
                &handle.session,
                &handle.frame,
                &[json!("rect"), json!(handle.id)],
            )
            .await?;
        let (Some(x), Some(y), Some(w), Some(h)) = (
            rect["x"].as_f64(),
            rect["y"].as_f64(),
            rect["w"].as_f64(),
            rect["h"].as_f64(),
        ) else {
            return Err(refused("That control is gone; look at the page again"));
        };
        let frames = self.frames(target).await;
        let frame = frames
            .iter()
            .find(|f| f.session == handle.session && f.id == handle.frame)
            .ok_or_else(|| refused("That control is out of view; scroll and look again"))?;
        let point = (frame.base.0 + x + w / 2., frame.base.1 + y + h / 2.);
        if !frame.shows(point) {
            return Err(refused(
                "That control is out of view; scroll and look again",
            ));
        }
        Ok(point)
    }

    async fn click_at(&self, session: &str, x: f64, y: f64) -> Result<(), AgentError> {
        self.send(
            session,
            "Input.dispatchMouseEvent",
            json!({"type":"mouseMoved","x":x,"y":y}),
        )
        .await?;
        let pressed = self
            .send(
                session,
                "Input.dispatchMouseEvent",
                json!({"type":"mousePressed","x":x,"y":y,"button":"left","clickCount":1}),
            )
            .await;
        // The button is let go even if pressing it went unanswered.
        let released = self
            .send(
                session,
                "Input.dispatchMouseEvent",
                json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}),
            )
            .await;
        pressed.and(released).map(|_| ())
    }

    async fn press(&self, session: &str, name: &str) -> Result<(), AgentError> {
        let (_, key, code, text) = KEYS
            .iter()
            .find(|k| k.0.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                refused(format!(
                    "Ophio presses only these keys: {}",
                    KEYS.iter().map(|k| k.0).collect::<Vec<_>>().join(", ")
                ))
            })?;
        let code_name = if *key == " " { "Space" } else { key };
        let mut down =
            json!({"type":"keyDown","key":key,"code":code_name,"windowsVirtualKeyCode":code});
        if !text.is_empty() {
            down["text"] = json!(text);
        }
        let pressed = self.send(session, "Input.dispatchKeyEvent", down).await;
        let released = self
            .send(
                session,
                "Input.dispatchKeyEvent",
                json!({"type":"keyUp","key":key,"code":code_name,"windowsVirtualKeyCode":code}),
            )
            .await;
        pressed.and(released).map(|_| ())
    }

    /// Refuses unless the tab's page is on one of `sites` over https.
    async fn check_top(&self, target: &str, sites: &[String]) -> Result<(), AgentError> {
        let url = self.top_url(target).await;
        let address = url::Url::parse(&url).map_err(|_| refused("This tab has no page"))?;
        if address.scheme() != "https" {
            return Err(refused(
                "This page is not secure, so the login won't be used on it",
            ));
        }
        let host = address.host_str().map(plain_host).unwrap_or_default();
        if !sites.iter().any(|site| on_site(&host, site)) {
            return Err(refused("This page is not on the login's site"));
        }
        Ok(())
    }

    /// The first frame whose `agent.js` search succeeds, preferring one that
    /// `best` accepts.
    async fn find(
        &self,
        target: &str,
        best: impl Fn(&Value) -> bool,
        args: &[Value],
    ) -> Result<(Frame, Value), AgentError> {
        let frames = self.frames(target).await;
        let mut fallback = None;
        let mut problem = None;
        for frame in frames {
            let Ok(found) = self.script(&frame.session, &frame.id, args).await else {
                continue;
            };
            if let Some(error) = found["error"].as_str() {
                // Why a frame with fields can't be used is worth more than
                // another frame having none.
                if problem
                    .as_deref()
                    .is_none_or(|p: &str| p.starts_with("There is no"))
                {
                    problem = Some(error.to_owned());
                }
                continue;
            }
            if best(&found) {
                return Ok((frame, found));
            }
            fallback.get_or_insert((frame, found));
        }
        fallback.ok_or_else(|| {
            refused(problem.unwrap_or_else(|| "There is no sign-in field here".into()))
        })
    }

    /// Gives a frame the keyboard by clicking its field, when it hasn't it, so
    /// the browser accepts text there.
    async fn focus_frame(
        &self,
        session: &str,
        frame: &Frame,
        found: &Value,
    ) -> Result<(), AgentError> {
        if found["focused"] == true {
            return Ok(());
        }
        let at = &found["at"];
        let (Some(x), Some(y), Some(w), Some(h)) = (
            at["x"].as_f64(),
            at["y"].as_f64(),
            at["w"].as_f64(),
            at["h"].as_f64(),
        ) else {
            return Err(changed());
        };
        let point = (frame.base.0 + x + w / 2., frame.base.1 + y + h / 2.);
        if !frame.shows(point) {
            return Err(refused(
                "The sign-in form is out of view; scroll to it first",
            ));
        }
        self.click_at(session, point.0, point.1).await
    }

    /// Clears the cookies of `sites` and whatever any origin on them stored.
    /// Run only on a browser that has just started, before anything else
    /// can use it, so no page or request of the sites' is left to put any of
    /// it back.
    pub(super) async fn sign_out(&self, sites: &[String]) -> Result<(), AgentError> {
        let failed = || AgentError::Failed("The agent browser could not be signed out".into());
        let keeper = self.keeper_session().await.ok_or_else(failed)?;
        let mut origins = BTreeSet::new();
        for site in sites {
            let site = plain_host(site);
            let site = site.strip_prefix("www.").unwrap_or(&site);
            origins.insert(format!("https://{site}"));
            origins.insert(format!("https://www.{site}"));
            origins.insert(format!("http://{site}"));
        }
        let cookies = self
            .pipe
            .call(None, "Storage.getCookies", json!({}))
            .await
            .map_err(|_| failed())?;
        for cookie in cookies["cookies"].as_array().into_iter().flatten() {
            let domain = cookie["domain"].as_str().unwrap_or_default();
            let host = plain_host(domain.trim_start_matches('.'));
            // A cookie anywhere on the whole site, which its subdomains
            // share and send to one another.
            if !sites.iter().any(|site| on_whole_site(&host, site)) {
                continue;
            }
            let mut params = json!({
                "name": cookie["name"],
                "domain": domain,
                "path": cookie["path"],
            });
            if !cookie["partitionKey"].is_null() {
                params["partitionKey"] = cookie["partitionKey"].clone();
            }
            self.send(&keeper, "Network.deleteCookies", params)
                .await
                .map_err(|_| failed())?;
            origins.insert(format!("https://{host}"));
        }
        // Whatever any origin on the whole site stored, on any port.
        origins.extend(self.lock().origins.on(sites));
        // Only a page's session can clear storage; the browser's own can't.
        for origin in &origins {
            self.send(
                &keeper,
                "Storage.clearDataForOrigin",
                json!({"origin":origin,"storageTypes":"all"}),
            )
            .await
            .map_err(|_| failed())?;
        }
        Ok(())
    }
}

/// A frame's view and visible part, from its parent's and where the element
/// holding it shows it.
fn place(base: (f64, f64), clip: [f64; 4], owner: [f64; 4]) -> ((f64, f64), [f64; 4]) {
    let frame_base = (base.0 + owner[0], base.1 + owner[1]);
    let frame_clip = [
        clip[0].max(frame_base.0),
        clip[1].max(frame_base.1),
        clip[2].min(base.0 + owner[2]),
        clip[3].min(base.1 + owner[3]),
    ];
    (frame_base, frame_clip)
}
