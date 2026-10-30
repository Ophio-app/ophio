// SPDX-License-Identifier: Apache-2.0
import type {
  DirListing,
  FileContent,
  LibraryRoot,
} from "../../protocol/types";
import { Store } from "../../state/store";
import { readSavedJson, UnpreservedJson, type StorageLike } from "../../state/savedJson";
export type { StorageLike } from "../../state/savedJson";

export const fileKey = (rootId: string, path: string) =>
  JSON.stringify([rootId, path]);
export interface Cached<T> {
  value: T;
  at: number;
}
export interface FileDraft {
  base: FileContent;
  text: string;
  updatedAt: number;
  /** When this draft was started (see `draftStart`), so a read begun for an
   * earlier draft of the same file is not applied to this one. */
  startedAt?: number;
  saveAttemptAt?: number;
  phase: "draft" | "sending" | "unknown" | "conflict";
  recoveryId?: string;
  current?: FileContent | null;
  /** The computer's version is too large to send to this device, so
   * `current` is null although the file exists. */
  currentTooLarge?: boolean;
  currentAt?: number;
  reviewMerge?: boolean;
  /** This merge marked overlaps; saving waits until its markers are gone. */
  mergeMarkers?: boolean;
  reviewRecovery?: string;
  /** Confirmed file destination when this draft could not follow it. */
  movedTo?: string;
}
let lastStart = 0;
/** A start mark no earlier draft on this device has had. */
export function draftStart(): number {
  lastStart = Math.max(Date.now(), lastStart + 1);
  return lastStart;
}
export interface Remembered {
  rootId: string;
  path: string;
  at: number;
}
interface LibraryState {
  drafts: Record<string, FileDraft>;
  uncheckedSaves: Record<string, UncheckedSave>;
  files: Record<string, Cached<FileContent>>;
  listings: Record<string, Cached<DirListing>>;
  /** Only these cache entries are serialized; live read results survive eviction. */
  persistedFiles: Record<string, Cached<FileContent>>;
  persistedListings: Record<string, Cached<DirListing>>;
  roots: Cached<LibraryRoot[]> | null;
  recent: Remembered[];
  favourites: Remembered[];
  forms: Record<string, string>;
  storageError: string | null;
  storageRecovery: string | null;
}
const empty = (): LibraryState => ({
  drafts: {},
  uncheckedSaves: {},
  files: {},
  listings: {},
  persistedFiles: {},
  persistedListings: {},
  roots: null,
  recent: [],
  favourites: [],
  forms: {},
  storageError: null,
  storageRecovery: null,
});
export interface UncheckedSave {
  rootId: string;
  path: string;
  at: number;
  recoveryId?: string;
  movedTo?: string;
}
function record(x: unknown): x is Record<string, unknown> {
  return !!x && typeof x === "object" && !Array.isArray(x);
}
function isFile(x: unknown): x is FileContent {
  return (
    record(x) &&
    typeof x.root_id === "string" &&
    typeof x.path === "string" &&
    typeof x.text === "string" &&
    typeof x.revision === "string" &&
    typeof x.size === "number" &&
    (x.newline === "lf" || x.newline === "cr_lf" || x.newline === "mixed") &&
    (x.encoding === "utf8" || x.encoding === "unsupported") &&
    typeof x.has_bom === "boolean" &&
    (x.modified_at === null || typeof x.modified_at === "number")
  );
}
function load(storage: StorageLike, key: string): LibraryState {
  const s = empty();
  try {
    const { value: raw, recovered } = readSavedJson(storage, key);
    if (recovered) s.storageRecovery =
      "Saved Library data was damaged. A copy has been kept on this device for recovery, but earlier drafts and edits could not be loaded. New changes can be saved again.";
    if (!record(raw) || raw.version !== 1 || !record(raw.state)) return s;
    const v = raw.state;
    if (record(v.uncheckedSaves))
      for (const [k, save] of Object.entries(v.uncheckedSaves))
        if (record(save) && typeof save.rootId === "string" && typeof save.path === "string" &&
          typeof save.at === "number" && k === fileKey(save.rootId, save.path))
          s.uncheckedSaves[k] = { rootId: save.rootId, path: save.path, at: save.at,
            recoveryId: typeof save.recoveryId === "string" ? save.recoveryId : undefined,
            movedTo: typeof save.movedTo === "string" ? save.movedTo : undefined };
    if (record(v.drafts))
      for (const [k, d] of Object.entries(v.drafts)) {
        if (
          !record(d) ||
          !isFile(d.base) ||
          typeof d.text !== "string" ||
          typeof d.updatedAt !== "number" ||
          k !== fileKey(d.base.root_id, d.base.path)
        )
          continue;
        s.drafts[k] = {
          base: d.base,
          text: d.text,
          updatedAt: d.updatedAt,
          ...(typeof d.startedAt === "number" ? { startedAt: d.startedAt } : {}),
          ...(typeof d.saveAttemptAt === "number" ? { saveAttemptAt: d.saveAttemptAt } : {}),
          phase:
            d.phase === "sending"
              ? "unknown"
              : d.phase === "unknown" || d.phase === "conflict"
                ? d.phase
                : "draft",
          ...(typeof d.recoveryId === "string"
            ? { recoveryId: d.recoveryId }
            : {}),
          ...(d.current === null || isFile(d.current)
            ? { current: d.current }
            : {}),
          ...(d.current === null && d.currentTooLarge === true
            ? { currentTooLarge: true }
            : {}),
          ...(typeof d.currentAt === "number"
            ? { currentAt: d.currentAt }
            : {}),
          reviewMerge: d.reviewMerge === true,
          ...(d.mergeMarkers === true ? { mergeMarkers: true } : {}),
          reviewRecovery: typeof d.reviewRecovery === "string" ? d.reviewRecovery : undefined,
          movedTo: typeof d.movedTo === "string" ? d.movedTo : undefined,
        };
      }
    if (record(v.files))
      for (const [k, c] of Object.entries(v.files))
        if (record(c) && isFile(c.value) && typeof c.at === "number")
          s.files[k] = { value: c.value, at: c.at };
    // Caches are expendable; drafts above are validated independently of them.
    if (record(v.listings))
      for (const [k, c] of Object.entries(v.listings)) {
        if (
          record(c) &&
          record(c.value) &&
          typeof c.at === "number" &&
          typeof c.value.root_id === "string" &&
          typeof c.value.dir === "string" &&
          Array.isArray(c.value.entries)
        ) {
          const entries = c.value.entries.filter(
            (e: unknown) =>
              record(e) &&
              typeof e.name === "string" &&
              typeof e.path === "string" &&
              ["markdown", "directory", "other"].includes(String(e.kind)) &&
              typeof e.size === "number",
          );
          s.listings[k] = {
            value: {
              root_id: c.value.root_id,
              dir: c.value.dir,
              entries,
            } as DirListing,
            at: c.at,
          };
        }
      }
    if (
      record(v.roots) &&
      Array.isArray(v.roots.value) &&
      typeof v.roots.at === "number"
    ) {
      const roots = v.roots.value.filter(
        (r: unknown) =>
          record(r) &&
          typeof r.root_id === "string" &&
          typeof r.label === "string" &&
          typeof r.host_path === "string" &&
          typeof r.writable === "boolean" &&
          [
            "skills",
            "agents",
            "instructions",
            "project",
            "notes",
            "drafts",
          ].includes(String(r.kind)),
      );
      s.roots = { value: roots as LibraryRoot[], at: v.roots.at };
    }
    for (const field of ["recent", "favourites"] as const)
      if (Array.isArray(v[field]))
        s[field] = v[field].filter(
          (r: unknown): r is Remembered =>
            record(r) &&
            typeof r.rootId === "string" &&
            typeof r.path === "string" &&
            typeof r.at === "number",
        );
    if (record(v.forms))
      s.forms = Object.fromEntries(
        Object.entries(v.forms).filter(
          (e): e is [string, string] => typeof e[1] === "string",
        ),
      );
  } catch (error) {
    s.storageError = error instanceof UnpreservedJson
      ? "Saved Library data is damaged and could not be set aside. The original is untouched. New changes are held only until the app closes. Copy new edits elsewhere, free some device storage, then restart to try again."
      : "Saved Library data could not be read. Existing storage has been left untouched. New changes are held only until the app closes. Copy any new edits elsewhere before restarting the app to try again.";
  }
  s.persistedFiles = s.files;
  s.persistedListings = s.listings;
  return s;
}
export class LibraryStore extends Store<LibraryState> {
  readonly key: string;
  private readonly loadError: string | null;
  constructor(
    profileId: string,
    private storage: StorageLike = localStorage,
  ) {
    const key = `library.v1.${profileId}`;
    super(load(storage, key));
    this.key = key;
    this.loadError = this.get().storageError;
  }
  private change(fn: (s: LibraryState) => LibraryState): void {
    let next = fn(this.get());
    // A failed read or recovery copy keeps saved drafts protected. Keep changes
    // in memory until a fresh store can load or preserve them; a successful
    // write alone would erase them. Write/quota failures after a good load retry.
    if (this.loadError !== null) {
      this.set({ ...next, storageError: this.loadError });
      return;
    }
    const write = (state: LibraryState) => {
      const { persistedFiles, persistedListings, storageRecovery: _recovery, ...live } = state;
      this.storage.setItem(
        this.key,
        JSON.stringify({ version: 1, state: {
          ...live, files: persistedFiles, listings: persistedListings, storageError: null,
        } }),
      );
    };
    try {
      try {
        write(next);
      } catch {
        // Cached file bodies are expendable; they must never crowd out drafts.
        // Drop them from memory first, even if this write fails too, so the
        // next edit does not serialize evicted caches again.
        next = { ...next, persistedFiles: {}, persistedListings: {} };
        write(next);
      }
      next.storageError = null;
    } catch {
      next.storageError =
        "Device storage is full or unavailable. These changes are held only until the app closes. Keep this screen open and free some storage.";
    }
    this.set(next);
  }
  draft(rootId: string, path: string): FileDraft | null {
    return this.get().drafts[fileKey(rootId, path)] ?? null;
  }
  saveDraft(draft: FileDraft): void {
    this.change((s) => ({
      ...s,
      drafts: {
        ...s.drafts,
        [fileKey(draft.base.root_id, draft.base.path)]: draft,
      },
    }));
  }
  removeDraft(rootId: string, path: string): void {
    this.change((s) => {
      const drafts = { ...s.drafts };
      delete drafts[fileKey(rootId, path)];
      return { ...s, drafts };
    });
  }
  /** Discard local text without erasing evidence of an unconfirmed remote save. */
  discardDraft(rootId: string, path: string): void {
    this.change((s) => {
      const key = fileKey(rootId, path), draft = s.drafts[key];
      if (draft?.phase === "sending") return s;
      const drafts = { ...s.drafts }, uncheckedSaves = { ...s.uncheckedSaves };
      if (draft?.phase === "unknown") uncheckedSaves[key] = {
        rootId, path, at: draft.updatedAt, recoveryId: draft.recoveryId, movedTo: draft.movedTo,
      };
      delete drafts[key];
      return { ...s, drafts, uncheckedSaves };
    });
  }
  checkedSave(rootId: string, path: string): void {
    this.change((s) => {
      const uncheckedSaves = { ...s.uncheckedSaves };
      delete uncheckedSaves[fileKey(rootId, path)];
      return { ...s, uncheckedSaves };
    });
  }
  /** Called only after the computer confirms the move, never on admission. */
  moveFile(rootId: string, from: string, to: string): void {
    if (from === to) return;
    this.change((s) => {
      const oldKey = fileKey(rootId, from), newKey = fileKey(rootId, to);
      const drafts = { ...s.drafts }, files = { ...s.files },
        persistedFiles = { ...s.persistedFiles }, uncheckedSaves = { ...s.uncheckedSaves };
      const draft = drafts[oldKey];
      if (draft) {
        if (drafts[newKey] || draft.phase === "sending" || draft.phase === "unknown") {
          drafts[oldKey] = { ...draft, movedTo: to };
        } else {
          drafts[newKey] = { ...draft, base: { ...draft.base, path: to },
            current: draft.current ? { ...draft.current, path: to } : draft.current, movedTo: undefined };
          delete drafts[oldKey];
        }
      }
      // Keep earlier stranded drafts reachable after a second move.
      for (const [key, d] of Object.entries(drafts))
        if (d.base.root_id === rootId && d.movedTo === from)
          // A file moved back to the draft's own path is no longer elsewhere.
          drafts[key] = { ...d, movedTo: d.base.path === to ? undefined : to };
      delete files[oldKey];
      delete persistedFiles[oldKey];
      for (const [key, save] of Object.entries(uncheckedSaves))
        if (save.rootId === rootId && (save.movedTo ?? save.path) === from) uncheckedSaves[key] = { ...save, movedTo: to };
      const isOld = (r: Remembered) => r.rootId === rootId && r.path === from;
      return { ...s, drafts, files, persistedFiles, uncheckedSaves,
        recent: s.recent.filter((r) => !isOld(r)),
        favourites: s.favourites.filter((r) => !isOld(r) || !s.favourites.some((v) => v.rootId === rootId && v.path === to))
          .map((r) => isOld(r) ? { ...r, path: to } : r),
      };
    });
  }
  cacheFile(file: FileContent): void {
    const key = fileKey(file.root_id, file.path), cached = { value: file, at: Date.now() };
    this.change((s) => ({
      ...s,
      files: trim(
        {
          ...s.files,
          [key]: cached,
        },
        16,
      ),
      persistedFiles: trim({ ...s.persistedFiles, [key]: cached }, 16),
    }));
  }
  cacheListing(listing: DirListing): void {
    const key = fileKey(listing.root_id, listing.dir), cached = { value: listing, at: Date.now() };
    this.change((s) => ({
      ...s,
      listings: trim(
        {
          ...s.listings,
          [key]: cached,
        },
        32,
      ),
      persistedListings: trim({ ...s.persistedListings, [key]: cached }, 32),
    }));
  }
  cacheRoots(roots: LibraryRoot[]): void {
    this.change((s) => ({ ...s, roots: { value: roots, at: Date.now() } }));
  }
  remember(rootId: string, path: string): void {
    this.change((s) => ({
      ...s,
      recent: [
        { rootId, path, at: Date.now() },
        ...s.recent.filter((r) => r.rootId !== rootId || r.path !== path),
      ].slice(0, 12),
    }));
  }
  favourite(rootId: string, path: string): void {
    this.change((s) => ({
      ...s,
      favourites: s.favourites.some(
        (r) => r.rootId === rootId && r.path === path,
      )
        ? s.favourites.filter((r) => r.rootId !== rootId || r.path !== path)
        : [...s.favourites, { rootId, path, at: Date.now() }],
    }));
  }
  form(key: string, value: string): void {
    this.change((s) => ({ ...s, forms: { ...s.forms, [key]: value } }));
  }
}
function trim<T>(
  all: Record<string, Cached<T>>,
  max: number,
): Record<string, Cached<T>> {
  return Object.fromEntries(
    Object.entries(all)
      .sort((a, b) => b[1].at - a[1].at)
      .slice(0, max),
  );
}
const stores = new Map<string, LibraryStore>();
export function libraryStore(profileId: string): LibraryStore {
  let s = stores.get(profileId);
  if (!s) {
    s = new LibraryStore(profileId);
    stores.set(profileId, s);
  }
  return s;
}
