// SPDX-License-Identifier: Apache-2.0
//
// The computers this device is paired with: switch between them, rename them,
// pair another. A computer that keeps its own name takes the new one, so
// every phone and tablet paired with it shows it; an older one is renamed on
// this device alone. Two pairings can carry one name (a reinstalled
// computer, an old pairing at another port); then each shows what tells
// them apart: the fingerprint `pair` printed, its address and when this
// device last reached it.

import { useId, useMemo, useState } from "react";
import { Pencil } from "lucide-react";
import { useApp } from "../../app/context";
import { COMPUTER_NAME_MAX, renameHint, renameWay, sharedNames } from "../../app/computerName";
import { useComputerEntries, useComputerFreshness, type ComputerEntry } from "../../app/fleet";
import { emptyHostState } from "../../state/projection";
import { Store, useStore } from "../../state/store";
import type { LinkState } from "../../host/link";
import { later, useHeardInBackground } from "../../app/lastHeard";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { deviceKind } from "../../design/formFactor";
import { Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { cachedState } from "../../platform/offline";
import { fingerprintWords } from "../../protocol/pairing";

/** The longest name this device keeps for a computer. */
const NAME_MAX = 64;

export { sharedNames };

export function PairedComputers() {
  const app = useApp();
  const entries = useComputerEntries();
  const profiles = useMemo(() => entries.map((e) => e.profile), [entries]);
  const [renaming, setRenaming] = useState<ComputerEntry | null>(null);
  const shared = useMemo(() => sharedNames(profiles), [profiles]);
  const heard = useHeardInBackground(shared.size > 0);
  // A computer let go of while the app was away is known from what this device kept.
  const reached = useMemo(
    () => new Map(profiles.filter((p) => shared.has(p.profileId)).map((p) => [p.profileId, cachedState(p)?.lastHeardAt ?? null])),
    [profiles, shared],
  );
  return (
    <Section title="Paired computers">
      <div className="settings-stack">
        {entries.map((entry) => (
          <Computer
            key={entry.profile.profileId}
            entry={entry}
            lastReached={reached.get(entry.profile.profileId)}
            heard={heard?.[entry.profile.profileId]}
            distinguish={shared.has(entry.profile.profileId)}
            onSwitch={() => app.switchTo(entry.profile.profileId)}
            onRename={() => setRenaming(entry)}
          />
        ))}
        <Button onClick={app.pairAnother}>Pair another computer</Button>
      </div>
      {renaming && <Rename entry={entries.find((e) => e.profile.profileId === renaming.profile.profileId) ?? renaming} onClose={() => setRenaming(null)} />}
    </Section>
  );
}

const IDLE = new Store<LinkState>({ state: "idle" });
const EMPTY = new Store(emptyHostState);

function Computer({ entry, lastReached, heard, distinguish, onSwitch, onRename }: {
  entry: ComputerEntry; lastReached: number | null | undefined; heard: number | undefined; distinguish: boolean;
  onSwitch(): void; onRename(): void;
}) {
  const facts = useId();
  const { profile: computer, current } = entry;
  const f = useComputerFreshness(entry);
  const own = useStore(entry.session?.state ?? EMPTY, (s) => s.lastHeardAt);
  const reached = later(own ?? lastReached, heard);
  const live = f.live;
  return (
    <div className="settings-card settings-stack">
      <h3>{computer.name}</h3>
      <p className="settings-meta">{current ? "Current computer · " : ""}{live ? "Connected" : f.label}</p>
      {distinguish && (
        <dl className="settings-facts" id={facts}>
          <div>
            <dt>Fingerprint</dt>
            <dd className="mono">{fingerprintWords(computer.certSha256)}</dd>
          </div>
          <div>
            <dt>{computer.addresses.length === 1 ? "Address" : "Addresses"}</dt>
            <dd className="mono">{computer.addresses.join(", ")}</dd>
          </div>
          <div>
            <dt>Last reached</dt>
            <dd>
              {live ? "Now" : reached ? <Age at={reached} suffix=" ago" /> : "Not recorded on this device"}
            </dd>
          </div>
        </dl>
      )}
      <div className="settings-actions">
        {!current && (
          <Button onClick={onSwitch} aria-describedby={distinguish ? facts : undefined}>
            Switch to {computer.name}
          </Button>
        )}
        <Button icon={Pencil} onClick={onRename} aria-describedby={distinguish ? facts : undefined}>
          Rename {computer.name}
        </Button>
      </div>
    </div>
  );
}

function Rename({ entry, onClose }: { entry: ComputerEntry; onClose(): void }) {
  const app = useApp();
  const computer = entry.profile;
  const state = useStore(entry.session?.state ?? EMPTY, (s) => s);
  const link = useStore(entry.session?.link ?? IDLE, (l) => l);
  const way = entry.session ? renameWay(state, link) : "phone";
  const [name, setName] = useState(computer.name);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trimmed = name.trim();
  const max = way === "phone" ? NAME_MAX : COMPUTER_NAME_MAX;
  const save = async () => {
    // The computer's name changes here at once; if it refuses, it goes back
    // and a message says why.
    if (way === "computer") { void app.rename(computer.profileId, trimmed); onClose(); return; }
    setPending(true);
    setError(null);
    try {
      await app.rename(computer.profileId, trimmed);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : "This device couldn’t save the name. Try again.");
    } finally {
      setPending(false);
    }
  };
  return (
    <Sheet
      open
      title={`Rename ${computer.name}`}
      onClose={() => { if (!pending) onClose(); }}
      footer={
        <Button variant="primary" pending={pending} disabled={way === "wait" || !trimmed || trimmed === computer.name || [...trimmed].length > max} onClick={() => void save()}>
          Save name
        </Button>
      }
    >
      <TextField
        label="Computer name"
        value={name}
        onChange={setName}
        maxLength={max}
        autoComplete="off"
        disabled={way === "wait"}
        hint={renameHint(way, computer.name, deviceKind(), state, link)}
      />
      {error && <Notice tone="stop" title="Name not saved">{error}</Notice>}
    </Sheet>
  );
}
