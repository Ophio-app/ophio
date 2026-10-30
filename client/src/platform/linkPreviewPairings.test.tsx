// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { profile, FakeLink, platform } from "./lifecycle.fixtures";
import { HostSession } from "../host/session";
import * as fx from "../dev/sim/scenario";
import type { HostProfile } from "../host/link";
import { LinkPreview } from "./LinkPreview";

beforeEach(() => {
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);

// "Pair again" on the same computer keeps the earlier pairing on this device
// (Welcome says so), so two profiles can carry one host id.
const earlier: HostProfile = { ...profile, profileId: "test-earlier", pairedAt: 0, deviceId: "earlier-device" };

it("opens a link to the connected computer on the pairing in use, not an earlier pairing of it", async () => {
  const l = new FakeLink(), session = new HostSession(profile.profileId, l); session.start();
  await waitFor(() => expect(session.online).toBe(true));
  const p = platform(), open = vi.fn();
  const control = vi.fn(p.value.control);
  p.value.control = control;
  render(<LinkPreview existingSession={session} platform={p.value} profiles={[earlier, profile]}
    link={{ hostId: fx.ids.host, kind: "task", id: fx.ids.tFlaky }} onClose={() => {}} onOpen={open} />);
  await screen.findByRole("button", { name: "Open item" });
  fireEvent.click(screen.getByRole("button", { name: "Open item" }));
  // The earlier pairing may be revoked on the computer; the link must not
  // open a second connection with it or move the app over to it.
  expect(control).not.toHaveBeenCalled();
  expect(open).toHaveBeenCalledWith(profile, { screen: "receipt", taskId: fx.ids.tFlaky }, session);
  session.stop();
});
