// SPDX-License-Identifier: Apache-2.0
//
// Ask, Plan or Work for a new task. The choice says what the person wants;
// the line under it (in Review) says what the computer actually enforces. A
// mode the agent can't restrict stays visible with the reason and is never
// quietly swapped for Work.

import { Hammer, ListChecks, MessageCircleQuestion, type LucideIcon } from "lucide-react";
import { useId } from "react";
import { availabilityText } from "../../design/Notice";
import type { HarnessInfo, TaskMode } from "../../protocol/types";
import { modeSupport } from "./intent";

export const MODES: { mode: TaskMode; label: string; icon: LucideIcon; does: string }[] = [
  { mode: "ask", label: "Ask", icon: MessageCircleQuestion, does: "Answers from the project. Changes nothing." },
  { mode: "plan", label: "Plan", icon: ListChecks, does: "Looks into it and proposes a plan. Changes nothing." },
  { mode: "work", label: "Work", icon: Hammer, does: "Edits files and runs commands." },
];

export function modeLabel(mode: TaskMode | undefined): string {
  return MODES.find((m) => m.mode === (mode ?? "work"))!.label;
}

/** `only`: what the task asks for runs in one mode alone, and why. */
export function ModePicker({ harness, value, onChange, only }: { harness: HarnessInfo | undefined; value: TaskMode; onChange: (m: TaskMode) => void; only?: { mode: TaskMode; why: string } }) {
  const id = useId();
  const offs = MODES.map((m) => {
    if (only && m.mode !== only.mode) return { ok: false, label: "Not for this task", detail: only.why };
    const { availability } = modeSupport(harness, m.mode);
    return availability.state === "available" ? null : availabilityText(availability);
  });
  // Ask and Plan are often off for one reason; it reads once, under the modes.
  const reasons = offs.flatMap((o) => (o?.detail ? [o.detail] : []));
  const shared = reasons.length > 1 && reasons.every((r) => r === reasons[0]) ? reasons[0]! : null;
  return (
    <fieldset className="modes">
      <legend className="visually-hidden">Mode</legend>
      {MODES.map((m, i) => {
        const off = offs[i]!;
        const on = value === m.mode;
        const Icon = m.icon;
        return (
          <label key={m.mode} className={["mode", on && "is-on", off && "is-off"].filter(Boolean).join(" ")}>
            <input
              type="radio"
              className="mode__input"
              name={id}
              value={m.mode}
              checked={on}
              disabled={off !== null}
              onChange={() => onChange(m.mode)}
              aria-labelledby={`${id}-${m.mode}-name`}
              aria-describedby={`${id}-${m.mode}-does${off && shared ? ` ${id}-why` : ""}`}
            />
            <span className="mode__icon" aria-hidden>
              <Icon strokeWidth={2.1} />
            </span>
            <span className="mode__text">
              <span className="mode__name" id={`${id}-${m.mode}-name`}>
                {m.label}
                {off && <span className="mode__state"> · {off.label}</span>}
              </span>
              <span className="mode__does" id={`${id}-${m.mode}-does`}>
                {off && !shared ? (off.detail ?? m.does) : m.does}
              </span>
            </span>
            <span className="mode__dot" aria-hidden />
          </label>
        );
      })}
      {shared && <p className="modes__why" id={`${id}-why`}>{shared}</p>}
    </fieldset>
  );
}
