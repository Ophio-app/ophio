// SPDX-License-Identifier: Apache-2.0
import {
  File,
  Image as ImageIcon,
  Inbox as InboxIcon,
  Link,
  MessageSquare,
  Trash2,
} from "lucide-react";
import { useEffect, useState } from "react";
import {
  describeError,
  shallowEqual,
  useApp,
  useHost,
  useUi,
} from "../../app/context";
import { Age } from "../../design/Age";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import { Screen } from "../../shell/Screen";
import { annotationStore, blobs } from "../../state/annotations";
import { useStore } from "../../state/store";
import type { RouteProps } from "../types";
import { inboxStore, type InboxItem } from "./inbox";
import {
  chooseComposerFile,
  composerFileKey,
  transferComposer,
} from "../transfers/composer";
import { transferBytes } from "../transfers/storage";
import { sha256Hex } from "../../host/transfer";
import { uuidv7 } from "../../protocol/ids";
import { bytes } from "../library/model";
import { imageSize } from "./pictures";
import { ConnectionNote } from "./shared";
import { InboxShown } from "./InboxShown";
import "./work.css";
import { PathText } from "../../design/PathText";
import { useWarning } from "../../app/warnings";

const KIND: Record<InboxItem["kind"], string> = {
  text: "Text",
  link: "Link",
  image: "Image",
  file: "File",
};
type DraftVersion = { revision: number; fileId?: string };

/** A shared text or link becomes a Markdown note named after the item. */
function noteName(item: InboxItem): string {
  const base =
    item.name
      .replace(/[\\/:*?"<>|\u0000-\u001f]/g, "-")
      .trim()
      .replace(/[. ]+$/, "") || "Shared note";
  return /\.(md|txt)$/i.test(base) ? base : `${base}.md`;
}

export default function Inbox(_: RouteProps<"inbox">) {
  const { profile, ui } = useApp();
  const store = inboxStore(profile.profileId);
  const inbox = useStore(store, (s) => s);
  const projects = useHost((s) => Object.values(s.projects), shallowEqual);
  const projectId = useUi((s) => s.projectId);
  const project = useHost((s) => projectId !== null && projectId in s.projects);
  const [selected, setSelected] = useState<{
    item: InboxItem;
    mode: "annotate" | "message" | "discard";
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** An unsent Transfers draft that sending this item would replace. */
  const [replacing, setReplacing] = useState<{
    item: InboxItem;
    draft: string;
    file: boolean;
    version: DraftVersion;
  } | null>(null);
  const warn = useWarning("inbox.discard");
  const discard = async (item: InboxItem) => {
    setBusy(true);
    try {
      await store.discard(item.id);
      setSelected(null);
      announce("Discarded from the inbox.");
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setBusy(false);
    }
  };
  const useItem = async (
    item: InboxItem,
    projectId: string,
    mode: "annotate" | "message",
  ) => {
    setBusy(true);
    setError(null);
    try {
      let annotationId: string | null = null;
      if (item.kind === "image") {
        const blob = item.blobKey ? await blobs.get(item.blobKey) : null;
        if (!blob)
          throw new Error(
            "This image is missing from this device’s storage. Share it again from the original app.",
          );
        const dimensions = await imageSize(blob);
        // The canvas and send both refuse bytes without a verified fingerprint.
        const sha256 = await sha256Hex(blob);
        const copy = await blobs.put(blob);
        const annotations = annotationStore(profile.profileId);
        // Both paths add to the markup already on this project's draft;
        // a separate draft would replace it at review.
        const existing = ui.draft(projectId, null)?.annotationId;
        const prior = existing ? annotations.get()[existing] : undefined;
        const draft =
          prior?.projectId === projectId && prior.taskId === null
            ? prior
            : annotations.create(projectId, null);
        annotationId = draft.id;
        annotations.update(draft.id, (d) => ({
          ...d,
          nextSource: d.nextSource + 1,
          selectedSource: `S${d.nextSource}`,
          sources: [
            ...d.sources,
            {
              key: `S${d.nextSource}`,
              blobKey: copy,
              artifactId: null,
              sha256,
              byteSize: blob.size,
              mediaType: item.mediaType,
              width: dimensions.width,
              height: dimensions.height,
              durationMs: null,
              origin: { kind: "phone_import", original_name: item.name },
              capturedAt: item.receivedAt,
              derivedFrom: null,
              derivation: null,
            },
          ],
        }));
      }
      store.chooseProject(item.id, projectId);
      if (mode === "annotate" && annotationId) {
        // "Back and keep draft" keeps it where Talk and the next Annotate find it.
        ui.saveDraft(projectId, null, { annotationId });
        setSelected(null);
        ui.push({ screen: "annotate", annotationId });
        return;
      }
      const text =
        item.text ??
        (item.kind === "file"
          ? `Shared file: ${item.name}. The file is still in the capture inbox; send it through Transfers before asking the agent to use it.`
          : "");
      const existingText = ui.draft(projectId, null)?.text ?? "";
      ui.saveDraft(projectId, null, {
        text: [existingText, text].filter(Boolean).join("\n\n"),
        ...(annotationId ? { annotationId } : {}),
      });
      ui.openTask(projectId, null);
      setSelected(null);
      announce("Added to this project’s draft. Nothing sent.");
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setBusy(false);
    }
  };
  /** Put the item in the Transfers composer so the person only picks where it goes.
   *  `agreed` identifies the draft the person chose to replace. */
  const sendToComputer = async (item: InboxItem, agreed?: DraftVersion) => {
    setBusy(true);
    setError(null);
    try {
      const composer = transferComposer(profile.profileId);
      const blob = item.blobKey ? await blobs.get(item.blobKey) : null;
      if (item.blobKey && !blob)
        throw new Error(
          "This item is missing from this device’s storage. Share it again from the original app.",
        );
      const text = item.text ?? "",
        name = noteName(item);
      // A file read or write can finish after another Transfers view edits
      // the draft. Check again before publishing any replacement.
      while (true) {
        const c = composer.get(),
          revision = c.revision ?? 0,
          // Send already took a copy of what it sent.
          sent = (mode: "file" | "text") =>
            c.mode === mode && c.submitted?.revision === revision;
        const draft = blob
          ? c.fileName &&
            !sent("file") &&
            !(c.fileName === item.name &&
              c.fileSize === blob.size &&
              (await sameBytes(await transferBytes.get(composerFileKey(profile.profileId, c.fileId)), blob)))
            ? c.fileName
            : null
          : c.text && !sent("text") && !(c.text === text && c.name === name)
            ? // The name field can be cleared; the note is no less there.
              c.name.trim() || "the unnamed note"
            : null;
        if (composer.get() !== c) continue;
        // An inactive file can change without advancing the text revision.
        if (draft !== null && (agreed?.revision !== revision || (blob && agreed.fileId !== c.fileId))) {
          setReplacing({ item, draft, file: !!blob, version: { revision, fileId: c.fileId } });
          return;
        }
        if (blob) {
          const fileId = uuidv7(),
            fileKey = composerFileKey(profile.profileId, fileId);
          await transferBytes.put(fileKey, blob);
          if (composer.get() !== c) {
            await transferBytes.remove(fileKey);
            continue;
          }
          chooseComposerFile(profile.profileId, {
            mode: "file",
            fileId,
            fileName: item.name,
            fileSize: blob.size,
          });
        } else {
          composer.update({ mode: "text", text, name });
        }
        setReplacing(null);
        ui.push({ screen: "transfers" });
        return;
      }
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Screen title="Capture inbox" className="work-screen">
      <div className="work-stack">
        <ConnectionNote />
        {projectId && project && <InboxShown projectId={projectId} />}
        <Section title="Shared to this device" id="inbox-shared">
          <div className="work-stack">
            <p className="work-muted">
              Choose where each item belongs when you’re ready.
            </p>
            {inbox.error && (
              <Notice
                tone="stop"
                title="Inbox storage"
                action={
                  <Button
                    onClick={() => {
                      try {
                        store.retrySave();
                      } catch (e) {
                        setError(describeError(e, profile.name));
                      }
                    }}
                  >
                    Try saving again
                  </Button>
                }
              >
                {inbox.error}
              </Notice>
            )}
            {error && <Notice tone="stop">{error}</Notice>}
            {!inbox.items.length ? (
              <Empty icon={InboxIcon} title="A place for things to come back to">
                Share text, links, images or files from another app. They stay on
                this device until you choose what to do with them.
              </Empty>
            ) : (
              <ul role="list" className="work-inbox-list">
                {inbox.items.map((item) => {
                  const Icon =
                    item.kind === "image"
                      ? ImageIcon
                      : item.kind === "link"
                        ? Link
                        : item.kind === "file"
                          ? File
                          : MessageSquare;
                  return (
                    <li key={item.id} className="work-card work-stack">
                      <div className="work-inbox-heading">
                        <Icon aria-hidden />
                        <h2 className="work-card-title">{item.name}</h2>
                      </div>
                      <p className="work-caption">
                        {KIND[item.kind]}
                        {item.sourceApp && ` from ${item.sourceApp}`}
                        {item.kind === "file" && ` · ${bytes(item.size)}`} ·{" "}
                        <Age at={item.receivedAt} suffix=" ago" />
                      </p>
                      {item.kind === "image" && item.blobKey && (
                        <InboxPicture blobKey={item.blobKey} name={item.name} />
                      )}
                      {item.text && <p className="work-inbox-text">{item.text}</p>}
                      <p className="work-muted">
                        {projects.find((p) => p.project_id === item.projectId)
                          ?.name ??
                          (item.projectId
                            ? "Project no longer available"
                            : "No project yet")}
                      </p>
                      {!inbox.error && <StateLine phase="saved_on_phone" />}
                      <div className="work-inbox-actions">
                        {item.kind === "image" && (
                          <Button
                            onClick={() => setSelected({ item, mode: "annotate" })}
                          >
                            Annotate
                          </Button>
                        )}
                        <Button
                          onClick={() => setSelected({ item, mode: "message" })}
                        >
                          Use in a message
                        </Button>
                      </div>
                      <div className="work-inbox-more">
                        <Button
                          variant="quiet"
                          disabled={busy}
                          onClick={() => void sendToComputer(item)}
                        >
                          Send to computer…
                        </Button>
                        <Button
                          variant="quiet"
                          icon={Trash2}
                          disabled={busy}
                          onClick={() => warn.ask(() => setSelected({ item, mode: "discard" }), () => void discard(item))}
                        >
                          Discard{warn.more}
                        </Button>
                      </div>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        </Section>
        <Sheet
          className="work-sheet"
          open={selected !== null}
          onClose={() => setSelected(null)}
          title={
            selected?.mode === "discard"
              ? "Discard shared item"
              : "Choose project"
          }
          subtitle={selected?.item.name}
        >
          {selected?.mode === "discard" ? (
            <div className="work-stack">
              <p>
                Discard <strong>{selected.item.name}</strong> from this device’s
                inbox? Copies already added to a message or annotation stay in
                their drafts. The original in the sharing app is unchanged.
              </p>
              {warn.check}
              <Button
                variant="stop-outline"
                pending={busy}
                onClick={() => { warn.agreed(); void discard(selected.item); }}
              >
                Discard {selected.item.name}
              </Button>
            </div>
          ) : selected ? (
            <div className="work-stack">
              <p className="work-muted">
                This adds to a draft for a new task. Nothing is sent or started.
              </p>
              {selected.item.kind === "file" && (
                <Notice>
                  The message can refer to this file, but the file stays in the
                  inbox. Use Transfers to send its bytes.
                </Notice>
              )}
              {projects.map((p) => (
                <ListRow
                  key={p.project_id}
                  title={p.name}
                  subtitle={<span className="mono"><PathText path={p.host_path} /></span>}
                  onClick={
                    busy
                      ? undefined
                      : () =>
                          void useItem(
                            selected.item,
                            p.project_id,
                            selected.mode === "annotate"
                              ? "annotate"
                              : "message",
                          )
                  }
                />
              ))}
              {!projects.length && (
                <Notice>
                  Connect and add a project in Tasks first. This item will stay
                  in the inbox.
                </Notice>
              )}
              {busy && (
                <p role="status" className="work-muted">
                  Preparing the draft on this device…
                </p>
              )}
              {error && <Notice tone="stop">{error}</Notice>}
            </div>
          ) : null}
        </Sheet>
        <Sheet
          className="work-sheet"
          open={replacing !== null}
          onClose={() => setReplacing(null)}
          title={
            replacing?.file
              ? "Transfers has an unsent file"
              : "Transfers has an unsent note"
          }
        >
          {replacing && (
            <div className="work-stack">
              <p>
                Sending <strong>{replacing.item.name}</strong> replaces{" "}
                <strong>{replacing.draft}</strong>, which hasn’t been sent yet.
                {replacing.file
                  ? " You’d need to choose that file again."
                  : " Its text can’t be brought back."}
              </p>
              <Button
                variant="primary"
                disabled={busy}
                onClick={() => {
                  setReplacing(null);
                  ui.push({ screen: "transfers" });
                }}
              >
                Keep {replacing.draft} and open Transfers
              </Button>
              <Button
                variant="warn"
                pending={busy}
                onClick={() =>
                  void sendToComputer(replacing.item, replacing.version)
                }
              >
                Replace {replacing.draft}
              </Button>
              <Button disabled={busy} onClick={() => setReplacing(null)}>
                Cancel
              </Button>
              {error && <Notice tone="stop">{error}</Notice>}
            </div>
          )}
        </Sheet>
      </div>
    </Screen>
  );
}
/** Whether the composer already holds these bytes, so handing them over again loses nothing. */
async function sameBytes(held: Blob | null, blob: Blob): Promise<boolean> {
  return (
    !!held &&
    held.size === blob.size &&
    (await sha256Hex(held)) === (await sha256Hex(blob))
  );
}
function InboxPicture({ blobKey, name }: { blobKey: string; name: string }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    let owned: string | null = null;
    setUrl(null);
    void blobs
      .get(blobKey)
      .then((blob) => {
        if (alive && blob) {
          owned = URL.createObjectURL(blob);
          setUrl(owned);
        }
      })
      .catch(() => undefined);
    return () => {
      alive = false;
      if (owned) URL.revokeObjectURL(owned);
    };
  }, [blobKey]);
  return url ? (
    <img className="work-inbox-picture" src={url} alt={name} />
  ) : (
    <p className="work-caption">Image preview unavailable</p>
  );
}
