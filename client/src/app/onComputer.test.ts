// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { deferringUi, storeOf, whenShown } from "./onComputer";
import { UiStore } from "./ui";

beforeEach(() => localStorage.clear());
afterEach(() => vi.restoreAllMocks());

it("sends what moves the screen to the computer's own store once it is shown, and reads and keeps drafts in place", async () => {
  const ui = new UiStore("other");
  const visits: ((ui: UiStore) => void)[] = [];
  const deferred = deferringUi(ui, (act) => visits.push(act));
  // Nothing moves until the computer is shown.
  deferred.openTask("p1", "t1");
  deferred.push({ screen: "receipt", taskId: "t2" });
  expect(ui.get().stacks.talk).toHaveLength(1);
  expect(ui.get().stacks.work).toHaveLength(1);
  await Promise.resolve();
  expect(visits).toHaveLength(1);
  // Shown, each move happens there.
  for (const act of visits) act(ui);
  expect(ui.route()).toEqual({ screen: "receipt", taskId: "t2" });
  expect(ui.get().taskByProject.p1).toBe("t1");
  // Reading and drafts stay with the store, as before.
  deferred.saveDraft("p1", null, { text: "kept" });
  expect(ui.draft("p1", null)?.text).toBe("kept");
  expect(deferred.get()).toBe(ui.get());
  // One per store, so subscriptions hold.
  expect(deferringUi(ui, () => {})).toBe(deferred);
});

it("asks for one switch for every move one tap makes, done in order once shown", async () => {
  const ui = new UiStore("batched");
  const visits: ((ui: UiStore) => void)[] = [];
  const deferred = deferringUi(ui, (act) => visits.push(act));
  // As "Open in Next up" does: choose the project, then open Next up.
  deferred.contextProject("p9");
  deferred.push({ screen: "next_up" }, "work");
  await Promise.resolve();
  expect(visits).toHaveLength(1);
  visits[0]!(ui);
  expect(ui.get().projectId).toBe("p9");
  expect(ui.route()).toEqual({ screen: "next_up" });
  // A later tap is a switch of its own.
  deferred.push({ screen: "agents" });
  await Promise.resolve();
  expect(visits).toHaveLength(2);
});

it("runs what follows a move after the switch, never before, and moves nothing for a change in place", async () => {
  const ui = new UiStore("after");
  const visits: ((ui: UiStore) => void)[] = [];
  const deferred = deferringUi(ui, (act) => visits.push(act));
  const then = vi.fn();
  whenShown(deferred, then);
  deferred.selectProject("p2");
  await Promise.resolve();
  expect(then).not.toHaveBeenCalled();
  visits[0]!(ui);
  expect(then).toHaveBeenCalledTimes(1);
  // On the computer on screen, at once.
  const here = new UiStore("here");
  const now = vi.fn();
  whenShown(here, now);
  expect(now).toHaveBeenCalledTimes(1);
  // The store itself, for a change that moves nothing.
  expect(storeOf(deferred)).toBe(ui);
  storeOf(deferred).selectTask("p2", "t2");
  expect(visits).toHaveLength(1);
  // Each function is the same one every time, so a subscription holds.
  expect(deferred.subscribe).toBe(deferred.subscribe);
  expect(deferred.get).toBe(deferred.get);
});
