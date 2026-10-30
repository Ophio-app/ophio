// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Sheet } from "./Sheet";
import { UrgentSlot } from "./urgent";

beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);

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
