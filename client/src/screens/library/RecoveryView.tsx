// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import type { LibraryRecovery, RecoveryVersion } from "../../protocol/types";
import { commandResult } from "../../host/commandResult";
import { Screen, useRoute } from "../../shell/Screen";
import { Button } from "../../design/Button";
import { Empty } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Age } from "../../design/Age";
import { Sheet } from "../../design/Sheet";
import { Segmented } from "../../design/Chip";
import { announce } from "../../design/Announcer";
import { useStore } from "../../state/store";
import {
  ConnectionNote,
  Diff,
  StorageNote,
  useLibrary,
  useRoots,
} from "./shared";
import { bytes } from "./model";
import {
  recoveryLabels,
  stageRecovery,
  versionFile,
  type RecoveryChoice,
} from "./recovery";
import type { RouteProps } from "../types";
import "./library.css";
import { InlineCode } from "../../design/InlineCode";
import { useWarning } from "../../app/warnings";

export default function Recovery({ route }: RouteProps<"library_recovery">) {
  const { session, profile, ui } = useApp(),
    online = useLink().state === "online",
    { active } = useRoute();
  const store = useLibrary(),
    roots = useRoots(),
    root = roots.find((r) => r.root_id === route.rootId);
  const grants = useHost((s) => s.grants);
  const [observation, setObservation] = useState<{
      recovery: LibraryRecovery;
      rootPath: string;
    } | null>(null),
    [error, setError] = useState<string | null>(null),
    [retry, setRetry] = useState(0),
    [busy, setBusy] = useState(false),
    [discard, setDiscard] = useState(false),
    [removed, setRemoved] = useState(false);
  const warn = useWarning("recovery.discard");
  const recovery =
    observation?.rootPath === root?.host_path &&
    observation?.recovery.item.recovery_id === route.recoveryId
      ? observation.recovery
      : null;
  const key = `recovery-choice:${route.recoveryId}`;
  const savedChoice = useStore(store, (s) => s.forms[key]);
  const choice: RecoveryChoice =
    savedChoice === "base" ||
    savedChoice === "displaced" ||
    savedChoice === "current"
      ? savedChoice
      : "proposed";
  const [against, setAgainst] = useState<"base" | "current">("current");
  useEffect(() => {
    if (!active || !online || !grants?.library_read || !root) return;
    let alive = true;
    setError(null);
    void session
      .query({
        kind: "library_recovery_read",
        root_id: route.rootId,
        recovery_id: route.recoveryId,
      })
      .then((r) => {
        if (alive)
          setObservation({ recovery: r.recovery, rootPath: root.host_path });
      })
      .catch((e: unknown) => {
        if (alive) setError(describeError(e, profile.name));
      });
    return () => {
      alive = false;
    };
  }, [
    session,
    profile.name,
    route.rootId,
    route.recoveryId,
    root?.host_path,
    online,
    grants?.library_read,
    retry,
    active,
  ]);
  const keep = async () => {
    if (!recovery) return;
    store.form(key, choice);
    setBusy(true);
    setError(null);
    try {
      await stageRecovery(session, store, recovery, choice);
      ui.push({
        screen: "file",
        rootId: route.rootId,
        path: recovery.item.path,
      });
    } catch (e) {
      const message = describeError(e, profile.name);
      setError(message);
      announce(message);
    } finally {
      setBusy(false);
    }
  };
  const remove = async () => {
    setBusy(true);
    setError(null);
    try {
      const receipt = await session.command(
        {
          command: "library_recovery_discard",
          root_id: route.rootId,
          recovery_id: route.recoveryId,
        },
        {
          label: `Discard the recovery copy of ${recovery?.item.path ?? "this file"}`,
        },
      );
      const result = await commandResult(session, receipt);
      if (result?.kind !== "library_recovery_discarded")
        throw new Error(
          "The computer hasn’t confirmed the discard. Refresh the recovery list before trying again.",
        );
      setDiscard(false);
      setRemoved(true);
      announce("Recovery copy discarded. The file on disk was not changed.");
    } catch (e) {
      const message = describeError(e, profile.name);
      setError(message);
      announce(message);
    } finally {
      setBusy(false);
    }
  };
  const readable = !!grants?.library_read && !!root;
  const selected = recovery && versionFile(recovery[choice]),
    base = recovery && versionFile(recovery[against]);
  const canKeep =
    readable &&
    online &&
    root?.writable &&
    grants?.library_write &&
    selected &&
    selected.newline !== "mixed" &&
    recovery?.current.state === "text" &&
    recovery.current.file.newline !== "mixed";
  return (
    <Screen
      title="Check a save"
      subtitle={recovery?.item.path}
      className="library-screen"
      footerFlows
      footer={
        recovery && readable && !removed ? (
          <div className="lib-footer">
            <Button
              block
              variant="primary"
              pending={busy}
              disabled={choice === "current" ? !online || !root?.writable || !grants?.library_write || busy : !canKeep}
              onClick={() => void (choice === "current" ? remove() : keep())}
            >
              {choice === "current" ? "Keep what's on disk · discard the recovery copy" : `Keep “${recoveryLabels[choice]}” · Review save`}
            </Button>
          </div>
        ) : undefined
      }
    >
      <div className="lib-stack">
        <ConnectionNote at={recovery?.observed_at} />
        <StorageNote />
        {error && (
          <Notice tone="warn" title="Couldn’t continue">
            {error}
          </Notice>
        )}
        {!readable ? (
          <Notice title="Recovery unavailable">
            This device needs Library access to the original shared folder.
          </Notice>
        ) : removed ? (
          <Empty title="Recovery copy discarded">
            The file on disk was not changed.
          </Empty>
        ) : (
          <>
            <Button
              disabled={!online || busy}
              onClick={() => setRetry((n) => n + 1)}
            >
              Refresh comparison
            </Button>
            {!recovery ? (
              <Empty
                title={
                  online
                    ? "Reading recovery copy…"
                    : "Connect to inspect this save"
                }
              >
                Opening a recovery copy never changes the file.
              </Empty>
            ) : (
              <>
                <p className="lib-path mono">
                  {root.label} › {recovery.item.path}
                </p>
                <Notice tone="unknown" title="Needs checking">
                  The computer retained these versions from a save. Compare them
                  before choosing what to keep.
                </Notice>
                <p className="lib-meta">
                  Recovery kept{" "}
                  <Age at={recovery.item.created_at} suffix=" ago" /> · Versions
                  observed <Age at={recovery.observed_at} suffix=" ago" />
                </p>
                <fieldset className="lib-recovery-choices">
                  <legend>Version to keep</legend>
                  {(Object.keys(recoveryLabels) as RecoveryChoice[]).map(
                    (k) => (
                      <label key={k}>
                        <input
                          type="radio"
                          name={`recovery-${route.recoveryId}`}
                          checked={choice === k}
                          onChange={() => store.form(key, k)}
                        />
                        {recoveryLabels[k]}
                      </label>
                    ),
                  )}
                </fieldset>
                {choice === "current" && <>
                  <p>The file on disk stays as it is. Confirm below to permanently discard only the recovery copy.</p>
                  <Button disabled={!canKeep || busy} onClick={() => void keep()}>Edit the on-disk version…</Button>
                </>}
                <Segmented
                  label="Compare against"
                  value={against}
                  options={[
                    { value: "current", label: "On disk now" },
                    { value: "base", label: "Starting version" },
                  ]}
                  onChange={setAgainst}
                />
                <section className="lib-stack" aria-label="Selected comparison">
                  <h2>
                    {recoveryLabels[against]} → {recoveryLabels[choice]}
                  </h2>
                  {selected && base ? (
                    <Diff base={base.text} text={selected.text} />
                  ) : (
                    <Notice>
                      One of these versions cannot be shown as text. Details are
                      below.
                    </Notice>
                  )}
                </section>
                <details className="lib-details">
                  <summary>Read all versions</summary>
                  {(Object.keys(recoveryLabels) as RecoveryChoice[]).map(
                    (k) => (
                      <section key={k}>
                        <h2>{recoveryLabels[k]}</h2>
                        <Version version={recovery[k]} />
                      </section>
                    ),
                  )}
                </details>
                <p className="lib-meta">
                  An external editor may still be writing to the displaced
                  version. These are observations, not final versions.
                </p>
                {!root.writable || !grants?.library_write ? (
                  <Notice>
                    Library writes are unavailable for this folder. You can
                    compare the retained versions.
                  </Notice>
                ) : recovery.current.state !== "text" ? (
                  <Notice>
                    Restore a readable destination on the computer before saving
                    here. The recovery versions remain available.
                  </Notice>
                ) : recovery.current.file.newline === "mixed" ? (
                  <Notice>
                    The destination has mixed line endings. Edit on the computer
                    to preserve its formatting.
                  </Notice>
                ) : selected?.newline === "mixed" ? (
                  <Notice>
                    The selected version has mixed line endings. Review it on
                    the computer to preserve its formatting.
                  </Notice>
                ) : null}
                {choice !== "current" && <Button
                  variant="stop-outline"
                  disabled={
                    !online || !root.writable || !grants?.library_write || busy
                  }
                  pending={busy && !discard}
                  onClick={() => warn.ask(() => setDiscard(true), () => void remove())}
                >
                  Discard this recovery copy{warn.more}
                </Button>}
              </>
            )}
          </>
        )}
      </div>
      <Sheet
        className="library-sheet"
        open={discard && active}
        title={`Discard the recovery copy of ${recovery?.item.path ?? "this file"}?`}
        onClose={() => {
          if (!busy) setDiscard(false);
        }}
        footer={
          <Button
            block
            variant="stop"
            pending={busy}
            disabled={!online || !readable || !grants?.library_write}
            onClick={() => { warn.agreed(); void remove(); }}
          >
            Discard the recovery copy of {recovery?.item.path}
          </Button>
        }
      >
        <div className="lib-stack">
          <p>
            This permanently removes only this recovery snapshot. The file on
            disk is not changed. There is no undo for the recovery copy.
          </p>
          {warn.check}
          {error && <Notice tone="unknown">{error}</Notice>}
        </div>
      </Sheet>
    </Screen>
  );
}
function Version({ version }: { version: RecoveryVersion }) {
  if (version.state === "missing") return <p>Not present when checked.</p>;
  if (version.state === "unavailable")
    return (
      <Notice>
        {version.size == null ? "Size unknown" : bytes(version.size)} ·{" "}
        {version.reason}
      </Notice>
    );
  return (
    <>
      <p className="lib-meta">
        {bytes(version.file.size)} · Revision{" "}
        <InlineCode>{version.file.revision.slice(0, 12)}</InlineCode>
      </p>
      <pre className="lib-source">{version.file.text}</pre>
    </>
  );
}
