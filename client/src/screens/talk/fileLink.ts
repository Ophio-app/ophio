// SPDX-License-Identifier: Apache-2.0
//
// A link in an agent's reply to a file in the project's folder opens in
// Ophio, where the person can read it on their phone, rather than as a web
// address their phone can't reach.

import type { LibraryRoot } from "../../protocol/types";

/** The root-relative path `href` names in `root`, or null when it is a web
 * address, an anchor, or somewhere outside the project's folder. */
export function projectFileLink(href: string, root: Pick<LibraryRoot, "host_path"> | undefined): string | null {
  if (!root) return null;
  let path = href.trim().replace(/^file:\/\//i, "");
  if (!path || path.startsWith("#") || path.startsWith("?")) return null;
  // A line an agent cites, as in `docs/report.md:12`, `:12:5` or `:12-20`,
  // is in the file it names. A bare word before the number is a scheme or a
  // host, as in `tel:5551234` or `localhost:3000`, not a file.
  path = path.replace(/[?#].*$/, "").replace(/^(.*[\\/.][^\\/:]*):\d+(?:[:-]\d+)?$/, "$1");
  if (/^[a-z][a-z\d+.-]*:/i.test(path) && !/^[a-z]:[\\/]/i.test(path)) return null;
  try { path = decodeURI(path); } catch { return null; }
  const base = root.host_path.replace(/[\\/]+$/, "");
  if (/^(?:[\\/]|[a-z]:[\\/])/i.test(path)) {
    const inside = path.slice(0, base.length + 1).replace(/\\/g, "/") === `${base.replace(/\\/g, "/")}/`;
    if (!inside) return null;
    path = path.slice(base.length + 1);
  }
  const parts = path.split(/[\\/]+/).filter((p) => p && p !== ".");
  if (!parts.length || parts.includes("..")) return null;
  return parts.join("/");
}
