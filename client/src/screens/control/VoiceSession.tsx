// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, type ReactNode } from "react";
import { Mic, Square } from "lucide-react";
import { useApp } from "../../app/context";
import { announce } from "../../design/Announcer";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { TextArea } from "../../design/Field";
import { Empty, Section } from "../../design/ListRow";
import { CapabilityNote, Notice } from "../../design/Notice";
import { StateLine } from "../../design/StateLine";
import type { VoiceOperation, VoiceSessionView } from "../../protocol/types";
import { Screen } from "../../shell/Screen";
import { useVoiceVisible } from "./VoiceState";
import { useVoiceSession } from "./useVoiceSession";
import "./voice.css";
export function VoicePrompt({ value, disabled, send }: { value: VoiceSessionView; disabled: boolean; send: (operation: VoiceOperation) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (["confirm", "disambiguation", "refused"].includes(value.phase)) { ref.current?.focus({ preventScroll: true }); announce(value.understood); }
  }, [value.prompt_id, value.phase]);
  return <div className="voice-prompt voice-stack" ref={ref} tabIndex={-1} aria-label="Understood command">
    <p className="voice-label">{value.phase === "assessing" ? "Understanding" : "Understood"}</p><h2>{value.understood || "Ready for a command"}</h2>
    {value.phase === "disambiguation" && <ol className="voice-choices">{value.choices.map((e, i) => <li key={e.id}><Button block disabled={disabled} onClick={() => send({ operation: "choose", session_id: value.session_id, prompt_id: value.prompt_id, element_id: e.id })}>{i + 1}. {e.name || "Unnamed control"} · {e.role}</Button></li>)}</ol>}
    {value.phase === "confirm" && <><p>This action needs your confirmation. Say “confirm” or use the button.</p><Button variant="warn" disabled={disabled} onClick={() => send({ operation: "confirm", session_id: value.session_id, prompt_id: value.prompt_id })}>Confirm action</Button></>}
    {(value.phase === "confirm" || value.phase === "disambiguation") && <Button disabled={disabled} onClick={() => send({ operation: "cancel", session_id: value.session_id, prompt_id: value.prompt_id })}>Cancel command</Button>}
  </div>;
}
export function VoiceEnded({ value, action }: { value: VoiceSessionView; action?: ReactNode }) {
  return <Notice title="Voice control ended" action={action}>{value.ended === "heartbeat" ? "The computer stopped receiving this device’s heartbeat." : value.ended === "idle" ? "No commands were given for 10 minutes." : "The computer has ended this session."} Review a new session before sending more input.</Notice>;
}
/** The session's latest actions, newest first. */
export function VoiceSteps({ value, last }: { value: VoiceSessionView; last: number }) {
  return <ol className="voice-log">{value.steps.slice(-last).reverse().map((step, i) => <li key={`${step.utterance_id}-${i}`}><strong>{step.transcript}</strong><p>{step.detail}</p><span className="fine"><Age at={step.at} suffix=" ago" />{step.jev_usage ? ` · Jev: ${step.jev_usage.input_tokens ?? "Unknown"} input, ${step.jev_usage.output_tokens ?? "Unknown"} output tokens` : " · Jev usage not reported"}</span></li>)}</ol>;
}
export default function VoiceSession({ sessionId }: { sessionId: string }) {
  const { ui } = useApp();
  const voice = useVoiceSession(sessionId, useVoiceVisible());
  const { view, value, online, usable, busy, stopping, listening, heard, text, save, send, sendTyped, startSpeech, finishSpeech, closeSpeech, stop } = voice;
  const error = voice.error ?? voice.queryError;
  return <Screen title={value?.target === "desktop" ? "Desktop voice control" : "Browser voice control"} footer={<div className="voice-stop"><Button block variant="stop" icon={Square} className="intervention" pending={stopping} disabled={!online || !!value?.ended} onClick={() => void stop()}>Stop voice control</Button></div>}>
    <div className="voice-stack">
      {!online && <Notice tone="unknown">Offline. No commands can be sent. Input expires when the computer stops receiving this device’s heartbeat.</Notice>}
      {value?.ended && <VoiceEnded value={value} />}
      {value?.dry_run && <Notice>Preview only. The computer will interpret commands without sending input.</Notice>}
      {error && <Notice tone="unknown">{error}</Notice>}
      {value ? <>
        <VoicePrompt value={value} disabled={!usable || busy} send={o => void send(o)} />
        {(value.phase === "refused") && <Button onClick={() => ui.push({ screen: "desktop", targetId: value.target_id })}>Open remote keyboard</Button>}
        <Section title="Latest picture"><figure className="voice-picture">{value.picture ? <><img src={`data:image/png;base64,${value.picture.png_base64}`} alt="Last picture reported by the computer" /><figcaption>{online ? "Picture from " : "Last picture · "}<Age at={value.picture.at} suffix=" ago" /></figcaption></> : <Empty title="No picture received">The computer has not supplied a picture for this session.</Empty>}</figure><Button onClick={() => ui.push({ screen: "desktop", targetId: value.target_id })}>Watch live desktop</Button></Section>
      </> : <Empty title="Checking the session…">The computer has not confirmed its current state.</Empty>}
      <Section title="Give a command"><div className="voice-stack">
        {view && <CapabilityNote what="Speech" availability={view.speech} action={<Button onClick={() => ui.push({ screen: "settings", section: "settings-voice" })}>Set up local speech</Button>} />}
        <Button icon={Mic} block aria-label={listening ? "Release to finish speaking" : "Hold to talk"} aria-pressed={listening} disabled={!usable || view?.speech.state !== "available"} onPointerDown={e => { e.currentTarget.setPointerCapture(e.pointerId); startSpeech(); }} onPointerUp={finishSpeech} onPointerCancel={closeSpeech} onClick={e => { if (e.detail === 0) listening ? finishSpeech() : startSpeech(); }}>{listening ? "Listening… release to finish" : "Hold to talk"}</Button>
        <p className="fine">With a keyboard or screen reader, activate once to start and again to finish.</p>
        {(heard || value?.transcript) && <p className="voice-transcript" aria-label="Live transcript">{heard || value?.transcript}</p>}
        <TextArea label="Typed command" value={text} onChange={save} placeholder="Click Sign in, or type “hello”" rows={3} />
        <Button variant="primary" disabled={!usable || !text.trim()} pending={busy} onClick={() => void sendTyped()}>Send command</Button>
        {busy && <StateLine phase="sending" />}
      </div></Section>
      <Section title="Action log">{value?.steps.length ? <VoiceSteps value={value} last={6} /> : <p className="fine">No action recorded yet.</p>}</Section>
    </div>
  </Screen>;
}
