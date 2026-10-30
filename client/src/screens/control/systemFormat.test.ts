// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { ProcessStats, SystemStats } from "../../protocol/types";
import {
  busiestGpu, bytes, celsius, fullestDisk, hottest, networkScale, percent, rate, share, sparkline, topProcesses, uptime,
} from "./systemFormat";

const stats = (over: Partial<SystemStats> = {}): SystemStats => ({
  at: 0, cpu: null, memory: null, swap: null, load: null, uptime_s: null, disks: [], network: null,
  temperatures: [], gpus: [], battery: null, processes: [], history: [], ...over,
});
const sample = (received: number | null, sent: number | null) =>
  ({ at: 0, cpu: null, memory: null, swap: null, received_per_s: received, sent_per_s: sent });

it("counts bytes in binary units, with a decimal only where it says something", () => {
  expect(bytes(0)).toBe("0 B");
  expect(bytes(1023)).toBe("1023 B");
  expect(bytes(1024)).toBe("1.0 KB");
  expect(bytes(1536)).toBe("1.5 KB");
  expect(bytes(99.94 * 1024)).toBe("99.9 KB");
  expect(bytes(100 * 1024)).toBe("100 KB");
  expect(bytes(1023.6 * 1024)).toBe("1024 KB");
  expect(bytes(32 * 1024 ** 3)).toBe("32.0 GB");
  expect(bytes(5 * 1024 ** 6)).toBe("5120 PB");
  expect(rate(2 * 1024 ** 2)).toBe("2.0 MB/s");
});

it("rounds shares and heat to whole numbers", () => {
  expect(percent(37.5)).toBe("38%");
  expect(percent(0.4)).toBe("0%");
  expect(celsius(84.6)).toBe("85 °C");
  expect(share(1, 4)).toBe(25);
  expect(share(5, 4)).toBe(100);
  expect(share(-1, 4)).toBe(0);
  expect(share(3, 0)).toBe(0);
});

it("says how long the computer has been up in the two largest units", () => {
  expect(uptime(59)).toBe("0 min");
  expect(uptime(12 * 60 + 59)).toBe("12 min");
  expect(uptime(3_600)).toBe("1 h 0 min");
  expect(uptime(4 * 3_600 + 12 * 60)).toBe("4 h 12 min");
  expect(uptime(86_400 - 1)).toBe("23 h 59 min");
  expect(uptime(3 * 86_400 + 4 * 3_600 + 59 * 60)).toBe("3 d 4 h");
});

it("draws the newest sample at the right edge and a short history in from the right", () => {
  expect(sparkline([0, 50, 100], 100, 3)).toEqual({ line: "M0 100L50 50L100 0", area: "M0 100L0 100L50 50L100 0L100 100Z" });
  expect(sparkline([25], 100, 5)).toEqual({ line: "M100 75", area: "M100 100L100 75L100 100Z" });
  expect(sparkline([10, 20], 40, 3).line).toBe("M50 75L100 50");
  expect(sparkline([], 100)).toEqual({ line: "", area: "" });
});

it("breaks the line where a sample is missing, and keeps every point inside the box", () => {
  const { line, area } = sparkline([50, null, 50, 50], 100, 4);
  expect(line).toBe("M0 50M66.67 50L100 50");
  expect(area).toBe("M0 100L0 50L0 100ZM66.67 100L66.67 50L100 50L100 100Z");
  expect(sparkline([200, -5, Number.NaN, 1], 100, 4).line).toBe("M0 0L33.33 100M100 99");
  expect(sparkline([3], 0, 1).line).toBe("M0 0");
  expect(sparkline([0], 0, 1).line).toBe("M0 100");
});

it("scales the network graph to its busiest moment, never below an idle floor", () => {
  expect(networkScale(stats())).toBe(64 * 1024);
  expect(networkScale(stats({ history: [sample(10, 20), sample(null, null)] }))).toBe(64 * 1024);
  expect(networkScale(stats({ history: [sample(10, 300_000), sample(200_000, null)] }))).toBe(300_000);
  expect(networkScale(stats({ history: [sample(500_000, 1)] }))).toBe(500_000);
});

it("lists the busiest by what was asked, the other measure and then the id breaking ties", () => {
  const p = (pid: number, cpu: number, memory: number): ProcessStats => ({ pid, name: `p${pid}`, cpu, memory });
  const all = [p(1, 5, 10), p(2, 50, 1), p(5, 5, 30), p(4, 0, 99), p(3, 5, 30)];
  expect(topProcesses(all, "cpu").map((x) => x.pid)).toEqual([2, 3, 5, 1, 4]);
  expect(topProcesses(all, "memory").map((x) => x.pid)).toEqual([4, 3, 5, 1, 2]);
  expect(topProcesses(all, "cpu", 2).map((x) => x.pid)).toEqual([2, 3]);
  expect(all.map((x) => x.pid)).toEqual([1, 2, 5, 4, 3]);
  expect(topProcesses(Array.from({ length: 12 }, (_, i) => p(i, i, i)), "cpu")).toHaveLength(8);
});

it("picks the hottest sensor, the busiest graphics and the fullest disk", () => {
  expect(hottest(stats())).toBeUndefined();
  expect(hottest(stats({ temperatures: [{ label: "CPU", celsius: 60 }, { label: "NVMe", celsius: 71 }, { label: "Wi-Fi", celsius: 71 }] }))?.label).toBe("NVMe");
  const gpu = (name: string, usage: number | null) => ({ name, usage, memory_used: null, memory_total: null, celsius: null });
  expect(busiestGpu(stats())).toBeUndefined();
  expect(busiestGpu(stats({ gpus: [gpu("a", null), gpu("b", 0), gpu("c", 30), gpu("d", 30)] }))?.name).toBe("c");
  expect(busiestGpu(stats({ gpus: [gpu("a", null), gpu("b", null)] }))?.name).toBe("a");
  expect(fullestDisk(stats())).toBeUndefined();
  expect(fullestDisk(stats({ disks: [
    { mount: "/", total: 100, used: 50 }, { mount: "/data", total: 1_000, used: 900 }, { mount: "/boot", total: 0, used: 0 }, { mount: "/tmp", total: 10, used: 9 },
  ] }))?.mount).toBe("/data");
});
