// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "../app/App";
import { useApp } from "../app/context";
import { prefs } from "../app/prefs";
import { FakeLink, platform, profile } from "./lifecycle.fixtures";
import * as fx from "../dev/sim/scenario";

let fake: ReturnType<typeof platform>;
vi.mock("../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../shell/keyboard", () => ({ useKeyboardOpen: () => false, useCrampedLayout: () => false }));
vi.mock("../shell/Shell", () => ({ Shell: function TestShell() {
  const app = useApp();
  const [error, setError] = useState<string | null>(null);
  return <div><p>Computer {app.profile.name}</p>
    <button onClick={() => void app.forget().catch((e: Error) => setError(e.message))}>Forget this computer</button>
    {error && <p role="alert">{error}</p>}
  </div>;
} }));

beforeEach(() => {
  localStorage.clear();
  prefs.set({ theme: "system", reduceMotion: false, lastProfileId: null, claudeBypassPermissions: true, codexBypassPermissions: true });
  Element.prototype.scrollTo ??= () => {};
  fake = platform();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);

it("forgets a computer that revoked this device while it held control", async () => {
  const link = new FakeLink();
  link.snap.control = [{ target_id: fx.ids.tgDp1, input_state: "human", lease: { lease_id: fx.ids.lease1, target_id: fx.ids.tgDp1, owner: { kind: "human", device_id: profile.deviceId }, state: "active", generation: 1, boot_epoch: fx.ids.boot, granted_at: 1, renewal_window_ms: 15000 }, model_observation: null, private_step: null }];
  fake.value.control = () => link;
  const forget = vi.fn(async () => {});
  fake.value.forget = forget;
  render(<App />);
  await screen.findByText("Computer studio");
  await waitFor(() => expect(link.listeners.size).toBeGreaterThan(0));
  await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
  // The owner revokes this device on the computer; it refuses every later
  // connection, so control can't be left or confirmed from here any more.
  act(() => link.emit({ type: "state", state: { state: "failed", reason: "revoked", detail: "This device’s access was revoked." } }));
  fireEvent.click(screen.getByRole("button", { name: "Forget this computer" }));
  await waitFor(() => expect(forget).toHaveBeenCalledWith(profile.profileId));
  expect(screen.queryByRole("alert")).toBeNull();
});
