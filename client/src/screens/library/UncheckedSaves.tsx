// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { describeError, useApp, useLink } from "../../app/context";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { shallowEqual, useStore } from "../../state/store";
import type { UncheckedSave } from "./drafts";
import { useLibrary } from "./shared";

export function UncheckedSaves({ rootId, path }: { rootId: string; path: string }) {
  const { session, profile } = useApp(), store = useLibrary(), online = useLink().state === "online";
  const saves = useStore(store, (s) => Object.values(s.uncheckedSaves)
    .filter((save) => save.rootId === rootId && (save.movedTo ?? save.path) === path), shallowEqual);
  const [busy, setBusy] = useState(false), [message, setMessage] = useState<string | null>(null);
  const check = async (save: UncheckedSave) => {
    setBusy(true);
    try {
      const { file } = await session.query({ kind: "library_read", root_id: rootId, path });
      store.cacheFile(file);
      store.checkedSave(rootId, save.path);
      setMessage("Showing the file just read from the computer. The discarded draft can no longer be compared; this does not confirm which changes the earlier save made.");
    } catch (e) { setMessage(describeError(e, profile.name)); }
    finally { setBusy(false); }
  };
  return <>
    {saves.map((save) => <Notice key={save.path} tone="unknown" title="The earlier save still needs checking"
      action={<Button disabled={!online || busy} pending={busy} onClick={() => void check(save)}>Check computer file</Button>}>
      The device draft was discarded. The earlier save may already have changed the computer’s file.
      {" "}Requested <Age at={save.at} suffix=" ago" />.
    </Notice>)}
    {message && <Notice>{message}</Notice>}
  </>;
}
