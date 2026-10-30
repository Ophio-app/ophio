// SPDX-License-Identifier: Apache-2.0
use crate::{Error, Result, error::invalid};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use protocol::{ErrorCode, ProtocolError, QueryResult, ServerFrame};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

pub(crate) const MAX_JSON_BYTES: usize = 1024 * 1024;
/// Bytes of control frames the websocket holds before writing without a flush.
pub(crate) const CONTROL_WRITE_BUFFER: usize = 128 * 1024;
pub(crate) type Reader = Pin<Box<dyn Stream<Item = Result<Message>> + Send>>;
pub(crate) type Writer = Pin<Box<dyn Sink<Message, Error = Error> + Send>>;
pub(crate) struct Wire {
    pub reader: Reader,
    pub writer: Writer,
}
impl Wire {
    pub fn websocket(socket: WebSocket) -> Self {
        let (sink, stream) = socket.split();
        Self {
            reader: Box::pin(stream.map(|r| r.map_err(|_| Error::Closed))),
            writer: Box::pin(closed_on_error(sink)),
        }
    }
    #[cfg(unix)]
    pub fn local(stream: tokio::net::UnixStream) -> Self {
        use tokio_util::codec::{Framed, LinesCodec};
        let (sink, stream) =
            Framed::new(stream, LinesCodec::new_with_max_length(MAX_JSON_BYTES)).split();
        Self {
            reader: Box::pin(stream.map(|r| {
                r.map(|s| Message::Text(s.into()))
                    .map_err(|_| invalid("invalid local frame"))
            })),
            writer: Box::pin(closed_on_error(sink).with(|msg: Message| async move {
                match msg {
                    Message::Text(s) => Ok(s.to_string()),
                    _ => Err(invalid("local connection expects JSON")),
                }
            })),
        }
    }
}

/// `sink` with every error it reports read as a closed connection.
/// `sink_map_err` maps only the first and panics at the next, but a
/// connection whose write failed still tries to say why it is ending.
pub(crate) fn closed_on_error<S: Sink<T> + Unpin, T>(sink: S) -> impl Sink<T, Error = Error> {
    ClosedOnError(sink)
}
struct ClosedOnError<S>(S);
impl<S: Sink<T> + Unpin, T> Sink<T> for ClosedOnError<S> {
    type Error = Error;
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.0)
            .poll_ready(cx)
            .map_err(|_| Error::Closed)
    }
    fn start_send(mut self: Pin<&mut Self>, item: T) -> Result<()> {
        Pin::new(&mut self.0)
            .start_send(item)
            .map_err(|_| Error::Closed)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.0)
            .poll_flush(cx)
            .map_err(|_| Error::Closed)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.0)
            .poll_close(cx)
            .map_err(|_| Error::Closed)
    }
}

pub(crate) fn json<T: Serialize>(value: &T) -> Result<Message> {
    fitted(value)?.ok_or_else(|| invalid("JSON frame exceeds size limit"))
}
/// `value` as one frame, or `None` when it is larger than a frame.
pub(crate) fn fitted<T: Serialize>(value: &T) -> Result<Option<Message>> {
    let text = serde_json::to_string(value).map_err(|_| invalid("cannot encode frame"))?;
    Ok((text.len() <= MAX_JSON_BYTES).then(|| Message::Text(text.into())))
}
/// Queries change nothing, so an answer too large for one frame is refused in
/// its place and the connection outlives it.
pub(crate) fn query_result(result: QueryResult) -> Result<Message> {
    let query_id = result.query_id;
    if let Some(message) = fitted(&ServerFrame::QueryResult(result))? {
        return Ok(message);
    }
    json(&ServerFrame::QueryResult(QueryResult {
        query_id,
        result: Err(ProtocolError::new(
            ErrorCode::LimitExceeded,
            "This reply is too large to send.",
        )),
    }))
}
/// Owner requests can contain a bot token. Keep the serializer's allocation
/// protected, including failure paths, until the transport takes its own copy.
pub(crate) async fn send_owner_json<T: Serialize>(
    writer: &mut Writer,
    value: &T,
    deadline: Duration,
) -> Result<()> {
    let mut bytes = zeroize::Zeroizing::new(vec![0; MAX_JSON_BYTES]);
    let used = {
        // A fixed slice prevents secret-bearing reallocations on oversized input.
        let mut buffer = std::io::Cursor::new(bytes.as_mut_slice());
        serde_json::to_writer(&mut buffer, value)
            .map_err(|_| invalid("cannot encode bounded owner frame"))?;
        buffer.position() as usize
    };
    let text = std::str::from_utf8(&bytes[..used]).map_err(|_| invalid("cannot encode frame"))?;
    // Axum's immutable UTF-8 frame and LinesCodec's owned transport buffers
    // cannot be wiped through their public APIs.
    let frame = Message::Text(text.into());
    drop(bytes);
    send(writer, frame, deadline).await
}

pub(crate) fn parse<T: DeserializeOwned>(message: Message) -> Result<T> {
    match message {
        Message::Text(text) if text.len() <= MAX_JSON_BYTES => {
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(|_| invalid("invalid JSON frame"))?;
            protocol::integer::validate(&value).map_err(Error::from)?;
            serde_json::from_value(value).map_err(|_| invalid("invalid JSON frame"))
        }
        _ => Err(invalid("expected JSON text frame")),
    }
}
pub(crate) async fn receive<T: DeserializeOwned>(reader: &mut Reader) -> Result<T> {
    parse(reader.next().await.ok_or(Error::Closed)??)
}
pub(crate) async fn send(writer: &mut Writer, message: Message, deadline: Duration) -> Result<()> {
    tokio::time::timeout(deadline, writer.send(message))
        .await
        .map_err(|_| Error::Timeout)?
}
/// Queues a message to go out with the next `flush`. The writer may still
/// write it earlier to make room.
pub(crate) async fn feed(writer: &mut Writer, message: Message, deadline: Duration) -> Result<()> {
    tokio::time::timeout(deadline, writer.feed(message))
        .await
        .map_err(|_| Error::Timeout)?
}
pub(crate) async fn flush(writer: &mut Writer, deadline: Duration) -> Result<()> {
    tokio::time::timeout(deadline, writer.flush())
        .await
        .map_err(|_| Error::Timeout)?
}
pub(crate) async fn send_json<T: Serialize>(
    writer: &mut Writer,
    value: &T,
    deadline: Duration,
) -> Result<()> {
    send(writer, json(value)?, deadline).await
}
pub(crate) async fn deadline(clock: Arc<dyn ports::Clock>, end: Instant) {
    loop {
        let remaining = end.saturating_duration_since(clock.monotonic());
        if remaining.is_zero() {
            return;
        }
        tokio::time::sleep(remaining.min(Duration::from_millis(25))).await;
    }
}

#[cfg(test)]
mod writer_tests {
    use super::*;
    /// A connection the peer has dropped: every write fails.
    struct Dropped;
    impl Sink<Message> for Dropped {
        type Error = std::io::Error;
        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }
        fn start_send(self: Pin<&mut Self>, _: Message) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }
    }
    #[tokio::test]
    async fn a_dropped_connection_stays_closed_for_every_later_write() {
        let mut writer: Writer = Box::pin(closed_on_error(Dropped));
        // A failed write is followed by the fatal frame and the close.
        for _ in 0..3 {
            let sent = send(
                &mut writer,
                Message::Text("{}".into()),
                Duration::from_secs(1),
            )
            .await;
            assert!(matches!(sent, Err(Error::Closed)), "{sent:?}");
        }
        assert!(matches!(
            flush(&mut writer, Duration::from_secs(1)).await,
            Err(Error::Closed)
        ));
    }
}

#[cfg(test)]
mod integer_tests {
    use super::*;
    use protocol::{
        ClientFrame, Command, CommandEnvelope, Query, QueryEnvelope, QueryId,
        control::{InputAction, InputEnvelope},
        media::MediaClientFrame,
        terminal::TerminalClientFrame,
        transfer::TransferClientFrame,
    };
    fn rejected<T: DeserializeOwned>(value: impl Serialize) {
        let error = parse::<T>(json(&value).unwrap())
            .err()
            .expect("unsafe frame accepted");
        assert_eq!(error.protocol().code, protocol::ErrorCode::InvalidRequest);
    }
    #[test]
    fn every_numeric_ingress_uses_the_same_bound_including_nested_data() {
        let bad = protocol::MAX_SAFE_INTEGER + 1;
        let mut command = CommandEnvelope::new(Command::RunReadinessCheck);
        command.expected_revision = Some(bad);
        rejected::<ClientFrame>(ClientFrame::Command(Box::new(command)));
        for body in [
            Command::RequestHumanLease {
                target_id: Default::default(),
                geometry_revision: bad,
            },
            Command::TakeOver {
                target_id: Default::default(),
                geometry_revision: bad,
            },
            Command::CreateUpload {
                evidence: None,
                name: "a".into(),
                size: bad,
                sha256: "a".repeat(64),
                destination: protocol::transfer::TransferDestination::Inbox,
            },
        ] {
            rejected::<ClientFrame>(ClientFrame::Command(Box::new(CommandEnvelope::new(body))));
        }
        rejected::<ClientFrame>(ClientFrame::Query(QueryEnvelope {
            query_id: QueryId::new(),
            query: Query::TaskTimeline {
                task_id: Default::default(),
                before_seq: Some(bad),
                limit: 1,
            },
        }));
        rejected::<ClientFrame>(ClientFrame::RenewLease {
            lease_id: Default::default(),
            generation: bad,
        });
        rejected::<ClientFrame>(ClientFrame::Ping { nonce: bad });
        for field in 0..3 {
            rejected::<ClientFrame>(ClientFrame::Input(InputEnvelope {
                lease_id: Default::default(),
                generation: if field == 0 { bad } else { 1 },
                geometry_revision: if field == 1 { bad } else { 1 },
                seq: if field == 2 { bad } else { 1 },
                action: InputAction::ReleaseAll,
            }));
        }
        rejected::<MediaClientFrame>(MediaClientFrame::Ack { seq: bad });
        rejected::<TerminalClientFrame>(TerminalClientFrame::Attach {
            terminal_id: Default::default(),
            from_offset: Some(bad),
        });
        rejected::<TransferClientFrame>(TransferClientFrame::Download {
            transfer_id: Default::default(),
            offset: bad,
        });
        // The whole JSON tree is checked before typed dispatch, including
        // tuples/arrays and future optional fields, with no field-name allowlist.
        rejected::<ClientFrame>(serde_json::json!({"type":"ping","nonce":1,
            "nested":{"clip_range_ms":[1,bad],"origin":{"frame_seq":bad}}}));
        rejected::<ClientFrame>(serde_json::json!({"type":"ping","nonce":1e20}));
    }
}
