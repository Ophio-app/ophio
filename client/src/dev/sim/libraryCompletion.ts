// SPDX-License-Identifier: Apache-2.0
import type { SimHost } from "./host";
import type {
  FileContent,
  LibraryRecovery,
  LibraryRecoveryItem,
  Query,
  QueryData,
  ReusableFormat,
  ReusableFormatKind,
  RecoveryVersion,
} from "../../protocol/types";
import { libraryHash } from "./libraryWorkflows";
import { ids } from "./scenario";

type Saved = {
  item: LibraryRecoveryItem;
  base: string;
  proposed: string;
  displaced: string | null;
};
export class SimLibraryCompletion {
  readonly recoveries = new Map<string, Saved>();
  unknownVersion = false;
  private failure: string | null = null;
  constructor(private host: SimHost) {}
  seed(): string {
    const id = "sim-uncertain-save",
      path = "notes/plan.md";
    const base = "# Release plan\n\nReview the changes.\nRun the tests.\n",
      proposed = `${base}Keep the recovery copy until reviewed.\n`;
    const files = (this.host.files[ids.rootNotes] ??= {});
    files[path] = `${base}Check the Windows build.\n`;
    this.recoveries.set(id, {
      item: {
        recovery_id: id,
        root_id: ids.rootNotes,
        path,
        created_at: Date.now() - 240_000,
      },
      base,
      proposed,
      displaced: `${base}Check the Windows build.\n`,
    });
    return id;
  }
  retain(file: FileContent, text: string): string {
    const id = crypto.randomUUID();
    this.recoveries.set(id, {
      item: {
        recovery_id: id,
        root_id: file.root_id,
        path: file.path,
        created_at: Date.now(),
      },
      base: file.text,
      proposed: text,
      displaced: file.text,
    });
    return id;
  }
  clear() {
    this.recoveries.clear();
  }
  fail(
    message: string | null = "The computer could not read the recovery record.",
  ) {
    this.failure = message;
  }
  async query(
    q: Extract<
      Query,
      {
        kind:
          | "library_recovery_list"
          | "library_recovery_read"
          | "reusable_formats";
      }
    >,
  ): Promise<QueryData> {
    if (q.kind === "reusable_formats") return this.formats(q);
    if (this.failure) throw { code: "native_failure", message: this.failure };
    if (q.kind === "library_recovery_list")
      return {
        kind: q.kind,
        items: [...this.recoveries.values()]
          .map((r) => r.item)
          .filter((i) => this.host.roots.some((r) => r.root_id === i.root_id)),
      };
    this.host.library.assertRead(q.root_id);
    const r = this.recoveries.get(q.recovery_id);
    if (!r || r.item.root_id !== q.root_id)
      throw {
        code: "not_found",
        message: "Recovery copy is unavailable in this root.",
      };
    const version = async (
      text: string | null | undefined,
    ): Promise<RecoveryVersion> =>
      text == null
        ? { state: "missing" }
        : {
            state: "text",
            file: {
              root_id: r.item.root_id,
              path: r.item.path,
              text,
              revision: await libraryHash(text),
              size: new Blob([text]).size,
              has_bom: false,
              encoding: "utf8",
              newline: "lf",
              modified_at: null,
            },
          };
    const recovery: LibraryRecovery = {
      item: r.item,
      base: await version(r.base),
      proposed: await version(r.proposed),
      displaced: await version(r.displaced),
      current: await version(this.host.files[q.root_id]?.[r.item.path]),
      observed_at: Date.now(),
    };
    return { kind: q.kind, recovery };
  }
  discard(rootId: string, id: string) {
    this.host.library.assertWrite(rootId);
    const r = this.recoveries.get(id);
    if (!r || r.item.root_id !== rootId)
      throw {
        code: "not_found",
        message: "Recovery copy is unavailable in this root.",
      };
    this.recoveries.delete(id);
    return { kind: "library_recovery_discarded" as const, recovery_id: id };
  }
  private async formats(
    q: Extract<Query, { kind: "reusable_formats" }>,
  ): Promise<QueryData> {
    const draft = await this.host.libraryWorkflows.query({
      kind: "reusable_draft",
      task_id: q.task_id,
    });
    if (draft.kind !== "reusable_draft")
      throw new Error("Missing simulated task");
    const known =
      !this.unknownVersion &&
      (q.harness === "claude_code" || q.harness === "codex");
    const formats: ReusableFormat[] = (
      ["task", "skill", "subagent", "prompt"] as ReusableFormatKind[]
    ).map((kind) => {
      const available =
        kind === "task" ||
        (known &&
          (kind === "subagent" ||
            kind === (q.harness === "claude_code" ? "skill" : "prompt")));
      let text = draft.text;
      if (kind !== "task")
        text =
          kind === "subagent" && q.harness === "codex"
            ? `name = "reviewed_task"\ndescription = "Describe when to delegate to this agent."\nsandbox_mode = "read-only"\ndeveloper_instructions = ${tomlInstructions(draft.text)}\n`
            : `---\n${kind !== "prompt" ? "name: reviewed-task\n" : ""}description: Describe when to use this draft.\n${kind === "skill" ? "disable-model-invocation: true\n" : kind === "subagent" ? "tools: Read, Grep, Glob\n" : ""}---\n\n${draft.text}`;
      return {
        kind,
        availability: available
          ? { state: "available" }
          : {
              state: "unsupported",
              reason: known
                ? "No verified template for this agent and kind. Use a task draft."
                : "The installed version is unknown. Only a generic task draft is available.",
            },
        text: available ? text : null,
        suggested_name:
          kind === "subagent" && q.harness === "codex"
            ? "agent-draft.toml.md"
            : `${kind}-draft.md`,
        instructions:
          "Review before moving this draft into a native discovery folder. Saving does not activate it.",
        documentation: null,
      };
    });
    return {
      kind: q.kind,
      native_version: known
        ? q.harness === "claude_code"
          ? "2.1.280 (simulated)"
          : "0.153.2 (simulated)"
        : null,
      formats,
    };
  }
}

function tomlInstructions(text: string): string {
  const body = Array.from(text, (ch) =>
    ch === "\n" ? ch : JSON.stringify(ch).slice(1, -1),
  )
    .join("")
    .replace(/\u007f/g, "\\u007f");
  return `"""\n${body}"""`;
}
