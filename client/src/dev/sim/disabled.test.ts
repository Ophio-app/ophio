// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Command, QueryData } from "../../protocol/types";
import { SimHost } from "./host";
import { createSimPlatform } from "./platform";
import { ids } from "./scenario";

let host: SimHost;
let n = 0;
beforeEach(() => { host = new SimHost(); });
afterEach(() => { host.dispose(); window.__sim?.host.dispose(); vi.useRealTimers(); });
const send = (body: Command) => host.command({ command_id: `disabled-${++n}`, issued_at: Date.now(), expires_at: null, expected_revision: null, body });
const skill = "/home/sam/.claude/skills/design-review/SKILL.md";
const agent = "/home/sam/.claude/agents/reviewer.md";
const file = "/home/sam/Projects/tidewater-api/CLAUDE.md";
const catalog = async () => (await host.query({ kind: "catalog", project_id: ids.pTide }) as Extract<QueryData, { kind: "catalog" }>).catalog.entries;
const files = async () => (await host.query({ kind: "agent_files" }) as Extract<QueryData, { kind: "agent_files" }>).files;
const listed = async () => (await host.query({ kind: "disabled_in_ophio" }) as Extract<QueryData, { kind: "disabled_in_ophio" }>).items;

it("classifies skills, agents, instructions, plugin skills, and memory topics", async () => {
  expect((await catalog()).find((e) => e.host_path === skill)?.disable).toEqual({ state: "can", kind: "skill", disabled: false, key: skill });
  expect((await catalog()).find((e) => e.host_path === agent)?.disable).toEqual({ state: "can", kind: "agent", disabled: false, key: agent });
  expect((await catalog()).find((e) => e.source === "ophio_setup")?.disable?.state).toBe("cannot");
  const items = await files();
  expect(items.find((f) => f.host_path === file)?.disable).toEqual({ state: "can", kind: "file", disabled: false, key: file });
  expect(items.find((f) => f.path.endsWith("/memory/payments.md"))?.disable?.state).toBe("cannot");
  expect(items.find((f) => f.path === "plugins/demo/skills/sample/SKILL.md")?.disable?.state).toBe("cannot");
});

it("disables, preserves the first timestamp, and enables again", async () => {
  expect((await send({ command: "set_disabled_in_ophio", kind: "skill", host_path: skill, disabled: true })).status).toBe("accepted");
  const first = (await listed())[0]!;
  expect(first).toMatchObject({ host_path: skill, kind: "skill", found: true });
  expect((await catalog()).find((e) => e.host_path === skill)?.disable).toEqual({ state: "can", kind: "skill", disabled: true, key: skill });
  await send({ command: "set_disabled_in_ophio", kind: "skill", host_path: skill, disabled: true });
  expect((await listed())[0]?.disabled_at).toBe(first.disabled_at);
  await send({ command: "set_disabled_in_ophio", kind: "skill", host_path: skill, disabled: false });
  expect(await listed()).toEqual([]);
});

it("refuses wrong kind, unlisted and built-in or plugin paths", async () => {
  const builtIn = (await catalog()).find((e) => e.source === "ophio_setup")!;
  for (const [kind, path, code] of [["agent", skill, "invalid_request"], ["file", "/not/listed", "not_found"], [builtIn.kind, builtIn.host_path, "invalid_request"], ["skill", "/home/sam/.claude/plugins/demo/skills/sample/SKILL.md", "invalid_request"]] as const) {
    expect((await send({ command: "set_disabled_in_ophio", kind, host_path: path, disabled: true })).error?.code).toBe(code);
  }
  expect((await send({ command: "set_disabled_in_ophio", kind: "file", host_path: "/not/listed", disabled: false })).status).toBe("accepted");
});

it("refuses a task started from a disabled skill", async () => {
  const entry = (await catalog()).find((e) => e.host_path === skill)!;
  await host.disabled.seed([skill, agent, file]);
  expect((await listed()).length).toBe(3);
  expect((await files()).find((f) => f.host_path === file)?.disable).toEqual({ state: "can", kind: "file", disabled: true, key: file });
  const receipt = await send({ command: "create_task", project_id: ids.pTide, kind: "coding", mode: "work", title: "Review", prompt: "Review this", bundle: null, harness: "claude_code", model: null, effort: null, mission: null, target_id: null, placement: { mode: "start_now" }, start: { type: "skill", source: entry.source, id: entry.id, digest: entry.digest } });
  expect(receipt.error).toMatchObject({ code: "conflict", message: `“${entry.name}” is disabled in Øphio. Enable it in Library to start from it.` });
});

it("refuses a same-as start after its original agent is disabled", async () => {
  const entry = (await catalog()).find((e) => e.host_path === agent)!;
  const body: Extract<Command, { command: "create_task" }> = { command: "create_task", project_id: ids.pTide, kind: "coding", mode: "ask", title: "First review", prompt: "Review this", bundle: null, harness: "claude_code", model: null, effort: null, mission: null, target_id: null, placement: { mode: "start_now" }, start: { type: "agent", source: entry.source, id: entry.id, digest: entry.digest } };
  const first = await send(body);
  expect(first.result?.kind).toBe("task_created");
  if (first.result?.kind !== "task_created") return;
  await host.disabled.seed([agent]);
  const again = await send({ ...body, start: { type: "same_as", task_id: first.result.task_id } });
  expect(again.error?.code).toBe("conflict");
});

it("marks disabled instructions not applied while leaving missing sources missing", async () => {
  const task = host.tasks.find((t) => t.project_id === ids.pTide)!;
  await host.disabled.seed([file]);
  const query = await host.query({ kind: "instructions", project_id: ids.pTide, task_id: task.task_id }) as Extract<QueryData, { kind: "instructions" }>;
  expect(query.sources.find((source) => source.host_path === file)).toMatchObject({ disabled: true, state: "not_applied" });
  expect(query.sources.find((source) => source.state === "missing")?.disabled).toBeUndefined();
});

it("enforces library read and write grants on the wire", async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const controls = window.__sim!;
  controls.permissions({ library_read: false, library_write: false });
  const profile = (await platform.profiles())[0]!;
  const control = platform.control(profile.profileId);
  const frames: unknown[] = [];
  control.listen((event) => { if (event.type === "frame" && ["query_result", "receipt"].includes(event.frame.type)) frames.push(event.frame); });
  control.connect();
  await vi.advanceTimersByTimeAsync(900);
  control.send({ type: "query", query_id: "disabled-read", query: { kind: "disabled_in_ophio" } });
  control.send({ type: "command", command_id: "disabled-write", issued_at: Date.now(), expires_at: null, expected_revision: null, body: { command: "set_disabled_in_ophio", kind: "skill", host_path: skill, disabled: true } });
  await vi.advanceTimersByTimeAsync(900);
  expect(frames).toEqual(expect.arrayContaining([
    expect.objectContaining({ type: "query_result", result: { Err: expect.objectContaining({ code: "permission_denied" }) } }),
    expect.objectContaining({ type: "receipt", status: "rejected", error: expect.objectContaining({ code: "permission_denied" }) }),
  ]));
  control.disconnect();
});

it("keeps agent-file group on commands classified as skills", async () => {
  const command = (await files()).find((f) => f.path === "commands/changelog.md")!;
  expect(command.disable).toEqual({ state: "can", kind: "skill", disabled: false, key: command.host_path });
  await send({ command: "set_disabled_in_ophio", kind: "skill", host_path: command.host_path!, disabled: true });
  expect((await listed()).find((i) => i.host_path === command.host_path)).toMatchObject({ kind: "skill", group: "commands" });
});
