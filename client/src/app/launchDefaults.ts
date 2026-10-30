// SPDX-License-Identifier: Apache-2.0
import type { HostSession } from "../host/session";
import { uuidv7 } from "../protocol/ids";
import type { LaunchDefault } from "../protocol/types";
import { defaultLaunchPolicy } from "./launchPolicy";
import { prefs } from "./prefs";

/** The computer starts work nobody chose flags for with the phone's defaults. */
export const LAUNCH_DEFAULTS_FEATURE = "launch_defaults_v1";

/** This phone's launch defaults, one for each agent that has them. */
export function launchDefaults(): LaunchDefault[] {
  return (["claude_code", "codex"] as const).flatMap((harness) => {
    const policy = defaultLaunchPolicy(harness);
    return policy ? [{ harness, policy }] : [];
  });
}

const same = (a: LaunchDefault[], b: LaunchDefault[]) => {
  const key = (list: LaunchDefault[]) => JSON.stringify([...list].sort((x, y) => x.harness.localeCompare(y.harness)).map((d) => [d.harness, d.policy]));
  return key(a) === key(b);
};

/**
 * Keeps the computer's launch defaults the same as this phone's, so work an
 * agent starts there, such as an issue fix, begins with the flags the person
 * starts work with here. Tried once each time the phone connects or its
 * defaults change; what fails waits for the next of those.
 */
export function keepLaunchDefaults(session: HostSession): () => void {
  let tried: LaunchDefault[] | null = null;
  let busy = false;
  let stopped = false;
  const sync = () => {
    const s = session.state.get();
    if (session.link.get().state !== "online") { tried = null; return; }
    if (busy || stopped || !s.grants?.admin || !s.capabilities?.features.includes(LAUNCH_DEFAULTS_FEATURE)) return;
    const defaults = launchDefaults();
    if (tried && same(tried, defaults)) return;
    tried = defaults; busy = true;
    const commandId = uuidv7();
    void session.query({ kind: "agent_preferences" }).then(({ view }) => {
      if (stopped || same(view.launch_defaults ?? [], defaults)) return;
      return session.command({ command: "set_launch_defaults", defaults }, { label: "Share launch defaults with this computer", expectedRevision: view.revision, commandId })
        .finally(() => session.dismissCommand(commandId));
    }).catch(() => { /* Tried again when the phone next connects or its defaults change. */ })
      .finally(() => { busy = false; if (!stopped) sync(); });
  };
  const offs = [session.link.subscribe(sync), session.state.subscribe(sync), prefs.subscribe(sync)];
  sync();
  return () => { stopped = true; offs.forEach((off) => off()); };
}
