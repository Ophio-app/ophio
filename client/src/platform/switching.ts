// SPDX-License-Identifier: Apache-2.0
import type { HostProfile, Platform } from "../host/link";
import { HostSession, type PendingCommand } from "../host/session";
import type { HostState } from "../state/projection";
import type { Command, Snapshot } from "../protocol/types";
import { Store } from "../state/store";

export interface SwitchRisk {
  leases: { leaseId: string; targetId: string }[];
  privateTargets: string[];
  stoppingTasks: string[];
  pendingInput: { commandId: string; targetId: string }[];
}
const terminal = new Set(["completed", "failed", "interrupted", "archived"]);
export function switchRisk(state: HostState, commands: Record<string, PendingCommand>, deviceId: string): SwitchRisk {
  const leases = Object.values(state.control).flatMap((view) => {
    const lease = view.lease;
    return lease?.owner.kind === "human" && lease.owner.device_id === deviceId && !["revoked", "expired"].includes(lease.state)
      ? [{ leaseId: lease.lease_id, targetId: view.target_id }] : [];
  });
  const privateTargets = Object.values(state.control).filter((v) => v.private_step !== null).map((v) => v.target_id);
  const stoppingTasks = new Set(Object.values(state.runs).filter((r) => r.state === "stopping").map((r) => r.task_id));
  const pendingInput: SwitchRisk["pendingInput"] = [];
  for (const c of Object.values(commands)) {
    if (["rejected", "not_delivered"].includes(c.phase)) continue;
    const stage = state.commandStages[c.id]?.stage;
    if (c.body.command === "stop_task" && !terminal.has(state.tasks[c.body.task_id]?.state ?? "") && stage !== "observed") stoppingTasks.add(c.body.task_id);
    if (stage !== "observed" && stage !== "failed" && (c.body.command === "request_human_lease" || c.body.command === "take_over"))
      pendingInput.push({ commandId: c.id, targetId: c.body.target_id });
  }
  return { leases, privateTargets, stoppingTasks: [...stoppingTasks], pendingInput };
}
export const hasRisk = (risk: SwitchRisk): boolean => !!(risk.leases.length + risk.privateTargets.length + risk.stoppingTasks.length + risk.pendingInput.length);
export function remainingRisk(risk: SwitchRisk, snapshot: Snapshot, deviceId: string): SwitchRisk {
  return {
    leases: risk.leases.filter(({ leaseId, targetId }) => {
      const lease = snapshot.control.find((v) => v.target_id === targetId)?.lease;
      return lease?.lease_id === leaseId && lease.owner.kind === "human" && lease.owner.device_id === deviceId && !["revoked", "expired"].includes(lease.state);
    }),
    privateTargets: risk.privateTargets.filter((id) => snapshot.control.some((v) => v.target_id === id && v.private_step !== null)),
    stoppingTasks: risk.stoppingTasks.filter((id) => !terminal.has(snapshot.tasks.find((t) => t.task_id === id)?.state ?? "")),
    // Dispatch may still finish after a snapshot. Only command-status can settle these.
    pendingInput: risk.pendingInput,
  };
}
export interface RetainedControl {
  profileId: string;
  risk: SwitchRisk;
  since: number;
  detail: string;
  busy: boolean;
}
const KEY = "unresolved-computers.v1";
function read(): RetainedControl[] {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    if (!Array.isArray(raw)) return [];
    return raw.filter((r): r is RetainedControl => {
      if (!r || typeof r !== "object" || typeof r.profileId !== "string" || typeof r.since !== "number" || !r.risk) return false;
      const risk = r.risk as SwitchRisk;
      return Array.isArray(risk.leases) && risk.leases.every((l) => typeof l.leaseId === "string" && typeof l.targetId === "string")
        && Array.isArray(risk.privateTargets) && risk.privateTargets.every((id) => typeof id === "string")
        && Array.isArray(risk.stoppingTasks) && risk.stoppingTasks.every((id) => typeof id === "string")
        && Array.isArray(risk.pendingInput) && risk.pendingInput.every((c) => typeof c.commandId === "string" && typeof c.targetId === "string");
    }).map((r) => ({ ...r, busy: false, detail: "The computer hasn’t confirmed the previous handoff." }));
  } catch { return []; }
}
async function bounded<T>(work: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try { return await Promise.race([work, new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error("The computer hasn’t confirmed the request.")), 6500); })]); }
  finally { clearTimeout(timer); }
}

/** Each retained session keeps its own projection and command IDs. It never renews input. */
export class Handoffs extends Store<RetainedControl[]> {
  readonly sessions = new Map<string, HostSession>();
  #offs = new Map<string, () => void>();
  activeProfileId: string | null = null;
  constructor(readonly platform: Platform, public profiles: HostProfile[]) {
    super(read().filter((r) => profiles.some((p) => p.profileId === r.profileId)));
    this.subscribe(() => {
      try { localStorage.setItem(KEY, JSON.stringify(this.get().map((r) => ({ ...r, busy: false })))); }
      catch { /* Current-session controls remain reachable even when storage is full. */ }
    });
  }
  restore(): void {
    for (const r of this.get()) {
      // The computer in use already has its connection; a second one to the
      // same computer is refused by the native layer.
      if (this.sessions.has(r.profileId)) continue;
      const session = new HostSession(r.profileId, this.platform.control(r.profileId));
      this.attach(r.profileId, session); session.start();
    }
  }
  attach(profileId: string, session: HostSession): void {
    this.#offs.get(profileId)?.();
    this.sessions.set(profileId, session);
    let queued = false;
    const check = () => {
      if (!session.online || !session.state.get().caughtUp || queued) return;
      queued = true;
      queueMicrotask(() => { queued = false; void this.refresh(profileId); });
    };
    const off = session.link.subscribe(check), offState = session.state.subscribe(check);
    this.#offs.set(profileId, () => { off(); offState(); });
  }
  retain(profile: HostProfile, session: HostSession, risk: SwitchRisk): void {
    // What was already kept for this computer may be known only here (a Stop
    // sent before a restart); refresh drops what a snapshot settles.
    const kept = this.get().find((r) => r.profileId === profile.profileId)?.risk;
    if (kept) risk = {
      leases: [...kept.leases.filter((l) => !risk.leases.some((n) => n.leaseId === l.leaseId)), ...risk.leases],
      privateTargets: [...new Set([...kept.privateTargets, ...risk.privateTargets])],
      stoppingTasks: [...new Set([...kept.stoppingTasks, ...risk.stoppingTasks])],
      pendingInput: [...kept.pendingInput.filter((c) => !risk.pendingInput.some((n) => n.commandId === c.commandId)), ...risk.pendingInput],
    };
    this.set((rows) => [...rows.filter((r) => r.profileId !== profile.profileId), {
      profileId: profile.profileId, risk, since: Date.now(), busy: false,
      detail: "The computer hasn’t confirmed that control ended.",
    }]);
    this.attach(profile.profileId, session);
  }
  async release(profileId: string): Promise<void> {
    const row = this.get().find((r) => r.profileId === profileId), session = this.sessions.get(profileId);
    if (!row || !session || row.busy) return;
    this.update(profileId, { busy: true });
    try {
      if (!session.online) { session.reconnect(); throw new Error("The computer is offline. Control from this device may stay on until the computer times it out."); }
      // Release the phone's specific lease, never transfer or resume an agent lease.
      await Promise.all(row.risk.leases.map((l) => this.send(session, { command: "release_lease", lease_id: l.leaseId })));
      for (const target_id of new Set([...row.risk.pendingInput.map((p) => p.targetId), ...row.risk.privateTargets]))
        await this.send(session, { command: "pause_input", target_id });
    } catch (e) { this.update(profileId, { detail: e instanceof Error ? e.message : "The computer hasn’t confirmed the handoff." }); }
    finally { this.update(profileId, { busy: false }); }
    await this.refresh(profileId);
    // Receipt admission is not lease revocation. Keep this confirmation visible
    // until a fresh snapshot proves release, or retain the unresolved banner.
    if (session.online && this.get().some((r) => r.profileId === profileId && r.risk.leases.length)) {
      await new Promise<void>((resolve) => {
        const done = () => { clearTimeout(timer); off(); resolve(); };
        const off = this.subscribe(() => {
          if (!this.get().some((r) => r.profileId === profileId && r.risk.leases.length)) done();
        });
        const timer = setTimeout(done, 1500);
      });
    }
  }
  async send(session: HostSession, body: Command): Promise<void> {
    await bounded(session.command(body, { label: body.command === "stop_task" ? "Stop task on previous computer" : "Leave control before switching" }));
  }
  async stop(profileId: string): Promise<void> {
    const row = this.get().find((r) => r.profileId === profileId), session = this.sessions.get(profileId);
    if (!row || !session || row.busy) return;
    this.update(profileId, { busy: true });
    try {
      const tasks = new Set(row.risk.stoppingTasks);
      for (const task of Object.values(session.state.get().tasks))
        if (task.target_id && row.risk.privateTargets.includes(task.target_id) && !terminal.has(task.state)) tasks.add(task.task_id);
      await Promise.all([...tasks].map((task_id) => this.send(session, { command: "stop_task", task_id })));
      for (const l of row.risk.leases) await this.send(session, { command: "release_lease", lease_id: l.leaseId });
      for (const target_id of new Set([...row.risk.pendingInput.map((p) => p.targetId), ...row.risk.privateTargets]))
        await this.send(session, { command: "pause_input", target_id });
    } catch (e) { this.update(profileId, { detail: e instanceof Error ? e.message : "The computer hasn’t confirmed Stop." }); }
    finally { this.update(profileId, { busy: false }); }
    await this.refresh(profileId);
  }
  #refreshing = new Set<string>();
  async refresh(profileId: string): Promise<void> {
    const row = this.get().find((r) => r.profileId === profileId), session = this.sessions.get(profileId);
    const profile = this.profiles.find((p) => p.profileId === profileId);
    if (!row || !session?.online || !profile || this.#refreshing.has(profileId)) return;
    this.#refreshing.add(profileId);
    try {
      let risk = row.risk;
      for (const pending of risk.pendingInput) {
        const status = await session.query({ kind: "command_status", command_id: pending.commandId });
        if (status.receipt?.status === "rejected" || status.stage === "observed" || status.stage === "failed") {
          const result = status.result ?? status.receipt?.result;
          risk = { ...risk, pendingInput: risk.pendingInput.filter((c) => c.commandId !== pending.commandId),
            leases: result?.kind === "lease_granted" ? [...risk.leases, { leaseId: result.lease.lease_id, targetId: result.lease.target_id }] : risk.leases };
        }
      }
      const snap = await session.query({ kind: "snapshot" });
      if (snap.host.host_id !== profile.hostId) return;
      const grants = session.state.get().grants;
      if ((risk.leases.length || risk.privateTargets.length) && !grants?.view) {
        this.update(profileId, { detail: "Access changed. This device can no longer confirm whether control ended." });
        return;
      }
      risk = remainingRisk(risk, snap, profile.deviceId);
      if (!hasRisk(risk)) this.dismiss(profileId, profileId !== this.activeProfileId);
      else this.update(profileId, { risk, detail: risk.privateTargets.length ? "A private step is still open. Return to it to review what was typed and confirm it." : "The computer hasn’t confirmed all controls or Stop requests are settled." });
    } catch { /* Keep the last known unresolved state; lack of a reply is not success. */ }
    finally { this.#refreshing.delete(profileId); }
  }
  update(id: string, patch: Partial<RetainedControl>): void { this.set((rows) => rows.map((r) => r.profileId === id ? { ...r, ...patch } : r)); }
  dismiss(id: string, stop = true): void {
    this.set((rows) => rows.filter((r) => r.profileId !== id));
    this.#offs.get(id)?.(); this.#offs.delete(id);
    if (stop) this.sessions.get(id)?.stop();
    this.sessions.delete(id);
  }
  owns(session: HostSession): boolean { return this.get().some((r) => r.profileId === session.profileId) && this.sessions.get(session.profileId) === session; }
  dispose(): void { for (const off of this.#offs.values()) off(); for (const s of this.sessions.values()) s.stop(); this.#offs.clear(); this.sessions.clear(); }
}
