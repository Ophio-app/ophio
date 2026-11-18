// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { HarnessInfo, ModelOption } from "../../protocol/types";
import { harnesses } from "../../dev/sim/scenario";
import { claudeModels } from "../../dev/sim/modelCatalog";
import { isFixedModel, pickerModelLabel, shortlistModels } from "./modelPicker";
import type { Memory } from "./effortMemory";

const model = (id: string, alias = false): ModelOption => ({ id, label: id, is_default: false,
  suggested_for_computer_use: false, details: { alias, efforts: [], default_effort: null } });
const memory = (models: Memory["models"] = {}, efforts: Memory["efforts"] = {}): Memory => ({ models, efforts });
const harness = (kind: HarnessInfo["kind"], models: ModelOption[]): HarnessInfo =>
  ({ ...harnesses.find(h => h.kind === kind)!, models });
const ids = (models: ModelOption[]) => models.map(m => m.id);

it("shows a pinned Claude version as fixed but does not infer that from Codex alias:false", () => {
  expect(isFixedModel(model("claude-opus-5-5"))).toBe(true);
  expect(isFixedModel(model("claude-opus-5-5-20260901"))).toBe(true);
  expect(isFixedModel(model("claude-3-5-sonnet-20260101"))).toBe(true);
  expect(pickerModelLabel(model("claude-3-5-sonnet-20260101"), "Claude Code"))
    .toBe("3.5 Sonnet · 2026-01-01 · fixed");
  expect(isFixedModel(model("opus", true))).toBe(false);
  expect(isFixedModel(model("gpt-6-astra"))).toBe(false);
  expect(pickerModelLabel(model("claude-opus-5-5"), "Claude Code")).toBe("Opus 5.5 · fixed");
  expect(pickerModelLabel(model("claude-opus-5-5-20260901"), "Claude Code"))
    .toBe("Opus 5.5 · 2026-09-01 · fixed");
  expect(pickerModelLabel(model("gpt-6-astra"), "Codex")).toBe("GPT-6-Astra");
});

it("keeps a current old exact model and recent picks, capped at four in catalog order", () => {
  const h = harness("claude_code", [model("claude-opus-4-1"), model("opus", true),
    model("sonnet", true), model("haiku", true), model("claude-sonnet-3-5"), model("fable", true)]);
  expect(ids(shortlistModels(h, "claude-opus-4-1", memory({ claude_code: "haiku" },
    { "claude_code/sonnet": null, "claude_code/fable": null })))).toEqual([
      "claude-opus-4-1", "opus", "sonnet", "haiku",
    ]);
});

it("keeps an unavailable current model visible and reserves a place for it", () => {
  const h = harness("codex", [model("gpt-6-astra"), model("gpt-6-sol"),
    model("gpt-5.5-astra"), model("gpt-6-mini")]);
  expect(ids(shortlistModels(h, "gpt-5.1-sol", memory({ codex: "gpt-6-sol" }))))
    .toEqual(["gpt-5.1-sol", "gpt-6-astra", "gpt-6-sol", "gpt-6-mini"]);
});

it("prefers moving aliases over exact twins without hiding short catalogs", () => {
  const h = harness("claude_code", [model("claude-opus-5-5"), model("opus", true),
    model("sonnet", true)]);
  expect(ids(shortlistModels(h, null, memory()))).toEqual(["claude-opus-5-5", "opus", "sonnet"]);
  expect(ids(shortlistModels(harness("codex", [model("gpt-6-astra"), model("gpt-6-sol")]),
    null, memory()))).toEqual(["gpt-6-astra", "gpt-6-sol"]);
});

it("shows the newest four families from the full native catalog, not older aliases or exact twins", () => {
  const h = harness("claude_code", claudeModels);
  expect(ids(shortlistModels(h, null, memory()))).toEqual(["opus", "fable", "sonnet", "haiku"]);
  expect(ids(shortlistModels(h, "opus4.7", memory({ claude_code: "fable5.1" },
    { "claude_code/sonnet4.6": null, "claude_code/haiku4.5": null })))).toEqual([
      "opus", "fable", "fable5.1", "opus4.7",
    ]);
  expect(pickerModelLabel(claudeModels[13]!, "Claude Code")).toBe("Opus 5.5 · fixed");
  expect(pickerModelLabel(claudeModels[4]!, "Claude Code")).toBe("Fable 5.1");
});

it("does not mistake an older alias for the newest member of a family", () => {
  const h = harness("claude_code", [
    { ...model("fable5.1", true), label: "Claude Fable 5.1" },
    { ...model("claude-fable-5-2"), label: "claude-fable-5-2" },
    { ...model("sonnet", true), label: "Claude Sonnet 5.5" },
    { ...model("haiku", true), label: "Claude Haiku 5.5" },
    { ...model("opus", true), label: "Claude Opus 5.5" },
  ]);
  expect(ids(shortlistModels(h, null, memory()))).toEqual([
    "claude-fable-5-2", "sonnet", "haiku", "opus",
  ]);
  expect(pickerModelLabel(model("claude-opus-5-5-20261018"), "Claude Code"))
    .not.toBe(pickerModelLabel(model("claude-opus-5-5-20260901"), "Claude Code"));
  expect(pickerModelLabel(model("custom-thing_42"))).toBe("Custom Thing 42");
});
