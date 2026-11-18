// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { UiStore } from "../../app/ui";
import { toast } from "../../design/Toast";
import { AnnotationStore, blobs, type SourceDraft } from "../../state/annotations";
import { forgetRemovedMarkup, removeMarkup, resumeRemovedMarkup } from "./removedMarkup";

vi.mock("../../design/Toast", () => ({ toast: vi.fn(() => () => {}) }));

let serial = 0;
const roots: UiStore[] = [];
beforeEach(() => {
  localStorage.clear();
  vi.mocked(toast).mockClear();
  vi.spyOn(blobs, "remove").mockResolvedValue(undefined);
});
afterEach(() => {
  for (const root of roots.splice(0)) {
    forgetRemovedMarkup(root);
    for (const view of root.navigationStores()) view.retire();
  }
  vi.restoreAllMocks();
});

function setup() {
  const profileId = `split-removed-markup-${++serial}`;
  const root = new UiStore(profileId);
  roots.push(root);
  const right = root.fork("right");
  const store = new AnnotationStore(profileId);
  const { id } = store.create("project", null);
  const image: SourceDraft = {
    key: "S1", blobKey: `${id}-image`, artifactId: null, sha256: "a".repeat(64), mediaType: "image/png", width: 600, height: 400,
    durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 100, derivedFrom: null, derivation: null,
  };
  store.update(id, (draft) => ({ ...draft, sources: [image], nextSource: 2, generalNote: "Tighten the spacing" }));
  root.saveDraft("project", null, { text: "Look at this", annotationId: id });
  return { profileId, root, right, store, id, image };
}

function expireUndo() {
  const offered = vi.mocked(toast).mock.calls.findLast(([text]) => text === "Markup removed");
  if (!offered?.[1]?.expire) throw new Error("Undo was not offered");
  offered[1].expire();
}

function undoRemoval() {
  const offered = vi.mocked(toast).mock.calls.findLast(([text]) => text === "Markup removed");
  if (!offered?.[1]?.action) throw new Error("Undo was not offered");
  offered[1].action.run();
}

function restartRemoved() {
  const initial = setup();
  initial.right.push({ screen: "annotate", annotationId: initial.id });
  removeMarkup(initial.root, initial.store, "project", null, initial.id);
  expireUndo();
  initial.root.persistNow();
  initial.right.persistNow();
  forgetRemovedMarkup(initial.root);
  for (const view of initial.root.navigationStores()) view.retire();

  const root = new UiStore(initial.profileId);
  roots.push(root);
  const right = root.fork("right");
  const store = new AnnotationStore(initial.profileId);
  expect(root.route()).toEqual({ screen: "talk" });
  expect(right.route()).toMatchObject({ screen: "annotate", annotationId: initial.id });
  resumeRemovedMarkup(root, store);
  return { ...initial, root, right, store };
}

it("retains removed markup and its image on restart while only the right editor has it open", async () => {
  const { root, right, store, id, image } = restartRemoved();
  expect(store.get()[id]).toBeDefined();
  expect(root.draft("project", null)?.annotationId).toBeNull();
  expect(blobs.remove).not.toHaveBeenCalled();

  right.back();
  await Promise.resolve();

  expect(store.get()[id]).toBeUndefined();
  expect(blobs.remove).toHaveBeenCalledExactlyOnceWith(image.blobKey);
  expect(root.get().removedMarkup).toEqual({});
  expect(right.get().removedMarkup).toEqual({});
});

it.each(["left", "right"] as const)("a removal from the %s side waits until the other side's editor leaves", async (side) => {
  const { root, right, store, id, image } = setup();
  const removing = side === "left" ? root : right;
  const editing = side === "left" ? right : root;
  editing.push({ screen: "annotate", annotationId: id });
  removeMarkup(removing, store, "project", null, id);
  expireUndo();

  expect(store.get()[id]).toBeDefined();
  expect(blobs.remove).not.toHaveBeenCalled();
  expect(root.draft("project", null)?.annotationId).toBeNull();
  expect(right.draft("project", null)?.annotationId).toBeNull();
  editing.back();
  await Promise.resolve();

  expect(store.get()[id]).toBeUndefined();
  expect(blobs.remove).toHaveBeenCalledExactlyOnceWith(image.blobKey);
});

it("waits for the last matching editor, not merely the canonical editor, before deleting the image", async () => {
  const { root, right, store, id, image } = setup();
  root.push({ screen: "annotate", annotationId: id });
  right.push({ screen: "annotate", annotationId: id });
  const leftSubscription = vi.spyOn(root, "subscribe");
  const rightSubscription = vi.spyOn(right, "subscribe");
  const annotationSubscription = vi.spyOn(store, "subscribe");
  removeMarkup(root, store, "project", null, id);
  resumeRemovedMarkup(root, store);
  resumeRemovedMarkup(right, store);
  expireUndo();
  expect(leftSubscription).toHaveBeenCalledTimes(1);
  expect(rightSubscription).toHaveBeenCalledTimes(1);
  expect(annotationSubscription).toHaveBeenCalledTimes(1);

  root.back();
  expect(store.get()[id]).toBeDefined();
  expect(blobs.remove).not.toHaveBeenCalled();
  right.back();
  await Promise.resolve();

  expect(store.get()[id]).toBeUndefined();
  expect(blobs.remove).toHaveBeenCalledExactlyOnceWith(image.blobKey);
});

it.each([false, true])("reattaches edits made in the right editor, including after restart (%s)", async (restart) => {
  const { root, right, store, id } = restart ? restartRemoved() : setup();
  if (!restart) {
    right.push({ screen: "annotate", annotationId: id });
    removeMarkup(root, store, "project", null, id);
    expireUndo();
  }
  store.update(id, (draft) => ({ ...draft, generalNote: "Keep the newer margin edits" }));

  expect(root.draft("project", null)).toMatchObject({ text: "Look at this", annotationId: id });
  expect(right.draft("project", null)).toBe(root.draft("project", null));
  expect(root.get().removedMarkup).toEqual({});
  expect(right.get().removedMarkup).toEqual({});
  right.back();
  await Promise.resolve();

  expect(store.get()[id]?.generalNote).toBe("Keep the newer margin edits");
  expect(blobs.remove).not.toHaveBeenCalled();
});

it("Undo does not replace newer empty markup being edited on the other side", async () => {
  const { root, right, store, id, image } = setup();
  removeMarkup(root, store, "project", null, id);
  const newer = store.create("project", null).id;
  root.saveDraft("project", null, { annotationId: newer });
  right.push({ screen: "annotate", annotationId: newer });

  undoRemoval();
  await Promise.resolve();

  expect(root.draft("project", null)?.annotationId).toBe(newer);
  expect(store.get()[newer]).toBeDefined();
  expect(store.get()[id]).toBeUndefined();
  expect(blobs.remove).toHaveBeenCalledExactlyOnceWith(image.blobKey);
  expect(toast).toHaveBeenCalledWith("Markup not put back: the message has newer markup");
});

it.each(["left", "right"] as const)("a cached but closed %s editor no longer retains removed images", async side => {
  const { root, right, store, id, image } = setup();
  const editing = side === "left" ? root : right;
  editing.push({ screen: "annotate", annotationId: id });
  removeMarkup(root, store, "project", null, id);
  expireUndo();
  expect(store.get()[id]).toBeDefined();
  editing.navigationRetained.set(false);
  await Promise.resolve();
  expect(editing.route()).toMatchObject({ screen: "annotate", annotationId: id });
  expect(store.get()[id]).toBeUndefined();
  expect(blobs.remove).toHaveBeenCalledExactlyOnceWith(image.blobKey);
  expect(root.get().removedMarkup).toEqual({});
});

it("does not retain removed images for a closed side restored from disk", async () => {
  const initial = setup();
  initial.right.push({ screen: "annotate", annotationId: initial.id });
  removeMarkup(initial.root, initial.store, "project", null, initial.id);
  expireUndo();
  for (const view of initial.root.navigationStores()) view.retire();
  forgetRemovedMarkup(initial.root);
  const root = new UiStore(initial.profileId);
  roots.push(root);
  const right = root.fork("right");
  right.navigationRetained.set(false);
  const store = new AnnotationStore(initial.profileId);
  expect(right.route()).toMatchObject({ screen: "annotate", annotationId: initial.id });
  resumeRemovedMarkup(root, store);
  await Promise.resolve();
  expect(store.get()[initial.id]).toBeUndefined();
  expect(blobs.remove).toHaveBeenCalledExactlyOnceWith(initial.image.blobKey);
});

it("observes a side forked while an existing editor keeps removal pending", async () => {
  const { root, store, id, image } = setup();
  root.push({ screen: "annotate", annotationId: id });
  removeMarkup(root, store, "project", null, id);
  expireUndo();
  const later = root.fork("later");
  expect(later.route()).toEqual({ screen: "talk" });
  // A fresh side no longer clones an editor. Open one explicitly to check
  // that the existing cleanup observer discovers newly participating views.
  later.push({ screen: "annotate", annotationId: id });
  expect(later.route()).toMatchObject({ screen: "annotate", annotationId: id });

  root.back();
  expect(store.get()[id]).toBeDefined();
  expect(blobs.remove).not.toHaveBeenCalled();
  later.back();
  await Promise.resolve();

  expect(store.get()[id]).toBeUndefined();
  expect(blobs.remove).toHaveBeenCalledExactlyOnceWith(image.blobKey);
});

it("keeps edited markup restored on the left when the right side removes its replacement and then types", async () => {
  const { root, right, store, id } = setup();
  root.push({ screen: "annotate", annotationId: id });
  removeMarkup(right, store, "project", null, id);
  const replacement = store.create("project", null).id;
  right.saveDraft("project", null, { annotationId: replacement });
  store.update(id, (draft) => ({ ...draft, generalNote: "Edited after it was removed" }));
  expect(root.draft("project", null)?.annotationId).toBe(replacement);

  // The original's clean-up restores it while this removal is still notifying.
  removeMarkup(right, store, "project", null, replacement);
  expect(root.draft("project", null)?.annotationId).toBe(id);
  expect(right.draft("project", null)?.annotationId).toBe(id);
  expect(right.get().removedMarkup).toEqual(root.get().removedMarkup);

  right.saveDraft("project", null, { text: "Look at this again" });
  await Promise.resolve();
  expect(root.draft("project", null)).toMatchObject({ text: "Look at this again", annotationId: id });
  expect(store.get()[id]?.generalNote).toBe("Edited after it was removed");
});
