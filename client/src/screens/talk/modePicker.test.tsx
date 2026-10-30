// SPDX-License-Identifier: Apache-2.0
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { harnesses } from "../../dev/sim/scenario";
import type { HarnessInfo } from "../../protocol/types";
import { ModePicker } from "./ModePicker";

afterEach(cleanup);

const off = (reason: string, plan = reason): HarnessInfo => ({
  ...harnesses.find((h) => h.kind === "codex")!,
  modes: [
    { mode: "ask", availability: { state: "unsupported", reason }, enforcement: "" },
    { mode: "plan", availability: { state: "unsupported", reason: plan }, enforcement: "" },
    { mode: "work", availability: { state: "available" }, enforcement: "" },
  ],
});

it("says once why Ask and Plan are off when it's one reason, and what each does", () => {
  render(<ModePicker harness={off("Update Codex to use these modes.")} value="work" onChange={() => {}} />);
  expect(screen.getAllByText("Update Codex to use these modes.")).toHaveLength(1);
  expect(screen.getByText("Answers from the project. Changes nothing.")).toBeTruthy();
  const ask = screen.getByRole("radio", { name: /Ask/ });
  expect(ask.hasAttribute("disabled")).toBe(true);
  expect(ask.getAttribute("aria-describedby")?.split(" ")).toHaveLength(2);
});

it("keeps each reason on its own mode when they differ", () => {
  render(<ModePicker harness={off("No Ask here.", "No Plan here.")} value="work" onChange={() => {}} />);
  expect(screen.getByText("No Ask here.")).toBeTruthy();
  expect(screen.getByText("No Plan here.")).toBeTruthy();
  expect(document.querySelector(".modes__why")).toBeNull();
});

it("offers only the mode a task runs in, and says why once", () => {
  render(<ModePicker harness={harnesses.find((h) => h.kind === "claude_code")} value="plan" onChange={() => {}} only={{ mode: "plan", why: "Sorting issues only reads, so it runs in Plan." }} />);
  expect(screen.getByRole("radio", { name: /Work/ }).hasAttribute("disabled")).toBe(true);
  expect(screen.getByRole("radio", { name: /Ask/ }).hasAttribute("disabled")).toBe(true);
  expect(screen.getByRole("radio", { name: /Plan/ }).hasAttribute("disabled")).toBe(false);
  expect(screen.getAllByText("Sorting issues only reads, so it runs in Plan.")).toHaveLength(1);
});
