// SPDX-License-Identifier: Apache-2.0
import { taskActivity, taskStatus } from "./taskActivity";
import { Bot } from "lucide-react";
import { shallowEqual, useHost, useUi, useLink } from "../../app/context";
import { Markdown } from "../../design/Markdown";
import { Age } from "../../design/Age";
import { Empty, Section } from "../../design/ListRow";
import { CapabilityNote, Notice } from "../../design/Notice";
import { Button } from "../../design/Button";
import { Screen } from "../../shell/Screen";
import { isActiveTask, isTaskRunning, projectTasks } from "../../state/projection";
import type { Task } from "../../protocol/types";
import type { RouteProps } from "../types";
import {
  harnessLabel,
  latestRun,
  modelLabel,
  receiptHeadline,
  recentAgentTasks,
} from "./logic";
import {
  AttentionList,
  ConnectionNote,
  StopTask,
  TaskPermissionNote,
  useOpenTask,
} from "./shared";
import { AgentChildren, liveChild } from "./AgentChildren";
import { OutsideAgents } from "./OutsideAgents";
import { ReadinessCheck } from "./ReadinessCheck";
import "./work.css";
import "./agents.css";

export default function Agents(_: RouteProps<"agents">) {
  const projectId = useUi((s) => s.projectId);
  const unavailable = useHost(s => s.harnesses.filter(h => (h.kind === "claude_code" || h.kind === "codex") && h.capabilities.start_run.state !== "available"), shallowEqual);
  const hasCatalog = useHost(s => s.harnesses.some(h => h.kind === "claude_code" || h.kind === "codex"));
  const checkedAt = useHost(s => s.readiness?.checked_at);
  const recent = useHost((s) => projectId ? recentAgentTasks(projectTasks(s, projectId), s.runs) : [], shallowEqual);
  const tasks = useHost(
    (s) =>
      projectId
        ? projectTasks(s, projectId).filter(
            (t) =>
              Object.values(s.runs).some(r => r.task_id === t.task_id && r.children.some(liveChild)) ||
              (t.state !== "archived" &&
              ((t.state === "interrupted" && latestRun(t, s.runs)?.ended_at == null) || t.state === "reconciliation_required" ||
                !!latestRun(t, s.runs)?.outcome?.still_active.length ||
                (isActiveTask(t) && !!t.current_run))),
          )
        : [],
    shallowEqual,
  );
  const finished = recent.filter(t => !tasks.some(active => active.task_id === t.task_id));
  return (
    <Screen title="Live agents" className="work-screen agents-screen">
      <ConnectionNote />
      <TaskPermissionNote />
      {projectId && (
        <Section title="Needs you">
          <AttentionList projectId={projectId} />
        </Section>
      )}
      <Section title="Agent readiness" action={<ReadinessCheck />}>
        {unavailable.length === 0 && <p className="work-caption">{hasCatalog ? "No agent setup issues reported." : "The computer hasn’t reported agent readiness yet."}</p>}
        {unavailable.map(h => <CapabilityNote key={h.kind} what={h.display_name} availability={h.capabilities.start_run} />)}
        {checkedAt != null && <p className="work-caption">Checked <Age at={checkedAt} suffix=" ago" /></p>}
      </Section>
      <Section title="Running and unresolved">
        {tasks.length ? (
          tasks.map((t) => <AgentTask key={t.task_id} task={t} />)
        ) : (
          <Empty icon={Bot} title="No active agents">
            New tasks start from an explicit review, from New task in Tasks.
          </Empty>
        )}
      </Section>
      <Section title="Recently finished">
        {finished.length ? finished.map((t) => <AgentTask key={t.task_id} task={t} recent />)
          : <p className="work-muted">No finished agents have been reported for this project.</p>}
      </Section>
      <OutsideAgents />
    </Screen>
  );
}
function AgentTask({ task, recent = false }: { task: Task; recent?: boolean }) {
  const run = useHost((s) => latestRun(task, s.runs));
  const running = useHost((s) => isTaskRunning(s, task));
  const catalog = useHost((s) => s.harnesses);
  const status = useHost(s => taskStatus(task, run, s.control[task.target_id ?? ""]));
  const activity = useHost((s) => taskActivity(s, task));
  const open = useOpenTask();
  const online = useLink().state === "online";
  const harness = catalog.find((h) => h.kind === task.harness);
  const agent = harnessLabel(task.harness);
  const input = useHost((s) =>
    task.target_id ? s.control[task.target_id] : undefined,
  );
  return (
    <article className="work-card work-stack">
      <h3 className="work-card-title">{task.title}</h3>
      <p className="work-muted">
        {agent} · {modelLabel(run, catalog, undefined, agent)} ·{" "}
        <Age at={(recent ? run?.ended_at : activity?.at) ?? task.updated_at} suffix=" ago" />
      </p>
      <p>
        {running
          ? (activity?.text ??
            status ??
            "Waiting for a status from the agent")
          : receiptHeadline(task, run)}
        {!online && <> · last reported <Age at={activity?.at ?? task.updated_at} suffix=" ago" /></>}
      </p>
      {recent && run?.outcome?.summary && <Markdown className="work-muted" text={run.outcome.summary} />}
      {harness && harness.integration !== "managed" && (
        <Notice title="Observed only">The computer reports this agent’s activity. This connection does not manage its work or provide Stop.</Notice>
      )}
      {input?.input_state === "agent" && (
        <Notice title={online ? "Agent has desktop input" : "Agent last reported with desktop input"}>
          {online ? "This task can move the pointer and type on its target." : "The computer hasn’t confirmed its current input state."}
        </Notice>
      )}
      <AgentChildren task={task} />
      {harness && running && (
        <CapabilityNote
          what="Steering while running"
          availability={harness.capabilities.steer_running}
        />
      )}
      {harness && (
        <CapabilityNote
          what="Helper visibility"
          availability={harness.capabilities.child_agents}
        />
      )}
      <Button onClick={() => open(task)}>
        {isActiveTask(task) ? "Open in Chat" : "Open receipt"}
      </Button>
      {isActiveTask(task) &&
        (harness?.integration === "managed" &&
        harness.capabilities.interrupt.state === "available" ? (
          <StopTask task={task} />
        ) : (
          <Notice title="Stop unavailable">
            {harness
              ? "This agent is visible here, but this connection cannot stop it."
              : "The computer hasn’t reported this agent’s controls."}
          </Notice>
        ))}
    </article>
  );
}
