// SPDX-License-Identifier: Apache-2.0
import type { HarnessInfo, ModelOption } from "../../protocol/types";
import type { Memory } from "./effortMemory";
import { modelName } from "./catalog";

/** A full Claude version pins the model; Codex's alias:false does not establish this. */
export function isFixedModel(model: ModelOption): boolean {
  return /^claude-(?:[a-z]+-\d+(?:-\d+)?|\d+(?:-\d+)?-[a-z]+)(?:[-@]\d{8})?(?:\[1m\])?$/.test(model.id);
}

/** Human-readable picker text, even when native catalogs label exact twins with IDs. */
export function pickerModelLabel(model: ModelOption, harnessDisplayName?: string): string {
  const id = model.id;
  const name = model.label === id ? modelName(id) : model.label;
  let readable = name === id ? id.replace(/[-_]+/g, " ").replace(/\b[a-z]/g, c => c.toUpperCase()) : name;
  if (harnessDisplayName && readable.startsWith(`${harnessDisplayName} `)) {
    readable = readable.slice(harnessDisplayName.length + 1);
  } else if (harnessDisplayName?.startsWith("Claude") && readable.startsWith("Claude ")) {
    readable = readable.slice(7);
  }
  const date = /[-@](\d{4})(\d{2})(\d{2})(?:\[1m\])?$/.exec(id);
  if (date) {
    const compact = date[0].slice(1, 9);
    readable = readable.replace(new RegExp(`(?:[-@ ]${compact})$`), "");
    readable += ` · ${date[1]}-${date[2]}-${date[3]}`;
  }
  if (isFixedModel(model)) readable += " · fixed";
  return readable;
}

function family(model: ModelOption): string {
  const name = model.label === model.id ? modelName(model.id) : model.label;
  const claude = /^Claude\s+(?:\d+(?:\.\d+)?\s+)?([a-z]+)/i.exec(name)?.[1];
  if (claude) return `claude-${claude.toLowerCase()}`;
  const codex = /^GPT-\d+(?:\.\d+)?(?:[- ]([a-z]+))?/i.exec(name);
  if (codex) return `gpt-${(codex[1] ?? "base").toLowerCase()}`;
  // Fallback for native labels that aren't human-readable yet.
  const id = model.id.replace(/\[1m\]$/, "");
  const claudeId = /^(?:claude-)?([a-z]+)(?:\d|[.-]|$)/.exec(id)?.[1] ??
    /^claude-\d+(?:-\d+)?-([a-z]+)/.exec(id)?.[1];
  if (claudeId) return `claude-${claudeId}`;
  const gptId = /^gpt-\d+(?:\.\d+)?(?:-([a-z]+))?/.exec(id);
  return gptId ? `gpt-${gptId[1] ?? "base"}` : id;
}

function version(model: ModelOption): number {
  const name = model.label === model.id ? modelName(model.id) : model.label;
  // Alias IDs such as fable5.1 have no hyphen, but the catalog names their
  // version. An alias flag alone says nothing about how recent they are.
  if (/\(latest\)/i.test(name) && model.id === family(model).replace(/^claude-/, "")) return Number.MAX_SAFE_INTEGER;
  const named = /(?:^GPT-|^Claude\s+(?:[a-z]+\s+)?)(\d{1,2})(?:[.-](\d{1,2}))?/i.exec(name);
  if (named) return Number(named[1]) * 100 + Number(named[2] ?? 0);
  const id = /(?:^gpt-|^claude-(?:[a-z]+-)?)(\d{1,2})(?:[.-](\d{1,2})(?!\d))?/.exec(model.id);
  return id ? Number(id[1]) * 100 + Number(id[2] ?? 0) : 0;
}

/** The expanded list groups families, newest first, with fixed versions last. */
export function orderExtraModels(models: ModelOption[]): ModelOption[] {
  return [...models].sort((a, b) => Number(isFixedModel(a)) - Number(isFixedModel(b)) ||
    family(a).localeCompare(family(b), "en") || version(b) - version(a) || b.id.localeCompare(a.id, "en"));
}

/** Four explicit choices; the agent's default remains a separate caller-owned pill. */
export function shortlistModels(harness: HarnessInfo, current: string | null, memory: Memory): ModelOption[] {
  const models = harness.models;
  const selected = new Set<string>();
  const unavailable = !!current && !models.some(m => m.id === current);
  const limit = unavailable ? 3 : 4;
  const add = (id: string | null | undefined) => {
    if (id && selected.size < limit && models.some(m => m.id === id)) selected.add(id);
  };
  add(current);
  // One remembered alternative leaves room for current families.
  const remembered = memory.models[harness.kind];
  const prefix = `${harness.kind}/`;
  const previous = remembered && remembered !== current && models.some(m => m.id === remembered)
    ? remembered
    : Object.keys(memory.efforts).reverse()
      .map(key => key.startsWith(prefix) ? key.slice(prefix.length) : null)
      .find(id => id && id !== current && models.some(m => m.id === id));
  add(previous);

  // Group by the names the agent publishes, not the presence of "alias".
  // For each family choose its newest version; a moving bare-family alias wins
  // only when its published version matches the newest one.
  const groups = new Map<string, ModelOption[]>();
  for (const model of models) {
    const group = family(model);
    groups.set(group, [...(groups.get(group) ?? []), model]);
  }
  for (const members of groups.values()) {
    const newestVersion = Math.max(...members.map(version));
    const latest = members.filter(m => version(m) === newestVersion);
    add(latest.find(m => m.id === family(m).replace(/^claude-/, ""))?.id ??
      latest.find(m => !isFixedModel(m))?.id ?? latest[0]?.id);
  }
  // Fill spare places with current alternatives, then older aliases, then pins.
  for (const model of models.filter(m => !isFixedModel(m)).sort((a, b) => version(b) - version(a))) add(model.id);
  for (const model of models.filter(isFixedModel).sort((a, b) => version(b) - version(a))) add(model.id);
  const result = models.filter(m => selected.has(m.id));
  if (unavailable) {
    return [{ id: current!, label: current!, is_default: false, suggested_for_computer_use: false }, ...result];
  }
  return result;
}
