// SPDX-License-Identifier: Apache-2.0
import { expect, it, vi } from "vitest";
import cases from "../../../crates/protocol/tests/fixtures/report-scrub.json";
import * as limits from "./constants";
import { scrub } from "./scrub";
import { pseudonym, reportSalt } from "./pseudonym";
import { reportReference } from "./reference";
import { stateShape } from "./stateShape";
import { routePattern } from "./routes";
import { emptyHostState } from "../state/projection";

it.each(cases)("scrubs the shared Rust case %s", (input, want) => expect(scrub(input!)).toBe(want ?? undefined));
it("cuts scrubbed messages at the Rust character boundary", () => {
  expect(scrub("🙂".repeat(161))).toBe("🙂".repeat(160) + "…");
});
it("matches the Rust limits", async () => {
  const fs = await vi.importActual<{ readFileSync(path: string, encoding: string): string }>("node:fs");
  const contract = fs.readFileSync("../crates/protocol/src/reports.rs", "utf8");
  for (const [key, value] of Object.entries(limits)) {
    if (typeof value !== "number") continue;
    const expression = contract.match(new RegExp(`pub const ${key}: [^=]+ = ([^;]+);`))?.[1];
    expect(expression, key).toBeDefined();
    expect(expression!.split("*").map(s => Number(s.trim().replace(/_/g, ""))).reduce((a, b) => a * b, 1), key).toBe(value);
  }
});
it("matches the pseudonym vector, ignores ASCII case, and scopes ids to a report", async () => {
  const id = "0199F1C2-3B4D-7E5F-8A6B-1C2D3E4F5A6B";
  expect(await pseudonym("00112233445566778899aabbccddeeff", "task", id)).toBe("t:bc25fcec");
  expect(await pseudonym("00112233445566778899aabbccddeeff", "task", id.toLowerCase())).toBe("t:bc25fcec");
  expect(await pseudonym("different", "task", id)).not.toBe("t:bc25fcec");
  const salts = new Set(Array.from({ length: 100 }, reportSalt));
  expect(salts.size).toBe(100);
  for (const salt of salts) expect(salt).toMatch(/^[0-9a-f]{32}$/);
});
it("matches the short reference vectors", () => {
  expect(reportReference("0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b")).toBe("Z4YPKB");
  expect(reportReference("00000000-0000-0000-0000-000000000000")).toBe("000000");
  expect(reportReference("00000000-0000-7000-8000-000000000000")).toBe("000000");
});
it("redacts names, content, credentials, paths, URLs, and dynamic keys while retaining shape", async () => {
  const id = "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b";
  const source = { host: { ...emptyHostState, tasks: { [id]: { task_id: id, title: "Secret task", state: "running", model: "claude-opus-5-5", path: "/home/sam/private", url: "https://private.example", token: "fake-private-token", count: 42, enabled: true, nested: ["private words"] } },
    libraryRevisions: { "/home/sam/private": "secret-revision" } }, ui: { taskId: id, screen: "receipt", tab: "work", draft: { text: "running" } },
    "Private project": { "/private/path": "private-text" } };
  const shape = await stateShape(source, "00112233445566778899aabbccddeeff");
  const encoded = JSON.stringify(shape);
  for (const secret of [id, "Secret task", "/home/sam/private", "private.example", "fake-private-token", "private words", "Private project", "/private/path", "secret-revision", "private-text"]) expect(encoded).not.toContain(secret);
  expect(encoded).toContain('"task_id":"t:bc25fcec"');
  expect(encoded).toContain('"taskId":"t:bc25fcec"');
  expect(encoded).toContain('"state":"running"');
  expect(encoded).toContain('"text":"~text:7"');
  expect(encoded).toContain('"model":"claude-opus-5-5"');
  expect(encoded).toContain('"count":42');
  expect(encoded).toContain('"enabled":true');
  expect(encoded).toContain('"~path:17"');
  expect(source.ui.taskId).toBe(id);
});
it("uses logical routes and refuses filled paths", () => {
  expect(routePattern({ screen: "receipt", taskId: "Private task" })).toBe("/receipt");
  expect(routePattern("/work/:task")).toBe("/work/:task");
  expect(routePattern("/work/Private task")).toBe("/unknown");
  expect(routePattern("/work/secret")).toBe("/unknown");
});
it("keeps id kinds consistent across dictionary keys, references and composite draft keys", async () => {
  const run = "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b";
  const salt = "00112233445566778899aabbccddeeff";
  const source = { host: { projects: { "fake-project": { project_id: "fake-project" } }, tasks: { "fake-task": { task_id: "fake-task", current_run: run } }, runs: { [run]: { run_id: run } }, queues: { "fake-project": { writer: run } } },
    ui: { taskByProject: { "fake-project": "fake-task" }, drafts: { "fake-project/fake-task": { text: "Secret" } } } };
  const shape = await stateShape(source, salt);
  const r = await pseudonym(salt, "run", run), p = await pseudonym(salt, "project", "fake-project"), t = await pseudonym(salt, "task", "fake-task");
  expect(shape.store).toMatchObject({ host: { tasks: { [t]: { current_run: r } }, runs: { [r]: { run_id: r } }, queues: { [p]: { writer: r } } }, ui: { taskByProject: { [p]: t }, drafts: { [`${p}/${t}`]: { text: "~text:6" } } } });
});
it("redacts prototype-named properties as data without evaluating them", async () => {
  const shape = await stateShape(JSON.parse('{"toString":"Secret value","__proto__":{"secret":"Private"}}'), "salt");
  expect(JSON.stringify(shape)).not.toMatch(/Secret|Private|__proto__/);
});
