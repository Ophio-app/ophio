// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { agentSent, nextUp, startOrQueue } from "./common";

const opus = { harness: "claude_code" as const, model: "opus", effort: null, launchPolicy: "bypass_permissions" as const };

it("sends launch flags only to a computer that applies them", () => {
  expect(agentSent(opus, true)).toEqual(opus);
  expect(agentSent(opus, false)).toEqual({ ...opus, launchPolicy: null });
});

it("starts now, or waits in Next up while the project is busy", () => {
  expect(nextUp()).toEqual({ mode: "queue", position: null, authorization: "auto_start" });
  expect(startOrQueue(true)).toEqual(nextUp());
  expect(startOrQueue(false)).toEqual({ mode: "start_now" });
});
