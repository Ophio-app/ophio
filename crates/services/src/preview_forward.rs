// SPDX-License-Identifier: Apache-2.0
//! Bounded private byte streams. No DNS, arbitrary destination, listener, or
//! redirect handling: each stream connects to one checked registered socket.
use crate::{Caller, CompanionHub, error, internal};
use ports::sandbox::Sandbox;
use protocol::{
    ErrorCode, PreviewId, ProtocolError,
    auth::Principal,
    model::PreviewEndpoint,
    preview::{MAX_PREVIEW_CHUNK, PREVIEW_IDLE_TIMEOUT_MS, PreviewCloseReason},
};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream},
    net::TcpStream,
    sync::watch,
};

const MAX_STREAMS_PER_PRINCIPAL: usize = 16;
const MAX_STREAMS: usize = 128;
const CHECK_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(crate) struct PreviewForwards(Mutex<Vec<(Principal, Weak<watch::Sender<bool>>)>>);
impl PreviewForwards {
    fn register(&self, principal: Principal) -> Result<Arc<watch::Sender<bool>>, ProtocolError> {
        let mut active = self.0.lock().map_err(internal)?;
        active.retain(|(_, weak)| weak.strong_count() > 0);
        if active.len() >= MAX_STREAMS
            || active.iter().filter(|(p, _)| *p == principal).count() >= MAX_STREAMS_PER_PRINCIPAL
        {
            return Err(error(
                ErrorCode::LimitExceeded,
                "Too many preview connections",
            ));
        }
        let cancel = Arc::new(watch::channel(false).0);
        active.push((principal, Arc::downgrade(&cancel)));
        Ok(cancel)
    }
    pub(crate) fn disconnect(&self, principal: Principal) {
        if let Ok(active) = self.0.lock() {
            for (_, weak) in active.iter().filter(|(p, _)| *p == principal) {
                if let Some(cancel) = weak.upgrade() {
                    cancel.send_replace(true);
                }
            }
        }
    }
}

/// Resolve localhost ourselves. Numeric loopback variants stay numeric; no DNS
/// resolver or proxy environment may change where this connection goes.
pub fn loopback_socket(raw: &str) -> Result<SocketAddr, ProtocolError> {
    let url = crate::preview::validate(raw)?;
    if url.scheme() != "http" {
        return Err(error(
            ErrorCode::Unsupported,
            "Private forwarding currently requires an HTTP preview",
        ));
    }
    let ip = match url.host() {
        Some(url::Host::Domain("localhost")) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        Some(url::Host::Ipv4(ip)) if ip.is_loopback() => IpAddr::V4(ip),
        Some(url::Host::Ipv6(ip))
            if ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback()) =>
        {
            IpAddr::V6(ip)
        }
        _ => {
            return Err(error(
                ErrorCode::PathNotAllowed,
                "Only a registered loopback preview can be forwarded",
            ));
        }
    };
    Ok(SocketAddr::new(
        ip,
        url.port_or_known_default()
            .ok_or_else(|| error(ErrorCode::InvalidRequest, "Preview port missing"))?,
    ))
}

/// Fixed IPv4-then-IPv6 order, without invoking DNS or consulting proxy settings.
pub fn loopback_sockets(raw: &str) -> Result<Vec<SocketAddr>, ProtocolError> {
    let first = loopback_socket(raw)?;
    let mut addresses = vec![first];
    if crate::preview::validate(raw)?.host_str() == Some("localhost") {
        addresses.push(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            first.port(),
        ));
    }
    Ok(addresses)
}
/// A connection to the preview's server: on the loopback of the sandbox it
/// was shown from while that run lasts, else on the computer's.
async fn connect_loopback(
    raw: &str,
    sandbox: Option<Arc<dyn Sandbox>>,
) -> Result<(TcpStream, SocketAddr), ProtocolError> {
    let addresses = loopback_sockets(raw)?;
    if let Some(sandbox) = sandbox
        && let Some(found) = in_sandbox(&*sandbox, &addresses).await
    {
        return Ok(found);
    }
    for address in addresses {
        if let Ok(Ok(socket)) =
            tokio::time::timeout(Duration::from_millis(2400), TcpStream::connect(address)).await
        {
            return Ok((socket, address));
        }
    }
    Err(error(
        ErrorCode::TemporarilyUnavailable,
        "Neither registered loopback address answered",
    ))
}

/// A connection made in `sandbox`'s own network, as a Tokio stream.
pub(crate) async fn in_sandbox(
    sandbox: &dyn Sandbox,
    addresses: &[SocketAddr],
) -> Option<(TcpStream, SocketAddr)> {
    let connected = tokio::time::timeout(Duration::from_secs(5), sandbox.connect(addresses));
    let socket = connected.await.ok()?.ok()?;
    let address = socket.peer_addr().ok()?;
    socket.set_nonblocking(true).ok()?;
    Some((TcpStream::from_std(socket).ok()?, address))
}

impl CompanionHub {
    /// The sandbox of the run that showed `id`, while that run lasts.
    pub(crate) fn preview_sandbox(&self, id: PreviewId) -> Option<Arc<dyn Sandbox>> {
        let mut runs = self.inner.preview_runs.lock().ok()?;
        let run = *runs.get(&id)?;
        let sandbox = (self.inner.sandbox_runs.lock().ok()?.get(&run)).cloned();
        if sandbox.is_none() {
            runs.remove(&id);
        }
        sandbox
    }

    fn checked_preview(
        &self,
        caller: &Caller,
        id: PreviewId,
    ) -> Result<PreviewEndpoint, ProtocolError> {
        self.authorize(caller, |g| g.tasks)?;
        if !matches!(
            caller.principal,
            Principal::Device { .. } | Principal::LocalOwner
        ) {
            return Err(error(
                ErrorCode::PermissionDenied,
                "Preview forwarding requires a paired device",
            ));
        }
        let preview: PreviewEndpoint = self.read(|r| r.require("preview", id))?;
        if preview.reachable != Some(true) || preview.checked_at.is_none() {
            return Err(error(
                ErrorCode::Conflict,
                "The computer hasn’t confirmed this preview is reachable. Check it again.",
            ));
        }
        Ok(preview)
    }

    pub(crate) async fn preview_stream(
        &self,
        caller: &Caller,
        id: PreviewId,
    ) -> Result<DuplexStream, ProtocolError> {
        Ok(self.preview_connection(caller, id).await?.stream)
    }

    pub(crate) async fn preview_connection(
        &self,
        caller: &Caller,
        id: PreviewId,
    ) -> Result<crate::api::PreviewConnection, ProtocolError> {
        let preview = self.checked_preview(caller, id)?;
        loopback_sockets(&preview.url)?;
        let lifetime = self.inner.preview_forwards.register(caller.principal)?;
        let mut cancelled = lifetime.subscribe();
        let mut shutdown = self.inner.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(error(
                ErrorCode::Expired,
                "Øphio is shutting down on the computer",
            ));
        }
        let (socket, address) = tokio::select! {
            biased;
            _ = cancelled.changed() => return Err(error(ErrorCode::Expired, "Preview connection closed")),
            _ = shutdown.changed() => return Err(error(ErrorCode::Expired, "Øphio is shutting down on the computer")),
            connected = connect_loopback(&preview.url, self.preview_sandbox(id)) => connected?,
        };
        self.checked_preview(caller, id)?;
        let (phone, relay) = tokio::io::duplex(MAX_PREVIEW_CHUNK);
        let (closed_tx, closed) = watch::channel(None);
        let hub = self.clone();
        let caller = caller.clone();
        self.spawn(async move {
            let _lifetime = lifetime;
            let last_activity = Mutex::new(hub.inner.clock.monotonic());
            let (mut phone_read, mut phone_write) = tokio::io::split(relay);
            let (mut server_read, mut server_write) = socket.into_split();
            let cancel_check = cancelled.clone();
            let shutdown_check = shutdown.clone();
            let check = || -> std::io::Result<()> {
                if *cancel_check.borrow() || *shutdown_check.borrow() || hub.checked_preview(&caller, id).is_err() {
                    return Err(std::io::ErrorKind::PermissionDenied.into());
                }
                Ok(())
            };
            let activity = || { if let Ok(mut at) = last_activity.lock() { *at = hub.inner.clock.monotonic(); } };
            let traffic = async {
                tokio::select! {
                    result = relay_bytes(&mut phone_read, &mut server_write, &check, &activity) => result,
                    result = relay_bytes(&mut server_read, &mut phone_write, &check, &activity) => result,
                }
            };
            let monitor = async {
                loop {
                    tokio::time::sleep(CHECK_INTERVAL).await;
                    if hub.authorize(&caller, |g| g.tasks).is_err() { break PreviewCloseReason::DeviceRevoked; }
                    if hub.checked_preview(&caller, id).is_err() { break PreviewCloseReason::Removed; }
                    if expired(&last_activity, hub.inner.clock.monotonic()) { break PreviewCloseReason::IdleTimeout; }
                }
            };
            let reason = tokio::select! {
                biased;
                _ = cancelled.changed() => PreviewCloseReason::ControlLost,
                _ = shutdown.changed() => PreviewCloseReason::HostShutdown,
                reason = monitor => reason,
                _ = traffic => PreviewCloseReason::ServerClosed,
            };
            closed_tx.send_replace(Some(reason));
            // Dropping both halves also cancels blocked writes and upgrades.
        });
        Ok(crate::api::PreviewConnection {
            stream: phone,
            connected_address: Some(address.to_string()),
            closed,
        })
    }
}
fn expired(last: &Mutex<Instant>, now: Instant) -> bool {
    last.lock().map_or(true, |last| {
        now.saturating_duration_since(*last)
            >= Duration::from_millis(PREVIEW_IDLE_TIMEOUT_MS.into())
    })
}
async fn relay_bytes<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    check: &impl Fn() -> std::io::Result<()>,
    activity: &impl Fn(),
) -> std::io::Result<()> {
    let mut bytes = vec![0; MAX_PREVIEW_CHUNK];
    loop {
        let count = reader.read(&mut bytes).await?;
        if count == 0 {
            return writer.shutdown().await;
        }
        check()?;
        writer.write_all(&bytes[..count]).await?;
        activity();
    }
}

#[cfg(test)]
#[path = "preview_forward_tests.rs"]
mod tests;
