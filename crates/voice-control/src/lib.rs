// SPDX-License-Identifier: Apache-2.0
//! Voice interpretation does not confer input authority. The host owns fencing.
mod fenced;
mod interpreter;
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
    #[error("Control is unavailable: {0}")]
    Unavailable(String),
    #[error("The computer has not confirmed whether the action happened")]
    OutcomeUnknown,
    #[error("The control session has ended")]
    Stopped,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub elements: Vec<VoiceElement>,
    pub context: String,
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
        Self { elements, context }
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
        self.context == old.context
            && action
                .target
                .as_ref()
                .is_none_or(|expected| self.elements.iter().any(|current| current == expected))
    }
}
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
    /// Revalidates target identity inside the native driver, immediately before delivery.
    async fn act(&self, action: &VoiceAction, snapshot: &Snapshot) -> Result<(), Error>;
    async fn picture(&self) -> Result<Option<String>, Error>;
    /// Serial native barrier. No previously accepted action may arrive after Ok.
    async fn barrier(&self) -> Result<(), Error>;
    /// Detaches automation and preserves the person's browser/window.
    async fn detach(&self) -> Result<(), Error>;
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
mod tests;
