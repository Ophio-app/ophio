// SPDX-License-Identifier: Apache-2.0

import { beforeEach, describe, expect, it } from "vitest";
import { harnesses as simHarnesses, projects } from "../dev/sim/scenario";
import type { HarnessInfo, Project } from "../protocol/types";
import { asOffered, kindOf, remembered, rememberPr, rememberedPr, rememberUsual, usualAgent } from "./launchMemory";
import { prefs } from "./prefs";

const efforts = [{ id: "low", label: "Low" }, { id: "medium", label: "Medium" }, { id: "high", label: "High" }, { id: "xhigh", label: "Extra high" }];
/** The sim's agents, with the efforts each model offers. */
const harnesses: HarnessInfo[] = simHarnesses.map((h) => ({
  ...h,
  models: h.models.map((m) => ({ ...m, details: { alias: false, efforts, default_effort: "medium" } })),
}));
const now = 1_800_000_000_000;
const [tide, notes] = projects(now) as [Project, Project, ...Project[]];
const bare: Project = { ...notes, default_harness: null, default_model: null, default_effort: null };
const P = "profile-a";

beforeEach(() => {
  localStorage.clear();
  prefs.set((p) => ({ ...p, defaultAgent: "codex", claudeBypassPermissions: false, codexBypassPermissions: false }));
});

describe("the usual agent for each kind of task", () => {
  it("says which kind a task made by hand is", () => {
    expect(kindOf("ask")).toBe("find");
    expect(kindOf("plan")).toBe("plan");
    expect(kindOf("work")).toBe("change");
    expect(kindOf(undefined)).toBe("change");
    expect(kindOf("work", { issue: true })).toBe("fix");
    expect(kindOf("plan", { issue: true })).toBe("fix");
    expect(kindOf("work", { desktop: true })).toBe("desktop");
  });

  it("keeps a choice for one kind out of every other kind", () => {
    rememberUsual(P, tide.project_id, "find", { harness: "claude_code", model: "claude-haiku-4-5", effort: "low", launchPolicy: "standard" });
    expect(usualAgent(P, tide, "find", harnesses)).toEqual({ agent: { harness: "claude_code", model: "claude-haiku-4-5", effort: "low", launchPolicy: "standard" }, from: "project" });
    for (const kind of ["plan", "change", "fix", "desktop"] as const) {
      expect(remembered(P, tide.project_id, kind)).toBeNull();
      expect(usualAgent(P, tide, kind, harnesses)!.agent.model).not.toBe("claude-haiku-4-5");
    }
  });

  it("looks in this project, then any project, then the defaults at the highest effort", () => {
    // Nothing remembered: the project's own default, with its own effort.
    expect(usualAgent(P, tide, "change", harnesses)).toEqual({ agent: { harness: "claude_code", model: "claude-opus-5-5", effort: "high", launchPolicy: "standard" }, from: "default" });
    // A project with no default: the app's default agent, at the highest effort its model offers.
    expect(usualAgent(P, bare, "change", harnesses)).toEqual({ agent: { harness: "codex", model: null, effort: "xhigh", launchPolicy: "standard" }, from: "default" });

    // Codex can't hold a plan read-only here, so a plan's default is an agent that can.
    expect(usualAgent(P, bare, "plan", harnesses)).toMatchObject({ agent: { harness: "claude_code", model: null }, from: "default" });

    // Chosen for a change in another project: used here too.
    rememberUsual(P, tide.project_id, "change", { harness: "codex", model: "gpt-6-mini", effort: "medium", launchPolicy: "standard" });
    expect(usualAgent(P, bare, "change", harnesses)).toEqual({ agent: { harness: "codex", model: "gpt-6-mini", effort: "medium", launchPolicy: "standard" }, from: "any" });

    // Chosen for a change in this project: wins over the last choice anywhere.
    rememberUsual(P, bare.project_id, "change", { harness: "claude_code", model: "claude-sonnet-5", effort: "high", launchPolicy: "accept_edits" });
    rememberUsual(P, tide.project_id, "change", { harness: "codex", model: "gpt-6-astra", effort: "low", launchPolicy: "standard" });
    expect(usualAgent(P, bare, "change", harnesses)).toEqual({ agent: { harness: "claude_code", model: "claude-sonnet-5", effort: "high", launchPolicy: "accept_edits" }, from: "project" });
  });

  it("runs as this computer offers it when a remembered model or effort is gone, and keeps what was remembered", () => {
    rememberUsual(P, tide.project_id, "plan", { harness: "codex", model: "gpt-5-retired", effort: "high", launchPolicy: "standard" });
    expect(usualAgent(P, tide, "plan", harnesses)).toEqual({ agent: { harness: "codex", model: null, effort: null, launchPolicy: "standard" }, from: "project" });
    expect(remembered(P, tide.project_id, "plan")!.agent.model).toBe("gpt-5-retired");

    rememberUsual(P, tide.project_id, "find", { harness: "codex", model: "gpt-6-mini", effort: "ultra", launchPolicy: "standard" });
    expect(usualAgent(P, tide, "find", harnesses)!.agent).toEqual({ harness: "codex", model: "gpt-6-mini", effort: null, launchPolicy: "standard" });
    expect(remembered(P, tide.project_id, "find")!.agent.effort).toBe("ultra");

    // An agent this computer can't start falls back to the defaults.
    const noCodex = harnesses.filter((h) => h.kind !== "codex");
    expect(usualAgent(P, tide, "find", noCodex)).toMatchObject({ agent: { harness: "claude_code" }, from: "default" });
  });

  it("checks a remembered permission setting against the agent", () => {
    expect(asOffered({ harness: "codex", model: null, effort: null, launchPolicy: "accept_edits" }, harnesses)!.launchPolicy).toBe("standard");
    expect(asOffered({ harness: "claude_code", model: null, effort: null }, harnesses)!.launchPolicy).toBe("standard");
  });

  it("ignores what can't be read as a choice", () => {
    localStorage.setItem(`launch.usual.${P}`, JSON.stringify({ projects: { [tide.project_id]: { kinds: { change: { model: "x" }, find: "codex" } } }, any: { plan: 7 } }));
    expect(remembered(P, tide.project_id, "change")).toBeNull();
    expect(remembered(P, tide.project_id, "find")).toBeNull();
    expect(remembered(P, tide.project_id, "plan")).toBeNull();
    localStorage.setItem(`launch.usual.${P}`, "not json");
    expect(usualAgent(P, tide, "change", harnesses)!.from).toBe("default");
  });

  it("remembers the pull request choice per project, on by default", () => {
    expect(rememberedPr(P, tide.project_id)).toBe(true);
    rememberPr(P, tide.project_id, false);
    expect(rememberedPr(P, tide.project_id)).toBe(false);
    expect(rememberedPr(P, notes.project_id)).toBe(true);
    // Remembering an agent keeps the pull request choice.
    rememberUsual(P, tide.project_id, "change", { harness: "codex", model: null, effort: null });
    expect(rememberedPr(P, tide.project_id)).toBe(false);
  });

  it("leaves the settings an earlier release reads alone", () => {
    localStorage.setItem(`talk.applied-effort.${P}`, JSON.stringify({ efforts: { "codex/default": "high" }, models: { codex: null } }));
    rememberUsual(P, tide.project_id, "change", { harness: "codex", model: "gpt-6-mini", effort: "low" });
    rememberPr(P, tide.project_id, false);
    expect(JSON.parse(localStorage.getItem(`talk.applied-effort.${P}`)!)).toEqual({ efforts: { "codex/default": "high" }, models: { codex: null } });
    expect(Object.keys(localStorage).filter((k) => k.endsWith(P)).sort()).toEqual([`launch.usual.${P}`, `talk.applied-effort.${P}`].sort());
  });
});
