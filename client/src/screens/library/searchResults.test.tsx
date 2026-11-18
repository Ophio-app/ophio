// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { SearchHit } from "../../protocol/types";
import { groupSearchHits, SearchFileRow } from "./Files";

afterEach(cleanup);

const hits: SearchHit[] = [
  { root_id: "a", path: "docs/notes.md", snippet: "", },
  ...Array.from({ length: 5 }, (_, n) => ({ root_id: "a", path: "docs/notes.md", line: n + 1, snippet: `found ${n + 1}` })),
  { root_id: "b", path: "docs/notes.md", snippet: "" },
];

it("groups name and line matches by both root and path without losing search order", () => {
  const groups = [...groupSearchHits(hits).values()];
  expect(groups).toHaveLength(2);
  expect(groups[0]).toEqual(hits.slice(0, 6));
  expect(groups[1]).toEqual([hits[6]]);
});

it("shows three matching lines, expands the rest separately from opening the file, and collapses", () => {
  const open = vi.fn();
  render(<SearchFileRow hits={hits.slice(0, 6)} rootLabel="Project" open={open} />);
  expect(screen.getByText("5 matching lines")).toBeDefined();
  expect(screen.queryByText(/found 4/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Show 2 more matching lines" }));
  expect(open).not.toHaveBeenCalled();
  expect(screen.getByText(/found 5/)).toBeDefined();
  fireEvent.click(screen.getByRole("button", { name: "Show fewer lines" }));
  expect(screen.queryByText(/found 5/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /notes.md/ }));
  expect(open).toHaveBeenCalledTimes(1);
});

it("keeps a file-name-only match as one openable file", () => {
  const open = vi.fn();
  render(<SearchFileRow hits={[hits[6]!]} rootLabel="Other project" open={open} />);
  expect(screen.getByText("Name matches")).toBeDefined();
  expect(screen.queryByRole("button", { name: /more matching lines/ })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /notes.md/ }));
  expect(open).toHaveBeenCalledOnce();
});
