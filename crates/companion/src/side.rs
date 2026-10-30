// SPDX-License-Identifier: Apache-2.0
use crate::{
    Error, Result,
    error::invalid,
    server::{ServerDeps, current_caller},
    wire::{self, Wire},
};
use axum::extract::ws::Message;
use futures_util::StreamExt;
use protocol::{
    ErrorCode, ProtocolError,
    auth::ConnectionKind,
    media::{self, MediaClientFrame, SpeechClientFrame, SpeechServerFrame, StreamParams},
    terminal::{self, TerminalClientFrame, TerminalServerFrame},
    transfer::{MAX_CHUNK_BYTES, TransferClientFrame, TransferServerFrame},
};
use services::api::{Caller, MediaOut, SpeechOut, TerminalOut};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    shutdown: &CancellationToken,
    kind: ConnectionKind,
) -> Result<()> {
    match kind {
        ConnectionKind::Media => video(wire, deps, caller, shutdown).await,
        ConnectionKind::Terminal => terminal(wire, deps, caller, shutdown).await,
        ConnectionKind::Transfer => transfer(wire, deps, caller, shutdown).await,
        ConnectionKind::Speech => speech(wire, deps, caller, shutdown).await,
        ConnectionKind::Control | ConnectionKind::Preview | ConnectionKind::Alerts => {
            Err(invalid("wrong channel"))
        }
    }
}
async fn checked_caller(deps: &ServerDeps, caller: &Caller) -> Result<Caller> {
    let current = current_caller(deps, caller).await?;
    if current.grants != caller.grants {
        return Err(ProtocolError::new(
            ErrorCode::PermissionDenied,
            "device grants changed; reconnect",
        )
        .into());
    }
    Ok(current)
}
async fn guard(deps: &ServerDeps, caller: &Caller, shutdown: &CancellationToken) -> Result<()> {
    loop {
        tokio::select! { _ = shutdown.cancelled() => return Err(Error::Closed), _ = tokio::time::sleep(deps.limits.revocation_poll) => {} }
        checked_caller(deps, caller).await?;
    }
}
async fn first<T: serde::de::DeserializeOwned>(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    shutdown: &CancellationToken,
) -> Result<T> {
    tokio::select! {
        result = guard(deps, caller, shutdown) => { result?; Err(Error::Closed) },
        result = tokio::time::timeout(deps.limits.heartbeat, wire::receive(&mut wire.reader)) => result.map_err(|_| Error::Timeout)?,
    }
}
fn params_valid(params: StreamParams) -> Result<()> {
    if params.window == 0
        || params.window > 8
        || params.max_width == 0
        || params.max_height == 0
        || params.max_width > 8192
        || params.max_height > 8192
        || params.quality == 0
        || params.quality > 100
        || params.max_fps == 0
        || params.max_fps > 120
        || params.region.is_some_and(|r| !r.valid())
    {
        return Err(invalid("invalid media stream parameters"));
    }
    Ok(())
}
async fn video(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    shutdown: &CancellationToken,
) -> Result<()> {
    let MediaClientFrame::Open {
        target_id,
        mut params,
    } = first(wire, deps, caller, shutdown).await?
    else {
        return Err(invalid("expected media open"));
    };
    params_valid(params)?;
    let mut session = deps
        .hub
        .open_media(&checked_caller(deps, caller).await?, target_id, params)
        .await?;
    let run = async {
        let mut inflight = std::collections::VecDeque::<u64>::new();
        let mut last_seq = None;
        // Once a delta is dropped, only a full keyframe can restore the chain.
        let mut needs_keyframe = true;
        let mut pending_keyframe: Option<(u64, bytes::Bytes)> = None;
        loop {
            if inflight.len() < params.window as usize
                && let Some((seq, bytes)) = pending_keyframe.take()
            {
                wire::send(&mut wire.writer, Message::Binary(bytes), deps.limits.write).await?;
                inflight.push_back(seq);
                needs_keyframe = false;
            }
            tokio::select! {
                biased;
                message = wire.reader.next() => {
                    let message = message.ok_or(Error::Closed)??;
                    match message {
                        Message::Close(_) => return Ok(()),
                        Message::Ping(bytes) => { wire::send(&mut wire.writer, Message::Pong(bytes), deps.limits.write).await?; continue; },
                        Message::Pong(_) => continue,
                        _ => {},
                    }
                    checked_caller(deps, caller).await?;
                    match wire::parse::<MediaClientFrame>(message)? {
                        MediaClientFrame::Ack { seq } => {
                            // An ack proves display of a frame that was actually sent.
                            if !inflight.contains(&seq) { return Err(invalid("acknowledgement does not name an in-flight frame")); }
                            while inflight.front().is_some_and(|front| *front <= seq) { inflight.pop_front(); }
                            session.control.ack(seq).await;
                            if needs_keyframe && pending_keyframe.is_none() { session.control.request_keyframe().await; }
                        },
                        MediaClientFrame::Adjust { params: next } => { params_valid(next)?; params = next; needs_keyframe = true; pending_keyframe = None; session.control.adjust(params).await; },
                        MediaClientFrame::RequestKeyframe => { session.control.request_keyframe().await; },
                        MediaClientFrame::Close => return Ok(()),
                        MediaClientFrame::Open { .. } => return Err(invalid("media already opened")),
                    }
                },
                output = session.output.recv() => match output.ok_or(Error::Closed)? {
                    MediaOut::Control(frame) => {
                        if matches!(frame, media::MediaServerFrame::GeometryChanged { .. }) { needs_keyframe = true; pending_keyframe = None; }
                        let ended = matches!(frame, media::MediaServerFrame::Ended { .. });
                        wire::send_json(&mut wire.writer, &frame, deps.limits.write).await?;
                        if ended { return Ok(()); }
                    },
                    MediaOut::Frame(bytes) => {
                        if bytes.len() > 16 * 1024 * 1024 { return Err(invalid("media output exceeds frame limit")); }
                        let (header, _) = media::parse_tile_frame(&bytes).map_err(|_| invalid("invalid media output"))?;
                        if last_seq.is_some_and(|last| header.seq <= last) { return Err(invalid("media sequence is not increasing")); }
                        last_seq = Some(header.seq);
                        if inflight.len() >= params.window as usize {
                            if header.keyframe { pending_keyframe = Some((header.seq, bytes)); }
                            else { pending_keyframe = None; }
                            if !needs_keyframe { session.control.request_keyframe().await; }
                            needs_keyframe = true;
                        } else if needs_keyframe && !header.keyframe {
                            session.control.request_keyframe().await;
                        } else {
                            wire::send(&mut wire.writer, Message::Binary(bytes), deps.limits.write).await?;
                            inflight.push_back(header.seq); needs_keyframe = false;
                        }
                    }
                },
            }
        }
    };
    let result =
        tokio::select! { result = run => result, result = guard(deps, caller, shutdown) => result };
    session.control.close().await;
    result
}
async fn terminal(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    shutdown: &CancellationToken,
) -> Result<()> {
    let TerminalClientFrame::Attach {
        terminal_id,
        from_offset,
    } = first(wire, deps, caller, shutdown).await?
    else {
        return Err(invalid("expected terminal attach"));
    };
    let mut attachment = deps
        .hub
        .attach_terminal(
            &checked_caller(deps, caller).await?,
            terminal_id,
            from_offset,
        )
        .await?;
    let run = async {
        let mut accepting_input = true;
        wire::send_json(
            &mut wire.writer,
            &TerminalServerFrame::Attached {
                terminal: attachment.terminal.clone(),
                replay_from: attachment.replay_from,
                replay_truncated: attachment.replay_truncated,
            },
            deps.limits.write,
        )
        .await?;
        loop {
            tokio::select! {
                biased;
                message = wire.reader.next(), if accepting_input => {
                    checked_caller(deps, caller).await?;
                    match message.ok_or(Error::Closed)?? {
                        Message::Binary(bytes) if bytes.len() <= MAX_CHUNK_BYTES => accepting_input = terminal_input_result(attachment.input.write(&bytes).await)?,
                        Message::Close(_) => return Ok(()),
                        Message::Ping(bytes) => wire::send(&mut wire.writer, Message::Pong(bytes), deps.limits.write).await?,
                        Message::Pong(_) => {},
                        message => match wire::parse(message)? {
                            TerminalClientFrame::Resize { cols, rows } if cols > 0 && rows > 0 => accepting_input = terminal_input_result(attachment.input.resize(cols, rows).await)?,
                            TerminalClientFrame::Detach => return Ok(()),
                            _ => return Err(invalid("invalid terminal frame")),
                        },
                    }
                },
                output = attachment.output.recv() => match output.ok_or(Error::Closed)? {
                    TerminalOut::Output { offset, bytes } => {
                        for (index, chunk) in bytes.chunks(MAX_CHUNK_BYTES).enumerate() {
                            let offset = offset.checked_add((index * MAX_CHUNK_BYTES) as u64).ok_or_else(|| invalid("terminal offset overflow"))?;
                            wire::send(&mut wire.writer, Message::Binary(terminal::output_frame(offset, chunk).into()), deps.limits.write).await?;
                        }
                    },
                    TerminalOut::Resized { cols, rows } => wire::send_json(&mut wire.writer, &TerminalServerFrame::Resized { cols, rows }, deps.limits.write).await?,
                    TerminalOut::Exited { code } => { wire::send_json(&mut wire.writer, &TerminalServerFrame::Exited { code }, deps.limits.write).await?; return Ok(()); },
                }
            }
        }
    };
    let result =
        tokio::select! { result = run => result, result = guard(deps, caller, shutdown) => result };
    attachment.input.detach().await;
    result
}
// An input already in flight can meet a PTY that has just exited. Drain its
// ordered output and exit frame instead of replacing that fact with a fatal.
fn terminal_input_result(result: std::result::Result<(), ProtocolError>) -> Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(error)
            if error
                .detail
                .as_ref()
                .is_some_and(|d| d["terminal_state"] == "exited") =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

async fn transfer(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    shutdown: &CancellationToken,
) -> Result<()> {
    let request = first::<TransferClientFrame>(wire, deps, caller, shutdown).await?;
    tokio::select! {
        result = transfer_body(wire, deps, caller, request) => result,
        result = guard(deps, caller, shutdown) => result,
    }
}
async fn transfer_body(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    request: TransferClientFrame,
) -> Result<()> {
    let current = checked_caller(deps, caller).await?;
    match request {
        TransferClientFrame::Upload { transfer_id } => {
            let mut sink = deps.hub.upload(&current, transfer_id).await?;
            wire::send_json(
                &mut wire.writer,
                &TransferServerFrame::ResumeAt {
                    transfer_id,
                    offset: sink.resume_at(),
                },
                deps.limits.write,
            )
            .await?;
            loop {
                let message = tokio::time::timeout(deps.limits.upload_silence, wire.reader.next())
                    .await
                    .map_err(|_| ProtocolError::new(
                        ErrorCode::Expired,
                        "Nothing arrived for this upload for a while, so the computer let go of it.",
                    ))?
                    .ok_or(Error::Closed)??;
                checked_caller(deps, caller).await?;
                match message {
                    Message::Binary(bytes) if bytes.len() <= MAX_CHUNK_BYTES => {
                        sink.write(&bytes).await?
                    }
                    Message::Close(_) => return Ok(()),
                    Message::Ping(bytes) => {
                        wire::send(&mut wire.writer, Message::Pong(bytes), deps.limits.write)
                            .await?
                    }
                    Message::Pong(_) => {}
                    message => match wire::parse(message)? {
                        TransferClientFrame::Finish { transfer_id: id } if id == transfer_id => {
                            match sink.finish().await {
                                Ok(done) => {
                                    wire::send_json(
                                        &mut wire.writer,
                                        &TransferServerFrame::Completed {
                                            transfer_id,
                                            sha256: done.sha256,
                                            artifact_id: done.artifact_id,
                                        },
                                        deps.limits.write,
                                    )
                                    .await?
                                }
                                Err(error) => {
                                    wire::send_json(
                                        &mut wire.writer,
                                        &TransferServerFrame::Failed {
                                            transfer_id,
                                            reason: error.message,
                                        },
                                        deps.limits.write,
                                    )
                                    .await?
                                }
                            }
                            return Ok(());
                        }
                        TransferClientFrame::Cancel { transfer_id: id } if id == transfer_id => {
                            deps.hub
                                .cancel_transfer(&checked_caller(deps, caller).await?, transfer_id)
                                .await?;
                            return Ok(());
                        }
                        _ => return Err(invalid("invalid upload frame or transfer id")),
                    },
                }
            }
        }
        TransferClientFrame::Download {
            transfer_id,
            offset,
        } => {
            let mut source = deps.hub.download(&current, transfer_id, offset).await?;
            if source.offset != offset || offset > source.size {
                return Err(invalid("invalid download offset"));
            }
            wire::send_json(
                &mut wire.writer,
                &TransferServerFrame::Sending {
                    transfer_id,
                    offset,
                    size: source.size,
                    sha256: source.sha256.clone(),
                },
                deps.limits.write,
            )
            .await?;
            let mut transferred = offset;
            let mut buffer = vec![0; MAX_CHUNK_BYTES];
            while transferred < source.size {
                let limit = (source.size - transferred).min(MAX_CHUNK_BYTES as u64) as usize;
                tokio::select! {
                    biased;
                    message = wire.reader.next() => {
                        match message.ok_or(Error::Closed)?? {
                            Message::Close(_) => return Ok(()),
                            Message::Ping(bytes) => wire::send(&mut wire.writer, Message::Pong(bytes), deps.limits.write).await?,
                            Message::Pong(_) => {},
                            message => match wire::parse(message)? {
                                TransferClientFrame::Cancel { transfer_id: id } if id == transfer_id => { deps.hub.cancel_transfer(&checked_caller(deps, caller).await?, transfer_id).await?; return Ok(()); },
                                _ => return Err(invalid("invalid download frame")),
                            },
                        }
                    },
                    count = source.reader.read(&mut buffer[..limit]) => {
                        let count = count?;
                        if count == 0 { return Err(invalid("download source ended before advertised size")); }
                        wire::send(&mut wire.writer, Message::Binary(bytes::Bytes::copy_from_slice(&buffer[..count])), deps.limits.write).await?;
                        transferred += count as u64;
                    },
                }
            }
            wire::send_json(
                &mut wire.writer,
                &TransferServerFrame::Completed {
                    transfer_id,
                    sha256: source.sha256,
                    artifact_id: None,
                },
                deps.limits.write,
            )
            .await
        }
        TransferClientFrame::Cancel { transfer_id } => {
            deps.hub.cancel_transfer(&current, transfer_id).await?;
            Ok(())
        }
        TransferClientFrame::Finish { .. } => Err(invalid("expected upload or download")),
    }
}
async fn speech(
    wire: &mut Wire,
    deps: &ServerDeps,
    caller: &Caller,
    shutdown: &CancellationToken,
) -> Result<()> {
    let SpeechClientFrame::Start {
        session_id,
        language,
        keep_audio,
    } = first(wire, deps, caller, shutdown).await?
    else {
        return Err(invalid("expected speech start"));
    };
    if language.as_ref().is_some_and(|value| value.len() > 64) {
        return Err(invalid("invalid speech language"));
    }
    let mut stream = deps
        .hub
        .start_speech(
            &checked_caller(deps, caller).await?,
            session_id.to_string(),
            language,
            keep_audio,
        )
        .await?;
    let run = async {
        let mut finished_input = false;
        loop {
            tokio::select! {
                message = wire.reader.next() => {
                    checked_caller(deps, caller).await?;
                    match message.ok_or(Error::Closed)?? {
                        Message::Binary(bytes) if !finished_input && bytes.len() <= 32_000 && bytes.len() % 2 == 0 => stream.input.push(&bytes).await?,
                        Message::Close(_) => return Ok(false),
                        Message::Ping(bytes) => wire::send(&mut wire.writer, Message::Pong(bytes), deps.limits.write).await?,
                        Message::Pong(_) => {},
                        message => match wire::parse(message)? {
                            SpeechClientFrame::Finish if !finished_input => { finished_input = true; stream.input.finish().await; },
                            SpeechClientFrame::Cancel => return Ok(false),
                            _ => return Err(invalid("invalid speech frame")),
                        },
                    }
                },
                output = stream.output.recv() => {
                    let frame = match output.ok_or(Error::Closed)? {
                        SpeechOut::Ready { engine } => SpeechServerFrame::Ready { session_id, engine },
                        SpeechOut::Segment(segment) => SpeechServerFrame::Segment { segment },
                        SpeechOut::Finished { audio_artifact } => { wire::send_json(&mut wire.writer, &SpeechServerFrame::Finished { session_id, audio_artifact }, deps.limits.write).await?; return Ok(true); },
                        SpeechOut::Error { message } => { wire::send_json(&mut wire.writer, &SpeechServerFrame::Error { message }, deps.limits.write).await?; return Ok(false); },
                    };
                    wire::send_json(&mut wire.writer, &frame, deps.limits.write).await?;
                },
            }
        }
    };
    let result: Result<bool> = tokio::select! { result = run => result, result = guard(deps, caller, shutdown) => result.map(|_| false) };
    if !matches!(result, Ok(true)) {
        stream.input.cancel().await;
    }
    result.map(|_| ())
}
