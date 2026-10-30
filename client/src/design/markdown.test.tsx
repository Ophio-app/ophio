// SPDX-License-Identifier: Apache-2.0
// A code block keeps where the person scrolled it while the screen around it
// changes.
import { cleanup, render } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { Markdown } from "./Markdown";

afterEach(cleanup);

it("keeps the same code block, and its scroll, when the screen around it changes", () => {
  const text = "Intro\n\n```\na very long line of code that runs off the side of a phone screen\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
  const first = vi.fn();
  const { container, rerender } = render(<Markdown document text={text} onLink={first} />);
  const pre = container.querySelector("pre")!;
  const table = container.querySelector(".md__table")!;
  pre.scrollLeft = 120;
  // A parent re-rendering hands over a new link handler each time.
  const second = vi.fn();
  rerender(<Markdown document text={text + "\n[docs](https://ophio.app)"} onLink={second} />);
  expect(container.querySelector("pre")).toBe(pre);
  expect(container.querySelector(".md__table")).toBe(table);
  expect(pre.scrollLeft).toBe(120);
  container.querySelector("a")!.click();
  expect(second).toHaveBeenCalledWith("https://ophio.app");
  expect(first).not.toHaveBeenCalled();
});

it("shows a file's leading properties as File details, in plain words", () => {
  const { getByRole } = render(<Markdown document text={"---\nname: reviewer\ndescription: Reviews changes\n---\n\nBody"} />);
  const details = getByRole("region", { name: "File details" });
  expect(details.textContent).toMatch(/^File details/);
  expect(details.textContent).toMatch(/reviewer/);
});

it("hands over a link to a line in a file, and still drops a protocol it doesn't know", () => {
  const onLink = vi.fn();
  const { getByText } = render(<Markdown text={"[readme](README.md:3) [range](notes.md:3-9) [report](docs/report.md:12) [script](javascript:alert(1)) [call](tel:5551234)"} onLink={onLink} />);
  for (const name of ["readme", "range", "report", "script", "call"]) getByText(name).click();
  expect(onLink.mock.calls).toEqual([["README.md:3"], ["notes.md:3-9"], ["docs/report.md:12"]]);
});

it.each([
  "C:/Users/sam/site/docs/report.md",
  String.raw`C:\Users\sam\site\docs\report.md`,
  "d:/site/docs/report.md:12:5",
  String.raw`d:\site\docs\report.md:12-20`,
  String.raw`C:\Users\sam\site\docs\.config.md`,
  String.raw`C:\Users\sam\site\docs\.config.md:12`,
  String.raw`C:\Users\sam\site\docs\_report.md`,
  String.raw`C:\Users\sam\site\docs\_report.md:12:5`,
  String.raw`C:\_work\.config.md`,
])("hands over a Windows file link: %s", (href) => {
  const onLink = vi.fn();
  const { getByRole } = render(<Markdown text={`[report](${href})`} onLink={onLink} />);
  getByRole("link", { name: "report" }).click();
  expect(onLink).toHaveBeenCalledExactlyOnceWith(href);
});

it.each([
  [String.raw`[report](<C:\Users\sam\My Documents\.config.md:12> "File title")`, String.raw`C:\Users\sam\My%20Documents\.config.md:12`],
  [String.raw`[report][file]` + "\n\n" + String.raw`[file]: C:\Users\sam\site\docs\_report.md:12 "File title"`, String.raw`C:\Users\sam\site\docs\_report.md:12`],
  [String.raw`[report][file]` + "\n\n" + String.raw`[file]: <C:\Users\sam\My Documents\.config.md:12>`, String.raw`C:\Users\sam\My%20Documents\.config.md:12`],
  [String.raw`[report](C:\\Users\\sam\\site\\docs\\.config.md)`, String.raw`C:\Users\sam\site\docs\.config.md`],
  [String.raw`[report](C:\Users\sam\site\docs\_a&amp;b.md)`, String.raw`C:\Users\sam\site\docs\_a&b.md`],
  [String.raw`[report](C:/Users/sam/site/docs/report\(v1\).md)`, "C:/Users/sam/site/docs/report(v1).md"],
  [String.raw`[report](/home/sam/docs/\_report.md)`, "/home/sam/docs/_report.md"],
  [String.raw`[report](https://example.com/\_report?a=1&amp;b=2)`, "https://example.com/_report?a=1&b=2"],
])("preserves link destination syntax: %s", (text, href) => {
  const onLink = vi.fn();
  const { getByRole } = render(<Markdown text={text} onLink={onLink} />);
  getByRole("link", { name: "report" }).click();
  expect(onLink).toHaveBeenCalledExactlyOnceWith(href);
});

it("keeps Markdown escapes in labels and Windows paths in code unchanged", () => {
  const literal = String.raw`[report](C:\Users\sam\site\docs\.config.md)`;
  const text = String.raw`[\[report\]](C:\Users\sam\site\docs\.config.md)` + `\n\n\`${literal}\`\n\n\`\`\`\n${literal}\n\`\`\``;
  const { getByRole, container } = render(<Markdown text={text} />);
  expect(getByRole("link", { name: "[report]" })).toBeTruthy();
  expect([...container.querySelectorAll("code")].map((node) => node.textContent?.trimEnd())).toEqual([literal, literal]);
});

it.each(["javascript:alert(1)", "data:text/html,hello", "vbscript:msgbox(1)", "custom:/report.md", "C:report.md"])("still drops an unsupported protocol: %s", (href) => {
  const onLink = vi.fn();
  const { getByText } = render(<Markdown text={`[report](${href})`} onLink={onLink} />);
  getByText("report").click();
  expect(onLink).not.toHaveBeenCalled();
});
