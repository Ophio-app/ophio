// SPDX-License-Identifier: Apache-2.0
import type { AgentChoice } from "../../app/ui";
import type { HarnessInfo } from "../../protocol/types";
import { effortForModel, modelName } from "./catalog";
import { effortLabel } from "./intent";
export interface Memory { efforts: Record<string, string | null>; models: Record<string, string | null> }
const empty = (): Memory => ({ efforts: {}, models: {} });
const key = (profile: string) => `talk.applied-effort.${profile}`;
const choiceKey = (harness: string, model: string | null) => `${harness}/${model ?? "default"}`;
export function readEffortMemory(profile: string): Memory {
  try { const value = JSON.parse(localStorage.getItem(key(profile)) ?? "null"); return value?.efforts && value?.models ? value : empty(); } catch { return empty(); }
}
export function rememberEffort(profile: string, choice: AgentChoice): void {
  const value = readEffortMemory(profile);
  keepEffort(value, choice);
  saveEffortMemory(profile, value);
}
export function keepEffort(value: Memory, choice: AgentChoice): void {
  value.efforts[choiceKey(choice.harness, choice.model)] = choice.effort;
  value.models[choice.harness] = choice.model;
}
export function saveEffortMemory(profile: string, value: Memory): void {
  try { localStorage.setItem(key(profile), JSON.stringify(value)); } catch { /* Applied task choice still survives in its draft. */ }
}
export function rememberedModel(profile: string, harness: HarnessInfo, memory = readEffortMemory(profile)): string | null {
  const model = memory.models[harness.kind];
  return model && harness.models.some(m => m.id === model) ? model : null;
}
export function switchEffort(profile: string, harness: HarnessInfo | undefined, model: string | null, previous: AgentChoice, choices = readEffortMemory(profile)): { effort: string | null; notice: string | null } {
  if (!harness) return { effort: null, notice: "Agent choices are unavailable. Effort changed to the agent’s default." };
  const memory = choices.efforts;
  const k = choiceKey(harness.kind, model);
  const remembered = Object.hasOwn(memory, k);
  const wanted = remembered ? memory[k] ?? null : previous.effort;
  const effort = effortForModel(harness, model, wanted);
  const shown = model ?? harness.models.find(m => m.is_default)?.id;
  const name = `${harness.display_name} · ${shown ? modelName(shown, harness, harness.display_name) : "default model"}`;
  const label = effort ? effortLabel(effort) : "the agent’s default";
  return { effort, notice: wanted && !effort
    ? `${name} doesn’t offer ${effortLabel(wanted)} effort. Effort changed to the agent’s default.`
    : previous.effort !== effort ? `Effort changed to ${label} for ${name}${remembered ? ", restoring your choice" : ""}.` : null };
}
