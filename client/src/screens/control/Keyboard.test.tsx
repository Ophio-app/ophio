// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { CHORDS, keyChord } from "./keys";
import { Keyboard } from "./Keyboard";

const send = vi.fn(() => true);
const onClose = vi.fn();
const onEdit = vi.fn();
const app = { profile: { profileId: "phone-keyboard-test" } } as App;
const view = (enabled = true) => render(
  <AppProvider app={app}>
    <Keyboard targetId="display-a" enabled={enabled} send={send} onClose={onClose} onEdit={onEdit} />
  </AppProvider>,
);
const keys = () => screen.getByText("Keys and shortcuts").closest("details")!;
const press = (name: string) => fireEvent.click(within(keys()).getByRole("button", { name }));

beforeEach(() => { localStorage.clear(); send.mockClear(); onClose.mockClear(); onEdit.mockClear(); send.mockReturnValue(true); });
afterEach(() => { cleanup(); });

it("sends supported shortcuts in host vocabulary with lowercase letter names", () => {
  view();
  for (const label of CHORDS) {
    press(label);
    expect(send).toHaveBeenLastCalledWith(keyChord(label));
  }
  expect(keyChord("Super+Enter")).toEqual({ action: "key_chord", keys: ["super", "Enter"] });
  expect(keyChord("Ctrl+C")).toEqual({ action: "key_chord", keys: ["ctrl", "c"] });
  expect(keyChord("Alt+Tab")).toEqual({ action: "key_chord", keys: ["alt", "Tab"] });
});

it("shows sticky modifiers and clears them after success, on demand, or on target change", () => {
  const ui = view();
  press("Ctrl");
  expect(within(keys()).getByRole("status").textContent).toContain("Next key uses Ctrl");
  expect(within(keys()).getByRole("button", { name: "Ctrl" }).getAttribute("aria-pressed")).toBe("true");
  send.mockReturnValueOnce(false);
  press("Enter");
  expect(send).toHaveBeenLastCalledWith(keyChord("Ctrl+Enter"));
  expect(within(keys()).getByRole("button", { name: "Ctrl" }).getAttribute("aria-pressed")).toBe("true");
  press("Enter");
  expect(within(keys()).queryByRole("status")).toBeNull();
  press("Alt"); press("Clear modifiers");
  expect(within(keys()).queryByRole("status")).toBeNull();
  press("Super");
  fireEvent.change(screen.getByRole("combobox", { name: "Shortcut letter or number" }), { target: { value: "B" } });
  ui.rerender(<AppProvider app={app}><Keyboard targetId="display-b" enabled send={send} onClose={onClose} /></AppProvider>);
  expect(within(keys()).queryByRole("status")).toBeNull();
  expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Shortcut letter or number" }).value).toBe("");
});

it("keeps composition and autocorrect in the local draft until explicitly sent", () => {
  view();
  const input = screen.getByRole("textbox", { name: "Text for the computer" });
  fireEvent.compositionStart(input);
  fireEvent.change(input, { target: { value: "teh" } });
  fireEvent.click(screen.getByRole("button", { name: "Send text" }));
  press("Enter");
  expect(send).not.toHaveBeenCalled();
  fireEvent.compositionEnd(input, { data: "the", target: { value: "the 🦊" } });
  expect(input).toHaveProperty("value", "the 🦊");
  fireEvent.change(input, { target: { value: "the 🦊\n" } });
  fireEvent.change(input, { target: { value: "the 🦊" } }); // local backspace
  expect(send).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Send text" }));
  expect(send).toHaveBeenCalledExactlyOnceWith({ action: "text", text: "the 🦊" });
  expect(onEdit).toHaveBeenCalled();
});

it("sends selected letter and number chords with sticky Shift, retaining a refused chord", () => {
  view();
  const choice = screen.getByRole<HTMLSelectElement>("combobox", { name: "Shortcut letter or number" });
  const submit = screen.getByRole("button", { name: "Send shortcut" });
  expect(submit.hasAttribute("disabled")).toBe(true);
  press("Super");
  fireEvent.change(choice, { target: { value: "B" } });
  send.mockReturnValueOnce(false);
  fireEvent.click(submit);
  expect(send).toHaveBeenLastCalledWith({ action: "key_chord", keys: ["super", "b"] });
  expect(choice.value).toBe("B");
  expect(within(keys()).getByRole("button", { name: "Super" }).getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(submit);
  expect(choice.value).toBe("");
  expect(within(keys()).queryByRole("status")).toBeNull();
  press("Super");
  fireEvent.change(choice, { target: { value: "1" } });
  fireEvent.click(submit);
  expect(send).toHaveBeenLastCalledWith({ action: "key_chord", keys: ["super", "1"] });
  press("Ctrl"); press("Shift");
  fireEvent.change(choice, { target: { value: "C" } });
  fireEvent.click(submit);
  expect(send).toHaveBeenLastCalledWith({ action: "key_chord", keys: ["ctrl", "shift", "c"] });
});

it("composition never sends a selected shortcut", () => {
  view();
  press("Alt");
  const input = screen.getByRole("textbox", { name: "Text for the computer" });
  const choice = screen.getByRole("combobox", { name: "Shortcut letter or number" });
  fireEvent.change(choice, { target: { value: "B" } });
  fireEvent.compositionStart(input);
  fireEvent.click(screen.getByRole("button", { name: "Send shortcut" }));
  expect(send).not.toHaveBeenCalled();
  expect(within(keys()).getByRole("status").textContent).toContain("Alt");
});

it("system Back closes the keyboard without consuming an extra Back on a focused editor", () => {
  view();
  const input = screen.getByRole("textbox", { name: "Text for the computer" });
  fireEvent.change(input, { target: { value: "draft 🦊" } });
  input.focus();
  act(() => { expect(window.systemBack?.()).toBe(true); });
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(input).toHaveProperty("value", "draft 🦊");
});
