// SPDX-License-Identifier: Apache-2.0
//! Backend-owned identities and bounded native operations; authority stays in services.
use crate::PlatformError;
use async_trait::async_trait;
use protocol::windows::{WindowAction, WindowCapabilities, WindowCatalog, WindowOutcome};

#[async_trait]
pub trait WindowActions: Send + Sync + std::fmt::Debug + 'static {
    async fn capabilities(&self) -> WindowCapabilities;
    async fn catalog(&self, search: &str) -> Result<WindowCatalog, PlatformError>;
    /// Must revalidate the catalog identity. Never interpret an id as a command line.
    /// Completing (including error) means no queued action remains in this adapter.
    async fn act(
        &self,
        action: &WindowAction,
        authority: std::sync::Arc<dyn WindowAuthority>,
    ) -> Result<(WindowOutcome, String), PlatformError>;
    /// The focused window, when the desktop says which it is.
    async fn focused(&self) -> Option<FocusedWindow> {
        None
    }
}

/// Rechecked by the adapter after discovery, immediately before native dispatch.
pub trait WindowAuthority: Send + Sync {
    fn check(&self) -> Result<(), PlatformError>;
    /// Told of an app launch that couldn't be confirmed stopped, just before
    /// the action answers so, with how it can be checked again.
    fn unstopped(&self, _launch: std::sync::Arc<dyn crate::LaunchCheck>) {}
}

/// The window that has keyboard focus, and where it is in the desktop's
/// logical coordinates (those accessibility points use).
#[derive(Debug, Clone, PartialEq)]
pub struct FocusedWindow {
    /// The compositor's own identity for it.
    pub id: String,
    pub app: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
