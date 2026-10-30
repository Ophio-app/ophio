// SPDX-License-Identifier: Apache-2.0
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { commandResult, selfRevoked } from "./commandResult";
import { HostSession } from "./session";
import type { ControlLink } from "./link";
import type { CommandReceipt } from "../protocol/types";
const receipt: CommandReceipt = {
  command_id: "effect",
  status: "accepted",
  seq: 1,
  result: null,
  error: null,
};
beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());
function session() {
  const s = new HostSession("phone", {} as ControlLink);
  s.link.set({ state: "online", since: 1 });
  vi.spyOn(s, "query").mockResolvedValue({
    kind: "command_status",
    receipt,
    stage: "host_accepted",
  });
  return s;
}
it("keeps admission immutable and awaits a delayed journal result", async () => {
  const s = session(),
    waiting = commandResult(s, receipt);
  s.state.set((state) => ({
    ...state,
    commandStages: {
      effect: {
        stage: "observed",
        at: 2,
        detail: null,
        result: { kind: "created", path: "file.md", revision: "new" },
      },
    },
  }));
  expect(await waiting).toEqual({
    kind: "created",
    path: "file.md",
    revision: "new",
  });
  expect(receipt.result).toBeNull();
});
it("recovers the durable outcome when the event preceded the waiter", async () => {
  const s = session();
  vi.mocked(s.query).mockResolvedValue({
    kind: "command_status",
    receipt,
    stage: "observed",
    result: { kind: "created", path: "file.md", revision: "new" },
  });
  expect(await commandResult(s, receipt)).toMatchObject({ kind: "created" });
});
it("returns typed post-admission failures instead of a success toast", async () => {
  const s = session(),
    waiting = commandResult(s, receipt);
  const rejected = expect(waiting).rejects.toMatchObject({
    error: { code: "path_not_allowed" },
  });
  s.state.set((state) => ({
    ...state,
    commandStages: {
      effect: {
        stage: "failed",
        at: 2,
        detail: null,
        error: { code: "path_not_allowed", message: "Root removed" },
      },
    },
  }));
  await rejected;
});
it("reports unknown on disconnect/timeout and does not resend", async () => {
  const s = session(),
    waiting = commandResult(s, receipt);
  const rejected = expect(waiting).rejects.toMatchObject({
    error: { code: "outcome_unknown" },
  });
  s.link.set({ state: "offline", since: 2, reason: "Fixture", retryAt: null });
  await rejected;
  const next = session(),
    timeout = commandResult(next, receipt);
  const late = expect(timeout).rejects.toMatchObject({
    error: { code: "outcome_unknown" },
  });
  await vi.advanceTimersByTimeAsync(20_000);
  await late;
});
it("confirms a self-revocation by the refused reconnect, not the dropped connection", async () => {
  const s = session();
  s.link.set({ state: "offline", since: 2, reason: "Closed", retryAt: null });
  const revoked = selfRevoked(s);
  s.link.set({ state: "connecting", attempt: 1 });
  s.link.set({ state: "failed", reason: "revoked", detail: "Refused" });
  expect(await revoked).toBe(true);
  const back = session();
  back.link.set({ state: "offline", since: 2, reason: "Closed", retryAt: null });
  const returned = selfRevoked(back);
  back.link.set({ state: "online", since: 3 });
  expect(await returned).toBe(false);
  const silent = session();
  silent.link.set({ state: "offline", since: 2, reason: "Closed", retryAt: null });
  const undecided = selfRevoked(silent);
  await vi.advanceTimersByTimeAsync(15_000);
  expect(await undecided).toBe(false);
});
