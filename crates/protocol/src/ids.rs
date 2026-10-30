// SPDX-License-Identifier: Apache-2.0
//! Stable identities. All are UUIDs rendered as strings on the wire; UUIDv7 is
//! used for identities minted by the companion so they sort by creation time.
//! Identities never depend on display names, so a cosmetic rename cannot orphan
//! stored work or unpair devices.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
        #[serde(transparent)]
        #[ts(export, type = "string")]
        pub struct $name(pub Uuid);

        impl $name {
            /// Mint a new time-ordered identity.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

id_type!(
    /// One installation of the companion on one computer/user account.
    HostId
);
id_type!(
    /// One paired phone installation (its enrolled key).
    DeviceId
);
id_type!(
    /// A linked chat identity (Telegram chat/user, Discord user/channel).
    ChannelIdentityId
);
id_type!(
    /// Client-generated idempotency key for an effectful command.
    CommandId
);
id_type!(QueryId);
id_type!(ProjectId);
id_type!(TaskId);
id_type!(
    /// One dispatch of a task to a native harness session.
    RunId
);
id_type!(
    /// A harness-reported child agent/subagent under a run.
    ChildId
);
id_type!(ApprovalId);
id_type!(
    /// A capture/input target: an output (monitor) or window on a host session.
    TargetId
);
id_type!(
    /// A human or agent input lease on one target.
    LeaseId
);
id_type!(
    /// An observation session: a phone watch or a model observation grant.
    ObservationId
);
id_type!(AnnotationBundleId);
id_type!(
    /// A drawing inside an annotation bundle. Its visible label (A, B, C…) is
    /// stored separately and never recycled within a bundle.
    MarkId
);
id_type!(CommentId);
id_type!(ArtifactId);
id_type!(LibraryRootId);
id_type!(TerminalId);
id_type!(TransferId);
id_type!(QueueItemId);
id_type!(AttentionId);
id_type!(
    /// One problem report, minted by the device that made it.
    ReportId
);
id_type!(
    /// One question asked about a problem report.
    ReportQuestionId
);
id_type!(PreviewId);
id_type!(SpeechSessionId);
id_type!(MediaSessionId);
id_type!(
    /// Identity of one companion process lifetime. Leases, PTYs and pending
    /// effects from an earlier boot are never resurrected under a new one.
    BootEpoch
);
