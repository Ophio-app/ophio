// SPDX-License-Identifier: Apache-2.0
//
// Phone-local annotation drafts. A draft holds source images or
// clips, labelled marks, linked comments and reproduction steps. It stays on
// the phone until the person sends it through review, when each
// source is uploaded as an artifact and the draft becomes an immutable
// AnnotationBundle revision. Closing or losing the connection never sends
// or discards it.
//
// Metadata is kept per paired computer in localStorage; source bytes are
// kept in IndexedDB under `blobKey` so large images do not crowd out the
// interface state.

import { useCallback } from "react";
import type { HostSession } from "../host/session";
import type { Platform } from "../host/link";
import { TransferError, upload } from "../host/transfer";
import type {
  AnnotationBundle,
  AnnotationSource,
  Comment,
  Derivation,
  Mark,
  ReproStep,
  SourceOrigin,
  SpeechTrack,
  SpeechSegment,
} from "../protocol/types";
import type { UiStore } from "../app/ui";
import { Store, useStore } from "./store";
import { readSavedJson, UnpreservedJson } from "./savedJson";
import { uuidv7 } from "../protocol/ids";
import { reproductionProblem } from "../screens/annotate/trim";
import { verifyOutgoing, type DerivativeIdentity } from "../screens/annotate/derivatives";

export interface SourceDraft {
  /** "S1", "S2"… stable within the draft; never reused after removal. */
  key: string;
  /** IndexedDB key of the exact bytes; null once only the artifact remains. */
  blobKey: string | null;
  /** Set when the bytes already exist on the computer (host capture, or sent before). */
  artifactId: string | null;
  sha256: string | null;
  mediaType: string;
  width: number;
  height: number;
  /** Clip length; null for still images. */
  durationMs: number | null;
  origin: SourceOrigin;
  capturedAt: number;
  derivedFrom: string | null;
  derivation: Derivation | null;
  /** Explicit outgoing selection; videos are always excluded. */
  included?: boolean;
  watchedAt?: number;
  captureGeometry?: import("../protocol/types").CaptureGeometry;
  geometryChanged?: boolean;
  byteSize?: number;
  reproduction?: DerivativeIdentity;
  clipTransfer?: { transferId: string; artifactId: string | null };
}

export interface AnnotationDraft {
  id: string;
  projectId: string;
  /** null = for a new task. */
  taskId: string | null;
  createdAt: number;
  updatedAt: number;
  sources: SourceDraft[];
  /** Deleted marks stay (flagged) so comments that pointed at them show as unresolved. */
  marks: Mark[];
  comments: Comment[];
  steps: ReproStep[];
  generalNote: string | null;
  speech: SpeechTrack | null;
  /** Next source number: 1 → "S1". */
  nextSource: number;
  /** Bundle id reused across revisions of the same draft. */
  bundleId: string;
  /** Last revision sent; the next send is revision + 1. */
  sentRevision: number;
  sentContent?: string;
  /** Label index after the sent revisions' marks. Their letters stay taken
   * even if an undo removes those marks, so a bundle never reassigns one. */
  sentLabels?: number;
  /** Label index after the marks of each send not yet answered, by command
   * id. Held here rather than by the send's attempt, which a later send on
   * the same message replaces, until that send is known not to have arrived
   * or a confirmation reserves its letters (heldLabels). */
  held?: Record<string, number>;
  retainedBlobKeys?: string[];
  deletedSpeechSegments?: string[];
  pauseUnconfirmed?: boolean;
  mode?: "annotate" | "steps";
  expected?: string;
  actual?: string;
  descriptions?: Record<string, string>;
  commentState?: Record<
    string,
    "general" | "manual" | "suggested" | "unresolved"
  >;
  suggestions?: Record<string, string[]>;
  speechSuggestions?: Record<string, SpeechSegment>;
  composer?: { text: string; targets: string[]; general: boolean };
  selectedSource?: string;
  selectedClip?: string;
  clipEdits?: Record<string, { start: string; end: string; at: number }>;
  viewports?: Record<string, { zoom: number; panX: number; panY: number }>;
  /** How each video the agent showed was played, by source key: what a
   * comment made while it played was said over. */
  watch?: Record<string, WatchAnchor[]>;
  /** Comments made over a video that go without their moment, by comment
   * id: the message had no room for another still, or none could be made.
   * Kept with where in the video it was tried, so a moment that changes is
   * tried again. */
  momentless?: Record<string, { atMs: number; why: "full" | "failed" }>;
}

/** The video's position at a moment on the drawing clock, and whether it was
 * playing on from there. `watching: false` marks the person leaving it. */
export interface WatchAnchor {
  clock: number;
  atMs: number;
  playing: boolean;
  rate: number;
  watching: boolean;
}

export interface AnnotationSummary {
  sources: number;
  marks: number;
  comments: number;
  steps: number;
  /** Comments with any missing, excluded, deleted, or unconfirmed target. */
  unresolved: number;
}

/** "A"…"Z", "AA"… */
/** Milliseconds since a draft began, as the protocol's u32 fields carry them.
 * The phone's clock can be set back after a draft is made, and a draft can
 * outlive the range; a value outside it makes the message unreadable. */
export const draftMs = (ms: number): number => Math.min(2 ** 32 - 1, Math.max(0, ms));

export function markLabel(n: number): string {
  let s = "";
  n += 1;
  while (n > 0) {
    const r = (n - 1) % 26;
    s = String.fromCharCode(65 + r) + s;
    n = Math.floor((n - 1) / 26);
  }
  return s;
}

export function summarize(d: AnnotationDraft): AnnotationSummary {
  const included = new Set(outgoingSources(d).map((s) => s.key));
  const live = new Set(
    d.marks
      .filter((m) => !m.deleted && included.has(m.source_key))
      .map((m) => m.mark_id),
  );
  return {
    sources: outgoingSources(d).length,
    marks: live.size,
    comments: d.comments.filter((c) => c.text.trim()).length,
    steps: d.steps.length,
    unresolved: d.comments.filter((c) => commentUnresolved(d, c)).length,
  };
}

/** Whether the person has put anything into the draft, whether or not it
 * would be sent: a picture or clip, left out or not, words anywhere, a step
 * or a recording. */
export function holdsWork(d: AnnotationDraft): boolean {
  const words = (s?: string | null) => !!s?.trim();
  return (
    d.sources.length > 0 ||
    d.comments.some((c) => words(c.text)) ||
    d.steps.length > 0 ||
    !!d.speech ||
    [d.generalNote, d.composer?.text, d.expected, d.actual].some(words) ||
    Object.values(d.descriptions ?? {}).some(words)
  );
}

/** Short human description, e.g. "2 images · 3 marks · 2 comments". */
export function describe(s: AnnotationSummary): string {
  const n = (k: number, one: string, many: string) =>
    k ? `${k} ${k === 1 ? one : many}` : "";
  return (
    [
      n(s.sources, "image", "images"),
      n(s.marks, "mark", "marks"),
      n(s.comments, "comment", "comments"),
      n(s.steps, "step", "steps"),
    ]
      .filter(Boolean)
      .join(" · ") || "Empty"
  );
}

// ---- Blob storage -----------------------------------------------------------

const DB = "annotation-sources";

function db(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => r.result.createObjectStore("blobs");
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}

async function tx<T>(
  mode: IDBTransactionMode,
  fn: (s: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  const d = await db();
  return new Promise((resolve, reject) => {
    const t = d.transaction("blobs", mode);
    const req = fn(t.objectStore("blobs"));
    t.oncomplete = () => {
      d.close();
      resolve(req.result);
    };
    t.onerror = t.onabort = () => {
      d.close();
      reject(t.error ?? new Error("Source storage was interrupted."));
    };
  });
}

export const blobs = {
  async put(blob: Blob): Promise<string> {
    const key = uuidv7(Date.now());
    await tx("readwrite", (s) => s.put(blob, key));
    return key;
  },
  get: (key: string): Promise<Blob | null> =>
    tx("readonly", (s) => s.get(key)).then(
      (b) => (b as Blob | undefined) ?? null,
    ),
  remove: (key: string): Promise<void> =>
    tx("readwrite", (s) => s.delete(key)).then(() => undefined),
};

// ---- Drafts -----------------------------------------------------------------

type Drafts = Record<string, AnnotationDraft>;

/** The bundle's limit on sources. */
export const MAX_SOURCES = 16;

export function outgoingSources(d: AnnotationDraft): SourceDraft[] {
  return d.sources.filter(
    (s) => s.included !== false && s.mediaType.startsWith("image/"),
  );
}

export function commentUnresolved(d: AnnotationDraft, c: Comment): boolean {
  const state = d.commentState?.[c.comment_id];
  if (state === "unresolved" || state === "suggested") return true;
  const sources = new Set(outgoingSources(d).map((s) => s.key));
  return c.mark_ids.some(
    (id) =>
      !d.marks.some(
        (m) => m.mark_id === id && !m.deleted && sources.has(m.source_key),
      ),
  );
}

/** Content identity excludes upload bookkeeping and local viewing. */
export function contentIdentity(d: AnnotationDraft): string {
  return JSON.stringify([
    d.sources.map(
      ({ artifactId: _a, sha256: _sha, clipTransfer: _t, ...s }) => s,
    ),
    d.marks,
    d.comments,
    d.steps,
    d.generalNote,
    d.expected,
    d.actual,
    d.descriptions,
    commentStates(d),
    d.speech,
    d.composer?.text.trim() ? d.composer : null,
  ]);
}
export const editedSinceSent = (d: AnnotationDraft): boolean =>
  !d.sentRevision || d.sentContent !== contentIdentity(d);

/** A content identity as this build gives it. An earlier build left how a new
 * draft's comments were linked out of its identity; that is derived the way
 * loading derives it, so the content it sent is still the same content. */
function currentIdentity(content: string): string {
  try {
    const parts = JSON.parse(content) as unknown[];
    if (!Array.isArray(parts) || parts[8] !== null || !Array.isArray(parts[2])) return content;
    parts[8] = commentStates({ comments: parts[2] as Comment[] });
    return JSON.stringify(parts);
  } catch {
    return content;
  }
}

/** Older drafts remain usable; the source counter includes removed sources,
 * and a sent draft keeps the letters its old counter gave. */
export function migrateDraft(d: AnnotationDraft): AnnotationDraft {
  return {
    ...d,
    steps: d.steps ?? [],
    speech: d.speech ?? null,
    generalNote: d.generalNote ?? null,
    sentRevision: d.sentRevision ?? 0,
    // Older builds kept one counter of every letter given; once a revision
    // was sent, those letters may be in it.
    sentLabels:
      d.sentLabels ??
      (d.sentRevision ? (d as { nextLabel?: number }).nextLabel : undefined),
    nextSource: Math.max(
      d.nextSource ?? 1,
      ...d.sources
        .map((s) => Number(s.key.slice(1)) + 1)
        .filter(Number.isFinite),
    ),
    commentState: commentStates(d),
    ...(d.sentContent === undefined ? {} : { sentContent: currentIdentity(d.sentContent) }),
  };
}
/** How each comment's links were made. Drafts from before this was kept
 * take it from their marks, the same when the draft is loaded again, so
 * loading it is not a change to its content. */
function commentStates(d: Pick<AnnotationDraft, "comments" | "commentState">): NonNullable<AnnotationDraft["commentState"]> {
  return (
    d.commentState ??
    Object.fromEntries(
      d.comments.map((c) => [
        c.comment_id,
        c.mark_ids.length ? ("manual" as const) : ("general" as const),
      ]),
    )
  );
}
function labelIndex(label: string): number {
  return [...label].reduce((n, c) => n * 26 + c.charCodeAt(0) - 64, 0) - 1;
}
/** The next mark's label index, after the highest label in the draft. A
 * deleted mark keeps its letter, since unresolved comments still name it; an
 * unsent mark removed by undo frees its letter, so after undoing every mark
 * of a new draft the next is "A". `held` is the index after letters a send
 * whose outcome is not yet known may have given the computer (heldLabels). */
export function nextMarkIndex(d: AnnotationDraft, held = 0): number {
  return Math.max(
    d.sentLabels ?? 0,
    held,
    ...d.marks.map((m) => labelIndex(m.label) + 1),
  );
}

/** The label index after the marks of a submitted revision. A send recorded
 * before the index was kept still has its marks in its content identity. */
function submittedLabels(markup: { content: string; labels?: number }): number {
  if (markup.labels !== undefined) return markup.labels;
  try {
    const marks = (JSON.parse(markup.content) as unknown[])[1] as Mark[];
    return Math.max(0, ...marks.map((m) => labelIndex(m.label) + 1));
  } catch {
    return 0;
  }
}

/** The label index after the letters of draft `id` that a send not yet
 * answered may have given the computer: one taken at its tap or being
 * prepared, or one saved, which the draft holds across a restart. A send an earlier build saved is
 * read from its attempt. A new mark takes none of them, so a bundle never
 * names two marks alike; once the send is known not to have arrived, they
 * are free again. */
export function heldLabels(
  store: AnnotationStore,
  ui: Pick<UiStore, "get">,
  id: string,
): number {
  const { drafts, savedDrafts } = ui.get();
  let held = Math.max(
    store.preparing.get(id) ?? 0,
    ...store.taken(id).map((d) => nextMarkIndex(d)),
    ...Object.values(store.get()[id]?.held ?? {}),
  );
  for (const d of [...Object.values(drafts), ...Object.values(savedDrafts).flat()]) {
    const a = d.attempt;
    if (a?.annotationId === id && a.markup && (a.phase === "sending" || a.phase === "unknown"))
      held = Math.max(held, submittedLabels(a.markup));
  }
  return held;
}

const RECOVERED = "Saved drawings were damaged. A copy has been kept on this device for recovery. New changes can be saved again.";
const readError = (error: unknown) => error instanceof UnpreservedJson
  ? "Saved drawings are damaged and could not be set aside. Keep the app open; new drawings are only held in memory."
  : "Saved drawings could not be read. Keep the app open; new drawings are only held in memory until storage recovers.";

function loadDrafts(key: string): { drafts: Drafts; recovered: boolean } {
  const { value, recovered } = readSavedJson(localStorage, key);
  const drafts = Object.fromEntries(
    Object.entries(value && typeof value === "object" ? value : {})
      .filter(([, d]) => d && Array.isArray(d.sources) && Array.isArray(d.marks) && Array.isArray(d.comments))
      .map(([id, d]) => [id, migrateDraft(d)]),
  );
  return { drafts, recovered };
}

export class AnnotationStore extends Store<Drafts> {
  #key: string;
  #unread = false;
  #persisting = false;
  readonly recovery = new Store<string | null>(null);
  readonly persistence = new Store<string | null>(null);
  #undo = new Map<string, AnnotationDraft[]>();
  #redo = new Map<string, AnnotationDraft[]>();
  /** Each draft's last submitted revision: its identity and the label index
   * after its marks, which a confirmation reserves. */
  readonly prepared = new Map<string, { revision: number; content: string; labels: number }>();
  /** The label index after the marks of a revision being prepared, held
   * from its snapshot until the caller saves the send (heldLabels). */
  readonly preparing = new Map<string, number>();
  /** Versions of each draft taken to send (take). */
  #taken = new Map<string, Set<{ draft: AnnotationDraft }>>();
  #persist: boolean;

  constructor(profileId: string, persist = true) {
    const key = `annotations.${profileId}`;
    let drafts: Drafts = {}, error: string | null = null, recovered = false;
    // A report's markup lives only as long as the report sheet.
    if (persist) {
      try { ({ drafts, recovered } = loadDrafts(key)); }
      catch (e) { error = readError(e); }
    }
    super(drafts);
    this.#key = key;
    this.#persist = persist;
    this.#unread = error !== null;
    this.persistence.set(error);
    if (recovered) this.recovery.set(RECOVERED);
    this.subscribe(() => { if (!this.#persisting) this.persistNow(); });
  }

  persistNow(): boolean {
    if (!this.#persist) return true;
    if (this.#unread) {
      try {
        const loaded = loadDrafts(this.#key);
        this.#unread = false;
        if (loaded.recovered) this.recovery.set(RECOVERED);
        this.#persisting = true;
        try { this.set({ ...loaded.drafts, ...this.get() }); }
        finally { this.#persisting = false; }
      } catch (error) {
        this.persistence.set(readError(error));
        return false;
      }
    }
    try {
      localStorage.setItem(this.#key, JSON.stringify(this.get()));
      this.persistence.set(null);
      return true;
    } catch {
      this.persistence.set("Not saved on this device. Keep the app open and retry when storage is available.");
      return false;
    }
  }

  create(projectId: string, taskId: string | null): AnnotationDraft {
    const now = Date.now();
    const d: AnnotationDraft = {
      id: uuidv7(now),
      projectId,
      taskId,
      createdAt: now,
      updatedAt: now,
      sources: [],
      marks: [],
      comments: [],
      steps: [],
      generalNote: null,
      speech: null,
      nextSource: 1,
      bundleId: uuidv7(now),
      sentRevision: 0,
    };
    this.set((all) => ({ ...all, [d.id]: d }));
    return d;
  }

  update(id: string, fn: (d: AnnotationDraft) => AnnotationDraft): void {
    const previous = this.get()[id];
    if (previous) {
      this.#undo.set(id, [...(this.#undo.get(id) ?? []).slice(-59), previous]);
      this.#redo.delete(id);
    }
    this.record(id, fn);
  }

  /** Metadata/view changes do not enter the edit history. */
  record(id: string, fn: (d: AnnotationDraft) => AnnotationDraft): void {
    this.set((all) => {
      const d = all[id];
      return d ? { ...all, [id]: { ...fn(d), updatedAt: Date.now() } } : all;
    });
  }

  /** A transcript arrives on its own: every version of the draft receives it,
   * so undoing an edit never discards words spoken after that edit. */
  arrive(id: string, fn: (d: AnnotationDraft) => AnnotationDraft): void {
    for (const history of [this.#undo.get(id), this.#redo.get(id)])
      history?.forEach((d, i) => {
        history[i] = fn(d);
      });
    this.record(id, fn);
  }

  /** Found for what every version of the draft already says, like a
   * moment's still: each receives it, and so does a version taken to send. */
  place(id: string, fn: (d: AnnotationDraft) => AnnotationDraft): void {
    for (const taken of this.#taken.get(id) ?? []) taken.draft = fn(taken.draft);
    this.arrive(id, fn);
  }

  /** The draft as it is now, kept for a send: what is placed for it still
   * reaches it, and the person's later edits don't. Release it once sent. */
  take(id: string): { readonly draft: AnnotationDraft; release(): void } | null {
    const now = this.get()[id];
    if (!now) return null;
    const taken = { draft: now };
    const all = this.#taken.get(id) ?? new Set();
    this.#taken.set(id, all.add(taken));
    return {
      get draft() {
        return taken.draft;
      },
      release: () => {
        all.delete(taken);
        if (!all.size && this.#taken.get(id) === all) this.#taken.delete(id);
      },
    };
  }

  /** The versions of the draft taken to send. */
  taken(id: string): AnnotationDraft[] {
    return [...(this.#taken.get(id) ?? [])].map((t) => t.draft);
  }

  canUndo(id: string): boolean {
    return !!this.#undo.get(id)?.length;
  }
  canRedo(id: string): boolean {
    return !!this.#redo.get(id)?.length;
  }
  undo(id: string): void {
    this.#restore(id, this.#undo, this.#redo);
  }
  redo(id: string): void {
    this.#restore(id, this.#redo, this.#undo);
  }
  #restore(
    id: string,
    from: Map<string, AnnotationDraft[]>,
    to: Map<string, AnnotationDraft[]>,
  ): void {
    const current = this.get()[id],
      previous = from.get(id)?.pop();
    if (!current || !previous) return;
    to.set(id, [...(to.get(id) ?? []), current]);
    this.record(id, () => ({
      ...previous,
      nextSource: Math.max(current.nextSource, previous.nextSource),
      composer: current.composer?.text.trim()
        ? current.composer
        : previous.composer,
      viewports: current.viewports,
      watch: current.watch,
      selectedSource: current.selectedSource,
      // Recorded outside the edit history, like the viewport.
      mode: current.mode,
      selectedClip: current.selectedClip,
      clipEdits: current.clipEdits,
      retainedBlobKeys: [
        ...new Set([
          ...(current.retainedBlobKeys ?? []),
          ...current.sources.flatMap((s) => (s.blobKey ? [s.blobKey] : [])),
        ]),
      ],
      sentRevision: current.sentRevision,
      sentContent: current.sentContent,
      sentLabels: current.sentLabels,
      held: current.held,
    }));
  }

  /** Discard a draft and its phone-local source bytes. */
  async remove(id: string): Promise<void> {
    const d = this.get()[id];
    if (!d) return;
    this.set((all) => {
      const next = { ...all };
      delete next[id];
      return next;
    });
    if (this.persistence.get())
      throw new Error("Draft removal could not be saved on this device.");
    const referenced = new Set(
      Object.values(this.get()).flatMap((draft) => [
        ...draft.sources.map((s) => s.blobKey),
        ...(draft.retainedBlobKeys ?? []),
      ]),
    );
    await Promise.all(
      [
        ...new Set([
          ...d.sources.flatMap((s) => (s.blobKey ? [s.blobKey] : [])),
          ...(d.retainedBlobKeys ?? []),
        ]),
      ]
        .filter((key) => !referenced.has(key))
        .map((key) => blobs.remove(key)),
    );
  }
}

const stores = new Map<string, AnnotationStore>();

/** The annotation drafts for one paired computer. */
export function annotationStore(profileId: string): AnnotationStore {
  let s = stores.get(profileId);
  if (!s) stores.set(profileId, (s = new AnnotationStore(profileId)));
  return s;
}

export function useAnnotation(
  store: AnnotationStore,
  id: string | null,
): AnnotationDraft | null {
  const select = useCallback(
    (all: Drafts) => (id ? (all[id] ?? null) : null),
    [id],
  );
  return useStore(store, select);
}

// ---- Sending ----------------------------------------------------------------

/**
 * Upload every source that is not yet on the computer and build the next
 * immutable bundle revision. Returns the bundle and the draft with artifact
 * ids recorded, so a retry does not upload the same bytes again. Throws
 * UploadError from host/transfer when a source cannot be sent. The draft
 * goes as it is now, or as `taken` when the send took it earlier.
 */
export async function toBundle(
  ctx: { platform: Platform; profileId: string; session: HostSession },
  store: AnnotationStore,
  id: string,
  onProgress?: (done: number, total: number) => void,
  taken?: AnnotationDraft,
): Promise<AnnotationBundle> {
  const d0 = store.get()[id];
  if (!d0) throw new Error("This annotation no longer exists on this device.");
  const snapshot = structuredClone(taken ?? d0);
  const trimProblem = reproductionProblem(snapshot);
  if (trimProblem) throw new Error(trimProblem);
  if (snapshot.composer?.text.trim())
    throw new Error("Add your typed comment before sending.");
  if (snapshot.steps.some((s) => !s.text.trim()))
    throw new Error("Describe every step before sending.");
  if (summarize(snapshot).unresolved)
    throw new Error("Resolve comment targets before sending.");
  const selected = outgoingSources(snapshot);
  if (selected.length > MAX_SOURCES)
    throw new Error(`Include at most ${MAX_SOURCES} images, then send again.`);
  const keys = new Set(selected.map((s) => s.key));
  if (snapshot.steps.some((s) => !keys.has(s.source_key)))
    throw new Error("Choose an included image for every step.");
  // Its letters may reach the computer from here on; the caller lets go of
  // this hold once the saved send attempt holds them, or nothing was sent.
  store.preparing.set(id, nextMarkIndex(snapshot));
  const verified = selected.length ? await verifyOutgoing(selected) : new Map<string, Blob>();
  const total = selected.filter((s) => !s.artifactId).length;
  const sources: AnnotationSource[] = [];
  let done = 0;
  for (const s of selected) {
    let artifactId = s.artifactId,
      sha = s.sha256;
    if (!artifactId) {
      const blob = verified.get(s.key);
      if (!blob) throw new Error(`Image ${s.key} is missing from this device.`);
      if (!sha) throw new Error(`Image ${s.key} has no verified fingerprint.`);
      const r = await upload(ctx, {
        name: `${s.key}.${extension(s.mediaType)}`,
        blob,
        sha256: sha,
        destination: { kind: "artifact" },
        label: `Send ${s.key}`,
      });
      artifactId = r.artifactId;
      if (!artifactId)
        throw new TransferError(`The computer did not confirm image ${s.key}.`, r.transferId, false);
      const savedId = artifactId,
        savedSha = sha;
      store.record(id, (d) => ({
        ...d,
        sources: d.sources.map((x) =>
          x.key === s.key && x.blobKey === s.blobKey
            ? { ...x, artifactId: savedId, sha256: savedSha }
            : x,
        ),
      }));
      onProgress?.(++done, total);
    }
    if (!sha) throw new Error(`Image ${s.key} has no verified fingerprint.`);
    sources.push({
      key: s.key,
      artifact_id: artifactId,
      sha256: sha,
      media_type: s.mediaType,
      width: s.width,
      height: s.height,
      origin: s.origin,
      captured_at: s.capturedAt,
      derived_from: s.derivedFrom,
      derivation: s.derivation,
    });
  }
  const revision = snapshot.sentRevision + 1;
  store.prepared.set(id, {
    revision,
    content: contentIdentity(snapshot),
    labels: nextMarkIndex(snapshot),
  });
  const descriptions = snapshot.marks
    .filter((m) => !m.deleted && keys.has(m.source_key))
    .map((m) =>
      snapshot.descriptions?.[m.mark_id]
        ? `${m.label}: ${snapshot.descriptions[m.mark_id]}`
        : "",
    )
    .filter(Boolean);
  return {
    bundle_id: snapshot.bundleId,
    revision,
    sources,
    marks: snapshot.marks.filter((m) => keys.has(m.source_key)),
    // A moment names one of the frames sent; one left out takes the moment with it.
    comments: snapshot.comments.filter((c) => c.text.trim())
      .map(({ moment, ...c }) => moment && keys.has(moment.still) ? { ...c, moment } : c),
    steps: snapshot.steps,
    general_note:
      [
        snapshot.generalNote?.trim(),
        snapshot.expected?.trim() && `Expected: ${snapshot.expected.trim()}`,
        snapshot.actual?.trim() && `Actual: ${snapshot.actual.trim()}`,
        ...descriptions,
      ]
        .filter(Boolean)
        .join("\n") || null,
    speech: snapshot.speech
      ? {
          ...snapshot.speech,
          audio_artifact: null,
          segments: snapshot.speech.segments.flatMap((segment) => {
            const comment = snapshot.comments.find(
              (c) =>
                c.origin.kind === "spoken" &&
                c.origin.segment_ids.includes(segment.segment_id),
            );
            return comment ? [{ ...segment, text: comment.text }] : [];
          }),
        }
      : null,
  };
}

/** Holds the letters of the send leaving under `commandId` until its
 * outcome is known (heldLabels). */
export function holdLabels(
  store: AnnotationStore,
  id: string,
  commandId: string,
  labels: number,
): void {
  store.record(id, (d) => ({ ...d, held: { ...d.held, [commandId]: labels } }));
}

/** Gives each send not yet answered a hold of its own, as a send takes when
 * it leaves. One an earlier build saved, or the outbox put back, is held
 * only by its attempt until then, and a later send on the same message
 * replaces that attempt (heldLabels). */
export function holdUnanswered(
  store: AnnotationStore,
  ui: Pick<UiStore, "get">,
): void {
  const { drafts, savedDrafts } = ui.get();
  for (const d of [...Object.values(drafts), ...Object.values(savedDrafts).flat()]) {
    const a = d.attempt;
    if (!a?.annotationId || !a.commandId || !a.markup) continue;
    if (a.phase !== "sending" && a.phase !== "unknown") continue;
    const markup = store.get()[a.annotationId];
    if (markup && markup.held?.[a.commandId] === undefined)
      holdLabels(store, a.annotationId, a.commandId, submittedLabels(a.markup));
  }
}

/** Lets go of `commandId`'s hold: the send did not arrive, or its
 * confirmation reserved the letters (markSent). */
export function releaseLabels(
  store: AnnotationStore,
  id: string,
  commandId: string,
): void {
  if (store.get()[id]?.held?.[commandId] === undefined) return;
  store.record(id, (d) => {
    const { [commandId]: _released, ...held } = d.held ?? {};
    return { ...d, held };
  });
}

/**
 * Record that a revision was accepted by the computer. `content` is the
 * identity of the markup as submitted, which later edits may have changed;
 * `labels` is the label index after its marks, which stay taken. A send
 * recorded before the index was kept reserves the draft's current letters.
 */
export function markSent(
  store: AnnotationStore,
  id: string,
  revision: number,
  content: string,
  labels?: number,
): void {
  store.record(id, (d) => {
    const sentLabels = Math.max(d.sentLabels ?? 0, labels ?? Math.max(nextMarkIndex(d), submittedLabels({ content })));
    return revision < d.sentRevision
      ? { ...d, sentLabels }
      : { ...d, sentRevision: revision, sentContent: currentIdentity(content), sentLabels };
  });
}

function extension(mediaType: string): string {
  const sub = mediaType.split("/")[1] ?? "bin";
  return sub === "jpeg" ? "jpg" : sub.replace(/[^a-z0-9]/g, "") || "bin";
}
