// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import { createSimPlatform } from "./platform";
import type { LinkState } from "../../host/link";
afterEach(() => { window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); });
it.each(["terminal", "media", "speech", "transfer"] as const)("refuses only the next %s link and preserves its code and reason", async kind => {
  vi.useFakeTimers();
  const platform = createSimPlatform();
  window.__sim!.refuseSide(kind, "not_found", "Gone after restart.");
  const first = platform[kind]("sim-studio");
  const states: LinkState[] = [];
  first.listen(e => { if (e.type === "state") states.push(e.state); });
  await vi.advanceTimersByTimeAsync(300);
  expect(states.map(s => s.state)).toEqual(["connecting", "offline"]);
  expect(states[1]).toMatchObject({ code: "not_found", reason: "Gone after restart." });
  first.close();
  const next = platform[kind]("sim-studio");
  const refused: LinkState[] = [];
  next.listen(e => { if (e.type === "state" && e.state.state === "offline") refused.push(e.state); });
  await vi.advanceTimersByTimeAsync(300);
  expect(refused).toEqual([]);
  next.close();
});
it("restart drops in-memory terminals", () => {
  createSimPlatform();
  expect(window.__sim!.host.terminals.length).toBeGreaterThan(0);
  window.__sim!.restart();
  expect(window.__sim!.host.terminals).toEqual([]);
});
