// SPDX-License-Identifier: Apache-2.0
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { ToolInput, toolInput } from "./toolInput";
afterEach(cleanup);
it.each([
  ["Bash", { command: "pwd" }, "pwd"],
  ["Read", { file_path: "src/main.rs", offset: 10 }, "src/main.rs"],
  ["Edit", { file_path: "src/main.rs", old_string: "a", new_string: "b" }, "src/main.rs"],
  ["Write", { file_path: "README.md", content: "hello" }, "README.md"],
  ["Grep", { pattern: "fn main", path: "src" }, "fn main · src"],
  ["Glob", { pattern: "**/*.rs", path: "." }, "**/*.rs · ."],
  ["Task", { description: "Check the parser", prompt: "longer" }, "Check the parser"],
  ["Agent", { description: "Review tests" }, "Review tests"],
  ["functions.exec_command", { cmd: "cargo test" }, "cargo test"],
  ["shell", { command: ["ls", "src"] }, "ls src"],
  ["read_file", { path: "README.md" }, "README.md"],
  ["fileChange", { changes: [{ path: "a.rs" }, { path: "b.rs" }] }, "a.rs\nb.rs"],
  ["spawn_agent", { message: "Review parser tests" }, "Review parser tests"],
  ["ExitPlanMode", { plan: "1. Read\n2. Implement" }, "1. Read\n2. Implement"],
])("renders %s input and retains its raw details", (tool, input, expected) => {
  const detail = JSON.stringify(input);
  expect(toolInput(tool, detail)?.text).toBe(expected);
  const { container } = render(<ToolInput tool={tool} detail={detail} />);
  if (tool !== "ExitPlanMode") {
    expect(screen.getByText("Details")).toBeTruthy();
    expect(container.querySelector("details")?.open).toBe(false);
    expect(container.querySelector("details pre")?.textContent).toBe(detail);
  }
});
it("keeps already-readable Codex commands and folds unknown inputs", () => {
  expect(toolInput("commandExecution", "pwd")).toEqual({ text: "pwd", kind: "code" });
  expect(toolInput("unknown", '{"token":"value"}')).toBeNull();
  const { container } = render(<ToolInput tool="unknown" detail='{"token":"value"}' />);
  expect(container.querySelector("details")?.open).toBe(false);
});
