// SPDX-License-Identifier: Apache-2.0
import { useEffect, useId, useState } from "react";
import { useApp } from "../app/context";
import { announce } from "../design/Announcer";
import { Button } from "../design/Button";
import { Sheet } from "../design/Sheet";
import { alertsPreset, backgroundAlerts, type Alerts } from "../platform/alerts";
import { notificationDetail } from "../platform/notifications";
import { shallowEqual, useStore } from "../state/store";
import { useNotificationAccess } from "./AlertsOffer";
import "./alerts.css";

type Step = "ask" | "notifications" | "battery";

/** Asked once, after the first pairing or the update that adds alerts for
 * when the app is closed. Android lets an app ask for each permission, never
 * grant it, so each of its prompts follows with one line on why. */
export function AlertsSetup() {
  const { platform } = useApp();
  const watcher = backgroundAlerts(platform);
  return watcher ? <Setup watcher={watcher} /> : null;
}

function Setup({ watcher }: { watcher: Alerts }) {
  const { state, pending, error } = useStore(watcher, (s) => s, shallowEqual);
  const { access, permission, pending: asking } = useNotificationAccess();
  const [step, setStep] = useState<Step | null>(null);
  const [turnOn, setTurnOn] = useState(() => alertsPreset.get() ?? true);
  // Closing without choosing asks again next time the app opens.
  const [later, setLater] = useState(false);
  const id = useId();
  useEffect(() => watcher.start(), [watcher]);
  useEffect(() => { if (!later && step === null && state && state.settings === null) setStep("ask"); }, [later, step, state]);
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
      <legend id={`${id}-q`} className="alerts-setup__question">Get alerts when an agent asks something or finishes?</legend>
      <label className="settings-check"><input type="radio" name={`${id}-choice`} checked={turnOn} onChange={() => setTurnOn(true)} />Turn on</label>
      <p className="settings-meta">With Øphio’s battery use Unrestricted, alerts arrive even after you swipe the app away, with nothing kept in the status bar meanwhile.</p>
      <label className="settings-check"><input type="radio" name={`${id}-choice`} checked={!turnOn} onChange={() => setTurnOn(false)} />Not now</label>
      <p className="settings-meta">Alerts arrive only while the app is open. You can turn this on in Settings › Notifications.</p>
    </fieldset>}
    {step === "notifications" && <p>{permission === "blocked"
      ? "Notifications are blocked for Øphio. Turn them on in Android settings so alerts can show."
      : "Android asks next whether Øphio may show notifications. Allow it so alerts can show."}</p>}
    {step === "battery" && <p>Next, Android asks to let Øphio run without battery limits. Without it, alerts arrive only while the app is open.</p>}
    {error && <p className="error-text" role="status">{error}</p>}
  </Sheet>;
}
