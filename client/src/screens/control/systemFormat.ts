// SPDX-License-Identifier: Apache-2.0
// Control's System section: the computer's numbers as people read them.
import type { ProcessStats, SystemStats } from "../../protocol/types";

/** How often Control asks while the section is on screen: the companion's
 * `SYSTEM_SAMPLE_MS`. */
export const SYSTEM_SAMPLE_MS = 2_000;
/** Samples the companion keeps (`SYSTEM_HISTORY`), about three minutes. */
export const SYSTEM_HISTORY = 90;
/** Busy or hot enough to look at twice. */
export const HIGH_PERCENT = 90;
export const HIGH_CELSIUS = 85;

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"] as const;

/** Binary units, as system monitors count memory: 1 GB is 1024 MB. */
export function bytes(n: number): string {
  let i = 0;
  while (n >= 1024 && i < UNITS.length - 1) {
    n /= 1024;
    i++;
  }
  return `${i === 0 || n >= 100 ? Math.round(n) : n.toFixed(1)} ${UNITS[i]}`;
}

export const rate = (n: number): string => `${bytes(n)}/s`;
export const percent = (n: number): string => `${Math.round(n)}%`;
export const celsius = (c: number): string => `${Math.round(c)} °C`;
/** `part` of `whole` as a percentage; nothing of nothing is 0. */
export const share = (part: number, whole: number): number => (whole > 0 ? Math.min(Math.max(part / whole, 0), 1) * 100 : 0);

export function uptime(seconds: number): string {
  const d = Math.floor(seconds / 86_400);
  const h = Math.floor((seconds % 86_400) / 3_600);
  const m = Math.floor((seconds % 3_600) / 60);
  if (d > 0) return `${d} d ${h} h`;
  if (h > 0) return `${h} h ${m} min`;
  return `${m} min`;
}

const at = (n: number) => Math.round(n * 100) / 100;

/** A line through `values` (0 to `max`) in a 100 × 100 box, the newest at the
 * right edge and each a sample apart, so a short history fills in from the
 * right as it grows. A missing value breaks the line. `area` closes each run
 * down to the bottom, for its fill. */
export function sparkline(
  values: readonly (number | null | undefined)[],
  max: number,
  count = SYSTEM_HISTORY,
): { line: string; area: string } {
  const step = 100 / Math.max(count - 1, 1);
  const start = count - values.length;
  let line = "",
    area = "",
    run: [number, number][] = [];
  const close = () => {
    if (run.length > 0) area += `M${run[0]![0]} 100${run.map(([x, y]) => `L${x} ${y}`).join("")}L${run[run.length - 1]![0]} 100Z`;
    run = [];
  };
  values.forEach((v, i) => {
    if (v == null || !Number.isFinite(v)) return close();
    const point: [number, number] = [at((start + i) * step), at(100 - (Math.min(Math.max(v / (max || 1), 0), 1) * 100))];
    line += `${run.length > 0 ? "L" : "M"}${point[0]} ${point[1]}`;
    run.push(point);
  });
  close();
  return { line, area };
}

/** The network graph's top: its busiest moment, never less than 64 KB/s,
 * so an idle connection draws as idle rather than as noise. */
export function networkScale(stats: SystemStats): number {
  const all = stats.history.flatMap((s) => [s.received_per_s ?? 0, s.sent_per_s ?? 0]);
  return Math.max(64 * 1024, ...all);
}

/** The few busiest by `by`, the rest breaking the tie. */
export function topProcesses(processes: readonly ProcessStats[], by: "cpu" | "memory", count = 8): ProcessStats[] {
  const key = (p: ProcessStats) => (by === "cpu" ? [p.cpu, p.memory] : [p.memory, p.cpu]);
  return [...processes]
    .sort((a, b) => {
      const [a1, a2] = key(a),
        [b1, b2] = key(b);
      return b1! - a1! || b2! - a2! || a.pid - b.pid;
    })
    .slice(0, count);
}

/** The hottest sensor. */
export function hottest(stats: SystemStats): SystemStats["temperatures"][number] | undefined {
  return stats.temperatures.reduce<SystemStats["temperatures"][number] | undefined>(
    (best, t) => (best === undefined || t.celsius > best.celsius ? t : best),
    undefined,
  );
}

/** The busiest graphics processor, by what it reports. */
export function busiestGpu(stats: SystemStats): SystemStats["gpus"][number] | undefined {
  return stats.gpus.reduce<SystemStats["gpus"][number] | undefined>(
    (best, g) => (best === undefined || (g.usage ?? -1) > (best.usage ?? -1) ? g : best),
    undefined,
  );
}

/** The fullest disk, which is the one worth a tile. */
export function fullestDisk(stats: SystemStats): SystemStats["disks"][number] | undefined {
  return stats.disks.reduce<SystemStats["disks"][number] | undefined>(
    (best, d) => (best === undefined || share(d.used, d.total) > share(best.used, best.total) ? d : best),
    undefined,
  );
}
