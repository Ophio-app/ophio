// SPDX-License-Identifier: Apache-2.0
//
// The files a sent chat message carried, as small chips under it. Where
// this device may fetch files, a tap brings one to the phone as Send to
// phone does in Library, and Transfers shows it arriving; elsewhere the chip
// only names the file.

import { File as FileIcon } from "lucide-react";
import { useApp, useHost } from "../../app/context";
import type { AttachedFile } from "../../protocol/types";
import { transferManager } from "../transfers/manager";
import { amount } from "./chatFiles";

export function SentFiles({ files }: { files: AttachedFile[] }) {
  const app = useApp();
  const fetches = useHost((s) => !!s.grants?.transfers);
  const open = (f: AttachedFile) => {
    transferManager(app).startDownload({ kind: "artifact", artifact_id: f.artifact_id }, f.name);
    app.ui.push({ screen: "transfers" });
  };
  return (
    <ul className="talk-user__files" aria-label={`${files.length} ${files.length === 1 ? "file" : "files"} sent`}>
      {files.map((f) => {
        const body = <>
          <FileIcon aria-hidden strokeWidth={2.1} />
          <span className="talk-user__file-name">{f.name}</span>
          <span className="talk-user__file-size">{amount(f.size)}</span>
        </>;
        return (
          <li key={f.artifact_id}>
            {fetches
              ? <button type="button" className="talk-user__file" aria-label={`Send ${f.name} to this device`} onClick={() => open(f)}>{body}</button>
              : <span className="talk-user__file">{body}</span>}
          </li>
        );
      })}
    </ul>
  );
}
