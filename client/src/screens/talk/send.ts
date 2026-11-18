// SPDX-License-Identifier: Apache-2.0
//
// Sending from Talk and Review. The draft is the person's work: it is
// cleared only when the computer accepts, and every other outcome is kept on
// the draft with the most precise fact the phone has (refused, never
// received, or unknown), so a restart or a lost connection never turns into
// a silent loss or a silent double send.

import { useEffect } from "react";
import { Store, useStore } from "../../state/store";
import type { App } from "../../app/context";
import { describeProtocolError, useApp, useLink } from "../../app/context";
import { talkProject, talkTask, taskOnScreen } from "../../app/taskOnScreen";
import { announce, withdraw } from "../../design/Announcer";
import { toast } from "../../design/Toast";
import { previewCaptureIn, previewDeliveryFocus, recordPreviewSend } from "../control/previewDelivery";
import { recordSend } from "../../app/outbox";
import { draftKey, type Route, type SendAttempt, type UiState, type UiStore } from "../../app/ui";
import { CommandError, NotConnectedError, OutcomeUnknownError, type HostSession } from "../../host/session";
import { TransferError } from "../../host/transfer";
import type { AnnotationBundle, Command, CommandReceipt } from "../../protocol/types";
import { observedReplyForTask, refreshFromSnapshot, type HostState } from "../../state/projection";
import { uuidv7 } from "../../protocol/ids";
import { taskBadge, taskHeadingFocus } from "./TaskBar";
import { annotationStore, editedSinceSent, holdLabels, markSent, releaseLabels, toBundle } from "../../state/annotations";
import { placeMoments } from "../annotate/WatchView";
import { NEW_CONVERSATION } from "./conversation";
import { unplacedMoments } from "../annotate/watch";
import { consumeFiles, MissingFile, uploadFiles, withFiles } from "./chatFiles";

type SendBusy = { label: string; detail: string | null };
type AutoSettlement = { users: number; stop(): void };

/** A computer owns sends; the independently navigable panes only observe them. */
const sendingByComputer = new WeakMap<UiStore, {
  busy: Store<Readonly<Record<string, SendBusy>>>;
  inFlight: Map<string, Promise<SendOutcome>>;
  queries: Map<string, Promise<void>>;
  automatic: Map<string, AutoSettlement>;
  origins: Map<string, App>;
  completed: Set<string>;
}>();

function sendState(ui: UiStore) {
  const shared = ui.sharedStore();
  let state = sendingByComputer.get(shared);
  if (!state) {
    state = {
      busy: new Store<Readonly<Record<string, SendBusy>>>({}), inFlight: new Map(),
      queries: new Map(), automatic: new Map(), origins: new Map(), completed: new Set(),
    };
    sendingByComputer.set(shared, state);
  }
  return state;
}

export function useSendBusy(projectId: string, draftTaskId: string | null): SendBusy | null {
  const { ui } = useApp();
  const key = draftKey(projectId, draftTaskId);
  return useStore(sendState(ui).busy, (state) => state[key] ?? null);
}

/** Acquire before calling work: even markup preparation can yield to another pane. */
function onceSending(app: App, projectId: string, draftTaskId: string | null, label: string,
  work: (progress: (detail: string | null) => void) => Promise<SendOutcome>,
): Promise<SendOutcome> {
  const state = sendState(app.ui);
  const key = draftKey(projectId, draftTaskId);
  const existing = state.inFlight.get(key);
  if (existing) return existing.then((out) => ({ ...out, joined: true }));
  let resolve!: (out: SendOutcome) => void;
  let reject!: (error: unknown) => void;
  const pending = new Promise<SendOutcome>((yes, no) => { resolve = yes; reject = no; });
  state.inFlight.set(key, pending);
  const progress = (detail: string | null) => state.busy.set((before) => ({ ...before, [key]: { label, detail } }));
  progress(null);
  const release = () => {
    state.inFlight.delete(key);
    state.busy.set((before) => {
      const { [key]: _finished, ...remaining } = before;
      return remaining;
    });
  };
  // Do not defer work to a microtask: an ordinary send still leaves in this turn.
  try {
    work(progress).then((out) => { release(); resolve(out); }, (error) => { release(); reject(error); });
  } catch (error) {
    release();
    reject(error);
  }
  return pending;
}

function rememberOrigin(app: App, commandId: string): void {
  const origins = sendState(app.ui).origins;
  if (!origins.has(commandId)) origins.set(commandId, app);
}

/** Claim confirmation before consuming text or notifying any mounted view. */
function completeOnce(app: App, commandId: string): boolean {
  const state = sendState(app.ui);
  if (state.completed.has(commandId)) return false;
  state.completed.add(commandId);
  return true;
}

/** Follow a created task while making a change of project explicit. */
export function selectCreatedTask(app: App, projectId: string, taskId: string): boolean {
  const switched = app.ui.get().projectId !== projectId;
  app.ui.selectCreated(projectId, taskId);
  if (switched) toast(`Switched to ${app.session.state.get().projects[projectId]?.name ?? "the task’s project"} for this task`);
  return switched;
}

/**
 * Say once that a request for a task was confirmed. Where the task is on
 * screen its conversation shows the result; anywhere else the notice offers
 * to open it, and the person stays where they are.
 */
export function confirmed(app: App, projectId: string, taskId: string | null, said: string): void {
  const { ui, session } = app;
  const local = ui.get();
  const front = local.stacks[local.tab].at(-1);
  const shown = !taskId
    || (front?.screen === "talk" && !local.taskList && local.projectId === projectId && local.taskByProject[projectId] === taskId)
    || taskOnScreen(session.state.get(), local)?.task_id === taskId;
  if (shown) announce(said);
  else toast(said, { action: { label: session.state.get().tasks[taskId]?.conversation ? "Open conversation" : "Open task", run: () => ui.openTask(projectId, taskId) } });
}

/**
 * The computer has a draft's request. Once the draft is used up, every
 * Review of it closes in the tab it was opened from, whichever tab is
 * showing; a draft the person has changed since keeps its Review open. A
 * task the draft created replaces the draft in Talk when the person was
 * looking at the draft or Talk still shows it; the current tab never changes.
 * Call it after the sent words are consumed from the draft.
 */
function delivered(app: App, projectId: string, draftTaskId: string | null, body: Command | null | undefined, receipt: CommandReceipt, ordered = false): void {
  const { ui, session, profile } = app;
  const ofDraft = (r: Route | undefined) => r?.screen === "review" && !r.queueItemId && r.projectId === projectId && r.taskId === draftTaskId;
  const before = ui.get();
  const spent = ui.draft(projectId, draftTaskId) === null;
  const watching = spent && ofDraft(before.stacks[before.tab].at(-1));
  const reviewing = ui.navigationStores().filter((side) => side !== ui && ofDraft(side.get().stacks[side.get().tab].at(-1)));
  if (spent) for (const side of ui.navigationStores()) side.closeReviews(ofDraft);
  const state = session.state.get();
  const task = draftTaskId ? state.tasks[draftTaskId] : undefined;
  const title = task?.title;
  // A task on its own is in the project made for it, not the draft's.
  const home = body?.command === "create_task" ? body.project_id : projectId;
  const where = state.projects[home]?.name;
  const said = receipt.status === "duplicate" ? `${profile.name} already had it`
    : body?.command === "create_task" ? `${body.conversation ? "Conversation started" : body.placement.mode === "queue" ? "Added to Next up" : "Task started"}${where ? ` in ${where}` : ""}`
    // Talk asks read-only only for a summary.
    : body?.command === "resume_task" && body.read_only ? (title ? `Asked “${title}” for a summary` : "Asked for a summary")
    : body?.command === "resume_task" && body.mode === "work" && task?.conversation ? "Moved to Tasks"
    : body?.command === "resume_task" && task?.conversation ? "Sent"
    : body?.command === "resume_task" ? (title ? `Continued “${title}”` : "Continued")
    : body?.command === "queue_continuation" ? (title ? `Put “${title}” in Next up` : "Put in Next up")
    : title ? `Sent to “${title}”` : "Sent";
  const created = receipt.result?.kind === "task_created" ? receipt.result.task_id : null;
  // Reviewing one ordered step approves only that step. Return to its card
  // after the computer confirms the task, so the rest remain actionable.
  const chat = ordered && body?.command === "create_task" && body.origin?.reviewed ? state.tasks[body.origin.chat] : undefined;
  // Another pane participates only if its visible Review was for this draft.
  // Merely showing a new-task root does not invite a background pane to jump.
  if (spent) for (const side of reviewing) {
    if (created && chat && side.get().tab === "talk") side.selectTask(chat.project_id, chat.task_id);
    else {
      const capture = side.get().tab === "control" ? previewCaptureIn(body, profile.hostId) : null;
      if (capture) previewDeliveryFocus(side).set({ captureId: capture, at: Date.now() });
      if (created && (draftTaskId === null || draftTaskId === NEW_CONVERSATION)) {
        if (side.get().tab === "talk") taskHeadingFocus(side).set(created);
        side.selectCreated(home, created);
      }
    }
  }
  if (created && chat && watching && before.tab === "talk") {
    ui.selectTask(chat.project_id, chat.task_id);
    confirmed(app, home, created, said);
    return;
  }
  // Markup from a preview, sent from Control: Control shows where it went.
  const picture = watching && ui.get().tab === "control" ? previewCaptureIn(body, profile.hostId) : null;
  if (picture) previewDeliveryFocus(ui).set({ captureId: picture, at: Date.now() });
  const fresh = draftTaskId === null || draftTaskId === NEW_CONVERSATION;
  if (created && fresh && (watching || showsNewTask(state, ui.get(), projectId, draftTaskId === NEW_CONVERSATION))) {
    if (ui.get().tab === "talk") {
      taskHeadingFocus(ui).set(created);
      if (!selectCreatedTask(app, home, created)) announce(said);
      return;
    }
    ui.selectCreated(home, created);
  }
  confirmed(app, home, created ?? (fresh ? null : draftTaskId), said);
}

/** Whether Talk, on its own root, shows the project's new-task draft, or its
 * new-conversation draft for a `conversation`. */
function showsNewTask(state: HostState, local: UiState, projectId: string, conversation = false): boolean {
  const selected = local.taskByProject[projectId];
  // A chosen task the computer no longer has leaves its draft showing, as
  // does a new conversation begun with nothing chosen.
  return local.stacks.talk.length === 1 && !local.taskList && local.conversing === conversation
    && talkProject(state, local.projectId)?.project_id === projectId
    && (selected !== undefined || conversation)
    && talkTask(state, projectId, selected) === null;
}

export interface SendRequest {
  projectId: string;
  /** The task the draft belongs to; null for the new-task draft. */
  draftTaskId: string | null;
  text: string;
  annotationId: string | null;
  label: string;
  expectedRevision?: number | null;
  /** A read-only question asked in the task's session, not the draft's words. */
  readOnly?: boolean;
  /** A conversation carried on as a task, in words written for that. */
  asTask?: boolean;
  build(bundle: AnnotationBundle | null): Command;
}

export type SendOutcome = ({ ok: true; receipt: CommandReceipt } | { ok: false; attempt: SendAttempt }) & {
  /** Another pane owns the UI follow-up. */
  joined?: boolean;
};

export function send(app: App, req: SendRequest, progress: (detail: string | null) => void): Promise<SendOutcome> {
  return onceSending(app, req.projectId, req.draftTaskId, req.label, (sharedProgress) =>
    prepareAndSend(app, req, (detail) => { sharedProgress(detail); progress(detail); }));
}

async function prepareAndSend(app: App, req: SendRequest, progress: (detail: string | null) => void): Promise<SendOutcome> {
  const { session, platform, profile, ui } = app;
  // Files, like markup and words, belong to the tap, not to later edits made
  // in either pane while preparation is waiting.
  const files = req.readOnly || req.asTask ? [] : (ui.draft(req.projectId, req.draftTaskId)?.files ?? []);
  const store = annotationStore(profile.profileId);
  let holder: string | null = null;
  let outgoingId: string | null = null;
  const fail = (a: Omit<SendAttempt, "at" | "text">): SendOutcome => {
    if (a.phase !== "unknown") {
      letGo(app, req.annotationId, holder);
      if (outgoingId) sendState(ui).origins.delete(outgoingId);
    }
    const attempt: SendAttempt = { ...a, at: Date.now(), text: req.text, annotationId: req.annotationId, ...(req.readOnly ? { readOnly: true } : {}), ...(req.asTask ? { asTask: true } : {}) };
    ui.saveDraft(req.projectId, req.draftTaskId, { attempt });
    return { ok: false, attempt };
  };

  let bundle: AnnotationBundle | null = null;
  if (req.annotationId) {
    // The markup goes as it is at the tap. Each comment made over a video
    // goes with its moment, or as general; edits made meanwhile wait for
    // the next message.
    const taken = store.take(req.annotationId);
    try {
      progress("Preparing the markup");
      if (taken && unplacedMoments(taken.draft).length) await placeMoments(store, req.annotationId);
      bundle = await toBundle({ platform, profileId: profile.profileId, session }, store, req.annotationId, (done, total) =>
        progress(done < total ? `Sending image ${done + 1} of ${total}` : "Images sent"),
      taken?.draft);
    } catch (e) {
      store.preparing.delete(req.annotationId);
      return fail(unprepared(e, profile.name));
    } finally {
      taken?.release();
    }
  }

  // A chat message's files go after its markup and before the message, so
  // the message is never on the computer without them. One that never
  // arrives keeps them, and what did upload isn't sent again.
  if (files.length) {
    try {
      bundle = withFiles(bundle, await uploadFiles(app, req.projectId, req.draftTaskId, files, progress));
    } catch (e) {
      if (req.annotationId) store.preparing.delete(req.annotationId);
      return fail(unprepared(e, profile.name, files.length === 1 ? `“${files[0]!.name}”` : "the files"));
    }
  }
  const sentFiles = files.length ? { files: files.map((f) => f.key) } : {};

  const built = req.build(bundle);
  // Keep the phone's last observed words explicitly separate from the person's
  // message and host history. Capture once, before saving the command, so a
  // retry repeats exactly the same observation rather than newer local state.
  const observed_reply = built.command === "resume_task" && !built.read_only && built.mode === undefined && !req.readOnly && !req.asTask
    ? observedReplyForTask(session.state.get(), built.task_id) : undefined;
  const body: Command = built.command === "resume_task" && observed_reply ? { ...built, observed_reply } : built;
  const delivery = body.command === "send_message" ? body.delivery : undefined;
  progress(null);
  // Record the request before it leaves the phone: if the app stops before
  // the answer, the next start asks about this id instead of sending anew.
  const commandId = uuidv7();
  outgoingId = commandId;
  const previousId = ui.draft(req.projectId, req.draftTaskId)?.attempt?.commandId;
  if (previousId) sendState(ui).origins.delete(previousId);
  rememberOrigin(app, commandId);
  const historyId = session.commandHistory;
  const expectedRevision = req.expectedRevision ?? null;
  const markup = req.annotationId ? (store.prepared.get(req.annotationId) ?? null) : null;
  const attempt: SendAttempt = {
    phase: "sending",
    historyId,
    at: Date.now(),
    commandId,
    delivery,
    detail: `Waiting for ${profile.name} to answer.`,
    text: req.text,
    annotationId: req.annotationId,
    ...(req.readOnly ? { readOnly: true } : {}),
    ...(req.asTask ? { asTask: true } : {}),
    body,
    expectedRevision,
    markup,
    ...sentFiles,
  };
  // The markup now holds its letters under this send until its outcome is
  // known, whatever later sends on this message do to its attempt.
  if (req.annotationId && markup) {
    holdLabels(store, req.annotationId, commandId, markup.labels);
    holder = commandId;
  }
  ui.saveDraft(req.projectId, req.draftTaskId, { attempt });
  if (req.annotationId) store.preparing.delete(req.annotationId);
  const saved = recorded(app, req.projectId, req.draftTaskId, attempt);
  if (!(typeof saved === "boolean" ? saved : await saved)) return fail({ ...unrecorded(profile.name), delivery });
  const sent = { text: req.text, annotationId: req.annotationId, markup, commandId, ...sentFiles };
  return deliver(app, req.projectId, req.draftTaskId, { body, commandId, expectedRevision, label: req.label, sent }, (a) => fail({ ...a, historyId, delivery }));
}

/**
 * Make a send durable before its command leaves: in the platform's outbox
 * when it has one, otherwise in the phone's saved state. Without an outbox
 * there is nothing to wait for, so the command leaves in the same turn.
 */
function recorded(app: App, projectId: string, draftTaskId: string | null, attempt: SendAttempt): boolean | Promise<boolean> {
  const saved = app.ui.persistNow();
  const outbox = app.platform.outbox;
  if (!outbox) return saved;
  return recordSend(outbox, app.profile.profileId, { projectId, draftTaskId, attempt }).then(
    () => true,
    () => false,
  );
}

/**
 * The markup's images go before the request, so a failure while preparing it
 * never sent the request. A lost link says only that; the computer's refusal
 * or failure follows the same sentence. A problem found on this device
 * before anything left is said as it is.
 */
function unprepared(e: unknown, computer: string, what = "the markup"): Omit<SendAttempt, "at" | "text"> {
  const stage = `Couldn’t upload ${what}. Nothing was sent to ${computer}.`;
  if (e instanceof MissingFile) return { phase: "failed", commandId: null, detail: `${e.message} Nothing was sent to ${computer}.` };
  const lost = e instanceof NotConnectedError || e instanceof OutcomeUnknownError || (e instanceof TransferError && e.resumable)
    || (e instanceof CommandError && e.error.code === "outcome_unknown");
  if (lost) return { phase: "not_delivered", commandId: null, detail: stage };
  const why = e instanceof CommandError ? describeProtocolError(e.error) : e instanceof TransferError ? e.message : null;
  return { phase: "failed", commandId: null, detail: why ? `${stage} ${why}` : (e as Error).message };
}

const unrecorded = (computer: string): Omit<SendAttempt, "at" | "text"> => ({
  phase: "failed",
  commandId: null,
  detail: `This device couldn’t record this send, so it wasn’t sent. Nothing reached ${computer}.`,
});

/** Frees the letters a send held: it did not arrive, or its confirmation
 * reserved them. */
function letGo(app: App, annotationId: string | null | undefined, commandId: string | null): void {
  if (annotationId && commandId) releaseLabels(annotationStore(app.profile.profileId), annotationId, commandId);
}

/** Whether an unknown attempt can be sent again under its original id. */
export const repeatable = (a: SendAttempt | null, session: HostSession): boolean => a?.phase === "unknown" && !!a.body && !!a.commandId && session.sameCommandHistory(a.historyId);

/**
 * Send an unknown attempt again under its original id. The computer
 * recognises the id: it answers with what it recorded rather than acting twice.
 */
export function resend(app: App, projectId: string, draftTaskId: string | null, attempt: SendAttempt, label: string): Promise<SendOutcome> {
  return onceSending(app, projectId, draftTaskId, label, () => repeatSend(app, projectId, draftTaskId, attempt, label));
}

async function repeatSend(app: App, projectId: string, draftTaskId: string | null, attempt: SendAttempt, label: string): Promise<SendOutcome> {
  const { body, commandId } = attempt;
  if (!body || !commandId || !repeatable(attempt, app.session)) throw new Error("This attempt can’t be repeated safely. Check its outcome first.");
  rememberOrigin(app, commandId);
  const expectedRevision = attempt.expectedRevision ?? null;
  const fail = (a: Omit<SendAttempt, "at" | "text">): SendOutcome => {
    const next: SendAttempt = { ...attempt, ...a, at: Date.now() };
    if (next.phase !== "unknown") {
      letGo(app, attempt.annotationId, commandId);
      sendState(app.ui).origins.delete(commandId);
    }
    app.ui.saveDraft(projectId, draftTaskId, { attempt: next });
    return { ok: false, attempt: next };
  };
  const sending: SendAttempt = { ...attempt, phase: "sending", at: Date.now() };
  app.ui.saveDraft(projectId, draftTaskId, { attempt: sending });
  const saved = recorded(app, projectId, draftTaskId, sending);
  if (!(typeof saved === "boolean" ? saved : await saved))
    return fail({
      phase: "unknown",
      commandId,
      detail: `This device couldn’t record this send, so it wasn’t sent again. The first one may still have reached ${app.profile.name}.`,
    });
  // The computer may have restarted while the outbox write was pending.
  if (!repeatable(attempt, app.session)) {
    settle(app, projectId, draftTaskId, attempt, null);
    throw new Error("This attempt can’t be repeated safely. Check its outcome first.");
  }
  const sent = { text: attempt.text, annotationId: attempt.annotationId ?? null, markup: attempt.markup ?? null, commandId, files: attempt.files };
  return deliver(app, projectId, draftTaskId, { body, commandId, expectedRevision, label, sent }, (a) =>
    // Its id stays bound to that request, so a refusal can't be retried under it.
    fail(a.phase === "unknown" ? { ...a, body, expectedRevision }
      // The repeat never left, but the first send may still have arrived.
      : a.phase === "not_delivered" ? { phase: "unknown", commandId, body, expectedRevision, detail: `${app.profile.name} isn’t connected, so it wasn’t sent again. The first one may still have reached it.` }
      : a),
  );
}

async function deliver(
  app: App,
  projectId: string,
  draftTaskId: string | null,
  out: { body: Command; commandId: string; expectedRevision: number | null; label: string; sent: Sent },
  fail: (a: Omit<SendAttempt, "at" | "text">) => SendOutcome,
): Promise<SendOutcome> {
  const { session, platform, profile } = app;
  const { body, commandId } = out;
  const accepted = (receipt: CommandReceipt): SendOutcome => {
    if (!completeOnce(app, commandId)) return { ok: true, receipt };
    const origin = sendState(app.ui).origins.get(commandId) ?? app;
    sendState(app.ui).origins.delete(commandId);
    recordPreviewSend(app, body, receipt);
    const ordered = !!app.ui.draft(projectId, draftTaskId)?.review?.ordered;
    consume(app, projectId, draftTaskId, out.sent);
    platform.haptic?.("confirm");
    delivered(origin, projectId, draftTaskId, body, receipt, ordered);
    return { ok: true, receipt };
  };
  // The answer was lost, but the computer's record of the send may already be here.
  const unknown = (detail: string) => {
    const receipt = journaled(session, commandId, body);
    return receipt ? accepted(receipt) : fail({ phase: "unknown", commandId, detail, body, expectedRevision: out.expectedRevision, markup: out.sent.markup });
  };
  try {
    return accepted(await session.command(body, { label: out.label, expectedRevision: out.expectedRevision, commandId }));
  } catch (e) {
    if (e instanceof CommandError) {
      if (e.error.code === "outcome_unknown") return unknown(`${e.error.message} Look at the conversation before sending again.`);
      if (e.error.code === "stale_revision" && body.command === "resume_task") {
        const before = session.state.get().tasks[body.task_id];
        let detail = "The task changed. Your message was not sent.";
        try {
          const snapshot = await session.query({ kind: "snapshot" });
          session.state.set((state) => refreshFromSnapshot(state, snapshot, Date.now()));
          const current = session.state.get();
          const after = current.tasks[body.task_id];
          if (after) {
            const changes = [
              before?.title !== after.title ? `Title: ${after.title}.` : "",
              before?.state !== after.state ? `Now ${taskBadge(after, after.current_run ? current.runs[after.current_run] : undefined, current.queues[after.project_id]).label}.` : "",
              before?.current_run !== after.current_run ? "The computer reported a different run." : "",
              before?.status_line !== after.status_line ? after.status_line : "",
            ].filter(Boolean).join(" ");
            detail += ` ${changes || "The computer reported a newer version of this task."} Review the task before choosing Continue again.`;
          } else detail += " The task is no longer on this computer.";
        } catch {
          detail += " The current task could not be loaded. Reconnect and review it before continuing.";
        }
        return fail({ phase: "rejected", commandId: null, detail });
      }
      return fail({ phase: "rejected", commandId: null, detail: describeProtocolError(e.error) });
    }
    if (e instanceof OutcomeUnknownError) return unknown(`The connection dropped before ${profile.name} answered. It may have arrived.`);
    if (e instanceof NotConnectedError)
      return fail({ phase: "not_delivered", commandId: null, detail: `${profile.name} isn’t connected, so nothing was sent.` });
    return fail({ phase: "failed", commandId: null, detail: (e as Error).message || "Something went wrong." });
  }
}

/**
 * The computer has what was sent: take exactly that out of the draft. Text
 * typed or markup changed after Send is newer work and stays.
 */
type Sent = { text: string; annotationId: string | null; markup: SendAttempt["markup"]; commandId: string | null; files?: string[] };

function consume(app: App, projectId: string, draftTaskId: string | null, sent: Sent): void {
  const { ui } = app;
  // The files sent are on the computer now; any added since stay.
  consumeFiles(ui, projectId, draftTaskId, sent.files);
  const draft = ui.draft(projectId, draftTaskId);
  if (!draft) return;
  const store = annotationStore(app.profile.profileId);
  const sentMarkup = sent.annotationId;
  if (sentMarkup && sent.markup) {
    // Exactly the markup that was submitted is now the sent revision.
    if (store.get()[sentMarkup]) markSent(store, sentMarkup, sent.markup.revision, sent.markup.content, sent.markup.labels);
  }
  letGo(app, sentMarkup, sent.commandId);
  const markup = store.get()[draft.annotationId ?? ""];
  const markupKept = draft.annotationId !== null && (draft.annotationId !== sentMarkup || (markup !== undefined && editedSinceSent(markup)));
  const text = remaining(draft.text, sent.text);
  if (!text.trim() && !markupKept && !draft.files?.length) ui.clearDraft(projectId, draftTaskId);
  // Agent and review choices belonged to the request that was sent.
  else ui.saveDraft(projectId, draftTaskId, { text, agent: null, access: undefined, attempt: null, review: null, ...(markupKept ? {} : { annotationId: null }) });
}

/** What is left of the draft once `sent` is removed from it. */
function remaining(now: string, sent: string): string {
  if (now.trim() === sent.trim()) return "";
  if (sent.trim() && now.startsWith(sent)) return now.slice(sent.length).trimStart();
  return now;
}

/**
 * Settle an unknown attempt from what the computer recorded. Accepted clears
 * what was sent (not later edits) and completes it like a direct send; never
 * received leaves the draft ready to send again.
 */
export function settle(app: App, projectId: string, draftTaskId: string | null, attempt: SendAttempt, receipt: CommandReceipt | null): void {
  const { ui } = app;
  const current = ui.draft(projectId, draftTaskId)?.attempt;
  // A late receipt must not overwrite a newer command. An accepted receipt
  // can still resolve a prior rejection, or record delivery after the pending
  // draft was removed; the submitted content remains its own snapshot.
  if (!attempt.commandId || (current?.commandId && current.commandId !== attempt.commandId)) return;
  if ((!receipt || receipt.status === "rejected") && current && current.phase !== "unknown" && current.phase !== "sending") return;
  if (receipt && receipt.command_id !== attempt.commandId) return;
  const state = sendState(ui);
  if (state.completed.has(attempt.commandId)) return;
  const final = receipt !== null || app.session.sameCommandHistory(attempt.historyId);
  // A query begun before a retry can say "not received" just before that
  // retry arrives. Only positive proof claims the one-time consumption.
  const accepted = receipt !== null && receipt.status !== "rejected";
  if (accepted && !completeOnce(app, attempt.commandId)) return;
  const origin = state.origins.get(attempt.commandId) ?? app;
  if (final && (accepted || !state.inFlight.has(draftKey(projectId, draftTaskId)))) state.origins.delete(attempt.commandId);
  // Whatever it is now, it is no longer in doubt as announced.
  withdraw(attempt.detail);
  if (!receipt && !app.session.sameCommandHistory(attempt.historyId)) {
    ui.saveDraft(projectId, draftTaskId, {
      attempt: { ...attempt, phase: "unknown", detail: `${app.profile.name} restarted or its records changed since this was sent, so it can’t confirm whether it acted. Check the result before sending again.` },
    });
    return;
  }
  // Known not to have arrived, or refused: its letters are free again.
  if (!receipt || receipt.status === "rejected") letGo(app, attempt.annotationId, attempt.commandId);
  if (!receipt) {
    ui.saveDraft(projectId, draftTaskId, {
      attempt: { ...attempt, phase: "not_delivered", commandId: null, body: null, at: Date.now(), detail: `${app.profile.name} never received it.` },
    });
    return;
  }
  if (receipt.status === "rejected") {
    ui.saveDraft(projectId, draftTaskId, {
      attempt: {
        ...attempt,
        phase: "rejected",
        commandId: null,
        body: null,
        at: Date.now(),
        detail: receipt.error ? describeProtocolError(receipt.error) : "Refused.",
      },
    });
    return;
  }
  recordPreviewSend(app, attempt.body, receipt, attempt.at);
  const ordered = !!ui.draft(projectId, draftTaskId)?.review?.ordered;
  consume(app, projectId, draftTaskId, { text: attempt.text, annotationId: attempt.annotationId ?? null, markup: attempt.markup ?? null, commandId: attempt.commandId, files: attempt.files });
  delivered(origin, projectId, draftTaskId, attempt.body, receipt, ordered);
}

/** Explicit Check can refresh a stalled automatic query; two manual checks still join. */
export function check(app: App, projectId: string, draftTaskId: string | null, attempt: SendAttempt): Promise<void> {
  return queryStatus(app, projectId, draftTaskId, attempt, false);
}

function queryStatus(app: App, projectId: string, draftTaskId: string | null, attempt: SendAttempt, automatic: boolean): Promise<void> {
  if (!attempt.commandId) return Promise.resolve();
  const state = sendState(app.ui);
  if (state.completed.has(attempt.commandId)) return Promise.resolve();
  const key = JSON.stringify([draftKey(projectId, draftTaskId), attempt.commandId, automatic]);
  const existing = state.queries.get(key);
  if (existing) return existing;
  const pending = app.session.query({ kind: "command_status", command_id: attempt.commandId }).then((r) => {
    settle(app, projectId, draftTaskId, attempt, r.receipt);
  });
  state.queries.set(key, pending);
  const remove = () => { if (state.queries.get(key) === pending) state.queries.delete(key); };
  void pending.then(remove, remove);
  return pending;
}

/**
 * The receipt the computer's journal already proves. It records a command's
 * admission under the command's id in the same step as what the command
 * changed, and a refusal records nothing, so a message on screen means its
 * send was accepted. A new task's id comes with the receipt, not always the
 * journal: without it the person could not be taken to the task, so that
 * send waits for its receipt.
 */
function journaled(session: HostSession, commandId: string, body: Command | null | undefined): CommandReceipt | null {
  const progress = session.state.get().commandStages[commandId];
  if (!progress || (body?.command === "create_task" && progress.result?.kind !== "task_created")) return null;
  return { command_id: commandId, status: "accepted", seq: null, result: progress.result ?? null, error: null };
}

/**
 * While a draft's last send is unknown, settle it as soon as the session
 * learns that command's fate: from the journal the moment the computer's
 * record of it arrives, even offline, or from its receipt (after a reconnect
 * the session asks the computer). An attempt restored after a restart was
 * never sent by this session, so it is asked about directly once the phone
 * is connected.
 */
export function useAutoSettle(projectId: string, draftTaskId: string | null, attempt: SendAttempt | null): void {
  const app = useApp();
  const online = useLink().state === "online";
  const id = attempt?.phase === "unknown" ? attempt.commandId : null;
  useEffect(() => {
    if (!id || !attempt) return;
    const state = sendState(app.ui);
    const key = JSON.stringify([draftKey(projectId, draftTaskId), id, online]);
    let entry = state.automatic.get(key);
    if (!entry) {
      entry = { users: 0, stop: () => {} };
      state.automatic.set(key, entry);
      const { session, ui } = app;
      const current = () => {
        const now = ui.draft(projectId, draftTaskId)?.attempt;
        return now?.commandId === id && now.phase === "unknown";
      };
      const recorded = () => {
        const receipt = current() ? journaled(session, id, attempt.body) : null;
        if (receipt) settle(app, projectId, draftTaskId, attempt, receipt);
        return receipt !== null;
      };
      if (!recorded()) {
        const offJournal = session.state.subscribe(() => void recorded());
        if (!session.commands.get()[id]) {
          if (online && current()) void queryStatus(app, projectId, draftTaskId, attempt, true).catch(() => {});
          entry.stop = offJournal;
        } else {
          const test = () => {
            const c = session.commands.get()[id];
            if (!c || !current()) return;
            if ((c.phase === "accepted" || c.phase === "duplicate" || c.phase === "rejected") && c.receipt) settle(app, projectId, draftTaskId, attempt, c.receipt);
            else if (c.phase === "not_delivered") settle(app, projectId, draftTaskId, attempt, null);
            else if (c.phase === "unknown" && online && !session.sameCommandHistory(attempt.historyId)) settle(app, projectId, draftTaskId, attempt, null);
          };
          test();
          const offCommands = session.commands.subscribe(test);
          entry.stop = () => { offJournal(); offCommands(); };
        }
      }
    }
    entry.users++;
    return () => {
      if (--entry.users === 0) {
        entry.stop();
        state.automatic.delete(key);
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [app.ui, id, online]);
}
