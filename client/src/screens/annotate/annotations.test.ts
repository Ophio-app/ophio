// SPDX-License-Identifier: Apache-2.0
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  AnnotationStore,
  blobs,
  contentIdentity,
  editedSinceSent,
  heldLabels,
  markLabel,
  markSent,
  migrateDraft,
  nextMarkIndex,
  summarize,
  toBundle,
  type AnnotationDraft,
  type SourceDraft,
} from "../../state/annotations";
import { UiStore } from "../../app/ui";
import type { Mark, SpeechSegment } from "../../protocol/types";
import type { AnnotationContext } from "./sources";
import { redactPixels, validRegion } from "./sources";
import { mergeSegment, suggestedLinks } from "./speech";
const source = (key = "S1"): SourceDraft => ({
  key,
  blobKey: null,
  artifactId: `artifact-${key}`,
  sha256: `sha-${key}`,
  mediaType: "image/png",
  width: 100,
  height: 100,
  durationMs: null,
  origin: { kind: "phone_import", original_name: null },
  capturedAt: 100,
  derivedFrom: null,
  derivation: null,
});
const mark = (label: string, created_at = 1000): Mark => ({
  mark_id: label,
  label,
  source_key: "S1",
  shape: { kind: "pin", at: { x: 0.2, y: 0.4, t: null } },
  color: 0,
  created_at,
  deleted: false,
});
function fixture() {
  const store = new AnnotationStore("test"),
    d = store.create("p1", null);
  store.update(d.id, (v) => ({
    ...v,
    createdAt: 0,
    sources: [source()],
    marks: [mark("A"), mark("B"), mark("C")],
    comments: [
      {
        comment_id: "comment",
        text: "Contrast",
        mark_ids: ["A", "B"],
        link_origins: ["manual", "manual"],
        origin: { kind: "typed" },
      },
    ],
  }));
  return { store, id: d.id };
}
const ctx = {
  platform: {},
  profileId: "test",
  session: { sendInput: vi.fn(), command: vi.fn() },
} as unknown as AnnotationContext;

it("recovers saved drawings beside new ones after an initial read failure", () => {
  const original = new AnnotationStore("read-failure");
  const saved = original.create("p", null);
  original.update(saved.id, (d) => ({ ...d, generalNote: "saved drawing" }));
  const read = vi.spyOn(Storage.prototype, "getItem").mockImplementationOnce(() => {
    throw new Error("Storage temporarily unavailable");
  });
  const store = new AnnotationStore("read-failure");
  read.mockRestore();
  const fresh = store.create("q", null);
  const reopened = new AnnotationStore("read-failure");
  expect(reopened.get()[saved.id]?.generalNote).toBe("saved drawing");
  expect(reopened.get()[fresh.id]).toBeDefined();
});
beforeEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});
describe("annotation durability and revision boundaries", () => {
  it("keeps any deleted comment target unresolved and undo restores its exact ID and links", () => {
    const { store, id } = fixture();
    store.update(id, (d) => ({
      ...d,
      marks: d.marks.map((m) =>
        m.label === "B" ? { ...m, deleted: true } : m,
      ),
    }));
    expect(summarize(store.get()[id] as AnnotationDraft).unresolved).toBe(1);
    store.undo(id);
    expect(store.get()[id]?.marks.find((m) => m.label === "B")?.mark_id).toBe(
      "B",
    );
    expect(store.get()[id]?.comments[0]?.mark_ids).toEqual(["A", "B"]);
    expect(summarize(store.get()[id] as AnnotationDraft).unresolved).toBe(0);
  });
  it("takes the next letter from the marks in the draft", () => {
    const { store, id } = fixture();
    const next = () => markLabel(nextMarkIndex(store.get()[id] as AnnotationDraft));
    const add = () =>
      store.update(id, (d) => ({ ...d, marks: [...d.marks, mark(next())] }));
    add();
    expect(next()).toBe("E");
    // An undone mark leaves the draft, so its letter is free again…
    store.undo(id);
    expect(next()).toBe("D");
    // …and after undoing every mark the next is A.
    store.undo(id);
    expect(store.get()[id]?.marks).toEqual([]);
    expect(next()).toBe("A");
    expect(markLabel(26)).toBe("AA");
    expect(markLabel(701)).toBe("ZZ");
  });
  it("keeps the letters of deleted and sent marks, also after reload", () => {
    const { store, id } = fixture();
    // A deleted mark stays in the draft: unresolved comments still name it.
    store.update(id, (d) => ({
      ...d,
      marks: d.marks.map((m) => (m.label === "C" ? { ...m, deleted: true } : m)),
    }));
    expect(nextMarkIndex(store.get()[id] as AnnotationDraft)).toBe(3);
    // Once sent, a letter stays taken even when an undo removes its mark.
    store.update(id, (d) => ({ ...d, marks: [...d.marks, mark("D")] }));
    markSent(store, id, 1, "sent");
    store.undo(id);
    expect(store.get()[id]?.marks.map((m) => m.label)).toEqual(["A", "B", "C"]);
    expect(markLabel(nextMarkIndex(new AnnotationStore("test").get()[id] as AnnotationDraft))).toBe("E");
  });
  it("keeps the letters a send took even when its marks are undone before it is confirmed", async () => {
    const { store, id } = fixture();
    store.update(id, (d) => ({ ...d, marks: [...d.marks, mark("D")] }));
    const bundle = await toBundle(ctx, store, id);
    const submitted = store.prepared.get(id)!;
    // Undo while nobody knows yet whether the computer took the send.
    store.undo(id);
    expect(store.get()[id]?.marks.map((m) => m.label)).toEqual(["A", "B", "C"]);
    markSent(store, id, bundle.revision, submitted.content, submitted.labels);
    expect(markLabel(nextMarkIndex(store.get()[id] as AnnotationDraft))).toBe("E");
    // A later confirmation of an earlier send never gives letters back.
    markSent(store, id, bundle.revision, submitted.content, 1);
    expect(markLabel(nextMarkIndex(store.get()[id] as AnnotationDraft))).toBe("E");
  });
  it("holds a send's letters from its snapshot on, while its images are prepared", async () => {
    const { store, id } = fixture();
    const ui = new UiStore("test");
    store.update(id, (d) => ({ ...d, marks: [...d.marks, mark("D")] }));
    const preparing = toBundle(ctx, store, id);
    store.undo(id);
    expect(markLabel(nextMarkIndex(store.get()[id] as AnnotationDraft, heldLabels(store, ui, id)))).toBe("E");
    await preparing;
  });
  it("holds a send's letters from its tap on, while its moments are still placed", () => {
    const { store, id } = fixture();
    const ui = new UiStore("test");
    store.update(id, (d) => ({ ...d, marks: [...d.marks, mark("D")] }));
    const sending = store.take(id)!;
    store.undo(id);
    expect(markLabel(nextMarkIndex(store.get()[id] as AnnotationDraft, heldLabels(store, ui, id)))).toBe("E");
    // Nothing sent: the letter is free again.
    sending.release();
    expect(markLabel(nextMarkIndex(store.get()[id] as AnnotationDraft, heldLabels(store, ui, id)))).toBe("D");
  });
  it("holds the letters of a send an older build saved without them", () => {
    const { store, id } = fixture();
    const ui = new UiStore("test");
    const content = contentIdentity(store.get()[id] as AnnotationDraft);
    ui.saveDraft("project", null, { text: "Fix these", annotationId: id,
      attempt: { phase: "unknown", at: 1, commandId: "c1", detail: "", text: "Fix these", annotationId: id, markup: { revision: 1, content } } });
    store.update(id, (d) => ({ ...d, marks: [] }));
    expect(markLabel(nextMarkIndex(store.get()[id] as AnnotationDraft, heldLabels(store, ui, id)))).toBe("D");
  });
  it("keeps the letters an older build had sent when it upgrades the draft", () => {
    const { store, id } = fixture(),
      d = store.get()[id] as AnnotationDraft;
    // Sent with A–D, then D was undone; older builds counted with nextLabel.
    const legacy = { ...d, sentRevision: 1, nextLabel: 4, sentLabels: undefined };
    expect(markLabel(nextMarkIndex(migrateDraft(legacy)))).toBe("E");
    // An unsent draft frees an undone letter, as a new one does.
    expect(markLabel(nextMarkIndex(migrateDraft({ ...legacy, sentRevision: 0 })))).toBe("D");
  });
  it("migrates older drafts using stable labels and source keys", () => {
    const { store, id } = fixture(),
      d = store.get()[id] as AnnotationDraft;
    const migrated = migrateDraft({
      ...d,
      marks: [mark("AA")],
      sources: [source("S9")],
      nextSource: 1,
    });
    expect(nextMarkIndex(migrated)).toBe(27);
    expect(migrated.nextSource).toBe(10);
    expect(migrated.commentState?.comment).toBe("manual");
  });
  it("reports metadata storage failure and preserves the in-memory draft", () => {
    const { store, id } = fixture();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });
    store.update(id, (d) => ({ ...d, generalNote: "Keep me" }));
    expect(store.persistence.get()).toContain("Not saved on this device");
    expect(store.get()[id]?.generalNote).toBe("Keep me");
  });
  it("restores sources, comments and unsent composer text without sending or input", () => {
    const { store, id } = fixture();
    store.record(id, (d) => ({
      ...d,
      composer: { text: "unfinished", targets: ["A"], general: false },
    }));
    const restored = new AnnotationStore("test").get()[id];
    expect(restored?.sources).toEqual(store.get()[id]?.sources);
    expect(restored?.composer?.text).toBe("unfinished");
    expect(ctx.session.sendInput).not.toHaveBeenCalled();
    expect(ctx.session.command).not.toHaveBeenCalled();
  });
  it("freezes outgoing revisions and makes post-send edits revision two", async () => {
    const { store, id } = fixture(),
      first = await toBundle(ctx, store, id);
    markSent(store, id, first.revision, store.prepared.get(id)!.content);
    expect(editedSinceSent(store.get()[id] as AnnotationDraft)).toBe(false);
    store.update(id, (d) => ({
      ...d,
      comments: d.comments.map((c) => ({ ...c, text: "new wording" })),
    }));
    const second = await toBundle(ctx, store, id);
    expect(first.comments[0]?.text).toBe("Contrast");
    expect(second.revision).toBe(2);
    expect(editedSinceSent(store.get()[id] as AnnotationDraft)).toBe(true);
  });
  it("does not clear a pending composer after a sent revision", () => {
    const { store, id } = fixture();
    markSent(store, id, 1, contentIdentity(store.get()[id] as AnnotationDraft));
    store.record(id, (d) => ({
      ...d,
      composer: { text: "still typing", targets: [], general: false },
    }));
    expect(editedSinceSent(store.get()[id] as AnnotationDraft)).toBe(true);
  });
  it("does not count loading a draft again as a change to it", () => {
    const { store, id } = fixture();
    markSent(store, id, 1, contentIdentity(store.get()[id] as AnnotationDraft));
    const empty = store.create("p1", null);
    const loaded = new AnnotationStore("test").get();
    expect(contentIdentity(loaded[id] as AnnotationDraft)).toBe(contentIdentity(store.get()[id] as AnnotationDraft));
    expect(editedSinceSent(loaded[id] as AnnotationDraft)).toBe(false);
    expect(contentIdentity(loaded[empty.id] as AnnotationDraft)).toBe(contentIdentity(empty));
  });
  it("recognises markup an earlier build sent by the identity that build gave it", () => {
    // How an earlier build named a draft's content: a new draft kept no record
    // of how its comments were linked.
    const earlierIdentity = (d: AnnotationDraft) =>
      JSON.stringify([
        d.sources.map(({ artifactId: _a, sha256: _sha, clipTransfer: _t, ...s }) => s),
        d.marks, d.comments, d.steps, d.generalNote, d.expected, d.actual, d.descriptions,
        d.commentState, d.speech, d.composer?.text.trim() ? d.composer : null,
      ]);
    const { store, id } = fixture();
    const bare = store.create("p1", null);
    store.update(bare.id, (d) => ({ ...d, sources: [source()], marks: [mark("A")] }));
    for (const draftId of [id, bare.id]) {
      const draft = store.get()[draftId] as AnnotationDraft;
      expect(draft.commentState).toBeUndefined();
      // Accepted before this build, and recovered from the send it saved.
      markSent(store, draftId, 1, earlierIdentity(draft));
      expect(editedSinceSent(store.get()[draftId] as AnnotationDraft)).toBe(false);
      // Or recorded by it and loaded now.
      store.record(draftId, (d) => ({ ...d, sentRevision: 2, sentContent: earlierIdentity(draft) }));
      expect(editedSinceSent(new AnnotationStore("test").get()[draftId] as AnnotationDraft)).toBe(false);
    }
  });
  it("excludes video and redacted originals, retains provenance and expected/actual", async () => {
    const { store, id } = fixture();
    store.update(id, (d) => ({
      ...d,
      marks: [],
      comments: [],
      sources: [
        { ...source(), included: false },
        {
          ...source("S2"),
          derivedFrom: "S1",
          derivation: {
            kind: "redact",
            regions: [{ x: 0.1, y: 0.1, w: 0.2, h: 0.2 }],
          },
        },
        { ...source("S3"), mediaType: "video/webm" },
      ],
      expected: "One card",
      actual: "Two cards",
      steps: [
        { index: 1, source_key: "S2", text: "Click", clip_range_ms: null },
      ],
    }));
    const bundle = await toBundle(ctx, store, id);
    expect(bundle.sources.map((s) => s.key)).toEqual(["S2"]);
    expect(bundle.sources[0]?.derived_from).toBe("S1");
    expect(bundle.general_note).toBe("Expected: One card\nActual: Two cards");
  });
  it("rejects unresolved links before any upload", async () => {
    const { store, id } = fixture();
    store.update(id, (d) => ({
      ...d,
      sources: d.sources.map((s) => ({ ...s, included: false })),
    }));
    await expect(toBundle(ctx, store, id)).rejects.toThrow(
      "Resolve comment targets",
    );
  });
  it("local viewport changes leave content identity, source hashes and geometry unchanged", () => {
    const { store, id } = fixture(),
      before = contentIdentity(store.get()[id] as AnnotationDraft);
    store.record(id, (d) => ({
      ...d,
      viewports: { S1: { zoom: 4, panX: 0.2, panY: -0.4 } },
    }));
    expect(contentIdentity(store.get()[id] as AnnotationDraft)).toBe(before);
    expect(ctx.session.sendInput).not.toHaveBeenCalled();
  });
  it("does not delete a blob still referenced by another draft", async () => {
    const { store, id } = fixture(),
      other = store.create("p1", null),
      remove = vi.spyOn(blobs, "remove").mockResolvedValue();
    for (const key of [id, other.id])
      store.update(key, (d) => ({
        ...d,
        sources: [{ ...source(), blobKey: "shared" }],
      }));
    await store.remove(id);
    expect(remove).not.toHaveBeenCalled();
  });
  it("preserves unsent typing when undoing a prior mark edit", () => {
    const { store, id } = fixture();
    store.update(id, (d) => ({ ...d, marks: [...d.marks, mark("D")] }));
    store.record(id, (d) => ({
      ...d,
      composer: { text: "Keep this draft", targets: ["A"], general: false },
    }));
    store.undo(id);
    expect(store.get()[id]?.composer?.text).toBe("Keep this draft");
    expect(store.get()[id]?.marks.length).toBe(3);
  });
  it("remembers sources removed by undo for eventual storage cleanup", async () => {
    const { store, id } = fixture(),
      remove = vi.spyOn(blobs, "remove").mockResolvedValue();
    store.update(id, (d) => ({
      ...d,
      sources: [...d.sources, { ...source("S2"), blobKey: "kept-for-redo" }],
    }));
    store.undo(id);
    expect(store.get()[id]?.retainedBlobKeys).toContain("kept-for-redo");
    await store.remove(id);
    expect(remove).toHaveBeenCalledWith("kept-for-redo");
  });
  it("marks the prepared revision sent without erasing edits made during review", async () => {
    const { store, id } = fixture(),
      bundle = await toBundle(ctx, store, id);
    store.update(id, (d) => ({ ...d, generalNote: "A later edit" }));
    markSent(store, id, bundle.revision, store.prepared.get(id)!.content);
    expect(bundle.general_note).toBeNull();
    expect(editedSinceSent(store.get()[id] as AnnotationDraft)).toBe(true);
  });
});
describe("speech associations and revisions", () => {
  const segment: SpeechSegment = {
    segment_id: "s1",
    start_ms: 3000,
    end_ms: 5000,
    text: "space",
    revision: 1,
    is_final: false,
  };
  it("uses explicit before-speech tolerance without matching future or deleted marks", () => {
    expect(
      suggestedLinks(
        segment,
        [
          mark("A", 1200),
          mark("B", 1199),
          mark("C", 5001),
          { ...mark("D", 4000), deleted: true },
        ],
        0,
      ),
    ).toEqual(["A"]);
  });
  it("preserves manual wording and links across delayed suggestions and source switching", () => {
    const { store, id } = fixture();
    let d = {
      ...(store.get()[id] as AnnotationDraft),
      speech: {
        session_id: "session",
        segments: [],
        audio_artifact: null,
        transcript_revision: 0,
      },
    };
    let next = mergeSegment(d, segment);
    const comment = next.comments.at(-1);
    expect(comment).toBeDefined();
    if (!comment) return;
    next = {
      ...next,
      selectedSource: "S2",
      comments: next.comments.map((c) =>
        c.comment_id === comment.comment_id
          ? {
              ...c,
              text: "My wording",
              mark_ids: ["A", "C"],
              link_origins: ["manual", "manual"],
              origin: { kind: "spoken", segment_ids: ["s1"], edited: true },
            }
          : c,
      ),
      commentState: { ...next.commentState, [comment.comment_id]: "manual" },
    };
    next = mergeSegment(next, {
      ...segment,
      revision: 2,
      text: "later correction",
      is_final: true,
    });
    expect(next.comments.at(-1)?.text).toBe("My wording");
    expect(next.comments.at(-1)?.mark_ids).toEqual(["A", "C"]);
    expect(next.speechSuggestions?.s1?.text).toBe("later correction");
    expect(mergeSegment(next, segment)).toBe(next);
  });
  it("does not recreate a spoken comment the person deleted", () => {
    const { store, id } = fixture(),
      d = {
        ...(store.get()[id] as AnnotationDraft),
        speech: {
          session_id: "s",
          segments: [segment],
          audio_artifact: null,
          transcript_revision: 1,
        },
        deletedSpeechSegments: [segment.segment_id],
      };
    expect(mergeSegment(d, { ...segment, revision: 5 })).toBe(d);
  });
  it("sends only reviewed wording and no retained audio", async () => {
    const { store, id } = fixture();
    store.update(id, (d) => ({
      ...d,
      speech: {
        session_id: "s",
        segments: [segment],
        audio_artifact: "private-audio",
        transcript_revision: 1,
      },
      comments: [
        {
          comment_id: "spoken",
          text: "My wording",
          mark_ids: ["A"],
          link_origins: ["manual"],
          origin: {
            kind: "spoken",
            segment_ids: [segment.segment_id],
            edited: true,
          },
        },
      ],
      commentState: { spoken: "manual" },
    }));
    const bundle = await toBundle(ctx, store, id);
    expect(bundle.speech?.audio_artifact).toBeNull();
    expect(bundle.speech?.segments[0]?.text).toBe("My wording");
  });
});
describe("solid redaction before encoding", () => {
  it("replaces every pixel of fractional edge regions with opaque solid fill", () => {
    const pixels = new Uint8ClampedArray(4 * 4 * 4).fill(127);
    redactPixels(pixels, 4, 4, [{ x: 0.3, y: 0.3, w: 0.4, h: 0.4 }]);
    expect([...pixels.slice((1 * 4 + 1) * 4, (1 * 4 + 1) * 4 + 4)]).toEqual([
      0, 0, 0, 255,
    ]);
    expect([...pixels.slice(0, 4)]).toEqual([127, 127, 127, 127]);
    expect(validRegion({ x: 0.9, y: 0, w: 0.2, h: 0.5 })).toBe(false);
  });
});
