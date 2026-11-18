// SPDX-License-Identifier: Apache-2.0
//
// Streaming text arrives as deltas that are never replayed. Deltas sent while
// the phone was not listening are gone, so a reply shown mid-stream must not
// have later deltas spliced onto it as if nothing were missing.
import { afterEach, beforeEach, expect, it } from "vitest";
import { allGrants } from "../dev/sim/host";
import * as fx from "../dev/sim/scenario";
import { snapshot } from "../platform/lifecycle.fixtures";
import type { ClientFrame, EventEnvelope, QueryData, ServerFrame, TimelineItem } from "../protocol/types";
import { applyEvent, applyLive, emptyHostState, liveTextForTask } from "../state/projection";
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

it.each(["failed", "interrupted", "reconciliation_required"] as const)("keeps observed text when an unknown run ends as %s", state => {
  let s = applyLive(emptyHostState, { kind: "agent_text_delta", task_id: "task", run_id: "unknown", message_key: "m", text: "Sentence 42 is unfinished" }, 0);
  s = applyEvent(s, { seq: 1, at: 1, source: { kind: "companion" }, provenance: "native", event: { type: "boot_epoch_changed", boot_epoch: "new" } }, 1);
  s = applyEvent(s, { seq: 2, at: 2, source: { kind: "companion" }, provenance: "native", event: {
    type: "timeline_appended", task_id: "task", item: { seq: 2, at: 2, run_id: "unknown", item: { kind: "run_ended", run_id: "unknown", state } },
  } }, 2);
  expect(liveTextForTask(s, "task")).toMatchObject([{ text: "Sentence 42 is unfinished", interrupted: true }]);
  s = applyLive(s, { kind: "agent_text_delta", task_id: "task", run_id: "unknown", message_key: "m", text: "must not splice" }, 3);
  expect(liveTextForTask(s, "task")[0]?.text).toBe("Sentence 42 is unfinished");
});

const task = () => snapshot().tasks[0]!;
const delta = (text: string): ServerFrame => ({ type: "live", kind: "agent_text_delta", task_id: task().task_id, run_id: "run-1", message_key: "m1", text });

it.each(["failed", "interrupted", "reconciliation_required"] as const)("keeps observed text when the run record becomes %s", state => {
  const run = { ...fx.runs(0)[0]!, run_id: "run-1", task_id: task().task_id, state, ended_at: 1 };
  link.frame(delta("The newest cached sentence"));
  link.frame({ type: "event", seq: 1, at: 1, source: { kind: "companion" }, provenance: "native", event: { type: "run_upserted", run } });
  expect(liveTextForTask(session.state.get(), task().task_id)).toMatchObject([{ text: "The newest cached sentence", interrupted: true }]);
});

it("a completed run still retires unfinished live text", () => {
  const run = { ...fx.runs(0)[0]!, run_id: "run-1", task_id: task().task_id, state: "completed" as const, ended_at: 1 };
  link.frame(delta("Draft that was not the final wording"));
  link.frame({ type: "event", seq: 1, at: 1, source: { kind: "companion" }, provenance: "native", event: { type: "run_upserted", run } });
  expect(liveTextForTask(session.state.get(), task().task_id)).toEqual([]);
});

it("a journaled partial replaces its streamed prefix and survives replay", () => {
  link.frame(delta("Hello wor"));
  const item: TimelineItem = { seq: 1, at: 1, run_id: "run-1", item: { kind: "agent_message", text: "Hello wor", partial: true } };
  const partial: EventEnvelope = { seq: 1, at: 1, source: { kind: "harness", harness: "claude_code" }, provenance: "native",
    event: { type: "timeline_appended", task_id: task().task_id, item } };
  link.frame({ type: "event", ...partial });
  expect(liveTextForTask(session.state.get(), task().task_id)).toEqual([]);
  expect(session.state.get().timelines[task().task_id]?.items).toEqual([item]);
  // Replaying the same journal event must neither duplicate the reply nor
  // restore a live stream from before the interruption.
  link.authenticate();
  link.frame({ type: "event", ...partial });
  link.frame({ type: "caught_up", head: 1 });
  expect(session.state.get().timelines[task().task_id]?.items).toEqual([item]);
  expect(liveTextForTask(session.state.get(), task().task_id)).toEqual([]);
});

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
