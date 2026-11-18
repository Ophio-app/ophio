// SPDX-License-Identifier: Apache-2.0
//
// Tool names as the person reads them, wherever an agent's tool is named: a
// step in a chat, a request to use one, a run's receipt.

/** Codex's own names for its kinds of step, in the plain words the computer
 *  gives their summaries. */
const PLAIN: Record<string, string> = {
  ExitPlanMode: "Plan",
  commandExecution: "Command",
  fileChange: "File change",
  webSearch: "Web search",
  imageView: "Image view",
  imageGeneration: "Image generation",
  functionCallOutput: "Tool result",
  collabAgentToolCall: "Helper agents",
  Grep: "Search files",
  Glob: "Find files",
  WebFetch: "Open a web page",
};

/** Shared with the computer so notification titles use these exact words. */
import ownTools from "../../../crates/ports/src/tool_names.json";
export const OPHIO_TOOLS: Record<string, string> = ownTools;

/** A tool's own name in words: `create_issue` reads "Create issue". */
const words = (name: string) => {
  const said = name.replace(/[_-]+/g, " ").replace(/\s+/g, " ").trim();
  return said.charAt(0).toUpperCase() + said.slice(1);
};

/** A step's tool as the person reads it: Codex's kinds of step and Ophio's
 *  own tools in plain words, another MCP server's tool as its name in words
 *  with the server after it, and an agent's built-in tool (Bash, Read) as
 *  the agent names it. */
export function toolLabel(tool: string): string {
  const plain = PLAIN[tool] ?? OPHIO_TOOLS[tool];
  if (plain) return plain;
  const mcp = /^mcp__(.+?)__(.+)$/.exec(tool);
  if (mcp) {
    const [, server, name] = mcp as unknown as [string, string, string];
    if (server === "computer") return OPHIO_TOOLS[name] ?? words(name);
    return `${words(name)} (${server.replace(/[_-]+/g, " ")})`;
  }
  // A name in snake case, as other servers' tools reach Codex.
  return /^[a-z0-9]+(?:_[a-z0-9]+)+$/.test(tool) ? words(tool) : tool;
}

/** Raw MCP tool names in a line of text (an approval titled "Use
 *  mcp__github__create_issue") said as their labels. */
export function plainToolNames(text: string): string {
  const request = /^Use (mcp__[\w-]+|Grep|Glob|WebFetch)$/.exec(text);
  if (request) return toolLabel(request[1]!);
  return text.replace(/\bmcp__[\w-]+/g, (tool: string, at: number) => {
    const said = toolLabel(tool);
    // Mid-sentence, as in "Use create issue (github)".
    return at > 0 ? said.charAt(0).toLowerCase() + said.slice(1) : said;
  });
}
