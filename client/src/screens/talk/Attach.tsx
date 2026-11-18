// SPDX-License-Identifier: Apache-2.0
import { useRef } from "react";
import { Files, Film, ImagePlus, Monitor, Pencil } from "lucide-react";
import type { SourceStart } from "../../app/ui";
import { ListRow } from "../../design/ListRow";
import { Sheet } from "../../design/Sheet";
import { MAX_SOURCES } from "../../state/annotations";
import { amount, MAX_FILE_BYTES, MAX_FILES } from "./chatFiles";

/** Everything a message can carry, in one place: a capture of the computer,
 * images or a clip from this device, or the markup itself. Each opens the
 * drawing at that step, so drawing, a note and the review still follow.
 * In a chat on a computer that takes them, files of any kind go with the
 * message as they are, with nothing to draw or review. */
export function AttachSheet({
  open,
  onClose,
  online,
  computer,
  markup,
  onStart,
  onFiles,
}: {
  open: boolean;
  onClose(): void;
  online: boolean;
  computer: string;
  /** What the markup already holds, when it holds anything. */
  markup: string | null;
  /** Opens the drawing; without a start, as it was left. Absent where a
   * message can't carry markup, such as a chat's first message. */
  onStart?(start?: SourceStart): void;
  /** Where files can go with the message: takes those chosen. */
  onFiles?(files: File[]): void;
}) {
  const images = useRef<HTMLInputElement>(null),
    clip = useRef<HTMLInputElement>(null),
    any = useRef<HTMLInputElement>(null);
  // The pickers sit outside the sheet, as in Add a source, and the sheet
  // stays open until something is chosen: cancelling a picker comes back here.
  const chosen = (files: File[]) => {
    if (files.length) onStart?.({ kind: "files", files });
  };
  return (
    <>
      <input
        className="visually-hidden"
        type="file"
        accept="image/*"
        multiple
        ref={images}
        aria-label="Images to attach"
        onChange={(e) => {
          chosen([...(e.target.files ?? [])]);
          e.target.value = "";
        }}
      />
      <input
        className="visually-hidden"
        type="file"
        accept="video/*"
        ref={clip}
        aria-label="Clip to attach"
        onChange={(e) => {
          chosen([...(e.target.files ?? [])].slice(0, 1));
          e.target.value = "";
        }}
      />
      {onFiles && <input
        className="visually-hidden"
        type="file"
        multiple
        ref={any}
        aria-label="Files to attach"
        onChange={(e) => {
          const files = [...(e.target.files ?? [])];
          e.target.value = "";
          if (files.length) onFiles(files);
        }}
      />}
      <Sheet open={open} onClose={onClose} title="Attach">
        <div className="group">
          {onStart && <><ListRow
            icon={Monitor}
            title="Screen"
            subtitle={online ? `Capture what ${computer} shows` : "Capture needs a connection"}
            disabled={!online}
            chevron
            onClick={() => onStart({ kind: "screen" })}
          />
          <ListRow
            icon={ImagePlus}
            title="Images"
            subtitle={`From this device, up to ${MAX_SOURCES} a message`}
            chevron
            onClick={() => images.current?.click()}
          />
          <ListRow
            icon={Film}
            title="Clip"
            subtitle="From this device, to trim and pick frames from"
            chevron
            onClick={() => clip.current?.click()}
          /></>}
          {onFiles && <ListRow
            icon={Files}
            title="Files"
            subtitle={`Any kind, up to ${MAX_FILES} a message and ${amount(MAX_FILE_BYTES)} each`}
            chevron
            onClick={() => any.current?.click()}
          />}
          {onStart && <ListRow
            icon={Pencil}
            title="Mark up"
            subtitle={markup ?? "Choose what to draw on, then mark what should change"}
            chevron
            onClick={() => onStart()}
          />}
        </div>
        <p className="fine">{onFiles
          ? `What you attach stays on this device until you send it. ${computer} gives the agent a read-only copy of each file.`
          : "What you attach stays on this device until you review and send it."}</p>
      </Sheet>
    </>
  );
}
