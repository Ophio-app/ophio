// SPDX-License-Identifier: Apache-2.0
export function reportReference(id: string): string {
  const n = parseInt(id.replace(/-/g, "").slice(-8), 16) & 0x3fffffff;
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  return Array.from({ length: 6 }, (_, i) => alphabet[(n >>> (5 * (5 - i))) & 31]).join("");
}
