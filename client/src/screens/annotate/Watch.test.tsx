// SPDX-License-Identifier: Apache-2.0
// Talking over a video the agent showed: how it played is kept on the
// drawing clock, and each comment made meanwhile gets its moment and a still.
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { sha256Hex } from "../../host/transfer";
import { uuidv7 } from "../../protocol/ids";
import { AnnotationStore, blobs, MAX_SOURCES, outgoingSources, useAnnotation, type AnnotationDraft, type SourceDraft } from "../../state/annotations";
import { extractMoment, stillAt } from "./sources";
import { placeMoments, useMoments, Watch } from "./Watch";
import { videoAt } from "./watch";

vi.mock("./sources", async (original) => ({ ...(await original<typeof import("./sources")>()), stillAt: vi.fn() }));

const clip = Object.assign(new Blob(["agent video"], { type: "video/webm" }), {
  arrayBuffer: async () => new TextEncoder().encode("agent video").buffer,
});
let video: SourceDraft;
let store: AnnotationStore;
let id: string;
beforeEach(async () => {
  localStorage.clear();
  vi.spyOn(blobs, "get").mockImplementation(async (key) => (key === "clip" ? clip : null));
  vi.stubGlobal("URL", Object.assign(URL, { createObjectURL: () => "blob:clip", revokeObjectURL() {} }));
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => undefined);
  video = {
    key: "S1", blobKey: "clip", artifactId: "shown-clip", sha256: await sha256Hex(clip), byteSize: clip.size,
    mediaType: "video/webm", width: 640, height: 360, durationMs: 4000, included: false,
    origin: { kind: "shown", artifact_id: "shown-clip" }, capturedAt: 1, derivedFrom: null, derivation: null,
  };
  store = new AnnotationStore("watch-ui");
  id = store.create("project", "task").id;
  store.record(id, (d) => ({ ...d, sources: [video], nextSource: 2, selectedSource: "S1" }));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.mocked(stillAt).mockReset(); });

const draft = (): AnnotationDraft => store.get()[id]!;
function Harness({ composing = false }: { composing?: boolean }) {
  return <Watch draft={draft()} store={store} source={video} composing={composing} />;
}
const player = async () => {
  await waitFor(() => expect(document.querySelector("video.an-video")).toBeTruthy());
  // The player shows before it is listened to: a busy machine may not have
  // run the effect that listens yet.
  await act(async () => {});
  return document.querySelector<HTMLVideoElement>("video.an-video")!;
};
const at = (el: HTMLVideoElement, seconds: number, paused: boolean, readyState = 4) => {
  Object.defineProperty(el, "currentTime", { configurable: true, value: seconds });
  Object.defineProperty(el, "paused", { configurable: true, value: paused });
  Object.defineProperty(el, "readyState", { configurable: true, value: readyState });
};

it("keeps how the video played, from its first play until the person leaves it", async () => {
  const { unmount, rerender } = render(<Harness />);
  // A typed comment over the video needs no mark.
  expect(draft().composer).toMatchObject({ targets: [], general: true });
  const el = await player();
  at(el, 0.4, true);
  act(() => { el.dispatchEvent(new Event("seeked")); });
  expect(draft().watch).toBeUndefined();
  at(el, 0.4, false);
  act(() => { el.dispatchEvent(new Event("play")); });
  store.update(id, (d) => ({ ...d, comments: [{ comment_id: uuidv7(), text: "Hm.", mark_ids: [], origin: { kind: "typed" }, link_origins: [] }] }));
  at(el, 1.25, true);
  act(() => { el.dispatchEvent(new Event("pause")); });
  expect(draft().watch?.S1?.map(({ atMs, playing, watching }) => ({ atMs, playing, watching }))).toEqual([
    { atMs: 400, playing: true, watching: true },
    { atMs: 1250, playing: false, watching: true },
  ]);
  rerender(<Harness composing />);
  expect(HTMLMediaElement.prototype.pause).toHaveBeenCalled();
  unmount();
  expect(draft().watch?.S1?.at(-1)).toMatchObject({ watching: false, playing: false });
  // None of it is an edit the person would undo.
  store.undo(id);
  expect(draft().comments).toEqual([]);
  expect(draft().watch?.S1).toHaveLength(3);
});

it("holds the video where it stalled, and where it stopped without saying", async () => {
  render(<Harness />);
  const el = await player();
  const clock = vi.spyOn(Date, "now");
  const tick = (ms: number, seconds: number, event: string, readyState = 4) => {
    clock.mockReturnValue(draft().createdAt + ms);
    at(el, seconds, false, readyState);
    act(() => { el.dispatchEvent(new Event(event)); });
  };
  // Asked to play, it starts only once it has the data.
  tick(500, 0, "play", 1);
  tick(1000, 0, "playing");
  // It waits for data at one second, then plays on.
  tick(2000, 1, "waiting", 2);
  tick(7000, 1, "playing");
  // Later it stops without saying, and is found to have when it moves again.
  tick(8000, 2, "timeupdate");
  tick(8250, 2.25, "timeupdate");
  tick(12_000, 2.5, "timeupdate");
  // Seeking, it is held where it was until the frame sought is shown,
  // however long that takes.
  Object.defineProperty(el, "seeking", { configurable: true, value: true });
  tick(13_000, 0.5, "seeking");
  tick(13_400, 0.5, "waiting", 1);
  tick(13_600, 0.5, "timeupdate", 1);
  Object.defineProperty(el, "seeking", { configurable: true, value: false });
  tick(14_000, 0.5, "seeked");
  const anchors = draft().watch!.S1!;
  expect(videoAt(anchors, 800, 4000)).toBe(0);
  expect(videoAt(anchors, 1500, 4000)).toBe(500);
  expect(videoAt(anchors, 4000, 4000)).toBe(1000);
  expect(videoAt(anchors, 7500, 4000)).toBe(1500);
  expect(videoAt(anchors, 10_000, 4000)).toBe(2250);
  expect(videoAt(anchors, 12_500, 4000)).toBe(3000);
  expect(videoAt(anchors, 13_500, 4000)).toBe(3500);
  expect(videoAt(anchors, 13_900, 4000)).toBe(3500);
  expect(videoAt(anchors, 14_500, 4000)).toBe(1000);
  // Played about as expected, it needs no more anchors.
  const kept = anchors.length;
  tick(15_000, 1.6, "timeupdate");
  expect(draft().watch!.S1).toHaveLength(kept);
});

let renders = 0;
function Moments() {
  renders++;
  useMoments(store, useAnnotation(store, id));
  return null;
}

it("gives a comment made while the video played its moment and a still from then", async () => {
  const made = Date.now();
  store.record(id, (d) => ({
    ...d,
    watch: { S1: [{ clock: made - 1500 - d.createdAt, atMs: 0, playing: true, rate: 1, watching: true }] },
  }));
  store.update(id, (d) => ({ ...d, comments: [{ comment_id: uuidv7(made), text: "It flashes.", mark_ids: [], origin: { kind: "typed" }, link_origins: [] }] }));
  vi.mocked(stillAt).mockImplementation(async (s, annotation, source, atMs) => {
    const frame: SourceDraft = { ...source, key: "S2", included: true, mediaType: "image/png", durationMs: null, derivedFrom: "S1", derivation: { kind: "frame_from_clip", at_ms: atMs } };
    s.place(annotation, (d) => ({ ...d, sources: [...d.sources, frame], nextSource: 3 }));
    return frame;
  });
  render(<Moments />);
  await waitFor(() => expect(draft().comments[0]!.moment).toEqual({ still: "S2", start_ms: 1500, end_ms: 1500 }));
  expect(vi.mocked(stillAt).mock.calls[0]![3]).toBe(1500);
  // Undoing the comment leaves the still, which the person never added.
  store.undo(id);
  expect(draft().comments).toEqual([]);
  expect(draft().sources.map((s) => s.key)).toEqual(["S1", "S2"]);
  store.redo(id);
  expect(draft().comments[0]!.moment).toEqual({ still: "S2", start_ms: 1500, end_ms: 1500 });
});

it("leaves a comment general when no still can be made, without trying again", async () => {
  const made = Date.now();
  store.record(id, (d) => ({
    ...d,
    watch: { S1: [{ clock: made - 500 - d.createdAt, atMs: 0, playing: true, rate: 1, watching: true }] },
    comments: [{ comment_id: uuidv7(made), text: "Too fast.", mark_ids: [], origin: { kind: "typed" }, link_origins: [] }],
  }));
  vi.mocked(stillAt).mockRejectedValue(new Error("The video couldn’t be read."));
  const { rerender } = render(<Moments />);
  await waitFor(() => expect(stillAt).toHaveBeenCalledTimes(1));
  rerender(<Moments />);
  await act(async () => { await Promise.resolve(); });
  expect(stillAt).toHaveBeenCalledTimes(1);
  expect(draft().comments[0]!.moment).toBeUndefined();
  expect(draft().momentless).toEqual({ [draft().comments[0]!.comment_id]: { atMs: 500, why: "failed" } });
});

it("leaves a moment with no room for its still, and takes one once there is room", async () => {
  const made = Date.now();
  const pictures = Array.from({ length: 16 }, (_, i): SourceDraft => ({
    ...video, key: `P${i}`, mediaType: "image/png", durationMs: null, included: true, origin: { kind: "phone_import", original_name: null },
  }));
  const said = { comment_id: uuidv7(made), text: "Here.", mark_ids: [], origin: { kind: "spoken" as const, segment_ids: ["a"], edited: false }, link_origins: [] };
  store.record(id, (d) => ({
    ...d,
    sources: [video, ...pictures],
    watch: { S1: [{ clock: made - 500 - d.createdAt, atMs: 0, playing: true, rate: 1, watching: true }] },
    speech: { session_id: "s", audio_artifact: null, transcript_revision: 1,
      segments: [{ segment_id: "a", text: "Here.", start_ms: made - d.createdAt, end_ms: made - d.createdAt, revision: 1, is_final: true }] },
    comments: [said],
    commentState: { [said.comment_id]: "suggested" },
  }));
  vi.mocked(stillAt).mockImplementation(async (s, annotation, source, atMs) => {
    const frame: SourceDraft = { ...source, key: "S2", included: true, mediaType: "image/png", durationMs: null, derivedFrom: "S1", derivation: { kind: "frame_from_clip", at_ms: atMs } };
    s.place(annotation, (d) => ({ ...d, sources: [...d.sources, frame] }));
    return frame;
  });
  renders = 0;
  render(<Moments />);
  for (let i = 0; i < 5; i++) await act(async () => { await Promise.resolve(); });
  expect(stillAt).not.toHaveBeenCalled();
  expect(draft().comments[0]!.moment).toBeUndefined();
  // Said over the video and pointing at nothing, it goes as a general comment.
  expect(draft().commentState?.[said.comment_id]).toBe("general");
  expect(draft().momentless?.[said.comment_id]).toEqual({ atMs: 500, why: "full" });
  expect(renders).toBeLessThan(4);
  // A picture taken out makes room for its still.
  act(() => store.record(id, (d) => ({ ...d, sources: d.sources.filter((s) => s.key !== "P0") })));
  await waitFor(() => expect(draft().comments[0]!.moment).toEqual({ still: "S2", start_ms: 500, end_ms: 500 }));
  expect(draft().momentless).toEqual({});
});

/** Stills are taken as the app takes them: quietly, by every version of
 * the draft. Each waits for `ready` first. */
function extracting(ready: Promise<void> = Promise.resolve()) {
  vi.spyOn(blobs, "put").mockResolvedValue("frame");
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ drawImage() {} } as never);
  vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation((done) => done(new Blob(["frame png"], { type: "image/png" })));
  vi.stubGlobal("createImageBitmap", async () => ({ width: 640, height: 360, close() {} }));
  vi.mocked(stillAt).mockImplementation(async (s, annotation, source, atMs, wants) => {
    await ready;
    return extractMoment(s, annotation, source, { readyState: 4, seeking: false, currentTime: atMs / 1000, videoWidth: 640, videoHeight: 360 } as HTMLVideoElement, undefined, { wants });
  });
}
/** A typed comment made half a second into the video, beside `included` of 16 pictures. */
function overTheVideo(included: number, made = Date.now()) {
  const pictures = Array.from({ length: 16 }, (_, i): SourceDraft => ({
    ...video, key: `P${i}`, mediaType: "image/png", durationMs: null, included: i < included, origin: { kind: "phone_import", original_name: null },
  }));
  const typed = { comment_id: uuidv7(made), text: "Here.", mark_ids: [], origin: { kind: "typed" as const }, link_origins: [] };
  store.record(id, (d) => ({
    ...d,
    sources: [video, ...pictures],
    watch: { S1: [{ clock: made - 500 - d.createdAt, atMs: 0, playing: true, rate: 1, watching: true }] },
    comments: [typed],
  }));
  return typed.comment_id;
}
const leaveOut = (key: string, included = false) =>
  store.update(id, (d) => ({ ...d, sources: d.sources.map((s) => (s.key === key ? { ...s, included } : s)) }));

it.each([true, false])("keeps a still taken once there is room through an undo that leaves no room, out of the message (first left: %s)", async (first) => {
  extracting();
  const comment = overTheVideo(16);
  if (first) {
    await placeMoments(store, id);
    expect(stillAt).not.toHaveBeenCalled();
    expect(draft().momentless?.[comment]).toEqual({ atMs: 500, why: "full" });
  }
  // Leaving a picture out makes room, and the still is taken for the comment.
  leaveOut("P0");
  await placeMoments(store, id);
  expect(stillAt).toHaveBeenCalledTimes(1);
  expect(outgoingSources(draft())).toHaveLength(MAX_SOURCES);
  expect(draft().comments[0]!.moment).toEqual({ still: "S2", start_ms: 500, end_ms: 500 });
  // Undo puts the picture back: the still is kept but left out, so the
  // message stays within its limit and the comment goes without its moment.
  store.undo(id);
  expect(draft().sources.find((s) => s.key === "P0")?.included).toBe(true);
  expect(outgoingSources(draft())).toHaveLength(MAX_SOURCES);
  expect(draft().sources.find((s) => s.key === "S2")).toMatchObject({ included: false, blobKey: "frame" });
  expect(draft().comments[0]!.moment).toBeUndefined();
  expect(draft().momentless?.[comment]).toEqual({ atMs: 500, why: "full" });
  await placeMoments(store, id);
  expect(stillAt).toHaveBeenCalledTimes(1);
  store.redo(id);
  expect(outgoingSources(draft())).toHaveLength(MAX_SOURCES);
  expect(draft().comments[0]!.moment).toEqual({ still: "S2", start_ms: 500, end_ms: 500 });
});

it("takes the still a full message had no room for once undo brings back a version with room", async () => {
  extracting();
  const comment = overTheVideo(15);
  leaveOut("P15", true);
  await placeMoments(store, id);
  expect(stillAt).not.toHaveBeenCalled();
  expect(draft().momentless?.[comment]).toEqual({ atMs: 500, why: "full" });
  store.undo(id);
  expect(draft().momentless?.[comment]).toBeUndefined();
  await placeMoments(store, id);
  expect(stillAt).toHaveBeenCalledTimes(1);
  expect(draft().comments[0]!.moment).toEqual({ still: "S2", start_ms: 500, end_ms: 500 });
});

it("gives a version taken to send its still and moment, and none of the edits made since", async () => {
  extracting();
  overTheVideo(0);
  const sending = store.take(id)!;
  store.update(id, (d) => ({ ...d, comments: d.comments.map((c) => ({ ...c, text: "There." })) }));
  await placeMoments(store, id);
  expect(stillAt).toHaveBeenCalledTimes(1);
  expect(sending.draft.comments[0]).toMatchObject({ text: "Here.", moment: { still: "S2", start_ms: 500, end_ms: 500 } });
  expect(sending.draft.sources.find((s) => s.key === "S2")?.included).toBe(true);
  expect(draft().comments[0]).toMatchObject({ text: "There.", moment: { still: "S2", start_ms: 500, end_ms: 500 } });
  sending.release();
  expect(store.taken(id)).toEqual([]);
});

it("sends no still for a comment made after the send was taken", async () => {
  let open = () => {};
  extracting(new Promise((resolve) => (open = resolve)));
  const made = Date.now();
  overTheVideo(0, made);
  const sending = store.take(id)!;
  // Made while the first comment's still is being taken.
  const later = { comment_id: uuidv7(made + 1500), text: "And here.", mark_ids: [], origin: { kind: "typed" as const }, link_origins: [] };
  store.update(id, (d) => ({ ...d, comments: [...d.comments, later] }));
  const placing = placeMoments(store, id);
  open();
  await placing;
  expect(stillAt).toHaveBeenCalledTimes(2);
  expect(sending.draft.comments.map((c) => c.moment?.start_ms)).toEqual([500]);
  expect(outgoingSources(sending.draft).map((s) => s.key)).toEqual(["S2"]);
  expect(sending.draft.sources.find((s) => s.key === "S3")?.included).toBe(false);
  expect(draft().comments.map((c) => c.moment?.start_ms)).toEqual([500, 2000]);
  expect(outgoingSources(draft()).map((s) => s.key)).toEqual(["S2", "S3"]);
  sending.release();
});

it("sends no still from a moment none of the draft's comments waits for any more", async () => {
  let open = () => {};
  extracting(new Promise((resolve) => (open = resolve)));
  const made = Date.now();
  overTheVideo(0, made);
  const sending = store.take(id)!;
  // The first comment is replaced by one at another moment after the tap.
  const later = { comment_id: uuidv7(made + 1500), text: "Rather here.", mark_ids: [], origin: { kind: "typed" as const }, link_origins: [] };
  store.update(id, (d) => ({ ...d, comments: [later] }));
  const placing = placeMoments(store, id);
  open();
  await placing;
  expect(outgoingSources(sending.draft).map((s) => s.key)).toEqual(["S2"]);
  expect(outgoingSources(draft()).map((s) => s.key)).toEqual(["S3"]);
  expect(draft().comments[0]!.moment?.start_ms).toBe(2000);
  sending.release();
});

it("finishes a version taken to send while it was placing another's moments", async () => {
  let open = () => {};
  extracting(new Promise((resolve) => (open = resolve)));
  const made = Date.now();
  overTheVideo(0, made);
  const later = { comment_id: uuidv7(made + 1500), text: "And here.", mark_ids: [], origin: { kind: "typed" as const }, link_origins: [] };
  store.record(id, (d) => ({ ...d, comments: [...d.comments, later] }));
  // Already placing the first comment's moment when Send is tapped.
  const placing = placeMoments(store, id);
  const sending = store.take(id)!;
  store.update(id, (d) => ({ ...d, comments: d.comments.filter((c) => c.comment_id !== later.comment_id) }));
  open();
  await placing;
  expect(stillAt).toHaveBeenCalledTimes(2);
  expect(sending.draft.comments.map((c) => c.moment?.start_ms)).toEqual([500, 2000]);
  expect(draft().comments.map((c) => c.moment?.start_ms)).toEqual([500]);
  sending.release();
});

it("takes each still once however many ask, and is done only when it is", async () => {
  const made = Date.now();
  store.record(id, (d) => ({
    ...d,
    watch: { S1: [{ clock: made - 1000 - d.createdAt, atMs: 0, playing: true, rate: 1, watching: true }] },
    comments: [{ comment_id: uuidv7(made), text: "Now.", mark_ids: [], origin: { kind: "typed" }, link_origins: [] }],
  }));
  let taken: () => void = () => {};
  vi.mocked(stillAt).mockImplementation((s, annotation, source, atMs) => new Promise((resolve) => {
    taken = () => {
      const frame: SourceDraft = { ...source, key: "S2", included: true, mediaType: "image/png", durationMs: null, derivedFrom: "S1", derivation: { kind: "frame_from_clip", at_ms: atMs } };
      s.place(annotation, (d) => ({ ...d, sources: [...d.sources, frame] }));
      resolve(frame);
    };
  }));
  let done = 0;
  const both = [placeMoments(store, id), placeMoments(store, id)].map((p) => p.then(() => done++));
  for (let i = 0; i < 5; i++) await Promise.resolve();
  expect(done).toBe(0);
  taken();
  await Promise.all(both);
  expect(done).toBe(2);
  expect(stillAt).toHaveBeenCalledTimes(1);
  expect(draft().comments[0]!.moment).toEqual({ still: "S2", start_ms: 1000, end_ms: 1000 });
});
