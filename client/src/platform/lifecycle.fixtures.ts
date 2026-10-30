// SPDX-License-Identifier: Apache-2.0
import type { ControlLink, HostProfile, LinkEvent, Platform } from "../host/link";
import type { Command, Snapshot } from "../protocol/types";
import * as fx from "../dev/sim/scenario";
import { allGrants } from "../dev/sim/host";
import { simulatedLifecycle } from "../dev/sim/lifecycle";
export const profile: HostProfile = { profileId: "test-a", hostId: fx.ids.host, name: "studio", addresses: ["localhost:47100"], certSha256: "a".repeat(64), pairedAt: 1, deviceId: fx.ids.device };
export function snapshot(): Snapshot {
  return { host: fx.hostInfo(100), journal_head: 0, projects: fx.projects(100), tasks: fx.tasks(100), runs: fx.runs(100), approvals: fx.approvals(100), attention: fx.attention(100), targets: fx.targets(), control: [], library_roots: [] };
}
export class FakeLink implements ControlLink {
  listeners = new Set<(e: LinkEvent) => void>();
  sent: Command[] = [];
  offline = false;
  released = true;
  stopped = false;
  grants = { ...allGrants };
  snap = snapshot();
  emit(e: LinkEvent) { for (const receive of this.listeners) receive(e); }
  connect() {
    if (this.offline) { this.emit({ type: "state", state: { state: "offline", since: 1, reason: "Offline", retryAt: null } }); return; }
    this.emit({ type: "authenticated", challenge: { selected: { major: 1, minor: 0 }, host_id: this.snap.host.host_id, boot_epoch: fx.ids.boot, server_nonce: "fixture" }, auth: { host_id: this.snap.host.host_id, device_id: profile.deviceId, grants: this.grants, capabilities: fx.capabilities(), companion_version: "test", journal_head: 0 } });
  }
  disconnect() { this.stopped = true; }
  listen(receive: (e: LinkEvent) => void) { this.listeners.add(receive); return () => { this.listeners.delete(receive); }; }
  send(frame: Parameters<ControlLink["send"]>[0]) {
    if (this.offline) return false;
    queueMicrotask(() => {
      if (frame.type === "query") {
        const result = frame.query.kind === "snapshot" ? { kind: "snapshot" as const, ...this.snap } : { kind: "harnesses" as const, harnesses: [] };
        this.emit({ type: "frame", frame: { type: "query_result", query_id: frame.query_id, result: { Ok: result } } });
      } else if (frame.type === "subscribe") {
        this.emit({ type: "frame", frame: { type: "caught_up", head: 0 } });
      } else if (frame.type === "command") {
        this.sent.push(frame.body);
        if (frame.body.command === "release_lease" && this.released) this.snap.control = [];
        this.emit({ type: "frame", frame: { type: "receipt", command_id: frame.command_id, status: "accepted", seq: null, result: null, error: null } });
      }
    });
    return true;
  }
}
export function platform(links: FakeLink[] = []) {
  const lifecycle = simulatedLifecycle(true);
  const value: Platform = {
    kind: "simulated", ...lifecycle.capabilities, openExternal: async () => {},
    profiles: async () => [profile], forget: async () => {},
    parsePairingLink: () => { throw new Error("unused"); }, pair: async () => { throw new Error("unused"); }, cancelPairing: async () => {},
    renameProfile: async () => { throw new Error("unused"); },
    control: () => { const l = new FakeLink(); links.push(l); return l; },
    media: () => { throw new Error("unused"); }, terminal: () => { throw new Error("unused"); }, speech: () => { throw new Error("unused"); }, transfer: () => { throw new Error("unused"); },
  };
  return { value, lifecycle };
}
