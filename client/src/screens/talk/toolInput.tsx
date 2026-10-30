// SPDX-License-Identifier: Apache-2.0
import { Markdown } from "../../design/Markdown";

export type ReadableInput = { text: string; kind: "code" | "text" | "plan" };

const SHELL = ["bash", "shell", "shell_command", "exec_command", "commandexecution"];
const EDIT = ["edit", "multiedit", "edit_file", "write", "write_file", "notebookedit", "filechange"];
const toolName = (tool: string) => tool.replace(/^functions\./, "").toLowerCase();

/** Whether a step ran a command or changed files, by the tool names the
 *  harnesses report; other tools only read or talk. */
export function toolKind(tool: string): "command" | "edit" | null {
  const name = toolName(tool);
  return SHELL.includes(name) ? "command" : EDIT.includes(name) ? "edit" : null;
}

/** Native inputs stay available in Details; summaries only use known fields. */
export function toolInput(tool: string, detail: string): ReadableInput | null {
  let input: unknown;
  try { input = JSON.parse(detail); } catch { input = detail; }
  const name = toolName(tool);
  if (typeof input === "string") {
    if (name === "exitplanmode") return { text: input, kind: "plan" };
    if (SHELL.includes(name)) return { text: input, kind: "code" };
    return null;
  }
  if (!input || typeof input !== "object" || Array.isArray(input)) return null;
  const fields = input as Record<string, unknown>;
  const field = (...keys: string[]) => keys.map(k => fields[k]).find((v): v is string => typeof v === "string" && !!v.trim());
  const result = (text: string | undefined, kind: ReadableInput["kind"] = "text") => text ? { text, kind } : null;
  if (name === "exitplanmode") return result(field("plan"), "plan");
  if (SHELL.includes(name)) {
    const command = field("command", "cmd") ?? (Array.isArray(fields.command) && fields.command.every(v => typeof v === "string") ? fields.command.join(" ") : undefined);
    return result(command, "code");
  }
  if (name === "filechange" && Array.isArray(fields.changes)) {
    const paths = fields.changes.flatMap(v => v && typeof v === "object" && "path" in v && typeof v.path === "string" ? [v.path] : []);
    return result(paths.join("\n"), "code");
  }
  if (["read", "read_file", ...EDIT].includes(name)) return result(field("file_path", "path", "notebook_path"), "code");
  if (["grep", "glob", "search", "file_search", "list_files"].includes(name)) {
    const pattern = field("pattern", "query");
    const path = field("path", "directory", "cwd");
    return result(pattern ? `${pattern}${path ? ` · ${path}` : ""}` : path, "code");
  }
  if (["task", "agent", "spawn_agent", "collabagenttoolcall"].includes(name)) return result(field("description", "prompt", "message"));
  return null;
}

export function ToolInput({ tool, detail }: { tool: string; detail: string }) {
  const readable = toolInput(tool, detail);
  return <>
    {readable?.kind === "plan" ? <Markdown text={readable.text} /> : readable && <pre className="talk-steps__detail">{readable.kind === "code" ? <code>{readable.text}</code> : readable.text}</pre>}
    {readable?.kind !== "plan" && <details className="tool-input-details"><summary>Details</summary><pre className="talk-steps__detail">{detail}</pre></details>}
  </>;
}
