// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SpeechEvent, SpeechLink } from "../../host/link";
import { captureVoice } from "./VoiceSpeech";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());
function fixture() {
  let listener: ((event: SpeechEvent) => void) | undefined;
  const send = vi.fn().mockReturnValue(true);
  const close = vi.fn();
  const link: SpeechLink = { send, close, listen: callback => { listener = callback; return () => { listener = undefined; }; } };
  const transcript = vi.fn(), error = vi.fn(), ended = vi.fn();
  const capture = captureVoice(link, "utterance", transcript, error, ended);
  const emit = (event: SpeechEvent) => listener?.(event);
  const online = () => emit({ type: "state", state: { state: "online", since: 1 } });
  return { capture, send, close, transcript, error, ended, emit, online };
}
it("starts only after authentication and never retains audio", () => {
  const f = fixture(); expect(f.send).not.toHaveBeenCalled(); f.online(); f.online();
  expect(f.send).toHaveBeenCalledExactlyOnceWith({ type: "start", session_id: "utterance", language: null, keep_audio: false });
  f.capture.cancel(); expect(f.send).toHaveBeenLastCalledWith({ type: "cancel" }); expect(f.close).toHaveBeenCalledOnce();
});
it("releasing before connection prevents a late microphone start", () => {
  const f = fixture(); f.capture.finish(); f.online(); vi.advanceTimersByTime(10000);
  expect(f.send).not.toHaveBeenCalled(); expect(f.close).toHaveBeenCalledOnce(); expect(f.error).not.toHaveBeenCalled();
});
it("streams corrections and emits a final utterance once", () => {
  const f = fixture(); f.online();
  for (const [revision, text] of [[1, "click sing"], [2, "click sign in"]] as const) f.emit({ type: "text", frame: { type: "segment", segment: { segment_id: "one", revision, text, start_ms: 0, end_ms: 10, is_final: true } } });
  expect(f.transcript).toHaveBeenLastCalledWith("click sign in", false);
  f.capture.finish(); f.capture.finish();
  expect(f.send.mock.calls.filter(([value]) => value.type === "finish")).toHaveLength(1);
  f.emit({ type: "text", frame: { type: "finished", session_id: "utterance", audio_artifact: null } });
  expect(f.transcript).toHaveBeenLastCalledWith("click sign in", true);
  expect(f.transcript).toHaveBeenCalledTimes(3); expect(f.close).toHaveBeenCalledOnce();
});
it("does not send a final command after cancellation", () => {
  const f = fixture(); f.online(); f.capture.cancel();
  f.emit({ type: "text", frame: { type: "finished", session_id: "utterance", audio_artifact: null } });
  expect(f.transcript).not.toHaveBeenCalled(); expect(f.ended).toHaveBeenCalledOnce();
});
it("closes capture if speech disconnects", () => {
  const f = fixture(); f.online(); f.emit({ type: "state", state: { state: "offline", since: 1, reason: "Offline", retryAt: null } });
  expect(f.error).toHaveBeenCalledOnce(); expect(f.close).toHaveBeenCalledOnce();
});
it("bounds both authentication and final transcript waits", () => {
  const first = fixture(); vi.advanceTimersByTime(10000); expect(first.error).toHaveBeenCalledOnce(); expect(first.close).toHaveBeenCalledOnce();
  const f = fixture(); f.online(); f.capture.finish();
  f.emit({ type: "text", frame: { type: "ready", session_id: "utterance", engine: "fake" } });
  vi.advanceTimersByTime(10000); expect(f.error).toHaveBeenCalledOnce(); expect(f.close).toHaveBeenCalledOnce();
});

it.each(["offline", "failed"] as const)("retains a voice-control speech refusal (%s)", state => {
  const f = fixture();
  f.emit({ type: "state", state: state === "offline"
    ? { state, since: 0, reason: "Speech model was removed.", retryAt: null }
    : { state, reason: "revoked", detail: "Speech model was removed." } });
  expect(f.error).toHaveBeenCalledWith("Speech model was removed. Try the typed command box, or retry speech after resolving this.");
  expect(f.close).toHaveBeenCalledOnce();
});
