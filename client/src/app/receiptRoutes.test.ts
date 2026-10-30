// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it } from "vitest";
import { UiStore } from "./ui";
beforeEach(() => localStorage.clear());
it("opening another task clears a receipt and its child routes while preserving drafts", () => {
  const ui = new UiStore("navigation");
  ui.selectTask("project", "checkout");
  ui.saveDraft("project", "checkout", { text: "Keep this correction" });
  ui.push({ screen: "receipt", taskId: "checkout" });
  ui.push({ screen: "annotate", annotationId: "drawing" });
  ui.selectTab("work");
  ui.selectTask("project", "dashboard");
  ui.selectTab("talk");
  expect(ui.route()).toEqual({ screen: "talk" });
  expect(ui.get().taskByProject.project).toBe("dashboard");
  expect(ui.draft("project", "checkout")?.text).toBe("Keep this correction");
  ui.back();
  expect(ui.route()).toEqual({ screen: "talk" });
});
it("removes a mismatched receipt even when the selected task did not change", () => {
  const ui = new UiStore("navigation");
  ui.selectTask("project", "dashboard");
  ui.push({ screen: "receipt", taskId: "checkout" });
  ui.selectTask("project", "dashboard");
  expect(ui.route().screen).toBe("talk");
});
it("same-task navigation keeps its receipt; a file returns to the receipt's tab", () => {
  const ui = new UiStore("navigation");
  ui.selectTask("project", "checkout");
  ui.push({ screen: "receipt", taskId: "checkout" }, "work");
  ui.push({ screen: "file", rootId: "root", path: "README.md" });
  ui.back();
  expect(ui.route()).toEqual({ screen: "receipt", taskId: "checkout" });
  expect(ui.get().tab).toBe("work");
});
it.each([null, "task"])("finishes only the originating review after switching tabs (task %s)", taskId => {
  const ui = new UiStore("navigation");
  const review = { screen: "review", projectId: "project", taskId } as const;
  ui.push({ screen: "annotate", annotationId: "drawing" }, "control");
  ui.push(review);
  ui.push({ screen: "receipt", taskId: "other" }, "talk");
  const talk = ui.get().stacks.talk;
  ui.finish(["review", "annotate"], "control", review);
  expect(ui.get().tab).toBe("talk");
  expect(ui.get().stacks.talk).toBe(talk);
  expect(ui.get().stacks.control).toEqual([{ screen: "control" }]);
});
it("keeps destinations opened after the sending review and ignores a removed flow", () => {
  const ui = new UiStore("navigation");
  const review = { screen: "review", projectId: "project", taskId: null } as const;
  ui.push(review, "control");
  ui.push({ screen: "settings" });
  ui.finish(["review", "annotate"], "control", review);
  expect(ui.route()).toEqual({ screen: "settings" });
  const state = ui.get();
  ui.finish(["review", "annotate"], "control", review);
  expect(ui.get()).toBe(state);
});
