// SPDX-License-Identifier: Apache-2.0
// Macs and Windows treat Inbox and inbox as one name, so an import of "./Inbox"
// there can load inbox.ts instead of Inbox.tsx. No two modules in a folder may
// share a name that differs only in capitals, whatever their extensions.
// node scripts/case-check.mjs [source directory]
import { readdirSync } from "node:fs";
import { join } from "node:path";

const moduleName = /^(.*)\.(?:ts|tsx|js|jsx|mjs)$/;
const clashes = [];

function check(directory) {
  const seen = new Map();
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      check(join(directory, entry.name));
      continue;
    }
    const stem = moduleName.exec(entry.name)?.[1];
    if (stem === undefined) continue;
    const key = stem.toLowerCase();
    const earlier = seen.get(key);
    if (earlier !== undefined && earlier.stem !== stem) clashes.push(`${join(directory, earlier.name)} and ${entry.name}`);
    seen.set(key, { stem, name: entry.name });
  }
}

check(process.argv[2] ?? "src");
if (clashes.length > 0) {
  console.error(`These modules differ only in capitals, which breaks the build on a Mac:\n  ${clashes.join("\n  ")}`);
  process.exit(1);
}
console.log("Module names OK on case-insensitive disks");
