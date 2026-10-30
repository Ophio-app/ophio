// SPDX-License-Identifier: Apache-2.0
//! Point-and-ask annotation bundles.
//!
//! A bundle annotates one or more explicitly captured sources (never a moving
//! live view). Drawings have stable identities and labels (A, B, C…) that are
//! never recycled within a bundle. Comments reference any set of drawings, so
//! one comment can address several drawings and one drawing can have several
//! comments. Speech is optional: transcript segments suggest links, and manual
//! links/edits always override the suggestion.

use serde::{Deserialize, Serialize};

use crate::ids::{
    AnnotationBundleId, ArtifactId, CommentId, HostId, MarkId, PreviewId, SpeechSessionId, TargetId,
};
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AnnotationBundle {
    pub bundle_id: AnnotationBundleId,
    /// Immutable once sent; a later correction is a new revision.
    pub revision: u32,
    pub sources: Vec<AnnotationSource>,
    pub marks: Vec<Mark>,
    pub comments: Vec<Comment>,
    /// Ordered reproduction steps, each pointing at a source (and optionally a
    /// clip time range).
    pub steps: Vec<ReproStep>,
    /// Free-text instruction that applies to the whole bundle.
    pub general_note: Option<String>,
    pub speech: Option<SpeechTrack>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct AnnotationSource {
    /// Index-stable key within the bundle ("S1", "S2"…), shown to people.
    pub key: String,
    pub artifact_id: ArtifactId,
    /// Lowercase hex SHA-256 of the exact bytes reviewed and sent.
    pub sha256: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub origin: SourceOrigin,
    pub captured_at: Timestamp,
    /// When derived (crop/trim/redaction), the reviewed parent source key.
    pub derived_from: Option<String>,
    pub derivation: Option<Derivation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum SourceOrigin {
    /// Captured from a host target at a geometry revision.
    HostCapture {
        target_id: TargetId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        geometry_revision: u64,
    },
    /// A frame taken from the live view the person was watching (with the
    /// frame sequence that was displayed).
    LiveFrame {
        target_id: TargetId,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        geometry_revision: u64,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        frame_seq: u64,
    },
    /// Imported on the phone (screenshot/share). Not attributed to a target.
    PhoneImport { original_name: Option<String> },
    /// A still taken through this device's connection to a named computer preview.
    PreviewCapture {
        capture_id: String,
        host_id: HostId,
        computer_name: String,
        preview_id: PreviewId,
        preview_label: String,
        /// Native recovery may retain the picture without its capture clock.
        captured_at: Option<Timestamp>,
    },
    /// Something an agent showed the person (`Shown::Media`), or a frame of it.
    Shown { artifact_id: ArtifactId },
    /// A recording clip (reproduction), with duration.
    Clip {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        duration_ms: u64,
        target_id: Option<TargetId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum Derivation {
    Crop {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    },
    Trim {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        start_ms: u64,
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        end_ms: u64,
    },
    /// Regions blacked out, normalized to the parent.
    Redact {
        regions: Vec<NormRect>,
    },
    FrameFromClip {
        #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
        at_ms: u64,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct NormRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Mark {
    pub mark_id: MarkId,
    /// Visible label, e.g. "A". Never reassigned inside a bundle.
    pub label: String,
    pub source_key: String,
    pub shape: MarkShape,
    /// Palette index; colors are presentation only.
    pub color: u8,
    pub created_at: Timestamp,
    /// Deleted marks are kept so that comments pointing at them surface as
    /// unresolved rather than silently re-targeting.
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum MarkShape {
    /// Freehand stroke; points normalized to the source (0..1), with optional
    /// capture timestamps (ms since bundle start) for speech alignment.
    Stroke {
        points: Vec<StrokePoint>,
    },
    Rect {
        rect: NormRect,
    },
    Ellipse {
        rect: NormRect,
    },
    Arrow {
        from: StrokePoint,
        to: StrokePoint,
    },
    Pin {
        at: StrokePoint,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[ts(export)]
pub struct StrokePoint {
    pub x: f64,
    pub y: f64,
    /// Milliseconds since the bundle's drawing clock started.
    pub t: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Comment {
    pub comment_id: CommentId,
    pub text: String,
    /// Drawings this comment is about. Empty = general comment.
    pub mark_ids: Vec<MarkId>,
    pub origin: CommentOrigin,
    /// Link provenance for each mark id, parallel to `mark_ids`.
    pub link_origins: Vec<LinkOrigin>,
    /// Said while watching a video: where in the video, and a still from then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub moment: Option<VideoMoment>,
}

/// The part of a video a comment was said over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct VideoMoment {
    /// The bundle source key of a frame from `start_ms`.
    pub still: String,
    /// Position in the video, in milliseconds.
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub start_ms: u64,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub end_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum CommentOrigin {
    Typed,
    /// From speech segments (possibly edited afterwards).
    Spoken {
        segment_ids: Vec<String>,
        edited: bool,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LinkOrigin {
    /// The person selected the drawing.
    Manual,
    /// Suggested from speech/drawing timing and accepted by the person.
    SuggestedAccepted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ReproStep {
    pub index: u32,
    pub source_key: String,
    pub text: String,
    #[schemars(with = "Option<(crate::integer::SafeInteger, crate::integer::SafeInteger)>")]
    pub clip_range_ms: Option<(u64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct SpeechTrack {
    pub session_id: SpeechSessionId,
    /// Retained audio artifact, only if the person chose to keep it.
    pub audio_artifact: Option<ArtifactId>,
    pub segments: Vec<SpeechSegment>,
    pub transcript_revision: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct SpeechSegment {
    pub segment_id: String,
    pub text: String,
    /// Approximate span on the drawing clock. Timing is a hint only.
    pub start_ms: u32,
    pub end_ms: u32,
    pub revision: u32,
    pub is_final: bool,
}

/// Link suggestions computed from speech/drawing timing. Advisory only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct LinkSuggestion {
    pub segment_id: String,
    pub mark_ids: Vec<MarkId>,
    /// 0..1 heuristic confidence for ordering suggestions; never a claim of
    /// understanding.
    pub confidence: f32,
}
