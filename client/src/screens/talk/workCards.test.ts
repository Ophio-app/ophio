// SPDX-License-Identifier: Apache-2.0
//
// A suggestion's card says exactly what its Start sends: the kind of task,
// its route, what it may reach, the agent, model and effort on the chip, and
// anything past the project, which is always written out.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { harnesses as simHarnesses, projects as simProjects } from "../../dev/sim/scenario";
import { prefs } from "../../app/prefs";
import { rememberUsual, usualAgent } from "../../app/launchMemory";
import type { Command, GithubProjectView, HarnessInfo, NewWork, Project } from "../../protocol/types";
import { agentLabel } from "./intent";
import { cardKind, kindMode, leastAccess, planCard, places, WORK_CARDS_FEATURE, type CardInput, type CardPlan } from "./workCards";

const efforts = [{ id: "low", label: "Low" }, { id: "medium", label: "Medium" }, { id: "high", label: "High" }];
const harnesses: HarnessInfo[] = simHarnesses.map((h) => ({ ...h, models: h.models.map((m) => ({ ...m, details: { alias: false, efforts, default_effort: null } })) }));
const now = 1_800_000_000_000;
const all = simProjects(now);
const chat: Project = { ...all.find((p) => p.name === "field-notes")!, active_writer: null, default_harness: null, default_model: null, default_effort: null };
const tide = all.find((p) => p.name === "tidewater-api")!;
const garden = all.find((p) => p.name === "garden-planner")!;
const projects = Object.fromEntries([chat, tide, garden].map((p) => [p.project_id, p]));
const P = "profile-a";
const FEATURES = [WORK_CARDS_FEATURE, "launch_policy_v1"];
const github = (over: Partial<GithubProjectView["settings"]> = {}): GithubProjectView =>
  ({ settings: { mode: "drafts", warning_answered: true, workflow: { review_with: null }, ...over } }) as unknown as GithubProjectView;

const work = (over: Partial<NewWork>): NewWork => ({ title: "Add a retry", brief: "Retry the upload twice.", mode: "work", ...over });
const input = (w: NewWork, over: Partial<CardInput> = {}): CardInput => {
  const agent = usualAgent(P, chat, cardKind(w), harnesses)?.agent ?? null;
  return {
    w, index: 0, chatId: "chat-1", runId: "run-1", chat, projects, habits: agent ? { agent, pr: true } : null, harnesses,
    github: github(), reviewer: null, writer: null, paused: false, features: FEATURES, canStart: true, signInOn: null, ...over,
  };
};
const sent = (plan: CardPlan) => plan.start as Extract<Command, { command: "create_task" }>;

beforeEach(() => {
  localStorage.clear();
  prefs.set((p) => ({ ...p, defaultAgent: "claude_code", claudeBypassPermissions: false, codexBypassPermissions: false }));
});

describe("what a suggestion starts as", () => {
  it("starts a question read-only, with exactly what it names to reach", () => {
    const plan = planCard(input(work({ title: "Why is the build slow?", mode: "ask", needs: { read: ["tidewater-api"], changes: null, internet: true, local_network: false, whole_computer: false, desktop: false } })));
    expect(plan.line).toBe("Find out · read-only · starts now");
    expect(plan.beyond).toEqual(["reads tidewater-api"]);
    const c = sent(plan);
    expect(c.mode).toBe("ask");
    expect(c.access).toEqual({ read: [chat.project_id, tide.project_id], write: [], whole_read: false, whole_write: false, internet: true, local_network: false });
    expect(c.github ?? null).toBeNull();
    expect(c.origin).toEqual({ chat: "chat-1", run: "run-1", index: 0 });
  });

  it("says when a question stays in its project and whether it uses the internet", () => {
    expect(planCard(input(work({ mode: "ask" }))).line).toBe("Find out · read-only · this project only, no internet · starts now");
    const online = work({ mode: "ask", needs: { read: [], changes: null, internet: true, local_network: false, whole_computer: false, desktop: false } });
    expect(planCard(input(online)).line).toBe("Find out · read-only · this project only, uses the internet · starts now");
    expect(sent(planCard(input(online))).access!.internet).toBe(true);
    expect(sent(planCard(input(work({ mode: "ask" })))).access!.internet).toBe(false);
  });

  it("says a question that reaches your own network uses the internet too, as Start grants both", () => {
    const lan = work({ mode: "ask", needs: { read: [], changes: null, internet: false, local_network: true, whole_computer: false, desktop: false } });
    const plan = planCard(input(lan));
    expect(plan.line).toBe("Find out · read-only · this project only, uses the internet · starts now");
    expect(plan.beyond).toEqual(["reaches your own network"]);
    expect(sent(plan).access).toMatchObject({ internet: true, local_network: true });
  });

  it("always writes out the whole computer and the person's own network", () => {
    const wide = work({ mode: "ask", needs: { read: [], changes: null, internet: false, local_network: true, whole_computer: true, desktop: false } });
    const plan = planCard(input(wide));
    expect(plan.beyond).toEqual(["reads the whole computer", "reaches your own network"]);
    // The own network is reached through the internet, as the computer holds it.
    expect(sent(plan).access).toMatchObject({ whole_read: true, whole_write: false, internet: true, local_network: true, write: [] });
  });

  it("plans read-only, held to its mode, with no access of its own", () => {
    const plan = planCard(input(work({ mode: "plan" })));
    expect(plan.line).toBe("Plan · read-only, then you decide · this project · starts now");
    expect(sent(plan).mode).toBe("plan");
    expect(sent(plan).access).toBeUndefined();
    expect(sent(plan).github ?? null).toBeNull();
  });

  it("makes a change on its own branch and pull request where GitHub is on", () => {
    const plan = planCard(input(work({})));
    expect(plan.line).toBe("Change · own branch and pull request · asks before risky steps · starts now");
    expect(sent(plan)).toMatchObject({ mode: "work", github: { type: "new_pr" }, prompt: "Retry the upload twice.", title: "Add a retry" });
    expect(sent(plan).access).toBeUndefined();
  });

  it("changes the folder when GitHub is off or the person turned pull requests off", () => {
    for (const over of [{ github: null }, { habits: { ...input(work({})).habits!, pr: false } }]) {
      const plan = planCard(input(work({}), over));
      expect(plan.line).toBe("Change · changes the project folder · asks before risky steps · starts now");
      expect(sent(plan).github ?? null).toBeNull();
    }
  });

  it("says a change waits for the project's writer, and queues it", () => {
    const plan = planCard(input(work({}), { writer: { title: "Fix the flaky test" } }));
    expect(plan.line).toBe("Change · own branch and pull request · asks before risky steps · starts after “Fix the flaky test”");
    expect(sent(plan).placement).toEqual({ mode: "queue", position: null, authorization: "auto_start" });
    expect(plan.queued).toBe(true);
    // A question reads beside the writer, so it starts now.
    expect(sent(planCard(input(work({ mode: "ask" }), { writer: { title: "x" } }))).placement).toEqual({ mode: "start_now" });
  });

  it("never resumes a paused Next up: the task waits there for the person's own Start", () => {
    // Starting by itself from Next up resumes all of it on the computer.
    const plan = planCard(input(work({}), { writer: { title: "Fix the flaky test" }, paused: true }));
    expect(sent(plan).placement).toEqual({ mode: "queue", position: null, authorization: "ask_before_start" });
    expect(plan.line).toBe("Change · own branch and pull request · asks before risky steps · waits in Next up, which is paused, until you start it");
    // A pause matters only to what would wait in Next up.
    expect(sent(planCard(input(work({}), { paused: true }))).placement).toEqual({ mode: "start_now" });
  });

  it("finds a project by name the same in every locale, as the computer does", () => {
    // A Turkish phone lowercases a capital I to a dotless ı.
    const turkish = vi.spyOn(String.prototype, "toLocaleLowerCase").mockImplementation(function (this: string) {
      return this.replace(/I/g, "ı").toLowerCase();
    });
    try {
      const idx = { ...garden, project_id: "p-index", name: "index" };
      const found = places(work({ needs: { read: ["INDEX"], changes: null, internet: false, local_network: false, whole_computer: false, desktop: false } }), chat, { ...projects, [idx.project_id]: idx });
      expect(found.unknown).toEqual([]);
      expect(found.read.map((p) => p.name)).toEqual(["index"]);
    } finally {
      turkish.mockRestore();
    }
  });

  it("fixes an issue through the issue route, with the project's reviewer", () => {
    const reviewer = { harness: "claude_code" as const, model: "claude-opus-5-5", effort: "high" };
    const plan = planCard(input(work({ issue: 42 }), { reviewer }));
    expect(plan.line).toBe("Fix issue #42 · own branch and pull request, reviewed · asks before risky steps · starts now");
    expect(sent(plan).github).toEqual({ type: "issue", number: 42, review_with: reviewer });
    const planned = planCard(input(work({ issue: 42, mode: "plan" })));
    expect(planned.line).toBe("Plan a fix for issue #42 · read-only, then you decide · this project · starts now");
    expect(sent(planned).mode).toBe("plan");
  });

  it("marks starting without permission prompts when the person doesn't usually", () => {
    const w = work({});
    const i = input(w);
    const plan = planCard({ ...i, habits: { agent: { ...i.habits!.agent, launchPolicy: "bypass_permissions" }, pr: true } });
    expect(plan.line).toContain("no permission prompts");
    expect(plan.beyond).toEqual(["no permission prompts"]);
  });
});

describe("what goes through Review instead", () => {
  it("never starts work in another project or on the desktop with one tap", () => {
    const other = planCard(input(work({ needs: { read: [], changes: "tidewater-api", internet: false, local_network: false, whole_computer: false, desktop: false } })));
    expect(other.start).toBeNull();
    expect(other.beyond).toEqual(["works in tidewater-api"]);
    const desk = planCard(input(work({ needs: { read: [], changes: null, internet: false, local_network: false, whole_computer: false, desktop: true } })));
    expect(desk.kind).toBe("desktop");
    expect(desk.start).toBeNull();
    expect(desk.beyond).toEqual(["uses the desktop"]);
  });

  it("refuses what names a project this computer doesn't have, or what the computer refused", () => {
    const unknown = planCard(input(work({ mode: "ask", needs: { read: ["nowhere"], changes: null, internet: false, local_network: false, whole_computer: false, desktop: false } })));
    expect(unknown.refused).toBe("It names a project this computer doesn’t have: nowhere.");
    expect(unknown.start).toBeNull();
    const refused = planCard(input(work({ refused: "A question can’t fix an issue." })));
    expect(refused.refused).toBe("A question can’t fix an issue.");
  });

  it("opens Review on an older computer, offline, without an agent, or before a merge warning is answered", () => {
    expect(planCard(input(work({}), { features: ["launch_policy_v1"] })).review).toBe("This computer’s Øphio is older, so Start opens Review.");
    expect(planCard(input(work({}), { canStart: false })).start).toBeNull();
    expect(planCard(input(work({}), { habits: null })).review).toBe("Choose an agent first.");
    expect(planCard(input(work({}), { github: undefined })).review).toBe("Checking the project’s GitHub setting…");
    expect(planCard(input(work({}), { github: github({ mode: "through_merge", warning_answered: false }) })).start).toBeNull();
    expect(planCard(input(work({ issue: 3 }), { github: null })).review).toBe("Turn on GitHub for this project to fix the issue there.");
  });
});

describe("the agent chip", () => {
  it("shows the usual for that kind, and Start sends exactly that", () => {
    rememberUsual(P, chat.project_id, "find", { harness: "claude_code", model: "claude-haiku-4-5", effort: "low", launchPolicy: "standard" });
    rememberUsual(P, chat.project_id, "change", { harness: "claude_code", model: "claude-opus-5-5", effort: "high", launchPolicy: "accept_edits" });
    const find = planCard(input(work({ mode: "ask" })));
    expect(find.chip).toBe("Claude Code · Haiku 4.5 · Low");
    expect(sent(find)).toMatchObject({ harness: "claude_code", model: "claude-haiku-4-5", effort: "low", agent_chosen: true });
    const change = planCard(input(work({})));
    expect(change.chip).toBe("Claude Code · Opus 5.5 · High");
    expect(sent(change)).toMatchObject({ harness: "claude_code", model: "claude-opus-5-5", effort: "high", launch_policy: "accept_edits", agent_chosen: true });
    expect(change.line).toContain("edits without asking");
    // A plan, never chosen for, is the defaults: the app's agent at the highest effort.
    const plan = planCard(input(work({ mode: "plan" })));
    expect(plan.chip).toBe("Claude Code · Opus 5.5 · High");
    expect(sent(plan)).toMatchObject({ harness: "claude_code", model: null, effort: "high" });
  });

  it("matches what Start sends for every kind and pick", () => {
    const picks = [
      { harness: "claude_code" as const, model: "claude-sonnet-5", effort: "medium", launchPolicy: "standard" as const },
      { harness: "claude_code" as const, model: null, effort: null, launchPolicy: "accept_edits" as const },
    ];
    for (const w of [work({ mode: "ask" }), work({ mode: "plan" }), work({}), work({ issue: 9 })]) for (const agent of picks) {
      const plan = planCard({ ...input(w), habits: { agent, pr: true } });
      const c = sent(plan);
      expect(plan.chip).toBe(agentLabel(harnesses, { harness: c.harness, model: c.model, effort: c.effort }));
      expect([c.harness, c.model, c.effort]).toEqual([agent.harness, agent.model, agent.effort]);
    }
    // Codex can't hold a question or a plan read-only here, so those open Review rather than start otherwise.
    const codex = { harness: "codex" as const, model: "gpt-6-mini", effort: "medium", launchPolicy: "standard" as const };
    expect(planCard({ ...input(work({ mode: "ask" })), habits: { agent: codex, pr: true } }).start).toBeNull();
    expect(sent(planCard({ ...input(work({})), habits: { agent: codex, pr: true } }))).toMatchObject({ harness: "codex", model: "gpt-6-mini", effort: "medium" });
  });

  it("changes only the agent, never the kind of task or what it may reach", () => {
    const loose = [
      { harness: "codex" as const, model: "gpt-6-mini", effort: "low", launchPolicy: "bypass_permissions" as const },
      { harness: "claude_code" as const, model: "claude-haiku-4-5", effort: "low", launchPolicy: "bypass_permissions" as const },
    ];
    let compared = 0;
    const ask = work({ mode: "ask", needs: { read: [], changes: null, internet: true, local_network: false, whole_computer: false, desktop: false } });
    for (const w of [ask, work({ mode: "plan" }), work({}), work({ issue: 9 })]) for (const agent of loose) {
      const plain = sent(planCard(input(w)));
      const usual = planCard({ ...input(w), habits: { agent, pr: true } });
      if (!usual.start) continue;
      const c = sent(usual);
      expect([c.kind, c.mode, c.title, c.prompt, c.access, c.github, c.project_id]).toEqual([plain.kind, plain.mode, plain.title, plain.prompt, plain.access, plain.github, plain.project_id]);
      compared++;
    }
    expect(compared).toBeGreaterThanOrEqual(6);
  });

  it("can't be set by what the agent wrote in its suggestion", () => {
    // Whatever else a report carries, only the person's usual or pick reaches the command.
    const sneaky = { ...work({}), harness: "codex", model: "gpt-6-astra", effort: "xhigh", agent: { harness: "codex" }, launch_policy: "bypass_permissions", access: { whole_write: true }, approved: true } as unknown as NewWork;
    const c = sent(planCard(input(sneaky)));
    expect(c).toMatchObject({ harness: "claude_code", model: null, effort: "high", launch_policy: "standard" });
    expect(c.access).toBeUndefined();
    expect(usualAgent(P, chat, "change", harnesses)!.from).toBe("default");
  });
});

describe("kinds and places", () => {
  it("reads kinds and modes from the work", () => {
    expect(cardKind(work({ mode: "ask" }))).toBe("find");
    expect(cardKind(work({ issue: 1, mode: "plan" }))).toBe("fix");
    expect(kindMode("fix", { mode: "plan" })).toBe("plan");
    expect(kindMode("fix", { mode: "work" })).toBe("work");
  });

  it("names a project only when exactly one matches, never a one-off", () => {
    const twin = { ...tide, project_id: "twin", name: "Tidewater-API" };
    const oneOff = { ...garden, project_id: "once", name: "scratch", one_off: true };
    const p = places(work({ mode: "ask", needs: { read: ["tidewater-api", "scratch", "garden-planner", "GARDEN-PLANNER"], changes: null, internet: false, local_network: false, whole_computer: false, desktop: false } }), chat, { ...projects, twin, once: oneOff });
    expect(p.read.map((r) => r.name)).toEqual(["garden-planner"]);
    expect(p.unknown).toEqual(["tidewater-api", "scratch"]);
    expect(leastAccess(work({ mode: "work" }), p)).toBeNull();
  });
});
