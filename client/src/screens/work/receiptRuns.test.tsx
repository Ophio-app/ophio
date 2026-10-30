// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { workIds } from "../../dev/sim/work";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { unresolvedAttention as openAttention } from "../../state/projection";
import Receipt from "./Receipt";
import { taskMatches } from "./logic";
import type { AnnotationBundle, TimelineItem } from "../../protocol/types";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
function view(child: React.ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{child}</RouteProvider></AppProvider>);
}
it("keeps earlier receipts reachable and settles every completion for the task on open", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.receipt)!;
  const first = host.runs.find(r => r.task_id === task.task_id)!;
  const second = { ...first, run_id: "second-run", admitted_at: first.admitted_at + 60_000, state: "failed" as const,
    outcome: { ...first.outcome!, summary: "Second run result" } };
  host.runs.push(second);
  host.emit({ type: "run_upserted", run: second });
  host.emit({ type: "task_upserted", task: { ...task, current_run: second.run_id, state: "failed" } });
  for (let i = 0; i < 12; i++) {
    const item = { attention_id: `finish-${i}`, task_id: task.task_id, run_id: first.run_id, kind: "completed" as const, title: "Task finished", body: null, priority: 1, created_at: Date.now() + i, resolved_at: null, approval_id: null };
    host.attention.push(item); host.emit({ type: "attention_upserted", item });
  }
  expect(openAttention(app.session.state.get()).filter(a => a.task_id === task.task_id && a.kind === "completed")).toHaveLength(1);
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  expect(screen.getByText("Second run result")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Choose a run: showing Run 2,/ }));
  expect(screen.getAllByRole("button", { name: /^Run 2, .*, latest$/ })).toHaveLength(1);
  expect(screen.queryByRole("button", { name: /^Latest run ·/ })).toBeNull();
  // Run 1 was recorded without its mode and is no longer current, so no mode is claimed for it.
  fireEvent.click(screen.getByRole("button", { name: /^Run 1, Finished, started / }));
  expect(screen.queryByText("Second run result")).toBeNull();
  expect(screen.getByText(first.outcome!.summary!)).toBeTruthy();
  expect(document.querySelector(".work-receipt-state")?.textContent).toContain("Finished");
  await tick(900);
  expect(openAttention(app.session.state.get()).filter(a => a.task_id === task.task_id && a.kind === "completed")).toHaveLength(0);
});

it("finds removed queue work in Archived with its original request and attachment", async () => {
  await tick(900);
  const host = window.__sim!.host;
  const bundle: AnnotationBundle = { bundle_id: "queued-attachment", revision: 1, marks: [], comments: [], steps: [], general_note: null, speech: null,
    sources: [{ key: "S1", artifact_id: "original-picture", sha256: "0".repeat(64), media_type: "image/png", width: 390, height: 200,
      origin: { kind: "phone_import", original_name: "empty-states.png" }, captured_at: Date.now(), derived_from: null, derivation: null }] };
  const original = { taskId: workIds.waiting, bundle };
  host.emit({ type: "timeline_appended", task_id: original.taskId, item: { seq: host.seq + 1, at: Date.now(), run_id: null,
    item: { kind: "user_message", text: "Polish the empty states shown in this screenshot.", bundle, delivery: "host_accepted" } } });
  const removing = app.session.command({ command: "queue_remove", item_id: workIds.qWaiting }, { label: "Remove", expectedRevision: host.queue.revision });
  await tick(); await removing;
  const removed = app.session.state.get().tasks[original.taskId]!;
  expect(removed.state).toBe("archived");
  expect(taskMatches(removed, "Polish empty states")).toBe(true);
  const route = { screen: "receipt", taskId: original.taskId } as const;
  view(<Receipt route={route} />, route);
  await tick(900);
  expect(screen.getByText("Polish the empty states shown in this screenshot.")).toBeTruthy();
  const request = app.session.state.get().timelines[original.taskId]?.items.find(i => i.item.kind === "user_message" && i.item.bundle?.bundle_id === bundle.bundle_id);
  expect(request?.item.kind === "user_message" && request.item.bundle).toEqual(original.bundle);
  expect(screen.getByRole("button", { name: "Open conversation and attachments" })).toBeTruthy();
});

it("shows the computer's Stop receipt on the run it interrupted, not on earlier runs", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.receipt)!;
  const first = host.runs.find(r => r.task_id === task.task_id)!;
  const second = { ...first, run_id: "stopped-run", admitted_at: first.admitted_at + 60_000, state: "interrupted" as const,
    outcome: { ...first.outcome!, summary: "Stopped run result" } };
  host.runs.push(second);
  host.emit({ type: "run_upserted", run: second });
  host.emit({ type: "task_upserted", task: { ...task, current_run: second.run_id, state: "interrupted" } });
  // The companion records task Stop in the run timeline.
  host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: second.admitted_at + 5_000, run_id: second.run_id, item: { kind: "receipt", receipt: { kind: "stop_task", requested_at: second.admitted_at + 5_000,
    summary: "The agent confirmed Stop.", facts: [{ label: "Helpers", status: "unknown", detail: null }] } } } });
  await tick();
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  expect(screen.getByText("Stop receipt")).toBeTruthy();
  expect(screen.getByText("The agent confirmed Stop.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Choose a run: showing Run 2,/ }));
  fireEvent.click(screen.getByRole("button", { name: /^Run 1, / }));
  expect(screen.queryByText("Stop receipt")).toBeNull();
});

it("the Stop receipt says how far the run had got before Stop", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.receipt)!;
  const first = host.runs.find(r => r.task_id === task.task_id)!;
  const second = { ...first, run_id: "stopped-run", admitted_at: first.admitted_at + 60_000, state: "interrupted" as const,
    outcome: { ...first.outcome!, summary: "Stopped run result" } };
  host.runs.push(second);
  host.emit({ type: "run_upserted", run: second });
  host.emit({ type: "task_upserted", task: { ...task, current_run: second.run_id, state: "interrupted" } });
  const append = (offset: number, item: TimelineItem["item"]) =>
    host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: ++host.seq, at: second.admitted_at + offset, run_id: second.run_id, item } });
  append(1_000, { kind: "activity", key: "read-1", tool: "Read", summary: "Read", status: "running", detail: JSON.stringify({ file_path: "src/clock.rs" }) });
  append(1_500, { kind: "activity", key: "read-1", tool: "Read", summary: "Read finished", status: "succeeded", detail: "…" });
  append(2_000, { kind: "agent_message", text: "3 of 7 files read; checking src/retry.rs next.", partial: false });
  append(3_000, { kind: "activity", key: "read-2", tool: "Read", summary: "Read", status: "running", detail: JSON.stringify({ file_path: "src/retry.rs" }) });
  append(5_000, { kind: "receipt", receipt: { kind: "stop_task", requested_at: second.admitted_at + 5_000,
    summary: "The agent confirmed Stop.", facts: [{ label: "Helpers", status: "unknown", detail: null }] } });
  await tick();
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick(900);
  const summary = screen.getByText("The agent confirmed Stop.").closest(".agent-stop-summary") as HTMLElement;
  expect(within(summary).getByText("The agent’s last message before Stop: “3 of 7 files read; checking src/retry.rs next.”")).toBeTruthy();
  expect(summary.textContent).toContain("1 step finished before Stop. Last step: Read src/retry.rs · did not finish");
});

it("stopping a retained helper from an earlier run does not attach its receipt to the latest run", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.receipt)!;
  const first = host.runs.find(r => r.task_id === task.task_id)!;
  const second = { ...first, run_id: "newer-run", admitted_at: first.admitted_at + 60_000,
    outcome: { ...first.outcome!, summary: "Newer run result" } };
  host.runs.push(second);
  host.emit({ type: "run_upserted", run: second });
  host.emit({ type: "task_upserted", task: { ...task, current_run: second.run_id } });
  // Exact event-only shape emitted by services::children::stop_child for a
  // helper retained by `first`; the wire event has no run_id or child_id.
  host.emit({ type: "intervention_recorded", task_id: task.task_id, target_id: null,
    receipt: { kind: "stop_task", requested_at: second.admitted_at + 5_000,
      summary: "The helper has ended", facts: [{ label: "Child: earlier-run helper", status: "done", detail: "Stopped" }] } });
  await tick();
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick(900); // Let timeline hydration finish: this event never has a timeline row.
  expect(screen.getByText("Newer run result")).toBeTruthy();
  expect(screen.queryByText("earlier-run helper")).toBeNull();
});

it("reviewing a failure resolves its alert without changing the outcome", async () => {
  const host = window.__sim!.host;
  const item = host.attention.find(a => a.kind === "failure")!;
  const task = host.tasks.find(t => t.task_id === item.task_id)!;
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick(900);
  expect(host.attention.find(a => a.attention_id === item.attention_id)?.resolved_at).not.toBeNull();
  expect(host.tasks.find(t => t.task_id === task.task_id)?.state).toBe("failed");
});

it("keeps a human decision on its run after simulator reload", async () => {
  const task = window.__sim!.host.tasks.find(t => t.state === "reconciliation_required")!;
  const request = app.session.command({ command: "reconcile_task", task_id: task.task_id, resolution: "mark_completed" }, { label: "Mark completed", expectedRevision: task.revision });
  await tick(400); await request;
  window.__sim!.restart();
  await tick(1500);
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  expect(screen.getByText(/Marked completed by you ·.*not verified by the computer/)).toBeTruthy();
});

it("offers what to do next only once the run has ended", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.receipt)!;
  const completed = host.runs.find(r => r.task_id === task.task_id)!;
  const run = { ...completed, run_id: "working-receipt-run", state: "working" as const, admitted_at: Date.now() - 60_000, ended_at: null, outcome: null };
  host.runs.push(run);
  host.emit({ type: "run_upserted", run });
  host.emit({ type: "task_upserted", task: { ...task, current_run: run.run_id, state: "working" } });

  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick(1000);
  expect(document.querySelector(".receipt-outcome")).toBeNull();
  expect(screen.getByText(/This run is still working/)).toBeTruthy();
  expect(screen.getByText("Running · 1 min so far")).toBeTruthy();
  act(() => {
    host.emit({ type: "run_upserted", run: { ...run, state: "completed", ended_at: Date.now(), outcome: completed.outcome } });
    host.emit({ type: "task_upserted", task: { ...task, current_run: run.run_id } });
  });
  // The sim's finished run left a failed check, a helper running and an
  // unknown, so it is finished, not done, and Archive is not the lead.
  const outcome = screen.getByRole("region", { name: "Finished, needs a look" });
  expect(outcome.className).toContain("receipt-outcome--doubt");
  expect(within(outcome).getByText("1 check failed · 1 helper still running · work still active · unknowns remain.")).toBeTruthy();
  expect(within(outcome).getByText("npm run test:keyboard failed")).toBeTruthy();
  expect(within(outcome).getByRole("button", { name: "Archive" }).className).not.toContain("btn--primary");
  for (const name of ["Archive", "Archive and start new"]) expect(within(outcome).getByRole("button", { name })).toBeTruthy();
  // Continuing is the button under the receipt, not a second one in the card.
  expect(within(outcome).queryByRole("button", { name: "Continue" })).toBeNull();
  expect(screen.getByRole("button", { name: "Continue this task" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Looks right" })).toBeNull();
});

it.each(["start_now", "queue", "continue"] as const)("keeps the annotated request for %s in its own run receipt", async path => {
  const { ids } = await import("../../dev/sim/scenario");
  const host = window.__sim!.host;
  const bundle: AnnotationBundle = { bundle_id: `request-${path}`, revision: 1, marks: [], sources: [],
    comments: [{ comment_id: "general", text: "Keep this general comment", mark_ids: [], origin: { kind: "typed" }, link_origins: [] }], steps: [],
    general_note: `Annotated ${path} request`, speech: null };
  const created = await host.command({ command_id: `create-${path}`, issued_at: Date.now(), expires_at: null, expected_revision: null,
    body: { command: "create_task", project_id: ids.pNotes, kind: "coding", mode: "ask", title: `Receipt ${path}`,
      prompt: "Original request", bundle: path === "continue" ? null : bundle, harness: "claude_code", model: null, effort: null, mission: null, target_id: null,
      placement: path === "queue" ? { mode: "queue", position: null, authorization: "ask_before_start" } : { mode: "start_now" } } });
  if (created.result?.kind !== "task_created") throw Error(JSON.stringify(created));
  const taskId = created.result.task_id;
  let previousRun: string | null = null;
  if (path === "queue") {
    const queue = host.talk.queue(ids.pNotes);
    queue.paused = false; host.talk.put(queue);
    const task = host.tasks.find(t => t.task_id === taskId)!;
    const result = await host.command({ command_id: "start-queued", issued_at: Date.now(), expires_at: null, expected_revision: queue.revision,
      body: { command: "queue_start", item_id: created.result.queue_item_id!, task_revision: task.revision } });
    expect(result.status).toBe("accepted");
  } else if (path === "continue") {
    await tick(30_000);
    const task = host.tasks.find(t => t.task_id === taskId)!;
    previousRun = task.current_run;
    const result = await host.command({ command_id: "continue-annotated", issued_at: Date.now(), expires_at: null, expected_revision: task.revision,
      body: { command: "resume_task", task_id: taskId, text: "Continuation request", bundle } });
    expect(result.status).toBe("accepted");
  }
  await tick(30_000);
  const route = { screen: "receipt", taskId } as const;
  view(<Receipt route={route} />, route);
  await tick(900);
  expect(screen.getByText(`Annotated ${path} request`)).toBeTruthy();
  expect(screen.getByText("Keep this general comment")).toBeTruthy();
  expect(screen.getByText(path === "continue" ? "Continuation request" : "Original request")).toBeTruthy();
  if (previousRun) {
    fireEvent.click(screen.getByRole("button", { name: /^Choose a run: showing / }));
    fireEvent.click(screen.getByRole("button", { name: /^Run 1, / }));
    expect(screen.queryByText(`Annotated ${path} request`)).toBeNull();
    expect(screen.getByText("Original request")).toBeTruthy();
  }
});

it("keeps non-delivery and preview origin, and restores the exact picture to a reviewed retry", async () => {
  const { annotationStore } = await import("../../state/annotations");
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.receipt)!;
  const run = host.runs.find(r => r.run_id === task.current_run)!;
  const bundle: AnnotationBundle = { bundle_id: "undelivered-markup", revision: 3, marks: [{ mark_id: "mark", color: 0, created_at: 1000, source_key: "S1", label: "A", shape: { kind: "rect", rect: { x: 0.1, y: 0.1, w: 0.5, h: 0.5 } }, deleted: false }],
    sources: [{ key: "S1", artifact_id: "original", sha256: "0".repeat(64), media_type: "image/png", width: 390, height: 200, captured_at: 1000, derived_from: null, derivation: null,
      origin: { kind: "preview_capture", capture_id: "capture", host_id: app.profile.hostId, computer_name: "studio", preview_id: "notes", preview_label: "Notes", captured_at: 1000 } }],
    comments: [], steps: [], general_note: null, speech: null };
  // Isolate this run from earlier fixture markup and its after pictures.
  host.journal.splice(0, host.journal.length, ...host.journal.filter(e => !(e.event.type === "timeline_appended" && e.event.task_id === task.task_id)));
  app.session.state.set(s => ({ ...s, timelines: { ...s.timelines, [task.task_id]: { loaded: false, hasMore: false, items: [] } } }));
  host.emit({ type: "run_upserted", run: { ...run, state: "interrupted" } });
  host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: 1000, run_id: run.run_id,
    item: { kind: "user_message", text: "Check this exact picture", bundle, delivery: "failed" } } });
  const route = { screen: "receipt", taskId: task.task_id } as const;
  app.ui.selectTask(task.project_id, task.task_id); app.ui.push(route, "talk");
  view(<Receipt route={route} />, route);
  await tick(900);
  expect(screen.getByText("Not delivered · task stopped")).toBeTruthy();
  expect(screen.getByText(/Taken from the Notes preview on studio/).textContent).toContain(new Date(1000).toLocaleString());
  expect(document.querySelector(".receipt-request")?.textContent).not.toContain("No after picture");
  expect(screen.queryByRole("button", { name: "Not quite — A" })).toBeNull();
  const before = host.journal.length;
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Put back in composer" })));
  expect(host.journal).toHaveLength(before);
  expect(app.ui.route()).toEqual({ screen: "talk" });
  const draft = app.ui.draft(task.project_id, task.task_id)!;
  expect(draft.text).toBe("Check this exact picture");
  const markup = annotationStore(app.profile.profileId).get()[draft.annotationId!]!;
  expect(markup.sources[0]?.origin).toEqual(bundle.sources[0]!.origin);
  expect(markup.marks).toEqual(bundle.marks);
});

it("a receipt says so when a run reported no steps, files or checks, and draws no empty group", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.failed)!;
  const run = host.runs.find(r => r.run_id === task.current_run)!;
  const bare = { ...run, outcome: { ...run.outcome!, changed_files: [], checks: [] } };
  host.runs.splice(host.runs.indexOf(run), 1, bare);
  host.emit({ type: "run_upserted", run: bare });
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick(900);
  const section = (title: string) => screen.getByRole("heading", { name: title }).closest("section")!;
  expect(section("Steps").textContent).toContain("No steps reported.");
  expect(section("Changed files").textContent).toContain("No changed files reported.");
  expect(section("Checks").textContent).toContain("No checks reported.");
  for (const group of document.querySelectorAll(".work-screen .group")) expect(group.children.length).toBeGreaterThan(0);
});

it("a receipt shows a denied check as not run, with the reason, apart from one that failed", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.failed)!;
  const run = host.runs.find(r => r.run_id === task.current_run)!;
  const checks = [
    { name: "Check command", command: "npm test", status: "not_run" as const, detail: "Denied before it ran: Not on the shared runner", provenance: "native" as const },
    { name: "Check command", command: "npm run build", status: "failed" as const, detail: "Exit status 2", provenance: "native" as const, exit_code: 2 },
  ];
  const denied = { ...run, outcome: { ...run.outcome!, checks } };
  host.runs.splice(host.runs.indexOf(run), 1, denied);
  host.emit({ type: "run_upserted", run: denied });
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick(900);
  const [notRun, failed] = [...document.querySelectorAll(".work-check-result")].map(c => c.textContent);
  expect(notRun).toContain("npm test");
  expect(notRun).toContain("Not run");
  expect(notRun).toContain("Denied before it ran: Not on the shared runner");
  expect(failed).toContain("Failed");
  expect(failed).toContain("Exit status 2");
});

it("a second tap after Mark completed lands on a confirmation, not on a verdict", async () => {
  const task = window.__sim!.host.tasks.find(t => t.state === "reconciliation_required")!;
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick(900);
  const lastAction = () => document.querySelector<HTMLButtonElement>(".work-review button:last-of-type");
  expect(lastAction()?.textContent).toBe("Mark completed");
  fireEvent.click(lastAction()!);
  for (let waited = 0; screen.queryByRole("button", { name: "Mark completed" }) && waited < 2000; waited += 50) await tick(50);
  await tick(200);
  const second = lastAction();
  if (second) fireEvent.click(second);
  await tick();
  expect(Object.keys(localStorage).some(k => k.includes(".review."))).toBe(false);
  expect(document.querySelector(".work-review")?.textContent).toContain("Marked completed");

  await tick(2000);
  expect(screen.getByRole("region", { name: "Done" })).toBeTruthy();
  expect(document.querySelectorAll(".work-review")).toHaveLength(0);
});

it("after Mark failed the receipt offers what to fix and a retry", async () => {
  const task = window.__sim!.host.tasks.find(t => t.state === "reconciliation_required")!;
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick(900);
  fireEvent.click(screen.getByRole("button", { name: "Mark failed" }));
  for (let waited = 0; screen.queryByRole("button", { name: "Mark failed" }) && waited < 2000; waited += 50) await tick(50);
  await tick(2200);
  expect(document.querySelector(".work-receipt-head")?.textContent).toContain("Marked failed");
  const outcome = screen.getByRole("region", { name: "Didn’t finish" });
  expect(within(outcome).getByText(/You marked it failed/)).toBeTruthy();
  expect(within(outcome).getByRole("button", { name: "Try again" }).className).toContain("btn--primary");
  expect(within(outcome).getByRole("button", { name: "Say what to fix" })).toBeTruthy();
  expect(within(outcome).queryByRole("button", { name: "Archive and start new" })).toBeNull();
});

it("what the run came to sits under the receipt's headline, and nothing is saved by reading it", async () => {
  const task = window.__sim!.host.tasks.find(t => t.task_id === workIds.receipt)!;
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick(900);
  const outcome = document.querySelector(".receipt-outcome")!;
  expect(document.querySelector(".work-receipt-head")?.nextElementSibling).toBe(outcome);
  expect(!!(outcome.compareDocumentPosition(screen.getByRole("heading", { name: "Result" })) & Node.DOCUMENT_POSITION_FOLLOWING)).toBe(true);
  expect(screen.queryByRole("button", { name: "Looks right" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Not quite" })).toBeNull();
  expect(Object.keys(localStorage).filter(k => k.includes(".review."))).toHaveLength(0);
});

it("shows verification, follow-up, and collapsed check output on a receipt", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.receipt)!;
  const run = host.runs.find(r => r.run_id === task.current_run)!;
  const updated = { ...run, follow_up: { run_id: "later", at: Date.now(), reason: "changed_files" as const },
    outcome: { ...run.outcome!, checks: [{ ...run.outcome!.checks[0]!, output: "Passed 3 tests\n", after_last_change: false }],
      verification: { status: "not_checked" as const, passed: 0, failed: 0, unknown: 0 } } };
  host.emit({ type: "run_upserted", run: updated });
  const route = { screen: "receipt", taskId: task.task_id } as const;
  view(<Receipt route={route} />, route);
  await tick(900);
  const checks = screen.getByRole("heading", { name: "Checks" }).closest("section")!;
  expect(within(checks).getByText("Not verified: no check ran after its last change.")).toBeTruthy();
  expect(within(checks).getByText("Ran before its last change")).toBeTruthy();
  const output = within(checks).getByText("Output").closest("details")!;
  expect(output.hasAttribute("open")).toBe(false);
  expect(output.querySelector("pre")?.textContent).toBe("Passed 3 tests\n");
  expect(document.querySelector(".work-receipt-head")?.textContent).toContain("Needed a follow-up: a later run changed files after this one finished.");
});
