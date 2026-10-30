// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { base64UrlEncode, fingerprintWords, parsePairingLink } from "./pairing";
import type { PairingOffer } from "./types";

const offer: PairingOffer = {
  host_id: "0f8b6c1e-2d3a-4b5c-8d9e-0a1b2c3d4e5f",
  host_name: "Desk",
  addresses: ["192.168.1.20:47100"],
  cert_sha256: "ab".repeat(32),
  pairing_secret: "c2VjcmV0LXNlY3JldC1zZWNyZXQ",
  expires_at: 1_900_000_000_000,
  protocol: { major: 1, minor: 0 },
};
const link = `ophio://pair?d=${base64UrlEncode(JSON.stringify(offer))}`;

describe("parsePairingLink", () => {
  it("reads the link the computer prints", () => {
    expect(parsePairingLink(link)).toEqual({ offer });
  });

  it.each([
    ["spaces and a line break around it", `  ${link}\n`],
    ["tabs and a Windows line ending around it", `\t${link}\r\n`],
    ["a line break where a terminal wrapped it", `${link.slice(0, 30)}\n${link.slice(30)}`],
    ["a Windows line break inside it", `${link.slice(0, 50)}\r\n${link.slice(50)}`],
  ])("ignores %s", (_case, input) => {
    expect(parsePairingLink(input)).toEqual({ offer });
  });

  it.each([
    ["on", { offer, alerts: true }],
    ["off", { offer, alerts: false }],
    ["yes", { offer }],
    ["", { offer }],
  ])("carries setup's alerts answer %j only when it is on or off", (value, expected) => {
    expect(parsePairingLink(`${link}&alerts=${value}`)).toEqual(expected);
  });

  it.each([
    ["a space inside the code", `${link.slice(0, 40)} ${link.slice(40)}`],
    ["a tab inside the code", `${link.slice(0, 40)}\t${link.slice(40)}`],
    ["a missing end", link.slice(0, -6)],
    ["no code at all", "ophio://pair"],
    ["an empty code", "ophio://pair?d="],
  ])("calls a pairing link with %s incomplete", (_case, input) => {
    expect(parsePairingLink(input)).toEqual({ problem: "incomplete" });
  });

  it.each([
    ["nothing", ""],
    ["plain words", "hello there"],
    ["a web address", "https://example.com/pair?d=abc"],
    ["another app's link", "ophio://settings?d=abc"],
    ["only the code", link.slice(link.indexOf("d=") + 2)],
  ])("says %s isn't a pairing link", (_case, input) => {
    expect(parsePairingLink(input)).toEqual({ problem: "not-pairing" });
  });

  it("refuses a code whose fields are wrong", () => {
    const bad = { ...offer, cert_sha256: "not-a-fingerprint" };
    expect(parsePairingLink(`ophio://pair?d=${base64UrlEncode(JSON.stringify(bad))}`)).toEqual({ problem: "incomplete" });
  });
});

/** The computer's packed form (crates/companion/src/cli.rs, `pairing_qr_link`). */
function pack(offer: PairingOffer): string {
  const bytes: number[] = [1];
  const hex = (h: string) => (h.match(/../g) ?? []).map((b) => parseInt(b, 16));
  bytes.push(...hex(offer.host_id.replace(/-/g, "")), ...hex(offer.cert_sha256));
  bytes.push(...Array.from(atob(offer.pairing_secret.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0)));
  const fixed = new DataView(new ArrayBuffer(12));
  fixed.setBigInt64(0, BigInt(offer.expires_at));
  fixed.setUint16(8, offer.protocol.major);
  fixed.setUint16(10, offer.protocol.minor);
  bytes.push(...new Uint8Array(fixed.buffer));
  const text = (s: string) => {
    const b = new TextEncoder().encode(s);
    bytes.push(b.length, ...b);
  };
  text(offer.host_name);
  bytes.push(offer.addresses.length);
  offer.addresses.forEach(text);
  let out = "";
  let buffer = 0;
  let bits = 0;
  for (const b of bytes) {
    buffer = ((buffer << 8) | b) & 0xfff;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      out += "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"[(buffer >> bits) & 31];
    }
  }
  if (bits > 0) out += "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"[(buffer << (5 - bits)) & 31];
  return `ophio://pair?c=${out}`;
}

describe("parsePairingLink on the QR code's packed form", () => {
  // The same text as the computer's test, so both sides agree on the format.
  const packed =
    "ophio://pair?c=AEHYW3A6FU5EWXENTYFBWLB5JZPV6HE2BZ6TWKSMN2HQUGZ4LV7J6KSLNSGQ4HZ2LN6J2HR7LJ5ZYDIAAEBAGBAFAYDQQCIKBMGA2DQPCAIREEYUCULBOGAZDINRYHI6D4AAAAN2MDJTQAAAAEAAECCCYO6HE3ZNKBBQEERRHEZC4MJWHAXDCLRSGA5DINZRGAYBSZDFONVS4ZLYMFWXA3DFFZ2HGLTOMV2DUNBXGEYDA";
  const computers: PairingOffer = {
    host_id: "0f8b6c1e-2d3a-4b5c-8d9e-0a1b2c3d4e5f",
    host_name: "Büro-PC",
    addresses: ["192.168.1.20:47100", "desk.example.ts.net:47100"],
    cert_sha256: "5f1c9a0e7d3b2a4c6e8f0a1b3c5d7e9f2a4b6c8d0e1f3a5b7c9d1e3f5a7b9c0d",
    pairing_secret: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
    expires_at: 1_900_000_000_000,
    protocol: { major: 1, minor: 2 },
  };

  it("reads the code the computer shows", () => {
    expect(parsePairingLink(packed)).toEqual({ offer: computers });
    expect(pack(computers)).toBe(packed);
  });

  it("reads the same offer from either form", () => {
    const full = `ophio://pair?d=${base64UrlEncode(JSON.stringify(computers))}`;
    expect(parsePairingLink(full)).toEqual(parsePairingLink(packed));
  });

  it.each([
    ["both forms at once", `${packed}&d=${base64UrlEncode(JSON.stringify(computers))}`],
    ["lower case", packed.replace(/c=.*$/, (code) => code.toLowerCase())],
    ["a character outside base32", `${packed.slice(0, -1)}1`],
    ["a missing end", packed.slice(0, -8)],
    ["one character too many", `${packed}A`],
    ["spare bits that are not zero", `${packed.slice(0, -1)}B`],
    ["an empty code", "ophio://pair?c="],
    ["a later format", packed.replace("?c=AE", "?c=AI")],
  ])("calls a packed code with %s incomplete", (_case, input) => {
    expect(parsePairingLink(input)).toEqual({ problem: "incomplete" });
  });

  it("reads a packed offer with an empty name, and refuses one without addresses or a usable expiry", () => {
    const unnamed = { ...computers, host_name: "" };
    expect(parsePairingLink(pack(unnamed))).toEqual({ offer: unnamed });
    expect(parsePairingLink(pack({ ...computers, addresses: [] }))).toEqual({ problem: "incomplete" });
    expect(parsePairingLink(pack({ ...computers, addresses: [""] }))).toEqual({ problem: "incomplete" });
    expect(parsePairingLink(pack({ ...computers, expires_at: 2 ** 53 }))).toEqual({ problem: "incomplete" });
  });

  it("keeps a byte-order mark at the start of a name or address, as the link's JSON form does", () => {
    const marked = { ...computers, host_name: "\uFEFFDesk", addresses: ["\uFEFF192.168.1.20:47100"] };
    const full = `ophio://pair?d=${base64UrlEncode(JSON.stringify(marked))}`;
    expect(parsePairingLink(pack(marked))).toEqual({ offer: marked });
    expect(parsePairingLink(pack(marked))).toEqual(parsePairingLink(full));
  });

  it("reads back generated offers and answers damaged ones without throwing", () => {
    const names = ["Desk", "Büro-PC", "工作站", "Laptop 😀", "a\u202eb", "x".repeat(255), ""];
    for (let seed = 1; seed <= 300; seed++) {
      const random = generator(seed);
      const offer: PairingOffer = {
        host_id: Array.from({ length: 32 }, () => "0123456789abcdef"[random(16)])
          .join("")
          .replace(/^(.{8})(.{4})(.{4})(.{4})/, "$1-$2-$3-$4-"),
        host_name: names[random(names.length)]!,
        addresses: Array.from({ length: 1 + random(4) }, (_, i) => `host-${i}.example:${1 + random(65535)}`),
        cert_sha256: Array.from({ length: 64 }, () => "0123456789abcdef"[random(16)]).join(""),
        pairing_secret: btoa(String.fromCharCode(...Array.from({ length: 32 }, () => random(256))))
          .replace(/\+/g, "-")
          .replace(/\//g, "_")
          .replace(/=+$/, ""),
        expires_at: 1_900_000_000_000 + random(1_000_000),
        protocol: { major: random(3), minor: random(70000) % 65536 },
      };
      const link = pack(offer);
      expect(parsePairingLink(link), `seed ${seed}`).toEqual({ offer });
      let damaged = link;
      for (let edits = 1 + random(4); edits > 0; edits--) {
        const at = random(damaged.length + 1);
        const c = "AZ27a01=&?"[random(10)]!;
        damaged = random(3) === 0 ? damaged.slice(0, at) + damaged.slice(at + 1) : damaged.slice(0, at) + c + damaged.slice(at);
      }
      const read = parsePairingLink(damaged);
      expect("offer" in read || "problem" in read, damaged).toBe(true);
    }
  });
});

describe("fingerprintWords", () => {
  // crates/companion/src/cli.rs checks that this line matches what `ophio pair` prints.
  it("shows the first 128 bits in eight groups, as the computer prints them", () => {
    expect(fingerprintWords("5f1c9a0e7d3b2a4c6e8f0a1b3c5d7e9f2a4b6c8d0e1f3a5b7c9d1e3f5a7b9c0d")).toBe("5F1C 9A0E 7D3B 2A4C 6E8F 0A1B 3C5D 7E9F");
  });
});

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

// The computer prints the link and a person carries it over by any means:
// scanned, pasted, wrapped by a terminal or a chat app. Generated offers and
// damage check what the parser promises for all of them.
describe("parsePairingLink on generated links", () => {
  const names = ["Desk", "Büro-PC", "工作站", "Laptop 😀", "a\u202eb", "x".repeat(300), ""];
  const offerFor = (random: (below: number) => number): PairingOffer => ({
    host_id: "0f8b6c1e-2d3a-4b5c-8d9e-0a1b2c3d4e5f",
    host_name: names[random(names.length)]!,
    addresses: Array.from({ length: 1 + random(4) }, (_, i) => `host-${i}.example:${1 + random(65535)}`),
    cert_sha256: Array.from({ length: 64 }, () => "0123456789abcdef"[random(16)]).join(""),
    pairing_secret: base64UrlEncode(String.fromCharCode(...Array.from({ length: 32 }, () => random(256)))),
    expires_at: 1_900_000_000_000 + random(1_000_000),
    protocol: { major: 1, minor: random(3) },
  });

  it("reads back every offer the computer can print, wrapped anywhere", () => {
    for (let seed = 1; seed <= 300; seed++) {
      const random = generator(seed);
      const offer = offerFor(random);
      const link = `ophio://pair?d=${base64UrlEncode(JSON.stringify(offer))}`;
      expect(parsePairingLink(link), `seed ${seed}`).toEqual({ offer });
      let wrapped = link;
      for (let breaks = random(4); breaks > 0; breaks--) {
        const at = random(wrapped.length + 1);
        wrapped = wrapped.slice(0, at) + (random(2) ? "\n" : "\r\n") + wrapped.slice(at);
      }
      expect(parsePairingLink(`  ${wrapped}\t`), `seed ${seed}`).toEqual({ offer });
    }
  });

  it("answers any damaged or foreign text without throwing", () => {
    const alphabet = "Aa9-_+/=%&?#:\\ \t\n\u0000é😀";
    for (let seed = 1; seed <= 2000; seed++) {
      const random = generator(seed);
      let text = `ophio://pair?d=${base64UrlEncode(JSON.stringify(offerFor(random)))}`;
      for (let edits = 1 + random(4); edits > 0; edits--) {
        const at = random(text.length + 1);
        const c = [...alphabet][random([...alphabet].length)]!;
        text = random(3) === 0 ? text.slice(0, at) + text.slice(at + 1) : text.slice(0, at) + c + text.slice(at);
      }
      const read = parsePairingLink(text);
      expect("offer" in read || "problem" in read, text).toBe(true);
    }
  });
});
