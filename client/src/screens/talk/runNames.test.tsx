// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { approvalCases as cases, ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { Command, Run } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { buildBrief, digestSentences } from "../work/brief";
import { receiptHeadline } from "../work/logic";
import { RunPicker } from "../work/RunPicker";
import { AttentionList } from "../work/shared";
import { ApprovalQuickActions } from "./ApprovalQuickActions";
import { taskBadge } from "./TaskBar";
import { Transcript } from "./Transcript";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const send = (body: Command) => window.__sim!.host.command({ command_id: crypto.randomUUID(), issued_at: Date.now(), expires_at: null, expected_revision: null, body });

it("says Stop withdrew a held message and puts it back in the composer without sending", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.state === "working" && t.harness === "claude_code")!;
  await send({ command: "send_message", task_id: task.task_id, text: "Also check the p99 line.", bundle: null, delivery: "next_turn" });
  await send({ command: "stop_task", task_id: task.task_id });
  await tick(1500);
  const stopped = app.session.state.get().tasks[task.task_id]!;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}>
    <Transcript task={stopped} agent="Claude Code" />
  </RouteProvider></AppProvider>);
  await tick();
  expect(screen.getByText("Withdrawn by Stop · not sent")).toBeTruthy();
  expect(screen.getByText(/Stopped · .* · 1 held message withdrawn/)).toBeTruthy();
  const commands = Object.keys(app.session.commands.get()).length;
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Put back in composer" })));
  expect(app.ui.draft(task.project_id, task.task_id)).toMatchObject({ text: "Also check the p99 line.", annotationId: null });
  expect(Object.keys(app.session.commands.get())).toHaveLength(commands);
});

function finishPlan() {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === ids.tHeaders)!;
  const run = host.runs.find(r => r.run_id === ids.rHeaders)!;
  Object.assign(task, { mode: "plan", state: "completed", current_run: run.run_id });
  run.mode = "plan";
  host.emit({ type: "task_upserted", task: { ...task } });
  host.emit({ type: "run_upserted", run: { ...run } });
  return { task, run };
}
const inRoute = (child: React.ReactNode) => render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}>{child}</RouteProvider></AppProvider>);

it("says a finished Plan task proposed a plan in its badge, divider, review row, receipt and brief", async () => {
  const { task, run } = finishPlan();
  await tick();
  const state = app.session.state.get();
  const planned = state.tasks[task.task_id]!;
  expect(taskBadge(planned)).toEqual({ label: "Plan proposed", tone: "ok" });
  expect(receiptHeadline(planned, state.runs[run.run_id])).toMatch(/^Plan proposed/);
  const talk = inRoute(<Transcript task={planned} agent="Codex" />);
  await tick();
  expect(within(talk.container).getByText(/^Plan proposed · /)).toBeTruthy();
  expect(within(talk.container).queryByText(/^Finished · /)).toBeNull();
  talk.unmount();
  const review = inRoute(<AttentionList projectId={ids.pTide} review />);
  const row = within(review.container).getByText(task.title).closest("button")!;
  expect(row.textContent).toContain("Plan proposed ·");
  expect(row.textContent).not.toContain("Finished");
  const brief = buildBrief(ids.pTide, 0, Object.values(state.tasks), Object.values(state.runs), []);
  expect(brief.proposed).toEqual([task.task_id]);
  expect(brief.finished).not.toContain(task.task_id);
  expect(digestSentences(brief, 0).details).toContain("1 plan was proposed.");
});

it("keeps an earlier Plan run named Plan proposed after the plan is carried out", async () => {
  const { task, run } = finishPlan();
  const host = window.__sim!.host;
  const work: Run = { ...run, run_id: "carried-out", mode: "work", admitted_at: run.ended_at! + 1000, ended_at: run.ended_at! + 60_000 };
  host.runs.push(work);
  host.emit({ type: "run_upserted", run: work });
  host.emit({ type: "task_upserted", task: { ...task, mode: "work", current_run: work.run_id } });
  await tick();
  const state = app.session.state.get();
  const carried = state.tasks[task.task_id]!;
  expect(taskBadge(carried).label).toBe("Finished");
  expect(taskBadge(carried, state.runs[run.run_id]).label).toBe("Plan proposed");
  expect(receiptHeadline(carried, state.runs[run.run_id])).toMatch(/^Plan proposed/);
  expect(receiptHeadline(carried, state.runs[work.run_id])).toMatch(/^Finished/);
});

it("names each run by number, mode and ending with its start, and says the same to a screen reader", async () => {
  const { task, run } = finishPlan();
  const plan: Run = { ...run, admitted_at: Date.UTC(2026, 8, 28, 9, 5) };
  const work: Run = { ...run, run_id: "work", mode: "work", admitted_at: plan.admitted_at + 3_600_000 };
  const stopped: Run = { ...run, run_id: "stopped", mode: "work", state: "interrupted", admitted_at: work.admitted_at + 3_600_000 };
  const runs = [stopped, work, plan];
  inRoute(<RunPicker task={{ ...task, mode: "work", current_run: stopped.run_id }} runs={runs} latest={stopped} selectedId={null} onChange={() => {}} />);
  const at = (r: Run) => new Date(r.admitted_at).toLocaleString([], { month: "short", day: "numeric", year: "numeric", hour: "numeric", minute: "2-digit" });
  const collapsed = screen.getByRole("button", { name: `Choose a run: showing Run 3, Work, Stopped, started ${at(stopped)}, latest` });
  expect(collapsed.textContent).toContain("Run 3 · Work · Stopped");
  fireEvent.click(collapsed);
  const lines = [["Run 3 · Work · Stopped", stopped, ", latest"], ["Run 2 · Work · Finished", work, ""], ["Run 1 · Plan · Plan proposed", plan, ""]] as const;
  for (const [title, r, latest] of lines) {
    const row = screen.getByRole("button", { name: `${title.replaceAll(" · ", ", ")}, started ${at(r)}${latest}` });
    expect(row.querySelector(".row__title")?.textContent).toBe(title);
    expect(row.querySelector(".row__subtitle")?.textContent).toBe(`Started ${at(r)}${latest ? " · Latest" : ""}`);
    expect(row.getAttribute("aria-current")).toBe(r === stopped ? "true" : null);
  }
});

it("says a shortened request was cut in the Needs you row, before and after the decision, and keeps the whole input one tap away", async () => {
  const host = window.__sim!.host;
  const approval = host.approvals.find(a => a.approval_id === ids.aTest)!;
  const { approval: cut, input } = cases.find(c => c.approval.shortened)!;
  const shortened = { ...approval, title: cut.title, detail: cut.detail, detail_lines: cut.detail_lines, shortened: true, raw_request: JSON.stringify(input) };
  act(() => host.emit({ type: "approval_upserted", approval: shortened }));
  await tick();
  render(<AppProvider app={app}><ApprovalQuickActions approvalId={ids.aTest} /></AppProvider>);
  expect([...document.querySelectorAll(".talk-quick-approval__detail .approval__line")].map(line => line.textContent).join("\n")).toBe(cut.detail);
  expect(screen.getByText("Shortened · the full request is under Exact request")).toBeTruthy();
  const details = screen.getByText("Exact request").closest("details")!;
  fireEvent.click(screen.getByText("Exact request"));
  expect(details.open).toBe(true);
  expect(details.querySelector("pre")?.textContent).toBe(JSON.stringify(input));
  act(() => host.emit({ type: "approval_upserted", approval: { ...shortened, state: "allowed", resolved_at: Date.now() } }));
  await tick();
  expect(document.querySelector(".approval--settled .approval__shortened")?.textContent).toBe("Shortened · the full request is under Exact request");
  expect(screen.getByText("Exact request")).toBeTruthy();
  act(() => host.emit({ type: "approval_upserted", approval }));
  await tick();
  expect(screen.queryByText("Shortened · the full request is under Exact request")).toBeNull();
  expect(screen.queryByText("Exact request")).toBeNull();
});
