// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { withoutSecret } from "./session";
import type { Command } from "../protocol/types";

it("withholds answer copies when authoritative questions are unavailable", () => {
  const command: Command = { command: "resolve_approval", approval_id: "secret", decision: {
    decision: "answer", text: "issue91-password-sentinel",
    answers: [{ question: "Password?", choices: ["issue91-password-sentinel"] }],
  } };
  expect(JSON.stringify(withoutSecret(command))).not.toContain("issue91-password-sentinel");
  expect(JSON.stringify(command)).toContain("issue91-password-sentinel");
});

import { snapshot } from "../platform/lifecycle.fixtures";
import { retainedApproval, retainedQuery } from "./secretAnswers";
import { HostSession } from "./session";
import type { ControlLink, LinkEvent } from "./link";
import type { Approval, ClientFrame } from "../protocol/types";

const decision = { decision: "answer" as const, text: "issue91-password-sentinel",
  answers: [{ question: "Password?", choices: ["issue91-password-sentinel"] }] };
const approval: Approval = { ...snapshot().approvals[0]!,
  questions: [{ question: "Password?", options: [], secret: true, multi_select: false }],
  delivery: { decision, at: 1, error: null } };

it("uses authoritative questions and preserves ordinary answers", () => {
  const body: Command = { command: "resolve_approval", approval_id: approval.approval_id, decision };
  expect(JSON.stringify(withoutSecret(body, approval))).not.toContain("issue91-password-sentinel");
  expect(withoutSecret(body, { ...approval, questions: [] })).toEqual(body);
  expect(retainedApproval({ ...approval, questions: [] }).delivery?.decision).toEqual(decision);
});

it("scrubs old delivery records in queries and snapshots without mutating the source", () => {
  expect(JSON.stringify(retainedQuery({ kind: "approval", approval }))).not.toContain("issue91-password-sentinel");
  expect(JSON.stringify(retainedQuery({ kind: "snapshot", ...snapshot(), approvals: [approval] }))).not.toContain("issue91-password-sentinel");
  expect(approval.delivery?.decision).toEqual(decision);
});

it("sends the original once while retaining only a redacted command and journal approval", async () => {
  const sent: ClientFrame[] = [];
  let listener: ((event: LinkEvent) => void) | undefined;
  const link: ControlLink = {
    connect() {}, disconnect() {},
    send(frame) { sent.push(frame); return true; },
    listen(fn) { listener = fn; return () => { listener = undefined; }; },
  };
  const session = new HostSession("secret", link);
  session.start();
  try {
    listener!({ type: "frame", frame: { type: "event", seq: 1, at: 1,
      source: { kind: "companion" }, provenance: "observed",
      event: { type: "approval_upserted", approval } } });
    expect(JSON.stringify(session.state.get().approvals)).not.toContain("issue91-password-sentinel");
    const promise = session.command({ command: "resolve_approval", approval_id: approval.approval_id, decision });
    const command = sent.find(f => f.type === "command")!;
    expect(JSON.stringify(command)).toContain("issue91-password-sentinel");
    expect(JSON.stringify(session.commands.get())).not.toContain("issue91-password-sentinel");
    if (command.type !== "command") throw new Error("command not sent");
    listener!({ type: "frame", frame: { type: "receipt", command_id: command.command_id,
      status: "accepted", seq: 2, result: null, error: null } });
    await promise;
  } finally { session.stop(); }
});
