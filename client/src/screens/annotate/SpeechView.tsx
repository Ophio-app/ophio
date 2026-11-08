// SPDX-License-Identifier: Apache-2.0
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Mic, Square } from "lucide-react";
import { useApp, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { CapabilityNote, Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { announce } from "../../design/Announcer";
import type { SpeechLink } from "../../host/link";
import { uuidv7 } from "../../protocol/ids";
import { draftMs, type AnnotationDraft, type AnnotationStore } from "../../state/annotations";
import { useRoute } from "../../shell/Screen";
import {
  voicePreferences,
  saveVoicePreferences,
} from "../settings/preferences";
import { mergeSegment } from "./speech";
import { useWarning } from "../../app/warnings";
import { Tool } from "../control/FullControl";
export function Speech({
  draft,
  store,
  recordingTarget,
  onRecording,
  compact,
  notesTarget,
  keepAudio,
}: {
  draft: AnnotationDraft;
  store: AnnotationStore;
  recordingTarget: HTMLElement | null;
  onRecording(active: boolean): void;
  /** One mic button among the picture's controls; why speaking isn't ready
   * opens on tap, and a failure is told in `notesTarget`. */
  compact?: boolean;
  notesTarget?: HTMLElement | null;
  keepAudio?: boolean;
}) {
  const { platform, profile, ui } = useApp(),
    link = useLink(),
    { active } = useRoute();
  const readiness = useHost((s) => s.readiness),
    caps = useHost((s) => s.capabilities?.speech);
  const [explain, setExplain] = useState(false),
    [why, setWhy] = useState(false),
    [recording, setRecording] = useState(false),
    [finishing, setFinishing] = useState(false);
  const [engine, setEngine] = useState<string | null>(null),
    [error, setError] = useState<string | null>(null),
    [rms, setRms] = useState(0),
    [seconds, setSeconds] = useState(0);
  const warn = useWarning("voice.mark");
  const channel = useRef<SpeechLink | null>(null),
    unsubscribe = useRef<(() => void) | null>(null),
    finishTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const speechCheck = readiness?.checks.find((c) => c.area === "speech");
  const ready =
    link.state === "online" &&
    caps?.state === "available" &&
    speechCheck?.status === "passed";
  const close = useCallback(() => {
    unsubscribe.current?.();
    unsubscribe.current = null;
    channel.current?.close();
    channel.current = null;
    if (finishTimer.current) clearTimeout(finishTimer.current);
  }, []);
  const stop = useCallback(() => {
    setRecording(false);
    setRms(0);
    if (!channel.current?.send({ type: "finish" })) {
      close();
      setError(
        "Recording stopped. The computer hasn’t confirmed the final transcript. Review the text kept here.",
      );
      setFinishing(false);
      return;
    }
    setFinishing(true);
    finishTimer.current = setTimeout(() => {
      close();
      setFinishing(false);
      setError(
        "The final transcript hasn’t arrived. Your existing text is kept.",
      );
    }, 15000);
    announce("Recording stopped");
  }, [close]);
  useEffect(() => {
    onRecording(recording || finishing);
    return () => onRecording(false);
  }, [recording, finishing, onRecording]);
  useEffect(() => {
    if (!recording) return;
    // Modal editing cannot cover an active microphone stop control.
    const observer = new MutationObserver(() => {
      if (document.querySelector("dialog[open]")) stop();
    });
    observer.observe(document.body, {
      subtree: true,
      attributes: true,
      attributeFilter: ["open"],
    });
    return () => observer.disconnect();
  }, [recording, stop]);
  useEffect(
    () => () => {
      channel.current?.send({ type: "cancel" });
      close();
    },
    [close],
  );
  useEffect(() => {
    if ((!active || !ready) && (recording || finishing)) {
      channel.current?.send({ type: "cancel" });
      close();
      setRecording(false);
      setFinishing(false);
      setError("Recording ended. Review the text kept on this device.");
    }
  }, [active, ready, recording, finishing, close]);
  useEffect(() => {
    if (!recording) return;
    const start = Date.now();
    const timer = setInterval(
      () => setSeconds(Math.floor((Date.now() - start) / 1000)),
      500,
    );
    const hidden = () => {
      if (document.hidden) stop();
    };
    document.addEventListener("visibilitychange", hidden);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", hidden);
    };
  }, [recording, stop]);
  const start = () => {
    if (!ready) return;
    close();
    setExplain(false);
    setError(null);
    setEngine(null);
    setSeconds(0);
    const preferences = { ...voicePreferences(profile.profileId).get(), ...(keepAudio === undefined ? {} : { keepAudio }) },
      sessionId = uuidv7(Date.now()),
      tapped = Date.now() - draft.createdAt;
    // Transcript times count from the first sound captured, which a
    // permission prompt or a slow link may hold for seconds after the tap.
    let captured: number | null = null;
    store.update(draft.id, (d) => ({
      ...d,
      speech: {
        session_id: sessionId,
        audio_artifact: null,
        segments: d.speech?.segments ?? [],
        transcript_revision: d.speech?.transcript_revision ?? 0,
      },
    }));
    const speech = platform.speech(profile.profileId);
    channel.current = speech;
    let started = false;
    const begin = () => {
      started = true;
      if (finishTimer.current) clearTimeout(finishTimer.current);
      if (
        !speech.send({
          type: "start",
          session_id: sessionId,
          language: preferences.language.trim() || null,
          keep_audio: preferences.keepAudio,
        })
      ) {
        close();
        setRecording(false);
        setError("Speaking couldn’t start on the computer. Keep typing.");
      }
    };
    unsubscribe.current = speech.listen((event) => {
      if (event.type === "level") {
        setRms(Math.min(1, Math.max(0, event.rms)));
        return;
      }
      if (event.type === "capturing") {
        captured = Date.now() - draft.createdAt;
        return;
      }
      if (event.type === "state") {
        // The speech connection opens after this; start once it is online.
        if (event.state.state === "online") {
          if (!started) begin();
        } else if (
          event.state.state === "offline" ||
          event.state.state === "failed"
        ) {
          close();
          setRecording(false);
          setFinishing(false);
          setError(
            `${event.state.state === "offline" ? event.state.reason : event.state.detail} Your text is kept. Keep typing, or try Speak again after resolving this.`,
          );
        }
        return;
      }
      const frame = event.frame;
      if (frame.type === "ready") {
        setEngine(frame.engine);
        announce(
          `Recording. Transcribed on ${profile.name} using ${frame.engine}.`,
        );
      } else if (frame.type === "segment") {
        const offset = captured ?? tapped;
        store.arrive(draft.id, (d) =>
          mergeSegment(d, {
            ...frame.segment,
            segment_id: `${sessionId}:${frame.segment.segment_id}`,
            start_ms: draftMs(frame.segment.start_ms + offset),
            end_ms: draftMs(frame.segment.end_ms + offset),
          }),
        );
      } else if (frame.type === "finished") {
        store.record(draft.id, (d) =>
          d.speech
            ? {
                ...d,
                speech: {
                  ...d.speech,
                  audio_artifact: preferences.keepAudio
                    ? frame.audio_artifact
                    : null,
                },
              }
            : d,
        );
        close();
        setRecording(false);
        setFinishing(false);
      } else if (frame.type === "error") {
        // The computer's words are terse engine text; say what is kept and what to do.
        const text = frame.message.trim().replace(/^./, (c) => c.toUpperCase());
        setError(
          `${text ? (/[.!?]$/.test(text) ? text : `${text}.`) : "Speaking stopped on the computer."} Your text is kept. Keep typing, or try Speak again after resolving this.`,
        );
        close();
        setRecording(false);
        setFinishing(false);
      }
    });
    finishTimer.current = setTimeout(() => {
      if (started) return;
      close();
      setRecording(false);
      setError(`${profile.name} didn’t open speech. Keep typing.`);
    }, 10000);
    setRecording(true);
    try {
      saveVoicePreferences(profile.profileId, { explained: true });
    } catch {
      setError(
        "The voice preference wasn’t saved. The transcript still belongs to this draft.",
      );
    }
  };
  const unready = ready ? null : link.state !== "online" ? (
    <Notice tone="unknown">
      Speaking needs {profile.name} to be connected. Keep typing; this
      draft stays on this device.
    </Notice>
  ) : caps && caps.state !== "available" ? (
    <CapabilityNote
      what="Speaking"
      availability={caps}
      action={
        <Button
          onClick={() => {
            setWhy(false);
            ui.push({ screen: "settings", section: "settings-voice" });
          }}
        >
          Set up speech
        </Button>
      }
    />
  ) : (
    <Notice tone="unknown">
      {speechCheck?.detail ??
        `${profile.name} hasn’t confirmed its speech recognizer yet.`}{" "}
      Keep typing.
    </Notice>
  );
  const panel = (
    <div className="an-recording">
      <p>
        <Mic aria-hidden /> Recording{" "}
        <time className="num">
          {Math.floor(seconds / 60)}:
          {String(seconds % 60).padStart(2, "0")}
        </time>
      </p>
      <meter aria-label="Microphone level" min={0} max={1} value={rms} />
      <p className="fine">
        {engine
          ? `Transcribed on ${profile.name} · ${engine}`
          : "Waiting for the computer to name its transcription engine…"}
      </p>
      <Button
        block
        size="lg"
        variant="stop-outline"
        icon={Square}
        onClick={stop}
      >
        Stop recording
      </Button>
    </div>
  );
  const sheet = (
    <Sheet
      open={explain}
      onClose={() => setExplain(false)}
      title="Speak while you mark"
      footer={
        <>
          <Button onClick={() => setExplain(false)}>Keep typing</Button>
          <Button variant="primary" icon={Mic} onClick={() => { warn.agreed(); start(); }}>
            Start recording
          </Button>
        </>
      }
    >
      <p>
        Circle one thing while explaining it, then the next. Review which
        comment belongs to each mark.
      </p>
      <p className="an-paragraph">
        This device’s microphone streams audio to {profile.name} for
        transcription. The computer reports its engine when recording starts;
        its name stays visible beside the timer.
      </p>
      <p className="an-paragraph">
        {voicePreferences(profile.profileId).get().keepAudio
          ? "Audio will be retained on the computer for correction."
          : "Keep audio is off."}{" "}
        Nothing goes to the agent until you review and send. Text and images
        are sent without audio.
      </p>
      {warn.check}
    </Sheet>
  );
  if (compact)
    return (
      <>
        {error &&
          (notesTarget ? (
            createPortal(
              <p className="an-note" role="status">
                {error}{" "}
                <button type="button" className="an-note__close" onClick={() => setError(null)}>
                  Dismiss
                </button>
              </p>,
              notesTarget,
            )
          ) : (
            <Notice tone="warn">{error}</Notice>
          ))}
        {recording && recordingTarget ? (
          createPortal(panel, recordingTarget)
        ) : (
          <Tool
            icon={Mic}
            label={finishing ? "Finishing transcript…" : "Speak"}
            short={finishing ? "Wait" : "Speak"}
            disabled={finishing}
            onClick={() =>
              ready ? warn.ask(() => setExplain(true), start) : setWhy(true)
            }
          />
        )}
        <Sheet open={why} onClose={() => setWhy(false)} title="Speaking isn’t ready">
          {unready}
        </Sheet>
        {sheet}
      </>
    );
  return (
    <div className="an-speech">
      {unready}
      {error && <Notice tone="warn">{error}</Notice>}
      {recording && recordingTarget ? (
        createPortal(panel, recordingTarget)
      ) : (
        <Button
          icon={Mic}
          disabled={!ready || finishing}
          pending={finishing}
          onClick={() => warn.ask(() => setExplain(true), start)}
        >
          {finishing ? "Finishing transcript…" : "Speak"}
        </Button>
      )}
      {sheet}
    </div>
  );
}
