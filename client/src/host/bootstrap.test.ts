// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it } from "vitest";
import { FakeLink, snapshot } from "../platform/lifecycle.fixtures";
import type { ClientFrame } from "../protocol/types";
import { HostSession } from "./session";

const sessions: HostSession[] = [];
afterEach(() => sessions.splice(0).forEach(s => s.stop()));
const flush = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };
class BootstrapLink extends FakeLink {
  frames: ClientFrame[] = [];
  answers: (() => void)[] = [];
  refuse = false;
  override send(frame: Parameters<FakeLink["send"]>[0]) {
    this.frames.push(frame);
    if (frame.type !== "query" || frame.query.kind !== "snapshot") return super.send(frame);
    const result = this.refuse
      ? { Err: { code: "limit_exceeded" as const, message: "This reply is too large to send." } }
      : { Ok: { kind: "snapshot" as const, ...this.snap } };
    this.answers.push(() => this.emit({ type: "frame", frame: { type: "query_result", query_id: frame.query_id, result } }));
    return true;
  }
}
function start(link: BootstrapLink) {
  const session = new HostSession(`bootstrap-${sessions.length}`, link);
  sessions.push(session);
  session.start();
  return session;
}
it("surfaces a refused bootstrap and retries on the same transport", async () => {
  const link = new BootstrapLink();
  link.refuse = true;
  const session = start(link);
  link.answers.shift()!();
  await flush();
  expect(session.online).toBe(false);
  expect(session.link.get()).toMatchObject({ state: "offline", reason: expect.stringContaining("too large") });
  expect(link.frames.filter(f => f.type === "subscribe")).toHaveLength(0);
  link.emit({ type: "frame", frame: { type: "caught_up", head: 0 } });
  expect(session.state.get().caughtUp).toBe(false);
  link.refuse = false;
  session.reconnect();
  link.answers.shift()!();
  await flush();
  expect(link.stopped).toBe(false);
  expect(session.online).toBe(true);
  expect(link.frames.filter(f => f.type === "subscribe")).toHaveLength(1);
});
it("discards a disconnected snapshot and applies only the new connection's reply", async () => {
  const link = new BootstrapLink();
  link.snap.tasks[0]!.title = "old connection";
  const session = start(link), stale = link.answers.shift()!;
  link.emit({ type: "state", state: { state: "offline", since: 1, reason: "Disconnected", retryAt: null } });
  link.snap = snapshot();
  link.snap.tasks[0]!.title = "new connection";
  session.reconnect();
  stale();
  await flush();
  expect(link.frames.filter(f => f.type === "subscribe")).toHaveLength(0);
  link.answers.shift()!();
  await flush();
  expect(session.state.get().tasks[link.snap.tasks[0]!.task_id]?.title).toBe("new connection");
  expect(link.frames.filter(f => f.type === "subscribe")).toHaveLength(1);
});
it("surfaces a refresh failure after a journal gap without resubscribing early", async () => {
  const link = new BootstrapLink(), session = start(link);
  link.answers.shift()!();
  await flush();
  link.refuse = true;
  link.emit({ type: "frame", frame: { type: "journal_gap", requested_from: 0, oldest_retained: 5 } });
  link.emit({ type: "frame", frame: { type: "subscription_closed", after_seq: 0, reason: { code: "limit_exceeded", message: "Overflow" } } });
  expect(link.frames.filter(f => f.type === "subscribe")).toHaveLength(1);
  link.answers.shift()!();
  await flush();
  expect(session.online).toBe(false);
  expect(session.state.get().caughtUp).toBe(false);
});
