// SPDX-License-Identifier: Apache-2.0
//! Where the computer learns of Øphio's newest release: the public
//! repository's latest release, asked without signing in.
use async_trait::async_trait;

/// The latest published release, as the repository names it. Drafts and
/// pre-releases are never the latest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    /// Its tag, such as `v0.3.0`.
    pub tag: String,
    /// Its release page, with what's new and the downloads.
    pub url: String,
}

#[async_trait]
pub trait ReleaseSource: Send + Sync + std::fmt::Debug + 'static {
    /// The latest release; none when the repository has none. An error is
    /// the reason the question went unanswered, for the log only: offline,
    /// a timeout, a refusal. The next check asks again.
    async fn latest(&self) -> Result<Option<LatestRelease>, String>;
}
