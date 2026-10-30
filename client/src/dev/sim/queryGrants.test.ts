// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import { createSimPlatform } from "./platform";
import { ids } from "./scenario";
import type { ServerFrame } from "../../protocol/types";

afterEach(() => { window.__sim?.host.dispose(); vi.useRealTimers(); vi.restoreAllMocks(); });

it("does not invent source for an unknown report", async () => {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  createSimPlatform();
  expect(await window.__sim?.host.query({ kind: "inbox_report_code", report_id: "missing" })).toEqual({ kind: "inbox_report_code", code: null });
});

it("requires the owner's admin grant to read private report code", async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const control = platform.control(profile.profileId);
  const results: ServerFrame[] = [];
  control.listen((e) => { if (e.type === "frame" && e.frame.type === "query_result") results.push(e.frame); });
  window.__sim?.permissions({ admin: false, view: true });
  control.connect();
  await vi.advanceTimersByTimeAsync(900);
  expect(control.send({ type: "query", query_id: "private-code", query: { kind: "inbox_report_code", report_id: "test-report" } })).toBe(true);
  await vi.advanceTimersByTimeAsync(900);
  expect(results).toEqual([{
    type: "query_result", query_id: "private-code",
    result: { Err: { code: "permission_denied", message: expect.any(String) } },
  }]);
  control.disconnect();
});

it("answers a device allowed to approve when it reads one approval", async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const control = platform.control(profile.profileId);
  const results: ServerFrame[] = [];
  control.listen((e) => { if (e.type === "frame" && e.frame.type === "query_result") results.push(e.frame); });
  control.connect();
  await vi.advanceTimersByTimeAsync(900);
  expect(control.send({ type: "query", query_id: "one-approval", query: { kind: "approval", approval_id: ids.aTest } })).toBe(true);
  await vi.advanceTimersByTimeAsync(900);
  expect(results).toEqual([{
    type: "query_result",
    query_id: "one-approval",
    result: { Ok: { kind: "approval", approval: expect.objectContaining({ approval_id: ids.aTest }) } },
  }]);
  control.disconnect();
});
