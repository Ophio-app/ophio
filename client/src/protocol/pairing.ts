// SPDX-License-Identifier: Apache-2.0
//
// Pairing links: `<scheme>://pair?d=<base64url(PairingOffer JSON)>`, or the
// same offer packed for the computer's QR code as `<scheme>://pair?c=<base32>`
// (crates/companion/src/cli.rs, `pairing_qr_link`). The scheme is not checked
// here; either payload is validated field by field.
//
// A link is taken exactly as the computer printed it. Only whitespace around
// it and line breaks inside it are ignored, because terminals and chat apps
// wrap long lines when they are copied. Any other change is refused.
//
// `ophio setup` adds `&alerts=on` or `&alerts=off`: the answer its owner
// already gave about alerts, which the app offers as its own first choice.

import type { PairingOffer } from "./types";

function base64UrlDecode(s: string): string {
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(s.length / 4) * 4, "=");
  const bin = atob(b64);
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

export function base64UrlEncode(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

const HEX64 = /^[0-9a-f]{64}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/**
 * Why a pasted or scanned link can't be used: it isn't a pairing link at all,
 * or it is one that lost or changed part of its code on the way.
 */
export type PairingLinkProblem = "not-pairing" | "incomplete";

export type ParsedPairingLink = { offer: PairingOffer; alerts?: boolean } | { problem: PairingLinkProblem };

export function parsePairingLink(input: string): ParsedPairingLink {
  const link = input.replace(/[\r\n]/g, "").trim();
  let url: URL;
  try {
    url = new URL(link);
  } catch {
    return { problem: "not-pairing" };
  }
  const route = (url.host || url.pathname.replace(/^\/+/, "")).toLowerCase();
  if (route !== "pair") return { problem: "not-pairing" };
  // The URL parser would quietly drop a tab and encode a space; either one
  // means the link was changed after it was copied.
  if (/\s/.test(link)) return { problem: "incomplete" };
  const d = url.searchParams.get("d");
  const c = url.searchParams.get("c");
  const offer = c === null ? decodeOffer(d) : d === null ? decodePackedOffer(c) : null;
  if (!offer) return { problem: "incomplete" };
  const alerts = url.searchParams.get("alerts");
  return alerts === "on" || alerts === "off" ? { offer, alerts: alerts === "on" } : { offer };
}

function decodeOffer(d: string | null): PairingOffer | null {
  if (!d) return null;
  try {
    return checkOffer(JSON.parse(base64UrlDecode(d)));
  } catch {
    return null;
  }
}

const BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/** RFC 4648 base32, upper case, no padding; unused bits must be zero. */
function base32Decode(s: string): Uint8Array | null {
  if (!/^[A-Z2-7]+$/.test(s)) return null;
  const bytes = new Uint8Array(Math.floor((s.length * 5) / 8));
  let buffer = 0;
  let bits = 0;
  let at = 0;
  for (const c of s) {
    buffer = ((buffer << 5) | BASE32.indexOf(c)) & 0xfff;
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      bytes[at++] = (buffer >> bits) & 0xff;
    }
  }
  return bits < 5 && (buffer & ((1 << bits) - 1)) === 0 ? bytes : null;
}

/**
 * Format 1: host id (16 bytes), certificate hash (32), pairing secret (32),
 * expiry in milliseconds (i64), protocol major and minor (u16 each), all
 * big-endian; then the host name, an address count and each address, every
 * text a length byte and UTF-8, and nothing after the last address.
 */
function decodePackedOffer(c: string): PairingOffer | null {
  const bytes = base32Decode(c);
  if (!bytes || bytes.length < 95 || bytes[0] !== 1) return null;
  const view = new DataView(bytes.buffer);
  const hex = (from: number, to: number) =>
    Array.from(bytes.subarray(from, to), (b) => b.toString(16).padStart(2, "0")).join("");
  // Every text keeps a leading byte-order mark, as the link's JSON form does.
  const utf8 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
  let at = 93;
  const text = () => {
    const end = at + 1 + (bytes[at] ?? 0);
    if (at >= bytes.length || end > bytes.length) throw new RangeError("short");
    const value = utf8.decode(bytes.subarray(at + 1, end));
    at = end;
    return value;
  };
  try {
    const id = hex(1, 17);
    const expires = view.getBigInt64(81);
    const host_name = text();
    const count = bytes[at++] ?? 0;
    const addresses = Array.from({ length: count }, text);
    if (at !== bytes.length || expires > BigInt(Number.MAX_SAFE_INTEGER) || expires < BigInt(Number.MIN_SAFE_INTEGER)) {
      return null;
    }
    let secret = "";
    for (const b of bytes.subarray(49, 81)) secret += String.fromCharCode(b);
    return checkOffer({
      host_id: `${id.slice(0, 8)}-${id.slice(8, 12)}-${id.slice(12, 16)}-${id.slice(16, 20)}-${id.slice(20)}`,
      host_name,
      addresses,
      cert_sha256: hex(17, 49),
      pairing_secret: btoa(secret).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, ""),
      expires_at: Number(expires),
      protocol: { major: view.getUint16(89), minor: view.getUint16(91) },
    });
  } catch {
    return null;
  }
}

function checkOffer(o: unknown): PairingOffer | null {
  if (typeof o !== "object" || o === null) return null;
  const r = o as Record<string, unknown>;
  const protocol = r.protocol as Record<string, unknown> | undefined;
  if (
    typeof r.host_id !== "string" ||
    !UUID.test(r.host_id) ||
    typeof r.host_name !== "string" ||
    !Array.isArray(r.addresses) ||
    r.addresses.length === 0 ||
    !r.addresses.every((a) => typeof a === "string" && a.length > 0) ||
    typeof r.cert_sha256 !== "string" ||
    !HEX64.test(r.cert_sha256) ||
    typeof r.pairing_secret !== "string" ||
    r.pairing_secret.length < 16 ||
    typeof r.expires_at !== "number" ||
    typeof protocol?.major !== "number" ||
    typeof protocol?.minor !== "number"
  ) {
    return null;
  }
  return {
    host_id: r.host_id,
    host_name: r.host_name,
    addresses: r.addresses as string[],
    cert_sha256: r.cert_sha256,
    pairing_secret: r.pairing_secret,
    expires_at: r.expires_at,
    protocol: { major: protocol.major, minor: protocol.minor },
  };
}

/**
 * The part of a certificate fingerprint a person compares by eye: its first
 * 128 bits, as eight groups of four hex digits, exactly as `ophio pair`
 * prints them.
 */
export function fingerprintWords(hex: string): string {
  return (hex.slice(0, 32).match(/.{4}/g) ?? []).join(" ").toUpperCase();
}
