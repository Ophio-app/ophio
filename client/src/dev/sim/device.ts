// SPDX-License-Identifier: Apache-2.0
import type { AlertChannel, AlertsState, Platform, SharedItem, LocalNotification, NotificationPermission } from "../../host/link";
import type { AlertType, Tier } from "../../protocol/types";
import { recommendedTiers } from "../../platform/alertTypes";
import { ANDROID_SAVE_LIMIT } from "../../host/transfer";

/** Deterministic local phone capabilities; no operating-system side effects. */
export function simulatedDevice() {
  const shared = new Set<(items: SharedItem[]) => void | Promise<void>>();
  const opens = new Set<(n: { id: string; route?: unknown }) => void>();
  const errors = new Set<(e: { operation: string; message: string }) => void>();
  const notifications: LocalNotification[] = [];
  /** Ids the app cleared, and the alerts it reported seen, in order. */
  const cancelled: string[] = [];
  const seen: { profileId: string; attentionIds: string[] }[] = [];
  /** Items the app said the person saw on screen, in order. */
  const handled: { profileId: string; attentionIds: string[] }[] = [];
  /** Each type's choice, as Android's categories keep it, and the changes asked. */
  const tierChanges: { alertType: AlertType; tier: Tier }[] = [];
  /** Set, the next change of tier is refused as Android would. */
  let refuseTier: string | null = null;
  /** The computer whose alerts the app's screen shows, as the watcher hears. */
  let viewing: string | null = null;
  /** Alerts the app raised, which the watcher records as raised. */
  const raised: string[] = [];
  /** Android's shade has no room left for the app's alerts. */
  let full = false;
  const awake: boolean[] = [];
  /** Already chosen, so screens open as on a phone set up before. */
  let alerts: AlertsState = {
    settings: { enabled: true, detail: "minimal" },
    statuses: {},
    tiers: recommendedTiers(),
    android: { running: true, unrestricted: false },
  };
  const channels: AlertChannel[] = [];
  let batteryRequests = 0;
  /** What Save to phone put in the simulated Downloads folder. */
  const downloads: { name: string; size: number }[] = [];
  let permission: NotificationPermission = "granted";
  let requestedPermission: NotificationPermission = "granted";
  let requests = 0;
  let settingsOpened = 0;
  let permissionFailure = false;
  let installedBuild = "sim";
  const installs: string[] = [];
  /**
   * What Android allows the app: whether it may install apps, what the person
   * chooses in Android's Install unknown apps setting, and whether Android
   * asks to confirm each update (as before Android 12).
   */
  let installing = { allowed: true, allowsInSettings: true, asks: false };
  let installSettingsOpened = 0;
  const foreground = new Set<() => void>();
  const backInFront = () => { for (const listener of foreground) listener(); };
  const controls = {
    installs,
    get appInstalls() { return { ...installing, settingsOpened: installSettingsOpened }; },
    /** Sets what Android allows the app, as its settings would. */
    setAppInstalls(next: Partial<typeof installing>) { installing = { ...installing, ...next }; },
    /** The app comes back to the front, as from Android's settings. */
    foreground: backInFront,
    notifications,
    cancelled,
    seen,
    handled,
    tierChanges,
    /** The next change of tier fails with `message`, as Android might. */
    refuseTier(message: string | null) { refuseTier = message; },
    raised,
    awake,
    channels,
    shadeFull(on: boolean) { full = on; },
    get viewing() { return viewing; },
    get alerts() { return alerts; },
    /** Replaces part of the watcher's state, as Android or the watcher would. */
    setAlerts(next: Partial<Omit<AlertsState, "android">> & { android?: Partial<AlertsState["android"]> }) {
      alerts = { ...alerts, ...next, android: { ...alerts.android, ...next.android } };
    },
    get batteryRequests() { return batteryRequests; },
    downloads,
    notificationPermission(state: NotificationPermission) { permission = state; },
    notificationRequestResult(state: NotificationPermission) { requestedPermission = state; },
    get notificationRequests() { return requests; },
    get notificationSettingsOpened() { return settingsOpened; },
    notificationPermissionFailure(on: boolean) { permissionFailure = on; },
    async share(items: SharedItem[]) {
      for (const listener of shared) await listener(items);
    },
    open(id: string) {
      const n = notifications.find((v) => v.id === id);
      if (n) for (const listener of opens) listener(n);
    },
    error(operation: string, message: string) {
      for (const listener of errors) listener({ operation, message });
    },
  };
  const capabilities: Pick<
    Platform,
    | "requestNotifications"
    | "onShared"
    | "onNotificationOpen"
    | "onPlatformError"
    | "notify"
    | "cancelNotifications"
    | "keepAwake"
    | "alerts"
    | "appInfo"
    | "appUpdate"
    | "onForeground"
    | "saveToPhone"
    | "downloadLimit"
    | "notificationPermission"
    | "openNotificationSettings"
  > = {
    onShared(listener) {
      shared.add(listener);
      return () => {
        shared.delete(listener);
      };
    },
    onNotificationOpen(listener) {
      opens.add(listener);
      return () => {
        opens.delete(listener);
      };
    },
    onPlatformError(listener) {
      errors.add(listener);
      return () => {
        errors.delete(listener);
      };
    },
    async requestNotifications() {
      requests++;
      if (permission === "promptable") permission = requestedPermission;
    },
    async notificationPermission() {
      if (permissionFailure) throw new Error("Notification permission is unavailable.");
      return permission;
    },
    async openNotificationSettings() { settingsOpened++; },
    async notify(notification) {
      if (permission !== "granted") throw new Error("Task alerts are off.");
      if (full) throw new Error("Android has no room for more alerts.");
      notifications.push(notification);
      raised.push(notification.id);
    },
    async cancelNotifications(ids) {
      cancelled.push(...ids);
    },
    keepAwake(on) {
      awake.push(on);
    },
    alerts: {
      async state() { return alerts; },
      async configure(settings) {
        // The watcher runs while the app does; the choice is whether it
        // keeps going once the app closes.
        alerts = { ...alerts, settings, android: { ...alerts.android, running: true } };
        return alerts;
      },
      async setTier(alertType, tier) {
        tierChanges.push({ alertType, tier });
        if (refuseTier) { const message = refuseTier; refuseTier = null; throw new Error(message); }
        alerts = { ...alerts, tiers: { ...alerts.tiers, [alertType]: tier } };
        if (tier === "off") cancelled.push(...notifications.filter((n) => n.alertType === alertType).map((n) => n.id));
        return alerts;
      },
      async handled(profileId, attentionIds) { handled.push({ profileId, attentionIds }); },
      async battery() { batteryRequests++; },
      async channel(channel) { channels.push(channel); },
      async seen(profileId, attentionIds) { seen.push({ profileId, attentionIds }); },
      async viewing(profileId, on) {
        if (on) viewing = profileId;
        else if (viewing === profileId) viewing = null;
      },
    },
    downloadLimit: ANDROID_SAVE_LIMIT,
    async saveToPhone(blob, name) {
      // Like Android's Downloads, a repeated name gets a number, not a replacement.
      const dot = name.lastIndexOf(".");
      const [stem, ext] =
        dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, ""];
      let savedName = name;
      for (let n = 1; downloads.some((d) => d.name === savedName); n++)
        savedName = `${stem} (${n})${ext}`;
      downloads.push({ name: savedName, size: blob.size });
      return { savedAs: `Downloads/${savedName}` };
    },
    appUpdate: {
      async check(offers, options) {
        const answer = { offer: offers.find(o => o.version !== installedBuild) ?? null, allowed: installing.allowed, asks: installing.asks, version: installedBuild };
        if (options?.openSettings && !installing.allowed) {
          // The person visits Android's setting and comes back a moment later.
          installSettingsOpened++;
          setTimeout(() => { installing = { ...installing, allowed: installing.allowsInSettings }; backInFront(); }, 400);
        }
        return answer;
      },
      async install(_profile, _app, sha256) { installs.push(sha256); installedBuild = "sim-next"; },
      onResult() { return () => {}; },
    },
    onForeground(listener) {
      foreground.add(listener);
      return () => { foreground.delete(listener); };
    },
    async appInfo() {
      return { version: "0.1.0", build: "sim", keyProtection: "simulated", deviceName: "Pixel 9" };
    },
  };
  return { controls, capabilities };
}
