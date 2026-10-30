// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import {
  breadcrumbs,
  category,
  forSave,
  relativeDir,
  validName,
} from "./model";
import { mergeMarkersLeft, lineDiff, mergeParts, resolveMerge } from "./diff";

describe("file identity and paths", () => {
  it("collapses only middle breadcrumbs, retaining exact paths", () => {
    expect(breadcrumbs("Project", "docs/guides/releases/next")).toEqual({
      visible: [
        { label: "Project", path: "" },
        { label: "next", path: "docs/guides/releases/next" },
      ],
      collapsed: [
        { label: "docs", path: "docs" },
        { label: "guides", path: "docs/guides" },
        { label: "releases", path: "docs/guides/releases" },
      ],
    });
    expect(breadcrumbs("Project", "docs").collapsed).toEqual([]);
  });
  it("does not classify AGENTS.md as an agent definition", () => {
    expect(category("AGENTS.md", "project")).toBe("Instructions");
    expect(category(".claude/agents/reviewer.md", "project")).toBe(
      "Agent definitions",
    );
    expect(category("release/SKILL.md")).toBe("Skills");
  });
  it.each([
    "/etc",
    "../docs",
    "docs/../x",
    "docs/./x",
    "C:/temp",
    "C:\\temp",
    "\\server",
    "docs//x",
    "docs/",
    "notes\u0000",
    "docs/NUL.txt",
  ])("rejects ambiguous or escaping directories: %s", (path) =>
    expect(relativeDir(path)).not.toBeNull(),
  );
  it.each(["", "docs/notes", "déjà vu/研究", ".config/notes"])(
    "accepts relative directories: %s",
    (path) => expect(relativeDir(path)).toBeNull(),
  );
  it("requires a single portable file name", () => {
    expect(validName("plan (copy).md")).toBeNull();
    expect(validName("../a.md")).not.toBeNull();
    expect(validName("a.md ")).not.toBeNull();
  });
  // The companion refuses these as Library paths; the phone says so before
  // sending rather than relaying a generic refusal.
  it.each(["a\u007f.md", "a\u0085.md", "CONIN$", "conout$.txt", "COM\u00b9.md", "lpt\u00b3", "con .txt", "LPT1  .md"])(
    "refuses a name the companion refuses: %j",
    (name) => expect(validName(name)).not.toBeNull(),
  );
  // Sent or saved, such a name could read as another: an .exe as a .pdf.
  it.each(["photo\u202egnp.exe", "report\u2067.pdf", "a\u200fb.md", "\u061cnotes.md"])(
    "refuses a name with text-direction characters: %j",
    (name) =>
      expect(validName(name)).toBe(
        "Remove the hidden text-direction characters from this name.",
      ),
  );
});
describe("line comparison and merge", () => {
  it("shows a one-line replacement without newline noise", () => {
    expect(lineDiff("# Notes\r\nold\r\n", "# Notes\nnew\n")).toEqual([
      { kind: "same", text: "# Notes" },
      { kind: "remove", text: "old" },
      { kind: "add", text: "new" },
      { kind: "same", text: "" },
    ]);
  });
  // The editor hands back every line break as "\n" (see Editor.test.tsx).
  // The companion reads and saves a note's bytes as they are, so a note
  // with CRLF mixed with LF, or a lone CR, gets its own breaks back.
  it("gives an edited note its own line breaks back", () => {
    const base = "# Plan\r\nfirst\nsecond\r\nprogress 50%\rprogress 100%\r\n";
    const editor = (text: string) => text.split(/\r\n|\r|\n/).join("\n");
    expect(forSave(editor(base), "mixed", base)).toBe(base);
    expect(forSave(editor(base).replace("second", "2nd"), "mixed", base)).toBe(base.replace("second", "2nd"));
    expect(forSave(`${editor(base)}done`, "mixed", base)).toBe(`${base}done`);
    // A new line takes the break before it.
    expect(forSave(editor(base).replace("first\n", "first\nadded\n"), "mixed", base)).toBe(
      base.replace("first\n", "first\nadded\n"),
    );
    const lone = "a\rb\nc\n";
    expect(forSave(editor(lone), "lf", lone)).toBe(lone);
    expect(forSave(editor(lone).replace("c", "d"), "lf", lone)).toBe("a\rb\nd\n");
  });
  it("keeps the breaks of every line an edit leaves alone, on generated notes", () => {
    let state = 7;
    const random = (below: number) => {
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      return (state >>> 0) % below;
    };
    const kinds = ["\n", "\r\n", "\r"];
    let serial = 0;
    for (let run = 0; run < 500; run++) {
      const lines = Array.from({ length: 1 + random(8) }, () => `line ${serial++}`);
      const breaks = lines.slice(1).map(() => kinds[random(3)]!);
      const join = (from: number, to: number) =>
        lines.slice(from, to).map((l, i) => (i ? breaks[from + i - 1] : "") + l).join("");
      const base = join(0, lines.length);
      const from = random(lines.length + 1),
        to = from + random(lines.length - from + 1);
      const added = Array.from({ length: random(3) }, () => `new ${serial++}`);
      const edited = [...lines.slice(0, from), ...added, ...lines.slice(to)];
      const saved = forSave(edited.join("\n"), "mixed", base);
      expect(saved.split(/\r\n|\r|\n/), base).toEqual(edited.length ? edited : [""]);
      expect(saved.startsWith(join(0, from)), base).toBe(true);
      expect(saved.endsWith(join(to, lines.length)), base).toBe(true);
    }
  });
  it("preserves CRLF when saving and does not double carriage returns", () => {
    expect(forSave("a\nb\r\n", "cr_lf")).toBe("a\r\nb\r\n");
    expect(forSave("a\r\nb\n", "mixed")).toBe("a\r\nb\n");
  });
  it("handles empty files, repeated lines and trailing newline removal", () => {
    for (const [a, b] of [
      ["", "hello"],
      ["a\na\nb", "a\nb\na"],
      ["a\n", "a"],
    ]) {
      const diff = lineDiff(a!, b!);
      expect(
        diff
          .filter((x) => x.kind !== "add")
          .map((x) => x.text)
          .join("\n"),
      ).toBe(a);
      expect(
        diff
          .filter((x) => x.kind !== "remove")
          .map((x) => x.text)
          .join("\n"),
      ).toBe(b);
    }
  });
  it("bounds memory for unrelated large documents without losing lines", () => {
    const a = "a\n".repeat(1200),
      b = "b\n".repeat(1200),
      diff = lineDiff(a, b);
    expect(
      diff
        .filter((x) => x.kind !== "remove")
        .map((x) => x.text)
        .join("\n"),
    ).toBe(b);
  });
  const mergeDraft = (base: string, mine: string, theirs: string) => {
    const parts = mergeParts(base, mine, theirs);
    return { text: resolveMerge(parts, []), conflicts: parts.filter((p) => p.kind === "overlap").length };
  };
  // A note under the 512 KiB read limit can have more lines than a call
  // takes arguments.
  it.each([
    ["a long unchanged end", `a\n${"\n".repeat(150_000)}`, `b\n${"\n".repeat(150_000)}`],
    ["a long replacement", "a\n".repeat(75_000), "b\n".repeat(75_000)],
  ])("compares notes with %s without losing lines", (_, a, b) => {
    const diff = lineDiff(a, b);
    expect(diff.filter((x) => x.kind !== "add").map((x) => x.text).join("\n")).toBe(a);
    expect(diff.filter((x) => x.kind !== "remove").map((x) => x.text).join("\n")).toBe(b);
  });
  it("merges notes with more lines than a call takes arguments", () => {
    const blank = "\n".repeat(150_000);
    expect(mergeDraft(`a\n${blank}z`, `A\n${blank}z`, `a\n${blank}Z`)).toEqual({
      text: `A\n${blank}Z`,
      conflicts: 0,
    });
    const base = "x\n".repeat(75_000);
    expect(mergeDraft(base, "p\n".repeat(75_000), "c\n".repeat(75_000))).toEqual({
      text: `<<<<<<< Draft on this device\n${"p\n".repeat(75_000)}=======\n${"c\n".repeat(75_000)}>>>>>>> Now on the computer\n`,
      conflicts: 1,
    });
    // One side edits the first line, the other deletes the whole note.
    expect(mergeDraft(base, `P\n${"x\n".repeat(74_999)}`, "")).toEqual({
      text: `<<<<<<< Draft on this device\nP\n${"x\n".repeat(74_999)}=======\n>>>>>>> Now on the computer\n`,
      conflicts: 1,
    });
  });
  it("merges separate edits and sets each overlap apart", () => {
    expect(mergeParts("a\nb\nc\nd", "A\nb\nc\nd", "a\nb\nc\nD")).toEqual([
      { kind: "same", lines: ["A", "b", "c", "D"] },
    ]);
    const parts = mergeParts("a\nb\nc\nd", "a\nmine\nc\nmine too", "a\ntheirs\nc\nd\ntheirs too");
    expect(parts).toEqual([
      { kind: "same", lines: ["a"] },
      { kind: "overlap", mine: ["mine"], theirs: ["theirs"] },
      { kind: "same", lines: ["c"] },
      { kind: "overlap", mine: ["mine too"], theirs: ["d", "theirs too"] },
    ]);
    expect(resolveMerge(parts, ["theirs", "both"])).toBe("a\ntheirs\nc\nmine too\nd\ntheirs too");
    expect(resolveMerge(parts, ["mine", "mine"])).toBe("a\nmine\nc\nmine too");
  });
  it("marks only the overlaps left to edit by hand", () =>
    expect(resolveMerge(mergeParts("a\nb\nc", "A\nb\nC", "a2\nb\nc2"), ["mine", "hand"])).toBe(
      "A\nb\n<<<<<<< Draft on this device\nC\n=======\nc2\n>>>>>>> Now on the computer",
    ));
  it("keeps lines both sides share at the edges of an overlap outside it", () =>
    expect(mergeParts("x\ny", "x\nshared\nmine\nend", "x\nshared\ntheirs\nend")).toEqual([
      { kind: "same", lines: ["x", "shared"] },
      { kind: "overlap", mine: ["mine"], theirs: ["theirs"] },
      { kind: "same", lines: ["end"] },
    ]));
  it("does not set identical changes apart", () =>
    expect(mergeParts("a", "b", "b")).toEqual([{ kind: "same", lines: ["b"] }]));
});

  it("finds only the marker lines a merge added", () => {
    const merged = resolveMerge(mergeParts("Title\r\n=======\r\na", "Title\r\n=======\r\nphone", "Title\r\n=======\r\ncomputer"), []);
    expect(mergeMarkersLeft(merged, "Title\r\n=======\r\ncomputer", true)).toBe(true);
    expect(mergeMarkersLeft(merged.replace(/^[<>].*\n?/gm, ""), "Title\n=======\ncomputer", true)).toBe(true);
    expect(mergeMarkersLeft("Title\n=======\nphone", "Title\r\n=======\r\ncomputer", true)).toBe(false);
    // A heading underline is never a marker in a draft the merge did not mark.
    expect(mergeMarkersLeft("License\n=======\nAll\n=======\n", "", false)).toBe(false);
    expect(mergeMarkersLeft("Overview\n========\n<<<<<<<< x\n>>>>>>>>", "", true)).toBe(false);
    // The merge's own labelled lines block any draft, unless the computer's file already has them.
    expect(mergeMarkersLeft("a\n>>>>>>> Now on the computer", "", false)).toBe(true);
    expect(mergeMarkersLeft("<<<<<<< Draft on this device\r\nb", "<<<<<<< Draft on this device", false)).toBe(false);
  });
