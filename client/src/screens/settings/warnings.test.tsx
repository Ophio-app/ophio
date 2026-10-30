// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { prefs } from "../../app/prefs";
import { warns } from "../../app/warnings";
import { Warnings } from "./Warnings";

const initial = prefs.get();
afterEach(() => { cleanup(); prefs.set(initial); });

it("turns every warning off with one box", () => {
  prefs.set({ ...initial, warningsOff: false, hiddenWarnings: [] });
  render(<Warnings />);
  const box = screen.getByRole("checkbox", { name: "Never show warnings" }) as HTMLInputElement;
  expect(box.checked).toBe(false);
  expect(document.getElementById(box.getAttribute("aria-describedby")!)?.textContent).toContain("still ask");
  fireEvent.click(box);
  expect(prefs.get().warningsOff).toBe(true);
  expect(warns("task.archive")).toBe(false);
  expect(JSON.parse(localStorage.getItem("prefs")!).warningsOff).toBe(true);
  fireEvent.click(box);
  expect(warns("task.archive")).toBe(true);
});

it("lists the warnings turned off one at a time, to show again", () => {
  prefs.set({ ...initial, warningsOff: false, hiddenWarnings: ["terminal.close", "gone.from.the.app", "issues.stop"] });
  render(<Warnings />);
  const list = within(screen.getByRole("list"));
  expect(list.getAllByRole("listitem").map((li) => li.querySelector("span")!.textContent)).toEqual(["Close a terminal", "Stop Run all"]);
  fireEvent.click(list.getAllByRole("button", { name: "Show again" })[0]!);
  expect(prefs.get().hiddenWarnings).toEqual(["gone.from.the.app", "issues.stop"]);
  expect(warns("terminal.close")).toBe(true);
  expect(screen.getAllByRole("listitem")).toHaveLength(1);

  // With every warning off, there is nothing to pick from.
  fireEvent.click(screen.getByRole("checkbox", { name: "Never show warnings" }));
  expect(screen.queryByRole("list")).toBeNull();
});
