// SPDX-License-Identifier: Apache-2.0
import { RetryRequest } from "../work/RetryRequest";
import { forwardRef, useEffect, useRef, useState } from "react";
import { Camera, ChevronDown, ExternalLink, Link, MessageCircleQuestionMark, Plus, Trash2, X } from "lucide-react";
import {
  describeError,
  shallowEqual,
  useApp,
  useHost,
  useLink,
  useUi,
} from "../../app/context";
import { useStore } from "../../state/store";
import { PREVIEW_FOCUS_FRESH_MS, previewDeliveries, previewDeliveryFocus, previewDeliveryText, resolvePreviewDelivery, watchPreviewDeliveries } from "./previewDelivery";
import { useRoute } from "../../shell/Screen";
import { OtherProject, OtherProjects, useByProject, useProjectScope } from "./projectScope";
import { clockTime } from "../../design/time";
import { taskBadge } from "../talk/TaskBar";
import { latestRun } from "../work/logic";
import { readLocal, writeLocal } from "../work/local";
import { Age } from "../../design/Age";
import { Button, IconButton } from "../../design/Button";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import { announce } from "../../design/Announcer";
import { commandResult } from "../../host/commandResult";
import { upload } from "../../host/transfer";
import type {
  PreviewHandle,
  PreviewPicture,
  PreviewState,
} from "../../host/preview";
import type { ApprovalQuestion, PreviewEndpoint, QueueView, Run, Task } from "../../protocol/types";
import { ApprovalQuestions, choices, type Picked } from "../../design/ApprovalQuestions";
import { anyAnswered } from "../talk/previewAnswers";
import {
  previewCloseText,
  previewDrafts,
  type PreviewDraft,
} from "./previewDrafts";
import { PendingMessageError, previewAnnotation } from "./previewAnnotation";
import "./previews.css";
import { AddPreviewSheet, useRemovePreview } from "./AddPreview";
import { useWarning } from "../../app/warnings";

const runningPictureTask = (task: Task) => ["dispatching", "working", "awaiting_decision"].includes(task.state);
export function pictureTasks(tasks: Task[]): Task[] {
  return [...tasks].sort((a, b) => Number(runningPictureTask(b)) - Number(runningPictureTask(a)) || b.updated_at - a.updated_at || a.task_id.localeCompare(b.task_id));
}
export function pictureTaskLabel(task: Task, runs: Record<string, Run> = {}, queues: Record<string, QueueView> = {}): string {
  return `${task.title} · ${taskBadge(task, latestRun(task, runs), queues[task.project_id]).label} · ${clockTime(task.updated_at)}`;
}
export function pictureTarget(tasks: Task[], bound: string | null, runs: Record<string, Run> = {}): { taskId: string | null; notice: string | null } {
  const previous = tasks.find(t => t.task_id === bound);
  const running = pictureTasks(tasks).find(runningPictureTask);
  if (previous && ["interrupted", "completed", "failed", "archived"].includes(previous.state) && running) {
    return { taskId: running.task_id, notice: `Selected “${pictureTaskLabel(running, runs)}” because “${pictureTaskLabel(previous, runs)}” has ended.` };
  }
  return { taskId: previous?.task_id ?? null, notice: null };
}

/** The run in `tasks` that most recently ended having changed files. */
export function latestFileChange(tasks: Task[], runs: Record<string, Run>): { task: Task; run: Run } | null {
  let found: { task: Task; run: Run } | null = null;
  for (const run of Object.values(runs)) {
    const task = tasks.find((t) => t.task_id === run.task_id);
    if (!task || run.ended_at === null || !run.outcome?.changed_files.length) continue;
    if (!found || run.ended_at > (found.run.ended_at ?? 0)) found = { task, run };
  }
  return found;
}

/** The card's last error, described. A refusal for a task's pending message
 * lapses once that message is no longer pending. */
function useCardError(): [string | null, (e: unknown) => void] {
  const { profile } = useApp();
  const [error, setError] = useState<unknown>(null);
  const refused = error instanceof PendingMessageError ? error.draftKey : null;
  const pending = useUi((s) => refused !== null && !!s.drafts[refused]?.attempt);
  useEffect(() => {
    if (refused !== null && !pending) setError(null);
  }, [refused, pending]);
  return [error === null ? null : describeError(error, profile.name), setError];
}

export function previewAddress(raw: string): {
  url: string;
  loopback: boolean;
} {
  const url = new URL(raw);
  if (
    !["http:", "https:"].includes(url.protocol) ||
    url.username ||
    url.password
  )
    throw new Error("This preview does not use an allowed web address.");
  const host = url.hostname.toLowerCase();
  return {
    url: url.href,
    loopback:
      host === "localhost" ||
      host === "[::1]" ||
      /^127\./.test(host) ||
      /^\[::ffff:7f[0-9a-f]{2}:/.test(host),
  };
}
/** The address a page on the computer is opened at: `localhost` at
 * 127.0.0.1, as the computer opens it, and an IPv4 address written as IPv6
 * at that IPv4 address. */
function loopbackAddress(u: URL): string {
  const host = u.hostname.toLowerCase();
  if (host === "localhost") return "127.0.0.1";
  const [, high = "", low = ""] = /^\[::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})\]$/.exec(host) ?? [];
  if (!high) return host;
  const [h, l] = [parseInt(high, 16), parseInt(low, 16)];
  return [h >> 8, h & 255, l >> 8, l & 255].join(".");
}
/** Whether two addresses name the same page. On the computer `localhost` is
 * 127.0.0.1, but `[::1]` and each other 127.* address may be another server
 * on the same port. */
export function samePage(a: string, b: string): boolean {
  try {
    const [x, y] = [new URL(a), new URL(b)];
    if (previewAddress(a).loopback && previewAddress(b).loopback) {
      const port = (u: URL) => u.port || (u.protocol === "https:" ? "443" : "80");
      return (
        x.protocol === y.protocol &&
        loopbackAddress(x) === loopbackAddress(y) &&
        port(x) === port(y) &&
        x.pathname === y.pathname &&
        x.search === y.search
      );
    }
  } catch {
    /* Compared as written. */
  }
  return a === b;
}

/** How the computer last found the preview's server. */
function reachability(p: PreviewEndpoint, project: string): string {
  // A found preview's state is its server's own, not a check's.
  if (p.automatic) return `${p.reachable === false ? "Stopped running" : "Running"} in ${project}`;
  return p.reachable === true
    ? "Reachable from the computer when checked"
    : p.reachable === false
      ? "Could not be reached from the computer"
      : "Reachability unknown";
}

/** How long a preview's server has to start and answer: the computer waits
 * up to a minute for it, then says what it saw. */
const STARTING_WAIT_MS = 75_000;

/** `compact` makes the card one row with Open at its end; the screen it sits
 * on says once what Open does and when the computer is out of reach. */
/** What the agent that showed a preview asks about it, answered beside the page. */
export interface PreviewQuestions {
  questions: ApprovalQuestion[];
  picked: Picked[];
  onPicked(picked: Picked[]): void;
  /** The person finished answering and is back in Ophio to send them on. */
  onSubmit(picked: Picked[]): void;
}

/** `remove`, where previews are managed, offers taking it off the list. */
export function PreviewCard({ preview: p, compact = false, ask, remove }: { preview: PreviewEndpoint; compact?: boolean; ask?: PreviewQuestions; remove?: () => void }) {
  const { platform, profile, session, ui } = useApp();
  const link = useLink();
  const taskId = useUi((s) => s.taskByProject[p.project_id] ?? null);
  const grants = useHost((s) => s.grants);
  const tasks = useHost(
    (s) => Object.values(s.tasks).filter((t) => t.project_id === p.project_id),
    shallowEqual,
  );
  const runs = useHost((s) => s.runs);
  const project = useHost((s) => s.projects[p.project_id]?.name);
  const [opening, setOpening] = useState(false);
  /** Opening waits for the computer to start the preview's server. */
  const [starting, setStarting] = useState(false);
  const [state, setState] = useState<PreviewState | null>(null);
  const [error, setError] = useCardError();
  /** The computer's latest check found nothing answering at the address. */
  const [unanswered, setUnanswered] = useState(false);
  const [handle, setHandle] = useState<PreviewHandle | null>(null);
  const activeHandle = useRef<PreviewHandle | null>(null);
  const [drafts, setDrafts] = useState<PreviewDraft[]>([]);
  const [captured, setCaptured] = useState<PreviewDraft | null>(null);
  const [pictureCount, setPictureCount] = useState(0);
  const [capturing, setCapturing] = useState(false);
  const alive = useRef(true);
  const binding = useRef<string | null>(null);
  // The preview stays open across renders; it answers to the latest questions.
  const asking = useRef(ask);
  asking.current = ask;
  // Minimized, the questions wait in a bubble over the page until tapped.
  const [folded, setFolded] = useState(false);
  const fold = useRef<HTMLButtonElement>(null);
  const unfold = useRef<HTMLButtonElement>(null);
  const folding = useRef(false);
  useEffect(() => {
    if (!handle) setFolded(false);
  }, [handle]);
  useEffect(() => {
    if (!folding.current) return;
    folding.current = false;
    (folded ? unfold : fold).current?.focus();
  }, [folded]);
  const foldTo = (away: boolean) => {
    folding.current = true;
    setFolded(away);
  };
  let address: ReturnType<typeof previewAddress> | null = null;
  try {
    address = previewAddress(p.url);
  } catch {
    /* Explicit Open reports invalid addresses. */
  }
  const online = link.state === "online";
  useEffect(() => {
    alive.current = true;
    if (!address?.loopback)
      return () => {
        alive.current = false;
      };
    void previewDrafts
      .list(profile.profileId, p.preview_id)
      .then(async (d) => {
        const recovered =
          (await platform.pendingPreviews?.(profile.profileId, p.preview_id)) ??
          [];
        for (const picture of recovered) {
          if (!d.some((saved) => saved.id === picture.id)) {
            const restored: PreviewDraft = {
              id: picture.id ?? previewDrafts.id(),
              profileId: profile.profileId,
              previewId: p.preview_id,
              previewLabel: p.label,
              computerId: profile.hostId,
              computerName: profile.name,
              projectId: p.project_id,
              taskId: picture.taskId ?? null,
              blob: picture.blob,
              at: picture.capturedAt ?? Date.now(),
              captureTimeKnown: picture.capturedAt !== undefined,
            };
            await previewDrafts.put(restored);
            d.push(restored);
          }
          await picture.acknowledge();
        }
        if (alive.current) setDrafts(d);
      })
      .catch((e) => {
        if (alive.current) setError(e);
      });
    return () => {
      alive.current = false;
      void activeHandle.current?.close();
    };
  }, [profile.profileId, profile.name, p.preview_id]);
  useEffect(() => {
    if (!online) void activeHandle.current?.close();
  }, [online]);
  const save = async (picture: PreviewPicture) => {
    const draft: PreviewDraft = {
      id: picture.id ?? previewDrafts.id(),
      profileId: profile.profileId,
      previewId: p.preview_id,
      previewLabel: p.label,
      computerId: profile.hostId,
      computerName: profile.name,
      projectId: p.project_id,
      taskId: picture.taskId === undefined ? binding.current : picture.taskId,
      blob: picture.blob,
      at: picture.capturedAt ?? Date.now(),
    };
    await previewDrafts.put(draft);
    if (alive.current) setDrafts((all) => [...all, draft]);
    await picture.acknowledge();
    if (alive.current) { setCaptured(draft); setPictureCount(n => n + 1); }
    announce("Preview picture saved on this device");
    return draft;
  };
  const close = async () => {
    const current = activeHandle.current;
    activeHandle.current = null;
    setHandle(null);
    if (current) await current.close();
  };
  const annotate = async (draft: PreviewDraft) => {
    // The tasks as they are now: a native preview can stay open while its
    // task ends and another starts.
    const now = session.state.get();
    const current = Object.values(now.tasks).filter((t) => t.project_id === p.project_id);
    const chosen = { ...draft, taskId: pictureTarget(current, draft.taskId, now.runs).taskId };
    await previewDrafts.put(chosen);
    setDrafts(all => all.map(d => d.id === chosen.id ? chosen : d));
    const id = await previewAnnotation(chosen, ui);
    await close();
    ui.push({ screen: "annotate", annotationId: id });
  };
  /** `start` has the computer start the preview's server with the command
   * the agent that showed it gave, when nothing answers. */
  const open = async (start = false) => {
    setOpening(true);
    setStarting(start);
    setError(null);
    setUnanswered(false);
    setState(null);
    setCaptured(null);
    setPictureCount(0);
    binding.current = tasks.some((t) => t.task_id === taskId) ? taskId : null;
    try {
      const parsed = previewAddress(p.url);
      if (!parsed.loopback) {
        await platform.openExternal(parsed.url);
        return;
      }
      if (!platform.preview)
        throw new Error("Private previews are unavailable on this device.");
      await commandResult(
        session,
        await session.command(
          start
            ? { command: "check_preview", preview_id: p.preview_id, start: true }
            : { command: "check_preview", preview_id: p.preview_id },
          { label: start ? `Start the server for ${p.label}` : "Check preview before opening" },
        ),
        start ? { waitMs: STARTING_WAIT_MS } : undefined,
      );
      const url = new URL(parsed.url);
      let finished: Picked[] | null = null;
      const questions = asking.current;
      const current = await platform.preview(
        profile.profileId,
        p.preview_id,
        url.pathname + url.search,
        (next) => {
          if (!alive.current) return;
          setState(next);
          if (next.state === "closed") {
            setHandle(null);
            announce(previewCloseText(next.reason));
          }
        },
        binding.current,
        questions?.questions.length ? {
          questions: questions.questions,
          answers: questions.picked,
          answered: (answers, submitted) => {
            asking.current?.onPicked(answers);
            if (submitted) finished = answers;
          },
        } : undefined,
      );
      activeHandle.current = current;
      if (!alive.current) {
        await current.close();
        return;
      }
      setHandle(current);
      setOpening(false);
      if (!current.document) {
        const result = await current.show();
        const pictures = Array.isArray(result) ? result : result ? [result] : [];
        let chosen: PreviewDraft | undefined;
        for (const picture of pictures) {
          const draft = await save(picture);
          if (picture.annotate) chosen = draft;
        }
        await close();
        if (chosen) await annotate(chosen);
        else if (finished) asking.current?.onSubmit(finished);
      }
    } catch (e) {
      // The computer opens only a preview its latest check reached.
      if (e instanceof Error && "code" in e && e.code === "conflict") setUnanswered(true);
      else setError(e);
      await close().catch(() => undefined);
    } finally {
      if (alive.current) {
        setOpening(false);
        setStarting(false);
      }
    }
  };
  const openable =
    online && !(address?.loopback && (!grants?.tasks || !platform.preview)) && !handle;
  // Every card has an Open button, so its name says which preview.
  const openButton = (
    <Button
      icon={ExternalLink}
      size={compact ? "sm" : undefined}
      pending={opening}
      disabled={!openable}
      onClick={() => void open()}
    >
      {opening ? (
        starting ? "Starting server…" : compact ? "Opening…" : "Opening preview…"
      ) : (
        <>
          Open{" "}
          <span className="visually-hidden">{p.label}</span>
          {state?.state === "closed" && " again"}
        </>
      )}
    </Button>
  );
  const closeButton = handle && (
    <Button icon={X} size={compact ? "sm" : undefined} onClick={() => void close()}>
      {compact ? "Close" : "Close preview"}
    </Button>
  );
  const removeButton = remove && !handle && <IconButton icon={Trash2} label={`Remove ${p.label}`} onClick={remove} />;
  return (
    <article className={compact ? "preview-card preview-card--compact" : "control-target preview-card"}>
      <ListRow
        trailing={compact ? <div className="control-keys">{closeButton ?? openButton}{removeButton}</div> : undefined}
        title={p.label}
        subtitle={
          <>
            {reachability(p, project ?? "its project")}
            {p.checked_at !== null && (
              <>
                {" "}
                · <Age at={p.checked_at} suffix=" ago" />
              </>
            )}
          </>
        }
      />
      <div className="preview-content">
        {address?.loopback &&
          (platform.preview ? (!compact && (
            <p className="fine">
              Opens a private connection to this web preview on {profile.name}.
            </p>
          )) : (
            <Notice tone="warn">
              Private previews are unavailable on this device.
            </Notice>
          ))}
        {!online && !compact && (
          <Notice tone="warn">
            {link.state === "failed" && link.reason === "revoked"
              ? "This device’s access was revoked."
              : "Reconnect to the computer to open this preview."}
          </Notice>
        )}
        {error && <Notice tone="warn">{error}</Notice>}
        {unanswered && (p.start && grants?.terminal ? (
          <Notice
            tone="warn"
            title={`${profile.name} couldn’t reach ${p.label}`}
            action={
              <Button pending={opening} disabled={!openable} onClick={() => void open(true)}>
                Start server
              </Button>
            }
          >
            Nothing answered at <span className="mono">{p.url}</span>. A
            server an agent starts stops when the agent finishes. Start this
            one with <span className="mono">{p.start}</span> in the project’s
            folder? It runs in a terminal you can close in Control, and the
            preview opens once it answers.
          </Notice>
        ) : (
          <Notice
            tone="warn"
            title={`${profile.name} couldn’t reach ${p.label}`}
            action={
              <Button pending={opening} disabled={!openable} onClick={() => void open()}>
                Check again
              </Button>
            }
          >
            Nothing answered at <span className="mono">{p.url}</span> when it
            last checked.{" "}
            {p.start ? (
              <>
                Start its server with <span className="mono">{p.start}</span>{" "}
                in the project’s folder on {profile.name}, then check again to
                open it. This device can’t open terminals there.
              </>
            ) : (
              <>
                Start the preview’s server on {profile.name}, then check again
                to open it.
              </>
            )}
          </Notice>
        ))}
        {state?.state === "closed" && (
          <p role="status" className="fine">
            {previewCloseText(state.reason)}
          </p>
        )}
        {state?.state === "open" && (
          <p role="status" className="fine">
            Preview open · Connected to{" "}
            <span className="mono">{state.connected_address}</span> on the
            computer
          </p>
        )}
        {!compact && <div className="preview-actions">
          {openButton}
          {closeButton}
          {removeButton}
        </div>}
        {drafts.map((draft) => (
          <PictureDraft
            key={draft.id}
            draft={draft}
            tasks={tasks}
            onChange={(next) =>
              setDrafts((all) => all.map((d) => (d.id === next.id ? next : d)))
            }
            onRemove={() =>
              setDrafts((all) => all.filter((d) => d.id !== draft.id))
            }
          />
        ))}
      </div>
      <Sheet
        open={!!handle?.document}
        onClose={() => void close()}
        title={p.label}
        subtitle="Private preview"
        size="full"
        footer={
          <div className="preview-capture-actions">
            {captured && <p className="fine" role="status">{pictureCount === 1 ? "Picture saved." : `${pictureCount} pictures saved.`} Take another or close the preview.</p>}
            {error && <Notice tone="warn">{error}</Notice>}
            {captured && pictureTarget(tasks, captured.taskId, runs).notice && <p role="status" className="fine">{pictureTarget(tasks, captured.taskId, runs).notice} Any earlier markup is kept with its original task.</p>}
            {ask && ask.questions.length > 0 && (folded ? (
              <QuestionsBubble ref={unfold} questions={ask.questions} picked={ask.picked} onClick={() => foldTo(false)} />
            ) : (
              <section className="preview-questions" aria-label="The agent’s questions">
                <div className="preview-questions__head">
                  <IconButton ref={fold} icon={ChevronDown} label="Minimize questions" onClick={() => foldTo(true)} />
                  <p>{ask.questions.length === 1 ? "The agent asks" : `The agent asks ${ask.questions.length} questions`}</p>
                </div>
                <ApprovalQuestions questions={ask.questions} picked={ask.picked} onChange={ask.onPicked} disabled={false} titled={false} />
              </section>
            ))}
            <div className="preview-actions">
              <Button icon={Camera} pending={capturing} disabled={capturing} onClick={async () => {
                const current = handle;
                if (!current?.picture) return;
                setCapturing(true); setError(null);
                try { await save(await current.picture()); }
                catch (e) { setError(e); }
                finally { setCapturing(false); }
              }}>Take picture</Button>
              {captured && <Button variant="primary" disabled={capturing} onClick={async () => {
                setCapturing(true); setError(null);
                try { await annotate(captured); }
                catch (e) { setError(e); }
                finally { setCapturing(false); }
              }}>Annotate picture</Button>}
              {ask && ask.questions.length > 0 && !folded && <Button variant="primary" disabled={capturing || !anyAnswered(ask.questions, ask.picked)} onClick={async () => {
                const picked = ask.picked;
                await close();
                asking.current?.onSubmit(picked);
              }}>Save answers</Button>}
            </div>
          </div>
        }
      >
        {handle?.document && (
          <iframe
            className="preview-frame"
            title={p.label}
            sandbox=""
            srcDoc={handle.document}
          />
        )}
      </Sheet>
    </article>
  );
}
/** The minimized questions: a bubble saying how many are still open. */
const QuestionsBubble = forwardRef<HTMLButtonElement, { questions: ApprovalQuestion[]; picked: Picked[]; onClick(): void }>(
  function QuestionsBubble({ questions, picked, onClick }, ref) {
    const open = questions.filter((q, i) => !picked[i] || choices(q, picked[i]).length === 0).length;
    const label =
      (questions.length === 1 ? "Show the agent’s question" : `Show the agent’s ${questions.length} questions`) +
      (open === 0 ? ", all answered" : open === questions.length ? "" : `, ${open} not answered`);
    return (
      <button ref={ref} type="button" className="preview-bubble" aria-label={label} title={label} onClick={onClick}>
        <MessageCircleQuestionMark aria-hidden strokeWidth={2} />
        <span className="preview-bubble__badge num" aria-hidden>{open === 0 ? "✓" : open}</span>
      </button>
    );
  },
);

function PictureDraft({
  draft,
  tasks,
  onChange,
  onRemove,
}: {
  draft: PreviewDraft;
  tasks: Task[];
  onChange(d: PreviewDraft): void;
  onRemove(): void;
}) {
  const app = useApp();
  const { platform, profile, session, ui } = app;
  const online = useLink().state === "online";
  const record = useStore(previewDeliveries(profile.profileId), all => all[draft.id]);
  const host = useHost(s => s);
  const delivery = record ? resolvePreviewDelivery(record, host) : undefined;
  const sent = delivery?.state === "native_delivered";
  const retry = delivery?.state === "failed" || delivery?.state === "cancelled";
  useEffect(() => watchPreviewDeliveries(app), [app]);
  const status = useRef<HTMLParagraphElement>(null);
  const { active } = useRoute();
  const focusRequest = useStore(previewDeliveryFocus(ui), (s) => s);
  useEffect(() => {
    if (!active || !delivery || focusRequest?.captureId !== draft.id) return;
    previewDeliveryFocus(ui).set(null);
    if (Date.now() - focusRequest.at < PREVIEW_FOCUS_FRESH_MS) status.current?.focus();
  }, [ui, active, delivery, focusRequest, draft.id]);
  const grants = useHost((s) => s.grants);
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useCardError();
  const [discard, setDiscard] = useState(false);
  const warn = useWarning("picture.remove");
  const remove = () => void previewDrafts.remove(draft.id).then(onRemove).catch(setError);
  const [targetNotice, setTargetNotice] = useState<string | null>(null);
  const checkedTarget = useRef(false);
  useEffect(() => {
    if (checkedTarget.current || !tasks.length || delivery) return;
    const chosen = pictureTarget(tasks, draft.taskId, host.runs);
    if (chosen.notice) {
      checkedTarget.current = true;
      setTargetNotice(`${chosen.notice} Any earlier markup is kept with its original task.`);
      void change({ ...draft, taskId: chosen.taskId, transferId: undefined, artifactId: undefined })
        .catch(setError);
    }
  }, [tasks, draft.taskId, delivery, retry]);
  useEffect(() => {
    const url = URL.createObjectURL(draft.blob);
    setUrl(url);
    return () => URL.revokeObjectURL(url);
  }, [draft.blob]);
  const change = async (next: PreviewDraft) => {
    await previewDrafts.put(next);
    onChange(next);
  };
  const attach = async () => {
    if (!draft.taskId || busy) return;
    setBusy(true);
    setError(null);
    let transferId = draft.transferId;
    let attachedArtifact = draft.artifactId;
    try {
      const result = await upload(
        { platform, profileId: profile.profileId, session },
        {
          blob: draft.blob,
          name: `Preview ${draft.previewLabel ?? "web preview"} ${draft.at}.png`,
          destination: { kind: "artifact" },
          evidence: {
            task_id: draft.taskId,
            run_id: null,
            annotation: null,
            role: "source",
          },
          label: "Attach preview picture",
          transferId,
          onDeclared: async (id) => {
            transferId = id;
            await change({ ...draft, transferId: id });
          },
        },
      );
      if (!result.artifactId)
        throw new Error(
          "The computer hasn’t confirmed the picture’s evidence link.",
        );
      attachedArtifact = result.artifactId;
      const attached = { ...draft, transferId: result.transferId, artifactId: result.artifactId };
      await change(attached);
      await previewAnnotation(attached, ui);
      announce("Preview picture attached to the task as Source");
    } catch (e) {
      if (transferId)
        await change({ ...draft, transferId, artifactId: attachedArtifact }).catch(() => undefined);
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="preview-draft">
      <img src={url || undefined} alt="Picture taken in this preview" />
      {delivery ? <p ref={status} tabIndex={-1} role="status" className="fine">{previewDeliveryText(delivery, host)}</p> : <StateLine
        phase={
          draft.artifactId
            ? "saved_on_computer"
            : busy
              ? "sending"
              : "saved_on_phone"
        }
        computer={profile.name}
        at={draft.at}
        detail={draft.artifactId ? "Attached as Source" : undefined}
      />}
      {!delivery && <>
      <label className="field__label" htmlFor={`picture-task-${draft.id}`}>
        Attach picture to
      </label>
      <select
        className="field__input"
        id={`picture-task-${draft.id}`}
        value={draft.taskId ?? ""}
        disabled={busy}
        onChange={(e) => {
          checkedTarget.current = true;
          const previous = tasks.find(t => t.task_id === draft.taskId);
          setTargetNotice(previous ? `Any earlier markup is kept with “${pictureTaskLabel(previous, host.runs, host.queues)}”. This task gets its own draft.` : null);
          void change({ ...draft, taskId: e.target.value || null, transferId: undefined, artifactId: undefined }).catch(setError);
        }}
      >
        <option value="">Choose a task</option>
        {pictureTasks(tasks).map((t) => (
          <option key={t.task_id} value={t.task_id}>
            {pictureTaskLabel(t, host.runs, host.queues)}
          </option>
        ))}
      </select>
      </>}
      {targetNotice && <p role="status" className="fine">{targetNotice}</p>}
      {error && <Notice tone="warn">{error}</Notice>}
      <div className="preview-actions">
        <Button disabled={busy} onClick={async () => {
          setBusy(true); setError(null);
          try { const id = await previewAnnotation(draft, ui); ui.push({ screen: "annotate", annotationId: id }); }
          catch (e) { setError(e); }
          finally { setBusy(false); }
        }}>Annotate picture</Button>
        {(delivery || (draft.artifactId && draft.taskId)) && <Button onClick={() => {
          ui.openTask(draft.projectId, delivery?.taskId ?? draft.taskId);
        }}>Open task</Button>}
        {retry && delivery && host.tasks[delivery.taskId] && <RetryRequest task={host.tasks[delivery.taskId]!} load={async () => {
          if (delivery.request) return { item: delivery.request };
          await session.loadTimeline(delivery.taskId);
          const original = resolvePreviewDelivery(delivery, session.state.get()).request;
          if (!original) throw new Error("The submitted request isn’t available yet. Connect and load the task’s receipt before retrying.");
          return { item: original };
        }} />}
        {!delivery && !draft.artifactId && (
          <Button
            icon={Link}
            variant="primary"
            disabled={
              !online || !draft.taskId || !grants?.tasks || !grants.transfers
            }
            pending={busy}
            onClick={() => void attach()}
          >
            {draft.transferId ? "Resume attachment" : "Attach to this task"}
          </Button>
        )}
        <Button
          variant="quiet"
          disabled={busy}
          onClick={() => warn.ask(() => setDiscard(true), remove)}
        >
          Remove from this device{warn.more}
        </Button>
      </div>
      <Sheet
        open={discard}
        onClose={() => setDiscard(false)}
        title="Remove this picture from this device?"
        footer={
          <Button
            variant="stop"
            onClick={() => { warn.agreed(); remove(); }}
          >
            Remove picture
          </Button>
        }
      >
        <p>
          {sent || draft.artifactId
            ? "The task’s copy on the computer stays attached."
            : "This removes the picture saved on this device. You can take another picture by reopening the preview."}
        </p>
        {warn.check}
      </Sheet>
    </div>
  );
}
/**
 * The computer can't tell when a preview's server started, so after a task in
 * `projectId` changes files, say that a server which doesn't reload is still
 * serving the old version, until that change is acknowledged.
 */
function FileChangeNote({ projectId, named }: { projectId: string; named: boolean }) {
  const { profile } = useApp();
  const project = useHost((s) => s.projects[projectId]?.name);
  const tasks = useHost((s) => Object.values(s.tasks).filter((t) => t.project_id === projectId), shallowEqual);
  const runs = useHost((s) => s.runs);
  const change = latestFileChange(tasks, runs);
  const key = `preview-change-seen.${profile.profileId}.${projectId}`;
  const [seen, setSeen] = useState(() => readLocal<string | null>(key, null));
  if (!change || seen === change.run.run_id) return null;
  return (
    <Notice
      title={<>“{change.task.title}” changed files{named && project ? ` in ${project}` : ""} <Age at={change.run.ended_at!} suffix=" ago" /></>}
      action={
        <>
          <Button size="sm" variant="secondary" onClick={() => document.getElementById("control-terminals")?.scrollIntoView?.({ block: "start" })}>
            Go to Terminals
          </Button>
          <Button size="sm" variant="quiet" onClick={() => {
            writeLocal(key, change.run.run_id);
            setSeen(change.run.run_id);
          }}>
            Got it
          </Button>
        </>
      }
    >
      If a preview still shows the old version, its server may not reload
      changed files. Restart it on {profile.name}, then open the preview again.
    </Notice>
  );
}
export default function Previews({
  previews: all,
  compact = false,
  title = "Previews",
}: {
  previews: PreviewEndpoint[];
  compact?: boolean;
  title?: string | null;
}) {
  const { profile } = useApp();
  const finds = useHost((s) => !!s.capabilities?.features.includes("found_previews_v1"));
  const [adding, setAdding] = useState(false);
  const removing = useRemovePreview();
  const add = () => setAdding(true);
  // A found site that stopped is kept a moment on the computer in case it
  // starts again, but there is nothing to open meanwhile.
  const reachable = all.filter((p) => !(p.automatic && p.reachable === false));
  const scope = useProjectScope();
  const previews = reachable.filter((p) => scope.here(p.project_id));
  const elsewhere = useByProject(reachable.filter((p) => !scope.here(p.project_id)), (p) => p.project_id);
  const [others, setOthers] = useState(false);
  const projects = [...new Set(previews.map((p) => p.project_id))];
  const cards = (list: PreviewEndpoint[]) => compact ? (
    <div className="group">
      {list.map((p) => <PreviewCard key={p.preview_id} preview={p} compact remove={removing.allowed ? () => removing.ask(p) : undefined} />)}
    </div>
  ) : list.map((p) => <PreviewCard key={p.preview_id} preview={p} remove={removing.allowed ? () => removing.ask(p) : undefined} />);
  return (
    <Section title={title ?? undefined} id={title ? "control-previews" : undefined} className={compact ? "control-part" : undefined}
      action={compact && previews.length > 0 && <Button size="sm" icon={Plus} onClick={add}>Add</Button>}>
      {projects.map((id) => <FileChangeNote key={id} projectId={id} named={projects.length > 1} />)}
      {previews.length ? cards(previews) : compact ? (
        <p className="fine control-none">
          {finds
            ? scope.name ? `No sites running in ${scope.name}. A dev server started in its folder appears here by itself. ` : "No sites running. A dev server started in a project folder appears here by itself. "
            : scope.name ? `No previews in ${scope.name}. ` : "No previews added. "}
          <button type="button" className="linklike" onClick={add}>Add a preview</button>
        </p>
      ) : (
        <Empty
          title={finds ? "No sites running" : "No previews added"}
          action={
            <Button onClick={add}>
              Add a preview
            </Button>
          }
        >
          {finds
            ? `A site started in one of your projects’ folders, such as a dev server, appears here by itself. Add one running elsewhere on ${profile.name} to open it on this device.`
            : "Add a web app that’s running on the computer, such as a dev server, to open it on this device."}
        </Empty>
      )}
      <OtherProjects count={elsewhere.reduce((n, g) => n + g.items.length, 0)} open={others} onToggle={() => setOthers((o) => !o)}>
        {elsewhere.map((g) => (
          <OtherProject key={g.id ?? ""} name={g.name}>
            {g.id && <FileChangeNote projectId={g.id} named={false} />}
            {cards(g.items)}
          </OtherProject>
        ))}
      </OtherProjects>
      {removing.error && <Notice tone="stop">{removing.error}</Notice>}
      <AddPreviewSheet open={adding} onClose={() => setAdding(false)} projectId={scope.projectId} />
      {removing.sheet}
    </Section>
  );
}
