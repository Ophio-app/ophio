// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { Approval, TimelineBody, TimelineItem } from "../../protocol/types";
import { approvals } from "../../dev/sim/scenario";
import { answerTime, questionReply } from "./answeredReply";

const a: Approval = { ...approvals(1000)[0]!, kind: "question", state: "allowed", requested_at: 100, resolved_at: 200,
  delivery: { decision: { decision: "answer", text: "Explain the choices" }, at: 210, error: null } };
const item = (seq: number, at: number, body: TimelineBody, run = a.run_id): TimelineItem => ({ seq, at, run_id: run, item: body });
const anchor = item(1, 100, { kind: "approval", approval_id: a.approval_id });
const message = (seq: number, at: number, text: string, run = a.run_id) => item(seq, at, { kind: "agent_message", text, partial: false }, run);

it("retains every subsequent saved message, not just the short final", () => {
  const result = questionReply(a, [message(0, 50, "Before"), anchor, message(2, 150, "Before answer"), message(3, 211, "Detailed explanation"), message(4, 500, "Choose A or B")], {});
  expect(result.messages.map(i => i.item)).toEqual([
    { kind: "agent_message", text: "Detailed explanation", partial: false },
    { kind: "agent_message", text: "Choose A or B", partial: false },
  ]);
});

it.each(["user", "question", "missing request", "run start", "run end"])("stops at the next %s even when its timestamp equals the answer's", boundary => {
  const next = { ...a, approval_id: "next", requested_at: 210 };
  const body: TimelineBody = boundary === "user" ? { kind: "user_message", text: "New topic", bundle: null, delivery: "native_delivered" }
    : boundary === "run start" ? { kind: "run_started", run_id: "other" }
    : boundary === "run end" ? { kind: "run_ended", run_id: a.run_id, state: "completed" }
    : { kind: "approval", approval_id: "next" };
  const result = questionReply(a, [anchor, message(2, 211, "Explanation"), item(3, 210, body), message(4, 500, "Not its reply")], boundary === "missing request" ? {} : { next });
  expect(result.closed).toBe(true);
  expect(result.messages).toHaveLength(1);
});

it("ignores other runs and equal-time messages, using the later of delivery and resolution", () => {
  const result = questionReply(a, [anchor, message(2, 201, "After resolution, before delivery"), message(3, 210, "Equal time"), message(4, 300, "Other run", "other"), message(5, 400, "Safe")], {});
  expect(result.messages.map(i => i.seq)).toEqual([5]);
  expect(result.uncertain).toBe(true);
  expect(questionReply({ ...a, resolved_at: 350 }, [anchor, message(2, 300, "Before resolution")], {}).messages).toEqual([]);
});

it("does not cross a question that was asked before answer delivery or infer ordering from time alone", () => {
  const next = { ...a, approval_id: "next" };
  const result = questionReply(a, [anchor, item(2, 150, { kind: "approval", approval_id: "next" }), message(3, 500, "For the newer question")], { next });
  expect(result.messages).toEqual([]);
  expect(result.closed).toBe(true);
});

it("passes an ordinary permission request without adding permission UI or treating it as a question", () => {
  const permission = { ...a, approval_id: "permission", kind: "command" as const };
  const result = questionReply(a, [anchor, item(2, 220, { kind: "approval", approval_id: permission.approval_id }), message(3, 230, "Still explaining")], { permission });
  expect(result.messages).toHaveLength(1);
  expect(result.closed).toBe(false);
});

it("requires a successful delivered answer, its own run anchor and a known resolution time", () => {
  const messages = [anchor, message(2, 400, "Do not attach")];
  for (const changed of [
    { ...a, state: "pending" as const },
    { ...a, delivery: undefined },
    { ...a, delivery: { ...a.delivery!, error: { code: "native_failure" as const, message: "Failed" } } },
    { ...a, delivery: { ...a.delivery!, decision: { decision: "allow" as const } } },
    { ...a, resolved_at: null },
    { ...a, outcome: "denied" as const },
    { ...a, kind: "command" as const },
  ]) {
    expect(answerTime(changed)).toBeNull();
    expect(questionReply(changed, messages, {}).messages).toEqual([]);
  }
  expect(questionReply(a, messages.slice(1), {}).anchored).toBe(false);
  expect(questionReply({ ...a, run_id: "other" }, messages, {}).messages).toEqual([]);
});

it("never lends pre-answer message segments or an unanchored tail to the answer", () => {
  const segment = (seq: number, at: number, key: string, offset: number) => item(seq, at, { kind: "agent_message", text: "part", partial: true, message_key: key, text_offset: offset });
  const result = questionReply(a, [segment(0, 50, "old", 0), anchor, segment(2, 150, "early", 0), segment(3, 211, "old", 4), segment(4, 212, "early", 4), segment(5, 213, "tail", 4), segment(6, 214, "new", 0), segment(7, 215, "new", 4)], {});
  expect(result.messages.map(i => i.seq)).toEqual([6, 7]);
  expect(result.uncertain).toBe(true);
});
