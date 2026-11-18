// SPDX-License-Identifier: Apache-2.0
// Real browser decoders, not jsdom's image mocks. Run from client with
// node scripts/image-decode-check.mjs (CHROMIUM_PATH may select a browser).
import assert from "node:assert/strict";
import { chromium } from "playwright";
import { createServer } from "vite";

const server = await createServer({
  mode: "sim",
  server: { host: "127.0.0.1", port: 0, strictPort: false, open: false },
});
server.middlewares.use("/__image_decode_check__", (_req, res) => {
  res.setHeader("Content-Type", "text/html");
  res.end("<!doctype html><title>Image decoder check</title>");
});
let browser;
try {
  await server.listen();
  browser = await chromium.launch({
    headless: true,
    ...(process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {}),
  });
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__image_decode_check__`);
  const result = await page.evaluate(async () => {
    const { imageSize, importSource, deriveStill } = await import("/src/screens/annotate/sources.ts");
    const { imageSize: displayedSize } = await import("/src/screens/work/pictures.ts");
    const { annotationStore, blobs } = await import("/src/state/annotations.ts");
    const { sha256Hex } = await import("/src/host/transfer.ts");
    const store = annotationStore("image-decoder-check");
    const draft = store.create("project", null);
    const results = [];
    // SVG is accepted by image/* and by the drawing's image element, but
    // Chromium's createImageBitmap rejects SVG blobs, even with explicit size.
    for (const dimensions of ['width="32" height="20"', 'viewBox="0 0 32 20"']) {
      const file = new File([
        `<svg xmlns="http://www.w3.org/2000/svg" ${dimensions}><rect width="32" height="20" fill="red"/></svg>`,
      ], "drawing.svg", { type: "image/svg+xml" });
      const shown = await displayedSize(file);
      try {
        const attached = await importSource(store, draft.id, {
          blob: file, mediaType: file.type,
          origin: { kind: "phone_import", original_name: file.name },
        });
        const cropped = await deriveStill(store, draft.id, attached, { kind: "crop", x: 0, y: 0, w: 0.5, h: 1 });
        const cropBlob = await blobs.get(cropped.blobKey);
        results.push({
          dimensions, shown, attached: { width: attached.width, height: attached.height },
          unchanged: attached.sha256 === await sha256Hex(file),
          cropped: await imageSize(cropBlob), cropType: cropBlob.type,
        });
      } catch (error) {
        results.push({ dimensions, shown, error: error.message });
      }
    }
    for (const [name, bytes] of [["empty", []], ["truncated-png", [137, 80, 78, 71, 13, 10, 26, 10]]]) {
      try {
        await imageSize(new Blob([new Uint8Array(bytes)], { type: "image/png" }));
        results.push({ name, rejected: false });
      } catch { results.push({ name, rejected: true }); }
    }
    return results;
  });
  console.log(JSON.stringify({ browser: await browser.version(), result }, null, 2));
  for (const check of result) {
    if (check.name) assert.equal(check.rejected, true, check.name);
    else {
      assert.equal(check.error, undefined, check.dimensions);
      assert.deepEqual(check.attached, check.shown);
      assert.equal(check.unchanged, true);
      assert.deepEqual(check.cropped, { width: check.shown.width / 2, height: check.shown.height });
      assert.equal(check.cropType, "image/png");
    }
  }
} finally {
  await browser?.close();
  await server.close();
}
