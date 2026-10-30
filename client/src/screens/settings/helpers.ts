// SPDX-License-Identifier: Apache-2.0
import type { ChannelKind, Grants } from "../../protocol/types";
export type PreviewUrl =
  | { valid: true; url: string; privateHost: boolean }
  | { valid: false; reason: string };
/** Host classification is a disclosure hint; the computer enforces its own URL/DNS policy. */
export function classifyPreviewUrl(input: string): PreviewUrl {
  let url: URL;
  try {
    url = new URL(input.trim());
  } catch {
    return {
      valid: false,
      reason: "Enter a complete http:// or https:// URL.",
    };
  }
  if (!["http:", "https:"].includes(url.protocol))
    return {
      valid: false,
      reason: "Previews use http:// or https:// URLs only.",
    };
  if (url.username || url.password)
    return {
      valid: false,
      reason: "Remove the username and password from this URL.",
    };
  const h = url.hostname.toLowerCase().replace(/\.$/, ""),
    parts = h.split(".").map(Number);
  const ip =
    parts.length === 4 &&
    parts.every((n) => Number.isInteger(n) && n >= 0 && n <= 255);
  const a = parts[0] ?? -1,
    b = parts[1] ?? -1;
  const privateIp =
    ip &&
    (a === 10 ||
      a === 127 ||
      (a === 192 && b === 168) ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 100 && b >= 64 && b <= 127));
  const privateV6 = h === "[::1]" || /^\[f[cd][0-9a-f]{2}:/.test(h);
  const privateHost =
    privateIp ||
    privateV6 ||
    h === "localhost" ||
    h.endsWith(".localhost") ||
    h.endsWith(".ts.net") ||
    h.endsWith(".local") ||
    (!h.includes(".") && !h.includes(":"));
  return { valid: true, url: url.href, privateHost };
}
export const CHANNEL_NAME: Record<ChannelKind, string> = {
  telegram: "Telegram",
  discord: "Discord",
};
export const GRANT_WORDS: Record<keyof Grants, string> = {
  view: "Watch desktop",
  control_input: "Control input",
  terminal: "Terminals",
  tasks: "Agent tasks",
  approvals: "Agent approvals",
  computer_use: "AI computer use",
  library_read: "Read Library",
  library_write: "Edit Library",
  transfers: "File transfers",
  admin: "Manage access",
  accounts: "Accounts",
};
export function grantWords(grants: Grants): string {
  return (
    (Object.keys(GRANT_WORDS) as (keyof Grants)[])
      .filter((k) => grants[k])
      .map((k) => GRANT_WORDS[k])
      .join(" · ") || "No permissions"
  );
}
/** One grant switched, keeping the pairs the computer relies on: input
 * control needs a view of the desktop, and editing the Library needs reading it. */
export function withGrant(grants: Grants, key: keyof Grants, on: boolean): Grants {
  const next = { ...grants, [key]: on };
  if (on) {
    next.view ||= next.control_input;
    next.library_read ||= next.library_write;
  } else {
    next.control_input &&= next.view;
    next.library_write &&= next.library_read;
  }
  return next;
}
/** Browser download seam; native save/share can replace this without changing export content. */
export function saveBlob(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob),
    anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = name;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 30000);
}

export function plural(count: number, singular: string, pluralForm = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : pluralForm}`;
}
