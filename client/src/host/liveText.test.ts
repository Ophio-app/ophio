// SPDX-License-Identifier: Apache-2.0
//
// Streaming text arrives as deltas that are never replayed. Deltas sent while
// the phone was not listening are gone, so a reply shown mid-stream must not
// have later deltas spliced onto it as if nothing were missing.
import { afterEach, beforeEach, expect, it } from "vitest";
import { allGrants } from "../dev/sim/host";
import * as fx from "../dev/sim/scenario";
import { snapshot } from "../platform/lifecycle.fixtures";
import type { ClientFrame, EventEnvelope, QueryData, ServerFrame } from "../protocol/types";
import { liveTextForTask } from "../state/projection";
import type { ControlLink, LinkEvent } from "./link";
import { HostSession } from "./session";

class ScriptedLink implements ControlLink {
  sent: ClientFrame[] = [];
  #listener: ((e: LinkEvent) => void) | undefined;
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
    this.sent.push(frame);
    return true;
  }
  listen(listener: (e: LinkEvent) => void) {
    this.#listener = listener;
    return () => { this.#listener = undefined; };
  }
  emit(e: LinkEvent) {
    this.#listener?.(e);
  }
  frame(frame: ServerFrame) {
    this.emit({ type: "frame", frame });
  }
  answer(data: QueryData) {
    const query = this.sent.findLast((f) => f.type === "query" && f.query.kind === data.kind);
    if (query?.type !== "query") throw new Error(`No ${data.kind} query`);
    this.frame({ type: "query_result", query_id: query.query_id, result: { Ok: data } });
  }
  authenticate() {
    const snap = snapshot();
    this.emit({ type: "authenticated", challenge: { selected: { major: 1, minor: 0 }, host_id: snap.host.host_id, boot_epoch: fx.ids.boot, server_nonce: "fixture" },
      auth: { host_id: snap.host.host_id, device_id: fx.ids.device, grants: allGrants, capabilities: fx.capabilities(), companion_version: "test", journal_head: 0 } });
  }
}

const flush = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };
let link: ScriptedLink;
let session: HostSession;
beforeEach(async () => {
  localStorage.clear();
  link = new ScriptedLink();
  session = new HostSession("live", link);
  session.start();
  link.authenticate();
  link.answer({ kind: "snapshot", ...snapshot() });
  await flush();
  link.frame({ type: "caught_up", head: 0 });
});
afterEach(() => session.stop());

const task = () => snapshot().tasks[0]!;
const delta = (text: string): ServerFrame => ({ type: "live", kind: "agent_text_delta", task_id: task().task_id, run_id: "run-1", message_key: "m1", text });

it.each([
  ["the connection drops", () => {
    link.emit({ type: "state", state: { state: "offline", since: 1, reason: "Connection interrupted", retryAt: null } });
    link.authenticate();
  }],
  ["the computer closes the subscription", () => {
    link.frame({ type: "subscription_closed", after_seq: 0, reason: { code: "limit_exceeded", message: "event consumer fell behind" } });
  }],
])("a reply streaming when %s is not spliced with what comes after", async (_, interrupt) => {
  link.frame(delta("Hello "));
  link.frame(delta("wor"));
  expect(liveTextForTask(session.state.get(), task().task_id).map((l) => l.text)).toEqual(["Hello wor"]);
  interrupt();
  await flush();
  link.frame({ type: "caught_up", head: 0 });
  // "ld, how" was sent while the phone was not listening.
  link.frame(delta(" are you"));
  const shown = liveTextForTask(session.state.get(), task().task_id).map((l) => l.text);
  expect(shown).toEqual(["Hello wor"]);

  // The journaled reply replaces what was streamed.
  const reply: EventEnvelope = { seq: 1, at: 1, source: { kind: "harness", harness: "claude_code" }, provenance: "native",
    event: { type: "timeline_appended", task_id: task().task_id, item: { seq: 1, at: 1, run_id: "run-1", item: { kind: "agent_message", text: "Hello world, how are you", partial: false } } } };
  link.frame({ type: "event", ...reply });
  expect(liveTextForTask(session.state.get(), task().task_id)).toEqual([]);
});
