// SPDX-License-Identifier: Apache-2.0
import { useEffect, useId, useMemo } from "react";
import { useApp } from "../app/context";
import { announce } from "../design/Announcer";
import { Button } from "../design/Button";
import { Sheet } from "../design/Sheet";
import { alertsPreset, backgroundAlerts, type Alerts } from "../platform/alerts";
import { notificationDetail } from "../platform/notifications";
import { shallowEqual, Store, useStore } from "../state/store";
import { useNotificationAccess } from "./AlertsOffer";
import "./alerts.css";

type Step = "ask" | "notifications" | "battery";
interface SetupState { step: Step | null; turnOn: boolean; later: boolean }
const lifetimes = new WeakMap<object, WeakMap<Alerts, Store<SetupState>>>();
function setupState(watcher: Alerts, lifetime?: object): Store<SetupState> {
  const create = () => new Store<SetupState>({ step: null, turnOn: alertsPreset.get() ?? true, later: false });
  if (!lifetime) return create();
  let watchers = lifetimes.get(lifetime);
  if (!watchers) { watchers = new WeakMap(); lifetimes.set(lifetime, watchers); }
  let state = watchers.get(watcher);
  if (!state) { state = create(); watchers.set(watcher, state); }
  return state;
}

/** Asked once, after the first pairing or the update that adds alerts for
 * when the app is closed. Android lets an app ask for each permission, never
 * grant it, so each of its prompts follows with one line on why. */
export function AlertsSetup({ lifetime }: {
  /** Shared by portal remounts, but replaced when the whole app restarts. */
  lifetime?: object;
} = {}) {
  const { platform } = useApp();
  const watcher = backgroundAlerts(platform);
  return watcher ? <Setup watcher={watcher} lifetime={lifetime} /> : null;
}

function Setup({ watcher, lifetime }: { watcher: Alerts; lifetime?: object }) {
  const { state, pending, error } = useStore(watcher, (s) => s, shallowEqual);
  const { access, permission, pending: asking } = useNotificationAccess();
  const setup = useMemo(() => setupState(watcher, lifetime), [watcher, lifetime]);
  const { step, turnOn, later } = useStore(setup, s => s);
  const setStep = (step: Step | null) => setup.set(s => ({ ...s, step }));
  const setTurnOn = (turnOn: boolean) => setup.set(s => ({ ...s, turnOn }));
  // Closing without choosing asks again next time the whole app opens,
  // not when changing the active side remounts its global portal.
  const setLater = (later: boolean) => setup.set(s => ({ ...s, later }));
  const id = useId();
  useEffect(() => watcher.start(), [watcher]);
  useEffect(() => {
    if (!later && step === null && state && state.settings === null) setup.set(s => ({ ...s, step: "ask" }));
  }, [setup, later, step, state]);
  // Each step Android already allows is skipped, read as the person returns from Android's own screen.
  const after = (from: Step) => {
    const current = watcher.get().state;
    const order: Step[] = ["ask", "notifications", "battery"];
    for (const next of order.slice(order.indexOf(from) + 1)) {
      const allowed = access.get().permission;
      if (next === "notifications" && (allowed === "promptable" || allowed === "blocked")) return setStep(next);
      if (next === "battery" && current && !current.android.unrestricted) return setStep(next);
    }
    setStep(null);
    announce("Alerts are on.");
  };
  const choose = async () => {
    const saved = await watcher.configure({ enabled: turnOn, detail: notificationDetail.get() });
    if (!saved) return;
    alertsPreset.clear();
    if (turnOn) after("ask"); else setStep(null);
  };
  const close = () => { if (step === "ask") setLater(true); setStep(null); };
  const footer = step === "ask" ? <Button variant="primary" pending={pending} onClick={() => void choose()}>Continue</Button>
    : step === "notifications" ? <><Button variant="quiet" disabled={asking} onClick={() => after("notifications")}>Skip</Button>
        <Button variant="primary" pending={asking} onClick={async () => { await access.allow(); await watcher.refresh(); after("notifications"); }}>{permission === "blocked" ? "Open Android settings" : "Continue"}</Button></>
    : <><Button variant="quiet" onClick={() => after("battery")}>Skip</Button>
        <Button variant="primary" onClick={async () => { await watcher.battery(); after("battery"); }}>Continue</Button></>;
  return <Sheet open={step !== null} title="Alerts" onClose={close} footer={<div className="alerts-actions">{footer}</div>}>
    {step === "ask" && <fieldset className="alerts-setup" aria-describedby={`${id}-q`}>
      <legend id={`${id}-q`} className="alerts-setup__question">Notify me when Øphio is closed?</legend>
      <p className="settings-meta">Questions, approvals and chat replies you aren’t viewing make a sound by default. You can choose for each kind in Settings › Notifications.</p>
      <label className="settings-check"><input type="radio" name={`${id}-choice`} checked={turnOn} onChange={() => setTurnOn(true)} />Turn on</label>
      <p className="settings-meta">With Øphio’s battery use Unrestricted, notifications come even after you swipe the app away, with nothing kept in the status bar meanwhile.</p>
      <label className="settings-check"><input type="radio" name={`${id}-choice`} checked={!turnOn} onChange={() => setTurnOn(false)} />Not now</label>
      <p className="settings-meta">Notifications come only while Øphio is open. You can turn this on in Settings › Notifications.</p>
    </fieldset>}
    {step === "notifications" && <p>{permission === "blocked"
      ? "Notifications are blocked for Øphio. Turn them on in Android settings so alerts can show."
      : "Android asks next whether Øphio may show notifications. Allow it so alerts can show."}</p>}
    {step === "battery" && <p>Next, Android asks to let Øphio run without battery limits. Without it, alerts arrive only while the app is open.</p>}
    {error && <p className="error-text" role="status">{error}</p>}
  </Sheet>;
}
