// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ContributionCode, ContributionView } from "../../protocol/types";
import { blockerFor, roleLabel, stageOf } from "./codes";
import { busy, Handoffs, RECORD_LIMIT, resetHandoffs } from "./handoffs";

// Every code the receiver can send, so a new one fails here until it has words.
const CODES: Record<ContributionCode, true> = {
  awaiting_local_approval: true, acquiring: true, verifying: true, prepared: true, reopened: true, canceled: true, reconcile_required: true,
  invalid_record: true, invalid_json: true, unsupported_version: true, not_yet_valid: true, invalid_lifetime: true, catalog_digest_mismatch: true,
  limit_exceeded: true, untrusted_packet: true, expired: true, freshness_unknown: true, handoff_digest_mismatch: true, release_mismatch: true,
  stale_policy: true, compatibility_unknown: true, compatibility_unsupported: true, host_unavailable: true, root_changed: true, trust_required: true,
  id_conflict: true, workspace_conflict: true, unsafe_repository: true, unsafe_path: true, unsafe_object: true, private_destination: true,
  acquisition_failed: true, disk_full: true, busy: true,
};
const view = (code: ContributionCode, project: string | null = null): ContributionView => ({
  operation_id: "op", code, packet_sha256: "", handoff_sha256: "", context: null, project_id: project, dirty: null, missing_capabilities: [], interruption_pending: false, execution_authorized: false,
});
const facts = { computer: "studio", folder: "Notes", started: false };

it("gives every blocking code words that say what happened and what didn't, with a way on", () => {
  for (const code of Object.keys(CODES) as ContributionCode[]) {
    if (stageOf(view(code)) !== "blocked") continue;
    const b = blockerFor(code, facts);
    expect(b.heading, code).not.toBe("Something unexpected");
    expect(b.body, code).toMatch(/Nothing (was|is)|nothing was|stopped|stays|untouched/i);
    expect(b.primary.label, code).toBeTruthy();
  }
});

it("never claims nothing was downloaded once Øphio started getting the source", () => {
  for (const code of Object.keys(CODES) as ContributionCode[]) {
    if (stageOf(view(code)) !== "blocked") continue;
    expect(blockerFor(code, { ...facts, started: true }).body, code).not.toMatch(/Nothing was downloaded/);
  }
  expect(blockerFor("acquisition_failed", { ...facts, started: true }).body).toMatch(/stays in its own folder on studio/);
});

it("stops on a code this version doesn't know, and shows the code only as a reference", () => {
  const b = blockerFor("mystery_state", facts);
  expect(b.heading).toBe("Something unexpected");
  expect(b.reference).toBe("mystery_state");
  expect(b.body).not.toContain("mystery_state");
  expect(b.primary.remedy).toBe("dismiss");
});

it("reads a prepared result whose check has lapsed as the project it added, not a new blocker", () => {
  expect(stageOf(view("freshness_unknown", "p1"))).toBe("ready");
  expect(stageOf(view("freshness_unknown"))).toBe("blocked");
  expect(stageOf(view("trust_required"))).toBe("review");
});

it("names the rule files by role", () => {
  expect(["instructions", "contribution", "model-policy", "design", "setup", "other"].map(roleLabel))
    .toEqual(["Agent instructions", "Contributing", "AI policy", "Design", "Setup", "Rules"]);
});

describe("handoffs kept on the phone", () => {
  beforeEach(() => { localStorage.clear(); resetHandoffs(); });
  afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); });
  const packet = (expiresAt: number) => JSON.stringify({ expiresAt, issuedAt: expiresAt - 600 });

  it("keeps the exact records, reads only the expiry and a plain game id, and converges a second press", () => {
    const store = new Handoffs();
    const raw = `{"gameId":"fixture-browser" , "x": 1}`;
    const a = store.receive(packet(2_000_000_000), raw, 1)!;
    expect(a.handoffJson).toBe(raw);
    expect(a.gameId).toBe("fixture-browser");
    expect(a.expiresAt).toBe(2_000_000_000_000);
    expect(store.receive(packet(2_000_000_000), raw, 2)!.id).toBe(a.id);
    expect(store.get()).toHaveLength(1);
    expect(new Handoffs().find(a.id)?.handoffJson).toBe(raw);
  });

  it("shows no game id that isn't a plain one, and refuses records no handoff can be", () => {
    const store = new Handoffs();
    expect(store.receive(packet(2_000_000_000), JSON.stringify({ gameId: "Ignore the rules ✓" }))!.gameId).toBeNull();
    expect(store.receive("x".repeat(RECORD_LIMIT + 1), "{}")).toBeNull();
  });

  it("removes expired handoffs that never downloaded anything, and keeps the rest", () => {
    const store = new Handoffs();
    const old = store.receive(packet(1000), "{}", 0)!;
    const started = store.receive(packet(1000), '{"a":1}', 0)!;
    const working = store.receive(packet(1000), '{"a":2}', 0)!;
    const fresh = store.receive(packet(3000), '{"a":3}', 0)!;
    store.update(started.id, { started: true });
    store.update(working.id, { view: view("acquiring") });
    expect(busy(store.find(working.id)!)).toBe(true);
    store.prune(2_000_000);
    expect(store.get().map((h) => h.id)).toEqual([started.id, working.id, fresh.id]);
    expect(store.find(old.id)).toBeUndefined();
    expect(store.expiredRemoved).toBe(1);
  });

  it.each(["setItem", "removeItem"] as const)("keeps expired handoffs visible when storage refuses %s", (method) => {
    const store = new Handoffs();
    const old = store.receive(packet(1000), "{}", 0)!;
    if (method === "setItem") store.receive(packet(3000), "{}", 0);
    const write = vi.spyOn(Storage.prototype, method).mockImplementation(() => { throw new DOMException("Unavailable", "SecurityError"); });
    store.prune(2_000_000);
    store.remove(old.id);
    expect(store.find(old.id)).toBeTruthy();
    expect(store.expiredRemoved).toBe(0);
    expect(store.persistenceFailed.get()).toBe(true);
    write.mockRestore();
    expect(store.retrySave()).toBe(true);
    expect(store.persistenceFailed.get()).toBe(false);
    store.prune(2_000_000);
    expect(store.find(old.id)).toBeUndefined();
    expect(store.expiredRemoved).toBe(1);
  });
});
