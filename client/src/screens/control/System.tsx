// SPDX-License-Identifier: Apache-2.0
// Control's System section: how busy the computer is. Tiles give the glance;
// the tile chosen opens what is behind it, with the processes that explain it.
import { useEffect, useState, type ReactNode } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { clockTime } from "../../design/time";
import { Section } from "../../design/ListRow";
import { NotAllowed } from "../../shell/NotAllowed";
import type { SystemSample, SystemStats } from "../../protocol/types";
import { useVisible } from "./useControl";
import {
  HIGH_CELSIUS,
  HIGH_PERCENT,
  SYSTEM_SAMPLE_MS,
  busiestGpu,
  bytes,
  celsius,
  fullestDisk,
  hottest,
  networkScale,
  percent,
  rate,
  share,
  sparkline,
  topProcesses,
  uptime,
} from "./systemFormat";
import "./system.css";

type Detail = "cpu" | "memory" | "network" | "disks" | "gpu" | "sensors";

export default function System() {
  const { session, profile } = useApp();
  const active = useVisible();
  const online = useLink().state === "online";
  const offered = useHost((s) => !!s.capabilities?.features.includes("system_stats_v1"));
  const mayView = useHost((s) => s.grants?.view !== false);
  const [stats, setStats] = useState<SystemStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [detail, setDetail] = useState<Detail>("cpu");
  const asking = active && online && offered && mayView;
  useEffect(() => {
    if (!asking) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // The next ask waits for this answer, so a slow link never stacks them.
    const ask = () =>
      session
        .query({ kind: "system_stats" })
        .then(
          (r) => {
            if (disposed || r.kind !== "system_stats") return;
            setStats(r.stats);
            setError(null);
          },
          (e: unknown) => {
            if (!disposed) setError(describeError(e, profile.name));
          },
        )
        .finally(() => {
          if (!disposed) timer = setTimeout(ask, SYSTEM_SAMPLE_MS);
        });
    void ask();
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [asking, session, profile.name]);
  if (!offered) return null;
  const tiles = stats ? tilesFor(stats) : [];
  const shown = tiles.some((t) => t.detail === detail) ? detail : "cpu";
  return (
    <Section
      title="System"
      id="control-system"
      action={stats?.uptime_s != null && <span className="system__uptime num">Up {uptime(stats.uptime_s)}</span>}
    >
      {!mayView ? (
        <NotAllowed what="see how busy the computer is" permission="view" />
      ) : !stats ? (
        !online ? (
          <p className="system__note">Shown while {profile.name} is connected.</p>
        ) : error ? (
          <p className="system__note" role="status">{error}</p>
        ) : (
          <p className="system__note">Reading {profile.name}…</p>
        )
      ) : (
        <div className="system">
          {(!online || error) && (
            <p className="system__note" role="status">
              {error ?? "Not connected"} · as of {clockTime(stats.at)}
            </p>
          )}
          <div className="system__tiles" role="group" aria-label="What to show">
            {tiles.map((t) => (
              <button
                key={t.key}
                type="button"
                className={["system-tile", `system-tile--${t.tone}`, t.detail === shown && "is-on", t.high && "is-high"]
                  .filter(Boolean)
                  .join(" ")}
                aria-pressed={t.detail === shown}
                onClick={() => setDetail(t.detail)}
              >
                <span className="system-tile__label">{t.label}</span>
                <span className="system-tile__value num">
                  {t.value}
                  {t.high && <span className="visually-hidden"> (high)</span>}
                </span>
                <span className="system-tile__sub num">{t.sub}</span>
                {t.graph}
              </button>
            ))}
          </div>
          <DetailPanel stats={stats} detail={shown} />
        </div>
      )}
    </Section>
  );
}

interface Tile {
  key: string;
  detail: Detail;
  label: string;
  value: ReactNode;
  sub: ReactNode;
  high: boolean;
  /** Which of the palette's lines the graph is drawn in. */
  tone: "accent" | "bloom" | "pulse" | "plain";
  graph?: ReactNode;
}

/** The tiles this computer can fill, in reading order. */
export function tilesFor(stats: SystemStats): Tile[] {
  const tiles: Tile[] = [];
  const history = stats.history;
  const series = (pick: (s: SystemSample) => number | null | undefined) => history.map(pick);
  const cpuHeat = stats.temperatures.find((t) => t.label === "CPU");
  if (stats.cpu) {
    const load = stats.load?.[0];
    tiles.push({
      key: "cpu",
      detail: "cpu",
      label: "Processor",
      value: percent(stats.cpu.usage),
      sub: [cpuHeat && celsius(cpuHeat.celsius), load != null && `load ${load.toFixed(2)}`].filter(Boolean).join(" · ") || `${stats.cpu.cores.length || 1} processors`,
      high: stats.cpu.usage >= HIGH_PERCENT,
      tone: "accent",
      graph: <Graph lines={[series((s) => s.cpu)]} max={100} />,
    });
  }
  if (stats.memory) {
    const m = stats.memory;
    tiles.push({
      key: "memory",
      detail: "memory",
      label: "Memory",
      value: percent(share(m.used, m.total)),
      sub: `${bytes(m.used)} of ${bytes(m.total)}`,
      high: share(m.used, m.total) >= HIGH_PERCENT,
      tone: "bloom",
      graph: <Graph lines={[series((s) => s.memory)]} max={100} />,
    });
  }
  if (stats.swap && stats.swap.total > 0) {
    const s = stats.swap;
    tiles.push({
      key: "swap",
      detail: "memory",
      label: "Swap",
      value: percent(share(s.used, s.total)),
      sub: `${bytes(s.used)} of ${bytes(s.total)}`,
      high: share(s.used, s.total) >= HIGH_PERCENT,
      tone: "pulse",
      graph: <Graph lines={[series((x) => x.swap)]} max={100} />,
    });
  }
  if (stats.network) {
    const n = stats.network;
    tiles.push({
      key: "network",
      detail: "network",
      label: "Network",
      value: <>↓ {rate(n.received_per_s)}</>,
      sub: <>↑ {rate(n.sent_per_s)}</>,
      high: false,
      tone: "accent",
      graph: <Graph lines={[series((s) => s.received_per_s), series((s) => s.sent_per_s)]} max={networkScale(stats)} />,
    });
  }
  const gpu = busiestGpu(stats);
  if (gpu) {
    tiles.push({
      key: "gpu",
      detail: "gpu",
      label: stats.gpus.length > 1 ? "Graphics" : gpu.name,
      value: gpu.usage != null ? percent(gpu.usage) : "—",
      sub: [stats.gpus.length > 1 && gpu.name, gpu.celsius != null && celsius(gpu.celsius)].filter(Boolean).join(" · ") ||
        (gpu.memory_total ? `${bytes(gpu.memory_used ?? 0)} of ${bytes(gpu.memory_total)}` : "Busy"),
      high: (gpu.usage ?? 0) >= HIGH_PERCENT,
      tone: "plain",
    });
  }
  const disk = fullestDisk(stats);
  if (disk) {
    tiles.push({
      key: "disks",
      detail: "disks",
      label: stats.disks.length > 1 ? "Disks" : "Disk",
      value: percent(share(disk.used, disk.total)),
      sub: `${bytes(disk.total - disk.used)} free on ${disk.mount}`,
      high: share(disk.used, disk.total) >= HIGH_PERCENT,
      tone: "plain",
      graph: <Meter value={share(disk.used, disk.total)} />,
    });
  }
  const heat = hottest(stats);
  if (heat || stats.battery) {
    tiles.push({
      key: "sensors",
      detail: "sensors",
      label: heat ? "Temperature" : "Battery",
      value: heat ? celsius(heat.celsius) : percent(stats.battery!.percent),
      sub: heat
        ? stats.temperatures.length > 1 ? `${heat.label}, the hottest of ${stats.temperatures.length}` : heat.label
        : stats.battery!.charging ? "Charging" : "On battery",
      high: heat ? heat.celsius >= HIGH_CELSIUS : false,
      tone: "plain",
    });
  }
  return tiles;
}

/** Each line in turn takes the next of the tile's colours. */
function Graph({ lines, max }: { lines: (number | null | undefined)[][]; max: number }) {
  return (
    <svg className="system-graph" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden focusable="false">
      {lines.map((values, i) => {
        const { line, area } = sparkline(values, max);
        return (
          <g key={i} className={`system-graph__series system-graph__series--${i}`}>
            <path className="system-graph__area" d={area} />
            <path className="system-graph__line" d={line} vectorEffect="non-scaling-stroke" />
          </g>
        );
      })}
    </svg>
  );
}

function Meter({ value, label }: { value: number; label?: string }) {
  return (
    <span className="system-meter" aria-hidden={label === undefined} role={label === undefined ? undefined : "img"} aria-label={label}>
      <span className="system-meter__fill" style={{ transform: `scaleX(${value / 100})` }} />
    </span>
  );
}

function DetailPanel({ stats, detail }: { stats: SystemStats; detail: Detail }) {
  switch (detail) {
    case "cpu":
      return (
        <div className="system-detail">
          {stats.cpu && stats.cpu.cores.length > 1 && (
            <div
              className="system-cores"
              role="img"
              aria-label={`${stats.cpu.cores.length} processors, the busiest at ${percent(Math.max(...stats.cpu.cores))}`}
            >
              {stats.cpu.cores.map((c, i) => (
                <span key={i} className="system-cores__bar">
                  <span className="system-cores__fill" style={{ transform: `scaleY(${Math.min(Math.max(c, 0), 100) / 100})` }} />
                </span>
              ))}
            </div>
          )}
          <dl className="system-facts">
            {stats.cpu?.model && <Fact term="Model">{stats.cpu.model}</Fact>}
            {stats.load && (
              <Fact term="Load">
                {stats.load.map((l) => l.toFixed(2)).join(" · ")} <span className="faint">over 1, 5 and 15 minutes</span>
              </Fact>
            )}
          </dl>
          <Processes stats={stats} by="cpu" />
        </div>
      );
    case "memory": {
      const m = stats.memory;
      const swap = stats.swap;
      return (
        <div className="system-detail">
          {m && (
            <>
              <div className="system-split" role="img" aria-label={`Memory: ${bytes(m.used)} in use, ${bytes(m.cached)} cached, ${bytes(Math.max(m.total - m.used - m.cached, 0))} free`}>
                <span className="system-split__used" style={{ flexGrow: share(m.used, m.total) }} />
                <span className="system-split__cached" style={{ flexGrow: share(m.cached, m.total) }} />
                <span className="system-split__free" style={{ flexGrow: share(m.total - m.used - m.cached, m.total) }} />
              </div>
              <ul className="system-legend" aria-hidden>
                <li className="system-legend__used">In use {bytes(m.used)}</li>
                <li className="system-legend__cached">Cached {bytes(m.cached)}</li>
                <li className="system-legend__free">Free {bytes(Math.max(m.total - m.used - m.cached, 0))}</li>
              </ul>
            </>
          )}
          {swap && (
            <dl className="system-facts">
              <Fact term="Swap">
                {swap.total > 0 ? `${bytes(swap.used)} of ${bytes(swap.total)}` : "None"}
                {swap.in_per_s != null && swap.out_per_s != null && (
                  <span className="faint">
                    {" "}· in {rate(swap.in_per_s)} · out {rate(swap.out_per_s)}
                  </span>
                )}
              </Fact>
            </dl>
          )}
          <Processes stats={stats} by="memory" />
        </div>
      );
    }
    case "network":
      return (
        <div className="system-detail">
          <ul className="system-list">
            {stats.network?.interfaces.map((i) => (
              <li key={i.name} className="system-list__row">
                <span className="mono">{i.name}</span>
                <span className="num">
                  ↓ {rate(i.received_per_s)} · ↑ {rate(i.sent_per_s)}
                </span>
              </li>
            ))}
          </ul>
        </div>
      );
    case "disks":
      return (
        <div className="system-detail">
          <ul className="system-list">
            {stats.disks.map((d) => (
              <li key={d.mount} className="system-list__row system-list__row--meter">
                <span className="mono">{d.mount}</span>
                <span className="num">
                  {bytes(d.used)} of {bytes(d.total)}
                </span>
                <Meter value={share(d.used, d.total)} label={`${d.mount} ${percent(share(d.used, d.total))} full`} />
              </li>
            ))}
          </ul>
        </div>
      );
    case "gpu":
      return (
        <div className="system-detail">
          <ul className="system-list">
            {stats.gpus.map((g, i) => (
              <li key={`${g.name}-${i}`} className="system-list__row">
                <span>{g.name}</span>
                <span className="num">
                  {[
                    g.usage != null && `${percent(g.usage)} busy`,
                    g.memory_total != null && `${bytes(g.memory_used ?? 0)} of ${bytes(g.memory_total)}`,
                    g.celsius != null && celsius(g.celsius),
                  ]
                    .filter(Boolean)
                    .join(" · ") || "Nothing reported"}
                </span>
              </li>
            ))}
          </ul>
        </div>
      );
    case "sensors":
      return (
        <div className="system-detail">
          <ul className="system-list">
            {stats.temperatures.map((t, i) => (
              <li key={`${t.label}-${i}`} className={["system-list__row", t.celsius >= HIGH_CELSIUS && "is-high"].filter(Boolean).join(" ")}>
                <span>{t.label}</span>
                <span className="num">
                  {celsius(t.celsius)}
                  {t.celsius >= HIGH_CELSIUS && " · hot"}
                </span>
              </li>
            ))}
            {stats.battery && (
              <li className="system-list__row">
                <span>Battery</span>
                <span className="num">
                  {percent(stats.battery.percent)} · {stats.battery.charging ? "charging" : "on battery"}
                </span>
              </li>
            )}
          </ul>
        </div>
      );
  }
}

function Fact({ term, children }: { term: string; children: ReactNode }) {
  return (
    <div className="system-facts__row">
      <dt>{term}</dt>
      <dd>{children}</dd>
    </div>
  );
}

function Processes({ stats, by }: { stats: SystemStats; by: "cpu" | "memory" }) {
  const top = topProcesses(stats.processes, by);
  if (top.length === 0) return null;
  return (
    <table className="system-processes">
      <caption className="visually-hidden">{by === "cpu" ? "Busiest processes" : "Largest processes"}</caption>
      <thead>
        <tr>
          <th scope="col">Process</th>
          <th scope="col" className={by === "cpu" ? "is-sorted" : undefined} aria-sort={by === "cpu" ? "descending" : undefined}>
            Processor
          </th>
          <th scope="col" className={by === "memory" ? "is-sorted" : undefined} aria-sort={by === "memory" ? "descending" : undefined}>
            Memory
          </th>
        </tr>
      </thead>
      <tbody>
        {top.map((p) => (
          <tr key={p.pid}>
            <th scope="row">
              <span className="system-processes__name">{p.name}</span> <span className="faint num">{p.pid}</span>
            </th>
            <td className="num">{p.cpu < 0.1 ? "0%" : p.cpu < 10 ? `${p.cpu.toFixed(1)}%` : percent(p.cpu)}</td>
            <td className="num">{bytes(p.memory)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
