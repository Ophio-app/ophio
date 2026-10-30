// SPDX-License-Identifier: Apache-2.0
import type { DeviceRecord } from "../../protocol/types";

/** Mirrors the computer's review threshold: owners pair a tablet beside a phone,
 * so a pairing alone says nothing about the other devices. A device still in
 * use reconnects within days; two weeks of silence is a real gap. */
export const REVIEW_AFTER_MS = 14 * 24 * 60 * 60 * 1000;

/** When an active record was last seen (or paired, if it never connected),
 * once that is at least two weeks before `now` and another device has paired
 * since. An observation gap, never proof that two records represent one install. */
export function notSeenSince(device: DeviceRecord, devices: readonly DeviceRecord[], now: number): number | null {
  if (device.revoked_at !== null || device.is_current) return null;
  const seen = device.last_seen_at ?? device.paired_at;
  const replaced = devices.some(d => d.device_id !== device.device_id && d.paired_at > seen);
  return now - seen >= REVIEW_AFTER_MS && replaced ? seen : null;
}
