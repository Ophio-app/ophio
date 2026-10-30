// SPDX-License-Identifier: Apache-2.0
// An upload whose connection drops, as when the computer lets go of one that
// went silent while the phone was frozen or in the background, continues by
// itself once the app is in the foreground with a working link. The computer
// says where to continue from. A person's Pause or Cancel is never undone.
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { App } from "../../app/context";
import type { Platform, TransferLink } from "../../host/link";
import { HostSession } from "../../host/session";
import { TransferError, upload, type UploadResult } from "../../host/transfer";
import { announce } from "../../design/Announcer";
import { TransferManager } from "./manager";
import { TransferRow } from "./TransferRow";

vi.mock("./storage", () => ({
  transferBytes: {
    put: vi.fn().mockResolvedValue(undefined),
    get: vi.fn().mockResolvedValue(null),
  },
}));
vi.mock("../../design/Announcer", () => ({ announce: vi.fn() }));
vi.mock("../../host/transfer", async (original) => {
  const actual = await original<typeof import("../../host/transfer")>();
  return { ...actual, upload: vi.fn(), sha256Hex: vi.fn().mockResolvedValue("expected-sha") };
});

let visibility: DocumentVisibilityState = "visible";
Object.defineProperty(document, "visibilityState", { configurable: true, get: () => visibility });
const hide = () => { visibility = "hidden"; document.dispatchEvent(new Event("visibilitychange")); };
const show = () => { visibility = "visible"; document.dispatchEvent(new Event("visibilitychange")); };

const done: UploadResult = { transferId: "t1", sha256: "expected-sha", artifactId: null };
const silence = () => new TransferError("Nothing arrived for this upload for a while, so the computer let go of it.", "t1", true);
function deferred() {
  let resolve!: (r: UploadResult) => void, reject!: (e: unknown) => void;
  const promise = new Promise<UploadResult>((a, b) => { resolve = a; reject = b; });
  return { promise, resolve, reject };
}

const connected = () => {
  const session = new HostSession("p", { connect() {}, disconnect() {}, listen: () => () => {}, send: () => true });
  session.link.set({ state: "online", since: 0 });
  return session;
};
const later = (ms = 50) => new Promise((resolve) => setTimeout(resolve, ms));
function setup() {
  const session = connected();
  const foreground = new Set<() => void>();
  const link: TransferLink = { send: () => true, close: vi.fn(), chunk: async () => true, listen: () => () => {} };
  const platform = {
    transfer: () => link,
    onForeground: (listener: () => void) => { foreground.add(listener); return () => { foreground.delete(listener); }; },
  } as unknown as Platform;
  const app = { profile: { profileId: "p", name: "Computer" }, platform, session } as App;
  const manager = new TransferManager(app);
  /** The native app resumes, as after the phone was in the background. */
  const resumeApp = () => { visibility = "visible"; for (const listener of foreground) listener(); };
  const offline = () => session.link.set({ state: "offline", since: 1, reason: "Connection lost", retryAt: null });
  const online = () => session.link.set({ state: "online", since: 2 });
  return { manager, app, session, resumeApp, offline, online };
}
beforeEach(() => { localStorage.clear(); vi.clearAllMocks(); visibility = "visible"; });
afterEach(() => { cleanup(); vi.useRealTimers(); });

it("continues an upload the computer let go of once the app is back in the foreground with a working link", async () => {
  const { manager, resumeApp, offline, online } = setup();
  const first = deferred(), second = deferred();
  vi.mocked(upload)
    .mockImplementationOnce(async (_ctx, o) => { await o.onDeclared?.("t1"); o.onProgress?.(100, 300); return first.promise; })
    .mockImplementationOnce(async (_ctx, o) => { await o.onDeclared?.("t1"); return second.promise; });
  const id = manager.startUpload("notes.md", new Blob(["draft"]), { kind: "inbox" });
  await vi.waitFor(() => expect(manager.get().jobs[id]?.phase).toBe("sending"));

  // The phone goes to the background and the computer lets go of the silent upload.
  hide(); offline();
  first.reject(silence());
  await vi.waitFor(() => expect(manager.get().jobs[id]).toMatchObject({ phase: "paused", resumable: true, interrupted: true }));
  render(<TransferRow job={manager.get().jobs[id]} online={false} />);
  expect(screen.getByText(/To computer · Continues when connected/)).toBeTruthy();
  cleanup();
  // A link that comes back while the app is in the background is not enough.
  online();
  await later();
  expect(upload).toHaveBeenCalledTimes(1);
  expect(manager.get().jobs[id]?.phase).toBe("paused");

  resumeApp();
  await vi.waitFor(() => expect(upload).toHaveBeenCalledTimes(2));
  // It continues the same transfer, from the offset the computer confirms.
  expect(vi.mocked(upload).mock.calls[1]?.[1].transferId).toBe("t1");
  await vi.waitFor(() => expect(manager.get().jobs[id]).toMatchObject({ phase: "sending", interrupted: true, error: null }));
  render(<TransferRow job={manager.get().jobs[id]} online />);
  expect(screen.getByText(/To computer · Continuing where it stopped/)).toBeTruthy();
  second.resolve(done);
  await vi.waitFor(() => expect(manager.get().jobs[id]).toMatchObject({ phase: "completed", interrupted: false }));
});

it("continues when the app returns to the foreground after its link is back", async () => {
  const { manager, offline, online } = setup();
  vi.mocked(upload).mockRejectedValueOnce(silence()).mockResolvedValueOnce(done);
  hide(); offline(); online();
  const id = manager.startUpload("notes.md", new Blob(["draft"]), { kind: "inbox" });
  await vi.waitFor(() => expect(manager.get().jobs[id]).toMatchObject({ phase: "paused", interrupted: true }));
  await later();
  expect(upload).toHaveBeenCalledTimes(1);
  show();
  await vi.waitFor(() => expect(manager.get().jobs[id]?.phase).toBe("completed"));
  expect(upload).toHaveBeenCalledTimes(2);
});

it("tries again quietly after a growing wait when it drops while the app is already in the foreground", async () => {
  vi.useFakeTimers();
  const { manager } = setup();
  vi.mocked(upload).mockRejectedValueOnce(silence()).mockRejectedValueOnce(silence()).mockResolvedValueOnce(done);
  const id = manager.startUpload("notes.md", new Blob(["draft"]), { kind: "inbox" });
  await vi.advanceTimersByTimeAsync(0);
  expect(manager.get().jobs[id]).toMatchObject({ phase: "paused", interrupted: true });
  render(<TransferRow job={manager.get().jobs[id]} online />);
  expect(screen.getByText(/To computer · Continues shortly/)).toBeTruthy();
  await vi.advanceTimersByTimeAsync(1_900);
  expect(upload).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(100);
  expect(upload).toHaveBeenCalledTimes(2);
  await vi.advanceTimersByTimeAsync(3_900);
  expect(upload).toHaveBeenCalledTimes(2);
  await vi.advanceTimersByTimeAsync(100);
  expect(upload).toHaveBeenCalledTimes(3);
  expect(manager.get().jobs[id]?.phase).toBe("completed");
  // Paused once, then arrived; the attempt in between was not announced.
  expect(vi.mocked(announce).mock.calls.map(([text]) => text)).toEqual(["notes.md: paused.", "notes.md arrived and its bytes were verified."]);
});

it("waits for this computer's current connection, not one that was stopped", async () => {
  const { manager, app, session, resumeApp } = setup();
  vi.mocked(upload).mockRejectedValueOnce(silence()).mockResolvedValueOnce(done);
  hide();
  const id = manager.startUpload("notes.md", new Blob(["draft"]), { kind: "inbox" });
  await vi.waitFor(() => expect(manager.get().jobs[id]).toMatchObject({ phase: "paused", interrupted: true }));
  // Another computer is chosen, which stops this one's connection.
  session.stop();
  resumeApp();
  await later();
  expect(upload).toHaveBeenCalledTimes(1);
  // Back on this computer, its new connection is already up.
  manager.bind({ ...app, session: connected() });
  await vi.waitFor(() => expect(manager.get().jobs[id]?.phase).toBe("completed"));
  expect(upload).toHaveBeenCalledTimes(2);
});

it("never undoes a person's Pause or Cancel", async () => {
  const { manager, session, resumeApp, offline, online } = setup();
  // Paused before it got going, while not connected.
  offline();
  const early = manager.startUpload("early.md", new Blob(["draft"]), { kind: "inbox" });
  manager.pause(early);
  await vi.waitFor(() => expect(manager.get().jobs[early]).toMatchObject({ phase: "paused", interrupted: false }));
  online(); resumeApp();
  await later();
  expect(upload).not.toHaveBeenCalled();

  vi.mocked(upload).mockImplementationOnce(async (ctx, o) => {
    await o.onDeclared?.("t1");
    return new Promise((_resolve, reject) => {
      ctx.platform.transfer("p").listen((e) => { if (e.type === "state" && e.state.state === "offline") reject(new TransferError("Paused", "t1", true)); });
    });
  });
  const paused = manager.startUpload("paused.md", new Blob(["draft"]), { kind: "inbox" });
  await vi.waitFor(() => expect(manager.get().jobs[paused]?.phase).toBe("sending"));
  manager.pause(paused);
  await vi.waitFor(() => expect(manager.get().jobs[paused]).toMatchObject({ phase: "paused", interrupted: false }));

  vi.mocked(upload).mockRejectedValueOnce(silence());
  const cancelled = manager.startUpload("cancelled.md", new Blob(["draft"]), { kind: "inbox" });
  await vi.waitFor(() => expect(manager.get().jobs[cancelled]).toMatchObject({ phase: "paused", interrupted: true }));
  vi.spyOn(session, "command").mockResolvedValue({ command_id: "c", status: "accepted", seq: 1, result: null, error: null });
  const cancelling = manager.cancel(cancelled);
  // The app returning while the cancellation is on its way does not restart it.
  offline(); online(); resumeApp();
  session.state.set({ ...session.state.get(), commandStages: { c: { stage: "observed", at: 1, detail: null } } });
  await cancelling;
  expect(manager.get().jobs[cancelled]?.phase).toBe("cancelled");

  offline(); online(); resumeApp(); hide(); show();
  await later();
  expect(upload).toHaveBeenCalledTimes(2);
  expect(manager.get().jobs[paused]?.phase).toBe("paused");
  expect(manager.get().jobs[early]?.phase).toBe("paused");
});
