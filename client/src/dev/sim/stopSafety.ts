// SPDX-License-Identifier: Apache-2.0
import { stopFrequency } from "../../screens/control/stops";
import type { Run, Task, TaskMode } from "../../protocol/types";
import { stillFinishing } from "../../state/projection";
import type { SimHost } from "./host";

/** Native Stop failures without a model call or a real process. */
export class StopSafetySim {
  #ignoring = new Map<string, boolean>();
  #ending = new Set<string>();
  #timers = new Set<ReturnType<typeof setInterval>>();
  constructor(private readonly host: SimHost) {}

  /** `activity=false` exercises the bounded wait without any new observation. */
  ignore(taskId: string, activity = true, ends = false): void {
    this.#ignoring.set(taskId, activity);
    if (ends) this.#ending.add(taskId); else this.#ending.delete(taskId);
  }
  endsProcess(taskId: string): boolean { return this.#ending.has(taskId); }

  handle(task: Task, run: Run, count = 1): boolean {
    const activity = this.#ignoring.get(task.task_id);
    if (activity === undefined) return false;
    const requestedAt = Date.now();
    // A turn that already ended keeps its outcome; Stop reaches only what it left.
    const stopping = stillFinishing(run) ? run : { ...run, state: "stopping" as const };
    this.host.runs = this.host.runs.map((r) => r.run_id === run.run_id ? stopping : r);
    this.host.emit({ type: "run_upserted", run: stopping });
    this.host.emit({ type: "intervention_recorded", task_id: task.task_id, target_id: task.target_id,
      receipt: { kind: "stop_task", requested_at: requestedAt, summary: "The agent hasn’t confirmed Stop. Check the task on the computer.",
        facts: [{ label: "Agent confirmed Stop", status: "unknown", detail: null }, { label: "Stop requests", status: "done", detail: `Stop sent ${stopFrequency(count)}` }] } });
    if (activity) {
      const timer = setInterval(() => { if (this.host.runs.find(r => r.run_id === run.run_id)?.ended_at) { clearInterval(timer); this.#timers.delete(timer); return; } this.host.live({ kind: "activity_status", task_id: task.task_id,
        run_id: run.run_id, text: "Still running after Stop" }); }, 1_000);
      this.#timers.add(timer);
    }
    return true;
  }

  /** The turn ended, but the computer hasn't confirmed that what it started
   * has ended, so Stop still reaches the run. */
  leaveUnconfirmed(taskId: string, mode: TaskMode): void {
    const task = this.host.tasks.find((t) => t.task_id === taskId);
    const run = this.host.runs.find((r) => r.run_id === task?.current_run);
    if (!task || !run) throw new Error("The Stop drill needs an existing run");
    const ended = { ...run, state: "completed" as const, ended_at: Date.now(), holds_writer: false, cleanup_pending: true };
    const finished = { ...task, mode, state: "completed" as const, revision: task.revision + 1, updated_at: Date.now(), status_line: null };
    this.host.runs = this.host.runs.map((r) => r.run_id === run.run_id ? ended : r);
    this.host.tasks = this.host.tasks.map((t) => t.task_id === taskId ? finished : t);
    this.host.emit({ type: "run_upserted", run: ended });
    this.host.emit({ type: "task_upserted", task: finished });
  }

  /** Same-run recovery after a companion restart; never invent a new run. */
  reportRunning(taskId: string): void {
    const task = this.host.tasks.find((t) => t.task_id === taskId);
    const run = this.host.runs.find((r) => r.run_id === task?.current_run);
    if (!task || !run) throw new Error("The Stop drill needs an existing run");
    const running = { ...run, state: "working" as const, ended_at: null };
    const working = { ...task, state: "working" as const, updated_at: Date.now(), status_line: "Running after restart" };
    this.host.runs = this.host.runs.map((r) => r.run_id === run.run_id ? running : r);
    this.host.tasks = this.host.tasks.map((t) => t.task_id === taskId ? working : t);
    this.host.emit({ type: "run_upserted", run: running });
    this.host.emit({ type: "task_upserted", task: working });
  }

  dispose(): void { for (const timer of this.#timers) clearInterval(timer); this.#timers.clear(); }
}
