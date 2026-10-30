// SPDX-License-Identifier: Apache-2.0
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { Remedy } from "../screens/control/ReadinessNote";

afterEach(cleanup);

// A phone screen reader stops before, on and after any element with a role of
// its own, so a command inside a sentence split it into three stops.
it("reads a sentence with a command in it as one piece", () => {
  const { container } = render(<Remedy text="Run ophio desktop enable on the computer to permit desktop capture" />);
  expect(container.querySelector("code")?.textContent).toBe("ophio desktop enable");
  expect(screen.queryAllByRole("code")).toHaveLength(0);
});

