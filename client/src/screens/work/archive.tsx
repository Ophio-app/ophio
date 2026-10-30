// SPDX-License-Identifier: Apache-2.0
//
// Archiving from where finished tasks are listed and read: a task's receipt,
// Talk's conversations, and Work › Finished. Archived tasks move to Work ›
// Archived, and conversations to Talk › Archived, with their history, to read.
// Where the computer can bring a task back, archiving goes ahead at once with
// Undo; elsewhere it says what it does first, unless its warning is turned
// off. One still working is never archived.

import { useState, type ReactNode } from "react";
import { describeError, useApp, useHost } from "../../app/context";
import { useWarning } from "../../app/warnings";
import { Button } from "../../design/Button";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import { settle } from "../../host/expected";
import { CommandError, type HostSession } from "../../host/session";
import type { Task, TaskState } from "../../protocol/types";
import { type HostState, isActiveTask } from "../../state/projection";
import { useCanWork } from "./shared";

export const tasksWord = (n: number) => (n === 1 ? "1 task" : `${n} tasks`);

/** The computer can bring an archived task back, so archiving can be undone. */
export const UNARCHIVE_FEATURE = "unarchive_task_v1";

/** Whether a task can be archived: it is known, finished and not archived yet. */
const archivable = (task: Task | undefined): task is Task => !!task && task.state !== "archived" && !isActiveTask(task);

/** Shows the tasks as archived, ahead of the computer, while each still can be. */
function asArchived(ids: ReadonlySet<string>) {
  return (s: HostState): HostState => {
    let tasks = s.tasks;
    for (const id of ids) {
      const task = tasks[id];
      if (!archivable(task)) continue;
      if (tasks === s.tasks) tasks = { ...s.tasks };
      tasks[id] = { ...task, state: "archived" };
    }
    return tasks === s.tasks ? s : { ...s, tasks };
  };
}

/** Shows the tasks as they were before they were archived, ahead of the computer. */
function asRestored(was: ReadonlyMap<string, TaskState>) {
  return (s: HostState): HostState => {
    let tasks = s.tasks;
    for (const [id, state] of was) {
      const task = tasks[id];
      if (task?.state !== "archived") continue;
      if (tasks === s.tasks) tasks = { ...s.tasks };
      tasks[id] = { ...task, state };
    }
    return tasks === s.tasks ? s : { ...s, tasks };
  };
}

/** Archives one task, reading it again when it changed on the computer
 * meanwhile. False when it is working, or already archived. */
async function archiveOne(session: HostSession, task_id: string): Promise<boolean> {
  for (let attempt = 1; ; attempt++) {
    const task = session.state.base().tasks[task_id];
    if (!archivable(task)) return false;
    try {
      await commandResult(session, await session.command({ command: "archive_task", task_id }, { label: `Archive ${task.title}`, expectedRevision: task.revision }));
      return true;
    } catch (e) {
      if (attempt < 3 && e instanceof CommandError && e.error.code === "stale_revision") continue;
      throw e;
    }
  }
}

/** Some of the tasks were not archived: the first refusal, and how many were. */
export class ArchiveError extends Error {
  constructor(readonly cause: unknown, readonly archived: number, readonly refused: number) {
    super("Not archived");
  }
}

/** Tasks being archived: `sent` once the computer has answered for each,
 * and `undo` to bring back the ones it archived. */
export type Archiving = { sent: Promise<number>; undo(): Promise<number> };

/**
 * Archives the tasks all at once. Each shows as archived as soon as it is
 * asked for, and goes back where it was if the computer refuses it. One that
 * is working, or already archived, is left. `sent` throws an `ArchiveError`
 * once every task is answered if any was refused.
 *
 * `undo` shows them where they were at once, waits for the archiving to be
 * answered, then brings back each the computer archived. It throws an
 * `UnarchiveError` if any stays archived, which shows it archived again.
 */
export function archiveTasks(session: HostSession, ids: string[]): Archiving {
  const was = new Map<string, TaskState>();
  for (const id of ids) {
    const task = session.state.base().tasks[id];
    if (archivable(task)) was.set(id, task.state);
  }
  const shown = new Set(was.keys());
  let undone = false;
  let drop = session.state.expect(asArchived(shown));
  const sent = (async () => {
    const results = await Promise.allSettled([...ids].map(async (id) => {
      try { return await archiveOne(session, id); } catch (e) {
        shown.delete(id);
        if (!undone) {
          drop();
          drop = session.state.expect(asArchived(shown));
        }
        throw e;
      }
    }));
    if (!undone) settle(session.state, () => drop(), (s) => [...shown].every((id) => !archivable(s.tasks[id])));
    const archived = results.filter((r) => r.status === "fulfilled" && r.value).length;
    const refused = results.filter((r): r is PromiseRejectedResult => r.status === "rejected");
    if (refused.length) throw new ArchiveError(refused[0]!.reason, archived, refused.length);
    return archived;
  })();
  const undo = async () => {
    if (undone) return 0;
    undone = true;
    drop();
    const back = new Map(was);
    let restore = session.state.expect(asRestored(back));
    await sent.catch(() => undefined);
    // Only what the computer archived comes back; a refused one never left.
    for (const id of [...back.keys()]) if (session.state.base().tasks[id]?.state !== "archived") back.delete(id);
    const results = await Promise.allSettled([...back.keys()].map(async (id) => {
      try {
        await commandResult(session, await session.command({ command: "unarchive_task", task_id: id }, { label: `Bring back ${session.state.base().tasks[id]?.title ?? "task"}` }));
      } catch (e) {
        back.delete(id);
        restore();
        restore = session.state.expect(asRestored(back));
        throw e;
      }
    }));
    settle(session.state, () => restore(), (s) => [...back.keys()].every((id) => s.tasks[id]?.state !== "archived"));
    const refused = results.filter((r): r is PromiseRejectedResult => r.status === "rejected");
    if (refused.length) throw new UnarchiveError(refused[0]!.reason, refused.length);
    return back.size;
  };
  return { sent, undo };
}

/** Some of the tasks stayed archived after Undo: the first refusal, and how many. */
export class UnarchiveError extends Error {
  constructor(readonly cause: unknown, readonly refused: number) {
    super("Not brought back");
  }
}

/** What went wrong, and how many went back where they were. */
export function describeArchiveError(e: unknown, computer: string): string {
  if (e instanceof UnarchiveError) return `${e.refused === 1 ? "1 task stays" : `${e.refused} tasks stay`} in Archived. ${describeError(e.cause, computer)}`;
  if (!(e instanceof ArchiveError)) return describeError(e, computer);
  const why = describeError(e.cause, computer);
  return e.archived ? `${tasksWord(e.refused)} not archived and back in the list. ${why}` : `Not archived. ${why}`;
}

/**
 * `ask(tasks, then)` archives them, then calls `then`: at once with Undo
 * where the computer can bring them back, otherwise after the warning, or at
 * once when it is off. They leave their list at once, and `then` runs
 * straight away; any the computer refuses come back, with why. `sheet` is
 * the warning, to render where it's used.
 */
export function useArchive(): { ask: (tasks: Task[], then?: () => void) => void; sheet: ReactNode; can: boolean } {
  const { session, profile } = useApp();
  const can = useCanWork();
  const one = useWarning("task.archive");
  const many = useWarning("tasks.archive_all");
  const undoable = useHost((s) => !!s.capabilities?.features.includes(UNARCHIVE_FEATURE));
  const [asking, setAsking] = useState<{ tasks: Task[]; then?: () => void } | null>(null);
  const failed = (e: unknown) => toast(describeArchiveError(e, profile.name), { tone: "stop" });
  const go = (tasks: Task[], then?: () => void) => {
    setAsking(null);
    const archiving = archiveTasks(session, tasks.map((t) => t.task_id));
    if (undoable) toast(archivedWords(tasks), { action: { label: "Undo", run: () => void archiving.undo().catch(failed) } });
    else if (tasks.length > 1) toast(`Archived ${tasksWord(tasks.length)}`);
    then?.();
    archiving.sent.catch(failed);
  };
  const ask = (tasks: Task[], then?: () => void) => {
    const ready = tasks.filter(archivable);
    if (!ready.length || !can) return;
    if (undoable) return go(ready, then);
    (ready.length > 1 ? many : one).ask(() => setAsking({ tasks: ready, then }), () => go(ready, then));
  };
  const warn = asking && asking.tasks.length > 1 ? many : one;
  // Conversations leave Talk; tasks leave Work's lists.
  const talk = !!asking?.tasks.every((t) => t.conversation);
  const several = (asking?.tasks.length ?? 0) > 1;
  const named = asking && (several ? (talk ? `${asking.tasks.length} conversations` : tasksWord(asking.tasks.length)) : talk ? "conversation" : "task");
  const sheet = asking && (
    <Sheet open onClose={() => setAsking(null)} className="work-sheet"
      title={`Archive ${named}`}
      subtitle={several ? undefined : asking.tasks[0]!.title}>
      <div className="work-stack">
        <p>{several ? "They move" : "It moves"} to {talk ? "Chat" : "Tasks"} › Archived, with {several ? "their" : "its"} history, requests and attachments, to read.</p>
        {warn.check}
        <Button variant="primary" disabled={!can} onClick={() => { warn.agreed(); go(asking.tasks, asking.then); }}>
          {several ? `Archive ${named}` : "Archive"}
        </Button>
      </div>
    </Sheet>
  );
  return { ask, sheet, can };
}

/** What the Undo toast says was archived, and where it went in place of the warning. */
function archivedWords(tasks: Task[]): string {
  const talk = tasks.every((t) => t.conversation);
  const n = tasks.length;
  const what = n === 1 ? (talk ? "conversation" : "task") : talk ? `${n} conversations` : tasksWord(n);
  return `Archived ${what}, now in ${talk ? "Chat" : "Tasks"} › Archived`;
}
