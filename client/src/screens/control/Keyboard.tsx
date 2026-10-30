// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState, type ReactNode } from "react";
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
  const [functions, setFunctions] = useState(false);
  const [modifiers, setModifiers] = useState<string[]>([]);
  const composing = useRef(false);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    const element = heading.current;
    element?.focus({ preventScroll: true });
  }, []);
  useBackLayer(true, onClose);
  const key = (label: string) => {
    if (["Ctrl", "Alt", "Super"].includes(label)) {
      setModifiers((m) =>
        m.includes(label) ? m.filter((v) => v !== label) : [...m, label],
      );
      return;
    }
    send(keyChord([...modifiers, label].join("+")));
    setModifiers([]);
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
          onCompositionEnd={() => {
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
      <div className="control-keys">
        {CHORDS.map((label) => (
          <Button
            key={label}
            size="sm"
            disabled={!enabled}
            onClick={() => send(keyChord(label))}
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
