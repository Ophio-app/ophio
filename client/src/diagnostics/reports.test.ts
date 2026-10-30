// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { CommandReceipt, ReportBundle, ReportSummary } from "../protocol/types";
import { HostSession } from "../host/session";
import type { ControlLink } from "../host/link";
import { capabilities } from "../dev/sim/scenario";
import { UiStore } from "../app/ui";
import { ReportService } from "./reports";
import { PhoneTrail } from "./trail";
import { CrashRegistry } from "./crashes";
import { describeBundle } from "./bundle";

const receipt: CommandReceipt = { command_id: "fake", status: "accepted", result: null, error: null, seq: 1 };
const part = { versions: { build: "computer-build", os: "Linux", protocol: { major: 1, minor: 0 }, features: ["reports_v1"], harnesses: [] }, trail: [], crashes: [], extras: [], sending: true };
function setup(feature = true, online = true, direct = true) {
  const session = new HostSession("fake-profile", {} as ControlLink);
  session.state.set(s => ({ ...s, capabilities: { ...capabilities(), features: feature ? ["reports_v1"] : [] } }));
  session.link.set(online ? { state: "online", since: 1 } : { state: "idle" });
  const query = vi.spyOn(session, "query").mockResolvedValue({ kind: "report_preview", part });
  const command = vi.spyOn(session, "command").mockResolvedValue(receipt);
  const intake = { available: vi.fn(async () => direct), submit: vi.fn(async (bundle: ReportBundle) => ({ report_id: bundle.report_id, reference: "ignored", status: { state: "received" as const } })),
    status: vi.fn(async () => ({ reports: [], now: 1, paused: false })), answer: vi.fn(async () => ({ reports: [], now: 1, paused: false })), withdraw: vi.fn(async () => ({ reports: [], now: 1, paused: false })), releases: vi.fn(async () => ({ releases: [], now: 1 })) };
  const crashes = new CrashRegistry();
  const service = new ReportService({ intake, crashes, trail: new PhoneTrail() });
  const off = service.attach(session, new UiStore("fake-profile"));
  return { service, intake, session, query, command, crashes, off };
}
beforeEach(() => { localStorage.clear(); vi.useFakeTimers(); });
afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); });
it("previews and submits through a reachable computer advertising reports_v1", async () => {
  const { service, query, command, intake } = setup();
  const draft = await service.startReport({ type: "settings" }, { screen: "/settings" });
  expect(draft.id).toMatch(/^[0-9a-f-]{14}7/); expect(draft.salt).toMatch(/^[0-9a-f]{32}$/); expect(draft.reference).toHaveLength(6);
  expect(draft.phoneOnly).toBe(false); expect(query).toHaveBeenCalledWith({ kind: "report_preview", salt: draft.salt, extras: [] });
  service.setWords(draft, "I expected the task to start.");
  expect(draft.description).toEqual(describeBundle(draft.bundle));
  expect(service.send(draft)).toBe(draft.reference);
  expect(JSON.parse(localStorage.getItem("diagnostics.reports.v1")!).queued).toHaveLength(1);
  await service.flush();
  expect(command).toHaveBeenCalledWith({ command: "submit_report", bundle: draft.bundle }, { label: "Send report" });
  expect(JSON.stringify(command.mock.calls)).not.toContain(draft.salt);
  expect(intake.submit).not.toHaveBeenCalled(); expect(service.get().queued).toEqual([]);
});
it.each([[false, true], [true, false]])("sends directly when reports_v1=%s and online=%s", async (feature, online) => {
  const { service, intake, command, query } = setup(feature, online);
  const draft = await service.startReport({ type: "error" }, { screen: "/settings" });
  expect(draft.phoneOnly).toBe(true); expect(query).not.toHaveBeenCalled();
  const reference = service.send(draft); await service.flush();
  expect(reference).toBe(draft.reference); expect(command).not.toHaveBeenCalled(); expect(intake.submit).toHaveBeenCalledOnce();
  expect(service.get().direct[0]?.report_id).toBe(draft.id); expect(service.get().queued).toHaveLength(0);
});
it("uses direct when the computer has sending off, or when command submission fails", async () => {
  const one = setup(); one.query.mockResolvedValue({ kind: "report_preview", part: { ...part, sending: false } });
  const draft = await one.service.startReport({ type: "settings" }, { screen: "/settings" }); one.service.send(draft); await one.service.flush();
  expect(one.command).not.toHaveBeenCalled(); expect(one.intake.submit).toHaveBeenCalledOnce(); one.off();
  localStorage.clear(); const two = setup(); two.command.mockRejectedValue(new Error("connection dropped"));
  const second = await two.service.startReport({ type: "settings" }, { screen: "/settings" }); two.service.send(second); await two.service.flush();
  expect(two.command).toHaveBeenCalledOnce(); expect(two.intake.submit).toHaveBeenCalledOnce();
});
it("queues durably when both paths fail, reloads and retries the exact bundle and id", async () => {
  const { service, intake, command, crashes } = setup(false, false, true);
  crashes.capture("first crash"); intake.submit.mockRejectedValue(new Error("offline"));
  const draft = await service.startReport({ type: "crash" }, { screen: "/settings" }); service.send(draft); await service.flush();
  expect(service.get().queued).toHaveLength(1); expect(crashes.pending()).toEqual([]);
  const retry = new ReportService({ intake, crashes, trail: new PhoneTrail() });
  expect(retry.get().queued[0]?.bundle).toEqual(JSON.parse(JSON.stringify(draft.bundle)));
  intake.submit.mockImplementation(async bundle => ({ report_id: bundle.report_id, reference: draft.reference, status: { state: "received" } }));
  await retry.flush(); expect(retry.get().queued).toEqual([]); expect(retry.get().direct).toHaveLength(1); expect(command).not.toHaveBeenCalled();
  expect(intake.submit.mock.calls[0]?.[0].report_id).toBe(intake.submit.mock.calls[1]?.[0].report_id);
});
it("keeps the report and pending crashes if durable storage refuses the save", async () => {
  const { intake, crashes } = setup(false, false, false); crashes.capture("pending");
  const service = new ReportService({ storage: { getItem: () => null, setItem: () => { throw new Error("full"); } }, intake, crashes });
  const draft = await service.startReport({ type: "crash" }, { screen: "/settings" });
  expect(() => service.send(draft)).toThrow("couldn’t be saved"); expect(crashes.pending()).toHaveLength(1);
});
it("keeps slim copies of sent reports and lets them go when a new report needs the room", async () => {
  const { intake, crashes } = setup(false, false, true);
  const items = new Map<string, string>(); let room = Infinity;
  const storage = { getItem: (k: string) => items.get(k) ?? null, setItem: (k: string, v: string) => { if (v.length > room) throw new Error("QuotaExceededError"); items.set(k, v); } };
  const service = new ReportService({ storage, intake, crashes, trail: new PhoneTrail() });
  const shot = { data: "A".repeat(400_000), media_type: "image/png" as const, width: 1, height: 1, marked: false };
  const ids: string[] = [];
  for (let i = 0; i < 3; i++) {
    vi.setSystemTime(1_000 + i);
    const draft = await service.startReport({ type: "settings" }, { screen: "/settings" });
    service.setScreenshot(draft, shot); service.send(draft); await service.flush(); ids.push(draft.id);
  }
  expect(Object.keys(service.get().sent).sort()).toEqual([...ids].sort());
  expect(Object.values(service.get().sent).every(b => !b.screenshot)).toBe(true);
  expect(items.get("diagnostics.reports.v1")!.length).toBeLessThan(400_000);
  room = items.get("diagnostics.reports.v1")!.length + 200;
  const next = await service.startReport({ type: "settings" }, { screen: "/settings" });
  service.setWords(next, "Still room for this one."); service.send(next);
  expect(service.get().queued.map(q => q.bundle.report_id)).toEqual([next.id]);
  expect(service.get().sent[ids[0]!]).toBeUndefined(); expect(Object.keys(service.get().sent).sort()).toEqual([ids[1]!, ids[2]!].sort());
  expect(JSON.parse(items.get("diagnostics.reports.v1")!).sent).toEqual(service.get().sent);
});
it("never retries through a different paired computer", async () => {
  const { service, session, command } = setup(true, false, false);
  const draft = await service.startReport({ type: "settings" }, { screen: "/settings" }); service.send(draft); await service.flush();
  const second = new HostSession("another-computer", {} as ControlLink); second.state.set(session.state.get()); second.link.set({ state: "online", since: 1 });
  const otherCommand = vi.spyOn(second, "command").mockResolvedValue(receipt);
  service.attach(second, new UiStore("another-computer")); await service.flush();
  expect(command).not.toHaveBeenCalled(); expect(otherCommand).not.toHaveBeenCalled(); expect(service.get().queued).toHaveLength(1);
});
it("clears extras immediately and ignores a late preview that would restore withdrawn consent", async () => {
  const { service, query, session } = setup(); const draft = await service.startReport({ type: "settings" }, { screen: "/settings" });
  let resolve!: (value: { kind: "report_preview"; part: typeof part }) => void;
  query.mockImplementation(() => new Promise(r => { resolve = r; }));
  const enabling = service.toggleExtra(draft, { type: "names" }, true);
  session.link.set({ state: "idle" }); await service.toggleExtra(draft, { type: "names" }, false);
  resolve({ kind: "report_preview", part: { ...part, extras: [{ type: "names", names: [{ name: "private", pseudonym: "t:12345678" }] }] } } as never);
  await enabling; expect(draft.extras).toEqual([]); expect(draft.bundle.extras).toEqual([]); expect(JSON.stringify(draft.description)).not.toContain("private");
});
it("polls direct report status at a calm interval only while visible", async () => {
  const { service, intake } = setup(false, false, true);
  const draft = await service.startReport({ type: "settings" }, { screen: "/settings" }); service.send(draft); await service.flush();
  const summary: ReportSummary = service.get().direct[0]!;
  intake.status.mockResolvedValue({ reports: [{ report_id: draft.id, history: [{ at: 5, status: { state: "reproduced" } }], questions: [] }], now: 10, paused: false } as never);
  await service.poll(); expect(service.get().direct[0]?.history.at(-1)?.status.state).toBe("reproduced");
  expect(service.get().direct[0]?.signature).toEqual(summary.signature);
  const off = service.follow(); await vi.advanceTimersByTimeAsync(0); const calls = intake.status.mock.calls.length;
  await vi.advanceTimersByTimeAsync(4 * 60_000); expect(intake.status).toHaveBeenCalledTimes(calls);
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden"); await vi.advanceTimersByTimeAsync(60_000); expect(intake.status).toHaveBeenCalledTimes(calls);
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible"); await vi.advanceTimersByTimeAsync(5 * 60_000); expect(intake.status).toHaveBeenCalledTimes(calls + 1); off();
});
it("gates answer and withdrawal requests for older computers", async () => {
  const { service, command } = setup(false, true, false);
  await expect(service.answer({ report_id: "fake", question_id: "fake", answer: { at: 1, choice: { choice: "yes" } } })).rejects.toThrow("Connect");
  await expect(service.withdraw("fake")).rejects.toThrow("Connect"); expect(command).not.toHaveBeenCalled();
});
it("keeps unsolicited extras out of the preview and selects the build for the failing side", async () => {
  const { service, query } = setup();
  query.mockResolvedValue({ kind: "report_preview", part: { ...part, extras: [{ type: "names", names: [{ name: "Unrequested private name", pseudonym: "t:12345678" }] }] } });
  const draft = await service.startReport({ type: "error" }, { screen: "/receipt", command: "start_task", error: { code: "internal" } });
  expect(draft.bundle.signature.build).toBe("computer-build"); expect(draft.bundle.extras).toEqual([]);
  await service.toggleExtra(draft, { type: "names" }, true); expect(draft.bundle.extras).toHaveLength(1);
  await service.toggleExtra(draft, { type: "names" }, false); expect(draft.bundle.extras).toEqual([]);
});
it("matches a crash report against the build that actually crashed", async () => {
  const { service, crashes } = setup(false, false, false); crashes.build = "older-phone-build"; crashes.capture(new TypeError("scrubbed"));
  const draft = await service.startReport({ type: "crash" }, { screen: "/talk" });
  expect(draft.bundle.signature).toEqual({ code: "js:TypeError@unknown", screen: "/talk", build: "older-phone-build" });
});
it("also gates raw session report queries and commands before they reach the transport", async () => {
  const transport = { send: vi.fn(() => true) } as unknown as ControlLink;
  const session = new HostSession("older", transport);
  for (const query of [{ kind: "reports" }, { kind: "report", report_id: "fake" }, { kind: "report_preview", salt: "fake", extras: [] }] as const) {
    await expect(session.query(query as Parameters<HostSession["query"]>[0])).rejects.toMatchObject({ error: { code: "incompatible_version" } });
  }
  await expect(session.command({ command: "withdraw_report", report_id: "fake" })).rejects.toMatchObject({ error: { code: "incompatible_version" } });
  await expect(session.command({ command: "set_report_settings", sending: true, ring: "everyone" })).rejects.toMatchObject({ error: { code: "incompatible_version" } });
  expect(transport.send).not.toHaveBeenCalled();
});
it("keeps a directly delivered bundle for review after restarting, and withdrawal removes it", async () => {
  const { service, intake } = setup(false, false, true);
  const draft = await service.startReport({ type: "settings" }, { screen: "/settings" });
  service.send(draft); await service.flush();
  const restarted = new ReportService({ intake });
  expect(restarted.get().sent[draft.id]).toEqual(JSON.parse(JSON.stringify(draft.bundle)));
  await restarted.withdraw(draft.id);
  expect(restarted.get().sent[draft.id]).toBeUndefined();
});
it("keeps reports on the phone while sending is off and can withdraw a queued report", async () => {
  const { service, intake } = setup(false, false, true);
  service.setSettings({ sending: false, ring: "early", revision: 1 });
  const draft = await service.startReport({ type: "settings" }, { screen: "/settings" });
  service.send(draft); await service.flush();
  expect(intake.submit).not.toHaveBeenCalled(); expect(service.get().queued).toHaveLength(1);
  await service.withdraw(draft.id);
  expect(service.get().queued).toHaveLength(0);
  expect(service.get().direct[0]?.history.at(-1)?.status.state).toBe("withdrawn");
});
