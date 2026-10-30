// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { emptyHostState } from "../../state/projection";
import { previewDeliveryText } from "./previewDelivery";

it.each([
  ["host_accepted", "Handed to the computer for Fix layout · not yet confirmed by the agent"],
  ["queued_for_boundary", "Waiting to send to Fix layout · held until its turn ends"],
  ["native_delivered", "Sent with markup to Fix layout ·"],
  ["cancelled", "Not delivered to Fix layout · cancelled"],
  ["failed", "Not delivered to Fix layout · delivery failed"],
  ["unknown", "Delivery to Fix layout is unconfirmed"],
  [undefined, "Delivery to Fix layout is unconfirmed"],
] as const)("preview wording for %s", (state, expected) => {
  const text = previewDeliveryText({ taskId: "task", taskTitle: "Fix layout", at: 0, state }, emptyHostState);
  expect(text).toContain(expected);
  if (state === "host_accepted") expect(text).not.toContain("Waiting to send");
});
