// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Toasts } from "../design/Toast";
import { SETTLE_MS } from "../host/expected";
import { CHANGING_AFTER_MS, useCommandControl, type CommandControl, type ControlKind } from "./commandControl";

const note = (what: string) => ({ label: `Change ${what}`, what });
const tick = async (ms = 0) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

interface Deferred { resolve: () => void; reject: (e: unknown) => void; value: boolean }
let sent: Deferred[];
let control: CommandControl<boolean>;
let computer: boolean;
let rerender: () => void;

function Probe({ kind, value }: { kind: ControlKind; value: boolean }) {
  control = useCommandControl({
    kind,
    value,
    describe: (e) => (e instanceof Error ? e.message : "Something went wrong."),
    send: (next) => new Promise<void>((resolve, reject) => { sent.push({ resolve, reject, value: next }); }),
  });
  return <><p>{`shown ${control.value}`}</p>{control.changing !== null && <p>Changing…</p>}<Toasts /></>;
}
function mount(kind: ControlKind) {
  computer = false;
  const view = render(<Probe kind={kind} value={computer} />);
  rerender = () => view.rerender(<Probe kind={kind} value={computer} />);
}
/** The computer takes the change, and its state then shows it. */
async function accept(i: number) {
  await act(async () => { sent[i]!.resolve(); });
  computer = sent[i]!.value;
  rerender();
  await tick();
}

beforeEach(() => { sent = []; vi.useFakeTimers(); });
afterEach(() => { cleanup(); vi.clearAllTimers(); vi.useRealTimers(); });

it("an instant control shows the choice on the tap and keeps it once the computer has it", async () => {
  mount("instant");
  act(() => { void control.set(true, note("questions alerts")); });
  expect(screen.getByText("shown true")).toBeTruthy();
  expect(sent.map((s) => s.value)).toEqual([true]);
  await accept(0);
  expect(screen.getByText("shown true")).toBeTruthy();
  expect(screen.queryByRole("status")).toBeNull();
});

it("a refusal puts back what the computer has and names what didn't change", async () => {
  mount("instant");
  act(() => { void control.set(true, note("questions alerts")); });
  await act(async () => { sent[0]!.reject(new Error("It changed on the computer.")); });
  expect(screen.getByText("shown false")).toBeTruthy();
  expect(screen.getByText("Couldn’t change questions alerts. It changed on the computer.")).toBeTruthy();
});

it("taps made while one is on its way go after it, as the latest choice", async () => {
  mount("instant");
  act(() => { void control.set(true, note("a")); });
  act(() => { void control.set(false, note("a")); });
  act(() => { void control.set(true, note("a")); });
  expect(screen.getByText("shown true")).toBeTruthy();
  // One on its way; the rest wait for it, and only the latest goes.
  expect(sent).toHaveLength(1);
  await act(async () => { sent[0]!.resolve(); });
  expect(sent.map((s) => s.value)).toEqual([true, true]);
  await accept(1);
  expect(screen.getByText("shown true")).toBeTruthy();
});

it("a change the computer has but its state doesn't yet show is kept until the state catches up, or for a while", async () => {
  mount("instant");
  act(() => { void control.set(true, note("a")); });
  await act(async () => { sent[0]!.resolve(); });
  // Nothing flickers back between the answer and the computer's state.
  expect(screen.getByText("shown true")).toBeTruthy();
  await tick(SETTLE_MS);
  expect(screen.getByText("shown false")).toBeTruthy();
});

it("a safety control shows what is in force, and Changing… once the change takes a moment", async () => {
  mount("safety");
  act(() => { void control.set(true, note("the internet")); });
  expect(screen.getByText("shown false")).toBeTruthy();
  expect(screen.queryByText("Changing…")).toBeNull();
  expect(control.pending).toBe(true);
  await tick(CHANGING_AFTER_MS);
  expect(screen.getByText("Changing…")).toBeTruthy();
  expect(screen.getByText("shown false")).toBeTruthy();
  await accept(0);
  expect(screen.getByText("shown true")).toBeTruthy();
  expect(screen.queryByText("Changing…")).toBeNull();
  expect(control.pending).toBeNull();
});

it("a refused safety change never shows as in force", async () => {
  mount("safety");
  act(() => { void control.set(true, note("the internet")); });
  await tick(CHANGING_AFTER_MS);
  await act(async () => { sent[0]!.reject(new Error("Not sent.")); });
  expect(screen.getByText("shown false")).toBeTruthy();
  expect(screen.queryByText("Changing…")).toBeNull();
  expect(screen.getByText("Couldn’t change the internet. Not sent.")).toBeTruthy();
});

it("says whether the latest choice was taken, and an instant control can show its own as waiting", async () => {
  mount("instant");
  let first!: Promise<boolean>, second!: Promise<boolean>;
  act(() => { first = control.set(true, note("a")); });
  await tick(CHANGING_AFTER_MS);
  expect(control.waiting).toBe(true);
  expect(control.changing).toBeNull();
  act(() => { second = control.set(false, { ...note("a"), done: "It changed" }); });
  await act(async () => { sent[0]!.resolve(); });
  await act(async () => { sent[1]!.resolve(); });
  // Both learn the latest went; it is confirmed once.
  await expect(first).resolves.toBe(true);
  await expect(second).resolves.toBe(true);
  expect(screen.getAllByText("It changed")).toHaveLength(1);

  let refused!: Promise<boolean>;
  act(() => { refused = control.set(true, note("b")); });
  await act(async () => { sent[2]!.reject(new Error("No.")); });
  await expect(refused).resolves.toBe(false);
  expect(control.waiting).toBeNull();
});
