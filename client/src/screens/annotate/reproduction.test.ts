// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it, vi } from "vitest";
import {
  AnnotationStore,
  blobs,
  type SourceDraft,
} from "../../state/annotations";
import { sha256Hex, upload } from "../../host/transfer";
import { createClip } from "./clipEncoding";
import { prepareReproduction, sendReviewedClip } from "./reproduction";
import { videoSize, type AnnotationContext } from "./sources";
vi.mock("./clipEncoding", () => ({ createClip: vi.fn() }));
vi.mock("./sources", () => ({ videoSize: vi.fn() }));
vi.mock("../../host/transfer", async (original) => ({
  ...(await original<typeof import("../../host/transfer")>()),
  upload: vi.fn(),
}));
const data = (text: string, type = "video/webm") =>
  Object.assign(new Blob([text], { type }), {
    arrayBuffer: async () => new TextEncoder().encode(text).buffer,
  });
beforeEach(() => {
  vi.restoreAllMocks();
  vi.clearAllMocks();
  localStorage.clear();
});
async function setup() {
  const store = new AnnotationStore("reproduction-test"),
    draft = store.create("project", "task");
  const original = data("original video");
  const source: SourceDraft = {
    key: "S1",
    blobKey: "original",
    artifactId: null,
    sha256: await sha256Hex(original),
    byteSize: original.size,
    width: 480,
    height: 270,
    mediaType: original.type,
    durationMs: 8000,
    origin: { kind: "clip", target_id: null, duration_ms: 8000 },
    capturedAt: 1,
    derivation: null,
    derivedFrom: null,
    included: false,
  };
  store.record(draft.id, (d) => ({
    ...d,
    sources: [source],
    nextSource: 2,
    clipEdits: { S1: { start: "2", end: "5", at: 2 } },
    marks: [
      {
        mark_id: "kept",
        label: "A",
        source_key: "S1",
        shape: { kind: "pin", at: { x: 0.5, y: 0.5, t: null } },
        color: 0,
        created_at: 1,
        deleted: false,
      },
    ],
  }));
  const stored = new Map<string, Blob>([["original", original]]);
  vi.spyOn(blobs, "get").mockImplementation(
    async (key) => stored.get(key) ?? null,
  );
  vi.spyOn(blobs, "put").mockImplementation(async (value) => {
    const key = String(stored.size);
    stored.set(key, value);
    return key;
  });
  vi.spyOn(blobs, "remove").mockImplementation(async (key) => {
    stored.delete(key);
  });
  vi.mocked(createClip).mockResolvedValue({
    blob: data("three second export"),
    range: { start: 2, end: 5 },
    method: "webcodecs",
    frames: [2000, 3500, 4933].map((atMs) => ({
      blob: data(`frame ${atMs}`, "image/png"),
      atMs,
    })),
  });
  vi.mocked(videoSize).mockResolvedValue({
    width: 480,
    height: 270,
    durationMs: 3000,
  });
  return { store, id: draft.id, source, stored };
}
it("saves clip and frames atomically with source and byte identities; range and marks survive reload", async () => {
  const { store, id, source } = await setup();
  await prepareReproduction(
    store,
    id,
    source,
    { start: 2, end: 5 },
    new AbortController().signal,
  );
  const restored = new AnnotationStore("reproduction-test").get()[id];
  expect(restored?.sources).toHaveLength(5);
  for (const derived of restored?.sources.slice(1) ?? []) {
    expect(derived.reproduction).toMatchObject({
      sourceSha256: source.sha256,
      sourceSize: source.byteSize,
      actualRangeMs: [2000, 5000],
    });
    expect(derived.sha256).toHaveLength(64);
    expect(derived.byteSize).toBeGreaterThan(0);
  }
  expect(restored?.marks[0]?.mark_id).toBe("kept");
  expect(restored?.clipEdits?.S1).toEqual({ start: "2", end: "5", at: 2 });
  expect(upload).not.toHaveBeenCalled();
});
it("a decode or storage failure publishes no partial clip", async () => {
  const { store, id, source, stored } = await setup();
  vi.spyOn(blobs, "put")
    .mockImplementationOnce(async (value) => {
      stored.set("partial", value);
      return "partial";
    })
    .mockRejectedValueOnce(new Error("Storage full"));
  await expect(
    prepareReproduction(
      store,
      id,
      source,
      { start: 2, end: 5 },
      new AbortController().signal,
    ),
  ).rejects.toThrow("Storage full");
  expect(store.get()[id]?.sources).toEqual([source]);
  expect(stored.has("partial")).toBe(false);
  vi.mocked(createClip).mockRejectedValueOnce(new Error("Cannot decode"));
  await expect(
    prepareReproduction(
      store,
      id,
      source,
      { start: 2, end: 5 },
      new AbortController().signal,
    ),
  ).rejects.toThrow("Cannot decode");
  expect(store.get()[id]?.sources).toEqual([source]);
});
it("range edits during encoding reject the older result and retain the newer draft", async () => {
  const { store, id, source } = await setup();
  store.record(id, (d) => ({
    ...d,
    clipEdits: { S1: { start: "3", end: "4", at: 3 } },
  }));
  await expect(
    prepareReproduction(
      store,
      id,
      source,
      { start: 2, end: 5 },
      new AbortController().signal,
    ),
  ).rejects.toThrow("range changed");
  expect(store.get()[id]?.sources).toHaveLength(1);
  expect(blobs.remove).toHaveBeenCalledTimes(4);
});
it("checks the clip before transfer and sends the reviewed immutable Blob", async () => {
  const { store, id, source, stored } = await setup();
  await prepareReproduction(
    store,
    id,
    source,
    { start: 2, end: 5 },
    new AbortController().signal,
  );
  const clip = store.get()[id]?.sources[1];
  if (!clip?.blobKey) throw new Error("Missing clip fixture");
  const reviewed = stored.get(clip.blobKey);
  stored.set(clip.blobKey, data("mutated"));
  await expect(
    sendReviewedClip({} as AnnotationContext, store, id, clip),
  ).rejects.toThrow("changed since review");
  expect(upload).not.toHaveBeenCalled();
  if (!reviewed) throw new Error("Missing bytes");
  stored.set(clip.blobKey, reviewed);
  vi.mocked(upload).mockResolvedValue({
    transferId: "transfer",
    artifactId: "artifact",
    sha256: clip.sha256 ?? "",
  });
  await sendReviewedClip({} as AnnotationContext, store, id, clip);
  expect(upload).toHaveBeenCalledWith(
    {},
    expect.objectContaining({ blob: reviewed, sha256: clip.sha256 }),
  );
  expect(store.get()[id]?.sources[1]?.clipTransfer?.artifactId).toBe(
    "artifact",
  );
});
it("clears a confirmed failed transfer so an explicit retry can start anew", async () => {
  const { TransferError } = await import("../../host/transfer");
  const { store, id, source } = await setup();
  await prepareReproduction(
    store,
    id,
    source,
    { start: 2, end: 5 },
    new AbortController().signal,
  );
  const clip = store.get()[id]?.sources[1];
  if (!clip) throw new Error("Missing fixture");
  store.record(id, (d) => ({
    ...d,
    sources: d.sources.map((s) =>
      s.key === clip.key
        ? { ...s, clipTransfer: { transferId: "failed", artifactId: null } }
        : s,
    ),
  }));
  vi.mocked(upload).mockRejectedValueOnce(
    new TransferError("Transfer refused", "failed", false),
  );
  await expect(
    sendReviewedClip({} as AnnotationContext, store, id, {
      ...clip,
      clipTransfer: { transferId: "failed", artifactId: null },
    }),
  ).rejects.toThrow("Transfer refused");
  expect(store.get()[id]?.sources[1]?.clipTransfer).toBeUndefined();
});
it("rolls back derivatives when their draft metadata cannot persist", async () => {
  const { store, id, source, stored } = await setup();
  vi.spyOn(store.persistence, "get").mockReturnValue("Storage full");
  await expect(
    prepareReproduction(
      store,
      id,
      source,
      { start: 2, end: 5 },
      new AbortController().signal,
    ),
  ).rejects.toThrow("draft could not be saved");
  expect(store.get()[id]?.sources).toEqual([source]);
  expect(store.get()[id]?.marks[0]?.mark_id).toBe("kept");
  expect(stored.size).toBe(1);
});
