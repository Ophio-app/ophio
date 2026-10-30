// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { AnnotationStore, useAnnotation, type SourceDraft } from "../../state/annotations";
import { Repro } from "./Repro";

vi.mock("./useSourceUrl", () => ({ useSourceUrl: () => ({ url: "blob:clip", error: null }) }));

const clip: SourceDraft = {
  key: "S1", blobKey: "clip", artifactId: null, sha256: "c".repeat(64), mediaType: "video/webm",
  width: 480, height: 270, durationMs: 10_000, included: false,
  origin: { kind: "clip", duration_ms: 10_000, target_id: null }, capturedAt: 1, derivedFrom: null, derivation: null,
};
const frame: SourceDraft = {
  ...clip, key: "S2", blobKey: "frame", mediaType: "image/png", durationMs: null, included: true,
  derivedFrom: "S1", derivation: { kind: "frame_from_clip", at_ms: 3500 },
};

beforeEach(() => localStorage.clear());
afterEach(cleanup);

function Steps({ store, id }: { store: AnnotationStore; id: string }) {
  const draft = useAnnotation(store, id);
  return draft ? <Repro draft={draft} store={store} onMark={() => {}} /> : null;
}

it("undoing a mark keeps the trim range typed after it", () => {
  const store = new AnnotationStore("trim-undo");
  const { id } = store.create("project", null);
  store.record(id, (d) => ({ ...d, sources: [clip, frame], nextSource: 3, mode: "steps" }));
  // A mark on the frame (Annotate's add), then the person trims the clip in Steps.
  store.update(id, (d) => ({
    ...d,
    marks: [{ mark_id: "a", label: "A", color: 0, source_key: "S2", created_at: 1, deleted: false,
      shape: { kind: "pin", at: { x: 0.5, y: 0.5, t: null } } }],
  }));
  render(<Steps store={store} id={id} />);
  fireEvent.change(screen.getByLabelText("Range start (seconds)"), { target: { value: "2" } });
  fireEvent.change(screen.getByLabelText("Range end (seconds)"), { target: { value: "6" } });
  expect(store.get()[id]?.clipEdits?.S1).toMatchObject({ start: "2", end: "6" });

  store.undo(id); // removes mark A
  expect(store.get()[id]?.marks).toEqual([]);
  // The trim range is not part of the edit history and must not roll back.
  expect(store.get()[id]?.clipEdits?.S1).toMatchObject({ start: "2", end: "6" });
  expect((screen.getByLabelText("Range start (seconds)") as HTMLInputElement).value).toBe("2");
  expect(store.get()[id]?.mode).toBe("steps");
});
