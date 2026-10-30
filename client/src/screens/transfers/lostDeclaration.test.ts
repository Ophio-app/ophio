// SPDX-License-Identifier: Apache-2.0
import { act } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { TransferManager } from "./manager";

vi.mock("./storage", () => ({
  transferBytes: {
    put: vi.fn().mockResolvedValue(undefined),
    get: vi.fn().mockResolvedValue(null),
    remove: vi.fn().mockResolvedValue(undefined),
  },
}));

let app: App;
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  // Deterministic digests: native crypto work does not run under fake time.
  vi.spyOn(crypto.subtle, "digest").mockImplementation(async (_algorithm, data) => {
    const bytes = ArrayBuffer.isView(data)
      ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
      : new Uint8Array(data);
    const hash = new Uint8Array(32);
    bytes.forEach((value, i) => { hash[i % 32] = ((hash[i % 32] ?? 0) * 31 + value) % 256; });
    return hash.buffer;
  });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId),
    switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
});
afterEach(async () => {
  app.session.stop();
  window.__sim?.host.dispose();
  await advance(20);
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it("an upload whose declaration receipt was lost continues once the computer is back, without a second declaration", async () => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing simulated computer");
  const manager = new TransferManager(app);
  // The computer declares the transfer, but the connection drops before its receipt arrives.
  window.__sim?.dropNextReceipt();
  const id = manager.startUpload("field-notes.md", new Blob(["Field notes\n"]), { kind: "inbox" });
  await advance(500);
  expect(app.session.online).toBe(false);
  expect(manager.get().jobs[id]?.transferId).toBeNull();
  expect(manager.get().jobs[id]).toMatchObject({ phase: "paused", resumable: true, interrupted: true });
  // Once connected again it finishes sending the file the person chose.
  await advance(8000);
  expect(app.session.online).toBe(true);
  const declared = host.transfers.filter((t) => t.name === "field-notes.md");
  expect(declared).toHaveLength(1);
  expect(manager.get().jobs[id]).toMatchObject({ phase: "completed", transferId: declared[0]?.transfer_id });
  expect(host.transfers.filter((t) => t.name === "field-notes.md")).toHaveLength(1);
});
