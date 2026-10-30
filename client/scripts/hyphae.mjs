// SPDX-License-Identifier: Apache-2.0
// Grows the original hyphal textures in src/design/assets from fixed seeds, so
// the files are reproducible and reviewable: `node scripts/hyphae.mjs`.
//
//   mat-dark.svg, mat-light.svg  a seamless tile for screen backgrounds, with
//                                its colour baked in (backgrounds can't take
//                                a mask colour without an extra layer).
//   reach-1..3.svg               radial networks used as masks by touch
//                                responses; black strokes, no colour.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const out = (name) => fileURLToPath(new URL(`../src/design/assets/${name}`, import.meta.url));

function random(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * Tips advance a step at a time, drift, branch, and stop when they meet
 * another thread (fusing into it) or run out of reach. `wrap` makes space
 * periodic so a tile repeats without seams.
 */
function grow({ seed, size, tips, step, turn, branch, reach, budget, wrap, bound, core }) {
  const rnd = random(seed);
  const cell = 8;
  const grid = new Map();
  const mod = (v) => (wrap ? ((v % size) + size) % size : v);
  const key = (x, y) => `${Math.floor(mod(x) / cell)},${Math.floor(mod(y) / cell)}`;
  const near = (a, b) => {
    let d = a - b;
    if (wrap) d -= size * Math.round(d / size);
    return d;
  };
  const threads = [];
  const joins = [];
  const active = tips.map((t) => ({ ...t, pts: [[t.x, t.y]], len: 0, curl: 0 }));
  let used = 0;

  const hit = (x, y, self) => {
    const cx = Math.floor(mod(x) / cell);
    const cy = Math.floor(mod(y) / cell);
    for (let i = -1; i <= 1; i++) {
      for (let j = -1; j <= 1; j++) {
        const gx = wrap ? (((cx + i) % (size / cell)) + size / cell) % (size / cell) : cx + i;
        const gy = wrap ? (((cy + j) % (size / cell)) + size / cell) % (size / cell) : cy + j;
        for (const p of grid.get(`${gx},${gy}`) ?? []) {
          if (p.t === self.id && p.i > self.pts.length - 6) continue;
          if (p.t === self.parent && p.i >= self.from - 2 && p.i <= self.from + 2) continue;
          if (Math.hypot(near(x, p.x), near(y, p.y)) < step * 0.7) return p;
        }
      }
    }
    return null;
  };
  const mark = (x, y, t, i) => {
    const k = key(x, y);
    if (!grid.has(k)) grid.set(k, []);
    grid.get(k).push({ x: mod(x), y: mod(y), t, i });
  };

  let id = 0;
  for (const t of active) {
    t.id = id++;
    mark(t.x, t.y, t.id, 0);
  }
  while (active.length && used < budget) {
    for (let n = active.length - 1; n >= 0; n--) {
      const t = active[n];
      t.curl = t.curl * 0.9 + (rnd() - 0.5) * turn * 0.35;
      t.a += t.curl + (rnd() - 0.5) * turn;
      const [px, py] = t.pts[t.pts.length - 1];
      const x = px + Math.cos(t.a) * step;
      const y = py + Math.sin(t.a) * step;
      const met = core?.(x, y) ? null : hit(x, y, t);
      const out = bound ? bound(x, y) : false;
      t.len += step;
      if (met) {
        // Fuse: finish on the thread it met, where a node forms.
        const fx = px + near(met.x, px);
        const fy = py + near(met.y, py);
        t.pts.push([fx, fy]);
        joins.push([fx, fy]);
      } else if (!out) {
        t.pts.push([x, y]);
        mark(x, y, t.id, t.pts.length - 1);
        used++;
      }
      if (met || out || t.len > t.reach) {
        active.splice(n, 1);
        if (t.pts.length > 1) threads.push(t);
        if (!met && !out && t.tip) joins.push(t.pts[t.pts.length - 1]);
        continue;
      }
      if (t.pts.length > 3 && t.w > branch.minW && rnd() < branch.p) {
        const side = rnd() < 0.5 ? -1 : 1;
        active.push({
          id: id++,
          parent: t.id,
          from: t.pts.length - 1,
          x,
          y,
          a: t.a + side * (branch.min + rnd() * (branch.max - branch.min)),
          w: t.w * branch.thin,
          reach: t.reach * (0.45 + rnd() * 0.4) * (reach ?? 1),
          tip: t.tip,
          pts: [[x, y]],
          len: 0,
          curl: 0,
        });
        joins.push([x, y]);
      }
    }
  }
  for (const t of active) if (t.pts.length > 1) threads.push(t);
  return { threads, joins };
}

const r = (v) => Math.round(v);

/** A polyline as a compact relative path, drawn again wherever it crosses a tile edge. */
function pathData(pts, size, wrap) {
  const offsets = [[0, 0]];
  if (wrap) {
    const xs = pts.map((p) => p[0]);
    const ys = pts.map((p) => p[1]);
    const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
    offsets.length = 0;
    for (const ox of [-size, 0, size]) {
      for (const oy of [-size, 0, size]) {
        if (x1 + ox < -2 || x0 + ox > size + 2 || y1 + oy < -2 || y0 + oy > size + 2) continue;
        offsets.push([ox, oy]);
      }
    }
  }
  let d = "";
  for (const [ox, oy] of offsets) {
    let [lx, ly] = [r(pts[0][0] + ox), r(pts[0][1] + oy)];
    d += `M${lx} ${ly}l`;
    const parts = [];
    for (let i = 1; i < pts.length; i++) {
      const [x, y] = [r(pts[i][0] + ox), r(pts[i][1] + oy)];
      if (x === lx && y === ly) continue;
      parts.push(`${x - lx} ${y - ly}`);
      [lx, ly] = [x, y];
    }
    d += parts.join(" ").replace(/ -/g, "-");
  }
  return d;
}

function dots(points, size, wrap) {
  let d = "";
  for (let [x, y] of points) {
    const xs = [0];
    const ys = [0];
    if (wrap) {
      x = ((x % size) + size) % size;
      y = ((y % size) + size) % size;
      if (x < 3) xs.push(size);
      if (x > size - 3) xs.push(-size);
      if (y < 3) ys.push(size);
      if (y > size - 3) ys.push(-size);
    }
    for (const ox of xs) for (const oy of ys) d += `M${r(x + ox)} ${r(y + oy)}h0`;
  }
  return d;
}

function byWidth(threads, widths) {
  const groups = widths.map(() => []);
  for (const t of threads) {
    let g = widths.findIndex((w) => t.w >= w);
    if (g < 0) g = widths.length - 1;
    groups[g].push(t);
  }
  return groups;
}

function mat(theme, { line, node, faint }) {
  const size = 320;
  const rnd = random(7);
  const tips = Array.from({ length: 22 }, () => ({ x: rnd() * size, y: rnd() * size, a: rnd() * Math.PI * 2, w: 1, reach: 420 + rnd() * 360, tip: false }));
  const { threads, joins } = grow({
    seed: 11,
    size,
    tips,
    step: 7,
    turn: 0.32,
    branch: { p: 0.12, min: 0.5, max: 1.1, thin: 0.72, minW: 0.25 },
    budget: 3600,
    wrap: true,
  });
  const [thick, thin] = byWidth(threads, [0.7, 0]);
  const d = (list) => list.map((t) => pathData(t.pts, size, true)).join("");
  const spores = Array.from({ length: 26 }, () => [rnd() * size, rnd() * size]);
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" fill="none" stroke-linecap="round" stroke-linejoin="round">` +
    `<!-- SPDX-License-Identifier: Apache-2.0. Original Ophio background mat (${theme}), scripts/hyphae.mjs. -->` +
    `<path stroke="${line}" stroke-width=".8" d="${d(thick)}"/>` +
    `<path stroke="${faint}" stroke-width=".55" d="${d(thin)}"/>` +
    `<path stroke="${node}" stroke-width="2.6" d="${dots(joins.filter((_, i) => i % 3 === 0), size, true)}"/>` +
    `<path stroke="${faint}" stroke-width="1.3" d="${dots(spores, size, true)}"/>` +
    `</svg>\n`;
}

function reach(seed) {
  const size = 360;
  const c = size / 2;
  const rnd = random(seed);
  const n = 9;
  const tips = Array.from({ length: n }, (_, i) => ({
    x: c,
    y: c,
    a: (i / n) * Math.PI * 2 + (rnd() - 0.5) * 0.45,
    w: 1,
    reach: 125 + rnd() * 45,
    tip: true,
  }));
  const { threads, joins } = grow({
    seed: seed * 31,
    size,
    tips,
    step: 6,
    turn: 0.26,
    branch: { p: 0.11, min: 0.35, max: 0.85, thin: 0.7, minW: 0.3 },
    budget: 1300,
    wrap: false,
    core: (x, y) => Math.hypot(x - c, y - c) < 16,
    bound: (x, y) => Math.hypot(x - c, y - c) > c - 8,
  });
  const [a, b, rest] = byWidth(threads, [0.95, 0.65, 0]);
  const d = (list) => list.map((t) => pathData(t.pts, size, false)).join("");
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" fill="none" stroke="#000" stroke-linecap="round" stroke-linejoin="round">` +
    `<!-- SPDX-License-Identifier: Apache-2.0. Original Ophio touch network, scripts/hyphae.mjs. -->` +
    `<path stroke-width="2" d="${d(a)}"/>` +
    `<path stroke-width="1.3" d="${d(b)}"/>` +
    `<path stroke-width=".8" d="${d(rest)}"/>` +
    `<path stroke-width="3" d="${dots(joins.filter((p) => Math.hypot(p[0] - c, p[1] - c) > 24), size, false)}"/>` +
    `</svg>\n`;
}

// Colours follow tokens.css. Every stroke, under material.css's corner glows,
// keeps ordinary text colours at 4.5:1 or more if a line runs right under them.
writeFileSync(out("mat-dark.svg"), mat("dark", { line: "#34253e", faint: "#281d2f", node: "#362640" }));
writeFileSync(out("mat-light.svg"), mat("light", { line: "#e2d7e1", faint: "#e8e0e6", node: "#e2d7e1" }));
for (const n of [1, 2, 3]) writeFileSync(out(`reach-${n}.svg`), reach(n));
