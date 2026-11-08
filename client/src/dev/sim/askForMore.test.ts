// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Approval, Command, ConversationAccess, Task, TimelineBody } from "../../protocol/types";
import { startConversation } from "../../screens/talk/conversation";
import { actions, asked, refusal, switchRefusal } from "./approvalRule";
import { missing, needsText, title, widen } from "./askForMore";
import { SimHost } from "./host";

let host: SimHost;
let seq = 0;
beforeEach(() => {
  vi.useFakeTimers();
  host = new SimHost();
  expect(host.projects[1]!.active_writer).toBeNull();
});
afterEach(() => { host.dispose(); vi.useRealTimers(); });
const send = (body: Command, expected: number | null = null) =>
  host.command({ command_id: `ask-for-more-${++seq}`, issued_at: Date.now(), expires_at: null, expected_revision: expected, body });
const allow = (a: Approval) => send({ command: "resolve_approval", approval_id: a.approval_id, decision: { decision: "allow" } }, a.revision);
const approval = (id: string | null) => host.approvals.find((a) => a.approval_id === id)!;
const task = (id: string) => host.tasks.find((t) => t.task_id === id)!;
const run = (id: string | null) => host.runs.find((r) => r.run_id === id)!;
const notes = (taskId: string) => host.journal.flatMap(({ event }) =>
  event.type === "timeline_appended" && event.task_id === taskId && event.item.item.kind === "note" ? [(event.item.item as Extract<TimelineBody, { kind: "note" }>).text] : []);

// The first project's writer is held by the seeded flaky-test task; the second is free.
const free = () => host.projects[1]!;
const projectId = () => free().project_id;
const projectName = () => free().name;
const otherId = () => host.projects[0]!.project_id;
const agent = { harness: "claude_code" as const, model: null, effort: null };
const none: ConversationAccess = { read: [], write: [], whole_read: false, whole_write: false, internet: false, local_network: false };

/** Starts a task and lets its agent begin its turn. */
async function started(body: Command): Promise<Task> {
  const before = new Set(host.tasks.map((t) => t.task_id));
  expect((await send(body)).status).toBe("accepted");
  const id = host.tasks.find((t) => !before.has(t.task_id))!.task_id;
  await vi.advanceTimersByTimeAsync(600);
  host.keepWorking(id);
  return task(id);
}
const chat = (access: ConversationAccess = { ...none, read: [projectId()] }) =>
  started(startConversation(projectId(), "Look at the build", agent, { access }));
const planTask = () => started({ ...startConversation(projectId(), "Plan the retry change", agent), conversation: false, mode: "plan" } as Command);

it("ports the rule's buttons exactly", () => {
  const t = (over: Partial<Task>) => ({ task_id: "t", project_id: "p", kind: "coding", mode: "plan", ...over }) as Task;
  expect(actions(asked("command", "plan", "plan", t({})))).toEqual(["switch_to_work", "deny"]);
  expect(actions(asked("command", "plan", "plan", t({ github: { link: { type: "triage" } } } as Partial<Task>)))).toEqual(["deny"]);
  const chatTask = t({ mode: "ask", conversation: true, access: none });
  expect(actions(asked("command", "ask", "work", chatTask))).toEqual(["allow", "deny"]);
  expect(actions(asked("other", "ask", "work", chatTask, true))).toEqual(["allow", "deny"]);
  expect(actions(asked("login_code", "ask", "work", chatTask))).toEqual(["deny"]);
  expect(actions(asked("question", "plan", "plan", t({})))).toEqual(["answer", "deny"]);
  expect(refusal(asked("other", "ask", "work", chatTask, true), { decision: "allow" }, true)?.code).toBe("permission_denied");
  expect(switchRefusal(asked("command", "work", "work", t({ mode: "work" })))?.code).toBe("invalid_request");
});

it("ports the access words and gives exactly what was asked", () => {
  expect(title("Claude Code", ["change_files", "local_network"], "ophio", "to build the Mac app on the MacBook."))
    .toBe("Claude Code needs to change files in ophio and reach your own network to build the Mac app on the MacBook.");
  expect(title("Codex", ["internet", "local_network"], "site", "Fetch the release notes")).toBe("Codex needs to reach the internet and your own network: Fetch the release notes.");
  expect(needsText(["change_files", "internet", "whole_computer"], "app")).toBe("change files in app, reach the internet and read and change anything on this computer");
  const base = { ...none, read: ["other"] };
  expect(widen(base, "here", ["local_network"])).toEqual({ ...base, internet: true, local_network: true });
  expect(widen(base, "here", ["change_files"])).toEqual({ ...base, read: ["here", "other"], write: ["here"] });
  expect(missing({ ...none, write: ["here"] }, "here", ["change_files", "internet", "local_network"])).toEqual(["internet", "local_network"]);
});

it("switches a Plan task to Work to allow its command, never allowing it while the read-only turn lasts", async () => {
  const plan = await planTask();
  const a = approval(host.askToRunAndWait(plan.task_id, "Run the payments tests", "cargo test -p payments-client"));
  expect(a.actions).toEqual(["switch_to_work", "deny"]);
  expect((await allow(a)).error?.code).toBe("unsupported");
  const readOnly = a.run_id;
  const seen: { state: string; run: string }[] = [];
  host.attach((f) => {
    if (f.type === "event" && f.event.type === "approval_upserted" && f.event.approval.approval_id === a.approval_id)
      seen.push({ state: f.event.approval.state, run: run(readOnly).state });
  });
  expect((await send({ command: "switch_to_work", task_id: plan.task_id, approval_id: a.approval_id })).status).toBe("accepted");
  // The task shows Work at once, and the request is settled allowed.
  expect(task(plan.task_id).mode).toBe("work");
  expect(approval(a.approval_id)).toMatchObject({ state: "allowed", resolved_by: { kind: "device" } });
  expect(seen).toEqual([{ state: "allowed", run: "stopping" }]);
  await vi.advanceTimersByTimeAsync(1000);
  expect(run(readOnly).state).toBe("interrupted");
  const next = run(task(plan.task_id).current_run);
  expect(next.run_id).not.toBe(readOnly);
  expect(next.mode).toBe("work");
  expect(notes(plan.task_id)).toContain(`Switched to Work mode. Claude Code carries on in the same session and may now change ${projectName()}.`);
});

it("lets a chat allow exactly the access it asked for, and nothing more", async () => {
  const c = await chat();
  const id = host.askForMore(c.task_id, ["change_files", "local_network"], "to build the Mac app on the MacBook");
  const a = approval(id);
  expect(a).toMatchObject({
    kind: "other", actions: ["allow", "deny"], access_request: { needs: ["change_files", "internet", "local_network"] },
    title: `Claude Code needs to change files in ${projectName()} and reach the internet and your own network to build the Mac app on the MacBook.`,
    scope: `Turns on, for this chat: Change files in ${projectName()} and reach the internet and your own network. Nothing else changes.`,
  });
  expect(run(a.run_id).state).toBe("awaiting_approval");
  expect((await allow(a)).status).toBe("accepted");
  const access = task(c.task_id).access!;
  expect(access).toEqual({ ...none, read: [projectId()], write: [projectId()], internet: true, local_network: true });
  expect(access.write).not.toContain(otherId());
  expect(task(c.task_id).conversation).toBe(true);
  // Asked again, it has it all already.
  expect(host.askForMore(c.task_id, ["change_files"], "to try again")).toBeNull();
});

it("offers a Plan task's access request as a switch to Work", async () => {
  const plan = await planTask();
  const a = approval(host.askForMore(plan.task_id, ["change_files"], "to write the fix"));
  expect(a.actions).toEqual(["switch_to_work", "deny"]);
  expect(a.scope).toBe(`Switches this task to Work mode, in the same session: Claude Code can then change files and run commands in ${projectName()}, as far as its own permissions allow.`);
  // An older phone's Allow switches, as the card says.
  expect((await allow(a)).status).toBe("accepted");
  expect(task(plan.task_id).mode).toBe("work");
  expect(approval(a.approval_id).state).toBe("allowed");
});

it("offers a chat's login or GitHub post as Turn into a task, and refuses Allow", async () => {
  const c = await chat();
  const r = run(c.current_run);
  for (const kind of ["login_use", "github_post"] as const) {
    const id = `${kind}-${++seq}`;
    host.ask({
      approval_id: id, run_id: r.run_id, task_id: c.task_id, project_id: c.project_id, native_request_id: id, kind, title: kind, detail: null,
      raw_request: null, scope: "once", revision: 1, state: "pending", requested_at: Date.now(), expires_at: null, resolved_by: null, resolved_at: null,
    }, { attention_id: `at-${id}`, kind: "approval", title: kind, body: null, task_id: c.task_id, run_id: r.run_id, approval_id: id, created_at: Date.now(), resolved_at: null, priority: 10 });
    expect(approval(id).actions).toEqual(["turn_into_task", "deny"]);
    expect((await allow(approval(id))).error?.code).toBe("unsupported");
  }
});

it("waits in Next up when another task holds the project's writer", async () => {
  const holder = host.holdWriter(projectId());
  await vi.advanceTimersByTimeAsync(600);
  const plan = await planTask();
  const a = approval(host.askForMore(plan.task_id, ["change_files"], "to write the fix"));
  expect((await send({ command: "switch_to_work", task_id: plan.task_id, approval_id: a.approval_id })).status).toBe("accepted");
  await vi.advanceTimersByTimeAsync(1000);
  const ended = task(plan.task_id).current_run;
  expect(run(ended).state).toBe("interrupted");
  const waiting = host.talk.queue(projectId()).items.find((i) => i.task_id === plan.task_id && i.state.state !== "started");
  expect(waiting?.authorization).toBe("auto_start");
  expect(notes(plan.task_id)).toContain(`Switched to Work. Waits in Next up while “${task(holder).title}” changes ${projectName()}, then Claude Code carries on in the same session.`);
  // Once the other task is done, it carries on in Work by itself.
  host.finishTurn(holder);
  const next = run(task(plan.task_id).current_run);
  expect(next.run_id).not.toBe(ended);
  expect(next.mode).toBe("work");
});

it("turns a running chat into a task at once, without waiting for its turn to end", async () => {
  const c = await chat();
  const turn = c.current_run;
  expect((await send({ command: "switch_to_work", task_id: c.task_id })).status).toBe("accepted");
  const now = task(c.task_id);
  expect(now).toMatchObject({ conversation: false, mode: "work" });
  expect(now.access?.write).toContain(projectId());
  expect(run(turn).state).toBe("stopping");
  await vi.advanceTimersByTimeAsync(1000);
  expect(run(task(c.task_id).current_run).mode).toBe("work");
  expect(notes(c.task_id)).toContain(`Turned into a task in Work mode. Claude Code carries on in the same session and may now change ${projectName()}.`);
});

it("refuses a switch the computer would refuse", async () => {
  const plan = await planTask();
  expect((await send({ command: "switch_to_work", task_id: plan.task_id })).status).toBe("accepted");
  expect((await send({ command: "switch_to_work", task_id: plan.task_id })).error?.message).toBe("This task is already in Work mode");
  const fresh = (await send({ ...startConversation(projectId(), "Later", agent), conversation: false, mode: "plan", placement: { mode: "queue", position: null, authorization: "ask_before_start" } } as Command));
  const queued = (fresh.result as { task_id: string }).task_id;
  expect((await send({ command: "switch_to_work", task_id: queued })).error?.code).toBe("conflict");
});

it("scripts a chat's agent asking to build the Mac app, and a Plan task's asking to write the changelog", async () => {
  await send(startConversation(projectId(), "Build the Mac app and check it opens", agent, { access: { ...none, read: [projectId()] } }));
  await send({ ...startConversation(projectId(), "Plan the changelog for 2.0", agent), conversation: false, mode: "plan" } as Command);
  await vi.advanceTimersByTimeAsync(2000);
  const asks = host.approvals.filter((a) => !!a.access_request && a.state === "pending");
  expect(asks.map((a) => a.actions)).toEqual(expect.arrayContaining([["allow", "deny"], ["switch_to_work", "deny"]]));
  expect(asks.map((a) => a.title)).toContain(`Claude Code needs to change files in ${projectName()} and reach the internet and your own network to build the Mac app on the MacBook.`);
});
