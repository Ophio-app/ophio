// SPDX-License-Identifier: Apache-2.0
import type { Command, Run, TargetControlView, Task } from "../../protocol/types";
import { uuidv7 } from "../../protocol/ids";
import type { SimHost } from "./host";
import { reportedModel } from "./scenario";


export class ComputerSim {
  private timers: ReturnType<typeof setTimeout>[] = [];
  private generation = 200;
  unverified = false;
  refuseNextStart = false;
  constructor(private host: SimHost) {}
  dispose(): void { this.timers.forEach(clearTimeout); }
  private view(view: TargetControlView): void {
    this.host.control = this.host.control.map((v) => v.target_id === view.target_id ? view : v);
    this.host.emit({ type: "control_changed", view });
  }
  private run(run: Run): void {
    this.host.runs = this.host.runs.map((r) => r.run_id === run.run_id ? run : r);
    this.host.emit({ type: "run_upserted", run });
  }
  private task(task: Task): void {
    this.host.tasks = this.host.tasks.map((t) => t.task_id === task.task_id ? task : t);
    this.host.emit({ type: "task_upserted", task });
  }
  start(task: Task, run: Run): void {
    const now = Date.now();
    // Codex uses the computer itself; Claude Code hands it to a specialist.
    const itself = task.harness === "codex";
    this.run({ ...run, state: "working", dispatched_at: now, acknowledged_at: now,
      actual_model: reportedModel(task.model), native_session: `sim-${run.run_id}`, children: itself ? [] : [{
        child_id: uuidv7(now), native_ref: `delegate-${run.run_id}`, label: "Computer specialist",
        state: "active", started_at: now, ended_at: null, provenance: "native",
      }] });
    this.task({ ...task, current_run: run.run_id, state: "working", revision: task.revision + 2,
      updated_at: now, status_line: itself ? "Observing the target" : "Computer specialist is observing the target" });
    const view = this.host.control.find((v) => v.target_id === task.target_id);
    if (!view) return;
    this.view({ ...view, input_state: "agent", private_step: null,
      lease: { lease_id: uuidv7(now), target_id: view.target_id, owner: { kind: "agent", run_id: run.run_id },
        generation: ++this.generation, boot_epoch: this.host.bootEpoch, state: "active", granted_at: now, renewal_window_ms: 5000 },
      model_observation: { observation_id: uuidv7(now), run_id: run.run_id, target_id: view.target_id, state: "allowed", delivered_count: 1 },
    });
  }
  privateStep(targetId: string, start: boolean): void {
    const view = this.host.control.find((v) => v.target_id === targetId);
    if (!view) return;
    if (!start) { this.view({ ...view, private_step: null }); return; }
    const requested = Date.now();
    const runId = view.model_observation?.run_id;
    this.view({ ...view, input_state: "paused", lease: view.lease ? { ...view.lease, state: "revoked" } : null,
      model_observation: view.model_observation ? { ...view.model_observation, state: "withdrawn" } : null,
      private_step: { requested_at: requested, input_stopped: "acknowledged", managed_screenshots_stopped: "acknowledged",
        uncovered_routes: ["The agent’s session pause has not been confirmed."], prior_observations_delivered: view.model_observation?.delivered_count ?? 0, ready: false },
    });
    this.timers.push(setTimeout(() => {
      const current = this.host.control.find((v) => v.target_id === targetId);
      if (current?.private_step?.requested_at !== requested) return;
      if (!this.unverified) {
        const run = this.host.runs.find((r) => r.run_id === runId);
        const task = this.host.tasks.find((t) => t.current_run === runId);
        if (run) this.run({ ...run, state: "interrupted", ended_at: Date.now(), holds_writer: false,
          children: run.children.map((c) => ({ ...c, state: "stopped", ended_at: Date.now() })) });
        if (task) {
          this.task({ ...task, state: "interrupted", revision: task.revision + 1, status_line: "Paused for a private step" });
          this.host.projects = this.host.projects.map((p) => {
            if (p.project_id !== task.project_id) return p;
            const project = { ...p, active_writer: null }; this.host.emit({ type: "project_upserted", project }); return project;
          });
        }
      }
      this.view({ ...current, private_step: { ...current.private_step,
        uncovered_routes: this.unverified ? ["A native process is still running. Privacy is unverified."] : [], ready: !this.unverified } });
    }, 500));
  }
}

export function validateComputerStart(host: SimHost, command: Extract<Command, { command: "create_task" }>): void {
  if (command.kind !== "computer_use") return;
  if (host.computer.refuseNextStart) { host.computer.refuseNextStart = false; throw { code: "os_permission_missing", message: "Desktop permission changed. Check the computer before starting.", detail: null }; }
  const target = host.targets.find((t) => t.target_id === command.target_id);
  const fail = (message: string): never => { throw { code: "conflict", message, detail: null }; };
  if (!target) fail("That target is no longer available. Review the computer’s targets again.");
  if (target && (target.input.state !== "available" || target.capture.state !== "available")) fail("Desktop permission changed. Check the computer before starting.");
  const view = host.control.find((v) => v.target_id === command.target_id);
  if (view?.private_step || view?.lease?.state === "active") fail("That target is already in use. Review control before starting.");
}
