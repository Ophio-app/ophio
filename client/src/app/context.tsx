// SPDX-License-Identifier: Apache-2.0
//
// The connected app: platform, the paired computer's profile, its session
// and the phone-local interface state, plus hooks that read them.

import { createContext, useCallback, useContext, type ReactNode } from "react";
import type { HostProfile, LinkState, Platform } from "../host/link";
import type { Computers } from "./computers";
import { CommandError, NotConnectedError, OutcomeUnknownError, type HostSession } from "../host/session";
import type { Command, CommandReceipt, ProtocolError } from "../protocol/types";
import type { HostState } from "../state/projection";
import { shallowEqual, useStore } from "../state/store";
import { openReport } from "../reports/request";
import { toast } from "../design/Toast";
import { signInForCommand, startWhenShown } from "./signInFeature";
import type { Route, UiState, UiStore } from "./ui";

export interface App {
  platform: Platform;
  profile: HostProfile;
  /** Every computer paired with this phone. */
  profiles: HostProfile[];
  session: HostSession;
  ui: UiStore;
  /** Every paired computer's session while the app runs. The optional
   * parts are absent where a screen is shown on its own, as in tests. */
  computers?: Computers;
  /** The header shows every computer at once: projects from all of them,
   * each named with its computer. The rest of the screen is the computer
   * whose project is chosen. */
  all?: boolean;
  /** Show another paired computer alone, opening `route` there when given. */
  switchTo(profileId: string, route?: Route): void;
  /** Show every computer at once. */
  showAll?(): void;
  /** Choose a project on any paired computer, staying on the tab now shown.
   * `then` runs once it is chosen, never if the person stays where they are. */
  openProject?(profileId: string, projectId: string, then?: () => void): void;
  /** Forget a computer other than this one on this device. */
  forgetOther?(profileId: string): Promise<void>;
  /** Another computer's interface store, as All's rows read it. */
  uiOf?(profileId: string): UiStore;
  /** Switch to a computer, through the leave-control check, and then do
   * `act` on its interface, as opening one of its items from All. */
  visit?(profileId: string, act: (ui: UiStore) => void): void;
  /** Pair another computer; cancelling returns here. */
  pairAnother(): void;
  /** Rename a paired computer: on the computer, for every device, where it
   * keeps its own name, else on this device alone. */
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
  const { session, profile, platform, ui } = useApp();
  return useCallback(
    async (body, opts) => {
      platform.haptic?.("tap");
      try {
        const r = await session.command(body, { label: opts.label, expectedRevision: opts.expectedRevision ?? null });
        if (opts.success) toast(opts.success);
        return r;
      } catch (e) {
        // A report about it is filed through, and describes, this computer,
        // which may not be the one on screen, as for a row in All.
        // An agent that isn't signed in can be signed in again from here.
        const signIn = e instanceof CommandError && e.error.code === "account_not_ready" ? signInForCommand(body, session.state.get()) : null;
        if (opts.toastErrors !== false) toast(describeError(e, profile.name), { tone: "stop", code: e instanceof CommandError ? e.error.code : "command_error", action: signIn
          ? { label: "Sign in again", run: () => { startWhenShown(profile.profileId, signIn); ui.push({ screen: "sign_in", accountId: signIn }); } }
          : { label: "Report", run: () => openReport({ type: "error" }, { command: body.command, profileId: profile.profileId, error: e instanceof CommandError ? e.error : { code: "command_error", message: describeError(e, profile.name) } }) } });
        return null;
      }
    },
    [session, profile.name, profile.profileId, platform, ui],
  );
}
