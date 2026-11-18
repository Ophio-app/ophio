// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { Event, EventEnvelope, LiveEvent, TimelineItem } from "../protocol/types";
import { capabilities, runs, tasks } from "../dev/sim/scenario";
import {
  applyEvent, applyGap, applyJournalReset, applyLive, applyReplay, applyTimelinePage, emptyHostState,
  interruptLive, liveTextForTask, observedReplyForTask, timelineForTask, CHAT_REPLY_RECOVERY_FEATURE, OBSERVED_REPLY_BYTES, type HostState, type ProjectionFrame,
} from "./projection";

const bytes = (text: string) => new TextEncoder().encode(text).length;
const delta = (text: string, offset = 0, key = "message"): LiveEvent => {
  const value = { kind: "agent_text_delta" as const, task_id: "task", run_id: "run", message_key: key, text, text_offset: offset };
  return value;
};
const message = (seq: number, text: string, offset = 0, key = "message", partial = true): TimelineItem => {
  const item = { kind: "agent_message" as const, text, partial, message_key: key, text_offset: offset };
  return { seq, at: seq, run_id: "run", item };
};
const legacyDelta = (text: string, key = "legacy", runId = "run"): LiveEvent => ({
  kind: "agent_text_delta", task_id: "task", run_id: runId, message_key: key, text,
});
const legacyPartial = (seq: number, text: string, runId = "run"): TimelineItem => ({
  seq, at: seq, run_id: runId, item: { kind: "agent_message", text, partial: true },
});
const event = (seq: number, event: Event): EventEnvelope => ({ seq, at: seq, source: { kind: "companion" }, provenance: "native", event });
const appended = (item: TimelineItem): ProjectionFrame => ({ env: event(item.seq, { type: "timeline_appended", task_id: "task", item }), now: item.seq });
const shown = (s: HostState): string[] => [
  ...(timelineForTask(s, "task")?.items.flatMap(i => i.item.kind === "agent_message" ? [i.item.text] : []) ?? []),
  ...liveTextForTask(s, "task").map(l => l.text),
];

it.each([false, true])("keeps the exact newer cached sentence across boot and an unknown run ending (end first: %s)", endFirst => {
  const cached = "41. Visitors’ fond memories.\n42. They protect their delicate sweetness from";
  const durable = "41. Visitors’ fond";
  let state = interruptLive(applyLive(emptyHostState, delta(cached), 0));
  const partial = appended(message(3, durable));
  const ended: ProjectionFrame = { env: event(endFirst ? 2 : 4, { type: "timeline_appended", task_id: "task", item: {
    seq: endFirst ? 2 : 4, at: 4, run_id: "run", item: { kind: "run_ended", run_id: "run", state: "reconciliation_required" },
  } }), now: 4 };
  const boot: ProjectionFrame = { env: event(1, { type: "boot_epoch_changed", boot_epoch: "next" }), now: 1 };
  state = applyReplay(state, endFirst ? [boot, ended, partial] : [boot, partial, ended]);
  expect(shown(state)).toEqual([cached]);
  expect(liveTextForTask(state, "task")).toEqual([]);
  expect(state.timelines.task?.items.find(i => i.seq === 3)?.item).toMatchObject({ text: durable });
  // A replay and the later query page preserve one exact reply, not two copies.
  state = applyReplay(state, [partial, ended]);
  state = applyTimelinePage(state, "task", [message(3, durable)], false);
  expect(shown(state)).toEqual([cached]);
});

it("reconciles UTF-8 ranges even when the newest page starts inside the cached message", () => {
  const first = "é pond ";
  const second = "💧 grows ";
  const cached = first + second + "moss, not duplicates";
  let state = interruptLive(applyLive(emptyHostState, delta(cached), 0));
  state = applyGap(state, 1, 10, 1);
  state = applyTimelinePage(state, "task", [message(12, second, bytes(first))], true);
  expect(shown(state)).toEqual([cached]);
  // Older pages must remove the local prefix now covered by durable history,
  // without moving the newer cached suffix back onto an earlier segment.
  state = applyTimelinePage(state, "task", [message(11, first)], false, true);
  expect(shown(state)).toEqual([first, second + "moss, not duplicates"]);
  expect(shown(state).join("")).toBe(cached);
  state = applyTimelinePage(state, "task", [message(12, second, bytes(first))], true);
  expect(shown(state).join("")).toBe(cached);
});

it("does not guess between interleaved messages with the same opening", () => {
  let state = applyLive(emptyHostState, delta("Same opening, parent tail", 0, "parent-generation"), 0);
  state = applyLive(state, delta("Same opening, helper tail", 0, "helper-generation"), 1);
  state = interruptLive(state);
  state = applyTimelinePage(state, "task", [
    message(1, "Same opening, ", 0, "helper-generation"),
    message(2, "Same opening, ", 0, "parent-generation"),
  ], false);
  expect(shown(state)).toEqual(["Same opening, helper tail", "Same opening, parent tail"]);
  expect(liveTextForTask(state, "task")).toEqual([]);
});

it("preserves exact range coverage when cached deltas began after the message started", () => {
  const before = "Earlier words. ";
  const cached = "A pond 💧 and the newest tail";
  let state = interruptLive(applyLive(emptyHostState, delta(cached, bytes(before)), 0));
  state = applyTimelinePage(state, "task", [message(1, before + "A pond 💧")], false);
  expect(shown(state)).toEqual([before + cached]);
});

it("fills checkpoint gaps only from exact cached ranges of that same message", () => {
  let state = interruptLive(applyLive(emptyHostState, delta("one two three four"), 0));
  state = applyTimelinePage(state, "task", [message(1, "one "), message(3, "three ", bytes("one two "))], true);
  expect(shown(state)).toEqual(["one ", "two three four"]);
});

it("keeps a completed final authoritative even if its wording differs from cached text", () => {
  let state = applyLive(emptyHostState, delta("Draft wording with newer tail"), 0);
  state = applyLive(state, delta("A helper still has its own text", 0, "helper"), 1);
  state = applyTimelinePage(state, "task", [message(1, "Draft wording")], false);
  expect(shown(state)).toEqual(["Draft wording with newer tail", "A helper still has its own text"]);
  state = applyEvent(state, event(2, { type: "timeline_appended", task_id: "task", item: message(2, "Final corrected wording", 0, "message", false) }), 2);
  expect(Object.values(state.liveText).map(l => l.key)).toEqual(["helper"]);
  expect(shown(state)).toEqual(["Draft wording", "Final corrected wording", "A helper still has its own text"]);
});

it("accepts an empty final closure after earlier durable partials", () => {
  let state = interruptLive(applyLive(emptyHostState, delta("All words were already journaled"), 0));
  state = applyTimelinePage(state, "task", [message(1, "All words were already journaled")], false);
  state = applyTimelinePage(state, "task", [message(2, "", bytes("All words were already journaled"), "message", false)], false);
  expect(Object.values(state.liveText)).toEqual([]);
});

it("deduplicates explicit live offsets and freezes a missing or conflicting range", () => {
  let state = applyLive(emptyHostState, delta("é pond"), 0);
  state = applyLive(state, delta("é pond"), 1);
  state = applyLive(state, delta("pond grows", bytes("é ")), 2);
  expect(shown(state)).toEqual(["é pond grows"]);
  state = applyLive(state, delta("lost gap", 999), 3);
  state = applyLive(state, delta("must not splice", bytes("é pond grows")), 4);
  expect(shown(state)).toEqual(["é pond grows"]);
  expect(Object.values(state.liveText)[0]?.interrupted).toBe(true);
});

it("does not attach a cache to a conflicting prefix or invalid UTF-8 boundary", () => {
  let state = interruptLive(applyLive(emptyHostState, delta("é pond tail"), 0));
  state = applyTimelinePage(state, "task", [message(1, "different"), message(2, "pond", 1)], false);
  expect(liveTextForTask(state, "task").map(l => l.text)).toEqual(["é pond tail"]);
  expect(state.timelines.task?.items.map(i => i.item.kind === "agent_message" && i.item.text)).toEqual(["different", "pond"]);
});

it("continues an identified stream after a durable partial without duplicating its prefix", () => {
  let state = applyLive(emptyHostState, delta("Hello "), 0);
  state = applyTimelinePage(state, "task", [message(1, "Hello ")], false);
  state = applyLive(state, delta("world", bytes("Hello ")), 2);
  expect(shown(state)).toEqual(["Hello world"]);
  state = applyTimelinePage(state, "task", [message(2, "world", bytes("Hello "))], false);
  expect(shown(state)).toEqual(["Hello ", "world"]);
});

it("keeps successive covered legacy partials from replaying earlier streamed words", () => {
  const legacyDelta = (text: string): LiveEvent => ({ kind: "agent_text_delta", task_id: "task", run_id: "run", message_key: "legacy", text });
  const legacyPartial = (seq: number, text: string): TimelineItem => ({ seq, at: seq, run_id: "run", item: { kind: "agent_message", text, partial: true } });
  let state = applyLive(emptyHostState, legacyDelta("Hello "), 0);
  state = applyTimelinePage(state, "task", [legacyPartial(1, "Hello ")], false);
  state = applyLive(state, legacyDelta("world"), 2);
  state = applyTimelinePage(state, "task", [legacyPartial(2, "world")], false);
  expect(shown(state)).toEqual(["Hello ", "world"]);
});

it.each(["events", "replay", "page"])("projects successive legacy suffix checkpoints once, keeping the newest tail (%s)", delivery => {
  const cached = "Hello world newest";
  const partials: TimelineItem[] = ["Hello ", "world "].map((text, index) => ({
    seq: index + 1, at: index + 1, run_id: "run", item: { kind: "agent_message", text, partial: true },
  }));
  let state = interruptLive(applyLive(emptyHostState, {
    kind: "agent_text_delta", task_id: "task", run_id: "run", message_key: "legacy", text: cached,
  }, 0));
  if (delivery === "page") state = applyTimelinePage(state, "task", partials, false);
  else if (delivery === "replay") state = applyReplay(state, partials.map(appended));
  else for (const item of partials) state = applyEvent(state, event(item.seq, { type: "timeline_appended", task_id: "task", item }), item.seq);
  expect(shown(state)).toEqual(["Hello ", "world newest"]);
  expect(shown(state).join("")).toBe(cached);
  expect(liveTextForTask(state, "task")).toEqual([]);
  expect(state.timelines.task?.items.map(item => item.item)).toEqual(partials.map(item => item.item));
  state = applyTimelinePage(state, "task", partials, false);
  expect(shown(state)).toEqual(["Hello ", "world newest"]);
});

it("keeps the first legacy anchor when later suffixes repeat its opening", () => {
  const cached = "Hello Hello newest";
  let state = interruptLive(applyLive(emptyHostState, legacyDelta(cached), 0));
  state = applyTimelinePage(state, "task", [legacyPartial(1, "Hello "), legacyPartial(2, "Hello ")], false);
  expect(shown(state)).toEqual(["Hello ", "Hello newest"]);
  expect(Object.values(state.liveText)[0]?.timelineSeq).toBe(1);
});

it("reconciles legacy UTF-8 suffixes when older pages supply the missing opening", () => {
  const first = "é 💧 ";
  const second = "café ";
  const cached = first + second + "newest";
  let state = interruptLive(applyLive(emptyHostState, legacyDelta(cached), 0));
  state = applyTimelinePage(state, "task", [legacyPartial(2, second)], true);
  expect(liveTextForTask(state, "task").map(item => item.text)).toEqual([cached]);
  state = applyTimelinePage(state, "task", [legacyPartial(1, first)], false, true);
  expect(shown(state)).toEqual([first, second + "newest"]);
  expect(Object.values(state.liveText)[0]?.offset).toBeUndefined();
  state = applyTimelinePage(state, "task", [legacyPartial(2, second)], false);
  expect(shown(state).join("")).toBe(cached);
});

it("keeps growing a legacy observation after successive checkpoints cover it", () => {
  let state = applyLive(emptyHostState, legacyDelta("Hello world newest"), 0);
  state = applyTimelinePage(state, "task", [legacyPartial(1, "Hello "), legacyPartial(2, "world ")], false);
  state = applyLive(state, legacyDelta(" plus"), 3);
  expect(shown(state)).toEqual(["Hello ", "world newest plus"]);
  state = applyTimelinePage(state, "task", [legacyPartial(3, "newest plus")], false);
  expect(shown(state)).toEqual(["Hello ", "world ", "newest plus"]);
  expect(liveTextForTask(state, "task")).toEqual([]);
});

it("does not borrow a legacy helper's checkpoint for another live key", () => {
  let state = applyLive(emptyHostState, legacyDelta("Hello world newest", "parent"), 0);
  state = interruptLive(applyLive(state, legacyDelta("world helper tail", "helper"), 1));
  state = applyTimelinePage(state, "task", [legacyPartial(1, "Hello "), legacyPartial(2, "world ")], false);
  expect(shown(state)).toEqual(["Hello world newest", "world helper tail"]);
  expect(Object.values(state.liveText).map(item => item.key)).toEqual(["parent", "helper"]);
});

it("does not attach a suffix whose text is ambiguous between cached legacy keys", () => {
  let state = applyLive(emptyHostState, legacyDelta("Hello world newest", "parent"), 0);
  state = interruptLive(applyLive(state, legacyDelta("Earlier world helper tail", "helper"), 1));
  state = applyTimelinePage(state, "task", [legacyPartial(1, "Hello "), legacyPartial(2, "world ")], false);
  expect(shown(state)).toEqual(["Hello world newest", "world ", "Earlier world helper tail"]);
  expect(liveTextForTask(state, "task").map(item => item.key)).toEqual(["helper"]);
});

it("does not invent a legacy identity when separate live keys share an opening", () => {
  let state = applyLive(emptyHostState, legacyDelta("Hello world newest", "parent"), 0);
  state = interruptLive(applyLive(state, legacyDelta("Hello helper tail", "helper"), 1));
  state = applyTimelinePage(state, "task", [legacyPartial(1, "Hello "), legacyPartial(2, "world ")], false);
  expect(shown(state)).toEqual(["Hello ", "world ", "Hello world newest", "Hello helper tail"]);
  expect(liveTextForTask(state, "task").map(item => item.key)).toEqual(["parent", "helper"]);
});

it("does not shorten a legacy durable suffix that extends beyond the cached observation", () => {
  let state = interruptLive(applyLive(emptyHostState, legacyDelta("Hello world newest"), 0));
  state = applyTimelinePage(state, "task", [legacyPartial(1, "Hello "), legacyPartial(2, "world newest durable")], false);
  expect(shown(state)).toEqual(["Hello ", "world newest durable"]);
});

it.each(["other run", "identified message", "note", "conflicting partial"])("does not join legacy suffixes across a %s boundary", boundary => {
  const cached = "Hello world newest";
  let state = interruptLive(applyLive(emptyHostState, legacyDelta(cached), 0));
  const barrier: TimelineItem = boundary === "other run" ? legacyPartial(2, "world ", "later-run")
    : boundary === "identified message" ? message(2, "world ", 0, "helper")
    : boundary === "note" ? { seq: 2, at: 2, run_id: "run", item: { kind: "note", text: "A separate action" } }
    : legacyPartial(2, "different ");
  state = applyTimelinePage(state, "task", [legacyPartial(1, "Hello "), barrier, legacyPartial(3, "world ")], false);
  const texts = shown(state);
  expect(texts[0]).toBe(cached);
  expect(texts.at(-1)).toBe("world ");
  expect(state.timelines.task?.items[2]?.item).toMatchObject({ text: "world " });
});

it("does not attach legacy cached text to a final message or subsequent reply", () => {
  let state = interruptLive(applyLive(emptyHostState, legacyDelta("Hello world newest"), 0));
  const final: TimelineItem = { seq: 2, at: 2, run_id: "run", item: { kind: "agent_message", text: "Final correction", partial: false } };
  state = applyTimelinePage(state, "task", [legacyPartial(1, "Hello "), final, legacyPartial(3, "world ")], false);
  expect(shown(state)).toEqual(["Hello ", "Final correction", "world "]);
  expect(Object.values(state.liveText)).toEqual([]);
});

it("does not use another run's live key as a legacy suffix candidate", () => {
  let state = applyLive(emptyHostState, legacyDelta("Hello world newest", "parent"), 0);
  state = interruptLive(applyLive(state, legacyDelta("world newer run", "parent", "later-run"), 1));
  state = applyTimelinePage(state, "task", [legacyPartial(1, "Hello "), legacyPartial(2, "world ")], false);
  expect(shown(state)).toEqual(["Hello ", "world newest", "world newer run"]);
});

it("drops stream identities when the host journal is restored, rather than binding old words to reused sequence numbers", () => {
  let state = applyLive(emptyHostState, delta("Old journal text"), 0);
  state = applyTimelinePage(state, "task", [message(9, "Old journal")], false);
  state = applyJournalReset(state, 1, 2);
  state = applyTimelinePage(state, "task", [message(9, "Different restored text")], false);
  expect(shown(state)).toEqual(["Different restored text"]);
  expect(Object.values(state.liveText)).toEqual([]);
});

const recoveryState = (): HostState => ({
  ...emptyHostState,
  capabilities: { ...capabilities(), features: [CHAT_REPLY_RECOVERY_FEATURE] },
  tasks: { task: { ...tasks(0)[0]!, task_id: "task", current_run: "run", conversation: true } },
  runs: { run: { ...runs(0)[0]!, run_id: "run", task_id: "task", state: "reconciliation_required", nothing_running: true } },
});

it("bounds phone-observed carry-on context to the exact last 8 KiB at a UTF-8 boundary", () => {
  const durable = "前".repeat(3000) + "💧 newest";
  const text = durable + " exact tail";
  let state = interruptLive(applyLive(recoveryState(), delta(text, 25), 1));
  state = applyTimelinePage(state, "task", [message(1, durable, 25)], false);
  const observed = observedReplyForTask(state, "task")!;
  expect(observed).toMatchObject({ run_id: "run", message_key: "message" });
  expect(bytes(observed.text)).toBeLessThanOrEqual(OBSERVED_REPLY_BYTES);
  expect(observed.text).not.toContain("�");
  expect(text.endsWith(observed.text)).toBe(true);
  expect(observed.text_offset).toBe(25 + bytes(text) - bytes(observed.text));
});

it.each(["no history", "other identity", "conflicting bytes", "gap", "newer durable gap", "UTF-8 boundary", "clipped gap", "final closure"])("does not automatically send the newest cached observation without a verifiable durable join (%s)", reason => {
  const key = "newest";
  let text = "é🌿 main prefix and newest ending";
  let offset = 0;
  let items = [message(1, "é🌿 main prefix", 0, key)];
  if (reason === "no history") items = [];
  if (reason === "other identity") items = [message(1, "Older durable partial", 0, "older")];
  if (reason === "conflicting bytes") items = [message(1, "different prefix", 0, key)];
  if (reason === "gap") offset = 1000;
  if (reason === "newer durable gap") items.push(message(2, "Later durable range", 1000, key));
  if (reason === "UTF-8 boundary") offset = 1;
  if (reason === "clipped gap") { text = "é🌿 main prefix " + "x".repeat(OBSERVED_REPLY_BYTES + 100); }
  if (reason === "final closure") items.push(message(2, "", bytes("é🌿 main prefix"), key, false));
  let state = applyTimelinePage(recoveryState(), "task", items, false);
  // Do not replace the newest unanchored words with an older eligible stream.
  state = applyLive(state, delta("Older durable partial and cached tail", 0, "older"), 1);
  state = interruptLive(applyLive(state, delta(text, offset, key), 2));
  expect(observedReplyForTask(state, "task")).toBeUndefined();
  expect(Object.values(state.liveText).find(l => l.key === key)?.text).toBe(text);
});

it("does not send a matching last segment when earlier overlapping bytes conflict", () => {
  let state = applyTimelinePage(recoveryState(), "task", [message(1, "right "), message(2, "ending", bytes("right "))], false);
  state = interruptLive(applyLive(state, delta("wrong ending newer"), 1));
  expect(observedReplyForTask(state, "task")).toBeUndefined();
  expect(Object.values(state.liveText)[0]?.text).toBe("wrong ending newer");
});

it("allows an exact adjoining observation after the durable Unicode prefix", () => {
  const prefix = "é🌿 durable prefix";
  let state = applyTimelinePage(recoveryState(), "task", [message(1, prefix)], false);
  state = interruptLive(applyLive(state, delta(" newest ending", bytes(prefix)), 1));
  expect(observedReplyForTask(state, "task")).toEqual({ run_id: "run", message_key: "message", text_offset: bytes(prefix), text: " newest ending" });
});

it("verifies the contiguous durable main segments even when a helper row is newer", () => {
  const prefix = "é🌿 ";
  const ending = "main ending";
  let state = applyTimelinePage(recoveryState(), "task", [message(1, prefix), message(2, ending, bytes(prefix)), message(3, "Helper partial", 0, "helper")], false);
  state = interruptLive(applyLive(state, delta(prefix + ending + " newest"), 1));
  expect(observedReplyForTask(state, "task")).toEqual({ run_id: "run", message_key: "message", text_offset: 0, text: prefix + ending + " newest" });
});

it("includes the durable overlap when the full cached observation fits and chooses only the latest eligible stream", () => {
  let state = interruptLive(applyLive(recoveryState(), delta("Older message", 0, "older-generation"), 1));
  state = interruptLive(applyLive(state, delta("Durable prefix. Newest phone-only tail", 0, "newer-generation"), 2));
  state = applyTimelinePage(state, "task", [message(1, "Durable prefix.", 0, "newer-generation")], false);
  // Even newer legacy and other-run activity is not eligible context.
  state = interruptLive(applyLive(state, { kind: "agent_text_delta", task_id: "task", run_id: "run", message_key: "native-legacy-key", text: "Legacy text" }, 3));
  const other = { ...delta("Another run", 0, "other-generation"), run_id: "old-run" };
  state = interruptLive(applyLive(state, other, 4));
  expect(observedReplyForTask(state, "task")).toEqual({ run_id: "run", message_key: "newer-generation", text_offset: 0, text: "Durable prefix. Newest phone-only tail" });
  expect(state.timelines.task?.items[0]?.item).toMatchObject({ text: "Durable prefix." });
});

it.each(["capability", "conversation", "current run", "run state", "nothing running", "run task", "uninterrupted", "legacy offset"])("does not send observed context without %s eligibility", reason => {
  let state = applyLive(recoveryState(), delta("Cached text"), 1);
  state = applyTimelinePage(state, "task", [message(1, "Cached")], false);
  if (reason !== "uninterrupted") state = interruptLive(state);
  if (reason === "capability") state = { ...state, capabilities: { ...state.capabilities!, features: [] } };
  if (reason === "conversation") state = { ...state, tasks: { task: { ...state.tasks.task!, conversation: false } } };
  if (reason === "current run") state = { ...state, tasks: { task: { ...state.tasks.task!, current_run: "other" } } };
  if (reason === "run state") state = { ...state, runs: { run: { ...state.runs.run!, state: "completed" } } };
  if (reason === "nothing running") state = { ...state, runs: { run: { ...state.runs.run!, nothing_running: false } } };
  if (reason === "run task") state = { ...state, runs: { run: { ...state.runs.run!, task_id: "other" } } };
  if (reason === "legacy offset") state = interruptLive(applyLive(recoveryState(), { kind: "agent_text_delta", task_id: "task", run_id: "run", message_key: "legacy", text: "Cached text" }, 1));
  expect(observedReplyForTask(state, "task")).toBeUndefined();
});

it("preserves a unique legacy cached prefix without deleting unmatched legacy streams", () => {
  let state = interruptLive(applyLive(emptyHostState, { kind: "agent_text_delta", task_id: "task", run_id: "run", message_key: "old", text: "Sentence 41. Sentence 42 newest" }, 0));
  const partial: TimelineItem = { seq: 1, at: 1, run_id: "run", item: { kind: "agent_message", text: "Sentence 41.", partial: true } };
  state = applyTimelinePage(state, "task", [partial], false);
  expect(shown(state)).toEqual(["Sentence 41. Sentence 42 newest"]);
  state = applyLive(state, { kind: "agent_text_delta", task_id: "task", run_id: "run", message_key: "other", text: "Unmatched helper tail" }, 2);
  state = applyTimelinePage(state, "task", [{ ...partial, seq: 2, item: { kind: "agent_message", text: "A different checkpoint", partial: true } }], false);
  expect(liveTextForTask(state, "task").map(l => l.text)).toEqual(["Unmatched helper tail"]);
});
