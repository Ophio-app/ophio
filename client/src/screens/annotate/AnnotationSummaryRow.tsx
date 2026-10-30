// SPDX-License-Identifier: Apache-2.0
import { Pencil } from "lucide-react";
import { ListRow } from "../../design/ListRow";
import { Age } from "../../design/Age";
import {
  describe,
  summarize,
  editedSinceSent,
  type AnnotationDraft,
} from "../../state/annotations";
export function AnnotationSummaryRow({
  draft,
  onOpen,
}: {
  draft: AnnotationDraft;
  onOpen(): void;
}) {
  const s = summarize(draft);
  return (
    <ListRow
      icon={Pencil}
      title={describe(s)}
      subtitle={
        <>
          {s.unresolved ? `${s.unresolved} unresolved · ` : ""}
          {draft.sentRevision && !editedSinceSent(draft)
            ? `Sent as revision ${draft.sentRevision}`
            : "Draft on this device"}{" "}
          · <Age at={draft.updatedAt} suffix=" ago" />
        </>
      }
      onClick={onOpen}
      chevron
    />
  );
}
