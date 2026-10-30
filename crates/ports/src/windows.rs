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
}

/// Rechecked by the adapter after discovery, immediately before native dispatch.
pub trait WindowAuthority: Send + Sync {
    fn check(&self) -> Result<(), PlatformError>;
}
