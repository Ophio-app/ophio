// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { LinkState } from "../host/link";
import type { HostState } from "../state/projection";
import * as fx from "../dev/sim/scenario";
import { allGrants } from "../dev/sim/host";
import { COMPUTER_NAME_FEATURE, renameHint, renameWay } from "./computerName";

const online: LinkState = { state: "online", since: 1 };
const offline: LinkState = { state: "offline", since: 1, reason: "Offline", retryAt: null };
const caps = fx.capabilities();
const older = { ...caps, features: caps.features.filter((f) => f !== COMPUTER_NAME_FEATURE) };
type S = Pick<HostState, "capabilities" | "host" | "grants">;
const state = (o: { keeps?: boolean; admin?: boolean | null; renamed?: boolean }): S => ({
  capabilities: o.keeps === false ? older : caps,
  host: { ...fx.hostInfo(1), capabilities: o.keeps === false ? older : caps, renamed: o.renamed ?? false },
  grants: o.admin === null ? null : { ...allGrants, admin: o.admin ?? true },
});

it("renames on the computer only when it keeps names, this device may manage it, and it is connected", () => {
  expect(renameWay(state({}), online)).toBe("computer");
  // Older computers drop the connection on a request they don't know.
  expect(renameWay(state({ keeps: false }), online)).toBe("phone");
  // Offline, or not known yet: the computer would take the name back.
  expect(renameWay(state({}), offline)).toBe("wait");
  expect(renameWay(state({ admin: null }), offline)).toBe("wait");
  // A device that may not manage it names it for itself, until the owner names it there.
  expect(renameWay(state({ admin: false }), online)).toBe("phone");
  expect(renameWay(state({ admin: false, renamed: true }), online)).toBe("wait");
});

it("says plainly who sees the name", () => {
  expect(renameHint("computer", "studio", "phone", state({}), online)).toBe("The new name is kept on studio, so every phone and tablet paired with it shows it.");
  expect(renameHint("wait", "studio", "phone", state({}), offline)).toMatch(/^Connect to studio to rename it\./);
  expect(renameHint("phone", "studio", "phone", state({ keeps: false }), online)).toMatch(/^Only this phone uses this name\. Update .* on studio/);
  expect(renameHint("phone", "studio", "phone", state({ admin: false }), online)).toBe("Only this phone uses this name. This phone isn’t allowed to rename studio itself.");
});
