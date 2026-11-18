// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import type { CheckJob } from "../../protocol/generated/CheckJob";
import type { CheckOutcome } from "../../protocol/generated/CheckOutcome";
import type { OphioCheckResult } from "../../protocol/generated/OphioCheckResult";
import { OphioCheckResults } from "./OphioCheckResults";

afterEach(cleanup);

const result = (name: string, outcome: CheckOutcome): OphioCheckResult => ({
  check_id: name, name, outcome, duration_secs: 2.4, exit_code: null,
  output: `${name} output`, inputs_changed: [],
});
const job = (results: OphioCheckResult[], overrides: Partial<CheckJob> = {}): CheckJob => ({
  id: "job", project_id: "project", task_id: null, run_id: null,
  purpose: { kind: "after_run" }, checks_version: 1, checks: [], state: "finished",
  process: null, created_at: 0, started_at: 0, finished_at: 1,
  sandbox: { kind: "contained" }, results, message: null, ...overrides,
});

it("groups compact result rows in one surface and expands output by tapping the row", () => {
  const { container } = render(<OphioCheckResults jobs={[job([
    result("Unit tests", { kind: "passed" }), result("Build", { kind: "failed" }),
  ])]} />);
  expect(container.querySelectorAll(".receipt-checks")).toHaveLength(1);
  expect(container.querySelectorAll(".receipt-checks__result")).toHaveLength(2);
  expect(screen.getByText("Ophio ran 2 checks: 1 passed, 1 failed.")).toBeTruthy();
  const row = screen.getByText("Unit tests").closest("summary")!;
  const details = row.parentElement as HTMLDetailsElement;
  expect(details.open).toBe(false);
  expect(row.textContent).toBe("Unit testsPassed");
  expect(row.querySelector(".lucide-check")).toBeTruthy();
  fireEvent.click(row);
  expect(details.open).toBe(true);
  expect(screen.getByText("Unit tests output").closest("details")).toBe(details);
  expect(screen.getByText("Passed · 2 seconds")).toBeTruthy();
  fireEvent.click(row);
  expect(details.open).toBe(false);
});

it("distinguishes warning outcomes from passed and intentionally unrun checks", () => {
  const outcomes: CheckOutcome[] = [
    { kind: "passed" }, { kind: "failed" }, { kind: "timed_out" },
    { kind: "could_not_run", reason: "The tool is missing." }, { kind: "stopped" },
    { kind: "off" }, { kind: "skipped_paths" },
  ];
  render(<OphioCheckResults jobs={[job(outcomes.map(outcome => result(outcome.kind, outcome)))]} />);
  for (const outcome of outcomes) {
    const row = screen.getByText(outcome.kind).closest("summary")!;
    const warning = ["failed", "timed_out", "could_not_run", "stopped"].includes(outcome.kind);
    expect(row.parentElement?.classList.contains("receipt-checks__result--warning")).toBe(warning);
    expect(Boolean(row.querySelector(".lucide-circle-alert"))).toBe(outcome.kind !== "passed");
  }
  fireEvent.click(screen.getByText("could_not_run"));
  expect(screen.getByText("The tool is missing.").closest("details")?.open).toBe(true);
  expect(screen.getByText("Not needed for these changes")).toBeTruthy();
  expect(screen.getByText("Off")).toBeTruthy();
});

it("preserves uncertainty, sandbox and changed-file notices", () => {
  const changed = { ...result("Unit tests", { kind: "passed" }), inputs_changed: ["package.json"] };
  const { rerender } = render(<OphioCheckResults jobs={[job([changed], {
    state: "uncertain", sandbox: { kind: "unsandboxed", reason: "Sandbox unavailable." },
  })]} />);
  expect(screen.getByText("Check results are uncertain")).toBeTruthy();
  expect(screen.getByText(/These checks ran without a sandbox/)).toBeTruthy();
  expect(screen.getByText(/This task changed files a check depends on/)).toBeTruthy();
  expect(screen.getByText("package.json")).toBeTruthy();
  rerender(<OphioCheckResults jobs={[job([], {
    sandbox: { kind: "contained_without_network_limit", reason: "Network limit unavailable." },
  })]} />);
  expect(screen.getByText(/could not limit these checks’ internet access/)).toBeTruthy();
  expect(screen.getByText("Ophio has not reported any check results yet.")).toBeTruthy();
});

it("keeps running progress and the empty-output explanation", () => {
  const { container, rerender } = render(<OphioCheckResults jobs={[job([
    { ...result("Unit tests", { kind: "passed" }), output: "" },
  ], { state: "cancelling" })]} />);
  expect(screen.getByRole("status").textContent).toContain("Stopping checks…");
  expect(screen.getByText("No output was recorded.")).toBeTruthy();
  rerender(<OphioCheckResults jobs={[]} />);
  expect(container.textContent).toBe("");
  expect(screen.queryByText("Ophio’s results")).toBeNull();
});
