// SPDX-License-Identifier: Apache-2.0
//
// Sends whose answer the phone may not have heard. The WebView's own storage
// may not reach the disk before the app is killed, so where the platform has
// an outbox each send is also written there before its command leaves. A
// record lasts while its draft still waits on that command; on the next start
// any the drafts lost are restored as unknown, so the phone asks the computer
// what happened instead of the person sending again.

import type { SendOutbox } from "../host/link";
import type { SendAttempt, UiStore } from "./ui";

interface OutboxRecord {
  projectId: string;
  draftTaskId: string | null;
  attempt: SendAttempt;
}

/** Command ids with a record, per computer. */
const recorded = new Map<string, Set<string>>();
const ids = (profileId: string): Set<string> => {
  let set = recorded.get(profileId);
  if (!set) recorded.set(profileId, (set = new Set()));
  return set;
};

export async function recordSend(outbox: SendOutbox, profileId: string, record: OutboxRecord): Promise<void> {
  const id = record.attempt.commandId;
  if (!id) throw new Error("Only a send with an id can be recorded.");
  await outbox.put(profileId, id, JSON.stringify(record));
  ids(profileId).add(id);
}

function waiting(ui: UiStore): Set<string> {
  const now = new Set<string>();
  for (const d of Object.values(ui.get().drafts))
    if (d.attempt?.commandId && (d.attempt.phase === "sending" || d.attempt.phase === "unknown")) now.add(d.attempt.commandId);
  return now;
}

function parse(text: string): OutboxRecord | null {
  try {
    const r = JSON.parse(text) as Partial<OutboxRecord>;
    const a = r.attempt;
    if (typeof r.projectId !== "string" || (r.draftTaskId !== null && typeof r.draftTaskId !== "string")) return null;
    if (!a || typeof a.commandId !== "string" || typeof a.text !== "string") return null;
    return { projectId: r.projectId, draftTaskId: r.draftTaskId, attempt: a };
  } catch {
    return null;
  }
}

/** Put back a send the drafts lost, unless the draft has since moved on. */
function restore(ui: UiStore, computer: string, { projectId, draftTaskId, attempt }: OutboxRecord): void {
  const draft = ui.draft(projectId, draftTaskId);
  // Saved state that lost this send can still hold an earlier attempt; only
  // this send's own record or a later attempt means the draft moved on. A tie
  // goes to the send, which may have reached the computer.
  if (draft?.attempt && (draft.attempt.commandId === attempt.commandId || draft.attempt.at > attempt.at)) return;
  const unknown: SendAttempt = {
    ...attempt,
    phase: "unknown",
    detail: `The app closed before ${computer} answered. It may have arrived.`,
  };
  ui.saveDraft(projectId, draftTaskId, draft ? { attempt: unknown } : { text: attempt.text, annotationId: attempt.annotationId ?? null, attempt: unknown });
}

/**
 * Restore what the last run left in the outbox, then remove each record once
 * no draft waits on its command (answered, settled, or discarded).
 */
export function keepOutbox(outbox: SendOutbox, profileId: string, computer: string, ui: UiStore): () => void {
  let live = true;
  const prune = () => {
    const now = waiting(ui);
    for (const id of ids(profileId))
      if (!now.has(id)) {
        ids(profileId).delete(id);
        // A record that outlives its send is only asked about again.
        void outbox.remove(profileId, id).catch(() => undefined);
      }
  };
  outbox.list(profileId).then(
    (records) => {
      if (!live) return;
      for (const text of records) {
        const record = parse(text);
        if (!record) continue;
        restore(ui, computer, record);
        ids(profileId).add(record.attempt.commandId as string);
      }
      prune();
    },
    () => undefined,
  );
  const off = ui.subscribe(prune);
  return () => {
    live = false;
    off();
  };
}
