// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createRef } from "react";
import { LiveKeyboard, type LiveKeyboardHandle } from "./LiveKeyboard";
import type { InputAction } from "../../protocol/types";

let send = vi.fn<(action: InputAction, signal: AbortSignal) => Promise<boolean>>();
const close = vi.fn();
const show = () => render(<LiveKeyboard targetId="display" enabled send={send} onClose={close} />);
const input = () => screen.getByRole("textbox", { name: "Type on the computer" }) as HTMLTextAreaElement;
const edit = (value: string, inputType = "insertText", isComposing = false) => fireEvent.input(input(), { target: { value }, inputType, isComposing });
const flush = async () => act(async () => { await Promise.resolve(); await Promise.resolve(); });
const sent = () => send.mock.calls.map(([action]) => action);
beforeEach(() => { sessionStorage.clear(); localStorage.clear(); send = vi.fn(async () => true); close.mockClear(); });
afterEach(cleanup);

it("focuses the phone keyboard with suggestions off and no composer or send button", () => {
  show(); expect(document.activeElement).toBe(input());
  expect(input().autocomplete).toBe("off"); expect(input().getAttribute("autocorrect")).toBe("off");
  expect(input().getAttribute("autocapitalize")).toBe("off"); expect(input().getAttribute("spellcheck")).toBe("false");
  expect(screen.queryByRole("button", { name: "Send text" })).toBeNull();
  expect(screen.queryByRole("combobox")).toBeNull();
});
it("sends live input in acknowledgment order without storing it", async () => {
  let done!: (ok: boolean) => void;
  send.mockImplementationOnce(() => new Promise(resolve => { done = resolve; }));
  show(); edit("a"); edit("ab"); edit("abc");
  expect(sent()).toEqual([{ action: "text", text: "a" }]);
  await act(async () => done(true)); await flush();
  expect(sent()).toEqual(["a", "b", "c"].map(text => ({ action: "text", text })));
  expect(localStorage.length).toBe(0); expect(sessionStorage.length).toBe(0);
});
it("discards queued edits after refusal instead of replaying them", async () => {
  let done!: (ok: boolean) => void;
  send.mockImplementationOnce(() => new Promise(resolve => { done = resolve; }));
  show(); edit("a"); edit("ab");
  await act(async () => done(false));
  expect(sent()).toEqual([{ action: "text", text: "a" }]);
  expect(input().value).toBe(""); expect(screen.getByText(/Typing stopped/)).toBeTruthy();
  edit("c"); await flush(); expect(sent().at(-1)).toEqual({ action: "text", text: "c" });
});
it("waits for composition to commit, including Japanese and emoji", async () => {
  show(); fireEvent.compositionStart(input()); edit("に", "insertCompositionText", true);
  edit("日本語 👩🏽‍💻", "insertCompositionText", true); expect(sent()).toEqual([]);
  fireEvent.compositionEnd(input(), { data: "日本語 👩🏽‍💻" });
  edit("日本語 👩🏽‍💻", "insertFromComposition"); await flush();
  expect(sent()).toEqual([{ action: "text", text: "日本語 👩🏽‍💻" }]);
});
it("applies a committed ASCII correction only after the original text was acknowledged", async () => {
  let done!: (ok: boolean) => void;
  send.mockImplementationOnce(() => new Promise(resolve => { done = resolve; }));
  show(); edit("teh"); edit("the", "insertReplacementText");
  expect(send).toHaveBeenCalledTimes(1);
  await act(async () => done(true)); await flush();
  expect(sent()).toEqual([{ action: "text", text: "teh" }, { action: "key_chord", keys: ["Backspace"] }, { action: "key_chord", keys: ["Backspace"] }, { action: "text", text: "he" }]);
});
it("does not guess how many backspaces remove an emoji correction", async () => {
  show(); edit("👩🏽‍💻"); await flush(); edit("👨‍💻", "insertReplacementText");
  expect(send).toHaveBeenCalledTimes(1); expect(screen.getByText(/cannot be applied safely/)).toBeTruthy();
});
it("Backspace and Enter are live keys, never local-only edits", async () => {
  show(); fireEvent.keyDown(input(), { key: "Backspace" }); fireEvent.keyDown(input(), { key: "Enter" }); await flush();
  expect(sent()).toEqual([{ action: "key_chord", keys: ["Backspace"] }, { action: "key_chord", keys: ["Enter"] }]);
});
it("handles native mobile beforeinput for Backspace and Enter without duplicate input", async () => {
  show();
  act(() => { input().dispatchEvent(new InputEvent("beforeinput", { inputType: "deleteContentBackward", bubbles: true, cancelable: true })); });
  act(() => { input().dispatchEvent(new InputEvent("beforeinput", { inputType: "insertLineBreak", bubbles: true, cancelable: true })); });
  await flush(); expect(sent()).toEqual([{ action: "key_chord", keys: ["Backspace"] }, { action: "key_chord", keys: ["Enter"] }]);
});
it("one tap applies Super to a key typed on the phone, then clears it", async () => {
  show(); fireEvent.click(screen.getByRole("button", { name: "Super" })); edit("b"); await flush();
  expect(sent()).toEqual([{ action: "key_chord", keys: ["super", "b"] }]);
  expect(screen.getByRole("button", { name: "Super" }).getAttribute("aria-pressed")).toBe("false");
  edit("c"); await flush(); expect(sent().at(-1)).toEqual({ action: "text", text: "c" });
});
it("second modifier tap locks it until tapped again", async () => {
  show(); const ctrl = screen.getByRole("button", { name: "Ctrl" });
  fireEvent.click(ctrl); fireEvent.click(ctrl); fireEvent.keyDown(input(), { key: "c" }); fireEvent.keyDown(input(), { key: "v" }); await flush();
  expect(sent()).toEqual([{ action: "key_chord", keys: ["ctrl", "c"] }, { action: "key_chord", keys: ["ctrl", "v"] }]);
  expect(ctrl.getAttribute("data-locked")).toBe("true"); fireEvent.click(ctrl); expect(ctrl.getAttribute("aria-pressed")).toBe("false");
});
it("does not turn an IME phrase into a shortcut or unmodified text", () => {
  show(); fireEvent.click(screen.getByRole("button", { name: "Super" }));
  fireEvent.compositionStart(input()); edit("hello", "insertCompositionText", true); fireEvent.compositionEnd(input());
  expect(sent()).toEqual([]); expect(screen.getByText(/shortcut needs one key/)).toBeTruthy();
});
it("offers explicit long-text paste without saving or silently overwriting a clipboard", async () => {
  show(); fireEvent.click(screen.getByRole("button", { name: "Paste" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Text to paste" }), { target: { value: "long text" } });
  expect(send).not.toHaveBeenCalled(); fireEvent.click(screen.getByRole("button", { name: "Paste on computer" })); await flush();
  expect(sent()).toEqual([{ action: "paste", text: "long text" }]); expect(sessionStorage.length).toBe(0);
});
it("cancels pending typing and its replacement baseline when the desktop is touched", async () => {
  let done!: (ok: boolean) => void;
  send.mockImplementationOnce(() => new Promise(resolve => { done = resolve; }));
  show(); edit("a"); edit("ab");
  const stage = document.createElement("div"); stage.className = "picture-stage"; document.body.append(stage);
  fireEvent.pointerDown(stage); stage.remove();
  expect(send.mock.calls[0]![1].aborted).toBe(true); expect(input().value).toBe("");
  await act(async () => done(true)); expect(send).toHaveBeenCalledTimes(1);
  edit("z"); await flush(); expect(sent().at(-1)).toEqual({ action: "text", text: "z" });
});
it("cancels an active composition on pause rather than committing it after recovery", () => {
  const view = show(); fireEvent.compositionStart(input()); edit("日本", "insertCompositionText", true);
  view.rerender(<LiveKeyboard targetId="display" enabled={false} send={send} onClose={close} />);
  view.rerender(<LiveKeyboard targetId="display" enabled send={send} onClose={close} />);
  input().value = "日本"; fireEvent.compositionEnd(input()); edit("日本", "insertFromComposition");
  expect(send).not.toHaveBeenCalled(); expect(input().value).toBe("");
});
it("cancels pending sends on unmount", () => {
  send.mockImplementation(() => new Promise(() => {}));
  const view = show(); edit("secret"); view.unmount(); expect(send.mock.calls[0]![1].aborted).toBe(true);
  expect(sessionStorage.length).toBe(0);
});
it("system Back closes locally without sending Escape", () => {
  show(); act(() => { window.systemBack?.(); }); expect(close).toHaveBeenCalledOnce(); expect(send).not.toHaveBeenCalled();
});
it("keeps special key names and sends a compact preset", async () => {
  show(); fireEvent.keyDown(input(), { key: "ArrowLeft" });
  fireEvent.click(screen.getByRole("button", { name: "More" }));
  fireEvent.click(screen.getByRole("button", { name: "PageDown" }));
  fireEvent.click(screen.getByRole("button", { name: "Super+Enter" })); await flush();
  expect(sent()).toEqual([{ action: "key_chord", keys: ["ArrowLeft"] }, { action: "key_chord", keys: ["PageDown"] }, { action: "key_chord", keys: ["super", "Enter"] }]);
});

it("explicit external insertion waits for the current ack, drops queued corrections and invalidates their baseline", async () => {
  let done!: (ok: boolean) => void;
  send.mockImplementationOnce(() => new Promise(resolve => { done = resolve; }));
  const bridge = createRef<LiveKeyboardHandle>();
  render(<LiveKeyboard bridge={bridge} targetId="display" enabled send={send} onClose={close} />);
  edit("teh"); edit("the", "insertReplacementText");
  act(() => { expect(bridge.current!.insert({ action: "text", text: "123456" })).toBe(true); });
  expect(sent()).toEqual([{ action: "text", text: "teh" }]);
  await act(async () => done(true)); await flush();
  expect(sent()).toEqual([{ action: "text", text: "teh" }, { action: "text", text: "123456" }]);
  edit("the", "insertReplacementText"); await flush();
  expect(sent()).toHaveLength(2); expect(screen.getByText(/typing location changed/)).toBeTruthy();
});
it("an uncertain prior action also drops an explicit insertion rather than overtaking it", async () => {
  let done!: (ok: boolean) => void;
  send.mockImplementationOnce(() => new Promise(resolve => { done = resolve; }));
  const bridge = createRef<LiveKeyboardHandle>();
  render(<LiveKeyboard bridge={bridge} targetId="display" enabled send={send} onClose={close} />);
  edit("teh"); act(() => { bridge.current!.insert({ action: "text", text: "123456" }); });
  await act(async () => done(false)); await flush(); expect(send).toHaveBeenCalledTimes(1);
});
