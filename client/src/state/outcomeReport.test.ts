// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { withoutOutcome } from "./outcomeReport";

const block = "```ophio-outcome\n{\"status\":\"done\"}\n```";

describe("withoutOutcome", () => {
  it("takes the agent's report out of what the person reads", () => {
    expect(withoutOutcome(`Fixed the parser.\n\n${block}`)).toBe("Fixed the parser.");
    expect(withoutOutcome(`Done.\n\n\`\`\`ophio-outcome json\n{}\n\`\`\`\n`)).toBe("Done.");
  });

  it("takes a review's findings and the other blocks for Ophio out too", () => {
    const findings = "```ophio-findings\n[]\n```";
    expect(withoutOutcome(`Nothing worth filing.\n\n${findings}\n\n${block}`)).toBe("Nothing worth filing.");
    expect(withoutOutcome("Sorted.\n\n```ophio-triage json\n[]\n```")).toBe("Sorted.");
    const other = "```ophio-findings-old\n[]\n```";
    expect(withoutOutcome(other)).toBe(other);
  });

  it("leaves a message without one as it is", () => {
    const text = "Here is the code:\n\n```ts\nconst a = 1;\n```\n";
    expect(withoutOutcome(text)).toBe(text);
    expect(withoutOutcome("Plain words.  ")).toBe("Plain words.  ");
  });

  it("keeps one quoted inside another block, or mentioned in passing", () => {
    const quoted = "Agents write:\n\n````md\n" + block + "\n````";
    expect(withoutOutcome(quoted)).toBe(quoted);
    const inline = "End with an `ophio-outcome` block.";
    expect(withoutOutcome(inline)).toBe(inline);
  });

  it("hides one still streaming, and its first line as it arrives", () => {
    expect(withoutOutcome("All done.\n\n```ophio-outcome\n{\"status\":")).toBe("All done.");
    expect(withoutOutcome("All done.\n\n```ophio-out")).toBe("All done.");
    expect(withoutOutcome("All done.\n\n```")).toBe("All done.");
    expect(withoutOutcome("Code:\n```ts\nx\n```\nAfter")).toBe("Code:\n```ts\nx\n```\nAfter");
  });
});
