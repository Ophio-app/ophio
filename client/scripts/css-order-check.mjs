// SPDX-License-Identifier: Apache-2.0
// After `vite build`: the page loads all of the app-wide styles from one file,
// in main.tsx's order. When a stylesheet main.tsx imports is also imported by
// another module, the bundler can move it into a chunk the page loads first,
// and then the shared component rules after it win: Talk's Send button took
// the plain icon button's size and position and left the message box.
// node scripts/css-order-check.mjs [dist directory]
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const dist = process.argv[2] ?? "dist";
const html = readFileSync(join(dist, "index.html"), "utf8");
const sheets = [...html.matchAll(/<link\b[^>]*rel="stylesheet"[^>]*href="([^"]+)"/g)].map((m) => m[1]);
assert.equal(sheets.length, 1, `index.html should load one stylesheet, not ${sheets.join(", ")}. Import the app-wide styles from main.tsx only.`);
const css = readFileSync(join(dist, sheets[0].replace(/^\//, "")), "utf8");
const after = (later, earlier) => css.indexOf(later) > css.indexOf(earlier) && css.indexOf(earlier) >= 0;
assert(after(".composer__send{", ".iconbtn{"), "Talk's styles should come after the shared components'");
console.log(`CSS order OK: ${sheets[0]}`);
