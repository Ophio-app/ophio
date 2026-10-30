// SPDX-License-Identifier: Apache-2.0
use protocol::contribution::{
    CONTRACT, ContributionApproval, ContributionCode as Code, RECORD_BYTES,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

pub(super) const CAPABILITIES: &[&str] = &["pinned-import", "workspace-recovery", "policy-preview"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Packet {
    pub schema_version: u32,
    pub kind: String,
    pub packet_id: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub handoff_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Handoff {
    pub schema_version: u32,
    pub game_id: String,
    pub release_id: String,
    pub source: Source,
    pub policies: Vec<Policy>,
    pub compatibility: Compatibility,
    pub trust: Trust,
    pub task: Option<Task>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Source {
    pub repository_url: String,
    pub commit: String,
}
pub(super) use protocol::contribution::ContributionPolicy as Policy;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Compatibility {
    pub receiver: String,
    pub protocol_version: Option<String>,
    pub required_capabilities: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Trust {
    pub review_required: bool,
    pub isolated_workspace_required: bool,
    pub execution: String,
}
pub(super) use protocol::contribution::ContributionTaskReference as Task;

pub(super) fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
pub(super) fn hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub(super) fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
}
fn text(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 80 && !s.chars().any(char::is_control)
}

/// No URL normalization can change the reviewed identity. This narrow initial
/// profile accepts ASCII DNS names and HTTPS port 443 only, without credentials,
/// escapes, query, fragments or dot segments. Public IP checks happen at connect.
pub(super) fn repository(s: &str) -> Result<Url, Code> {
    if s.len() > 2048
        || !s.is_ascii()
        || s.bytes()
            .any(|c| c.is_ascii_control() || b"\\%?#@\" ".contains(&c))
    {
        return Err(Code::UnsafeRepository);
    }
    let u = Url::parse(s).map_err(|_| Code::UnsafeRepository)?;
    if u.scheme() != "https"
        || u.as_str() != s
        || u.port().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(Code::UnsafeRepository);
    }
    let host = u.host_str().ok_or(Code::UnsafeRepository)?;
    if host.parse::<std::net::IpAddr>().is_ok()
        || !host.contains('.')
        || host.ends_with('.')
        || host.split('.').any(|p| {
            p.is_empty()
                || p.starts_with('-')
                || p.ends_with('-')
                || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        || ["localhost", "local", "internal", "test", "invalid", "onion"]
            .iter()
            .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
    {
        return Err(Code::UnsafeRepository);
    }
    let path = u.path().strip_prefix('/').ok_or(Code::UnsafeRepository)?;
    if path.is_empty()
        || path.split('/').any(|s| {
            s.is_empty()
                || s == "."
                || s == ".."
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
    {
        return Err(Code::UnsafeRepository);
    }
    Ok(u)
}

/// A deliberately portable subset: reject Unicode (including normalization
/// collisions), Windows aliases, Git control paths and ambiguous separators.
pub(super) fn path(s: &str) -> Result<(), Code> {
    if s.is_empty() || s.len() > 512 || !s.is_ascii() || s.split('/').count() > 32 {
        return Err(Code::UnsafePath);
    }
    for part in s.split('/') {
        let lower = part.to_ascii_lowercase();
        let stem = lower.split('.').next().unwrap_or("");
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part
                .bytes()
                .any(|c| c.is_ascii_control() || b"\\:*?\"<>|~".contains(&c))
            || matches!(lower.as_str(), ".git" | ".gitmodules")
            || matches!(
                stem,
                "con"
                    | "prn"
                    | "aux"
                    | "nul"
                    | "com1"
                    | "com2"
                    | "com3"
                    | "com4"
                    | "com5"
                    | "com6"
                    | "com7"
                    | "com8"
                    | "com9"
                    | "lpt1"
                    | "lpt2"
                    | "lpt3"
                    | "lpt4"
                    | "lpt5"
                    | "lpt6"
                    | "lpt7"
                    | "lpt8"
                    | "lpt9"
            )
        {
            return Err(Code::UnsafePath);
        }
    }
    Ok(())
}

pub(super) fn decode<T: DeserializeOwned>(raw: &str) -> Result<T, Code> {
    if raw.len() > RECORD_BYTES {
        return Err(Code::LimitExceeded);
    }
    // Parse to Value first: last duplicate key wins consistently with CH-015's
    // JSON.parse rule. No validator or consumer sees a different interpretation.
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| Code::InvalidJson)?;
    fn depth(v: &serde_json::Value, d: usize) -> bool {
        d <= 16
            && match v {
                serde_json::Value::Array(a) => a.iter().all(|v| depth(v, d + 1)),
                serde_json::Value::Object(o) => o.values().all(|v| depth(v, d + 1)),
                _ => true,
            }
    }
    if !depth(&value, 0) {
        return Err(Code::LimitExceeded);
    }
    serde_json::from_value(value).map_err(|_| Code::Malformed)
}

pub(super) fn records(packet: &str, handoff: &str) -> Result<(Packet, Handoff), Code> {
    if packet.len() > RECORD_BYTES || handoff.len() > RECORD_BYTES {
        return Err(Code::LimitExceeded);
    }
    let p: Packet = decode(packet)?;
    let h: Handoff = decode(handoff)?;
    let value: serde_json::Value = decode(handoff)?;
    if value.get("task").is_some_and(serde_json::Value::is_null)
        || value
            .get("compatibility")
            .and_then(|v| v.get("protocolVersion"))
            .is_none()
    {
        return Err(Code::Malformed);
    }
    if p.schema_version != 1 || h.schema_version != 1 {
        return Err(Code::UnsupportedVersion);
    }
    if p.kind != "contribution-import"
        || !id(&p.packet_id)
        || !hex(&p.handoff_sha256, 64)
        || p.issued_at > protocol::MAX_SAFE_INTEGER
        || p.expires_at > protocol::MAX_SAFE_INTEGER
        || !id(&h.game_id)
        || !id(&h.release_id)
        || !(hex(&h.source.commit, 40) || hex(&h.source.commit, 64))
        || !(3..=8).contains(&h.policies.len())
        || h.compatibility.receiver != "ophio"
        || h.compatibility.required_capabilities.len() > 16
        || h.compatibility.required_capabilities.iter().any(|s| !id(s))
        || h.compatibility
            .required_capabilities
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != h.compatibility.required_capabilities.len()
        || h.compatibility
            .protocol_version
            .as_ref()
            .is_some_and(|s| !text(s))
        || !h.trust.review_required
        || !h.trust.isolated_workspace_required
        || h.trust.execution != "explicit-approval"
    {
        return Err(Code::Malformed);
    }
    repository(&h.source.repository_url)?;
    let mut paths = BTreeMap::new();
    let mut policies = BTreeSet::new();
    let mut roles = BTreeSet::new();
    for p in &h.policies {
        path(&p.path)?;
        if !hex(&p.sha256, 64)
            || ![
                "instructions",
                "contribution",
                "model-policy",
                "design",
                "setup",
            ]
            .contains(&p.role.as_str())
            || !policies.insert(p)
        {
            return Err(Code::Malformed);
        }
        if paths
            .insert(&p.path, &p.sha256)
            .is_some_and(|sha| sha != &p.sha256)
        {
            return Err(Code::PolicyMismatch);
        }
        roles.insert(p.role.as_str());
    }
    if !["instructions", "contribution", "model-policy"]
        .iter()
        .all(|s| roles.contains(s))
    {
        return Err(Code::Malformed);
    }
    if let Some(t) = &h.task {
        if !id(&t.id) || !text(&t.revision) {
            return Err(Code::Malformed);
        }
        if let Some(u) = &t.url {
            repository(u)?;
        }
    }
    if p.handoff_sha256 != digest(handoff) {
        return Err(Code::HandoffMismatch);
    }
    Ok((p, h))
}

pub(super) fn lifetime(p: &Packet, now: u64) -> Result<(), Code> {
    if p.expires_at <= p.issued_at || p.expires_at - p.issued_at > 86_400 {
        return Err(Code::InvalidLifetime);
    }
    if now < p.issued_at {
        return Err(Code::NotYetValid);
    }
    if now >= p.expires_at {
        return Err(Code::Expired);
    }
    Ok(())
}
pub(super) fn approval(
    p: &Packet,
    h: &Handoff,
    packet_hash: &str,
    a: &ContributionApproval,
    now: u64,
) -> Result<(), Code> {
    if a.display_name.as_ref().is_some_and(|name| {
        name.trim().is_empty()
            || name.len() > 120
            || name.chars().any(|c| {
                c.is_control()
                    || matches!(c, '/' | '\\' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
    }) {
        return Err(Code::Malformed);
    }
    if !id(&a.publisher_id) || a.packet_sha256 != packet_hash {
        return Err(Code::PacketUntrusted);
    }
    lifetime(p, now)?;
    if a.checked_at > now
        || now - a.checked_at > 300
        || a.valid_until <= now
        || a.valid_until > a.checked_at.saturating_add(300)
        || a.valid_until > p.expires_at
    {
        return Err(Code::FreshnessUnknown);
    }
    if a.handoff_sha256 != p.handoff_sha256 {
        return Err(Code::CatalogDigestMismatch);
    }
    if a.game_id != h.game_id
        || a.release_id != h.release_id
        || a.repository_url != h.source.repository_url
        || a.commit != h.source.commit
    {
        return Err(Code::IdentityMismatch);
    }
    Ok(())
}
pub(super) fn compatibility(h: &Handoff) -> Result<(), Code> {
    let Some(v) = &h.compatibility.protocol_version else {
        return Err(Code::CompatibilityUnknown);
    };
    if v != CONTRACT
        || h.source.commit.len() != 40
        || h.compatibility
            .required_capabilities
            .iter()
            .any(|c| !CAPABILITIES.contains(&c.as_str()))
    {
        return Err(Code::CompatibilityUnsupported);
    }
    Ok(())
}

/// Only validated identifiers leave the receiver; protocol/hash failures use
/// receiver-owned labels instead of echoing unsupported packet values.
pub(super) fn missing_capabilities(h: &Handoff) -> Vec<String> {
    let mut missing: Vec<_> = h
        .compatibility
        .required_capabilities
        .iter()
        .filter(|c| !CAPABILITIES.contains(&c.as_str()))
        .cloned()
        .collect();
    if h.compatibility
        .protocol_version
        .as_deref()
        .is_some_and(|v| v != CONTRACT)
    {
        missing.push("import-protocol-version".into());
    }
    if h.source.commit.len() != 40 {
        missing.push("git-sha256".into());
    }
    missing.sort();
    missing.dedup();
    missing
}
