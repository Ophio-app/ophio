// SPDX-License-Identifier: Apache-2.0
import type { HostSession } from "../../host/session";
import type { LeaseView } from "../../protocol/types";
import { useStore } from "../../state/store";
import { useNow } from "../../design/time";

export type Leaving = "pending" | "unknown" | null;
/** Admission alone does not end the lease. An unanswered release stays honest. */
export function useLeaving(session: HostSession, lease: LeaseView | null | undefined): Leaving {
  const now = useNow();
  const command = useStore(session.commands, (all) => Object.values(all).filter((c) =>
    c.body.command === "release_lease" && c.body.lease_id === lease?.lease_id).at(-1));
  const effect = useStore(session.state, (s) => command ? s.commandStages[command.id] : undefined);
  if (!lease || lease.state !== "active" || !command) return null;
  if (command.phase === "unknown" || effect?.error || ["rejected", "not_delivered"].includes(command.phase)
    || now - command.issuedAt >= 8_000) return "unknown";
  return "pending";
}
