// SPDX-License-Identifier: Apache-2.0
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useApp } from "../../app/context";
import { Button } from "../../design/Button";
import { TextArea } from "../../design/Field";
import { clockTime } from "../../design/time";
import { StateLine } from "../../design/StateLine";
import { useBackLayer } from "../../design/back";
import type { InputAction } from "../../protocol/types";
import { CHORDS, COMMON_KEYS, FUNCTION_KEYS, keyChord } from "./keys";
import { useLocalText } from "./local";
import { keyCap } from "./KeyCap";

export function Keyboard({
  targetId,
  enabled,
  send,
  onClose,
  feedback,
  pasteOffered = false,
  onEdit,
  confirmedText,
  pending = false,
}: {
  feedback?: ReactNode;
  pasteOffered?: boolean;
  onEdit?: () => void;
  confirmedText?: { text: string; at: number } | null;
  pending?: boolean;
  targetId: string;
  enabled: boolean;
  send: (action: InputAction) => boolean;
  onClose: () => void;
}) {
  const { profile } = useApp();
  const [text, setText] = useLocalText(
    `control.draft.${profile.profileId}.${targetId}`,
  );
  useEffect(() => {
    if (confirmedText && text === confirmedText.text) setText("");
  }, [confirmedText]);
  const shortcutId = useId();
  const [functions, setFunctions] = useState(false);
  const [modifiers, setModifiers] = useState<string[]>([]);
  const [shortcutKey, setShortcutKey] = useState("");
  const composing = useRef(false);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    const element = heading.current;
    element?.focus({ preventScroll: true });
  }, []);
  // Android normally consumes Back for the IME before the app callback runs.
  // Focus can remain after the IME is hidden, so do not require another Back.
  useBackLayer(true, onClose);
  useEffect(() => { setModifiers([]); setShortcutKey(""); }, [targetId]);
  const key = (label: string) => {
    if (!enabled || composing.current) return;
    if (["Ctrl", "Alt", "Super", "Shift"].includes(label)) {
      setModifiers((m) =>
        m.includes(label) ? m.filter((v) => v !== label) : [...m, label],
      );
      return;
    }
    if (send(keyChord([...modifiers, label].join("+")))) setModifiers([]);
  };
  return (
    <section className="remote-keyboard" aria-label="Remote keyboard">
      <h2 ref={heading} tabIndex={-1} className="visually-hidden">Remote keyboard</h2>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (
            !composing.current &&
            enabled &&
            text &&
            !pending
          ) send({ action: "text", text });
        }}
      >
        <TextArea
          label="Text for the computer"
          value={text}
          onChange={(value) => { setText(value); onEdit?.(); }}
          rows={1}
          maxRows={2}
          onCompositionStart={() => {
            composing.current = true;
          }}
          onCompositionEnd={(event) => {
            // Some Android IMEs replace their pre-edit text with a corrected
            // word or emoji only when the composition ends.
            setText(event.currentTarget.value);
            onEdit?.();
            composing.current = false;
          }}
        />
        {confirmedText && <p className="fine" role="status">Typed · {clockTime(confirmedText.at)}</p>}
        {pending && <p className="fine" role="status">Sent · waiting for the computer</p>}
        {feedback}
        {text && !pending && !feedback && <StateLine phase="saved_on_phone" />}
        {!pasteOffered && <Button type="submit" variant="primary" disabled={!enabled || !text || pending}>
          Send text
        </Button>}
      </form>
      <Button size="sm" variant="quiet" onClick={onClose}>Hide keyboard</Button>
      <details className="remote-keyboard__keys"><summary>Keys and shortcuts</summary>
      {modifiers.length > 0 && <div role="status">
        Next key uses {modifiers.join(" + ")}.{" "}
        <Button size="sm" variant="quiet" onClick={() => setModifiers([])}>Clear modifiers</Button>
      </div>}
      <div className="control-keys">
        {(functions ? FUNCTION_KEYS : COMMON_KEYS).map((label) => (
          <Button
            key={label}
            size="sm"
            aria-pressed={modifiers.includes(label)}
            disabled={!enabled}
            onClick={() => key(label)}
            {...keyCap(label)}
          />
        ))}
      </div>
      <div className="field">
        <label className="field__label" htmlFor={shortcutId}>Shortcut letter or number</label>
        <select
          className="field__input"
          id={shortcutId}
          value={shortcutKey}
          disabled={!enabled}
          onChange={(event) => setShortcutKey(event.target.value)}
        >
          <option value="">Choose a key</option>
          {"ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890".split("").map((letter) => (
            <option key={letter} value={letter}>{letter}</option>
          ))}
        </select>
        <Button
          size="sm"
          disabled={!enabled || !shortcutKey || modifiers.length === 0}
          onClick={() => {
            if (!composing.current && send(keyChord([...modifiers, shortcutKey].join("+")))) {
              setModifiers([]);
              setShortcutKey("");
            }
          }}
        >
          Send shortcut
        </Button>
      </div>
      <div className="control-keys">
        {CHORDS.map((label) => (
          <Button
            key={label}
            size="sm"
            disabled={!enabled}
            onClick={() => {
              if (!composing.current && send(keyChord(label))) setModifiers([]);
            }}
          >
            {label}
          </Button>
        ))}
        <Button size="sm" onClick={() => setFunctions((v) => !v)}>
          {functions ? "Main keys" : "Function keys"}
        </Button>
      </div>
      </details>
    </section>
  );
}
