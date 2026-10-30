// SPDX-License-Identifier: Apache-2.0
import type {
  FileContent,
  LibraryRecovery,
  RecoveryVersion,
} from "../../protocol/types";
import type { HostSession } from "../../host/session";
import { draftStart, type LibraryStore } from "./drafts";
import { lf } from "./model";

export type RecoveryChoice = "base" | "proposed" | "displaced" | "current";
export const recoveryLabels: Record<RecoveryChoice, string> = {
  base: "What you started from",
  proposed: "Tried to save",
  displaced: "Displaced version",
  current: "On disk now",
};
export const versionFile = (v: RecoveryVersion): FileContent | null =>
  v.state === "text" ? v.file : null;

/** Stage the exact compared destination; never silently rebase or replace another phone draft. */
export async function stageRecovery(
  session: HostSession,
  store: LibraryStore,
  recovery: LibraryRecovery,
  choice: RecoveryChoice,
): Promise<void> {
  const selected = versionFile(recovery[choice]),
    base = versionFile(recovery.current);
  if (!selected || !base)
    throw new Error(
      "The destination and chosen version must both be readable text before saving here.",
    );
  if (selected.newline === "mixed" || base.newline === "mixed")
    throw new Error(
      "This version has mixed line endings. Review it on the computer to preserve its formatting.",
    );
  const existing = store.draft(base.root_id, base.path);
  if (
    existing &&
    (lf(existing.text) !== lf(selected.text) ||
      existing.phase === "sending" ||
      (existing.phase === "unknown" &&
        existing.recoveryId !== recovery.item.recovery_id))
  )
    throw new Error(
      "This device already has a draft for this file. Review or save that draft first. The recovery copy is kept.",
    );
  const { file } = await session.query({
    kind: "library_read",
    root_id: base.root_id,
    path: base.path,
  });
  if (file.revision !== base.revision)
    throw new Error(
      "The destination changed after you compared it. Nothing was written. Your choice is kept; refresh and compare again.",
    );
  store.saveDraft({
    base,
    text: lf(selected.text),
    updatedAt: Date.now(),
    startedAt: draftStart(),
    phase: "draft",
    reviewRecovery: recovery.item.recovery_id,
  });
}
