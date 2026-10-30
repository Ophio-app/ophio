// SPDX-License-Identifier: Apache-2.0
import { Fragment, useRef } from "react";
import { useCommandControl } from "../../app/commandControl";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { CapabilityNote, Notice } from "../../design/Notice";
import { Section } from "../../design/ListRow";
import { Changing } from "../../design/StateLine";
import type { Availability } from "../../protocol/types";
import { useVoice } from "./VoiceState";
import "./voice.css";

const KINDS = [["browser", "Browser voice control"], ["desktop", "Desktop voice control"]] as const;
type Choice = { browser: boolean; desktop: boolean };

export function VoiceSettings() {
  const { session, profile } = useApp();
  const allowed = useHost(s => !!s.grants?.control_input);
  const admin = useHost(s => !!s.grants?.admin);
  const { view, error, reload } = useVoice(undefined, allowed);
  const online = useLink().state === "online";
  // The revision the last change saved, until the view shows it.
  const saved = useRef(0);
  // Voice control lets Jev work the computer, so it is a safety choice: the
  // switches show what is in force, and Changing… while a change is on its way.
  const control = useCommandControl<Choice>({
    kind: "safety",
    value: { browser: view?.settings.browser ?? false, desktop: view?.settings.desktop ?? false },
    equal: (a, b) => a.browser === b.browser && a.desktop === b.desktop,
    describe: (e) => describeError(e, profile.name),
    send: async (next, note) => {
      const revision = Math.max(view?.settings.revision ?? 0, saved.current);
      try {
        await session.command({ command: "set_voice", browser: next.browser, desktop: next.desktop }, { label: note.label, expectedRevision: revision });
        saved.current = revision + 1;
      } finally { reload(); }
    },
  });
  if (!allowed) return null;
  const settings = view ? control.value : null;
  const change = (kind: "browser" | "desktop", name: string, on: boolean) => {
    const what = name.toLowerCase();
    void control.set({ ...(control.pending ?? control.value), [kind]: on }, {
      label: `${on ? "Turn on" : "Turn off"} ${what}`, what, said: `${name} is ${on ? "on" : "off"}.`,
    });
  };
  // Why each one that's on can't be used, said once: an unlinked Jev stops both the same way.
  const notes: { what: string; availability: Availability }[] = [];
  if (view && settings) for (const [kind, name] of KINDS) {
    const availability = kind === "browser" ? view.voice_browser : view.voice_desktop;
    if (!settings[kind] || availability.state === "available") continue;
    const same = notes.find(n => JSON.stringify(n.availability) === JSON.stringify(availability));
    if (same) same.what = "Voice control";
    else notes.push({ what: name, availability });
  }
  return <Section title="Voice control" id="settings-voice-control"><div className="voice-stack">
    <p>Jev is the voice service that hears you and works the browser or desktop. It uses your own Jev account.</p>
    {settings && KINDS.map(([kind, name]) => <Fragment key={kind}>
      <label className="settings-check">
        <input type="checkbox" checked={settings[kind]} disabled={!online || !admin}
          onChange={e => change(kind, name, e.target.checked)} />{name}
      </label>
      {control.changing && control.changing[kind] !== settings[kind] &&
        <Changing to={`turning ${name.toLowerCase()} ${control.changing[kind] ? "on" : "off"}`} />}
    </Fragment>)}
    {notes.map(n => <CapabilityNote key={n.what} what={n.what} availability={n.availability} />)}
    {error && <Notice tone="unknown">{error}</Notice>}
  </div></Section>;
}
