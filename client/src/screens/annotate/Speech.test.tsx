// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { SpeechEvent } from "../../host/link";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import type { Availability, ReadinessCheck } from "../../protocol/types";
import type { AnnotationDraft, AnnotationStore } from "../../state/annotations";
import { Speech } from "./Speech";
let online = true;
let speech: Availability | undefined;
let checks: ReadinessCheck[] = [];
const push = vi.fn();
let listener: ((event: SpeechEvent) => void) | undefined;
let connected = false;
const send = vi.fn(() => connected);
const close = vi.fn();
const platform = { speech: () => ({ send, close, listen: (l: (event: SpeechEvent) => void) => { listener = l; return () => { listener = undefined; }; } }) };
vi.mock("../../app/context", () => ({
  useApp: () => ({ platform, profile: { profileId: "phone", name: "studio" }, ui: { push } }),
  useHost: <T,>(select: (s: HostState) => T) =>
    select({
      ...emptyHostState,
      capabilities: speech ? ({ speech } as HostState["capabilities"]) : null,
      readiness: { checks } as HostState["readiness"],
    }),
  useLink: () => ({ state: online ? "online" : "offline" }),
}));
vi.mock("../../shell/Screen", () => ({ useRoute: () => ({ active: true }) }));
const draft = { id: "d", createdAt: 0 } as AnnotationDraft;
const store = { update: vi.fn(), record: vi.fn(), arrive: vi.fn() } as unknown as AnnotationStore;
const view = () => render(<Speech draft={draft} store={store} recordingTarget={null} onRecording={() => {}} />);
beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  online = true;
  speech = { state: "available" };
  checks = [{ area: "speech", label: "Speech", status: "passed", detail: null, remedy: null }];
  push.mockClear();
  send.mockClear();
  connected = false;
});
afterEach(cleanup);

it("names a missing speech model and links to its setup", () => {
  speech = { state: "permission_missing", remedy: "ophio voice install tiny.en" };
  view();
  expect(screen.getByText(/Speaking: needs setup/)).toBeTruthy();
  expect(screen.getByText(/ophio voice install tiny\.en/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Set up speech" }));
  expect(push).toHaveBeenCalledWith({ screen: "settings", section: "settings-voice" });
  expect((screen.getByRole("button", { name: "Speak" }) as HTMLButtonElement).disabled).toBe(true);
});

it("says speaking needs the computer while offline", () => {
  online = false;
  view();
  expect(screen.getByText(/Speaking needs studio to be connected/)).toBeTruthy();
});

it("shows the computer's own speech check detail until it passes", () => {
  checks = [{ area: "speech", label: "Speech", status: "failed", detail: "The recognizer did not start.", remedy: null }];
  view();
  expect(screen.getByText(/The recognizer did not start\. Keep typing\./)).toBeTruthy();
});

it("offers Speak with no notice when the computer is ready", () => {
  view();
  expect(screen.queryByText(/Keep typing/)).toBeNull();
  expect((screen.getByRole("button", { name: "Speak" }) as HTMLButtonElement).disabled).toBe(false);
});

it("starts recording only once the speech connection is online", () => {
  view();
  fireEvent.click(screen.getByRole("button", { name: "Speak" }));
  fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
  expect(send).not.toHaveBeenCalled();
  connected = true;
  act(() => listener?.({ type: "state", state: { state: "online", since: 0 } }));
  expect(send).toHaveBeenCalledWith(expect.objectContaining({ type: "start" }));
  expect(screen.queryByText(/didn’t open speech|couldn’t start/)).toBeNull();
});

it("says so when the speech connection never opens", () => {
  vi.useFakeTimers();
  try {
    view();
    fireEvent.click(screen.getByRole("button", { name: "Speak" }));
    fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
    act(() => vi.advanceTimersByTime(10_000));
    expect(screen.getByText("studio didn’t open speech. Keep typing.")).toBeTruthy();
    expect(send).not.toHaveBeenCalled();
  } finally {
    vi.useRealTimers();
  }
});

it.each(["offline", "failed"] as const)("shows the speech refusal reason (%s) and preserves a way forward", state => {
  view();
  fireEvent.click(screen.getByRole("button", { name: "Speak" }));
  fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
  act(() => listener?.({ type: "state", state: state === "offline"
    ? { state, since: 0, code: "unsupported", reason: "Speech model was removed.", retryAt: null }
    : { state, reason: "revoked", detail: "Speech access was revoked." } }));
  expect(screen.getByText(state === "offline" ? /Speech model was removed/ : /Speech access was revoked/)).toBeTruthy();
  expect(screen.getByText(/Your text is kept. Keep typing, or try Speak again/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Speak" })).toBeTruthy();
});

it("times speech from when the phone's microphone started, not from the tap", () => {
  let now = 1000;
  const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
  try {
    view();
    fireEvent.click(screen.getByRole("button", { name: "Speak" }));
    fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
    connected = true;
    act(() => listener?.({ type: "state", state: { state: "online", since: 0 } }));
    // Three seconds on the permission prompt before the first sound.
    now = 4000;
    act(() => listener?.({ type: "capturing" }));
    now = 6000;
    act(() => listener?.({ type: "text", frame: { type: "segment", segment: {
      segment_id: "s1", text: "Here", start_ms: 500, end_ms: 900, revision: 1, is_final: true,
    } } }));
    const arrive = vi.mocked(store.arrive);
    const [, change] = arrive.mock.calls.at(-1)!;
    const kept = change({
      ...draft, comments: [], marks: [], speech: { session_id: "x", audio_artifact: null, segments: [], transcript_revision: 0 },
    } as AnnotationDraft);
    expect(kept.speech?.segments.map((s) => [s.start_ms, s.end_ms])).toEqual([[4500, 4900]]);
  } finally {
    clock.mockRestore();
  }
});
