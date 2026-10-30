// SPDX-License-Identifier: Apache-2.0
import type { AgentFile, CatalogEntry, Disable, DisabledItem, DisabledKind, ProtocolError } from "../../protocol/types";
import type { SimHost } from "./host";
import { catalog } from "./launchers";
import { sha256 } from "./host";

const BUILT_IN = "Built into Øphio, whose own setup and GitHub work start from it.";
const PLUGIN = "Claude Code can hide a plugin’s skills and commands only by turning the whole plugin off.";
const PART_OF_SKILL = "Part of a skill. Disable the skill itself, from its SKILL.md.";
const NOT_RUN = "No agent Øphio runs reads this file.";
const CODEX_GLOBAL = "Codex always reads this from its own folder, so Øphio would have to change that folder to hide it.";
const CODEX_PROMPTS = "Agents never see Codex prompts. Only you can send one, by typing its name in Codex.";
const MEMORY_TOPIC = "Claude Code finds this through the MEMORY.md beside it. Disable that to keep this folder’s memory from agents in Øphio.";
const can = (kind: DisabledKind, disabled: boolean, key: string): Disable => ({ state: "can", kind, disabled, key });
const cannot = (reason: string): Disable => ({ state: "cannot", reason });
const error = (code: ProtocolError["code"], message: string): ProtocolError => ({ code, message });

export class SimDisabled {
  readonly items = new Map<string, DisabledItem>();
  constructor(private host: SimHost) {}

  has(path: string): boolean { return this.items.has(path); }
  /** Every catalog entry, Home's and each project's, as the computer looks. */
  async entries(): Promise<CatalogEntry[]> {
    const all: CatalogEntry[] = [];
    for (const id of [null, ...this.host.projects.filter((p) => !p.home).map((p) => p.project_id)]) all.push(...(await catalog(this.host, sha256, id)).entries);
    return all;
  }
  seed(paths: string[]): Promise<void> {
    return paths.reduce<Promise<void>>((done, path) => done.then(async () => {
      const files = (await this.host.libraryWorkflows.query({ kind: "agent_files" }) as Extract<import("../../protocol/types").QueryData, { kind: "agent_files" }>).files;
      const file = files.find((f) => f.host_path === path);
      const entries = await this.entries();
      const entry = entries.find((e) => e.host_path === path);
      const classification = file?.disable ?? (entry && this.entry(entry));
      if (!classification || classification.state !== "can") throw error("invalid_request", `Cannot seed ${path}`);
      await this.set(classification.kind, path, true);
    }), Promise.resolve());
  }

  entry(entry: CatalogEntry): Disable {
    if (entry.copy || entry.source === "ophio_setup") return cannot(BUILT_IN);
    if (entry.kind === "agent" && ["ophio", "claude_user", "claude_project"].includes(entry.source)) return can("agent", this.has(entry.host_path), entry.host_path);
    if (entry.kind === "skill" && ["claude_user", "claude_project", "codex_user", "codex_project"].includes(entry.source)) return can("skill", this.has(entry.host_path), entry.host_path);
    return cannot(BUILT_IN);
  }

  file(file: AgentFile): Disable {
    const path = file.host_path ?? "";
    const name = path.split("/").at(-1) ?? "";
    const harness = file.harness;
    const plugin = file.scope.endsWith(" plugin");
    const result = (kind: DisabledKind) => can(kind, this.has(path), path);
    if (harness && !["claude_code", "codex"].includes(harness)) return cannot(NOT_RUN);
    switch (file.group) {
      case "skills":
        if (!/^SKILL\.md$/i.test(name)) return cannot(PART_OF_SKILL);
        if (plugin && harness === "claude_code") return cannot(PLUGIN);
        return harness ? result("skill") : cannot(NOT_RUN);
      case "agents":
        if (file.scope === "Ophio" && ["bug-hunter", "everyday-user"].includes(name.replace(/\.md$/, ""))) return cannot(BUILT_IN);
        return harness === "claude_code" || file.scope === "Ophio" ? result("agent") : cannot(NOT_RUN);
      case "commands":
        if (harness === "codex") return cannot(CODEX_PROMPTS);
        return plugin ? cannot(PLUGIN) : harness === "claude_code" ? result("skill") : cannot(NOT_RUN);
      case "instructions":
        if (harness === "codex" && file.everywhere) return cannot(CODEX_GLOBAL);
        return harness === "claude_code" || harness === "codex" ? result("file") : cannot(NOT_RUN);
      case "rules": return harness === "claude_code" ? result("file") : cannot(NOT_RUN);
      case "memory":
        return harness !== "claude_code" ? cannot(NOT_RUN)
          : name === "MEMORY.md" && /\/projects\/[^/]+\/memory\/MEMORY\.md$/.test(path) ? result("file") : cannot(MEMORY_TOPIC);
      default: return cannot(NOT_RUN);
    }
  }

  async set(kind: DisabledKind, path: string, disabled: boolean): Promise<void> {
    if (!disabled) { this.items.delete(path); return; }
    const files = (await this.host.libraryWorkflows.query({ kind: "agent_files" }) as Extract<import("../../protocol/types").QueryData, { kind: "agent_files" }>).files;
    const file = files.find((f) => f.host_path === path);
    const entry = file ? undefined : (await this.entries()).find((e) => e.host_path === path);
    if (!file && !entry) throw error("not_found", "Library doesn’t list that now. Look again, then try.");
    const classification = file ? this.file(file) : this.entry(entry!);
    if (classification.state === "cannot") throw error("invalid_request", classification.reason);
    if (classification.kind !== kind) throw error("invalid_request", "That isn’t what it was listed as. Look again, then try.");
    const root = file && this.host.roots.find((r) => r.root_id === file.root_id);
    const scope = entry && (entry.source === "claude_user" ? "Claude Code" : entry.source === "codex_user" ? "Codex" : entry.source === "ophio" ? "Ophio" : this.host.projects.find((p) => p.host_path && path.startsWith(`${p.host_path}/`))?.name ?? "");
    this.items.set(path, {
      kind, host_path: path, name: file ? file.title ?? file.shown.split("/").at(-1)! : entry!.name,
      description: file?.description ?? entry?.description ?? null, scope: file?.scope ?? scope ?? "",
      harness: file?.harness ?? entry?.harness ?? null, project_id: file?.project_id ?? (entry?.source.endsWith("_project") ? this.host.projects.find((p) => path.startsWith(`${p.host_path}/`))?.project_id ?? null : null),
      group: file?.group ?? null,
      library: file && root ? { root_id: root.root_id, path: file.path } : entry?.library ?? null,
      disabled_at: this.items.get(path)?.disabled_at ?? Date.now(), found: true,
    });
  }
  list(): DisabledItem[] {
    return [...this.items.values()].map((item) => {
      const root = this.host.roots.find((r) => item.host_path.startsWith(`${r.host_path}/`));
      const path = root && item.host_path.slice(root.host_path.length + 1);
      return { ...item, found: !!(path && this.host.files[root!.root_id]?.[path]) || (!root && item.found), library: path && this.host.files[root!.root_id]?.[path] ? { root_id: root!.root_id, path } : null };
    }).sort((a, b) => b.disabled_at - a.disabled_at);
  }
  refuse(path: string, name: string): void {
    if (this.has(path)) throw error("conflict", `“${name}” is disabled in Øphio. Enable it in Library to start from it.`);
  }
}
