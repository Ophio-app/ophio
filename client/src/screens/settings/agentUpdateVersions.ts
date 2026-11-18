// SPDX-License-Identifier: Apache-2.0
/** Registry release versions. Unknown formats are not evidence of a newer release. */
export function compareAgentVersions(installed: string, latest: string): number | null {
  const parse = (value: string) => {
    const m = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(value);
    return m ? { core: m.slice(1, 4).map(Number), pre: m[4]?.split(".") } : null;
  };
  const a = parse(installed), b = parse(latest);
  if (!a || !b) return null;
  for (let i = 0; i < 3; i++) if (a.core[i] !== b.core[i]) return Math.sign(b.core[i]! - a.core[i]!);
  if (!a.pre && !b.pre) return 0;
  if (!a.pre) return -1;
  if (!b.pre) return 1;
  for (let i = 0; i < Math.max(a.pre.length, b.pre.length); i++) {
    const x = a.pre[i], y = b.pre[i];
    if (x === y) continue;
    if (x === undefined) return 1;
    if (y === undefined) return -1;
    const xn = /^\d+$/.test(x), yn = /^\d+$/.test(y);
    if (xn && yn) return Math.sign(Number(y) - Number(x));
    if (xn !== yn) return xn ? 1 : -1;
    return y > x ? 1 : -1;
  }
  return 0;
}
