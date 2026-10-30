// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import { SimHost } from "./host";
import { ids } from "./scenario";
import { workIds } from "./work";
import type { Command, Event } from "../../protocol/types";

const hosts: SimHost[] = [];
afterEach(() => hosts.splice(0).forEach((h) => h.dispose()));
let n = 0;
async function send(h: SimHost, body: Command, expected: number | null = null) {
  return h.command({
    command_id: `settle-${++n}`,
    issued_at: Date.now(),
    expires_at: null,
    expected_revision: expected,
    body,
  });
}
function progress(h: SimHost, commandId: string) {
  return vi.waitFor(() => {
    const e = h.journal.find(
      (j) =>
        j.event.type === "command_progress" && j.event.command_id === commandId,
    )?.event as Extract<Event, { type: "command_progress" }> | undefined;
    if (!e) throw new Error("not settled yet");
    return e;
  });
}

// The companion settles these after admission (commands.rs `pending`); a
// sim that answered inline would hide screens that read the receipt.
// Capture settles the same way but needs a canvas, which jsdom lacks.
it("settles terminal, lease and reusable-draft results after admission", async () => {
  const h = new SimHost();
  hosts.push(h);
  const bodies: [Command, string][] = [
    [
      {
        command: "open_terminal",
        project_id: ids.pTide,
        harness: null,
        cols: 80,
        rows: 24,
      },
      "terminal_opened",
    ],
    [
      {
        command: "request_human_lease",
        target_id: ids.tgHdmi,
        geometry_revision: 1,
      },
      "lease_granted",
    ],
    [
      {
        command: "make_reusable",
        task_id: workIds.receipt,
        kind: "skill",
        name: "Fix flaky test",
      },
      "draft_created",
    ],
  ];
  for (const [body, kind] of bodies) {
    const receipt = await send(h, body);
    expect(receipt).toMatchObject({
      status: "accepted",
      result: null,
      error: null,
    });
    const done = await progress(h, receipt.command_id);
    expect(done).toMatchObject({ stage: "observed", result: { kind } });
    const status = await h.query({
      kind: "command_status",
      command_id: receipt.command_id,
    });
    expect(status).toMatchObject({
      stage: "observed",
      result: { kind },
      receipt: { result: null },
    });
  }
});

it("reports a refusal after admission as a failed stage", async () => {
  const h = new SimHost();
  hosts.push(h);
  const receipt = await send(h, {
    command: "request_human_lease",
    target_id: ids.tgHdmi,
    geometry_revision: 99,
  });
  expect(receipt.status).toBe("accepted");
  expect(await progress(h, receipt.command_id)).toMatchObject({
    stage: "failed",
    error: { code: "stale_revision" },
  });
});

it("refuses reconcile-by-retry like the computer", async () => {
  const h = new SimHost();
  hosts.push(h);
  const task = h.tasks.find((t) => t.task_id === workIds.unknown);
  expect(task?.state).toBe("reconciliation_required");
  const receipt = await send(
    h,
    {
      command: "reconcile_task",
      task_id: workIds.unknown,
      resolution: "retry_as_new_run",
    },
    task?.revision ?? null,
  );
  expect(receipt).toMatchObject({
    status: "rejected",
    error: { code: "conflict" },
  });
  expect(h.tasks.find((t) => t.task_id === workIds.unknown)?.state).toBe(
    "reconciliation_required",
  );
});
