// SPDX-License-Identifier: Apache-2.0
//
// Reading a project's FINDINGS.md. Apart from ./findings so the CommonMark
// parser loads with the Findings screen, not with every screen that links to it.

import { useCallback, useEffect, useState } from "react";
import { fromMarkdown } from "mdast-util-from-markdown";
import { describeError, useApp, useHost, useLink } from "./context";
import { useFindingsSupported } from "./findings";
import { projectTasks } from "../state/projection";
import type { EvidenceKind, FindingVerdict } from "../protocol/types";

export type Severity = "high" | "medium" | "low";

export interface Finding {
  number: number;
  title: string;
  /** Its box is ticked: fixed, or found not to be a problem. */
  done: boolean;
  severity: Severity | null;
  /** Where in the project, as the reviewer wrote it. */
  where: string | null;
  /** Everything under the heading, as written. */
  body: string;
  /** The body less its Severity and Where lines, which are shown apart from it. */
  details: string;
  /** The fixer's last word on it: what it did, or why not. */
  note: string | null;
  /** What the second agent that checks a review's findings made of it; null until it has. */
  verdict: FindingVerdict | null;
  /** What a confirmed one rests on: running it, or reading the code. */
  evidence: EvidenceKind | null;
  /** The checker's words after its verdict: how it was shown, or why it was set aside. */
  reason: string | null;
}

/** Judged not a problem or unsettled: kept in the file, left out of Fix all. */
export const isSetAside = (f: Pick<Finding, "verdict">): boolean => f.verdict === "rejected" || f.verdict === "uncertain";

const HEADING = /^## \[( |x|X)\] (\d+)\. (.+?)\s*$/;
const NOTE = /^(Fixed|Not a problem|Not fixed):\s*(.+)$/i;
/**
 * The checker's line, one per finding: "Verdict: confirmed, reproduced — how",
 * "Verdict: confirmed, from reading the code — the path", "Verdict: rejected —
 * why" or "Verdict: uncertain — what couldn't be settled". The dash may be an
 * em or en dash or a hyphen, and spacing may vary.
 */
const VERDICT = /^Verdict\s*:\s*(confirmed|rejected|uncertain)\b(?:\s*,\s*(reproduced|from\s+reading\s+the\s+code)\b)?(?:\s*[—–:-]+\s*|\s*,\s*|\s+)?(.*?)\s*$/i;

/** A Verdict line's verdict, evidence and reason; null when the line isn't one. */
export function parseVerdict(line: string): { verdict: FindingVerdict; evidence: EvidenceKind | null; reason: string | null } | null {
  const m = VERDICT.exec(line.trim());
  if (!m) return null;
  const evidence = m[2] ? (/^reproduced$/i.test(m[2]) ? "reproduced" : "source_reasoning") : null;
  return { verdict: m[1]!.toLowerCase() as FindingVerdict, evidence, reason: m[3]?.replace(/^[\s.]+$/, "") || null };
}

type Block = { type: string; position?: { start: { line: number }; end: { line: number } }; children?: Block[] };

/** The text's lines, counted from 1, that are in a code block, fences included, wherever CommonMark finds one: in a list or a quote too. */
function codeLines(text: string): Set<number> {
  const lines = new Set<number>();
  const visit = (node: Block) => {
    if (node.type === "code" && node.position) for (let n = node.position.start.line; n <= node.position.end.line; n++) lines.add(n);
    node.children?.forEach(visit);
  };
  visit(fromMarkdown(text));
  return lines;
}

/**
 * The findings in a FINDINGS.md, in the file's order. Anything not in the
 * form a review writes is left out. Code is kept as written: a heading or
 * field in a code example is part of the example, not of the file. A fence
 * left open runs to the end of the file, as it does in any Markdown viewer.
 */
export function parseFindings(text: string): Finding[] {
  const out: Finding[] = [];
  let at: { finding: Finding; lines: { text: string; code: boolean }[] } | null = null;
  const close = () => {
    if (!at) return;
    const lines = at.lines.map((l) => (l.code ? l : { ...l, text: l.text.trimEnd() }));
    const prose = lines.filter((l) => !l.code).map((l) => l.text);
    const field = (name: string) => prose.map((l) => new RegExp(`^${name}:\\s*(.+)$`, "i").exec(l)?.[1]?.trim()).find(Boolean) ?? null;
    const severity = field("Severity")?.toLowerCase();
    const notes = prose.filter((l) => NOTE.test(l));
    // The last Verdict line counts: the checker writes one, and a later one corrects it.
    const verdicts = prose.flatMap((l) => { const v = parseVerdict(l); return v ? [v] : []; });
    const judged = verdicts.at(-1) ?? null;
    // Trim blank lines without stripping an indented code block's first line.
    const join = (keep: typeof lines) => keep.map((l) => l.text).join("\n").replace(/^\n+|\n+$/g, "");
    out.push({
      ...at.finding,
      severity: severity === "high" || severity === "medium" || severity === "low" ? severity : null,
      where: field("Where"),
      body: join(lines),
      details: join(lines.filter((l) => l.code || !(/^(Severity|Where):/i.test(l.text) || parseVerdict(l.text)))),
      note: notes.length ? notes[notes.length - 1]! : null,
      verdict: judged?.verdict ?? null,
      evidence: judged?.evidence ?? null,
      reason: judged?.reason ?? null,
    });
    at = null;
  };
  const code = codeLines(text);
  // Line endings as CommonMark counts them, so the numbers match.
  text.split(/\r\n?|\n/).forEach((line, i) => {
    if (code.has(i + 1)) {
      at?.lines.push({ text: line, code: true });
      return;
    }
    const m = HEADING.exec(line);
    if (m) {
      close();
      at = { finding: { number: Number(m[2]), title: m[3]!, done: m[1] !== " ", severity: null, where: null, body: "", details: "", note: null, verdict: null, evidence: null, reason: null }, lines: [] };
    } else if (/^#{1,2} /.test(line)) close();
    else at?.lines.push({ text: line, code: false });
  });
  close();
  return out;
}

/**
 * The project's FINDINGS.md: null text when there is none. Read when opened,
 * again whenever one of the project's tasks changes state, as reviews and
 * fixes finish, and when asked.
 */
export function useFindings(projectId: string) {
  const { session, profile } = useApp();
  const online = useLink().state === "online";
  const supported = useFindingsSupported();
  const allowed = useHost((s) => s.grants?.tasks !== false);
  const states = useHost((s) => projectTasks(s, projectId).map((t) => `${t.task_id}:${t.state}`).join());
  const [text, setText] = useState<string | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [revision, refresh] = useState(0);
  const reload = useCallback(() => refresh((n) => n + 1), []);
  useEffect(() => {
    if (!supported || !allowed || !online) return;
    let cancelled = false;
    setLoading(true);
    void session.query({ kind: "findings", project_id: projectId }).then((r) => {
      if (!cancelled) { setText(r.text); setError(null); }
    }).catch((e: unknown) => { if (!cancelled) setError(describeError(e, profile.name)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [session, profile.name, projectId, supported, allowed, online, states, revision]);
  return { text, findings: text ? parseFindings(text) : null, error, loading, supported, reload };
}
