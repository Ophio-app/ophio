// SPDX-License-Identifier: Apache-2.0
//! Pairing and connection authentication.
//!
//! Reachability (Tailscale or LAN) is not authorization. A phone becomes a
//! principal only through a short-lived pairing ceremony started on the host,
//! after which it holds an Ed25519 device key protected by the phone OS
//! keystore. Every connection is TLS to the companion certificate whose SHA-256
//! fingerprint was pinned during pairing, followed by a challenge signed with
//! the device key and bound to the TLS session via an exporter value.
//!
//! A pairing request enrolls nothing by itself. The companion holds it until
//! the local owner confirms it at the computer, after comparing the
//! [`pairing_code`] both screens show.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::capability::HostCapabilities;
use crate::ids::{BootEpoch, ChannelIdentityId, DeviceId, HostId, ProjectId};
use crate::time::Timestamp;
use crate::version::ProtocolVersion;

/// Contents of the pairing QR code / pasteable link shown by the companion.
/// Encoded as `<scheme>://pair?d=<base64url(json)>` by the client layer; the
/// QR code carries the same fields packed as `<scheme>://pair?c=<base32>`
/// (`companion::cli::pairing_qr_link`) when they fit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct PairingOffer {
    pub host_id: HostId,
    pub host_name: String,
    /// Candidate addresses (`host:port`), most preferred first. Typically the
    /// Tailscale MagicDNS name and tailnet IP; LAN only if the owner allowed it.
    pub addresses: Vec<String>,
    /// Lowercase hex SHA-256 of the companion's TLS certificate (DER).
    pub cert_sha256: String,
    /// One-time secret (base64url, 32 random bytes). Single use; expires.
    pub pairing_secret: String,
    pub expires_at: Timestamp,
    pub protocol: ProtocolVersion,
}

/// Sent by the phone over a TLS connection pinned to `cert_sha256`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct PairRequest {
    pub pairing_secret: String,
    /// Base64url Ed25519 public key (32 bytes).
    pub device_public_key: String,
    /// Human label, e.g. "Pixel 9". Display only.
    pub device_name: String,
    pub device_platform: String,
    pub client_version: String,
    pub protocol: ProtocolVersion,
    /// True when the app shows the [`pairing_code`] while the owner confirms
    /// at the computer. Older apps omit it and show no code.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub shows_code: bool,
    /// The device this app paired as before, when the person chose to replace
    /// that pairing. Only the owner's confirmation at the computer, which
    /// names that device, moves its permissions to the new one and removes
    /// it. Older apps omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub replaces: Option<DeviceId>,
}

/// The six digits the computer and the phone both show while the owner
/// confirms a new device at the computer.
///
/// They are derived from the certificate the phone pinned, the window's
/// one-time secret and the device key in the request. Someone else who saw
/// the pairing link knows the first two and can choose their own key, but not
/// the key a phone generates for its own attempt, so their request shows other
/// digits than the owner's phone except by a one-in-a-million chance. Every
/// field is length-prefixed, as in [`auth_transcript`].
pub fn pairing_code(cert_sha256: &str, pairing_secret: &str, device_public_key: &str) -> String {
    let mut hash = Sha256::new();
    for part in [
        b"pairing-code-v1".as_slice(),
        cert_sha256.as_bytes(),
        pairing_secret.as_bytes(),
        device_public_key.as_bytes(),
    ] {
        hash.update((part.len() as u32).to_be_bytes());
        hash.update(part);
    }
    let digest = hash.finalize();
    let mut first = [0; 8];
    first.copy_from_slice(&digest[..8]);
    format!("{:06}", u64::from_be_bytes(first) % 1_000_000)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct PairResponse {
    pub device_id: DeviceId,
    pub host_id: HostId,
    pub host_name: String,
    pub grants: Grants,
}

/// First frame on a connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Hello {
    pub supported: Vec<ProtocolVersion>,
    pub device_id: DeviceId,
    /// Base64url 32 random bytes.
    pub client_nonce: String,
    pub client_version: String,
    /// Which connection this is; each kind is authenticated independently.
    pub channel: ConnectionKind,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ConnectionKind {
    Control,
    Media,
    Terminal,
    Transfer,
    Speech,
    Preview,
    /// The phone's background alert watcher; see [`crate::alerts`].
    Alerts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Challenge {
    pub selected: ProtocolVersion,
    pub host_id: HostId,
    pub boot_epoch: BootEpoch,
    /// Base64url 32 random bytes.
    pub server_nonce: String,
}

/// Signature input (all fields length-prefixed, see `auth_transcript`):
/// `"auth-v1" | host_id | device_id | channel | client_nonce | server_nonce | tls_exporter`
/// where `tls_exporter` is 32 bytes exported with label `EXPORTER-device-auth-v1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Authenticate {
    /// Base64url Ed25519 signature (64 bytes).
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Authenticated {
    pub host_id: HostId,
    pub device_id: DeviceId,
    pub grants: Grants,
    pub capabilities: HostCapabilities,
    pub companion_version: String,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub journal_head: u64,
    /// Opaque identity of this control connection; never carried across reconnects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub control_connection_id: Option<String>,
}

/// TLS exporter label used for channel binding.
pub const TLS_EXPORTER_LABEL: &[u8] = b"EXPORTER-device-auth-v1";

/// Build the exact byte string a device signs. Each field is prefixed with its
/// length as a big-endian u32 so no field boundary is ambiguous.
pub fn auth_transcript(
    host_id: &HostId,
    device_id: &DeviceId,
    channel: ConnectionKind,
    client_nonce: &[u8],
    server_nonce: &[u8],
    tls_exporter: &[u8],
) -> Vec<u8> {
    let channel_tag: &[u8] = match channel {
        ConnectionKind::Control => b"control",
        ConnectionKind::Media => b"media",
        ConnectionKind::Terminal => b"terminal",
        ConnectionKind::Transfer => b"transfer",
        ConnectionKind::Speech => b"speech",
        ConnectionKind::Preview => b"preview",
        ConnectionKind::Alerts => b"alerts",
    };
    let parts: [&[u8]; 7] = [
        b"auth-v1",
        host_id.0.as_bytes(),
        device_id.0.as_bytes(),
        channel_tag,
        client_nonce,
        server_nonce,
        tls_exporter,
    ];
    let mut out = Vec::with_capacity(parts.iter().map(|p| p.len() + 4).sum());
    for p in parts {
        out.extend_from_slice(&(p.len() as u32).to_be_bytes());
        out.extend_from_slice(p);
    }
    out
}

/// What a principal may do. Grants are enforced by the companion on every
/// command; the UI only mirrors them.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct Grants {
    /// Watch desktop targets (read-only frames).
    pub view: bool,
    /// Request a human input lease.
    pub control_input: bool,
    /// Attach to / create terminals.
    pub terminal: bool,
    /// Create tasks, steer, stop, manage queue.
    pub tasks: bool,
    /// Resolve approvals.
    pub approvals: bool,
    /// Start AI computer-use runs.
    pub computer_use: bool,
    /// Read Library roots.
    pub library_read: bool,
    /// Write Library roots.
    pub library_write: bool,
    /// Send/receive files.
    pub transfers: bool,
    /// Pair/revoke devices, link channels, change settings.
    pub admin: bool,
    /// See and manage the computer's accounts: sign in, pause, sign out,
    /// set keys. Off for every new device; only the computer turns it on.
    #[serde(default)]
    pub accounts: bool,
}

impl Grants {
    /// Change selected permissions while preserving concurrent unrelated changes.
    pub fn apply_delta(&mut self, mask: &Self, allowed: bool) {
        if mask.view {
            self.view = allowed;
        }
        if mask.control_input {
            self.control_input = allowed;
        }
        if mask.terminal {
            self.terminal = allowed;
        }
        if mask.tasks {
            self.tasks = allowed;
        }
        if mask.approvals {
            self.approvals = allowed;
        }
        if mask.computer_use {
            self.computer_use = allowed;
        }
        if mask.library_read {
            self.library_read = allowed;
        }
        if mask.library_write {
            self.library_write = allowed;
        }
        if mask.transfers {
            self.transfers = allowed;
        }
        if mask.admin {
            self.admin = allowed;
        }
        if mask.accounts {
            self.accounts = allowed;
        }
        if allowed {
            self.view |= self.control_input;
            self.library_read |= self.library_write;
        } else {
            self.control_input &= self.view;
            self.library_write &= self.library_read;
        }
    }

    pub fn owner() -> Self {
        Self {
            view: true,
            control_input: true,
            terminal: true,
            tasks: true,
            approvals: true,
            computer_use: true,
            library_read: true,
            library_write: true,
            transfers: true,
            admin: true,
            accounts: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct DeviceRecord {
    pub device_id: DeviceId,
    pub name: String,
    pub platform: String,
    pub paired_at: Timestamp,
    pub last_seen_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
    pub grants: Grants,
    /// True for the device this listing was requested from.
    pub is_current: bool,
}

/// A chat identity linked to this host. Chat linking is a separate grant from
/// device pairing and is revoked separately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ChannelIdentity {
    pub id: ChannelIdentityId,
    pub channel: ChannelKind,
    /// Platform user/chat identifiers, as strings.
    pub external_user: String,
    pub external_chat: String,
    pub display_name: String,
    pub linked_at: Timestamp,
    pub revoked_at: Option<Timestamp>,
    pub grants: Grants,
    /// The project this chat works in, read from the companion's chat
    /// settings when listed. Stored identities leave it empty.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
}

/// A chat's request to be linked, as the owner reviews it before approving or
/// refusing its code. The user and chat ids are the platform's, as the chat
/// reported them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct PendingChannelLink {
    pub channel: ChannelKind,
    pub external_user: String,
    pub external_chat: String,
    pub display_name: String,
    /// The project the chat works in once linked, from the companion's chat
    /// settings.
    pub project_id: Option<ProjectId>,
    pub expires_at: Timestamp,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChannelKind {
    Telegram,
    Discord,
}

/// Who issued a command. Supplied by authentication, never by the payload.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum Principal {
    Device {
        device_id: DeviceId,
    },
    Channel {
        identity_id: ChannelIdentityId,
    },
    /// The local owner at the host (companion CLI / local settings).
    LocalOwner,
    /// The companion itself (scheduled queue progression, reconciliation).
    System,
}

#[cfg(test)]
mod tests {
    use super::pairing_code;

    #[test]
    fn the_pairing_code_is_six_digits_bound_to_certificate_secret_and_key() {
        let (cert, secret, key) = ("ab".repeat(32), "c2VjcmV0", "a2V5");
        let code = pairing_code(&cert, secret, key);
        assert_eq!(code, "999791");
        assert!(code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()));
        for other in [
            pairing_code(&"cd".repeat(32), secret, key),
            pairing_code(&cert, "b3RoZXI", key),
            pairing_code(&cert, secret, "b3RoZXI"),
            // Fields cannot shift into each other.
            pairing_code(&cert, "c2VjcmV0a", "2V5"),
        ] {
            assert_ne!(other, code);
        }
    }
}
