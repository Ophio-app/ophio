// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useLocalText } from "./local";

afterEach(() => { cleanup(); localStorage.clear(); vi.restoreAllMocks(); });

it("reports whether a local choice was saved while sharing it immediately", () => {
  const first = renderHook(() => useLocalText("checks-offer-save-result"));
  let stored: boolean | undefined;
  act(() => { stored = first.result.current[1]("dismissed"); });
  expect(stored).toBe(true);
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("Storage full"); });
  act(() => { stored = first.result.current[1]("changed"); });
  expect(stored).toBe(false);
  expect(first.result.current[0]).toBe("changed");
  expect(first.result.current[2]).toBe(false);
});
