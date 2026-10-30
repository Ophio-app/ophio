// SPDX-License-Identifier: Apache-2.0
//
// Where a checklist's files go when the person taps to get one: to this
// phone, to a device by Taildrop, into a folder on the computer (a drive
// app's synced folder among them), or onto a drive rclone reaches. The
// setup on the computer asks the same; this changes it.

import { useEffect, useRef, useState } from "react";
import { FolderDown, HardDrive, Send, Smartphone } from "lucide-react";
import { CHECKLIST_UPDATE, deliveryWords, useFileDelivery } from "../../app/checklist";
import { useCommandControl } from "../../app/commandControl";
import { describeError, useApp, useHost } from "../../app/context";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import type { FileDelivery as Delivery, FileDeliveryView } from "../../protocol/types";
import { NotAllowed } from "../../shell/NotAllowed";

const same = (a: Delivery, b: Delivery) => JSON.stringify(a) === JSON.stringify(b);

export function FileDelivery() {
  const { session, profile } = useApp();
  const admin = useHost((s) => s.grants?.admin !== false);
  const { view, error, reload, supported, online } = useFileDelivery();
  const saved = useRef(0);
  const control = useCommandControl<Delivery>({
    kind: "instant",
    value: view?.settings.delivery ?? { kind: "phone" },
    equal: same,
    describe: (e) => describeError(e, profile.name),
    send: async (next, note) => {
      const revision = Math.max(view?.settings.revision ?? 0, saved.current);
      try {
        await session.command({ command: "set_file_delivery", delivery: next }, { label: note.label, expectedRevision: revision });
        saved.current = revision + 1;
      } finally { reload(); }
    },
  });
  if (!supported) return <Notice tone="warn" title="Update the computer">{CHECKLIST_UPDATE}</Notice>;
  if (!admin) return <NotAllowed what="change where checklist files go" permission="manage" />;
  const shown = view && { ...view, settings: { ...view.settings, delivery: control.value } };
  const choose = (next: Delivery) => {
    const words = deliveryWords(view && { ...view, settings: { ...view.settings, delivery: next } });
    const where = words[0]!.toLowerCase() + words.slice(1);
    void control.set(next, { label: `Send checklist files ${where}`, what: "where files go", said: `Checklist files go ${where}.` });
  };
  return (
    <>
      <p className="settings-meta">
        When a step on a checklist needs a file, such as a key file or a form, getting it is one tap in the step. Choose where it goes.
      </p>
      {!online && <Notice tone="unknown">Offline. Connect to change this.</Notice>}
      {error && <Notice tone="unknown" action={<Button size="sm" onClick={reload}>Try again</Button>}>{error}</Notice>}
      {shown && <Choices view={shown} disabled={!online} choose={choose} />}
    </>
  );
}

function Choices({ view, disabled, choose }: { view: FileDeliveryView; disabled: boolean; choose: (d: Delivery) => void }) {
  const d = view.settings.delivery;
  const knownFolder = d.kind === "folder" && view.folders.some((f) => f.path === d.path);
  return (
    <>
      <Section title="On a device" id="settings-files">
        <div className="group" role="radiogroup" aria-label="On a device">
          <ListRow icon={Smartphone} title="This phone" subtitle="Saved where the phone keeps downloads" checked={d.kind === "phone"} disabled={disabled}
            onClick={() => choose({ kind: "phone" })} />
          {view.taildrop?.map((device) => (
            <ListRow key={device} icon={Send} title={device} subtitle="By Taildrop" checked={d.kind === "taildrop" && d.device === device} disabled={disabled}
              onClick={() => choose({ kind: "taildrop", device })} />
          ))}
        </div>
        {!view.taildrop?.length && (
          <p className="settings-meta">{view.taildrop ? "No other device on your tailnet takes Taildrop files now." : "Taildrop needs Tailscale on the computer, signed in."}</p>
        )}
      </Section>
      <Section title="In a folder on the computer" id="settings-files-folders">
        <div className="group" role="radiogroup" aria-label="In a folder on the computer">
          {view.folders.map((f) => (
            <ListRow key={f.path} icon={FolderDown} title={f.label} subtitle={f.path} checked={d.kind === "folder" && d.path === f.path} disabled={disabled}
              onClick={() => choose({ kind: "folder", path: f.path })} />
          ))}
        </div>
        <p className="settings-meta">A folder Google Drive, Dropbox, OneDrive or iCloud keeps in sync puts the file on your other devices too. Each project’s files go in a folder of its own.</p>
        <OtherFolder current={d.kind === "folder" && !knownFolder ? d.path : ""} disabled={disabled} choose={(path) => choose({ kind: "folder", path })} />
      </Section>
      <Section title="On a drive" id="settings-files-drives">
        {view.drives?.length ? (
          <DriveChoice view={view} disabled={disabled} choose={choose} />
        ) : (
          <p className="settings-meta">{view.drives
            ? "rclone is on the computer with no drive set up. Run rclone config there to add Google Drive, Dropbox, OneDrive, S3 or another."
            : "Install rclone on the computer and run rclone config to send files straight to Google Drive, Dropbox, OneDrive, S3 or another."}</p>
        )}
      </Section>
    </>
  );
}

function OtherFolder({ current, disabled, choose }: { current: string; disabled: boolean; choose: (path: string) => void }) {
  const [path, setPath] = useState(current);
  useEffect(() => setPath(current), [current]);
  const t = path.trim();
  const ok = t.startsWith("/") || t.startsWith("~/") || /^[A-Za-z]:\\/.test(t);
  return (
    <div className="settings-card settings-stack">
      <TextField label="Another folder" value={path} onChange={setPath} placeholder="~/Documents/Checklists"
        hint={current ? "Files go here now." : "The full path of a folder on the computer."} />
      <Button size="sm" disabled={disabled || !ok || t === current} onClick={() => choose(t)}>Use this folder</Button>
    </div>
  );
}

function DriveChoice({ view, disabled, choose }: { view: FileDeliveryView; disabled: boolean; choose: (d: Delivery) => void }) {
  const d = view.settings.delivery;
  const [folder, setFolder] = useState(d.kind === "drive" ? d.folder : "Ophio");
  useEffect(() => { if (d.kind === "drive") setFolder(d.folder); }, [d]);
  const clean = folder.trim().replace(/^\/+|\/+$/g, "");
  return (
    <>
      <div className="group" role="radiogroup" aria-label="On a drive">
        {view.drives!.map((remote) => (
          <ListRow key={remote} icon={HardDrive} title={remote} subtitle={d.kind === "drive" && d.remote === remote ? `In ${d.folder || "its top folder"}` : "By rclone"}
            checked={d.kind === "drive" && d.remote === remote} disabled={disabled}
            onClick={() => choose({ kind: "drive", remote, folder: clean })} />
        ))}
      </div>
      <div className="settings-card settings-stack">
        <TextField label="Folder on the drive" value={folder} onChange={setFolder} hint="Each project’s files go in a folder of its own inside it." />
        {d.kind === "drive" && clean !== d.folder && (
          <Button size="sm" disabled={disabled} onClick={() => choose({ ...d, folder: clean })}>Use this folder</Button>
        )}
      </div>
    </>
  );
}
