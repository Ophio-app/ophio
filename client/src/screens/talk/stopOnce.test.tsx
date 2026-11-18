// SPDX-License-Identifier: Apache-2.0
//
// A confirmed Stop is said once in a chat: the turn's “Stopped” line, with
// the receipt behind it and the way to carry on under it. A Stop the agent
// hasn't confirmed keeps its own line, and “Stop requested” goes once the
// computer says it stopped.
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { projects, runs, tasks } from "../../dev/sim/scenario";
import { Toasts, toast } from "../../design/Toast";
import { HostSession } from "../../host/session";
import type { ReceiptFact, Run, Task, TimelineItem } from "../../protocol/types";
import { emptyHostState, type HostState } from "../../state/projection";
import { Store } from "../../state/store";
import { untilStopped } from "../control/StopTask";
import { OutcomeCard } from "../work/OutcomeCard";
import { runOutcome } from "../work/outcome";
import { rows } from "./timeline";

const now = 1_800_000_000_000;
let seq = 0;
const at = (body: TimelineItem["item"], run_id = "r1"): TimelineItem => ({ seq: ++seq, at: 1000 + seq, run_id, item: body }) as TimelineItem;
const stopReceipt = (...statuses: ReceiptFact["status"][]) => at({ kind: "receipt", receipt: {
  kind: "stop_task", requested_at: 1, summary: statuses.every(s => s === "done") ? "The agent confirmed Stop." : "The agent hasn’t confirmed Stop. Check the task on the computer.",
  facts: statuses.map(status => ({ label: "Agent", status, detail: null })) } });
const said = (items: TimelineItem[]) => rows(items).map(r => r.kind === "receipt" ? `receipt: ${r.summary}` : r.kind === "run_ended" ? `ended: ${r.state}` : r.kind);

describe("the transcript says a confirmed Stop once", () => {
  it("drops the confirmed Stop receipt beside the turn's Stopped line", () => {
    expect(said([at({ kind: "run_started", run_id: "r1" }), stopReceipt("done", "done"), at({ kind: "run_ended", run_id: "r1", state: "interrupted" })]))
      .toEqual(["run_started", "ended: interrupted"]);
  });

  it("keeps a Stop the agent hasn't confirmed, or that failed: that is what to check", () => {
    expect(said([stopReceipt("done", "unknown"), at({ kind: "run_ended", run_id: "r1", state: "interrupted" })]))
      .toEqual(["receipt: The agent hasn’t confirmed Stop. Check the task on the computer.", "ended: interrupted"]);
    expect(said([stopReceipt("failed"), at({ kind: "run_ended", run_id: "r1", state: "interrupted" })])[0]).toMatch(/^receipt:/);
  });

  it("keeps a confirmed Stop whose turn ended some other way, and other receipts", () => {
    expect(said([stopReceipt("done"), at({ kind: "run_ended", run_id: "r1", state: "completed" })])[0]).toMatch(/^receipt:/);
    expect(said([stopReceipt("done")])).toHaveLength(1);
    const control = at({ kind: "receipt", receipt: { kind: "take_over", requested_at: 1, summary: "You have control.", facts: [{ label: "x", status: "done", detail: null }] } });
    expect(said([control, at({ kind: "run_ended", run_id: "r1", state: "interrupted" })])).toEqual(["receipt: You have control.", "ended: interrupted"]);
  });
});

describe("“Stop requested” lasts only until the computer confirms", () => {
  const task = (state: Task["state"]): Task => ({ ...tasks(now)[0]!, current_run: null, state });
  let host: Store<HostState>;
  let ui: UiStore;
  beforeEach(() => {
    vi.useFakeTimers();
    host = new Store<HostState>({ ...emptyHostState, tasks: { [task("working").task_id]: task("working") } });
    ui = new UiStore("p1");
    render(<Toasts />);
  });
  afterEach(() => { act(() => { vi.runAllTimers(); }); cleanup(); vi.useRealTimers(); });
  const shown = () => document.querySelector(".toasts")?.textContent ?? "";
  const stop = () => act(() => { host.set(s => ({ ...s, tasks: { [task("interrupted").task_id]: task("interrupted") } })); });

  it("a toast can be taken down early", () => {
    let dismiss = () => {};
    act(() => { dismiss = toast("Something"); });
    expect(shown()).toBe("Something");
    act(() => dismiss());
    expect(shown()).toBe("");
  });

  it("goes once the task stopped, and the task on screen says so itself", () => {
    const t = task("working");
    act(() => ui.push({ screen: "receipt", taskId: t.task_id }));
    act(() => untilStopped(host, ui, t, toast(`Stop requested for “${t.title}”`)));
    expect(shown()).toMatch(/^Stop requested/);
    // Still running: the request is still true.
    act(() => { host.set(s => ({ ...s })); });
    expect(shown()).toMatch(/^Stop requested/);
    stop();
    expect(shown()).toBe("");
  });

  it("becomes “Stopped” where nothing else on screen confirms it", () => {
    const t = task("working");
    act(() => ui.push({ screen: "next_up" }, "work"));
    act(() => untilStopped(host, ui, t, toast(`Stop requested for “${t.title}”`)));
    stop();
    expect(shown()).toBe(`Stopped “${t.title}”`);
  });
});

describe("a stopped chat's card is only the way on", () => {
  let app: App;
  beforeEach(async () => {
    localStorage.clear(); vi.useFakeTimers();
    const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
    const session = new HostSession(profile.profileId, platform.control(profile.profileId));
    app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
    session.start(); await act(async () => { await vi.advanceTimersByTimeAsync(900); });
  });
  afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); });
  const card = (conversation: boolean, compact: boolean) => {
    const run: Run = { ...runs(now)[0]!, state: "interrupted", ended_at: now };
    const task: Task = { ...tasks(now).find(t => t.task_id === run.task_id)!, conversation, state: "interrupted", current_run: run.run_id };
    const project = projects(now).find(p => p.project_id === task.project_id)!;
    const outcome = runOutcome(task, run)!;
    render(<AppProvider app={app}><OutcomeCard task={task} run={run} project={project} outcome={outcome} plan={null} writing compact={compact} /></AppProvider>);
    return outcome;
  };

  it("in a chat, keeps Carry on and the heading for a screen reader, without saying Stopped again", () => {
    const outcome = card(true, true);
    expect(outcome.kind).toBe("stopped");
    const heading = screen.getByRole("heading", { name: "Stopped before it finished" });
    expect(heading.className).toBe("visually-hidden");
    expect(screen.queryByText("Carry on to pick up where it stopped.")).toBeNull();
    expect(screen.getByRole("button", { name: "Carry on" })).toBeTruthy();
    expect(document.querySelector(".receipt-outcome--quiet")).toBeTruthy();
  });

  it("a task's receipt still says it in full", () => {
    card(false, false);
    expect(screen.getByRole("heading", { name: "Stopped before it finished" }).className).toBe("receipt-outcome__title");
    expect(screen.getByText("Carry on to pick up where it stopped.")).toBeTruthy();
  });
});
