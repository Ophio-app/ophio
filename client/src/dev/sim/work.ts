// SPDX-License-Identifier: Apache-2.0
// Development fixtures and adverse-ordering drills for project review.
import { blockingAttentionCount } from "./attention";
import type { SimHost } from "./host";
import type {
  AnnotationBundle,
  ArtifactMeta,
  Command,
  CommandResult,
  QueueItem,
  Run,
  Task,
} from "../../protocol/types";
import { ids, harnesses } from "./scenario";
import { uuidv7 } from "../../protocol/ids";
import { resultFixture, linkReceiptPictures } from "./results";
import { seedShown } from "./shown";
import { inboxStore } from "../../screens/work/inbox";

export const workIds = {
  receipt: "0192f7a1-0000-7000-8000-000000002001",
  run: "0192f7a1-0000-7000-8000-000000002101",
  failed: "0192f7a1-0000-7000-8000-000000002002",
  unknown: "0192f7a1-0000-7000-8000-000000002003",
  waiting: "0192f7a1-0000-7000-8000-000000002004",
  blocked: "0192f7a1-0000-7000-8000-000000002005",
  ready: "0192f7a1-0000-7000-8000-000000002006",
  qWaiting: "0192f7a1-0000-7000-8000-000000002201",
  qBlocked: "0192f7a1-0000-7000-8000-000000002202",
  qReady: "0192f7a1-0000-7000-8000-000000002203",
  qStarted: "0192f7a1-0000-7000-8000-000000002204",
};
const fail = (code: string, message: string): never => {
  throw { code, message };
};
const device = { kind: "device" as const, device_id: ids.device };

export class WorkSim {
  #race = false;
  #images: Promise<void> | null = null;
  constructor(private readonly host: SimHost) {
    this.seed();
  }
  seed(): void {
    const host = this.host;
    const now = Date.now();
    const base = host.tasks[0];
    const baseRun = host.runs[0];
    if (
      !base ||
      !baseRun ||
      host.tasks.some((t) => t.task_id === workIds.receipt)
    )
      return;
    const task = (
      id: string,
      title: string,
      state: Task["state"],
      run: string | null,
    ): Task => ({
      ...base,
      task_id: id,
      title,
      state,
      revision: 1,
      current_run: run,
      updated_at: now - 180_000,
      created_at: now - 1_200_000,
      attention_count: 0,
      status_line: null,
      target_id: null,
    });
    const tasks = [
      task(
        workIds.receipt,
        "Refine the checkout page",
        "completed",
        workIds.run,
      ),
      task(
        workIds.failed,
        "Check the staging checkout",
        "failed",
        `${workIds.run}-failed`,
      ),
      task(
        workIds.unknown,
        "Recover the interrupted migration",
        "reconciliation_required",
        `${workIds.run}-unknown`,
      ),
      task(workIds.waiting, "Polish empty states", "accepted", null),
      task(workIds.blocked, "Document the new empty states", "accepted", null),
      task(workIds.ready, "Review keyboard navigation", "accepted", null),
    ];
    const result: Run = {
      ...baseRun,
      run_id: workIds.run,
      task_id: workIds.receipt,
      state: "completed",
      admitted_at: now - 1_200_000,
      ended_at: now - 180_000,
      holds_writer: false,
      requested_model: "claude-sonnet-5",
      actual_model: "claude-opus-5-5",
      requested_effort: "medium",
      actual_effort: "high",
      children: [
        {
          child_id: `${workIds.run}-child`,
          native_ref: "visual-check",
          label: "Check small-screen spacing",
          state: "active",
          started_at: now - 300_000,
          ended_at: null,
          provenance: "native",
        },
        {
          child_id: `${workIds.run}-observed`,
          native_ref: null,
          label: "Preview process",
          state: "unknown",
          started_at: now - 250_000,
          ended_at: null,
          provenance: "observed",
        },
      ],
      outcome: {
        summary:
          "I adjusted the heading spacing and kept the checkout button’s color. I couldn’t finish the card-row review. The small-screen helper is still checking its layout.",
        changed_files: [
          { path: "README.md", change: "modified", added: 12, removed: 4 },
          {
            path: "src/checkout.css",
            change: "modified",
            added: 18,
            removed: 6,
          },
        ],
        checks: [
          {
            name: "A: Heading spacing",
            command: "npm run test:layout -- heading",
            status: "passed",
            detail: "Spacing assertion passed at 390 px.",
            provenance: "observed",
          },
          {
            name: "B: Button contrast",
            command: "npm run test:contrast",
            status: "unknown",
            detail:
              "The agent reported a visual change; contrast was not measured.",
            provenance: "native",
          },
          {
            name: "Keyboard navigation",
            command: "npm run test:keyboard",
            status: "failed",
            detail: "Focus skipped the coupon field.",
            provenance: "observed",
          },
          {
            name: "C: Card row",
            command: null,
            status: "not_run",
            detail: "No after picture returned.",
            provenance: "native",
          },
        ],
        still_active: ["Small-screen layout review"],
        unknowns: ["Coupon field focus order has not been corrected."],
        usage: {
          input_tokens: 19000,
          output_tokens: 3000,
          duration_ms: 1_020_000,
          reported_cost: null,
        },
      },
    };
    const failed: Run = {
      ...result,
      run_id: `${workIds.run}-failed`,
      task_id: workIds.failed,
      state: "failed",
      children: [],
      outcome: {
        summary: "The staging server refused the connection.",
        changed_files: [],
        checks: [
          {
            name: "Staging smoke test",
            command: "npm run test:staging",
            status: "failed",
            detail: "Connection refused",
            provenance: "observed",
          },
        ],
        still_active: [],
        unknowns: ["Staging behavior has not been verified."],
        usage: null,
      },
    };
    const unknown: Run = {
      ...failed,
      run_id: `${workIds.run}-unknown`,
      task_id: workIds.unknown,
      state: "reconciliation_required",
      outcome: null,
    };
    resultFixture(tasks, result);
    host.tasks = [...host.tasks, ...tasks];
    host.runs = [...host.runs, result, failed, unknown];
    const qi = (
      id: string,
      taskId: string,
      position: number,
      state: QueueItem["state"],
      depends_on: string[] = [],
    ): QueueItem => ({
      item_id: id,
      task_id: taskId,
      position,
      state,
      depends_on,
      authorization: position === 1 ? "auto_start" : "ask_before_start",
      added_by: device,
      added_at: now - 360_000,
    });
    host.queue = {
      project_id: ids.pTide,
      revision: 5,
      paused: false,
      writer: ids.rFlaky,
      items: [
        qi(workIds.qStarted, ids.tFlaky, 0, {
          state: "started",
          run_id: ids.rFlaky,
        }),
        qi(workIds.qWaiting, workIds.waiting, 1, { state: "waiting" }),
        qi(
          workIds.qBlocked,
          workIds.blocked,
          2,
          { state: "blocked", reason: "Waiting for Polish empty states" },
          [workIds.qWaiting],
        ),
        qi(workIds.qReady, workIds.ready, 3, { state: "awaiting_start" }),
      ],
    };
    host.attention = [
      ...host.attention,
      ...tasks
        .filter((t) => ["completed", "failed", "reconciliation_required"].includes(t.state))
        .map((t) => ({
          attention_id: `${t.task_id}-attention`,
          kind:
            t.state === "failed"
              ? ("failure" as const)
              : t.state === "reconciliation_required"
                ? ("reconciliation_required" as const)
                : ("completed" as const),
          title: t.title,
          body: null,
          task_id: t.task_id,
          run_id: t.current_run,
          approval_id: null,
          created_at: t.updated_at,
          resolved_at: null,
          priority: t.state === "completed" ? 5 : 8,
        })),
    ];
    for (const task of host.tasks) task.attention_count = blockingAttentionCount(host.attention, task.task_id);
  }
  /** Called after subscription so artifact metadata is actually delivered. */
  async pictures(): Promise<void> {
    if (!this.#images) this.#images = this.#seedPictures();
    await this.#images;
    for (const a of this.artifacts())
      this.host.emit({ type: "artifact_created", artifact: a });
  }
  artifacts(): ArtifactMeta[] {
    const found = new Map<string, ArtifactMeta>();
    for (const event of this.host.journal)
      if (event.event.type === "artifact_created")
        found.set(event.event.artifact.artifact_id, event.event.artifact);
      else if (event.event.type === "artifact_deleted")
        found.delete(event.event.artifact_id);
    return [...found.values()];
  }
  async #seedPictures() {
    const run = this.host.runs.find((r) => r.run_id === workIds.run);
    if (!run) return;
    // Test environments without a 2D canvas get no seeded pictures.
    if (!document.createElement("canvas").getContext("2d")) return;
    const before = await png(false);
    const after = await png(true);
    const afterB = await png(true, 480);
    const add = async (blob: Blob, label: string, taskId: string | null) => {
      const bytes = new Uint8Array(await blob.arrayBuffer());
      const digest = new Uint8Array(
        await crypto.subtle.digest("SHA-256", bytes),
      );
      const sha = [...digest]
        .map((b) => b.toString(16).padStart(2, "0"))
        .join("");
      return this.host.addArtifact(bytes, "image/png", sha, label, taskId);
    };
    const original = await add(before, "Original checkout", null);
    const heading = await add(after, "Heading after", workIds.receipt);
    const button = await add(afterB, "Button after", workIds.receipt);
    const bundle: AnnotationBundle = {
      bundle_id: `${workIds.receipt}-bundle`,
      revision: 1,
      sources: [
        {
          key: "S1",
          artifact_id: original.artifact_id,
          sha256: original.sha256,
          media_type: "image/png",
          width: 390,
          height: 260,
          origin: { kind: "phone_import", original_name: "checkout.png" },
          captured_at: run.admitted_at - 1000,
          derived_from: null,
          derivation: null,
        },
      ],
      marks: ["A", "B", "C"].map((label, i) => ({
        mark_id: `mark-${label}`,
        label,
        source_key: "S1",
        shape: {
          kind: "rect",
          rect: { x: 0.05, y: 0.12 + i * 0.27, w: 0.9, h: 0.18 },
        },
        color: i,
        created_at: run.admitted_at - 800 + i,
        deleted: false,
      })),
      comments: [
        "Give the heading more breathing room.",
        "Keep the button color; improve contrast against its background.",
        "Give the card row more breathing room.",
      ].map((text, i) => ({
        comment_id: `comment-${i}`,
        text,
        mark_ids: [`mark-${["A", "B", "C"][i]}`],
        origin: { kind: "typed" },
        link_origins: ["manual"],
      })),
      steps: [],
      general_note: null,
      speech: null,
    };
    linkReceiptPictures(this.host, run, bundle, original, heading, button);
    this.host.emit({
      type: "timeline_appended",
      task_id: workIds.receipt,
      item: {
        seq: this.host.seq + 1,
        at: run.admitted_at - 500,
        run_id: run.run_id,
        item: {
          kind: "user_message",
          text: "Please refine these three parts of checkout.",
          bundle,
          delivery: "native_delivered",
        },
      },
    });
    const project = this.host.tasks.find((t) => t.task_id === workIds.receipt)?.project_id;
    if (project) await seedShown(this.host, run, project, afterB);
  }
  reusableDraft(taskId: string, name: string, slug: string): string {
    const task = this.host.tasks.find((t) => t.task_id === taskId);
    const run = this.host.runs
      .filter((r) => r.task_id === taskId)
      .sort((a, b) => b.admitted_at - a.admitted_at)[0];
    const outcome = run?.outcome;
    return [
      "---",
      `name: ${slug}`,
      "description: Draft for review from a task receipt.",
      "---",
      "",
      `# ${name}`,
      "",
      "> Draft for review. Review and adapt these instructions before use.",
      "",
      `Source task: ${task?.title ?? "Task details unavailable"}`,
      "",
      "## Reported result",
      "",
      outcome?.summary ?? "No result was reported.",
      "",
      "## Reported checks",
      "",
      ...(outcome?.checks.map(
        (c) =>
          `- ${c.name}: ${c.status.replaceAll("_", " ")} (${c.provenance === "observed" ? "run by the computer" : "reported, not verified"})`,
      ) ?? ["No checks reported."]),
      "",
      "## Unresolved work",
      "",
      ...(outcome?.unknowns.length || outcome?.still_active.length
        ? [...outcome.unknowns, ...outcome.still_active].map((s) => `- ${s}`)
        : [
            "No unresolved work was reported. Inspect the result before reuse.",
          ]),
      "",
    ].join("\n");
  }
  resolveElsewhere(id: string) {
    const old = this.host.approvals.find((a) => a.approval_id === id);
    if (!old) return;
    const approval = {
      ...old,
      state: "resolved_elsewhere" as const,
      revision: old.revision + 1,
      resolved_at: Date.now(),
      resolved_by: { kind: "local_owner" as const },
    };
    this.host.approvals = this.host.approvals.map((a) =>
      a.approval_id === id ? approval : a,
    );
    this.host.emit({ type: "approval_upserted", approval });
    this.host.attention = this.host.attention.map((a) => {
      if (a.approval_id !== id) return a;
      this.host.emit({
        type: "attention_resolved",
        attention_id: a.attention_id,
      });
      return { ...a, resolved_at: Date.now() };
    });
    const task = this.host.tasks.find(t => t.task_id === approval.task_id);
    if (task) {
      task.attention_count = blockingAttentionCount(this.host.attention, task.task_id);
      this.host.emit({ type: "task_upserted", task });
    }
  }
  admitNextBeforePause() {
    this.#race = true;
  }
  async addInbox() {
    const store = inboxStore("sim-studio");
    store.add({
      kind: "text",
      text: "Check the coupon field’s keyboard focus order.",
      name: "Checkout note",
      blobKey: null,
      mediaType: "text/plain",
      size: 51,
      sourceApp: "Notes",
      projectId: null,
    });
    await store.addBlob(await png(false), {
      kind: "image",
      name: "Checkout screenshot",
      sourceApp: "Browser",
      projectId: null,
    });
  }
  handle(
    c: Command,
    revision: number | null,
  ): { result: CommandResult | null } | null {
    if (!c.command.startsWith("queue_") || c.command === "queue_continuation") return null;
    const host = this.host;
    if (revision !== null && revision !== host.queue.revision)
      fail(
        "stale_revision",
        "The queue changed on the computer. Review its current order.",
      );
    if (c.command === "queue_pause" || c.command === "queue_resume") {
      if (c.project_id !== host.queue.project_id)
        fail("not_found", "This project has no queue.");
      let admitted: Task | undefined;
      if (c.command === "queue_pause" && this.#race) {
        this.#race = false;
        const item = host.queue.items.find(
          (i) =>
            i.state.state === "waiting" && i.authorization === "auto_start",
        );
        const task = item
          ? host.tasks.find((t) => t.task_id === item.task_id)
          : undefined;
        const base = host.runs[0];
        if (task && item && base) {
          const previous = host.runs.find(
            (r) => r.run_id === host.queue.writer,
          );
          if (previous) {
            const ended = {
              ...previous,
              state: "completed" as const,
              ended_at: Date.now(),
              holds_writer: false,
            };
            host.runs = host.runs.map((r) =>
              r.run_id === previous.run_id ? ended : r,
            );
            host.emit({ type: "run_upserted", run: ended });
            const t = host.tasks.find((t) => t.task_id === previous.task_id);
            if (t)
              this.#task({
                ...t,
                state: "completed",
                revision: t.revision + 1,
              });
          }
          const run: Run = {
            ...base,
            run_id: uuidv7(Date.now()),
            task_id: task.task_id,
            admitted_at: Date.now(),
            dispatched_at: Date.now(),
            acknowledged_at: Date.now(),
            state: "working",
            ended_at: null,
            children: [],
            outcome: null,
            holds_writer: true,
          };
          host.runs = [...host.runs, run];
          host.emit({ type: "run_upserted", run });
          admitted = {
            ...task,
            state: "working",
            current_run: run.run_id,
            revision: task.revision + 1,
            updated_at: Date.now(),
          };
          this.#task(admitted);
          host.queue = {
            ...host.queue,
            writer: run.run_id,
            items: host.queue.items.map((i) =>
              i.item_id === item.item_id
                ? { ...i, state: { state: "started", run_id: run.run_id } }
                : i,
            ),
          };
          host.projects = host.projects.map((p) => {
            if (p.project_id !== task.project_id) return p;
            const next = { ...p, active_writer: run.run_id };
            host.emit({ type: "project_upserted", project: next });
            return next;
          });
        }
      }
      host.queue = {
        ...host.queue,
        paused: c.command === "queue_pause",
        revision: host.queue.revision + 1,
      };
      host.emit({ type: "queue_changed", queue: host.queue });
      if (c.command === "queue_pause")
        host.emit({
          type: "intervention_recorded",
          task_id: admitted?.task_id ?? null,
          target_id: null,
          receipt: {
            kind: "pause_queue",
            requested_at: Date.now(),
            summary: admitted
              ? `Queue paused. “${admitted.title}” was admitted before the pause.`
              : "Queue paused. Already running work was not stopped.",
            facts: [
              {
                label: "Future queue admission paused",
                status: "done",
                detail: null,
              },
              ...(admitted
                ? [
                    {
                      label: admitted.title,
                      status: "still_active" as const,
                      detail:
                        "This run was admitted before the pause. Stop it separately.",
                    },
                  ]
                : []),
            ],
          },
        });
      return { result: null };
    }
    if (!("item_id" in c)) return null;
    const index = host.queue.items.findIndex((i) => i.item_id === c.item_id);
    const item = host.queue.items[index];
    if (!item) return fail("not_found", "That item has left Next up.");
    if (item.state.state === "started")
      return fail("conflict", "Already admitted. Stop the task separately.");
    let items = host.queue.items.map((i) => ({
      ...i,
      depends_on: [...i.depends_on],
    }));
    switch (c.command) {
      case "queue_move": {
        if (
          !Number.isInteger(c.position) ||
          c.position < 0 ||
          c.position >= items.length ||
          items[c.position]?.state.state === "started"
        )
          return fail(
            "invalid_request",
            "Choose an unstarted position in the queue.",
          );
        items.splice(index, 1);
        items.splice(c.position, 0, item);
        break;
      }
      case "queue_remove":
        if (items.some((i) => i.depends_on.includes(item.item_id)))
          return fail(
            "conflict",
            "Another item depends on this one. Remove that dependency first.",
          );
        items.splice(index, 1);
        { const task = host.tasks.find((t) => t.task_id === item.task_id);
          if (task) this.#task({ ...task, state: "archived", revision: task.revision + 1, updated_at: Date.now(), status_line: "Removed from Next up before starting" }); }
        break;
      case "queue_set_dependencies": {
        if (
          c.depends_on.includes(item.item_id) ||
          c.depends_on.some((id) => !items.some((i) => i.item_id === id))
        )
          return fail(
            "invalid_request",
            "A dependency is missing or points to this item.",
          );
        items[index] = { ...item, depends_on: c.depends_on };
        const visit = (id: string, path: Set<string>): boolean => {
          if (path.has(id)) return true;
          const next = new Set(path).add(id);
          return (items.find((i) => i.item_id === id)?.depends_on ?? []).some(
            (d) => visit(d, next),
          );
        };
        if (visit(item.item_id, new Set()))
          return fail("conflict", "These dependencies form a cycle.");
        if (
          c.depends_on.some(
            (id) =>
              (items.find((i) => i.item_id === id)?.position ?? Infinity) >=
              item.position,
          )
        )
          return fail(
            "invalid_request",
            "Choose dependencies earlier in the queue.",
          );
        break;
      }
      case "queue_set_authorization":
        items[index] = { ...item, authorization: c.authorization };
        break;
      default:
        return null;
    }
    items = items.map((i, position) => ({
      ...i,
      position,
      authorization:
        c.command !== "queue_set_authorization" && i.state.state !== "started"
          ? "ask_before_start"
          : i.authorization,
    }));
    host.queue = { ...host.queue, items, revision: host.queue.revision + 1 };
    host.emit({ type: "queue_changed", queue: host.queue });
    return { result: null };
  }
  #task(task: Task) {
    this.host.tasks = this.host.tasks.map((t) =>
      t.task_id === task.task_id ? task : t,
    );
    this.host.emit({ type: "task_upserted", task });
  }
  denyDefaults() {
    const changed = harnesses.map((h) =>
      h.kind === "codex"
        ? {
            ...h,
            capabilities: {
              ...h.capabilities,
              model_selection: {
                state: "unsupported" as const,
                reason: "This agent uses its configured model.",
              },
              effort_selection: {
                state: "unsupported" as const,
                reason: "This version does not expose effort selection.",
              },
            },
          }
        : h,
    );
    this.host.emit({ type: "harnesses_changed", harnesses: changed });
  }
}
async function png(after: boolean, width = 390): Promise<Blob> {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = 260;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas unavailable");
  const style = getComputedStyle(document.documentElement);
  const color = (name: string) => style.getPropertyValue(name).trim();
  ctx.fillStyle = color("--surface");
  ctx.fillRect(0, 0, width, 260);
  ctx.fillStyle = color("--ink");
  ctx.font = "bold 24px sans-serif";
  ctx.fillText("Your order", 24, after ? 52 : 35);
  ctx.font = "16px sans-serif";
  ctx.fillStyle = color("--ink-2");
  ctx.fillText("Ready when you are", 24, after ? 88 : 57);
  ctx.fillStyle = color("--accent");
  ctx.fillRect(24, 110, width - 48, 48);
  ctx.fillStyle = color("--on-accent");
  ctx.fillText("Continue to checkout", 42, 140);
  ctx.strokeStyle = color("--line-strong");
  ctx.strokeRect(24, 182, width - 48, 54);
  ctx.fillStyle = color("--ink-2");
  ctx.fillText("Order details", 42, 215);
  return new Promise((resolve, reject) =>
    canvas.toBlob(
      (b) =>
        b ? resolve(b) : reject(new Error("Could not create fixture image")),
      "image/png",
    ),
  );
}
