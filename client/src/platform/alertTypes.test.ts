// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { AlertTopic, AlertType, AttentionItem, ProjectNotifications, Tier } from "../protocol/types";
import { ALERT_TYPES, alertTypeOf, recommendedTiers, tierFor, withOwnTier } from "./alertTypes";

type Fields = Parameters<typeof alertTypeOf>[0];
const item = (over: Partial<Fields> & Pick<Fields, "kind">): Fields => ({ topic: undefined, alert: undefined, account_id: undefined, task_id: "task", report_id: undefined, ...over } as Fields);
type Kind = AttentionItem["kind"];

it("names all 18 types, each with a name, description, group and recommended choice", () => {
  expect(ALERT_TYPES).toHaveLength(18);
  expect(new Set(ALERT_TYPES.map((t) => t.type)).size).toBe(18);
  for (const t of ALERT_TYPES) {
    expect(t.name.length).toBeGreaterThan(0);
    expect(t.description.length).toBeGreaterThan(0);
    expect(["needs_you", "worth_a_look", "in_the_app"]).toContain(t.group);
    expect(["alert", "silent", "off"]).toContain(t.tier);
  }
  expect(recommendedTiers().question).toBe("alert");
  expect(recommendedTiers().finished).toBe("off");
  expect(recommendedTiers().reply).toBe("silent");
});

it("the item's own type wins over everything else about it", () => {
  expect(alertTypeOf(item({ kind: "completed", alert: "batch", account_id: "claude", task_id: null, report_id: "r" }), true)).toBe("batch");
  expect(alertTypeOf(item({ kind: "connection_issue", alert: "app_update" }), false)).toBe("app_update");
});

const rows: [string, Fields, boolean, AlertType | null][] = [
  ["an agent not signed in, with no task", item({ kind: "question", account_id: "claude", task_id: null }), false, "sign_in"],
  ["an account on an item that has a task", item({ kind: "question", account_id: "claude" }), false, "question"],
  ["a question about a report", item({ kind: "question", report_id: "r" }), false, "report_question"],
  ["a question about a report, with no task", item({ kind: "question", report_id: "r", task_id: null }), false, "report_question"],
  ["a report with no task", item({ kind: "completed", report_id: "r", task_id: null }), false, "reports"],
  ["a report that has a task falls through to its kind", item({ kind: "completed", report_id: "r" }), false, "finished"],
  ["a question", item({ kind: "question" }), false, "question"],
  ["an approval", item({ kind: "approval" }), false, "approval"],
  ["a conflict", item({ kind: "conflict" }), false, "decision"],
  ["a reconciliation", item({ kind: "reconciliation_required" }), false, "decision"],
  ["a failure", item({ kind: "failure" }), false, "failed"],
  ["a connection issue", item({ kind: "connection_issue" }), false, null],
  ["a connection issue with a topic", item({ kind: "connection_issue", topic: "finished" }), true, null],
  ["a plain completed task", item({ kind: "completed" }), false, "finished"],
  ["a completed chat", item({ kind: "completed" }), true, "reply"],
  ["completed, topic merged", item({ kind: "completed", topic: "merged" }), false, "merged"],
  ["completed, topic merged, in a chat", item({ kind: "completed", topic: "merged" }), true, "merged"],
  ["completed, topic pr_ready", item({ kind: "completed", topic: "pr_ready" }), true, "pr_ready"],
  ["completed, topic issues", item({ kind: "completed", topic: "issues" }), false, "issues"],
  ["completed, topic reports", item({ kind: "completed", topic: "reports" }), false, "reports"],
  ["completed, topic pr_needs_you", item({ kind: "completed", topic: "pr_needs_you" }), true, "pr_needs_you"],
  ["completed, another topic", item({ kind: "completed", topic: "finished" }), false, "finished"],
  ["completed, another topic in a chat", item({ kind: "completed", topic: "slow_work" }), true, "reply"],
  ["waiting, no topic", item({ kind: "waiting_for_you" }), false, "decision"],
  ["waiting, topic needs_you", item({ kind: "waiting_for_you", topic: "needs_you" }), true, "decision"],
  ["waiting, topic pr_needs_you", item({ kind: "waiting_for_you", topic: "pr_needs_you" }), false, "pr_needs_you"],
  ["waiting, topic issues", item({ kind: "waiting_for_you", topic: "issues" }), false, "issue_call"],
  ["waiting, topic slow_work", item({ kind: "waiting_for_you", topic: "slow_work" }), false, "slow"],
  ["waiting, topic finished", item({ kind: "waiting_for_you", topic: "finished" }), false, "failed"],
  ["waiting, topic failed", item({ kind: "waiting_for_you", topic: "failed" }), false, "failed"],
  ["waiting, topic reports", item({ kind: "waiting_for_you", topic: "reports" }), false, "reports"],
  ["waiting, topic pr_ready", item({ kind: "waiting_for_you", topic: "pr_ready" }), false, "pr_ready"],
  ["waiting, topic merged", item({ kind: "waiting_for_you", topic: "merged" }), false, "merged"],
  ["waiting, topic question", item({ kind: "waiting_for_you", topic: "question" }), false, "decision"],
];
it.each(rows)("alertTypeOf: %s", (_name, fields, conversation, expected) => {
  expect(alertTypeOf(fields, conversation)).toBe(expected);
});

it("alertTypeOf covers every kind and every topic without throwing", () => {
  const kinds: Kind[] = ["approval", "question", "failure", "completed", "reconciliation_required", "waiting_for_you", "conflict", "connection_issue"];
  const topics: (AlertTopic | undefined)[] = [undefined, "question", "approval", "needs_you", "finished", "failed", "pr_ready", "pr_needs_you", "merged", "issues", "slow_work", "reports"];
  for (const kind of kinds) for (const topic of topics) for (const conversation of [false, true]) {
    const type = alertTypeOf(item({ kind, topic }), conversation);
    if (kind === "connection_issue") expect(type).toBeNull();
    else expect(ALERT_TYPES.map((t) => t.type)).toContain(type);
  }
});

const project = (over: Partial<ProjectNotifications> = {}): ProjectNotifications => ({ project_id: "p", mute: false, on: [], off: [], ...over });

it("tierFor: the device's choice applies when the project has none", () => {
  expect(tierFor("question", "silent")).toBe("silent");
  expect(tierFor("question", "alert", project())).toBe("alert");
  expect(tierFor("finished", "off", project({ tiers: [{ type: "question", tier: "alert" }] }))).toBe("off");
});

it("tierFor: a project's own choice beats the device's, for that type only", () => {
  const p = project({ tiers: [{ type: "question", tier: "off" }, { type: "reply", tier: "alert" }] });
  expect(tierFor("question", "alert", p)).toBe("off");
  expect(tierFor("reply", "silent", p)).toBe("alert");
  expect(tierFor("approval", "silent", p)).toBe("silent");
});

it("tierFor: a muted project shows nothing, whatever the device or project says", () => {
  expect(tierFor("question", "alert", project({ mute: true }))).toBe("off");
  expect(tierFor("question", "alert", project({ mute: true, tiers: [{ type: "question", tier: "alert" }] }))).toBe("off");
});

it("tierFor: a task's bell makes finished, failed and reply alert, beating mute, project and device", () => {
  const p = project({ mute: true, tiers: [{ type: "finished", tier: "off" }, { type: "failed", tier: "silent" }, { type: "reply", tier: "off" }] });
  for (const type of ["finished", "failed", "reply"] as const) {
    expect(tierFor(type, "off", p, true)).toBe("alert");
    expect(tierFor(type, "off", undefined, true)).toBe("alert");
  }
});

it("tierFor: the bell changes nothing for other types", () => {
  const cases: [AlertType, Tier, ProjectNotifications | undefined, Tier][] = [
    ["merged", "off", undefined, "off"],
    ["question", "silent", undefined, "silent"],
    ["pr_ready", "alert", project({ mute: true }), "off"],
    ["issues", "off", project({ tiers: [{ type: "issues", tier: "silent" }] }), "silent"],
  ];
  for (const [type, device, p, expected] of cases) expect(tierFor(type, device, p, true)).toBe(expected);
});

it("withOwnTier sets, replaces and clears a project's choice for one type", () => {
  const set = withOwnTier(project(), "reply", "alert");
  expect(set.tiers).toEqual([{ type: "reply", tier: "alert" }]);
  expect(withOwnTier(set, "reply", "off").tiers).toEqual([{ type: "reply", tier: "off" }]);
  expect(withOwnTier(set, "reply", null).tiers).toBeUndefined();
});
