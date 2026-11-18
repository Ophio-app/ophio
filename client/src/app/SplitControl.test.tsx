// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { SplitControl, SplitControls, type SplitControlsValue } from "./SplitControl";

afterEach(cleanup);

function controls(overrides: Partial<SplitControlsValue> = {}): SplitControlsValue {
  return { side: "left", available: true, opened: false, twoVisible: false, open: vi.fn(), close: vi.fn(), ...overrides };
}

it.each([
  { name: "screen header", props: {} },
  { name: "pushed or focused header", props: { focused: true } },
  { name: "wide side rail", props: { rail: true } },
])("shows a named open action in the $name", ({ props }) => {
  const value = controls();
  const { container } = render(<SplitControls.Provider value={value}><SplitControl {...props} /></SplitControls.Provider>);
  const button = screen.getByRole("button", { name: "Open split view" });
  expect(button.textContent).toBe("Split view");
  expect(button.querySelector("svg.lucide-columns-2[aria-hidden='true']")).not.toBeNull();
  expect(screen.queryByRole("button", { name: "Split view" })).toBeNull();
  expect(container.querySelector(props.rail ? ".split-control--rail" : ".split-control--header")).not.toBeNull();
  if (props.focused) expect(container.querySelector(".split-control--focused")).not.toBeNull();
  if (props.rail) {
    expect(button.classList.contains("nav__tab")).toBe(true);
    expect(button.querySelector(".nav__pill > .nav__icon")).not.toBeNull();
    expect(button.querySelector(".nav__label")?.textContent).toBe("Split view");
  } else {
    expect(button.classList.contains("btn--secondary")).toBe(true);
  }
  fireEvent.click(button);
  expect(value.open).toHaveBeenCalledOnce();
  expect(value.close).not.toHaveBeenCalled();
});

it("keeps the visible label and accessible name in the floating copy", () => {
  render(<SplitControls.Provider value={controls()}><div className="split-floating"><SplitControl focused /></div></SplitControls.Provider>);
  expect(screen.getByRole("button", { name: "Open split view" }).textContent).toBe("Split view");
});

it("renders nothing without split context", () => {
  const { container } = render(<SplitControl />);
  expect(container.childElementCount).toBe(0);
});

it.each([{}, { focused: true }, { rail: true }])("renders nothing when unavailable (%j)", props => {
  const { container } = render(<SplitControls.Provider value={controls({ available: false })}><SplitControl {...props} /></SplitControls.Provider>);
  expect(container.childElementCount).toBe(0);
});

it.each([
  { opened: false, twoVisible: true },
  { opened: true, twoVisible: false },
  { opened: true, twoVisible: true },
])("suppresses the rail action in existing split states (%j)", state => {
  const { container } = render(<SplitControls.Provider value={controls(state)}><SplitControl rail /></SplitControls.Provider>);
  expect(container.childElementCount).toBe(0);
});

it.each(["left", "right"] as const)("keeps the %s close action icon-only, including when opening is unavailable", side => {
  const value = controls({ side, opened: true, available: false, twoVisible: true });
  const { container } = render(<SplitControls.Provider value={value}><SplitControl focused /></SplitControls.Provider>);
  const button = screen.getByRole("button", { name: `Close ${side} side` });
  expect(button.textContent).toBe("");
  expect(button.querySelector(`svg.lucide-panel-${side}-close[aria-hidden='true']`)).not.toBeNull();
  expect(container.querySelector(".split-control--close.split-control--focused")).not.toBeNull();
  expect(screen.queryByText("Split view")).toBeNull();
  fireEvent.click(button);
  expect(value.close).toHaveBeenCalledOnce();
  expect(value.open).not.toHaveBeenCalled();
});

it("keeps the normal header close action available", () => {
  const value = controls({ opened: true });
  render(<SplitControls.Provider value={value}><SplitControl /></SplitControls.Provider>);
  fireEvent.click(screen.getByRole("button", { name: "Close left side" }));
  expect(value.close).toHaveBeenCalledOnce();
});
