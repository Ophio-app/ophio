// SPDX-License-Identifier: Apache-2.0
//
// The simulated computer's agents, skills and one-offs, read from its Library
// files each time as the companion reads its folders
// (crates/services/src/launchers.rs).

import type { Catalog, CatalogEntry, CatalogKind, CatalogSource, CommandResult, HarnessKind, LaunchPolicy, LibraryRoot, Project, ProtocolError, StartedFrom, Task, TaskPlace, TaskStart } from "../../protocol/types";
import { slug } from "../../app/launchers";
import { ids } from "./scenario";
import type { SimDisabled } from "./disabled";

export const ONE_OFFS = "/home/sam/Ophio";
const AGENTS = `${ONE_OFFS}/Agents`;

/** Found on the computer outside every Library root. */
const ELSEWHERE: { kind: CatalogKind; source: CatalogSource; id: string; host_path: string; text: string }[] = [
  {
    kind: "skill",
    source: "codex_user",
    id: "imagegen",
    host_path: "/home/sam/.codex/skills/imagegen/SKILL.md",
    text: "---\nname: imagegen\ndescription: Make or edit images from a description, saved into the project.\n---\n\nUse the image model; save results under assets/.\n",
  },
  {
    kind: "skill",
    source: "codex_user",
    id: "api-docs",
    host_path: "/home/sam/.codex/skills/api-docs/SKILL.md",
    text: "---\nname: api-docs\ndescription: Generate reference docs for the HTTP endpoints from their handlers.\n---\n",
  },
  {
    kind: "skill",
    source: "codex_user",
    id: "changelog",
    host_path: "/home/sam/.codex/skills/changelog/SKILL.md",
    text: "---\nname: changelog\ndescription: Keep CHANGELOG.md current from merged work, newest first.\n---\n",
  },
  {
    kind: "skill",
    source: "codex_user",
    id: "db-migrations",
    host_path: "/home/sam/.codex/skills/db-migrations/SKILL.md",
    text: "---\nname: db-migrations\ndescription: Write a reversible database migration and test both directions.\n---\n",
  },
  {
    kind: "skill",
    source: "codex_user",
    id: "dependency-bump",
    host_path: "/home/sam/.codex/skills/dependency-bump/SKILL.md",
    text: "---\nname: dependency-bump\ndescription: Update one dependency, read its release notes and fix what breaks.\n---\n",
  },
  {
    kind: "skill",
    source: "codex_user",
    id: "perf-profile",
    host_path: "/home/sam/.codex/skills/perf-profile/SKILL.md",
    text: "---\nname: perf-profile\ndescription: Profile a slow path and report where the time goes before changing anything.\n---\n",
  },
  {
    kind: "skill",
    source: "codex_user",
    id: "screenshot-tests",
    host_path: "/home/sam/.codex/skills/screenshot-tests/SKILL.md",
    text: "---\nname: screenshot-tests\ndescription: Capture screens at phone sizes and compare them with the last run.\n---\n",
  },
  {
    kind: "skill",
    source: "codex_user",
    id: "translate-strings",
    host_path: "/home/sam/.codex/skills/translate-strings/SKILL.md",
    text: "---\nname: translate-strings\ndescription: Translate new interface strings, keeping placeholders intact.\n---\n",
  },
  {
    kind: "skill",
    source: "codex_user",
    id: "triage-issues",
    host_path: "/home/sam/.codex/skills/triage-issues/SKILL.md",
    text: "---\nname: triage-issues\ndescription: Label and sort new GitHub issues, and close duplicates with a link.\n---\n",
  },
];

/** Øphio's own skills, as `launchers::SETUP_SKILLS`. */
export const SETUP_SKILLS: Record<string, string> = {
  "setup-phone": "---\nname: setup-phone\ndescription: Pair another phone with this computer and check it can reach it from anywhere.\n---\n\nWalk the person through pairing, one step at a time.\n",
  "checklist": "---\nname: Give the person a checklist\ndescription: Puts whatever the person has to do themselves on the project's checklist in Øphio, as complete steps with every link, text and file in the step that needs it, and carries on the moment they tick off what you're waiting on.\n---\n\nWhenever the person has to do something themselves, it goes on the project's checklist, not into your reply.\n",
};
/** Some of Øphio's own agents, as `setup_agents::setup_agents`. */
export const SETUP_AGENTS: Record<string, string> = {
  "bug-hunter": "---\nname: Bug hunter\ndescription: Hunts for bugs in the code: logic errors, edge cases, ignored errors, races and code that breaks what it or its docs promise. Use to find bugs before people do.\n---\n\nReview the part of the project the person names.\n",
  "everyday-user": "---\nname: Everyday user\ndescription: Tests the running app live as an everyday user: brings it up, uses it on the screen as a typical person would, and reports unclear steps, surprises, dead ends and anything broken. Use to see how an app works for its users.\n---\n\nTest the running app live, as the person described below.\n",
};
const SETUP_FOLDER = "/home/sam/.local/share/ophio/setup-skills";
/** Øphio's own agent or skill `id`. */
function setupFile(id: string): { kind: CatalogKind; text: string } | undefined {
  if (id in SETUP_AGENTS) return { kind: "agent", text: SETUP_AGENTS[id]! };
  if (id in SETUP_SKILLS) return { kind: "skill", text: SETUP_SKILLS[id]! };
  return undefined;
}
/** Where a person's copy of one of Øphio's agents or skills is kept, in the one-offs root. */
const copyPath = (kind: CatalogKind, id: string) => (kind === "agent" ? `Agents/${id}.md` : `Skills/${id}/SKILL.md`);
/** Where the person's copy of Øphio's `id` is kept, when Øphio has one by that name. */
export const setupCopyPath = (id: string) => {
  const found = setupFile(id);
  return found && copyPath(found.kind, id);
};

export interface LauncherFiles {
  roots: LibraryRoot[];
  files: Record<string, Record<string, string>>;
  /** The built-in version each copied agent or skill was copied from, by id. */
  skillBases: Record<string, string>;
}

function err(code: ProtocolError["code"], message: string): ProtocolError {
  return { code, message };
}

function frontmatter(text: string): Record<string, string> {
  const lines = text.split("\n");
  if (lines[0]?.trim() !== "---") return {};
  const end = lines.indexOf("---", 1);
  if (end < 0) return {};
  const out: Record<string, string> = {};
  for (const line of lines.slice(1, end)) {
    const at = line.indexOf(":");
    if (at > 0 && !line.startsWith(" ")) out[line.slice(0, at).trim()] = line.slice(at + 1).trim().replace(/^"(.*)"$/, "$1");
  }
  return out;
}

const HARNESS: Record<string, HarnessKind> = { claude: "claude_code", claude_code: "claude_code", codex: "codex" };
const PERMISSION: Record<string, LaunchPolicy> = { default: "standard", acceptEdits: "accept_edits", bypassPermissions: "bypass_permissions" };

function entry(kind: CatalogKind, source: CatalogSource, id: string, hostPath: string, text: string, digest: string, library: CatalogEntry["library"]): CatalogEntry {
  const f = frontmatter(text);
  const native: HarnessKind | null = source.startsWith("claude") ? "claude_code" : source.startsWith("codex") ? "codex" : null;
  return {
    kind,
    source,
    id,
    name: f.name || id,
    description: f.description || null,
    digest,
    host_path: hostPath,
    library,
    harness: (f.harness && HARNESS[f.harness.toLowerCase().replace(/[- ]/g, "_")]) || native,
    model: f.model && f.model !== "inherit" ? f.model : null,
    effort: f.effort || null,
    launch_policy: (f.launch_policy as LaunchPolicy | undefined) ?? (f.permissionMode ? PERMISSION[f.permissionMode] ?? null : null),
  };
}

/** tidewater-api's own agent and skill, in its .claude and .codex folders. */
const PROJECT_OWN: { kind: CatalogKind; source: CatalogSource; id: string; host_path: string; text: string }[] = [
  {
    kind: "agent",
    source: "claude_project",
    id: "api-reviewer",
    host_path: "/home/sam/Projects/tidewater-api/.claude/agents/api-reviewer.md",
    text: "---\nname: api-reviewer\ndescription: Reviews a change to the public API for breaking changes.\n---\n\nRead the diff against the OpenAPI spec and name anything a client would notice.\n",
  },
  {
    kind: "skill",
    source: "codex_project",
    id: "migrations",
    host_path: "/home/sam/Projects/tidewater-api/.codex/skills/migrations/SKILL.md",
    text: "---\nname: migrations\ndescription: Write and check a database migration.\n---\n\nWrite the up and down steps, then run both against a copy.\n",
  },
];

/** Everything a task can start from, with `projectId`'s own when it has any,
 * and every project's when it's undefined, as a start is checked. */
export async function catalog(lib: LauncherFiles, sha256: (text: string) => Promise<string>, projectId?: string | null, disabled?: SimDisabled): Promise<Catalog> {
  const entries: CatalogEntry[] = [];
  const add = async (kind: CatalogKind, source: CatalogSource, rootId: string, match: RegExp, builtIn: Record<string, string> = {}) => {
    const root = lib.roots.find((r) => r.root_id === rootId);
    if (!root) return;
    for (const [path, text] of Object.entries(lib.files[rootId] ?? {})) {
      const m = match.exec(path);
      // A copy of one of Øphio's own is listed once, as that.
      if (!m || m[1]! in builtIn) continue;
      entries.push(entry(kind, source, m[1]!, `${root.host_path}/${path}`, text, await sha256(text), { root_id: rootId, path }));
    }
  };
  await add("agent", "ophio", ids.rootOphio, /^Agents\/([^/]+)\.md$/, SETUP_AGENTS);
  await add("agent", "claude_user", ids.rootAgents, /^([^/]+)\.md$/);
  await add("skill", "claude_user", ids.rootSkills, /^([^/]+)\/SKILL\.md$/);
  for (const e of ELSEWHERE) entries.push(entry(e.kind, e.source, e.id, e.host_path, e.text, await sha256(e.text), null));
  if (projectId === undefined || projectId === ids.pTide)
    for (const e of PROJECT_OWN) entries.push(entry(e.kind, e.source, e.id, e.host_path, e.text, await sha256(e.text), null));
  const builtIns = [...Object.keys(SETUP_AGENTS), ...Object.keys(SETUP_SKILLS)];
  for (const id of builtIns) {
    const { kind, text } = setupFile(id)!;
    const builtIn = await sha256(text);
    const path = copyPath(kind, id);
    const mine = lib.files[ids.rootOphio]?.[path];
    if (mine === undefined) {
      entries.push(entry(kind, "ophio_setup", id, `${SETUP_FOLDER}/${kind === "agent" ? `${id}.md` : `${id}/SKILL.md`}`, text, builtIn, null));
      continue;
    }
    const digest = await sha256(mine);
    const copied = entry(kind, "ophio_setup", id, `${ONE_OFFS}/${path}`, mine, digest, { root_id: ids.rootOphio, path });
    entries.push({ ...copied, copy: { newer: (lib.skillBases[id] ?? digest) !== builtIn } });
  }
  if (disabled) for (const e of entries) e.disable = disabled.entry(e);
  entries.sort((a, b) => a.kind.localeCompare(b.kind) || a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
  return { entries, one_offs: ONE_OFFS, agents_folder: { root_id: ids.rootOphio, path: "Agents" }, problems: [], truncated: false };
}

/** What a task starts from, checked against the files as they are now. */
export async function resolveStart(lib: LauncherFiles, start: TaskStart, sha256: (text: string) => Promise<string>, disabled?: SimDisabled): Promise<StartedFrom> {
  if (start.type === "scratch") return { type: "scratch" };
  if (start.type === "same_as") throw err("internal", "Another task's start");
  const found = (await catalog(lib, sha256)).entries.find((e) => e.kind === start.type && e.source === start.source && e.id === start.id);
  if (!found) throw err("not_found", `That ${start.type} is no longer on this computer`);
  disabled?.refuse(found.host_path, found.name);
  if (found.digest !== start.digest) throw err("conflict", `${found.name} changed since it was listed. Look at it again before starting.`);
  return { type: start.type, name: found.name };
}

/** As `launchers::same_start`: what a task in the same project started from. */
export function sameStart(tasks: Task[], projectId: string, taskId: string, alone: boolean): StartedFrom | undefined {
  if (alone) throw err("invalid_request", "A task on its own can’t start as another task did");
  const source = tasks.find((t) => t.task_id === taskId && t.project_id === projectId);
  if (!source) throw err("not_found", "The task this was to start as is no longer in this project");
  return source.start ?? undefined;
}

/** A new one-off's project, in a new folder named `<date>-<slug>`. */
export function oneOff(lib: LauncherFiles, projects: Project[], projectId: string, place: Extract<TaskPlace, { type: "one_off" }>, now: number): Project {
  if (projects.some((p) => p.project_id === projectId)) throw err("conflict", "A one-off needs a new project id");
  const name = place.name.trim();
  if (!name) throw err("invalid_request", "Name the task");
  const date = place.date ?? new Date(now).toISOString().slice(0, 10);
  const taken = new Set(projects.map((p) => p.host_path));
  for (const path of Object.keys(lib.files[ids.rootOphio] ?? {})) taken.add(`${ONE_OFFS}/${path.split("/")[0]}`);
  const base = `${date}-${slug(name)}`;
  for (let n = 1; n <= 99; n++) {
    const host_path = `${ONE_OFFS}/${n === 1 ? base : `${base}-${n}`}`;
    if (taken.has(host_path)) continue;
    return { project_id: projectId, name, host_path, default_harness: null, default_model: null, default_effort: null, active_writer: null, vcs: null, created_at: now, one_off: true };
  }
  throw err("conflict", "There are already 99 one-offs with that name today. Choose another name.");
}

type SkillCopied = Extract<CommandResult, { kind: "setup_skill_copied" }>;

/** The person's copy of Øphio's agent or skill `id`, as
 * `CompanionHub::copy_setup_skill`. `text` is set when it was made now; a
 * copy already there is kept. */
export async function copySetupSkill(lib: LauncherFiles, id: string, sha256: (text: string) => Promise<string>): Promise<SkillCopied & { text: string | null }> {
  const found = setupFile(id);
  if (found === undefined) throw err("not_found", "Øphio has no built-in agent or skill by that name");
  const { kind, text } = found;
  const files = (lib.files[ids.rootOphio] ??= {});
  const path = copyPath(kind, id);
  const made = !(path in files);
  if (made) {
    files[path] = text;
    lib.skillBases[id] = await sha256(text);
  }
  return { kind: "setup_skill_copied", id, host_path: `${ONE_OFFS}/${path}`, file: { root_id: ids.rootOphio, path }, text: made ? text : null };
}

/** As `CompanionHub::choose_setup_skill_version`. A copy taken over by the
 * new version goes to `putAside`, for Library's Recently deleted, before it
 * is replaced. The new text when the file changed. */
export async function chooseSetupSkillVersion(lib: LauncherFiles, id: string, keepMine: boolean, sha256: (text: string) => Promise<string>, putAside: (old: string) => Promise<unknown>): Promise<string | null> {
  const found = setupFile(id);
  if (found === undefined) throw err("not_found", "Øphio has no built-in agent or skill by that name");
  const { kind, text } = found;
  const path = copyPath(kind, id);
  const builtIn = await sha256(text);
  if (!keepMine) {
    const files = (lib.files[ids.rootOphio] ??= {});
    const old = files[path];
    if (old === undefined || (await sha256(old)) !== builtIn) {
      if (old !== undefined) await putAside(old);
      files[path] = text;
      lib.skillBases[id] = builtIn;
      return text;
    }
  }
  lib.skillBases[id] = builtIn;
  return null;
}

/** A new agent in Ophio's agents folder, as `launchers::make_agent`. */
export function createAgent(lib: LauncherFiles, name: string, description: string | null | undefined): Extract<CommandResult, { kind: "agent_created" }> & { text: string } {
  const trimmed = name.trim();
  if (!/[a-z0-9]/i.test(trimmed) || trimmed.length > 100) throw err("invalid_request", "Give the agent a name of up to 100 characters with a letter or digit in it");
  const id = slug(trimmed);
  const path = `Agents/${id}.md`;
  const files = (lib.files[ids.rootOphio] ??= {});
  if (path in files) throw err("conflict", `There's already an agent called ${id}. Choose another name.`);
  const builtIn = SETUP_AGENTS[id];
  if (builtIn !== undefined) throw err("conflict", `Øphio already has an agent called ${frontmatter(builtIn).name}. Open it to make your own copy, or choose another name.`);
  const said = description?.trim() || trimmed;
  const text = `---\nname: ${id}\ndescription: ${JSON.stringify(said)}\n---\n\nDescribe who this agent is and how it works. Everything below the line above is given to the model when a task starts from it.\n`;
  files[path] = text;
  return { kind: "agent_created", id, host_path: `${AGENTS}/${id}.md`, file: { root_id: ids.rootOphio, path }, text };
}
