// SPDX-License-Identifier: Apache-2.0
//! CDP never binds a network debugging port or attaches to an everyday profile.
pub mod agent;
mod pipe;
mod profile;
use async_trait::async_trait;
pub use profile::sweep;
use protocol::voice::*;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
use voice_control::{Driver, Error, Snapshot};

/// How long a new browser has to open its first tab.
const FIRST_TAB_WAIT: Duration = Duration::from_secs(10);

pub fn discover() -> Option<PathBuf> {
    #[cfg(windows)]
    let candidates: Vec<PathBuf> = ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .flat_map(|root| {
            [
                "Google/Chrome/Application/chrome.exe",
                "Microsoft/Edge/Application/msedge.exe",
                "BraveSoftware/Brave-Browser/Application/brave.exe",
                "Chromium/Application/chrome.exe",
            ]
            .map(|relative| PathBuf::from(&root).join(relative))
        })
        .collect();
    #[cfg(not(windows))]
    let candidates = [
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/usr/bin/microsoft-edge",
        "/usr/bin/brave-browser",
    ];
    candidates.iter().map(PathBuf::from).find(|p| p.is_file())
}
/// What the browser at `executable` is called.
pub fn name(executable: &Path) -> String {
    let stem = executable
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let named = [
        ("chrome", "Google Chrome"),
        ("edge", "Microsoft Edge"),
        ("brave", "Brave"),
        ("chromium", "Chromium"),
    ];
    // Chromium's own Windows build is chrome.exe in a Chromium folder.
    if stem == "chrome"
        && executable
            .components()
            .any(|part| part.as_os_str().eq_ignore_ascii_case("Chromium"))
    {
        return "Chromium".into();
    }
    named
        .iter()
        .find(|(part, _)| stem.contains(part))
        .map_or_else(|| "Chromium".into(), |(_, name)| (*name).into())
}
#[derive(Debug)]
pub struct Browser {
    pipe: Arc<pipe::Pipe>,
    attached: Mutex<Option<Page>>,
    serial: Mutex<()>,
    held: Mutex<Vec<Value>>,
}
#[derive(Debug, Clone)]
struct Page {
    target: String,
    session: String,
    context: i64,
}
impl Browser {
    pub async fn launch(
        executable: &Path,
        data_dir: &Path,
        headless: bool,
    ) -> Result<Arc<Self>, Error> {
        // A new profile for each window prevents reattaching to a pre-existing process.
        let profile = profile::directory(data_dir).join(protocol::CommandId::new().to_string());
        tokio::fs::create_dir_all(&profile)
            .await
            .map_err(|_| Error::Unavailable("Cannot create the separate browser profile".into()))?;
        let pipe = match pipe::Pipe::launch(executable, &profile, headless, false, &[]).await {
            Ok(pipe) => pipe,
            Err(e) => {
                tokio::spawn(profile::remove_when_unused(profile));
                return Err(e);
            }
        };
        let browser = Arc::new(Self {
            pipe,
            attached: Mutex::new(None),
            serial: Mutex::new(()),
            held: Mutex::new(Vec::new()),
        });
        if let Err(e) = browser.attach_first().await {
            browser.pipe.close_for_test().await;
            return Err(e);
        }
        Ok(browser)
    }
    async fn attach_first(&self) -> Result<(), Error> {
        // The browser can answer before its first tab exists.
        let deadline = tokio::time::Instant::now() + FIRST_TAB_WAIT;
        loop {
            let targets = match self.pipe.call(None, "Target.getTargets", json!({})).await {
                Ok(targets) => targets,
                // A slow start can outlast one call's wait. Asking for the
                // targets again changes nothing, so keep asking until the deadline.
                Err(Error::OutcomeUnknown) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                Err(e) => return Err(e),
            };
            let id = targets["targetInfos"]
                .as_array()
                .and_then(|ts| ts.iter().find(|t| t["type"] == "page"))
                .and_then(|t| t["targetId"].as_str());
            if let Some(id) = id {
                return self.attach(id).await;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Stopped);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    async fn attach(&self, id: &str) -> Result<(), Error> {
        let v = self
            .pipe
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId":id,"flatten":true}),
            )
            .await?;
        let session = v["sessionId"].as_str().ok_or(Error::Stopped)?.to_owned();
        self.pipe
            .call(Some(&session), "Page.enable", json!({}))
            .await?;
        let tree = self
            .pipe
            .call(Some(&session), "Page.getFrameTree", json!({}))
            .await?;
        let frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or(Error::Stopped)?;
        let world = self
            .pipe
            .call(
                Some(&session),
                "Page.createIsolatedWorld",
                json!({"frameId":frame,"worldName":"ophio-voice","grantUniveralAccess":false}),
            )
            .await?;
        let context = world["executionContextId"].as_i64().ok_or(Error::Stopped)?;
        *self.attached.lock().await = Some(Page {
            target: id.into(),
            session,
            context,
        });
        Ok(())
    }
    async fn page(&self) -> Result<Page, Error> {
        self.attached.lock().await.clone().ok_or(Error::Stopped)
    }
    async fn eval(&self, expression: &str) -> Result<Value, Error> {
        let page = self.page().await?;
        let value = self
            .pipe
            .call(
                Some(&page.session),
                "Runtime.evaluate",
                json!({"expression":expression,"contextId":page.context,"returnByValue":true}),
            )
            .await?;
        if value.get("exceptionDetails").is_some() {
            return Err(Error::StaleTarget);
        }
        Ok(value["result"]["value"].clone())
    }
    async fn read_snapshot(&self) -> Result<Snapshot, Error> {
        // Navigation destroys isolated worlds. Recreate on the current frame before reading.
        let page = self.page().await?;
        let tree = self
            .pipe
            .call(Some(&page.session), "Page.getFrameTree", json!({}))
            .await?;
        let frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or(Error::StaleTarget)?;
        let world = self
            .pipe
            .call(
                Some(&page.session),
                "Page.createIsolatedWorld",
                json!({"frameId":frame,"worldName":"ophio-voice","grantUniveralAccess":false}),
            )
            .await?;
        if let Some(current) = self.attached.lock().await.as_mut() {
            current.context = world["executionContextId"]
                .as_i64()
                .ok_or(Error::StaleTarget)?;
        }
        let value = self.eval(include_str!("snapshot.js")).await?;
        let mut elements: Vec<VoiceElement> =
            serde_json::from_value(value["elements"].clone()).map_err(|_| Error::StaleTarget)?;
        // Read native accessibility names/roles only for non-editable, non-sensitive buttons/links.
        // Text/Value nodes and relatives are deliberately excluded from these AX requests.
        if let Some(ids) = value["ax"].as_array() {
            for element in elements
                .iter_mut()
                .filter(|e| ids.iter().any(|id| id.as_str() == Some(&e.id)))
            {
                let id = serde_json::to_string(&element.id).map_err(|_| Error::StaleTarget)?;
                let page = self.page().await?;
                let node=self.pipe.call(Some(&page.session),"Runtime.evaluate",json!({"expression":format!("globalThis.__voiceControls.nodes.get({id})"),"contextId":page.context,"objectGroup":"voice-ax"})).await?;
                if let Some(object) = node["result"]["objectId"].as_str() {
                    let ax = self
                        .pipe
                        .call(
                            Some(&page.session),
                            "Accessibility.getPartialAXTree",
                            json!({"objectId":object,"fetchRelatives":false}),
                        )
                        .await?;
                    if let Some(control) = ax["nodes"].as_array().and_then(|nodes| nodes.first()) {
                        if let Some(name) = control["name"]["value"].as_str() {
                            element.name = name.to_owned();
                        }
                        if let Some(role) = control["role"]["value"].as_str() {
                            element.role = role.to_owned();
                        }
                    }
                }
            }
            let _ = self
                .pipe
                .call(
                    Some(&page.session),
                    "Runtime.releaseObjectGroup",
                    json!({"objectGroup":"voice-ax"}),
                )
                .await;
        }
        Ok(Snapshot::new(
            elements,
            value["context"].as_str().ok_or(Error::StaleTarget)?.into(),
        ))
    }
    /// Whether the window has closed and all of its processes have exited.
    pub fn is_closed(&self) -> bool {
        self.pipe.is_closed()
    }
    /// Test teardown only: normal session close uses detach and leaves the window open.
    pub async fn close_for_test(&self) {
        self.pipe.close_for_test().await;
    }
}
#[async_trait]
impl Driver for Browser {
    async fn snapshot(&self) -> Result<Snapshot, Error> {
        let _serial = self.serial.lock().await;
        self.read_snapshot().await
    }
    async fn act(&self, action: &VoiceAction, snapshot: &Snapshot) -> Result<(), Error> {
        let _serial = self.serial.lock().await;
        let current = self.read_snapshot().await?;
        if !current.still_matches(snapshot, action) {
            return Err(Error::StaleTarget);
        }
        let page = self.page().await?;
        let call = |method, params| self.pipe.call(Some(&page.session), method, params);
        if let Some(target) = &action.target {
            if action.kind == VoiceActionKind::Type && target.sensitive {
                return Err(Error::Unavailable(
                    "Use the remote keyboard for sensitive fields".into(),
                ));
            }
            let id = serde_json::to_string(&target.id).map_err(|_| Error::StaleTarget)?;
            let rect = self.eval(&format!("(() => {{ const e=globalThis.__voiceControls?.nodes.get({id}); if(!e || !e.isConnected) return null; e.scrollIntoView({{block:'nearest',inline:'nearest'}}); e.focus(); const r=e.getBoundingClientRect(); let x=r.x,y=r.y,w=e.ownerDocument.defaultView; while(w.frameElement) {{ const f=w.frameElement,b=f.getBoundingClientRect(); x+=b.x+f.clientLeft; y+=b.y+f.clientTop; w=w.parent; }} return {{x:x+r.width/2,y:y+r.height/2}}; }})()")).await?;
            if action.kind == VoiceActionKind::Click {
                let x = rect["x"].as_f64().ok_or(Error::StaleTarget)?;
                let y = rect["y"].as_f64().ok_or(Error::StaleTarget)?;
                self.held.lock().await.push(json!({"method":"Input.dispatchMouseEvent","params":{"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}}));
                call(
                    "Input.dispatchMouseEvent",
                    json!({"type":"mousePressed","x":x,"y":y,"button":"left","clickCount":1}),
                )
                .await?;
                call(
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}),
                )
                .await?;
                self.held.lock().await.clear();
            }
        }
        match action.kind {
            VoiceActionKind::Click | VoiceActionKind::Focus => {}
            VoiceActionKind::Type => {
                if action.target.is_none() {
                    return Err(Error::StaleTarget);
                }
                call(
                    "Input.insertText",
                    json!({"text":action.literal.as_deref().ok_or(Error::StaleTarget)?}),
                )
                .await?;
            }
            VoiceActionKind::Navigate => {
                let url = action.literal.as_deref().ok_or(Error::StaleTarget)?;
                if !url.starts_with("https://") && !url.starts_with("http://") {
                    return Err(Error::StaleTarget);
                }
                let result = call("Page.navigate", json!({"url":url})).await?;
                if result.get("errorText").is_some() {
                    return Err(Error::Unavailable(
                        "The browser could not open that address".into(),
                    ));
                }
            }
            VoiceActionKind::Scroll => {
                call("Input.dispatchMouseEvent",json!({"type":"mouseWheel","x":100,"y":100,"deltaX":0,"deltaY":action.scroll_y})).await?;
            }
            VoiceActionKind::Back | VoiceActionKind::Forward => {
                let history = call("Page.getNavigationHistory", json!({})).await?;
                let index = history["currentIndex"].as_i64().ok_or(Error::StaleTarget)?
                    + if action.kind == VoiceActionKind::Back {
                        -1
                    } else {
                        1
                    };
                let entry = history["entries"]
                    .as_array()
                    .and_then(|e| usize::try_from(index).ok().and_then(|i| e.get(i)))
                    .and_then(|v| v["id"].as_i64())
                    .ok_or_else(|| Error::Unavailable("No history in that direction".into()))?;
                call("Page.navigateToHistoryEntry", json!({"entryId":entry})).await?;
            }
            VoiceActionKind::SwitchTab => {
                let targets = self.pipe.call(None, "Target.getTargets", json!({})).await?;
                let pages: Vec<_> = targets["targetInfos"]
                    .as_array()
                    .ok_or(Error::StaleTarget)?
                    .iter()
                    .filter(|t| t["type"] == "page")
                    .filter_map(|t| t["targetId"].as_str())
                    .collect();
                let at = pages
                    .iter()
                    .position(|id| *id == page.target)
                    .ok_or(Error::StaleTarget)?;
                let next = if action.previous_tab {
                    (at + pages.len() - 1) % pages.len()
                } else {
                    (at + 1) % pages.len()
                };
                self.pipe
                    .call(
                        None,
                        "Target.detachFromTarget",
                        json!({"sessionId":page.session}),
                    )
                    .await?;
                self.attach(pages[next]).await?;
                self.pipe
                    .call(
                        None,
                        "Target.activateTarget",
                        json!({"targetId":pages[next]}),
                    )
                    .await?;
            }
            VoiceActionKind::PressKey => {
                let key = action
                    .literal
                    .as_deref()
                    .filter(|k| voice_control::policy::allowed_key(k))
                    .ok_or(Error::StaleTarget)?;
                let native = match key {
                    "enter" => "Enter",
                    "tab" => "Tab",
                    "escape" => "Escape",
                    "delete" => "Delete",
                    "backspace" => "Backspace",
                    other => other,
                };
                let code = match native {
                    "Enter" => 13,
                    "Tab" => 9,
                    "Escape" => 27,
                    "Backspace" => 8,
                    "Delete" => 46,
                    "ArrowLeft" => 37,
                    "ArrowUp" => 38,
                    "ArrowRight" => 39,
                    "ArrowDown" => 40,
                    "Home" => 36,
                    "End" => 35,
                    "PageUp" => 33,
                    "PageDown" => 34,
                    _ => 0,
                };
                self.held.lock().await.push(json!({"method":"Input.dispatchKeyEvent","params":{"type":"keyUp","key":native,"windowsVirtualKeyCode":code}}));
                call(
                    "Input.dispatchKeyEvent",
                    json!({"type":"keyDown","key":native,"windowsVirtualKeyCode":code}),
                )
                .await?;
                call(
                    "Input.dispatchKeyEvent",
                    json!({"type":"keyUp","key":native,"windowsVirtualKeyCode":code}),
                )
                .await?;
                self.held.lock().await.clear();
            }
            VoiceActionKind::Cancel | VoiceActionKind::None => {}
        }
        Ok(())
    }
    async fn picture(&self) -> Result<Option<String>, Error> {
        let _serial = self.serial.lock().await;
        let page = self.page().await?;
        let layout = self
            .pipe
            .call(Some(&page.session), "Page.getLayoutMetrics", json!({}))
            .await?;
        let width = layout["cssVisualViewport"]["clientWidth"]
            .as_f64()
            .ok_or(Error::StaleTarget)?;
        let height = layout["cssVisualViewport"]["clientHeight"]
            .as_f64()
            .ok_or(Error::StaleTarget)?;
        for scale in [1., 0.75, 0.5, 0.25] {
            let result=self.pipe.call(Some(&page.session),"Page.captureScreenshot",json!({"format":"png","captureBeyondViewport":false,"clip":{"x":0,"y":0,"width":width,"height":height,"scale":scale}})).await?;
            let data = result["data"].as_str().ok_or(Error::StaleTarget)?;
            if data.len() <= 384 * 1024 {
                return Ok(Some(data.into()));
            }
        }
        Err(Error::Unavailable(
            "The browser picture is too large".into(),
        ))
    }
    async fn barrier(&self) -> Result<(), Error> {
        let _serial = self.serial.lock().await;
        let page = self.page().await?;
        let mut held = self.held.lock().await;
        for release in held.iter() {
            self.pipe
                .call(
                    Some(&page.session),
                    release["method"].as_str().ok_or(Error::OutcomeUnknown)?,
                    release["params"].clone(),
                )
                .await?;
        }
        held.clear();
        self.eval("void 0").await?;
        Ok(())
    }
    async fn detach(&self) -> Result<(), Error> {
        let _serial = self.serial.lock().await;
        if let Some(page) = self.attached.lock().await.take() {
            self.pipe
                .call(
                    None,
                    "Target.detachFromTarget",
                    json!({"sessionId":page.session}),
                )
                .await?;
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
