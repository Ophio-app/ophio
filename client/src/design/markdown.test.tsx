// SPDX-License-Identifier: Apache-2.0
// A code block keeps where the person scrolled it while the screen around it
// changes.
import { cleanup, render } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { Markdown } from "./Markdown";

afterEach(cleanup);

it("keeps ordered markers outside wrapped text with a digit-sized, text-scaled gutter", async () => {
  // jsdom cannot measure layout. Pin the shared CSS contract here; actual
  // narrow-screen rendering still needs a sandboxed browser/device check.
  const fs = await vi.importActual<{ readFileSync(path: string, encoding: "utf8"): string }>("node:fs");
  const css = fs.readFileSync("src/design/components.css", "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
  expect(css).toMatch(/\.md ol\s*\{\s*padding-inline-start:\s*max\(1\.3em, calc\(var\(--md-marker-width, 1ch\) \+ 0\.75em\)\);\s*\}/);
  expect(css).toMatch(/\.md ol > li::marker\s*\{\s*font-variant-numeric:\s*tabular-nums;/);
});

it.each([100, 1000])("reserves a marker gutter for a list starting at %i without widening its nested list", (start) => {
  const indent = " ".repeat(String(start).length + 2);
  const { container } = render(<Markdown text={`${start}. Parent item with wrapping text\n${indent}1. Nested ordered item\n${indent}2. Another nested item\n\n${start + 1}. Next parent`} />);
  const outer = container.querySelector("ol")!;
  const inner = outer.querySelector("ol")!;
  expect(outer.start).toBe(start);
  expect(outer.style.getPropertyValue("--md-marker-width")).toBe(`${String(start).length}ch`);
  expect(outer.querySelectorAll(":scope > li")).toHaveLength(2);
  expect(inner.parentElement?.tagName).toBe("LI");
  expect(inner.start).toBe(1);
  expect(inner.style.getPropertyValue("--md-marker-width")).toBe("1ch");
});

it("preserves mixed nested lists and paragraph alignment", () => {
  const { container } = render(<Markdown text={"1000. Parent paragraph\n\n      Another paragraph in the same item.\n\n      - Nested bullet\n        1. Nested ordered item\n\n1001. Next parent"} />);
  const outer = container.querySelector("ol")!;
  expect(outer.children).toHaveLength(2);
  expect(outer.querySelectorAll(":scope > li:first-child > p")).toHaveLength(2);
  expect(outer.querySelector("li > ul > li > ol")?.getAttribute("style")).toBe("--md-marker-width: 1ch;");
});

it.each([100, 1000])("grows the marker gutter as streamed items reach %i", (end) => {
  const text = Array.from({ length: end - 1 }, (_, i) => `${i + 1}. Item`).join("\n");
  const { container, rerender } = render(<Markdown text={text} />);
  expect(container.querySelector("ol")!.style.getPropertyValue("--md-marker-width")).toBe(`${String(end - 1).length}ch`);
  rerender(<Markdown text={`${text}\n${end}. Last item`} />);
  const list = container.querySelector("ol")!;
  expect(list.children).toHaveLength(end);
  expect(list.style.getPropertyValue("--md-marker-width")).toBe(`${String(end).length}ch`);
});

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
