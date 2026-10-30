// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { Square } from "lucide-react";
import { shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { Age } from "../../design/Age";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { availabilityText } from "../../design/Notice";
import { useStore } from "../../state/store";
import { useRoute } from "../../shell/Screen";
import type { ChildAgent, Run, Task } from "../../protocol/types";
import { childProvenance, earlierHelpers } from "./logic";
import { agentStops } from "./agentStops";
import { AgentStopReceipt } from "./AgentFacts";
import "./agents.css";

export const liveChild = (c: ChildAgent) => c.state === "active" || c.state === "unknown";
export function useAgentStop(key: string) {
  const { session } = useApp();
  const { active } = useRoute();
  const link = useLink();
  const store = agentStops(session);
  const intent = useStore(store, s => s[key]);
  useEffect(() => {
    if (!active) return;
    const off = session.state.subscribe(() => store.reconcile());
    store.reconcile();
    if (link.state === "online") void store.check(key);
    else store.disconnected(key);
    return off;
  }, [active, key, link.state, session, store]);
  return { store, intent, pending: !!intent && ["sending", "accepted", "unknown"].includes(intent.phase) };
}
export function AgentStopFeedback({ stopKey }: { stopKey: string }) {
  const link = useLink();
  const { store, intent } = useAgentStop(stopKey);
  if (!intent) return null;
  return intent.receipt ? <AgentStopReceipt receipt={intent.receipt} /> : <div className="work-stack">
    <p className="work-caption" role="status">{intent.detail}</p>
    {intent.phase !== "sending" && intent.phase !== "refused" && <Button disabled={link.state !== "online"} onClick={() => void store.check(stopKey)}>Check Stop result</Button>}
  </div>;
}
export function ChildRow({ child, run, task }: { child: ChildAgent; run: Run; task: Task }) {
  const link = useLink();
  const granted = useHost(s => s.grants?.tasks === true);
  const key = `${run.run_id}:${child.child_id}`;
  const { store, pending } = useAgentStop(key);
  const availability = child.stop ?? { state: "unsupported", reason: "This agent cannot stop this helper separately. Stop it in the agent’s terminal on the computer." };
  const canStop = availability.state === "available";
  return <li className="agent-child" data-child-id={child.child_id}>
    <div className="work-stack">
      <h4>{child.label}</h4>
      <p className="work-caption">{child.state === "active" ? link.state === "online" ? "Running" : "Last reported running" : child.state === "unknown" ? "State unknown" : child.state === "stopped" ? "Stopped" : child.state === "completed" ? "Completed" : "Failed"} · <Age at={child.ended_at ?? child.started_at} suffix=" ago" /></p>
      <p className="work-caption">{childProvenance(child, run)}</p>
      {liveChild(child) && (canStop ? <Button variant="stop-outline" className="agent-child-stop" icon={Square} aria-label={`${pending ? "Stop requested for" : "Stop helper ·"} ${child.label}`}
        disabled={!granted || link.state !== "online" || pending}
        onClick={() => { announce(`Stop requested for ${child.label}`); void store.request(key, { command: "stop_child", run_id: run.run_id, child_id: child.child_id }, task.revision, `Stop helper · ${child.label}`); }}>
        {pending ? "Stop requested" : "Stop helper"}
      </Button> : <p className="work-caption">Stop unavailable. {availabilityText(availability).detail}</p>)}
      <AgentStopFeedback stopKey={key} />
    </div>
  </li>;
}
export function AgentChildren({ task }: { task: Task }) {
  const { session } = useApp();
  const stops = useStore(agentStops(session), s => s);
  const [showEnded, setShowEnded] = useState(false);
  const runs = useHost(s => Object.values(s.runs).filter(r => r.task_id === task.task_id && r.children.length > 0), shallowEqual);
  const live = runs.flatMap(run => run.children.filter(c => liveChild(c) || !!stops[`${run.run_id}:${c.child_id}`]).map(child => ({ run, child })));
  const ended = runs.flatMap(run => run.children.filter(c => !liveChild(c) && !stops[`${run.run_id}:${c.child_id}`]).map(child => ({ run, child })));
  const earlier = runs.reduce((count, run) => count + (run.earlier_children ?? 0), 0);
  const endedCount = ended.length + earlier;
  if (!live.length && !endedCount) return null;
  return <section className="agent-children" aria-label="Known helpers">
    {live.length > 0 && <><h3 className="work-caption">Known helpers</h3>
      <ul>{live.map(({ run, child }) => <ChildRow key={`${run.run_id}:${child.child_id}`} child={child} run={run} task={task} />)}</ul></>}
    {endedCount > 0 && <details open={showEnded} onToggle={event => setShowEnded(event.currentTarget.open)}><summary>{endedCount} ended {endedCount === 1 ? "helper" : "helpers"}</summary>{showEnded && <>
      <ul>{ended.map(({ run, child }) => <ChildRow key={`${run.run_id}:${child.child_id}`} child={child} run={run} task={task} />)}</ul>
      {earlier > 0 && <p className="work-caption">{earlierHelpers(earlier)}</p>}
    </>}</details>}
  </section>;
}
