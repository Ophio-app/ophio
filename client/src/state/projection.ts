// SPDX-License-Identifier: Apache-2.0
//
// The phone's projection of host state. It is rebuilt only from a snapshot
// plus journal events in `seq` order, so everything the UI shows is traceable
// to something the host said. Nothing here guesses a newer state from a timer.

import type { ProjectChecksView } from "../protocol/generated/ProjectChecksView";
import type { CheckJob } from "../protocol/generated/CheckJob";
import { handBackReason, handBackTarget } from "./handBack";
import { applyDeliveries } from "./timeline";
import { withoutOutcome } from "./outcomeReport";
import type { ObservedReply } from "../protocol/generated/ObservedReply";
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
  /** UTF-8 byte position of the first cached delta, when supplied by the host. */
  offset?: number;
  bytes?: number;
  /** A durable partial shares this stream. Its range and the cached range
   * are projected together, without changing either observation. */
  timelineSeq?: number;
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
  /** Latest checks views read from the computer, including immediate local approvals. */
  checksViews?: Record<string, ProjectChecksView>;
  /** Check jobs positively identified as project holders, including taskless runs. */
  checkJobs?: Record<string, CheckJob>;
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
  let next = {
    ...state,
    timelines: {
      ...state.timelines,
      [taskId]: { items: merged, hasMore: prev?.loaded ? prev.hasMore && hasMore : hasMore, loaded: true },
    },
  };
  // Query pages can be the only delivery of a recovered partial after a
  // snapshot or a journal gap; they reconcile the cache just like events.
  for (const item of items) next = reconcileLiveText(next, taskId, item);
  return next;
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
      if (isTerminalRunState(e.run.state)) next = endLiveForRun(next, e.run.run_id, e.run.state);
      return next;
    }
    case "timeline_appended": {
      let next = reconcileLiveText(append(s, e.task_id, e.item), e.task_id, e.item);
      const body = e.item.item;
      if (body.kind === "run_ended") next = endLiveForRun(next, body.run_id, body.state);
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
      // Old leases are void and deltas can no longer be joined, but words this
      // phone already saw remain observations even if a checkpoint is older.
      return interruptLive({
        ...s,
        bootEpoch: e.boot_epoch,
        control: Object.fromEntries(
          Object.entries(s.control).map(([k, v]) => [
            k,
            { ...v, input_state: "unknown" as const, lease: v.lease ? { ...v.lease, state: "unknown" as const } : null },
          ]),
        ),
        activity: {},
      });
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
      const offset = live.text_offset;
      let value: LiveText = { taskId: live.task_id, runId: live.run_id, key: live.message_key, text: live.text, at: now,
        ...(offset === undefined ? {} : { offset, bytes: utf8Length(live.text) }) };
      if (prev) {
        value = { ...prev, at: now };
        if (!prev.interrupted) {
          if (offset === undefined || prev.offset === undefined) {
            value.text += live.text;
            if (value.bytes !== undefined) value.bytes += utf8Length(live.text);
          } else {
            const bytes = prev.bytes ?? utf8Length(prev.text);
            const end = prev.offset + bytes;
            if (offset === end) { value.text += live.text; value.bytes = bytes + utf8Length(live.text); }
            else {
              const overlapAt = utf8Index(prev.text, offset - prev.offset);
              const overlap = overlapAt === null ? null : prev.text.slice(overlapAt);
              // Offsets also make a repeated live frame harmless. A missing or
              // conflicting range freezes the observation instead of splicing.
              if (overlap !== null && live.text.startsWith(overlap)) {
                const tail = live.text.slice(overlap.length);
                value.text += tail; value.bytes = bytes + utf8Length(tail);
              } else if (!(overlap !== null && overlap.startsWith(live.text))) value.interrupted = true;
            }
          }
        }
      }
      return { ...state, lastHeardAt: now, liveText: { ...state.liveText, [key]: value } };
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
    liveText: {},
    activity: {},
    resolvedAttention: {},
    gaps: [...state.gaps.slice(-(MAX_GAPS - 1)), { requestedFrom: hostHead + 1, oldestRetained: hostHead + 1, at: now }],
  };
}

export function isTerminalRunState(s: Run["state"]): boolean {
  return s === "completed" || s === "failed" || s === "interrupted" || s === "reconciliation_required";
}

function endLiveForRun(state: HostState, runId: string, status: Run["state"]): HostState {
  if (status === "completed") return clearLiveForRun(state, runId);
  const entries = Object.entries(state.liveText);
  if (!entries.some(([, v]) => v.runId === runId && !v.interrupted)) return state;
  return { ...state, liveText: Object.fromEntries(entries.map(([k, v]) => [k, v.runId === runId ? { ...v, interrupted: true } : v])) };
}

function clearLiveForRun(state: HostState, runId: string): HostState {
  const entries = Object.entries(state.liveText).filter(([, v]) => v.runId !== runId);
  if (entries.length === Object.keys(state.liveText).length) return state;
  return { ...state, liveText: Object.fromEntries(entries) };
}

const utf8Length = (text: string): number => new TextEncoder().encode(text).length;

/** Convert a byte position only at an actual UTF-8 character boundary. */
function utf8Index(text: string, bytes: number): number | null {
  if (!Number.isSafeInteger(bytes) || bytes < 0) return null;
  let at = 0;
  let index = 0;
  for (const char of text) {
    if (at === bytes) return index;
    const point = char.codePointAt(0)!;
    at += point <= 0x7f ? 1 : point <= 0x7ff ? 2 : point <= 0xffff ? 3 : 4;
    if (at > bytes) return null;
    index += char.length;
  }
  return at === bytes ? index : null;
}

function utf8Slice(text: string, from: number, to: number): string | null {
  const start = utf8Index(text, from);
  const end = utf8Index(text, to);
  return start === null || end === null ? null : text.slice(start, end);
}

function reconcileLiveText(state: HostState, taskId: string, item: TimelineItem): HostState {
  const body = item.item;
  if (body.kind !== "agent_message" || !item.run_id) return state;
  const entries = Object.entries(state.liveText);
  const candidates = entries.filter(([, v]) => v.runId === item.run_id && v.taskId === taskId);
  if (body.message_key !== undefined) {
    const match = candidates.find(([, v]) => v.key === body.message_key);
    if (!match) return state;
    const [key, live] = match;
    if (!body.partial) {
      return { ...state, liveText: Object.fromEntries(entries.filter(([k]) => k !== key)) };
    }
    if (body.text_offset === undefined || live.offset === undefined || !sharedRange(live, body.text, body.text_offset)) return state;
    if (live.timelineSeq !== undefined && live.timelineSeq >= item.seq) return state;
    return { ...state, liveText: { ...state.liveText, [key]: { ...live, timelineSeq: item.seq } } };
  }
  if (!body.partial) return supersedeLiveText(state, item.run_id, body.text);
  // Old computers cannot identify interleaved messages. Only an unambiguous
  // shared prefix is usable; never discard an unmatched stream for a partial.
  const matches = candidates.filter(([, v]) => (v.offset === undefined || v.offset === 0) && body.text.length > 0 && (body.text.startsWith(v.text) || v.text.startsWith(body.text)));
  if (matches.length !== 1) return state;
  const [key, live] = matches[0]!;
  if (body.text.startsWith(live.text)) {
    return { ...state, liveText: Object.fromEntries(entries.filter(([k]) => k !== key)) };
  }
  // A legacy message's first unique prefix anchors its following suffixes.
  // A repeated opening in a later suffix must not move that anchor forward.
  if (live.timelineSeq !== undefined && live.timelineSeq <= item.seq) return state;
  return { ...state, liveText: { ...state.liveText, [key]: { ...live, timelineSeq: item.seq } } };
}

/** Exact overlap, or adjacent ranges of the same identified message. */
function sharedRange(live: LiveText, text: string, offset: number): boolean {
  if (!Number.isSafeInteger(offset) || offset < 0 || live.offset === undefined) return false;
  const start = Math.max(live.offset, offset);
  const end = Math.min(live.offset + (live.bytes ?? utf8Length(live.text)), offset + utf8Length(text));
  if (start > end) return false;
  const cached = utf8Slice(live.text, start - live.offset, end - live.offset);
  return cached !== null && cached === utf8Slice(text, start - offset, end - offset);
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

type PartialRange = { item: TimelineItem; text: string; offset: number; end: number };

/** Without message identities only consecutive, exact suffixes are usable.
 * Stop at any boundary or text that another cached stream could own, rather
 * than guessing across helpers, completed messages or a later run. */
function legacyPartialRanges(timeline: TimelineState, cached: LiveText, live: LiveText[]): PartialRange[] {
  const anchor = timeline.items.findIndex(item => item.seq === cached.timelineSeq);
  if (anchor < 0 || (cached.offset !== undefined && cached.offset !== 0)) return [];
  const positioned = { ...cached, offset: 0 };
  const ranges: PartialRange[] = [];
  let offset = 0;
  const end = cached.bytes ?? utf8Length(cached.text);
  for (let index = anchor; index < timeline.items.length && offset < end; index++) {
    const item = timeline.items[index]!;
    const body = item.item;
    if (item.run_id !== cached.runId || body.kind !== "agent_message" || !body.partial
      || body.message_key !== undefined || !body.text || !sharedRange(positioned, body.text, offset)) break;
    // Even an exact suffix can be a helper's independent message. With no
    // durable key, preserve that ambiguity instead of borrowing its row.
    if (live.some(other => other !== cached && other.runId === cached.runId && other.text
      && (other.text.startsWith(body.text) || body.text.startsWith(other.text)
        || (index > anchor && other.text.includes(body.text))))) break;
    const next = offset + utf8Length(body.text);
    ranges.push({ item, text: body.text, offset, end: next });
    offset = next;
  }
  return ranges;
}

type ConversationProjection = { source: TimelineState | undefined; timeline: TimelineState | undefined; live: LiveText[] };
const conversationMemo = new WeakMap<HostState["liveText"], Map<string, ConversationProjection>>();

/** Keep locally observed tails on their original reply, including when a page
 * begins inside that message. Durable items themselves remain unchanged. */
function conversationProjection(s: HostState, taskId: string): ConversationProjection {
  let memo = conversationMemo.get(s.liveText);
  if (!memo) { memo = new Map(); conversationMemo.set(s.liveText, memo); }
  const timeline = s.timelines[taskId];
  const prior = memo.get(taskId);
  if (prior && prior.source === timeline) return prior;
  const live = Object.values(s.liveText).filter(l => l.taskId === taskId).sort((a, b) => a.at - b.at);
  const standalone: LiveText[] = [];
  const replacements = new Map<number, string>();
  for (const cached of live) {
    if (cached.timelineSeq === undefined || !timeline) { standalone.push(cached); continue; }
    const start = cached.offset ?? 0;
    const positioned = cached.offset === undefined ? { ...cached, offset: start } : cached;
    const end = start + (cached.bytes ?? utf8Length(cached.text));
    const anchor = timeline.items.find(item => item.seq === cached.timelineSeq);
    const legacy = anchor?.item.kind === "agent_message" && anchor.item.message_key === undefined;
    const matching = legacy ? legacyPartialRanges(timeline, cached, live) : timeline.items.flatMap(item => {
      if (item.run_id !== cached.runId || item.item.kind !== "agent_message" || !item.item.partial) return [];
      const body = item.item;
      const offset = body.message_key === cached.key ? body.text_offset : undefined;
      return offset !== undefined && sharedRange(positioned, body.text, offset) ? [{ item, text: body.text, offset, end: offset + utf8Length(body.text) }] : [];
    }).sort((a, b) => a.offset - b.offset || a.item.seq - b.item.seq);
    if (!matching.length) { standalone.push(cached); continue; }
    let covered = start;
    for (const part of matching) {
      const missingEnd = Math.min(part.offset, end);
      const prefix = covered < missingEnd ? utf8Slice(cached.text, covered - start, missingEnd - start) ?? "" : "";
      replacements.set(part.item.seq, prefix + part.text);
      covered = Math.max(covered, Math.min(part.end, end));
    }
    const last = matching.at(-1)!;
    const suffix = covered < end ? utf8Slice(cached.text, covered - start, end - start) ?? "" : "";
    replacements.set(last.item.seq, withoutOutcome(replacements.get(last.item.seq)! + suffix));
  }
  let projected = timeline;
  if (timeline && replacements.size) {
    let changed = false;
    const items = timeline.items.map(item => {
      const text = replacements.get(item.seq);
      if (text === undefined || item.item.kind !== "agent_message" || text === item.item.text) return item;
      changed = true;
      return { ...item, item: { ...item.item, text } };
    });
    if (changed) projected = { ...timeline, items };
  }
  const value = { source: timeline, timeline: projected, live: standalone };
  memo.delete(taskId);
  memo.set(taskId, value);
  if (memo.size > INACTIVE_TIMELINES + 1) memo.delete(memo.keys().next().value!);
  return value;
}

export const CHAT_REPLY_RECOVERY_FEATURE = "chat_reply_recovery_v1";
export const OBSERVED_REPLY_BYTES = 8 * 1024;

function boundedReplyTail(text: string): { text: string; removed: number } | null {
  const encoded = new TextEncoder().encode(text);
  let removed = Math.max(0, encoded.length - OBSERVED_REPLY_BYTES);
  // Start after continuation bytes, never inside an emoji or accent.
  while (removed < encoded.length && (encoded[removed]! & 0xc0) === 0x80) removed++;
  const index = utf8Index(text, removed);
  return index === null ? null : { text: text.slice(index), removed };
}

/** A bounded observation from this phone, never an amendment to host history.
 * Evaluate at send time so a newer run or a changed capability cannot inherit
 * an older reply's cached words. Only attach the newest stream when its bounded
 * ending verifiably joins a durable partial; omitting optional evidence must
 * not erase visible words or substitute an older stream for the newest one. */
export function observedReplyForTask(s: HostState, taskId: string): ObservedReply | undefined {
  if (!s.capabilities?.features.includes(CHAT_REPLY_RECOVERY_FEATURE)) return undefined;
  const task = s.tasks[taskId];
  const run = s.runs[task?.current_run ?? ""];
  if (!task?.conversation || !run || run.task_id !== taskId || run.state !== "reconciliation_required" || !run.nothing_running) return undefined;
  const cached = Object.values(s.liveText).filter(l => l.taskId === taskId && l.runId === run.run_id && l.interrupted
    && l.offset !== undefined && Number.isSafeInteger(l.offset) && l.offset >= 0 && l.key && l.text)
    .sort((a, b) => a.at - b.at).at(-1);
  if (!cached) return undefined;
  const parts = s.timelines[taskId]?.items.flatMap(item => item.run_id === run.run_id
    && item.item.kind === "agent_message" && item.item.message_key === cached.key ? [item.item] : []) ?? [];
  const latest = parts.at(-1);
  if (!latest?.partial || latest.text_offset === undefined || !Number.isSafeInteger(latest.text_offset) || latest.text_offset < 0) return undefined;
  let durable = latest.text;
  let durableStart = latest.text_offset;
  let durableBytes = utf8Length(durable);
  // Like the companion, use only the latest contiguous durable suffix of this
  // exact identity. An older matching segment cannot bridge a newer gap/final.
  for (let index = parts.length - 2; index >= 0 && durableBytes < OBSERVED_REPLY_BYTES; index--) {
    const part = parts[index]!;
    const length = utf8Length(part.text);
    if (!part.partial || part.text_offset === undefined || !Number.isSafeInteger(part.text_offset) || part.text_offset < 0
      || part.text_offset + length !== durableStart) break;
    durable = part.text + durable;
    durableStart = part.text_offset;
    durableBytes += length;
  }
  const anchor = boundedReplyTail(durable);
  const observed = boundedReplyTail(cached.text);
  if (!anchor || !observed) return undefined;
  const offset = cached.offset! + observed.removed;
  const anchorOffset = durableStart + anchor.removed;
  const length = utf8Length(observed.text);
  if (!Number.isSafeInteger(offset + length) || !Number.isSafeInteger(anchorOffset + utf8Length(anchor.text))
    || !sharedRange({ ...cached, text: observed.text, offset, bytes: length }, anchor.text, anchorOffset)) return undefined;
  return { run_id: run.run_id, message_key: cached.key, text_offset: offset, text: observed.text };
}

export const timelineForTask = (s: HostState, taskId: string): TimelineState | undefined => conversationProjection(s, taskId).timeline;

export const liveTextForTask = (s: HostState, taskId: string): LiveText[] => [...conversationProjection(s, taskId).live];

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
  // Ophio checks after the agent finishes; its ordinary Stop still applies.
  if (task.state === "working" && task.checking) return true;
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
