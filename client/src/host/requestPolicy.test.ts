// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { ClientFrame, Command, Query } from "../protocol/types";
import type { ControlLink, LinkEvent } from "./link";
import { capabilities } from "../dev/sim/scenario";
import { commandPolicy, queryPolicy, requestRefusal, type RequestPolicy } from "./requestPolicy";
import { HostSession } from "./session";

class RecordingLink implements ControlLink {
  sent: ClientFrame[] = [];
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) { this.sent.push(frame); return true; }
  listen(_: (event: LinkEvent) => void) { return () => {}; }
}

// These go through the real public send methods. A decorative inventory, or
// removing the enforcement calls, must not leave the test green.
for (const [kind, policies] of [["command", commandPolicy], ["query", queryPolicy]] as const) {
  for (const [name, policy] of Object.entries(policies) as [string, RequestPolicy][]) {
    if (!("feature" in policy)) continue;
    it(`${kind} ${name} sends nothing without ${policy.feature}, then recovers when advertised`, async () => {
      const link = new RecordingLink();
      const session = new HostSession(`policy-${name}`, link);
      const send = () => kind === "command"
        ? session.command({ command: name } as Command)
        : session.query({ kind: name } as Query);
      try {
        await expect(send()).rejects.toMatchObject({ error: { code: expect.stringMatching(/incompatible_version|unsupported/) } });
        expect(link.sent).toEqual([]);
        session.state.set((state) => ({ ...state, capabilities: { ...capabilities(), features: [policy.feature] } }));
        const pending = send().catch(() => undefined);
        expect(link.sent).toHaveLength(1);
        expect(link.sent[0]?.type).toBe(kind);
        session.stop();
        await pending;
      } finally { session.stop(); }
    });
  }
}

it("allows explicit compatibility exceptions, not unknown future requests", () => {
  expect(requestRefusal({ allow: "Baseline session outcome recovery." }, [])).toBeNull();
  expect(requestRefusal(undefined, ["future_v1"])).toContain("Nothing was sent");
});

it("unknown request names cannot bypass either send path", async () => {
  const link = new RecordingLink();
  const session = new HostSession("unknown-policy", link);
  await expect(session.command({ command: "new_unmapped_request" } as unknown as Command)).rejects.toMatchObject({ error: { code: "incompatible_version" } });
  await expect(session.query({ kind: "new_unmapped_request" } as unknown as Query)).rejects.toMatchObject({ error: { code: "incompatible_version" } });
  expect(link.sent).toEqual([]);
  session.stop();
});
