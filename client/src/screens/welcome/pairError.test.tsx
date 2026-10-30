// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { Platform } from "../../host/link";
import type { PairingOffer } from "../../protocol/types";
import { Welcome } from "./Welcome";
vi.mock("../../shell/keyboard", () => ({ useKeyboardOpen: () => false, useCrampedLayout: () => false }));
afterEach(cleanup);

const offer = (): PairingOffer => ({ host_id: "host", host_name: "Desk", cert_sha256: "00".repeat(32), addresses: ["127.0.0.1"],
  pairing_secret: "fixture", expires_at: Date.now() + 60_000, protocol: { major: 1, minor: 0 } });

/** What the person reads after the phone's native layer refuses with `message`
 * (native errors reach this screen as an Error carrying only the message). */
async function refusal(message: string): Promise<string> {
  localStorage.clear();
  Element.prototype.scrollTo ??= () => {};
  const platform = { profiles: async () => [], pair: async () => { throw new Error(message); }, parsePairingLink: () => ({ offer: offer() }) } as unknown as Platform;
  render(<Welcome platform={platform} onPaired={() => {}} onCancel={() => {}} />);
  fireEvent.change(screen.getByLabelText("Pairing link"), { target: { value: "fixture" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Pair" })); });
  return screen.getByRole("alert").textContent ?? "";
}

// client/src-tauri/src/error.rs: Error::Storage, Error::Timeout and Error::Key
// are sentences that end with a full stop, and none matches a plainer wording.
it.each([
  "The phone could not save its pairing information.",
  "The computer did not answer in time.",
  "The phone could not use its protected device key.",
])("does not double the full stop of %j", async (message) => {
  const text = await refusal(message);
  expect(text).toBe(`Not paired${message} Nothing was changed on this phone.Report`);
  expect(text).not.toContain("..");
});
