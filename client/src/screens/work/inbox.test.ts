// SPDX-License-Identifier: Apache-2.0
import { beforeEach, describe, expect, it, vi } from "vitest";
import { InboxError, InboxStore } from "./inbox";
const text = {
  kind: "text" as const,
  text: "Keep this idea",
  blobKey: null,
  name: "Idea",
  mediaType: "text/plain",
  size: 14,
  sourceApp: "Notes",
  projectId: null,
};
beforeEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});
describe("phone-local inbox", () => {
  it("persists intake and project choice, isolated by profile", () => {
    const store = new InboxStore("one");
    const item = store.add(text);
    store.chooseProject(item.id, "project");
    expect(new InboxStore("one").get().items).toEqual([
      { ...item, projectId: "project" },
    ]);
    expect(new InboxStore("two").get().items).toEqual([]);
  });
  it("removes only the named item", async () => {
    const store = new InboxStore("one");
    const a = store.add(text);
    const b = store.add({ ...text, name: "Second idea" });
    await store.discard(a.id);
    expect(new InboxStore("one").get().items.map((i) => i.id)).toEqual([b.id]);
  });
  it("rejects incomplete intake", () => {
    expect(() => new InboxStore("one").add({ ...text, text: null })).toThrow(
      InboxError,
    );
    expect(() =>
      new InboxStore("one").add({ ...text, kind: "image", text: null }),
    ).toThrow(InboxError);
  });
  it("keeps unsaved intake in memory and reports disk-full", () => {
    const store = new InboxStore("one");
    const spy = vi
      .spyOn(Storage.prototype, "setItem")
      .mockImplementation(() => {
        throw new DOMException("Full", "QuotaExceededError");
      });
    expect(() => store.add(text)).toThrow(InboxError);
    expect(store.get().items).toHaveLength(1);
    expect(store.get().error).toContain("Not saved on this device");
    spy.mockRestore();
    store.retrySave();
    expect(new InboxStore("one").get().items).toHaveLength(1);
    expect(store.get().error).toBeNull();
  });
  it("preserves corrupt storage for recovery", () => {
    localStorage.setItem("capture-inbox.one", "[{}]");
    const store = new InboxStore("one");
    expect(store.get().error).not.toBeNull();
    expect(localStorage.getItem("capture-inbox.one")).toBe("[{}]");
    store.add(text);
    const recovery = Object.keys(localStorage).find((k) =>
      k.startsWith("capture-inbox.one.recovery."),
    );
    expect(recovery && localStorage.getItem(recovery)).toBe("[{}]");
    expect(new InboxStore("one").get().items).toHaveLength(1);
  });
});
