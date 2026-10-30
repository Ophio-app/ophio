// SPDX-License-Identifier: Apache-2.0
import { MAX_REPORT_CODE } from "./constants";

/** The same character scanner as protocol::reports::scrub. */
export function scrub(text: string): string | undefined {
  const chars = Array.from(text);
  let out = "";
  for (let i = 0; i < chars.length; i++) {
    const c = chars[i]!;
    if ('"\'`“‘'.includes(c)) {
      const close = c === "“" ? "”" : c === "‘" ? "’" : c;
      const end = chars.indexOf(close, i + 1);
      if (end >= 0) { out += '"…"'; i = end; } else out += c;
    } else if (/\p{White_Space}/u.test(c)) out += " ";
    else {
      let word = c;
      while (i + 1 < chars.length && !/[\p{White_Space}"`“‘]/u.test(chars[i + 1]!)) word += chars[++i];
      const coreWithOpen = word.replace(/[.,;:)\]]+$/, "");
      const tail = word.slice(coreWithOpen.length);
      const open = coreWithOpen.match(/^[([]*/)?.[0] ?? "";
      const core = coreWithOpen.slice(open.length);
      const body = core.includes("://") ? "<url>"
        : core.includes("@") && core.includes(".") && !core.startsWith("@") ? "<email>"
        : /^(\/|~\/|\.\/|\.\.\/)/.test(core) || core.includes("\\") || (core.match(/\//g)?.length ?? 0) >= 2 ? "<path>"
        : core.replace(/[0-9]{5,}/g, "<n>");
      out += open + body + tail;
    }
  }
  out = out.split(/\p{White_Space}+/u).filter(Boolean).join(" ");
  const points = Array.from(out);
  return (points.length > MAX_REPORT_CODE ? points.slice(0, MAX_REPORT_CODE).join("") + "…" : out) || undefined;
}
