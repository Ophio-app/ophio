// SPDX-License-Identifier: Apache-2.0
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { TextArea, TextField } from "./Field";
afterEach(cleanup);

// A phone screen reader stops on a label's own text as well as on the field it
// names; a label nobody can see is then a stop with nothing on screen.
it("names a field whose label is hidden without leaving a separate, invisible label", () => {
  render(
    <>
      <TextArea label="What should it do next?" hideLabel value="" onChange={() => {}} />
      <TextField label="Search" hideLabel value="" onChange={() => {}} />
    </>,
  );
  expect(screen.getByRole("textbox", { name: "What should it do next?" }).tagName).toBe("TEXTAREA");
  expect(screen.getByRole("textbox", { name: "Search" }).tagName).toBe("INPUT");
  expect(document.querySelector("label")).toBeNull();
});

it("keeps a shown label as a label", () => {
  render(<TextField label="Name" value="" onChange={() => {}} />);
  expect(screen.getByText("Name").tagName).toBe("LABEL");
  expect(screen.getByRole("textbox", { name: "Name" })).toBeTruthy();
});
