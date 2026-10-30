// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import type { Command, TimelineItem } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { portableBrief } from "./portable";
import Receipt from "./Receipt";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const send = (body: Command) => window.__sim!.host.command({ command_id: crypto.randomUUID(), issued_at: Date.now(), expires_at: null, expected_revision: null, body });

it("what Stop could not confirm stays under Still running / unknown, not just in the Stop receipt", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.state === "working" && t.harness === "claude_code")!;
  await send({ command: "stop_task", task_id: task.task_id });
  await tick(1500);
  const route = { screen: "receipt", taskId: task.task_id } as const;
  app.ui.push(route, "work");
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: app.ui.get().stacks.work.indexOf(route), active: true, route }}><Receipt route={route} /></RouteProvider></AppProvider>);
  await tick(900);
  const outcomes = within(screen.getByRole("list", { name: "Stop outcomes" })).getAllByRole("listitem").map(li => li.textContent);
  expect(outcomes.some(o => o?.startsWith("Shell commands started by the agent · Unknown"))).toBe(true);
  const remaining = screen.getByRole("heading", { name: "Still running / unknown" }).closest("section")!;
  expect(within(remaining).getByText("Shell commands started by the agent · Unknown")).toBeTruthy();
  expect(within(remaining).queryByText("No remaining work or unknowns reported in this result.")).toBeNull();
  // Confirmed facts stay in the Stop receipt only.
  expect(within(remaining).queryByText(/Desktop access withdrawn/)).toBeNull();
});

it("the portable brief carries Stop's unknowns and the agent's last words before Stop", () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.state === "working" && t.harness === "claude_code")!;
  const run = { ...host.runs.find(r => r.run_id === task.current_run)!, children: [] };
  let seq = 0;
  const item = (body: TimelineItem["item"]): TimelineItem => ({ seq: ++seq, at: 1_000 + seq, run_id: run.run_id, item: body });
  const timeline = [
    item({ kind: "user_message", text: "Read the retry code.", bundle: null, delivery: "native_delivered" }),
    item({ kind: "agent_message", text: "3 of 8 files read; checking src/retry.rs next.", partial: false }),
    item({ kind: "receipt", receipt: { kind: "stop_task", requested_at: 2_000, summary: "The agent stopped.", facts: [
      { label: "Agent confirmed Stop", status: "done", detail: null },
      { label: "Everything the agent started", status: "unknown", detail: "Work it moved into a session of its own may still be running. Check the computer." },
    ] } }),
  ];
  const brief = portableBrief({ task, runs: [run], timeline, artifacts: [], historyComplete: true, evidenceComplete: true });
  expect(brief).toContain("The agent’s last message before Stop: “3 of 8 files read; checking src/retry.rs next.”");
  const gaps = brief.slice(brief.indexOf("## Open gaps"), brief.indexOf("## Source record"));
  expect(gaps).toContain(`- After Stop, Everything the agent started: unknown. Work it moved into a session of its own may still be running. Check the computer. (run ${run.run_id})`);
  expect(gaps).not.toContain("Agent confirmed Stop");
  expect(gaps).not.toContain("No open gaps were reported");
});
