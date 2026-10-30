// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import type { TimelineItem } from "../../protocol/types";
import { excerpt, stopProgress } from "./stopProgress";

let seq = 0;
const at = (body: TimelineItem["item"], run_id: string | null = "r1"): TimelineItem => ({ seq: ++seq, at: 1_000 + seq, run_id, item: body });
const said = (text: string, run_id?: string) => at({ kind: "agent_message", text, partial: false }, run_id);
const step = (key: string, tool: string, status: "running" | "succeeded" | "failed", detail: string | null = null, summary = tool) =>
  at({ kind: "activity", key, tool, summary, status, detail });
const stop = () => at({ kind: "receipt", receipt: { kind: "stop_task", requested_at: 9_000, summary: "The agent confirmed Stop.", facts: [] } });

describe("stopProgress", () => {
  it("reports the last message and the step Stop cut short, not what came after Stop", () => {
    const items = [
      said("Reading the retry code first."),
      step("a", "Read", "running", JSON.stringify({ file_path: "src/clock.rs" })),
      step("a", "Read", "succeeded", "…file contents…", "Read finished"),
      step("b", "Grep", "running", JSON.stringify({ pattern: "retry" })),
      step("b", "Grep", "failed", null, "Grep finished"),
      said("3 of 7 files read; checking src/retry.rs next."),
      step("c", "Read", "running", JSON.stringify({ file_path: "src/retry.rs" })),
      stop(),
      said("This came after Stop."),
    ];
    expect(stopProgress(items, "r1")).toEqual({
      said: "3 of 7 files read; checking src/retry.rs next.",
      finished: 1,
      failed: 1,
      last: { name: "Read", input: "src/retry.rs", ended: "did not finish" },
    });
  });

  it("keeps the step's own summary when it names no input, and says why it never ran", () => {
    const items = [
      step("a", "Read", "succeeded", null, "Read crates/payments-client/src/retry.rs"),
      at({ kind: "activity", key: "b", tool: "Bash", summary: "Bash", status: "failed", detail: null, not_run: "stopped" }),
    ];
    expect(stopProgress(items, "r1")?.last).toEqual({ name: "Bash: Not run (stopped)", input: null, ended: "not run (stopped)" });
  });

  it("is absent when the run recorded neither a message nor a step, and ignores other runs", () => {
    expect(stopProgress([said("Another run's words", "r0"), stop()], "r1")).toBeNull();
  });
});

describe("excerpt", () => {
  it("keeps short text whole on one line", () => {
    expect(excerpt("Done.\n\nAll  good.")).toBe("Done. All good.");
  });

  it("cuts long text at a word boundary and marks the cut", () => {
    const text = `${"word ".repeat(40)}end`;
    const cut = excerpt(text, 30);
    expect(cut).toBe("word word word word word word…");
    expect(cut.length).toBeLessThanOrEqual(31);
  });
});
