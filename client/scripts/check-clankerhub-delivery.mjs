// SPDX-License-Identifier: Apache-2.0
// Read-only interop check against an independently checked-out Clankerhub.
// Uses synthetic catalog authority; not a deployed catalog or phone pilot.
// node client/scripts/check-clankerhub-delivery.mjs /path/to/clankerhub
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createServer } from "vite";

if (!process.argv[2]) throw new Error("Pass the Clankerhub checkout to inspect");
const upstream = resolve(process.argv[2]);
const load = file => import(pathToFileURL(resolve(upstream, file)).href);
const { buildImport } = await load("src/core/handoff/index.ts");
const { decodeCatalog, digest, releaseKey } = await load("src/core/catalog/loading.ts");
const fixture = async name => JSON.parse(await readFile(resolve(upstream, `fixtures/catalog/${name}.json`), "utf8"));
const handoff = await fixture("handoff");
handoff.compatibility.protocolVersion = "ch-import-draft-1";
const handoffJson = ` \r\n${JSON.stringify(handoff, null, 2)}\r\n`;
const encode = value => new TextEncoder().encode(value);
const release = await fixture("browser");
release.handoff.sha256 = await digest(encode(handoffJson));
const key = releaseKey(release);
const catalog = encode(JSON.stringify({ schemaVersion: 1, records: [JSON.stringify(release)] }));
const session = await decodeCatalog(catalog, {
  url: "https://catalog.example/catalog.json", sha256: await digest(catalog),
  issuedAt: 100, expiresAt: 1000, allowedSourceOrigins: ["https://source.example"],
  allowedPlayOrigins: ["https://play.example"], revoked: [], references: { [key]: "current" },
}, () => 150);
const request = await buildImport(session, key, encode(handoffJson), 150);
const packetJson = JSON.stringify(request.packet);
const vite = await createServer({ root: resolve(dirname(fileURLToPath(import.meta.url)), ".."), server: { middlewareMode: true } });
try {
  const { contributionLink, parseContributionLink } = await vite.ssrLoadModule("/src/platform/contributionLinks.ts");
  const url = contributionLink(packetJson, "https://catalog.example/handoffs/fixture.json");
  const parsed = parseContributionLink(url);
  assert.equal(parsed.packetJson, packetJson);
  assert.equal(parsed.expiresAt, 750);
  assert.equal(request.handoffJson, handoffJson);
  assert.equal(JSON.parse(parsed.packetJson).handoffSha256, await digest(encode(request.handoffJson)));
  assert.ok(url.length < 1200, "ordinary CH-060 packet/reference should fit the QR budget");
  session.invalidate();
  await assert.rejects(buildImport(session, key, encode(handoffJson), 150), { code: "freshness_unknown" });
  console.log(`PASS: Clankerhub buildImport → Ophio link parser; exact packet and handoff bytes, freshness refusal; ${url.length}-byte QR link for ${encode(handoffJson).length}-byte handoff.`);
} finally { await vite.close(); }
