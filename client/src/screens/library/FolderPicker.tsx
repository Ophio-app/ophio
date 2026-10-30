// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { Folder } from "lucide-react";
import { Button } from "../../design/Button";
import { ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { useLink } from "../../app/context";
import { dirname } from "./model";
import { useFolder } from "./shared";

/** Selecting a folder only changes the draft destination. */
export function FolderPicker({
  rootId,
  initial = "",
  onChoose,
}: {
  rootId: string;
  initial?: string;
  onChoose: (path: string) => void;
}) {
  const [path, setPath] = useState(initial);
  const q = useFolder(rootId, path),
    online = useLink().state === "online";
  return (
    <div className="lib-stack">
      <p className="lib-path mono">{path || "Root folder"}</p>
      {path && (
        <Button onClick={() => setPath(dirname(path))}>Parent folder</Button>
      )}
      {q.error && <Notice tone="stop">{q.error}</Notice>}
      {q.loading && <p role="status">Loading folders…</p>}
      <div className="group">
        {q.cached?.value.entries
          .filter((e) => e.kind === "directory")
          .map((e) => (
            <ListRow
              key={e.path}
              icon={Folder}
              title={e.name}
              onClick={() => setPath(e.path)}
              chevron
            />
          ))}
      </div>
      <Button
        disabled={!online || !q.cached || !!q.error || q.loading}
        onClick={() => onChoose(path)}
      >
        Choose this folder
      </Button>
    </div>
  );
}
