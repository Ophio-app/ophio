// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { KeyRound } from "lucide-react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { commandResult, reconnected } from "../../host/commandResult";
import { OutcomeUnknownError, type HostSession } from "../../host/session";
import type { CommandReceipt } from "../../protocol/types";
import { uuidv7 } from "../../protocol/ids";
import { Store, useStore } from "../../state/store";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { NotAllowed } from "../../shell/NotAllowed";
import { canUnlockHere } from "./readiness";
import { useOnScreen } from "./useControl";
import "./unlock.css";

/** Long enough for the computer to check the password, release its lock
 * screen and report that it unlocked. */
const UNLOCK_WAIT_MS = 40_000;

/** The unlock being checked, by its command's id, and why the last one didn't
 * unlock the computer, shared by every place unlocking is offered. Never the
 * password. */
interface Unlocking {
  checking: string | null;
  error: string | null;
  /** The kind of place it was asked from, as its button says: only those
   * show it, so one screen never shows it twice. */
  from: string;
}

interface Shared {
  state: Store<Unlocking>;
  /** How often the computer has been seen to lock or unlock, so a try that
   * spanned one says nothing about the lock after it. */
  lockChanges: number;
}

const unlocking = new WeakMap<HostSession, Shared>();

function unlockingFor(session: HostSession): Shared {
  const found = unlocking.get(session);
  if (found) return found;
  const isLocked = () => Object.values(session.state.get().control).some((v) => v.desktop_lock === "locked");
  const created: Shared = { state: new Store<Unlocking>({ checking: null, error: null, from: PLAIN }), lockChanges: 0 };
  let locked = isLocked();
  // Why the last try failed is old news once the computer has been seen to
  // unlock or lock again, however that happened.
  session.state.subscribe(() => {
    if (isLocked() === locked) return;
    locked = !locked;
    created.lockChanges += 1;
    if (created.state.get().error !== null) created.state.set((s) => ({ ...s, error: null }));
  });
  unlocking.set(session, created);
  return created;
}

/** Sends the password once and follows that one unlock to its end. While the
 * computer may still be checking it, through a dropped connection too, no
 * other password is sent from here. Resolves true once the computer unlocked. */
async function unlockWith(session: HostSession, password: string, computer: string, from: string): Promise<boolean> {
  const shared = unlockingFor(session);
  if (shared.state.get().checking !== null) return false;
  const id = uuidv7();
  const since = shared.lockChanges;
  shared.state.set({ checking: id, error: null, from });
  announce("Unlocking");
  let failure: unknown = null;
  try {
    await checked(session, id, password, computer);
  } catch (e) {
    failure = e;
  }
  // A failure from before the computer last locked or unlocked is not about
  // the lock it has now. The field reads a failure out.
  const error = failure !== null && shared.lockChanges === since ? describeError(failure, computer) : null;
  shared.state.set({ checking: null, error, from });
  if (failure === null) announce("Unlocked");
  return failure === null;
}

async function checked(session: HostSession, id: string, password: string, computer: string): Promise<void> {
  let receipt: CommandReceipt | null = null;
  try {
    receipt = await session.command(
      { command: "unlock_desktop", password },
      { label: "Unlock requested", commandId: id },
    );
  } catch (e) {
    if (!(e instanceof OutcomeUnknownError)) throw e;
  }
  for (;;) {
    if (receipt) {
      try {
        await commandResult(session, receipt, { waitMs: UNLOCK_WAIT_MS });
        return;
      } catch (e) {
        // Only a dropped connection leaves the computer perhaps still
        // checking it; a check that took too long or ended uncertain is over.
        if (session.online) throw e;
      }
    }
    // Ask how it ended once connected again, by its id, never sending the
    // password again.
    while (!(await reconnected(session, UNLOCK_WAIT_MS)))
      if (session.link.get().state === "failed") throw new OutcomeUnknownError(id);
    try {
      const status = await session.query({ kind: "command_status", command_id: id });
      if (!status.receipt) throw new Error(`It didn’t reach ${computer}, so nothing was checked. Try again.`);
      receipt = status.receipt;
    } catch (e) {
      if (session.online) throw e;
      receipt = null;
    }
  }
}

const PLAIN = "Unlock with password";

/** Whether the screen `targetId` is locked, or any screen when none is named:
 * wherever that stops something, its unlock is offered right there. */
export function useLocked(targetId?: string | null): boolean {
  return useHost((s) => targetId === undefined
    ? Object.values(s.control).some((v) => v.desktop_lock === "locked")
    : !!targetId && s.control[targetId]?.desktop_lock === "locked");
}

/** Whether `targetId` is locked and this device can unlock it with its
 * password, so a lock in the way is undone where it stops something rather
 * than in Control. */
export function useUnlockable(targetId: string | null | undefined): boolean {
  const locked = useLocked(targetId ?? null);
  const can = useHost((s) => canUnlockHere(s.capabilities, s.grants));
  return locked && can;
}

interface UnlockProps {
  /** What its button says, when unlocking from it does more. */
  label?: string;
  /** Done once the computer unlocked from here. */
  onUnlocked?: () => void;
}

/**
 * Unlock a locked computer with its account's password, which the computer
 * checks. The password is sent once and not kept, here or there. It opens the whole computer, so it takes Manage as well as Control.
 */
export function Unlock(props: UnlockProps) {
  const grants = useHost((s) => s.grants);
  const available = useHost((s) => s.capabilities?.unlock_session?.state === "available");
  const onScreen = useOnScreen();
  // Without Control the screen already says what is missing.
  if (!available || grants?.control_input === false) return null;
  if (grants?.admin === false)
    return (
      <NotAllowed
        what="unlock the computer"
        permission="manage"
        detail="Its password opens the whole computer, so unlocking it from here takes Manage access as well as Control."
      />
    );
  // The form goes, and what was typed with it, whenever it can't be used or
  // seen: behind a gate that closed, on a screen left for another, or with
  // the app in the background. It comes back empty.
  return onScreen ? <UnlockForm {...props} /> : null;
}

function UnlockForm({ label = PLAIN, onUnlocked }: UnlockProps) {
  const { profile, session } = useApp();
  const online = useLink().state === "online";
  const shared = unlockingFor(session).state;
  const { checking, error: lastError, from } = useStore(shared, (s) => s);
  const busy = checking !== null && from === label;
  const error = from === label ? lastError : null;
  const [typing, setTyping] = useState(false);
  const [password, setPassword] = useState("");
  // An unlock asked for elsewhere is shown here as it is checked, and its
  // failure after.
  const open = typing || busy || error !== null;
  const unlock = async () => {
    const sent = password;
    setPassword("");
    if (await unlockWith(session, sent, profile.name, label)) {
      setTyping(false);
      onUnlocked?.();
    }
  };
  const cancel = () => {
    setPassword("");
    shared.set((s) => ({ ...s, error: null }));
    setTyping(false);
  };
  return (
    <div className="control-unlock">
      {open ? (
        <form
          className="control-unlock__form"
          onSubmit={(e) => {
            e.preventDefault();
            if (password && !busy) void unlock();
          }}
        >
          <TextField
            label={`${profile.name}’s password`}
            type="password"
            value={password}
            onChange={setPassword}
            autoFocus
            autoComplete="current-password"
            spellCheck={false}
            autoCapitalize="off"
            disabled={busy}
            error={error ?? undefined}
            hint={`${profile.name} checks it against its account’s password. It is sent once and not kept.`}
          />
          <div className="control-unlock__actions">
            <Button variant="quiet" type="button" disabled={busy} onClick={cancel}>
              Cancel
            </Button>
            <Button
              variant="primary"
              type="submit"
              disabled={!online || !password || checking !== null}
              pending={busy}
            >
              Unlock
            </Button>
          </div>
          <p className="fine">
            After three tries that don’t unlock it, unlocking from here pauses
            for 15 minutes.
          </p>
        </form>
      ) : (
        <Button icon={KeyRound} disabled={!online} onClick={() => setTyping(true)}>
          {label}
        </Button>
      )}
    </div>
  );
}
