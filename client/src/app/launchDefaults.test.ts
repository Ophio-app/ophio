// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import type { HostSession } from "../host/session";
import type { LinkState } from "../host/link";
import type { AgentPreferences, Command } from "../protocol/types";
import { emptyHostState, type HostState } from "../state/projection";
import { Store } from "../state/store";
import { allGrants } from "../dev/sim/host";
import { capabilities } from "../dev/sim/scenario";
import { keepLaunchDefaults } from "./launchDefaults";
import { prefs } from "./prefs";

afterEach(() => { prefs.set((p) => ({ ...p, claudeBypassPermissions: true, codexBypassPermissions: true })); });

function computer(view: AgentPreferences, features = capabilities().features) {
  const sent: { body: Command; expectedRevision?: number | null }[] = [];
  const link = new Store<LinkState>({ state: "connecting" } as LinkState);
  const state = new Store<HostState>({ ...emptyHostState, grants: allGrants, capabilities: { ...capabilities(), features } });
  const session = {
    link, state,
    query: vi.fn(async () => ({ kind: "agent_preferences", view: structuredClone(view) })),
    command: vi.fn(async (body: Command, opts: { expectedRevision?: number | null }) => {
      sent.push({ body, expectedRevision: opts.expectedRevision });
      if (body.command === "set_launch_defaults") view = { ...view, launch_defaults: body.defaults, revision: view.revision + 1 };
      return { command_id: "c", status: "accepted", seq: 1, result: null, error: null };
    }),
    dismissCommand: vi.fn(),
  };
  const online = () => link.set({ state: "online" } as LinkState);
  return { session: session as unknown as HostSession, sent, online, link, state, raw: session };
}
const settle = () => new Promise((r) => setTimeout(r, 0));

it("tells a computer this phone's launch defaults once it connects", async () => {
  const c = computer({ questions: true, revision: 3 });
  const stop = keepLaunchDefaults(c.session);
  await settle();
  expect(c.sent).toEqual([]);
  c.online(); await settle();
  expect(c.sent).toEqual([{ body: { command: "set_launch_defaults", defaults: [
    { harness: "claude_code", policy: "bypass_permissions" },
    { harness: "codex", policy: "bypass_permissions" },
  ] }, expectedRevision: 3 }]);
  // The send isn't left among the person's own commands.
  expect(c.raw.dismissCommand).toHaveBeenCalled();
  // Other changes on the computer don't send them again.
  c.state.set((s) => ({ ...s, lastHeardAt: 1 })); await settle();
  expect(c.sent).toHaveLength(1);
  // A changed default is sent at once, with the revision it read.
  prefs.set((p) => ({ ...p, codexBypassPermissions: false })); await settle();
  expect(c.sent.at(-1)).toEqual({ body: { command: "set_launch_defaults", defaults: [
    { harness: "claude_code", policy: "bypass_permissions" },
    { harness: "codex", policy: "standard" },
  ] }, expectedRevision: 4 });
  stop();
});

it("leaves a computer alone that has them already, is older, or isn't the person's to change", async () => {
  const same = computer({ questions: true, revision: 1, launch_defaults: [
    { harness: "codex", policy: "bypass_permissions" },
    { harness: "claude_code", policy: "bypass_permissions" },
  ] });
  const stopSame = keepLaunchDefaults(same.session);
  same.online(); await settle();
  expect(same.raw.query).toHaveBeenCalledTimes(1);
  expect(same.sent).toEqual([]);
  stopSame();

  const older = computer({ questions: true, revision: 0 }, capabilities().features.filter((f) => f !== "launch_defaults_v1"));
  const stopOlder = keepLaunchDefaults(older.session);
  older.online(); await settle();
  expect(older.raw.query).not.toHaveBeenCalled();
  stopOlder();

  const viewer = computer({ questions: true, revision: 0 });
  viewer.state.set((s) => ({ ...s, grants: { ...allGrants, admin: false } }));
  const stopViewer = keepLaunchDefaults(viewer.session);
  viewer.online(); await settle();
  expect(viewer.raw.query).not.toHaveBeenCalled();
  stopViewer();
});

it("tries a refused send again only after the phone reconnects", async () => {
  const c = computer({ questions: true, revision: 0 });
  c.raw.command.mockRejectedValueOnce(new Error("stale"));
  const stop = keepLaunchDefaults(c.session);
  c.online(); await settle();
  expect(c.raw.command).toHaveBeenCalledTimes(1);
  c.state.set((s) => ({ ...s, lastHeardAt: 2 })); await settle();
  expect(c.raw.command).toHaveBeenCalledTimes(1);
  c.link.set({ state: "offline" } as LinkState); c.online(); await settle();
  expect(c.raw.command).toHaveBeenCalledTimes(2);
  expect(c.sent.at(-1)?.body.command).toBe("set_launch_defaults");
  stop();
});
