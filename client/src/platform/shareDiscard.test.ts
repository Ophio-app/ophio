// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it, vi } from "vitest";
import { InboxStore } from "../screens/work/inbox";
import { blobs } from "../state/annotations";
import { platformProblems } from "./errors";
import { PHONE_INBOX, ShareIntake } from "./shares";

function intake() {
  const stores = new Map<string, InboxStore>();
  const store = (id: string) => {
    let s = stores.get(id);
    if (!s) stores.set(id, (s = new InboxStore(id)));
    return s;
  };
  return { store, shares: new ShareIntake(store) };
}
const picture = { id: "intake-picture", name: "Screenshot.png", mime: "image/png", bytes: new Uint8Array([1, 2, 3]) };

beforeEach(() => {
  localStorage.clear();
  platformProblems.set([]);
  vi.restoreAllMocks();
});

it.each([false, true])(
  "an item discarded after its computer's inbox couldn't save stays discarded (restart: %s)",
  async (restart) => {
    const stored = new Set<string>();
    vi.spyOn(blobs, "put").mockImplementation(async () => {
      stored.add("picture-bytes");
      return "picture-bytes";
    });
    vi.spyOn(blobs, "remove").mockImplementation(async (key) => {
      stored.delete(key);
    });
    const first = intake();
    // Storage is nearly full: the holding inbox saves, computer A's inbox doesn't.
    const setItem = Storage.prototype.setItem;
    const full = vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (this: Storage, key, value) {
      if (key === "capture-inbox.a") throw new DOMException("Full", "QuotaExceededError");
      setItem.call(this, key, value);
    });
    await expect(first.shares.receive([picture], "a")).rejects.toThrow("Not saved on this device");
    expect(first.store("a").get().items).toHaveLength(1);
    full.mockRestore();
    // The person frees space by discarding the new item from A's inbox.
    await first.store("a").discard(first.store("a").get().items[0]!.id);
    expect(first.store("a").get().items).toHaveLength(0);
    expect(stored.has("picture-bytes")).toBe(false);
    // The next share, a switch back to A, or a restart reconciles the holding inbox.
    const later = restart ? intake() : first;
    await later.shares.paired("a");
    expect(later.store("a").get().items).toEqual([]);
    expect(later.store(PHONE_INBOX).get().items).toEqual([]);
  },
);
