// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { computerUseBlocker } from "./Control";
import { allGrants } from "../../dev/sim/host";
it("names only the missing computer-use permissions", () => {
  expect(computerUseBlocker(true, "project", null, { ...allGrants, computer_use: false }))
    .toBe("This device needs computer use permission.");
  expect(computerUseBlocker(true, "project", null, { ...allGrants, view: false, control_input: false }))
    .toBe("This device needs desktop viewing and input permissions.");
  expect(computerUseBlocker(true, "project", null, { ...allGrants, tasks: false }))
    .toBe("This device needs task permission.");
});
