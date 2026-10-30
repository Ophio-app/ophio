// SPDX-License-Identifier: Apache-2.0
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { RequestedChecks } from "./RequestedChecks";
afterEach(cleanup);
it("presents prose completion checks as requested outcomes, without claiming execution", () => {
  const { container } = render(<RequestedChecks requests={["Report whether the checklist covers 200% text"]} checks={[]} />);
  expect(container.querySelector("code")).toBeNull();
  expect(screen.getByText("Requested outcome")).toBeTruthy();
  expect(container.textContent).not.toContain("exact command");
});
it("separates recorded command results from an unconfirmed request", () => {
  const { container } = render(<RequestedChecks requests={["cargo test", "Check portrait"]} checks={[{ name: "Unit tests", command: "cargo test", status: "passed", provenance: "observed", detail: null }]} />);
  expect(container.querySelector("code")?.textContent).toBe("cargo test");
  expect(screen.getByText("Passed · Checked on the computer")).toBeTruthy();
  expect(screen.getByText(/No result explicitly matching/)).toBeTruthy();
});
