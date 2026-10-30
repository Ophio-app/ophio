// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import { SimHost } from "./host";
import { ids } from "./scenario";

let host: SimHost;
afterEach(() => { host.dispose(); vi.useRealTimers(); });

// As on the computer, only the agent's own completion starts what waits next.
it("starts the next task only after a run that completed on its own", async () => {
  vi.useFakeTimers();
  host = new SimHost();
  // Another task is changing files, so this one waits rather than starting at once.
  const busy = (writer: string | null) => host.projects = host.projects.map((p) => p.project_id === ids.pNotes ? { ...p, active_writer: writer } : p);
  busy("next-up-busy");
  const created = await host.command({ command_id: "next-up-1", issued_at: Date.now(), expires_at: null, expected_revision: null,
    body: { command: "create_task", project_id: ids.pNotes, kind: "coding", mode: "work", title: "Tidy the notes", prompt: "Tidy the notes", bundle: null,
      harness: "codex", model: null, effort: null, mission: null, target_id: null, placement: { mode: "queue", position: null, authorization: "auto_start" } } });
  if (created.result?.kind !== "task_created") throw Error(JSON.stringify(created));
  const waiting = created.result.task_id;
  const queue = host.talk.queue(ids.pNotes);
  queue.paused = false;
  host.talk.put(queue);
  expect(host.tasks.find((t) => t.task_id === waiting)?.current_run).toBeFalsy();
  busy(null);
  const reviewed = { resolution: "mark_completed" as const, at: Date.now(), by: { kind: "device" as const, device_id: ids.device } };
  for (const [state, human_resolution, starts] of [["failed", undefined, false], ["completed", reviewed, false], ["completed", undefined, true]] as const) {
    const run = { ...host.runs[0]!, run_id: `next-up-${state}-${starts}`, state, human_resolution };
    host.runs.push(run);
    host.talk.released(ids.pNotes, run.run_id);
    expect(host.tasks.find((t) => t.task_id === waiting)?.current_run != null, `${state} ${human_resolution ? "reviewed" : "native"}`).toBe(starts);
  }
});
