// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { setNavBack, useBackLayer } from "./back";

// The guard entries above the page. The browser removes one on system back;
// history.back() removes one and reports it like the browser does.
let guards = 0;
const navigateBack = vi.fn();
const systemBack = () => {
  guards--;
  act(() => window.dispatchEvent(new PopStateEvent("popstate")));
};

beforeEach(() => {
  guards = 0;
  navigateBack.mockClear();
  vi.spyOn(history, "pushState").mockImplementation(() => { guards++; });
  vi.spyOn(history, "back").mockImplementation(() => {
    guards--;
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
});
afterEach(() => {
  cleanup();
  setNavBack(null);
  vi.restoreAllMocks();
});

function Layer({ open, onBack, name }: { open: boolean; onBack: () => void; name: string }) {
  useBackLayer(open, onBack);
  return open ? <p>{name}</p> : null;
}

it.each(["browser", "android"])("asks the editor again after its close confirmation is dismissed (%s)", (platform) => {
  const back = () => platform === "android" ? act(() => { expect(window.systemBack?.()).toBe(true); }) : systemBack();
  setNavBack({ canGoBack: () => true, back: navigateBack });
  function Editor() {
    const [editing, setEditing] = useState(true), [confirming, setConfirming] = useState(false);
    return <>
      <Layer open={editing} name="editor" onBack={() => setConfirming(true)} />
      <Layer open={confirming} name="keep draft?" onBack={() => setConfirming(false)} />
      <button onClick={() => { setConfirming(false); setEditing(false); }}>Keep draft</button>
    </>;
  }
  render(<Editor />);
  back();
  expect(screen.getByText("keep draft?")).toBeTruthy();
  back();
  expect(screen.queryByText("keep draft?")).toBeNull();
  back();
  expect(screen.getByText("editor")).toBeTruthy();
  expect(screen.getByText("keep draft?")).toBeTruthy();
  expect(navigateBack).not.toHaveBeenCalled();
  act(() => screen.getByRole("button", { name: "Keep draft" }).click());
  back();
  expect(navigateBack).toHaveBeenCalledOnce();
});

it("keeps a dialog that refuses to close while busy first in line, guard included", () => {
  setNavBack({ canGoBack: () => false, back: navigateBack });
  let setBusy: (busy: boolean) => void = () => {};
  function Review() {
    const [open, setOpen] = useState(true), [busy, busyState] = useState(true);
    setBusy = busyState;
    return <Layer open={open} name="review" onBack={() => { if (!busy) setOpen(false); }} />;
  }
  render(<Review />);
  expect(guards).toBe(1);
  systemBack();
  expect(screen.getByText("review")).toBeTruthy();
  // Without a guard, the next system back would leave the app.
  expect(guards).toBe(1);
  systemBack();
  expect(screen.getByText("review")).toBeTruthy();
  expect(guards).toBe(1);
  act(() => setBusy(false));
  systemBack();
  expect(screen.queryByText("review")).toBeNull();
  expect(guards).toBe(0);
  expect(navigateBack).not.toHaveBeenCalled();
});

it("closes stacked dialogs top first, then navigates", () => {
  setNavBack({ canGoBack: () => true, back: navigateBack });
  function Stack() {
    const [lower, setLower] = useState(true), [upper, setUpper] = useState(true);
    return <>
      <Layer open={lower} name="lower" onBack={() => setLower(false)} />
      <Layer open={upper} name="upper" onBack={() => setUpper(false)} />
    </>;
  }
  render(<Stack />);
  systemBack();
  expect(screen.queryByText("upper")).toBeNull();
  expect(screen.getByText("lower")).toBeTruthy();
  systemBack();
  expect(screen.queryByText("lower")).toBeNull();
  expect(navigateBack).not.toHaveBeenCalled();
  systemBack();
  expect(navigateBack).toHaveBeenCalledOnce();
  expect(guards).toBe(1);
});
