// SPDX-License-Identifier: Apache-2.0
import { carriedPlan, sentPlan, useCarryPlan, reviewPlan } from "../talk/carryPlan";
import { ApprovalReason, settledText } from "../../design/ApprovalCard";
import { PlanFigure, Steps } from "../talk/Transcript";
import { ModeSwitch } from "../talk/ModeSwitch";
import { NOT_RUN, rows } from "../talk/timeline";
import { ReconcileButtons, ReconcileNote, useJustRecorded, type ReceiptParts } from "./Reconcile";
import { SummarizeButton, useSummarizable } from "./AskWhatHappened";
import {
  CircleCheck,
  CircleHelp,
  CircleSlash,
  CircleX,
  FileDiff,
  MessageSquare,
  Minus,
} from "lucide-react";
import { useBackLayer } from "../../design/back";
import { useEffect, useId, useRef, useState } from "react";
import {
  describeError,
  shallowEqual,
  useApp,
  useCommand,
  useHost,
  useLink,
  useUi,
} from "../../app/context";
import { useNow } from "../../design/time";
import { Age } from "../../design/Age";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Pill, Segmented } from "../../design/Chip";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Markdown } from "../../design/Markdown";
import { UnifiedDiff } from "../../design/DiffLines";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import {
  annotationStore,
  blobs,
  type SourceDraft,
} from "../../state/annotations";
import { uuidv7 } from "../../protocol/ids";
import { isResult } from "../../state/projection";
import type { ActivityStatus, AnnotationSource, CarriedPlan, ChangedFile, DeliveryState, ReceiptFact, Run, Task } from "../../protocol/types";
import { Screen, useRoute } from "../../shell/Screen";
import { NotAllowed } from "../../shell/NotAllowed";
import type { RouteProps } from "../types";
import {
  childProvenance,
  latestBundle,
  latestRun,
  displayModel,
  markEvidence,
  receiptHeadline,
  followUpLine,
  verificationLine,
  type MarkEvidence,
} from "./logic";
import { runOutcome } from "./outcome";
import { OutcomeCard } from "./OutcomeCard";
import { carryOn, carryOnHold } from "./carryOn";
import { observedWork, type ObservedWork } from "./observed";
import { acquirePicture, usePicture, type Picture } from "./pictures";
import {
  CommandFeedback,
  ConnectionNote,
  useCanWork,
  useUnconfirmedCommand,
} from "./shared";
import { RequestedChecks } from "./RequestedChecks";
import PortableBrief from "./PortableBrief";
import { useTaskArtifacts } from "./useTaskArtifacts";
import { bundleSummary } from "../talk/timeline";
import { effortLabel, runAgent } from "../talk/intent";
import "./work.css";
import "./results.css";
import { runDuration } from "./runDuration";
import { StopFacts } from "./AgentFacts";
import { RunPicker } from "./RunPicker";
import { PutBack } from "./RetryRequest";
import { previewCaptureWords } from "../annotate/sources";
import { heldWithdrawn, heldWithdrawnFact, receiptRequests } from "./receiptRequests";
import { stopFactWord, stopProgress, stopReceipt, stopUnknowns, type StopProgress } from "./stopProgress";
import { receiptTitle } from "./receiptTitle";
import { withdrawnByRevocation, withdrawnByStop } from "../../state/timeline";
import { continuationSelection } from "../talk/continuationSelection";
import { draftKey } from "../../app/ui";
import { Composer } from "../talk/Composer";
import { CommandError } from "../../host/session";
import { useWholeRun, withReportedDiffs } from "./wholeRun";
import { useViewingTask } from "../../app/viewing";

export default function Receipt({ route }: RouteProps<"receipt">) {
  const { session, profile } = useApp();
  const link = useLink();
  const task = useHost((s) => s.tasks[route.taskId]);
  const held = !!task;
  // A task older than those the computer's snapshot carries is read when its
  // receipt opens.
  const [reading, setReading] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  useEffect(() => {
    setReading(false);
    if (held || link.state !== "online" || !session.readsHistory) return;
    let alive = true;
    setReading(true);
    setFailure(null);
    session.loadTask(route.taskId).then(
      () => { if (alive) setReading(false); },
      (e) => {
        if (!alive) return;
        setReading(false);
        if (!(e instanceof CommandError && e.error.code === "not_found")) setFailure(describeError(e, profile.name));
      },
    );
    return () => { alive = false; };
  }, [held, link.state, route.taskId, session, profile.name]);
  return task ? (
    <TaskReceipt key={task.task_id} task={task} initialRunId={route.runId} section={route.section} />
  ) : (
    <Screen title="Task receipt" className="work-screen">
      <ConnectionNote />
      {reading ? (
        <p className="work-muted" role="status">Reading this task from the computer…</p>
      ) : (
        <Empty title="Task not available">
          {failure ?? "The computer hasn’t provided a record for this task."}
        </Empty>
      )}
    </Screen>
  );
}
const RETEST = "Please retest the changes in this task and report the checks you ran.";

function TaskReceipt({ task, initialRunId, section }: { task: Task; initialRunId?: string; section?: "changes" }) {
  const app = useApp();
  const { session, profile, ui } = app;
  const send = useCommand();
  const { active } = useRoute();
  const link = useLink();
  const plan = useCarryPlan(task);
  const latest = useHost((s) => latestRun(task, s.runs));
  const runs = useHost(s => Object.values(s.runs).filter(r => r.task_id === task.task_id).sort((a, b) => b.admitted_at - a.admitted_at), shallowEqual);
  const [selectedRun, selectRun] = useState<string | null>(initialRunId ?? null);
  useEffect(() => { selectRun(initialRunId ?? null); }, [initialRunId]);
  const run = selectedRun ? runs.find(r => r.run_id === selectedRun) : latest;
  const whole = useWholeRun(run, active);
  // A long task's earlier runs are not in the computer's lists; its
  // conversation names them, and they are read by id.
  const unheld = useHost((s) =>
    [...new Set((s.timelines[task.task_id]?.items ?? []).flatMap((i) => (i.run_id && !(i.run_id in s.runs) ? [i.run_id] : [])))].join(" "),
  );
  useEffect(() => {
    if (!unheld || !active || link.state !== "online" || !session.readsHistory) return;
    // One that can't be read stays out of the list, and is not asked for
    // again until the runs to read change.
    for (const runId of unheld.split(" ")) void session.loadRun(runId).catch(() => undefined);
  }, [unheld, active, link.state, session]);
  const changedFiles = withReportedDiffs(run?.outcome?.changed_files ?? [], whole);
  // Opened to see a finished task's changes, it starts at them.
  const changesId = useId();
  const seeChanges = () => document.getElementById(changesId)?.scrollIntoView?.({ block: "start" });
  const stepsId = useId(), helpersId = useId();
  const see = (part: keyof ReceiptParts) =>
    document.getElementById(part === "changes" ? changesId : part === "steps" ? stepsId : helpersId)?.scrollIntoView?.({ block: "start" });
  useEffect(() => {
    if (section === "changes") seeChanges();
  }, [section, changesId]);
  useViewingTask(task.task_id);
  const now = useNow();
  const canWork = useCanWork();
  const uncheckedCommand = useUnconfirmedCommand(c => c.command === "check_helpers" && c.run_id === run?.run_id);
  const working = !!run && ["admitted", "dispatching", "working", "awaiting_approval", "awaiting_input", "stopping"].includes(run.state);
  const current = run?.run_id === latest?.run_id;
  // What the latest run came to heads its receipt; an earlier one's is history.
  const outcome = current ? runOutcome(task, run) : null;
  const recorded = useJustRecorded(run);
  const timeline = useHost((s) => s.timelines[task.task_id]);
  const artifacts = useHost(
    (s) => Object.values(s.artifacts).filter((a) => a.task_id === task.task_id || a.links?.some((l) => l.task_id === task.task_id)),
    shallowEqual,
  );
  const root = useHost((s) => s.libraryRoots.find((r) => r.project_id === task.project_id && r.kind === "project"));
  const harness = useHost((s) => s.harnesses.find((h) => h.kind === (run?.harness ?? task.harness)));
  const taskHarness = useHost((s) => s.harnesses.find((h) => h.kind === task.harness));
  const project = useHost((s) => s.projects[task.project_id]);
  const completions = useHost(
    (s) =>
      Object.values(s.attention).filter(
        (a) =>
          a.task_id === task.task_id &&
          isResult(s, a) &&
          a.resolved_at === null,
      ),
    shallowEqual,
  );
  const decisions = useHost(s => Object.values(s.approvals).filter(a => a.task_id === task.task_id && a.run_id === run?.run_id && a.state !== "pending"), shallowEqual);
  const stop = run ? stopReceipt(timeline?.items ?? [], run.run_id) : undefined;
  const progress = stop && run ? stopProgress(timeline?.items ?? [], run.run_id) : null;
  const steps = rows((timeline?.items ?? []).filter(item => item.run_id === run?.run_id), run ? [run] : []).filter(row => row.kind === "steps");
  const observed = observedWork(steps);
  const unsure = remains(run, stopUnknowns(stop));
  const parts: ReceiptParts = {
    changes: changedFiles.length || observed.edits.count,
    steps: !!timeline,
    helpers: unsure ? run?.children.filter((c) => c.state === "active" || c.state === "unknown").length ?? 0 : 0,
  };
  const summarize = useSummarizable(task, run ?? null);
  // Helpers that ended with the agent, one line however many there were.
  const ended = [...(run?.children ?? []).filter(c => c.provenance === "observed" && c.stop?.state === "unsupported" && c.stop.reason === "Ended with the agent process")
    .reduce((n, c) => n.set(c.label, (n.get(c.label) ?? 0) + 1), new Map<string, number>())];
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const [busy, setBusy] = useState(false);
  const [diff, setDiff] = useState<ChangedFile | null>(null);
  const [brief, setBrief] = useState(false);
  const evidence = useTaskArtifacts(task.task_id, active, retry);
  useEffect(() => {
    if (!active || link.state !== "online") return;
    let alive = true;
    setError(null);
    void (async () => {
      await session.loadTimeline(task.task_id);
      while (alive) {
        const page = session.state.get().timelines[task.task_id];
        if (!page?.hasMore) break;
        await session.loadTimeline(task.task_id, true);
        if (
          session.state.get().timelines[task.task_id]?.items.length ===
          page.items.length
        )
          break;
      }
    })().catch((e) => {
      if (alive) setError(describeError(e, profile.name));
    });
    return () => {
      alive = false;
    };
  }, [active, link.state, task.task_id, session, profile.name, retry]);
  const acknowledged = useRef(new Set<string>());
  const [attentionError, setAttentionError] = useState(false);
  useEffect(() => {
    if (!active || link.state !== "online" || !run) return;
    // Opening a receipt settles notifications, never approvals or the run outcome.
    for (const a of completions) {
      if (acknowledged.current.has(a.attention_id)) continue;
      acknowledged.current.add(a.attention_id);
      void send({ command: "acknowledge_attention", attention_id: a.attention_id },
        { label: "Mark result reviewed", toastErrors: false }).then(r => { if (!r) setAttentionError(true); });
    }
  }, [active, link.state, run, completions, send, retry]);
  const requests = receiptRequests(timeline?.items ?? [], run?.run_id);
  const withdrawn = withdrawnByStop(timeline?.items ?? []),
    revoked = withdrawnByRevocation(timeline?.items ?? []);
  const carried = (item: { text: string; carried_plan?: CarriedPlan }) =>
    timeline ? carriedPlan(item.text, sentPlan(item.text, item.carried_plan?.text, runs, timeline)) : null;
  const held = heldWithdrawn(timeline?.items ?? [], run?.run_id);
  const bundle = latestBundle(requests.filter(r => r.item.delivery === "native_delivered"));
  const marks = run && bundle ? markEvidence(bundle, run, artifacts) : [];
  const fileRoute = (f: ChangedFile) => root && /\.md$/i.test(f.path) && !/^(?:[\/\\]|[a-z]:)/i.test(f.path) && !f.path.split(/[\/\\]/).includes("..")
    ? { screen: "file" as const, rootId: root.root_id, path: f.path } : null;
  // The composer below continues this task. Closed, it is one button, so the
  // result is what the screen is about; the button opens it here,
  // Back closes it again with the draft kept, and a message sent from it is
  // followed in Talk.
  const [writing, setWriting] = useState(false);
  useBackLayer(writing && active, () => setWriting(false));
  const drafted = useUi((s) => !!s.drafts[draftKey(task.project_id, task.task_id)]?.text.trim());
  const write = () => {
    continuationSelection(ui).set({ taskId: task.task_id });
    setWriting(true);
  };
  const followInTalk = () => {
    ui.openTask(task.project_id, task.task_id);
  };
  // Retest is sent as it's tapped, and followed in Talk. One that wasn't
  // opens the message below, where what happened to it is kept.
  const hold = useHost((s) => carryOnHold(s, task), shallowEqual);
  const inDoubt = useUi((s) => {
    const phase = s.drafts[draftKey(task.project_id, task.task_id)]?.attempt?.phase;
    return phase === "sending" || phase === "unknown";
  });
  const [retesting, setRetesting] = useState(false);
  const [retestError, setRetestError] = useState<string | null>(null);
  const retest = async () => {
    setRetesting(true);
    setRetestError(null);
    try {
      const sent = await carryOn(app, task, RETEST, hold !== null);
      if (sent.ok) followInTalk();
      else {
        setRetestError(sent.attempt.detail ?? "It wasn’t sent.");
        write();
      }
    } finally {
      setRetesting(false);
    }
  };
  const correct = async (mark?: MarkEvidence) => {
    setBusy(true);
    setError(null);
    try {
      const store = annotationStore(profile.profileId);
      let source: SourceDraft | undefined;
      if (mark?.after) {
        const handle = acquirePicture(
          { platform: app.platform, session, profileId: profile.profileId },
          mark.after.artifact_id,
          mark.after.sha256,
        );
        try {
          const image = await handle.promise;
          source = {
            key: "S1",
            blobKey: await blobs.put(image.blob),
            artifactId: mark.after.artifact_id,
            sha256: image.sha256,
            mediaType: mark.after.media_type,
            width: image.width,
            height: image.height,
            durationMs: null,
            origin: {
              kind: "phone_import",
              original_name: `Receipt picture: ${mark.after.label ?? "after"}`,
            },
            capturedAt: mark.after.created_at,
            derivedFrom: null,
            derivation: null,
          };
        } finally {
          handle.release();
        }
      } else if (mark?.before) {
        const s = mark.before;
        source = {
          key: "S1",
          blobKey: null,
          artifactId: s.artifact_id,
          sha256: s.sha256,
          mediaType: s.media_type,
          width: s.width,
          height: s.height,
          durationMs: null,
          origin: s.origin,
          capturedAt: s.captured_at,
          derivedFrom: null,
          derivation: null,
        };
      }
      const draft = store.create(task.project_id, task.task_id);
      const reference = mark
        ? `Correction to ${mark.mark.label}: ${mark.comments.map((c) => c.text).join(" ") || "Original mark"}\n\nNot quite: `
        : `Correction to “${task.title}”:\n\nNot quite: `;
      store.update(draft.id, (d) => ({
        ...d,
        sources: source ? [source] : [],
        nextSource: source ? 2 : 1,
        comments: [
          {
            comment_id: uuidv7(Date.now()),
            text: reference,
            mark_ids: [],
            origin: { kind: "typed" },
            link_origins: [],
          },
        ],
        generalNote: mark?.after
          ? "Reviewing a picture returned with the task receipt. Its desktop target and capture geometry were not reported."
          : null,
      }));
      ui.push({ screen: "annotate", annotationId: draft.id });
      announce("Correction draft saved on the device.");
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Screen
      title="Task receipt"
      bodyHeading
      className="work-screen"
      stepBack={writing ? () => setWriting(false) : null}
      footer={
        project && task.state !== "archived" ? (
          writing ? <div className="talk-foot">
            <Composer project={project} task={task} run={latest ?? null} harness={taskHarness ?? null} agent={null} onSent={followInTalk} />
          </div> : <div className="receipt-continue">
            <Button block icon={MessageSquare} onClick={write}>{drafted ? "Continue your draft" : "Continue this task"}</Button>
          </div>
        ) : undefined
      }
    >
      <div className="work-stack">
        <ConnectionNote />
        {working && outcome?.kind !== "waiting" && <Notice>This run is still working. You can review its result when it ends.</Notice>}
        <header className="work-receipt-head">
          <h1>{receiptTitle(task.title, timeline?.items ?? [])}</h1>
          <p className="work-receipt-state">{receiptHeadline(task, run)}</p>
          {run && followUpLine(run) && <p className="work-caption">{followUpLine(run)}</p>}
          {run?.human_resolution && <p className="work-caption">Marked {run.human_resolution.resolution === "mark_failed" ? "failed" : "completed"} {run.human_resolution.by.kind === "device" && run.human_resolution.by.device_id === profile.deviceId ? "by you" : "by a person"} · {new Date(run.human_resolution.at).toLocaleString([], { dateStyle: "short", timeStyle: "short" })} · not verified by the computer</p>}
        </header>
        {current && task.state === "reconciliation_required" ? <>
          <ReconcileNote task={task} parts={parts} onSee={see} />
          <div className="work-review">
            <ReconcileButtons task={task} />
          </div>
        </> : recorded ? (
          <div className="work-review">
            <p className="work-review-recorded">
              Marked {recorded.resolution === "mark_failed" ? "failed" : "completed"}. The computer hasn’t verified it.
            </p>
          </div>
        ) : current && outcome && run && project && task.state !== "archived" && (
          <OutcomeCard task={task} run={run} project={project} outcome={outcome} plan={plan} onSeeChanges={seeChanges}
            writing={writing} onWrite={write} onSent={followInTalk} onFollow={followInTalk}
            onCorrect={() => void correct(marks.find((m) => m.status === "Not verified") ?? marks[0])} />
        )}
        {run?.helpers_checked && <p className="work-caption">Helpers checked {run.helpers_checked.by.kind === "device" && run.helpers_checked.by.device_id === profile.deviceId ? "by you" : "by a person"} · {new Date(run.helpers_checked.at).toLocaleString([], { dateStyle: "short", timeStyle: "short" })} · not verified by the computer</p>}
        {run?.ended_at && !run.helpers_checked && !run.human_resolution && run.children.some(c => c.state === "unknown") && <Notice tone="unknown" title="Check the helpers">
          <p>{run.children.filter(c => c.state === "unknown").map(c => c.label).join(" · ")}</p>
          <p>
            The run ended before the computer heard how {run.children.filter(c => c.state === "unknown").length === 1 ? "this helper" : "these helpers"} finished.
            What it last saw is under Still running / unknown below{summarize ? ", and Summarize asks this task’s agent what they did" : ""}.
            Recording your check clears the helper alerts; it does not verify their results.
          </p>
          <div className="work-actions">
            {current && summarize && <SummarizeButton task={task} onSent={followInTalk} />}
            <Button size="sm" onClick={() => see("helpers")}>See the helpers</Button>
            <Button pending={busy} disabled={!canWork || uncheckedCommand} onClick={async () => {
              setBusy(true);
              await send({ command: "check_helpers", run_id: run.run_id }, { expectedRevision: task.revision, label: "Helpers checked" });
              setBusy(false);
            }}>Helpers checked</Button>
          </div>
          <CommandFeedback match={c => c.command === "check_helpers" && c.run_id === run.run_id} />
        </Notice>}
        {(runs.length > 1 || selectedRun) && <RunPicker task={task} runs={runs} latest={latest} selectedId={selectedRun} onChange={id => { selectRun(id); setDiff(null); }} />}
        {selectedRun && !run && <Notice tone="unknown">This run is no longer in the computer’s available records. Select the latest run to see its receipt.</Notice>}
        {attentionError && <Notice tone="unknown" action={<Button onClick={() => { acknowledged.current.clear(); setAttentionError(false); setRetry(n => n + 1); }}>Try clearing updates again</Button>}>Completion updates haven’t been cleared on the computer.</Notice>}
        {evidence.missingGrants.length > 0 && <NotAllowed
          what="load task pictures and files"
          permission={evidence.missingGrants.join(" ")}
          detail={`Missing ${evidence.missingGrants.join(" and ")} permission. The rest of this receipt remains available.`}
        />}
        {(error || evidence.error) && !evidence.missingGrants.includes("tasks") && (
          <Notice
            tone="stop"
            action={
              <Button onClick={() => setRetry((n) => n + 1)}>
                Reload evidence
              </Button>
            }
          >
            {error ?? evidence.error}
          </Notice>
        )}
        {task.state === "archived" && <Section title="Saved request">
          <p className="work-muted">Archived on the computer. The request and its attachments are kept.</p>
          {(timeline?.items ?? []).filter(i => i.item.kind === "user_message").map(i => i.item.kind === "user_message" && <div className="work-card work-stack" key={i.seq}>
            {i.item.text && <RequestText text={i.item.text} carried={carried(i.item)} />}
            {i.item.bundle && <p>{bundleSummary(i.item.bundle)}</p>}
          </div>)}
          <Button onClick={() => ui.openTask(task.project_id, task.task_id)}>Open conversation and attachments</Button>
        </Section>}
        <Section title="Result">
          <div className="work-card work-stack">
            <p className="work-caption">Reported by the agent</p>
            {run?.outcome?.summary ? (
              <Markdown
                text={run.outcome.summary}
                onLink={(href) => void app.platform.openExternal(href)}
              />
            ) : (
              <p>The computer hasn’t provided a result summary.</p>
            )}
          </div>
        </Section>
        {/* Done, the plan is the first thing the outcome above offers. */}
        {current && plan && outcome?.kind !== "done" && <Section title="Carry out this plan">
          <ModeSwitch from={task.mode ?? "work"} to="work" who="It"
            flags={task.launch_policy ? { harness: task.harness, launchPolicy: task.launch_policy } : null} />
          <Button variant="primary" pending={busy} disabled={link.state !== "online" || !canWork} onClick={async () => {
            setBusy(true); setError(null);
            try { await reviewPlan(app, task, undefined, { send: true }); } catch (e) { setError(describeError(e, profile.name)); } finally { setBusy(false); }
          }}>Carry out this plan</Button>
        </Section>}
        {stop && (
          <Section title="Stop receipt">
            <Notice className="agent-stop-summary" title={stop.summary}>
              {progress && <StopProgressLines progress={progress} />}
              <StopFacts facts={held ? [...stop.facts, heldWithdrawnFact(held)] : stop.facts} />
            </Notice>
          </Section>
        )}
        {/* Left out when there is nothing of yours in it and nothing more to load. */}
        {(!timeline?.loaded || requests.length > 0 || timeline.hasMore) && <Section title="Your messages in this run">
          {!timeline?.loaded ? (
            <Notice>
              {link.state === "online"
                ? "Loading your messages…"
                : "Connect to load your messages."}
            </Notice>
          ) : requests.length ? requests.map(request => <div className="work-stack receipt-request" key={request.seq}>
            <p className="work-caption" role="status">{withdrawn.has(request.seq) ? "Withdrawn by Stop · not sent" : revoked.has(request.seq) ? "Withdrawn when its chat lost access · not sent" : requestDelivery(request.item.delivery, run)}</p>
            {request.item.text && <RequestText text={request.item.text} carried={carried(request.item)} />}
            {["failed", "cancelled", "unknown"].includes(request.item.delivery) && !revoked.has(request.seq) && <PutBack task={task} request={request} noteClassName="work-caption" />}
            {request.item.bundle && <>
              <p className="work-caption">{bundleSummary(request.item.bundle)}</p>
              {request.item.bundle.general_note && <p>{request.item.bundle.general_note}</p>}
              {request.item.bundle.comments.filter(c => !c.mark_ids.some(id => request.item.bundle!.marks.some(m => m.mark_id === id)))
                .map(c => <p className="work-original" key={c.comment_id}>{c.text}</p>)}
              {request.item.bundle.steps.length > 0 && <ol>{request.item.bundle.steps.map(step =>
                <li key={step.index}>{step.source_key}: {step.text}</li>)}</ol>}
              {run && markEvidence(request.item.bundle, run, artifacts).map(m => <MarkResult key={m.mark.mark_id}
                evidence={m} received={request.item.delivery === "native_delivered"} onCorrect={() => void correct(m)} busy={busy} />)}
              {request.item.bundle.sources.filter(s => !request.item.bundle!.marks.some(m => m.source_key === s.key))
                .map(s => <ReceiptSource key={s.key} source={s} />)}
            </>}
          </div>) : (
            <p className="work-muted">No message from you in the loaded history.</p>
          )}
          {timeline?.hasMore && (
            <Button
              onClick={async () => {
                try {
                  await session.loadTimeline(task.task_id, true);
                } catch (e) {
                  setError(describeError(e, profile.name));
                }
              }}
            >
              Load earlier messages
            </Button>
          )}
        </Section>}
        {timeline && <Section title="Steps" id={stepsId}>
          {steps.length ? steps.map(row => <Steps key={row.key} steps={row.steps} ended={row.ended} />)
            : <p className="work-muted">{!timeline.loaded ? (link.state === "online" ? "Loading the steps…" : "Connect to load the steps.")
              : timeline.hasMore ? "No steps in the loaded history." : "No steps reported."}</p>}
        </Section>}
        {decisions.length > 0 && <Section title="Approval decisions">
          {decisions.map(approval => {
            const decision = settledText(approval, profile.deviceId);
            return <div className="work-card work-stack" key={approval.approval_id}>
              <h3>{decision.title} · {approval.title}</h3>
              <p className="work-caption">{decision.meta}</p>
              <ApprovalReason approval={approval} />
            </div>;
          })}
        </Section>}
        <Section title="Changed files" id={changesId}>
          {changedFiles.length ? <div className="group">
            {changedFiles.map((f) => (
              <ListRow
                key={f.path}
                icon={FileDiff}
                title={<span className="mono">{f.path}</span>}
                subtitle={<>{f.change} · +{f.added ?? "?"} / −{f.removed ?? "?"}<br />
                  {f.provenance === "native" ? "Observed from the agent’s tools" : "The computer didn’t record how this file change was identified"}<br />
                  {f.diff ? "Open the recorded diff" : fileRoute(f) ? "Open the current file" : "No diff was recorded by the agent."}</>}
                chevron={!!f.diff || !!fileRoute(f)}
                onClick={f.diff ? () => setDiff(f) : fileRoute(f) ? () => { const route = fileRoute(f); if (route) ui.push(route); } : undefined}
              />
            ))}
          </div> : observed.edits.count ? <ObservedEdits edits={observed.edits} />
            : <p className="work-muted">No changed files reported.</p>}
        </Section>
        <RequestedChecks requests={task.mission?.completion_checks ?? []} checks={run?.outcome?.checks ?? []} />
        <Section title="Checks">
          {run && verificationLine(run) && <p className="work-caption">{verificationLine(run)}</p>}
          {run?.outcome?.checks.length ? (
            run.outcome.checks.map((c, i) => (
              <div
                key={i}
                className={`work-check-result work-card ${c.provenance === "observed" ? "is-observed" : ""}`}
              >
                <div className="work-check-title">
                  {c.status === "passed" ? (
                    <CircleCheck aria-hidden />
                  ) : c.status === "failed" ? (
                    <CircleX aria-hidden />
                  ) : c.status === "not_run" ? (
                    <Minus aria-hidden />
                  ) : (
                    <CircleHelp aria-hidden />
                  )}
                  <h3>{c.name}</h3>
                  <Pill>
                    {c.status === "not_run"
                      ? "Not run"
                      : c.status.charAt(0).toUpperCase() + c.status.slice(1)}
                  </Pill>
                </div>
                {c.command && <code className="work-command">{c.command}</code>}
                <p className="work-caption">
                  {c.provenance === "observed"
                    ? "Run by the computer"
                    : c.provenance === "native"
                      ? "Reported by the agent"
                      : "Inferred, not confirmed"}
                </p>
                {c.after_last_change === false && <p className="work-caption">Ran before its last change</p>}
                {c.detail && <p className="work-muted">{c.detail}</p>}
                {c.output !== undefined && <details className="work-check-output"><summary>Output</summary><pre>{c.output}</pre></details>}
              </div>
            ))
          ) : observed.commands.length ? (
            <ObservedCommands commands={observed.commands} />
          ) : (
            <p className="work-muted">No checks reported.</p>
          )}
        </Section>
        {unsure && <Section title="Still running / unknown" id={helpersId}>
          <Remaining run={run} stopped={stopUnknowns(stop)} />
        </Section>}
        {run && <details className="work-details">
          <summary>Run details</summary>
          <dl className="work-facts">
            <div>
              <dt>Requested</dt>
              <dd>
                {displayModel(run.requested_model, harness, "Agent default")} ·{" "}
                {run.requested_effort ? effortLabel(run.requested_effort) : "Default effort"}
              </dd>
            </div>
            <div>
              <dt>Actual{runAgent(harness ? [harness] : [], run).differs && <span className="work-row-line">Different from requested</span>}</dt>
              <dd>
                {displayModel(run.actual_model, harness, "Not reported")} ·{" "}
                {run.actual_effort ? effortLabel(run.actual_effort) : "Effort not reported"}
              </dd>
            </div>
            <div>
              <dt>Account</dt>
              <dd>{run.account_label ?? "Not reported"}</dd>
            </div>
            <div>
              <dt>Elapsed time</dt>
              <dd>
                {runDuration(run, timeline?.items ?? [], now)}
              </dd>
            </div>
            {run.outcome?.usage && <div>
              <dt>Usage<span className="work-row-line">Reported by the agent</span></dt>
              <dd>{run.outcome.usage.reported_cost ?? "No cost reported"}</dd>
            </div>}
          </dl>
          {ended.length > 0 && <p className="work-caption">{ended.map(([label, n]) => n > 1 ? `${label} ×${n}` : label).join(", ")} · Ended with the agent process · Observed on the computer</p>}
        </details>}
        <Section title="Continue from this result">
          <div className="work-actions">
            {project && task.state !== "archived" && (
              <Button icon={MessageSquare} pending={retesting} disabled={!canWork || inDoubt} onClick={() => void retest()}>
                {hold ? "Retest in Next up" : "Retest"}
              </Button>
            )}
            <Button onClick={() => ui.push({ screen: "reusable", taskId: task.task_id })}>Make reusable…</Button>
            <Button onClick={() => setBrief(true)}>Portable brief…</Button>
          </div>
          {retestError && <Notice tone="stop">{retestError}</Notice>}
        </Section>
        <CommandFeedback
          match={(c) =>
            c.command === "acknowledge_attention" &&
            completions.some((a) => a.attention_id === c.attention_id)
          }
        />
        {brief && <PortableBrief task={task} evidenceComplete={!evidence.loading && !evidence.error && !evidence.missingGrants.length && link.state === "online"} onClose={() => setBrief(false)} />}
        <Sheet
          className="work-sheet"
          size="tall"
          open={diff !== null}
          onClose={() => setDiff(null)}
          title="Agent-reported diff"
          subtitle={diff?.path}
        >
          <div className="work-stack">
            <p className="work-muted">Recorded from the agent’s file tool. The file may have changed since this run.</p>
            {diff?.diff?.truncated && <Notice>Only the beginning of this diff was retained.</Notice>}
            {diff?.diff && <UnifiedDiff text={diff.diff.text} label="Recorded file changes" />}
          </div>
        </Sheet>
      </div>
    </Screen>
  );
}
function ObservedEdits({ edits }: { edits: ObservedWork["edits"] }) {
  const files = edits.files.length;
  const lead = edits.unnamed.length
    ? `${edits.count} ${edits.count === 1 ? "edit" : "edits"}`
    : `edits to ${files} ${files === 1 ? "file" : "files"}`;
  return (
    <div className="work-observed">
      <p className="work-muted">The agent hasn’t reported changed files. Steps show {lead}:</p>
      <ul>
        {edits.files.map(path => <li key={path} className="mono">{path}</li>)}
        {edits.unnamed.map((line, i) => <li key={i}>{line}</li>)}
      </ul>
    </div>
  );
}
function ObservedCommands({ commands }: { commands: ObservedWork["commands"] }) {
  const failed = commands.filter(c => c.status === "failed" && !c.notRun).length;
  const notRun = (["denied", "stopped"] as const).map(why => [why, commands.filter(c => c.notRun === why).length] as const);
  return (
    <div className="work-observed">
      <p className="work-muted">
        The agent hasn’t reported checks. Steps show {commands.length} {commands.length === 1 ? "command" : "commands"}
        {failed > 0 && `, ${failed} failed`}
        {notRun.map(([why, n]) => n > 0 && `, ${n} ${NOT_RUN[why].counted}`)}:
      </p>
      <ul role="list">
        {commands.map(c => (
          <li key={c.key} className="work-observed__command">
            {c.notRun ? <CircleSlash aria-hidden /> : c.status === "succeeded" ? <CircleCheck aria-hidden /> : c.status === "failed" ? <CircleX className="work-observed__failed" aria-hidden /> : <CircleHelp aria-hidden />}
            <code className="work-command">{c.text}</code>
            <span className="work-caption">{c.notRun ? NOT_RUN[c.notRun].label : COMMAND_STATUS[c.status]}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
const COMMAND_STATUS: Record<ActivityStatus, string> = {
  running: "Running",
  succeeded: "Succeeded",
  failed: "Failed",
  unknown: "Result unknown",
};
/** Whether anything is left running or unconfirmed, or an ended run's
 * outcome is missing; a receipt with nothing to say leaves the section out. */
function remains(run: Run | undefined, stopped: ReceiptFact[]): boolean {
  if (stopped.length || run?.children.some((c) => c.state === "active" || c.state === "unknown")) return true;
  if (!run?.outcome) return !!run?.ended_at;
  return run.outcome.still_active.length > 0 || run.outcome.unknowns.length > 0;
}
function Remaining({ run, stopped }: { run: Run | undefined; stopped: ReceiptFact[] }) {
  const children =
    run?.children.filter(
      (c) => c.state === "active" || c.state === "unknown",
    ) ?? [];
  const active = run?.outcome?.still_active ?? [];
  const unknown = run?.outcome?.unknowns ?? [];
  // A helper already listed from the run keeps one row; Stop adds what only it knows.
  const fromStop = stopped.filter((f) => !children.some((c) => f.label === `Child: ${c.label}`));
  return (
    <>
      {(run?.human_resolution || run?.helpers_checked) && children.length > 0 && <p className="work-caption">These are the helper facts the computer last reported. Your decision covers this run and its helper unknowns; their results remain unverified.</p>}
      {children.map((c) => (
        <Notice
          key={c.child_id}
          tone="unknown"
          title={`${c.label} · ${c.state === "active" ? "Still running" : "State unknown"}`}
        >
          {run && childProvenance(c, run)} ·{" "}
          <Age at={c.started_at} suffix=" ago" />
          {c.stop && "reason" in c.stop && <p>{c.stop.reason}</p>}
        </Notice>
      ))}
      {active.map((s, i) => (
        <Notice key={`a${i}`} tone="unknown" title="Still active">
          {s}
        </Notice>
      ))}
      {unknown.map((s, i) => (
        <Notice key={`u${i}`} tone="unknown" title="Not confirmed">
          {s}
        </Notice>
      ))}
      {fromStop.map((f, i) => (
        <Notice key={`s${i}`} tone="unknown" title={`${f.label.replace(/^Child: /, "")} · ${stopFactWord(f)}`}>
          <p className="work-caption">From the Stop receipt</p>
          {f.detail && <p>{f.detail}</p>}
        </Notice>
      ))}
      {!children.length && !active.length && !unknown.length && !fromStop.length && (
        <p className="work-muted">
          {run?.outcome
            ? "No remaining work or unknowns reported in this result."
            : "The computer hasn’t provided a complete outcome."}
        </p>
      )}
    </>
  );
}
/** A request's words, with a plan it carried quoted as Talk quotes it, never as the person's words. */
function RequestText({ text, carried }: { text: string; carried: { message: string; number: number; plan: string } | null }) {
  const words = carried ? carried.message : text;
  return <>
    {words && <p className="work-original">{words}</p>}
    {carried && <PlanFigure carried={carried} />}
  </>;
}
function MarkResult({
  evidence: e,
  onCorrect,
  busy,
  received,
}: {
  received: boolean;
  evidence: MarkEvidence;
  onCorrect: () => void;
  busy: boolean;
}) {
  const before = usePicture(e.before?.artifact_id, e.before?.sha256);
  const after = usePicture(e.after?.artifact_id, e.after?.sha256);
  const [side, setSide] = useState<"before" | "after">("after");
  const picture = side === "before" ? before : after;
  const status =
    e.after && (before.error || after.error) ? "Not verified" : e.status;
  const different =
    before.picture &&
    after.picture &&
    (before.picture.width !== after.picture.width ||
      before.picture.height !== after.picture.height);
  if (!received) return <article className="work-card work-mark work-stack">
    <h3 className="work-mark-label">{e.mark.label}</h3>
    {e.comments.map(c => <p className="work-original" key={c.comment_id}>{c.text}</p>)}
    {e.before && <ReceiptSource source={e.before} />}
  </article>;
  return (
    <article className="work-card work-mark work-stack">
      <div className="work-mark-head">
        <h3 className="work-mark-label">{e.mark.label}</h3>
        <Pill icon={status === "Evidence found" ? CircleCheck : CircleHelp}>
          {status}
        </Pill>
      </div>
      {e.comments.map((c) => (
        <p className="work-original" key={c.comment_id}>
          {c.text}
        </p>
      ))}
      {!e.comments.length && (
        <p className="work-muted">No comment attached to this mark.</p>
      )}
      <p className="work-caption">{e.reason}</p>
      {e.before && (
        <p className="work-caption">
          {previewCaptureWords(e.before.origin) ?? `Original ${e.before.key}`} · {e.before.width} × {e.before.height} ·{" "}
          <Age at={e.before.captured_at} suffix=" ago" />
        </p>
      )}
      <Segmented
        label={`Picture for ${e.mark.label}`}
        value={side}
        onChange={setSide}
        options={[
          { value: "before", label: "Before" },
          { value: "after", label: "After" },
        ]}
      />
      {side === "after" && !e.after ? (
        <div className="work-picture-empty">
          No after picture from the computer
        </div>
      ) : picture.picture ? (
        <PictureView
          picture={picture.picture}
          label={`${side === "before" ? "Original" : "Returned"} picture for mark ${e.mark.label}`}
        />
      ) : picture.error ? (
        <Notice
          tone="unknown"
          action={<Button onClick={picture.retry}>Load picture again</Button>}
        >
          {picture.error}
        </Notice>
      ) : (
        <div className="work-picture-empty">Loading picture…</div>
      )}
      {different && (
        <Notice>
          Different picture dimensions. These views are shown separately; their
          marks are not aligned.
        </Notice>
      )}
      {e.after && (
        <p className="work-caption">
          {e.after.label ?? "Returned picture"} ·{" "}
          <Age at={e.after.created_at} suffix=" ago" />
        </p>
      )}
      <Button onClick={onCorrect} disabled={busy}>
        Not quite — {e.mark.label}
      </Button>
    </article>
  );
}
function PictureView({ picture, label }: { picture: Picture; label: string }) {
  return (
    <figure className="work-picture">
      <img src={picture.url} alt={label} />
      <figcaption>
        {picture.width} × {picture.height}
      </figcaption>
    </figure>
  );
}

/** Where the run had got to when it was stopped, as its own timeline shows. */
function StopProgressLines({ progress: { said, finished, failed, last } }: { progress: StopProgress }) {
  const steps = finished === 1 ? "1 step" : `${finished} steps`;
  return <div className="stop-progress work-stack">
    {said && <p>The agent’s last message before Stop: “{said}”</p>}
    {last && <p>{steps} finished before Stop{failed ? `, ${failed} failed` : ""}. Last step: {last.name}{last.input && <> <code>{last.input}</code></>} · {last.ended}</p>}
  </div>;
}

function requestDelivery(state: DeliveryState, run?: Run): string {
  switch (state) {
    case "native_delivered": return "Delivered to the agent";
    case "failed": return run?.state === "interrupted" ? "Not delivered · task stopped" : "Not delivered · delivery failed";
    case "cancelled": return "Not delivered · cancelled";
    case "queued_for_boundary": return "Waiting to send · held until the turn ends";
    case "host_accepted": return "Sent to the computer · delivery to the agent is unconfirmed";
    case "unknown": return "Delivery to the agent is unconfirmed";
  }
}
function ReceiptSource({ source }: { source: AnnotationSource }) {
  const { picture, error, retry } = usePicture(source.artifact_id, source.sha256);
  return <div className="work-stack">
    <p className="work-caption">{previewCaptureWords(source.origin) ?? `Original ${source.key} · ${new Date(source.captured_at).toLocaleString()}`}</p>
    {picture ? <PictureView picture={picture} label={`Original picture ${source.key}`} />
      : error ? <Notice action={<Button onClick={retry}>Load picture again</Button>}>{error}</Notice>
      : <p className="work-caption">Loading picture…</p>}
  </div>;
}
