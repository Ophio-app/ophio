// SPDX-License-Identifier: Apache-2.0
//! The alerts connection: the phone's background watcher (see
//! [`protocol::alerts`]).
use crate::{
    Error, Result,
    control::SEEN_EVERY,
    error::invalid,
    server::{ServerDeps, current_caller},
    wire::{self, Wire},
};
use axum::extract::ws::Message;
use futures_util::StreamExt;
use protocol::{
    AttentionId, ErrorCode, ProtocolError,
    alerts::{Alert, AlertClientFrame, AlertServerFrame, MAX_OPEN_ALERTS, MAX_SHOWING},
    auth::Principal,
    event::Event,
};
use services::api::{Caller, Outbound};
use std::{collections::HashSet, time::Duration};
use tokio::{sync::broadcast::error::RecvError, time::Instant};
use tokio_util::sync::CancellationToken;

/// How long after an alert leaves a full list the list is sent again, so a
/// burst of answers sends it once.
const REFILL: Duration = Duration::from_secs(2);

/// What the phone was last sent.
struct Listed {
    /// The open alerts it holds.
    open: HashSet<AttentionId>,
    /// The list was full, so older open alerts were left out.
    full: bool,
    /// One left a full list, making room for one left out.
    refill: bool,
}

/// Sends the open alerts, then each one raised or resolved. Any connection
/// problem ends it; the phone reconnects and starts again from `Open`.
pub(crate) async fn run(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    shutdown: &CancellationToken,
) -> Result<()> {
    let AlertClientFrame::Watch {
        showing,
        quiet,
        reports,
    } = tokio::select! {
        _ = shutdown.cancelled() => return Ok(()),
        first = tokio::time::timeout(deps.limits.heartbeat, wire::receive(&mut wire.reader)) => first.map_err(|_| Error::Timeout)??,
    };
    if showing.len() > MAX_SHOWING {
        return Err(invalid("watch names too many alerts as showing"));
    }
    // Attached before the open list is read, so nothing falls between them.
    let mut live = deps.hub.subscribe(caller, None).await?.live;
    let (mut listed, head) = send_open(wire, deps, caller, showing, quiet, reports).await?;
    let refill = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(refill);
    let mut ping = tokio::time::interval(deps.limits.alert_keepalive);
    ping.reset();
    let mut poll = tokio::time::interval(deps.limits.revocation_poll);
    let mut answered = true;
    let mut marked_seen = deps.clock.monotonic();
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Ok(()),
            _ = poll.tick() => {
                let current = current_caller(deps, caller).await?;
                if current.grants != caller.grants {
                    return Err(ProtocolError::new(ErrorCode::PermissionDenied, "device grants changed; reconnect").into());
                }
                let now = deps.clock.monotonic();
                if let Principal::Device { device_id } = &caller.principal
                    && now.duration_since(marked_seen) >= SEEN_EVERY
                {
                    marked_seen = now;
                    deps.hub.device_seen(device_id).await;
                }
            }
            // The list names no alerts the phone shows, so the events after
            // `head` are still read: an answer to one the list leaves out may
            // be among them, journaled before the list was.
            () = &mut refill, if listed.refill => (listed, _) = send_open(wire, deps, caller, Vec::new(), quiet, reports).await?,
            _ = ping.tick() => {
                // Nothing came back since the last ping: the phone is gone.
                if !answered {
                    return Err(Error::Timeout);
                }
                answered = false;
                wire::send(&mut wire.writer, Message::Ping(Default::default()), deps.limits.write).await?;
            }
            incoming = wire.reader.next() => match incoming.ok_or(Error::Closed)?? {
                Message::Close(_) => return Ok(()),
                Message::Pong(_) => answered = true,
                Message::Ping(bytes) => {
                    answered = true;
                    wire::send(&mut wire.writer, Message::Pong(bytes), deps.limits.write).await?;
                }
                _ => return Err(invalid("the alerts connection takes no requests after watch")),
            },
            received = live.recv() => match received {
                // The person changed which alerts reach them: the fresh list
                // marks those now quiet, or leaves them out for a phone that
                // can't hold them quietly, and the phone puts them away.
                Ok(Outbound::Event(envelope)) if envelope.seq > head && matches!(envelope.event, Event::NotificationSettingsChanged {}) => {
                    (listed, _) = send_open(wire, deps, caller, Vec::new(), quiet, reports).await?;
                }
                Ok(Outbound::Event(envelope)) if envelope.seq > head => {
                    let refilling = listed.refill;
                    if let Some(frame) = change(deps, caller, &mut listed, &envelope.event, quiet, reports).await? {
                        wire::send_json(&mut wire.writer, &frame, deps.limits.write).await?;
                    }
                    if listed.refill && !refilling {
                        refill.as_mut().reset(Instant::now() + REFILL);
                    }
                }
                Ok(_) => {}
                // Too far behind to know what changed, including the
                // answers to alerts a full list leaves out: the phone
                // reconnects and names what it shows again.
                Err(RecvError::Lagged(_)) => return Ok(()),
                Err(RecvError::Closed) => return Err(Error::Closed),
            },
        }
    }
}

/// Sends the open list, naming which of `showing` are no longer open. A
/// phone that can't hold alerts quietly isn't sent those the person turned
/// off.
async fn send_open(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    showing: Vec<AttentionId>,
    quiet: bool,
    reports: bool,
) -> Result<(Listed, u64)> {
    let (mut alerts, head) = deps.hub.open_alerts(caller).await?;
    let full = alerts.len() >= MAX_OPEN_ALERTS;
    alerts.retain(|a| (quiet || !a.quiet) && (reports || a.report.is_none()));
    let listed = Listed {
        open: alerts.iter().map(|a| a.attention_id).collect(),
        full,
        refill: false,
    };
    let gone = gone(deps, caller, &listed, showing, reports).await?;
    let frame = AlertServerFrame::Open {
        alerts,
        now: deps.clock.now(),
        gone,
    };
    wire::send_json(&mut wire.writer, &frame, deps.limits.write).await?;
    Ok((listed, head))
}

/// Of the alerts the phone shows, those no longer open, which it cannot
/// tell itself when the list is full: one answered while it was away would
/// otherwise stay. Those `caller` may not see read as no longer open, as do
/// those it never could.
async fn gone(
    deps: &ServerDeps,
    caller: &Caller,
    listed: &Listed,
    showing: Vec<AttentionId>,
    reports: bool,
) -> Result<Vec<AttentionId>> {
    let mut asked = HashSet::new();
    let mut gone = Vec::new();
    for attention_id in showing {
        if listed.open.contains(&attention_id) || !asked.insert(attention_id) {
            continue;
        }
        if listed.full
            && deps
                .hub
                .alert(caller, attention_id)
                .await?
                .is_some_and(|a| reports || a.report.is_none())
        {
            continue;
        }
        gone.push(attention_id);
    }
    Ok(gone)
}

/// The frame an event means for the watcher, if any. The live events are
/// those `caller` may see.
async fn change(
    deps: &ServerDeps,
    caller: &Caller,
    listed: &mut Listed,
    event: &Event,
    quiet: bool,
    reports: bool,
) -> Result<Option<AlertServerFrame>> {
    let (attention_id, resolved) = match event {
        Event::AttentionUpserted { item } => (item.attention_id, item.resolved_at.is_some()),
        Event::AttentionResolved { attention_id } => (*attention_id, true),
        _ => return Ok(None),
    };
    if resolved {
        let listed_here = listed.open.remove(&attention_id);
        // A full list left older alerts out, and the phone may show one of
        // those from before; one leaving the list makes room for the next.
        listed.refill |= listed_here && listed.full;
        return Ok(
            (listed_here || listed.full).then_some(AlertServerFrame::Resolved { attention_id })
        );
    }
    if listed.open.contains(&attention_id) {
        return Ok(None);
    }
    let alert: Option<Alert> = deps.hub.alert(caller, attention_id).await?;
    Ok(alert
        .filter(|a| (quiet || !a.quiet) && (reports || a.report.is_none()))
        .map(|alert| {
            listed.open.insert(attention_id);
            AlertServerFrame::Raised { alert }
        }))
}
