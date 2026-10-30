// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { shallowEqual, useStore } from "../../state/store";
import { useLibrary } from "./shared";
import { useWarning } from "../../app/warnings";

/** Never replace a destination draft or retarget an unresolved save. */
export function MovedDrafts({ rootId, path }: { rootId: string; path: string }) {
  const store = useLibrary();
  const drafts = useStore(store, (s) => Object.values(s.drafts)
    .filter((d) => d.base.root_id === rootId && d.movedTo === path), shallowEqual);
  const [opened, setOpened] = useState<string | null>(null);
  const [discard, setDiscard] = useState(false);
  const warn = useWarning("draft.discard");
  const draft = drafts.find((d) => d.base.path === opened);
  return <>
    {drafts.map((d) => <Notice key={d.base.path} title="A draft from this file’s old location exists">
      <span className="lib-path mono">{d.base.path}</span>
      It remains separate from any draft at this location.
      <div className="lib-actions">
        <Button onClick={() => { setOpened(d.base.path); setDiscard(false); }}>Open old draft</Button>
        <Button variant="quiet" onClick={() => warn.ask(() => { setOpened(d.base.path); setDiscard(true); }, () => {
          // A save still being checked keeps its warning, which says so.
          if (d.phase === "sending" || d.phase === "unknown") { setOpened(d.base.path); setDiscard(true); }
          else store.discardDraft(rootId, d.base.path);
        })}>Discard old draft{warn.more}</Button>
      </div>
    </Notice>)}
    <Sheet className="library-sheet" open={!!draft} title={discard ? "Discard old draft?" : "Draft from the old location"}
      subtitle={opened} onClose={() => setOpened(null)} footer={discard && draft && <Button variant="stop-outline" block
        disabled={draft.phase === "sending"}
        onClick={() => { warn.agreed(); store.discardDraft(rootId, draft.base.path); setOpened(null); }}>Discard old device draft</Button>}>
      {draft && <div className="lib-stack">
        {(draft.phase === "sending" || draft.phase === "unknown") && <Notice tone="unknown">
          The earlier save hasn’t been confirmed. It may already have changed the computer’s file. Discarding removes only this device’s draft; the save still needs checking.
        </Notice>}
        {discard ? <><p>This permanently removes only the draft on this device. It does not change the moved file or its destination draft.</p>{warn.check}</>
          : <pre className="lib-source">{draft.text}</pre>}
      </div>}
    </Sheet>
  </>;
}
