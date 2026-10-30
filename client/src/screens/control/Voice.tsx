// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { Globe, Monitor } from "lucide-react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { CapabilityNote, Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import type { VoiceTarget } from "../../protocol/types";
import { Screen } from "../../shell/Screen";
import { NotAllowed } from "../../shell/NotAllowed";
import type { RouteProps } from "../types";
import { useVoice, useVoiceVisible } from "./VoiceState";
import VoiceSession from "./VoiceSession";
import { startVoice } from "./voiceHold";
import "./voice.css";
export function VoiceDisclosure() {
  return <div className="voice-stack"><p>Voice control sends what you said, as text, and the names and roles of up to 100 on-screen controls to TypeSafe’s Jev.</p><p>Pictures, other page text and field values are not sent. Audio stays between this device and the computer’s local speech recognizer and is not saved.</p><p>Charges go to your own Jev access, separate from native agent usage. Every command and its result are recorded on the computer.</p></div>;
}
export default function Voice({ route }: RouteProps<"voice">) { return route.sessionId ? <VoiceSession sessionId={route.sessionId} /> : <VoiceStart />; }
function VoiceStart() {
  const { ui, profile } = useApp();
  const grants = useHost(s => s.grants);
  // The computer refuses voice control without both; say which, not its refusal.
  const missing = ([["view", "view"], ["control_input", "control"]] as const).filter(([grant]) => grants?.[grant] === false).map(([, permission]) => permission);
  const { view, error: queryError, reload } = useVoice(undefined, useVoiceVisible() && !missing.length);
  const online = useLink().state === "online";
  const [choice, setChoice] = useState<VoiceTarget | null>(null);
  return <Screen title="Voice control"><div className="voice-stack">
    <p className="voice-heading">Say what to do.</p><p>Choose a separate browser or the app in focus on {profile.name}. A typed command works too.</p>
    {!online && <Notice tone="unknown">Offline. Connect to check availability and start voice control.</Notice>}
    {missing.length > 0 ? <NotAllowed what="use voice control" permission={missing.join(" ")} /> : <>
    {queryError && <Notice tone="unknown" action={<Button disabled={!online} onClick={reload}>Check again</Button>}>{queryError}</Notice>}
    {!view ? <Empty title={queryError ? "Availability not confirmed" : "Checking voice control…"}>The computer must confirm Jev access and control support.</Empty> : <Section title="Choose where">
      {(["browser", "desktop"] as const).map(kind => {
        const availability = kind === "browser" ? view.voice_browser : view.voice_desktop;
        const name = kind === "browser" ? "Browser" : "This desktop";
        return <div className="voice-target" key={kind}><ListRow icon={kind === "browser" ? Globe : Monitor} title={name} subtitle={kind === "browser" ? "A separate browser window" : "The focused app on the computer"} chevron={availability.state === "available"} onClick={availability.state === "available" && online && grants?.control_input ? () => setChoice(kind) : undefined} /><CapabilityNote what={name} availability={availability} /></div>;
      })}
    </Section>}
    </>}
    {view && !missing.length && <CapabilityNote what="Speech" availability={view.speech} action={<Button onClick={() => ui.push({ screen: "settings", section: "settings-voice" })}>Set up local speech</Button>} />}
    <p className="fine">Start with a preview. When you allow input, Stop stays in reach and the computer shows who has control.</p>
    <StartVoice choice={choice} onClose={() => setChoice(null)} />
  </div></Screen>;
}

/** Accept the disclosure and start voice control of `choice`: on `targetId`
 * when a display already chose it, or on one picked here. */
export function StartVoice({ choice, onClose, targetId: fixed, title }: { choice: VoiceTarget | null; onClose: () => void; targetId?: string; title?: string }) {
  const { session, ui, profile } = useApp();
  const targets = useHost(s => s.targets);
  const online = useLink().state === "online";
  const [targetId, setTargetId] = useState<string | null>(null);
  const [dryRun, setDryRun] = useState(true);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const target = fixed ? targets.find(t => t.target_id === fixed) : targets.find(t => t.target_id === targetId) ?? targets[0];
  const start = async () => {
    if (!choice || !target) return;
    setPending(true); setError(null);
    try {
      const started = await startVoice(session, profile.profileId, target, choice, dryRun);
      if (started) { onClose(); ui.push({ screen: "voice", sessionId: started.session_id }); }
      else setError("The computer hasn’t confirmed a voice session. Check Control before starting again.");
    } catch (e) { setError(describeError(e, profile.name)); }
    finally { setPending(false); }
  };
  return <Sheet open={choice !== null} onClose={() => !pending && onClose()} title={title ?? (choice === "browser" ? "Start browser voice control" : "Start desktop voice control")} footer={<Button block variant="primary" pending={pending} disabled={!online || !target} onClick={() => void start()}>{dryRun ? "Accept and preview commands" : "Accept and take over"}</Button>}>
      <div className="voice-stack"><Notice title="Take over computer control">Starting ends the current managed input on {target?.label ?? "the selected display"}. Anyone at the computer can still use it and stop voice control.</Notice>
        {choice === "browser" && <p>A separate browser window, not signed in to your usual browser. Ending voice control leaves that window open.</p>}
        {!fixed && targets.length > 1 && <label className="voice-stack">Control target<select value={target?.target_id ?? ""} onChange={e => setTargetId(e.target.value)}>{targets.map(t => <option key={t.target_id} value={t.target_id}>{t.label}</option>)}</select></label>}
        {!target && <Notice tone="warn">No control target is available. Enable desktop sharing on the computer first.</Notice>}
        <label className="voice-check"><input type="checkbox" checked={dryRun} onChange={e => setDryRun(e.target.checked)} /><span>Preview only · send no input</span></label>
        <VoiceDisclosure />{error && <Notice tone="unknown">{error}</Notice>}{pending && <StateLine phase="sending" />}
      </div>
    </Sheet>;
}
