// SPDX-License-Identifier: Apache-2.0
import { type ReactNode } from "react";
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi, type MockInstance } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { InputAck, InputEnvelope } from "../../protocol/types";
import type { Picture } from "./useMedia";
import { useControl } from "./useControl";

let app: App;
let picture: { current: Picture };
let listeners: Set<(ack: InputAck) => void>;
let sent: MockInstance<HostSession["sendInput"]>;
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}>{children}</AppProvider>;
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const ack = (env: InputEnvelope, outcome: InputAck["outcome"] = "injected") => {
  act(() => { for (const listener of [...listeners]) listener({ lease_id: env.lease_id, seq: env.seq, outcome }); });
};

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start();
  await advance(900);
  picture = { current: { at: Date.now(), revision: 1, connected: true, error: null } as Picture };
  listeners = new Set();
  vi.spyOn(session, "onInputAck").mockImplementation(listener => {
    listeners.add(listener);
    return () => { listeners.delete(listener); };
  });
  sent = vi.spyOn(session, "sendInput").mockReturnValue(true);
});
afterEach(() => {
  cleanup();
  app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

async function ready() {
  const { result, rerender } = renderHook(() => useControl(ids.tgHdmi, picture, true), { wrapper });
  await advance(150);
  act(() => result.current.arm());
  const receipt = app.session.command({ command: "request_human_lease", target_id: ids.tgHdmi, geometry_revision: 1 }, { label: "Control" });
  await advance(650);
  await receipt;
  expect(result.current.enabled).toBe(true);
  return { result, rerender };
}

it("accepts only the exact lease and sequence, including an immediate ack; does not save live text", async () => {
  const { result } = await ready();
  const original = sent.getMockImplementation()!;
  sent.mockImplementation(env => { // An acknowledgement can arrive during transmit.
    for (const listener of [...listeners]) listener({ lease_id: env.lease_id, seq: env.seq, outcome: "injected" });
    return original(env);
  });
  let first!: Promise<boolean>;
  act(() => { first = result.current.sendLive({ action: "text", text: "secret 🦊" }, new AbortController().signal); });
  expect(await first).toBe(true);
  expect(result.current.savedText).toBe("");
  expect(result.current.confirmedText).toBeNull();
  expect(result.current.textPending).toBe(false);
  expect(result.current.pasteOffered).toBe(false);
  expect(localStorage.getItem(`control.pending.${app.profile.profileId}.${ids.tgHdmi}`)).toBeNull();
  sent.mockImplementation(original);
  const second = result.current.sendLive({ action: "key_chord", keys: ["ctrl", "c"] }, new AbortController().signal);
  const env = sent.mock.calls.at(-1)![0];
  act(() => { for (const listener of [...listeners]) listener({ lease_id: "other", seq: env.seq, outcome: "injected" }); });
  ack({ ...env, seq: env.seq - 1 });
  let settled = false;
  void second.then(() => { settled = true; });
  await act(async () => { await Promise.resolve(); });
  expect(settled).toBe(false);
  ack(env);
  expect(await second).toBe(true);
  expect(listeners.size).toBe(1); // Hook's ordinary recovery listener remains.
});

it("fails on refusal, abort, timeout and local send refusal without keeping sensitive text", async () => {
  const { result } = await ready();
  const refused = result.current.sendLive({ action: "text", text: "password" }, new AbortController().signal);
  ack(sent.mock.calls.at(-1)![0], "failed");
  expect(await refused).toBe(false);
  expect(result.current.savedText).toBe("");
  expect(result.current.pasteOffered).toBe(false);
  const untypable = result.current.sendLive({ action: "text", text: "秘密" }, new AbortController().signal);
  const rejected = sent.mock.calls.at(-1)![0];
  act(() => { for (const listener of [...listeners]) listener({
    lease_id: rejected.lease_id, seq: rejected.seq, outcome: "failed",
    refusal: { kind: "text_unavailable", characters: "秘密", paste_available: true },
  }); });
  expect(await untypable).toBe(false);
  expect(result.current.pasteOffered).toBe(false);
  expect(result.current.savedText).toBe("");
  expect(localStorage.getItem(`control.pending.${app.profile.profileId}.${ids.tgHdmi}`)).toBeNull();

  const cancel = new AbortController();
  const aborted = result.current.sendLive({ action: "key_chord", keys: ["Tab"] }, cancel.signal);
  cancel.abort();
  expect(await aborted).toBe(false);

  const timeout = result.current.sendLive({ action: "key_chord", keys: ["Tab"] }, new AbortController().signal);
  await advance(3_001);
  expect(await timeout).toBe(false);
  expect(result.current.error).toMatch(/did not confirm/);
  expect(listeners.size).toBe(1);
  sent.mockReturnValue(false);
  expect(await result.current.sendLive({ action: "text", text: "not sent" }, new AbortController().signal)).toBe(false);
  expect(result.current.savedText).toBe("");
  expect(listeners.size).toBe(1);
});

it("keeps stale-geometry recovery active for live input without retaining text", async () => {
  const { result } = await ready();
  const query = vi.spyOn(app.session, "query");
  const pending = result.current.sendLive({ action: "text", text: "private" }, new AbortController().signal);
  ack(sent.mock.calls.at(-1)![0], "stale_geometry");
  expect(await pending).toBe(false);
  expect(query).toHaveBeenCalledWith({ kind: "snapshot" });
  expect(result.current.enabled).toBe(false);
  expect(result.current.savedText).toBe("");
  expect(localStorage.getItem(`control.pending.${app.profile.profileId}.${ids.tgHdmi}`)).toBeNull();
});

it("shares the ordinary input gate for disabled, stale geometry and abort", async () => {
  const { result, rerender } = await ready();
  sent.mockClear();
  const ac = new AbortController(); ac.abort();
  expect(await result.current.sendLive({ action: "text", text: "x" }, ac.signal)).toBe(false);
  picture.current = { ...picture.current, revision: 99 };
  expect(await result.current.sendLive({ action: "text", text: "x" }, new AbortController().signal)).toBe(false);
  rerender();
  picture.current = { ...picture.current, revision: 1, connected: false };
  expect(await result.current.sendLive({ action: "text", text: "x" }, new AbortController().signal)).toBe(false);
  expect(sent.mock.calls.filter(([env]) => env.action.action === "text")).toHaveLength(0);
});

it("cleans up its listener and timeout if the transport throws", async () => {
  const { result } = await ready();
  sent.mockImplementation(() => { throw new Error("closed transport"); });
  expect(await result.current.sendLive({ action: "text", text: "private" }, new AbortController().signal)).toBe(false);
  expect(listeners.size).toBe(1);
  await advance(3_001);
  expect(result.current.error ?? "").not.toMatch(/did not confirm/);
  expect(result.current.savedText).toBe("");
});
