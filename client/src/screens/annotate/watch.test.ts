// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it } from "vitest";
import { uuidv7, uuidv7Time } from "../../protocol/ids";
import type { Comment } from "../../protocol/types";
import { AnnotationStore, toBundle, type AnnotationDraft, type SourceDraft, type WatchAnchor } from "../../state/annotations";
import type { AnnotationContext } from "./sources";
import { attachMoment, existingStill, leaveMomentless, MAX_SOURCES, momentWords, pendingMoments, unplacedMoments, videoAt, videoTime } from "./watch";

const CREATED = 1_800_000_000_000;

const video: SourceDraft = {
  key: "S1", blobKey: "video", artifactId: "shown-video", sha256: "a".repeat(64), mediaType: "video/webm",
  width: 640, height: 360, durationMs: 4000, included: false,
  origin: { kind: "shown", artifact_id: "shown-video" }, capturedAt: CREATED,
  derivedFrom: null, derivation: null,
};
const still = (key: string, atMs: number, extra: Partial<SourceDraft> = {}): SourceDraft => ({
  ...video, key, blobKey: key, artifactId: null, sha256: null, mediaType: "image/png", durationMs: null,
  included: true, derivedFrom: "S1", derivation: { kind: "frame_from_clip", at_ms: atMs }, ...extra,
});
const typed = (atClock: number, extra: Partial<Comment> = {}): Comment => ({
  comment_id: uuidv7(CREATED + atClock), text: "It flashes here.", mark_ids: [], origin: { kind: "typed" },
  link_origins: [], ...extra,
});
const anchor = (clock: number, atMs: number, playing = true, watching = true, rate = 1): WatchAnchor =>
  ({ clock, atMs, playing, rate, watching });

let draft: AnnotationDraft;
beforeEach(() => {
  localStorage.clear();
  draft = { ...new AnnotationStore("watch-test").create("project", "task"), createdAt: CREATED, sources: [video], nextSource: 2 };
});

it("reads a typed comment's time from its id", () => {
  expect(uuidv7Time(uuidv7(CREATED + 1234))).toBe(CREATED + 1234);
  expect(uuidv7Time("speech-segment-1")).toBeNull();
  expect(uuidv7Time("0189f7e2-0000-4000-8000-000000000000")).toBeNull();
});

it("follows the video from play through pauses, seeks and speed changes", () => {
  const anchors = [anchor(1000, 0), anchor(3000, 2000, false), anchor(5000, 500), anchor(6000, 1500, true, true, 2)];
  expect(videoAt(anchors, 500, 4000)).toBeNull(); // not yet played
  expect(videoAt(anchors, 2000, 4000)).toBe(1000);
  expect(videoAt(anchors, 4000, 4000)).toBe(2000); // paused
  expect(videoAt(anchors, 5500, 4000)).toBe(1000); // after seeking back
  expect(videoAt(anchors, 6500, 4000)).toBe(2500); // at double speed
  expect(videoAt(anchors, 60_000, 4000)).toBe(4000); // never past the end
  expect(videoAt([...anchors, anchor(7000, 3500, false, false)], 7500, 4000)).toBeNull(); // left the video
});

it("finds where a typed comment was made while the video played", () => {
  const d: AnnotationDraft = { ...draft, watch: { S1: [anchor(1000, 0)] }, comments: [typed(2700), typed(500)] };
  expect(pendingMoments(d)).toEqual([{ commentId: d.comments[0]!.comment_id, video, startMs: 1700, endMs: 1700 }]);
});

it("spans a spoken comment over its final segments, and waits for them to be final", () => {
  const segment = (id: string, start_ms: number, end_ms: number, is_final = true) =>
    ({ segment_id: id, text: "words", start_ms, end_ms, revision: 1, is_final });
  const spoken = typed(0, { comment_id: "speech-a", origin: { kind: "spoken", segment_ids: ["a", "b"], edited: false } });
  const d: AnnotationDraft = {
    ...draft,
    watch: { S1: [anchor(1000, 0)] },
    comments: [spoken],
    speech: { session_id: "s", audio_artifact: null, segments: [segment("a", 1500, 2000), segment("b", 2000, 3200)], transcript_revision: 1 },
  };
  expect(pendingMoments(d)).toEqual([{ commentId: "speech-a", video, startMs: 500, endMs: 2200 }]);
  const unsettled = { ...d, speech: { ...d.speech!, segments: [segment("a", 1500, 2000), segment("b", 2000, 3200, false)] } };
  expect(pendingMoments(unsettled)).toEqual([]);
});

it("keeps a moment where it began when the person seeks back mid-sentence", () => {
  const spoken = typed(0, { comment_id: "speech-a", origin: { kind: "spoken", segment_ids: ["a"], edited: false } });
  const d: AnnotationDraft = {
    ...draft,
    watch: { S1: [anchor(1000, 0), anchor(2500, 0)] },
    comments: [spoken],
    speech: { session_id: "s", audio_artifact: null, segments: [{ segment_id: "a", text: "w", start_ms: 2000, end_ms: 3000, revision: 1, is_final: true }], transcript_revision: 1 },
  };
  expect(pendingMoments(d)[0]).toMatchObject({ startMs: 1000, endMs: 1000 });
});

it("finds a moment again when its still was removed or left out", () => {
  const c = typed(2000, { moment: { still: "S2", start_ms: 1000, end_ms: 1000 } });
  const d: AnnotationDraft = { ...draft, sources: [video, still("S2", 1000)], watch: { S1: [anchor(1000, 0)] }, comments: [c] };
  expect(pendingMoments(d)).toEqual([]);
  expect(pendingMoments({ ...d, sources: [video] })).toHaveLength(1);
  expect(pendingMoments({ ...d, sources: [video, still("S2", 1000, { included: false })] })).toHaveLength(1);
});

it("reuses a close still, and never a far one, even once the message is full", () => {
  const d: AnnotationDraft = { ...draft, sources: [video, still("S2", 1000), still("S3", 3000)] };
  expect(existingStill(d, video, 1400)?.key).toBe("S2");
  expect(existingStill(d, video, 2000)).toBeNull();
  const others = Array.from({ length: MAX_SOURCES - 2 }, (_, i) => still(`P${i}`, 0, { derivedFrom: null, derivation: null }));
  const full: AnnotationDraft = { ...d, sources: [...d.sources, ...others] };
  expect(existingStill(full, video, 2200)).toBeNull();
  expect(existingStill(full, video, 2800)?.key).toBe("S3");
});

it("makes a spoken comment over the video general once it has its moment", () => {
  const c = typed(0, { comment_id: "speech-a", origin: { kind: "spoken", segment_ids: ["a"], edited: false } });
  const d: AnnotationDraft = { ...draft, comments: [c], commentState: { "speech-a": "suggested" } };
  const moment = { still: "S2", start_ms: 1000, end_ms: 2500 };
  const next = attachMoment(d, "speech-a", moment);
  expect(next.comments[0]!.moment).toEqual(moment);
  expect(next.commentState?.["speech-a"]).toBe("general");
  // One the person pointed at a mark keeps waiting for them to confirm it.
  const pointed = attachMoment({ ...d, comments: [{ ...c, mark_ids: ["m1"] }] }, "speech-a", moment);
  expect(pointed.commentState?.["speech-a"]).toBe("suggested");
  expect(attachMoment(d, "missing", moment)).toBe(d);
  // Without a still it is about the video all the same.
  expect(leaveMomentless(d, "speech-a", 1000, "failed").commentState?.["speech-a"]).toBe("general");
  expect(leaveMomentless({ ...d, comments: [{ ...c, mark_ids: ["m1"] }] }, "speech-a", 1000, "failed").commentState?.["speech-a"]).toBe("suggested");
  expect(leaveMomentless(d, "missing", 1000, "failed")).toBe(d);
});

it("passes over a comment left without a still until its moment moves, or there is room", () => {
  const d: AnnotationDraft = { ...draft, watch: { S1: [anchor(1000, 0)] }, comments: [typed(2000)] };
  const [p] = unplacedMoments(d);
  expect(p?.startMs).toBe(1000);
  const failed = leaveMomentless(d, p!.commentId, 1000, "failed");
  expect(failed.momentless).toEqual({ [p!.commentId]: { atMs: 1000, why: "failed" } });
  expect(pendingMoments(failed)).toHaveLength(1);
  expect(unplacedMoments(failed)).toEqual([]);
  expect(unplacedMoments({ ...failed, watch: { S1: [anchor(1000, 500)] } })).toHaveLength(1);
  const others = Array.from({ length: MAX_SOURCES }, (_, i) => still(`P${i}`, 0, { derivedFrom: null, derivation: null }));
  const full = leaveMomentless({ ...d, sources: [video, ...others] }, p!.commentId, 1000, "full");
  expect(unplacedMoments(full)).toEqual([]);
  expect(unplacedMoments({ ...full, sources: [video, ...others.slice(1)] })).toHaveLength(1);
  // Placed after all, it no longer says it goes without.
  expect(attachMoment(full, p!.commentId, { still: "S2", start_ms: 1000, end_ms: 1000 }).momentless).toEqual({});
});

it("words a moment the way the player shows time", () => {
  expect(videoTime(65_400)).toBe("1:05");
  expect(momentWords({ still: "S2", start_ms: 12_000, end_ms: 15_900 })).toBe("0:12–0:15");
  expect(momentWords({ still: "S2", start_ms: 1700, end_ms: 1900 })).toBe("0:01");
});

it("sends a moment only with the still it names", async () => {
  const store = new AnnotationStore("watch-bundle");
  const { id } = store.create("project", "task");
  // Already on the computer, so nothing is read or uploaded.
  const sent = still("S2", 1000, { blobKey: null, artifactId: "frame-2", sha256: "b".repeat(64) });
  const kept = typed(2000, { text: "Kept", moment: { still: "S2", start_ms: 1000, end_ms: 1000 } });
  const dropped = typed(2100, { text: "Dropped", moment: { still: "S3", start_ms: 1100, end_ms: 1100 } });
  store.record(id, (d) => ({ ...d, sources: [video, sent], nextSource: 3, comments: [kept, dropped], selectedSource: "S2" }));
  const bundle = await toBundle({} as AnnotationContext, store, id);
  expect(bundle.comments.map((c) => [c.text, c.moment ?? null])).toEqual([
    ["Kept", { still: "S2", start_ms: 1000, end_ms: 1000 }],
    ["Dropped", null],
  ]);
});
