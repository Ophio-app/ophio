// SPDX-License-Identifier: Apache-2.0
//! Voice interpretation does not confer input authority. The host owns fencing.
pub mod apps;
mod fenced;
mod interpreter;
pub mod keys;
pub use fenced::FencedDriver;
pub mod policy;
pub mod session;
pub mod thresholds;
use async_trait::async_trait;
pub use interpreter::Interpreter;
use protocol::{capability::Availability, voice::*};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("The newer transcript replaced this interpretation")]
    Superseded,
    #[error("Jev did not answer in time. Nothing was acted on")]
    Timeout,
    #[error("Jev could not interpret this command. Nothing was acted on")]
    Assessment,
    #[error("The control changed. Review a fresh picture and ask again")]
    StaleTarget,
    #[error("{0}")]
    Unavailable(String),
    #[error("The computer has not confirmed whether the action happened")]
    OutcomeUnknown,
    #[error("The control session has ended")]
    Stopped,
    #[error("TypeSafe refused the Jev key. Link Jev again on the computer: ophio jev connect")]
    KeyRefused,
    #[error(
        "An app may still open: the computer couldn’t stop its launch. Check the computer before continuing"
    )]
    LaunchUnstopped,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub elements: Vec<VoiceElement>,
    /// Which window or page the elements belong to. An action decided for
    /// one is refused once another has taken its place.
    pub context: String,
    /// Why no controls could be read, when none could. Commands that need
    /// no control (open an app, press keys, type, scroll) still work.
    pub unreadable: Option<String>,
    /// The focused window's bounds, in the desktop's coordinates, when the
    /// desktop says: scrolling goes there rather than to the screen's centre.
    pub window: Option<VoiceBounds>,
    /// The apps a command may open, or why none can be.
    pub apps: Result<Vec<apps::LaunchApp>, String>,
    /// A desktop: keys and text with no control go to the focused window.
    /// A browser page has no such window, so they are refused there.
    pub desktop: bool,
}
impl Snapshot {
    pub fn new(mut elements: Vec<VoiceElement>, context: String) -> Self {
        elements.retain(|e| {
            !e.id.is_empty()
                && e.id.len() <= 128
                && e.bounds.x.is_finite()
                && e.bounds.y.is_finite()
                && e.bounds.width.is_finite()
                && e.bounds.height.is_finite()
                && e.bounds.width > 0.0
                && e.bounds.height > 0.0
        });
        elements.sort_by_key(|e| (!e.visible, !e.focused));
        let mut seen = std::collections::HashSet::new();
        elements.retain(|e| seen.insert(e.id.clone()));
        elements.truncate(100);
        for e in &mut elements {
            e.role = bounded(&e.role, 64);
            e.name = bounded(&e.name, 256);
        }
        Self {
            elements,
            context,
            unreadable: None,
            window: None,
            apps: Err(NO_APPS.into()),
            desktop: false,
        }
    }
    /// A desktop whose controls can't be read, for the reason given.
    pub fn unreadable(context: String, reason: String) -> Self {
        let mut snapshot = Self::new(vec![], context);
        snapshot.unreadable = Some(reason);
        snapshot.desktop = true;
        snapshot
    }
    pub fn request(&self, transcript: String) -> jev::questions::VoiceRequest {
        jev::questions::VoiceRequest {
            transcript,
            controls: self
                .elements
                .iter()
                .map(|e| jev::questions::ControlLabel {
                    id: e.id.clone(),
                    role: e.role.clone(),
                    name: e.name.clone(),
                })
                .collect(),
        }
    }
    pub fn still_matches(&self, old: &Self, action: &VoiceAction) -> bool {
        // Opening an app or an address doesn't depend on what has focus.
        let focus_free = matches!(
            action.kind,
            VoiceActionKind::Launch | VoiceActionKind::Navigate
        ) && action.target.is_none();
        (focus_free || self.context == old.context)
            && action
                .target
                .as_ref()
                .is_none_or(|expected| self.elements.iter().any(|current| current == expected))
    }
}
/// Why a browser session can't open apps.
pub const NO_APPS: &str = "Opening apps works in desktop voice control";
fn bounded(s: &str, max: usize) -> String {
    let mut n = s.len().min(max);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    s[..n].to_owned()
}
#[async_trait]
pub trait Driver: Send + Sync + std::fmt::Debug {
    async fn snapshot(&self) -> Result<Snapshot, Error>;
    /// Revalidates target identity inside the native driver, immediately
    /// before delivery, and checks `authority` last of all, just before it
    /// sends or opens anything.
    async fn act(
        &self,
        action: &VoiceAction,
        snapshot: &Snapshot,
        authority: std::sync::Arc<dyn Authority>,
    ) -> Result<(), Error>;
    async fn picture(&self) -> Result<Option<String>, Error>;
    /// Serial native barrier. No previously accepted action may arrive after Ok.
    async fn barrier(&self) -> Result<(), Error>;
    /// Detaches automation and preserves the person's browser/window.
    async fn detach(&self) -> Result<(), Error>;
}
/// Whether this session may still send input, asked just before each
/// injection or launch. The host owns the answer: the lease, its deadline,
/// a local Stop, the device's permissions.
pub trait Authority: Send + Sync {
    fn check(&self) -> Result<(), Error>;
    /// Told of a launch that couldn't be confirmed stopped, just before the
    /// launcher answers [`Error::LaunchUnstopped`], with how it can be
    /// checked again. A launcher that can't check again tells nothing.
    fn unstopped(&self, _launch: std::sync::Arc<dyn ports::LaunchCheck>) {}
}
#[async_trait]
pub trait DriverFactory: Send + Sync + std::fmt::Debug {
    async fn availability(&self, target: VoiceTarget) -> Availability;
    async fn open(&self, target: VoiceTarget) -> Result<std::sync::Arc<dyn Driver>, Error>;
}
/// Native accessibility adapters expose only metadata, never field values.
#[async_trait]
pub trait Accessibility: Send + Sync + std::fmt::Debug {
    async fn snapshot(&self) -> Result<Snapshot, Error>;
    /// True means a native accessibility action was acknowledged. False asks the
    /// caller to use its existing, fenced input sink at the verified coordinates.
    async fn activate(&self, element: &VoiceElement, focus: bool) -> Result<bool, Error>;
    /// Native adapters that resolve controls asynchronously override this to
    /// check authority on the dispatch thread, after their final revalidation.
    /// The default preserves existing adapters and refuses an already stopped
    /// session before asking them to act.
    async fn activate_authorized(
        &self,
        element: &VoiceElement,
        focus: bool,
        authority: std::sync::Arc<dyn Authority>,
    ) -> Result<bool, Error> {
        authority.check()?;
        self.activate(element, focus).await
    }
}

/// The longest a [`Launcher`]'s `launch` or `open_url` may take to answer,
/// stopping a hung launch included. Voice control waits this long for an
/// answer, so a launch's own cleanup always finishes and reports.
pub const LAUNCH_LIMIT: std::time::Duration = std::time::Duration::from_millis(7000);
/// The longest one may take once its final authority check has passed: a
/// release withdraws authority first, then waits this long for a launch
/// already past that check to end.
pub const LAUNCH_AFTER_CHECK: std::time::Duration = std::time::Duration::from_millis(4500);
/// Opens installed apps and web addresses for desktop voice control.
/// Every implementation answers within [`LAUNCH_LIMIT`], and within
/// [`LAUNCH_AFTER_CHECK`] of its last authority check.
#[async_trait]
pub trait Launcher: Send + Sync + std::fmt::Debug {
    /// The apps a command may open, with the roles the desktop gives them.
    async fn apps(&self) -> Result<Vec<apps::LaunchApp>, Error>;
    /// Opens the app with this id, found again in a fresh list first. Its id
    /// is never run as a command line. `authority` is checked last, after
    /// everything is ready and just before the app's process starts.
    async fn launch(&self, app: &VoiceApp, authority: &dyn Authority) -> Result<(), Error>;
    /// Opens an http or https address in the default browser, checking
    /// `authority` just before the opener starts.
    async fn open_url(&self, url: &str, authority: &dyn Authority) -> Result<(), Error>;
    /// Opens an app with owned authority. Native adapters override this when
    /// dispatch is delayed, carrying authority into the native task so it can
    /// be checked immediately before launch rather than before queuing it.
    async fn launch_authorized(
        &self,
        app: &VoiceApp,
        authority: std::sync::Arc<dyn Authority>,
    ) -> Result<(), Error> {
        self.launch(app, authority.as_ref()).await
    }
    /// Opens an address with owned authority. Native adapters override this
    /// to carry authority through delayed native dispatch and check it just
    /// before the opener starts. Existing adapters use the borrowed method.
    async fn open_url_authorized(
        &self,
        url: &str,
        authority: std::sync::Arc<dyn Authority>,
    ) -> Result<(), Error> {
        self.open_url(url, authority.as_ref()).await
    }
}
/// A computer that can't open apps by voice yet.
#[derive(Debug)]
pub struct NoLauncher(pub &'static str);
#[async_trait]
impl Launcher for NoLauncher {
    async fn apps(&self) -> Result<Vec<apps::LaunchApp>, Error> {
        Err(Error::Unavailable(self.0.into()))
    }
    async fn launch(&self, _: &VoiceApp, _: &dyn Authority) -> Result<(), Error> {
        Err(Error::Unavailable(self.0.into()))
    }
    async fn open_url(&self, _: &str, _: &dyn Authority) -> Result<(), Error> {
        Err(Error::Unavailable(self.0.into()))
    }
}

pub fn capability(connected: jev::Connection, enabled: bool, native: Availability) -> Availability {
    if connected != jev::Connection::Connected {
        return Availability::TemporarilyUnavailable { reason: if connected == jev::Connection::LastCheckFailed { "The last Jev check failed. Check Jev access on the computer before using voice control" } else { "Link Jev on the computer to use voice control: ophio jev connect" }.into() };
    }
    if !enabled {
        return Availability::TemporarilyUnavailable {
            reason: "Voice control is turned off in Settings".into(),
        };
    }
    native
}

#[cfg(test)]
mod accessibility_tests;
#[cfg(test)]
mod tests;
