// SPDX-License-Identifier: Apache-2.0
import { Chip } from "../../design/Chip";
import type { Mark } from "../../protocol/types";
export function TargetChips({
  marks,
  targets,
  general,
  row,
  onChange,
}: {
  marks: Mark[];
  targets: string[];
  general: boolean;
  /** One line that scrolls sideways, as over the keyboard. */
  row?: boolean;
  onChange(ids: string[], general: boolean): void;
}) {
  return (
    <fieldset className="an-targets">
      <legend>Applies to</legend>
      <div className="an-wrap" data-scroll-x={row || undefined}>
        {marks
          .filter((m) => !m.deleted)
          .map((m) => (
            <Chip
              key={m.mark_id}
              selected={targets.includes(m.mark_id)}
              aria-label={`Target ${m.label}`}
              onPointerDown={(e) => e.preventDefault()}
              onClick={() =>
                onChange(
                  targets.includes(m.mark_id)
                    ? targets.filter((id) => id !== m.mark_id)
                    : [...targets, m.mark_id],
                  false,
                )
              }
            >
              {m.label} <span className="an-chip-source">{m.source_key}</span>
            </Chip>
          ))}
        <Chip
          selected={general}
          onPointerDown={(e) => e.preventDefault()}
          onClick={() => onChange([], true)}
        >
          General comment
        </Chip>
      </div>
    </fieldset>
  );
}
