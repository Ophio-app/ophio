// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { CrashRegistry, DiagnosticsBoundary, ownFrames } from "./crashes";
import { MAX_FRAMES } from "./constants";

beforeEach(() => localStorage.clear());
afterEach(() => { cleanup(); vi.restoreAllMocks(); });
const stack = () => `TypeError: private\n    at render (${location.origin}/assets/index-abc.js:42:7)\n    at other (https://outside.example/assets/index-abc.js:1:1)\n    at secret (file:///home/sam/app.js:2:1)\n    at addon (${location.origin}/extension.js:3:2)\n    at /private/path (${location.origin}/assets/chunk-def.js:8:1)`;
it("keeps only app frames, reduces files to asset names and limits frames", () => {
  expect(ownFrames(stack())).toEqual([{ function: "render", file: "index-abc.js", line: 42, column: 7 }, { file: "chunk-def.js", line: 8, column: 1 }]);
  expect(ownFrames(Array.from({ length: 60 }, () => `render@${location.origin}/assets/index-abc.js:3:4`).join("\n"))).toHaveLength(MAX_FRAMES);
});
it("captures onerror and rejection durably and restores the previous handler", () => {
  const previous = vi.fn(() => false); window.onerror = previous;
  const registry = new CrashRegistry(); registry.build = "phone-build";
  const off = registry.install();
  const error = new TypeError('open /home/sam/private failed for "Secret"'); error.stack = stack();
  window.onerror?.("private", "file:///secret", 1, 1, error);
  const rejection = new Event("unhandledrejection"); Object.defineProperty(rejection, "reason", { value: new Error("fetch https://private.example failed") });
  window.dispatchEvent(rejection);
  expect(previous).toHaveBeenCalled();
  expect(registry.pending()).toHaveLength(2);
  expect(registry.pending()[1]).toMatchObject({ side: "phone", kind: "script", code: "js:TypeError@render", build: "phone-build", message: 'open <path> failed for "…"' });
  const encoded = JSON.stringify(new CrashRegistry().pending());
  expect(encoded).not.toContain("private.example"); expect(encoded).not.toContain("/home/sam");
  off(); expect(window.onerror).toBe(previous); window.onerror = null;
});
it("records render errors and provides the calm reload fallback", () => {
  vi.spyOn(console, "error").mockImplementation(() => {});
  const registry = new CrashRegistry(), reload = vi.fn();
  function Broken(): never { throw new Error("open /private/path failed"); }
  render(<DiagnosticsBoundary registry={registry} reload={reload}><Broken /></DiagnosticsBoundary>);
  expect(registry.pending()).toHaveLength(1);
  expect(registry.pending()[0]?.kind).toBe("render");
  expect(screen.getByText("Something went wrong")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Reload" })); expect(reload).toHaveBeenCalledOnce();
});
it("merges native sources once, tolerates source failures and acknowledges only sent crashes", async () => {
  const registry = new CrashRegistry(); registry.capture("first");
  const source = vi.fn(async () => [{ at: 1, side: "phone" as const, kind: "system" as const, code: "exit:low_memory", build: "build", frames: [] }]);
  const off = registry.addCrashSource(source); registry.addCrashSource(async () => { throw new Error("unsupported"); });
  await Promise.all([registry.collect(), registry.collect()]); expect(source).toHaveBeenCalledOnce();
  expect(registry.pending()).toHaveLength(2);
  await registry.collect(); expect(registry.pending()).toHaveLength(2);
  const sent = registry.pending(); registry.capture("second"); registry.acknowledge(sent); expect(registry.pending()).toHaveLength(1); off();
});
it("forgets what came before an update, kept or still to come", () => {
  let now = 100;
  const registry = new CrashRegistry(localStorage, () => now); registry.capture("old build");
  now = 300; registry.capture("new build");
  registry.forgetBefore(200);
  expect(registry.pending().map(c => c.message)).toEqual(["new build"]);
  expect(new CrashRegistry(localStorage).pending()).toHaveLength(1);
  // The update's own stop, reported late by the phone.
  registry.keep({ at: 150, side: "phone", kind: "system", code: "exit:signaled", build: "build", frames: [] });
  registry.forgetBefore(50);
  expect(registry.pending()).toHaveLength(1);
});
it("reduces native panic paths and drops arbitrary native frame functions", () => {
  const registry = new CrashRegistry();
  registry.keep({ at: 1, side: "phone", kind: "panic", code: "panic:/home/sam/src/native.rs:42", build: "test", message: "open /home/sam/private failed", frames: [{ file: "native.rs", function: "/home/sam/private", line: 42 }, { file: "/device/private.rs", line: 1 }] });
  expect(registry.pending()[0]).toMatchObject({ code: "panic:native.rs:42", message: "open <path> failed", frames: [{ file: "native.rs", line: 42 }] });
  expect(JSON.stringify(registry.pending())).not.toContain("/home/sam");
});
