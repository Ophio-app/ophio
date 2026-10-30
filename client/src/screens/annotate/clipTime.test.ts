// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { clipTime } from "./derivatives";

it("rounds clip times across a minute boundary instead of showing 60 seconds", () => {
  // e.g. an Android cut whose container reports 59.9975 s.
  expect(clipTime(59_997.5)).toBe("1:00.00");
  expect(clipTime(119_996)).toBe("2:00.00");
  expect(clipTime(3_500)).toBe("0:03.50");
  expect(clipTime(61_250)).toBe("1:01.25");
});
