// SPDX-License-Identifier: Apache-2.0
import { useEffect } from "react";
import type { HostSession } from "../../host/session";
import { commandResult } from "../../host/commandResult";
import type {
  ControlTarget,
  LeaseView,
  VoiceSessionView,
  VoiceTarget,
} from "../../protocol/types";
import { Store, useStore } from "../../state/store";

/** A voice session this device runs on a display, and the lease it holds.
 * The lease is this device's but voice control's: a view of the display
 * neither offers touch input under it nor releases it. A `null` lease is a
 * session still starting, which counts any lease of this device. */
export interface VoiceHold {
  sessionId: string | null;
  leaseId: string | null;
}
type Holds = Record<string, VoiceHold>;
const stores = new WeakMap<HostSession, Store<Holds>>();
const savedKey = (profileId: string, targetId: string) =>
  `voice-session.${profileId}.${targetId}`;

export function voiceHolds(session: HostSession): Store<Holds> {
  let store = stores.get(session);
  if (!store) {
    store = new Store<Holds>({});
    stores.set(session, store);
  }
  return store;
}

export function holdVoice(
  session: HostSession,
  profileId: string,
  targetId: string,
  hold: VoiceHold,
): void {
  voiceHolds(session).set((s) =>
    s[targetId]?.sessionId === hold.sessionId &&
    s[targetId]?.leaseId === hold.leaseId
      ? s
      : { ...s, [targetId]: hold },
  );
  // Kept so a reloaded app still knows whose lease it is.
  if (hold.sessionId && hold.leaseId)
    try {
      localStorage.setItem(savedKey(profileId, targetId), JSON.stringify(hold));
    } catch {
      /* Only this run of the app knows it. */
    }
}

/** Forget the display's voice session; given an id, only that session. */
export function dropVoice(
  session: HostSession,
  profileId: string,
  targetId: string,
  sessionId?: string | null,
): void {
  const held = voiceHolds(session).get()[targetId];
  if (held && (sessionId === undefined || held.sessionId === sessionId))
    voiceHolds(session).set((s) => {
      const rest = { ...s };
      delete rest[targetId];
      return rest;
    });
  try {
    const saved = JSON.parse(
      localStorage.getItem(savedKey(profileId, targetId)) ?? "null",
    ) as VoiceHold | null;
    if (saved && (sessionId === undefined || saved.sessionId === sessionId))
      localStorage.removeItem(savedKey(profileId, targetId));
  } catch {
    /* Nothing kept. */
  }
}

export function voiceHoldsLease(
  session: HostSession,
  lease: LeaseView | null | undefined,
): boolean {
  if (!lease) return false;
  const hold = voiceHolds(session).get()[lease.target_id];
  return !!hold && (hold.leaseId === null || hold.leaseId === lease.lease_id);
}

/** The display's voice session, including one started before a reload. */
export function useVoiceHold(
  session: HostSession,
  profileId: string,
  targetId: string | undefined,
): VoiceHold | undefined {
  useEffect(() => {
    if (!targetId || voiceHolds(session).get()[targetId]) return;
    try {
      const saved = JSON.parse(
        localStorage.getItem(savedKey(profileId, targetId)) ?? "null",
      ) as Partial<VoiceHold> | null;
      if (
        typeof saved?.sessionId === "string" &&
        typeof saved.leaseId === "string"
      )
        holdVoice(session, profileId, targetId, {
          sessionId: saved.sessionId,
          leaseId: saved.leaseId,
        });
    } catch {
      /* Nothing kept. */
    }
  }, [session, profileId, targetId]);
  return useStore(voiceHolds(session), (s) =>
    targetId ? s[targetId] : undefined,
  );
}

/** Start voice control, holding the display's lease for it from the moment
 * it may be granted. Null when the computer did not confirm a session. */
export async function startVoice(
  session: HostSession,
  profileId: string,
  target: ControlTarget,
  kind: VoiceTarget,
  dryRun: boolean,
): Promise<VoiceSessionView | null> {
  const id = target.target_id;
  holdVoice(session, profileId, id, { sessionId: null, leaseId: null });
  let started: VoiceSessionView | null = null;
  try {
    const receipt = await session.command(
      {
        command: "voice",
        operation: {
          operation: "start",
          target: kind,
          target_id: id,
          geometry_revision: target.geometry.revision,
          dry_run: dryRun,
          disclosure_accepted: true,
        },
      },
      { label: "Start voice control", expiresInMs: 10000 },
    );
    const result = await commandResult(session, receipt);
    if (result?.kind === "voice_session") started = result.session;
    return started;
  } finally {
    if (started)
      holdVoice(session, profileId, id, {
        sessionId: started.session_id,
        leaseId: started.lease.lease_id,
      });
    else dropVoice(session, profileId, id, null);
  }
}
