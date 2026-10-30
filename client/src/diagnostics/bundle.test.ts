// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { ComputerPart, ReportBundle } from "../protocol/types";
import { buildBundle, bundleBytes, describeBundle, enforceSize, checkedScreenshot } from "./bundle";
import { MAX_REPORT_BYTES, MAX_REPORT_WORDS, MAX_SCREENSHOT_BYTES } from "./constants";
const id = "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b";
const base = () => buildBundle({ id, now: 1, salt: "00112233445566778899aabbccddeeff", origin: { type: "error" }, context: { screen: "/receipt", task: id, command: "start_task", error: { code: "internal", message: "open /home/sam/private failed" } } });
export const computer: ComputerPart = { versions: { build: "computer-build", os: "Linux", protocol: { major: 1, minor: 0 }, features: ["reports_v1"], harnesses: [] }, trail: [{ at: 1, event: { type: "task_state", task: "t:bc25fcec", state: "failed" } }], crashes: [], extras: [], sending: true };
it("builds a scrubbed, salted bundle, merges the exact computer preview, and never sends the salt", async () => {
  const report = await base();
  expect(report.context.task).toBe("t:bc25fcec"); expect(report.context.error?.message).toBe("open <path> failed");
  expect(report.signature).toEqual({ code: "internal", screen: "/receipt", build: "unknown" });
  const merged = await buildBundle({ salt: "salt", origin: { type: "settings" }, context: { screen: "/settings" }, computer, words: "🙂".repeat(MAX_REPORT_WORDS + 1) });
  expect(merged.versions.computer).toEqual(computer.versions); expect(merged.computer_trail).toEqual(computer.trail);
  expect(Array.from(merged.words!).length).toBe(MAX_REPORT_WORDS);
  expect(JSON.stringify(merged)).not.toContain('"salt"');
});
it("cuts store, then oldest events across both devices, then picture, with words unchanged", async () => {
  const bundle = await base(); bundle.words = "The person’s words always stay.";
  bundle.state = { version: 1, store: "x".repeat(1000) };
  bundle.phone_trail = [{ at: 2, event: { type: "route", route: "/work" } }, { at: 3, event: { type: "route", route: "/settings" } }];
  bundle.computer_trail = [{ at: 1, event: { type: "service", state: "started" } }];
  bundle.screenshot = { media_type: "image/png", data: "YQ==".repeat(10), width: 1, height: 1, marked: false };
  const noStore = structuredClone(bundle); delete noStore.state;
  const first = enforceSize(bundle, bundleBytes(noStore)); expect(first).toEqual(noStore);
  const noOldest = structuredClone(noStore); noOldest.computer_trail = [];
  const second = enforceSize(bundle, bundleBytes(noOldest)); expect(second).toEqual(noOldest);
  const noTrails = { ...noOldest, phone_trail: [] };
  const third = enforceSize(bundle, bundleBytes(noTrails) - 1); expect(third.phone_trail).toEqual([]); expect(third.screenshot).toBeUndefined(); expect(third.words).toBe(bundle.words);
  expect(bundle.state).toBeDefined(); expect(bundle.phone_trail).toHaveLength(2);
});
it("enforces the real UTF-8 cap and removes extras before crashes when necessary", async () => {
  const bundle = await base(); bundle.words = "🙂".repeat(MAX_REPORT_WORDS);
  bundle.state = { version: 1, store: "🙂".repeat(MAX_REPORT_BYTES) };
  bundle.extras = [{ type: "task_transcript", task: "t:12345678", entries: [{ at: 1, role: "person", text: "🙂".repeat(MAX_REPORT_BYTES) }], cut: false }];
  bundle.crashes = [{ side: "phone", kind: "script", code: "js:Error@unknown", at: 1, build: "phone", frames: [] }];
  const capped = enforceSize(bundle); expect(bundleBytes(capped)).toBeLessThanOrEqual(MAX_REPORT_BYTES); expect(capped.words).toBe(bundle.words); expect(capped.crashes).toHaveLength(1);
  expect(capped.extras[0]).toMatchObject({ entries: [], cut: true });
  expect(() => enforceSize(bundle, 1)).toThrow("too large");
});
it("checks screenshot media, dimensions, base64, and decoded size", () => {
  const good = { media_type: "image/png", data: "YQ==", width: 1, height: 2, marked: true };
  expect(checkedScreenshot(good)).toEqual(good);
  for (const bad of [{ ...good, media_type: "text/html" }, { ...good, width: -1 }, { ...good, data: "%%%" }, { ...good, data: "AAAA".repeat(Math.ceil(MAX_SCREENSHOT_BYTES / 3) + 1) }]) expect(() => checkedScreenshot(bad)).toThrow();
});
function reconstruct(bundle: ReportBundle): unknown {
  let result: unknown = {};
  for (const { fields } of describeBundle(bundle)) for (const { path, value } of fields) {
    let target = result as Record<string | number, unknown>;
    for (let i = 0; i < path.length - 1; i++) {
      const key = path[i]!;
      target[key] ??= typeof path[i + 1] === "number" ? [] : {};
      target = target[key] as Record<string | number, unknown>;
    }
    target[path.at(-1)!] = value;
  }
  return result;
}
it("See all losslessly describes every sent field, null and empty container, across nested bundles", async () => {
  const bundle = await base();
  bundle.words = "Exact words"; bundle.versions.computer = computer.versions; bundle.computer_trail = computer.trail;
  bundle.crashes = [{ at: 1, side: "phone", kind: "render", code: "js:Error@render", build: "test", message: "scrubbed", frames: [{ function: "render", file: "index.js", line: 1, column: 2 }] }];
  bundle.screenshot = { media_type: "image/png", data: "YQ==", width: 1, height: 2, marked: true };
  bundle.extras = [{ type: "names", names: [{ name: "Explicit consent", pseudonym: "t:12345678" }] }, { type: "task_transcript", task: "t:12345678", entries: [{ at: 1, role: "person", text: "Explicit consent" }], cut: false }];
  for (let i = 0; i < 30; i++) {
    bundle.state = { version: 1, store: { nested: [null, false, i, `~text:${i}`, [], {}, { other: [1, 2, { last: "~url:42" }] }] } };
    expect(reconstruct(bundle)).toEqual(JSON.parse(JSON.stringify(bundle)));
  }
});
