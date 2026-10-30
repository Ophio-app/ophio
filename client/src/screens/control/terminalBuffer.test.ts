// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { terminalFrame } from "../../protocol/binary";
import { TerminalOffset, TerminalReplayError } from "./terminalBuffer";

/** xorshift32: the same cases on every run. */
function generator(seed: number) {
  let state = seed || 1;
  return (below: number) => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) % below;
  };
}

// Terminal output is whatever a program prints, cut into frames wherever the
// computer's reads end, replayed after every reattach and sometimes overlapping
// what the phone already shows. Across any such sequence, what the phone shows
// is the output exactly once, in order, from the last point it was told the
// computer no longer retains.
describe("terminal output across generated reattaches", () => {
  it("shows every byte once and in order", () => {
    for (let seed = 1; seed <= 400; seed++) {
      const random = generator(seed);
      const output = Uint8Array.from({ length: random(600) }, () => random(256));
      const offset = new TerminalOffset();
      let shown: number[] = [];
      let start = 0;
      offset.attach(0, false);
      while (offset.next! < output.length) {
        const next = offset.next!;
        const step = random(10);
        if (step === 0) {
          // Reattach at the next byte, or later when earlier output is gone.
          const lost = random(4) === 0 ? 1 + random(output.length - next) : 0;
          offset.attach(next + lost, lost > 0);
          if (lost > 0) {
            shown = [];
            start = next + lost;
          }
          continue;
        }
        // A frame starting at or before the next byte, possibly one already shown.
        const from = Math.max(0, next - random(40));
        const to = Math.min(output.length, from + 1 + random(80));
        shown.push(...offset.accept(terminalFrame(from, output.subarray(from, to))));
      }
      expect(shown, `seed ${seed}`).toEqual([...output.subarray(start)]);
      expect(offset.next).toBe(output.length);
    }
  });

  it("refuses a frame that leaves a gap", () => {
    for (let seed = 1; seed <= 100; seed++) {
      const random = generator(seed);
      const offset = new TerminalOffset();
      offset.attach(random(1000), false);
      const ahead = offset.next! + 1 + random(1000);
      expect(() => offset.accept(terminalFrame(ahead, new Uint8Array([1])))).toThrow(TerminalReplayError);
      expect(() => offset.attach(ahead, false)).toThrow(TerminalReplayError);
    }
  });
});
