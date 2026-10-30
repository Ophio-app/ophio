// SPDX-License-Identifier: Apache-2.0
use crate::BridgeError;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

// Images may be several MiB. Bound allocation before reading an entire line.
pub(crate) const MAX_LINE: usize = 32 * 1024 * 1024;

pub(crate) async fn line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, BridgeError> {
    let mut bytes = Vec::new();
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(BridgeError::Protocol)
            };
        }
        let end = buf.iter().position(|b| *b == b'\n');
        let count = end.map_or(buf.len(), |n| n + 1);
        if bytes.len() + count > MAX_LINE {
            return Err(BridgeError::Protocol);
        }
        bytes.extend_from_slice(&buf[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(bytes));
        }
    }
}

pub(crate) async fn write<W: AsyncWrite + Unpin, T: serde::Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), BridgeError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| BridgeError::Protocol)?;
    if bytes.len() >= MAX_LINE {
        return Err(BridgeError::Protocol);
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
