// SPDX-License-Identifier: Apache-2.0
import type {
  CommandReceipt,
  CommandResult,
  CommandStage,
  ProtocolError,
} from "../protocol/types";
import { CommandError, type HostSession } from "./session";

/** Admission is immutable. Read a later effect from the journal/status query;
 * never resend an effect because the admission receipt has no result.
 * `waitMs` is how long the effect may take before its outcome is unknown. */
export async function commandResult(
  session: HostSession,
  receipt: CommandReceipt,
  { waitMs = 20_000 }: { waitMs?: number } = {},
): Promise<CommandResult | null> {
  if (receipt.error) throw new CommandError(receipt.error, null);
  if (receipt.result) return receipt.result;
  return new Promise((resolve, reject) => {
    let done = false;
    const unknown = () =>
      finish(undefined, {
        code: "outcome_unknown",
        message: "Reconnect to see what it recorded before trying again.",
      });
    const finish = (result?: CommandResult | null, error?: ProtocolError) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      offState();
      offLink();
      if (error)
        reject(
          new CommandError(
            error,
            session.commands.get()[receipt.command_id] ?? null,
          ),
        );
      else resolve(result ?? null);
    };
    const check = (
      stage?: CommandStage,
      result?: CommandResult | null,
      error?: ProtocolError | null,
    ) => {
      if (error) finish(undefined, error);
      else if (stage === "observed") finish(result);
      else if (stage === "failed")
        finish(undefined, {
          code: "native_failure",
          message: "The computer could not complete this request.",
        });
      else if (stage === "uncertain" || stage === "reconciliation_required")
        unknown();
    };
    const changed = () => {
      const progress = session.state.get().commandStages[receipt.command_id];
      if (progress) check(progress.stage, progress.result, progress.error);
    };
    const offState = session.state.subscribe(changed);
    const offLink = session.link.subscribe(() => {
      if (!session.online) unknown();
    });
    const timer = setTimeout(unknown, waitMs);
    changed();
    if (!done)
      void session
        .query({ kind: "command_status", command_id: receipt.command_id })
        .then(
          (status) =>
            check(
              status.stage,
              status.result ?? status.receipt?.result,
              status.error ?? status.receipt?.error,
            ),
          () => {
            if (!session.online) unknown();
          },
        );
  });
}

/** Revoking this device ends its own connection, and the computer then refuses
 * the next handshake. That refusal, not the dropped connection, confirms the
 * revocation. Resolves false when the link comes back instead, or nothing is
 * decided in time. */
export function selfRevoked(
  session: HostSession,
  ms = 15_000,
): Promise<boolean> {
  const decided = (): boolean | null => {
    const now = session.link.get();
    if (now.state === "failed") return now.reason === "revoked";
    return now.state === "online" ? false : null;
  };
  const first = decided();
  if (first !== null) return Promise.resolve(first);
  return new Promise((resolve) => {
    const settle = (revoked: boolean) => {
      clearTimeout(timer);
      off();
      resolve(revoked);
    };
    const timer = setTimeout(() => settle(false), ms);
    const off = session.link.subscribe(() => {
      const d = decided();
      if (d !== null) settle(d);
    });
  });
}

/** A change to this device's own grants closes its connection so it
 * reconnects under them. Resolves true once the link is back online. */
export function reconnected(session: HostSession, ms = 15_000): Promise<boolean> {
  if (session.link.get().state === "online") return Promise.resolve(true);
  return new Promise((resolve) => {
    const settle = (online: boolean) => {
      clearTimeout(timer);
      off();
      resolve(online);
    };
    const timer = setTimeout(() => settle(false), ms);
    const off = session.link.subscribe(() => {
      const now = session.link.get().state;
      if (now === "online") settle(true);
      else if (now === "failed") settle(false);
    });
  });
}
