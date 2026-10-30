// SPDX-License-Identifier: Apache-2.0

/** One row of a rendered diff. `gap` stands for unchanged lines left out. */
export type DiffRowKind = "file" | "hunk" | "same" | "add" | "remove" | "note" | "text" | "gap";
export interface DiffRow {
  kind: DiffRowKind;
  text: string;
}
export interface Segment {
  text: string;
  changed: boolean;
}

const HUNK = /^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@/;
// Titles the companion writes around reported edits. A written file or
// notebook cell is its content, not a patch, so its lines carry no signs.
const EDIT_SECTION = /^(Native edit excerpt|Next tool edit):$/;
const PLAIN_SECTION = /^(Written content \(previous content not reported\)|Notebook cell .*):$/;

/**
 * Types each line of reported diff text. Hunk headers are counted, so a
 * removed line that begins "--- " is not taken for a file header.
 */
export function parseUnifiedDiff(text: string): DiffRow[] {
  const lines = text.split(/\r?\n/);
  if (lines.at(-1) === "") lines.pop();
  const rows: DiffRow[] = [];
  let old = 0,
    next = 0,
    header = false,
    plain = false,
    section = true;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!,
      sign = line[0];
    if (old > 0 || next > 0) {
      if (sign === "\\") rows.push({ kind: "note", text: line });
      else if (sign === "+") next--, rows.push({ kind: "add", text: line.slice(1) });
      else if (sign === "-") old--, rows.push({ kind: "remove", text: line.slice(1) });
      else old--, next--, rows.push({ kind: "same", text: line.slice(1) });
      continue;
    }
    if (line === "Next tool edit:" && lines[i - 1] === "") {
      if (rows.at(-1)?.kind === "text" && rows.at(-1)?.text === "") rows.pop();
      plain = false;
      section = true;
      rows.push({ kind: "file", text: line });
      continue;
    }
    if (section && (EDIT_SECTION.test(line) || PLAIN_SECTION.test(line))) {
      plain = PLAIN_SECTION.test(line);
      section = false;
      rows.push({ kind: "file", text: line });
      continue;
    }
    section = false;
    const hunk = plain ? null : HUNK.exec(line);
    if (plain) rows.push({ kind: "text", text: line });
    else if (hunk) {
      old = hunk[1] === undefined ? 1 : Number(hunk[1]);
      next = hunk[2] === undefined ? 1 : Number(hunk[2]);
      header = false;
      rows.push({ kind: "hunk", text: line });
    } else if (line.startsWith("diff ")) {
      header = true;
      rows.push({ kind: "file", text: line });
    } else if (line.startsWith("--- ") && lines[i + 1]?.startsWith("+++ ")) {
      header = false;
      rows.push({ kind: "file", text: line }, { kind: "file", text: lines[++i]! });
    } else if (header && !/^[-+ @\\]/.test(line)) rows.push({ kind: "file", text: line });
    else if (sign === "+") rows.push({ kind: "add", text: line.slice(1) });
    else if (sign === "-") rows.push({ kind: "remove", text: line.slice(1) });
    else if (sign === " ") rows.push({ kind: "same", text: line.slice(1) });
    else if (sign === "\\") rows.push({ kind: "note", text: line });
    else rows.push({ kind: "text", text: line });
  }
  return rows;
}

// Word comparison is a refinement, so it stays small: long lines and work
// beyond the budget keep whole-line colour only.
const WORD_LINE_LIMIT = 400;
const WORD_BUDGET = 1_000_000;
const TOKEN = /[\p{L}\p{N}_]+|\s+|[^\p{L}\p{N}_\s]/gu;

/**
 * Marks the changed words of removed lines that were edited into added ones.
 * In each run of removals and the additions after it, a removed line pairs,
 * in order, with the first addition similar enough to compare. Rows are
 * keyed by index; unpaired and rewritten lines are absent.
 */
export function wordChanges(rows: readonly DiffRow[]): Map<number, Segment[]> {
  const out = new Map<number, Segment[]>();
  const tokens = (i: number) => {
    const text = rows[i]!.text;
    return text && text.length <= WORD_LINE_LIMIT ? text.match(TOKEN)! : null;
  };
  let budget = WORD_BUDGET;
  for (let i = 0; i < rows.length; ) {
    if (rows[i]!.kind !== "remove") {
      i++;
      continue;
    }
    let added = i;
    while (rows[added]?.kind === "remove") added++;
    let end = added;
    while (rows[end]?.kind === "add") end++;
    const after = Array.from({ length: end - added }, (_, k) => tokens(added + k));
    for (let from = added; i < added; i++) {
      const x = tokens(i);
      for (let a = from; x && a < end; a++) {
        const y = after[a - added];
        if (!y || rows[i]!.text === rows[a]!.text) continue;
        budget -= x.length * y.length;
        if (budget < 0) return out;
        const pair = compareWords(x, y);
        if (pair) {
          out.set(i, pair[0]);
          out.set(a, pair[1]);
          from = a + 1;
          break;
        }
      }
    }
    i = end;
  }
  return out;
}

function compareWords(x: string[], y: string[]): [Segment[], Segment[]] | null {
  const w = y.length + 1,
    table = new Uint16Array((x.length + 1) * w);
  for (let i = x.length - 1; i >= 0; i--)
    for (let j = y.length - 1; j >= 0; j--)
      table[i * w + j] =
        x[i] === y[j]
          ? table[(i + 1) * w + j + 1]! + 1
          : Math.max(table[(i + 1) * w + j]!, table[i * w + j + 1]!);
  const keepX = new Array<boolean>(x.length).fill(false),
    keepY = new Array<boolean>(y.length).fill(false);
  for (let i = 0, j = 0; i < x.length && j < y.length; ) {
    if (x[i] === y[j]) (keepX[i++] = true), (keepY[j++] = true);
    else if (table[(i + 1) * w + j]! >= table[i * w + j + 1]!) i++;
    else j++;
  }
  const shared = x.filter((t, i) => keepX[i] && t.trim()).join("").length,
    shorter = Math.min(x.join("").replace(/\s/g, "").length, y.join("").replace(/\s/g, "").length);
  // A line that is mostly rewritten reads better as a whole-line change.
  if (shared * 2 < shorter) return null;
  return [segments(x, keepX), segments(y, keepY)];
}

function segments(tokens: string[], keep: boolean[]): Segment[] {
  const out: Segment[] = [];
  tokens.forEach((text, i) => {
    // Spaces between two changed words belong to the change.
    const between = i > 0 && i < tokens.length - 1 && !keep[i - 1] && !keep[i + 1];
    const changed = !keep[i] || (between && !text.trim());
    const last = out.at(-1);
    if (last && last.changed === changed) last.text += text;
    else out.push({ text, changed });
  });
  return out;
}
