// SPDX-License-Identifier: Apache-2.0
import {
  ArrowDown,
  ArrowUp,
  CircleHelp,
  Clock,
  ListOrdered,
  Pause,
  Play,
} from "lucide-react";
import { useRef, useState } from "react";
import {
  shallowEqual,
  useApp,
  useCommand,
  useHost,
  useLink,
  useUi,
} from "../../app/context";
import { Button } from "../../design/Button";
import { Pill } from "../../design/Chip";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { Screen } from "../../shell/Screen";
import { useStore } from "../../state/store";
import { helpersFinishing } from "../../state/projection";
import type { Command, QueueItem, QueueView, Task } from "../../protocol/types";
import type { RouteProps } from "../types";
import { dependencyError } from "./logic";
import {
  CommandFeedback,
  ConnectionNote,
  TaskPermissionNote,
  inDoubt,
  useCanWork,
  useOpenTask,
  useUnconfirmedCommand,
} from "./shared";
import { expectQueue, pendingQueueItems, queueItemState, queueMoveError, queueState, useQueue } from "./useQueue";
import { useLocalText } from "./local";
import { writerHold } from "../talk/intent";
import "./work.css";
import { useWarning } from "../../app/warnings";

type Editing = {
  mode: "remove" | "dependencies" | "authorization";
  item: QueueItem;
  revision: number;
};
export default function NextUp(_: RouteProps<"next_up">) {
  const { ui, session } = useApp();
  const projectId = useUi((s) => s.projectId);
  const { queue, error, allowed, reload } = useQueue(projectId);
  const tasks = useHost((s) => s.tasks);
  const writer = useHost((s) =>
    queue?.writer ? s.tasks[s.runs[queue.writer]?.task_id ?? ""] : undefined,
  );
  const writerRun = useHost((s) => queue?.writer ? s.runs[queue.writer] : undefined);
  const hasWriter = !!queue?.writer && (!writerRun || writerRun.holds_writer);
  const openTask = useOpenTask();
  const held = useHost((s) => hasWriter ? writerHold(queue?.writer, s) : null, shallowEqual);
  const writerDescription = !hasWriter
    ? "No writer is holding this project."
    : !writerRun
      ? "Waiting for the computer to report the writer’s state."
      : writerRun.state === "reconciliation_required"
        ? "Stopped and waiting for your decision. Nothing else can change files until you decide."
        : writerRun.state === "stopping"
          ? "Stopping. The computer is checking what ended."
          : helpersFinishing(writerRun)
            ? "Its turn has ended, but helpers it started are still finishing. Nothing else can change files until they end."
            : ["completed", "failed", "interrupted"].includes(writerRun.state)
              ? "This task has ended. The computer is updating the writer slot."
              : ["admitted", "dispatching"].includes(writerRun.state)
                ? "Starting. The computer has reserved this project’s writer slot."
                : writerRun.state === "awaiting_approval"
                  ? "Waiting for approval. This task holds the project’s writer slot."
                  : writerRun.state === "awaiting_input"
                    ? "Agent paused. This task holds the project’s writer slot until you resume it."
                    : "Running. This task is changing files.";
  const pendingItems = pendingQueueItems(queue);
  const [editing, setEditing] = useState<Editing | null>(null);
  const warn = useWarning("next_up.remove");
  const [localError, setLocalError] = useState<string | null>(null);
  const can = useCanWork();
  const send = useCommand();
  const [pending, setPending] = useState(false);
  const [pauseAfter, setPauseAfter] = useState<number | null>(null);
  const commands = useStore(
    session.commands,
    (all) =>
      Object.values(all).filter(
        (c) =>
          c.body.command.startsWith("queue_") &&
          (("project_id" in c.body && c.body.project_id === projectId) ||
            ("item_id" in c.body &&
              queue?.items.some(
                (i) => "item_id" in c.body && i.item_id === c.body.item_id,
              ))),
      ),
    shallowEqual,
  );
  const online = useLink().state === "online";
  // A change still on its way isn't in doubt: the next one waits for it.
  // Only one whose outcome is unknown holds the list back.
  const uncertain = commands.some((c) => c.phase !== "sending" && inDoubt(session, online, c));
  const lastStopped = useStore(session.commands, (all) =>
    Object.values(all)
      .filter(
        (c) =>
          c.body.command === "stop_task" &&
          tasks[c.body.task_id]?.project_id === projectId,
      )
      .at(-1),
  );
  const stopped =
    lastStopped?.body.command === "stop_task"
      ? tasks[lastStopped.body.task_id]
      : undefined;
  const pause = commands.filter((c) => c.body.command === "queue_pause").at(-1);
  const receipt = useHost((s) =>
    pause
      ? s.interventions
          .filter(
            (i) =>
              i.receipt.kind === "pause_queue" &&
              pauseAfter !== null &&
              i.seq > pauseAfter &&
              (i.taskId
                ? s.tasks[i.taskId]?.project_id === projectId
                : i.seq === pause.receipt?.seq),
          )
          .at(-1)
      : undefined,
  );
  const runTask = receipt?.taskId ? tasks[receipt.taskId] : undefined;
  // Next up's changes go one at a time. Each is made against the queue's
  // revision, so one tapped while another is on its way waits for it and
  // goes against the revision that one leaves; the buttons stay usable.
  const line = useRef<Promise<number | null> | null>(null);
  const sendQueue = (body: Command, label: string, revision: number) => {
    const pausing = body.command === "queue_pause" || body.command === "queue_resume";
    if (body.command === "queue_pause")
      setPauseAfter(session.state.get().journalHead);
    if (pausing) setPending(true);
    const before = line.current;
    // A move or removal shows at once. If the computer refuses it, the item
    // goes back and the refusal is a toast, so it never moves back silently.
    const shown = body.command === "queue_move" || body.command === "queue_remove";
    const expected = projectId && shown ? expectQueue(session.state, projectId, body, revision, !!before) : null;
    const mine = (async () => {
      let at = before ? await before : revision;
      // A move made over one the computer refused goes back with it; a
      // pause or resume still goes, against the queue as it is.
      if (at === null && pausing && projectId) at = session.state.base().queues[projectId]?.revision ?? null;
      if (at === null) {
        expected?.settled(false);
        return null;
      }
      expected?.sending(at);
      const r = await send(body, {
        label,
        expectedRevision: at,
        toastErrors: !!expected,
      });
      expected?.settled(!!r);
      return r ? at + 1 : null;
    })();
    line.current = mine;
    void mine.finally(() => {
      if (line.current === mine) line.current = null;
      if (pausing) setPending(false);
    });
    return mine;
  };
  const safety = queue ? (
    <div className="work-safety">
      <Button
        size="lg"
        variant={queue.paused ? "secondary" : "warn"}
        icon={queue.paused ? Play : Pause}
        pending={pending}
        disabled={!can || pending || uncertain}
        onClick={() =>
          void sendQueue(
            {
              command: queue.paused ? "queue_resume" : "queue_pause",
              project_id: queue.project_id,
            },
            queue.paused ? "Resume queue" : "Pause queue",
            queue.revision,
          )
        }
      >
        {queue.paused ? "Resume queue" : "Pause queue"}
      </Button>
    </div>
  ) : undefined;
  return (
    <Screen title="Next up" className="work-screen" footer={safety}>
      <div className="work-stack">
        <ConnectionNote />
        <TaskPermissionNote />
        {error && (
          <Notice
            tone="stop"
            action={<Button onClick={reload}>Reload queue</Button>}
          >
            {error}
          </Notice>
        )}
        {!projectId ? (
          <Empty title="Choose a project">
            Open Tasks to choose the project whose queue you want to review.
          </Empty>
        ) : !queue ? (
          allowed && <Notice title="Queue not loaded">
            Connect to the computer to see the current queue.
          </Notice>
        ) : (
          <>
            <Section title="Changing files now">
              {/* The task that holds the project opens from here, as anywhere a task is named. */}
              {hasWriter && writer ? (
                <div className="group">
                  <ListRow title={writer.title} subtitle={writerDescription} chevron onClick={() => openTask(writer)} />
                </div>
              ) : (
                <div className="work-card">
                  <p className="work-card-title">
                    {hasWriter ? "The computer hasn’t reported this task" : "No task is changing files"}
                  </p>
                  <p className="work-muted">
                    {writerDescription}
                  </p>
                </div>
              )}
              {hasWriter && writer && (
                <CommandFeedback
                  match={(c) =>
                    c.command === "stop_task" && c.task_id === writer.task_id
                  }
                />
              )}
                {hasWriter && writer && writerRun?.state === "reconciliation_required" && <Button onClick={() => ui.push({ screen: "receipt", taskId: writer.task_id })}>Decide what happened</Button>}
            </Section>
            {stopped?.state === "reconciliation_required" && (
              <Notice title="Stop receipt ready">
                <p>{stopped.title}</p>
                <Button
                  onClick={() =>
                    ui.push({ screen: "receipt", taskId: stopped.task_id })
                  }
                >
                  View stop receipt
                </Button>
              </Notice>
            )}
            {queue.paused && <p>{queue.pause_reason ?? "Next up is paused. Resume it to let tasks start."}</p>}
            <div className="work-chips">
              <Pill icon={{ Paused: Pause, "Needs your go-ahead": CircleHelp, Blocked: Clock, Running: Play }[queueState(queue)]}>
                {queueState(queue)}
              </Pill>
            </div>
            <CommandFeedback
              limit={1}
              match={(c) =>
                c.command.startsWith("queue_") &&
                (("project_id" in c && c.project_id === projectId) ||
                  ("item_id" in c &&
                    queue.items.some((i) => i.item_id === c.item_id)))
              }
            />
            {receipt && (
              <Notice title="Queue pause receipt">
                <p>{receipt.receipt.summary}</p>
                {receipt.receipt.facts.map((f, i) => (
                  <p key={i}>
                    {f.label}: {f.status.replaceAll("_", " ")}
                    {f.detail ? ` · ${f.detail}` : ""}
                  </p>
                ))}
                {runTask && (
                  <div className="work-stack">
                    <p>
                      Already admitted: <strong>{runTask.title}</strong>
                    </p>
                  </div>
                )}
              </Notice>
            )}
            <p className="work-caption">Each task must stay after the tasks it waits for. Moves that break that order are refused.</p>
            {localError && <Notice tone="stop">{localError}</Notice>}
            {!pendingItems.length ? (
              <Empty icon={ListOrdered} title="Nothing lined up">
                In Tasks, choose New task, write a request and choose Add to Next up. Each item
                keeps the start permission you choose during review.
              </Empty>
            ) : (
              <ol className="work-queue" role="list">
                {pendingItems
                  .map((item, index) => {
                    const task = tasks[item.task_id];
                    const started = item.state.state === "started";
                    return (
                      <li className="work-card work-stack" key={item.item_id}>
                        <div className="work-queue-heading">
                          <span
                            className="work-position"
                            aria-label={`Position ${index + 1}`}
                          >
                            {index + 1}
                          </span>
                          <h2 className="work-card-title">
                            {task?.title ?? "Task details not reported"}
                          </h2>
                        </div>
                        <p className="work-queue-state">
                          {queueItemState(item, index, (task?.mode ?? "work") === "work" ? held : null)}
                        </p>
                        {item.authorization_expires_at && <p className="work-caption">Permission expires {new Date(item.authorization_expires_at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}. One start only.</p>}
                        <div className="work-chips">
                          <Pill
                            icon={
                              item.authorization === "auto_start"
                                ? Play
                                : CircleHelp
                            }
                          >
                            {item.authorization === "auto_start"
                              ? "Starts by itself"
                              : "Asks before starting"}
                          </Pill>
                          {item.depends_on.map((id) => {
                            const dependency = queue.items.find(
                              (i) => i.item_id === id,
                            );
                            return (
                              <Pill key={id} icon={Clock}>
                                After:{" "}
                                {dependency
                                  ? (tasks[dependency.task_id]?.title ??
                                    "Task not reported")
                                  : "Item no longer in queue"}
                              </Pill>
                            );
                          })}
                        </div>
                        {task && (
                          <Button
                            variant="primary"
                            onClick={() =>
                              ui.push({
                                screen: "review",
                                projectId: queue.project_id,
                                taskId: item.task_id,
                                queueItemId: item.item_id,
                              })
                            }
                          >
                            {index === 0 ? "Review and start" : "Review"}
                          </Button>
                        )}
                        {!started && (
                          <>
                            {item.authorization === "auto_start" && (
                              <p className="work-caption">
                                Moving or editing this item revokes its earlier
                                permission to start by itself.
                              </p>
                            )}
                            <div className="work-actions">
                              <Button
                                icon={ArrowUp}
                                disabled={
                                  !can ||
                                  uncertain ||
                                  !!queueMoveError(
                                    queue,
                                    item.item_id,
                                    (pendingItems[index - 1]?.position ?? -1),
                                  )
                                }
                                onClick={() => {
                                  const error = queueMoveError(
                                    queue,
                                    item.item_id,
                                    (pendingItems[index - 1]?.position ?? -1),
                                  );
                                  setLocalError(error);
                                  if (!error)
                                    void sendQueue(
                                      {
                                        command: "queue_move",
                                        item_id: item.item_id,
                                        position: (pendingItems[index - 1]?.position ?? -1),
                                      },
                                      `Move ${task?.title ?? "item"} up`,
                                      queue.revision,
                                    );
                                }}
                              >
                                Move up
                              </Button>
                              <Button
                                icon={ArrowDown}
                                disabled={
                                  !can ||
                                  uncertain ||
                                  !!queueMoveError(
                                    queue,
                                    item.item_id,
                                    (pendingItems[index + 1]?.position ?? -1),
                                  )
                                }
                                onClick={() => {
                                  const error = queueMoveError(
                                    queue,
                                    item.item_id,
                                    (pendingItems[index + 1]?.position ?? -1),
                                  );
                                  setLocalError(error);
                                  if (!error)
                                    void sendQueue(
                                      {
                                        command: "queue_move",
                                        item_id: item.item_id,
                                        position: (pendingItems[index + 1]?.position ?? -1),
                                      },
                                      `Move ${task?.title ?? "item"} down`,
                                      queue.revision,
                                    );
                                }}
                              >
                                Move down
                              </Button>
                            </div>
                            <div className="work-actions">
                              <Button
                                variant="quiet"
                                disabled={!can || uncertain}
                                onClick={() =>
                                  setEditing({
                                    item,
                                    revision: queue.revision,
                                    mode: "dependencies",
                                  })
                                }
                              >
                                Dependencies
                              </Button>
                              <Button
                                variant="quiet"
                                disabled={!can || uncertain}
                                onClick={() =>
                                  setEditing({
                                    item,
                                    revision: queue.revision,
                                    mode: "authorization",
                                  })
                                }
                              >
                                Start permission
                              </Button>
                              <Button
                                variant="quiet"
                                disabled={!can || uncertain}
                                onClick={() => warn.ask(
                                  () => setEditing({
                                    item,
                                    revision: queue.revision,
                                    mode: "remove",
                                  }),
                                  () => void sendQueue(
                                    { command: "queue_remove", item_id: item.item_id },
                                    `Remove ${tasks[item.task_id]?.title ?? "item"} from queue`,
                                    queue.revision,
                                  ),
                                )}
                              >
                                Remove{warn.more}
                              </Button>
                            </div>
                          </>
                        )}
                      </li>
                    );
                  })}
              </ol>
            )}
            {editing && (
              <QueueEdit
                key={`${editing.item.item_id}.${editing.mode}`}
                editing={editing}
                queue={queue}
                tasks={tasks}
                safety={safety}
                warn={warn}
                remove={sendQueue}
                onClose={() => setEditing(null)}
              />
            )}
          </>
        )}
      </div>
    </Screen>
  );
}
function QueueEdit({
  editing,
  queue,
  tasks,
  safety,
  warn,
  remove,
  onClose,
}: {
  editing: Editing;
  queue: QueueView;
  tasks: Record<string, Task>;
  /** Remove's warning. */
  warn: ReturnType<typeof useWarning>;
  safety: React.ReactNode;
  /** Sends a removal after any change still on its way. */
  remove: (body: Command, label: string, revision: number) => unknown;
  onClose: () => void;
}) {
  const { profile } = useApp();
  const send = useCommand();
  const can = useCanWork();
  const { item, mode, revision } = editing;
  const uncertain = useUnconfirmedCommand(
    (c) => "item_id" in c && c.item_id === item.item_id,
  );
  const task = tasks[item.task_id];
  const [raw, setRaw] = useLocalText(
    `${profile.profileId}.dependencies.${item.item_id}.${revision}`,
    JSON.stringify(item.depends_on),
  );
  let dependencies: string[] = item.depends_on;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (Array.isArray(parsed) && parsed.every((x) => typeof x === "string"))
      dependencies = parsed;
  } catch {
    /* Use the last computer-confirmed links. */
  }
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const stale = queue.revision !== revision;
  const earlier = queue.items.filter((i) => i.position < item.position);
  const submit = async () => {
    const issue =
      mode === "dependencies"
        ? dependencyError(queue.items, item.item_id, dependencies)
        : null;
    setError(issue);
    if (issue) return;
    // A removal leaves the list and this sheet at once; if the computer
    // refuses it, the item comes back and the refusal is a toast.
    if (mode === "remove") {
      onClose();
      remove({ command: "queue_remove", item_id: item.item_id }, `Remove ${task?.title ?? "item"} from queue`, revision);
      return;
    }
    setPending(true);
    const command: Command =
      mode === "dependencies"
        ? {
            command: "queue_set_dependencies",
            item_id: item.item_id,
            depends_on: dependencies,
          }
        : {
            command: "queue_set_authorization",
            item_id: item.item_id,
            authorization:
              item.authorization === "auto_start"
                ? "ask_before_start"
                : "auto_start",
          };
    const result = await send(command, {
      label: mode === "dependencies" ? "Save dependencies" : "Change start permission",
      expectedRevision: revision,
    });
    setPending(false);
    if (result) onClose();
  };
  return (
    <Sheet
      open
      onClose={onClose}
      className="work-sheet"
      title={
        mode === "remove"
          ? "Remove from Next up"
          : mode === "dependencies"
            ? "Dependencies"
            : "Start permission"
      }
      subtitle={task?.title}
      footer={safety}
    >
      <div className="work-stack">
        {stale && (
          <Notice tone="unknown">
            The queue changed. Close this sheet and review the current order
            before editing.
          </Notice>
        )}
        {mode === "remove" ? (
          <>
            <p>
              Remove <strong>{task?.title ?? "this item"}</strong> from Next up?
              This only removes it from the queue. Task history stays on the
              computer.
              {pendingQueueItems(queue).some((i) => i.item_id !== item.item_id && i.authorization === "auto_start") &&
                " Items set to start by themselves will then ask before they start."}
            </p>
            {warn.check}
          </>
        ) : mode === "authorization" ? (
          <p>
            {item.authorization === "auto_start"
              ? "Require a fresh review and Start before this item runs."
              : "Allow this item to start by itself when earlier tasks finish and no other task is changing files. Permission lasts a day and covers one start. Editing or reordering requires permission again."}
          </p>
        ) : (
          <>
            <p className="work-muted">
              Wait for these earlier items to finish successfully. Unknown or
              failed results keep dependent work blocked.
            </p>
            {!earlier.length && <p>No earlier items to depend on.</p>}
            {earlier.map((i) => (
              <label key={i.item_id} className="work-check">
                <input
                  type="checkbox"
                  checked={dependencies.includes(i.item_id)}
                  disabled={stale}
                  onChange={(e) =>
                    setRaw(
                      JSON.stringify(
                        e.target.checked
                          ? [...dependencies, i.item_id]
                          : dependencies.filter((id) => id !== i.item_id),
                      ),
                    )
                  }
                />
                <span>{tasks[i.task_id]?.title ?? "Task not reported"}</span>
              </label>
            ))}
            {dependencies.some(
              (id) => !earlier.some((i) => i.item_id === id),
            ) && (
              <Notice tone="unknown">
                An existing dependency is no longer earlier in the queue.{" "}
                <Button
                  onClick={() =>
                    setRaw(
                      JSON.stringify(
                        dependencies.filter((id) =>
                          earlier.some((i) => i.item_id === id),
                        ),
                      ),
                    )
                  }
                >
                  Remove unavailable dependencies
                </Button>
              </Notice>
            )}
            {item.authorization === "auto_start" && (
              <Notice>
                Saving dependencies changes this item to Ask before start.
              </Notice>
            )}
          </>
        )}
        {error && <Notice tone="stop">{error}</Notice>}
        <Button
          variant={mode === "remove" ? "stop-outline" : "primary"}
          disabled={!can || stale || uncertain}
          pending={pending}
          onClick={() => { if (mode === "remove") warn.agreed(); void submit(); }}
        >
          {mode === "remove"
            ? `Remove ${task?.title ?? "item"}`
            : mode === "dependencies"
              ? "Save dependencies"
              : item.authorization === "auto_start"
                ? "Ask before starting"
                : "Allow this item to start by itself"}
        </Button>
        <CommandFeedback
          match={(c) => "item_id" in c && c.item_id === item.item_id}
        />
      </div>
    </Sheet>
  );
}
