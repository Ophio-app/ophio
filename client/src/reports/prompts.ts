// SPDX-License-Identifier: Apache-2.0
import { SavedDiagnostics } from "../diagnostics/storage";
import type { CrashRecord } from "../protocol/types";

export const CHIP_MS = 6500;
export const PAUSE_MS = 24 * 60 * 60_000;
// Letting the offer fade isn't a no: most screenshots are taken to share. Only
// closing it three times in a row pauses it. v1 also counted fades and muted
// for a week, so its saved pause is left behind.
export class ScreenshotOffer {
  #saved = new SavedDiagnostics<{ ignored: number; until: number }>("reports.screenshot-offer.v2");
  #state = this.#saved.load({ ignored: 0, until: 0 });
  allowed(now = Date.now()) { return now >= this.#state.until; }
  dismiss(now = Date.now()) {
    const ignored = this.#state.ignored + 1;
    this.#state = ignored >= 3 ? { ignored: 0, until: now + PAUSE_MS } : { ignored, until: 0 };
    this.#saved.save(this.#state);
  }
  accept() { this.#state = { ignored: 0, until: 0 }; this.#saved.save(this.#state); }
}
export const crashKey = (c: CrashRecord) => `${c.side}/${c.at}/${c.build}/${c.code}`;
const dismissed = new SavedDiagnostics<string[]>("reports.dismissed-crashes.v1");
export const undismissedCrashes = (records: CrashRecord[]) => { const keys = new Set(dismissed.load([])); return records.filter(c => !keys.has(crashKey(c))); };
export function dismissCrashes(records: CrashRecord[]) { dismissed.save([...new Set([...dismissed.load([]), ...records.map(crashKey)])]); }
