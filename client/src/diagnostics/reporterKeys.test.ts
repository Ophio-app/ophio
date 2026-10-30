// SPDX-License-Identifier: Apache-2.0
import { expect, it, vi } from "vitest";
import { chromium } from "playwright";
import { createServer } from "vite";

it("keeps a non-extractable Ed25519 install key in real IndexedDB across reloads", async () => {
  const process = await vi.importActual<{ cwd(): string }>("node:process");
  const server = await createServer({ configFile: false, root: process.cwd(), cacheDir: process.cwd() + "/node_modules/.vite-reporter-keys", optimizeDeps: { noDiscovery: true, include: [] },
    plugins: [{ name: "reporter-key-test-page", configureServer(server) {
      server.middlewares.use((request, response, next) => {
        if ((request as unknown as { url?: string }).url !== "/key-test") return next();
        response.setHeader("content-type", "text/html"); response.end("<!doctype html><title>Reporter key test</title>");
      });
    } }], server: { host: "127.0.0.1", port: 0, strictPort: true } });
  const browser = await chromium.launch({ headless: true });
  try {
    await server.listen();
    const address = server.httpServer!.address(); if (!address || typeof address === "string") throw new Error("No test address");
    const base = `http://127.0.0.1:${address.port}`;
    const page = await browser.newPage();
    await page.route("**/*", route => route.request().url().startsWith(base + "/") ? route.continue() : route.abort());
    await page.goto(base + "/key-test");
    const check = async () => page.evaluate(async (path) => {
      const { IndexedReporterKeys, base64url } = await new Function("path", "return import(path)")(path);
      const pair = await new IndexedReporterKeys().get();
      if (!pair) throw new Error("Ed25519 unavailable in test browser");
      let exported = false;
      try { await crypto.subtle.exportKey("pkcs8", pair.privateKey); exported = true; } catch { /* Expected for an install key. */ }
      const message = new TextEncoder().encode("obviously fake test message");
      const signature = await crypto.subtle.sign("Ed25519", pair.privateKey, message);
      return { extractable: pair.privateKey.extractable, exported, verified: await crypto.subtle.verify("Ed25519", pair.publicKey, signature, message), publicKey: base64url(new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey))) };
    }, "/src/diagnostics/intakeClient.ts");
    const first = await check(); await page.reload(); const second = await check();
    expect(first).toMatchObject({ extractable: false, exported: false, verified: true }); expect(second).toEqual(first);
    expect(await page.evaluate(async path => {
      await new Promise<void>((resolve, reject) => { const request = indexedDB.deleteDatabase("ophio-reporter"); request.onsuccess = () => resolve(); request.onerror = () => reject(request.error); });
      const { IndexedReporterKeys } = await new Function("path", "return import(path)")(path);
      Object.defineProperty(crypto.subtle, "generateKey", { value: async () => { throw new Error("Ed25519 unsupported"); } });
      return await new IndexedReporterKeys().get();
    }, "/src/diagnostics/intakeClient.ts")).toBeNull();
  } finally { await browser.close(); await server.close(); }
}, 20_000);
