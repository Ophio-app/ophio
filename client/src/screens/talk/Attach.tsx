// SPDX-License-Identifier: Apache-2.0
import { useRef } from "react";
import { Film, ImagePlus, Monitor, Pencil } from "lucide-react";
import type { SourceStart } from "../../app/ui";
import { ListRow } from "../../design/ListRow";
import { Sheet } from "../../design/Sheet";
import { MAX_SOURCES } from "../../state/annotations";

/** Everything a message can carry, in one place: a capture of the computer,
 * images or a clip from this device, or the markup itself. Each opens the
 * drawing at that step, so drawing, a note and the review still follow. */
export function AttachSheet({
  open,
  onClose,
  online,
  computer,
  markup,
  onStart,
}: {
  open: boolean;
  onClose(): void;
  online: boolean;
  computer: string;
  /** What the markup already holds, when it holds anything. */
  markup: string | null;
  /** Opens the drawing; without a start, as it was left. */
  onStart(start?: SourceStart): void;
}) {
  const images = useRef<HTMLInputElement>(null),
    clip = useRef<HTMLInputElement>(null);
  // The pickers sit outside the sheet, as in Add a source, and the sheet
  // stays open until something is chosen: cancelling a picker comes back here.
  const chosen = (files: File[]) => {
    if (files.length) onStart({ kind: "files", files });
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
      <Sheet open={open} onClose={onClose} title="Attach">
        <div className="group">
          <ListRow
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
          />
          <ListRow
            icon={Pencil}
            title="Mark up"
            subtitle={markup ?? "Choose what to draw on, then mark what should change"}
            chevron
            onClick={() => onStart()}
          />
        </div>
        <p className="fine">What you attach stays on this device until you review and send it.</p>
      </Sheet>
    </>
  );
}
