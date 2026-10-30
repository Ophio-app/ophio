// SPDX-License-Identifier: Apache-2.0
// Prerequisites travel with the urgent item that needs them, across sections.
import { expect, it } from "vitest";
import type { Checklist, ChecklistItem } from "../../protocol/types";
import { checklistUrgency, nextItem, orderedChecklistItems, ticked } from "../../app/checklist";
import { checklistOrder, compareTodo } from "./todoOrder";

function item(id: string, rest: Partial<ChecklistItem> = {}): ChecklistItem {
  return { item_id: id, title: id, priority: "medium", steps: [], added_at: 0, changed_at: 0, ...rest };
}
function list(project: string, items: ChecklistItem[]): Checklist {
  return { project_id: project, title: project, sections: [{ section_id: "s", title: "S", items }], revision: 1, created_at: 0, updated_at: 0 };
}

it("carries prerequisites ahead of due dependents in All, including chains and another project", () => {
  const a = list("a", [item("due", { due: "2026-10-18", after: ["prep"] }), item("prep", { after: ["first"], priority: "low" }), item("first", { priority: "low" }), item("rest")]);
  const b = list("b", [item("older", { due: "2026-10-17" })]);
  expect([...checklistOrder(a, "2026-10-18"), ...checklistOrder(b, "2026-10-18")].sort(compareTodo).map((v) => v.id)).toEqual(["older", "first", "prep", "due", "rest"]);
});

it("keeps a finished prerequisite in the done fold instead of reopening it", () => {
  const a = list("a", [item("due", { due: "2026-10-18", after: ["prep"] }), item("prep", { done_at: 1 })]);
  expect(checklistOrder(a, "2026-10-18").sort(compareTodo).map((v) => [v.id, v.done])).toEqual([["due", false], ["prep", true]]);
});

it("keeps a waited-on person's item in place after its optimistic tick", () => {
  const a = list("a", [item("card", { priority: "high" }), item("grant"), item("deploy", { owner: "agent", after: ["grant"] })]);
  const before = orderedChecklistItems(a).map(i => i.item_id);
  const done = { ...a, sections: a.sections.map(s => ({ ...s, items: s.items.map(i => i.item_id === "grant" ? ticked(i, undefined, true, Date.now()) : i) })) };
  expect(orderedChecklistItems(done).map(i => i.item_id)).toEqual(before);
});

it("has no person Next while its only open person item awaits the agent", () => {
  const a = list("a", [item("grant", { done_at: 1 }), item("deploy", { owner: "agent", after: ["grant"] }), item("review", { after: ["deploy"] })]);
  expect(nextItem(a)).toBeNull();
});

it("only promotes person prerequisites of open agents, preserving a finished handoff's rank", () => {
  const a = list("a", [item("build", { owner: "agent" }), item("grant"), item("deploy", { owner: "agent", after: ["build", "grant"] })]);
  expect(checklistUrgency(a).get("build")?.tier).toBe(2);
  expect(checklistUrgency(a).get("grant")?.tier).toBe(0);
  a.sections[0]!.items[2]!.done_at = 20;
  expect(checklistUrgency(a).get("grant")?.tier).toBe(2);
  a.sections[0]!.items[1]!.done_at = 10;
  expect(checklistUrgency(a).get("grant")?.tier).toBe(0);
  a.sections[0]!.items[1]!.done_at = 30;
  expect(checklistUrgency(a).get("grant")?.tier).toBe(2);
});
