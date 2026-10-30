// SPDX-License-Identifier: Apache-2.0
import { commandResult } from "../../host/commandResult";
import { useId, useMemo, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { useStore } from "../../state/store";
import { Screen } from "../../shell/Screen";
import { Button } from "../../design/Button";
import { Chip, Segmented } from "../../design/Chip";
import { TextField } from "../../design/Field";
import { Empty } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { Age } from "../../design/Age";
import { announce } from "../../design/Announcer";
import { draftStart, fileKey } from "./drafts";
import { basename, copyName, dirname, forSave, join, validName } from "./model";
import { mergeFitsLimits } from "./save";
import { mergeParts, resolveMerge, type Keep, type MergePart } from "./diff";
import {
  ConnectionNote,
  Diff,
  StorageNote,
  useLibrary,
  useScreenBody,
  useRoots,
} from "./shared";
import type { RouteProps } from "../types";
import "./library.css";
import { InlineCode } from "../../design/InlineCode";
import { useWarning } from "../../app/warnings";
export default function Conflict({ route }: RouteProps<"conflict">) {
  const bodyRef = useScreenBody();
  const { ui, session, profile } = useApp(),
    store = useLibrary(),
    link = useLink(),
    roots = useRoots();
  const draft = useStore(
      store,
      (s) => s.drafts[fileKey(route.rootId, route.path)],
    ),
    root = roots.find((r) => r.root_id === route.rootId),
    grant = useHost((s) => s.grants?.library_write);
  const [pair, setPair] = useState<"phone" | "computer" | "both">("phone"),
    [sheet, setSheet] = useState<"copy" | "discard" | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState<string | null>(null);
  const warn = useWarning("draft.discard");
  const discard = () => {
    store.removeDraft(route.rootId, route.path);
    setSheet(null);
    ui.back();
  };
  const nameKey = `copy:${route.rootId}:${route.path}`,
    name = useStore(store, (s) => s.forms[nameKey] ?? copyName(route.path));
  const overlapsId = useId();
  const current = draft?.current;
  // Only this device's changes can be compared when the computer's version
  // is too large to send here.
  const tooLarge = !current && !!draft?.currentTooLarge;
  const parts = useMemo(
    () => (draft && current ? mergeParts(draft.base.text, draft.text, current.text) : []),
    [draft, current],
  );
  const overlaps = parts.filter((p) => p.kind === "overlap");
  // Choices belong to the versions they were made for.
  const versions = `${current?.revision}:${draft?.updatedAt}`;
  const [chosen, setChosen] = useState<{ versions: string; keep: Keep[] }>({ versions, keep: [] });
  const keep = chosen.versions === versions ? chosen.keep : [];
  const merged = useMemo(() => current ? forSave(resolveMerge(parts, keep), current.newline) : "", [parts, keep, current]);
  const mergeTooLarge = !!current && !mergeFitsLimits(current, merged);
  const unresolved = overlaps.filter((_, n) => !keep[n]).length;
  const choose = (n: number, k: Keep) => {
    const next = [...keep];
    next[n] = k;
    setChosen({ versions, keep: next });
  };
  const merge = () => {
    if (!draft || !current || mergeTooLarge || unresolved > 0) return;
    const byHand = keep.filter((k) => k === "hand").length;
    store.saveDraft({
      base: current,
      text: merged,
      updatedAt: Date.now(),
      startedAt: draftStart(),
      phase: "draft",
      reviewMerge: true,
      mergeMarkers: byHand > 0,
    });
    announce(
      byHand
        ? `Merged into a new draft. ${byHand} ${byHand === 1 ? "overlap is" : "overlaps are"} marked to edit by hand.`
        : "Merged into a new draft. Review every change before saving.",
    );
    ui.back();
  };
  const saveCopy = async () => {
    if (!draft) return;
    setBusy(true);
    setError(null);
    try {
      const path = join(dirname(route.path), name);
      const r = await session.command(
        {
          command: "library_create",
          root_id: route.rootId,
          path,
          text: forSave(draft.text, draft.base.newline, draft.base.text),
        },
        { label: `Save new copy ${name}` },
      );
      const result = await commandResult(session, r);
      if (result?.kind !== "created")
        throw new Error(
          "The computer hasn’t confirmed the new copy. Check the folder before trying again.",
        );
      announce(`Created ${result.path}. Your original draft is kept.`);
      setSheet(null);
      ui.push({ screen: "file", rootId: route.rootId, path: result.path });
    } catch (e) {
      setError(
        session.online
          ? describeError(e, profile.name)
          : "The computer hasn’t confirmed the new copy. Reconnect and check the folder before trying again.",
      );
    } finally {
      setBusy(false);
    }
  };
  return (
    <Screen
      bodyRef={bodyRef}
      className="library-screen"
      title="Review conflicting changes"
      subtitle={basename(route.path)}
      footer={
        draft ? (
          <div className="lib-footer">
            <div className="lib-stack">
              {mergeTooLarge && <Notice title="Finish this merge on the computer">
                The combined file is too large to edit and save here. Your original draft is kept.
                Save it as a new copy, then combine the files on the computer.
              </Notice>}
              {unresolved > 0 && (
                <p className="lib-meta">
                  Choose what to keep for {unresolved} more{" "}
                  {unresolved === 1 ? "overlap" : "overlaps"} before merging.
                </p>
              )}
              <Button
                block
                variant="primary"
                disabled={!current || mergeTooLarge || !root?.writable || !grant || unresolved > 0}
                onClick={merge}
              >
                Merge into a new draft
              </Button>
            </div>
          </div>
        ) : undefined
      }
    >
      {!draft ? (
        <Empty
          title="No draft on this device to recover"
          action={<Button onClick={() => ui.back()}>Back to file</Button>}
        >
          The draft may already have been saved or discarded.
        </Empty>
      ) : (
        <div className="lib-stack">
          <p className="lib-path mono">
            {root?.label} › {route.path}
          </p>
          <Notice
            tone="warn"
            title={
              current
                ? "Both versions are kept"
                : tooLarge
                  ? "The computer’s version is too large to show here"
                  : "The file is missing on the computer"
            }
          >
            {tooLarge
              ? "Your draft is kept on this device. The file on the computer is too large to compare or merge here. Save your draft as a new copy, or keep it and compare the two on the computer."
              : "Your draft is kept on this device. Review the computer’s current state before saving."}
          </Notice>
          <ConnectionNote at={draft.currentAt} />
          <StorageNote />
          <div className="lib-versions">
            <div>
              <h2>Base</h2>
              <p className="lib-meta">
                What you started from ·{" "}
                <InlineCode>{draft.base.revision.slice(0, 10)}</InlineCode>
                {draft.base.modified_at != null && (
                  <>
                    {" "}
                    · <Age at={draft.base.modified_at} suffix=" ago" />
                  </>
                )}
              </p>
            </div>
            <div>
              <h2>Draft on this device</h2>
              <p className="lib-meta">
                Edited <Age at={draft.updatedAt} suffix=" ago" />
              </p>
            </div>
            <div>
              <h2>Now on the computer</h2>
              <p className="lib-meta">
                {current ? (
                  <>
                    Revision <InlineCode>{current.revision.slice(0, 10)}</InlineCode>
                    {draft.currentAt && (
                      <>
                        {" "}
                        · Checked <Age at={draft.currentAt} suffix=" ago" />
                      </>
                    )}
                  </>
                ) : tooLarge ? (
                  "Too large to show on this device"
                ) : (
                  "File missing"
                )}
              </p>
            </div>
          </div>
          {current && (
            <section className="lib-stack lib-overlaps" aria-labelledby={overlapsId}>
              <h2 id={overlapsId}>Overlapping changes</h2>
              {overlaps.length === 0 ? (
                <p className="lib-meta">
                  None. Merging keeps the changes from both sides.
                </p>
              ) : (
                <>
                  <p className="lib-meta">
                    Both sides changed these lines. Choose what to keep in each;
                    Keep both puts your draft’s lines first. Other changes merge
                    on their own.
                  </p>
                  {overlaps.map((part, n) => (
                    <Overlap
                      key={n}
                      part={part}
                      n={n}
                      count={overlaps.length}
                      keep={keep[n]}
                      onChoose={(k) => choose(n, k)}
                    />
                  ))}
                </>
              )}
            </section>
          )}
          {!tooLarge && (
            <Segmented
              label="Compare versions"
              value={pair}
              options={[
                { value: "phone", label: "Base → This device" },
                { value: "computer", label: "Base → Computer" },
                { value: "both", label: "Computer → This device" },
              ]}
              onChange={setPair}
            />
          )}
          <Diff
            key={tooLarge ? "phone" : pair}
            base={!tooLarge && pair === "both" ? (current?.text ?? "") : draft.base.text}
            text={!tooLarge && pair === "computer" ? (current?.text ?? "") : draft.text}
          />
          <details className="lib-details">
            <summary>Read the three versions</summary>
            {[
              { name: "Base", text: draft.base.text },
              { name: "Draft on this device", text: draft.text },
              {
                name: "Now on the computer",
                text:
                  current?.text ??
                  (tooLarge ? "Too large to show on this device." : "File missing"),
              },
            ].map((v) => (
              <section key={v.name}>
                <h2>{v.name}</h2>
                <pre className="lib-source">{v.text}</pre>
              </section>
            ))}
          </details>
          <div className="lib-stack">
            <Button
              disabled={link.state !== "online" || !root?.writable || !grant}
              onClick={() => setSheet("copy")}
            >
              Save as a new copy…
            </Button>
            <Button variant="quiet" onClick={() => ui.back()}>
              Keep draft for later
            </Button>
            <Button variant="stop-outline" onClick={() => warn.ask(() => setSheet("discard"), discard)}>
              Discard my draft{warn.more}
            </Button>
          </div>
        </div>
      )}
      <Sheet
        className="library-sheet"
        open={sheet !== null}
        onClose={() => {
          if (!busy) setSheet(null);
        }}
        title={
          sheet === "copy"
            ? "Save beside the original"
            : `Discard draft for ${basename(route.path)}?`
        }
        footer={
          sheet === "copy" ? (
            <Button
              block
              variant="primary"
              pending={busy}
              disabled={
                link.state !== "online" ||
                !!validName(name) ||
                name === basename(route.path)
              }
              onClick={() => void saveCopy()}
            >
              Create new copy
            </Button>
          ) : (
            <Button
              block
              variant="stop"
              onClick={() => { warn.agreed(); discard(); }}
            >
              Discard draft for {basename(route.path)}
            </Button>
          )
        }
      >
        <div className="lib-stack">
          {sheet === "copy" ? (
            <>
              <TextField
                label="New copy name"
                value={name}
                onChange={(v) => store.form(nameKey, v)}
                error={
                  name === basename(route.path)
                    ? "Choose a different name from the original."
                    : validName(name)
                }
              />
              <p className="lib-path mono">
                {root?.label} › {join(dirname(route.path), name)}
              </p>
              <Notice>
                This creates a separate file. If this name already exists,
                creation is refused. The original draft on this device is kept.
              </Notice>
              {draft?.base.has_bom && (
                <Notice>
                  The new copy uses UTF-8 without a byte-order mark.
                </Notice>
              )}
            </>
          ) : (
            <p>
              Remove the draft from this device permanently. The computer’s file stays
              unchanged.
            </p>
          )}
          {sheet === "discard" && warn.check}
          {error && <Notice tone="stop">{error}</Notice>}
        </div>
      </Sheet>
    </Screen>
  );
}
const KEEP: { value: Keep; label: string }[] = [
  { value: "mine", label: "Keep my draft" },
  { value: "theirs", label: "Keep the computer’s" },
  { value: "both", label: "Keep both" },
  { value: "hand", label: "Edit by hand" },
];
function Overlap({
  part,
  n,
  count,
  keep,
  onChoose,
}: {
  part: Extract<MergePart, { kind: "overlap" }>;
  n: number;
  count: number;
  keep: Keep | undefined;
  onChoose: (k: Keep) => void;
}) {
  const id = useId();
  return (
    <section className="lib-overlap" aria-labelledby={id}>
      <h3 id={id}>
        Overlap {n + 1} of {count}
      </h3>
      {[
        { name: "Draft on this device", lines: part.mine, removed: "Removed in your draft" },
        { name: "Now on the computer", lines: part.theirs, removed: "Removed on the computer" },
      ].map((side) => (
        <div key={side.name} className="lib-overlap-side">
          <p className="lib-meta">{side.name}</p>
          {side.lines.length ? (
            <pre className="lib-source">{side.lines.join("\n")}</pre>
          ) : (
            <p className="lib-overlap-removed">{side.removed}</p>
          )}
        </div>
      ))}
      <div className="lib-filters" role="group" aria-label={`Keep for overlap ${n + 1}`}>
        {KEEP.map((k) => (
          <Chip key={k.value} selected={keep === k.value} onClick={() => onChoose(k.value)}>
            {k.label}
          </Chip>
        ))}
      </div>
      {keep === "hand" && (
        <p className="lib-meta">
          Both versions go into the draft between markers. Keep the text you
          want and remove each marker line before saving.
        </p>
      )}
    </section>
  );
}
