// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Sheet } from "./Sheet";
import { UrgentSlot } from "./urgent";
import { SurfaceActive } from "./surface";
import { useEffect, useState, type ReactNode } from "react";
import { PaneContext } from "../shell/pane";
import { setActiveBackPane } from "./back";

beforeEach(() => {
  HTMLDialogElement.prototype.showModal = vi.fn(function (this: HTMLDialogElement) { this.open = true; });
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(history, "pushState").mockImplementation(() => {});
  vi.spyOn(history, "back").mockImplementation(() => { window.dispatchEvent(new PopStateEvent("popstate")); });
});
afterEach(() => { cleanup(); setActiveBackPane(null); vi.restoreAllMocks(); });

it("dismisses a kept screen's modal on navigation and does not reopen it on Back", () => {
  const close = vi.fn();
  function Picture({ active }: { active: boolean }) {
    const [open, setOpen] = useState(true);
    return <SurfaceActive.Provider value={active}><Sheet open={open} title="Picture" onClose={() => { close(); setOpen(false); }}><button>Draw on it</button></Sheet></SurfaceActive.Provider>;
  }
  const view = render(<Picture active />);
  expect(document.querySelector("dialog[open]")).not.toBeNull();
  view.rerender(<Picture active={false} />);
  expect(document.querySelector("dialog[open]")).toBeNull();
  expect(close).toHaveBeenCalledTimes(1);
  view.rerender(<Picture active />);
  expect(document.querySelector("dialog[open]")).toBeNull();
});

it("keeps a sheet holding unsent work: it leaves the top layer while hidden and is back, as left, on Back", () => {
  const close = vi.fn();
  function Draft({ active }: { active: boolean }) {
    const [text, setText] = useState("");
    return <SurfaceActive.Provider value={active}>
      <Sheet open keepWhenHidden title="Fix it" onClose={close}><input aria-label="Direction" value={text} onChange={(e) => setText(e.target.value)} /></Sheet>
    </SurfaceActive.Provider>;
  }
  const view = render(<Draft active />);
  fireEvent.change(screen.getByRole("textbox", { name: "Direction" }), { target: { value: "Keep the retry" } });
  view.rerender(<Draft active={false} />);
  // No invisible modal left behind to catch taps on the screen in front.
  expect(document.querySelector("dialog[open]")).toBeNull();
  expect(close).not.toHaveBeenCalled();
  view.rerender(<Draft active />);
  expect(document.querySelector("dialog[open]")).not.toBeNull();
  expect(screen.getByRole<HTMLInputElement>("textbox", { name: "Direction" }).value).toBe("Keep the retry");
  expect(close).not.toHaveBeenCalled();
});

it("waits for Back to show a sheet holding a choice that was asked for while its screen was hidden", () => {
  const close = vi.fn();
  const tree = (active: boolean, open: boolean) => <SurfaceActive.Provider value={active}><Sheet open={open} keepWhenHidden title="Which project?" onClose={close}>Projects</Sheet></SurfaceActive.Provider>;
  const view = render(tree(false, false));
  // The computer answered after another screen came in front.
  view.rerender(tree(false, true));
  expect(document.querySelector("dialog[open]")).toBeNull();
  view.rerender(tree(true, true));
  expect(screen.getByRole("dialog", { name: "Which project?" })).toBeTruthy();
  expect(close).not.toHaveBeenCalled();
});

it("does not open an asynchronous modal on an already hidden screen", () => {
  const close = vi.fn();
  const view = render(<SurfaceActive.Provider value={false}><Sheet open={false} title="Picture" onClose={close}>Picture</Sheet></SurfaceActive.Provider>);
  view.rerender(<SurfaceActive.Provider value={false}><Sheet open title="Picture" onClose={close}>Picture</Sheet></SurfaceActive.Provider>);
  expect(document.querySelector("dialog[open]")).toBeNull();
  expect(close).toHaveBeenCalledTimes(1);
});

it("puts the keyboard away on the first tap outside a focused field, then closes", () => {
  const close = vi.fn();
  render(<Sheet open title="Add a preview" onClose={close}><input aria-label="Address" /></Sheet>);
  const dialog = screen.getByRole("dialog", { name: "Add a preview" });
  const field = screen.getByRole("textbox", { name: "Address" });
  field.focus();
  fireEvent.click(dialog);
  expect(close).not.toHaveBeenCalled();
  expect(document.activeElement).not.toBe(field);
  fireEvent.click(dialog);
  expect(close).toHaveBeenCalledTimes(1);
});

it("closes on a tap outside when nothing is being typed", () => {
  const close = vi.fn();
  render(<Sheet open title="Details" onClose={close}><button>Copy</button></Sheet>);
  screen.getByRole("button", { name: "Copy" }).focus();
  fireEvent.click(screen.getByRole("dialog", { name: "Details" }));
  expect(close).toHaveBeenCalledTimes(1);
});

it.each(["mouse", "touch"])("keeps the field text when %s focus changes before click", (pointerType) => {
  const close = vi.fn();
  render(<Sheet open title="Rename device" onClose={close}><input aria-label="Name" defaultValue="Tablet draft" /></Sheet>);
  const dialog = screen.getByRole("dialog");
  dialog.tabIndex = -1;
  const field = screen.getByRole("textbox") as HTMLInputElement;
  field.focus();
  fireEvent.pointerDown(dialog, { pointerType });
  dialog.focus();
  fireEvent.pointerUp(dialog, { pointerType });
  fireEvent.click(dialog);
  expect(close).not.toHaveBeenCalled();
  expect(field.value).toBe("Tablet draft");
  fireEvent.pointerDown(dialog, { pointerType });
  fireEvent.pointerUp(dialog, { pointerType });
  fireEvent.click(dialog);
  expect(close).toHaveBeenCalledTimes(1);
});

it("does not retain a cancelled outside gesture", () => {
  const close = vi.fn();
  render(<Sheet open title="Editor" onClose={close}><textarea aria-label="Text" /></Sheet>);
  const dialog = screen.getByRole("dialog");
  const field = screen.getByRole("textbox");
  field.focus();
  fireEvent.pointerDown(dialog, { pointerType: "touch" });
  fireEvent.pointerCancel(dialog, { pointerType: "touch" });
  field.blur();
  fireEvent.click(dialog);
  expect(close).toHaveBeenCalledTimes(1);
});

it("gives the topmost sheet the urgent slot and restores its predecessor on close", () => {
  const view = render(<><Sheet open title="Review" onClose={() => {}}>Review body</Sheet><Sheet open={false} title="Keep draft" onClose={() => {}}>Draft body</Sheet></>);
  expect(screen.getByRole("dialog", { name: "Review" }).dataset.urgentHost).toBe("true");
  view.rerender(<><Sheet open title="Review" onClose={() => {}}>Review body</Sheet><Sheet open title="Keep draft" onClose={() => {}}>Draft body</Sheet></>);
  expect(screen.getByRole("dialog", { name: "Review" }).dataset.urgentHost).toBe("false");
  expect(screen.getByRole("dialog", { name: "Keep draft" }).dataset.urgentHost).toBe("true");
  view.rerender(<><Sheet open title="Review" onClose={() => {}}>Review body</Sheet><Sheet open={false} title="Keep draft" onClose={() => {}}>Draft body</Sheet></>);
  expect(screen.getByRole("dialog", { name: "Review" }).dataset.urgentHost).toBe("true");
});

it("keeps the urgent controls outside what scrolls, and marks a footer that is the task's own Stop", () => {
  const view = render(<UrgentSlot.Provider value={() => <button>Stop all</button>}>
    <Sheet open title="Review request" onClose={() => {}} urgentExcept={{ taskId: "t1" }} footer={<button>Stop this task</button>}>Request</Sheet>
  </UrgentSlot.Provider>);
  const main = screen.getByRole("dialog").querySelector(".sheet__main")!;
  expect(main.querySelector(".sheet__header")?.textContent).toContain("Review request");
  expect(main.querySelector(".sheet__body")?.textContent).toBe("Request");
  expect(main.querySelector(".sheet__footer")?.classList.contains("sheet__footer--stop")).toBe(true);
  expect(main.contains(screen.getByRole("button", { name: "Stop all" }))).toBe(false);
  view.rerender(<UrgentSlot.Provider value={() => <button>Stop all</button>}>
    <Sheet open title="Close window" onClose={() => {}} urgentExcept={{ targetId: "d1" }} footer={<button>Request close</button>}>Window</Sheet>
  </UrgentSlot.Provider>);
  expect(screen.getByRole("button", { name: "Request close" }).closest(".sheet__footer")?.classList.contains("sheet__footer--stop")).toBe(false);
});

function Pane({ id, active = true, visible = true, split = true, children }: {
  id: string; active?: boolean; visible?: boolean; split?: boolean; children: ReactNode;
}) {
  const [root, setRoot] = useState<HTMLElement | null>(null);
  return <section ref={setRoot} data-testid={`${id}-pane`} hidden={!visible}>
    <PaneContext.Provider value={{ id, active, visible, split, root, width: 500, height: 800 }}>
      <div data-testid={`${id}-background`}>{children}</div>
    </PaneContext.Provider>
  </section>;
}

it("contains two nonmodal sheets and their Stop controls in independent panes", () => {
  const closeLeft = vi.fn(), closeRight = vi.fn();
  const tree = (left: boolean, global = false) => <UrgentSlot.Provider value={() => <button>Stop all</button>}>
    <button>Global safety control</button>
    <Pane id="left"><button>Left background</button><Sheet open={left} title="Left sheet" onClose={closeLeft}>Left body</Sheet></Pane>
    <Pane id="right" active={false}><button>Right background</button><Sheet open title="Right sheet" onClose={closeRight}>Right body</Sheet></Pane>
    <Sheet open={global} title="Global error" onClose={() => {}}>Error</Sheet>
  </UrgentSlot.Provider>;
  const view = render(tree(true));
  const left = screen.getByRole("dialog", { name: "Left sheet" });
  const right = screen.getByRole<HTMLDialogElement>("dialog", { name: "Right sheet" });
  expect(HTMLDialogElement.prototype.showModal).not.toHaveBeenCalled();
  // Directly in its pane, through a host that draws no box of its own.
  for (const [dialog, pane] of [[left, "left-pane"], [right, "right-pane"]] as const) {
    expect(dialog.parentElement?.className).toBe("sheet-host");
    expect(dialog.parentElement?.parentElement).toBe(screen.getByTestId(pane));
  }
  expect(left.getAttribute("aria-modal")).not.toBe("true");
  expect(right.getAttribute("aria-modal")).not.toBe("true");
  expect(screen.getByTestId("left-background").hasAttribute("inert")).toBe(true);
  expect(screen.getByTestId("right-background").hasAttribute("inert")).toBe(true);
  expect(left.hasAttribute("inert")).toBe(false);
  expect(right.hasAttribute("inert")).toBe(false);
  expect(left.dataset.urgentHost).toBe("true");
  expect(right.dataset.urgentHost).toBe("true");
  expect(within(left).getByRole("button", { name: "Stop all" }).closest("[inert]")).toBeNull();
  expect(within(right).getByRole("button", { name: "Stop all" }).closest("[inert]")).toBeNull();
  const safety = screen.getByRole("button", { name: "Global safety control" });
  safety.focus();
  expect(document.activeElement).toBe(safety);
  view.rerender(tree(true, true));
  expect(HTMLDialogElement.prototype.showModal).toHaveBeenCalledOnce();
  expect(screen.getByRole("dialog", { name: "Global error" }).dataset.urgentHost).toBe("true");
  expect(left.dataset.urgentHost).toBe("false");
  expect(right.dataset.urgentHost).toBe("false");
  view.rerender(tree(false));
  expect(screen.getByTestId("left-background").hasAttribute("inert")).toBe(false);
  expect(screen.getByTestId("right-background").hasAttribute("inert")).toBe(true);
  expect(right.open).toBe(true);
  expect(right.dataset.urgentHost).toBe("true");
  expect(closeLeft).not.toHaveBeenCalled();
  expect(closeRight).not.toHaveBeenCalled();
});

it("switches active sheets for Back without dismissing or focusing the other pane", () => {
  function Side({ id, active }: { id: string; active: boolean }) {
    const [open, setOpen] = useState(true);
    return <Pane id={id} active={active}><Sheet open={open} title={`${id} sheet`} onClose={() => setOpen(false)}><button>{id} action</button></Sheet></Pane>;
  }
  const tree = (active: string) => <><Side id="left" active={active === "left"} /><Side id="right" active={active === "right"} /></>;
  setActiveBackPane("left");
  const view = render(tree("left"));
  const rightAction = screen.getByRole("button", { name: "right action" });
  rightAction.focus();
  act(() => { view.rerender(tree("right")); setActiveBackPane("right"); });
  expect(document.activeElement).toBe(rightAction);
  act(() => { expect(window.systemBack?.()).toBe(true); });
  expect(screen.queryByRole("dialog", { name: "right sheet" })).toBeNull();
  expect(screen.getByRole("dialog", { name: "left sheet" })).toBeTruthy();
  // The remaining inactive sheet must not trap another Back.
  expect(window.systemBack?.()).toBe(false);
  act(() => { view.rerender(tree("left")); setActiveBackPane("left"); });
  act(() => { expect(window.systemBack?.()).toBe(true); });
  expect(screen.queryByRole("dialog", { name: "left sheet" })).toBeNull();
});

it("retains a hidden pane's sheet and field without stealing focus when it returns", () => {
  const close = vi.fn();
  const tree = (visible: boolean) => <>
    <Pane id="left" active={false} visible={visible}>
      <Sheet open title="Kept draft" onClose={close}><input aria-label="Draft" defaultValue="Original" /></Sheet>
    </Pane>
    <Pane id="right"><button>Other pane action</button></Pane>
  </>;
  const view = render(tree(true));
  fireEvent.change(screen.getByRole("textbox", { name: "Draft" }), { target: { value: "Unsent text" } });
  const other = screen.getByRole("button", { name: "Other pane action" });
  other.focus();
  view.rerender(tree(false));
  expect(screen.queryByRole("dialog", { name: "Kept draft" })).toBeNull();
  expect(screen.getByTestId("left-background").hasAttribute("inert")).toBe(false);
  expect(window.systemBack?.()).toBe(false);
  view.rerender(tree(true));
  expect(screen.getByRole<HTMLInputElement>("textbox", { name: "Draft" }).value).toBe("Unsent text");
  expect(document.activeElement).toBe(other);
  expect(close).not.toHaveBeenCalled();
});

it("restores a pane opener only when closing has not moved focus elsewhere", () => {
  function Editor() {
    const [open, setOpen] = useState(false);
    return <Pane id="left"><button onClick={() => setOpen(true)}>Open editor</button>
      <Sheet open={open} title="Editor" onClose={() => setOpen(false)}><input aria-label="Text" /></Sheet>
    </Pane>;
  }
  render(<><Editor /><Pane id="right" active={false}><button>Elsewhere</button></Pane></>);
  const opener = screen.getByRole("button", { name: "Open editor" });
  opener.focus();
  fireEvent.click(opener);
  expect(document.activeElement).toBe(screen.getByRole("heading", { name: "Editor" }));
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  expect(document.activeElement).toBe(opener);
  fireEvent.click(opener);
  const elsewhere = screen.getByRole("button", { name: "Elsewhere" });
  elsewhere.focus();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  expect(document.activeElement).toBe(elsewhere);
});

it("allows Escape only in the active pane's top sheet and preserves lower inert state", () => {
  // jsdom does not enforce inert, so model the browser refusing focus in it.
  const focus = HTMLElement.prototype.focus;
  vi.spyOn(HTMLElement.prototype, "focus").mockImplementation(function (this: HTMLElement, options?: FocusOptions) {
    if (!this.closest("[inert]")) focus.call(this, options);
  });
  const closeLower = vi.fn(), closeUpper = vi.fn();
  const tree = (upper: boolean, active = true) => <Pane id="left" active={active}>
    <Sheet open title="Lower" onClose={closeLower}>Lower content</Sheet>
    <Sheet open={upper} title="Upper" onClose={closeUpper}>Upper content</Sheet>
  </Pane>;
  const view = render(tree(true));
  const lower = screen.getByRole("dialog", { name: "Lower" });
  const upper = screen.getByRole("dialog", { name: "Upper" });
  expect(document.activeElement).toBe(screen.getByRole("heading", { name: "Upper" }));
  expect(lower.hasAttribute("inert")).toBe(true);
  fireEvent.keyDown(lower, { key: "Escape" });
  expect(closeLower).not.toHaveBeenCalled();
  fireEvent.keyDown(upper, { key: "Escape" });
  expect(closeUpper).toHaveBeenCalledOnce();
  view.rerender(tree(true, false));
  fireEvent.keyDown(upper, { key: "Escape" });
  expect(closeUpper).toHaveBeenCalledOnce();
  view.rerender(tree(false));
  expect(lower.hasAttribute("inert")).toBe(false);
  expect(screen.getByTestId("left-background").hasAttribute("inert")).toBe(true);
  fireEvent.keyDown(lower, { key: "Escape" });
  expect(closeLower).toHaveBeenCalledOnce();
});

it("keeps native modal behavior when a pane is not split", () => {
  render(<Pane id="left" split={false}><Sheet open title="Single view" onClose={() => {}}>Body</Sheet></Pane>);
  expect(HTMLDialogElement.prototype.showModal).toHaveBeenCalledOnce();
  expect(screen.getByRole("dialog").classList.contains("pane-sheet")).toBe(false);
  expect(screen.getByTestId("left-background").hasAttribute("inert")).toBe(false);
});

it("still obeys a hidden screen inside a visible pane", () => {
  const close = vi.fn();
  render(<Pane id="left"><SurfaceActive.Provider value={false}>
    <Sheet open title="Old screen sheet" onClose={close}>Hidden</Sheet>
  </SurfaceActive.Provider></Pane>);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(close).toHaveBeenCalledOnce();
});

it("unmounts one pane without clearing another pane's sheet or pre-existing inert state", () => {
  const tree = (left: boolean) => <>
    {left && <Pane id="left"><Sheet open title="Left" onClose={() => {}}>Left body</Sheet></Pane>}
    <Pane id="right" active={false}>
      <div inert data-testid="already-inert">Protected background</div>
      <Sheet open title="Right" onClose={() => {}}>Right body</Sheet>
    </Pane>
  </>;
  const view = render(tree(true));
  view.rerender(tree(false));
  expect(screen.queryByRole("dialog", { name: "Left" })).toBeNull();
  expect(screen.getByRole("dialog", { name: "Right" }).dataset.urgentHost).toBe("true");
  expect(screen.getByTestId("right-background").hasAttribute("inert")).toBe(true);
  view.unmount();
  expect(document.querySelector(".pane-sheet[open]")).toBeNull();
});

it.each([
  { split: false, first: "Global", action: "Back" },
  { split: false, first: "Pane", action: "Back" },
  { split: true, first: "Global", action: "Back" },
  { split: true, first: "Pane", action: "Back" },
  { split: false, first: "Global", action: "Stop" },
  { split: false, first: "Pane", action: "Stop" },
  { split: true, first: "Global", action: "Stop" },
  { split: true, first: "Pane", action: "Stop" },
])("keeps $action and the Stop host on the actual top sheet (split=$split, $first opens first)", ({ split, first, action }) => {
  // Model the browser's native top layer independently from Sheet's registry.
  // Contained split sheets never enter it; showModal adds above older modals.
  const nativeModals: HTMLDialogElement[] = [];
  HTMLDialogElement.prototype.showModal = vi.fn(function (this: HTMLDialogElement) {
    this.open = true;
    nativeModals.push(this);
  });
  HTMLDialogElement.prototype.close = function () {
    this.open = false;
    const index = nativeModals.indexOf(this);
    if (index !== -1) nativeModals.splice(index, 1);
  };
  const closed = vi.fn(), stopped = vi.fn();
  function Pair() {
    const [globalOpen, setGlobalOpen] = useState(false), [paneOpen, setPaneOpen] = useState(false);
    return <UrgentSlot.Provider value={({ dismiss }) => <button onClick={() => { stopped(); dismiss?.(); }}>Stop all</button>}>
      <button onClick={() => setGlobalOpen(true)}>Open Global</button>
      <button onClick={() => setPaneOpen(true)}>Open Pane</button>
      <Sheet open={globalOpen} title="Global" onClose={() => { closed("Global"); setGlobalOpen(false); }}>App-wide issue</Sheet>
      <Pane id="left" split={split}>
        <Sheet open={paneOpen} title="Pane" onClose={() => { closed("Pane"); setPaneOpen(false); }}>Computer details</Sheet>
      </Pane>
    </UrgentSlot.Provider>;
  }
  setActiveBackPane("left");
  render(<Pair />);
  const second = first === "Global" ? "Pane" : "Global";
  fireEvent.click(screen.getByRole("button", { name: `Open ${first}` }));
  // Opening the second sheet can also be an asynchronous response, so it is
  // allowed even while the first native modal makes its opener inaccessible.
  fireEvent.click(screen.getByRole("button", { name: `Open ${second}` }));
  const expectedTop = split ? "Global" : second;
  const expectedLower = expectedTop === "Global" ? "Pane" : "Global";
  const top = screen.getByRole<HTMLDialogElement>("dialog", { name: expectedTop });
  const lower = screen.getByRole<HTMLDialogElement>("dialog", { name: expectedLower });
  expect(nativeModals.at(-1)).toBe(top);
  expect(nativeModals).toHaveLength(split ? 1 : 2);
  expect(top.dataset.urgentHost).toBe("true");
  expect(lower.dataset.urgentHost).toBe("false");
  expect(document.querySelectorAll('dialog[open][data-urgent-host="true"]')).toHaveLength(1);
  if (action === "Back") act(() => { expect(window.systemBack?.()).toBe(true); });
  else fireEvent.click(within(top).getByRole("button", { name: "Stop all" }));
  expect(closed.mock.calls).toEqual([[expectedTop]]);
  expect(stopped).toHaveBeenCalledTimes(action === "Stop" ? 1 : 0);
  expect(top.open).toBe(false);
  expect(lower.open).toBe(true);
  expect(lower.dataset.urgentHost).toBe("true");
  expect(nativeModals.at(-1)).toBe(split ? undefined : lower);
  act(() => { expect(window.systemBack?.()).toBe(true); });
  expect(closed.mock.calls).toEqual([[expectedTop], [expectedLower]]);
  expect(nativeModals).toHaveLength(0);
  expect(window.systemBack?.()).toBe(false);
});

it("keeps what is typed in a sheet, and the field's focus, when the window folds into one view and widens again", () => {
  const mounted = vi.fn();
  function Editor() {
    const [name, setName] = useState("Unit tests");
    useEffect(() => { mounted(); }, []);
    return <input aria-label="Name" value={name} onChange={(e) => setName(e.target.value)} />;
  }
  const tree = (split: boolean) => <Pane id="left" split={split}><Sheet open title="Edit check" onClose={() => {}}><Editor /></Sheet></Pane>;
  const view = render(tree(true));
  const field = screen.getByRole<HTMLInputElement>("textbox", { name: "Name" });
  fireEvent.change(field, { target: { value: "Unit tests, not saved" } });
  field.focus();

  view.rerender(tree(false));
  const dialog = screen.getByRole<HTMLDialogElement>("dialog", { name: "Edit check" });
  expect(HTMLDialogElement.prototype.showModal).toHaveBeenCalledOnce();
  expect(dialog.classList.contains("pane-sheet")).toBe(false);
  // Back in its own place in the screen, as a native modal is.
  expect(screen.getByTestId("left-background").contains(dialog)).toBe(true);
  expect(screen.getByRole<HTMLInputElement>("textbox", { name: "Name" })).toBe(field);
  expect(field.value).toBe("Unit tests, not saved");
  expect(document.activeElement).toBe(field);

  view.rerender(tree(true));
  expect(dialog.classList.contains("pane-sheet")).toBe(true);
  expect(dialog.parentElement?.parentElement).toBe(screen.getByTestId("left-pane"));
  expect(screen.getByRole<HTMLInputElement>("textbox", { name: "Name" }).value).toBe("Unit tests, not saved");
  expect(document.activeElement).toBe(field);
  expect(mounted).toHaveBeenCalledOnce();
});

/** Model the browser's top layer and the taps and focus it blocks. */
function modelTopLayer(): HTMLDialogElement[] {
  const nativeModals: HTMLDialogElement[] = [];
  HTMLDialogElement.prototype.showModal = vi.fn(function (this: HTMLDialogElement) {
    this.open = true;
    nativeModals.push(this);
  });
  HTMLDialogElement.prototype.close = function () {
    this.open = false;
    const index = nativeModals.indexOf(this);
    if (index !== -1) nativeModals.splice(index, 1);
  };
  // A modal makes everything outside the topmost one inert, as does [inert];
  // nothing inside a closed dialog is drawn, so nothing there takes focus.
  const focus = HTMLElement.prototype.focus;
  vi.spyOn(HTMLElement.prototype, "focus").mockImplementation(function (this: HTMLElement, options?: FocusOptions) {
    const top = nativeModals.at(-1);
    const hidden = (el: Element | null): boolean => !!el && ((el instanceof HTMLDialogElement && !el.open) || hidden(el.parentElement));
    if (!this.closest("[inert]") && !hidden(this) && (!top || top.contains(this))) focus.call(this, options);
  });
  return nativeModals;
}

it.each([2, 3])("keeps %i sheets opened one inside another stacked in that order through split, single and split view", (depth) => {
  const nativeModals = modelTopLayer();
  const names = ["Safety", "Force stop", "Really force stop"].slice(0, depth);
  const closed = vi.fn();
  function Level({ i, opens, setOpen }: { i: number; opens: boolean[]; setOpen: (i: number, open: boolean) => void }) {
    return <Sheet open={!!opens[i]} title={names[i]} onClose={() => { closed(names[i]); setOpen(i, false); }}>
      <button>{names[i]} action</button>
      {i + 1 < depth && <><button onClick={() => setOpen(i + 1, true)}>Open {names[i + 1]}</button><Level i={i + 1} opens={opens} setOpen={setOpen} /></>}
    </Sheet>;
  }
  function Screen({ split }: { split: boolean }) {
    const [opens, set] = useState(names.map(() => false));
    const setOpen = (i: number, open: boolean) => set((now) => now.map((was, j) => j === i ? open : was));
    return <Pane id="left" split={split}><button onClick={() => setOpen(0, true)}>Open {names[0]}</button><Level i={0} opens={opens} setOpen={setOpen} /></Pane>;
  }
  setActiveBackPane("left");
  const view = render(<Screen split />);
  for (const name of names) fireEvent.click(screen.getByRole("button", { name: `Open ${name}` }));
  const dialogs = names.map((name) => screen.getByRole<HTMLDialogElement>("dialog", { name }));
  const innermost = screen.getByRole("button", { name: `${names.at(-1)} action` });
  innermost.focus();
  const expectStacked = (split: boolean, open = dialogs) => {
    const top = open.at(-1)!;
    for (const dialog of open) {
      expect(dialog.open).toBe(true);
      expect(dialog.dataset.urgentHost).toBe(String(dialog === top));
    }
    if (split) {
      // Later in the pane is drawn on top; every lower sheet is inert.
      const hosts = [...screen.getByTestId("left-pane").children].filter((el) => el.className === "sheet-host");
      expect(hosts.map((host) => host.firstElementChild).filter((d) => open.includes(d as HTMLDialogElement))).toEqual(open);
      for (const dialog of open) expect(dialog.hasAttribute("inert")).toBe(dialog !== top);
      expect(nativeModals).toEqual([]);
    } else {
      expect(nativeModals).toEqual(open);
    }
  };
  expectStacked(true);
  view.rerender(<Screen split={false} />);
  expectStacked(false);
  expect(document.activeElement).toBe(innermost);
  view.rerender(<Screen split />);
  expectStacked(true);
  expect(document.activeElement).toBe(innermost);
  // Escape closes only the top sheet, which can still be used.
  fireEvent.keyDown(innermost, { key: "Escape" });
  expect(closed.mock.calls).toEqual([[names.at(-1)]]);
  expectStacked(true, dialogs.slice(0, -1));
  view.rerender(<Screen split={false} />);
  expectStacked(false, dialogs.slice(0, -1));
  view.rerender(<Screen split />);
  view.rerender(<Screen split={false} />);
  // System Back closes the top sheet too, and the browser sends cancel
  // (Escape) only to the topmost modal.
  act(() => { expect(window.systemBack?.()).toBe(true); });
  expect(closed.mock.calls).toEqual([[names.at(-1)], [names.at(-2)]]);
  const rest = dialogs.slice(0, -2);
  if (rest.length) {
    expectStacked(false, rest);
    fireEvent(nativeModals.at(-1)!, new Event("cancel", { cancelable: true }));
    expect(closed.mock.calls.at(-1)).toEqual([names[0]]);
  }
  expect(nativeModals).toEqual([]);
});

it("puts sheets in a pane in the order they were opened, not mounted", () => {
  const nativeModals = modelTopLayer();
  const tree = (split: boolean, first: boolean) => <Pane id="left" split={split}>
    <Sheet open={first} title="First" onClose={() => {}}><button>First action</button></Sheet>
    <Sheet open title="Second" onClose={() => {}}><button>Second action</button></Sheet>
  </Pane>;
  const view = render(tree(true, false));
  view.rerender(tree(true, true));
  const first = screen.getByRole("dialog", { name: "First" });
  const second = screen.getByRole("dialog", { name: "Second" });
  expect(first.compareDocumentPosition(second) & Node.DOCUMENT_POSITION_PRECEDING).toBeTruthy();
  expect(first.dataset.urgentHost).toBe("true");
  expect(second.hasAttribute("inert")).toBe(true);
  expect(first.hasAttribute("inert")).toBe(false);
  view.rerender(tree(false, true));
  expect(nativeModals).toEqual([second, first]);
  // A sheet that opens while the window is folding still opens on top.
  view.rerender(tree(true, false));
  view.rerender(tree(false, true));
  expect(nativeModals).toEqual([second, first]);
  expect(first.dataset.urgentHost).toBe("true");
});

it("opens a sheet and the confirmation inside it in that order when both open at once", () => {
  const nativeModals = modelTopLayer();
  const tree = (split: boolean) => <Pane id="left" split={split}>
    <Sheet open title="Safety" onClose={() => {}}><Sheet open title="Force stop" onClose={() => {}}><button>Stop now</button></Sheet></Sheet>
  </Pane>;
  render(tree(false));
  const safety = screen.getByRole("dialog", { name: "Safety" });
  const stop = screen.getByRole("dialog", { name: "Force stop" });
  expect(nativeModals).toEqual([safety, stop]);
  expect(stop.dataset.urgentHost).toBe("true");
  expect(safety.dataset.urgentHost).toBe("false");
});

it("keeps a sheet where it was scrolled to when the window folds, scrolling only to a field being typed in that is out of sight", () => {
  const scrolledTo = vi.fn();
  Element.prototype.scrollIntoView = scrolledTo;
  const tree = (split: boolean) => <Pane id="left" split={split}><Sheet open title="Long sheet" onClose={() => {}}>
    <input aria-label="Name" /><p>Long text</p>
  </Sheet></Pane>;
  const view = render(tree(true));
  const heading = screen.getByRole("heading", { name: "Long sheet" });
  const body = screen.getByRole("dialog").querySelector<HTMLElement>(".sheet__body")!;
  // Opening focuses the heading; the person then scrolls down to read.
  expect(document.activeElement).toBe(heading);
  body.scrollTop = 800;
  view.rerender(tree(false));
  expect(document.activeElement).toBe(heading);
  expect(body.scrollTop).toBe(800);
  expect(scrolledTo).not.toHaveBeenCalled();
  // A field being typed in that is still in sight is left where it is.
  const field = screen.getByRole("textbox", { name: "Name" });
  field.focus();
  view.rerender(tree(true));
  expect(body.scrollTop).toBe(800);
  expect(scrolledTo).not.toHaveBeenCalled();
  // One the narrower view has pushed out of sight is brought back.
  vi.spyOn(field, "getBoundingClientRect").mockReturnValue(new DOMRect(0, window.innerHeight + 40, 200, 30));
  view.rerender(tree(false));
  expect(document.activeElement).toBe(field);
  expect(scrolledTo).toHaveBeenCalledOnce();
  expect(scrolledTo.mock.contexts[0]).toBe(field);
});

it("closes only the confirmation when Escape cancels it inside another sheet", () => {
  const nativeModals = modelTopLayer();
  const closed = vi.fn();
  render(<Sheet open title="Safety" onClose={() => closed("Safety")}>
    <Sheet open title="Force stop" onClose={() => closed("Force stop")}><button>Stop now</button></Sheet>
  </Sheet>);
  fireEvent(nativeModals.at(-1)!, new Event("cancel", { cancelable: true }));
  expect(closed.mock.calls).toEqual([["Force stop"]]);
});

it("returns focus to a pane sheet's opener when the sheet is removed while open", () => {
  function Editor() {
    const [shown, setShown] = useState(false);
    return <Pane id="left"><button onClick={() => setShown(true)}>Open editor</button>
      {shown && <Sheet open title="Editor" onClose={() => setShown(false)}><input aria-label="Text" /></Sheet>}
    </Pane>;
  }
  render(<Editor />);
  const opener = screen.getByRole("button", { name: "Open editor" });
  opener.focus();
  fireEvent.click(opener);
  screen.getByRole("textbox", { name: "Text" }).focus();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(opener);
});
