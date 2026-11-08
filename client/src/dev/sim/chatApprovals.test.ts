// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Approval, AttentionItem, Command, Task } from "../../protocol/types";
import { startConversation } from "../../screens/talk/conversation";
import { SimHost } from "./host";

let host: SimHost;
let seq = 0;
beforeEach(() => { vi.useFakeTimers(); host = new SimHost(); });
afterEach(() => { host.dispose(); vi.useRealTimers(); });
const send = (body: Command, expected: number | null = null) =>
  host.command({ command_id: `chat-approvals-${++seq}`, issued_at: Date.now(), expires_at: null, expected_revision: expected, body });

/** A run of `task` asks the person for `kind`, as the host relays it. */
function raise(task: Task, kind: Approval["kind"]): Approval {
  const run = host.runs.find((r) => r.task_id === task.task_id)!;
  const id = `approval-${++seq}`;
  const approval: Approval = {
    approval_id: id, run_id: run.run_id, task_id: task.task_id, project_id: task.project_id, native_request_id: id, kind,
    title: kind, detail: null, raw_request: null, scope: "once", revision: 1, state: "pending",
    requested_at: Date.now(), expires_at: null, resolved_by: null, resolved_at: null,
  };
  const item: AttentionItem = {
    attention_id: `attention-${id}`, kind: "approval", title: kind, body: null, task_id: task.task_id, run_id: run.run_id,
    approval_id: id, created_at: Date.now(), resolved_at: null, priority: 10,
  };
  host.ask(approval, item);
  return approval;
}
const allow = (a: Approval) => send({ command: "resolve_approval", approval_id: a.approval_id, decision: { decision: "allow" } }, a.revision);
const deny = (a: Approval) => send({ command: "resolve_approval", approval_id: a.approval_id, decision: { decision: "deny", reason: null } }, a.revision);

async function started(body: Command): Promise<Task> {
  const before = new Set(host.tasks.map((t) => t.task_id));
  expect((await send(body)).status).toBe("accepted");
  return host.tasks.find((t) => !before.has(t.task_id))!;
}

it("lets a chat allow a command or edit inside its sandbox, and only decline what acts outside it", async () => {
  const projectId = host.projects[0]!.project_id;
  const chat = await started(startConversation(projectId, "Tidy the scratch folder", { harness: "claude_code", model: null, effort: null }, {
    access: { read: [projectId], write: [projectId], whole_read: false, whole_write: false, internet: false, local_network: false },
  }));
  expect(chat).toMatchObject({ conversation: true, mode: "ask" });
  for (const kind of ["command", "file_edit"] as const) {
    const a = raise(chat, kind);
    // The card offers exactly what the computer takes.
    expect(host.approvals.find((x) => x.approval_id === a.approval_id)?.actions).toEqual(["allow", "deny"]);
    expect((await allow(a)).status).toBe("accepted");
  }
  for (const kind of ["tool", "network", "github_post", "other"] as const) {
    const a = raise(chat, kind);
    expect((await allow(a)).error?.code).toBe("unsupported");
    expect((await deny(a)).status).toBe("accepted");
  }
});

it("still refuses Allow in an Ask task that is not a chat", async () => {
  const projectId = host.projects[0]!.project_id;
  const ask = await started({ ...startConversation(projectId, "Why is this slow?", { harness: "claude_code", model: null, effort: null }), conversation: false } as Command);
  expect(ask.conversation).toBeFalsy();
  expect(ask.mode).toBe("ask");
  const a = raise(ask, "command");
  expect(host.approvals.find((x) => x.approval_id === a.approval_id)?.actions).toEqual(["switch_to_work", "deny"]);
  expect((await allow(a)).error?.code).toBe("unsupported");
  expect((await deny(a)).status).toBe("accepted");
});
