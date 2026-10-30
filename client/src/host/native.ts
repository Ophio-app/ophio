// SPDX-License-Identifier: Apache-2.0
//
// Platform backed by the app's native layer (src-tauri). The native side owns
// TLS with certificate pinning, the device key in Android Keystore and the
// authentication handshake, and hands this side authenticated frames only.
//
// IPC contract (implemented in src-tauri/src/links.rs):
//   profiles() -> HostProfile[]
//   pair({ offer, deviceName, replace, waiting: Channel<string> }) -> PairingResult
//   pair_cancel()
//   rename_profile({ profileId, name }) -> HostProfile
//   forget({ profileId })
//   link_page() -> number                 the page loaded now
//   link_open({ page, profileId, kind, events: Channel<NativeLinkEvent | ArrayBuffer> }) -> number
//                                         refused for a page since left
//   link_connect({ linkId })              starts a control link; once
//                                         started, tries again now
//   link_send({ linkId, text }) -> bool   one JSON frame
//   link_send_binary(bytes, header "x-link-id") -> bool
//   link_sent({ linkId }) -> number       messages it has handed this page
//   link_close({ linkId })
// Binary frames from the host arrive on the channel as ArrayBuffer.

import { nativeLifecycle, type LifecycleEvent } from "../platform/nativeLifecycle";
import { Channel, invoke as tauriInvoke } from "@tauri-apps/api/core";
import type {
  Authenticated,
  Challenge,
  ClientFrame,
  MediaClientFrame,
  PairingOffer,
  ServerFrame,
  SpeechClientFrame,
  TerminalClientFrame,
  TransferClientFrame,
} from "../protocol/types";
import { parsePairingLink } from "../protocol/pairing";
import { ANDROID_SAVE_LIMIT } from "./transfer";
import type {
  AlertChannel,
  AlertSettings,
  AlertsState,
  ControlLink,
  HostProfile,
  LinkEvent,
  LinkState,
  LocalNotification,
  MediaEvent,
  MediaLink,
  PairingResult,
  Platform,
  NotificationPermission,
  SharedItem,
  SpeechEvent,
  SpeechLink,
  TerminalEvent,
  TerminalLink,
  TransferEvent,
  TransferLink,
  AppUpdater,
  AppUpdateResult,
} from "./link";

/** Keep typed native errors readable by the existing UI error handlers,
 * with the protocol code that says how, for instance, a pairing ended. */
async function invoke<T>(...args: Parameters<typeof tauriInvoke>): Promise<T> {
  try {
    return await tauriInvoke<T>(...args);
  } catch (error) {
    if (error instanceof Error) throw error;
    const typed = typeof error === "object" && error !== null ? error : {};
    const message =
      "message" in typed && typeof typed.message === "string"
        ? typed.message
        : "This device could not complete this operation.";
    const code = "code" in typed && typeof typed.code === "string" ? typed.code : undefined;
    throw Object.assign(new Error(message), code && { code });
  }
}

type LinkKind = "control" | "media" | "terminal" | "speech" | "transfer";

type NativeLinkEvent =
  | { type: "state"; state: LinkState }
  | { type: "authenticated"; challenge: Challenge; auth: Authenticated }
  | { type: "text"; text: string }
  /** Speech only: microphone level measured on the phone. */
  | { type: "level"; rms: number }
  /** Speech only: the phone's microphone started. */
  | { type: "capturing" };

// A replacement side link must wait for the previous native slot to be freed.
const closingLinks = new Map<string, Promise<unknown>>();

// The native side numbers each page it loads and refuses a link for any
// other, so a reload never leaves one of this page's behind. A page is asked
// again once a link was refused: it may have asked before its own load was
// counted.
let loadedPage: Promise<number> | undefined;
function currentPage(): Promise<number> {
  loadedPage ??= invoke<number>("link_page");
  return loadedPage;
}

/** How often a watched link checks that what the native side sent arrived. */
export const LINK_CHECK_MS = 5_000;

/** One native link: owns its id, state and listener fan-out. */
class NativeLink<E> {
  #id: Promise<number>;
  #open: () => Promise<number>;
  /** The native side refused this link; Retry connection opens it again. */
  #refused = false;
  #state: LinkState = { state: "idle" };
  #listeners = new Set<(e: E) => void>();
  #closed = false;
  #key: string;
  #started = false;
  /** The channel this link's messages arrive on now, and how many have. */
  #events: Channel<NativeLinkEvent | ArrayBuffer> | undefined;
  #received = 0;
  /** At the last check: what the native side had sent, and what had arrived. */
  #checked: { sent: number; received: number } | null = null;
  #watch: ReturnType<typeof setInterval> | undefined;

  constructor(
    profileId: string,
    kind: LinkKind,
    private readonly map: (
      e: NativeLinkEvent | ArrayBuffer,
      link: NativeLink<E>,
    ) => E | null,
    watch = false,
  ) {
    this.#key = `${profileId}:${kind}`;
    this.#open = () => {
      const events = this.#channel();
      return currentPage().then((page) => invoke<number>("link_open", { page, profileId, kind, events }));
    };
    this.#id = this.#opening();
    if (watch) this.#watch = setInterval(() => void this.#check(), LINK_CHECK_MS);
  }

  /** A new channel for the native side's messages; an earlier one's are
   * dropped from now on. */
  #channel(): Channel<NativeLinkEvent | ArrayBuffer> {
    const events = new Channel<NativeLinkEvent | ArrayBuffer>();
    this.#events = events;
    this.#received = 0;
    this.#checked = null;
    events.onmessage = (m) => {
      if (this.#closed || this.#events !== events) return;
      this.#received += 1;
      if (!(m instanceof ArrayBuffer)) {
        if (m.type === "state") this.#state = m.state;
        if (m.type === "authenticated")
          this.#state = { state: "online", since: Date.now() };
      }
      const e = this.map(m, this);
      if (e) for (const l of this.#listeners) l(e);
    };
    return events;
  }

  /** Whether the native side's messages still arrive. A channel delivers
   * them strictly in order, so one the webview failed to take holds back
   * every later one for good, and the page goes on showing the last state it
   * received: Connecting, say, while the computer answers. A page that has
   * gone a whole check without receiving what was already sent opens the
   * link again on a new channel. */
  async #check(): Promise<void> {
    if (this.#closed || !this.#started) return;
    // A hidden page's webview may hold messages back until it is shown.
    if (document.visibilityState === "hidden") {
      this.#checked = null;
      return;
    }
    const id = this.#id;
    let sent: number;
    try {
      sent = await invoke<number>("link_sent", { linkId: await id });
    } catch {
      return;
    }
    if (this.#closed || this.#id !== id) return;
    const last = this.#checked;
    this.#checked = { sent, received: this.#received };
    if (last && this.#received === last.received && this.#received < last.sent)
      this.#reopen();
  }

  /** Close the native link and open another in its place. */
  #reopen(): void {
    const ended = this.#id
      .then((linkId) => invoke("link_close", { linkId }))
      .catch(() => undefined);
    this.#id = this.#opening(ended);
    this.#state = { state: "connecting", attempt: 1 };
    const event = this.map({ type: "state", state: this.#state }, this);
    if (event) for (const listener of this.#listeners) listener(event);
    this.#start();
  }

  /** Open the native link, once `after` (or a replaced link's close) is done. */
  #opening(after = closingLinks.get(this.#key)): Promise<number> {
    const id = after ? after.then(this.#open) : this.#open();
    void id.catch((error: unknown) => this.#refuse(id, error));
    return id;
  }

  /** The native side has no link for `id`, so the next try opens one. */
  #refuse(id: Promise<number>, error: unknown): void {
    loadedPage = undefined;
    if (this.#id !== id) return;
    this.#refused = true;
    this.#failed(error);
  }

  #failed(error: unknown): void {
    if (this.#closed) return;
    const reason =
      error instanceof Error ? error.message : "The native connection failed.";
    this.#state = {
      state: "offline",
      since: Date.now(),
      reason,
      retryAt: null,
    };
    const event = this.map({ type: "state", state: this.#state }, this);
    if (event) for (const listener of this.#listeners) listener(event);
  }

  get online(): boolean {
    return this.#state.state === "online";
  }

  connect(): void {
    if (this.#closed) return;
    if (this.#refused) {
      this.#refused = false;
      const ended = this.#id
        .then((linkId) => invoke("link_close", { linkId }))
        .catch(() => undefined);
      this.#id = this.#opening(ended);
    }
    this.#start();
  }

  #start(): void {
    this.#started = true;
    const id = this.#id;
    void id
      .then((linkId) => invoke("link_connect", { linkId }))
      .catch((error: unknown) => this.#refuse(id, error));
  }

  sendText(frame: unknown): boolean {
    if (!this.online || this.#closed) return false;
    const text = JSON.stringify(frame);
    void this.#id
      .then((linkId) => this.#closed ? false : invoke<boolean>("link_send", { linkId, text }))
      .then((sent) => {
        if (!sent)
          this.#failed(new Error("The connection could not send this frame."));
      })
      .catch((error: unknown) => this.#failed(error));
    return true;
  }

  sendBinary(bytes: Uint8Array): boolean {
    if (!this.online || this.#closed) return false;
    void this.#id
      .then((linkId) =>
        this.#closed ? false : invoke<boolean>("link_send_binary", bytes, {
          headers: { "x-link-id": String(linkId) },
        }),
      )
      .then((sent) => {
        if (!sent)
          this.#failed(new Error("The connection could not send these bytes."));
      })
      .catch((error: unknown) => this.#failed(error));
    return true;
  }

  /** Like sendBinary, but resolves once the native side has taken the bytes. */
  async sendBinaryPaced(bytes: Uint8Array): Promise<boolean> {
    if (!this.online || this.#closed) return false;
    try {
      const linkId = await this.#id;
      return await invoke<boolean>("link_send_binary", bytes, {
        headers: { "x-link-id": String(linkId) },
      });
    } catch {
      return false;
    }
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    clearInterval(this.#watch);
    this.#state = { state: "idle" };
    const closing = this.#id
      .then((linkId) => invoke("link_close", { linkId }))
      .catch(() => undefined);
    closingLinks.set(this.#key, closing);
    void closing.then(() => {
      if (closingLinks.get(this.#key) === closing) closingLinks.delete(this.#key);
    });
  }

  listen(listener: (e: E) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }
}

function parse<T>(text: string): T | null {
  try {
    return JSON.parse(text) as T;
  } catch {
    return null;
  }
}

class NativeControlLink implements ControlLink {
  #link: NativeLink<LinkEvent>;
  constructor(profileId: string) {
    this.#link = new NativeLink<LinkEvent>(profileId, "control", (m) => {
      if (m instanceof ArrayBuffer) return null;
      switch (m.type) {
        case "state":
          return { type: "state", state: m.state };
        case "authenticated":
          return {
            type: "authenticated",
            challenge: m.challenge,
            auth: m.auth,
          };
        case "text": {
          const frame = parse<ServerFrame>(m.text);
          return frame ? { type: "frame", frame } : null;
        }
        case "level":
        case "capturing":
          return null;
      }
    }, true);
  }
  connect(): void {
    this.#link.connect();
  }
  disconnect(): void {
    this.#link.close();
  }
  send(
    frame: Exclude<ClientFrame, { type: "hello" } | { type: "authenticate" }>,
  ): boolean {
    return this.#link.sendText(frame);
  }
  listen(listener: (e: LinkEvent) => void): () => void {
    return this.#link.listen(listener);
  }
}

function sideLink<E, C>(
  profileId: string,
  kind: LinkKind,
  binary: (data: Uint8Array) => E,
  text: (frame: unknown) => E,
  level?: (rms: number) => E,
  capturing?: () => E,
) {
  const link = new NativeLink<E>(profileId, kind, (m) => {
    if (m instanceof ArrayBuffer) return binary(new Uint8Array(m));
    switch (m.type) {
      case "state":
        return { type: "state", state: m.state } as E;
      case "authenticated":
        return {
          type: "state",
          state: { state: "online", since: Date.now() },
        } as E;
      case "text": {
        const f = parse<unknown>(m.text);
        return f ? text(f) : null;
      }
      case "level":
        return level ? level(m.rms) : null;
      case "capturing":
        return capturing ? capturing() : null;
    }
  });
  link.connect();
  return {
    link,
    send: (frame: C) => link.sendText(frame),
    close: () => link.close(),
    listen: (l: (e: E) => void) => link.listen(l),
  };
}

import { nativePreview, pendingPreviews } from "./preview";

export function createNativePlatform(): Platform {
  const integrations = nativeIntegrations();
  return {
    ...integrations,
    kind: "native",
    preview: nativePreview,
    pendingPreviews,
    profiles: () => invoke<HostProfile[]>("profiles"),
    parsePairingLink,
    pair: (offer: PairingOffer, deviceName: string, replace: boolean, onCode: (code: string) => void) => {
      const waiting = new Channel<string>();
      waiting.onmessage = onCode;
      return invoke<PairingResult>("pair", { offer, deviceName, replace, waiting });
    },
    cancelPairing: () => invoke<void>("pair_cancel"),
    renameProfile: (profileId: string, name: string) => invoke<HostProfile>("rename_profile", { profileId, name }),
    forget: (profileId: string) => invoke<void>("forget", { profileId }),
    outbox: {
      put: (profileId, commandId, record) =>
        invoke<void>("outbox_put", { profileId, commandId, record }),
      list: (profileId) => invoke<string[]>("outbox_list", { profileId }),
      remove: (profileId, commandId) =>
        invoke<void>("outbox_remove", { profileId, commandId }),
    },
    control: (profileId) => new NativeControlLink(profileId),
    media(profileId): MediaLink {
      return sideLink<MediaEvent, MediaClientFrame>(
        profileId,
        "media",
        (data) => ({ type: "binary", data }),
        (frame) => ({ type: "text", frame }) as MediaEvent,
      );
    },
    terminal(profileId): TerminalLink {
      const s = sideLink<TerminalEvent, TerminalClientFrame>(
        profileId,
        "terminal",
        (data) => ({ type: "output", data }),
        (frame) => ({ type: "text", frame }) as TerminalEvent,
      );
      return { ...s, input: (bytes) => s.link.sendBinary(bytes) };
    },
    speech(profileId): SpeechLink {
      return sideLink<SpeechEvent, SpeechClientFrame>(
        profileId,
        "speech",
        () => ({ type: "level", rms: 0 }),
        (frame) => ({ type: "text", frame }) as SpeechEvent,
        (rms) => ({ type: "level", rms }),
        () => ({ type: "capturing" }),
      );
    },
    transfer(profileId): TransferLink {
      const s = sideLink<TransferEvent, TransferClientFrame>(
        profileId,
        "transfer",
        (data) => ({ type: "chunk", data }),
        (frame) => ({ type: "text", frame }) as TransferEvent,
      );
      return { ...s, chunk: (bytes) => s.link.sendBinaryPaced(bytes) };
    },
    async shareText(text) {
      if (await invoke<boolean>("share_text", { text })) return "shared";
      await navigator.clipboard.writeText(text);
      return "copied";
    },
    async scanPairingCode() {
      try {
        return await invoke<string | null>("scan_pairing_code");
      } catch (error) {
        integrations.report("Scan pairing code", error);
        return null;
      }
    },
    openExternal: (url: string) => {
      const parsed = new URL(url);
      // A web page, or an email or call a checklist step starts; the phone's
      // own app opens it and nothing is sent until the person sends it.
      if (
        !["http:", "https:", "mailto:", "tel:"].includes(parsed.protocol) ||
        parsed.username ||
        parsed.password
      ) {
        return Promise.reject(
          new Error("Only web, email and phone addresses can be opened."),
        );
      }
      return invoke<void>("plugin:opener|open_url", { url, with: null });
    },
    haptic() {
      void invoke("haptic").catch(() => undefined);
    },
  };
}

type SharedDescriptor = {
  id: string;
  name: string;
  mime: string;
  size: number;
  text?: string;
  error?: string;
};
type NotificationOpen = { id: string; route?: unknown };
type DeviceEvent =
  | { type: "shared"; items: SharedDescriptor[] }
  | ({ type: "appUpdate" } & AppUpdateResult)
  | { type: "notificationOpen"; notification: NotificationOpen }
  | { type: "background" | "foreground" }
  | LifecycleEvent;

function nativeIntegrations() {
  const lifecycle = nativeLifecycle();
  const sharedListeners = new Set<
    (items: SharedItem[]) => void | Promise<void>
  >();
  const notificationListeners = new Set<(n: NotificationOpen) => void>();
  const errorListeners = new Set<
    (e: { operation: string; message: string }) => void
  >();
  const platformErrors: { operation: string; message: string }[] = [];
  const pending: SharedDescriptor[] = [];
  const notifications: NotificationOpen[] = [];
  const foregroundListeners = new Set<() => void>();
  const updateListeners = new Set<(result: AppUpdateResult) => void>();
  const seen = new Set<string>();
  let draining = false;
  const report = (operation: string, error: unknown) => {
    const message =
      error instanceof Error
        ? error.message
        : "This device could not complete this operation.";
    const failure = { operation, message };
    if (errorListeners.size)
      for (const listener of errorListeners) listener(failure);
    else {
      platformErrors.push(failure);
      if (platformErrors.length > 32) platformErrors.shift();
    }
  };
  const drain = async () => {
    if (draining || sharedListeners.size === 0) return;
    draining = true;
    try {
      while (pending.length && sharedListeners.size) {
        const descriptor = pending[0];
        if (!descriptor) break;
        const item: SharedItem = {
          id: descriptor.id,
          name: descriptor.name,
          mime: descriptor.mime,
        };
        try {
          if (descriptor.error) item.error = descriptor.error;
          else if (descriptor.text !== undefined) item.text = descriptor.text;
          else {
            if (
              !Number.isSafeInteger(descriptor.size) ||
              descriptor.size < 0 ||
              descriptor.size > 50 * 1024 * 1024
            )
              throw new Error("This shared file exceeds the 50 MB limit.");
            const bytes = new Uint8Array(descriptor.size);
            for (let offset = 0; offset < bytes.length; ) {
              const chunk = new Uint8Array(
                await invoke<ArrayBuffer>("shared_read", {
                  id: descriptor.id,
                  offset,
                }),
              );
              if (!chunk.length || offset + chunk.length > bytes.length)
                throw new Error(
                  "The shared file could not be read completely.",
                );
              bytes.set(chunk, offset);
              offset += chunk.length;
            }
            item.bytes = bytes;
          }
        } catch (error) {
          item.error =
            error instanceof Error
              ? error.message
              : "The shared item could not be read.";
        }
        if (!sharedListeners.size) break;
        for (const listener of sharedListeners) await listener([item]);
        pending.shift();
        await invoke("shared_ack", { id: descriptor.id }).catch(
          (error: unknown) => report("Import shared item", error),
        );
      }
    } finally {
      draining = false;
    }
  };
  const events = new Channel<DeviceEvent>();
  events.onmessage = (event) => {
    if (event.type === "foreground") {
      for (const listener of foregroundListeners) listener();
      return;
    }
    if (event.type === "appLink" || event.type === "lockChanged") { lifecycle.receive(event); return; }
    if (event.type === "appUpdate") {
      for (const listener of updateListeners) listener({ state: event.state, message: event.message });
      return;
    }
    if (event.type === "shared") {
      for (const item of event.items)
        if (!seen.has(item.id)) {
          seen.add(item.id);
          pending.push(item);
        }
      void drain().catch((error: unknown) =>
        report("Import shared item", error),
      );
    } else if (event.type === "notificationOpen") {
      if (notificationListeners.size)
        for (const listener of notificationListeners)
          listener(event.notification);
      else notifications.push(event.notification);
    }
  };
  void invoke("device_events", { events }).catch((error: unknown) =>
    report("Device events", error),
  );
  const syncTheme = () => {
    void invoke("shell_theme", {
      dark: document.documentElement.dataset.theme === "dark",
    }).catch(() => undefined);
  };
  new MutationObserver(syncTheme).observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme"],
  });
  syncTheme();
  return {
    ...lifecycle.capabilities,
    report,
    downloadLimit: ANDROID_SAVE_LIMIT,
    async saveToPhone(blob: Blob, name: string): Promise<{ savedAs: string }> {
      const { id } = await invoke<{ id: string }>("save_begin", {
        name,
        mime: blob.type || "application/octet-stream",
        size: blob.size,
      });
      try {
        for (let offset = 0; offset < blob.size; offset += 256 * 1024) {
          const chunk = await blob
            .slice(offset, offset + 256 * 1024)
            .arrayBuffer();
          await invoke("save_chunk", chunk, { headers: { "x-save-id": id } });
        }
        return await invoke<{ savedAs: string }>("save_finish", { id });
      } catch (error) {
        await invoke("save_cancel", { id }).catch(() => undefined);
        throw error;
      }
    },
    appUpdate: {
      check: (offers) => invoke("app_update_check", { offers }),
      async install(profileId: string, app: Blob, sha256: string) {
        await invoke("app_update_begin", { profileId, size: app.size, sha256 });
        try {
          for (let offset = 0; offset < app.size; offset += 256 * 1024)
            await invoke("app_update_chunk", await app.slice(offset, offset + 256 * 1024).arrayBuffer());
          await invoke("app_update_commit");
        } catch (error) {
          await invoke("app_update_cancel").catch(() => undefined);
          throw error;
        }
      },
      onResult(listener: (result: AppUpdateResult) => void) {
        updateListeners.add(listener);
        return () => {
          updateListeners.delete(listener);
        };
      },
    } satisfies AppUpdater,
    onShared(listener: (items: SharedItem[]) => void | Promise<void>) {
      sharedListeners.add(listener);
      void drain().catch((error: unknown) =>
        report("Import shared item", error),
      );
      return () => {
        sharedListeners.delete(listener);
      };
    },
    requestNotifications: () => invoke<void>("request_notifications"),
    notify(notification: LocalNotification) {
      return invoke<void>("notify", { notification });
    },
    cancelNotifications: (ids: string[]) => invoke<void>("cancel_notifications", { ids }),
    onNotificationOpen(listener: (notification: NotificationOpen) => void) {
      notificationListeners.add(listener);
      const waiting = notifications.splice(0);
      queueMicrotask(() => {
        if (notificationListeners.has(listener))
          for (const n of waiting) listener(n);
        else notifications.unshift(...waiting);
      });
      return () => {
        notificationListeners.delete(listener);
      };
    },
    appInfo: () =>
      invoke<{ version: string; build: string; keyProtection: string; deviceName?: string }>(
        "app_info",
      ),
    keepAwake(on: boolean) {
      void invoke("keep_awake", { on }).catch((error: unknown) =>
        report("Keep screen awake", error),
      );
    },
    immersive(on: boolean) {
      // The bars only cost room; a failure leaves them showing.
      void invoke("shell_immersive", { on }).catch(() => undefined);
    },
    alerts: {
      state: () => invoke<AlertsState>("alerts_state"),
      configure: (settings: AlertSettings) => invoke<AlertsState>("alerts_configure", { settings }),
      battery: () => invoke<void>("alerts_battery"),
      channel: (channel: AlertChannel) => invoke<void>("alerts_channel", { channel }),
      seen: (profileId: string, attentionIds: string[]) => invoke<void>("alerts_seen", { profileId, attentionIds }),
      viewing: (profileId: string, on: boolean) => invoke<void>("alerts_viewing", { profileId, on }),
    },
    onPlatformError(
      listener: (error: { operation: string; message: string }) => void,
    ) {
      errorListeners.add(listener);
      const waiting = platformErrors.splice(0);
      queueMicrotask(() => {
        if (errorListeners.has(listener))
          for (const error of waiting) listener(error);
        else platformErrors.unshift(...waiting);
      });
      return () => {
        errorListeners.delete(listener);
      };
    },
    notificationPermission: () => invoke<NotificationPermission>("notification_permission"),
    openNotificationSettings: () => invoke<void>("request_notifications", { openSettings: true }),
    onForeground(listener: () => void) {
      foregroundListeners.add(listener);
      return () => { foregroundListeners.delete(listener); };
    },
  };
}
