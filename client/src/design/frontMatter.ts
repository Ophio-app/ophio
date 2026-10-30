// SPDX-License-Identifier: Apache-2.0
//
// Leading YAML front matter, as in agent skill and definition files. Shown as
// properties rather than passed to Markdown, where its closing `---` would
// turn the fields above it into a heading.

export type FrontMatter = {
  /** The raw YAML between the fences. */
  raw: string;
  /** Flat `key: value` fields, or null when the YAML is nested or unusual. */
  fields: Array<[string, string]> | null;
};

const FENCE = /^﻿?---[ \t]*\r?\n([\s\S]*?)\r?\n(?:---|\.\.\.)[ \t]*(?:\r?\n|$)/;
const FIELD = /^([A-Za-z0-9_.-]+):[ \t]*(.*)$/;

function unquote(value: string): string {
  const v = value.trim();
  if (v.length >= 2 && (v[0] === '"' || v[0] === "'") && v.at(-1) === v[0])
    return v.slice(1, -1);
  return v;
}

export function splitFrontMatter(text: string): {
  front: FrontMatter | null;
  body: string;
} {
  const m = FENCE.exec(text);
  if (!m) return { front: null, body: text };
  const raw = m[1] ?? "";
  const fields: Array<[string, string]> = [];
  let flat = true;
  for (const line of raw.split(/\r?\n/)) {
    if (!line.trim() || line.trimStart().startsWith("#")) continue;
    const [, key, value] = FIELD.exec(line) ?? [];
    // An empty value means a nested block or list follows; show the source.
    if (!key || !value?.trim()) {
      flat = false;
      break;
    }
    fields.push([key, unquote(value)]);
  }
  return {
    front: { raw, fields: flat && fields.length ? fields : null },
    body: text.slice(m[0].length),
  };
}
