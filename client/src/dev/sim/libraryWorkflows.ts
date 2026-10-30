// SPDX-License-Identifier: Apache-2.0
import type {
  AgentFile,
  Command,
  CommandResult,
  FileContent,
  InstructionSource,
  LibraryTrashItem,
  Query,
  QueryData,
  UploadCollision,
} from "../../protocol/types";
import type { SimHost } from "./host";
import { uuidv7 } from "../../protocol/ids";
import { agentFilePlaces } from "./scenario";

export const libraryHash = async (text: string) =>
  [
    ...new Uint8Array(
      await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)),
    ),
  ]
    .map((v) => v.toString(16).padStart(2, "0"))
    .join("");
const fail = (message: string): never => {
  throw { code: "conflict", message };
};
// Folders above a host path, from the filesystem root down.
const ancestors = (path: string): string[] =>
  path
    .split("/")
    .slice(0, -1)
    .map((_, i, parts) => parts.slice(0, i + 1).join("/") || "/");
export class SimLibraryWorkflows {
  readonly trash = new Map<string, { item: LibraryTrashItem; text: string }>();
  readonly collisions = new Map<string, UploadCollision>();
  private baseline = new Map<string, string>();
  constructor(private host: SimHost) {
    host.roots.push({
      root_id: "sim-drafts",
      label: "Task drafts",
      host_path: "/home/sam/Drafts",
      kind: "drafts",
      writable: true,
      project_id: null,
      harness: null,
    });
    host.files["sim-drafts"] = {};
    for (const [root, files] of Object.entries(host.files))
      for (const [path, text] of Object.entries(files))
        this.baseline.set(`${root}/${path}`, text);
  }
  private file(item: LibraryTrashItem, text: string): FileContent {
    return {
      root_id: item.root_id,
      path: item.path,
      text,
      revision: item.revision,
      size: item.size,
      modified_at: item.deleted_at,
      encoding: "utf8",
      has_bom: false,
      newline: "lf",
    };
  }
  async deleted(
    root: string,
    path: string,
    text: string,
  ): Promise<CommandResult> {
    const item: LibraryTrashItem = {
      trash_id: uuidv7(),
      root_id: root,
      path,
      revision: await libraryHash(text),
      size: new Blob([text]).size,
      deleted_at: Date.now(),
    };
    this.trash.set(item.trash_id, { item, text });
    return { kind: "library_deleted", item };
  }
  async query(
    q: Extract<
      Query,
      {
        kind:
          | "library_trash"
          | "library_trash_read"
          | "agent_files"
          | "instructions"
          | "reusable_draft"
          | "upload_destination";
      }
    >,
  ): Promise<QueryData> {
    switch (q.kind) {
      case "library_trash":
        return {
          kind: q.kind,
          items: [...this.trash.values()].map((x) => x.item),
        };
      case "agent_files": {
        const files: AgentFile[] = [];
        for (const p of agentFilePlaces) {
          const text = this.host.files[p.rootId]?.[p.path];
          if (text === undefined) continue;
          const fm = /^---\n([\s\S]*?)\n---/.exec(text)?.[1] ?? "",
            within = p.path.replace(/^(projects\/[^/]+\/memory|commands|Agents)\//, ""),
            folder = within.split("/").at(-2);
          files.push({
            root_id: p.rootId,
            path: p.path,
            shown: within,
            title:
              /^name:\s*(.+)$/m.exec(fm)?.[1] ??
              (p.group === "skills" ? (folder ?? null) : null),
            description: /^description:\s*(.+)$/m.exec(fm)?.[1] ?? null,
            group: p.group,
            scope: p.scope,
            everywhere: !p.projectId,
            project_id: p.projectId ?? null,
            harness: p.scope === "Claude Code" || p.scope.endsWith(" plugin") || p.path === "CLAUDE.md" ? "claude_code" : p.path === "AGENTS.md" ? "codex" : null,
            size: new TextEncoder().encode(text).length,
            modified_at: null,
            host_path: `${this.host.roots.find((r) => r.root_id === p.rootId)!.host_path}/${p.path}`,
          });
        }
        for (const file of files) file.disable = this.host.disabled.file(file);
        return { kind: q.kind, files, truncated: false };
      }
      case "library_trash_read": {
        const saved = this.trash.get(q.trash_id);
        if (!saved) return fail("Deleted file is no longer available.");
        this.host.library.assertRead(saved.item.root_id);
        return { kind: q.kind, file: this.file(saved.item, saved.text) };
      }
      case "reusable_draft": {
        const task = this.host.tasks.find((t) => t.task_id === q.task_id);
        return {
          kind: q.kind,
          text: `# ${task?.title ?? "Reusable task"}\n\n## Fill in before starting\n\n- Project: [fill in]\n- Inputs: [fill in]\n\n## Prompt\n\n${task?.title ?? "[Describe the work]"}\n\n## Boundaries\n\n[Name allowed changes and exclusions.]\n\n## Checks\n\n- [ ] [Add a verification command.]\n\nSaving this draft does not activate it.\n`,
        };
      }
      case "instructions": {
        const root = this.host.roots.find((r) => r.project_id === q.project_id);
        const task = this.host.tasks.find((t) => t.task_id === q.task_id);
        const sources: InstructionSource[] = [],
          harness = task?.harness ?? "claude_code";
        // Like the computer, check the usual names in every folder above the
        // project; none of them exist on the simulated computer.
        if (root && harness === "claude_code")
          for (const dir of ancestors(root.host_path))
            for (const name of [
              "CLAUDE.md",
              ".claude/CLAUDE.md",
              "CLAUDE.local.md",
            ])
              sources.push({
                label: name,
                root_id: null,
                path: null,
                host_path: `${dir === "/" ? "" : dir}/${name}`,
                harness,
                scope: "directory",
                state: "missing",
                loaded_reported: false,
                current_revision: null,
                revision_at_start: null,
              });
        if (root)
          for (const path of [
            task?.harness === "codex" ? "AGENTS.md" : "CLAUDE.md",
            "docs/guides/release.md",
            "docs/retry-policy.md",
          ]) {
            const text = this.host.files[root.root_id]?.[path],
              start = this.baseline.get(`${root.root_id}/${path}`);
            sources.push({
              label: path,
              root_id: root.root_id,
              path,
              host_path: `${root.host_path}/${path}`,
              harness,
              scope: path.includes("/") ? "directory" : "project",
              state:
                start !== text
                  ? "changed"
                  : path === "docs/guides/release.md"
                    ? "loaded"
                    : "discovered",
              loaded_reported: path === "docs/guides/release.md",
              current_revision:
                text === undefined ? null : await libraryHash(text),
              revision_at_start:
                start === undefined ? null : await libraryHash(start),
              size: text === undefined ? undefined : new Blob([text]).size,
            });
          }
        // As the computer lists a Claude Code project's committed settings
        // before the owner turns them on: by existence, outside the Library.
        const project = this.host.projects.find(
          (p) => p.project_id === q.project_id,
        );
        if (project && task?.harness !== "codex")
          sources.push({
            label: "Project settings",
            root_id: null,
            path: null,
            host_path: `${project.host_path}/.claude/settings.json`,
            harness: "claude_code",
            scope: "settings",
            state: "not_applied",
            loaded_reported: false,
            current_revision: null,
            revision_at_start: null,
          });
        for (const source of sources) if (source.host_path && this.host.disabled.has(source.host_path) && source.state !== "missing") {
          source.disabled = true; source.state = "not_applied";
        }
        return { kind: q.kind, sources };
      }
      case "upload_destination": {
        const dest = q.destination;
        const root =
          dest.kind === "project_dir"
            ? this.host.roots.find((r) => r.project_id === dest.project_id)
            : undefined;
        const dir =
          dest.kind === "project_dir" && dest.dir ? `${dest.dir}/` : "";
        const files = root ? (this.host.files[root.root_id] ?? {}) : {};
        const occupied = (name: string) =>
          files[`${dir}${name}`] !== undefined ||
          this.host.transfers.some(
            (t) =>
              t.state.state === "completed" &&
              JSON.stringify(t.destination) === JSON.stringify(dest) &&
              (dest.kind !== "inbox" || t.destination_folder === this.host.host.inbox_path) &&
              t.name === name,
          );
        const exists = dest.kind !== "artifact" && occupied(q.name);
        let name = q.name;
        for (let n = 1; occupied(name); n++)
          name = q.name.replace(/(\.[^.]+)?$/, ` (${n})$1`);
        const old = files[`${dir}${q.name}`];
        return {
          kind: q.kind,
          check: {
            exists,
            keep_both_name: name,
            replace_revision:
              root?.writable && old !== undefined
                ? await libraryHash(old)
                : null,
            replace_unavailable:
              "Replacement needs a writable project Library root.",
          },
        };
      }
    }
  }
  async command(
    c: Extract<
      Command,
      { command: "library_restore" | "create_reusable_draft" }
    >,
  ): Promise<CommandResult> {
    if (c.command === "library_restore") {
      const saved = this.trash.get(c.trash_id);
      if (!saved) return fail("Deleted file is no longer available.");
      this.host.library.assertWrite(saved.item.root_id);
      if (saved.item.revision !== c.base_revision)
        return fail("Deleted file revision changed.");
      const path = c.copy_path ?? saved.item.path,
        files = this.host.files[saved.item.root_id];
      if (!files || files[path] !== undefined)
        return fail(
          "A file already exists here. Restore as a copy with a different name.",
        );
      files[path] = saved.text;
      if (!c.copy_path) this.trash.delete(c.trash_id);
      this.host.emit({
        type: "library_file_changed",
        root_id: saved.item.root_id,
        path,
        revision: saved.item.revision,
      });
      return {
        kind: "library_restored",
        root_id: saved.item.root_id,
        path,
        revision: saved.item.revision,
      };
    }
    this.host.library.assertWrite(c.root_id);
    if (
      !this.host.roots.some(
        (r) => r.root_id === c.root_id && r.kind === "drafts",
      )
    )
      return fail("Choose a Drafts root.");
    const files = this.host.files[c.root_id];
    if (!files || files[c.path] !== undefined)
      return fail("A file already exists here.");
    files[c.path] = c.text;
    this.host.emit({
      type: "library_file_changed",
      root_id: c.root_id,
      path: c.path,
      revision: await libraryHash(c.text),
    });
    return { kind: "draft_created", root_id: c.root_id, path: c.path };
  }
}
