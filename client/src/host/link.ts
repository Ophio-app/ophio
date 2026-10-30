// SPDX-License-Identifier: Apache-2.0
//
// The boundary between the interface and whatever carries protocol frames.
// In the app the native layer owns TLS pinning, the device key and the
// authentication handshake; this side only ever sees authenticated frames.

import type {
  ErrorCode,
  Authenticated,
  Challenge,
  ClientFrame,
  MediaClientFrame,
  MediaServerFrame,
  PairingOffer,
  PairResponse,
  ServerFrame,
  SpeechClientFrame,
  SpeechServerFrame,
  TerminalClientFrame,
  TerminalServerFrame,
  TransferClientFrame,
  TransferServerFrame,
  ProtocolVersion,
  AppOffer,
} from "../protocol/types";
import type { ParsedPairingLink } from "../protocol/pairing";

export type LinkState =
  | { state: "idle" }
  | { state: "connecting"; attempt: number }
  | { state: "authenticating" }
  | { state: "online"; since: number; route?: "direct" | "relay" }
  | {
      state: "offline"; since: number; reason: string; code?: ErrorCode; retryAt: number | null;
      /** Another device answered at the computer's address: no address presented
       * the pinned certificate. The link keeps trying. */
      mismatch?: boolean;
    }
  /** Will not retry without the person acting (revoked, incompatible). */
  | { state: "failed"; reason: LinkFailure; detail: string };

export type LinkFailure =
  | "revoked"
  | "incompatible_version"
  | "unauthenticated"
  | "not_paired"
  | "unreachable";

export type LinkEvent =
  | { type: "state"; state: LinkState }
  | { type: "authenticated"; challenge: Challenge; auth: Authenticated }
  | { type: "frame"; frame: ServerFrame };

/** Control connection to one paired computer. */
export interface ControlLink {
  connect(): void;
  disconnect(): void;
  /** Returns false when the link is not online; nothing is queued. */
  send(
    frame: Exclude<ClientFrame, { type: "hello" } | { type: "authenticate" }>,
  ): boolean;
  listen(listener: (e: LinkEvent) => void): () => void;
}

export type MediaEvent =
  | { type: "state"; state: LinkState }
  | { type: "text"; frame: MediaServerFrame }
  | { type: "binary"; data: Uint8Array };

export interface MediaLink {
  send(frame: MediaClientFrame): boolean;
  close(): void;
  listen(listener: (e: MediaEvent) => void): () => void;
}

export type TerminalEvent =
  | { type: "state"; state: LinkState }
  | { type: "text"; frame: TerminalServerFrame }
  | { type: "output"; data: Uint8Array };

export interface TerminalLink {
  send(frame: TerminalClientFrame): boolean;
  /** Raw input bytes for the attached terminal. Dropped when offline. */
  input(bytes: Uint8Array): boolean;
  close(): void;
  listen(listener: (e: TerminalEvent) => void): () => void;
}

export type SpeechEvent =
  | { type: "state"; state: LinkState }
  | { type: "text"; frame: SpeechServerFrame }
  | { type: "level"; rms: number }
  /** The phone's microphone started: transcript times count from here. */
  | { type: "capturing" };

export interface SpeechLink {
  /** Starts microphone capture on the phone and streams it to the host. */
  send(frame: SpeechClientFrame): boolean;
  close(): void;
  listen(listener: (e: SpeechEvent) => void): () => void;
}

export type TransferEvent =
  | { type: "state"; state: LinkState }
  | { type: "text"; frame: TransferServerFrame }
  /** Download bytes, in order from the offset in the last `sending` frame. */
  | { type: "chunk"; data: Uint8Array };

/** Resumable file bytes; transfers are declared by command first. */
export interface TransferLink {
  send(frame: TransferClientFrame): boolean;
  /**
   * Upload bytes (at most MAX_CHUNK_BYTES). Resolves once the platform has
   * handed them to the connection, so callers can pace large files; false
   * when offline.
   */
  chunk(bytes: Uint8Array): Promise<boolean>;
  close(): void;
  listen(listener: (e: TransferEvent) => void): () => void;
}

/** A remembered computer. Keys and certificate pins stay in native storage. */
export interface HostProfile {
  profileId: string;
  hostId: string;
  name: string;
  addresses: string[];
  /** Hex SHA-256 of the pinned certificate, shown for verification. */
  certSha256: string;
  pairedAt: number;
  deviceId: string;
  /** How Android protects this profile’s device key. */
  keyProtection?: "keystore" | "wrapped";
}

export interface PairingResult {
  profile: HostProfile;
  response: PairResponse;
}

/** A local share, never an instruction to the computer. */
export interface SharedItem {
  /** Stable intake id, usable by a persisted inbox for deduplication. */
  id?: string;
  name: string;
  mime: string;
  bytes?: Uint8Array;
  text?: string;
  /** An item can be refused independently of the other items in a share. */
  error?: string;
}

export interface LocalNotification {
  id: string;
  title: string;
  body: string;
  route?: unknown;
  /** Android's category: questions and approvals, or finished tasks. */
  category?: "needs_you" | "finished";
  /** Finished tasks from one computer stack under its name. */
  group?: string;
  groupTitle?: string;
}

/** How the background watcher's connection to one computer is doing. */
export type AlertStatus = "connecting" | "watching" | "update_needed" | "unreachable" | "not_allowed";
export interface AlertSettings {
  enabled: boolean;
  detail: "minimal" | "task_title";
}
export interface AlertsState {
  /** Null until the person has been asked. */
  settings: AlertSettings | null;
  /** By profile id, while the watcher runs. */
  statuses: Record<string, AlertStatus>;
  android: {
    running: boolean;
    /** Android lets the watcher run without battery limits, which it needs
     * to keep running once the app leaves the screen. */
    unrestricted: boolean;
    /** Whether each category of alert can show. */
    needsYou: boolean;
    finished: boolean;
  };
}
export type AlertChannel = "needs-you" | "finished";
/** Alerts while the app is closed, from a watcher that runs without the screen. */
export interface BackgroundAlerts {
  state(): Promise<AlertsState>;
  /** Saves the choice and starts or stops the watcher. */
  configure(settings: AlertSettings): Promise<AlertsState>;
  /** Android's own dialog to let the watcher run without battery limits. */
  battery(): Promise<void>;
  /** Android's settings for one category of alert. */
  channel(channel: AlertChannel): Promise<void>;
  /** Alerts the app showed on its screen, which the watcher then does not raise. */
  seen?(profileId: string, attentionIds: string[]): Promise<void>;
  /** The app's screen began or stopped showing a computer's alerts. Only
   * that computer's wait for the app, not every computer it is connected to. */
  viewing?(profileId: string, on: boolean): Promise<void>;
}

export type NotificationPermission = "granted" | "promptable" | "blocked";

export interface SendOutbox {
  put(profileId: string, commandId: string, record: string): Promise<void>;
  list(profileId: string): Promise<string[]>;
  remove(profileId: string, commandId: string): Promise<void>;
}

/** Everything the interface needs from the platform. */
export interface Platform {
  readonly kind: "native" | "simulated";
  profiles(): Promise<HostProfile[]>;
  parsePairingLink(link: string): ParsedPairingLink;
  /**
   * The computer holds the request until its owner confirms it there.
   * `waiting` receives the six-digit code both screens show meanwhile. A
   * computer already paired here is refused unless `replace`, which keeps the
   * new pairing in place of the earlier one.
   */
  pair(offer: PairingOffer, deviceName: string, replace: boolean, waiting: (code: string) => void): Promise<PairingResult>;
  /** Withdraws the request waiting for the computer's owner. Nothing is paired. */
  cancelPairing(): Promise<void>;
  /** The name this device shows for the computer; the computer keeps its own. */
  renameProfile(profileId: string, name: string): Promise<HostProfile>;
  forget(profileId: string): Promise<void>;
  /** Sends whose answer the phone may not have heard, kept in storage that
   * survives the app being killed. Written before a command leaves. */
  outbox?: SendOutbox;
  control(profileId: string): ControlLink;
  media(profileId: string): MediaLink;
  terminal(profileId: string): TerminalLink;
  speech(profileId: string): SpeechLink;
  transfer(profileId: string): TransferLink;
  /** Scan a pairing QR code with the camera, if the platform can. */
  scanPairingCode?(): Promise<string | null>;
  openExternal(url: string): Promise<void>;
  /** Plain-text system sharing, or clipboard on platforms without a share sheet. */
  shareText?(text: string): Promise<"shared" | "copied">;
  preview?(profileId: string, previewId: string, path: string, changed: (state: import("./preview").PreviewState) => void, taskId?: string | null, ask?: import("./preview").PreviewAsk): Promise<import("./preview").PreviewHandle>;
  pendingPreviews?(profileId: string, previewId: string): Promise<import("./preview").PreviewPicture[]>;
  haptic?(kind: "tap" | "confirm"): void;
  saveToPhone?(blob: Blob, name: string): Promise<{ savedAs: string }>;
  /** Largest file, in bytes, this device takes from the computer. */
  downloadLimit?: number;
  onShared?(
    listener: (items: SharedItem[]) => void | Promise<void>,
  ): () => void;
  /** Explicit foreground permission request; never posts a task alert. */
  requestNotifications?(): Promise<void>;
  notify?(notification: LocalNotification): void | Promise<void>;
  /** Clears alerts by id, whichever of the app and the watcher raised them. */
  cancelNotifications?(ids: string[]): Promise<void>;
  onNotificationOpen?(
    listener: (notification: { id: string; route?: unknown }) => void,
  ): () => void;
  appInfo?(): Promise<{
    version: string;
    build: string;
    keyProtection: string;
    installerPackage?: string | null;
    /** The name the person gave this device, or its model. */
    deviceName?: string;
    protocol?: ProtocolVersion;
  }>;
  /** Updates this app to one a paired computer offers. Android only. */
  appUpdate?: AppUpdater;
  keepAwake?(on: boolean): void;
  /** Hide the system bars while a display is shown full screen. */
  immersive?(on: boolean): void;
  alerts?: BackgroundAlerts;
  /** Local authentication only; never conveys computer authority. */
  appLock?: AppLock;
  onAppLink?(listener: (url: string) => void): () => void;
  onPlatformError?(
    listener: (error: { operation: string; message: string }) => void,
  ): () => void;
  notificationPermission?(): Promise<NotificationPermission>;
  openNotificationSettings?(): Promise<void>;
  /** Native activity resumed, including a return from Android settings. */
  onForeground?(listener: () => void): () => void;
}

/** What Android made of an update the app handed it. */
export interface AppUpdateResult {
  /** Android asks the person; installed apps restart; failed ones explain. */
  state: "asking" | "installed" | "failed";
  message: string | null;
}
export interface AppUpdater {
  /** The offer this app can update to, if any, and the version it runs. */
  check(offers: AppOffer[]): Promise<{
    offer: AppOffer | null;
    version: string;
    /** Android asks the person to confirm each update (before Android 12). */
    asks: boolean;
  }>;
  /** Hands Android the app, verified as `sha256`; the result follows by `onResult`. */
  install(profileId: string, app: Blob, sha256: string): Promise<void>;
  onResult(listener: (result: AppUpdateResult) => void): () => void;
}

/** Native-owned, persisted local lock policy. Browser platforms omit this. */
export interface AppLockState {
  available: boolean;
  reason: string | null;
  enabled: boolean;
  locked: boolean;
  relockMinutes: 0 | 1 | 5 | 15;
}
export interface AppLock {
  state(): Promise<AppLockState>;
  configure(enabled: boolean, relockMinutes: AppLockState["relockMinutes"]): Promise<AppLockState>;
  unlock(): Promise<AppLockState>;
  /** Asks for the phone's own unlock before one sensitive action, every time,
   * whether or not the app lock is on. `title` names the action. */
  confirm?(title: string): Promise<AppLockState>;
  lock(): Promise<AppLockState>;
  listen(listener: (state: AppLockState) => void): () => void;
}
