// SPDX-License-Identifier: Apache-2.0
//! Terminal sessions on the host.
//!
//! Terminal bytes use their own authenticated connection. Text WebSocket
//! frames carry [`TerminalClientFrame`] / [`TerminalServerFrame`] JSON; binary
//! frames carry raw bytes:
//!
//! * server → client: 8-byte big-endian output offset, then PTY output bytes.
//! * client → server: raw input bytes for the attached terminal.
//!
//! Offsets let a reconnecting client resume from the last byte it rendered;
//! if the host's scrollback no longer holds that offset it says so
//! (`replay_truncated`) rather than pretending the stream was continuous.

use serde::{Deserialize, Serialize};

use crate::capability::HarnessKind;
use crate::ids::{ProjectId, TerminalId};
use crate::time::Timestamp;

/// Size of the offset prefix on server→client binary terminal frames.
pub const OUTPUT_OFFSET_PREFIX: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct TerminalInfo {
    pub terminal_id: TerminalId,
    pub title: String,
    pub cwd: String,
    pub project_id: Option<ProjectId>,
    pub harness: Option<HarnessKind>,
    pub cols: u16,
    pub rows: u16,
    pub state: TerminalState,
    pub created_at: Timestamp,
    /// Total output bytes produced so far (the next output offset).
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub output_offset: u64,
    /// Last nonempty output observed by the host; absent on older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_output_at: Option<Timestamp>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export)]
pub enum TerminalState {
    Running,
    Exited { code: Option<i32> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum TerminalClientFrame {
    /// Attach to a terminal, replaying from `from_offset` if still retained.
    Attach {
        terminal_id: TerminalId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        from_offset: Option<u64>,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Detach,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum TerminalServerFrame {
    Attached {
        terminal: TerminalInfo,
        /// Offset of the first byte that will follow in binary frames.
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        replay_from: u64,
        /// True if the requested offset was older than the retained buffer.
        replay_truncated: bool,
    },
    Resized {
        cols: u16,
        rows: u16,
    },
    Exited {
        code: Option<i32>,
    },
    Error {
        message: String,
    },
}

/// Split a server→client binary terminal frame into (offset, bytes).
pub fn split_output_frame(frame: &[u8]) -> Option<(u64, &[u8])> {
    if frame.len() < OUTPUT_OFFSET_PREFIX {
        return None;
    }
    let (head, rest) = frame.split_at(OUTPUT_OFFSET_PREFIX);
    let mut buf = [0u8; OUTPUT_OFFSET_PREFIX];
    buf.copy_from_slice(head);
    Some((u64::from_be_bytes(buf), rest))
}

/// Build a server→client binary terminal frame.
pub fn output_frame(offset: u64, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(OUTPUT_OFFSET_PREFIX + bytes.len());
    out.extend_from_slice(&offset.to_be_bytes());
    out.extend_from_slice(bytes);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_frame_round_trip() {
        let f = output_frame(42, b"hello");
        assert_eq!(split_output_frame(&f), Some((42, &b"hello"[..])));
        assert_eq!(split_output_frame(&f[..3]), None);
    }

    #[test]
    fn older_terminal_records_have_no_output_time() {
        let value = serde_json::json!({
            "terminal_id": TerminalId::new(), "title": "Shell", "cwd": "/tmp",
            "project_id": null, "harness": null, "cols": 80, "rows": 24,
            "state": {"state": "running"}, "created_at": 1000, "output_offset": 42
        });
        let mut info: TerminalInfo = serde_json::from_value(value).unwrap();
        assert_eq!(info.last_output_at, None);
        info.last_output_at = Some(Timestamp(2000));
        let wire = serde_json::to_value(&info).unwrap();
        assert_eq!(wire["last_output_at"], 2000);
        assert_eq!(serde_json::from_value::<TerminalInfo>(wire).unwrap(), info);
    }
}
