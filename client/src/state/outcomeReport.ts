// SPDX-License-Identifier: Apache-2.0
//
// Agents end a turn's last message with a fenced `ophio-outcome` block: what
// the turn came to, for Ophio to act on. The computer keeps it with the run
// and takes it out of the run's summary; the conversation still holds the
// message as the agent wrote it, so it is taken out here before anyone reads
// it. Mirrors `reported::take` on the computer. The other blocks agents
// report to Ophio in, like a review's findings, are data for it alone and
// go the same way.

const KINDS = ["ophio-outcome", "ophio-findings", "ophio-triage", "ophio-review", "ophio-start"];

const ours = (info: string) =>
  KINDS.some((kind) => {
    if (!info.startsWith(kind)) return false;
    const rest = info.slice(kind.length).trim();
    return rest === "" || rest.toLowerCase() === "json";
  });

/** Whether `info`, still arriving, may be the start of one. */
const starts = (info: string) => KINDS.some((kind) => info.length < kind.length && kind.startsWith(info));

/** A line that opens or closes a fence: its character, how many, and what follows them. */
function fenceLine(line: string): { c: string; n: number; info: string } | null {
  const rest = line.replace(/^ +/, "");
  if (line.length - rest.length > 3) return null;
  const c = rest[0];
  if (c !== "`" && c !== "~") return null;
  let n = 0;
  while (rest[n] === c) n++;
  return n >= 3 ? { c, n, info: rest.slice(n).trim() } : null;
}

/**
 * `text` without its blocks for Ophio: those a line opens outside any other
 * fence, not one quoted inside another block or mentioned in passing. One
 * left open runs to the end, so a message still streaming never shows its
 * report, nor the first line of one still being written.
 */
export function withoutOutcome(text: string): string {
  if (!text.includes("```")) return text;
  const lines = text.split("\n");
  const kept: string[] = [];
  let found = false;
  let open: { c: string; n: number; ours: boolean } | null = null;
  lines.forEach((line, i) => {
    const fence = fenceLine(line);
    if (open) {
      const closes = !!fence && fence.c === open.c && fence.n >= open.n && fence.info === "";
      if (!open.ours) kept.push(line);
      if (closes) open = null;
      return;
    }
    if (fence && fence.c === "`" && ours(fence.info)) {
      open = { c: fence.c, n: fence.n, ours: true };
      found = true;
      return;
    }
    // The last line, still arriving, may be the start of one.
    if (fence && fence.c === "`" && i === lines.length - 1 && starts(fence.info)) {
      found = true;
      return;
    }
    kept.push(line);
    if (fence) open = { c: fence.c, n: fence.n, ours: false };
  });
  return found ? kept.join("\n").trimEnd() : text;
}
