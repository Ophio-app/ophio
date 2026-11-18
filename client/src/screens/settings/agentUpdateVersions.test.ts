// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { compareAgentVersions } from "./agentUpdateVersions";
it.each([
  ["2.1.9", "2.1.10", 1], ["0.20.0", "0.19.9", -1], ["2.1.0", "2.1.0", 0],
  ["v2.1.0", "2.1.1", 1], ["2.1.0-beta.9", "2.1.0-beta.10", 1],
  ["2.1.0-beta", "2.1.0", 1], ["2.1.0", "2.1.0-beta", -1],
  ["2.1.0-beta.1", "2.1.0-beta", -1], ["2.1.0-1", "2.1.0-beta", 1],
  ["2.1.0+build", "2.1.0", 0], ["unknown", "2.1.0", null],
] as const)("compares %s with %s without inventing availability", (installed, latest, result) => {
  expect(compareAgentVersions(installed, latest)).toBe(result);
});
