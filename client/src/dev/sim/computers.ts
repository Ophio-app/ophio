// SPDX-License-Identifier: Apache-2.0
//
// The other simulated computers' projects and work: a spare desktop and a
// laptop, each with its own Home, and one project the main computer has too,
// as a checkout on two computers is. Each has a little work of its own, so a
// view across computers has tasks, requests and to-dos from more than one.

import type { Approval, AttentionItem, Checklist, ChecklistItem, Project, Run, Task } from "../../protocol/types";
import { blockingAttentionCount } from "./attention";
import { ids, type TimelineSeed } from "./scenario";

const MIN = 60_000;
const DAY = 24 * 60 * MIN;

function project(id: string, name: string, path: string, at: number, extra: Partial<Project> = {}): Project {
  return { project_id: id, name, host_path: path, default_harness: null, default_model: null, default_effort: null, active_writer: null, vcs: null, created_at: at, ...extra };
}

/** What the simulated computer `name` holds, by its name. */
export function otherProjects(name: "spare" | "laptop", now: number): Project[] {
  if (name === "spare") return [
    project("0192f7a1-0000-7000-8000-000000000201", "Home", "/home/sam", now - 200 * DAY, { home: true }),
    project("0192f7a1-0000-7000-8000-000000000202", "field-notes", "/home/sam/src/field-notes", now - 60 * DAY,
      { vcs: { kind: "git", branch: "draft/winter", head: "41b0c9e", dirty_files: 1 } }),
    project("0192f7a1-0000-7000-8000-000000000203", "render-farm", "/home/sam/src/render-farm", now - 2 * DAY,
      { vcs: { kind: "git", branch: "main", head: "9f2d7a1", dirty_files: 0 } }),
  ];
  return [
    project("0192f7a1-0000-7000-8000-000000000301", "Home", "/Users/sam", now - 30 * DAY, { home: true }),
    project("0192f7a1-0000-7000-8000-000000000302", "travel-journal", "/Users/sam/Code/travel-journal", now - 7 * DAY,
      { vcs: { kind: "git", branch: "main", head: "c3e81f0", dirty_files: 0 } }),
  ];
}

/** Ids for the other computers' work, in ranges no fixture of studio's uses:
 * `52xx` on spare and `53xx` on laptop. */
export const otherIds = {
  spare: {
    tRender: "0192f7a1-0000-7000-8000-000000005201",
    tArchive: "0192f7a1-0000-7000-8000-000000005202",
    tChapters: "0192f7a1-0000-7000-8000-000000005203",
    rRender: "0192f7a1-0000-7000-8000-000000005211",
    rArchive: "0192f7a1-0000-7000-8000-000000005212",
    rChapters: "0192f7a1-0000-7000-8000-000000005213",
    aArchive: "0192f7a1-0000-7000-8000-000000005221",
    aChapters: "0192f7a1-0000-7000-8000-000000005222",
    atArchive: "0192f7a1-0000-7000-8000-000000005231",
    atChapters: "0192f7a1-0000-7000-8000-000000005232",
  },
  laptop: {
    tLisbon: "0192f7a1-0000-7000-8000-000000005301",
    rLisbon: "0192f7a1-0000-7000-8000-000000005311",
    atLisbon: "0192f7a1-0000-7000-8000-000000005331",
  },
} as const;

/** What the other computer's own work is, as `SimHost` keeps it. */
export type OtherWork = {
  tasks: Task[];
  runs: Run[];
  approvals: Approval[];
  attention: AttentionItem[];
  checklists: Checklist[];
  /** Each task's transcript, oldest first, as `scenario.timelines` gives studio's. */
  timelines: Record<string, TimelineSeed[]>;
};

const device = { kind: "device", device_id: ids.device } as const;

function task(t: Pick<Task, "task_id" | "project_id" | "title" | "state" | "current_run" | "status_line" | "created_at" | "updated_at"> & Partial<Task>): Task {
  return { kind: "coding", revision: 3, harness: "claude_code", model: "claude-opus-5-5", effort: "high", created_by: device,
    target_id: null, mission: null, attention_count: 0, ...t };
}

function run(r: Pick<Run, "run_id" | "task_id" | "state" | "admitted_at"> & Partial<Run>): Run {
  return { harness: "claude_code", requested_model: "claude-opus-5-5", actual_model: "claude-opus-5-5", requested_effort: "high",
    actual_effort: "high", account_label: "Personal", native_session: r.run_id.slice(-8), dispatched_at: r.admitted_at + 300,
    acknowledged_at: r.admitted_at + 1500, ended_at: null, children: [], outcome: null, holds_writer: false, force_stoppable: true, ...r };
}

function item(i: Pick<ChecklistItem, "item_id" | "title" | "steps" | "added_at"> & Partial<ChecklistItem>): ChecklistItem {
  return { priority: "medium", changed_at: i.added_at, ...i };
}

const user = (text: string) => ({ kind: "user_message", text, bundle: null, delivery: "native_delivered" } as const);

/** The work of the simulated computer `name`: a few tasks, what they ask of
 * the person, and a checklist, so every computer has something to show. Built
 * fresh on each call: the simulated computer and the phone must not share it. */
export function otherWork(name: "spare" | "laptop", now: number): OtherWork {
  const [, second, third] = otherProjects(name, now);
  if (name === "spare") {
    const s = otherIds.spare, notes = second!.project_id, farm = third!.project_id;
    const archive = "tar -czf archive/renders-october.tar.gz out/october";
    const question = {
      header: "Order",
      question: "How should the chapters be ordered?",
      options: [
        { label: "By date", description: "From the first snowfall to the thaw" },
        { label: "By place", description: "One chapter for each valley" },
      ],
    };
    const approvals: Approval[] = [
      {
        approval_id: s.aArchive, run_id: s.rArchive, task_id: s.tArchive, project_id: farm, native_request_id: "toolu_01Rf",
        kind: "command", title: "Run a shell command", headline: "Pack October's renders into one archive",
        detail: `${archive}\nPack October's renders into one archive`,
        detail_lines: [{ text: archive, code: true }, { text: "Pack October's renders into one archive" }],
        raw_request: JSON.stringify({ command: archive, description: "Pack October's renders into one archive" }),
        scope: "This request once", shortened: false, actions: ["allow", "deny"],
        revision: 1, state: "pending", requested_at: now - 3 * MIN, expires_at: null, resolved_by: null, resolved_at: null,
      },
      {
        approval_id: s.aChapters, run_id: s.rChapters, task_id: s.tChapters, project_id: notes, native_request_id: "toolu_01Fn",
        kind: "question", title: "Answer a question",
        detail: `Order: ${question.question}\nBy date: ${question.options[0]!.description}\nBy place: ${question.options[1]!.description}`,
        detail_lines: [
          { label: "Order", text: question.question },
          { label: "By date", text: question.options[0]!.description },
          { label: "By place", text: question.options[1]!.description },
        ],
        questions: [question],
        raw_request: JSON.stringify({ questions: [{ question: question.question, header: question.header, multiSelect: false, options: question.options }] }),
        scope: "Only your answer; nothing runs", shortened: false, actions: ["answer", "deny"],
        revision: 1, state: "pending", requested_at: now - 11 * MIN, expires_at: null, resolved_by: null, resolved_at: null,
      },
    ];
    // As the computer raises them: a question reads as one, not as an approval.
    const attention: AttentionItem[] = [
      { attention_id: s.atArchive, kind: "approval", title: "Archive last month's renders", body: archive, task_id: s.tArchive,
        run_id: s.rArchive, approval_id: s.aArchive, created_at: now - 3 * MIN, resolved_at: null, priority: 10 },
      { attention_id: s.atChapters, kind: "question", title: "Answer a question", body: null, task_id: s.tChapters,
        run_id: s.rChapters, approval_id: s.aChapters, created_at: now - 11 * MIN, resolved_at: null, priority: 10 },
    ];
    const tasks = [
      task({ task_id: s.tRender, project_id: farm, title: "Render the winter tiles at 4K", state: "working", current_run: s.rRender,
        status_line: "Rendering tile 38 of 120", created_at: now - 25 * MIN, updated_at: now - 30_000 }),
      task({ task_id: s.tArchive, project_id: farm, title: "Archive last month's renders", state: "awaiting_decision", current_run: s.rArchive,
        status_line: "Waiting for approval to pack the October renders", created_at: now - 9 * MIN, updated_at: now - 3 * MIN }),
      task({ task_id: s.tChapters, project_id: notes, title: "Sort the winter notes into chapters", state: "awaiting_decision", current_run: s.rChapters,
        status_line: "Asking how to order the chapters", harness: "codex", model: "gpt-6-astra", created_at: now - 40 * MIN, updated_at: now - 11 * MIN }),
    ].map((t) => ({ ...t, attention_count: blockingAttentionCount(attention, t.task_id) }));
    return {
      tasks,
      runs: [
        run({ run_id: s.rRender, task_id: s.tRender, state: "working", admitted_at: now - 25 * MIN }),
        run({ run_id: s.rArchive, task_id: s.tArchive, state: "awaiting_approval", admitted_at: now - 9 * MIN }),
        run({ run_id: s.rChapters, task_id: s.tChapters, state: "awaiting_approval", admitted_at: now - 40 * MIN, harness: "codex",
          requested_model: "gpt-6-astra", actual_model: "gpt-6-astra" }),
      ],
      approvals,
      attention,
      checklists: [{
        project_id: farm, title: "Get the render farm going", summary: "One sign-in from you, then the agent can send the tiles out.",
        task_id: s.tRender, created_at: now - 20 * MIN, updated_at: now - 20 * MIN, revision: 1,
        sections: [{ section_id: "access", title: "Access", items: [
          item({ item_id: "render-sign-in", title: "Sign in to the render service", task_id: s.tRender, priority: "high", minutes: 3,
            why: "The agent sends the 4K tiles to the farm under your account.", waiting: "Sends the 4K tiles to the render farm",
            added_at: now - 20 * MIN,
            steps: [{ step_id: "sign-in", text: "On the computer, run this and sign in when the browser opens.", copy: "farmctl login" }] }),
        ] }],
      }],
      timelines: {
        [s.tRender]: [
          { runId: null, ago: 25 * MIN, body: user("Render the winter tile set at 4K and check every tile for seams.") },
          { runId: s.rRender, ago: 25 * MIN - 1500, body: { kind: "run_started", run_id: s.rRender } },
          { runId: s.rRender, ago: 30_000, body: { kind: "activity", tool: "Bash", summary: "render --set winter --size 4k", status: "running", detail: null } },
        ],
        [s.tArchive]: [
          { runId: null, ago: 9 * MIN, body: user("Pack October's renders into one archive so the disk has room again.") },
          { runId: s.rArchive, ago: 9 * MIN - 1500, body: { kind: "run_started", run_id: s.rArchive } },
          { runId: s.rArchive, ago: 3 * MIN, body: { kind: "approval", approval_id: s.aArchive } },
        ],
        [s.tChapters]: [
          { runId: null, ago: 40 * MIN, body: user("Sort this winter's notes into chapters.") },
          { runId: s.rChapters, ago: 40 * MIN - 1500, body: { kind: "run_started", run_id: s.rChapters } },
          { runId: s.rChapters, ago: 11 * MIN, body: { kind: "approval", approval_id: s.aChapters } },
        ],
      },
    };
  }
  const l = otherIds.laptop, journal = second!.project_id;
  const attention: AttentionItem[] = [
    { attention_id: l.atLisbon, kind: "completed", title: "Write up the Lisbon trip", body: null, task_id: l.tLisbon,
      run_id: l.rLisbon, approval_id: null, created_at: now - 34 * MIN, resolved_at: null, priority: 5 },
  ];
  return {
    tasks: [task({ task_id: l.tLisbon, project_id: journal, title: "Write up the Lisbon trip", state: "completed", current_run: null,
      status_line: null, revision: 9, created_at: now - 80 * MIN, updated_at: now - 34 * MIN })],
    runs: [run({
      run_id: l.rLisbon, task_id: l.tLisbon, state: "completed", admitted_at: now - 80 * MIN, ended_at: now - 34 * MIN,
      outcome: {
        summary: "Wrote a post about the Lisbon trip from the photo captions, with one section for each day and the photos in order.",
        changed_files: [
          { path: "posts/2026-10-lisbon.md", change: "added", added: 142, removed: 0 },
          { path: "posts/index.md", change: "modified", added: 1, removed: 0 },
        ],
        checks: [{ name: "Site build", command: "npm run build", status: "passed", detail: null, provenance: "native" }],
        still_active: [],
        unknowns: ["Which photo should be the cover"],
        usage: { input_tokens: 61_400, output_tokens: 5_210, duration_ms: 46 * MIN, reported_cost: null },
      },
    })],
    approvals: [],
    attention,
    checklists: [{
      project_id: journal, title: "Publish the Lisbon post", summary: "One choice from you before the post goes up.",
      task_id: l.tLisbon, created_at: now - 34 * MIN, updated_at: now - 34 * MIN, revision: 1,
      sections: [{ section_id: "publish", title: "Before it goes up", items: [
        item({ item_id: "lisbon-cover", title: "Choose the cover photo for the Lisbon post", task_id: l.tLisbon, minutes: 2,
          added_at: now - 34 * MIN,
          steps: [{ step_id: "pick", text: "Open `posts/2026-10-lisbon.md` and move the photo you like best to the top." }] }),
      ] }],
    }],
    timelines: {
      [l.tLisbon]: [
        { runId: null, ago: 80 * MIN, body: user("Write up the Lisbon trip from the photo captions in photos/lisbon.") },
        { runId: l.rLisbon, ago: 80 * MIN - 1500, body: { kind: "run_started", run_id: l.rLisbon } },
        { runId: l.rLisbon, ago: 35 * MIN, body: { kind: "agent_message", text: "The post is written, one section a day. I left the cover photo for you to choose.", partial: false } },
        { runId: l.rLisbon, ago: 34 * MIN, body: { kind: "run_ended", run_id: l.rLisbon, state: "completed" } },
      ],
    },
  };
}
