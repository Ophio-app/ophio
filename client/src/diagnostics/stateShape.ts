// SPDX-License-Identifier: Apache-2.0
import type { StateShape } from "../protocol/types";
import { pseudonym, type PseudonymKind } from "./pseudonym";
import { enumValues, shapeKeys } from "./stateSchema";

export const STATE_SHAPE_VERSION = 1;
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const maps: Record<string, PseudonymKind> = {
  projects: "project", tasks: "task", runs: "run", approvals: "approval", attention: "attention", resolvedAttention: "attention", commandStages: "command", commands: "command",
  timelines: "task", timelineReaders: "task", activity: "task", liveText: "other", control: "other", queues: "project", previews: "project", artifacts: "other",
  olderTasks: "project", taskByProject: "project", drafts: "other", savedDrafts: "other", removedMarkup: "other",
};
export function idKind(key: string): PseudonymKind {
  const name = key.replace(/_id$|Id$|_ids$|Ids$/, "").toLowerCase();
  return (["project", "task", "run", "approval", "device", "command", "attention"] as const).find((kind) => name.endsWith(kind)) ?? "other";
}
const mapKind = (key: string, parent: string) => ["", "host", "ui"].includes(parent) && Object.hasOwn(maps, key) ? maps[key] : undefined;
const idField = (key: string) => key === "id" || /(?:_id|Id|_ids|Ids)$/.test(key) || key === "bootEpoch" || key === "boot_epoch";
const modelName = (key: string, value: string) => /^(?:model|model_id|modelId)$/.test(key) && /^(?:claude-|gpt-|codex|o[1-9](?:-|$))[a-zA-Z0-9._-]*$/.test(value);
export function textShape(text: string): string {
  const kind = /(?:^[a-z][a-z0-9+.-]*:\/\/)/i.test(text) ? "url" : /^(?:\/|~\/|\.\.?\/|[A-Za-z]:\\)/.test(text) || text.includes("\\") ? "path" : "text";
  return `~${kind}:${Array.from(text).length}`;
}

/** Unknown keys are data too (file maps and draft maps often contain paths). */
export async function stateShape(store: unknown, salt: string): Promise<StateShape> {
  const ids = new Map<string, PseudonymKind>();
  const remember = (id: string, kind: PseudonymKind) => {
    if (kind !== "other" || !ids.has(id.toLowerCase())) ids.set(id.toLowerCase(), kind);
  };
  const valueKind = (key: string, parent: string): PseudonymKind | undefined => parent === "taskByProject" ? "task"
    : key === "writer" || key === "current_run" ? "run" : idField(key) ? idKind(key) : undefined;
  const scan = (value: unknown, key = "", parent = "") => {
    if (typeof value === "string" && (valueKind(key, parent) || uuid.test(value))) remember(value, valueKind(key, parent) ?? "other");
    else if (Array.isArray(value)) value.forEach((v) => scan(v, key, parent));
    else if (value && typeof value === "object") for (const [k, v] of Object.entries(value)) {
      const kind = mapKind(key, parent);
      if (kind) remember(k, kind);
      scan(v, k, key);
    }
  };
  scan(store);
  const cache = new Map<string, Promise<string>>();
  const pseudo = (value: string, kind: PseudonymKind) => {
    const key = `${kind}:${value.toLowerCase()}`;
    if (!cache.has(key)) cache.set(key, pseudonym(salt, kind, value));
    return cache.get(key)!;
  };
  async function visit(value: unknown, key = "", parent = ""): Promise<unknown> {
    if (typeof value === "string") {
      if (valueKind(key, parent) || uuid.test(value) || ids.has(value.toLowerCase())) return pseudo(value, ids.get(value.toLowerCase()) ?? valueKind(key, parent) ?? "other");
      if (Array.isArray(enumValues[key]) && enumValues[key]!.includes(value) || modelName(key, value)) return value;
      // Route patterns are produced by routePattern, never copied from locations.
      if ((key === "route" || key === "screen") && /^\/[a-z_]+(?:\/:\w+)*$/.test(value) && enumValues.screen?.includes(value.split("/")[1]!)) return value;
      return textShape(value);
    }
    if (Array.isArray(value)) return Promise.all(value.map(v => visit(v, key, parent)));
    if (value && typeof value === "object") {
      const result: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(value)) {
        const kind = mapKind(key, parent) ?? ids.get(k.toLowerCase());
        const draftParts = parent === "ui" && (key === "drafts" || key === "savedDrafts") ? k.split("/") : [];
        const safeKey = draftParts.length === 2 ? `${await pseudo(draftParts[0]!, "project")}/${draftParts[1] === "new" ? "new" : await pseudo(draftParts[1]!, "task")}`
          : kind || uuid.test(k) ? await pseudo(k, kind ?? "other")
          : shapeKeys.has(k) ? k : await pseudo(k, "other");
        Object.defineProperty(result, safeKey, { value: await visit(v, k, key), enumerable: true, configurable: true, writable: true });
      }
      return result;
    }
    return typeof value === "number" || typeof value === "boolean" || value === null ? value : null;
  }
  return { version: STATE_SHAPE_VERSION, store: await visit(store) };
}
