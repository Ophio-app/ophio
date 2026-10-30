// SPDX-License-Identifier: Apache-2.0
import { blobs } from "../../state/annotations";
import { Store } from "../../state/store";
import { uuidv7 } from "../../protocol/ids";

export interface InboxItem {
  id: string;
  intakeId?: string;
  destinationProfileId?: string | null;
  kind: "text" | "link" | "image" | "file";
  text: string | null;
  blobKey: string | null;
  name: string;
  mediaType: string;
  size: number;
  sourceApp: string | null;
  receivedAt: number;
  projectId: string | null;
}
export type InboxInput = Omit<InboxItem, "id" | "receivedAt"> & {
  receivedAt?: number;
};
export interface InboxState {
  items: InboxItem[];
  error: string | null;
}
export class InboxError extends Error {
  constructor(
    readonly code: "invalid_item" | "storage" | "missing_blob",
    message: string,
  ) {
    super(message);
    this.name = "InboxError";
  }
}
function valid(value: unknown): value is InboxItem {
  if (!value || typeof value !== "object") return false;
  const v = value as Record<string, unknown>;
  const nullable = (x: unknown) => x === null || typeof x === "string";
  return (
    typeof v.id === "string" &&
    ["text", "link", "image", "file"].includes(String(v.kind)) &&
    typeof v.name === "string" &&
    typeof v.mediaType === "string" &&
    typeof v.size === "number" &&
    Number.isFinite(v.size) &&
    v.size >= 0 &&
    typeof v.receivedAt === "number" &&
    Number.isFinite(v.receivedAt) &&
    nullable(v.text) &&
    nullable(v.blobKey) &&
    nullable(v.sourceApp) &&
    nullable(v.projectId)
  );
}
export class InboxStore extends Store<InboxState> {
  readonly key: string;
  #unreadable: boolean;
  #intakeIds = new Set<string>();
  constructor(profileId: string) {
    const key = `capture-inbox.${profileId}`;
    let state: InboxState = { items: [], error: null };
    let intakeIds: string[] = [];
    try {
      const raw: unknown = JSON.parse(localStorage.getItem(key) ?? "[]");
      if (Array.isArray(raw) && raw.every(valid))
        state = { items: raw, error: null };
      else if (
        raw &&
        typeof raw === "object" &&
        "items" in raw &&
        "intakeIds" in raw &&
        Array.isArray(raw.items) &&
        raw.items.every(valid) &&
        Array.isArray(raw.intakeIds) &&
        raw.intakeIds.every((id) => typeof id === "string")
      ) {
        state = { items: raw.items, error: null };
        intakeIds = raw.intakeIds;
      } else
        state.error =
          "Saved inbox data could not be read. The original data is still on this device.";
    } catch {
      state.error = "This device couldn’t read the saved inbox.";
    }
    super(state);
    this.key = key;
    this.#intakeIds = new Set(intakeIds);
    this.#unreadable = state.error !== null;
  }
  #persist(items: InboxItem[]): void {
    try {
      if (this.#unreadable) {
        const original = localStorage.getItem(this.key);
        if (original !== null)
          localStorage.setItem(`${this.key}.recovery.${Date.now()}`, original);
        this.#unreadable = false;
      }
      localStorage.setItem(
        this.key,
        JSON.stringify({ items, intakeIds: [...this.#intakeIds] }),
      );
      this.set({ items, error: null });
    } catch {
      this.set({
        items,
        error:
          "Not saved on this device. Storage is unavailable or full. Keep the app open and free some space.",
      });
      throw new InboxError(
        "storage",
        this.get().error ?? "Not saved on this device.",
      );
    }
  }
  hasIntake(id: string): boolean {
    return this.#intakeIds.has(id);
  }
  /** Commit the destination before removing metadata here. Bytes retain their key.
   * A crash between commits is retried by item identity without copying bytes. */
  moveTo(item: InboxItem, destination: InboxStore): void {
    const present = destination.get().items.some((i) => i.id === item.id);
    // Its intake recorded there without the item: the person discarded it.
    const discarded =
      !present && !!item.intakeId && destination.#intakeIds.has(item.intakeId);
    if (!present && !discarded) {
      if (item.intakeId) destination.#intakeIds.add(item.intakeId);
      destination.#persist([item, ...destination.get().items]);
    } else if (present && destination.get().error) destination.retrySave();
    this.#persist(this.get().items.filter((i) => i.id !== item.id));
  }
  /** Intake is local. Call blobs.put first for a native share, or addBlob. Never sends. */
  add(input: InboxInput): InboxItem {
    const item: InboxItem = {
      ...input,
      id: uuidv7(Date.now()),
      receivedAt: input.receivedAt ?? Date.now(),
    };
    if (
      !valid(item) ||
      !item.name.trim() ||
      ((item.kind === "image" || item.kind === "file") && !item.blobKey) ||
      ((item.kind === "text" || item.kind === "link") && !item.text?.trim())
    )
      throw new InboxError(
        "invalid_item",
        "The shared item has no usable content.",
      );
    if (item.intakeId) this.#intakeIds.add(item.intakeId);
    this.#persist([item, ...this.get().items]);
    return item;
  }
  async addBlob(
    blob: Blob,
    input: Omit<InboxInput, "blobKey" | "size" | "mediaType" | "text">,
  ): Promise<InboxItem> {
    let blobKey: string;
    try {
      blobKey = await blobs.put(blob);
    } catch {
      throw new InboxError(
        "storage",
        "The shared file couldn’t be saved on this device.",
      );
    }
    try {
      return this.add({
        ...input,
        blobKey,
        size: blob.size,
        mediaType: blob.type || "application/octet-stream",
        text: null,
      });
    } catch (error) {
      // Disk-full metadata remains in memory; its bytes must survive a retry.
      if (error instanceof InboxError && error.code === "invalid_item")
        await blobs.remove(blobKey);
      throw error;
    }
  }
  chooseProject(id: string, projectId: string): void {
    this.#persist(
      this.get().items.map((i) => (i.id === id ? { ...i, projectId } : i)),
    );
  }
  async discard(id: string): Promise<void> {
    const item = this.get().items.find((i) => i.id === id);
    this.#persist(this.get().items.filter((i) => i.id !== id));
    if (item?.blobKey) await blobs.remove(item.blobKey).catch(() => undefined);
  }
  retrySave(): void {
    this.#persist(this.get().items);
  }
}
const stores = new Map<string, InboxStore>();
export function inboxStore(profileId: string): InboxStore {
  let store = stores.get(profileId);
  if (!store) {
    store = new InboxStore(profileId);
    stores.set(profileId, store);
  }
  return store;
}
