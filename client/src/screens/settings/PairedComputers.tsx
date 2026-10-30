// SPDX-License-Identifier: Apache-2.0
//
// The computers this device is paired with: switch between them, rename them
// here, pair another. Two pairings can carry one name (a reinstalled
// computer, an old pairing at another port); then each shows what tells
// them apart: the fingerprint `pair` printed, its address and when this
// device last reached it.

import { useId, useMemo, useState } from "react";
import { useApp, useHost, useLink } from "../../app/context";
import type { HostProfile } from "../../host/link";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { deviceKind } from "../../design/formFactor";
import { Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { cachedState } from "../../platform/offline";
import { fingerprintWords } from "../../protocol/pairing";

const NAME_MAX = 64;

const key = (name: string) => name.trim().toLocaleLowerCase();

/** Profiles whose name another paired computer also has. */
export function sharedNames(profiles: readonly HostProfile[]): Set<string> {
  const seen = new Map<string, number>();
  for (const p of profiles) seen.set(key(p.name), (seen.get(key(p.name)) ?? 0) + 1);
  return new Set(profiles.filter((p) => (seen.get(key(p.name)) ?? 0) > 1).map((p) => p.profileId));
}

export function PairedComputers() {
  const app = useApp();
  const { profile, profiles } = app;
  const online = useLink().state === "online";
  const lastHeard = useHost((s) => s.lastHeardAt);
  const [renaming, setRenaming] = useState<HostProfile | null>(null);
  const shared = useMemo(() => sharedNames(profiles), [profiles]);
  // Other computers are only known from what this device kept after its last visit.
  const reached = useMemo(
    () => new Map(profiles.filter((p) => shared.has(p.profileId)).map((p) => [p.profileId, cachedState(p)?.lastHeardAt ?? null])),
    [profiles, shared],
  );
  return (
    <Section title="Paired computers">
      <div className="settings-stack">
        {profiles.map((computer) => {
          const current = computer.profileId === profile.profileId;
          return (
            <Computer
              key={computer.profileId}
              computer={computer}
              current={current}
              online={online}
              lastReached={current ? lastHeard ?? reached.get(computer.profileId) ?? null : reached.get(computer.profileId) ?? null}
              distinguish={shared.has(computer.profileId)}
              onSwitch={() => app.switchTo(computer.profileId)}
              onRename={() => setRenaming(computer)}
            />
          );
        })}
        <Button onClick={app.pairAnother}>Pair another computer</Button>
      </div>
      {renaming && <Rename computer={renaming} onClose={() => setRenaming(null)} />}
    </Section>
  );
}

function Computer({ computer, current, online, lastReached, distinguish, onSwitch, onRename }: {
  computer: HostProfile; current: boolean; online: boolean; lastReached: number | null; distinguish: boolean;
  onSwitch(): void; onRename(): void;
}) {
  const facts = useId();
  return (
    <div className="settings-card settings-stack">
      <h3>{computer.name}</h3>
      {current && <p className="settings-meta">Current computer · {online ? "Connected" : "Disconnected"}</p>}
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
              {current && online ? "Now" : lastReached ? <Age at={lastReached} suffix=" ago" /> : "Not recorded on this device"}
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
        <Button onClick={onRename} aria-describedby={distinguish ? facts : undefined}>
          Rename {computer.name}
        </Button>
      </div>
    </div>
  );
}

function Rename({ computer, onClose }: { computer: HostProfile; onClose(): void }) {
  const app = useApp();
  const [name, setName] = useState(computer.name);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    setPending(true);
    setError(null);
    try {
      await app.rename(computer.profileId, name.trim());
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
        <Button variant="primary" pending={pending} disabled={!name.trim() || name.trim() === computer.name} onClick={() => void save()}>
          Save name
        </Button>
      }
    >
      <TextField
        label="Computer name"
        value={name}
        onChange={setName}
        maxLength={NAME_MAX}
        autoComplete="off"
        hint={`Only this ${deviceKind()} uses this name. The computer keeps its own.`}
      />
      {error && <Notice tone="stop" title="Name not saved">{error}</Notice>}
    </Sheet>
  );
}
