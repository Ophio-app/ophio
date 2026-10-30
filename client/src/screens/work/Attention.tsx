// SPDX-License-Identifier: Apache-2.0
import { ReportAttention } from "../../reports/Questions";
import { useLayoutEffect, useRef, useState } from "react";
import { useApp, useHost, useLink, useCommand, shallowEqual } from "../../app/context";
import type { UiStore, Route } from "../../app/ui";
import { ApprovalCard } from "../../design/ApprovalCard";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { Age } from "../../design/Age";
import { useNow } from "../../design/time";
import { Screen, useRoute } from "../../shell/Screen";
import type { HostState } from "../../state/projection";
import type { RouteProps } from "../types";
import type { Approval, AttentionItem, DeclineReason, InboxReportCode, ReportVerdict, Task } from "../../protocol/types";
import { declineWords } from "../../reports/words";
import { REPORT_INBOX_FEATURE, REPORT_LOOK_INTO_FEATURE } from "../../diagnostics/constants";
import { APPROVALS_NOT_GRANTED, UnsavedAnswer, useApprovalDecision } from "./shared";
import "./work.css";
import { useQueue, pendingQueueItems } from "./useQueue";
import { PrChip } from "../github/PrSheet";
import { unlockFor, useGithubRun } from "../github/common";

type Destination =
  | { kind: "inbox" }
  | { kind: "report_fix"; task: Task }
  | { kind: "receipt"; task: Task; runId?: string }
  | { kind: "request"; approval: Approval }
  | { kind: "talk"; task: Task }
  | { kind: "queue"; task: Task }
  | { kind: "missing"; reason: string };

/** A problem report someone sent to the owner's intake, for the owner to read. */
export function isInboxItem(item: AttentionItem): boolean {
  return !!item.report_id && !item.task_id && item.kind === "waiting_for_you";
}

/** Never substitute the task's newest run for the immutable alert reference. */
export function attentionDestination(item: AttentionItem | undefined, state: Pick<HostState, "tasks" | "runs" | "approvals" | "caughtUp"> & Partial<Pick<HostState, "grants">>, now: number): Destination {
  if (!item) return { kind: "missing", reason: state.caughtUp ? "This item is no longer available on the computer." : "Waiting for the computer’s current items…" };
  if (item.resolved_at !== null) return { kind: "missing", reason: "This update has already been handled. It no longer needs a decision." };
  if (item.kind === "approval" || item.approval_id) {
    const approval = item.approval_id ? state.approvals[item.approval_id] : undefined;
    if (!approval && state.grants?.approvals === false) return { kind: "missing", reason: APPROVALS_NOT_GRANTED };
    if (!approval || (item.task_id && item.task_id !== approval.task_id) || (item.run_id && item.run_id !== approval.run_id))
      return { kind: "missing", reason: "The request this alert refers to is no longer available." };
    if (approval.state !== "pending") return { kind: "missing", reason: "This request has already been settled. No further decision is needed." };
    if (approval.expires_at !== null && approval.expires_at <= now) return { kind: "missing", reason: "This request’s deadline has passed. It can no longer be answered here." };
    return { kind: "request", approval };
  }
  if (isInboxItem(item)) return { kind: "inbox" };
  const task = item.task_id ? state.tasks[item.task_id] : undefined;
  if (item.report_id && task?.github?.pr && task.github.repo) return { kind: "report_fix", task };
  if (!task) return { kind: "missing", reason: "The task this alert refers to is no longer available." };
  if (item.run_id && state.runs[item.run_id]?.task_id !== task.task_id)
    return { kind: "missing", reason: "This alert refers to an earlier or unavailable run. The task’s current receipt may describe different work." };
  if (["completed", "failure", "reconciliation_required", "conflict"].includes(item.kind)) return { kind: "receipt", task, ...(item.run_id ? { runId: item.run_id } : {}) };
  if (item.kind === "waiting_for_you" && task.state === "accepted" && !item.run_id) return { kind: "queue", task };
  if (item.kind === "waiting_for_you" || item.kind === "question") return { kind: "talk", task };
  return { kind: "missing", reason: "This update has no request or result to open." };
}

/** Replace only the opened attention route; leave every unrelated stack intact. */
export function placeAttention(ui: UiStore, attentionId: string, destination: Route): void {
  ui.set((s) => {
    const source = s.stacks[s.tab];
    const top = source.at(-1);
    if (top?.screen !== "attention" || top.attentionId !== attentionId) return s;
    if (s.tab === "work" && destination.screen === "attention") return s;
    const stacks = { ...s.stacks, [s.tab]: source.slice(0, -1) };
    const work = stacks.work;
    const last = work.at(-1);
    const same = last && JSON.stringify(last) === JSON.stringify(destination);
    const next = same ? work : [...work, destination].slice(-12);
    // Opened from another tab, Back from it goes back there.
    const returns = s.tab === "work" ? s.returns : { ...s.returns, work: { tab: s.tab, depth: next.length } };
    return { ...s, tab: "work", stacks: { ...stacks, work: next }, returns };
  });
}

/** Opening an alert is navigation only. Decisions still quote the request revision. */
export default function Attention({ route }: RouteProps<"attention">) {
  const { ui } = useApp();
  const { active } = useRoute();
  const state = useHost((s) => ({ attention: s.attention, tasks: s.tasks, runs: s.runs, approvals: s.approvals, projects: s.projects, caughtUp: s.caughtUp, grants: s.grants }), shallowEqual);
  const link = useLink();
  const now = useNow();
  const item = state.attention[route.attentionId];
  const task = item?.task_id ? state.tasks[item.task_id] : undefined;
  const destination = attentionDestination(item, state, now);
  const kind = destination.kind;
  const queuedTask = kind === "queue" ? destination.task : null;
  const { queue, error: queueError } = useQueue(queuedTask?.project_id ?? null);
  const front = pendingQueueItems(queue)[0];
  const queueItemId = queuedTask && !queue?.paused && front?.task_id === queuedTask.task_id && front.state.state === "awaiting_start" ? front.item_id : null;
  const taskId = kind === "receipt" || kind === "talk" || kind === "queue" ? destination.task.task_id : null;
  const runId = destination.kind === "receipt" ? destination.runId : undefined;
  const projectId = kind === "talk" || kind === "queue" ? destination.task.project_id : null;
  useLayoutEffect(() => {
    if (!active || (item?.report_id && kind !== "report_fix")) return;
    if (kind === "queue" && taskId && projectId && queueItemId) {
      ui.contextProject(projectId);
      placeAttention(ui, route.attentionId, { screen: "review", projectId, taskId, queueItemId });
    } else if (kind === "talk" && taskId && projectId) {
      ui.finish(["attention"]);
      ui.openTask(projectId, taskId);
    } else {
      placeAttention(ui, route.attentionId, kind === "receipt" && taskId ? { screen: "receipt", taskId, ...(runId ? { runId } : {}) } : route);
    }
  }, [ui, route, active, kind, taskId, projectId, runId, queueItemId]);
  if (item && isInboxItem(item)) return <Screen title="Problem report" className="work-screen"><InboxReport item={item} /></Screen>;
  // A fix waiting for review carries its report too; only status updates open the report.
  if (item?.report_id && kind !== "report_fix") return <Screen title="Report update"><ReportAttention reportId={item.report_id} /></Screen>;
  const request = destination.kind === "request" ? destination.approval : null;
  return (
    <Screen title={request ? request.kind === "question" ? "Question" : "Approval" : "Task update"} className="work-screen">
      <div className="work-stack">
        {link.state !== "online" ? <Notice tone="unknown">Connect to the computer to check this item’s current state.</Notice>
          : !state.caughtUp && item && <Notice tone="unknown">Checking this item’s current state with the computer…</Notice>}
        {queuedTask && <Notice>{queueError ?? (queue ? "This queue item no longer needs a go-ahead at the front of Next up." : "Loading the queued request…")}</Notice>}
        {destination.kind === "missing" && <Notice>{destination.reason}</Notice>}
        {request ? <AttentionApproval key={`${request.approval_id}:${request.requested_at}:${request.kind}`} approval={request} agent={task?.harness === "codex" ? "Codex" : task ? "Claude Code" : "The agent"} /> : item && <>
          <h2 className="work-card-title">{item.title}</h2>
          {task && <p className="work-muted">{state.projects[task.project_id]?.name} · {task.title}</p>}
          <p className="work-caption">Received <Age at={item.created_at} suffix=" ago" /></p>
          {item.body && <p>{item.body}</p>}
          {destination.kind === "report_fix" && <ReportFixDecision item={item} task={destination.task} />}
          {task && destination.kind === "missing" && <Button onClick={() => ui.push({ screen: "receipt", taskId: task.task_id }, "work")}>Open current task receipt</Button>}
        </>}
      </div>
    </Screen>
  );
}

/** Shown as it came: the reporter's words are data, and nothing here starts work. */
function InboxReport({ item }: { item: AttentionItem }) {
  const online = useLink().state === "online";
  const canAct = useHost((s) => s.grants?.tasks === true);
  const send = useCommand();
  const open = item.resolved_at === null;
  return <div className="work-stack">
    {!online && <Notice tone="unknown">Connect to the computer to check this report’s current state.</Notice>}
    {!open && <Notice>You’ve put this report away.</Notice>}
    <h2 className="work-card-title">{item.title}</h2>
    <p className="work-caption">Received <Age at={item.created_at} suffix=" ago" /></p>
    {item.body && <p className="work-report-detail">{item.body}</p>}
    {item.report_id && <InboxTest key={item.report_id} reportId={item.report_id} body={item.body ?? ""} />}
    <p className="work-muted">The words are the reporter’s own. Putting this away doesn’t change their report.</p>
    {open && item.report_id && <ReportVerdicts reportId={item.report_id} />}
    {open && item.report_id && !item.body?.split("\n").some((line) =>
      line === "Look into it can't use it: its code can't be read safely enough to show an agent." ||
      line === "Look into it can't use it: it has a string longer than 256 bytes, which no agent is shown.") && <LookIntoReport reportId={item.report_id} />}
    {open && <Button aria-label="Put this report away" disabled={!online || !canAct} onClick={() => void send({ command: "acknowledge_attention", attention_id: item.attention_id }, { label: "Put problem report away" })}>Done</Button>}
  </div>;
}

/** The test and patch are reporter-supplied text, never executable UI or a command. */
function plainReporterText(text: string): string {
  return text.replace(/[\p{Cc}\p{Cf}]/gu, (char) => char === "\n" || char === "\t" ? char : "");
}

function InboxTest({ reportId, body }: { reportId: string; body: string }) {
  const { session } = useApp();
  const online = useLink().state === "online";
  const supported = useHost((s) => !!s.capabilities?.features.includes("report_inbox_code_v1"));
  const canRead = useHost((s) => s.grants?.admin === true);
  const [code, setCode] = useState<InboxReportCode | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [shown, setShown] = useState<"test" | "fix" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const loading = useRef(false);
  const testLine = body.split("\n").find((line) => line.startsWith("Their test: "));
  const authorLine = body.split("\n").find((line) => line.startsWith("Written by their model: "));
  const fixOffered = body.split("\n").includes("Their suggested fix is available.");
  if (!supported || !canRead || !testLine) return null;
  const show = (part: "test" | "fix") => {
    if (!supported || !canRead || !online) return;
    if (shown === part) { setShown(null); return; }
    setShown(part);
    if (loaded || loading.current) return;
    loading.current = true;
    setError(null);
    void session.query({ kind: "inbox_report_code", report_id: reportId }).then(
      (result) => { setCode(result.code); setLoaded(true); },
      (reason: unknown) => setError(reason instanceof Error ? reason.message : "The computer could not load this test."),
    ).finally(() => { loading.current = false; });
  };
  return <section className="work-stack" aria-label="Reporter’s test and suggested fix">
    <p className="work-muted">Reporter-supplied code, shown as text only.</p>
    <div className="work-actions">
      <Button disabled={!online} aria-expanded={shown === "test"} onClick={() => show("test")}>See test</Button>
      {fixOffered && (!loaded || code?.patch != null) && <Button disabled={!online} aria-expanded={shown === "fix"} onClick={() => show("fix")}>Their suggested fix</Button>}
    </div>
    {shown && !loaded && !error && <p className="work-muted">Loading the reporter’s code…</p>}
    {error && <Notice tone="stop">Couldn’t load the reporter’s code: {error}</Notice>}
    {shown && loaded && !code && <Notice>This report no longer has a test on this computer.</Notice>}
    {shown === "fix" && loaded && code && code.patch == null && <Notice>This report no longer has a suggested fix on this computer.</Notice>}
    {shown && code && (shown === "test" || code.patch != null) && <div className="work-stack" role="region" aria-label={shown === "test" ? "Reporter’s test code" : "Reporter’s suggested fix code"}>
      <p className="work-caption">{plainReporterText(authorLine ?? "Written by their model")}. Not checked: read it as text only.</p>
      {shown === "test" && <p className="work-caption">{plainReporterText(code.path)}</p>}
      {shown === "fix" && <p className="work-caption">Their suggestion only. Ophio never applies it.</p>}
      <pre className="work-reporter-code"><code>{plainReporterText(shown === "test" ? code.source : code.patch ?? "")}</code></pre>
    </div>}
  </section>;
}

const DECLINE_LABELS: Record<DeclineReason, string> = {
  works_as_documented: "Works as intended",
  feature_request: "A feature request",
  duplicate: "A duplicate",
  not_enough_information: "Not enough to go on",
  outside_ophio: "Not about Ophio",
};

/** The owner's word on a problem, for everyone who sent it. */
function ReportVerdicts({ reportId }: { reportId: string }) {
  const online = useLink().state === "online";
  const canAnswer = useHost((s) => s.grants?.admin === true);
  const supported = useHost((s) => !!s.capabilities?.features.includes(REPORT_INBOX_FEATURE));
  const send = useCommand();
  const [declining, setDeclining] = useState(false);
  if (!supported) return null;
  const disabled = !online || !canAnswer;
  const tell = (verdict: ReportVerdict, label: string) => {
    setDeclining(false);
    void send({ command: "set_report_verdict", report_id: reportId, verdict }, { label, success: "Reporters will see this" });
  };
  return <section className="work-stack" aria-label="Tell the reporters">
    <p className="work-caption">Everyone who sent this problem sees what you choose here.</p>
    <div className="work-actions">
      <Button disabled={disabled} onClick={() => tell({ verdict: "reproduced" }, "Mark problem reproduced")}>Reproduced</Button>
      <Button disabled={disabled} onClick={() => tell({ verdict: "not_reproduced" }, "Mark problem not reproduced")}>Couldn’t reproduce</Button>
      <Button disabled={disabled} onClick={() => tell({ verdict: "fixed" }, "Mark problem fixed")}>Fixed in this build</Button>
      <Button disabled={disabled} aria-expanded={declining} onClick={() => setDeclining((d) => !d)}>Decline…</Button>
    </div>
    {declining && <div className="work-stack" role="group" aria-label="Why decline">
      {(Object.keys(DECLINE_LABELS) as DeclineReason[]).map((reason) => <div key={reason}>
        <Button disabled={disabled} onClick={() => tell({ verdict: "declined", reason }, "Decline problem report")}>{DECLINE_LABELS[reason]}</Button>
        <p className="work-muted">They’ll read: “{declineWords[reason]}”</p>
      </div>)}
    </div>}
  </section>;
}

/** The fixer takes no report the owner hasn't chosen here. */
function LookIntoReport({ reportId }: { reportId: string }) {
  const online = useLink().state === "online";
  const canChoose = useHost((s) => s.grants?.admin === true);
  const fixer = useHost((s) => !!s.capabilities?.features.includes(REPORT_LOOK_INTO_FEATURE));
  const send = useCommand();
  if (!fixer) return null;
  return <section className="work-stack" aria-label="Report fixer">
    <p className="work-caption">The fixer takes only the reports you choose. A fix comes back here for you to merge.</p>
    <div className="work-actions">
      <Button disabled={!online || !canChoose} onClick={() => void send({ command: "look_into_report", report_id: reportId }, { label: "Choose report for the fixer", success: "Ophio will look into it" })}>Look into it</Button>
    </div>
  </section>;
}

function ReportFixDecision({ item, task }: { item: AttentionItem; task: Task }) {
  const { platform } = useApp();
  const online = useLink().state === "online";
  const canAct = useHost((s) => s.grants?.tasks === true);
  const { busy, run } = useGithubRun();
  const send = useCommand();
  const pr = task.github?.pr;
  const repo = task.github?.repo;
  if (!pr || !repo) return null;
  return <>
    <PrChip task={task} />
    <Button disabled={!online || !canAct || !!busy || pr.state !== "open"} onClick={() => void run({ action: "merge_pr", project_id: task.project_id, repo, number: pr.number, expected_head: pr.head, method: "merge" }, "Merging report fix", "Report fix merged", () => unlockFor(platform, "Merge report fix"))}>Merge</Button>
    <Button aria-label="Close report fix request" disabled={!online || !canAct || !!busy} onClick={() => void send({ command: "acknowledge_attention", attention_id: item.attention_id }, { label: "Close report fix request" })}>Close</Button>
  </>;
}

function AttentionApproval({ approval, agent }: { approval: Approval; agent: string }) {
  const { profile } = useApp();
  const canDecide = useHost((s) => s.grants?.approvals === true);
  const { card, saved } = useApprovalDecision(approval);
  return <>
    <ApprovalCard approval={approval} agent={agent} myDeviceId={profile.deviceId} canDecide={canDecide} {...card} />
    {!saved && <UnsavedAnswer />}
  </>;
}
