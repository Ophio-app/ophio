// SPDX-License-Identifier: Apache-2.0
//
// A chat whose last reply was cut off, where the agent and what it started
// under Ophio have stopped, is replied to like any other: one send, in the
// same session.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { harnesses, runs, tasks } from "../../dev/sim/scenario";
import { cutOffChat, cutOffIds } from "../../dev/sim/talkWork";
import { HostSession } from "../../host/session";
import type { Run, Task, TimelineItem } from "../../protocol/types";
import { applyLive, interruptLive, liveTextForTask, openAttention, replyAttention, CHAT_REPLY_RECOVERY_FEATURE } from "../../state/projection";
import Talk from "./Talk";
import { appLock } from "../../platform/lock";
import { RouteProvider } from "../../shell/Screen";
import { Composer } from "./Composer";
import { CHAT_CARRY_ON_FEATURE, composerMode, cutOff, defaultIntent, type WriterHold } from "./intent";
import { rows } from "./timeline";
import { decisionLabel } from "../work/logic";
import Receipt from "../work/Receipt";
import { taskAttentionReason } from "../work/taskActivity";
import { OutcomeUnknownError } from "../../host/session";
import { resend, send } from "./send";

const now = 1_800_000_000_000;
const claude = harnesses.find((h) => h.kind === "claude_code")!;
const chat = (over: Partial<Task> = {}): Task => ({ ...tasks(now)[0]!, mode: "ask", conversation: true, state: "reconciliation_required", current_run: "r-cut", ...over });
const run = (over: Partial<Run> = {}): Run => ({ ...runs(now)[0]!, run_id: "r-cut", state: "reconciliation_required", nothing_running: true, children: [], cleanup_pending: false, ...over });

describe("cut-off chats in the composer", () => {
  it("offers Reply, carrying on from the cut-off reply, on a computer that settles it", () => {
    const m = composerMode(chat(), run(), claude, null, "api", true);
    expect(m).toMatchObject({ mode: "ended", cutOff: true });
    if (m.mode !== "ended") throw new Error(m.mode);
    expect(m.options.map((o) => o.label)).toEqual(["Reply"]);
    expect(m.options[0]!.unavailable).toBeNull();
    expect(m.options[0]!.detail).toMatch(/^Goes on with the conversation from where the last reply was cut off\./);
    expect(defaultIntent(m)).toEqual({ kind: "continue" });
  });

  it("keeps today's review on an older computer", () => {
    expect(composerMode(chat(), run(), claude, null, "api", false).mode).toBe("reconcile");
    expect(composerMode(chat(), run(), claude).mode).toBe("reconcile");
  });

  it("keeps the review while anything the old run started may still be running", () => {
    const helper = { child_id: "h", native_ref: null, label: "A helper", started_at: now, ended_at: null, provenance: "native" as const };
    for (const r of [
      run({ nothing_running: undefined }),
      run({ cleanup_pending: true }),
      run({ children: [{ ...helper, state: "active" }] }),
      run({ children: [{ ...helper, state: "unknown" }] }),
      run({ state: "interrupted" }),
      run({ run_id: "r-older" }),
    ]) {
      expect(cutOff(chat(), r)).toBe(false);
      expect(composerMode(chat(), r, claude, null, "api", true).mode).toBe("reconcile");
    }
  });

  it("never carries on a task, only a chat", () => {
    const work = chat({ conversation: false, mode: "work" });
    expect(cutOff(work, run())).toBe(false);
    expect(composerMode(work, run(), claude, null, "api", true).mode).toBe("reconcile");
  });

  it("isn't held by its own cut-off reply, but is by another task's", () => {
    const own: WriterHold = { task: chat({ mode: "work" }), unresolved: true, waiting: null, helpers: false };
    const mine = composerMode(chat({ mode: "work" }), run(), claude, own, "api", true);
    if (mine.mode !== "ended") throw new Error(mine.mode);
    expect(mine.options[0]!.unavailable).toBeNull();
    const other: WriterHold = { task: { ...chat(), task_id: "t-other", title: "Summarize the README" }, unresolved: true, waiting: null, helpers: false };
    const theirs = composerMode(chat({ mode: "work" }), run(), claude, other, "api", true);
    if (theirs.mode !== "ended") throw new Error(theirs.mode);
    expect(theirs.options[0]!.unavailable).toMatch(/^Can’t continue yet: “Summarize the README” ended without a known result/);
  });
});

describe("cut-off turns in the conversation", () => {
  const item = (seq: number, body: TimelineItem["item"], runId = "r-cut"): TimelineItem => ({ seq, at: now + seq, run_id: runId, item: body });
  const said = [
    item(1, { kind: "user_message", text: "Why?", bundle: null, delivery: "native_delivered" }),
    item(2, { kind: "agent_message", text: "Looking.", partial: false }),
  ];

  it("keeps a journaled partial reply and its steps in order, with one cut-off line", () => {
    const transcript = [
      item(1, { kind: "user_message", text: "Why?", bundle: null, delivery: "native_delivered" }),
      item(2, { kind: "agent_message", text: "Looking at the rollup.", partial: false }),
      item(3, { kind: "activity", tool: "Read", key: "read-rollup", status: "running", summary: "Read jobs/rollup.ts", detail: null }),
      item(4, { kind: "activity", tool: "Read", key: "read-rollup", status: "succeeded", summary: "Read jobs/rollup.ts", detail: "Opened the rollup job" }),
      item(5, { kind: "agent_message", text: "The query reads the old tab", partial: true }),
      item(6, { kind: "run_ended", run_id: "r-cut", state: "reconciliation_required" }),
    ];
    const next = item(7, { kind: "user_message", text: "Continue", bundle: null, delivery: "native_delivered" }, "r-next");
    const out = rows([...transcript, next], [run({ carried_on: { at: now + 7, by: { kind: "device", device_id: "d" } } })]);
    expect(out.filter(r => r.kind === "agent").map(r => r.text)).toEqual(["Looking at the rollup.", "The query reads the old tab"]);
    expect(out.map(r => r.kind)).toEqual(["user", "agent", "steps", "agent", "run_ended", "run_started", "user"]);
    expect(out.filter(r => r.kind === "run_ended")).toEqual([expect.objectContaining({ cutOff: true })]);
    expect(out.find(r => r.kind === "steps")).toMatchObject({ ended: true, steps: [expect.objectContaining({ output: "Opened the rollup job" })] });
  });

  it("keeps partial fragments around a step in journal order", () => {
    const transcript = [
      item(1, { kind: "user_message", text: "Why?", bundle: null, delivery: "native_delivered" }),
      item(2, { kind: "agent_message", text: "First part", partial: true }),
      item(3, { kind: "activity", tool: "Read", key: "read-rollup", status: "succeeded", summary: "Read jobs/rollup.ts", detail: null }),
      item(4, { kind: "agent_message", text: " then the next part", partial: true }),
      item(5, { kind: "run_ended", run_id: "r-cut", state: "reconciliation_required" }),
    ];
    const out = rows(transcript, [run()]);
    expect(out.map(r => r.kind)).toEqual(["user", "agent", "steps", "agent", "run_ended"]);
    expect(out.filter(r => r.kind === "agent").map(r => r.text)).toEqual(["First part", " then the next part"]);
    expect(out.filter(r => r.kind === "run_ended")).toEqual([expect.objectContaining({ cutOff: true })]);
  });

  it.each([false, true])("shows one Cut off divider for an unconfirmed chat with saved partial text (recorded end: %s)", recordedEnd => {
    const unconfirmed = run({ nothing_running: undefined });
    const partial = item(3, { kind: "agent_message", text: "The answer starts here", partial: true });
    const ended = item(4, { kind: "run_ended", run_id: "r-cut", state: "reconciliation_required" });
    const transcript = [...said, partial, ...(recordedEnd ? [ended] : [])];
    const shown = rows(transcript, [unconfirmed], true);
    expect(shown.filter(r => r.kind === "agent").map(r => r.text)).toEqual(["Looking.", "The answer starts here"]);
    expect(shown.filter(r => r.kind === "run_ended")).toEqual([expect.objectContaining({ cutOff: true, runId: "r-cut" })]);
    expect(shown.at(-1)).toMatchObject({ kind: "run_ended", cutOff: true });
    expect(rows(transcript, [unconfirmed]).filter(r => r.kind === "run_ended" && r.cutOff)).toHaveLength(0);
    expect(rows(said, [unconfirmed], true).some(r => r.kind === "run_ended")).toBe(false);
    const carried = run({ state: "interrupted", nothing_running: true, carried_on: { at: now + 10, by: { kind: "device", device_id: "d" } } });
    const later = item(5, { kind: "user_message", text: "Go on", bundle: null, delivery: "native_delivered" }, "r-next");
    const continued = rows([...transcript, later], [carried], true);
    expect(continued.filter(r => r.kind === "run_ended" && r.runId === "r-cut")).toEqual([expect.objectContaining({ cutOff: true })]);
    expect(continued.findIndex(r => r.kind === "run_ended")).toBeLessThan(continued.findIndex(r => r.kind === "user" && r.text === "Go on"));
  });

  it("reads as cut off after a restart that recorded no end", () => {
    const out = rows(said, [run()]);
    expect(out.at(-1)).toMatchObject({ kind: "run_ended", runId: "r-cut", cutOff: true });
    expect(out.filter((r) => r.kind === "run_ended")).toHaveLength(1);
  });

  it("reads as cut off, once, where the computer recorded an unknown end", () => {
    const ended = [...said, item(3, { kind: "run_ended", run_id: "r-cut", state: "reconciliation_required" })];
    const carried = run({ state: "interrupted", nothing_running: true, carried_on: { at: now + 10, by: { kind: "device", device_id: "d" } } });
    const out = rows(ended, [carried]);
    expect(out.filter((r) => r.kind === "run_ended")).toEqual([expect.objectContaining({ key: "s3", cutOff: true })]);
  });

  it("sits after the cut-off turn, not after the reply that carried it on", () => {
    const carried = run({ state: "interrupted", carried_on: { at: now + 10, by: { kind: "device", device_id: "d" } } });
    const next = item(5, { kind: "user_message", text: "Go on.", bundle: null, delivery: "native_delivered" }, "r-next");
    const out = rows([...said, next], [carried]);
    const at = out.findIndex((r) => r.kind === "run_ended");
    expect(out[at]).toMatchObject({ cutOff: true, at: now + 2 });
    expect(out.slice(at + 1).some((r) => r.kind === "user")).toBe(true);
  });

  it("draws nothing for a carried-on turn whose messages aren't loaded, and returns in place with them", () => {
    const carried = run({ state: "interrupted", carried_on: { at: now + 10, by: { kind: "device", device_id: "d" } } });
    const later = run({ run_id: "r-next", state: "completed", nothing_running: undefined, admitted_at: now + 20 });
    const newer = [
      item(30, { kind: "user_message", text: "And the weekly chart?", bundle: null, delivery: "native_delivered" }, "r-next"),
      item(31, { kind: "agent_message", text: "Same rollup, same fix.", partial: false }, "r-next"),
    ];
    // The transcript keeps only the newest items: the cut-off turn's are gone.
    const trimmed = rows(newer, [carried, later]);
    expect(trimmed.some((r) => r.kind === "run_ended" && r.cutOff)).toBe(false);
    expect(trimmed.at(-1)).toMatchObject({ kind: "agent", text: "Same rollup, same fix." });
    // Loading older messages brings it back, after its own turn.
    const all = rows([...said, ...newer], [carried, later]);
    const at = all.findIndex((r) => r.kind === "run_ended" && r.cutOff);
    expect(all[at - 1]).toMatchObject({ kind: "agent", text: "Looking." });
    expect(all[at + 1]).toMatchObject({ kind: "run_started", runId: "r-next" });
  });

  it("is neither marked failed nor completed, and a person's mark still wins", () => {
    const carried = run({ state: "interrupted", carried_on: { at: now, by: { kind: "device", device_id: "d" } } });
    expect(decisionLabel(carried)).toBe("Cut off");
    expect(decisionLabel(run({ state: "failed", human_resolution: { resolution: "mark_failed", at: now, by: { kind: "device", device_id: "d" } } }))).toBe("Marked failed");
    expect(rows(said, [run({ nothing_running: undefined })]).some((r) => r.kind === "run_ended")).toBe(false);
  });
});

describe("the message box for a cut-off chat", () => {
  let app: App;
  beforeEach(async () => {
    localStorage.clear(); vi.useFakeTimers();
    vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
    const platform = createSimPlatform();
    const profile = (await platform.profiles())[0]!;
    const session = new HostSession(profile.profileId, platform.control(profile.profileId));
    app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
    session.start(); await vi.advanceTimersByTimeAsync(900);
  });
  afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
  const tick = async (ms = 180) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
  const view = (child: React.ReactNode, route: Route = { screen: "talk" }) =>
    render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{child}</RouteProvider></AppProvider>);
  async function shown(confirmed = true) {
    const host = window.__sim!.host;
    cutOffChat(host, { confirmed });
    await tick();
    const s = app.session.state.get();
    const task = s.tasks[cutOffIds.task]!, run = s.runs[cutOffIds.run]!;
    const project = s.projects[task.project_id]!;
    const harness = s.harnesses.find((h) => h.kind === task.harness)!;
    return { host, task, run, project, harness, composer: () => <Composer project={project} task={task} run={run} harness={harness} agent={null} /> };
  }

  it("seeing the cut-off Chat acknowledges its reply notice without deciding its outcome", async () => {
    const { host, task, run } = await shown();
    const original = host.attention.find(a => a.task_id === task.task_id)!;
    const item = { ...original, kind: "completed" as const, topic: "finished" as const, title: "Reply cut off. Write to carry on." };
    host.attention = host.attention.map(a => a.attention_id === item.attention_id ? item : a);
    host.emit({ type: "attention_upserted", item });
    await tick();
    expect(replyAttention(app.session.state.get()).some(a => a.attention_id === item.attention_id)).toBe(true);
    const command = vi.spyOn(app.session, "command");
    const offLock = appLock(app.platform).start();
    act(() => app.ui.openTask(task.project_id, task.task_id));
    view(<Talk route={{ screen: "talk" }} />);
    await tick(900);
    expect(command.mock.calls.some(([c]) => c.command === "acknowledge_attention" && c.attention_id === item.attention_id)).toBe(true);
    expect(replyAttention(app.session.state.get()).some(a => a.attention_id === item.attention_id)).toBe(false);
    expect(openAttention(app.session.state.get()).some(a => a.task_id === task.task_id)).toBe(false);
    expect(app.session.state.get().runs[run.run_id]?.state).toBe("reconciliation_required");
    expect(command.mock.calls.some(([c]) => c.command === "reconcile_task")).toBe(false);
    offLock();
  });

  it("keeps the receipt useful without demanding an outcome for a settled chat", async () => {
    const { task } = await shown();
    const route = { screen: "receipt" as const, taskId: task.task_id };
    view(<Receipt route={route} />, route);
    await tick();
    expect(screen.getByText(/The agent and what it started have stopped/)).toBeTruthy();
    expect(screen.queryByText("The outcome needs your decision")).toBeNull();
    expect(screen.queryByRole("button", { name: "Mark failed" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Mark completed" })).toBeNull();
    expect(screen.getByRole("heading", { name: "Changed files" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Steps" })).toBeTruthy();
  });

  it.each(["task", "unconfirmed", "helper", "cleanup", "older computer"])("keeps the receipt decision for %s", async (reason) => {
    const { task, run } = await shown(reason !== "unconfirmed");
    act(() => app.session.state.set((s) => ({
      ...s,
      tasks: { ...s.tasks, [task.task_id]: { ...task, conversation: reason !== "task" } },
      runs: { ...s.runs, [run.run_id]: { ...run,
        cleanup_pending: reason === "cleanup",
        children: reason === "helper" ? [{ child_id: "unknown", native_ref: null, label: "Unknown helper", state: "unknown", started_at: now, ended_at: null, provenance: "native" }] : run.children,
      } },
      capabilities: reason === "older computer" ? { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== CHAT_CARRY_ON_FEATURE) } : s.capabilities,
    })));
    const route = { screen: "receipt" as const, taskId: task.task_id };
    view(<Receipt route={route} />, route);
    expect(screen.getByText("The outcome needs your decision")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Mark failed" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Mark completed" })).toBeTruthy();
  });

  it.each([false, true])("labels the old receipt as carried on, without another decision (other device: %s)", async (otherDevice) => {
    const { task, run } = await shown();
    await act(async () => {
      const sent = app.session.command({ command: "resume_task", task_id: task.task_id, text: "Please carry on", read_only: false }, { label: "Carry on", expectedRevision: task.revision });
      await vi.advanceTimersByTimeAsync(400);
      await sent;
    });
    if (otherDevice) act(() => app.session.state.set((s) => ({ ...s, runs: { ...s.runs, [run.run_id]: { ...s.runs[run.run_id]!, carried_on: { at: now, by: { kind: "device", device_id: "other-device" } } } } })));
    const route = { screen: "receipt" as const, taskId: task.task_id, runId: run.run_id };
    view(<Receipt route={route} />, route);
    await tick();
    expect(screen.getByText(new RegExp(`^Cut off, then carried on by ${otherDevice ? "a person" : "you"} in the next reply · .+ · its result isn’t known$`))).toBeTruthy();
    expect(screen.queryByText("The outcome needs your decision")).toBeNull();
    expect(screen.queryByRole("button", { name: "Mark failed" })).toBeNull();
    expect(screen.getByRole("heading", { name: "Changed files" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Steps" })).toBeTruthy();
  });

  it("shows one calm line and sends the reply once, in the same chat", async () => {
    const { host, composer } = await shown();
    view(composer());
    expect(screen.getByText(/The last reply was cut off\. Your next message carries on from there\./)).toBeTruthy();
    expect(screen.queryByText("Outcome unknown")).toBeNull();
    expect(screen.queryByText(/becomes a separate task/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Turn into a task" })).toBeNull();
    // What happened lives in the conversation's Cut off divider, once.
    expect(screen.queryByRole("button", { name: "What happened" })).toBeNull();

    app.ui.saveDraft(host.tasks.find((t) => t.task_id === cutOffIds.task)!.project_id, cutOffIds.task, { text: "Go on: check the rollup table." });
    const command = vi.spyOn(app.session, "command");
    cleanup();
    view(composer());
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await tick(400);
    expect(command).toHaveBeenCalledTimes(1);
    expect(command.mock.calls[0]![0]).toMatchObject({ command: "resume_task", task_id: cutOffIds.task, text: "Go on: check the rollup table." });
    expect(command.mock.calls[0]![0]).not.toHaveProperty("mode");
    const old = host.runs.find((r) => r.run_id === cutOffIds.run)!;
    expect(old).toMatchObject({ state: "interrupted", holds_writer: false });
    expect(old.carried_on?.by).toMatchObject({ kind: "device" });
    expect(old.human_resolution).toBeUndefined();
    const s = app.session.state.get();
    expect(s.tasks[cutOffIds.task]!.current_run).not.toBe(cutOffIds.run);
    expect(openAttention(s).some((a) => a.task_id === cutOffIds.task)).toBe(false);
  });

  it.each([false, true])("sends cached phone observations separately only when recovery is advertised (supported: %s)", async supported => {
    const { host, task, composer } = await shown();
    const text = "Sentence 41. Sentence 42 has the newest phone-only tail";
    const live = { kind: "agent_text_delta" as const, task_id: task.task_id, run_id: cutOffIds.run,
      message_key: "019a6e40-21f0-7000-8000-000000000001", text_offset: 0, text };
    host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: cutOffIds.run,
      item: { kind: "agent_message", partial: true, message_key: live.message_key, text_offset: 0, text: "Sentence 41." } } });
    await tick();
    act(() => {
      app.session.state.set(s => interruptLive(applyLive({ ...s, capabilities: { ...s.capabilities!,
        features: supported ? [...s.capabilities!.features, CHAT_REPLY_RECOVERY_FEATURE] : s.capabilities!.features.filter(f => f !== CHAT_REPLY_RECOVERY_FEATURE),
      } }, live, Date.now())));
      app.ui.saveDraft(task.project_id, task.task_id, { text: "Finish that sentence." });
    });
    const command = vi.spyOn(app.session, "command");
    view(composer());
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await tick(400);
    const body = command.mock.calls[0]![0];
    expect(body).toMatchObject({ command: "resume_task", task_id: task.task_id, text: "Finish that sentence." });
    if (supported) expect(body).toMatchObject({ observed_reply: {
      run_id: cutOffIds.run, message_key: live.message_key, text_offset: 0, text,
    } });
    else expect(body).not.toHaveProperty("observed_reply");
  });

  it("sends without unanchored evidence before any durable checkpoint and keeps its words visible", async () => {
    const { host, task, composer } = await shown();
    const text = "The phone saw this unfinished sentence before the first checkpoint";
    const live = { kind: "agent_text_delta" as const, task_id: task.task_id, run_id: cutOffIds.run,
      message_key: "019a6e40-21f0-7000-8000-000000000001", text_offset: 0, text };
    // Nothing from this reply reached the companion's durable transcript.
    const kept = host.journal.filter(e => !(e.event.type === "timeline_appended" && e.event.task_id === task.task_id && e.event.item.item.kind === "agent_message"));
    host.journal.splice(0, host.journal.length, ...kept);
    act(() => {
      app.session.state.set(s => {
        const timeline = s.timelines[task.task_id];
        const withoutReply = { ...s, timelines: { ...s.timelines, [task.task_id]: {
          items: (timeline?.items ?? []).filter(i => i.item.kind !== "agent_message"), loaded: true, hasMore: false,
        } }, capabilities: { ...s.capabilities!, features: [...s.capabilities!.features, CHAT_REPLY_RECOVERY_FEATURE] } };
        return interruptLive(applyLive(withoutReply, live, Date.now()));
      });
      app.ui.saveDraft(task.project_id, task.task_id, { text: "Finish that sentence." });
    });
    const command = vi.spyOn(app.session, "command");
    view(composer());
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await tick(400);
    expect(command).toHaveBeenCalledTimes(1);
    expect(command.mock.calls[0]![0]).toMatchObject({ command: "resume_task", task_id: task.task_id, text: "Finish that sentence." });
    expect(command.mock.calls[0]![0]).not.toHaveProperty("observed_reply");
    const state = app.session.state.get();
    expect(state.tasks[task.task_id]!.current_run).not.toBe(cutOffIds.run);
    expect(state.runs[cutOffIds.run]).toMatchObject({ state: "interrupted", outcome: null });
    expect(liveTextForTask(state, task.task_id).map(v => v.text)).toEqual([text]);
    expect(state.timelines[task.task_id]!.items.some(i => i.item.kind === "agent_message" && i.item.text.includes(text))).toBe(false);
    expect(app.ui.draft(task.project_id, task.task_id)?.text ?? "").toBe("");
    // The companion also tolerates an older saved command that still contains
    // unanchored evidence, covered by review_recovery_phone_only_observation.
  });

  it("sends normally when a newer helper has no checkpoint beside an existing durable partial", async () => {
    const { host, task, composer } = await shown();
    const mainKey = "019a6e40-21f0-7000-8000-000000000001";
    const helperKey = "019a6e40-21f0-7000-8000-000000000002";
    const main = "Main durable prefix. Main newer ending";
    const helper = "A newer helper stream with no surviving checkpoint";
    host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: cutOffIds.run,
      item: { kind: "agent_message", partial: true, message_key: mainKey, text_offset: 0, text: "Main durable prefix." } } });
    await tick();
    act(() => {
      app.session.state.set(s => {
        const ready = { ...s, capabilities: { ...s.capabilities!, features: [...s.capabilities!.features, CHAT_REPLY_RECOVERY_FEATURE] } };
        const previous = applyLive(ready, { kind: "agent_text_delta", task_id: task.task_id, run_id: cutOffIds.run,
          message_key: mainKey, text_offset: 0, text: main }, Date.now());
        return interruptLive(applyLive(previous, { kind: "agent_text_delta", task_id: task.task_id, run_id: cutOffIds.run,
          message_key: helperKey, text_offset: 0, text: helper }, Date.now() + 1));
      });
      app.ui.saveDraft(task.project_id, task.task_id, { text: "Please carry on." });
    });
    const command = vi.spyOn(app.session, "command");
    view(composer());
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await tick(400);
    expect(command).toHaveBeenCalledTimes(1);
    expect(command.mock.calls[0]![0]).toMatchObject({ command: "resume_task", task_id: task.task_id, text: "Please carry on." });
    expect(command.mock.calls[0]![0]).not.toHaveProperty("observed_reply");
    const state = app.session.state.get();
    expect(state.tasks[task.task_id]!.current_run).not.toBe(cutOffIds.run);
    expect(state.runs[cutOffIds.run]).toMatchObject({ state: "interrupted", outcome: null });
    expect(Object.values(state.liveText).filter(v => v.runId === cutOffIds.run).map(v => v.text)).toEqual([main, helper]);
    expect(liveTextForTask(state, task.task_id).some(v => v.text === helper)).toBe(true);
    expect(state.timelines[task.task_id]!.items.some(i => i.item.kind === "agent_message" && i.item.text.includes(helper))).toBe(false);
    expect(app.ui.draft(task.project_id, task.task_id)?.text ?? "").toBe("");
  });

  it("freezes observed context into the saved command so a retry never substitutes newer cached text", async () => {
    const { host, task } = await shown();
    const live = { kind: "agent_text_delta" as const, task_id: task.task_id, run_id: cutOffIds.run,
      message_key: "019a6e40-21f0-7000-8000-000000000001", text_offset: 0, text: "Exact tail at the original send" };
    host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: cutOffIds.run,
      item: { kind: "agent_message", partial: true, message_key: live.message_key, text_offset: 0, text: "Exact tail" } } });
    await tick();
    app.session.state.set(s => interruptLive(applyLive({ ...s, capabilities: { ...s.capabilities!, features: [...s.capabilities!.features, CHAT_REPLY_RECOVERY_FEATURE] } }, live, Date.now())));
    app.ui.saveDraft(task.project_id, task.task_id, { text: "Carry on." });
    const command = vi.spyOn(app.session, "command").mockRejectedValue(new OutcomeUnknownError("lost-reply"));
    const first = await send(app, { projectId: task.project_id, draftTaskId: task.task_id, text: "Carry on.", annotationId: null,
      label: "Send", build: () => ({ command: "resume_task", task_id: task.task_id, text: "Carry on." }) }, () => {});
    expect(first.ok).toBe(false);
    if (first.ok) throw new Error("Expected the lost receipt to leave a saved attempt");
    expect(first.attempt.body).toMatchObject({ observed_reply: { text: live.text } });
    app.session.state.set(s => interruptLive(applyLive(s, { ...live, message_key: "019a6e40-21f0-7000-8000-000000000002", text: "A newer local observation" }, Date.now() + 1)));
    await resend(app, task.project_id, task.task_id, first.attempt, "Send again");
    expect(command.mock.calls[1]![0]).toEqual(command.mock.calls[0]![0]);
    expect(command.mock.calls[1]![1]?.commandId).toBe(command.mock.calls[0]![1]?.commandId);
  });

  it.each(["summary", "turn into work", "waiting message"])("does not attach observed recovery context to %s", async kind => {
    const { host, task } = await shown();
    const live = { kind: "agent_text_delta" as const, task_id: task.task_id, run_id: cutOffIds.run,
      message_key: "019a6e40-21f0-7000-8000-000000000001", text_offset: 0, text: "Cached interrupted words" };
    host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: cutOffIds.run,
      item: { kind: "agent_message", partial: true, message_key: live.message_key, text_offset: 0, text: "Cached interrupted" } } });
    await tick();
    app.session.state.set(s => interruptLive(applyLive({ ...s, capabilities: { ...s.capabilities!, features: [...s.capabilities!.features, CHAT_REPLY_RECOVERY_FEATURE] } }, live, Date.now())));
    const command = vi.spyOn(app.session, "command").mockRejectedValue(new OutcomeUnknownError("lost-reply"));
    await send(app, { projectId: task.project_id, draftTaskId: task.task_id, text: "Requested words", annotationId: null, label: "Send",
      build: () => kind === "waiting message" ? { command: "send_message", task_id: task.task_id, text: "Requested words", bundle: null, delivery: "next_turn" }
        : { command: "resume_task", task_id: task.task_id, text: "Requested words", ...(kind === "summary" ? { read_only: true } : { mode: "work" }) },
    }, () => {});
    expect(command.mock.calls[0]![0]).not.toHaveProperty("observed_reply");
  });

  it("keeps the typed reply and says why when the computer refuses it", async () => {
    const { host, composer, task } = await shown();
    // Since the phone last heard, the computer found a helper still running.
    host.runs = host.runs.map((r) => (r.run_id === cutOffIds.run ? { ...r, cleanup_pending: true } : r));
    app.ui.saveDraft(task.project_id, cutOffIds.task, { text: "Go on: check the rollup table." });
    view(composer());
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await tick(400);
    expect(screen.getByText(/can’t confirm that everything it started has stopped/)).toBeTruthy();
    expect(screen.getByText(/The last reply was cut off\. Your next message carries on from there\./)).toBeTruthy();
    expect(app.ui.draft(task.project_id, cutOffIds.task)?.text).toBe("Go on: check the rollup table.");
    expect(host.runs.find((r) => r.run_id === cutOffIds.run)!.state).toBe("reconciliation_required");
  });

  it("asks for a reply in Needs you, and only where the computer settles it", async () => {
    await shown();
    expect(taskAttentionReason(app.session.state.get(), app.session.state.get().tasks[cutOffIds.task]!)).toBe("Cut off · Reply to carry on");
    act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== CHAT_CARRY_ON_FEATURE) } })));
    expect(taskAttentionReason(app.session.state.get(), app.session.state.get().tasks[cutOffIds.task]!)).not.toMatch(/Reply to carry on/);
  });

  it("keeps today's notice when the computer can't confirm nothing is running", async () => {
    const { composer } = await shown(false);
    view(composer());
    expect(screen.getByText("Outcome unknown")).toBeTruthy();
    expect(screen.queryByText(/carries on from there/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
  });

  it("keeps today's notice on a computer without the feature", async () => {
    const { composer } = await shown();
    act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== CHAT_CARRY_ON_FEATURE) } })));
    view(composer());
    expect(screen.getByText("Outcome unknown")).toBeTruthy();
    expect(screen.queryByText(/carries on from there/)).toBeNull();
  });
});
