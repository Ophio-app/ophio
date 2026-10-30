// SPDX-License-Identifier: Apache-2.0
// What holds a Work request back, said the same way wherever it waits: the
// Review for a new task, the Review for a queued one, and Next up's front item.
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { workIds } from "../../dev/sim/work";
import { HostSession } from "../../host/session";
import type { RunState } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Review from "../talk/Review";
import NextUp from "./NextUp";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "work" }, depth: 1, active: true }}>{children}</RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
  app.ui.selectTask(ids.pTide, null);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

const WRITER = "“Fix the flaky retry test in the payments client”";
type Writer = "working" | "awaiting_approval" | "none";

/** Puts tidewater-api's writer in `writer`, as the companion would report it. */
async function writerIs(writer: Writer) {
  const host = window.__sim!.host;
  if (writer === "none") {
    host.runs = host.runs.map((r) => (r.run_id === ids.rFlaky ? { ...r, state: "completed" as RunState, holds_writer: false } : r));
    host.projects = host.projects.map((p) => ({ ...p, active_writer: null }));
    host.queue = { ...host.queue, writer: null };
  } else host.runs = host.runs.map((r) => (r.run_id === ids.rFlaky ? { ...r, state: writer } : r));
  act(() => {
    host.emit({ type: "run_upserted", run: host.runs.find((r) => r.run_id === ids.rFlaky)! });
    for (const project of host.projects) host.emit({ type: "project_upserted", project });
    host.emit({ type: "queue_changed", queue: host.queue });
  });
  await advance();
}

const frontItem = () => window.__sim!.host.queue.items.find((i) => i.state.state !== "started")!;
const pause = () => {
  const host = window.__sim!.host;
  host.queue = { ...host.queue, paused: true, pause_reason: "Paused for review." };
  act(() => host.emit({ type: "queue_changed", queue: host.queue }));
};

const cases: [Writer, string | null, string][] = [
  ["awaiting_approval", `${WRITER} is waiting for your approval. It is changing files in tidewater-api, so this can’t start until it finishes.`, `Waiting for your approval on ${WRITER}`],
  ["working", `${WRITER} is changing files in tidewater-api, so this can’t start until it finishes.`, `After ${WRITER} finishes`],
  ["none", null, "Waiting"],
];

it.each(cases)("a new Work request says what the writer is doing (%s) and links to it", async (writer, why) => {
  await writerIs(writer);
  app.ui.saveDraft(ids.pTide, null, { text: "Tidy the README" });
  render(<Review route={{ screen: "review", projectId: ids.pTide, taskId: null }} />, { wrapper });
  expect(screen.getByRole("radio", { name: "Start now" }).hasAttribute("disabled")).toBe(writer !== "none");
  if (!why) return expect(screen.queryByRole("button", { name: "Open it" })).toBeNull();
  expect(screen.getByText(why)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Open it" }));
  expect(app.ui.route()).toMatchObject({ screen: "talk" });
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(window.__sim!.host.runs.find((r) => r.run_id === ids.rFlaky)!.task_id);
});

it.each(cases)("a queued Work request says the same (%s), and a paused queue's note waits until it can start", async (writer, why) => {
  await writerIs(writer);
  pause();
  await advance();
  const item = frontItem();
  const route: Route = { screen: "review", projectId: ids.pTide, taskId: item.task_id, queueItemId: item.item_id };
  render(<Review route={route} />, { wrapper });
  await advance();
  const foot = document.querySelector(".review-foot")!;
  const start = screen.getByRole("button", { name: "Start only this task" });
  expect(start.hasAttribute("disabled")).toBe(!!why);
  if (!why) {
    expect(foot.textContent).toContain("Only this task will start.");
    return expect(screen.queryByRole("button", { name: "Open it" })).toBeNull();
  }
  expect(screen.getByText(why)).toBeTruthy();
  expect(foot.textContent).not.toContain("Only this task will start.");
  fireEvent.click(screen.getByRole("button", { name: "Open it" }));
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(window.__sim!.host.runs.find((r) => r.run_id === ids.rFlaky)!.task_id);
});

it.each(cases)("Next up's front item says what holds it (%s)", async (writer, _why, label) => {
  await writerIs(writer);
  render(<NextUp route={{ screen: "next_up" }} />, { wrapper });
  await advance();
  const states = [...document.querySelectorAll(".work-queue-state")].map((p) => p.textContent);
  expect(states[0]).toBe(label);
  // Only the front item can be held by the writer; the rest wait on it.
  expect(states.slice(1).every((s) => /^(After \d|Blocked)/.test(s ?? ""))).toBe(true);
});

it("only a front item that can start now reads Ready — needs your go-ahead", async () => {
  const host = window.__sim!.host;
  // An ask-first item moves to the front: the computer calls it awaiting its start.
  void app.session.command({ command: "queue_move", item_id: workIds.qReady, position: 1 }, { label: "Move", expectedRevision: host.queue.revision });
  await advance();
  render(<NextUp route={{ screen: "next_up" }} />, { wrapper });
  await advance();
  const front = () => document.querySelector(".work-queue-state")?.textContent;
  expect(document.querySelector(".work-queue h2")?.textContent).toBe("Review keyboard navigation");
  expect(frontItem().state.state).toBe("awaiting_start");
  expect(front()).toBe(`Waiting for your approval on ${WRITER}`);
  await writerIs("none");
  expect(front()).toBe("Ready — needs your go-ahead");
});

it("a queued task held by the writer takes its go-ahead now and starts by itself when it’s free", async () => {
  await writerIs("working");
  const host = window.__sim!.host;
  const item = frontItem();
  host.queue = { ...host.queue, items: host.queue.items.map((i) => (i.item_id === item.item_id ? { ...i, authorization: "ask_before_start" as const, authorization_expires_at: undefined } : i)) };
  act(() => host.emit({ type: "queue_changed", queue: host.queue }));
  await advance();
  render(<Review route={{ screen: "review", projectId: ids.pTide, taskId: item.task_id, queueItemId: item.item_id }} />, { wrapper });
  await advance();
  expect(screen.getByText(`${WRITER} is changing files in tidewater-api, so this can’t start until it finishes.`)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Start queued task" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Start when it’s free" }));
  await advance();
  expect(host.queue.items.find((i) => i.item_id === item.item_id)?.authorization).toBe("auto_start");
  // Nothing starts under the writer: it waits its turn with the go-ahead in hand.
  expect(host.queue.items.find((i) => i.item_id === item.item_id)?.state.state).not.toBe("started");
});
