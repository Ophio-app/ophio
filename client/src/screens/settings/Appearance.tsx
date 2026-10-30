// SPDX-License-Identifier: Apache-2.0
import { useId } from "react";
import { prefs, type Theme } from "../../app/prefs";
import { Segmented } from "../../design/Chip";
import { Section } from "../../design/ListRow";
import { useStore } from "../../state/store";

export function Appearance() {
  const theme = useStore(prefs, (s) => s.theme);
  const reduceMotion = useStore(prefs, (s) => s.reduceMotion);
  const motionHint = useId();
  return <Section title="Appearance" id="settings-appearance"><div className="settings-card">
    <Segmented<Theme> label="Appearance" value={theme} onChange={(theme) => prefs.set((p) => ({ ...p, theme }))}
      options={[{ value: "system", label: "System" }, { value: "light", label: "Light" }, { value: "dark", label: "Dark" }]} />
    <label className="settings-check">
      <input type="checkbox" checked={reduceMotion} aria-describedby={motionHint}
        onChange={e => prefs.set(p => ({ ...p, reduceMotion: e.target.checked }))} />
      Reduce motion
    </label>
    <p className="settings-meta" id={motionHint}>Keep things still. Your system’s reduced-motion setting is always respected.</p>
  </div></Section>;
}
