// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { toolLabel } from "./toolInput";
import { OPHIO_TOOLS, plainToolNames } from "../../design/toolNames";
import type { TimelineItem } from "../../protocol/types";
import { rows, stepSummary } from "./timeline";
import { Steps } from "./Transcript";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); });

it("names Codex's kinds of step in plain words, and an agent's built-in tool as the agent does", () => {
  expect(toolLabel("commandExecution")).toBe("Command");
  expect(toolLabel("fileChange")).toBe("File change");
  expect(toolLabel("webSearch")).toBe("Web search");
  expect(toolLabel("ExitPlanMode")).toBe("Plan");
  expect(toolLabel("Bash")).toBe("Bash");
  expect(toolLabel("functions.exec_command")).toBe("functions.exec_command");
});

it("gives every one of Ophio's own tools a short label, however the agent names it", async () => {
  const fs = await vi.importActual<{ readFileSync(path: string, encoding: string): string }>("node:fs");
  const bridge = fs.readFileSync("../crates/ports/src/bridge.rs", "utf8");
  const server = fs.readFileSync("../crates/computer-mcp/src/lib.rs", "utf8");
  // Every tool list and single tool the bridge declares, and the permission prompt.
  const lists = [...bridge.matchAll(/pub const [A-Z_]+_TOOLS: [^=]+= &?\[([^\]]*)\]/g)].map(m => m[1]!);
  const single = [...bridge.matchAll(/pub const [A-Z_]+_TOOL: &str = "([a-z_]+)"/g), ...server.matchAll(/pub const PERMISSION_TOOL: &str = "([a-z_]+)"/g)].map(m => m[1]!);
  const names = new Set([...lists.flatMap(l => [...l.matchAll(/"([a-z_]+)"/g)].map(m => m[1]!)), ...single]);
  expect(names.size).toBeGreaterThan(40);
  expect(new Set(Object.keys(OPHIO_TOOLS))).toEqual(names);
  for (const name of ["request_access", "show_person", "browser_open", "github_pr", "wake_me", "permission_prompt"]) expect(names.has(name)).toBe(true);
  for (const name of names) {
    for (const said of [toolLabel(name), toolLabel(`mcp__computer__${name}`)]) {
      expect(said, name).not.toMatch(/_|mcp/);
      expect(said.length, name).toBeLessThanOrEqual(26);
    }
    expect(toolLabel(`mcp__computer__${name}`)).toBe(toolLabel(name));
  }
  expect(toolLabel("mcp__computer__request_access")).toBe("Ask for more access");
});

it("rewrites old computer titles but leaves newer plain titles alone", () => {
  expect(plainToolNames("Use mcp__computer__request_access")).toBe("Ask for more access");
  expect(plainToolNames("Use Grep")).toBe("Search files");
  expect(plainToolNames("Use Glob")).toBe("Find files");
  expect(plainToolNames("Use WebFetch")).toBe("Open a web page");
  expect(plainToolNames("Ask for more access")).toBe("Ask for more access");
  expect(plainToolNames("Search files")).toBe("Search files");
});

it("reads another server's tool as its name in words, with the server after it", () => {
  expect(toolLabel("mcp__github__create_issue")).toBe("Create issue (github)");
  expect(toolLabel("mcp__linear_app__list-issues")).toBe("List issues (linear app)");
  expect(toolLabel("mcp__computer__something_new")).toBe("Something new");
  expect(toolLabel("create_issue")).toBe("Create issue");
});

it("says a step by its tool's label, its state left to its mark", () => {
  const step = (tool: string, summary: string, status: "running" | "succeeded" | "failed") =>
    ({ seq: 1, at: 0, run_id: "run", item: { kind: "activity" as const, tool, summary, status, detail: null, key: "k" } });
  expect(stepSummary(step("mcp__computer__request_access", "mcp__computer__request_access", "running"), false)).toBe("Ask for more access");
  expect(stepSummary(step("mcp__computer__request_access", "mcp__computer__request_access finished", "succeeded"), false)).toBe("Ask for more access");
  expect(stepSummary(step("request_access", "request_access finished", "failed"), false)).toBe("Ask for more access");
  expect(stepSummary(step("mcp__github__create_issue", "mcp__github__create_issue finished", "succeeded"), true)).toBe("Create issue (github)");
  expect(stepSummary(step("Bash", "Bash finished", "succeeded"), false)).toBe("Bash");
  expect(stepSummary(step("Read", "Read crates/payments/src/retry.rs", "succeeded"), false)).toBe("Read crates/payments/src/retry.rs");
});

it("an opened Codex step reads as a command, not Codex's name for it", async () => {
  const step = (seq: number, key: string, tool: string, summary: string, detail: string, status: "succeeded" | "failed") => ({
    seq, at: Date.UTC(2026, 10, 7, 13, 12) + seq, run_id: "run",
    item: { kind: "activity" as const, tool, summary, status, detail, key },
  });
  render(<AppProvider app={app}><Steps ended steps={[
    step(1, "a", "commandExecution", "Command finished", "ls", "succeeded"),
    step(2, "b", "commandExecution", "Command finished", "ls /Users/sam/Documents", "failed"),
  ]} /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  fireEvent.click(screen.getByRole("button", { expanded: false }));
  // Its line says Command; the line under it is only the time, not Command again.
  expect([...document.querySelectorAll(".talk-steps__summary")].map(p => p.textContent)).toEqual(["Command", "Command"]);
  expect([...document.querySelectorAll(".talk-steps__tool")].map(p => p.textContent).join()).not.toContain("·");
  expect(document.body.textContent).not.toContain("commandExecution");
});

it("marks a row of steps running, done or stopped, with the spinner only while one runs", async () => {
  const step = (status: "running" | "succeeded") =>
    ({ seq: 1, at: 0, run_id: "run", item: { kind: "activity" as const, tool: "Bash", summary: "Bash", status, detail: null, key: "k" } });
  const head = () => document.querySelector<HTMLElement>(".talk-steps__head")!;
  const { rerender } = render(<AppProvider app={app}><Steps ended={false} steps={[step("running")]} /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  expect(within(head()).getByRole("img", { name: "Running" }).classList.contains("stateline__spinner")).toBe(true);
  rerender(<AppProvider app={app}><Steps ended={false} steps={[step("succeeded")]} /></AppProvider>);
  expect(within(head()).getByRole("img", { name: "Done" })).toBeTruthy();
  expect(head().querySelector(".stateline__spinner")).toBeNull();
  // A step still marked running when its run was stopped is not running.
  rerender(<AppProvider app={app}><Steps ended stopped steps={[step("running")]} /></AppProvider>);
  expect(within(head()).getByRole("img", { name: "Stopped" })).toBeTruthy();
  expect(head().querySelector(".stateline__spinner")).toBeNull();
});

it("knows a row of steps was stopped from its run's end, whether recorded or only in the run", () => {
  const at = (seq: number, item: TimelineItem["item"]): TimelineItem => ({ seq, at: seq, run_id: "r1", item });
  const items = [at(1, { kind: "activity", key: "k", tool: "Bash", summary: "Bash", status: "running", detail: null })];
  const stepsRow = (r: ReturnType<typeof rows>) => r.find(row => row.kind === "steps") as Extract<ReturnType<typeof rows>[number], { kind: "steps" }>;
  expect(stepsRow(rows([...items, at(2, { kind: "run_ended", run_id: "r1", state: "interrupted" })]))).toMatchObject({ ended: true, stopped: true });
  expect(stepsRow(rows([...items, at(2, { kind: "run_ended", run_id: "r1", state: "completed" })])).stopped).toBeUndefined();
  expect(stepsRow(rows(items)).ended).toBe(false);
});

it("names a step's tool under it only where the step's own line doesn't already", async () => {
  const step = (seq: number, tool: string, summary: string) =>
    ({ seq, at: Date.UTC(2026, 10, 7, 13, 12) + seq, run_id: "run", item: { kind: "activity" as const, tool, summary, status: "succeeded" as const, detail: null, key: `k${seq}` } });
  render(<AppProvider app={app}><Steps ended steps={[
    step(1, "mcp__computer__show_person", "mcp__computer__show_person finished"),
    step(2, "Bash", "Ran the parser tests"),
  ]} /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  fireEvent.click(screen.getByRole("button", { expanded: false }));
  const tools = [...document.querySelectorAll(".talk-steps__tool")].map(p => p.textContent);
  expect(tools[0]).not.toContain("·");
  expect(tools[1]).toMatch(/^Bash · /);
  expect(screen.getAllByText("Show you something")).toHaveLength(1);
});
