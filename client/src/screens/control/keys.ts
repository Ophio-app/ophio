// SPDX-License-Identifier: Apache-2.0
import type { InputAction } from "../../protocol/types";

const NAMES: Record<string, string> = {
  Ctrl: "ctrl",
  Alt: "alt",
  Super: "super",
  Shift: "shift",
  Esc: "Escape",
  Tab: "Tab",
  Enter: "Enter",
  Backspace: "Backspace",
  "←": "ArrowLeft",
  "↑": "ArrowUp",
  "→": "ArrowRight",
  "↓": "ArrowDown",
};
export const COMMON_KEYS = [
  "Esc",
  "Tab",
  "Ctrl",
  "Alt",
  "Super",
  "←",
  "↑",
  "↓",
  "→",
  "Enter",
  "Backspace",
];
export const FUNCTION_KEYS = Array.from({ length: 12 }, (_, i) => `F${i + 1}`);
export const CHORDS = ["Ctrl+C", "Ctrl+V", "Ctrl+Z"];
export function keyChord(label: string): InputAction {
  return {
    action: "key_chord",
    keys: label
      .split("+")
      .map(
        (key) => NAMES[key] ?? (/^F\d+$/.test(key) ? key : key.toLowerCase()),
      ),
  };
}

/** Terminal control sequences, independent of desktop key names. */
export function terminalKey(label: string): string | null {
  const keys: Record<string, string> = {
    Esc: "\x1b",
    Tab: "\t",
    Enter: "\r",
    Backspace: "\x7f",
    "←": "\x1b[D",
    "↑": "\x1b[A",
    "→": "\x1b[C",
    "↓": "\x1b[B",
    "Ctrl+C": "\x03",
    "Ctrl+V": "\x16",
    "Ctrl+Z": "\x1a",
  };
  return keys[label] ?? null;
}
