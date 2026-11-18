// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it } from "vitest";
import type { Command, ConversationAccess } from "../../protocol/types";
import { SimHost } from "./host";
import { ids } from "./scenario";

const hosts: SimHost[] = [];
afterEach(() => { for (const host of hosts.splice(0)) host.dispose(); });
const fixture = (cards = false) => { const host = new SimHost({ cards, pace: 0 }); hosts.push(host); return host; };
let seq = 0;
const send = (host: SimHost, body: Command) => host.command({
  command_id: `cards-${++seq}`, issued_at: Date.now(), expires_at: null, expected_revision: null, body,
});
const access: ConversationAccess = { read: [], write: [], whole_read: false, whole_write: false, internet: true, local_network: false };
const start = (host: SimHost, index: number, chat = ids.tCards, run = ids.rCards): Extract<Command, { command: "create_task" }> => {
  const suggestion = host.runs.find((r) => r.run_id === run)!.outcome!.report!.new_work[index]!;
  return { command: "create_task", project_id: ids.pTide, origin: { chat, run, index },
    kind: "coding", mode: suggestion.mode, title: suggestion.title, prompt: suggestion.brief, bundle: null,
    harness: "claude_code", model: null, effort: null, target_id: null, mission: null, browser: false,
    placement: { mode: "queue", position: null, authorization: "ask_before_start" },
    ...(suggestion.mode === "ask" ? { access } : {}),
    ...(suggestion.issue ? { github: { type: "issue", number: suggestion.issue } } : {}),
  };
};

it("only loads chat suggestions and the long model in the opt-in fixture", () => {
  const normal = fixture();
  const cards = fixture(true);
  expect(normal.tasks.some((t) => t.task_id === ids.tCards)).toBe(false);
  expect(cards.tasks.filter((t) => t.conversation).map((t) => t.task_id)).toEqual([ids.tCards, ids.tOneCard, ids.tSet, ids.tRunAll]);
  expect(cards.runs.find((r) => r.run_id === ids.rCards)?.outcome?.report?.new_work).toHaveLength(3);
  expect(cards.runs.find((r) => r.run_id === ids.rOneCard)?.outcome?.report?.new_work).toHaveLength(1);
  expect(normal.harnesses.find((h) => h.kind === "codex")?.models).toHaveLength(2);
  expect(cards.harnesses.find((h) => h.kind === "codex")?.models).toHaveLength(3);
});

it("normalizes a project's chat default and rejects unknown projects", async () => {
  const host = fixture();
  expect((await send(host, { command: "set_chat_access_default", project_id: ids.pTide, access: { ...access, write: [ids.pNotes] } })).status).toBe("accepted");
  expect(host.projects.find((p) => p.project_id === ids.pTide)?.chat_access).toMatchObject({ read: [ids.pNotes], write: [ids.pNotes], internet: true });
  expect((await send(host, { command: "set_chat_access_default", project_id: ids.pTide, access: { ...access, read: ["missing"] } })).error?.code).toBe("invalid_request");
  expect((await send(host, { command: "set_chat_access_default", project_id: "missing", access: null })).error?.code).toBe("not_found");
  expect((await send(host, { command: "set_chat_access_default", project_id: ids.pTide, access: null })).status).toBe("accepted");
  expect(host.projects.find((p) => p.project_id === ids.pTide)?.chat_access).toBeUndefined();
});

it("binds cards to their chat and prevents duplicate or altered starts", async () => {
  const host = fixture(true);
  const ask = start(host, 0);
  expect((await send(host, { ...ask, access: undefined })).error?.message).toBe("What would start differs from the suggestion you saw.");
  expect((await send(host, { ...ask, prompt: "Different instructions" })).error?.code).toBe("invalid_request");
  expect((await send(host, { ...ask, origin: { ...ask.origin!, chat: ids.tHeaders } })).error?.code).toBe("invalid_request");
  expect((await send(host, { ...ask, origin: { ...ask.origin!, run: ids.rHeaders } })).error?.code).toBe("invalid_request");
  const first = await send(host, ask);
  expect(first.status).toBe("accepted");
  expect(host.tasks.find((t) => t.task_id === (first.result?.kind === "task_created" ? first.result.task_id : ""))?.origin).toEqual(ask.origin);
  expect((await send(host, ask)).error).toMatchObject({ code: "conflict", message: "This was already started.", detail: { task_id: (first.result as { task_id: string }).task_id } });
  const issue = start(host, 2);
  expect((await send(host, { ...issue, github: { type: "issue", number: 221 } })).error?.code).toBe("invalid_request");
  expect((await send(host, issue)).status).toBe("accepted");
  // As the computer's binding: a question never gets a pull request, an
  // issue only its own route, and a change may have its own pull request.
  const fresh = fixture(true);
  expect((await send(fresh, { ...start(fresh, 0), github: { type: "new_pr" } })).error?.message).toBe("What would start differs from the suggestion you saw.");
  expect((await send(fresh, { ...start(fresh, 2), github: { type: "new_pr" } })).error?.code).toBe("invalid_request");
  const one = fixture(true);
  const w = one.runs.find((r) => r.run_id === ids.rOneCard)!.outcome!.report!.new_work[0]!;
  expect(w.mode).toBe("work");
  expect((await send(one, { ...start(one, 0), origin: { chat: ids.tOneCard, run: ids.rOneCard, index: 0 }, mode: w.mode, title: w.title, prompt: w.brief, access: undefined, github: { type: "new_pr" } })).status).toBe("accepted");
});

const notes = (host: SimHost, taskId: string) => host.journal.flatMap(({ event }) =>
  event.type === "timeline_appended" && event.task_id === taskId && event.item.item.kind === "note" ? [event.item.item.text] : []);
const made = (r: Awaited<ReturnType<typeof send>>) => (r.result as { task_id: string }).task_id;

it("tells the chat in one plain line when work from its card ends, as the computer does", async () => {
  const host = fixture(true);
  const ask = start(host, 0);
  const id = made(await send(host, { ...ask, placement: { mode: "start_now" } }));
  const run = () => host.runs.find((r) => r.task_id === id)!;
  for (let i = 0; i < 200 && !["completed", "failed", "interrupted"].includes(run()?.state ?? ""); i++) await new Promise((r) => setTimeout(r, 5));
  expect(run().state).toBe("completed");
  expect(notes(host, ids.tCards)).toEqual([`“${ask.title}” finished.`]);
});

it("never lets a card's start resume a paused Next up", async () => {
  const host = fixture(true);
  const paused = await host.command({ command_id: `cards-${++seq}`, issued_at: Date.now(), expires_at: null,
    expected_revision: host.talk.queue(ids.pTide).revision, body: { command: "queue_pause", project_id: ids.pTide } });
  expect(paused.error ?? null).toBeNull();
  const ask = start(host, 0);
  const refused = await send(host, { ...ask, placement: { mode: "queue", position: null, authorization: "auto_start" } });
  expect(refused.error).toMatchObject({ code: "conflict", message: expect.stringMatching(/^Next up is paused/) });
  expect(host.talk.queue(ids.pTide).paused).toBe(true);
  expect((await send(host, ask)).status).toBe("accepted");
  expect(host.talk.queue(ids.pTide).paused).toBe(true);
});

it("keeps the chat a task is turned from, and refuses one that isn't a live chat in its project", async () => {
  const host = fixture(true);
  const body: Extract<Command, { command: "create_task" }> = { command: "create_task", project_id: ids.pTide, from_chat: ids.tCards,
    kind: "coding", mode: "work", title: "Carry it on", prompt: "Carry on from the chat.", bundle: null,
    harness: "claude_code", model: null, effort: null, target_id: null, mission: null, browser: false,
    placement: { mode: "queue", position: null, authorization: "ask_before_start" } };
  expect((await send(host, { ...body, from_chat: ids.tHeaders })).error?.message).toBe("This is not a conversation.");
  expect((await send(host, { ...body, project_id: ids.pNotes })).error?.code).toBe("invalid_request");
  expect((await send(host, { ...body, origin: start(host, 0).origin })).error?.code).toBe("invalid_request");
  const id = made(await send(host, body));
  expect(host.tasks.find((t) => t.task_id === id)?.from_chat).toBe(ids.tCards);
  expect(host.tasks.find((t) => t.task_id === ids.tCards)).toMatchObject({ conversation: true });
  expect(host.host.capabilities.features).toContain("chat_hears_back_v1");
});

it("keeps a card started from Review as that card's task, and refuses any card from an archived chat, as the computer does", async () => {
  const host = fixture(true);
  const ask = start(host, 0);
  const reviewed = { ...ask, origin: { ...ask.origin!, reviewed: true }, prompt: "Changed in Review", access: undefined };
  const id = made(await send(host, reviewed));
  expect(host.tasks.find((t) => t.task_id === id)?.origin).toEqual(reviewed.origin);
  expect((await send(host, ask)).error).toMatchObject({ code: "conflict", message: "This was already started." });
  // A fix never decides by itself to merge.
  const fix = start(host, 2);
  expect((await send(host, { ...fix, github: { type: "issue", number: 219, when_green: "merge" } })).error?.code).toBe("invalid_request");
  host.tasks.find((t) => t.task_id === ids.tCards)!.state = "archived";
  expect((await send(host, fix)).error?.message).toBe("This chat is archived. Bring it back first, or start the work from Tasks.");
  expect(host.host.capabilities.features).toContain("chat_work_reviewed_v1");
});

it("starts an ordered set in its order, each waiting in Next up for the one before", async () => {
  const host = fixture(true);
  const step = (i: number) => start(host, i, ids.tSet, ids.rSet);
  // Not before the one it follows.
  expect((await send(host, step(1))).error).toMatchObject({ code: "conflict", message: "This follows “Add the new settings store”. Start that first." });
  const made: string[] = [];
  for (const i of [0, 1, 2, 3]) {
    const r = await send(host, step(i));
    expect(r.status, `step ${i}`).toBe("accepted");
    made.push((r.result as { task_id: string }).task_id);
  }
  const items = host.talk.queue(ids.pTide).items;
  const item = (task: string) => items.find((i) => i.task_id === task)!;
  for (const i of [1, 2, 3]) expect(item(made[i]!).depends_on).toEqual([item(made[i - 1]!).item_id]);
  expect(item(made[0]!).depends_on).toEqual([]);
});

it("won't start a set's task now while the one before is still going", async () => {
  const host = fixture(true);
  const step = (i: number) => start(host, i, ids.tSet, ids.rSet);
  // Waiting in Next up for the person is still going.
  expect((await send(host, step(0))).status).toBe("accepted");
  expect((await send(host, { ...step(1), placement: { mode: "start_now" } })).error).toMatchObject({
    code: "conflict", message: "This follows “Add the new settings store”, which hasn't finished. Put it in Next up to start after it." });
});

it("starts no task from a card for fixing every issue, even from Review", async () => {
  const host = fixture(true);
  const every = start(host, 0, ids.tRunAll, ids.rRunAll);
  for (const reviewed of [false, true])
    expect((await send(host, { ...every, origin: { ...every.origin!, reviewed } })).error?.message).toBe("What would start differs from the suggestion you saw.");
});
