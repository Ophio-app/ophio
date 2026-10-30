// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import { approvals, harnesses, projects, runs, tasks } from "../../dev/sim/scenario";
import { settledText } from "../../design/ApprovalCard";
import { prefs } from "../../app/prefs";
import type { Run, TargetControlView, Task } from "../../protocol/types";
import { preferredComputerAgent } from "../control/computerReview";
import type { HostState } from "../../state/projection";
import { agentChoiceBlocker, agentLabel, composerMode, createTaskCommand, defaultIntent, firstLine, missionFrom, modeSupport, needsReview, resolveAgent, runAgent, sendLabel, writerHold, writerHoldBesideText, writerHoldState, writerHoldText, writerQueueLine, type IntentOption, type WriterHold } from "./intent";

const now = 1_800_000_000_000;
const claude = harnesses.find((h) => h.kind === "claude_code")!;
const codex = harnesses.find((h) => h.kind === "codex")!;
const pi = harnesses.find((h) => h.kind === "pi")!;
const task = (state: Task["state"]): Task => ({ ...tasks(now)[0]!, state });
const run = (over: Partial<Run> = {}): Run => ({ ...runs(now)[0]!, ...over });
const unavailable = (options: IntentOption[]) => Object.fromEntries(options.map((o) => [o.label, o.unavailable]));

describe("writerHold", () => {
  const holding = { ...task("reconciliation_required"), task_id: "t-held", title: "Summarize the README" } as Task;
  const held = run({ run_id: "r-held", task_id: holding.task_id, state: "reconciliation_required" });
  const state = (r: Run[], t: Task[], control: HostState["control"] = {}) => ({ runs: Object.fromEntries(r.map(x => [x.run_id, x])), tasks: Object.fromEntries(t.map(x => [x.task_id, x])), control });

  it("is empty when nothing holds the writer", () => {
    expect(writerHold(null, state([], []))).toBeNull();
  });

  it("finds the holding task and says when its run ended without a known result", () => {
    const hold = writerHold(held.run_id, state([held], [holding]));
    expect(hold).toEqual({ task: holding, unresolved: true, waiting: null, helpers: false });
    expect(writerHoldText(hold!, "api")).toBe("“Summarize the README” ended without a known result, so nothing else can change files in api until you check what happened.");
    expect(writerHoldState(hold!)).toBe("Waiting for you to check “Summarize the README”");
  });

  it("describes a running holder, and an unknown one without inventing a name", () => {
    const working = { ...held, state: "working" as const };
    const hold = writerHold(held.run_id, state([working], [{ ...holding, state: "working", current_run: held.run_id }]))!;
    expect(hold.unresolved).toBe(false);
    expect(hold.waiting).toBeNull();
    expect(writerHoldText(hold, "api")).toBe("“Summarize the README” is changing files in api, so this can’t start until it finishes.");
    expect(writerHoldState(hold)).toBe("After “Summarize the README” finishes");
    const unknown = writerHold(held.run_id, state([], []))!;
    expect(writerHoldText(unknown, "api")).toBe("Another task is changing files in api, so this can’t start until it finishes.");
    expect(writerHoldState(unknown)).toBe("After another task finishes");
  });

  it("says when helpers a finished turn left still hold the project, and Continue waits for them", () => {
    const finished = { ...holding, state: "completed" as const, current_run: held.run_id };
    const hold = writerHold(held.run_id, state([{ ...held, state: "completed", ended_at: now, holds_writer: true }], [finished]))!;
    expect(hold).toMatchObject({ unresolved: false, waiting: null, helpers: true });
    expect(writerHoldText(hold, "api")).toBe("Helpers from “Summarize the README” are still finishing in api, so this can’t start until they end.");
    expect(writerHoldBesideText(hold, "api", "Ask")).toBe("Helpers from “Summarize the README” are still finishing in api. Ask only reads, so it can start now, but it may see those changes half done.");
    expect(writerHoldState(hold)).toBe("After helpers from “Summarize the README” finish");
    const continued = composerMode(finished, null, claude, hold, "api");
    if (continued.mode !== "ended") throw new Error(continued.mode);
    expect(unavailable(continued.options)["Continue this task"]).toBe("Can’t continue yet: Helpers from “Summarize the README” are still finishing in api. Continue once they end, or stop them.");
  });

  it("says when the holder waits for the person's approval, answer or input back", () => {
    const current = { ...holding, state: "awaiting_decision" as const, current_run: held.run_id, target_id: "dp-1" };
    const approval = writerHold(held.run_id, state([{ ...held, state: "awaiting_approval" }], [current]))!;
    expect(approval.waiting).toBe("approval");
    expect(writerHoldText(approval, "api")).toBe("“Summarize the README” is waiting for your approval. It is changing files in api, so this can’t start until it finishes.");
    expect(writerHoldBesideText(approval, "api", "Ask")).toBe("“Summarize the README” is waiting for your approval partway through changing files in api. Ask only reads, so it can start now, but it may see those changes half done.");
    expect(writerHoldState(approval)).toBe("Waiting for your approval on “Summarize the README”");
    const answer = writerHold(held.run_id, state([{ ...held, state: "awaiting_input" }], [current]))!;
    expect(writerHoldState(answer)).toBe("Waiting for your answer to “Summarize the README”");
    const paused = { input_state: "paused", private_step: null, lease: { owner: { kind: "agent", run_id: held.run_id } } } as unknown as TargetControlView;
    const back = writerHold(held.run_id, state([{ ...held, state: "working", ended_at: null }], [current], { "dp-1": paused }))!;
    expect(back.waiting).toBe("input_back");
    expect(writerHoldText(back, "api")).toBe("“Summarize the README” is waiting for you to resume it. It is changing files in api, so this can’t start until it finishes.");
    expect(writerHoldState(back)).toBe("Waiting for you to resume “Summarize the README”");
  });
});

describe("composerMode", () => {
  it("offers a new task when nothing is on screen", () => {
    expect(composerMode(null, null, claude)).toEqual({ mode: "new" });
  });

  it("says what a conversation's reply may do from its access, not its mode", () => {
    const chat = { ...task("completed"), mode: "ask", conversation: true } as Task;
    const reply = (t: Task) => { const m = composerMode(t, null, claude); return m.mode === "ended" ? m.options[0]!.detail : ""; };
    expect(reply(chat)).toContain("reads but changes nothing");
    const open = { read: [], write: [chat.project_id], whole_read: false, whole_write: false, internet: true, local_network: false };
    expect(reply({ ...chat, access: open })).toContain("may change files where you’ve let it");
    expect(reply({ ...chat, access: open })).not.toContain("changes nothing");
    expect(reply({ ...chat, access: { ...open, write: [] } })).toContain("reads but changes nothing");
  });

  it("lets a steering-capable agent take messages mid-turn", () => {
    const m = composerMode(task("working"), run({ state: "working" }), claude);
    expect(m.mode).toBe("running");
    if (m.mode !== "running") return;
    expect(unavailable(m.options)["Steer now"]).toBeNull();
    expect(defaultIntent(m)).toEqual({ kind: "message", delivery: "steer" });
  });

  it("keeps steering visible but unavailable, with the agent's reason, when the harness cannot steer", () => {
    const m = composerMode(task("working"), run({ state: "working" }), codex);
    if (m.mode !== "running") throw new Error(m.mode);
    expect(unavailable(m.options)["Steer now"]).toBe("Codex accepts new text at the next turn boundary");
    expect(defaultIntent(m)).toEqual({ kind: "message", delivery: "next_turn" });
  });

  it("says a new task from a Plan starts in Work, unless it is a stranger's work", () => {
    const detail = (t: Task, label: string) => {
      const m = composerMode(t, run({ state: "working" }), claude);
      if (m.mode !== "running" && m.mode !== "ended") throw new Error(m.mode);
      return m.options.find((o) => o.label === label)?.detail;
    };
    const plan = { ...task("working"), mode: "plan" as const };
    expect(detail(plan, "Add to Next up")).toBe("Saved as a new task that waits its turn, in Work mode so it can change files. This task stays in Plan.");
    expect(detail({ ...plan, state: "completed" }, "Start a new task")).toBe("A separate task in this project, in Work mode so it can change files. This one stays in Plan.");
    const stranger = { ...plan, github: { ...plan.github!, from_stranger: true } };
    expect(detail(stranger, "Add to Next up")).toBe("Saved as a new task that waits its turn. This task isn’t changed.");
    expect(detail(task("working"), "Add to Next up")).toBe("Saved as a new task that waits its turn. This task isn’t changed.");
  });

  it("does not steer past a waiting approval", () => {
    const m = composerMode(task("awaiting_decision"), run({ state: "awaiting_approval" }), claude);
    if (m.mode !== "running") throw new Error(m.mode);
    expect(unavailable(m.options)["Steer now"]).toMatch(/Answer the waiting request/);
  });

  it("offers Continue for an ended task unless another writer holds the project", () => {
    const free = composerMode(task("completed"), null, claude);
    if (free.mode !== "ended") throw new Error(free.mode);
    expect(defaultIntent(free)).toEqual({ kind: "continue" });

    const holder: WriterHold = { task: { ...task("working"), title: "Fix the flaky test" }, unresolved: false, waiting: null, helpers: false };
    const busy = composerMode(task("completed"), null, claude, holder, "api");
    if (busy.mode !== "ended") throw new Error(busy.mode);
    expect(unavailable(busy.options)["Continue this task"]).toBe("Can’t continue yet: “Fix the flaky test” is changing files in api. Put your message in Next up to continue when it finishes.");
    // So the message waits in Next up and continues this task after.
    expect(defaultIntent(busy)).toEqual({ kind: "continue_later" });
    expect(busy.options.find(o => o.intent.kind === "continue_later")?.detail)
      .toBe("Waits in Next up, then continues this task with your message. After “Fix the flaky test” finishes.");
  });

  it("names a task that ended without a known result as the one holding the project", () => {
    const holder: WriterHold = { task: { ...task("reconciliation_required"), title: "Summarize the README" }, unresolved: true, waiting: null, helpers: false };
    const m = composerMode(task("completed"), null, claude, holder, "api");
    if (m.mode !== "ended") throw new Error(m.mode);
    expect(unavailable(m.options)["Continue this task"]).toMatch(/^Can’t continue yet: “Summarize the README” ended without a known result, so nothing else can change files in api until you check what happened\. Check it first/);
  });

  it("lets a read-only task continue beside another task's writer", () => {
    const ask: Task = { ...task("completed"), mode: "ask" };
    const m = composerMode(ask, null, claude, { task: null, unresolved: false, waiting: null, helpers: false });
    if (m.mode !== "ended") throw new Error(m.mode);
    expect(unavailable(m.options)["Continue this task"]).toBeNull();
    expect(defaultIntent(m)).toEqual({ kind: "continue" });
  });

  it("says when continuing starts a fresh session", () => {
    const m = composerMode(task("failed"), null, pi);
    if (m.mode !== "ended") throw new Error(m.mode);
    expect(m.options[0]!.detail).toMatch(/fresh session/);
    expect(m.options[0]!.unavailable).toBe("Open Pi in a terminal");
  });

  it("maps the remaining states to their own modes", () => {
    expect(composerMode(task("accepted"), null, claude).mode).toBe("waiting");
    expect(composerMode(task("dispatching"), null, claude).mode).toBe("starting");
    expect(composerMode(task("reconciliation_required"), null, claude).mode).toBe("reconcile");
    expect(composerMode(task("archived"), null, claude).mode).toBe("archived");
  });
});

describe("review and labels", () => {
  it("reviews anything that creates a task or carries markup", () => {
    expect(needsReview({ kind: "new_task" }, false)).toBe(true);
    expect(needsReview({ kind: "next_up" }, false)).toBe(true);
    expect(needsReview({ kind: "message", delivery: "steer" }, false)).toBe(false);
    expect(needsReview({ kind: "message", delivery: "steer" }, true)).toBe(true);
    expect(sendLabel({ kind: "continue" }, false)).toBe("Continue");
    expect(sendLabel({ kind: "message", delivery: "next_turn" }, false)).toBe("Send");
    expect(sendLabel({ kind: "continue" }, true)).toBe("Review");
    expect(needsReview({ kind: "continue_later" }, false)).toBe(false);
    expect(sendLabel({ kind: "continue_later" }, false)).toBe("Put in Next up");
    expect(sendLabel({ kind: "continue_later" }, true)).toBe("Review");
  });
});

describe("agent choice", () => {
  const project = projects(now)[0]!;

  it("keeps an unavailable selection visible but blocks starting it", () => {
    const chosen = { harness: "pi" as const, model: null, effort: null, launchPolicy: null };
    expect(resolveAgent(project, harnesses, chosen)).toEqual(chosen);
    expect(agentChoiceBlocker(pi, chosen)).toBe("Open Pi in a terminal");
    expect(resolveAgent({ ...project, default_harness: null }, [pi], null)).toBeNull();
  });

  it("starts new work and computer use with the Settings default, after a project's own", () => {
    const free = { ...project, default_harness: null };
    try {
      expect(resolveAgent(free, harnesses, null)?.harness).toBe("claude_code");
      prefs.set((p) => ({ ...p, defaultAgent: "codex" }));
      expect(resolveAgent(free, harnesses, null)?.harness).toBe("codex");
      expect(preferredComputerAgent(harnesses, free)?.kind).toBe("codex");
      // A project's own default still wins, for both.
      expect(resolveAgent({ ...project, default_harness: "claude_code" }, harnesses, null)?.harness).toBe("claude_code");
      expect(preferredComputerAgent(harnesses, { ...project, default_harness: "claude_code" })?.kind).toBe("claude_code");
      prefs.set((p) => ({ ...p, defaultAgent: "claude_code" }));
      expect(preferredComputerAgent(harnesses, free)?.kind).toBe("claude_code");
      // A default that can't start falls back to the first ready agent.
      prefs.set((p) => ({ ...p, defaultAgent: "pi" }));
      expect(resolveAgent(free, harnesses, null)?.harness).toBe("claude_code");
    } finally {
      prefs.set((p) => ({ ...p, defaultAgent: null }));
    }
  });

  it("keeps an unlisted model and effort instead of silently using defaults", () => {
    const chosen = { harness: "codex" as const, model: "gpt-0", effort: "ludicrous", launchPolicy: "bypass_permissions" as const };
    expect(resolveAgent(project, harnesses, chosen)).toEqual(chosen);
    expect(agentChoiceBlocker(codex, chosen)).toMatch(/selected model.*unavailable/);
  });

  it("keeps valid choices and names defaults", () => {
    expect(resolveAgent(project, harnesses, { harness: "claude_code", model: "claude-sonnet-5", effort: "high" })).toEqual({
      harness: "claude_code",
      model: "claude-sonnet-5",
      effort: "high",
      launchPolicy: "bypass_permissions",
    });
    expect(agentLabel(harnesses, { harness: "claude_code", model: null, effort: "xhigh" })).toBe("Claude Code · Opus 5.5 · Extra high");
  });

  it("reports what actually ran and what was asked for", () => {
    const r = run({ harness: "claude_code", requested_model: "claude-opus-5-5", actual_model: "claude-sonnet-5", requested_effort: "high", actual_effort: "high" });
    const a = runAgent(harnesses, r);
    expect(a.label).toBe("Claude Code · Sonnet 5 · High");
    expect(a.differs).toBe("You asked for Claude Opus 5.5");
  });

  it.each<[string, Run["harness"], string, string, string | null]>([
    ["an alias that resolved", "claude_code", "opus", "claude-opus-5-5", null],
    ["an alias with a dated, wider-context model", "claude_code", "sonnet[1m]", "claude-sonnet-4-5-20250929[1m]", null],
    ["the default", "claude_code", "default", "claude-sonnet-5", null],
    ["the best model", "claude_code", "best", "claude-fable-5-1", null],
    ["Opus for plans, Sonnet otherwise", "claude_code", "opusplan", "claude-sonnet-5", null],
    ["an exact id that matched", "claude_code", "claude-opus-5-5", "claude-opus-5-5", null],
    ["an exact id reported with its date", "claude_code", "claude-opus-4-1", "claude-opus-4-1-20250805", null],
    ["an alias that fell back", "claude_code", "opus", "claude-sonnet-5", "You asked for Claude Opus (latest)"],
    ["opusplan that ran neither", "claude_code", "opusplan", "claude-haiku-4-5", "You asked for opusplan"],
    ["another version of the named model", "claude_code", "claude-opus-5-5", "claude-opus-5-1", "You asked for Claude Opus 5.5"],
    ["another snapshot of a dated id", "claude_code", "claude-3-5-sonnet-20241022", "claude-3-5-sonnet-20240620", "You asked for Claude 3.5 Sonnet"],
    ["a dated id reported without its date", "claude_code", "claude-3-5-sonnet-20241022", "claude-3-5-sonnet", "You asked for Claude 3.5 Sonnet"],
    ["another Codex model", "codex", "gpt-6-astra", "gpt-6-sol", "You asked for GPT-6-Astra"],
  ])("flags a model only when it is not what was asked: %s", (_, harness, requested_model, actual_model, differs) => {
    expect(runAgent(harnesses, run({ harness, requested_model, actual_model })).differs).toBe(differs);
  });
});

describe("text helpers", () => {
  it("builds a mission only from a stopping point, never enforced", () => {
    expect(missionFrom("Fix it", "  ")).toBeNull();
    expect(missionFrom("Fix the flake\nmore", "Stop before pushing")).toEqual({ goal: "Fix the flake", bounds: [], completion_checks: [], stop_point: "Stop before pushing" });
  });

  it("titles from the first line", () => {
    expect(firstLine("  one\ntwo")).toBe("one");
    expect(firstLine("x".repeat(100), 10)).toBe(`${"x".repeat(9)}…`);
    expect(firstLine("A follow-up accepted just before a task stops disappears from the conversation", 48)).toBe("A follow-up accepted just before a task stops…");
    // Beside the composer, the holder is short enough that the line keeps its ending.
    const long: WriterHold = { task: { ...task("working"), title: "A follow-up accepted just before a task stops disappears from the conversation" }, unresolved: false, waiting: null, helpers: false };
    expect(writerQueueLine(long)).toBe("Waits in Next up until “A follow-up accepted just…” finishes");
  });
});

describe("task modes", () => {
  const claude = harnesses.find((h) => h.kind === "claude_code")!;
  const codex = harnesses.find((h) => h.kind === "codex")!;

  it("uses what the harness declared, with its enforcement", () => {
    const ask = modeSupport(claude, "ask");
    expect(ask.availability.state).toBe("available");
    expect(ask.enforcement).toBeTruthy();
    expect(modeSupport(codex, "plan").availability.state).toBe("unsupported");
  });

  it("treats a harness that declares nothing as Work-only", () => {
    const bare = { ...claude, modes: undefined };
    expect(modeSupport(bare, "work")).toEqual({ availability: { state: "available" }, enforcement: null });
    const plan = modeSupport(bare, "plan");
    expect(plan.availability.state).toBe("unsupported");
    expect(plan.enforcement).toBeNull();
    expect(modeSupport(undefined, "ask").availability).toMatchObject({ state: "unsupported", reason: expect.stringContaining("This agent") });
  });

  it("sends the chosen mode with a new task", () => {
    const cmd = createTaskCommand({
      projectId: "p",
      prompt: "  What does retry do?  ",
      title: "  ",
      agent: { harness: "claude_code", model: null, effort: null },
      mode: "ask",
      placement: { mode: "start_now" },
      mission: null,
      bundle: null,
    });
    expect(cmd).toMatchObject({ command: "create_task", mode: "ask", prompt: "What does retry do?", title: null, harness: "claude_code" });
  });
});

describe("settled approvals", () => {
  const base = approvals(0)[0]!;
  const me = "dev-me";

  it("shows the recorded decision without claiming its effect", () => {
    const a = { ...base, state: "allowed" as const, resolved_by: { kind: "device" as const, device_id: me } };
    expect(settledText(a, me)).toEqual({ title: "Allowed", meta: "Allowed by you. Sent to the agent" });
  });

  it("names the reported outcome, and a mismatch with what was sent", () => {
    const ok = { ...base, state: "denied" as const, outcome: "denied" as const, resolved_by: null };
    expect(settledText(ok, me)).toEqual({ title: "Denied", meta: "Denied" });
    const odd = { ...base, state: "allowed" as const, outcome: "denied" as const, resolved_by: null };
    expect(settledText(odd, me)).toEqual({ title: "Denied", meta: "Allowed, but the agent reported it denied" });
  });
});
