// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { describeError, shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { TextArea } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import { announce } from "../../design/Announcer";
import type { Task } from "../../protocol/types";
import { readLocal, useLocalText } from "./local";
import { eligibleBriefAgents, portableBrief } from "./portable";
import { ConnectionNote } from "./shared";
import { writerHold, writerHoldText } from "../talk/intent";
import "./results.css";

export default function PortableBrief({ task, evidenceComplete = false, onClose }: { task: Task; evidenceComplete?: boolean; onClose(): void }) {
  const { ui, session, profile, platform } = useApp();
  const link = useLink();
  const projects = useHost((s) => Object.values(s.projects), shallowEqual);
  const project = projects.find((p) => p.project_id === task.project_id);
  const runs = useHost((s) => Object.values(s.runs).filter((r) => r.task_id === task.task_id), shallowEqual);
  const timeline = useHost((s) => s.timelines[task.task_id]);
  const approvals = useHost((s) => Object.values(s.approvals).filter((a) => a.task_id === task.task_id), shallowEqual);
  const artifacts = useHost((s) => Object.values(s.artifacts), shallowEqual);
  const harnesses = useHost((s) => s.harnesses);
  const allowed = useHost((s) => s.grants?.tasks === true);
  const key = `${profile.profileId}.portable.${task.task_id}`;
  const generate = () => portableBrief({ task, project, runs, artifacts, approvals, evidenceComplete, timeline: timeline?.items ?? [], historyComplete: !!timeline?.loaded && !timeline.hasMore });
  const [text, setText, saved] = useLocalText(key, generate());
  const [review, setReview] = useState(false);
  const [replace, setReplace] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedAs, setSavedAs] = useState<string | null>(null);
  const [projectId, setProjectId, projectSaved] = useLocalText(`${key}.destination`);
  const [harnessKind, setHarnessKind, agentSaved] = useLocalText(`${key}.agent`);
  const agents = eligibleBriefAgents(harnesses);
  const destination = projects.find((p) => p.project_id === projectId);
  const held = useHost((s) => writerHold(destination?.active_writer, s), shallowEqual);
  const agent = agents.find((h) => h.kind === harnessKind);
  const [occupied, setOccupied] = useState(false);
  useEffect(() => {
    if (readLocal<unknown>(key, null) === null) setText(text);
  }, [key]); // Preserve the generated version, including across phone restart.
  useEffect(() => {
    if (!review || link.state !== "online") return;
    void session.refreshHarnesses().catch((e: unknown) => setError(describeError(e, profile.name)));
  }, [review, link.state, session, profile.name]);
  const ready = link.state === "online" && allowed && destination && !destination.active_writer && agent && text.trim() && saved && projectSaved && agentSaved;
  return (
    <Sheet open size="full" title={review ? "New task from brief" : "Portable brief"} subtitle={task.title}
      className="work-sheet brief-sheet" onClose={onClose}
      footer={<div className="work-sheet-primary"><Button variant="primary" disabled={review ? !ready : !text.trim() || !saved} onClick={() => {
        if (!review) { setReview(true); return; }
        if (!ready || !destination || !agent) return;
        const existing = ui.draft(destination.project_id, null);
        if (existing && (existing.text.trim() || existing.annotationId || existing.attempt || existing.review)) {
          setOccupied(true); return;
        }
        ui.saveDraft(destination.project_id, null, {
          text, annotationId: null, agent: { harness: agent.kind, model: null, effort: null }, mode: "work",
          review: { title: `Follow-up: ${task.title}`.slice(0, 120), stopWhen: "", placement: "start_now", authorization: "auto_start" },
        });
        if (!ui.persistNow()) { setError("The new draft could not be saved on this device. The brief is still here."); return; }
        ui.selectTask(destination.project_id, null);
        onClose();
        ui.push({ screen: "review", projectId: destination.project_id, taskId: null, intent: "new_task" }, "talk");
        announce("New task draft saved. Review it before starting.");
      }}>{review ? "Review new task" : "Choose destination"}</Button></div>}>
      <div className="work-stack">
        <ConnectionNote />
        {!review ? <>
          <div className="work-actions">
            <Button pending={busy} disabled={!platform.saveToPhone || !text.trim()} onClick={async () => {
              if (!platform.saveToPhone) return;
              setBusy(true); setError(null);
              try {
                const result = await platform.saveToPhone(new Blob([text], { type: "text/markdown;charset=utf-8" }), `task-brief-${task.task_id}.md`);
                setSavedAs(result.savedAs); announce("Task brief saved.");
              } catch (e) { setError(describeError(e, profile.name)); }
              finally { setBusy(false); }
            }}>Save to device</Button>
            <Button onClick={() => setReplace(true)}>Regenerate…</Button>
          </div>
          <TextArea label="Brief Markdown" className="brief-editor" rows={8} maxRows={24} value={text} onChange={(v) => { setText(v); setSavedAs(null); }} />
          {saved && <StateLine phase="saved_on_phone" detail="This document has not been sent to an agent." />}
          {!saved && <Notice tone="stop">The edits could not be saved on this device. Keep this sheet open.</Notice>}
          {!platform.saveToPhone && <Notice>Document saving is unavailable on this device.</Notice>}
          {savedAs && <Notice title="Document saved">{savedAs}</Notice>}
          {replace && <Notice title="Replace the edited brief?">This replaces this document with the currently loaded task evidence.
            <div className="work-actions">
              <Button onClick={() => setReplace(false)}>Keep edits</Button>
              <Button onClick={() => { setText(generate()); setReplace(false); setSavedAs(null); }}>Replace edited brief</Button>
            </div>
          </Notice>}
        </> : <>
          <Button variant="quiet" onClick={() => setReview(false)}>Edit brief</Button>
          <p>This creates a new task with a fresh agent conversation. The source task stays in its history. The next screen reviews the exact prompt before Start.</p>
          <label className="field"><span className="field__label">Destination project</span>
            <select className="field__input" value={projectId} onChange={(e) => { setProjectId(e.target.value); setOccupied(false); }}>
              <option value="">Choose project</option>
              {projects.map((p) => <option key={p.project_id} value={p.project_id} disabled={!!p.active_writer}>{p.name}{p.active_writer ? " — busy" : ""}</option>)}
            </select>
          </label>
          <label className="field"><span className="field__label">Destination agent</span>
            <select className="field__input" value={harnessKind} onChange={(e) => setHarnessKind(e.target.value)}>
              <option value="">Choose agent</option>
              {agents.map((h) => <option key={h.kind} value={h.kind}>{h.display_name}</option>)}
            </select>
          </label>
          <p className="work-caption">The model and effort will be shown in the final review. The computer checks availability and the project’s writer again at Start.</p>
          {!agents.length && <Notice>No agent has reported that it can start Work on this computer.</Notice>}
          {destination && held && <Notice>{writerHoldText(held, destination.name)}</Notice>}
          {!allowed && <Notice>This device does not have permission to start tasks.</Notice>}
          {occupied && <Notice title="A new-task draft is already waiting">Open and finish that draft first, or choose another project. Your portable brief is saved here.
            <Button onClick={() => { if (destination) { onClose(); ui.openTask(destination.project_id, null); } }}>Open saved draft</Button>
          </Notice>}
        </>}
        {error && <Notice tone="unknown">{error}</Notice>}
      </div>
    </Sheet>
  );
}
