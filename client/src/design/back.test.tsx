// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { setActiveBackPane, setNavBack, useBackLayer } from "./back";
import { PaneContext, type PaneScope } from "../shell/pane";
import { SurfaceActive } from "./surface";

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
  setNavBack(null, "left");
  setNavBack(null, "right");
  setActiveBackPane(null);
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

const pane = (id: string, active: boolean, visible = true): PaneScope => ({
  id, active, visible, split: true, root: null, width: 500, height: 800,
});

it.each(["browser", "android"])("closes only the active pane's layer before its own navigation (%s)", (platform) => {
  const leftNav = vi.fn(), rightNav = vi.fn();
  setNavBack({ canGoBack: () => true, back: leftNav }, "left");
  setNavBack({ canGoBack: () => true, back: rightNav }, "right");
  setActiveBackPane("left");
  function Side({ id, active }: { id: string; active: boolean }) {
    const [open, setOpen] = useState(true);
    return <PaneContext.Provider value={pane(id, active)}>
      <Layer open={open} name={`${id} sheet`} onBack={() => setOpen(false)} />
    </PaneContext.Provider>;
  }
  const tree = (active: string) => <><Side id="left" active={active === "left"} /><Side id="right" active={active === "right"} /></>;
  const view = render(tree("left"));
  const back = () => platform === "android" ? act(() => { expect(window.systemBack?.()).toBe(true); }) : systemBack();
  expect(guards).toBe(1);
  back();
  expect(screen.queryByText("left sheet")).toBeNull();
  expect(screen.getByText("right sheet")).toBeTruthy();
  back();
  expect(leftNav).toHaveBeenCalledOnce();
  expect(rightNav).not.toHaveBeenCalled();
  act(() => { view.rerender(tree("right")); setActiveBackPane("right"); });
  back();
  expect(screen.queryByText("right sheet")).toBeNull();
  back();
  expect(rightNav).toHaveBeenCalledOnce();
  expect(guards).toBe(1);
});

it("gives app-wide layers priority even when a pane sheet registered later", () => {
  const closeGlobal = vi.fn(), closePane = vi.fn();
  setActiveBackPane("left");
  render(<>
    <Layer open name="global lock" onBack={closeGlobal} />
    <PaneContext.Provider value={pane("left", true)}><Layer open name="pane sheet" onBack={closePane} /></PaneContext.Provider>
  </>);
  systemBack();
  expect(closeGlobal).toHaveBeenCalledOnce();
  expect(closePane).not.toHaveBeenCalled();
  expect(guards).toBe(1);
});

it("never lets inactive, invisible or covered pane layers catch Back", () => {
  const close = vi.fn();
  setActiveBackPane("left");
  const tree = (scope: PaneScope, surface = true) => <PaneContext.Provider value={scope}>
    <SurfaceActive.Provider value={surface}><Layer open name="kept layer" onBack={close} /></SurfaceActive.Provider>
  </PaneContext.Provider>;
  const view = render(tree(pane("left", false)));
  expect(guards).toBe(0);
  expect(window.systemBack?.()).toBe(false);
  view.rerender(tree(pane("left", true, false)));
  expect(guards).toBe(0);
  expect(window.systemBack?.()).toBe(false);
  view.rerender(tree(pane("left", true), false));
  expect(guards).toBe(0);
  expect(window.systemBack?.()).toBe(false);
  expect(close).not.toHaveBeenCalled();
  view.rerender(tree(pane("left", true)));
  expect(guards).toBe(1);
  systemBack();
  expect(close).toHaveBeenCalledOnce();
});

it("cleans up one shell without clearing the other shell's navigation or guard", () => {
  const left = vi.fn(), right = vi.fn();
  setNavBack({ canGoBack: () => true, back: left }, "left");
  setNavBack({ canGoBack: () => true, back: right }, "right");
  setActiveBackPane("right");
  expect(guards).toBe(1);
  setNavBack(null, "left");
  expect(guards).toBe(1);
  systemBack();
  expect(right).toHaveBeenCalledOnce();
  expect(left).not.toHaveBeenCalled();
  setNavBack(null, "right");
  expect(guards).toBe(0);
  expect(window.systemBack?.()).toBe(false);
});
