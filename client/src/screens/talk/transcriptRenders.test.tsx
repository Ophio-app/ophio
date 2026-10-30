// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const parsed: string[] = [];
vi.mock("react-markdown", async (original) => {
  const actual = await original<typeof import("react-markdown")>();
  const Parse = actual.default;
  return { ...actual, default: (props: Parameters<typeof Parse>[0]) => { parsed.push(String(props.children)); return Parse(props); } };
});

vi.mock("./timeline", async (original) => {
  const actual = await original<typeof import("./timeline")>();
  return { ...actual, rows: vi.fn(actual.rows), stepsSummary: vi.fn(actual.stepsSummary) };
});

vi.mock("./handBackHistory", async (original) => {
  const actual = await original<typeof import("./handBackHistory")>();
  return { ...actual, withHandBacks: vi.fn(actual.withHandBacks) };
});

import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { TimelineItem } from "../../protocol/types";
import { applyEvent, applyLive, applyTimelinePage } from "../../state/projection";
import { withHandBacks } from "./handBackHistory";
import { rows, stepsSummary } from "./timeline";
import { Transcript } from "./Transcript";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

function conversation(runId: string): TimelineItem[] {
  return [1, 2, 3].flatMap((n): TimelineItem[] => [
    { seq: n * 10, at: n * 1000, run_id: runId, item: { kind: "user_message", text: `Question ${n}`, bundle: null, delivery: "native_delivered" } },
    { seq: n * 10 + 1, at: n * 1000 + 1, run_id: runId, item: { kind: "activity", tool: "Bash", summary: `Ran check ${n}`, status: "succeeded", detail: null, key: `k${n}` } },
    { seq: n * 10 + 2, at: n * 1000 + 2, run_id: runId, item: { kind: "agent_message", text: `Answer **${n}**`, partial: false } },
  ]);
}

it("parses each finished reply once while the clock ticks and a reply streams", async () => {
  const { session } = app;
  vi.spyOn(session, "loadTimeline").mockResolvedValue();
  const task = session.state.get().tasks[ids.tHeaders]!;
  session.state.set((s) => applyTimelinePage(s, task.task_id, conversation(task.current_run!), false));
  render(<AppProvider app={app}><Transcript task={task} agent="Claude Code" /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  expect(screen.getByText("3")).toBeTruthy();
  parsed.length = 0;

  await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
  for (const word of ["Checking ", "the ", "chart"]) {
    act(() => session.state.set((s) => applyLive(s, { kind: "agent_text_delta", task_id: task.task_id, run_id: task.current_run!, message_key: "m", text: word }, Date.now())));
  }

  expect(screen.getByText("Checking the chart")).toBeTruthy();
  expect(parsed).toEqual(["Checking ", "Checking the ", "Checking the chart"]);
});

it("builds the conversation's rows again only when its timeline or runs change", async () => {
  const { session } = app;
  vi.spyOn(session, "loadTimeline").mockResolvedValue();
  const task = session.state.get().tasks[ids.tHeaders]!;
  session.state.set((s) => applyTimelinePage(s, task.task_id, conversation(task.current_run!), false));
  render(<AppProvider app={app}><Transcript task={task} agent="Claude Code" /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  vi.mocked(rows).mockClear();

  await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
  act(() => session.state.set((s) => applyLive(s, { kind: "agent_text_delta", task_id: task.task_id, run_id: task.current_run!, message_key: "m", text: "Checking" }, Date.now())));
  expect(screen.getByText("Checking")).toBeTruthy();
  expect(rows).not.toHaveBeenCalled();

  const item: TimelineItem = { seq: 40, at: 4000, run_id: task.current_run!, item: { kind: "agent_message", text: "Checked", partial: false } };
  const seq = session.state.get().journalHead + 1;
  act(() => session.state.set((s) => applyEvent(s, { seq, at: 4000, source: { kind: "companion" }, provenance: "native", event: { type: "timeline_appended", task_id: task.task_id, item } }, Date.now())));
  expect(screen.getByText("Checked")).toBeTruthy();
  expect(rows).toHaveBeenCalledTimes(1);
});

it("leaves the conversation alone while a reply streams, activity changes and the clock ticks", async () => {
  const { session } = app;
  vi.spyOn(session, "loadTimeline").mockResolvedValue();
  const task = { ...session.state.get().tasks[ids.tHeaders]!, state: "working" as const, current_run: "run" };
  session.state.set((s) => applyTimelinePage({ ...s, tasks: { ...s.tasks, [task.task_id]: task } }, task.task_id, conversation("run"), false));
  render(<AppProvider app={app}><Transcript task={task} agent="Claude Code" /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  expect(screen.getByText("Working")).toBeTruthy();
  vi.mocked(withHandBacks).mockClear();

  await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
  act(() => session.state.set((s) => applyLive(s, { kind: "agent_text_delta", task_id: task.task_id, run_id: "run", message_key: "m", text: "Checking" }, Date.now())));
  act(() => session.state.set((s) => applyLive(s, { kind: "activity_status", task_id: task.task_id, run_id: "run", text: "Reading the chart" }, Date.now())));

  expect(screen.getByText("Checking")).toBeTruthy();
  expect(screen.getByText("Reading the chart")).toBeTruthy();
  expect(withHandBacks).not.toHaveBeenCalled();
});

it("renders only the row a new timeline item adds", async () => {
  const { session } = app;
  vi.spyOn(session, "loadTimeline").mockResolvedValue();
  const task = session.state.get().tasks[ids.tHeaders]!;
  session.state.set((s) => applyTimelinePage(s, task.task_id, conversation(task.current_run!), false));
  render(<AppProvider app={app}><Transcript task={task} agent="Claude Code" /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  expect(screen.getAllByText(/^Ran check/)).toHaveLength(3);
  vi.mocked(stepsSummary).mockClear();
  parsed.length = 0;

  const item: TimelineItem = { seq: 40, at: 4000, run_id: task.current_run!, item: { kind: "agent_message", text: "Checked", partial: false } };
  const seq = session.state.get().journalHead + 1;
  act(() => session.state.set((s) => applyEvent(s, { seq, at: 4000, source: { kind: "companion" }, provenance: "native", event: { type: "timeline_appended", task_id: task.task_id, item } }, Date.now())));

  expect(screen.getByText("Checked")).toBeTruthy();
  expect(parsed).toEqual(["Checked"]);
  expect(stepsSummary).not.toHaveBeenCalled();
});

it("keeps earlier pages while reading and releases them when the transcript closes", () => {
  const { session } = app;
  vi.spyOn(session, "loadTimeline").mockResolvedValue();
  const task = session.state.get().tasks[ids.tHeaders]!;
  const items: TimelineItem[] = Array.from({ length: 160 }, (_, seq) => ({ seq, at: seq, run_id: null, item: { kind: "note", text: `Step ${seq}` } }));
  session.state.set((s) => applyTimelinePage(s, task.task_id, items, false));
  const view = render(<AppProvider app={app}><Transcript task={task} agent="Claude Code" /></AppProvider>);
  const seq = session.state.get().journalHead + 1;
  act(() => session.state.set((s) => applyEvent(s, { seq, at: 4000, source: { kind: "companion" }, provenance: "native",
    event: { type: "timeline_appended", task_id: task.task_id, item: { ...items[0]!, seq: 160 } } }, 4000)));
  expect(session.state.get().timelines[task.task_id]!.items).toHaveLength(161);
  view.unmount();
  expect(session.state.get().timelineReaders).toEqual({});
  expect(session.state.get().timelines[task.task_id]!.items).toHaveLength(80);
  expect(session.state.get().timelines[task.task_id]!.hasMore).toBe(true);
});
