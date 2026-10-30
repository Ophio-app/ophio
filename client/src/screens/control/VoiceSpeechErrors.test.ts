// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SpeechEvent, SpeechLink } from "../../host/link";
import { captureVoice } from "./VoiceSpeech";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

// crates/services/src/runtime.rs (speech_error) and plumbing.rs send these
// as SpeechServerFrame::Error, lower case and without a next step.
it.each([
  ["local speech model computation failed", "Local speech model computation failed."],
  ["speech session exceeds ten minutes; finish and start a new session", "Speech session exceeds ten minutes; finish and start a new session."],
  ["This device no longer has the Agent tasks permission", "This device no longer has the Agent tasks permission."],
])("a speech failure reported by the computer (%j) says what to do next, like a speech disconnection does", (message, sentence) => {
  let listener: ((event: SpeechEvent) => void) | undefined;
  const link: SpeechLink = { send: vi.fn().mockReturnValue(true), close: vi.fn(), listen: (callback) => { listener = callback; return () => { listener = undefined; }; } };
  const transcript = vi.fn(), error = vi.fn(), ended = vi.fn();
  const capture = captureVoice(link, "utterance", transcript, error, ended);
  listener?.({ type: "state", state: { state: "online", since: 1 } });
  listener?.({ type: "text", frame: { type: "segment", segment: { segment_id: "one", revision: 1, text: "click sign in", start_ms: 0, end_ms: 10, is_final: true } } });
  capture.finish();
  listener?.({ type: "text", frame: { type: "error", message } });
  expect(transcript).not.toHaveBeenCalledWith(expect.anything(), true);
  expect(error).toHaveBeenCalledExactlyOnceWith(`${sentence} Try the typed command box, or retry speech after resolving this.`);
  expect(ended).toHaveBeenCalledOnce();
});
