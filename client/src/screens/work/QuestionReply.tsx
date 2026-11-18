// SPDX-License-Identifier: Apache-2.0
import { useCallback, useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Markdown } from "../../design/Markdown";
import type { Approval } from "../../protocol/types";
import { projectFileLink } from "../talk/fileLink";
import { answerTime, answeredQuestion, questionReply } from "./answeredReply";

/** Read-only continuation of this answer, not another transcript or a way to
 * grant permission. Shared by the notification detail and Work's request sheet. */
export function QuestionReply({ approval, onOpenTask }: { approval: Approval; onOpenTask?: () => void }) {
  return answeredQuestion(approval) ? <SavedQuestionReply approval={approval} onOpenTask={onOpenTask} /> : null;
}

function SavedQuestionReply({ approval, onOpenTask }: { approval: Approval; onOpenTask?: () => void }) {
  const { session, profile, platform, ui } = useApp();
  // Match transcript navigation: project files stay inside Ophio's existing
  // file reader; other links use the platform's checked external opener.
  const root = useHost(s => s.libraryRoots.find(r => r.kind === "project" && r.project_id === s.tasks[approval.task_id]?.project_id));
  const openLink = useCallback((href: string) => {
    const path = projectFileLink(href, root);
    if (path && root) ui.push({ screen: "file", rootId: root.root_id, path, taskId: approval.task_id });
    else void platform.openExternal(href);
  }, [platform, root, ui, approval.task_id]);
  const online = useLink().state === "online";
  const allowed = useHost(s => s.grants?.tasks === true && s.grants?.approvals === true);
  const caughtUp = useHost(s => s.caughtUp);
  // Saved messages only: streamed text has no journal order against questions.
  const timeline = useHost(s => s.timelines[approval.task_id]);
  const approvals = useHost(s => s.approvals);
  const run = useHost(s => s.runs[approval.run_id]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [retry, setRetry] = useState(0);
  const at = answerTime(approval);
  useEffect(() => {
    if (!allowed || at === null) return;
    return session.watchTimeline(approval.task_id);
  }, [session, approval.task_id, allowed, at]);
  useEffect(() => {
    let cancelled = false;
    setError(null);
    setLoading(false);
    if (!allowed || !online || !caughtUp || at === null) return;
    setLoading(true);
    const load = async () => {
      await session.loadTimeline(approval.task_id);
      for (let page = 0; !cancelled; page++) {
        const current = session.state.get().timelines[approval.task_id];
        if (!current?.loaded) throw new Error("The conversation history is not available yet.");
        if (current.items.some(i => i.run_id === approval.run_id && i.item.kind === "approval" && i.item.approval_id === approval.approval_id) || !current.hasMore) return;
        if (page === 20) throw new Error("This question is further back in the conversation. Try again to load more history.");
        const before = current.items[0]?.seq;
        await session.loadTimeline(approval.task_id, true);
        if (session.state.get().timelines[approval.task_id]?.items[0]?.seq === before)
          throw new Error("The computer could not load the earlier conversation history.");
      }
    };
    void load().catch(e => { if (!cancelled) setError(describeError(e, profile.name)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [session, profile.name, approval.task_id, approval.run_id, approval.approval_id, allowed, online, caughtUp, at, retry]);

  const reply = questionReply(approval, timeline?.loaded ? timeline.items : [], approvals);
  const ended = !!run && run.task_id === approval.task_id && run.ended_at !== null;
  let status: string | null = null;
  if (!allowed) status = "This device was not granted permission to read task replies.";
  else if (at === null) status = "The computer hasn’t recorded a delivered answer for this question, so a reply can’t be matched here.";
  else if (!online) status = "Offline. Showing saved replies only; newer replies can’t be checked.";
  else if (!caughtUp) status = "Checking the conversation with the computer…";
  else if (error) status = `Couldn’t load the reply: ${error}`;
  else if (!reply.anchored) status = loading ? "Loading the reply’s conversation history…" : "This question isn’t in the available conversation history, so a reply can’t be matched safely.";
  else if (!reply.messages.length) status = reply.closed || ended ? "No saved reply before this conversation moved on." : "Waiting for agent’s reply…";
  return <section className="work-stack" aria-label="Agent’s reply">
    <h3 className="work-card-title">Agent’s reply</h3>
    {allowed && reply.messages.map(i => <div className="work-stack" key={i.seq}>
      <p className="work-caption"><Age at={i.at} clock />{i.item.kind === "agent_message" && i.item.partial ? " · Saved partial reply" : ""}</p>
      {i.item.kind === "agent_message" && <Markdown text={i.item.text} onLink={openLink} />}
    </div>)}
    {status && <p className="work-muted" role="status">{status}</p>}
    {allowed && reply.uncertain && <p className="work-muted">Some messages can’t be safely matched to this answer. Open the task to read them in context.</p>}
    {error && allowed && online && <Button size="sm" disabled={loading} onClick={() => setRetry(n => n + 1)}>Try again</Button>}
    {allowed && onOpenTask && <Button onClick={onOpenTask}>Open task</Button>}
  </section>;
}
