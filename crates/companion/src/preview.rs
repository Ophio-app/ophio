// SPDX-License-Identifier: Apache-2.0
//! A side stream is bound to one live authenticated control connection.
use crate::{
    Error, Result,
    error::invalid,
    server::{ServerDeps, current_caller},
    wire::{self, Wire},
};
use axum::extract::ws::Message;
use futures_util::StreamExt;
use protocol::{ErrorCode, ProtocolError, auth::Principal, preview::*};
use services::Caller;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub(crate) struct Controls(Arc<Mutex<HashMap<String, (Caller, CancellationToken)>>>);
pub(crate) struct Control {
    pub id: String,
    controls: Controls,
    stop: CancellationToken,
}
impl Drop for Control {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Ok(mut map) = self.controls.0.lock() {
            map.remove(&self.id);
        }
    }
}
impl Controls {
    pub fn register(&self, caller: &Caller) -> Result<Control> {
        let id = protocol::CommandId::new().to_string();
        let stop = CancellationToken::new();
        self.0
            .lock()
            .map_err(|_| Error::Closed)?
            .insert(id.clone(), (caller.clone(), stop.clone()));
        Ok(Control {
            id,
            controls: self.clone(),
            stop,
        })
    }
    fn owner(&self, id: &str, caller: &Caller) -> Result<CancellationToken> {
        let denied = || {
            Error::Protocol(ProtocolError::new(
                ErrorCode::PermissionDenied,
                "A live control connection from this device is required",
            ))
        };
        if !matches!(caller.principal, Principal::Device { .. }) || !caller.grants.tasks {
            return Err(denied());
        }
        let controls = self.0.lock().map_err(|_| Error::Closed)?;
        let (owner, token) = controls.get(id).ok_or_else(denied)?;
        if owner != caller || token.is_cancelled() {
            return Err(denied());
        }
        Ok(token.clone())
    }
}

pub(crate) async fn run(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    shutdown: &CancellationToken,
    controls: &Controls,
) -> Result<()> {
    let first = tokio::select! {
        biased;
        _ = shutdown.cancelled() => return Err(Error::Closed),
        frame = tokio::time::timeout(deps.limits.authentication, wire::receive::<PreviewClientFrame>(&mut wire.reader)) => frame.map_err(|_| Error::Timeout)??,
    };
    let PreviewClientFrame::Open {
        preview_id,
        control_connection_id: Some(owner),
    } = first
    else {
        return Err(invalid("Expected preview and owning control connection"));
    };
    let current = current_caller(deps, caller).await?;
    let control = controls.owner(&owner, &current)?;
    let opening = deps.hub.open_preview_connection(&current, preview_id);
    let connection = tokio::select! {
        biased;
        _ = control.cancelled() => return close(wire, deps, Some(PreviewCloseReason::ControlLost), None).await,
        _ = shutdown.cancelled() => return close(wire, deps, Some(PreviewCloseReason::HostShutdown), None).await,
        result = opening => match result {
            Ok(connection) => connection,
            Err(error) => return close(wire, deps, None, Some(error)).await,
        }
    };
    if control.is_cancelled() {
        return close(wire, deps, Some(PreviewCloseReason::ControlLost), None).await;
    }
    wire::send_json(
        &mut wire.writer,
        &PreviewServerFrame::Opened {
            preview_id,
            idle_timeout_ms: PREVIEW_IDLE_TIMEOUT_MS,
            connected_address: connection.connected_address,
        },
        deps.limits.write,
    )
    .await?;
    let (mut from_host, mut to_host) = tokio::io::split(connection.stream);
    let activity = Arc::new(Mutex::new(deps.clock.monotonic()));
    let touch = || {
        if let Ok(mut last) = activity.lock() {
            *last = deps.clock.monotonic();
        }
    };
    // Independent pumps: a blocked upload never prevents responses or revocation.
    let incoming = async {
        loop {
            match wire.reader.next().await.ok_or(Error::Closed)?? {
                Message::Binary(bytes) if !bytes.is_empty() && bytes.len() <= MAX_PREVIEW_CHUNK => {
                    let current = current_caller(deps, caller).await?;
                    controls.owner(&owner, &current)?;
                    tokio::time::timeout(deps.limits.write, to_host.write_all(&bytes))
                        .await
                        .map_err(|_| Error::Timeout)??;
                    touch();
                }
                Message::Close(_) => return Ok(PreviewCloseReason::Closed),
                Message::Ping(_) | Message::Pong(_) => {}
                message => match wire::parse(message)? {
                    PreviewClientFrame::Close => return Ok(PreviewCloseReason::Closed),
                    PreviewClientFrame::Open { .. } => {
                        return Err(invalid("A preview stream cannot be reopened or replayed"));
                    }
                },
            }
        }
    };
    let outgoing = async {
        let mut buffer = vec![0; MAX_PREVIEW_CHUNK];
        loop {
            let count = from_host.read(&mut buffer).await?;
            if count == 0 {
                return Ok(PreviewCloseReason::ServerClosed);
            }
            wire::send(
                &mut wire.writer,
                Message::Binary(bytes::Bytes::copy_from_slice(&buffer[..count])),
                deps.limits.write,
            )
            .await?;
            touch();
        }
    };
    let monitor = async {
        loop {
            tokio::time::sleep(deps.limits.revocation_poll).await;
            let current = current_caller(deps, caller).await?;
            controls.owner(&owner, &current)?;
            if activity.lock().map_or(true, |at| {
                deps.clock.monotonic().saturating_duration_since(*at)
                    >= Duration::from_millis(PREVIEW_IDLE_TIMEOUT_MS.into())
            }) {
                return Ok(PreviewCloseReason::IdleTimeout);
            }
        }
    };
    let result: Result<PreviewCloseReason> = tokio::select! {
        biased;
        _ = control.cancelled() => Ok(PreviewCloseReason::ControlLost),
        _ = shutdown.cancelled() => Ok(PreviewCloseReason::HostShutdown),
        result = monitor => result,
        result = incoming => result,
        result = outgoing => result,
    };
    // Service records its reason before closing the byte stream.
    let reason = *connection.closed.borrow();
    match result {
        Ok(fallback) => close(wire, deps, reason.or(Some(fallback)), None).await,
        Err(error) => close(wire, deps, reason, Some(error.protocol())).await,
    }
}
async fn close(
    wire: &mut Wire,
    deps: &ServerDeps,
    reason: Option<PreviewCloseReason>,
    error: Option<ProtocolError>,
) -> Result<()> {
    wire::send_json(
        &mut wire.writer,
        &PreviewServerFrame::Closed { error, reason },
        deps.limits.write,
    )
    .await
}
