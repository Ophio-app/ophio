// SPDX-License-Identifier: Apache-2.0
//! Private preview byte stream on an authenticated side connection. Open names
//! only a registered preview; the peer can never supply a destination address.
//! After Opened, binary frames carry at most MAX_PREVIEW_CHUNK bytes, in order.
//! One connection carries one stream, including HTTP upgrades. A transport must
//! close it on loss of the owning control connection and never replay bytes.
use crate::{PreviewId, ProtocolError};
use serde::{Deserialize, Serialize};

pub const MAX_PREVIEW_CHUNK: usize = 64 * 1024;
pub const PREVIEW_IDLE_TIMEOUT_MS: u32 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export)]
pub enum PreviewClientFrame {
    Open {
        preview_id: PreviewId,
        /// Must name a live control connection owned by this authenticated device.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        control_connection_id: Option<String>,
    },
    Close,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum PreviewServerFrame {
    Opened {
        preview_id: PreviewId,
        idle_timeout_ms: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        connected_address: Option<String>,
    },
    Closed {
        error: Option<ProtocolError>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reason: Option<PreviewCloseReason>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cannot_supply_a_destination() {
        let request = serde_json::json!({"type":"open", "preview_id":PreviewId::new(), "url":"http://127.0.0.1:9"});
        assert!(serde_json::from_value::<PreviewClientFrame>(request).is_err());
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PreviewCloseReason {
    Closed,
    Removed,
    DeviceRevoked,
    IdleTimeout,
    ControlLost,
    HostShutdown,
    ServerClosed,
}
