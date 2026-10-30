// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { branchCommand, branchNameInput, branchNameProblem, branchStatus } from "./taskBranch";

it("accepts the branch names Git does and says why it refuses the rest", () => {
  // The same cases as the companion's own check.
  for (const ok of ["main", "feature/search", "fix-1.2", "Ünïcode", "a@b", "x/y/z"]) expect(branchNameProblem(ok), ok).toBeNull();
  for (const bad of ["", "@", "HEAD", "-rf", "--force", "a..b", "a@{1}", "end.", "end/", "/start", "a//b", ".hidden", "a/.b", "x.lock", "a/b.lock/c", "sp ace", "tab\t", "a~1", "a^", "a:b", "a?", "a*", "a[", "a\\b", "nul\0", "b".repeat(256)]) {
    expect(branchNameProblem(bad), JSON.stringify(bad)).not.toBeNull();
  }
  expect(branchNameProblem("é".repeat(128))).toBe("Use a shorter name.");
  expect(branchNameProblem("a~1")).toBe("A branch name can’t contain “~”.");
  expect(branchNameProblem("tab\t")).toBe("A branch name can’t contain control characters.");
  expect(branchNameInput("rate limits  v2")).toBe("rate-limits-v2");
});

it("leaves new off the wire for an existing branch", () => {
  expect(branchCommand({ name: "main", new: false })).toEqual({ name: "main" });
  expect(branchCommand({ name: "spike", new: true })).toEqual({ name: "spike", new: true });
});

it("describes a new branch from what is checked out", () => {
  expect(branchStatus({ name: "spike", new: true }, "main", 0, true)).toBe("New branch from main. The agent makes it before changing any file.");
  expect(branchStatus({ name: "spike", new: true }, null, 1, true)).toBe("New branch from the checked-out commit, bringing 1 uncommitted change. The agent makes it before changing any file.");
  expect(branchStatus({ name: "spike", new: true }, null, 0, false)).toBe("New branch. The agent makes it before changing any file.");
  expect(branchStatus({ name: "main", new: true }, "main", 0, true, true)).toBe("main already exists. Choose it from the list, or the agent will ask you before using it.");
});
