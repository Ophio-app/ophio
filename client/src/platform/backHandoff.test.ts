// SPDX-License-Identifier: Apache-2.0
import { expect, it, vi } from "vitest";
import { setNavBack, sync } from "../design/back";

it("serializes removal of a sheet guard before arming the destination guard", () => {
  const push = vi.spyOn(history, "pushState").mockImplementation(() => {});
  const back = vi.spyOn(history, "back").mockImplementation(() => {});
  let wanted = true;
  const navigateBack = vi.fn();
  setNavBack({ canGoBack: () => wanted, back: navigateBack });
  expect(push).toHaveBeenCalledTimes(1);
  wanted = false; sync();
  expect(back).toHaveBeenCalledTimes(1);
  wanted = true; sync(); // Destination opens before the browser's queued pop.
  wanted = false; sync(); wanted = true; sync();
  expect(back).toHaveBeenCalledTimes(1); expect(push).toHaveBeenCalledTimes(1);
  window.dispatchEvent(new PopStateEvent("popstate"));
  expect(navigateBack).not.toHaveBeenCalled(); expect(push).toHaveBeenCalledTimes(2);
  setNavBack(null); window.dispatchEvent(new PopStateEvent("popstate"));
  push.mockRestore(); back.mockRestore();
});
