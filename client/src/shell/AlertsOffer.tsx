// SPDX-License-Identifier: Apache-2.0
import { Bell, BellOff } from "lucide-react";
import { useEffect, useState } from "react";
import { useApp } from "../app/context";
import { announce } from "../design/Announcer";
import { Button } from "../design/Button";
import { Notice } from "../design/Notice";
import { missedAlertText, notificationAccess, taskAlertsOffer } from "../platform/notifications";
import { shallowEqual, useStore } from "../state/store";
import { useRoute } from "./Screen";
import "./alerts.css";

export function useNotificationAccess() {
  const { platform } = useApp();
  const access = notificationAccess(platform);
  const state = useStore(access, (s) => ({ permission: s.permission, pending: s.pending, error: s.error }), shallowEqual);
  useEffect(() => {
    const stop = access.start();
    void access.refresh();
    return stop;
  }, [access]);
  return { access, ...state };
}

/** Shared by Settings, the contextual offer and missed-alert recovery. */
export function AlertPermissionAction() {
  const { platform } = useApp();
  const { access, permission, pending } = useNotificationAccess();
  if (permission === "granted" || !permission) return null;
  if (permission === "blocked" ? !platform.openNotificationSettings : !platform.requestNotifications) return null;
  return <Button size="sm" pending={pending} onClick={() => void access.allow()}>
    {permission === "blocked" ? "Open Android settings" : "Allow alerts"}
  </Button>;
}

/** Lives after the conversation, near the send that just started/continued it. */
export function AlertsOffer({ projectId, taskId }: { projectId: string; taskId: string | null }) {
  const { session, profile } = useApp();
  const [confirmedTask, setConfirmedTask] = useState<string | null>(null);
  const { active } = useRoute();
  const offer = taskAlertsOffer(session);
  const context = useStore(offer, (s) => s);
  const { access, permission, pending, error } = useNotificationAccess();
  const shown = active && permission === "promptable" && context?.projectId === projectId && context.taskId === taskId;
  useEffect(() => {
    if (context && (permission === "granted" || permission === "blocked")) offer.dismiss();
  }, [offer, context, permission]);
  useEffect(() => {
    if (!shown) return;
    offer.shown();
    announce("Get an alert when this needs you or finishes? Allow alerts or choose Not now.");
  }, [offer, shown]);
  if (active && permission === "granted" && taskId && confirmedTask === taskId) return <p className="alerts-offer" role="status">Alerts are on. We’ll let you know when tasks on {profile.name} need you or finish.</p>;
  if (!shown) return null;
  return <aside className="alerts-offer" aria-label="Task alerts">
    <p className="alerts-offer__title"><Bell aria-hidden /><span>Get an alert when this needs you or finishes?</span></p>
    <div className="alerts-actions"><Button size="sm" pending={pending} onClick={async () => { await access.allow(); if (access.get().permission === "granted") setConfirmedTask(taskId); }}>Allow alerts</Button><Button size="sm" variant="quiet" disabled={pending} onClick={() => offer.dismiss()}>Not now</Button></div>
    {error && <p className="error-text" role="status">{error}</p>}
  </aside>;
}

/** Shell lifetime, so returning from Review cannot miss the accepted command. */
export function TaskAlerts() {
  const { platform, session, profile } = useApp();
  const { access, error, permission } = useNotificationAccess();
  const missed = useStore(access, (s) => s.missed[profile.profileId]);
  const [visible, setVisible] = useState(document.visibilityState === "visible");
  useEffect(() => taskAlertsOffer(session).watch(session, access), [session, access]);
  useEffect(() => {
    const change = () => setVisible(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", change);
    const off = platform.onForeground?.(() => setVisible(true));
    return () => { document.removeEventListener("visibilitychange", change); off?.(); };
  }, [platform]);
  const text = missed ? missedAlertText(missed, permission) : null;
  useEffect(() => { if (visible && text) announce(text); }, [visible, text]);
  if (!visible || !missed) return null;
  return <div className="platform-notices alerts-missed" aria-label="Missed task alerts">
    <Notice tone="warn" icon={BellOff} title={missed.off && permission !== "granted" ? "Alerts are off" : "Missed task alerts"}
      action={<div className="alerts-actions"><AlertPermissionAction /><Button size="sm" variant="quiet" onClick={() => access.dismissMissed(profile.profileId)}>Dismiss</Button></div>}>
      {text}
      {error && <p className="error-text">{error}</p>}
    </Notice>
  </div>;
}
