// SPDX-License-Identifier: Apache-2.0
//! The companion's application boundary.
//!
//! Every transport — phone connections, chat channel adapters, the local
//! owner CLI — reaches host state only through [`Hub`]. Transports
//! authenticate the caller and pass the resulting [`Caller`]; the hub enforces
//! grants on every call and never trusts identity carried in a payload.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};

use protocol::auth::{
    ChannelIdentity, ChannelKind, DeviceRecord, Grants, PairRequest, PairResponse, Principal,
};
use protocol::capability::HostCapabilities;
use protocol::command::{CommandEnvelope, CommandReceipt};
use protocol::control::{InputAck, InputEnvelope};
use protocol::event::{EventEnvelope, LiveEvent};
use protocol::ids::{
    BootEpoch, DeviceId, HostId, LeaseId, ProjectId, TargetId, TerminalId, TransferId,
};
use protocol::media::{MediaServerFrame, StreamParams};
use protocol::terminal::TerminalInfo;
use protocol::time::Timestamp;
use protocol::{ProtocolError, Query, QueryData};

/// An authenticated principal and the grants it holds right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub principal: Principal,
    pub grants: Grants,
}

impl Caller {
    pub fn local_owner() -> Self {
        Self {
            principal: Principal::LocalOwner,
            grants: Grants::owner(),
        }
    }
}

/// What the transport needs to verify a device's challenge signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCredential {
    pub device_id: DeviceId,
    /// Raw Ed25519 public key.
    pub public_key: [u8; 32],
    pub grants: Grants,
    pub revoked: bool,
}

/// A pairing window opened by the local owner. The transport renders the
/// [`protocol::auth::PairingOffer`] from these parts plus its own addresses
/// and certificate fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTicket {
    /// Base64url secret to embed in the offer. Only its hash is stored.
    pub secret: String,
    pub expires_at: Timestamp,
}

/// A device's pairing request as the local owner sees it at the computer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairingPrompt {
    pub device_name: String,
    pub device_platform: String,
    /// The six digits of [`protocol::auth::pairing_code`].
    pub code: String,
    /// False for apps that show no code. The owner is told, and answering
    /// never pairs it: its name alone proves nothing.
    pub shows_code: bool,
    pub expires_at: Timestamp,
    /// The paired device this one replaces, as it stands now. Confirming
    /// removes it and gives the new device its permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<DeviceRecord>,
}

/// A pairing request held until the local owner answers at the computer.
/// Dropping it withdraws the request.
#[derive(Debug)]
pub struct PairingClaim {
    /// When the window closes; an unanswered request enrolls nothing.
    pub expires_at: Timestamp,
    /// The enrollment once the owner confirms, or why there is none.
    pub answer: oneshot::Receiver<Result<PairResponse, ProtocolError>>,
}

/// Something a subscriber receives, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Outbound {
    Event(Arc<EventEnvelope>),
    Live(Arc<LiveEvent>),
}

/// Result of `subscribe`: a replay backlog followed by the live tail.
///
/// The live receiver is attached *before* the backlog is read, so the
/// transport sends `backlog`, then `caught_up { head }`, then forwards live
/// items whose event `seq` is greater than `head` (live items with a lower or
/// equal seq were already in the backlog). A lagged live receiver means the
/// subscriber fell behind; the transport must resubscribe from its last seq.
#[derive(Debug)]
pub struct Subscription {
    pub backlog: Backlog,
    pub head: u64,
    pub live: broadcast::Receiver<Outbound>,
}

#[derive(Debug)]
pub enum Backlog {
    Events(Vec<Arc<EventEnvelope>>),
    /// `after_seq` is older than retained history; client must re-snapshot.
    Gap {
        requested_from: u64,
        oldest_retained: u64,
    },
}

/// Output of a media session.
#[derive(Debug)]
pub enum MediaOut {
    /// Encoded `TDV1` tile frame (binary WebSocket frame).
    Frame(bytes::Bytes),
    Control(MediaServerFrame),
}

#[async_trait]
pub trait MediaControl: Send + Sync {
    async fn ack(&self, seq: u64);
    async fn adjust(&self, params: StreamParams);
    async fn request_keyframe(&self);
    async fn close(&self);
}

pub struct MediaSession {
    /// First item is always `MediaOut::Control(MediaServerFrame::Opened{..})`.
    pub output: mpsc::Receiver<MediaOut>,
    pub control: Arc<dyn MediaControl>,
}

impl std::fmt::Debug for MediaSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaSession").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum TerminalOut {
    Output { offset: u64, bytes: bytes::Bytes },
    Resized { cols: u16, rows: u16 },
    Exited { code: Option<i32> },
}

#[async_trait]
pub trait TerminalInput: Send + Sync {
    async fn write(&self, bytes: &[u8]) -> Result<(), ProtocolError>;
    async fn resize(&self, cols: u16, rows: u16) -> Result<(), ProtocolError>;
    /// Detach this attachment; the terminal keeps running.
    async fn detach(&self);
}

pub struct TerminalAttachment {
    pub terminal: TerminalInfo,
    pub replay_from: u64,
    pub replay_truncated: bool,
    /// Replay bytes first, then live output.
    pub output: mpsc::Receiver<TerminalOut>,
    pub input: Arc<dyn TerminalInput>,
}

impl std::fmt::Debug for TerminalAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalAttachment")
            .field("terminal", &self.terminal)
            .field("replay_from", &self.replay_from)
            .finish_non_exhaustive()
    }
}

/// Receiving side of an upload.
#[async_trait]
pub trait UploadSink: Send {
    /// Offset the client must resume from.
    fn resume_at(&self) -> u64;
    async fn write(&mut self, chunk: &[u8]) -> Result<(), ProtocolError>;
    /// Verify size and SHA-256 and publish to the destination.
    async fn finish(self: Box<Self>) -> Result<UploadDone, ProtocolError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadDone {
    pub sha256: String,
    pub artifact_id: Option<protocol::ids::ArtifactId>,
}

pub struct DownloadSource {
    pub offset: u64,
    pub size: u64,
    pub sha256: String,
    pub reader: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
}

impl std::fmt::Debug for DownloadSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadSource")
            .field("offset", &self.offset)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpeechOut {
    Ready {
        engine: String,
    },
    Segment(protocol::annotation::SpeechSegment),
    Finished {
        audio_artifact: Option<protocol::ids::ArtifactId>,
    },
    Error {
        message: String,
    },
}

#[async_trait]
pub trait SpeechInput: Send + Sync {
    /// 16 kHz mono PCM16 little-endian bytes.
    async fn push(&self, pcm: &[u8]) -> Result<(), ProtocolError>;
    async fn finish(&self);
    async fn cancel(&self);
}

pub struct SpeechStream {
    pub output: mpsc::Receiver<SpeechOut>,
    pub input: Arc<dyn SpeechInput>,
}

impl std::fmt::Debug for SpeechStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpeechStream").finish_non_exhaustive()
    }
}

/// How long a chat link code can be approved. After that the chat user asks
/// the bot again for a new one.
pub const LINK_CODE_MINUTES: u64 = 10;

/// A chat user asking to be linked; shown to the owner for approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelLinkRequest {
    pub channel: ChannelKind,
    pub external_user: String,
    pub external_chat: String,
    pub display_name: String,
}

/// Where a chat's link code stands, so its bot can tell the person who asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelLinkOutcome {
    Waiting,
    Approved,
    Refused,
}

/// Where linked chats do their work. The companion keeps this with its chat
/// settings, the same ones `ophio channels map` edits, and applies a
/// change to the running chat connectors before returning.
#[async_trait]
pub trait ChatProjects: Send + Sync {
    async fn set(
        &self,
        channel: ChannelKind,
        chat: &str,
        project: Option<ProjectId>,
    ) -> Result<(), ProtocolError>;
}

#[async_trait]
pub trait Hub: Send + Sync + 'static {
    fn host_id(&self) -> HostId;
    fn host_name(&self) -> String;
    fn boot_epoch(&self) -> BootEpoch;
    fn journal_head(&self) -> u64;
    async fn capabilities(&self) -> HostCapabilities;

    // ── Devices and pairing ────────────────────────────────────────────
    /// Open a single-use pairing window (local owner only).
    async fn open_pairing(&self, caller: &Caller) -> Result<PairingTicket, ProtocolError>;
    /// Hold a request carrying the open window's secret until the local owner
    /// answers it. One request waits at a time. `code` is what both screens
    /// show; nothing is enrolled until the owner confirms.
    async fn request_pairing(
        &self,
        request: PairRequest,
        code: String,
    ) -> Result<PairingClaim, ProtocolError>;
    /// The request waiting for the local owner, if any (local owner only).
    async fn waiting_pairing(
        &self,
        caller: &Caller,
    ) -> Result<Option<PairingPrompt>, ProtocolError>;
    /// Answer the waiting request with this code (local owner only).
    /// Confirming enrolls the device; either answer closes the window.
    async fn answer_pairing(
        &self,
        caller: &Caller,
        code: &str,
        confirm: bool,
    ) -> Result<Option<DeviceRecord>, ProtocolError>;
    async fn device_credential(&self, device_id: &DeviceId) -> Option<DeviceCredential>;
    /// Record that an authenticated connection from this device was seen.
    async fn device_seen(&self, device_id: &DeviceId);

    // ── Chat channel identities ────────────────────────────────────────
    async fn channel_identity(
        &self,
        channel: ChannelKind,
        external_user: &str,
        external_chat: &str,
    ) -> Option<ChannelIdentity>;
    /// Returns a short link code the owner approves from a paired device
    /// (`approve_channel_link`). Rate-limited per external user.
    async fn request_channel_link(
        &self,
        request: ChannelLinkRequest,
    ) -> Result<String, ProtocolError>;
    /// What the owner decided about a link code issued on `channel`. `None`
    /// once the code has expired, or when the computer holds no such code.
    async fn channel_link_outcome(
        &self,
        channel: ChannelKind,
        link_code: &str,
    ) -> Option<ChannelLinkOutcome>;

    // ── Control connection ─────────────────────────────────────────────
    async fn submit(&self, caller: &Caller, command: CommandEnvelope) -> CommandReceipt;

    /// A transport that queues commands supplies the original host receipt time
    /// so time in its queue counts toward the command's relative expiry.
    async fn submit_received(
        &self,
        caller: &Caller,
        command: CommandEnvelope,
        _received: Timestamp,
    ) -> CommandReceipt {
        self.submit(caller, command).await
    }

    /// Refuse work before preparation while preserving authorization and the
    /// durable receipt for this command ID, including any earlier admission.
    async fn reject_command(
        &self,
        _caller: &Caller,
        command: CommandEnvelope,
        error: ProtocolError,
    ) -> CommandReceipt {
        CommandReceipt {
            command_id: command.command_id,
            status: protocol::command::ReceiptStatus::Rejected,
            seq: None,
            result: None,
            error: Some(error),
        }
    }
    async fn query(&self, caller: &Caller, query: Query) -> Result<QueryData, ProtocolError>;
    /// A query asked from inside a run, through Ophio's own command line,
    /// with what the person disabled in Ophio left out (D-071).
    async fn agent_query(&self, caller: &Caller, query: Query) -> Result<QueryData, ProtocolError> {
        self.query(caller, query).await
    }
    /// Why a command asked from inside a run is refused: it would read or
    /// change a file the person disabled in Ophio.
    async fn agent_refusal(&self, _command: &protocol::Command) -> Option<ProtocolError> {
        None
    }
    async fn subscribe(
        &self,
        caller: &Caller,
        after_seq: Option<u64>,
    ) -> Result<Subscription, ProtocolError>;
    /// An in-run connection can hold owner credentials, but cannot observe
    /// attention reserved for the person who receives incoming reports.
    async fn agent_subscribe(
        &self,
        caller: &Caller,
        after_seq: Option<u64>,
    ) -> Result<Subscription, ProtocolError> {
        self.subscribe(caller, after_seq).await
    }
    /// One page of the backlog between `after_seq` and a subscription's
    /// `head`, for transports that replay it a page at a time: the events the
    /// caller may see among at most `limit` journal entries, and the sequence
    /// the next page starts after (`head` once the backlog is complete). Gaps
    /// and cursors ahead of `head` are reported as by `subscribe`.
    async fn backlog(
        &self,
        caller: &Caller,
        after_seq: u64,
        head: u64,
        limit: usize,
    ) -> Result<(Backlog, u64), ProtocolError>;
    async fn agent_backlog(
        &self,
        caller: &Caller,
        after_seq: u64,
        head: u64,
        limit: usize,
    ) -> Result<(Backlog, u64), ProtocolError> {
        self.backlog(caller, after_seq, head, limit).await
    }
    async fn input(&self, caller: &Caller, input: InputEnvelope) -> InputAck;
    async fn renew_lease(
        &self,
        caller: &Caller,
        lease_id: LeaseId,
        generation: u64,
    ) -> Result<(), ProtocolError>;
    /// The control connection of `caller` closed; leases it holds stop being
    /// renewed (they still expire on the arbiter's own clock).
    async fn control_disconnected(&self, caller: &Caller);

    // ── Background alerts ──────────────────────────────────────────────
    /// Every alert open for `caller`, oldest first and at most
    /// [`protocol::alerts::MAX_OPEN_ALERTS`] of the newest, with the journal
    /// head they were read at. A live event with a higher seq is newer.
    async fn open_alerts(
        &self,
        _caller: &Caller,
    ) -> Result<(Vec<protocol::alerts::Alert>, u64), ProtocolError> {
        Err(ProtocolError::new(
            protocol::ErrorCode::Unsupported,
            "Background alerts are unavailable",
        ))
    }
    /// One attention item as an alert, when it is open and `caller` may see it.
    async fn alert(
        &self,
        _caller: &Caller,
        _attention_id: protocol::AttentionId,
    ) -> Result<Option<protocol::alerts::Alert>, ProtocolError> {
        Err(ProtocolError::new(
            protocol::ErrorCode::Unsupported,
            "Background alerts are unavailable",
        ))
    }

    // ── Side connections ───────────────────────────────────────────────
    async fn open_media(
        &self,
        caller: &Caller,
        target_id: TargetId,
        params: StreamParams,
    ) -> Result<MediaSession, ProtocolError>;
    async fn attach_terminal(
        &self,
        caller: &Caller,
        terminal_id: TerminalId,
        from_offset: Option<u64>,
    ) -> Result<TerminalAttachment, ProtocolError>;
    async fn upload(
        &self,
        caller: &Caller,
        transfer_id: TransferId,
    ) -> Result<Box<dyn UploadSink>, ProtocolError>;
    async fn download(
        &self,
        caller: &Caller,
        transfer_id: TransferId,
        offset: u64,
    ) -> Result<DownloadSource, ProtocolError>;
    async fn cancel_transfer(
        &self,
        caller: &Caller,
        transfer_id: TransferId,
    ) -> Result<(), ProtocolError>;
    async fn start_speech(
        &self,
        caller: &Caller,
        session_id: String,
        language: Option<String>,
        keep_audio: bool,
    ) -> Result<SpeechStream, ProtocolError>;
    async fn open_preview_connection(
        &self,
        caller: &Caller,
        preview_id: protocol::PreviewId,
    ) -> Result<PreviewConnection, ProtocolError> {
        Ok(PreviewConnection {
            stream: self.open_preview(caller, preview_id).await?,
            connected_address: None,
            closed: tokio::sync::watch::channel(None).1,
        })
    }
    /// A private connection to one checked loopback preview. The transport owns
    /// this stream and must drop it when its authenticated side link closes.
    async fn open_preview(
        &self,
        _caller: &Caller,
        _preview_id: protocol::PreviewId,
    ) -> Result<tokio::io::DuplexStream, ProtocolError> {
        Err(ProtocolError::new(
            protocol::ErrorCode::Unsupported,
            "Private preview forwarding is unavailable",
        ))
    }
}

/// One non-replayable preview connection, with authoritative termination evidence.
#[derive(Debug)]
pub struct PreviewConnection {
    pub stream: tokio::io::DuplexStream,
    pub connected_address: Option<String>,
    pub closed: tokio::sync::watch::Receiver<Option<protocol::preview::PreviewCloseReason>>,
}
