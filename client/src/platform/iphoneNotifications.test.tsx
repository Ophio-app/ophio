// SPDX-License-Identifier: Apache-2.0
// The path without a background watcher (iPhone, a browser): the app raises
// its own notifications while it is open, by this device's choices kept in storage.
import { act } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { HostSession } from "../host/session";
import type { Platform } from "../host/link";
import type { AttentionItem } from "../protocol/types";
import { deviceTiers } from "./deviceTiers";
import { notificationSettings } from "./notificationSettings";
import { notificationDetail, watchAttention } from "./notifications";

let platform: Platform;
let session: HostSession;
let stop = () => {};
const advance = async (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
function visibility(value: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, value });
  document.dispatchEvent(new Event("visibilitychange"));
}
const device = () => window.__sim!.device;

beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  visibility("visible");
  notificationDetail.set("minimal");
  const sim = createSimPlatform();
  // No watcher here, as on an iPhone.
  delete (sim as { alerts?: unknown }).alerts;
  platform = sim;
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing fixture profile");
  session = new HostSession(profile.profileId, platform.control(profile.profileId));
  session.start();
  await advance(900);
  stop = watchAttention(platform, session, profile.name);
  await advance();
  visibility("hidden");
});
afterEach(() => {
  stop(); session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); visibility("visible");
});

function raise(id: string, over: Partial<AttentionItem> = {}) {
  const original = session.state.get().attention[ids.atReady];
  if (!original) throw new Error("Missing attention fixture");
  act(() => session.state.set((s) => ({ ...s, attention: { ...s.attention, [id]: { ...original, attention_id: id, ...over } } })));
}
const posted = () => device().notifications;
const bell = (on: boolean) => act(() => session.state.set((s) => {
  const task = s.tasks[ids.tHeaders]!;
  return { ...s, tasks: { ...s.tasks, [ids.tHeaders]: { ...task, notify_done: on } } };
}));
const attentionTask = () => session.state.get().attention[ids.atReady]!.task_id!;
const setSettings = (project: object) => act(() => notificationSettings(session).set({ view: { off: [], bundle: false, revision: 1, projects: [{ project_id: ids.pTide, mute: false, on: [], off: [], ...project }] }, error: null }));

it("has no watcher, and keeps the choices on the device", () => {
  expect(platform.alerts).toBeUndefined();
  expect(deviceTiers(platform).get().native).toBe(false);
  expect(deviceTiers(platform).tier("question")).toBe("alert");
  expect(deviceTiers(platform).tier("finished")).toBe("off");
});

it("posts a question, as an alert of the question type", async () => {
  raise("ask", { kind: "question" }); await advance();
  expect(posted()).toHaveLength(1);
  expect(posted()[0]).toEqual(expect.objectContaining({ id: expect.stringMatching(/\/ask$/), alertType: "question", want: "alert", category: "needs_you" }));
  expect(posted()[0]!.silent).toBeUndefined();
});

it("does not post a finished task by default", async () => {
  raise("done"); await advance();
  expect(posted()).toHaveLength(0);
});

it("posts a finished task, silently, once this device's choice for it is Silent, and keeps the choice", async () => {
  await deviceTiers(platform).setTier("finished", "silent");
  raise("done"); await advance();
  expect(posted()).toHaveLength(1);
  expect(posted()[0]).toEqual(expect.objectContaining({ alertType: "finished", want: "silent", silent: true }));
  expect(JSON.parse(localStorage.getItem("alert-tiers")!)).toMatchObject({ finished: "silent", question: "alert" });
});

it("posts a finished task as an alert when this device's choice is Alert", async () => {
  await deviceTiers(platform).setTier("finished", "alert");
  raise("done"); await advance();
  expect(posted()).toHaveLength(1);
  expect(posted()[0]).toEqual(expect.objectContaining({ alertType: "finished", want: "alert" }));
  expect(posted()[0]!.silent).toBeUndefined();
});

it("stops posting once the choice goes back to Off", async () => {
  await deviceTiers(platform).setTier("finished", "alert");
  await deviceTiers(platform).setTier("finished", "off");
  raise("done"); await advance();
  expect(posted()).toHaveLength(0);
});

it("a task's bell makes its finish alert, even with the project muted", async () => {
  expect(attentionTask()).toBe(ids.tHeaders);
  bell(true);
  setSettings({ mute: true });
  raise("done"); await advance();
  expect(posted()).toHaveLength(1);
  expect(posted()[0]).toEqual(expect.objectContaining({ alertType: "finished", want: "alert" }));
  expect(posted()[0]!.silent).toBeUndefined();
});

it("a muted project posts nothing without the bell, not even a question", async () => {
  setSettings({ mute: true });
  raise("done"); raise("ask", { kind: "question" }); await advance();
  expect(posted()).toHaveLength(0);
});

it("a project's own choice of Off stops a question there", async () => {
  setSettings({ tiers: [{ type: "question", tier: "off" }] });
  raise("ask", { kind: "question" }); await advance();
  expect(posted()).toHaveLength(0);
  raise("approve", { kind: "approval" }); await advance();
  expect(posted().map((n) => n.alertType)).toEqual(["approval"]);
});

it("a project's own choice of Alert beats this device's Off", async () => {
  setSettings({ tiers: [{ type: "finished", tier: "alert" }] });
  raise("done"); await advance();
  expect(posted().map((n) => n.alertType)).toEqual(["finished"]);
});

it("does not post an item already handled on another device", async () => {
  raise("ask", { kind: "question", alert_handled_at: Date.now() }); await advance();
  expect(posted()).toHaveLength(0);
  raise("again", { kind: "question" }); await advance();
  expect(posted().map((n) => n.id.split("/").pop())).toEqual(["again"]);
});

it("does not post what the computer raised quietly", async () => {
  raise("held", { kind: "question", quiet: true }); await advance();
  expect(posted()).toHaveLength(0);
});
