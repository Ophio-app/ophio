// SPDX-License-Identifier: Apache-2.0
//! Whether a newer Øphio is out for this computer. Once a day the computer
//! asks the public repository for its latest release, without signing in,
//! and tells the person's phones once about each newer stable version.
//! Nothing is downloaded or installed.
use crate::time::Timestamp;
use serde::{Deserialize, Serialize};

/// The computer's word that it checks for newer versions of itself, answers
/// [`crate::Query::ReleaseCheck`] and takes
/// [`crate::Command::SetReleaseCheck`].
pub const RELEASE_CHECK_FEATURE: &str = "release_check_v1";

/// Where the computer stands with the newest release.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct ReleaseCheckView {
    /// Whether the computer checks once a day. On by default in public
    /// builds, off in personal ones.
    pub enabled: bool,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub revision: u64,
    /// The version of Øphio on this computer.
    pub installed: String,
    /// When a check last got an answer.
    pub checked_at: Option<Timestamp>,
    /// A stable release newer than the installed version, as the last check
    /// found it. None while the check is off.
    pub newer: Option<NewerRelease>,
    /// How to update this computer, in a sentence for the person; it
    /// depends on the computer's system.
    pub how_to_update: String,
    /// A check is asking now.
    #[serde(default)]
    pub checking: bool,
}

/// A newer release of Øphio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct NewerRelease {
    /// Its version, such as `0.3.0`.
    pub version: String,
    /// Its release page, with what's new and the downloads.
    pub url: String,
}

/// A version as releases are numbered: `MAJOR.MINOR.PATCH`, perhaps with a
/// leading `v`, a pre-release after `-` and build details after `+`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// A pre-release, such as `0.3.0-rc.1`: never offered as an update.
    pub pre: bool,
}

impl Version {
    /// None for anything that isn't such a version, as Semantic Versioning
    /// spells it: three numbers without leading zeros, then perhaps a
    /// pre-release and build details, each dot-separated identifiers of
    /// letters, digits and hyphens.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let (rest, build) = match text.split_once('+') {
            Some((rest, build)) => (rest, Some(build)),
            None => (text, None),
        };
        if build.is_some_and(|b| !identifiers(b, false)) {
            return None;
        }
        let (core, pre) = match rest.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (rest, None),
        };
        if pre.is_some_and(|p| !identifiers(p, true)) {
            return None;
        }
        let mut parts = core.split('.');
        let mut number = || -> Option<u64> {
            let part = parts.next()?;
            if !numeric(part) {
                return None;
            }
            part.parse().ok()
        };
        let version = Self {
            major: number()?,
            minor: number()?,
            patch: number()?,
            pre: pre.is_some(),
        };
        parts.next().is_none().then_some(version)
    }
    /// A version people are offered: not a pre-release.
    pub fn stable(&self) -> bool {
        !self.pre
    }
}

/// Digits, with no leading zero unless it is the only one.
fn numeric(part: &str) -> bool {
    !part.is_empty()
        && part.bytes().all(|b| b.is_ascii_digit())
        && !(part.len() > 1 && part.starts_with('0'))
}

/// Dot-separated, non-empty identifiers of letters, digits and hyphens; in
/// a pre-release, one of digits only has no leading zero.
fn identifiers(text: &str, pre: bool) -> bool {
    text.split('.').all(|id| {
        !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && (!pre || !id.bytes().all(|b| b.is_ascii_digit()) || numeric(id))
    })
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Version {
    /// By number; of two the same, the pre-release comes first.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.major, self.minor, self.patch, !self.pre).cmp(&(
            other.major,
            other.minor,
            other.patch,
            !other.pre,
        ))
    }
}

impl std::fmt::Display for Version {
    /// The number alone, as the notice names it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Whether the release tagged `tag` is a stable version newer than
/// `installed`: the one case the person is told of. A tag or an installed
/// version that doesn't parse is never newer.
pub fn newer_than(tag: &str, installed: &str) -> Option<Version> {
    let release = Version::parse(tag)?;
    let installed = Version::parse(installed)?;
    (release.stable() && release > installed).then_some(release)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_as_releases_are_tagged() {
        let v = |major, minor, patch, pre| Version {
            major,
            minor,
            patch,
            pre,
        };
        assert_eq!(Version::parse("0.2.0"), Some(v(0, 2, 0, false)));
        assert_eq!(Version::parse("v1.10.3"), Some(v(1, 10, 3, false)));
        assert_eq!(Version::parse("v0.3.0-rc.1"), Some(v(0, 3, 0, true)));
        assert_eq!(Version::parse("0.3.0+build.5"), Some(v(0, 3, 0, false)));
        assert_eq!(
            Version::parse("v0.3.0-rc-1.2+exp.sha-5114f85"),
            Some(v(0, 3, 0, true))
        );
        assert_eq!(Version::parse("v0.3.0+001"), Some(v(0, 3, 0, false)));
        for bad in [
            "",
            "v",
            "0.2",
            "0.2.0.1",
            "0.02.0",
            "1.x.0",
            "-1.0.0",
            "0.2.0-",
            "latest",
            " v 1.0.0",
            "1.0.0 beta",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert_eq!(v(0, 3, 0, false).to_string(), "0.3.0");
    }

    #[test]
    fn only_a_strictly_newer_stable_release_is_newer() {
        assert_eq!(newer_than("v0.3.0", "0.2.0").unwrap().to_string(), "0.3.0");
        assert!(
            newer_than("v0.10.0", "0.9.9").is_some(),
            "by number, not text"
        );
        assert!(newer_than("v1.0.0", "0.99.0").is_some());
        assert!(newer_than("v0.2.0", "0.2.0").is_none(), "the same");
        assert!(newer_than("v0.1.1", "0.2.0").is_none(), "older");
        assert!(
            newer_than("v0.3.0-rc.1", "0.2.0").is_none(),
            "a pre-release"
        );
        assert!(newer_than("nightly", "0.2.0").is_none(), "no version");
        assert!(newer_than("v0.3.0", "unknown").is_none());
        // A computer on a pre-release is offered the release it leads to.
        assert!(newer_than("v0.3.0", "0.3.0-dev").is_some());
        assert!(newer_than("v0.3.0", "0.3.1-dev").is_none());
    }
}
