// SPDX-License-Identifier: Apache-2.0
import type { EffortOption, HarnessInfo } from "../../protocol/types";

const FAMILIES = ["opus", "sonnet", "haiku", "fable"];
const title = (w: string) => w.charAt(0).toUpperCase() + w.slice(1);

/**
 * How a model is named to a person: as its harness catalog publishes it, else
 * read from the id the way the catalogs write names (a Claude id as Anthropic
 * writes it, "Claude Opus 5.5"; a GPT id as Codex publishes it, "GPT-6-Astra"),
 * else the id verbatim, because a guessed
 * name misleads where the id does not. Shown beside the agent's name, a model
 * drops the vendor that name already gives: "Claude Code · Opus 5.5". The
 * companion names its Claude catalog by the same rules;
 * both are held to the shared model-names fixture.
 */
export function modelName(id: string, harness?: HarnessInfo, agent?: string): string {
  const name = harness?.models.find((m) => m.id === id)?.label ?? claudeName(id) ?? gptName(id) ?? id;
  return agent === undefined ? name : besideAgent(agent, name);
}
function besideAgent(agent: string, name: string): string {
  const space = name.indexOf(" ");
  const vendor = name.slice(0, space), rest = name.slice(space + 1);
  // "Claude 3.5 Sonnet" keeps its vendor: a version alone is not a name.
  return space > 0 && /^[A-Za-z]/.test(rest) && (agent === vendor || agent.startsWith(`${vendor} `)) ? rest : name;
}
function claudeName(id: string): string | null {
  const m = claudeModel(id);
  if (!m) return null;
  const family = title(m.family);
  const name = m.version === null ? `Claude ${family}` : m.versionFirst ? `Claude ${m.version} ${family}` : `Claude ${family} ${m.version}`;
  const notes = [...(m.version === null ? ["latest"] : []), ...(m.million ? ["1M context"] : [])];
  return notes.length ? `${name} (${notes.join(", ")})` : name;
}
interface ClaudeModel {
  family: string;
  /** Null for an alias, which stands for the family's latest model. */
  version: string | null;
  versionFirst: boolean;
  date: string | null;
  million: boolean;
}
function claudeModel(id: string): ClaudeModel | null {
  const million = id.endsWith("[1m]");
  if (million) id = id.slice(0, -"[1m]".length);
  if (FAMILIES.includes(id)) return { family: id, version: null, versionFirst: false, date: null, million };
  if (!id.startsWith("claude-")) return null;
  const date = /[-@](\d{8})$/.exec(id)?.[1] ?? null;
  const parts = id.slice("claude-".length, date ? -9 : undefined).split("-");
  // A major and at most one minor number, never a date.
  const version = (v: string[]) => v.length >= 1 && v.length <= 2 && v.every((p) => /^(0|[1-9]\d?)$/.test(p)) ? v.join(".") : null;
  const versionFirst = !FAMILIES.includes(parts[0]!);
  const family = versionFirst ? parts[parts.length - 1]! : parts[0]!;
  const v = FAMILIES.includes(family) ? version(versionFirst ? parts.slice(0, -1) : parts.slice(1)) : null;
  return v ? { family, version: v, versionFirst, date, million } : null;
}

/**
 * Whether the model an agent reports running is the one the task asked for.
 * Claude Code reports the model an alias stands for, not the alias: a family
 * alias ("opus", "sonnet[1m]") is met by any model of that family, "opusplan"
 * by Opus or Sonnet, and "default" or "best" by whichever model the agent
 * chose. A named model is met by the same model with or without its date, a
 * dated one only by that snapshot, and an id this app cannot read only by
 * itself. The context window is a setting, not a different model.
 */
export function ranAsAsked(asked: string, reported: string): boolean {
  if (asked === reported || asked === "default" || asked === "best") return true;
  const got = claudeModel(reported);
  if (asked === "opusplan" || asked === "opusplan[1m]") return got?.family === "opus" || got?.family === "sonnet";
  const want = claudeModel(asked);
  if (!want || !got || want.family !== got.family) return false;
  return want.version === null || (want.version === got.version && (!want.date || want.date === got.date));
}
/** "gpt-5.6-sol" → "GPT-5.6-Sol". */
function gptName(id: string): string | null {
  const m = /^gpt-(\d{1,2}(?:\.\d{1,2})?)((?:-[a-z]+)*)$/.exec(id);
  return m ? `GPT-${m[1]}${m[2]!.replace(/-([a-z])/g, (_, c: string) => `-${c.toUpperCase()}`)}` : null;
}
export function modelEfforts(h: HarnessInfo | undefined, model: string | null): EffortOption[] {
  const m = model ? h?.models.find((m) => m.id === model) : h?.models.find((m) => m.is_default);
  return m?.details?.efforts ?? [];
}
export function modelChoiceValid(h: HarnessInfo | undefined, model: string | null, effort: string | null): boolean {
  return !!h && h.integration === "managed" && (!model || h.models.some((m) => m.id === model)) &&
    (!effort || modelEfforts(h, model).some((e) => e.id === effort));
}

/** Model changes preserve an explicit effort whenever the reported catalog allows it. */
export function effortForModel(h: HarnessInfo | undefined, model: string | null, effort: string | null): string | null {
  return effort && h?.capabilities.effort_selection.state === "available" && modelEfforts(h, model).some(e => e.id === effort) ? effort : null;
}
