// SPDX-License-Identifier: Apache-2.0

import { useMemo } from "react";
import { parseUnifiedDiff, wordChanges, type DiffRow, type DiffRowKind } from "./unifiedDiff";

const SIGN: Partial<Record<DiffRowKind, string>> = { add: "+", remove: "−" };
const SPOKEN: Partial<Record<DiffRowKind, string>> = { add: "Added: ", remove: "Removed: " };

/**
 * Diff rows that wrap on a narrow screen. Signs sit in their own column, so
 * they stay beside a wrapped line and colour is never the only cue.
 */
export function DiffLines({ rows, label }: { rows: readonly DiffRow[]; label: string }) {
  const words = useMemo(() => wordChanges(rows), [rows]);
  return (
    <div className="diff" role="region" aria-label={label}>
      {rows.map((row, i) =>
        row.kind === "gap" ? (
          <p key={i} className="diff__gap">… Unchanged lines …</p>
        ) : (
          <div key={i} className={`diff__line diff__line--${row.kind}`}>
            <span className="diff__sign" aria-hidden>{SIGN[row.kind]}</span>
            <code className="diff__text">
              {SPOKEN[row.kind] && <span className="visually-hidden">{SPOKEN[row.kind]}</span>}
              {row.text === "" ? (
                <span className="visually-hidden">blank line</span>
              ) : (
                words.get(i)?.map((s, j) =>
                  s.changed ? <span key={j} className="diff__word">{s.text}</span> : s.text,
                ) ?? row.text
              )}
            </code>
          </div>
        ),
      )}
    </div>
  );
}

/** Unified diff text, such as an agent's reported edit, as typed rows. */
export function UnifiedDiff({ text, label }: { text: string; label: string }) {
  const rows = useMemo(() => parseUnifiedDiff(text), [text]);
  return <DiffLines rows={rows} label={label} />;
}
