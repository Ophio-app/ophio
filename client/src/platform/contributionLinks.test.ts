// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it } from "vitest";
import { contributionLink, ContributionInbox, parseContributionLink } from "./contributionLinks";
import { Handoffs } from "../screens/contribute/handoffs";

const handoffUrl = "https://catalog.example.com/handoffs/game.json";
const packet = ` { "schemaVersion":1, "kind":"contribution-import", "packetId":"fixture", "issuedAt":100, "expiresAt":700, "handoffSha256":"${"a".repeat(64)}" }\n`;
beforeEach(() => localStorage.clear());
it("carries exact packet bytes and a reference instead of the large handoff", () => {
  const url = contributionLink(packet, handoffUrl);
  expect(url.length).toBeLessThan(1000);
  expect(parseContributionLink(url)).toEqual({ url, packetJson: packet, handoffUrl, expiresAt: 700 });
  const padded = packet.padEnd(65_536, " ");
  expect(parseContributionLink(contributionLink(padded, handoffUrl))?.packetJson).toBe(padded);
  expect(() => contributionLink(padded + " ", handoffUrl)).toThrow();
});
it("rejects ambiguity, credentials, private destinations, unsafe paths and bad packets", () => {
  for (const destination of ["http://catalog.example.com/h.json", "https://127.0.0.1/h.json", "https://private.local/h.json",
    "https://user:pass@catalog.example.com/h.json", "https://catalog.example.com:443/h.json", "https://catalog.example.com/a/../h.json",
    "https://catalog.example.com/h.json?q=x", "https://catalog.example.com/h.json#x", "https://catalog.example.com/a%2fb"]) {
    expect(() => contributionLink(packet, destination), destination).toThrow();
  }
  const url = contributionLink(packet, handoffUrl);
  for (const altered of [url + "&packet=YQ", url + "&extra=x", url + "#data", url.replace("packet=", "packet=%FF"),
    url.replace("packet=", "packet=="), url.replace("open/v1", "open/v2")]) expect(parseContributionLink(altered)).toBeNull();
  for (const bad of [packet.replace('"expiresAt":700', '"expiresAt":701'), packet.replace('"schemaVersion":1', '"schemaVersion":2'),
    packet.replace('"packetId":"fixture"', '"packetId":"../unsafe"'), packet.replace('{', '{"approval":true,'), "\uFEFF" + packet]) {
    expect(() => contributionLink(bad, handoffUrl)).toThrow();
  }
});
it("keeps one pending reference over app restart without extending its expiry", () => {
  const inbox = new ContributionInbox();
  const link = parseContributionLink(contributionLink(packet, handoffUrl));
  inbox.receive(link);
  expect(new ContributionInbox().get()).toEqual(link);
  inbox.receive(null);
  expect(new ContributionInbox().get()).toBeNull();
});
it("bounds received UTF-8 bytes and deduplicates exact records", () => {
  const store = new Handoffs();
  const raw = "{}".padEnd(65_536, " ");
  const first = store.receive(packet, raw);
  expect(first).not.toBeNull();
  expect(store.receive(packet, raw)?.id).toBe(first?.id);
  expect(store.receive(packet, "é".repeat(32_769))).toBeNull();
  expect(store.receive(packet, raw + " ")).toBeNull();
  expect(store.get()).toHaveLength(1);
});
