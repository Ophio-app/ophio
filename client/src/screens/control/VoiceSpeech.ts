// SPDX-License-Identifier: Apache-2.0
import type { SpeechLink } from "../../host/link";

export interface VoiceCapture { finish(): void; cancel(): void }

/** Own one foreground microphone stream, including connection and finish races. */
export function captureVoice(
  link: SpeechLink,
  sessionId: string,
  transcript: (text: string, final: boolean) => void,
  error: (message: string) => void,
  ended: () => void,
): VoiceCapture {
  let closed = false;
  let started = false;
  let finishing = false;
  const parts = new Map<string, string>();
  let text = "";
  let unlisten = () => {};
  let timer: ReturnType<typeof setTimeout>;
  const close = (cancel: boolean) => {
    if (closed) return;
    closed = true;
    clearTimeout(timer);
    unlisten();
    if (cancel && started) link.send({ type: "cancel" });
    link.close();
    ended();
  };
  const fail = (message: string) => { error(message); close(true); };
  /** The computer's own words, as a sentence, with the next step. */
  const refused = (message: string) => {
    const text = message.trim().replace(/^./, (c) => c.toUpperCase());
    fail(`${text ? (/[.!?]$/.test(text) ? text : `${text}.`) : "Local speech stopped."} Try the typed command box, or retry speech after resolving this.`);
  };
  timer = setTimeout(() => fail("The computer did not connect to local speech. Try the typed command box."), 10000);
  unlisten = link.listen(event => {
    if (closed) return;
    if (event.type === "state") {
      if (event.state.state === "online" && !started) {
        started = link.send({ type: "start", session_id: sessionId, language: null, keep_audio: false });
        if (!started) fail("The computer could not start local speech. Try the typed command box.");
      } else if (event.state.state === "offline" || event.state.state === "failed") {
        const reason = event.state.state === "offline" ? event.state.reason : event.state.detail;
        fail(`${reason} Try the typed command box, or retry speech after resolving this.`);
      }
      return;
    }
    if (event.type !== "text") return;
    const frame = event.frame;
    if (frame.type === "ready" && frame.session_id === sessionId && !finishing) clearTimeout(timer);
    else if (frame.type === "segment") {
      parts.set(frame.segment.segment_id, frame.segment.text);
      text = [...parts.values()].join(" ");
      transcript(text, false);
    } else if (frame.type === "finished" && frame.session_id === sessionId) {
      if (text.trim()) transcript(text, true);
      close(false);
    } else if (frame.type === "error") refused(frame.message);
  });
  return {
    finish() {
      if (closed || finishing) return;
      // Releasing before authentication must never start a microphone later.
      if (!started) { close(true); return; }
      finishing = true;
      clearTimeout(timer);
      timer = setTimeout(() => fail("The computer did not finish the transcript. Try the typed command box."), 10000);
      if (!link.send({ type: "finish" })) fail("The computer could not finish local speech. Try the typed command box.");
    },
    cancel: () => close(true),
  };
}
