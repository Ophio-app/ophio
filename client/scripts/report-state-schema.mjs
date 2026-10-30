// SPDX-License-Identifier: Apache-2.0
// Read only generated declarations; string data never contributes to this list.
import { readdir, readFile, writeFile } from 'node:fs/promises';
const root = new URL('../src/', import.meta.url);
const generated = new URL('protocol/generated/', root);
const texts = await Promise.all((await readdir(generated)).filter(n => n.endsWith('.ts')).map(n => readFile(new URL(n, generated), 'utf8')));
for (const name of ['app/ui.ts', 'state/projection.ts']) {
  const text = await readFile(new URL(name, root), 'utf8');
  texts.push(...Array.from(text.matchAll(/^export interface \w+ \{[\s\S]*?^\}/gm), m => m[0]));
  if (name === 'app/ui.ts') {
    texts.push(text.slice(text.indexOf('export type Route ='), text.indexOf('/** Home tab')));
    texts.push(...Array.from(text.matchAll(/^export type \w+ = [^;\n]+;/gm), m => m[0]));
  }
}
const clean = texts.map(s => s.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/[^\n]*/g, ''));
function literals(type) {
  const parts = type.trim().split(/\s*\|\s*/).filter(p => p !== 'null' && p !== 'undefined');
  return parts.length && parts.every(p => /^"[^"\n]+"$/.test(p)) ? parts.map(p => p.slice(1, -1)) : [];
}
const aliases = new Map();
for (const text of clean) for (const m of text.matchAll(/export type (\w+) = ([^;]+);/g)) {
  const values = literals(m[2]); if (values.length) aliases.set(m[1], values);
}
const keys = new Set(['host', 'ui', 'prefs', 'talk', 'control', 'work', 'library']);
const values = {};
for (const text of clean) for (const m of text.matchAll(/(?:"(\w+)"|\b(\w+))\??:\s*([^,;{}\n]+)/g)) {
  const key = m[1] ?? m[2]; keys.add(key);
  const type = m[3].trim();
  const allowed = literals(type).length ? literals(type) : aliases.get(type) ?? [];
  if (allowed.length) values[key] = [...new Set([...(values[key] ?? []), ...allowed])].sort();
}
await writeFile(new URL('diagnostics/stateSchema.json', root), JSON.stringify({ keys: [...keys].sort(), values }, null, 2) + '\n');
