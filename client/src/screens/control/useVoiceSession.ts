// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { commandResult } from "../../host/commandResult";
import { uuidv7 } from "../../protocol/ids";
import type { VoiceOperation, VoiceSessionView } from "../../protocol/types";
import { captureVoice, type VoiceCapture } from "./VoiceSpeech";
import { useVoice } from "./VoiceState";
import { dropVoice, holdVoice } from "./voiceHold";

/** One voice session as a screen shows it: its state, commands, speech and the
 * heartbeat that keeps its lease while `active`. No session: all inert. */
export function useVoiceSession(sessionId: string | undefined, active: boolean) {
  const { session, platform, profile } = useApp();
  const { view, error: queryError, reload } = useVoice(sessionId, active && !!sessionId);
  const online = useLink().state === "online";
  const grants = useHost(s => s.grants);
  const [override, setOverride] = useState<VoiceSessionView | null>(null);
  const value = (sessionId && view?.session?.session_id === sessionId ? view.session : null) ?? (override?.session_id === sessionId ? override : null);
  const control = useHost(s => value ? s.control[value.target_id] : undefined);
  const draftKey = `voice-command.${profile.profileId}.${sessionId ?? "none"}`;
  const [draft, setDraft] = useState(() => ({ key: draftKey, text: localStorage.getItem(draftKey) ?? "" }));
  const text = draft.key === draftKey ? draft.text : localStorage.getItem(draftKey) ?? "";
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [listening, setListening] = useState(false);
  const [heard, setHeard] = useState("");
  const speech = useRef<VoiceCapture | null>(null);
  const usable = online && !value?.ended && !!value && !!grants?.control_input && (control?.lease?.lease_id === value.lease.lease_id ? control.lease.state === "active" : true);
  const save = (value: string) => { setDraft({ key: draftKey, text: value }); try { localStorage.setItem(draftKey, value); } catch { setError("Not saved on this device. Keep this screen open to preserve your command."); } };
  // Every view of the display must know this lease is voice control's.
  const target = value?.target_id, lease = value?.lease.lease_id, ended = !!value?.ended;
  useEffect(() => {
    if (!sessionId || !target || !lease) return;
    if (ended) dropVoice(session, profile.profileId, target, sessionId);
    else holdVoice(session, profile.profileId, target, { sessionId, leaseId: lease });
  }, [session, profile.profileId, sessionId, target, lease, ended]);
  const send = async (operation: VoiceOperation, background = false): Promise<boolean> => {
    if (!online) return false;
    if (!background) { setBusy(true); setError(null); }
    try {
      // A late command is refused, but never a late Stop: the computer checks the
      // deadline by its own clock, which may be ahead of this phone's.
      const stop = operation.operation === "stop";
      const receipt = await session.command({ command: "voice", operation }, { label: stop ? "Stop voice control" : "Voice command", ...(stop ? {} : { expiresInMs: 10000 }) });
      const result = await commandResult(session, receipt);
      if (result?.kind === "voice_session") setOverride(result.session);
      reload();
      return true;
    } catch (e) {
      if (!background) setError(describeError(e, profile.name));
      return false;
    } finally { if (!background) setBusy(false); }
  };
  /** A command the computer took is no longer a draft, unless it was edited meanwhile. */
  const sendTyped = async () => {
    if (!sessionId) return;
    const typed = text, key = draftKey;
    if (await send({ operation: "transcript", session_id: sessionId, utterance_id: uuidv7(Date.now()), revision: 1, text: typed, final_segment: true })) {
      setDraft(current => current.key === key && current.text === typed ? { key, text: "" } : current);
      try { if (localStorage.getItem(key) === typed) localStorage.removeItem(key); } catch { /* The draft stays; it is only text. */ }
    }
  };
  const sendRef = useRef(send); sendRef.current = send;
  const closeSpeech = () => { const s = speech.current; speech.current = null; s?.cancel(); setListening(false); };
  useEffect(() => {
    const hidden = () => { if (document.hidden) closeSpeech(); };
    document.addEventListener("visibilitychange", hidden);
    return () => document.removeEventListener("visibilitychange", hidden);
  }, []);
  useEffect(() => {
    if (!active || !online || value?.ended) closeSpeech();
    return () => closeSpeech();
  }, [active, online, value?.ended, sessionId]);
  useEffect(() => {
    if (!active || !online || !value || value.ended || stopping) return;
    const lease = value.lease;
    const renew = () => { if (document.visibilityState === "visible") session.renewLease(lease.lease_id, lease.generation); };
    renew(); const timer = setInterval(renew, Math.min(1000, lease.renewal_window_ms / 3));
    return () => clearInterval(timer);
  }, [session, active, online, value?.lease.lease_id, value?.ended, stopping]);
  const startSpeech = () => {
    if (!sessionId || !usable || speech.current || view?.speech.state !== "available") return;
    const link = platform.speech(profile.profileId);
    const utterance = uuidv7(Date.now());
    let revision = 0;
    setHeard(""); setListening(true); setError(null);
    speech.current = captureVoice(link, utterance, (text, final) => {
      setHeard(text);
      void sendRef.current({ operation: "transcript", session_id: sessionId, utterance_id: utterance, revision: ++revision, text, final_segment: final }, true);
    }, setError, () => { speech.current = null; setListening(false); });
  };
  const finishSpeech = () => { speech.current?.finish(); setListening(false); };
  const stop = async (): Promise<boolean> => {
    if (!sessionId) return false;
    closeSpeech(); setStopping(true);
    const stopped = await send({ operation: "stop", session_id: sessionId });
    setStopping(false);
    return stopped;
  };
  return { view, value, queryError, error, online, usable, busy, stopping, listening, heard, text, save, send, sendTyped, startSpeech, finishSpeech, closeSpeech, stop };
}
export type VoiceSessionState = ReturnType<typeof useVoiceSession>;
