// SPDX-License-Identifier: Apache-2.0

/** What to call the device in hand. Android's tablet layouts start at a 600 dp shortest side. */
export function deviceKind(): "phone" | "tablet" {
  if (typeof navigator === "undefined" || !/Android/i.test(navigator.userAgent)) return "phone";
  return Math.min(screen.width, screen.height) >= 600 ? "tablet" : "phone";
}
