// SPDX-License-Identifier: Apache-2.0
//
// The phone's projection of host state. It is rebuilt only from a snapshot
// plus journal events in `seq` order, so everything the UI shows is traceable
// to something the host said. Nothing here guesses a newer state from a timer.

import { handBackReason, handBackTarget } from "./handBack";
import { applyDeliveries } from "./timeline";
import { withoutOutcome } from "./outcomeReport";
import type {
  Approval,
  ArtifactMeta,
  AttentionItem,
  Authenticated,
  ChannelIdentity,
  CommandStage,
  CommandResult,
  ProtocolError,
  ControlTarget,
  DeviceRecord,
  EventEnvelope,
  Grants,
  HarnessInfo,
  HostCapabilities,
  HostInfo,
  InterventionReceipt,
  LibraryRoot,
  LiveEvent,
  PreviewEndpoint,
  Project,
  QueueView,
  ReadinessReport,
  Run,
  Snapshot,
  TargetControlView,
  Task,
  TaskCursor,
  TaskRecords,
  TerminalInfo,
  TimelineItem,
  TransferRecord,
} from "../protocol/types";

export interface TimelineState {
  /** Items in ascending `seq`. */
  items: TimelineItem[];
  /** True when older items may exist on the host. */
  hasMore: boolean;
  /** False until the first page has been loaded for this task. */
  loaded: boolean;
}

export interface LiveText {
  taskId: string;
  runId: string;
  key: string;
  text: string;
  at: number;
  /** Deltas were sent while the phone wasn't listening and are gone. Later
   * ones are not appended to it; the journaled message replaces it. */
  interrupted?: boolean;
}

export interface InterventionRecord {
  seq: number;
  at: number;
  taskId: string | null;
  targetId: string | null;
  receipt: InterventionReceipt;
}

export interface JournalGap {
  requestedFrom: number;
  oldestRetained: number;
  at: number;
}

export interface HostState {
  hostId: string | null;
  bootEpoch: string | null;
  grants: Grants | null;
  /** Highest journal `seq` applied. */
  journalHead: number;
  /** True once the host has said `caught_up` for the current subscription. */
  caughtUp: boolean;
  /** Local time of the last snapshot/event/pong, for freshness display. */
  lastHeardAt: number | null;
  host: HostInfo | null;
  capabilities: HostCapabilities | null;
  harnesses: HarnessInfo[];
  projects: Record<string, Project>;
  tasks: Record<string, Task>;
  /** Per project, where tasks older than those held begin, for projects the
   * computer has more of. */
  olderTasks: Record<string, TaskCursor>;
  runs: Record<string, Run>;
  timelines: Record<string, TimelineState>;
  /** Local readers (screens and in-flight queries); never persisted. */
  timelineReaders: Record<string, number>;
  approvals: Record<string, Approval>;
  attention: Record<string, AttentionItem>;
  /** When attention this phone did not hold was resolved, by id. A read the
   * computer answered just before is applied after, and brings it unresolved. */
  resolvedAttention: Record<string, number>;
  targets: ControlTarget[];
  control: Record<string, TargetControlView>;
  interventions: InterventionRecord[];
  queues: Record<string, QueueView>;
  libraryRoots: LibraryRoot[];
  /** Latest revision the host reported per `${root}\u0000${path}`; null = deleted. */
  libraryRevisions: Record<string, string | null>;
  terminals: TerminalInfo[];
  transfers: Record<string, TransferRecord>;
  previews: Record<string, PreviewEndpoint[]>;
  artifacts: Record<string, ArtifactMeta>;
  devices: DeviceRecord[];
  identities: ChannelIdentity[];
  readiness: ReadinessReport | null;
  /** Counts the computer's word that its accounts changed; the Accounts screen asks again. */
  accountsRevision: number;
  /** Counts the computer's word that a project's checklist changed; what shows one asks again. */
  checklistsRevision: number;
  /** Full-list readers invalidate just the changed project, or all on resync. */
  checklistProjects: Record<string, number>;
  checklistEpoch: number;
  /** Bumped when the computer's agent logins change or one is used. */
  agentLoginsRevision: number;
  /** Counts the computer's word that which alerts reach the person changed. */
  notificationsRevision: number;
  /** Counts the computer's word that problem reports or their settings changed. */
  reportsRevision: number;
  /** Bumped by `github_changed` and `pr_changed`, so GitHub views read again. */
  githubRevision: number;
  /** The latest use of a login its owner asked to hear about, heard live:
   *  a replay after reconnecting doesn't tell of old uses again. */
  agentLoginUse: { seq: number; loginId: string; name: string; taskId: string; site: string } | null;
  commandStages: Record<string, { stage: CommandStage; detail: string | null; at: number; result?: CommandResult; error?: ProtocolError }>;
  liveText: Record<string, LiveText>;
  activity: Record<string, { runId: string; text: string; at: number }>;
  gaps: JournalGap[];
}

export const emptyHostState: HostState = {
  hostId: null,
  bootEpoch: null,
  grants: null,
  journalHead: 0,
  caughtUp: false,
  lastHeardAt: null,
  host: null,
  capabilities: null,
  harnesses: [],
  projects: {},
  tasks: {},
  olderTasks: {},
  runs: {},
  timelines: {},
  timelineReaders: {},
  approvals: {},
  attention: {},
  resolvedAttention: {},
  targets: [],
  control: {},
  interventions: [],
  queues: {},
  libraryRoots: [],
  libraryRevisions: {},
  terminals: [],
  transfers: {},
  previews: {},
  artifacts: {},
  devices: [],
  identities: [],
  readiness: null,
  accountsRevision: 0,
  checklistsRevision: 0,
  checklistProjects: {},
  checklistEpoch: 0,
  agentLoginsRevision: 0,
  notificationsRevision: 0,
  reportsRevision: 0,
  githubRevision: 0,
  agentLoginUse: null,
  commandStages: {},
  liveText: {},
  activity: {},
  gaps: [],
};

const MAX_INTERVENTIONS = 50;
const MAX_GAPS = 20;
const MAX_RESOLVED_ATTENTION = 200;
export const INACTIVE_TIMELINE_ITEMS = 80;
export const INACTIVE_TIMELINES = 8;

/** Outside an open conversation keep only a small, contiguous recent tail.
 * Its first item remains a valid cursor for loading everything before it. */
function trimTimeline(timeline: TimelineState): TimelineState {
  return timeline.items.length <= INACTIVE_TIMELINE_ITEMS ? timeline : {
    ...timeline, items: timeline.items.slice(-INACTIVE_TIMELINE_ITEMS), hasMore: true,
  };
}

function keepTimeline(state: HostState, taskId: string, timeline: TimelineState): HostState {
  const timelines = { ...state.timelines };
  // Insertion order records which conversations were touched most recently.
  delete timelines[taskId];
  timelines[taskId] = state.timelineReaders[taskId] ? timeline : trimTimeline(timeline);
  const inactive = Object.keys(timelines).filter((id) => !state.timelineReaders[id]);
  for (const id of inactive.slice(0, Math.max(0, inactive.length - INACTIVE_TIMELINES))) delete timelines[id];
  return { ...state, timelines };
}

/** Pin before asking for a page: every event racing its answer must survive,
 * even when that answer predates more than a tail's worth of new events. */
export function retainTimeline(state: HostState, taskId: string): HostState {
  return {
    ...state,
    timelineReaders: { ...state.timelineReaders, [taskId]: (state.timelineReaders[taskId] ?? 0) + 1 },
  };
}

export function releaseTimeline(state: HostState, taskId: string): HostState {
  const timelineReaders = { ...state.timelineReaders };
  if ((timelineReaders[taskId] ?? 0) > 1) timelineReaders[taskId] = timelineReaders[taskId]! - 1;
  else delete timelineReaders[taskId];
  const next = { ...state, timelineReaders };
  const timeline = next.timelines[taskId];
  return timeline ? keepTimeline(next, taskId, timeline) : next;
}

export const libraryKey = (rootId: string, path: string) => `${rootId}\u0000${path}`;

function byId<T>(items: T[], id: (t: T) => string): Record<string, T> {
  const out: Record<string, T> = {};
  for (const item of items) out[id(item)] = item;
  return out;
}

export function applyAuthenticated(state: HostState, auth: Authenticated, bootEpoch: string, now: number): HostState {
  const sameHost = state.hostId === auth.host_id;
  const base = sameHost ? state : { ...emptyHostState };
  // A new connection carries no deltas sent while the phone was away.
  return interruptLive({
    ...base,
    hostId: auth.host_id,
    // A new boot epoch voids per-boot state; the snapshot that follows
    // replaces leases and targets.
    bootEpoch,
    grants: auth.grants,
    capabilities: auth.capabilities,
    caughtUp: false,
    lastHeardAt: now,
  });
}

/** Replace durable state with a host snapshot. Loaded timelines are kept and
 * extended by later events; they are refetched after a journal gap. */
export function applySnapshot(state: HostState, snap: Snapshot, now: number): HostState {
  return {
    ...state,
    journalHead: snap.journal_head,
    checklistsRevision: state.checklistsRevision + 1,
    checklistEpoch: state.checklistEpoch + 1,
    lastHeardAt: now,
    host: snap.host,
    projects: byId(snap.projects, (p) => p.project_id),
    tasks: byId(snap.tasks, (t) => t.task_id),
    olderTasks: Object.fromEntries((snap.older_tasks?.projects ?? []).map((p) => [p, snap.older_tasks!.from])),
    runs: byId(snap.runs, (r) => r.run_id),
    approvals: byId(snap.approvals, (a) => a.approval_id),
    attention: byId(snap.attention, (a) => a.attention_id),
    targets: snap.targets,
    control: currentControls(snap.targets, byId(snap.control, (c) => c.target_id)),
    libraryRoots: snap.library_roots,
  };
}

/** A snapshot a screen asked for, outside the subscription. The computer
 * answers queries ahead of journal events it has already queued for this
 * phone, so the phone keeps its own place in the journal: moving it to the
 * snapshot's head would drop those events as replays, and with them what a
 * snapshot does not carry. An answer older than that place changes nothing.
 * Older tasks this phone read stay, and so does where it reads on: the
 * journal it follows keeps them current. */
export function refreshFromSnapshot(state: HostState, snap: Snapshot, now: number): HostState {
  if (snap.journal_head < state.journalHead) return state;
  const next = applySnapshot(state, snap, now);
  return {
    ...next,
    journalHead: state.journalHead,
    // A screen refresh does not replace the subscription or miss its events.
    checklistsRevision: state.checklistsRevision,
    checklistEpoch: state.checklistEpoch,
    olderTasks: state.olderTasks,
    tasks: { ...state.tasks, ...next.tasks },
    runs: { ...state.runs, ...next.runs },
    approvals: { ...state.approvals, ...next.approvals },
    attention: { ...state.attention, ...next.attention },
  };
}

/** The computer reads older tasks and whole runs on request. One from before
 * that sends every task and run whole in its snapshot, and closes a
 * connection that asks it for them. */
export const readsHistory = (s: HostState): boolean => !!s.capabilities?.features.includes("task_history");

/** Tasks a snapshot left out, read as the person reaches them. Records this
 * phone already holds are kept: the journal keeps those current, and an
 * answer can be older than what it has applied. */
export function applyTaskRecords(state: HostState, records: TaskRecords): HostState {
  const add = <T,>(held: Record<string, T>, read: T[], id: (t: T) => string): Record<string, T> => {
    const missing = read.filter((r) => !(id(r) in held));
    return missing.length ? { ...byId(missing, id), ...held } : held;
  };
  return {
    ...state,
    tasks: add(state.tasks, records.tasks, (t) => t.task_id),
    runs: add(state.runs, records.runs, (r) => r.run_id),
    approvals: add(state.approvals, records.approvals, (a) => a.approval_id),
    attention: add(state.attention, records.attention.map((a) => resolvedSince(state, a)), (a) => a.attention_id),
  };
}

const resolvedSince = (state: HostState, a: AttentionItem): AttentionItem => {
  const at = state.resolvedAttention[a.attention_id];
  return at === undefined || a.resolved_at !== null ? a : { ...a, resolved_at: at };
};

/** A page of a project's older tasks read from `from`. It moves the place
 * the list reads on from only if that is still where the list ends. */
export function applyOlderTasks(state: HostState, projectId: string, from: TaskCursor, records: TaskRecords, next: TaskCursor | null): HostState {
  const read = applyTaskRecords(state, records);
  const place = state.olderTasks[projectId];
  if (place?.task_id !== from.task_id || place.updated_at !== from.updated_at) return read;
  const olderTasks = { ...state.olderTasks };
  if (next) olderTasks[projectId] = next;
  else delete olderTasks[projectId];
  return { ...read, olderTasks };
}

export function applyTimelinePage(
  state: HostState,
  taskId: string,
  items: TimelineItem[],
  hasMore: boolean,
  older = false,
): HostState {
  const prev = state.timelines[taskId];
  // The newest page after a journal gap may not reach the items kept from
  // before it. Anything older than the page is dropped rather than left behind
  // a hole that loading earlier items could never fill.
  const oldest = items.length ? Math.min(...items.map((i) => i.seq)) : null;
  const kept = prev && !prev.loaded && !older && oldest !== null ? prev.items.filter((i) => i.seq >= oldest) : (prev?.items ?? []);
  const merged = mergeTimeline(kept, items);
  return {
    ...state,
    timelines: {
      ...state.timelines,
      [taskId]: { items: merged, hasMore: prev?.loaded ? prev.hasMore && hasMore : hasMore, loaded: true },
    },
  };
}

/** An agent's message as the person reads it: without the report it ends with for Ophio. */
function readable(i: TimelineItem): TimelineItem {
  if (i.item.kind !== "agent_message") return i;
  const text = withoutOutcome(i.item.text);
  return text === i.item.text ? i : { ...i, item: { ...i.item, text } };
}

function mergeTimeline(existing: TimelineItem[], incoming: TimelineItem[]): TimelineItem[] {
  if (incoming.length === 0) return existing;
  // Journal appends are already ordered. Copy once and only scan delivery
  // history when a new delivery could change an earlier message.
  let last = existing.at(-1)?.seq ?? -1;
  const appending = incoming.every((item) => {
    const ordered = item.seq > last;
    last = item.seq;
    return ordered;
  });
  if (appending) {
    const items = [...existing, ...incoming.map(readable)];
    return incoming.some((i) => i.item.kind === "delivery") ? applyDeliveries(items) : items;
  }
  const seen = new Map<number, TimelineItem>();
  for (const i of existing) seen.set(i.seq, i);
  for (const i of incoming) seen.set(i.seq, readable(i));
  return applyDeliveries([...seen.values()].sort((a, b) => a.seq - b.seq));
}

function appendTimeline(state: HostState, taskId: string, items: TimelineItem[]): HostState {
  const prev = state.timelines[taskId];
  return keepTimeline(state, taskId, {
    hasMore: true, loaded: false, ...prev, items: mergeTimeline(prev?.items ?? [], items),
  });
}

export type ProjectionFrame = { env: EventEnvelope; now: number } | { live: LiveEvent; now: number };

/** Reduce journal/live effects in wire order, but merge each task's timeline
 * just once. No other event reducer reads timeline items. */
export function applyReplay(state: HostState, frames: ProjectionFrame[]): HostState {
  const pending = new Map<string, TimelineItem[]>();
  const collect = (s: HostState, taskId: string, item: TimelineItem): HostState => {
    const items = pending.get(taskId) ?? [];
    items.push(item);
    pending.delete(taskId);
    pending.set(taskId, items);
    return s;
  };
  let next = state;
  for (const frame of frames) next = "env" in frame
    ? reduceEvent(next, frame.env, frame.now, collect)
    : applyLive(next, frame.live, frame.now);
  for (const [taskId, items] of pending) next = appendTimeline(next, taskId, items);
  return next;
}

export function applyEvent(state: HostState, env: EventEnvelope, now: number): HostState {
  return reduceEvent(state, env, now, (s, taskId, item) => appendTimeline(s, taskId, [item]));
}

function reduceEvent(state: HostState, env: EventEnvelope, now: number,
  append: (state: HostState, taskId: string, item: TimelineItem) => HostState): HostState {
  // The journal is ordered; replays after reconnect may repeat old seqs.
  if (env.seq <= state.journalHead) return state;
  const s: HostState = { ...state, journalHead: env.seq, lastHeardAt: now };
  const e = env.event;
  switch (e.type) {
    case "host_updated":
      // The host record carries what the computer can do now.
      return { ...s, host: e.host, capabilities: e.host.capabilities };
    case "capabilities_changed":
      return { ...s, capabilities: e.capabilities };
    case "harnesses_changed":
      return { ...s, harnesses: e.harnesses };
    case "project_upserted":
      return { ...s, projects: { ...s.projects, [e.project.project_id]: e.project } };
    case "project_removed": {
      const projects = { ...s.projects };
      delete projects[e.project_id];
      return { ...s, projects };
    }
    case "task_upserted":
      return { ...s, tasks: { ...s.tasks, [e.task.task_id]: e.task } };
    case "run_upserted": {
      let next: HostState = { ...s, runs: { ...s.runs, [e.run.run_id]: e.run } };
      if (isTerminalRunState(e.run.state)) next = clearLiveForRun(next, e.run.run_id);
      return next;
    }
    case "timeline_appended": {
      let next = append(s, e.task_id, e.item);
      const body = e.item.item;
      if (body.kind === "agent_message" && !body.partial && e.item.run_id) {
        next = supersedeLiveText(next, e.item.run_id, body.text);
      }
      if (body.kind === "run_ended") next = clearLiveForRun(next, body.run_id);
      return next;
    }
    case "command_progress":
      return {
        ...s,
        commandStages: { ...s.commandStages, [e.command_id]: { stage: e.stage, detail: e.detail, at: env.at, result: e.result, error: e.error } },
      };
    case "approval_upserted":
      return { ...s, approvals: { ...s.approvals, [e.approval.approval_id]: e.approval } };
    case "attention_upserted":
      return { ...s, attention: { ...s.attention, [e.item.attention_id]: e.item } };
    case "attention_resolved": {
      const item = s.attention[e.attention_id];
      if (!item) {
        const kept = Object.entries(s.resolvedAttention).slice(1 - MAX_RESOLVED_ATTENTION);
        return { ...s, resolvedAttention: { ...Object.fromEntries(kept), [e.attention_id]: env.at } };
      }
      return { ...s, attention: { ...s.attention, [e.attention_id]: { ...item, resolved_at: item.resolved_at ?? env.at } } };
    }
    case "targets_changed":
      return { ...s, targets: e.targets, control: currentControls(e.targets, s.control) };
    case "control_changed":
      if (!s.targets.some((target) => target.target_id === e.view.target_id)) return s;
      return { ...s, control: { ...s.control, [e.view.target_id]: e.view } };
    case "intervention_recorded":
      return {
        ...s,
        interventions: [
          ...s.interventions.slice(-(MAX_INTERVENTIONS - 1)),
          { seq: env.seq, at: env.at, taskId: e.task_id, targetId: e.target_id, receipt: e.receipt },
        ],
      };
    case "queue_changed":
      return { ...s, queues: { ...s.queues, [e.queue.project_id]: e.queue } };
    case "library_roots_changed":
      return { ...s, libraryRoots: e.roots };
    case "library_file_changed":
      return { ...s, libraryRevisions: { ...s.libraryRevisions, [libraryKey(e.root_id, e.path)]: e.revision } };
    case "terminals_changed":
      return { ...s, terminals: e.terminals };
    case "transfer_upserted":
      return { ...s, transfers: { ...s.transfers, [e.transfer.transfer_id]: e.transfer } };
    case "previews_changed":
      return { ...s, previews: { ...s.previews, [e.project_id]: e.previews } };
    case "artifact_created":
      return { ...s, artifacts: { ...s.artifacts, [e.artifact.artifact_id]: e.artifact } };
    case "artifact_deleted": {
      const { [e.artifact_id]: _gone, ...artifacts } = s.artifacts;
      return { ...s, artifacts };
    }
    case "devices_changed":
      return { ...s, devices: e.devices };
    case "channel_identities_changed":
      return { ...s, identities: e.identities };
    case "readiness_reported":
      return { ...s, readiness: e.report };
    case "accounts_changed":
      return { ...s, accountsRevision: s.accountsRevision + 1 };
    case "checklist_changed":
      return { ...s, checklistsRevision: s.checklistsRevision + 1, checklistProjects: { ...s.checklistProjects, [e.project_id]: (s.checklistProjects[e.project_id] ?? 0) + 1 } };
    case "agent_logins_changed":
      return { ...s, agentLoginsRevision: s.agentLoginsRevision + 1 };
    case "notification_settings_changed":
      return { ...s, notificationsRevision: s.notificationsRevision + 1 };
    case "reports_changed":
      return { ...s, reportsRevision: s.reportsRevision + 1 };
    case "github_changed":
    case "pr_changed":
      return { ...s, githubRevision: s.githubRevision + 1 };
    case "agent_login_used":
      return {
        ...s,
        agentLoginsRevision: s.agentLoginsRevision + 1,
        agentLoginUse: e.notify && state.caughtUp
          ? { seq: env.seq, loginId: e.login_id, name: e.name, taskId: e.task_id, site: e.site }
          : s.agentLoginUse,
      };
    case "boot_epoch_changed":
      // Leases and live streams from the previous boot are void. The host
      // follows with fresh control views; until then ownership is unknown.
      return {
        ...s,
        bootEpoch: e.boot_epoch,
        control: Object.fromEntries(
          Object.entries(s.control).map(([k, v]) => [
            k,
            { ...v, input_state: "unknown" as const, lease: v.lease ? { ...v.lease, state: "unknown" as const } : null },
          ]),
        ),
        liveText: {},
        activity: {},
      };
    default: {
      const unreachable: never = e;
      void unreachable;
      return s;
    }
  }
}

/** A current target inventory retires per-target state, not historical receipts.
 * Disconnection alone is not an inventory and must retain unknown ownership. */
function currentControls(targets: ControlTarget[], control: HostState["control"]): HostState["control"] {
  const ids = new Set(targets.map((target) => target.target_id));
  return Object.fromEntries(Object.entries(control).filter(([id]) => ids.has(id)));
}

export function applyLive(state: HostState, live: LiveEvent, now: number): HostState {
  switch (live.kind) {
    case "agent_text_delta": {
      const key = `${live.run_id}\u0000${live.message_key}`;
      const prev = state.liveText[key];
      return {
        ...state,
        lastHeardAt: now,
        liveText: {
          ...state.liveText,
          [key]: prev?.interrupted
            ? { ...prev, at: now }
            : {
                taskId: live.task_id,
                runId: live.run_id,
                key: live.message_key,
                text: (prev?.text ?? "") + live.text,
                at: now,
              },
        },
      };
    }
    case "activity_status":
      return {
        ...state,
        lastHeardAt: now,
        activity: { ...state.activity, [live.task_id]: { runId: live.run_id, text: live.text, at: now } },
      };
  }
}

/** The live stream broke. What was streamed so far stays until its journaled
 * message replaces it, but grows no further. */
export function interruptLive(state: HostState): HostState {
  const entries = Object.entries(state.liveText);
  if (entries.every(([, v]) => v.interrupted)) return state;
  return { ...state, liveText: Object.fromEntries(entries.map(([k, v]) => [k, { ...v, interrupted: true }])) };
}

export function applyGap(state: HostState, requestedFrom: number, oldestRetained: number, now: number): HostState {
  // Timelines may be missing items inside the gap; mark them for refetch.
  const timelines: Record<string, TimelineState> = {};
  for (const [k, v] of Object.entries(state.timelines)) timelines[k] = { ...v, loaded: false };
  return {
    ...state,
    timelines,
    gaps: [...state.gaps.slice(-(MAX_GAPS - 1)), { requestedFrom, oldestRetained, at: now }],
  };
}

/** The computer's journal is behind what this phone applied: its records were
 * reset or restored, so sequence numbers the phone holds now name different
 * facts. Nothing from the old journal is merged with the new one; the caller
 * reloads a snapshot, and open conversations are fetched again. Queue
 * revisions restart with the records, so an older Next up is dropped rather
 * than left to outrank the one the computer now reports. Resolutions it
 * remembered for notices it did not hold may never have happened in the
 * restored records, and are forgotten. */
export function applyJournalReset(state: HostState, hostHead: number, now: number): HostState {
  const timelines: Record<string, TimelineState> = {};
  for (const k of Object.keys(state.timelines)) timelines[k] = { items: [], hasMore: true, loaded: false };
  return {
    ...state,
    journalHead: hostHead,
    timelines,
    queues: {},
    resolvedAttention: {},
    gaps: [...state.gaps.slice(-(MAX_GAPS - 1)), { requestedFrom: hostHead + 1, oldestRetained: hostHead + 1, at: now }],
  };
}

export function isTerminalRunState(s: Run["state"]): boolean {
  return s === "completed" || s === "failed" || s === "interrupted" || s === "reconciliation_required";
}

function clearLiveForRun(state: HostState, runId: string): HostState {
  const entries = Object.entries(state.liveText).filter(([, v]) => v.runId !== runId);
  if (entries.length === Object.keys(state.liveText).length) return state;
  return { ...state, liveText: Object.fromEntries(entries) };
}

function supersedeLiveText(state: HostState, runId: string, finalText: string): HostState {
  const entries = Object.entries(state.liveText);
  const keep = entries.filter(([, v]) => !(v.runId === runId && finalText.startsWith(v.text.trimEnd())));
  if (keep.length === entries.length) {
    // No streamed prefix matched; drop the oldest stream for this run so a
    // finished message never shows twice.
    const oldest = entries.filter(([, v]) => v.runId === runId).sort((a, b) => a[1].at - b[1].at)[0];
    if (!oldest) return state;
    return { ...state, liveText: Object.fromEntries(entries.filter(([k]) => k !== oldest[0])) };
  }
  return { ...state, liveText: Object.fromEntries(keep) };
}

// ---- Selectors -----------------------------------------------------------

/** Latest update per task and kind. Distinct pending decisions remain reachable. */
export const unresolvedAttention = (s: HostState): AttentionItem[] => [...attentionOf(s)];

/** What the attention selectors read. Many views ask on every store update,
 * and most updates (live text, pongs, timeline items) leave all of it alone. */
type AttentionInputs = Pick<HostState, "attention" | "tasks" | "approvals" | "runs" | "control" | "targets">;
const attentionMemo = new WeakMap<HostState["attention"], { inputs: AttentionInputs; items: AttentionItem[] }>();

function attentionOf(s: HostState): AttentionItem[] {
  const memo = attentionMemo.get(s.attention);
  if (memo && memo.inputs.tasks === s.tasks && memo.inputs.approvals === s.approvals && memo.inputs.runs === s.runs && memo.inputs.control === s.control && memo.inputs.targets === s.targets) return memo.items;
  const inputs: AttentionInputs = { attention: s.attention, tasks: s.tasks, approvals: s.approvals, runs: s.runs, control: s.control, targets: s.targets };
  const items = computeAttention(inputs);
  attentionMemo.set(s.attention, { inputs, items });
  return items;
}

function computeAttention(s: AttentionInputs): AttentionItem[] {
  const latest = new Map<string, AttentionItem>();
  for (const a of Object.values(s.attention)) {
    if ((a.task_id && s.tasks[a.task_id]?.state === "archived") || a.resolved_at !== null || (a.approval_id && s.approvals[a.approval_id] && s.approvals[a.approval_id]?.state !== "pending")) continue;
    const key = a.task_id ? `${a.task_id}/${a.child_id ? a.run_id ?? "" : ""}/${a.child_id ?? ""}/${a.kind}/${a.approval_id ?? ""}` : a.attention_id;
    const prev = latest.get(key);
    if (!prev || a.created_at > prev.created_at || (a.created_at === prev.created_at && a.attention_id > prev.attention_id)) latest.set(key, a);
  }
  // The host normally publishes an approval or waiting item with this state.
  // Older/incomplete projections must still expose the person's next action.
  for (const task of Object.values(s.tasks)) {
    const target = handBackTarget(s, task);
    const run = s.runs[task.current_run ?? ""];
    if ((!target && (task.state !== "awaiting_decision" || (run && (run.ended_at !== null || isTerminalRunState(run.state))))) || pendingApprovals(s, task.task_id).length ||
      [...latest.values()].some(a => a.task_id === task.task_id && a.kind !== "completed" && a.kind !== "failure")) continue;
    const attention_id = `waiting:${task.task_id}`;
    latest.set(attention_id, { attention_id, task_id: task.task_id, run_id: task.current_run,
      approval_id: null, kind: "waiting_for_you", title: task.title,
      body: target ? handBackReason(target) : task.status_line, priority: 70, created_at: task.updated_at, resolved_at: null });
  }
  return [...latest.values()].map(a => {
    const task = a.task_id ? s.tasks[a.task_id] : undefined;
    if (!task) return a;
    const child = a.child_id && a.run_id ? s.runs[a.run_id]?.children.find(c => c.child_id === a.child_id) : undefined;
    const title = a.child_id
      ? child ? `${task.title} · helper ‘${child.label}’${a.kind === "reconciliation_required" ? " · Outcome unknown" : ""}` : `${task.title} · ${a.title}`
      : task.title;
    const target = !a.child_id && a.kind === "waiting_for_you" ? handBackTarget(s, task) : undefined;
    const body = target ? handBackReason(target) : a.body;
    return a.title === title && a.body === body ? a : { ...a, title, body };
  }).sort((a, b) => b.priority - a.priority || b.created_at - a.created_at);
}

export const pendingApprovals = (s: Pick<HostState, "approvals">, taskId?: string): Approval[] => {
  const pending = pendingOf(s.approvals);
  return [...(taskId === undefined ? pending.all : pending.byTask.get(taskId) ?? [])];
};

/** Pending approvals, oldest request first, sorted once per approvals record:
 * task rows ask for their own on every store update. */
const pendingMemo = new WeakMap<HostState["approvals"], { all: Approval[]; byTask: Map<string, Approval[]> }>();

function pendingOf(approvals: HostState["approvals"]): { all: Approval[]; byTask: Map<string, Approval[]> } {
  let memo = pendingMemo.get(approvals);
  if (!memo) {
    const all = Object.values(approvals).filter((a) => a.state === "pending").sort((a, b) => a.requested_at - b.requested_at);
    const byTask = new Map<string, Approval[]>();
    for (const a of all) {
      const list = byTask.get(a.task_id);
      if (list) list.push(a);
      else byTask.set(a.task_id, [a]);
    }
    memo = { all, byTask };
    pendingMemo.set(approvals, memo);
  }
  return memo;
}

/** A task's runs in the order the runs record lists them. */
export const taskRuns = (s: Pick<HostState, "runs">, taskId: string): Run[] => [...(runsByTask(s.runs).get(taskId) ?? [])];

/** Each task's runs, grouped once per runs record: the conversation and its
 * plan ask for them on every store update. */
const runsMemo = new WeakMap<HostState["runs"], Map<string, Run[]>>();

function runsByTask(runs: HostState["runs"]): Map<string, Run[]> {
  let byTask = runsMemo.get(runs);
  if (!byTask) {
    byTask = new Map();
    for (const r of Object.values(runs)) {
      const list = byTask.get(r.task_id);
      if (list) list.push(r);
      else byTask.set(r.task_id, [r]);
    }
    runsMemo.set(runs, byTask);
  }
  return byTask;
}

export const projectTasks = (s: HostState, projectId: string): Task[] =>
  Object.values(s.tasks)
    .filter((t) => t.project_id === projectId)
    .sort((a, b) => b.updated_at - a.updated_at);

export const liveTextForTask = (s: HostState, taskId: string): LiveText[] =>
  Object.values(s.liveText)
    .filter((l) => l.taskId === taskId)
    .sort((a, b) => a.at - b.at);

/** Unfinished task-list membership includes queued accepted tasks and uses the task summary, unlike isTaskRunning. */
export function isActiveTask(t: Task): boolean {
  return t.state === "accepted" || t.state === "dispatching" || t.state === "working" || t.state === "awaiting_decision";
}

/** A turn that ended while helpers it started still run. The computer keeps
 * the project's writer for them, and Stop still reaches them, until they end. */
export function helpersFinishing(run: Run): boolean {
  return run.ended_at !== null && run.holds_writer && ["completed", "failed", "interrupted"].includes(run.state);
}

/** A turn that ended while something it started may still run: helpers a
 * Work turn left, or, in any mode, what the computer hasn't yet confirmed
 * ended. Stop and Force stop still reach it until the computer says so. */
export function stillFinishing(run: Run): boolean {
  return helpersFinishing(run)
    || (run.ended_at !== null && !!run.cleanup_pending && ["completed", "failed", "interrupted"].includes(run.state));
}

/** Tasks the computer reports as running, for every Stop, strip and badge. A
 * known current run's native state takes precedence over the task's older
 * summary, so an ended run never keeps a stale badge alive, unless something
 * it started is still finishing. Until that run's record arrives, the task's
 * own state decides: hiding a Stop for work that may be running is the worse
 * error. */
export function isTaskRunning(s: HostState, task: Task): boolean {
  // Unlike isActiveTask, excludes queued-only accepted tasks and follows native run state when it disagrees with the task summary.
  const run = task.current_run ? s.runs[task.current_run] : undefined;
  if (!run) return ["dispatching", "working", "awaiting_decision"].includes(task.state);
  return run.task_id === task.task_id && (stillFinishing(run) || (run.ended_at === null
    && ["admitted", "dispatching", "working", "awaiting_approval", "awaiting_input", "stopping"].includes(run.state)));
}

export const runningTasks = (s: HostState): Task[] => Object.values(s.tasks).filter((task) => isTaskRunning(s, task));

/** Badges count blockers; results have a separate review list. */
export const openAttention = (s: HostState): AttentionItem[] => attentionOf(s).filter(a => !isResult(s, a));
export const reviewAttention = (s: HostState): AttentionItem[] => attentionOf(s).filter(a => isResult(s, a) && !inConversation(s, a));
/** Replies in Chat's conversations not yet on screen; Tasks' results are tasks'. */
export const replyAttention = (s: HostState): AttentionItem[] => attentionOf(s).filter(a => isResult(s, a) && inConversation(s, a));
const inConversation = (s: HostState, a: AttentionItem): boolean => !!(a.task_id && s.tasks[a.task_id]?.conversation);
/** What a finished task says is part of its result, unread until the task is
 * opened, and never waits under Needs you: a finished task is in Finished.
 * An approval or question waits until it's decided, and an outcome nobody
 * knows until it's checked, whatever its task. */
export function isResult(s: Pick<HostState, "tasks">, a: AttentionItem): boolean {
  if (a.kind === "completed" || a.kind === "failure") return true;
  if (a.approval_id || a.kind === "approval" || a.kind === "question" || a.kind === "reconciliation_required") return false;
  const state = a.task_id ? s.tasks[a.task_id]?.state : undefined;
  return state === "completed" || state === "failed" || state === "interrupted";
}

/** When each task last did something that moves its project up the list:
 * it was made, a turn of it started or ended, or it asked for the person.
 * The steps an agent takes in between, each status line, don't. */
export function tasksMovedAt(s: HostState): Record<string, number> {
  const out: Record<string, number> = {};
  const mark = (taskId: string | null, at: number | null) => {
    if (taskId !== null && at !== null) out[taskId] = Math.max(out[taskId] ?? 0, at);
  };
  for (const t of Object.values(s.tasks)) mark(t.task_id, t.created_at);
  for (const [taskId, at] of Object.entries(runsMovedAt(s.runs))) mark(taskId, at);
  for (const a of Object.values(s.attention)) mark(a.task_id, a.created_at);
  return out;
}

// Streamed text changes the state many times a second but never its runs, so
// the runs are walked once per change to them, not once per word.
const runsMoved = new WeakMap<HostState["runs"], Record<string, number>>();
function runsMovedAt(runs: HostState["runs"]): Record<string, number> {
  let out = runsMoved.get(runs);
  if (!out) {
    out = {};
    for (const r of Object.values(runs))
      for (const at of [r.admitted_at, r.ended_at])
        if (at !== null) out[r.task_id] = Math.max(out[r.task_id] ?? 0, at);
    runsMoved.set(runs, out);
  }
  return out;
}
