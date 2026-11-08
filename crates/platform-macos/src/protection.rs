// SPDX-License-Identifier: Apache-2.0
//! The local Stop indicator: a menu bar item. A menu needs a program of its own
//! whose first thread runs the system's event loop, so Øphio starts itself in
//! that role and speaks to it over its input and output. If it goes away, for
//! any reason, its protection has gone, and control stops.
#![allow(unsafe_code)]

use async_trait::async_trait;
use objc2::{
    MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, NSObjectProtocol},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{MainThreadMarker, NSString};
use ports::desktop::{IndicatorSession, LocalIndicator, PlatformError};
use std::{
    io::{BufRead, Write},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin},
    sync::{Mutex, mpsc},
};

/// The command line argument that starts this program as the indicator.
pub const INDICATOR_ROLE: &str = "__local-stop-indicator";

const TITLE: &str = "Øphio";
const STOP: &str = "Stop computer control";

/// Lines the two sides send each other. The holder's name is cleaned of any line
/// break by the sender, so one message is always one line.
fn show_line(holders: &str) -> String {
    let holders: String = holders.chars().filter(|c| !c.is_control()).collect();
    format!("show {holders}\n")
}

#[derive(Debug, PartialEq, Eq)]
enum Request {
    Show(String),
    Hide,
}
fn request(line: &str) -> Option<Request> {
    match line.trim_end_matches('\n') {
        "hide" => Some(Request::Hide),
        line => line
            .strip_prefix("show ")
            .map(|holders| Request::Show(holders.to_owned())),
    }
}

struct Indicator {
    input: Mutex<ChildStdin>,
    acknowledged: Mutex<mpsc::Receiver<()>>,
    /// Ends the program with this.
    _child: Mutex<Child>,
}

impl Indicator {
    async fn send(&self, line: &str) -> Result<(), PlatformError> {
        let unavailable = || PlatformError::Native("Local Stop indicator is unavailable".into());
        self.input
            .lock()
            .await
            .write_all(line.as_bytes())
            .await
            .map_err(|_| unavailable())?;
        tokio::time::timeout(
            Duration::from_secs(5),
            self.acknowledged.lock().await.recv(),
        )
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)
    }
}

#[async_trait]
impl LocalIndicator for Indicator {
    async fn show(&self, holders: &str) -> Result<(), PlatformError> {
        self.send(&show_line(holders)).await
    }
    async fn hide(&self) -> Result<(), PlatformError> {
        self.send("hide\n").await
    }
}

/// Starts the indicator. It says it is ready only once it is in the menu bar's
/// reach, so a session with no screen fails here and control stays off.
pub async fn connect() -> Result<IndicatorSession, PlatformError> {
    let unavailable =
        |why: &str| PlatformError::TemporarilyUnavailable(format!("Local Stop indicator: {why}"));
    let program = std::env::current_exe().map_err(|_| unavailable("no program to start"))?;
    let mut child = tokio::process::Command::new(program)
        .arg(INDICATOR_ROLE)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| unavailable("it could not start"))?;
    let input = child.stdin.take().ok_or_else(|| unavailable("no input"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| unavailable("no output"))?;
    let mut lines = BufReader::new(output).lines();
    match tokio::time::timeout(Duration::from_secs(5), lines.next_line()).await {
        Ok(Ok(Some(line))) if line == "ready" => {}
        _ => return Err(unavailable("this session has no menu bar")),
    }
    let (stop, events) = mpsc::channel(4);
    let (acknowledge, acknowledged) = mpsc::channel(4);
    tokio::spawn(async move {
        // Ending here closes `stop`, which says the protection has gone.
        while let Ok(Some(line)) = lines.next_line().await {
            match line.as_str() {
                "stop" => {
                    if stop.send(()).await.is_err() {
                        break;
                    }
                }
                "ok" => {
                    let _ = acknowledge.send(()).await;
                }
                _ => {}
            }
        }
    });
    Ok(IndicatorSession {
        indicator: Arc::new(Indicator {
            input: Mutex::new(input),
            acknowledged: Mutex::new(acknowledged),
            _child: Mutex::new(child),
        }),
        stop: events,
    })
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OphioLocalStop"]
    struct StopTarget;

    unsafe impl NSObjectProtocol for StopTarget {}

    impl StopTarget {
        #[unsafe(method(stopControl:))]
        fn stop_control(&self, _sender: Option<&NSObject>) {
            say("stop");
        }
    }
);

impl StopTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init, on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

/// Sends a line to Øphio. If it has gone there is no one to tell, and this
/// program is about to end too.
fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Runs as the indicator until Øphio goes away. Call on the program's first
/// thread, before anything else.
pub fn run_indicator() -> ! {
    let Some(mtm) = MainThreadMarker::new() else {
        std::process::exit(2);
    };
    let application = NSApplication::sharedApplication(mtm);
    // No Dock icon and no window: only the item in the menu bar.
    application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let target = StopTarget::new(mtm);
    let item: Retained<NSStatusItem> = {
        let bar = NSStatusBar::systemStatusBar();
        bar.statusItemWithLength(-1.0)
    };
    item.setVisible(false);
    if let Some(button) = item.button(mtm) {
        button.setTitle(&NSString::from_str(TITLE));
    }
    let menu = NSMenu::new(mtm);
    // SAFETY: the action names a method the target has, and the target lives
    // until this program ends.
    let stop = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(STOP),
            Some(sel!(stopControl:)),
            &NSString::from_str(""),
        )
    };
    // SAFETY: as above; the menu item keeps only a weak reference to the target.
    unsafe { stop.setTarget(Some(&target)) };
    menu.addItem(&stop);
    item.setMenu(Some(&menu));

    let main = SendItem(item.clone());
    std::thread::spawn(move || {
        let main = main;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let Some(request) = request(&line) else {
                continue;
            };
            let item = SendItem(main.0.clone());
            dispatch2::DispatchQueue::main().exec_async(move || {
                let item = item;
                apply(&item.0, &request);
            });
        }
        // Øphio has gone, and with it any control this stood for.
        std::process::exit(0);
    });
    say("ready");
    application.run();
    std::process::exit(0)
}

fn apply(item: &NSStatusItem, request: &Request) {
    match request {
        Request::Show(holders) => {
            let label = if holders.is_empty() {
                format!("{TITLE} in control")
            } else {
                format!("{TITLE}: {holders}")
            };
            if let Some(mtm) = MainThreadMarker::new()
                && let Some(button) = item.button(mtm)
            {
                button.setTitle(&NSString::from_str(&label));
            }
            item.setVisible(true);
        }
        Request::Hide => item.setVisible(false),
    }
    say("ok");
}

struct SendItem(Retained<NSStatusItem>);
// SAFETY: moved between threads only to reach the main queue, where it is used.
unsafe impl Send for SendItem {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_message_is_always_one_line() {
        assert_eq!(show_line("a phone"), "show a phone\n");
        assert_eq!(show_line("a\nphone\r"), "show aphone\n");
        assert_eq!(
            request(&show_line("a\nhide")),
            Some(Request::Show("ahide".into()))
        );
    }

    #[test]
    fn requests_are_read_back() {
        assert_eq!(request("hide\n"), Some(Request::Hide));
        assert_eq!(request("show x"), Some(Request::Show("x".into())));
        assert_eq!(request("something else"), None);
    }
}
