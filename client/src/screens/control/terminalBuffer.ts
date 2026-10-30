// SPDX-License-Identifier: Apache-2.0
import { splitTerminalFrame } from "../../protocol/binary";

export class TerminalReplayError extends Error {}
export class TerminalOffset {
  next: number | null = null;
  attach(replayFrom: number, truncated: boolean): void {
    if (!Number.isSafeInteger(replayFrom) || replayFrom < 0)
      throw new TerminalReplayError("Invalid terminal replay offset.");
    if (this.next === null || truncated) this.next = replayFrom;
    else if (replayFrom > this.next)
      throw new TerminalReplayError(
        "Some terminal output is missing. Detach and attach again to review the retained output.",
      );
  }
  accept(frame: Uint8Array): Uint8Array {
    const part = splitTerminalFrame(frame);
    if (!part || !Number.isSafeInteger(part.offset) || this.next === null)
      throw new TerminalReplayError("Invalid terminal output frame.");
    if (part.offset > this.next)
      throw new TerminalReplayError(
        "Terminal output has a gap. Reattach to recover it.",
      );
    const skip = Math.min(part.bytes.length, this.next - part.offset);
    this.next = Math.max(this.next, part.offset + part.bytes.length);
    return part.bytes.subarray(skip);
  }
}

/** Prevent a paste from escaping bracketed-paste mode or injecting controls. */
export function cleanPaste(text: string): string {
  return text.replace(/\r\n?/g, "\n").replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "");
}

/** A closing newline is the Enter after the last line, not a line of its own. */
export function pasteLines(text: string): string {
  const lines = cleanPaste(text).replace(/\n$/, "").split("\n").length;
  return lines === 1 ? "1 line" : `${lines} lines`;
}
