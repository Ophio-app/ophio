// SPDX-License-Identifier: Apache-2.0
import type { Comment, Mark, SpeechSegment } from "../../protocol/types";
import type { AnnotationDraft } from "../../state/annotations";
/** A timing hint, never an automatic association. Include marks up to 1800ms before speech. */
export const LINK_TOLERANCE_MS = 1800;
export function suggestedLinks(
  segment: SpeechSegment,
  marks: Mark[],
  drawingClock: number,
  tolerance = LINK_TOLERANCE_MS,
): string[] {
  return marks
    .filter(
      (m) =>
        !m.deleted &&
        m.created_at - drawingClock >= segment.start_ms - tolerance &&
        m.created_at - drawingClock <= segment.end_ms,
    )
    .map((m) => m.mark_id);
}
export function mergeSegment(
  d: AnnotationDraft,
  incoming: SpeechSegment,
): AnnotationDraft {
  if (!d.speech || d.deletedSpeechSegments?.includes(incoming.segment_id))
    return d;
  const previous = d.speech.segments.find(
    (s) => s.segment_id === incoming.segment_id,
  );
  if (previous && previous.revision >= incoming.revision) return d;
  const comment = d.comments.find(
    (c) =>
      c.origin.kind === "spoken" &&
      c.origin.segment_ids.includes(incoming.segment_id),
  );
  const edited = comment?.origin.kind === "spoken" && comment.origin.edited;
  const speech = {
    ...d.speech,
    segments: [
      ...d.speech.segments.filter((s) => s.segment_id !== incoming.segment_id),
      incoming,
    ],
    transcript_revision: d.speech.transcript_revision + 1,
  };
  if (edited)
    return {
      ...d,
      speech,
      speechSuggestions: {
        ...d.speechSuggestions,
        [incoming.segment_id]: incoming,
      },
    };
  const id = comment?.comment_id ?? `speech-${incoming.segment_id}`;
  const next: Comment = comment
    ? { ...comment, text: incoming.text }
    : {
        comment_id: id,
        text: incoming.text,
        mark_ids: [],
        link_origins: [],
        origin: {
          kind: "spoken",
          segment_ids: [incoming.segment_id],
          edited: false,
        },
      };
  const manual =
    d.commentState?.[id] === "manual" || d.commentState?.[id] === "general";
  return {
    ...d,
    speech,
    comments: comment
      ? d.comments.map((c) => (c.comment_id === id ? next : c))
      : [...d.comments, next],
    commentState: {
      ...d.commentState,
      [id]: d.commentState?.[id] ?? "suggested",
    },
    suggestions: manual
      ? d.suggestions
      : {
          ...d.suggestions,
          [id]: suggestedLinks(incoming, d.marks, d.createdAt),
        },
  };
}
