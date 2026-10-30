// SPDX-License-Identifier: Apache-2.0
import { render, screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { Button } from "../../design/Button";
import { COMMON_KEYS } from "./keys";
import { keyCap } from "./KeyCap";

it("draws every arrow key from one icon set with a spoken name", () => {
  render(<div>{COMMON_KEYS.map((k) => <Button key={k} size="sm" {...keyCap(k)} />)}</div>);
  for (const name of ["Left arrow", "Up arrow", "Down arrow", "Right arrow"]) {
    const key = screen.getByRole("button", { name });
    expect(key.querySelector("svg")).toBeTruthy();
    expect(key.textContent).toBe("");
  }
  expect(screen.getByRole("button", { name: "Enter" }).textContent).toBe("Enter");
});
