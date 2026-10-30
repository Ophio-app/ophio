// SPDX-License-Identifier: Apache-2.0
//
// The connected app: platform, the paired computer's profile, its session
// and the phone-local interface state, plus hooks that read them.

import { createContext, useCallback, useContext, type ReactNode } from "react";
import type { HostProfile, LinkState, Platform } from "../host/link";
import { CommandError, NotConnectedError, OutcomeUnknownError, type HostSession } from "../host/session";
import type { Command, CommandReceipt, ProtocolError } from "../protocol/types";
import type { HostState } from "../state/projection";
import { shallowEqual, useStore } from "../state/store";
import { openReport } from "../reports/request";
import { toast } from "../design/Toast";
import type { Route, UiState, UiStore } from "./ui";

export interface App {
  platform: Platform;
  profile: HostProfile;
  /** Every computer paired with this phone. */
  profiles: HostProfile[];
  session: HostSession;
  ui: UiStore;
  /** Stop this computer's session and connect to another paired one,
   * opening `route` there when given. */
  switchTo(profileId: string, route?: Route): void;
  /** Pair another computer; cancelling returns here. */
  pairAnother(): void;
  /** Change the name this phone shows for a paired computer. */
  rename(profileId: string, name: string): Promise<void>;
  /** Forget this computer on this phone and return to pairing. */
  forget(): Promise<void>;
}

const Ctx = createContext<App | null>(null);

export function AppProvider({ app, children }: { app: App; children: ReactNode }) {
  return <Ctx.Provider value={app}>{children}</Ctx.Provider>;
}

export function useApp(): App {
  const app = useContext(Ctx);
  if (!app) throw new Error("useApp outside AppProvider");
  return app;
}

/** Select from the host projection. Pass `shallowEqual` for derived objects/arrays. */
export function useHost<S>(selector: (s: HostState) => S, equal: (a: S, b: S) => boolean = Object.is): S {
  return useStore(useApp().session.state, selector, equal);
}

export function useUi<S>(selector: (s: UiState) => S, equal: (a: S, b: S) => boolean = Object.is): S {
  return useStore(useApp().ui, selector, equal);
}

const linkSelf = (l: LinkState) => l;
export function useLink(): LinkState {
  return useStore(useApp().session.link, linkSelf);
}

export { shallowEqual };

/** A plain-language explanation of why an action did not happen. */
export function describeError(e: unknown, computer: string): string {
  if (e instanceof NotConnectedError) return `Not sent. ${computer} isn’t connected right now.`;
  if (e instanceof OutcomeUnknownError)
    return `Sent, but the connection dropped before ${computer} answered. It may have happened; this device will check when it reconnects.`;
  if (e instanceof CommandError) return describeProtocolError(e.error);
  if (e instanceof Error) return e.message;
  return "Something went wrong.";
}

export function describeProtocolError(e: ProtocolError): string {
  switch (e.code) {
    case "permission_denied":
      return e.message || "This device isn’t allowed to do that.";
    case "stale_revision":
      return "It changed on the computer. Review the current version first.";
    case "already_resolved":
      return "Already handled somewhere else.";
    case "expired":
      return "That request expired.";
    case "outcome_unknown":
      return `The computer couldn’t confirm the result. ${e.message}`;
    default:
      return e.message;
  }
}

export interface RunOptions {
  label: string;
  expectedRevision?: number | null;
  /** Shown when the computer accepts. Omit when the screen shows the result itself. */
  success?: string;
  /** Show errors as a toast (default). Set false when the caller renders them inline. */
  toastErrors?: boolean;
}

/**
 * Send a command and report the outcome. Resolves to the receipt, or null
 * when the command was refused or not sent (already reported).
 */
export function useCommand(): (body: Command, opts: RunOptions) => Promise<CommandReceipt | null> {
  const { session, profile, platform } = useApp();
  return useCallback(
    async (body, opts) => {
      platform.haptic?.("tap");
      try {
        const r = await session.command(body, { label: opts.label, expectedRevision: opts.expectedRevision ?? null });
        if (opts.success) toast(opts.success);
        return r;
      } catch (e) {
        if (opts.toastErrors !== false) toast(describeError(e, profile.name), { tone: "stop", code: e instanceof CommandError ? e.error.code : "command_error", action: { label: "Report", run: () => openReport({ type: "error" }, { command: body.command, error: e instanceof CommandError ? e.error : { code: "command_error", message: describeError(e, profile.name) } }) } });
        return null;
      }
    },
    [session, profile.name, platform],
  );
}
