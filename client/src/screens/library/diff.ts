// SPDX-License-Identifier: Apache-2.0
import { lf } from "./model";
export interface DiffLine {
  kind: "same" | "add" | "remove";
  text: string;
}
/** A note can have more lines than a call takes arguments. */
function append<T>(out: T[], items: readonly T[]): T[] {
  for (const item of items) out.push(item);
  return out;
}
/** Bounded LCS. Very large replacements retain exact lines without a quadratic allocation. */
export function lineDiff(base: string, next: string): DiffLine[] {
  const a = lf(base).split("\n"),
    b = lf(next).split("\n");
  let start = 0,
    end = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start++;
  while (
    end < a.length - start &&
    end < b.length - start &&
    a[a.length - 1 - end] === b[b.length - 1 - end]
  )
    end++;
  const x = a.slice(start, a.length - end),
    y = b.slice(start, b.length - end);
  const out: DiffLine[] = a
    .slice(0, start)
    .map((text) => ({ kind: "same", text }));
  if (x.length * y.length > 1_000_000) {
    for (const text of x) out.push({ kind: "remove", text });
    for (const text of y) out.push({ kind: "add", text });
  } else {
    const w = y.length + 1,
      table = new Uint32Array((x.length + 1) * w);
    for (let i = x.length - 1; i >= 0; i--)
      for (let j = y.length - 1; j >= 0; j--) {
        table[i * w + j] =
          x[i] === y[j]
            ? table[(i + 1) * w + j + 1]! + 1
            : Math.max(table[(i + 1) * w + j]!, table[i * w + j + 1]!);
      }
    let i = 0,
      j = 0;
    while (i < x.length || j < y.length) {
      if (i < x.length && j < y.length && x[i] === y[j]) {
        out.push({ kind: "same", text: x[i++]! });
        j++;
      } else if (
        i < x.length &&
        (j === y.length || table[(i + 1) * w + j]! >= table[i * w + j + 1]!)
      )
        out.push({ kind: "remove", text: x[i++]! });
      else out.push({ kind: "add", text: y[j++]! });
    }
  }
  for (const text of a.slice(a.length - end)) out.push({ kind: "same", text });
  return out;
}
interface Hunk {
  from: number;
  to: number;
  lines: string[];
}
function hunks(base: string, next: string): Hunk[] {
  const out: Hunk[] = [];
  let at = 0,
    current: Hunk | null = null;
  for (const l of lineDiff(base, next)) {
    if (l.kind === "same") {
      current = null;
      at++;
      continue;
    }
    if (!current) {
      current = { from: at, to: at, lines: [] };
      out.push(current);
    }
    if (l.kind === "remove") current.to = ++at;
    else current.lines.push(l.text);
  }
  return out;
}
/** Lines both sides agree on, or a region both changed differently. */
export type MergePart =
  | { kind: "same"; lines: string[] }
  | { kind: "overlap"; mine: string[]; theirs: string[] };
const OURS = "<<<<<<< Draft on this device",
  SPLIT = "=======",
  THEIRS = ">>>>>>> Now on the computer";
const markerLine = new RegExp(`^(${OURS}|${SPLIT}|${THEIRS})\\r?$`, "gm");
function markerCounts(text: string): Map<string, number> {
  const counts = new Map<string, number>();
  for (const [, line] of text.matchAll(markerLine))
    counts.set(line!, (counts.get(line!) ?? 0) + 1);
  return counts;
}
/**
 * Whether `text` has more of the merge’s marker lines than `base`. Markdown underlines a
 * heading with "=======", so the bare separator counts only in a draft the merge marked.
 */
export function mergeMarkersLeft(text: string, base: string, marked: boolean): boolean {
  const had = markerCounts(base);
  for (const [line, n] of markerCounts(text))
    if ((marked || line !== SPLIT) && n > (had.get(line) ?? 0)) return true;
  return false;
}
/** What to keep of one overlap. `hand` keeps both sides between markers. */
export type Keep = "mine" | "theirs" | "both" | "hand";
/** Preserve independent edits; set every overlapping region apart to resolve. */
export function mergeParts(
  base: string,
  phone: string,
  computer: string,
): MergePart[] {
  if (lf(phone) === lf(computer))
    return [{ kind: "same", lines: lf(phone).split("\n") }];
  const original = lf(base).split("\n");
  const changes = [
    ...hunks(base, phone).map((h) => ({ ...h, side: "phone" })),
    ...hunks(base, computer).map((h) => ({ ...h, side: "computer" })),
  ].sort((a, b) => a.from - b.from || a.to - b.to);
  const parts: MergePart[] = [];
  const same = (lines: string[]) => {
    const last = parts.at(-1);
    if (last?.kind === "same") append(last.lines, lines);
    else if (lines.length) parts.push({ kind: "same", lines: [...lines] });
  };
  let at = 0;
  for (let i = 0; i < changes.length;) {
    const first = changes[i++]!;
    const group = [first];
    let end = first.to;
    while (i < changes.length && changes[i]!.from <= end) {
      const h = changes[i++]!;
      group.push(h);
      end = Math.max(end, h.to);
    }
    same(original.slice(at, first.from));
    const apply = (side: string) => {
      const lines: string[] = [];
      let p = first.from;
      for (const h of group.filter((h) => h.side === side)) {
        append(append(lines, original.slice(p, h.from)), h.lines);
        p = h.to;
      }
      append(lines, original.slice(p, end));
      return lines;
    };
    const p = apply("phone"),
      c = apply("computer");
    if (group.every((h) => h.side === first.side))
      same(first.side === "phone" ? p : c);
    else if (p.join("\n") === c.join("\n")) same(p);
    else {
      // Lines both sides share at the edges of a region are not in conflict.
      let head = 0,
        tail = 0;
      while (head < p.length && head < c.length && p[head] === c[head]) head++;
      while (
        tail < p.length - head &&
        tail < c.length - head &&
        p[p.length - 1 - tail] === c[c.length - 1 - tail]
      )
        tail++;
      same(p.slice(0, head));
      parts.push({
        kind: "overlap",
        mine: p.slice(head, p.length - tail),
        theirs: c.slice(head, c.length - tail),
      });
      same(p.slice(p.length - tail));
    }
    at = end;
  }
  same(original.slice(at));
  return parts;
}
/** The merged text. An overlap without a choice keeps its markers. */
export function resolveMerge(
  parts: readonly MergePart[],
  keep: readonly (Keep | undefined)[],
): string {
  let n = 0;
  return parts
    .flatMap((part) => {
      if (part.kind === "same") return part.lines;
      const k = keep[n++];
      return k === "mine"
        ? part.mine
        : k === "theirs"
          ? part.theirs
          : k === "both"
            ? [...part.mine, ...part.theirs]
            : [
                "<<<<<<< Draft on this device",
                ...part.mine,
                "=======",
                ...part.theirs,
                ">>>>>>> Now on the computer",
              ];
    })
    .join("\n");
}
