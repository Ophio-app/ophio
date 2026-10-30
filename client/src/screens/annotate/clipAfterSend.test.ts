// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it, vi } from "vitest";
import {
  AnnotationStore,
  blobs,
  editedSinceSent,
  markSent,
  toBundle,
  type AnnotationDraft,
  type SourceDraft,
} from "../../state/annotations";
import { sha256Hex, upload } from "../../host/transfer";
import { sendReviewedClip } from "./reproduction";
import type { AnnotationContext } from "./sources";

vi.mock("../../host/transfer", async (original) => ({
  ...(await original<typeof import("../../host/transfer")>()),
  upload: vi.fn(),
}));
const data = (text: string, type: string) =>
  Object.assign(new Blob([text], { type }), {
    arrayBuffer: async () => new TextEncoder().encode(text).buffer,
  });

beforeEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
  vi.clearAllMocks();
});

it("sending the reviewed clip after the markup doesn't make the sent markup look edited", async () => {
  const clipBytes = data("trimmed clip", "video/webm"),
    frameBytes = data("key frame", "image/png");
  const stored = new Map<string, Blob>([["clip", clipBytes], ["frame", frameBytes]]);
  vi.spyOn(blobs, "get").mockImplementation(async (key) => stored.get(key) ?? null);
  const identity = {
    sourceKey: "S1", sourceSha256: "0".repeat(64), sourceSize: 100,
    requestedRangeMs: [2000, 5000] as [number, number], actualRangeMs: [2000, 5000] as [number, number],
    method: "webcodecs" as const, silent: true as const,
  };
  const clip: SourceDraft = {
    key: "S2", blobKey: "clip", artifactId: null, sha256: await sha256Hex(clipBytes), byteSize: clipBytes.size,
    mediaType: "video/webm", width: 480, height: 270, durationMs: 3000, included: false,
    origin: { kind: "clip", duration_ms: 8000, target_id: null }, capturedAt: 1,
    derivedFrom: "S1", derivation: { kind: "trim", start_ms: 2000, end_ms: 5000 }, reproduction: identity,
  };
  const frame: SourceDraft = {
    ...clip, key: "S3", blobKey: "frame", sha256: await sha256Hex(frameBytes), byteSize: frameBytes.size,
    mediaType: "image/png", durationMs: null, included: true,
    derivation: { kind: "frame_from_clip", at_ms: 3500 }, reproduction: { ...identity, method: "frame", frameAtMs: 3500 },
  };
  const store = new AnnotationStore("clip-after-send");
  const { id } = store.create("project", null);
  store.update(id, (d) => ({
    ...d,
    sources: [clip, frame],
    nextSource: 4,
    steps: [{ index: 1, source_key: "S3", text: "Open the menu", clip_range_ms: null }],
  }));
  const ctx = {} as AnnotationContext;

  // Review and send: the frame and step reach the computer as revision 1.
  vi.mocked(upload).mockResolvedValueOnce({ transferId: "t-frame", artifactId: "frame-artifact", sha256: frame.sha256! });
  const bundle = await toBundle(ctx, store, id);
  markSent(store, id, bundle.revision, store.prepared.get(id)!.content);
  expect(editedSinceSent(store.get()[id] as AnnotationDraft)).toBe(false);

  // Then the person sends the reviewed clip to the computer as a separate file.
  vi.mocked(upload).mockResolvedValueOnce({ transferId: "t-clip", artifactId: "clip-artifact", sha256: clip.sha256! });
  await sendReviewedClip(ctx, store, id, store.get()[id]!.sources[0]!);
  expect(store.get()[id]?.sources[0]?.clipTransfer?.artifactId).toBe("clip-artifact");

  // Nothing in the markup changed: no "Revision 2 pending", and it still counts as sent.
  expect(editedSinceSent(store.get()[id] as AnnotationDraft)).toBe(false);
});
