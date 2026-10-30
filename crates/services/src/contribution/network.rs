// SPDX-License-Identifier: Apache-2.0
//! A bounded smart-HTTP client. Git, shell, credential helpers and repository
//! configuration are never invoked. DNS answers are pinned into the TLS client.
use super::{Code, validate};
use async_trait::async_trait;
use std::{
    net::{IpAddr, SocketAddr},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub(super) const TRANSFER: usize = 256 * 1024 * 1024;
#[async_trait]
pub(crate) trait Fetch: std::fmt::Debug + Send + Sync {
    async fn handoff(&self, _url: &str) -> Result<Vec<u8>, Code> {
        Err(Code::AcquisitionFailed)
    }
    async fn pack(
        &self,
        repository: &str,
        commit: &str,
        received: &AtomicU64,
    ) -> Result<Vec<u8>, Code>;
}
#[derive(Debug)]
pub(super) struct Network;

// IANA IPv4/IPv6 Special-Purpose Address Registries (2026-10-10).
// Reject even globally reachable special-purpose assignments for this profile.
pub(super) fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && ((b == 0 && (c == 0 || c == 2))
                        || b == 168
                        || (b == 88 && c == 99)
                        || (b == 31 && c == 196)
                        || (b == 52 && c == 193)
                        || (b == 175 && c == 48)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            s[0] & 0xe000 == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && !(s[0] == 0x3fff && s[1] < 0x1000)
                && s[..3] != [0x2620, 0x004f, 0x8000]
        }
    }
}

pub(super) fn packet(bytes: &[u8], at: &mut usize) -> Result<Option<Vec<u8>>, Code> {
    let header = bytes
        .get(*at..at.saturating_add(4))
        .ok_or(Code::AcquisitionFailed)?;
    let n = usize::from_str_radix(
        std::str::from_utf8(header).map_err(|_| Code::AcquisitionFailed)?,
        16,
    )
    .map_err(|_| Code::AcquisitionFailed)?;
    *at += 4;
    if n == 0 {
        return Ok(None);
    }
    if !(4..=65_520).contains(&n) {
        return Err(Code::AcquisitionFailed);
    }
    let value = bytes
        .get(*at..at.saturating_add(n - 4))
        .ok_or(Code::AcquisitionFailed)?;
    *at += n - 4;
    Ok(Some(value.to_vec()))
}
fn pkt(s: &str) -> String {
    format!("{:04x}{s}", s.len() + 4)
}
pub(super) async fn body(
    mut r: reqwest::Response,
    limit: usize,
    received: &AtomicU64,
) -> Result<Vec<u8>, Code> {
    if r.status() != reqwest::StatusCode::OK
        || r.headers().contains_key(reqwest::header::CONTENT_ENCODING)
    {
        return Err(Code::AcquisitionFailed);
    }
    if r.content_length().is_some_and(|n| n > limit as u64) {
        return Err(Code::LimitExceeded);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|_| Code::AcquisitionFailed)? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(Code::LimitExceeded);
        }
        bytes.extend_from_slice(&chunk);
        received.fetch_add(chunk.len() as u64, Ordering::Relaxed);
    }
    Ok(bytes)
}
#[async_trait]
impl Fetch for Network {
    async fn handoff(&self, url: &str) -> Result<Vec<u8>, Code> {
        let client = public_client(url).await?;
        let response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| Code::AcquisitionFailed)?;
        body(
            response,
            protocol::contribution::RECORD_BYTES,
            &AtomicU64::new(0),
        )
        .await
    }

    async fn pack(
        &self,
        repository: &str,
        commit: &str,
        received: &AtomicU64,
    ) -> Result<Vec<u8>, Code> {
        let client = public_client(repository).await?;
        transfer(&client, repository, commit, received).await
    }
}

async fn public_client(address: &str) -> Result<reqwest::Client, Code> {
    let url = validate::repository(address)?;
    let host = url.host_str().ok_or(Code::UnsafeRepository)?;
    let addrs: Vec<SocketAddr> =
        tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((host, 443)))
            .await
            .map_err(|_| Code::AcquisitionFailed)?
            .map_err(|_| Code::AcquisitionFailed)?
            .take(33)
            .collect();
    if addrs.is_empty() || addrs.len() > 32 || addrs.iter().any(|a| !public(a.ip())) {
        return Err(Code::PrivateDestination);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .resolve_to_addrs(host, &addrs)
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .user_agent("ophio-contribution/1")
        .build()
        .map_err(|_| Code::AcquisitionFailed)?;
    Ok(client)
}
pub(super) async fn transfer(
    client: &reqwest::Client,
    repository: &str,
    commit: &str,
    received: &AtomicU64,
) -> Result<Vec<u8>, Code> {
    let r = client
        .get(format!("{repository}/info/refs?service=git-upload-pack"))
        .send()
        .await
        .map_err(|_| Code::AcquisitionFailed)?;
    if r.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some("application/x-git-upload-pack-advertisement")
    {
        return Err(Code::AcquisitionFailed);
    }
    let advertisement = body(r, 1024 * 1024, received).await?;
    let mut at = 0;
    if packet(&advertisement, &mut at)?.as_deref() != Some(b"# service=git-upload-pack\n")
        || packet(&advertisement, &mut at)?.is_some()
    {
        return Err(Code::AcquisitionFailed);
    }
    let first = packet(&advertisement, &mut at)?.ok_or(Code::AcquisitionFailed)?;
    let nul = first
        .iter()
        .position(|c| *c == 0)
        .ok_or(Code::AcquisitionFailed)?;
    let capabilities = &first[nul + 1..];
    let caps: Vec<_> = std::str::from_utf8(capabilities)
        .map_err(|_| Code::AcquisitionFailed)?
        .split_ascii_whitespace()
        .collect();
    if !caps.contains(&"shallow")
        || !caps.contains(&"side-band-64k")
        || caps
            .iter()
            .any(|c| c.starts_with("object-format=") && *c != "object-format=sha1")
    {
        return Err(Code::CompatibilityUnsupported);
    }
    // Only one pinned commit, depth one; no tags, history, submodules or LFS.
    let wants = format!(
        "{}{}0000{}",
        pkt(&format!(
            "want {commit} side-band-64k{}{}\n",
            if caps.contains(&"ofs-delta") {
                " ofs-delta"
            } else {
                ""
            },
            if caps.contains(&"no-progress") {
                " no-progress"
            } else {
                ""
            }
        )),
        pkt("deepen 1\n"),
        pkt("done\n")
    );
    let r = client
        .post(format!("{repository}/git-upload-pack"))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-git-upload-pack-request",
        )
        .header(
            reqwest::header::ACCEPT,
            "application/x-git-upload-pack-result",
        )
        .body(wants)
        .send()
        .await
        .map_err(|_| Code::AcquisitionFailed)?;
    if r.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some("application/x-git-upload-pack-result")
    {
        return Err(Code::AcquisitionFailed);
    }
    let response = body(r, TRANSFER - advertisement.len(), received).await?;
    unpack_response(&response)
}

pub(super) fn unpack_response(response: &[u8]) -> Result<Vec<u8>, Code> {
    let mut at = 0;
    let mut pack = Vec::new();
    let mut data_started = false;
    while at < response.len() {
        if let Some(line) = packet(response, &mut at)? {
            if line.first() == Some(&1) {
                data_started = true;
                if pack.len() + line.len() - 1 > TRANSFER {
                    return Err(Code::LimitExceeded);
                }
                pack.extend_from_slice(&line[1..]);
            } else if line.first() == Some(&2) {
                // Progress is untrusted and never enters diagnostics.
            } else if data_started || !(line == b"NAK\n" || line.starts_with(b"shallow ")) {
                return Err(Code::AcquisitionFailed);
            }
        }
    }
    if pack.is_empty() {
        return Err(Code::AcquisitionFailed);
    }
    Ok(pack)
}
