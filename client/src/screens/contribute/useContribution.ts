// SPDX-License-Identifier: Apache-2.0
//
// One handoff on the computer the phone is connected to. Checking a handoff
// is inert, so it runs as soon as there is a computer and a folder; getting
// the source happens only when the person confirms their own check, and is
// then followed until the receiver settles. Every result is the receiver's:
// the phone never decides a handoff is fine.

import { useCallback, useEffect, useRef, useState } from "react";
import { shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { CONTRIBUTION_FEATURE } from "../../app/contributionFeature";
import { CommandError, NotConnectedError } from "../../host/session";
import { uuidv7 } from "../../protocol/ids";
import type { CommandResult, ContributionApproval, ContributionCapabilities, ContributionView, LibraryRoot } from "../../protocol/types";
import { useStore } from "../../state/store";
import { stageOf } from "./codes";
import { chooseFolder, chosenFolder, handoffs, type Handoff } from "./handoffs";

/** How long a person's check that a handoff is Clankerhub's counts, as the
 * receiver allows. */
export const CHECK_LIFETIME_S = 300;
/** The phone records the check this much earlier than it happened, so a
 * phone clock a little ahead of the computer's isn't refused as a check from
 * the future. It only shortens how long the check counts. */
const CLOCK_MARGIN_S = 30;
/** The one publisher this receiver profile knows: the person's check names it. */
export const PUBLISHER = "clankerhub";
const POLL_MS = 1500;

/** Whether the connected computer can take a handoff at all. */
export type Reach =
  | { state: "offline" }
  /** Its Øphio predates the receiver. */
  | { state: "old" }
  /** This device may not add projects there. */
  | { state: "denied" }
  | { state: "checking" }
  /** The receiver can't download source on that computer's system. */
  | { state: "unsupported" }
  | { state: "ready"; capabilities: ContributionCapabilities };

/** Library folders that can hold contribution projects: ones Øphio may
 * write as a whole, that aren't a project's own or an agent's files. */
export function contributionFolders(roots: LibraryRoot[]): LibraryRoot[] {
  return roots.filter((r) => r.writable && !r.only && r.project_id === null && r.harness === null
    && (r.kind === "project" || r.kind === "notes" || r.kind === "drafts"));
}

const viewOf = (r: CommandResult | null | undefined): ContributionView | null => (r?.kind === "contribution" ? r.operation : null);

/** A request the receiver refused outright, before it had an operation: its
 * code is the message (`invalid_request`), or this device lacks permission. */
function refusalOf(e: unknown): string | null {
  if (!(e instanceof CommandError)) return null;
  if (e.error.code === "permission_denied") return "permission";
  if (e.error.code === "invalid_request" && /^[a-z_]{3,40}$/.test(e.error.message)) return e.error.message;
  return null;
}

const busyCode = (v: ContributionView | null): boolean =>
  !!v && (v.code === "acquiring" || v.code === "verifying" || v.interruption_pending);

export interface Contribution {
  handoff: Handoff | undefined;
  hostId: string | null;
  reach: Reach;
  folders: LibraryRoot[];
  /** The chosen folder, while it can still hold contribution projects. */
  folder: LibraryRoot | null;
  /** The receiver's word on this handoff, from this computer. */
  view: ContributionView | null;
  /** What the phone is waiting on the computer for. */
  pending: null | "review" | "prepare" | "cancel";
  /** The receiver refused the request itself: a code, or "permission". */
  refused: string | null;
  /** A request that didn't reach the computer, or whose outcome is unknown. */
  sendError: "offline" | "unknown" | null;
  setFolder(rootId: string): void;
  review(): Promise<void>;
  /** Get the source, on the person's check made at `checkedAt` (ms). */
  prepare(checkedAt: number): Promise<boolean>;
  cancel(): Promise<void>;
  refresh(): Promise<void>;
}

/** `follow: false` for a screen over the handoff's own, which already checks
 * and follows it: two would ask the computer everything twice. */
export function useContribution(id: string, { follow = true }: { follow?: boolean } = {}): Contribution {
  const { session } = useApp();
  const store = handoffs();
  const handoff = useStore(store, (all) => all.find((h) => h.id === id));
  const online = useLink().state === "online";
  const hostId = useHost((s) => s.hostId);
  const has = useHost((s) => s.capabilities?.features.includes(CONTRIBUTION_FEATURE) ?? false);
  const admin = useHost((s) => s.grants?.admin === true);
  const roots = useHost((s) => contributionFolders(s.libraryRoots), shallowEqual);
  const [capabilities, setCapabilities] = useState<{ hostId: string; value: ContributionCapabilities | null } | null>(null);
  const [pending, setPending] = useState<Contribution["pending"]>(null);
  const [refused, setRefused] = useState<string | null>(null);
  const [sendError, setSendError] = useState<Contribution["sendError"]>(null);
  // A check that couldn't be sent is tried again once the computer is back,
  // as the screen says; checking runs nothing.
  useEffect(() => { if (online) setSendError((e) => (e === "offline" ? null : e)); }, [online]);
  const [folderId, setFolderId] = useState(() => (hostId ? chosenFolder(hostId) : null));
  useEffect(() => { setFolderId(hostId ? chosenFolder(hostId) : null); }, [hostId]);
  const folder = roots.find((r) => r.root_id === folderId) ?? (roots.length === 1 ? roots[0]! : null);
  const view = handoff && hostId && handoff.hostId === hostId ? handoff.view : null;
  // The folder a reply was asked about, read when it arrives.
  const folderNow = useRef(folder);
  folderNow.current = folder;

  const reach: Reach = !online ? { state: "offline" }
    : !has ? { state: "old" }
    : !admin ? { state: "denied" }
    : !capabilities || capabilities.hostId !== hostId ? { state: "checking" }
    : capabilities.value?.supported ? { state: "ready", capabilities: capabilities.value }
    : { state: "unsupported" };

  useEffect(() => {
    if (!online || !has || !admin || !hostId) return;
    let live = true;
    session.query({ kind: "contribution_capabilities" }).then(
      (r) => { if (live) setCapabilities({ hostId, value: r.capabilities }); },
      () => { if (live) setCapabilities({ hostId, value: null }); },
    );
    return () => { live = false; };
  }, [session, online, has, admin, hostId]);

  /** Every request asks first: a companion without the receiver can't read
   * one and drops the connection, whatever this phone remembers. */
  const supported = useCallback(() => session.state.get().capabilities?.features.includes(CONTRIBUTION_FEATURE) === true, [session]);
  const generation = useCallback(() => store.find(id)?.generation ?? -1, [store, id]);
  /** A new request or choice: replies to earlier ones are out of date. */
  const next = useCallback((patch: Partial<Handoff> = {}) => {
    const g = generation() + 1;
    store.update(id, { ...patch, generation: g });
    return g;
  }, [store, id, generation]);

  /** Keep the receiver's answer, unless it would put a settled operation back
   * into progress: a poll sent before it settled can arrive after. */
  const keep = useCallback((v: ContributionView, patch: Partial<Handoff> = {}) => {
    const now = store.find(id);
    if (!now) return;
    const settled = now.view && now.view.operation_id === v.operation_id && !busyCode(now.view);
    if (settled && busyCode(v) && !patch.started) return;
    const started = now.started || patch.started || ["acquiring", "verifying", "prepared", "reopened"].includes(v.code);
    store.update(id, { ...patch, operationId: v.operation_id, view: v, started });
  }, [store, id]);

  const failed = useCallback((e: unknown) => {
    const code = refusalOf(e);
    if (code) { setRefused(code); return; }
    setSendError(e instanceof NotConnectedError ? "offline" : "unknown");
  }, []);

  const review = useCallback(async () => {
    const h = store.find(id);
    const root = folder;
    if (!h || !hostId || !root || !supported()) return;
    const g = next({ prepareCommand: null });
    setPending("review"); setRefused(null); setSendError(null);
    try {
      const receipt = await session.command({
        command: "review_contribution",
        input: { packet_json: h.packetJson, handoff_json: h.handoffJson, host_id: hostId, root_id: root.root_id },
      }, { label: "Check a handoff from Clankerhub" });
      // A reply about a folder or request since replaced is out of date.
      if (generation() !== g || folderNow.current?.root_id !== root.root_id) return;
      const v = viewOf(receipt.result);
      if (v) { store.update(id, { hostId, view: null }); keep(v); }
    } catch (e) {
      if (generation() === g) failed(e);
    } finally {
      setPending(null);
    }
  }, [session, store, id, hostId, folder, supported, next, generation, keep, failed]);

  const refresh = useCallback(async () => {
    const h = store.find(id);
    if (!h?.operationId || h.hostId !== hostId || !supported()) return;
    const g = h.generation;
    try {
      const r = await session.query({ kind: "contribution_operation", operation_id: h.operationId });
      if (generation() === g) keep(r.operation);
    } catch (e) {
      // The computer no longer holds it: check the handoff afresh.
      if (generation() === g && e instanceof CommandError && e.error.code === "not_found") store.update(id, { operationId: null, view: null });
    }
  }, [session, store, id, hostId, supported, generation, keep]);

  /** Ask the computer what became of a download request whose answer never
   * arrived, rather than assuming either way. */
  const reconcile = useCallback(async () => {
    const h = store.find(id);
    const command = h?.unsettled;
    if (!h || !command || h.hostId !== hostId || !supported()) return;
    if (session.commands.get()[command]?.phase === "sending") return;
    const g = h.generation;
    try {
      const r = await session.query({ kind: "command_status", command_id: command });
      if (generation() !== g || store.find(id)?.unsettled !== command) return;
      const v = viewOf(r.result ?? r.receipt?.result);
      // Its admission, or its outcome: either way the download was asked for.
      if (v) { keep(v, { unsettled: null, started: true }); setSendError(null); return; }
      if (r.receipt?.error) { store.update(id, { unsettled: null }); failed(new CommandError(r.receipt.error, null)); return; }
      // The computer never received it, so nothing started from it.
      if (!r.receipt) { store.update(id, { unsettled: null, prepareCommand: null }); setSendError(null); }
    } catch { /* Asked again when the computer is back. */ }
  }, [session, store, id, hostId, supported, generation, keep, failed]);

  const prepare = useCallback(async (checkedAt: number) => {
    const h = store.find(id);
    const v = h && h.hostId === hostId ? h.view : null;
    const ctx = v?.context;
    if (!h || !v || !ctx || h.unsettled || !supported()) return false;
    const checked = Math.floor(checkedAt / 1000) - CLOCK_MARGIN_S;
    const expires = h.expiresAt === null ? Number.MAX_SAFE_INTEGER : Math.floor(h.expiresAt / 1000);
    const approval: ContributionApproval = {
      publisher_id: PUBLISHER,
      packet_sha256: v.packet_sha256,
      // The fingerprint the person compared with the game's Clankerhub page.
      handoff_sha256: v.handoff_sha256,
      game_id: ctx.game_id,
      release_id: ctx.release_id,
      repository_url: ctx.repository_url,
      commit: ctx.commit,
      checked_at: checked,
      valid_until: Math.min(checked + CHECK_LIFETIME_S, expires),
    };
    // Kept before it's sent: a lost answer is asked about, never assumed.
    const commandId = uuidv7(Date.now());
    const g = h.generation + 1;
    if (!store.update(id, { generation: g, prepareCommand: commandId, unsettled: commandId }, { durable: true })) return false;
    setPending("prepare"); setRefused(null); setSendError(null);
    try {
      const receipt = await session.command({ command: "prepare_contribution", operation_id: v.operation_id, approval }, { label: "Get a game’s source", commandId });
      const result = viewOf(receipt.result);
      // Save the outcome and clear its recovery marker together. If storage
      // fails now, the saved command still survives a restart for reconciliation.
      if (result && generation() === g) keep(result, { unsettled: null, started: busyCode(result) ? true : undefined });
      return true;
    } catch (e) {
      // Not sent at all: nothing to ask about later.
      if (e instanceof NotConnectedError || refusalOf(e)) store.update(id, { unsettled: null, prepareCommand: null });
      if (generation() === g) failed(e);
      return false;
    } finally {
      setPending(null);
    }
  }, [session, store, id, hostId, supported, generation, keep, failed]);

  const cancel = useCallback(async () => {
    const h = store.find(id);
    if (!h?.operationId || h.hostId !== hostId || !supported()) return;
    const g = next();
    setPending("cancel"); setSendError(null);
    try {
      const receipt = await session.command({ command: "cancel_contribution", operation_id: h.operationId }, { label: "Stop getting a game’s source" });
      const v = viewOf(receipt.result);
      if (v && generation() === g) store.update(id, { view: v });
    } catch (e) {
      if (generation() === g) failed(e);
    } finally {
      setPending(null);
    }
  }, [session, store, id, hostId, supported, next, generation, failed]);

  const setFolder = useCallback((rootId: string) => {
    if (!hostId) return;
    chooseFolder(hostId, rootId);
    setFolderId(rootId);
    // Another folder is another workspace: the earlier check doesn't carry over.
    next({ hostId: null, operationId: null, view: null, prepareCommand: null });
  }, [hostId, next]);

  // Check the handoff once there is somewhere to put it, and again whenever
  // the screen opens on one still to approve: checking runs nothing, and the
  // receiver checks expiry and the folder afresh each time.
  const ready = reach.state === "ready";
  const folderReady = !!folder;
  const stage = view ? stageOf(view) : null;
  const unsettled = handoff?.unsettled ?? null;
  const opened = useRef(false);
  useEffect(() => {
    if (!follow || !ready || !folderReady || pending || unsettled) return;
    if (!view) { if (!refused && !sendError) { opened.current = true; void review(); } return; }
    if (opened.current) return;
    opened.current = true;
    if (stage === "review") void review();
    else if (stage !== "blocked") void refresh();
  }, [follow, ready, folderReady, view, stage, pending, unsettled, refused, sendError, review, refresh]);

  // A download request whose answer was lost is asked about once the
  // computer can be asked, including after a restart.
  useEffect(() => {
    if (follow && online && has && unsettled && !pending) void reconcile();
  }, [follow, online, has, unsettled, pending, reconcile]);

  // Follow a download or a stop until the receiver settles it.
  const following = follow && online && has && busyCode(view);
  useEffect(() => {
    if (!following) return;
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [following, refresh]);
  const prepareCommand = handoff?.prepareCommand ?? null;
  const settledResult = useHost((s) => (prepareCommand ? s.commandStages[prepareCommand]?.result : undefined));
  useEffect(() => {
    const v = viewOf(settledResult);
    if (v && store.find(id)?.prepareCommand === prepareCommand) keep(v);
  }, [settledResult, prepareCommand, store, id, keep]);

  return { handoff, hostId, reach, folders: roots, folder, view, pending, refused, sendError, setFolder, review, prepare, cancel, refresh };
}
