// SPDX-License-Identifier: Apache-2.0
import type { ProcessStats, SystemSample, SystemStats } from "../../protocol/types";
import { SYSTEM_HISTORY, SYSTEM_SAMPLE_MS } from "../../screens/control/systemFormat";

const GiB = 1024 ** 3;
/** `GAP` in `crates/services/src/system.rs`: longer than this unasked, the history starts again. */
const GAP_MS = 30_000;
const CORES = 16;

/** A slow wave between `low` and `high`, `period` seconds round, so the
 * numbers drift the way a working computer's do. */
const wave = (t: number, period: number, low: number, high: number, phase = 0) =>
  low + ((high - low) * (1 + Math.sin((t / period) * 2 * Math.PI + phase))) / 2;

/**
 * The computer's System section (`crates/services/src/system.rs`), made up.
 * The first ask finds the last few minutes already there, so the graphs have
 * something to draw; a gap in asking starts them again like the companion.
 */
export class SystemSim {
  #history: SystemSample[] = [];
  #last = -Infinity;
  readonly #booted: number;

  constructor(now: number) {
    this.#booted = now - (3 * 86_400 + 4 * 3_600 + 17 * 60) * 1000;
  }

  stats(now: number): SystemStats {
    if (now - this.#last > GAP_MS) {
      this.#history = [];
      for (let i = SYSTEM_HISTORY - 1; i > 0; i--) this.#history.push(sample(reading(now - i * SYSTEM_SAMPLE_MS, this.#booted)));
    }
    this.#last = now;
    const stats = reading(now, this.#booted);
    this.#history.push(sample(stats));
    this.#history = this.#history.slice(-SYSTEM_HISTORY);
    return { ...stats, history: [...this.#history] };
  }
}

function reading(now: number, booted: number): SystemStats {
  const t = now / 1000;
  const cores = Array.from({ length: CORES }, (_, i) =>
    Math.round(wave(t, 9 + i * 1.7, 2, i % 5 === 0 ? 96 : 48, i) * 10) / 10);
  const cpu = cores.reduce((a, b) => a + b, 0) / CORES;
  const total = 32 * GiB;
  const used = Math.round(wave(t, 120, 11, 15) * GiB);
  const cached = Math.round(wave(t, 300, 9, 11) * GiB);
  const received = Math.round(wave(t, 14, 2_000, 2_400_000) * (wave(t, 47, 0, 1) > 0.6 ? 1 : 0.02));
  const sent = Math.round(wave(t, 11, 1_000, 320_000, 1));
  const processes: ProcessStats[] = [
    { pid: 2211, name: "cargo", cpu: Math.round(cpu * 2.1 * 10) / 10, memory: Math.round(1.4 * GiB) },
    { pid: 2290, name: "rustc", cpu: Math.round(cpu * 1.3 * 10) / 10, memory: Math.round(2.6 * GiB) },
    { pid: 1408, name: "firefox", cpu: Math.round(wave(t, 23, 1, 9) * 10) / 10, memory: Math.round(3.1 * GiB) },
    { pid: 1630, name: "rust-analyzer", cpu: Math.round(wave(t, 31, 0, 6) * 10) / 10, memory: Math.round(4.2 * GiB) },
    { pid: 1012, name: "ophio-companion", cpu: 0.4, memory: 180 * 1024 ** 2 },
    { pid: 1803, name: "node", cpu: Math.round(wave(t, 17, 0, 4) * 10) / 10, memory: Math.round(0.9 * GiB) },
    { pid: 980, name: "pipewire", cpu: 0.6, memory: 42 * 1024 ** 2 },
    { pid: 1101, name: "Hyprland", cpu: 1.2, memory: 310 * 1024 ** 2 },
  ].map((p) => ({ ...p, cpu: Math.min(p.cpu, 100) }));
  const cpuHeat = Math.round(wave(t, 19, 52, 88, 2) * 10) / 10;
  return {
    at: now,
    cpu: { usage: Math.round(cpu * 10) / 10, cores, model: "Synthetic 16-core processor" },
    memory: { total, used, cached },
    swap: { total: 8 * GiB, used: Math.round(0.6 * GiB), in_per_s: 0, out_per_s: Math.round(wave(t, 40, 0, 40_000)) },
    load: [Math.round((cpu / 100) * CORES * 100) / 100, 3.42, 2.87],
    uptime_s: Math.floor((now - booted) / 1000),
    disks: [
      { mount: "/", total: 512 * GiB, used: Math.round(301.4 * GiB) },
      { mount: "/data", total: 2048 * GiB, used: Math.round(1_721.9 * GiB) },
    ],
    network: {
      received_per_s: received,
      sent_per_s: sent,
      interfaces: [
        { name: "eth0", received_per_s: received, sent_per_s: sent },
        { name: "wlan0", received_per_s: 0, sent_per_s: 0 },
      ],
    },
    temperatures: [
      { label: "CPU", celsius: cpuHeat },
      { label: "NVMe", celsius: Math.round(wave(t, 60, 38, 46) * 10) / 10 },
    ],
    gpus: [{ name: "Synthetic graphics", usage: Math.round(wave(t, 13, 0, 34)), memory_used: Math.round(2.3 * GiB), memory_total: 12 * GiB, celsius: 47 }],
    battery: null,
    processes,
    history: [],
  };
}

/** `sample` in `crates/services/src/system.rs`. */
function sample(stats: SystemStats): SystemSample {
  const percent = (part: number, whole: number) => (whole > 0 ? (part / whole) * 100 : null);
  return {
    at: stats.at,
    cpu: stats.cpu?.usage ?? null,
    memory: stats.memory ? percent(stats.memory.used, stats.memory.total) : null,
    swap: stats.swap ? percent(stats.swap.used, stats.swap.total) : null,
    received_per_s: stats.network?.received_per_s ?? null,
    sent_per_s: stats.network?.sent_per_s ?? null,
  };
}
