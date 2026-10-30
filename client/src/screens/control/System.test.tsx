// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { RouteProvider } from "../../shell/Screen";
import { UiStore } from "../../app/ui";
import { HostSession, NotConnectedError } from "../../host/session";
import type { Platform } from "../../host/link";
import type { SystemStats } from "../../protocol/types";
import { allGrants } from "../../dev/sim/host";
import * as fx from "../../dev/sim/scenario";
import { SystemSim } from "../../dev/sim/system";
import System, { tilesFor } from "./System";
import { SYSTEM_SAMPLE_MS } from "./systemFormat";

const GiB = 1024 ** 3;
let app: App;
let query: ReturnType<typeof vi.fn>;

const stats = (over: Partial<SystemStats> = {}): SystemStats => ({
  at: Date.UTC(2026, 9, 4, 9, 41),
  cpu: { usage: 42, cores: [10, 74], model: "Test processor" },
  memory: { total: 16 * GiB, used: 6 * GiB, cached: 4 * GiB },
  swap: { total: 4 * GiB, used: 1 * GiB, in_per_s: 0, out_per_s: 2048 },
  load: [1.5, 1.25, 1],
  uptime_s: 4 * 3_600 + 12 * 60,
  disks: [{ mount: "/", total: 100 * GiB, used: 60 * GiB }],
  network: { received_per_s: 2 * 1024 ** 2, sent_per_s: 3 * 1024, interfaces: [{ name: "eth0", received_per_s: 2 * 1024 ** 2, sent_per_s: 3 * 1024 }] },
  temperatures: [{ label: "CPU", celsius: 61 }, { label: "NVMe", celsius: 44 }],
  gpus: [],
  battery: null,
  processes: [
    { pid: 10, name: "builder", cpu: 38, memory: 1 * GiB },
    { pid: 11, name: "browser", cpu: 3, memory: 3 * GiB },
  ],
  history: [{ at: 0, cpu: 42, memory: 37.5, swap: 25, received_per_s: 2 * 1024 ** 2, sent_per_s: 3 * 1024 }],
  ...over,
});

function On({ active = true, children }: { active?: boolean; children: ReactNode }) {
  return <RouteProvider value={{ tab: "control", route: { screen: "control" }, depth: 0, active }}>{children}</RouteProvider>;
}
const mount = async (active = true) => {
  let mounted!: ReturnType<typeof render>;
  await act(async () => { mounted = render(<AppProvider app={app}><On active={active}><System /></On></AppProvider>); });
  return {
    ...mounted,
    show: async (now: boolean) => act(async () => { mounted.rerender(<AppProvider app={app}><On active={now}><System /></On></AppProvider>); }),
  };
};
const host = (features: string[], grants = allGrants) =>
  app.session.state.set((s) => ({ ...s, caughtUp: true, grants, capabilities: { ...fx.capabilities(), features } }));

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
  vi.setSystemTime(Date.UTC(2026, 9, 4, 9, 42));
  const session = new HostSession("system", { connect() {}, disconnect() {}, send: () => false, listen: () => () => {} });
  session.link.set({ state: "online", since: Date.now() });
  query = vi.fn(async () => ({ kind: "system_stats", stats: stats() }));
  vi.spyOn(session, "query").mockImplementation(query as never);
  app = {
    session, ui: new UiStore("system"), profiles: [], switchTo() {}, pairAnother() {}, async rename() {}, forget: async () => {},
    profile: { profileId: "system", hostId: "host", name: "Studio", deviceId: "phone", addresses: [], certSha256: "", pairedAt: 0 },
    platform: { kind: "web" } as unknown as Platform,
  };
  host(["system_stats_v1"]);
});
afterEach(() => { cleanup(); app.session.stop(); app.ui.persistNow(); vi.useRealTimers(); vi.restoreAllMocks(); });

it("isn't there on a computer that can't say how busy it is", async () => {
  host(["github_v1"]);
  const { container } = await mount();
  expect(container.innerHTML).toBe("");
  expect(query).not.toHaveBeenCalled();
});

it("tells a device without view what it would need, and doesn't ask", async () => {
  host(["system_stats_v1"], { ...allGrants, view: false });
  await mount();
  expect(screen.getByRole("heading", { name: "System" })).toBeTruthy();
  expect(screen.getByText(/see how busy the computer is/)).toBeTruthy();
  expect(query).not.toHaveBeenCalled();
});

it("asks while on screen, one answer at a time, and stops when it isn't", async () => {
  let answer!: (v: unknown) => void;
  query.mockImplementationOnce(() => new Promise((r) => { answer = r; }));
  const view = await mount();
  expect(query).toHaveBeenCalledTimes(1);
  expect(query).toHaveBeenLastCalledWith({ kind: "system_stats" });
  expect(screen.getByText("Reading Studio…")).toBeTruthy();
  // A slow answer holds the next ask back rather than stacking another.
  await act(async () => { await vi.advanceTimersByTimeAsync(SYSTEM_SAMPLE_MS * 3); });
  expect(query).toHaveBeenCalledTimes(1);
  await act(async () => { answer({ kind: "system_stats", stats: stats() }); });
  expect(screen.getByText("Up 4 h 12 min")).toBeTruthy();
  await act(async () => { await vi.advanceTimersByTimeAsync(SYSTEM_SAMPLE_MS - 1); });
  expect(query).toHaveBeenCalledTimes(1);
  await act(async () => { await vi.advanceTimersByTimeAsync(1); });
  expect(query).toHaveBeenCalledTimes(2);
  await view.show(false);
  await act(async () => { await vi.advanceTimersByTimeAsync(SYSTEM_SAMPLE_MS * 5); });
  expect(query).toHaveBeenCalledTimes(2);
  await view.show(true);
  expect(query).toHaveBeenCalledTimes(3);
});

it("an answer that lands after leaving changes nothing and asks no more", async () => {
  let answer!: (v: unknown) => void;
  query.mockImplementationOnce(() => new Promise((r) => { answer = r; }));
  const view = await mount();
  await view.show(false);
  await act(async () => { answer({ kind: "system_stats", stats: stats() }); });
  await act(async () => { await vi.advanceTimersByTimeAsync(SYSTEM_SAMPLE_MS * 3); });
  expect(query).toHaveBeenCalledTimes(1);
  expect(screen.queryByText("Up 4 h 12 min")).toBeNull();
});

it("waits for the computer while it isn't connected", async () => {
  app.session.link.set({ state: "offline", since: Date.now() } as never);
  await mount();
  expect(screen.getByText("Shown while Studio is connected.")).toBeTruthy();
  expect(query).not.toHaveBeenCalled();
});

it("keeps the last numbers and says how old they are when an ask fails", async () => {
  await mount();
  query.mockRejectedValueOnce(new NotConnectedError());
  await act(async () => { await vi.advanceTimersByTimeAsync(SYSTEM_SAMPLE_MS); });
  expect(screen.getByRole("status").textContent).toMatch(/Studio isn’t connected right now\. · as of/);
  expect(screen.getByText("42%")).toBeTruthy();
  await act(async () => { await vi.advanceTimersByTimeAsync(SYSTEM_SAMPLE_MS); });
  expect(screen.queryByRole("status")).toBeNull();
  await act(async () => { app.session.link.set({ state: "offline", since: Date.now() } as never); });
  expect(screen.getByRole("status").textContent).toMatch(/^Not connected · as of \S/);
  expect(screen.getByText("42%")).toBeTruthy();
});

it("says why when the first ask fails, and keeps asking", async () => {
  query.mockRejectedValueOnce(new NotConnectedError());
  await mount();
  expect(screen.getByRole("status").textContent).toMatch(/Studio isn’t connected right now\.$/);
  expect(screen.queryByText(/Reading/)).toBeNull();
  await act(async () => { await vi.advanceTimersByTimeAsync(SYSTEM_SAMPLE_MS); });
  expect(screen.queryByRole("status")).toBeNull();
  expect(screen.getByText("42%")).toBeTruthy();
});

it("goes back to the processor when what was open is no longer read", async () => {
  await mount();
  const tiles = within(screen.getByRole("group", { name: "What to show" }));
  fireEvent.click(tiles.getByRole("button", { name: /Temperature/ }));
  query.mockResolvedValue({ kind: "system_stats", stats: stats({ temperatures: [] }) });
  await act(async () => { await vi.advanceTimersByTimeAsync(SYSTEM_SAMPLE_MS); });
  expect(tiles.queryByRole("button", { name: /Temperature/ })).toBeNull();
  expect(tiles.getByRole("button", { name: /Processor/ }).getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByRole("table", { name: "Busiest processes" })).toBeTruthy();
});

it("shows the processor first and opens what each tile stands for", async () => {
  await mount();
  const tiles = within(screen.getByRole("group", { name: "What to show" }));
  const cpu = tiles.getByRole("button", { name: /Processor/ });
  expect(cpu.getAttribute("aria-pressed")).toBe("true");
  expect(cpu.textContent).toContain("42%61 °C · load 1.50");
  expect(screen.getByRole("img", { name: "2 processors, the busiest at 74%" })).toBeTruthy();
  expect(screen.getByText("Test processor")).toBeTruthy();
  expect(screen.getByRole("table", { name: "Busiest processes" })).toBeTruthy();
  expect(screen.getAllByRole("row").slice(1).map((r) => r.textContent)).toEqual(["builder 1038%1.0 GB", "browser 113.0%3.0 GB"]);

  fireEvent.click(tiles.getByRole("button", { name: /Swap/ }));
  expect(tiles.getByRole("button", { name: /Memory/ }).getAttribute("aria-pressed")).toBe("true");
  expect(tiles.getByRole("button", { name: /Swap/ }).getAttribute("aria-pressed")).toBe("true");
  expect(cpu.getAttribute("aria-pressed")).toBe("false");
  expect(screen.getByRole("img", { name: "Memory: 6.0 GB in use, 4.0 GB cached, 6.0 GB free" })).toBeTruthy();
  expect(screen.getByText("Swap", { selector: "dt" }).nextElementSibling!.textContent).toBe("1.0 GB of 4.0 GB · in 0 B/s · out 2.0 KB/s");
  expect(screen.getAllByRole("row").slice(1).map((r) => r.querySelector(".system-processes__name")?.textContent)).toEqual(["browser", "builder"]);

  fireEvent.click(tiles.getByRole("button", { name: /Network/ }));
  expect(screen.getByText("eth0").parentElement!.textContent).toContain("eth0↓ 2.0 MB/s · ↑ 3.0 KB/s");
  fireEvent.click(tiles.getByRole("button", { name: /Disk/ }));
  expect(screen.getByRole("img", { name: "/ 60% full" })).toBeTruthy();
  fireEvent.click(tiles.getByRole("button", { name: /Temperature/ }));
  expect(screen.getByText("NVMe").parentElement!.textContent).toContain("NVMe44 °C");
});

it("marks what runs high in words as well as colour", async () => {
  query.mockResolvedValue({ kind: "system_stats", stats: stats({
    cpu: { usage: 90, cores: [], model: null },
    memory: { total: 10, used: 8.9, cached: 0 },
    temperatures: [{ label: "CPU", celsius: 85 }, { label: "Board", celsius: 84.9 }],
  }) });
  await mount();
  const tiles = within(screen.getByRole("group", { name: "What to show" }));
  expect(tiles.getByRole("button", { name: /Processor/ }).textContent).toContain("90% (high)");
  expect(tiles.getByRole("button", { name: /Processor/ }).classList.contains("is-high")).toBe(true);
  expect(tiles.getByRole("button", { name: /Memory/ }).textContent).not.toContain("(high)");
  expect(tiles.getByRole("button", { name: /Temperature/ }).textContent).toContain("85 °C (high)CPU, the hottest of 2");
  expect(screen.queryByRole("img", { name: /processors/ })).toBeNull();
  fireEvent.click(tiles.getByRole("button", { name: /Temperature/ }));
  expect(screen.getByText("CPU").parentElement!.textContent).toContain("85 °C · hot");
  expect(screen.getByText("Board").parentElement!.textContent).not.toContain("hot");
});

it("fills only the tiles the computer can read", () => {
  const keys = (s: SystemStats) => tilesFor(s).map((t) => t.key);
  expect(keys(stats())).toEqual(["cpu", "memory", "swap", "network", "disks", "sensors"]);
  expect(keys(stats({ cpu: null, memory: null, swap: { total: 0, used: 0, in_per_s: null, out_per_s: null }, network: null, disks: [], temperatures: [] }))).toEqual([]);
  const laptop = tilesFor(stats({ temperatures: [], battery: { percent: 64, charging: true } })).find((t) => t.key === "sensors")!;
  expect([laptop.label, laptop.value, laptop.sub]).toEqual(["Battery", "64%", "Charging"]);
  const one = tilesFor(stats({ gpus: [{ name: "Graphics A", usage: 12, memory_used: GiB, memory_total: 8 * GiB, celsius: null }] })).find((t) => t.key === "gpu")!;
  expect([one.label, one.value, one.sub]).toEqual(["Graphics A", "12%", "1.0 GB of 8.0 GB"]);
  const two = tilesFor(stats({ gpus: [
    { name: "Graphics A", usage: 12, memory_used: null, memory_total: null, celsius: null },
    { name: "Graphics B", usage: 91, memory_used: null, memory_total: null, celsius: 70 },
  ] })).find((t) => t.key === "gpu")!;
  expect([two.label, two.value, two.sub, two.high]).toEqual(["Graphics", "91%", "Graphics B · 70 °C", true]);
  const quiet = tilesFor(stats({ temperatures: [{ label: "CPU", celsius: 40 }], load: null }));
  expect(quiet.find((t) => t.key === "cpu")!.sub).toBe("40 °C");
  expect(quiet.find((t) => t.key === "sensors")!.sub).toBe("CPU");
  const full = tilesFor(stats({ memory: { total: 10, used: 9, cached: 0 } })).find((t) => t.key === "memory")!;
  expect([full.value, full.high]).toEqual(["90%", true]);
  expect(tilesFor(stats({ temperatures: [], load: null })).find((t) => t.key === "cpu")!.sub).toBe("2 processors");
});

it("the simulator keeps a history like the companion, and starts it again after a gap", () => {
  const sim = new SystemSim(0);
  const first = sim.stats(100_000);
  expect(first.history).toHaveLength(90);
  expect(first.history.at(-1)!.at).toBe(100_000);
  expect(first.history[0]!.at).toBe(100_000 - 89 * SYSTEM_SAMPLE_MS);
  expect(sim.stats(102_000).history).toHaveLength(90);
  expect(sim.stats(102_000).history.at(-2)!.at).toBe(102_000);
  expect(sim.stats(140_000).history[0]!.at).toBe(140_000 - 89 * SYSTEM_SAMPLE_MS);
});
