// SPDX-License-Identifier: Apache-2.0
import type { Project, ProjectFolder, ProjectFolderListing, Query } from "../../protocol/types";

const home = "/home/sam";
const tree: Record<string, string[]> = {
  "/": ["home", "mnt"],
  "/home": ["sam"],
  [home]: ["Projects", "Documents", ".notes"],
  [`${home}/Projects`]: ["field-notes", "garden-planner", "tidewater-api", "weather-station", "weekend-site", ".archive"],
  [`${home}/Documents`]: ["Research"],
  "/mnt": ["Backup drive"],
};
export function projectFolders(q: Extract<Query, { kind: "project_folders" }>, projects: Project[]): ProjectFolderListing {
  const entry = (path: string, name = path.split("/").pop() || "/"): ProjectFolder => ({
    path, name, project_id: projects.find(p => p.host_path === path)?.project_id ?? null,
    unavailable: ["/", "/home", home].includes(path) ? "Choose the project’s own folder inside it." : null,
  });
  if (q.path === null) return { current: null, ancestors: [], next_after: null, folders: [entry(home, "Home"), entry(`${home}/Projects`, "Projects"), entry(`${home}/Documents`, "Documents"), entry("/", "Computer")] };
  if (!q.path.startsWith("/") || q.path.split("/").includes("..")) throw { code: "path_not_allowed", message: "Choose an absolute folder path without parent segments" };
  const path = q.path;
  const known = path in tree || Object.entries(tree).some(([parent, names]) => names.some(name => `${parent === "/" ? "" : parent}/${name}` === path)) || projects.some(p => p.host_path === path);
  if (!known) throw { code: "not_found", message: "This folder is no longer available." };
  const ancestors: ProjectFolder[] = [];
  let parent = path;
  while (parent !== "/") { parent = parent.slice(0, parent.lastIndexOf("/")) || "/"; ancestors.unshift(entry(parent)); }
  const names = (tree[path] ?? []).filter(n => (q.show_hidden || !n.startsWith(".")) && (!q.after || n > q.after) && n.toLowerCase().includes(q.search.toLowerCase())).sort();
  const folders = names.slice(0, 100).map(n => entry(`${path === "/" ? "" : path}/${n}`, n));
  return { current: entry(path), ancestors, folders, next_after: names.length > 100 ? folders.at(-1)!.name : null };
}
