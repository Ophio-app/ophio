// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { describeError, useApp, useUi } from "../../app/context";
import { draftKey } from "../../app/ui";
import { Button, type ButtonProps } from "../../design/Button";
import { Sheet } from "../../design/Sheet";
import { Notice } from "../../design/Notice";
import type { Task } from "../../protocol/types";
import { annotationStore, useAnnotation } from "../../state/annotations";
import { openRetryDraft } from "../talk/openRetryDraft";
import type { ReceiptRequest } from "./receiptRequests";
import { holdsRequest, retryReceipt, RetryDraftConflict } from "./retryReceipt";

type Request = Pick<ReceiptRequest, "item">;
/** Restores a request as a draft for review; nothing is sent from here. */
export function RetryRequest({ task, load, ...button }: { task: Task; load: () => Promise<Request> | Request } & Pick<ButtonProps, "size" | "variant">) {
  const app = useApp();
  const [choice, setChoice] = useState<Request | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const current = useUi(s => s.drafts[draftKey(task.project_id, task.task_id)]);
  const retry = async (request?: Request) => {
    setBusy(true); setError(null);
    try {
      const original = request ?? await load();
      try { retryReceipt(app, task, original, !!request); setChoice(null); }
      catch (e) { if (e instanceof RetryDraftConflict) setChoice(original); else throw e; }
    } catch (e) { setError(describeError(e, app.profile.name)); }
    finally { setBusy(false); }
  };
  return <>
    <Button {...button} disabled={busy} onClick={() => void retry()}>Put back in composer</Button>
    {error && <Notice tone="warn">{error}</Notice>}
    <Sheet open={!!choice} onClose={() => setChoice(null)} title="Choose a draft" footer={<>
      <Button onClick={() => { setChoice(null); openRetryDraft(app.ui, task.project_id, task.task_id); }}>Keep current draft</Button>
      <Button variant="primary" disabled={busy} onClick={() => choice && void retry(choice)}>Use submitted request</Button>
    </>}>
      <p>This task already has a draft. Using the submitted request saves your current draft separately; you can switch back in Chat.</p>
      <h3>Current draft</h3><p>{current?.text || "Markup without a message"}</p>
      {current?.annotationId && <p>Includes your current markup.</p>}
      <h3>Submitted request</h3><p>{choice?.item.text || "Markup without a message"}</p>
      {choice?.item.bundle && <p>Includes the submitted picture and markup.</p>}
    </Sheet>
  </>;
}

/**
 * A request that never reached the agent goes back to the task's composer.
 * While the composer holds exactly those words and markup, say so rather
 * than offer them twice.
 */
export function PutBack({ task, request, noteClassName, ...button }: { task: Task; request: Request; noteClassName: string } & Pick<ButtonProps, "size" | "variant">) {
  const { profile } = useApp();
  const draft = useUi(s => s.drafts[draftKey(task.project_id, task.task_id)]);
  const markup = useAnnotation(annotationStore(profile.profileId), draft?.annotationId ?? null);
  if (holdsRequest(draft, markup, request)) return <span className={noteClassName}>Back in the composer</span>;
  return <RetryRequest {...button} task={task} load={() => request} />;
}

/** Drafts set aside by an explicit retry choice remain available across reloads. */
export function SavedRetryDrafts({ projectId, taskId }: { projectId: string; taskId: string | null }) {
  const { ui } = useApp();
  const key = draftKey(projectId, taskId);
  const saved = useUi(s => s.savedDrafts[key]);
  const current = useUi(s => s.drafts[key]);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!saved?.length) return null;
  return <div>
    <Button size="sm" variant="quiet" onClick={() => setOpen(true)}>Saved drafts · {saved.length}</Button>
    <Sheet open={open} onClose={() => setOpen(false)} title="Saved drafts">
      <p>Switching saves what’s in the composer separately. Nothing is sent.</p>
      {saved.map((draft, index) => <div key={index}>
        <p>{draft.text || "Markup without a message"}</p>{draft.annotationId && <p>Includes markup.</p>}
        <Button disabled={!!current?.attempt} onClick={() => {
          ui.set(s => ({ ...s, drafts: { ...s.drafts, [key]: draft }, savedDrafts: { ...s.savedDrafts,
            [key]: [...saved.filter((_, i) => i !== index), ...(current ? [current] : [])] } }));
          if (ui.persistNow()) setOpen(false); else setError("The drafts could not be saved. Keep the app open and try again.");
        }}>Use this draft</Button>
      </div>)}
      {current?.attempt && <p>Check the pending send before switching drafts.</p>}
      {error && <Notice tone="warn">{error}</Notice>}
    </Sheet>
  </div>;
}
