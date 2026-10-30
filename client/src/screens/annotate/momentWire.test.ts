// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AnnotationStore, blobs, toBundle, type SourceDraft } from "../../state/annotations";
import { sha256Hex, upload } from "../../host/transfer";
import { extractMoment, type AnnotationContext } from "./sources";

vi.mock("../../host/transfer", async (original) => ({
  ...(await original<typeof import("../../host/transfer")>()),
  upload: vi.fn(),
}));
const data = (text: string, type: string) =>
  Object.assign(new Blob([text], { type }), {
    arrayBuffer: async () => new TextEncoder().encode(text).buffer,
  });

const stored = new Map<string, Blob>();
beforeEach(() => {
  localStorage.clear();
  stored.clear();
  vi.spyOn(blobs, "put").mockImplementation(async (blob) => {
    const key = `blob-${stored.size}`;
    stored.set(key, blob);
    return key;
  });
  vi.spyOn(blobs, "get").mockImplementation(async (key) => stored.get(key) ?? null);
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ drawImage() {} } as never);
  vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation((done) => done(data("frame png", "image/png")));
  vi.stubGlobal("createImageBitmap", async () => ({ width: 480, height: 270, close() {} }));
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

it("a clip moment sent in a bundle carries whole-millisecond clip durations the computer accepts", async () => {
  const clipBytes = data("recorded clip", "video/mp4");
  stored.set("clip", clipBytes);
  // A 241-frame, 30 fps recording: the browser reports duration 8.0333… s.
  const durationMs = (241 / 30) * 1000;
  const clip: SourceDraft = {
    key: "S1", blobKey: "clip", artifactId: null, sha256: await sha256Hex(clipBytes), byteSize: clipBytes.size,
    mediaType: "video/mp4", width: 480, height: 270, durationMs, included: false,
    origin: { kind: "clip", duration_ms: durationMs, target_id: null }, capturedAt: 1,
    derivedFrom: null, derivation: null,
  };
  const store = new AnnotationStore("moment-wire");
  const { id } = store.create("project", null);
  store.record(id, (d) => ({ ...d, sources: [clip], nextSource: 2 }));
  const video = { readyState: 4, seeking: false, currentTime: 3.5, videoWidth: 480, videoHeight: 270 } as HTMLVideoElement;

  // "Add this moment" in Steps, then describe the step.
  const frame = await extractMoment(store, id, clip, video, { start: 0, end: durationMs / 1000 });
  store.update(id, (d) => ({
    ...d,
    steps: [{ index: 1, source_key: frame.key, text: "Tap Save", clip_range_ms: [0, Math.round(durationMs)] }],
  }));
  vi.mocked(upload).mockResolvedValue({ transferId: "t", artifactId: "frame-artifact", sha256: frame.sha256! });
  const bundle = await toBundle({} as AnnotationContext, store, id);

  // crates/protocol/src/annotation.rs: SourceOrigin::Clip { duration_ms: u64 }.
  // serde refuses a fractional number there, so the computer can't parse the command.
  const origin = bundle.sources[0]!.origin;
  expect(origin.kind === "clip" ? origin.duration_ms : null).toBe(8033);
});

it("a message with more images than the computer accepts is refused before any is sent", async () => {
  const store = new AnnotationStore("moment-wire");
  const { id } = store.create("project", null);
  const pictures = Array.from({ length: 17 }, (_, i): SourceDraft => ({
    key: `S${i + 1}`, blobKey: `p${i}`, artifactId: null, sha256: "c".repeat(64), byteSize: 1, mediaType: "image/png", width: 4, height: 4,
    durationMs: null, included: true, origin: { kind: "phone_import", original_name: null }, capturedAt: 1, derivedFrom: null, derivation: null,
  }));
  store.record(id, (d) => ({ ...d, sources: pictures, nextSource: 18 }));
  await expect(toBundle({} as AnnotationContext, store, id)).rejects.toThrow("Include at most 16 images, then send again.");
  expect(upload).not.toHaveBeenCalled();
  expect(store.preparing.has(id)).toBe(false);
});
