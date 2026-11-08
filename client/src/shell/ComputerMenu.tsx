// SPDX-License-Identifier: Apache-2.0
//
// The header's computer menu: the pill names the computer on screen, or All
// computers, with a dot for how current each is. The menu lists All and
// every paired computer, each with its own dot and its connection details,
// and pairs another.

import { Check, ChevronDown, Info, Plus } from "lucide-react";
import { useMemo, useState } from "react";
import { AppProvider, useApp } from "../app/context";
import { outOfReach, useComputerEntries, useComputerFreshness, useEveryComputer, type ComputerEntry } from "../app/fleet";
import type { Route } from "../app/ui";
import { Age } from "../design/Age";
import { IconButton } from "../design/Button";
import { ListRow, Section } from "../design/ListRow";
import { Sheet } from "../design/Sheet";
import type { LinkState } from "../host/link";
import type { HostState } from "../state/projection";
import { sharedNames } from "../app/computerName";
import { freshness, type Freshness } from "./freshness";
import { StatusSheet } from "./StatusSheet";
import { toast } from "../design/Toast";
import "./ComputerMenu.css";

type Tone = Freshness["tone"];

/** How each computer is doing, for All's dots: the link and its catch-up. */
function toneOf(entry: ComputerEntry, state: HostState | null, link: LinkState): { tone: Tone; live: boolean; offline: boolean } {
  const f = freshness(link, state?.caughtUp ?? false, null, 0, entry.profile.name);
  // A computer trying again after it went offline still counts as offline,
  // so All's word doesn't flicker with each attempt.
  const offline = outOfReach(link);
  return { tone: offline && f.tone === "neutral" ? "warn" : f.tone, live: f.live, offline };
}
/** All that `toneOf` reads of a computer's state. */
const caughtUp = (s: HostState | undefined) => s?.caughtUp;
const sameTones = (a: ReturnType<typeof toneOf>[], b: ReturnType<typeof toneOf>[]) =>
  a.length === b.length && a.every((x, i) => x.tone === b[i]?.tone && x.live === b[i]?.live && x.offline === b[i]?.offline);

/** What All's pill says beside its name: nothing while every computer is live. */
export function allWord(tones: { live: boolean; offline: boolean }[]): string | null {
  const offline = tones.filter((t) => t.offline).length;
  if (offline) return `${offline} offline`;
  return tones.some((t) => !t.live) ? "Connecting" : null;
}

/** The pill at the end of a tab's header. `compact` is a narrow bar's. */
export function ComputerButton({ onOpen, compact = false }: { onOpen: () => void; compact?: boolean }) {
  const { all } = useApp();
  const entries = useComputerEntries();
  return all ? <AllPill entries={entries} onOpen={onOpen} compact={compact} /> : <OnePill entry={entries.find((e) => e.current)!} onOpen={onOpen} compact={compact} />;
}

function OnePill({ entry, onOpen, compact }: { entry: ComputerEntry; onOpen: () => void; compact: boolean }) {
  const f = useComputerFreshness(entry);
  const { profiles } = useApp();
  const name = entry.profile.name;
  // A narrow bar with one computer paired keeps the project's name whole:
  // the dot alone, as before, while there is nothing to tell apart.
  const dotOnly = compact && f.live && profiles.length < 2;
  return (
    <button type="button" className={`status status--${f.tone} computer-pill${compact ? " computer-pill--compact" : ""}${dotOnly ? " computer-pill--dot" : ""}`} onClick={onOpen} aria-haspopup="dialog"
      aria-label={`${name}: ${f.label}. Choose a computer`}>
      <span className="status__dot" aria-hidden />
      {!dotOnly && <bdi className="computer-pill__name">{name}</bdi>}
      {!f.live && <span className="computer-pill__word">{f.label}</span>}
      <ChevronDown className="computer-pill__chevron" aria-hidden />
    </button>
  );
}

function AllPill({ entries, onOpen, compact }: { entries: ComputerEntry[]; onOpen: () => void; compact: boolean }) {
  const tones = useEveryComputer(entries, toneOf, sameTones, caughtUp);
  const word = allWord(tones);
  const tone: Tone = tones.some((t) => t.offline) ? "warn" : tones.some((t) => !t.live) ? "neutral" : "ok";
  return (
    <button type="button" className={`status status--${tone} computer-pill computer-pill--all${compact ? " computer-pill--compact" : ""}`} onClick={onOpen} aria-haspopup="dialog"
      aria-label={`All computers${word ? `: ${word}` : ""}. Choose a computer`}>
      <Dots tones={tones.map((t) => t.tone)} />
      {/* Beside a word, or in a narrow bar, "All" alone keeps the word
          whole; a narrow bar with a word lets the dots say All. */}
      {!(compact && word) && <bdi className="computer-pill__name">{word || compact ? "All" : "All computers"}</bdi>}
      {word && <span className="computer-pill__word">{word}</span>}
      <ChevronDown className="computer-pill__chevron" aria-hidden />
    </button>
  );
}

/** A dot for each computer, joined by a thread: All is them together. */
function Dots({ tones }: { tones: Tone[] }) {
  return (
    <span className="computer-dots" aria-hidden>
      {tones.slice(0, 4).map((tone, i) => <span key={i} className={`computer-dots__dot computer-dots__dot--${tone}`} />)}
    </span>
  );
}

/** Where each computer's dot sits in All's mark, by how many there are. */
const CLUSTER: Record<number, [number, number][]> = {
  1: [[12, 12]],
  2: [[5, 12], [19, 12]],
  3: [[12, 4.5], [4.5, 18.5], [19.5, 18.5]],
  4: [[5, 5], [19, 5], [19, 19], [5, 19]],
};

/** All's mark in the menu: the computers' dots, threaded together. */
function Cluster({ tones }: { tones: Tone[] }) {
  const shown = tones.slice(0, 4);
  const at = CLUSTER[shown.length] ?? CLUSTER[1]!;
  const threads = at.length > 2 ? [...at, at[0]!] : at;
  return (
    <svg className="computer-cluster" viewBox="0 0 24 24" aria-hidden>
      <polyline className="computer-cluster__thread" points={threads.map(([x, y]) => `${x},${y}`).join(" ")} />
      {shown.map((tone, i) => <circle key={i} className={`computer-cluster__dot computer-cluster__dot--${tone}`} cx={at[i]![0]} cy={at[i]![1]} r={3.4} />)}
    </svg>
  );
}

/** The menu the pill opens. `onDetails` opens a computer's connection details. */
export function ComputerMenu({ open, onClose, onDetails }: { open: boolean; onClose: () => void; onDetails: (profileId: string) => void }) {
  return (
    <Sheet open={open} onClose={onClose} title="Computers" size="drop" closeLabel="Close computers">
      {open && <MenuBody onClose={onClose} onDetails={onDetails} />}
    </Sheet>
  );
}

function MenuBody({ onClose, onDetails }: { onClose: () => void; onDetails: (profileId: string) => void }) {
  const app = useApp();
  const entries = useComputerEntries();
  const tones = useEveryComputer(entries, toneOf, sameTones, caughtUp);
  const shared = useMemo(() => sharedNames(entries.map((e) => e.profile)), [entries]);
  const all = app.all === true;
  return (
    <>
      <Section>
        <div className="group computer-menu">
          {entries.length > 1 && app.showAll && (
            <ListRow
              leading={<span className="row__icon computer-menu__all" aria-hidden><Cluster tones={tones.map((t) => t.tone)} /></span>}
              title="All computers"
              subtitle="Projects from every computer"
              selected={all}
              trailing={all && <Check className="row__check" aria-label="Current" />}
              onClick={() => { app.showAll?.(); onClose(); }}
            />
          )}
          {entries.map((entry) => (
            <div key={entry.profile.profileId} className="project-row">
              <ComputerRow entry={entry} selected={entry.current && !all} distinguish={shared.has(entry.profile.profileId)}
                onChoose={() => { onClose(); if (!entry.current || all) app.switchTo(entry.profile.profileId); }} />
              <IconButton icon={Info} label={`Connection details for ${entry.profile.name}`} onClick={() => onDetails(entry.profile.profileId)} />
            </div>
          ))}
        </div>
      </Section>
      <Section>
        <div className="group">
          <ListRow icon={Plus} title="Pair another computer" subtitle="Connect this device to another computer running it" chevron
            onClick={() => { onClose(); app.pairAnother(); }} />
        </div>
      </Section>
    </>
  );
}

function ComputerRow({ entry, selected, distinguish, onChoose }: { entry: ComputerEntry; selected: boolean; distinguish: boolean; onChoose: () => void }) {
  const f = useComputerFreshness(entry);
  const heard = useEveryComputer(useMemo(() => [entry], [entry]), lastHeard, sameNumbers, heardAt)[0] ?? null;
  return (
    <ListRow
      leading={<span className={`row__icon computer-menu__dot status--${f.tone}`} aria-hidden><span className="status__dot" /></span>}
      title={<bdi>{entry.profile.name}</bdi>}
      subtitle={<>
        {f.live ? "Connected" : f.label}
        {!f.live && heard !== null && <> · Last heard <Age at={heard} suffix=" ago" /></>}
        {distinguish && <><br /><span className="mono">{entry.profile.addresses.join(", ")}</span></>}
      </>}
      selected={selected}
      trailing={selected && <Check className="row__check" aria-label="Current" />}
      onClick={onChoose}
    />
  );
}
const lastHeard = (_: ComputerEntry, state: HostState | null) => state?.lastHeardAt ?? null;
const heardAt = (s: HostState | undefined) => s?.lastHeardAt;
const sameNumbers = (a: (number | null)[], b: (number | null)[]) => a.length === b.length && a.every((x, i) => x === b[i]);

/** A computer's connection details, from the menu. Another computer's are
 * read from its own session; what they open, they open on that computer. */
export function ComputerDetails({ profileId, onClose }: { profileId: string | null; onClose: () => void }) {
  const app = useApp();
  const entries = useComputerEntries();
  const entry = entries.find((e) => e.profile.profileId === profileId);
  const scoped = useMemo(() => entry && !entry.current && entry.session ? {
    ...app, profile: entry.profile, session: entry.session,
    // Refused, as while control is still being handed back, it says why.
    forget: () => (app.forgetOther?.(entry.profile.profileId) ?? Promise.resolve())
      .catch((e: unknown) => { toast(e instanceof Error ? e.message : `${entry.profile.name} wasn’t forgotten.`, { tone: "stop" }); }),
  } : null, [app, entry]);
  if (!entry) return null;
  if (entry.current) return <StatusSheet open onClose={onClose} />;
  if (!scoped) return null;
  const open = (route: Route) => { onClose(); app.switchTo(entry.profile.profileId, route); };
  return <AppProvider app={scoped}><StatusSheet open onClose={onClose} onOpenRoute={open} /></AppProvider>;
}

/** The header's pill and the menu it opens, with each computer's details. */
export function ComputerControl({ compact = false }: { compact?: boolean }) {
  const [menu, setMenu] = useState(false);
  const [details, setDetails] = useState<string | null>(null);
  return (
    <>
      <ComputerButton compact={compact} onOpen={() => setMenu(true)} />
      <ComputerMenu open={menu} onClose={() => setMenu(false)} onDetails={(id) => { setMenu(false); setDetails(id); }} />
      {details && <ComputerDetails profileId={details} onClose={() => setDetails(null)} />}
    </>
  );
}
