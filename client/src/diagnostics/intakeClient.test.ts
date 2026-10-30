// SPDX-License-Identifier: Apache-2.0
import { expect, it, vi } from "vitest";
import { IntakeClient, IndexedReporterKeys, IntakeUnavailable, base64url, intakePaths, signingPayload } from "./intakeClient";
import { buildBundle } from "./bundle";

it("matches the Rust signing payload vector", async () => {
  expect(new TextDecoder().decode(await signingPayload("post", "/v1/reports", 1760000000000, "{}"))).toBe("ophio-report-v1\nPOST\n/v1/reports\n1760000000000\n44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a");
});
it("signs exact bodies and paths for every endpoint using the install key", async () => {
  const keys = await crypto.subtle.generateKey("Ed25519", false, ["sign", "verify"]) as CryptoKeyPair;
  expect(keys.privateKey.extractable).toBe(false);
  const fetcher = vi.fn(async (_url: string | URL | Request, _options?: RequestInit) => ({ ok: true, json: async () => ({}) }) as Response);
  const client = new IntakeClient("https://intake.example", { get: async () => keys }, fetcher, () => 1760000000000);
  const bundle = await buildBundle({ salt: "test", origin: { type: "settings" }, context: { screen: "/settings" } });
  await client.submit(bundle); await client.status({ report_ids: [bundle.report_id] });
  await client.answer({ report_id: bundle.report_id, question_id: bundle.report_id, answer: { at: 1, choice: { choice: "yes" } } });
  await client.withdraw({ report_id: bundle.report_id }); await client.releases();
  expect(fetcher).toHaveBeenCalledTimes(5);
  for (let i = 0; i < 5; i++) {
    const [url, options] = fetcher.mock.calls[i]!;
    expect(url).toBe("https://intake.example" + Object.values(intakePaths)[i]);
    expect(options).toMatchObject({ method: i === 4 ? "GET" : "POST", redirect: "error", credentials: "omit" });
    const headers = options!.headers as Record<string, string>;
    expect(headers["ophio-report-time"]).toBe("1760000000000");
    expect(headers["ophio-report-key"]).toBe(base64url(new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey))));
    const signature = Uint8Array.from(atob(headers["ophio-report-signature"]!.replace(/-/g, "+").replace(/_/g, "/")), c => c.charCodeAt(0));
    expect(await crypto.subtle.verify("Ed25519", keys.publicKey, signature, await signingPayload(options!.method!, new URL(String(url)).pathname, 1760000000000, options!.body as string ?? ""))).toBe(true);
  }
  expect(JSON.parse(fetcher.mock.calls[0]![1]!.body as string)).toEqual({ bundle, sender: "phone" });
  const body = fetcher.mock.calls[0]![1]!.body as string; expect(body).not.toContain('"salt"');
});
it("turns off the direct path with no URL, unsafe endpoint, storage, or Ed25519", async () => {
  const get = vi.fn(async () => null), fetcher = vi.fn();
  const off = new IntakeClient("", { get }, fetcher); expect(await off.available()).toBe(false); expect(get).not.toHaveBeenCalled();
  expect(await new IntakeClient("https://intake.example", { get }, fetcher).available()).toBe(false);
  expect(await new IndexedReporterKeys().get()).toBeNull();
  await expect(off.status({ report_ids: [] })).rejects.toBeInstanceOf(IntakeUnavailable); expect(fetcher).not.toHaveBeenCalled();
  for (const url of ["http://remote.example", "https://person:fake@intake.example", "https://intake.example?token=fake"]) expect(new IntakeClient(url).baseUrl).toBe("");
});
it("does not follow redirects or accept HTTP failures", async () => {
  const keys = await crypto.subtle.generateKey("Ed25519", false, ["sign", "verify"]) as CryptoKeyPair;
  const client = new IntakeClient("http://127.0.0.1", { get: async () => keys }, vi.fn(async () => ({ ok: false }) as Response));
  await expect(client.status({ report_ids: [] })).rejects.toThrow("couldn’t answer");
});
