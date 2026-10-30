// SPDX-License-Identifier: Apache-2.0
import type { AgentFileGroup, NewlineStyle, RootKind } from "../../protocol/types";

export const READ_LIMIT = 512 * 1024;
export const basename = (path: string) => path.split("/").at(-1) || path;
export const dirname = (path: string) =>
  path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "";
export const join = (dir: string, name: string) =>
  dir ? `${dir}/${name}` : name;
export const isMarkdown = (path: string) =>
  /\.(md|markdown|mdown)$/i.test(path);
export function bytes(n: number): string {
  if (n < 1024) return `${n.toLocaleString()} B`;
  if (n < 1024 * 1024)
    return `${(n / 1024).toLocaleString(undefined, { maximumFractionDigits: 1 })} KB`;
  return `${(n / (1024 * 1024)).toLocaleString(undefined, { maximumFractionDigits: 1 })} MB`;
}
export function validName(name: string): string | null {
  if (!name.trim()) return "Enter a name.";
  if (
    name !== name.trim() ||
    /[\\/:*?"<>|\u0000-\u001f\u007f-\u009f]/.test(name) ||
    name === "." ||
    name === ".." ||
    name.endsWith(".")
  )
    return "Use a single file name without slashes, reserved characters or trailing spaces.";
  // Windows drops spaces before the extension: "con .txt" is the console too.
  if (/^(con|prn|aux|nul|conin\$|conout\$|com[1-9\u00b9\u00b2\u00b3]|lpt[1-9\u00b9\u00b2\u00b3]) *(\.|$)/i.test(name))
    return "That name is reserved on Windows.";
  // Unseen, these change how the rest of the name reads.
  if (/[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/.test(name))
    return "Remove the hidden text-direction characters from this name.";
  return null;
}
export function relativeDir(path: string): string | null {
  if (!path) return null;
  if (/^[\\/]|^[a-z]:/i.test(path) || path.includes("\\"))
    return "Use a relative folder, such as docs/notes.";
  if (path.split("/").some((p) => !!validName(p)))
    return "Use folder names separated by /, without . or .. segments.";
  return null;
}
export type Filter =
  "All" | "Skills" | "Agent definitions" | "Instructions" | "Documents";
export const filters: Filter[] = [
  "All",
  "Skills",
  "Agent definitions",
  "Instructions",
  "Documents",
];
export const rootLabel: Record<RootKind, string> = {
  skills: "Skills",
  agents: "Agent definitions",
  instructions: "Instructions",
  notes: "Documents",
  project: "Project",
  drafts: "Drafts",
  agent_files: "Agent files",
};
/** Agent files by what they are for, in the order Library shows them. */
export const agentFileGroups: {
  group: AgentFileGroup;
  label: string;
  about: string;
  filter: Exclude<Filter, "All">;
}[] = [
  { group: "instructions", label: "Instructions", about: "Read at the start of every task", filter: "Instructions" },
  { group: "agents", label: "Agents", about: "Agents a task can hand work to", filter: "Agent definitions" },
  { group: "skills", label: "Skills", about: "What agents can do when asked", filter: "Skills" },
  { group: "commands", label: "Commands", about: "Prompts sent by name", filter: "Documents" },
  { group: "rules", label: "Rules and styles", about: "How agents write and work", filter: "Documents" },
  { group: "memory", label: "Memory", about: "What agents keep to remember", filter: "Documents" },
];
export function category(
  path: string,
  kind?: RootKind,
): Exclude<Filter, "All"> {
  if (
    /^(AGENTS|CLAUDE|GEMINI)\.md$/i.test(basename(path)) ||
    kind === "instructions"
  )
    return "Instructions";
  if (
    kind === "skills" ||
    /(^|\/)skills?\//i.test(path) ||
    basename(path).toUpperCase() === "SKILL.MD"
  )
    return "Skills";
  if (kind === "agents" || /(^|\/)(\.claude\/)?agents\//i.test(path))
    return "Agent definitions";
  return "Documents";
}
export interface Crumb {
  label: string;
  path: string;
}
export function breadcrumbs(
  label: string,
  path: string,
): { visible: Crumb[]; collapsed: Crumb[] } {
  const parts = path.split("/").filter(Boolean);
  const all = [
    { label, path: "" },
    ...parts.map((p, i) => ({
      label: p,
      path: parts.slice(0, i + 1).join("/"),
    })),
  ];
  return all.length > 2
    ? { visible: [all[0]!, all.at(-1)!], collapsed: all.slice(1, -1) }
    : { visible: all, collapsed: [] };
}
export const lf = (text: string) => text.replace(/\r\n/g, "\n");
/**
 * Text to save. The editor splits on every line break and joins with "\n".
 * Given the note it was opened from, a note whose own breaks that would
 * change (CRLF mixed with LF, or a lone CR) gets them back.
 */
export function forSave(text: string, newline: NewlineStyle, base?: string): string {
  if (base !== undefined && (newline === "mixed" || /\r(?!\n)/.test(base)))
    return withBreaks(base, text);
  return newline === "cr_lf"
    ? lf(text).replace(/\n/g, "\r\n")
    : newline === "lf"
      ? lf(text)
      : text;
}
/** Lines before and after the edited region keep their own breaks; lines
 * inside it take the break next to the region. */
function withBreaks(base: string, text: string): string {
  const parts = base.split(/(\r\n|\r|\n)/),
    old = parts.filter((_, i) => i % 2 === 0),
    breaks = parts.filter((_, i) => i % 2 === 1),
    next = text.split(/\r\n|\r|\n/);
  let start = 0,
    end = 0;
  while (start < old.length && start < next.length && old[start] === next[start]) start++;
  while (
    end < old.length - start &&
    end < next.length - start &&
    old[old.length - 1 - end] === next[next.length - 1 - end]
  )
    end++;
  const near = breaks[start - 1] ?? breaks[old.length - end - 1] ?? breaks[0] ?? "\n";
  let out = next[0]!;
  for (let i = 1; i < next.length; i++) {
    const fromEnd = next.length - i;
    const before = i < start ? breaks[i - 1] : fromEnd <= end ? breaks[old.length - fromEnd - 1] : near;
    out += (before ?? near) + next[i];
  }
  return out;
}
export function copyName(path: string): string {
  const name = basename(path),
    dot = name.lastIndexOf(".");
  return dot > 0
    ? `${name.slice(0, dot)} (copy)${name.slice(dot)}`
    : `${name} (copy)`;
}
