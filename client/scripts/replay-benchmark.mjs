// SPDX-License-Identifier: Apache-2.0
// Node 24, as used by CI. Event construction and UI rendering are excluded.
import { registerHooks } from "node:module";

// The client uses extensionless TypeScript imports for its bundler.
registerHooks({ resolve(specifier, context, next) {
  try { return next(specifier, context); }
  catch (error) {
    if (error.code === "ERR_MODULE_NOT_FOUND" && specifier.startsWith(".") && !specifier.endsWith(".ts"))
      return next(`${specifier}.ts`, context);
    throw error;
  }
} });
const { applyReplay, applyTimelinePage, emptyHostState, retainTimeline } = await import("../src/state/projection.ts");

for (const open of [false, true]) {
  for (const count of [1_000, 5_000, 10_000]) {
    const frames = Array.from({ length: count }, (_, n) => ({
      env: { seq: n + 1, at: n, source: { kind: "companion" }, provenance: "native",
        event: { type: "timeline_appended", task_id: "task", item: { seq: n + 1, at: n, run_id: null,
          item: { kind: "activity", tool: "test", summary: "Working", status: "succeeded", detail: "x".repeat(2048) } } } }, now: n,
    }));
    const initial = open ? retainTimeline(applyTimelinePage(emptyHostState, "task", [], false), "task") : emptyHostState;
    const samples = [];
    let result;
    for (let repeat = 0; repeat < 12; repeat++) {
      const start = performance.now();
      result = applyReplay(initial, frames);
      const elapsed = performance.now() - start;
      if (repeat >= 3) samples.push(elapsed);
    }
    samples.sort((a, b) => a - b);
    console.log(JSON.stringify({ open, events: count, median_ms: +samples[4].toFixed(3), retained: result.timelines.task.items.length }));
  }
}
