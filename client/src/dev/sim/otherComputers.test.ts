// SPDX-License-Identifier: Apache-2.0
// The spare desktop and laptop in `?three-computers` have work of their own,
// held and answered by their own simulated computer, not studio's.
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { HostSession } from "../../host/session";
import type { Platform } from "../../host/link";
import type { Command } from "../../protocol/types";
import { otherIds } from "./computers";
import { ids } from "./scenario";
import { createSimPlatform } from "./platform";

let platform: Platform;
const sessions: HostSession[] = [];

async function open(profileId: string): Promise<HostSession> {
  const session = new HostSession(profileId, platform.control(profileId));
  sessions.push(session);
  session.start();
  await vi.advanceTimersByTimeAsync(900);
  return session;
}

async function send(session: HostSession, body: Command, expectedRevision: number | null = null) {
  const receipt = session.command(body, { label: "test", expectedRevision });
  await vi.advanceTimersByTimeAsync(900);
  return receipt;
}

async function query<T>(promise: Promise<T>): Promise<T> {
  await vi.advanceTimersByTimeAsync(300);
  return promise;
}

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  window.history.replaceState(null, "", "/?three-computers");
  platform = createSimPlatform();
});
afterEach(() => {
  for (const s of sessions.splice(0)) s.stop();
  window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.clearAllTimers();
  vi.useRealTimers();
});

const s = otherIds.spare, l = otherIds.laptop;

it("gives spare a running task, a command to allow and a question to answer", async () => {
  const spare = await open("sim-spare");
  const state = spare.state.get();
  expect(state.host?.name).toBe("spare");
  expect(state.tasks[s.tRender]).toMatchObject({ title: "Render the winter tiles at 4K", state: "working", current_run: s.rRender });
  expect(state.runs[s.rRender]?.state).toBe("working");
  expect(state.approvals[s.aArchive]).toMatchObject({ kind: "command", state: "pending", actions: ["allow", "deny"] });
  expect(state.attention[s.atArchive]).toMatchObject({ kind: "approval", approval_id: s.aArchive, resolved_at: null });
  expect(state.approvals[s.aChapters]).toMatchObject({ kind: "question", state: "pending", actions: ["answer", "deny"] });
  expect(state.attention[s.atChapters]).toMatchObject({ kind: "question", approval_id: s.aChapters, resolved_at: null });
  expect(state.tasks[s.tArchive]?.attention_count).toBe(1);
  // Studio's work stays on studio.
  expect(state.tasks[ids.tFlaky]).toBeUndefined();
  expect(state.approvals[ids.aTest]).toBeUndefined();

  const timeline = await query(spare.query({ kind: "task_timeline", task_id: s.tArchive, before_seq: null, limit: 50 }));
  expect(timeline.items.map((i) => i.item.kind)).toEqual(["user_message", "run_started", "approval"]);
});

it("allows spare's command on spare, not on studio", async () => {
  const spare = await open("sim-spare");
  const studio = await open((await platform.profiles())[0]!.profileId);
  const approval = spare.state.get().approvals[s.aArchive]!;
  await send(spare, { command: "resolve_approval", approval_id: s.aArchive, decision: { decision: "allow" } }, approval.revision);
  expect(spare.state.get().approvals[s.aArchive]?.state).toBe("allowed");
  expect(spare.state.get().attention[s.atArchive]?.resolved_at).toEqual(expect.any(Number));
  expect(spare.state.get().tasks[s.tArchive]?.state).toBe("working");
  expect(window.__sim!.host.approvals.some((a) => a.approval_id === s.aArchive)).toBe(false);
  expect(studio.state.get().approvals[ids.aTest]?.state).toBe("pending");
});

it("takes an answer to spare's question", async () => {
  const spare = await open("sim-spare");
  const approval = spare.state.get().approvals[s.aChapters]!;
  await send(spare, { command: "resolve_approval", approval_id: s.aChapters,
    decision: { decision: "answer", text: "By date", answers: [{ question: "How should the chapters be ordered?", choices: ["By date"] }] } }, approval.revision);
  expect(spare.state.get().approvals[s.aChapters]?.state).toBe("allowed");
  expect(spare.state.get().tasks[s.tChapters]?.attention_count).toBe(0);
});

it("gives laptop a finished task to look at", async () => {
  const laptop = await open("sim-laptop");
  const state = laptop.state.get();
  expect(state.host?.name).toBe("laptop");
  expect(state.tasks[l.tLisbon]).toMatchObject({ title: "Write up the Lisbon trip", state: "completed", current_run: null });
  expect(state.runs[l.rLisbon]).toMatchObject({ state: "completed", ended_at: expect.any(Number) });
  expect(state.attention[l.atLisbon]).toMatchObject({ kind: "completed", task_id: l.tLisbon, resolved_at: null });
  expect(Object.keys(state.approvals)).toEqual([]);
});

it("lists spare's own checklist and ticks its item there", async () => {
  const spare = await open("sim-spare");
  const farm = Object.values(spare.state.get().projects).find((p) => p.name === "render-farm")!;
  const before = await query(spare.query({ kind: "checklists" }));
  expect(before.checklists).toEqual([expect.objectContaining({ project_id: farm.project_id, open: 1, done: 0, next: "Sign in to the render service" })]);
  await send(spare, { command: "checklist_tick", project_id: farm.project_id, item_id: "render-sign-in", done: true });
  const after = await query(spare.query({ kind: "checklist", project_id: farm.project_id }));
  expect(after.checklist?.sections[0]?.items[0]?.done_at).toEqual(expect.any(Number));

  const laptop = await open("sim-laptop");
  const lists = await query(laptop.query({ kind: "checklists" }));
  expect(lists.checklists).toEqual([expect.objectContaining({ title: "Publish the Lisbon post", open: 1 })]);
});

it("leaves the reinstalled computer of `same-name` without work", async () => {
  window.history.replaceState(null, "", "/?same-name");
  platform = createSimPlatform();
  const spare = await open("sim-spare");
  expect(Object.keys(spare.state.get().tasks)).toEqual([]);
  expect(Object.keys(spare.state.get().attention)).toEqual([]);
  expect((await query(spare.query({ kind: "checklists" }))).checklists).toEqual([]);
});
