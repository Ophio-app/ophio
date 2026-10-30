// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { UiStore } from "../app/ui";
import { TransferComposer } from "../screens/transfers/composer";
import { AnnotationStore } from "./annotations";

const retired: UiStore[] = [];
beforeEach(() => { localStorage.clear(); vi.useFakeTimers(); });
afterEach(() => {
  vi.restoreAllMocks();
  for (const store of retired.splice(0)) store.retire();
  vi.clearAllTimers();
  vi.useRealTimers();
});

const kinds = ["chat", "drawing", "transfer"] as const;
function open(kind: typeof kinds[number]) {
  if (kind === "chat") {
    const store = new UiStore("recovery");
    retired.push(store);
    return {
      key: "ui.recovery", store,
      edit: (text: string) => { store.saveDraft("p", null, { text }); store.persistNow(); },
      error: () => store.persistence.get(),
      recovery: () => store.recovery.get(),
      retry: () => store.persistNow(),
      texts: () => [...Object.values(store.get().drafts), ...Object.values(store.get().savedDrafts).flat()].map(d => d.text),
    };
  }
  if (kind === "drawing") {
    const store = new AnnotationStore("recovery");
    return {
      key: "annotations.recovery", store,
      edit: (text: string) => { const d = store.create("p", null); store.update(d.id, d => ({ ...d, generalNote: text })); },
      error: () => store.persistence.get(),
      recovery: () => store.recovery.get(),
      retry: () => store.persistNow(),
      texts: () => Object.values(store.get()).map(d => d.generalNote),
    };
  }
  const store = new TransferComposer("recovery");
  return {
    key: "transfer-composer.recovery", store,
    edit: (text: string) => store.update({ mode: "text", text }),
    error: () => store.get().error,
    recovery: () => store.recovery.get(),
    retry: () => store.persistNow(),
    texts: () => [store.get().text],
  };
}

describe.each(kinds)("%s draft recovery", kind => {
  it("keeps unread data and its warning through edits, then preserves both versions on retry", () => {
    const seed = open(kind);
    seed.edit("earlier work");
    const saved = localStorage.getItem(seed.key);
    const read = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("Unavailable"); });
    const current = open(kind);
    const write = vi.spyOn(Storage.prototype, "setItem");
    expect(current.error()).toContain("could not be read");
    current.edit("new work");
    current.edit("latest work");
    expect(current.error()).toContain("could not be read");
    expect(write).not.toHaveBeenCalled();
    read.mockRestore();
    expect(localStorage.getItem(seed.key)).toBe(saved);
    expect(current.retry()).toBe(true);
    expect(current.error()).toBeNull();
    const reopened = open(kind);
    expect(reopened.texts()).toContain("latest work");
    if (kind === "transfer") {
      expect(JSON.parse(localStorage.getItem(`${seed.key}.recovered`)!)).toMatchObject({ text: "earlier work" });
      expect(current.recovery()).toContain("earlier transfer draft");
    } else expect(reopened.texts()).toContain("earlier work");
    current.retry();
    if (kind !== "transfer") expect(current.texts().filter(text => text === "earlier work")).toHaveLength(1);
  });

  it("cannot replace damaged JSON until the exact recovery copy fits", () => {
    const key = open(kind).key;
    const damaged = '{"unsent":"keep 🌱';
    localStorage.setItem(key, damaged);
    const write = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("Full"); });
    const current = open(kind);
    expect(current.error()).toContain("could not be set aside");
    current.edit("new work");
    expect(current.error()).toContain("could not be set aside");
    expect(localStorage.getItem(key)).toBe(damaged);
    expect(write.mock.calls.every(([k]) => k === `${key}.unreadable`)).toBe(true);
    write.mockRestore();
    expect(current.retry()).toBe(true);
    expect(localStorage.getItem(`${key}.unreadable`)).toBe(damaged);
    expect(current.error()).toBeNull();
    expect(current.recovery()).toContain("damaged");
    expect(open(kind).texts()).toContain("new work");
  });

  it("keeps the warning and merged work if a read recovers before writes do", () => {
    const seed = open(kind);
    seed.edit("earlier work");
    const saved = localStorage.getItem(seed.key);
    vi.spyOn(Storage.prototype, "getItem").mockImplementationOnce(() => { throw new Error("Unavailable"); });
    const current = open(kind);
    const setItem = Storage.prototype.setItem;
    const write = vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (this: Storage, key, value) {
      if (key === seed.key) throw new Error("Full");
      setItem.call(this, key, value);
    });
    current.edit("new work");
    expect(current.error()).not.toBeNull();
    expect(current.texts()).toContain("new work");
    expect(localStorage.getItem(seed.key)).toBe(saved);
    write.mockRestore();
    expect(current.retry()).toBe(true);
    expect(current.error()).toBeNull();
    expect(open(kind).texts()).toContain("new work");
    if (kind !== "transfer") expect(current.texts().filter(t => t === "earlier work")).toHaveLength(1);
  });
});

it("retains chat history, markup links and uncertain sends alongside work made during the outage", () => {
  const seed = new UiStore("metadata");
  retired.push(seed);
  seed.saveDraft("p", "t", { text: "earlier", annotationId: "a", attempt: { phase: "sending", at: 1, commandId: "send", detail: "Sending", text: "earlier" } });
  seed.set(s => ({ ...s, savedDrafts: { "p/t": [s.drafts["p/t"]!] }, removedMarkup: { a: { projectId: "p", taskId: "t", removedAs: "identity" } } }));
  seed.persistNow();
  vi.spyOn(Storage.prototype, "getItem").mockImplementationOnce(() => { throw new Error("Unavailable"); });
  const current = new UiStore("metadata");
  retired.push(current);
  current.saveDraft("q", null, { text: "new" });
  current.persistNow();
  expect(current.draft("p", "t")).toMatchObject({ text: "earlier", annotationId: "a", attempt: { phase: "unknown" } });
  expect(current.get().savedDrafts["p/t"]).toHaveLength(1);
  expect(current.get().removedMarkup).toEqual(seed.get().removedMarkup);
});

it("does not let an older completed transfer clear an edit recovered after a read failure", () => {
  const seed = new TransferComposer("revisions");
  seed.update({ mode: "text", text: "earlier" });
  seed.submitted("old-job");
  vi.spyOn(Storage.prototype, "getItem").mockImplementationOnce(() => { throw new Error("Unavailable"); });
  const current = new TransferComposer("revisions");
  current.update({ name: "new-name.md" });
  expect(current.completed("old-job")).toBe(false);
  expect(current.get().text).toBe("earlier");
  current.submitted("new-job");
  expect(current.completed("new-job")).toBe(true);
  expect(new TransferComposer("revisions").get().text).toBe("");
});
