// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { UiStore } from "./ui";
import { Computers } from "./computers";
import { platform, profile } from "../platform/lifecycle.fixtures";

beforeEach(() => { localStorage.clear(); vi.useFakeTimers(); });
afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); });
it("shares work synchronously without sharing navigation or saving a second draft copy", () => {
  const left = new UiStore("sides"), right = left.fork("right");
  left.selectProject("one"); left.selectTab("work");
  right.selectProject("two"); right.push({ screen: "settings" });
  left.saveDraft("one", null, { text: "first" });
  expect(right.draft("one", null)?.text).toBe("first");
  right.saveDraft("one", null, { text: "first and second" });
  expect(left.draft("one", null)?.text).toBe("first and second");
  expect(left.get().projectId).toBe("one"); expect(left.get().tab).toBe("work");
  expect(right.get().projectId).toBe("two"); expect(right.route().screen).toBe("settings");
  right.persistNow();
  const saved = JSON.parse(localStorage.getItem("ui.sides.side.right")!);
  expect(saved.state.drafts).toBeUndefined();
  expect(JSON.parse(localStorage.getItem("ui.sides")!).state.drafts["one/new"].text).toBe("first and second");
  left.retire(); right.retire();
  const restored = new UiStore("sides"), other = restored.fork("right");
  expect(other.get().projectId).toBe("two"); expect(other.route().screen).toBe("settings");
  expect(other.draft("one", null)?.text).toBe("first and second");
  expect(other.sharedStore()).toBe(restored); expect(restored.navigationStores()).toEqual([restored, other]);
  restored.retire(); other.retire();
});
it("starts a fresh side at root destinations rather than copying an editor or its Back history", () => {
  const left = new UiStore("fresh-history");
  left.selectProject("project");
  left.push({ screen: "annotate", annotationId: "image" });
  left.push({ screen: "review", projectId: "project", taskId: null });
  left.saveDraft("project", null, { text: "shared unsent words" });
  const right = left.fork("right");
  expect(right.route()).toEqual({ screen: "talk" });
  expect(Object.values(right.get().stacks).every(stack => stack.length === 1)).toBe(true);
  expect(right.get().returns).toEqual({});
  expect(right.draft("project", null)?.text).toBe("shared unsent words");
  right.push({ screen: "inbox" }); right.persistNow();
  left.retire(); right.retire();
  const restored = new UiStore("fresh-history"), restoredRight = restored.fork("right");
  expect(restoredRight.route()).toEqual({ screen: "inbox" });
  expect(restored.route()).toEqual({ screen: "review", projectId: "project", taskId: null });
  restored.retire(); restoredRight.retire();
});
it("restores only existing saved side positions without manufacturing new navigation", () => {
  const root = new UiStore("restore-only");
  root.restoreNavigation("right");
  expect(root.navigationStores()).toEqual([root]);
  const right = root.fork("right");
  right.push({ screen: "annotate", annotationId: "saved-image" });
  right.retire(); root.retire();
  const restored = new UiStore("restore-only");
  expect(restored.navigationStores()).toEqual([restored]);
  restored.restoreNavigation("right");
  restored.restoreNavigation("right");
  expect(restored.navigationStores()).toHaveLength(2);
  expect(restored.fork("right").route()).toEqual({ screen: "annotate", annotationId: "saved-image" });
  for (const view of restored.navigationStores()) view.retire();
});
it("does not hide a failed shared-chat recovery when side navigation loads successfully", () => {
  localStorage.setItem("ui.damaged-sides", "{");
  const set = Storage.prototype.setItem;
  const failBackup = vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (this: Storage, key, value) {
    if (key.startsWith("ui.damaged-sides.unreadable")) throw new Error("Storage full");
    set.call(this, key, value);
  });
  const left = new UiStore("damaged-sides");
  const warning = left.persistence.get();
  expect(warning).toContain("could not be set aside");
  const right = left.fork("right");
  expect(right.persistence).toBe(left.persistence);
  expect(left.persistence.get()).toBe(warning);
  expect(localStorage.getItem("ui.damaged-sides")).toBe("{");
  left.retire(); right.retire(); failBackup.mockRestore();
});
it("keeps a shared computer visibly held until its last side leaves", () => {
  const fake = platform(), computers = new Computers(fake.value, [profile]);
  const stop = computers.hold();
  const left = computers.show(profile.profileId), right = computers.show(profile.profileId);
  expect(computers.visibleCount(profile.profileId)).toBe(2);
  left(); left();
  expect(computers.visible.get().has(profile.profileId)).toBe(true);
  right(); expect(computers.visible.get().size).toBe(0);
  stop();
});
it("gives every side the newest shared work when a listener changes it while an update is still notifying", () => {
  const left = new UiStore("reentrant"), right = left.fork("right"), third = left.fork("third");
  left.saveDraft("one", null, { text: "first" });
  const off = left.subscribe(() => {
    if (left.draft("one", null)?.text === "second") left.saveDraft("one", null, { text: "third" });
  });
  for (const writer of [left, right]) {
    left.saveDraft("one", null, { text: "first" });
    writer.saveDraft("one", null, { text: "second" });
    for (const view of [left, right, third]) expect(view.draft("one", null)?.text).toBe("third");
  }
  off(); left.retire(); right.retire(); third.retire();
});
it("makes a side's change to the newest shared work even before that side has been told of it", () => {
  const left = new UiStore("reentrant-side"), right = left.fork("right");
  left.saveDraft("one", null, { text: "first" });
  const off = left.subscribe(() => {
    // Runs while the computer store is notifying, before the right side hears of "second".
    if (left.draft("one", null)?.text === "second" && !left.draft("one", null)?.annotationId)
      right.saveDraft("one", null, { annotationId: "markup" });
  });
  left.saveDraft("one", null, { text: "second" });
  for (const view of [left, right]) expect(view.draft("one", null)).toMatchObject({ text: "second", annotationId: "markup" });
  off(); left.retire(); right.retire();
});
it("keeps a side's save warning until that side's position has saved, whatever the shared chats do", () => {
  const set = Storage.prototype.setItem;
  let refuseSide = true;
  const refuse = vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (this: Storage, key, value) {
    if (refuseSide && key === "ui.side-warning.side.right") throw new Error("Storage full");
    set.call(this, key, value);
  });
  const left = new UiStore("side-warning"), right = left.fork("right");
  right.push({ screen: "inbox" });
  expect(right.persistNow()).toBe(false);
  const warning = left.persistence.get();
  expect(warning).toContain("This side’s position could not be saved");
  left.saveDraft("one", null, { text: "saved with the computer" });
  expect(left.persistNow()).toBe(true);
  expect(left.persistence.get()).toBe(warning);
  refuseSide = false;
  expect(left.persistAll()).toBe(true);
  expect(left.persistence.get()).toBeNull();
  expect(JSON.parse(localStorage.getItem("ui.side-warning.side.right")!).state.stacks.talk.at(-1)).toEqual({ screen: "inbox" });
  left.retire(); right.retire(); refuse.mockRestore();
});
