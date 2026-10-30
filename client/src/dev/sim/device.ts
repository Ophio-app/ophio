// SPDX-License-Identifier: Apache-2.0
import type { AlertChannel, AlertsState, Platform, SharedItem, LocalNotification, NotificationPermission } from "../../host/link";
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
    android: { running: true, unrestricted: false, needsYou: true, finished: true },
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
  const controls = {
    installs,
    notifications,
    cancelled,
    seen,
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
        alerts = { ...alerts, settings, android: { ...alerts.android, running: settings.enabled } };
        return alerts;
      },
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
      async check(offers) { return { offer: offers.find(o => o.version !== installedBuild) ?? null, asks: false, version: installedBuild }; },
      async install(_profile, _app, sha256) { installs.push(sha256); installedBuild = "sim-next"; },
      onResult() { return () => {}; },
    },
    async appInfo() {
      return { version: "0.1.0", build: "sim", keyProtection: "simulated", deviceName: "Pixel 9" };
    },
  };
  return { controls, capabilities };
}
