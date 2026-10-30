// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { SpeechEvent } from "../../host/link";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import type { AnnotationDraft, AnnotationStore } from "../../state/annotations";
import { Speech } from "./Speech";
let listener: ((event: SpeechEvent) => void) | undefined;
const send = vi.fn(() => true);
const platform = { speech: () => ({ send, close: vi.fn(), listen: (l: (event: SpeechEvent) => void) => { listener = l; return () => { listener = undefined; }; } }) };
vi.mock("../../app/context", () => ({
  useApp: () => ({ platform, profile: { profileId: "phone", name: "studio" }, ui: { push: vi.fn() } }),
  useHost: <T,>(select: (s: HostState) => T) => select({
    ...emptyHostState,
    capabilities: { speech: { state: "available" } } as HostState["capabilities"],
    readiness: { checks: [{ area: "speech", label: "Speech", status: "passed", detail: null, remedy: null }] } as unknown as HostState["readiness"],
  }),
  useLink: () => ({ state: "online" }),
}));
vi.mock("../../shell/Screen", () => ({ useRoute: () => ({ active: true }) }));
const draft = { id: "d", createdAt: 0 } as AnnotationDraft;
const store = { update: vi.fn(), record: vi.fn() } as unknown as AnnotationStore;
beforeEach(() => {
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);

// crates/services/src/runtime.rs (speech_error) sends lower-case engine
// failures as SpeechServerFrame::Error; a disconnection already gets a way forward.
it("a speech failure reported by the computer says the text is kept and what to do, as a disconnection does", () => {
  render(<Speech draft={draft} store={store} recordingTarget={null} onRecording={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Speak" }));
  fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
  act(() => listener?.({ type: "state", state: { state: "online", since: 0 } }));
  act(() => listener?.({ type: "text", frame: { type: "error", message: "speech worker stopped unexpectedly" } }));
  expect(screen.getByText("Speech worker stopped unexpectedly. Your text is kept. Keep typing, or try Speak again after resolving this.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Speak" })).toBeTruthy();
});
