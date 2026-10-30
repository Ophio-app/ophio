// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { Breadcrumbs } from "./shared";

let ui: UiStore;
beforeEach(() => {
  localStorage.clear();
  ui = new UiStore("crumbs");
  ui.selectTab("library");
});
afterEach(cleanup);

const crumbs = (path: string) =>
  render(
    <AppProvider app={{ ui } as App}>
      <Breadcrumbs rootId="r" label="Root" path={path} />
    </AppProvider>,
  );

it("a breadcrumb in File details goes up to the folder already open, and Back goes further up", () => {
  ui.push({ screen: "folder", rootId: "r", path: "" });
  ui.push({ screen: "folder", rootId: "r", path: "skills" });
  ui.push({ screen: "folder", rootId: "r", path: "skills/foo" });
  ui.push({ screen: "file", rootId: "r", path: "skills/foo/SKILL.md" });
  crumbs("skills/foo");
  fireEvent.click(screen.getByRole("button", { name: "foo" }));
  expect(ui.route()).toEqual({ screen: "folder", rootId: "r", path: "skills/foo" });
  expect(ui.get().stacks.library).toHaveLength(4);
  ui.back();
  expect(ui.route()).toEqual({ screen: "folder", rootId: "r", path: "skills" });
});

it("a breadcrumb for a folder that isn't open beneath opens it", () => {
  ui.push({ screen: "file", rootId: "r", path: "skills/foo/SKILL.md" });
  crumbs("skills/foo");
  fireEvent.click(screen.getByRole("button", { name: "Root" }));
  expect(ui.get().stacks.library).toEqual([
    { screen: "library" },
    { screen: "file", rootId: "r", path: "skills/foo/SKILL.md" },
    { screen: "folder", rootId: "r", path: "" },
  ]);
});
