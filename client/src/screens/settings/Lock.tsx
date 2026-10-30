// SPDX-License-Identifier: Apache-2.0
import { useApp } from "../../app/context";
import { Section } from "../../design/ListRow";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { appLock } from "../../platform/lock";
import type { AppLockState } from "../../host/link";
import { useStore } from "../../state/store";

export function Lock() {
  const controller = appLock(useApp().platform);
  const state = useStore(controller, (s) => s);
  return <Section title="Lock" id="settings-lock"><div className="settings-card settings-stack">
    <label className="settings-check"><input type="checkbox" checked={state.enabled} disabled={!state.available || state.busy}
      onChange={(e) => void controller.configure(e.target.checked, state.relockMinutes)} />Lock the app</label>
    {!state.available ? <Notice>{state.reason}</Notice> : <p className="settings-meta">Use your device’s biometrics or screen lock. Unlocking never approves a request or gives control of a computer.</p>}
    {state.enabled && <><label className="field"><span className="field__label">Lock again</span><select className="field__input"
      value={state.relockMinutes} disabled={state.busy} onChange={(e) => void controller.configure(true, Number(e.target.value) as AppLockState["relockMinutes"])}>
      <option value={0}>On leaving the app</option><option value={1}>After 1 minute away</option><option value={5}>After 5 minutes away</option><option value={15}>After 15 minutes away</option>
    </select></label><Button onClick={() => void controller.lock()}>Lock now</Button></>}
    {state.error && <Notice tone="warn">{state.error}</Notice>}
  </div></Section>;
}
