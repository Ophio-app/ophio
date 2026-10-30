// SPDX-License-Identifier: Apache-2.0
import { useId } from "react";
import { prefs } from "../../app/prefs";
import { WARNINGS, showWarning } from "../../app/warnings";
import { Button } from "../../design/Button";
import { Section } from "../../design/ListRow";
import { useStore } from "../../state/store";

const sameList = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);

/** Every warning off in one box, and the ones turned off one at a time, to show again. */
export function Warnings() {
  const off = useStore(prefs, (s) => s.warningsOff === true);
  const hidden = useStore(prefs, (s) => (s.hiddenWarnings ?? []).filter((id) => id in WARNINGS), sameList);
  const hint = useId();
  return <Section title="Warnings" id="settings-warnings"><div className="settings-card">
    <label className="settings-check">
      <input type="checkbox" checked={off} aria-describedby={hint}
        onChange={(e) => prefs.set((p) => ({ ...p, warningsOff: e.target.checked }))} />
      Never show warnings
    </label>
    <p className="settings-meta" id={hint}>
      Actions go ahead without asking first. Approvals the computer asks for, and checks that it’s you, still ask.
    </p>
    {!off && hidden.length > 0 && <>
      <p className="settings-meta">Turned off with “Don’t show me again”:</p>
      <ul className="settings-warnings">
        {hidden.map((id) => (
          <li key={id}>
            <span>{WARNINGS[id as keyof typeof WARNINGS]}</span>
            <Button size="sm" variant="quiet" onClick={() => showWarning(id)}>Show again</Button>
          </li>
        ))}
      </ul>
    </>}
  </div></Section>;
}
