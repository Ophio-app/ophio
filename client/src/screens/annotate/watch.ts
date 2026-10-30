// SPDX-License-Identifier: Apache-2.0
//
// Watching a video the agent showed while talking over it. Playback is kept
// as anchors on the drawing clock, the clock speech is timed on, so each
// comment can say where in the video it was made and carry a frame from then.

import { uuidv7Time } from "../../protocol/ids";
import type { VideoMoment } from "../../protocol/types";
import type { AnnotationDraft, SourceDraft, WatchAnchor } from "../../state/annotations";
import { MAX_SOURCES, outgoingSources } from "../../state/annotations";

export { MAX_SOURCES };
/** A frame within this of another from the same video is the same moment. */
export const SAME_FRAME_MS = 500;

/** Where the video was at `clock`, or null when it wasn't being watched. */
export function videoAt(anchors: WatchAnchor[], clock: number, durationMs: number): number | null {
  let last: WatchAnchor | undefined;
  for (const a of anchors) if (a.clock <= clock) last = a;
  if (!last?.watching) return null;
  const at = last.atMs + (last.playing ? (clock - last.clock) * last.rate : 0);
  return Math.round(Math.min(durationMs, Math.max(0, at)));
}

/** When on the drawing clock a comment was made: a spoken one over its
 * final segments, a typed one when it was added. */
function spoken(d: AnnotationDraft, commentId: string): [number, number] | null {
  const c = d.comments.find((x) => x.comment_id === commentId);
  if (!c) return null;
  if (c.origin.kind === "spoken") {
    const ids = c.origin.segment_ids;
    const segments = (d.speech?.segments ?? []).filter((s) => ids.includes(s.segment_id));
    if (!segments.length || segments.some((s) => !s.is_final)) return null;
    return [Math.min(...segments.map((s) => s.start_ms)), Math.max(...segments.map((s) => s.end_ms))];
  }
  const made = uuidv7Time(c.comment_id);
  return made === null ? null : [made - d.createdAt, made - d.createdAt];
}

export interface PendingMoment {
  commentId: string;
  video: SourceDraft;
  startMs: number;
  endMs: number;
}

/** Comments made while a shown video was watched that don't yet say where. */
export function pendingMoments(d: AnnotationDraft): PendingMoment[] {
  const videos = d.sources.filter((s) => s.mediaType.startsWith("video/") && d.watch?.[s.key]?.length);
  if (!videos.length) return [];
  const outgoing = new Set(outgoingSources(d).map((s) => s.key));
  return d.comments.flatMap((c) => {
    // A still the person removed is found again.
    if (c.moment && outgoing.has(c.moment.still)) return [];
    const span = spoken(d, c.comment_id);
    if (!span) return [];
    for (const video of videos) {
      const anchors = d.watch?.[video.key] ?? [];
      const duration = video.durationMs ?? Infinity;
      const start = videoAt(anchors, span[0], duration);
      if (start === null) continue;
      // Seeking back mid-sentence leaves the moment where it began.
      const end = Math.max(start, videoAt(anchors, span[1], duration) ?? start);
      return [{ commentId: c.comment_id, video, startMs: start, endMs: end }];
    }
    return [];
  });
}

/** Moments still to be found. One left without a still is passed over
 * until it moves, or, when the message had no room for one, until it has. */
export function unplacedMoments(d: AnnotationDraft): PendingMoment[] {
  const room = outgoingSources(d).length < MAX_SOURCES;
  return pendingMoments(d).filter((p) => {
    const left = d.momentless?.[p.commentId];
    return !left || left.atMs !== p.startMs || (left.why === "full" && room);
  });
}

/** A frame from `video` already in the draft close enough to `atMs` to
 * show it. A full message gets no farther one: a still from another moment
 * would show the agent something other than what was said over. */
export function existingStill(d: AnnotationDraft, video: SourceDraft, atMs: number): SourceDraft | null {
  const frames = outgoingSources(d)
    .filter((s) => s.derivedFrom === video.key && s.derivation?.kind === "frame_from_clip")
    .map((s) => ({ s, gap: Math.abs((s.derivation?.kind === "frame_from_clip" ? s.derivation.at_ms : 0) - atMs) }))
    .sort((a, b) => a.gap - b.gap);
  const nearest = frames[0];
  return nearest && nearest.gap <= SAME_FRAME_MS ? nearest.s : null;
}

/** Give a comment its moment, or say why it goes without one. Said over a
 * video and pointing at no mark, it is about the video either way, so it no
 * longer waits for targets. */
export function attachMoment(d: AnnotationDraft, commentId: string, moment: VideoMoment): AnnotationDraft {
  const placed = aboutTheVideo(d, commentId);
  if (!placed) return d;
  const momentless = { ...placed.momentless };
  delete momentless[commentId];
  return {
    ...placed,
    comments: placed.comments.map((x) => (x.comment_id === commentId ? { ...x, moment } : x)),
    ...(placed.momentless ? { momentless } : {}),
  };
}

export function leaveMomentless(d: AnnotationDraft, commentId: string, atMs: number, why: "full" | "failed"): AnnotationDraft {
  const placed = aboutTheVideo(d, commentId);
  return placed ? { ...placed, momentless: { ...placed.momentless, [commentId]: { atMs, why } } } : d;
}

function aboutTheVideo(d: AnnotationDraft, commentId: string): AnnotationDraft | null {
  const c = d.comments.find((x) => x.comment_id === commentId);
  if (!c) return null;
  const general = d.commentState?.[commentId] === "suggested" && !c.mark_ids.length && !d.suggestions?.[commentId]?.length;
  return general ? { ...d, commentState: { ...d.commentState, [commentId]: "general" } } : d;
}

/** A position in a video as the person sees it on the player: 1:05. */
export function videoTime(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

export function momentWords(m: VideoMoment): string {
  const start = videoTime(m.start_ms), end = videoTime(m.end_ms);
  return start === end ? start : `${start}–${end}`;
}
