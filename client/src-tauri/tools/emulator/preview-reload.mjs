// SPDX-License-Identifier: Apache-2.0
// Checks a separate, unprivileged preview WebView in the isolated emulator.
import fs from "node:fs";
import { fileURLToPath } from "node:url";
const repo = fileURLToPath(new URL("../../../../", import.meta.url));
const port = Number(
  fs.readFileSync(`${repo}/target/preview-e2e/cdp-port`, "utf8"),
);
const targets = await (
  await fetch(`http://127.0.0.1:${port}/json/list`)
).json();
const target = targets.find(
  (t) => t.type === "page" && t.url.startsWith("http://127.0.0.1:"),
);
if (!target) throw new Error("Private preview WebView missing");
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
  ws.onopen = resolve;
  ws.onerror = reject;
});
let id = 0;
const pending = new Map();
ws.onmessage = ({ data }) => {
  const response = JSON.parse(data),
    p = pending.get(response.id);
  if (p) {
    pending.delete(response.id);
    clearTimeout(p.timer);
    response.error
      ? p.reject(new Error(response.error.message))
      : p.resolve(response.result);
  }
};
const send = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const n = ++id;
    pending.set(n, {
      resolve,
      reject,
      timer: setTimeout(() => reject(new Error("Preview CDP timeout")), 10000),
    });
    ws.send(JSON.stringify({ id: n, method, params }));
  });
const evaluate = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true });
  if (r.exceptionDetails)
    throw new Error(
      r.exceptionDetails.exception?.description?.split("\n")[0] ??
        "Preview script failed",
    );
  return r.result.value;
};
try {
  for (let n = 0; n < 100; n++) {
    if (
      await evaluate(
        "document.querySelector('#message')?.textContent === 'Before hot reload'",
      )
    )
      break;
    await new Promise((r) => setTimeout(r, 100));
  }
  const before = await evaluate(
    "({text:document.querySelector('#message')?.textContent,bridge:typeof window.__TAURI_INTERNALS__,cookie:document.cookie})",
  );
  if (
    before.text !== "Before hot reload" ||
    before.bridge !== "undefined" ||
    before.cookie.includes("preview_")
  )
    throw new Error("Preview isolation or initial page failed");
  fs.writeFileSync(
    `${repo}/target/preview-fixture/message.ts`,
    "export const message = 'After hot reload';\n",
  );
  let updated = false;
  for (let n = 0; n < 100; n++) {
    await new Promise((r) => setTimeout(r, 150));
    if (
      await evaluate(
        "document.querySelector('#message')?.textContent === 'After hot reload'",
      )
    ) {
      updated = true;
      break;
    }
  }
  if (!updated)
    throw new Error("Vite hot reload did not cross the preview WebSocket");
  const screen = await send("Page.captureScreenshot", { format: "png" });
  fs.writeFileSync(
    `${repo}/target/preview-results/hot-reload.png`,
    Buffer.from(screen.data, "base64"),
  );
  const result = {
    http: true,
    websocketHotReload: true,
    privilegedBridgeAbsent: true,
    gateCookieNotScriptReadable: true,
  };
  fs.writeFileSync(
    `${repo}/target/preview-results/result.json`,
    JSON.stringify(result, null, 2) + "\n",
  );
  console.log(result);
} finally {
  ws.close();
  for (const p of pending.values()) clearTimeout(p.timer);
}
