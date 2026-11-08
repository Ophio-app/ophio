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
#[derive(Default)]
struct Listed {
    /// The open alerts it holds.
    open: HashSet<AttentionId>,
    /// The list was full, so older open alerts were left out.
    full: bool,
    /// One left a full list, making room for one left out.
    refill: bool,
    /// Those of `open` it was told were handled, on whichever device, so
    /// it is told once.
    handled: HashSet<AttentionId>,
}

/// What the phone's `Watch` said it understands: alerts it may hold
/// quietly, and those about problem reports and sign-ins, which older
/// phones can't read and aren't sent. A phone that chooses by type
/// (`types`) keeps the person's choices itself, and says which of its
/// notifications were handled.
#[derive(Debug, Clone, Copy)]
struct Takes {
    quiet: bool,
    reports: bool,
    sign_in: bool,
    types: bool,
}
impl Takes {
    /// The phone can read an alert of this kind.
    fn kind(self, alert: &Alert) -> bool {
        (self.reports || alert.report.is_none()) && (self.sign_in || alert.sign_in.is_none())
    }
    /// The phone is sent this alert.
    fn alert(self, alert: &Alert) -> bool {
        (self.quiet || !alert.quiet) && self.kind(alert)
    }
}

/// Sends the open alerts, then each one raised or resolved. Any connection
/// problem ends it; the phone reconnects and starts again from `Open`.
pub(crate) async fn run(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    shutdown: &CancellationToken,
) -> Result<()> {
    let first: AlertClientFrame = tokio::select! {
        _ = shutdown.cancelled() => return Ok(()),
        first = tokio::time::timeout(deps.limits.heartbeat, wire::receive(&mut wire.reader)) => first.map_err(|_| Error::Timeout)??,
    };
    let AlertClientFrame::Watch {
        showing,
        quiet,
        reports,
        sign_in,
        types,
        handled,
    } = first
    else {
        return Err(invalid("the alerts connection starts with watch"));
    };
    if showing.len() > MAX_SHOWING {
        return Err(invalid("watch names too many alerts as showing"));
    }
    if handled.len() > MAX_SHOWING {
        return Err(invalid("watch names too many alerts as handled"));
    }
    let takes = Takes {
        quiet,
        reports,
        sign_in,
        types,
    };
    // What the phone handled while it couldn't say so is recorded before
    // the list is read, so the list already counts it, on this phone and
    // every other.
    if !handled.is_empty() {
        deps.hub.alerts_handled(caller, &handled).await?;
    }
    // Attached before the open list is read, so nothing falls between them.
    let mut live = deps.hub.subscribe(caller, None).await?.live;
    let (mut listed, head) =
        send_open(wire, deps, caller, showing, takes, &Listed::default()).await?;
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
            () = &mut refill, if listed.refill => (listed, _) = send_open(wire, deps, caller, Vec::new(), takes, &listed).await?,
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
                // Notifications this phone handled, which go from every other.
                message @ Message::Text(_) if takes.types => match wire::parse(message)? {
                    AlertClientFrame::Handled { attention_ids } if attention_ids.len() <= MAX_SHOWING => {
                        deps.hub.alerts_handled(caller, &attention_ids).await?;
                    }
                    AlertClientFrame::Handled { .. } => return Err(invalid("handled names too many alerts")),
                    AlertClientFrame::Watch { .. } => return Err(invalid("the alerts connection takes one watch")),
                },
                _ => return Err(invalid("the alerts connection takes no requests after watch")),
            },
            received = live.recv() => match received {
                // The person changed which alerts reach them: the fresh list
                // marks those now quiet, or leaves them out for a phone that
                // can't hold them quietly, and the phone puts them away.
                Ok(Outbound::Event(envelope)) if envelope.seq > head && matches!(envelope.event, Event::NotificationSettingsChanged {}) => {
                    (listed, _) = send_open(wire, deps, caller, Vec::new(), takes, &listed).await?;
                }
                Ok(Outbound::Event(envelope)) if envelope.seq > head => {
                    let refilling = listed.refill;
                    if let Some(frame) = change(deps, caller, &mut listed, &envelope.event, takes).await? {
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
/// off. `before` is what the phone was sent last on this connection, if
/// anything.
async fn send_open(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    showing: Vec<AttentionId>,
    takes: Takes,
    before: &Listed,
) -> Result<(Listed, u64)> {
    let (mut alerts, head) = deps.hub.open_alerts(caller, takes.types).await?;
    let full = alerts.len() >= MAX_OPEN_ALERTS;
    alerts.retain(|a| takes.alert(a));
    let open: HashSet<AttentionId> = alerts.iter().map(|a| a.attention_id).collect();
    let listed = Listed {
        handled: before.handled.intersection(&open).copied().collect(),
        open,
        full,
        refill: false,
    };
    let gone = gone(deps, caller, &listed, showing, takes).await?;
    let frame = AlertServerFrame::Open {
        alerts,
        now: deps.clock.now(),
        gone,
        // Only a phone that chooses by type says what it handled.
        handled: takes.types,
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
    takes: Takes,
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
                .alert(caller, attention_id, takes.types)
                .await?
                .is_some_and(|a| takes.kind(&a))
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
    takes: Takes,
) -> Result<Option<AlertServerFrame>> {
    let (attention_id, resolved, handled) = match event {
        Event::AttentionUpserted { item } => (
            item.attention_id,
            item.resolved_at.is_some(),
            item.alert_handled_at.is_some(),
        ),
        Event::AttentionResolved { attention_id } => (*attention_id, true, false),
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
        // Handled on one of the person's devices, the device that did so
        // included: the notification goes from this one too. An older phone
        // can't be told, and keeps it until the item closes.
        let tell = handled && takes.types && listed.handled.insert(attention_id);
        return Ok(tell.then_some(AlertServerFrame::Handled { attention_id }));
    }
    let alert: Option<Alert> = deps.hub.alert(caller, attention_id, takes.types).await?;
    Ok(alert.filter(|a| takes.alert(a)).map(|alert| {
        listed.open.insert(attention_id);
        AlertServerFrame::Raised { alert }
    }))
}
