// SPDX-License-Identifier: Apache-2.0
import { useEffect, useImperativeHandle, useLayoutEffect, useRef, useState, type Ref } from "react";
import { useBackLayer } from "../../design/back";
import { useKeyboardOpen } from "../../shell/keyboard";
import { Button } from "../../design/Button";
import type { InputAction } from "../../protocol/types";
import { CHORDS, keyChord } from "./keys";

const MODIFIERS = ["Ctrl", "Alt", "Super", "Shift"];
const EXTRA = ["Esc", "Tab", ...MODIFIERS, "←", "↑", "↓", "→"];
const MORE = [...Array.from({ length: 12 }, (_, i) => `F${i + 1}`), "Home", "End", "PageUp", "PageDown", "Delete", "Enter", "Backspace"];
const SPECIAL: Record<string, string> = { Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown", Delete: "Delete", ArrowLeft: "ArrowLeft", ArrowRight: "ArrowRight", ArrowUp: "ArrowUp", ArrowDown: "ArrowDown", "+": "+" };
const namedKey = (label: string): string => SPECIAL[label] ?? (keyChord(label) as Extract<InputAction, { action: "key_chord" }>).keys[0]!;

export interface LiveKeyboardHandle {
  /** An explicit insertion from another control, such as a sign-in code. */
  insert: (action: InputAction) => boolean;
}

/** A private, ephemeral IME bridge. It is not a draft or a copy of the remote
 * field. Only committed edits are sent, in order, after each preceding ack. */
export function LiveKeyboard({ targetId, enabled, send, onClose, bridge }: {
  bridge?: Ref<LiveKeyboardHandle>;
  targetId: string;
  enabled: boolean;
  send: (action: InputAction, signal: AbortSignal) => Promise<boolean>;
  onClose: () => void;
}) {
  const keyboardOpen = useKeyboardOpen();
  const input = useRef<HTMLTextAreaElement>(null);
  const known = useRef("");
  const composing = useRef(false);
  const cancelledComposition = useRef(false);
  const modifiers = useRef<Record<string, 1 | 2>>({});
  const [selected, setSelected] = useState<Record<string, 1 | 2>>({});
  const [more, setMore] = useState(false);
  const [paste, setPaste] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const queue = useRef<InputAction[]>([]);
  const controller = useRef(new AbortController());
  const running = useRef(false);
  const current = useRef({ send, enabled });
  current.current = { send, enabled };
  const focus = () => input.current?.focus({ preventScroll: true });
  const resetText = () => { known.current = ""; if (composing.current) cancelledComposition.current = true; composing.current = false; if (input.current) input.current.value = ""; };
  const cancel = () => {
    controller.current.abort(); controller.current = new AbortController();
    queue.current = []; running.current = false; resetText(); setBusy(false);
  };
  const clearModifiers = () => { modifiers.current = {}; setSelected({}); };
  const stop = (message: string) => { cancel(); clearModifiers(); setNote(message); };
  const pump = async () => {
    if (running.current) return;
    running.current = true;
    const token = controller.current;
    setBusy(true);
    while (queue.current.length && !token.signal.aborted) {
      const action = queue.current.shift()!;
      let ok = false;
      try { ok = current.current.enabled && await current.current.send(action, token.signal); } catch { /* A failed send is never retried. */ }
      if (token.signal.aborted) return;
      if (!ok) { stop("Typing stopped. Check the picture before typing again. Nothing will be sent again automatically."); return; }
    }
    running.current = false; setBusy(false);
  };
  const enqueue = (actions: InputAction[]) => {
    if (!current.current.enabled) { stop("Typing is paused until control and the picture are current."); return false; }
    if (queue.current.length + actions.length > 256) { stop("Typing stopped because too much input is waiting. Check the picture before continuing."); return false; }
    queue.current.push(...actions); setNote(null); void pump(); return true;
  };
  useImperativeHandle(bridge, () => ({ insert: (action) => {
    if (!current.current.enabled) return false;
    // Drop unsent edits, but let the current action settle before inserting.
    // A failure also drops this insertion; nothing overtakes an uncertain key.
    queue.current = []; resetText(); clearModifiers();
    return enqueue([action]);
  } }));
  const consume = () => {
    modifiers.current = Object.fromEntries(Object.entries(modifiers.current).filter(([, state]) => state === 2));
    setSelected({ ...modifiers.current });
  };
  const chord = (label: string) => {
    if (composing.current) return;
    const keys = [...Object.keys(modifiers.current).map(namedKey), namedKey(label)];
    resetText();
    if (enqueue([{ action: "key_chord", keys }])) consume();
  };
  const key = (label: string) => {
    if (!enabled || composing.current) return;
    if (MODIFIERS.includes(label)) {
      const value = modifiers.current[label];
      const next = { ...modifiers.current };
      if (value === 2) delete next[label]; else next[label] = value === 1 ? 2 : 1;
      modifiers.current = next; setSelected(next);
    } else chord(label);
    focus();
  };
  const commit = (value: string, inputType = "") => {
    if (composing.current) return;
    if (!current.current.enabled) { stop("Typing is paused until control and the picture are current."); return; }
    if (inputType === "insertLineBreak" || inputType === "insertParagraph") { chord("Enter"); return; }
    if (inputType === "deleteContentBackward") { chord("Backspace"); return; }
    if (inputType === "deleteContentForward") { chord("Delete"); return; }
    if (value === known.current) return;
    if (value.length > 8192) { stop("Use Paste for long text. This typing buffer was cleared without sending it."); return; }
    const before = known.current;
    if (inputType === "insertReplacementText" && !before) {
      stop("The typing location changed. This correction was not sent; check the computer before continuing."); return;
    }
    let prefix = 0;
    while (prefix < before.length && prefix < value.length && before[prefix] === value[prefix]) prefix++;
    const removed = before.slice(prefix), added = value.slice(prefix);
    // Remote applications disagree about how many Backspaces remove a composed
    // character or emoji. Never guess, or delete text before our own insertion.
    if (removed && !/^[\x20-\x7e]+$/.test(removed)) {
      stop("This keyboard correction cannot be applied safely. Check the computer, then correct it with Backspace or type it again."); return;
    }
    const mods = Object.keys(modifiers.current);
    if (mods.length) {
      // An IME phrase has no single physical key. Only an unambiguous one-key
      // commit may become a chord; a glide/voice phrase must not become text.
      if (removed || !/^[\x20-\x7e]$/.test(added)) {
        stop("A shortcut needs one key. Choose its modifiers again, then type a single letter or number."); return;
      }
      chord(added); return;
    }
    const actions: InputAction[] = Array.from(removed, () => ({ action: "key_chord" as const, keys: ["Backspace"] }));
    if (added) actions.push({ action: "text", text: added });
    known.current = value;
    enqueue(actions);
  };
  // Keep native handlers current without recreating the focused input or
  // disturbing a composition whenever an acknowledgement updates the screen.
  const handlers = useRef({ commit, chord, stop });
  handlers.current = { commit, chord, stop };
  useLayoutEffect(() => { focus(); }, []);
  useEffect(() => {
    const element = input.current!;
    const beforeInput = (event: InputEvent) => {
      if (composing.current || event.isComposing) return;
      const label = ({ insertLineBreak: "Enter", insertParagraph: "Enter", deleteContentBackward: "Backspace", deleteContentForward: "Delete" } as Record<string, string>)[event.inputType];
      if (label && event.cancelable) { event.preventDefault(); handlers.current.chord(label); }
    };
    const changed = (event: Event) => {
      const edit = event as InputEvent;
      if (cancelledComposition.current && (edit.isComposing || edit.inputType === "insertFromComposition" || edit.inputType === "insertCompositionText")) { element.value = ""; return; }
      if (!edit.isComposing) handlers.current.commit(element.value, edit.inputType);
    };
    element.addEventListener("beforeinput", beforeInput);
    element.addEventListener("input", changed);
    return () => { element.removeEventListener("beforeinput", beforeInput); element.removeEventListener("input", changed); };
  }, []);
  useEffect(() => {
    cancel(); clearModifiers(); setPaste(null);
    if (!enabled) setNote("Typing is paused until control and the picture are current.");
    return () => { controller.current.abort(); queue.current = []; known.current = ""; };
  }, [enabled, targetId]);
  useEffect(() => {
    const moved = (event: PointerEvent) => {
      if ((event.target as Element)?.closest?.(".picture-stage")) {
        // Moving the remote pointer must not blur the IME bridge and collapse
        // the phone keyboard. The picture still receives the pointer event.
        if (document.activeElement === input.current) event.preventDefault();
        const waiting = running.current || queue.current.length > 0;
        cancel();
        if (waiting) setNote("Pending typing was cancelled because you touched the desktop. Check the picture before continuing.");
      }
    };
    document.addEventListener("pointerdown", moved, true);
    return () => document.removeEventListener("pointerdown", moved, true);
  }, []);
  useBackLayer(true, () => { if (paste !== null) { setPaste(null); focus(); } else if (more) { setMore(false); focus(); } else onClose(); });
  return <section className="live-keyboard" aria-label="Remote keyboard">
    <p className="visually-hidden" role="status">{enabled ? "This device holds input" : "This device holds input · sending paused"}</p>
    <textarea ref={input} className="live-keyboard__input" aria-label="Type on the computer" readOnly={!enabled} autoComplete="off" autoCorrect="off" autoCapitalize="off" spellCheck={false} enterKeyHint="enter" rows={1}
      onCompositionStart={() => { composing.current = true; cancelledComposition.current = false; }}
      onCompositionEnd={(event) => {
        composing.current = false;
        if (cancelledComposition.current) { resetText(); return; }
        commit(event.currentTarget.value);
      }}
      onKeyDown={(event) => {
        if (event.nativeEvent.isComposing || composing.current || event.key === "Process" || event.keyCode === 229) return;
        if (["Enter", "Backspace", "Tab", "Escape", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown", "Delete"].includes(event.key)) {
          event.preventDefault(); chord(event.key === "Escape" ? "Esc" : event.key);
        } else if (event.key.length === 1 && Object.keys(modifiers.current).length) { event.preventDefault(); chord(event.key); }
      }}
      onPaste={(event) => { event.preventDefault(); cancel(); setPaste(event.clipboardData.getData("text/plain")); }} />
    <div className="live-keyboard__bar" role="group" aria-label="Extra keys" onPointerDown={(event) => { if ((event.target as Element).closest("button")) event.preventDefault(); }}>
      {EXTRA.map(label => <button key={label} type="button" className="live-key" disabled={!enabled} aria-label={label} aria-pressed={MODIFIERS.includes(label) ? !!selected[label] : undefined} data-locked={selected[label] === 2 || undefined} onClick={() => key(label)}>{label}{selected[label] === 2 && <span className="live-key__lock" aria-label="locked">●</span>}</button>)}
      <button type="button" className="live-key" aria-expanded={more} onClick={() => { setMore(!more); focus(); }}>More</button>
      <button type="button" className="live-key" onClick={() => { cancel(); setPaste(""); }}>Paste</button>
      <button type="button" className="live-key" onClick={onClose}>Hide keyboard</button>
    </div>
    {Object.keys(selected).length > 0 && <p className="fine live-keyboard__status" role="status">{Object.entries(selected).map(([label, state]) => `${label} ${state === 2 ? "locked" : "next key"}`).join(" · ")}<button type="button" onPointerDown={event => event.preventDefault()} onClick={() => { clearModifiers(); focus(); }}>Clear</button></p>}
    {note && <p className="fine live-keyboard__status" role="status">{note}</p>}
    {more && <div className="live-keyboard__more" onPointerDown={event => { if ((event.target as Element).closest("button")) event.preventDefault(); }}>
      <div className="live-keyboard__shortcuts" role="group" aria-label="Shortcuts">{CHORDS.map(label => <button className="live-key" key={label} type="button" disabled={!enabled} onClick={() => { resetText(); if (enqueue([keyChord(label)])) consume(); focus(); }}>{label}</button>)}</div>
      <div className="live-keyboard__shortcuts" role="group" aria-label="More keys">{MORE.map(label => <button className="live-key" key={label} type="button" disabled={!enabled} onClick={() => key(label)}>{label}</button>)}</div>
    </div>}
    {paste !== null && <div className="live-keyboard__paste">
      <label>Text to paste<textarea aria-label="Text to paste" value={paste} onChange={event => setPaste(event.target.value)} autoComplete="off" autoCorrect="off" autoCapitalize="off" spellCheck={false} rows={2} /></label>
      <p className="fine">Replaces the computer’s clipboard and sends its paste shortcut to the focused app. This text is not saved on the phone.</p>
      <Button disabled={!enabled || !paste || busy} onClick={() => { if (enqueue([{ action: "paste", text: paste }])) { setPaste(null); focus(); } }}>Paste on computer</Button>
      <Button variant="quiet" onClick={() => { setPaste(null); focus(); }}>Cancel paste</Button>
    </div>}
    {!keyboardOpen && <button className="live-keyboard__resume" type="button" onClick={focus}>Show phone keyboard</button>}
  </section>;
}
