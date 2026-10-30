// SPDX-License-Identifier: Apache-2.0
//! Journal events. Every durable fact the phone renders arrives as an
//! [`EventEnvelope`] with a monotonic `seq`; clients resume with
//! `subscribe { after_seq }` and treat any gap as an explicit observation gap.
//!
//! High-rate, non-durable updates (streaming agent text) travel as
//! [`LiveEvent`]s outside the journal; the durable final form is journaled.

use serde::{Deserialize, Serialize};

use crate::auth::{ChannelIdentity, DeviceRecord};
use crate::capability::{HarnessInfo, HarnessKind, HostCapabilities};
use crate::command::CommandStage;
use crate::control::{ControlTarget, TargetControlView};
use crate::ids::*;
use crate::library::LibraryRoot;
use crate::model::{
    Approval, ArtifactMeta, AttentionItem, HostInfo, InterventionReceipt, PreviewEndpoint, Project,
    Provenance, ReadinessReport, Run, Task, TimelineItem,
};
use crate::queue::QueueView;
use crate::terminal::TerminalInfo;
use crate::time::Timestamp;
use crate::transfer::TransferRecord;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct EventEnvelope {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub seq: u64,
    pub at: Timestamp,
    pub source: EventSource,
    pub provenance: Provenance,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum EventSource {
    Companion,
    Harness { harness: HarnessKind },
    Platform,
    Channel { identity_id: ChannelIdentityId },
    Device { device_id: DeviceId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum Event {
    HostUpdated {
        host: Box<HostInfo>,
    },
    CapabilitiesChanged {
        capabilities: Box<HostCapabilities>,
    },
    HarnessesChanged {
        harnesses: Vec<HarnessInfo>,
    },

    ProjectUpserted {
        project: Project,
    },
    ProjectRemoved {
        project_id: ProjectId,
    },

    TaskUpserted {
        task: Task,
    },
    RunUpserted {
        run: Box<Run>,
    },
    TimelineAppended {
        task_id: TaskId,
        item: TimelineItem,
    },
    /// Progress of an admitted command beyond its receipt.
    CommandProgress {
        command_id: CommandId,
        stage: CommandStage,
        detail: Option<String>,
        /// Result of the effect, separate from the immutable admission receipt.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        result: Option<crate::command::CommandResult>,
        /// Typed failure after admission; absence does not imply success.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        error: Option<crate::error::ProtocolError>,
    },

    ApprovalUpserted {
        approval: Approval,
    },
    AttentionUpserted {
        item: AttentionItem,
    },
    AttentionResolved {
        attention_id: AttentionId,
    },

    TargetsChanged {
        targets: Vec<ControlTarget>,
    },
    ControlChanged {
        view: TargetControlView,
    },
    InterventionRecorded {
        task_id: Option<TaskId>,
        target_id: Option<TargetId>,
        receipt: Box<InterventionReceipt>,
    },

    QueueChanged {
        queue: QueueView,
    },

    LibraryRootsChanged {
        roots: Vec<LibraryRoot>,
    },
    /// A file under a root changed on disk (by anyone). `revision` is `None`
    /// when the file no longer exists.
    LibraryFileChanged {
        root_id: LibraryRootId,
        path: String,
        revision: Option<String>,
    },

    TerminalsChanged {
        terminals: Vec<TerminalInfo>,
    },
    TransferUpserted {
        transfer: TransferRecord,
    },
    PreviewsChanged {
        project_id: ProjectId,
        previews: Vec<PreviewEndpoint>,
    },
    /// A project's checklist was made, changed or put away; read it with
    /// `Query::Checklist`.
    ChecklistChanged {
        project_id: ProjectId,
    },
    ArtifactCreated {
        artifact: ArtifactMeta,
    },
    /// The artifact and its bytes are gone; references to it stay as they were.
    ArtifactDeleted {
        artifact_id: ArtifactId,
    },

    DevicesChanged {
        devices: Vec<DeviceRecord>,
    },
    ChannelIdentitiesChanged {
        identities: Vec<ChannelIdentity>,
    },
    ReadinessReported {
        report: ReadinessReport,
    },
    /// The computer's accounts changed; read them with `Query::Accounts`.
    /// It carries nothing, so no code or identity is journaled.
    AccountsChanged {},
    /// The computer's agent logins changed; read them with
    /// `Query::AgentLogins`.
    AgentLoginsChanged {},
    /// Which alerts reach the person changed; read them with
    /// `Query::NotificationSettings`.
    NotificationSettingsChanged {},
    /// The person's problem reports, or their settings, changed; read them
    /// with `Query::Reports`.
    ReportsChanged {},
    /// Ophio filled a login, or a code for it, for a task.
    AgentLoginUsed {
        login_id: String,
        name: String,
        task_id: TaskId,
        site: String,
        /// The owner asked to hear of each use.
        notify: bool,
    },
    /// A project's GitHub settings or repository changed; read them with
    /// `Query::GithubProject`.
    GithubChanged {
        project_id: ProjectId,
    },
    /// A pull request the agent works on moved: a check, a comment, a
    /// conflict, a merge. Read it with `Query::GithubPr`.
    PrChanged {
        project_id: ProjectId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        number: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        task_id: Option<TaskId>,
    },

    /// The companion restarted; per-boot state (leases, live streams) from
    /// earlier epochs is void.
    BootEpochChanged {
        boot_epoch: BootEpoch,
    },
}

/// Non-durable updates delivered only to live subscribers.
///
/// Tagged with `kind`, not `type`: it is carried inside
/// `ServerFrame::Live`, whose own tag is `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum LiveEvent {
    /// Append-only streaming text for an agent message identified by
    /// `message_key` within a run. The journaled `timeline_appended` with the
    /// full text supersedes the deltas.
    AgentTextDelta {
        task_id: TaskId,
        run_id: RunId,
        message_key: String,
        text: String,
    },
    /// Short-lived activity status ("Running tests…").
    ActivityStatus {
        task_id: TaskId,
        run_id: RunId,
        text: String,
    },
}
