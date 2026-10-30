// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it, vi } from "vitest";
import { UiStore } from "../../app/ui";
import { annotationStore, type AnnotationStore } from "../../state/annotations";
import { previewAnnotation } from "./previewAnnotation";
import type { PreviewDraft } from "./previewDrafts";
import type { ImportSource } from "../annotate/sources";

vi.mock("../annotate/sources", () => ({ importSource: vi.fn(async (store: AnnotationStore, id: string, source: ImportSource, extra: object) => {
  store.update(id, d => ({ ...d, sources: [...d.sources, { key: `S${d.nextSource}`, blobKey: "retained", artifactId: null,
    sha256: "verified", width: 600, height: 400, mediaType: source.mediaType, durationMs: null,
    capturedAt: source.capturedAt ?? 0, origin: source.origin, derivedFrom: null, derivation: null, ...extra }], nextSource: d.nextSource + 1 }));
}) }));
beforeEach(() => localStorage.clear());
it("preview Source retains its origin and attaches to the original task draft without replacing text", async () => {
  const ui = new UiStore("preview-test"), store = annotationStore("preview-test");
  ui.saveDraft("project", "task", { text: "Unsent correction" });
  const draft: PreviewDraft = { id: "picture", profileId: "preview-test", projectId: "project", taskId: "task", previewId: "preview", previewLabel: "Garden", computerId: "host", computerName: "studio", blob: new Blob(["picture"], { type: "image/png" }), at: 1000, artifactId: "artifact" };
  const id = await previewAnnotation(draft, ui);
  expect(ui.draft("project", "task")).toMatchObject({ text: "Unsent correction", annotationId: id });
  expect(store.get()[id]?.sources[0]).toMatchObject({ artifactId: "artifact", blobKey: "retained", capturedAt: 1000, origin: { kind: "preview_capture", capture_id: "picture", host_id: "host", computer_name: "studio", preview_id: "preview", preview_label: "Garden" } });
  expect(await previewAnnotation(draft, ui)).toBe(id);
  expect(store.get()[id]?.sources).toHaveLength(1);
  expect(ui.route()).toEqual({ screen: "talk" });
});

it("Send again restores the retained picture's marks after its composer was consumed", async () => {
  const ui = new UiStore("preview-retry"), store = annotationStore("preview-retry");
  const draft: PreviewDraft = { id: "retry-picture", profileId: "preview-retry", projectId: "project", taskId: "task", previewId: "preview", computerId: "host", computerName: "studio", blob: new Blob(["picture"], { type: "image/png" }), at: 1000 };
  const id = await previewAnnotation(draft, ui);
  store.update(id, d => ({ ...d, marks: [{ mark_id: "mark", created_at: 1000, label: "A", color: 0, source_key: "S1", shape: { kind: "rect", rect: { x: 0.1, y: 0.1, w: 0.2, h: 0.2 } }, deleted: false }], generalNote: "Keep this note" }));
  ui.clearDraft("project", "task");
  expect(await previewAnnotation(draft, ui)).toBe(id);
  expect(store.get()[id]?.marks[0]?.label).toBe("A");
  expect(store.get()[id]?.generalNote).toBe("Keep this note");
  expect(ui.draft("project", "task")?.annotationId).toBe(id);
});
