// SPDX-License-Identifier: Apache-2.0
import { useRef, useState } from "react";
import { CHANGING_AFTER_MS } from "../app/commandControl";
import { describeError, useApp, useHost } from "../app/context";
import { PROGRAM_NAME } from "../brand";
import { Notice } from "../design/Notice";
import { Changing } from "../design/StateLine";
import { toast } from "../design/Toast";
import { commandResult, reconnected } from "../host/commandResult";
import type { Grants } from "../protocol/types";
import { GRANT_WORDS, withGrant } from "../screens/settings/helpers";
import "./NotAllowed.css";

/** False only once the computer has said this device lacks `grant`; while
 * grants are unknown the computer stays the judge. */
export function useMay(grant: keyof Grants): boolean {
  return useHost((s) => s.grants?.[grant] !== false);
}

/** The companion's `devices allow` words, by the grant each allows. */
const ALLOW_WORDS: Record<string, keyof Grants> = {
  view: "view",
  control: "control_input",
  terminal: "terminal",
  tasks: "tasks",
  approvals: "approvals",
  "computer-use": "computer_use",
  "library-read": "library_read",
  "library-write": "library_write",
  transfers: "transfers",
  manage: "admin",
  accounts: "accounts",
};

/** The grants `permission`'s words name that this device lacks. */
export function missingGrants(permission: string, grants: Grants | null | undefined): (keyof Grants)[] {
  return permission
    .split(/\s+/)
    .flatMap((word) => (word in ALLOW_WORDS ? [ALLOW_WORDS[word]!] : []))
    .filter((grant) => grants?.[grant] !== true);
}

const NONE = Object.fromEntries(Object.keys(GRANT_WORDS).map((k) => [k, false])) as unknown as Grants;

/** Why a section is empty when this device wasn't granted what it needs.
 * A device that manages access gets a switch for each grant it lacks;
 * any other gets the exact command that allows it. `permission` is the
 * companion's `devices allow` words; `computerOnly` is a grant no device
 * can give. */
export function NotAllowed({
  what,
  permission,
  detail,
  computerOnly,
}: {
  what: string;
  permission: string;
  detail?: string;
  computerOnly?: boolean;
}) {
  const { profile, session } = useApp();
  const grants = useHost((s) => s.grants);
  // The grants asked for, and those that have taken a moment; a switch
  // shows what is in force until the computer says the grant is.
  const [asked, setAsked] = useState<ReadonlySet<keyof Grants>>(new Set());
  const [slow, setSlow] = useState<ReadonlySet<keyof Grants>>(new Set());
  // Each change closes this device's connection, so they go one at a time.
  const line = useRef(Promise.resolve());
  const missing = missingGrants(permission, grants);
  // Accounts are the computer's alone to allow, whoever manages access.
  const switches = !computerOnly && grants?.admin === true && missing.length > 0 && !missing.includes("accounts");
  const allow = (grant: keyof Grants) => {
    if (asked.has(grant)) return;
    const mark = (set: typeof setAsked, on: boolean) => set((s) => { const next = new Set(s); if (on) next.add(grant); else next.delete(grant); return next; });
    mark(setAsked, true);
    const timer = setTimeout(() => mark(setSlow, true), CHANGING_AFTER_MS);
    line.current = line.current.then(() => send(grant)).finally(() => { clearTimeout(timer); mark(setAsked, false); mark(setSlow, false); });
  };
  const send = async (grant: keyof Grants) => {
    try {
      const receipt = await session.command(
        { command: "change_device_grants", device_id: profile.deviceId, mask: withGrant(NONE, grant, true), allowed: true },
        { label: `Allow ${GRANT_WORDS[grant]}` },
      );
      // A change to this device's own grants closes its connection; the
      // result is read once it is back under them.
      try {
        await commandResult(session, receipt);
      } catch (e) {
        if (!(await reconnected(session))) throw e;
        await commandResult(session, receipt);
      }
    } catch (e) {
      toast(`Couldn’t allow ${GRANT_WORDS[grant]}. ${describeError(e, profile.name)}`, { tone: "stop" });
    }
  };
  if (switches)
    return (
      <Notice tone="warn" className="permission-note" title={`This device may not ${what}`}>
        {detail && <p>{detail}</p>}
        <div className="permission-note__switches">
          {missing.map((grant) => (
            <div key={grant}>
              <label className="permission-note__switch">
                <input type="checkbox" role="switch" checked={false} onChange={() => allow(grant)} />
                <span>Allow {GRANT_WORDS[grant]}</span>
              </label>
              {slow.has(grant) && <Changing to={`allowing ${GRANT_WORDS[grant]}`} />}
            </div>
          ))}
        </div>
      </Notice>
    );
  return (
    <Notice tone="warn" className="permission-note" title={`This device may not ${what}`}>
      {detail && <>{detail} </>}
      To allow it, run{" "}
      <code className="cmd cmd--inline">
        {PROGRAM_NAME} devices allow {profile.deviceId} {permission}
      </code>{" "}
      on {profile.name}
      {computerOnly ? ". Only the computer can allow it" : ", or allow Manage access for this device once, and switch it on here"}.
      This device reconnects on its own.
    </Notice>
  );
}
