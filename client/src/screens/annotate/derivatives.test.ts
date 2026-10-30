// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import {
  AnnotationStore,
  blobs,
  toBundle,
  type SourceDraft,
} from "../../state/annotations";
import { sha256Hex, upload } from "../../host/transfer";
import type { AnnotationContext } from "./sources";
import { verifiedSource } from "./derivatives";
vi.mock("../../host/transfer", async (original) => ({
  ...(await original<typeof import("../../host/transfer")>()),
  upload: vi.fn(),
}));
afterEach(() => {
  vi.restoreAllMocks();
  vi.clearAllMocks();
  localStorage.clear();
});
const blob = (text: string) =>
  Object.assign(new Blob([text], { type: "image/png" }), {
    arrayBuffer: async () => new TextEncoder().encode(text).buffer,
  });
async function source(data: Blob, key = "S1"): Promise<SourceDraft> {
  return {
    key,
    blobKey: key,
    artifactId: null,
    sha256: await sha256Hex(data),
    byteSize: data.size,
    mediaType: "image/png",
    width: 20,
    height: 20,
    durationMs: null,
    origin: { kind: "phone_import", original_name: null },
    capturedAt: 1,
    derivedFrom: null,
    derivation: null,
  };
}
it("refuses a same-size mutated blob before any upload, even after an earlier upload", async () => {
  const good = blob("reviewed"),
    changed = blob("tampered");
  const store = new AnnotationStore("verified");
  const draft = store.create("project", null);
  const first = await source(good),
    second = { ...(await source(good, "S2")), artifactId: "existing" };
  store.update(draft.id, (d) => ({ ...d, sources: [first, second] }));
  vi.spyOn(blobs, "get").mockImplementation(async (key) =>
    key === "S1" ? good : changed,
  );
  await expect(
    toBundle({} as AnnotationContext, store, draft.id),
  ).rejects.toThrow("changed since review");
  expect(upload).not.toHaveBeenCalled();
});
it("returns the exact verified immutable Blob, rejects size changes and missing hashes", async () => {
  const data = blob("reviewed");
  vi.spyOn(blobs, "get").mockResolvedValue(data);
  const original = await source(data);
  expect(await verifiedSource(original)).toBe(data);
  await expect(verifiedSource({ ...original, byteSize: 99 })).rejects.toThrow(
    "changed since review",
  );
  await expect(verifiedSource({ ...original, sha256: null })).rejects.toThrow(
    "changed since review",
  );
});
