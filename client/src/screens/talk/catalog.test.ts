// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { harnesses, runs, tasks } from "../../dev/sim/scenario";
import { simCatalog } from "../../dev/sim/harnesses";
import names from "../../../../crates/ports/test-fixtures/model-names.json";
import { effortForModel, modelName } from "./catalog";
it("retains supported effort and uses the default only for an unsupported choice", () => {
  const h = simCatalog(harnesses).find(h => h.kind === "codex");
  if (!h) throw new Error("Missing Codex fixture");
  const model = h.models.find(m => m.details?.efforts.some(e => e.id === "high"));
  if (!model) throw new Error("Missing effort fixture");
  expect(effortForModel(h, model.id, "high")).toBe("high");
  expect(effortForModel(h, model.id, "unsupported")).toBeNull();
  expect(effortForModel(h, model.id, null)).toBeNull();
});

it("names a model as the computer does, and a catalog's own name as published", () => {
  for (const { id, name } of names.names) expect(modelName(id), id).toBe(name);
  const codex = harnesses.find(h => h.kind === "codex")!;
  expect(modelName("gpt-6-astra", codex)).toBe(codex.models.find(m => m.id === "gpt-6-astra")!.label);
  expect(modelName("claude-opus-5-5", codex)).toBe("Claude Opus 5.5");
});

it("reads the ids harnesses and the simulator report as their catalogs name them, once beside the agent", () => {
  for (const { id, agent, shown } of names.beside) expect(modelName(id, undefined, agent), id).toBe(shown);
  for (const h of [...harnesses, ...simCatalog(harnesses)]) for (const m of h.models) expect(modelName(m.id), m.id).toBe(m.label);
  const reported = [...runs(0).flatMap(r => [r.requested_model, r.actual_model]), ...tasks(0).map(t => t.model)];
  for (const id of reported) if (id) expect(modelName(id), id).not.toBe(id);
});

import { rememberEffort, switchEffort } from "./effortMemory";
it("restores applied Astra effort after Sol, including a fresh memory read", () => {
  localStorage.clear();
  const h = simCatalog(harnesses).find(h => h.kind === "codex")!;
  const astra = { harness: "codex" as const, model: "gpt-6-astra", effort: "xhigh" };
  rememberEffort("test", astra);
  const solModel = h.models.find(m => m.label.includes("Sol"))!.id;
  const sol = switchEffort("test", h, solModel, astra);
  expect(sol.effort).toBeNull();
  expect(sol.notice).toContain("doesn’t offer Extra high");
  rememberEffort("test", { ...astra, model: solModel, effort: sol.effort });
  expect(switchEffort("test", h, astra.model, { ...astra, model: solModel, effort: null }).effort).toBe("xhigh");
});
it("explains effort fallback when changing agents", () => {
  localStorage.clear();
  const codex = simCatalog(harnesses).find(h => h.kind === "codex")!;
  const pick = switchEffort("test", codex, null, { harness: "claude_code", model: "claude-opus-5-5", effort: "max" });
  expect(pick.effort).toBeNull();
  expect(pick.notice).toMatch(/Codex.*doesn’t offer Max.*default/);
  const claude = simCatalog(harnesses).find(h => h.kind === "claude_code")!;
  expect(switchEffort("test", claude, "claude-opus-5-5", { harness: "codex", model: "gpt-6-astra", effort: "minimal" }).notice)
    .toBe("Claude Code · Opus 5.5 doesn’t offer Minimal effort. Effort changed to the agent’s default.");
});
