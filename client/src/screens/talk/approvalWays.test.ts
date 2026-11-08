// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { Approval, ApprovalAction, Task } from "../../protocol/types";
import { approvalWays, asSwitched, canSwitch, switchCommand } from "./approvalWays";

// An agent's request for more access is stored as kind "other" with what it asks.
const approval = (kind: Approval["kind"] | "access", actions?: ApprovalAction[]): Approval => ({
  approval_id: "a1", run_id: "r1", task_id: "t1", project_id: "p1", native_request_id: "n1",
  kind: kind === "access" ? "other" : kind,
  ...(kind === "access" ? { access_request: { needs: ["change_files"], reason: "to fix it" } } : {}),
  title: "Run the build", detail: null, raw_request: null, scope: "once", revision: 1, state: "pending",
  requested_at: 0, expires_at: null, resolved_by: null, resolved_at: null,
  ...(actions ? { actions } : {}),
});
const task = (over: Partial<Task> = {}): Task => ({
  task_id: "t1", project_id: "p1", title: "Fix the build", mode: "plan", kind: "coding", state: "working",
  ...over,
} as Task);
const ways = (a: Approval, over: Partial<Parameters<typeof approvalWays>[1]> = {}) =>
  approvalWays(a, { task: task(), agent: "Claude Code", project: "ophio", holder: null, switches: true, ...over });
const labels = (a: Approval, over: Partial<Parameters<typeof approvalWays>[1]> = {}) => ways(a, over).ways.map((w) => w.label);

it("offers only what the computer says it takes", () => {
  expect(labels(approval("command", ["switch_to_work", "deny"]))).toEqual(["Switch to Work and allow", "Decline"]);
  expect(labels(approval("command", ["allow", "deny"]))).toEqual(["Allow", "Decline"]);
  expect(labels(approval("access", ["switch_to_work", "deny"]))).toEqual(["Switch to Work", "Not now"]);
  expect(labels(approval("access", ["allow", "deny"]), { task: task({ conversation: true }) })).toEqual(["Allow", "Not now"]);
  expect(labels(approval("login_use", ["turn_into_task", "deny"]), { task: task({ conversation: true }) })).toEqual(["Turn into a task", "Decline"]);
  // Never an Allow the computer would refuse.
  expect(labels(approval("command", ["switch_to_work", "deny"]))).not.toContain("Allow");
  expect(ways(approval("command", ["switch_to_work", "deny"])).ways[0]!.primary).toBe(true);
});

it("keeps the old buttons for a computer that predates switching", () => {
  expect(labels(approval("command"), { switches: false })).toEqual(["Allow", "Decline"]);
  expect(labels(approval("question"), { switches: false })).toEqual(["Answer", "Decline"]);
  // A newer card sent to an older computer never sends a switch.
  expect(labels(approval("command", ["switch_to_work", "deny"]), { switches: false })).toEqual(["Allow", "Decline"]);
});

it("says when a switch waits its turn", () => {
  const { note } = ways(approval("access", ["switch_to_work", "deny"]), { holder: { title: "Ship the release" } });
  expect(note).toBe("“Ship the release” is changing ophio, so this waits its turn in Next up, then Claude Code carries on in Work.");
  expect(ways(approval("access", ["switch_to_work", "deny"])).note).toBeNull();
});

it("switches a Plan or Ask task, or a chat, and nothing else", () => {
  expect(canSwitch(task())).toBe(true);
  expect(canSwitch(task({ mode: "ask" }))).toBe(true);
  expect(canSwitch(task({ mode: "ask", conversation: true }))).toBe(true);
  expect(canSwitch(task({ mode: "work" }))).toBe(false);
  expect(canSwitch(task({ kind: "computer_use" }))).toBe(false);
  expect(canSwitch(task({ github: { link: { type: "triage" } } } as Partial<Task>))).toBe(false);
  expect(switchCommand("t1", "a1")).toEqual({ command: "switch_to_work", task_id: "t1", approval_id: "a1" });
  expect(switchCommand("t1", null, "  ")).toEqual({ command: "switch_to_work", task_id: "t1" });
  const chat = asSwitched(task({ mode: "ask", conversation: true, access: { read: [], write: [], whole_read: false, whole_write: false, internet: true, local_network: false } }));
  expect(chat).toMatchObject({ mode: "work", conversation: false, access: { write: ["p1"], read: ["p1"] } });
});

it("never offers what this phone may not do", () => {
  const phone = { changesTasks: false };
  expect(labels(approval("access", ["switch_to_work", "deny"]), phone)).toEqual(["Not now"]);
  expect(labels(approval("access", ["allow", "deny"]), { ...phone, task: task({ conversation: true }) })).toEqual(["Not now"]);
  // Allowing a command it asked for doesn't change the task.
  expect(labels(approval("command", ["allow", "deny"]), phone)).toEqual(["Allow", "Decline"]);
  expect(ways(approval("access", ["switch_to_work", "deny"]), phone).note).toMatch(/can’t do this/);
});
