// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { parseUnifiedDiff, wordChanges, type DiffRow } from "./unifiedDiff";

describe("parseUnifiedDiff", () => {
  it("types file headers, counted hunks and the missing newline note", () => {
    expect(parseUnifiedDiff([
      "diff --git a/notes.md b/notes.md",
      "index 1111111..2222222 100644",
      "--- a/notes.md",
      "+++ b/notes.md",
      "@@ -1,3 +1,3 @@ # Notes",
      " intro",
      "---- a rule that was removed",
      "+kept",
      "\\ No newline at end of file",
      "",
    ].join("\n"))).toEqual([
      { kind: "file", text: "diff --git a/notes.md b/notes.md" },
      { kind: "file", text: "index 1111111..2222222 100644" },
      { kind: "file", text: "--- a/notes.md" },
      { kind: "file", text: "+++ b/notes.md" },
      { kind: "hunk", text: "@@ -1,3 +1,3 @@ # Notes" },
      { kind: "same", text: "intro" },
      { kind: "remove", text: "--- a rule that was removed" },
      { kind: "add", text: "kept" },
      { kind: "note", text: "\\ No newline at end of file" },
    ]);
  });

  it("reads a single-line hunk header and CRLF text", () =>
    expect(parseUnifiedDiff("@@ -1 +1 @@\r\n-old\r\n+new")).toEqual([
      { kind: "hunk", text: "@@ -1 +1 @@" },
      { kind: "remove", text: "old" },
      { kind: "add", text: "new" },
    ]));

  it("keeps written content as text and signs only the reported edits", () =>
    expect(parseUnifiedDiff([
      "Native edit excerpt:",
      "-- first",
      "+- second",
      "",
      "Next tool edit:",
      "Written content (previous content not reported):",
      "- a list item",
      "+ not an addition",
    ].join("\n"))).toEqual([
      { kind: "file", text: "Native edit excerpt:" },
      { kind: "remove", text: "- first" },
      { kind: "add", text: "- second" },
      { kind: "file", text: "Next tool edit:" },
      { kind: "file", text: "Written content (previous content not reported):" },
      { kind: "text", text: "- a list item" },
      { kind: "text", text: "+ not an addition" },
    ]));
});

describe("wordChanges", () => {
  const rows = (...lines: [DiffRow["kind"], string][]) => lines.map(([kind, text]) => ({ kind, text }));

  it("marks only the changed words of a replaced line", () => {
    const changes = wordChanges(rows(["same", "a"], ["remove", "Keep the line endings."], ["add", "Keep the CRLF line endings."]));
    expect(changes.get(1)).toEqual([{ text: "Keep the line endings.", changed: false }]);
    expect(changes.get(2)).toEqual([
      { text: "Keep the ", changed: false },
      { text: "CRLF ", changed: true },
      { text: "line endings.", changed: false },
    ]);
    expect(changes.has(0)).toBe(false);
  });

  it("joins neighbouring changed words across the space between them", () =>
    expect(wordChanges(rows(
      ["remove", "Set the margin to 12px on the checkout page"],
      ["add", "Set the padding of 24px on the checkout page"],
    )).get(1)).toEqual([
      { text: "Set the ", changed: false },
      { text: "padding of 24px", changed: true },
      { text: " on the checkout page", changed: false },
    ]));

  it("pairs each removed line in order with the first similar addition", () => {
    const changes = wordChanges(rows(["remove", "one two"], ["remove", "three four"], ["add", "one 2"]));
    expect([...changes.keys()]).toEqual([0, 2]);
    const edited = wordChanges(rows(["remove", "an old rule"], ["remove", "Keep the line endings."], ["add", "Keep the CRLF line endings."]));
    expect([...edited.keys()]).toEqual([1, 2]);
  });

  it("leaves rewritten, identical and overlong lines to whole-line colour", () => {
    expect(wordChanges(rows(["remove", "alpha beta"], ["add", "gamma delta"])).size).toBe(0);
    expect(wordChanges(rows(["remove", "same"], ["add", "same"])).size).toBe(0);
    const long = "word ".repeat(100);
    expect(wordChanges(rows(["remove", long], ["add", `${long}more`])).size).toBe(0);
  });

  it("stops comparing words once its budget is spent", () => {
    const before = Array.from({ length: 80 }, (_, i) => `w${i}`).join(" ");
    const many = Array.from({ length: 200 }, () => rows(["remove", before], ["add", before.replace("w0", "v0")])).flat();
    const changes = wordChanges(many);
    expect(changes.size).toBeGreaterThan(0);
    expect(changes.size).toBeLessThan(many.length);
  });
});
