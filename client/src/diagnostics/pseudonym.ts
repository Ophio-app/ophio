// SPDX-License-Identifier: Apache-2.0
export const prefixes = { project: "p", task: "t", run: "r", approval: "a", device: "d", command: "c", attention: "n", other: "x" } as const;
export type PseudonymKind = keyof typeof prefixes;
export const hex = (bytes: Uint8Array) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
export async function pseudonym(salt: string, kind: PseudonymKind, id: string): Promise<string> {
  // Rust's to_ascii_lowercase deliberately leaves non-ASCII unchanged.
  const lower = id.replace(/[A-Z]/g, (c) => c.toLowerCase());
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(`${salt}:${lower}`));
  return `${prefixes[kind]}:${hex(new Uint8Array(digest).slice(0, 4))}`;
}
export function reportSalt(): string { return hex(crypto.getRandomValues(new Uint8Array(16))); }
