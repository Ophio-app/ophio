// SPDX-License-Identifier: Apache-2.0
import type { AgentChoice } from "../../app/ui";
import type { HarnessInfo, Run, Task } from "../../protocol/types";
import { effortForModel, modelName } from "./catalog";
import { effortLabel } from "./intent";
export interface Memory {
  efforts: Record<string, string | null>;
  models: Record<string, string | null>;
  /** Saved run ordering, only for a conversation's history cache. */
  applied?: Record<string, { at: number; run: string }>;
}
const empty = (): Memory => ({ efforts: {}, models: {} });
const key = (profile: string, conversation?: string) => `talk.applied-effort.${profile}${conversation ? `.conversation.${conversation}` : ""}`;
const choiceKey = (harness: string, model: string | null) => `${harness}/${model ?? "default"}`;
export function readEffortMemory(profile: string, conversation?: string): Memory {
  try { const value = JSON.parse(localStorage.getItem(key(profile, conversation)) ?? "null"); return value?.efforts && value?.models ? value : empty(); } catch { return empty(); }
}

/** Cache this chat's requested choices, not another task's preferences or a
 * native agent's substituted model. The cache retains providers omitted from
 * a size-limited snapshot; newer saved history replaces it when available. */
export function conversationMemory(task: Task, runs: Record<string, Run>, cached: Memory = empty()): Memory {
  const memory: Memory = { efforts: { ...cached.efforts }, models: { ...cached.models }, applied: { ...cached.applied } };
  const apply = (choice: AgentChoice, at: number, run: string) => {
    const prior = memory.applied![choice.harness];
    if (prior && (prior.at > at || (prior.at === at && prior.run > run))) return;
    keepEffort(memory, choice);
    memory.applied![choice.harness] = { at, run };
  };
  Object.values(runs).filter(r => r.task_id === task.task_id)
    .sort((a, b) => a.admitted_at - b.admitted_at || a.run_id.localeCompare(b.run_id))
    .forEach(r => apply({ harness: r.harness, model: r.requested_model, effort: r.requested_effort }, r.admitted_at, r.run_id));
  // The current task is the saved choice, even before its run arrives, and
  // keeps an explicit choice of the agent's default (null) intact.
  const current = task.current_run ? runs[task.current_run] : undefined;
  keepEffort(memory, { harness: task.harness, model: task.model, effort: task.effort });
  memory.applied![task.harness] = { at: current?.admitted_at ?? task.updated_at, run: task.current_run ?? "" };
  return memory;
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
export function saveEffortMemory(profile: string, value: Memory, conversation?: string): void {
  try { localStorage.setItem(key(profile, conversation), JSON.stringify(value)); } catch { /* Applied task choice still survives in its draft. */ }
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
