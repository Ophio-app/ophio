// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import type { CommandEnvelope, CommandReceipt } from "../../protocol/types";
import { stopStore } from "./stops";
import { StopTask } from "./StopTask";

let app: App;
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start();
  await advance(900);
});
afterEach(() => {
  cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("a Stop the computer refused is not shown as accepted when its resend comes back as a duplicate", async () => {
  const host = window.__sim!.host;
  const task = app.session.state.get().tasks[ids.tFlaky]!;
  // The companion records a refused command and answers a resend of the same id
  // with that receipt marked Duplicate, its error kept (services/commands.rs dedupe).
  const stored = new Map<string, CommandReceipt>();
  const original = host.command.bind(host);
  vi.spyOn(host, "command").mockImplementation(async (env: CommandEnvelope) => {
    if (env.body.command !== "stop_task") return original(env);
    const prior = stored.get(env.command_id);
    if (prior) return { ...prior, status: "duplicate" };
    const refused: CommandReceipt = { command_id: env.command_id, status: "rejected", seq: null, result: null,
      error: { code: "temporarily_unavailable", message: "Øphio is shutting down on the computer" } };
    stored.set(env.command_id, refused);
    return refused;
  });
  render(<AppProvider app={app}><StopTask task={task} /></AppProvider>);
  // The computer refuses the Stop, but the connection drops before its receipt arrives.
  act(() => window.__sim!.dropNextReceipt());
  fireEvent.click(screen.getByRole("button", { name: /^Stop ·/ }));
  await advance(3000);
  expect(app.session.link.get().state).toBe("offline");
  // Reconnected, the phone sends the same Stop again under its id.
  await advance(15000);
  expect(app.session.link.get().state).toBe("online");
  expect(vi.mocked(host.command).mock.calls.filter(([env]) => env.body.command === "stop_task")).toHaveLength(2);
  expect(stored.size).toBe(1);
  const status = () => screen.queryByRole("status")?.textContent ?? "";
  expect(status()).not.toMatch(/accepted/i);
  expect(status()).toContain("Øphio is shutting down on the computer");
});
