// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { findings as fixtures, ids } from "../dev/sim/scenario";
import { fixAllPrompt, fixOnePrompt, fixOneTitle, reviewPrompt, reviewTitle } from "./findings";
import { isSetAside, parseFindings, parseVerdict } from "./findingsFile";

it("reads each finding's box, severity, place and the fixer's last word", () => {
  const found = parseFindings(fixtures[ids.pGarden]!);
  expect(found.map((f) => [f.number, f.done, f.severity])).toEqual([[1, true, "high"], [2, false, "medium"], [3, false, "low"]]);
  expect(found[0]).toMatchObject({ title: "Frost dates ignore the chosen hemisphere", where: "src/frost.ts:41" });
  expect(found[0]!.note).toMatch(/^Fixed: frost\.ts reads/);
  expect(found[1]!.note).toBeNull();
  expect(found[1]!.body).toContain("removeBed drops the bed");
});

it("leaves out what isn't in the form a review writes, and keeps the last note", () => {
  const text = [
    "# Findings", "Some preamble", "",
    "## [X] 4. Uppercase tick", "Severity: Critical", "Not fixed: needs a schema change", "Fixed: done after all",
    "## Notes", "A section of its own, not part of 4",
    "## [ ] 5 Missing dot", "## [ ] 6. Last one\r", "Where: a.ts",
  ].join("\n");
  const found = parseFindings(text);
  expect(found.map((f) => f.number)).toEqual([4, 6]);
  expect(found[0]).toMatchObject({ done: true, severity: null, note: "Fixed: done after all" });
  expect(found[0]!.body).not.toContain("A section of its own");
  expect(found[1]).toMatchObject({ title: "Last one", where: "a.ts" });
  expect(parseFindings("")).toEqual([]);
});

it("asks the reviewer to write only FINDINGS.md, in the form the phone reads", () => {
  const prompt = reviewPrompt({ focus: "security", where: { scope: "folder", path: "src/orders" }, most: 7 });
  expect(prompt).toContain("Review the folder src/orders as a security researcher");
  expect(prompt).toContain("Change no other file, and don't commit.");
  expect(prompt).toContain("write at most 7");
  expect(prompt).toContain("## [ ] 1. A short title");
  expect(parseFindings(prompt).map((f) => f.number)).toEqual([1]);
  expect(reviewPrompt({ focus: null, where: { scope: "project" }, most: 3 })).toContain("the whole project as a careful senior engineer");
  expect(reviewTitle("security", { scope: "changes" })).toBe("Find security issues in recent changes for FINDINGS.md");
  expect(reviewTitle(null, { scope: "project" })).toBe("Find issues for FINDINGS.md");
});

it("asks the reviewer for each finding's concrete failure, and says a second agent checks them where one does", () => {
  const checked = reviewPrompt({ focus: null, where: { scope: "project" }, most: 5, checked: true });
  expect(checked).toContain("give its concrete failure: the steps or inputs that make it happen, what should happen, and what happens instead");
  expect(checked).toContain("A second agent will check each finding before anything is fixed");
  expect(checked).toContain("Don't write \"Verdict:\" lines yourself");
  // Its own words about Verdict lines aren't read as one.
  expect(parseFindings(checked).map((f) => f.verdict)).toEqual([null]);
  const unchecked = reviewPrompt({ focus: null, where: { scope: "project" }, most: 5 });
  expect(unchecked).toContain("give its concrete failure");
  expect(unchecked).not.toContain("second agent");
  expect(unchecked).not.toContain("Verdict");
});

it("has Fix all skip set-aside findings and leave Verdict lines alone; fixing one still fixes it", () => {
  const all = fixAllPrompt();
  expect(all).toContain('Skip any finding whose "Verdict:" line says rejected or uncertain');
  expect(all).toContain('Never change or remove a line that starts with "Verdict:".');
  expect(all).toContain("When every finding you were to fix is ticked");
  // Queued after a checked review, it fixes only what was confirmed.
  const checked = fixAllPrompt({ checked: true });
  expect(checked).toContain('Fix only findings whose last "Verdict:" line says confirmed');
  expect(checked).toContain("with no \"Verdict:\" line or one that says rejected or uncertain, and leave it for me");
  expect(checked).not.toContain("Skip any finding whose");
  expect(all).not.toContain("Fix only findings");
  const one = fixOnePrompt({ number: 3, title: "Seeds" });
  expect(one).not.toContain("Skip any finding");
  expect(one).toContain("fix it even if that agent set it aside");
  expect(one).toContain('Never change or remove a line that starts with "Verdict:".');
});

it("reads every form of the Verdict line", () => {
  expect(parseVerdict("Verdict: confirmed, reproduced — a test showed it")).toEqual({ verdict: "confirmed", evidence: "reproduced", reason: "a test showed it" });
  expect(parseVerdict("Verdict: confirmed, from reading the code — save() skips the lock")).toEqual({ verdict: "confirmed", evidence: "source_reasoning", reason: "save() skips the lock" });
  expect(parseVerdict("Verdict: rejected — intended: the docs say so")).toEqual({ verdict: "rejected", evidence: null, reason: "intended: the docs say so" });
  expect(parseVerdict("Verdict: uncertain — couldn't run the device test")).toEqual({ verdict: "uncertain", evidence: null, reason: "couldn't run the device test" });
  // A hyphen or en dash for the em dash, odd spacing and capitals.
  expect(parseVerdict("Verdict: confirmed, reproduced - ran it twice")).toEqual({ verdict: "confirmed", evidence: "reproduced", reason: "ran it twice" });
  expect(parseVerdict("verdict :Confirmed ,  From  Reading the Code–the path is a-b-c ")).toEqual({ verdict: "confirmed", evidence: "source_reasoning", reason: "the path is a-b-c" });
  expect(parseVerdict("Verdict:rejected—already there before the change")).toEqual({ verdict: "rejected", evidence: null, reason: "already there before the change" });
  expect(parseVerdict("  Verdict: REJECTED -- can't happen  ")).toEqual({ verdict: "rejected", evidence: null, reason: "can't happen" });
  // No reason, or only a full stop.
  expect(parseVerdict("Verdict: confirmed, reproduced")).toEqual({ verdict: "confirmed", evidence: "reproduced", reason: null });
  expect(parseVerdict("Verdict: uncertain.")).toEqual({ verdict: "uncertain", evidence: null, reason: null });
  // Not Verdict lines.
  for (const line of ["Verdict: maybe — who knows", "Verdict: confirmedly", "The verdict: confirmed", "Verdict:", "Fixed: done"]) expect(parseVerdict(line)).toBeNull();
});

it("reads each finding's verdict and keeps it out of the text shown under it", () => {
  const text = [
    "## [ ] 1. Confirmed by running it", "Severity: high", "Where: a.ts:1", "Saves twice.", "Verdict: confirmed, reproduced — ran save() twice",
    "## [ ] 2. Confirmed by reading", "Verdict:   confirmed,from reading the code -   lock is skipped",
    "## [ ] 3. Not a problem", "Verdict: rejected — intended", "",
    "## [ ] 4. Unsettled", "Verdict: uncertain - no device here",
    "## [x] 5. Fixed after checking", "Verdict: confirmed, reproduced — test", "Fixed: added the lock",
    "## [ ] 6. From an older file", "Where: b.ts", "No verdict at all.",
    "## [ ] 7. Checked twice", "Verdict: uncertain — first look", "Verdict: confirmed, reproduced — second look",
    "## [ ] 8. A sample, not a verdict", "```", "Verdict: rejected — in a code block", "```",
  ].join("\n");
  const found = parseFindings(text);
  expect(found.map((f) => [f.number, f.verdict, f.evidence])).toEqual([
    [1, "confirmed", "reproduced"], [2, "confirmed", "source_reasoning"], [3, "rejected", null], [4, "uncertain", null],
    [5, "confirmed", "reproduced"], [6, null, null], [7, "confirmed", "reproduced"], [8, null, null],
  ]);
  expect(found[0]).toMatchObject({ reason: "ran save() twice", details: "Saves twice." });
  expect(found[0]!.body).toContain("Verdict: confirmed, reproduced — ran save() twice");
  expect(found[1]!.reason).toBe("lock is skipped");
  expect(found.map(isSetAside)).toEqual([false, false, true, true, false, false, false, false]);
  expect(found[4]).toMatchObject({ done: true, note: "Fixed: added the lock" });
  expect(found[5]).toMatchObject({ verdict: null, evidence: null, reason: null, where: "b.ts", details: "No verdict at all." });
  expect(found[6]!.reason).toBe("second look");
  expect(found[7]!.details).toContain("Verdict: rejected — in a code block");
});

it("reads the sim's FINDINGS.md verdicts, hyphen and all", () => {
  const found = parseFindings(fixtures[ids.pGarden]!);
  expect(found.map((f) => [f.number, f.verdict, f.evidence])).toEqual([[1, "confirmed", "reproduced"], [2, "confirmed", "source_reasoning"], [3, "rejected", null]]);
  expect(found[1]!.reason).toMatch(/^removeBed deletes from beds only/);
});

it("asks the fixer to tick each box and to ask before deleting the file", () => {
  for (const prompt of [fixAllPrompt(), fixOnePrompt({ number: 2, title: "Deleting a bed leaves its plantings behind" })]) {
    expect(prompt).toContain('tick its box ("## [x]")');
    expect(prompt).toContain("ask me whether to delete FINDINGS.md, and delete it only if I say yes.");
    expect(prompt).toContain("leave it out of any commit");
  }
  expect(fixOnePrompt({ number: 2, title: "Beds" })).toMatch(/^Fix finding 2 in FINDINGS\.md/);
  expect(fixOneTitle({ number: 9, title: "x".repeat(200) })).toHaveLength(120);
});

it("reads headings and fields in code examples as code, keeping the explanation after them", () => {
  const sample = (heading: string) => [
    "# Findings", "",
    "## [ ] 1. Headings in a sample end the explanation",
    "Severity: medium", "Where: src/parse.ts:12",
    "Given this file:", "",
    "```md", heading, "Severity: low", "Where: elsewhere.ts", "Fixed: not really", "```", "",
    "the parser stops at the sample's heading.",
    "",
    "## [ ] 2. The next real one",
  ].join("\n");
  for (const heading of ["# Example document", "## [ ] 99. This is sample input", "## Notes"]) {
    const found = parseFindings(sample(heading));
    expect(found.map((f) => f.number)).toEqual([1, 2]);
    expect(found[0]).toMatchObject({ severity: "medium", where: "src/parse.ts:12", note: null });
    expect(found[0]!.body).toContain(["```md", heading, "Severity: low", "Where: elsewhere.ts", "Fixed: not really", "```"].join("\n"));
    expect(found[0]!.body).toMatch(/the parser stops at the sample's heading\.$/);
    // The sheet shows severity and place above the text; the sample's own lines stay in it.
    expect(found[0]!.details).not.toMatch(/^Severity: medium$|^Where: src/m);
    expect(found[0]!.details).toContain("Where: elsewhere.ts");
  }
});

it("closes a code example only with the same fence, at least as long", () => {
  const text = [
    "## [ ] 1. Nested fences",
    "````md", "```", "## [ ] 2. Inside the outer fence", "```", "~~~", "## [ ] 3. Still inside", "````",
    "~~~", "```", "## [ ] 4. Inside a tilde fence", "~~~~",
    "   ```js", "# indented fence, still code", "   ```",
    "    ```", "## [ ] 5. A real one: four spaces is an indented block, not a fence", "```text extra words", "## [ ] 6. Code to the end of the file",
  ].join("\n");
  const found = parseFindings(text);
  expect(found.map((f) => f.number)).toEqual([1, 5]);
  expect(found[0]!.body).toContain("# indented fence, still code");
  expect(found[1]!.body).toContain("## [ ] 6. Code to the end of the file");
  // A backtick fence's words can't hold a backtick, so this is inline code, not a fence.
  expect(parseFindings("## [ ] 1. A\n``` not `a fence`\n## [ ] 2. B").map((f) => f.number)).toEqual([1, 2]);
});

it("finds code in lists and quotes, so the findings after it stay", () => {
  const text = [
    "## [x] 1. First", "",
    "- ```sh", "  echo example", "  ```", "",
    "> ```md", "> ## [ ] 98. Quoted sample", "> ```", "",
    "1. Step one:", "   ~~~", "   Severity: low", "   ~~~", "",
    "## [ ] 2. Second", "Severity: high", "Must stay visible.",
  ].join("\n");
  const found = parseFindings(text);
  expect(found.map((f) => [f.number, f.done, f.severity])).toEqual([[1, true, null], [2, false, "high"]]);
  expect(found[0]!.body).toContain("- ```sh\n  echo example\n  ```");
  expect(found[1]!.body).toBe("Severity: high\nMust stay visible.");
});

it("reads line ends and code as CommonMark does, as the computer's own reading of the file does", () => {
  // The same files as the host's findings tests, which must list the same findings.
  const files: [string, number[]][] = [
    ["## [ ] 1. A bug\r## [ ] 2. After a lone return\r", [1, 2]],
    ["## [ ] 1. A bug\n```text```\n## [ ] 2. After inline code\n```\n", [1, 2]],
    ["## [ ] 1. A bug\n\n    ```\n## [ ] 2. After indented code\n    ```\n", [1, 2]],
    ["## [ ] 1. A bug\n- a step\n  ```\n## [ ] 2. After a list's code\n  ```\n", [1, 2]],
    ["## [ ] 1. A bug\n> ```\n## [ ] 2. After a quote's code\n> ```\n", [1, 2]],
    ["## [ ] 1. A bug\n````\n```\n## [ ] 2. Still code\n~~~~\n````\n## [ ] 3. After\n", [1, 3]],
    ["## [ ] 1. A bug\n```\n    ```\n## [ ] 2. Still code\n```\n## [ ] 3. After\n", [1, 3]],
  ];
  for (const [text, shown] of files) expect(parseFindings(text).map((f) => f.number), JSON.stringify(text)).toEqual(shown);
});

it("keeps the indentation of a code example at the start of the sheet's text", () => {
  const code = "    Where: sample.ts  \n    ## [ ] 99. Sample";
  const found = parseFindings(`## [ ] 1. First\nSeverity: high\nWhere: real.ts\n\n${code}\n\n## [ ] 2. Second`);
  expect(found.map((f) => f.number)).toEqual([1, 2]);
  expect(found[0]).toMatchObject({ where: "real.ts", details: code });
});
