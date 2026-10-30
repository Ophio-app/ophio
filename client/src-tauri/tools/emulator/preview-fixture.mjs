// SPDX-License-Identifier: Apache-2.0
import fs from "node:fs";
import { fileURLToPath } from "node:url";
const root = fileURLToPath(
  new URL("../../../../target/preview-fixture/", import.meta.url),
);
fs.mkdirSync(root, { recursive: true });
fs.mkdirSync(
  new URL("../../../../target/preview-results/", import.meta.url),
  { recursive: true },
);
// Canonical /@fs paths are necessary for Vite to match accepted HMR dependencies.
const entry = `/@fs${root}entry.ts`;
fs.writeFileSync(
  `${root}index.html`,
  `<!doctype html><html lang="en"><head><meta name="viewport" content="width=device-width, initial-scale=1"><title>Preview check</title></head><body><h1>Private preview</h1><p id="message"></p><script type="module" src="/@vite/client"></script><script type="module" src="${entry}"></script></body></html>`,
);
fs.writeFileSync(
  `${root}entry.ts`,
  `import {message} from './message.ts';\ndocument.querySelector('#message').textContent=message;\nif(import.meta.hot)import.meta.hot.accept('./message.ts', next=>{document.querySelector('#message').textContent=next.message;});\n`,
);
fs.writeFileSync(
  `${root}message.ts`,
  "export const message = 'Before hot reload';\n",
);
console.log(
  "Created a local Vite fixture; no agent session or desktop access.",
);
