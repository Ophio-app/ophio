// SPDX-License-Identifier: Apache-2.0

/** RFC 9562 UUIDv7: time-ordered, matching the host's id generation. */
export function uuidv7(now: number = Date.now()): string {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  const ms = BigInt(now);
  for (let i = 0; i < 6; i++) b[i] = Number((ms >> BigInt(8 * (5 - i))) & 0xffn);
  b[6] = (b[6]! & 0x0f) | 0x70;
  b[8] = (b[8]! & 0x3f) | 0x80;
  const h = [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/** When a UUIDv7 was made, or null for any other id. */
export function uuidv7Time(id: string): number | null {
  const hex = id.replace(/-/g, "");
  return /^[0-9a-f]{12}7[0-9a-f]{19}$/i.test(hex) ? parseInt(hex.slice(0, 12), 16) : null;
}
