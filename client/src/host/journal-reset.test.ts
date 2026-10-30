// SPDX-License-Identifier: Apache-2.0
//
// A computer that restarts without its journal numbers new facts from where
// its records now end. The phone must not mistake them for replays.
import { afterEach, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { createElement } from "react";
import { AppProvider, type App } from "../app/context";
import { keepOutbox } from "../app/outbox";
import { UiStore } from "../app/ui";
import { check, repeatable, resend, send, useAutoSettle } from "../screens/talk/send";
import { HostSession } from "./session";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";

afterEach(() => {
  window.__sim?.host.dispose();
  vi.useRealTimers();
});

// Restoring a paired database can lose a command without leaving the
// journal behind the phone. Real-store counterpart: backup_restore_* in
// crates/services/src/restart_recovery_tests.rs.
it.each(["check", "repeat"])("%s must stay uncertain after a restored journal catches up", async (action) => {
  vi.useFakeTimers();
  localStorage.clear();
  const disk = new Map<string, string>();
  const platform = createSimPlatform();
  platform.outbox = {
    async put(_profile, id, record) { disk.set(id, record); },
    async list() { return [...disk.values()]; },
    async remove(_profile, id) { disk.delete(id); },
  };
  const profile = (await platform.profiles())[0]!;
  const makeApp = (): App => ({ platform, profile, profiles: [profile],
    session: new HostSession(profile.profileId, platform.control(profile.profileId)), ui: new UiStore(profile.profileId),
    switchTo() {}, pairAnother() {}, async rename() {}, async forget() {},
  });
  const first = makeApp();
  let second: App | undefined;
  let stopOutbox: (() => void) | undefined;
  try {
    first.session.start();
    await vi.advanceTimersByTimeAsync(900);
    const old = window.__sim!.host;
    const task = old.tasks[0]!;
    const req = { projectId: task.project_id, draftTaskId: task.task_id, text: "Effect before backup restore", annotationId: null, label: "Rename",
      expectedRevision: task.revision, build: () => ({ command: "rename_task" as const, task_id: task.task_id, title: "Effect before backup restore" }) };
    first.ui.saveDraft(req.projectId, req.draftTaskId, { text: req.text });
    window.__sim!.dropNextReceipt();
    const sent = send(first, req, () => {});
    await vi.advanceTimersByTimeAsync(300);
    await sent;
    const id = first.ui.draft(req.projectId, req.draftTaskId)!.attempt!.commandId!;
    expect(old.receipts.get(id)?.status).toBe("accepted");
    expect(old.tasks.find(t => t.task_id === task.task_id)!.title).toBe(req.text);
    const previousHead = first.session.state.get().journalHead;
    first.session.stop();
    first.ui.persistNow();

    // The restored database still knows this device and task, but predates
    // this command. Startup/other activity catches up before the phone returns.
    window.__sim!.restart();
    const restoredHost = window.__sim!.host;
    Object.defineProperty(restoredHost, "bootEpoch", { value: "boot-after-backup-restore" });
    while (restoredHost.seq <= previousHead) {
      restoredHost.emit({ type: "harnesses_changed", harnesses: first.session.state.get().harnesses });
    }
    expect(restoredHost.receipts.has(id)).toBe(false);
    second = makeApp();
    stopOutbox = keepOutbox(platform.outbox, profile.profileId, profile.name, second.ui);
    second.session.start();
    await vi.advanceTimersByTimeAsync(1000);
    expect(second.session.online).toBe(true);
    expect(second.session.state.get().bootEpoch).toBe("boot-after-backup-restore");
    const attempt = second.ui.draft(req.projectId, req.draftTaskId)!.attempt!;
    expect(attempt.phase).toBe("unknown");
    if (action === "check") {
      const checking = check(second, req.projectId, req.draftTaskId, attempt);
      await vi.advanceTimersByTimeAsync(1000);
      await checking;
      expect(second.ui.draft(req.projectId, req.draftTaskId)!.attempt!.phase).toBe("unknown");
      expect(second.ui.draft(req.projectId, req.draftTaskId)!.attempt!.detail).toBe(`${profile.name} restarted or its records changed since this was sent, so it can’t confirm whether it acted. Check the result before sending again.`);
      // The explicit Send anyway path makes a new request with a new id.
      const fresh = send(second, req, () => {});
      await vi.advanceTimersByTimeAsync(1000);
      const outcome = await fresh;
      expect(outcome.ok).toBe(true);
      if (outcome.ok) expect(outcome.receipt.command_id).not.toBe(id);
      expect(restoredHost.receipts.has(id)).toBe(false);
    } else {
      expect.soft(repeatable(attempt, second.session)).toBe(false);
      const repeated = resend(second, req.projectId, req.draftTaskId, attempt, "Repeat").catch(e => e as Error);
      await vi.advanceTimersByTimeAsync(1000);
      expect.soft(await repeated).toBeInstanceOf(Error);
      expect(restoredHost.receipts.has(id)).toBe(false);
    }
  } finally {
    stopOutbox?.();
    second?.session.stop();
    first.session.stop();
  }
});

it.each([
  { restarted: false, receipt: "missing" },
  { restarted: true, receipt: "missing" },
  { restarted: true, receipt: "accepted" },
  { restarted: true, receipt: "rejected" },
])("reconciles a pending $receipt receipt with restarted=$restarted", async ({ restarted, receipt }) => {
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const control = platform.control(profile.profileId);
  const session = new HostSession(profile.profileId, control);
  const app: App = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId),
    switchTo() {}, pairAnother() {}, async rename() {}, async forget() {},
  };
  session.start();
  try {
    await vi.advanceTimersByTimeAsync(900);
    const host = window.__sim!.host;
    const task = host.tasks[0]!;
    const req = { projectId: task.project_id, draftTaskId: task.task_id, text: "Pending rename", annotationId: null, label: "Rename",
      expectedRevision: receipt === "rejected" ? -1 : task.revision,
      build: () => ({ command: "rename_task" as const, task_id: task.task_id, title: "Pending rename" }),
    };
    app.ui.saveDraft(req.projectId, req.draftTaskId, { text: req.text });
    if (receipt === "missing") {
      // The write leaves the phone but is lost before reaching the computer.
      const write = control.send.bind(control);
      vi.spyOn(control, "send").mockImplementation(frame => frame.type === "command" ? true : write(frame));
    } else window.__sim!.dropNextReceipt();
    const sent = send(app, req, () => {});
    await vi.advanceTimersByTimeAsync(300);
    window.__sim!.offline("Connection interrupted");
    await sent;
    const attempt = app.ui.draft(req.projectId, req.draftTaskId)!.attempt!;
    expect(attempt.phase).toBe("unknown");
    expect(host.receipts.get(attempt.commandId!)?.status ?? "missing").toBe(receipt);
    if (restarted) Object.defineProperty(host, "bootEpoch", { value: "next-boot" });
    window.__sim!.online();
    await vi.advanceTimersByTimeAsync(6000);
    const pending = session.commands.get()[attempt.commandId!]!;
    expect(pending.phase).toBe(receipt === "missing" ? (restarted ? "unknown" : "not_delivered") : receipt);
    const hook = renderHook(() => useAutoSettle(req.projectId, req.draftTaskId, attempt), {
      wrapper: ({ children }) => createElement(AppProvider, { app, children }),
    });
    hook.unmount();
    const draft = app.ui.draft(req.projectId, req.draftTaskId);
    if (receipt === "accepted") expect(draft).toBeNull();
    else {
      expect(draft?.attempt?.phase).toBe(pending.phase);
      expect(draft?.text).toBe(req.text);
      if (restarted && receipt === "missing") {
        expect(draft?.attempt?.detail).toContain(`${profile.name} restarted`);
        expect(repeatable(draft?.attempt ?? null, session)).toBe(false);
      }
    }
  } finally { session.stop(); }
});

it("a missing command after a detected journal rollback stays unknown", async () => {
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  session.start();
  try {
    await vi.advanceTimersByTimeAsync(900);
    const old = window.__sim!.host;
    for (let i = 0; i < 200; i++) old.emit({ type: "harnesses_changed", harnesses: session.state.get().harnesses });
    await vi.advanceTimersByTimeAsync(100);
    window.__sim!.dropNextReceipt();
    const task = old.tasks[0]!;
    // Use a simple admitted effect to establish the lost-answer case. A
    // native task can similarly have acted before its database is restored.
    const sent = session.command({ command: "rename_task", task_id: task.task_id, title: "Effect before rollback" }, { label: "Rename", expectedRevision: task.revision }).catch(() => undefined);
    await vi.advanceTimersByTimeAsync(250);
    const pending = Object.values(session.commands.get()).find(c => c.label === "Rename")!;
    expect(old.receipts.get(pending.id)?.status).toBe("accepted");
    window.__sim!.restart(); // Restored records omit the accepted command.
    await vi.advanceTimersByTimeAsync(6000);
    await sent;
    expect(session.state.get().gaps).not.toHaveLength(0);
    expect(session.online).toBe(true);
    expect(session.commands.get()[pending.id]?.phase).toBe("unknown");
  } finally {
    session.stop();
  }
});

it("a normal computer restart keeps its records, so the phone keeps its view and command outcomes", async () => {
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  session.start();
  try {
    await vi.advanceTimersByTimeAsync(900);
    const host = window.__sim!.host;
    const task = host.tasks[0]!;
    const loading = session.loadTimeline(task.task_id);
    await vi.advanceTimersByTimeAsync(300);
    await loading;
    const accepted = session.command({ command: "rename_task", task_id: task.task_id, title: "Before restart" }, { label: "Rename", expectedRevision: task.revision });
    await vi.advanceTimersByTimeAsync(250);
    expect((await accepted).status).toBe("accepted");
    const id = Object.values(session.commands.get()).find(c => c.label === "Rename")!.id;
    const history = session.commandHistory;
    const items = session.state.get().timelines[task.task_id]!.items.length;
    expect(items).toBeGreaterThan(0);

    // Same records, new process: only the boot epoch changes.
    Object.defineProperty(host, "bootEpoch", { value: "next-boot" });
    window.__sim!.offline("Computer restarting");
    await vi.advanceTimersByTimeAsync(100);
    window.__sim!.online();
    await vi.advanceTimersByTimeAsync(6000);

    const after = session.state.get();
    expect(session.online).toBe(true);
    expect(after.bootEpoch).toBe("next-boot");
    expect(after.gaps).toHaveLength(0);
    expect(after.timelines[task.task_id]!.items).toHaveLength(items);
    expect(session.commandHistory).not.toBe(history);
    expect(session.sameCommandHistory(history)).toBe(false);
    expect(session.commands.get()[id]?.phase).toBe("accepted");
  } finally {
    session.stop();
  }
});

it("a journal that restarts behind this device is reloaded, so an answered approval reads as answered", async () => {
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  session.start();
  await vi.advanceTimersByTimeAsync(900);
  const old = window.__sim!.host;
  for (let i = 0; i < 200; i++) old.emit({ type: "harnesses_changed", harnesses: session.state.get().harnesses });
  await vi.advanceTimersByTimeAsync(100);
  expect(session.state.get().journalHead).toBe(old.seq);

  // The simulated computer restarts with a fresh journal.
  window.__sim!.restart();
  await vi.advanceTimersByTimeAsync(6000);
  const host = window.__sim!.host;
  expect(host.seq).toBeLessThan(old.seq);
  const before = session.state.get();
  expect(session.link.get().state).toBe("online");
  expect(before.journalHead).toBe(host.seq);
  expect(before.gaps).toHaveLength(1);

  const approval = before.approvals[ids.aTest]!;
  const receipt = session.command(
    { command: "resolve_approval", approval_id: approval.approval_id, decision: { decision: "allow" } },
    { label: "Allow", expectedRevision: approval.revision },
  );
  await vi.advanceTimersByTimeAsync(1000);
  expect((await receipt).status).toBe("accepted");
  const after = session.state.get();
  expect(after.approvals[ids.aTest]?.state).toBe("allowed");
  expect(after.tasks[approval.task_id]?.attention_count).toBe(0);
  expect(after.journalHead).toBe(host.seq);
  session.stop();
});

it.each(["rollback", "lost phone storage"])("persisted outbox attempts remain unknown after %s across an app restart", async (change) => {
  vi.useFakeTimers();
  localStorage.clear();
  const disk = new Map<string, string>();
  const platform = createSimPlatform();
  platform.outbox = {
    async put(_profile, id, record) { disk.set(id, record); },
    async list() { return [...disk.values()]; },
    async remove(_profile, id) { disk.delete(id); },
  };
  const profile = (await platform.profiles())[0]!;
  const makeApp = (): App => ({ platform, profile, profiles: [profile],
    session: new HostSession(profile.profileId, platform.control(profile.profileId)), ui: new UiStore(profile.profileId),
    switchTo() {}, pairAnother() {}, async rename() {}, async forget() {},
  });
  const first = makeApp();
  first.session.start();
  await vi.advanceTimersByTimeAsync(900);
  const old = window.__sim!.host;
  for (let i = 0; i < 200; i++) old.emit({ type: "harnesses_changed", harnesses: first.session.state.get().harnesses });
  await vi.advanceTimersByTimeAsync(100);
  const task = old.tasks[0]!;
  const req = { projectId: task.project_id, draftTaskId: task.task_id, text: "Effect before restart", annotationId: null, label: "Rename",
    expectedRevision: task.revision, build: () => ({ command: "rename_task" as const, task_id: task.task_id, title: "Effect before restart" }) };
  first.ui.saveDraft(req.projectId, req.draftTaskId, { text: req.text });
  window.__sim!.dropNextReceipt();
  const sent = send(first, req, () => {});
  await vi.advanceTimersByTimeAsync(300);
  await sent;
  const id = first.ui.draft(req.projectId, req.draftTaskId)!.attempt!.commandId!;
  expect(old.receipts.get(id)?.status).toBe("accepted");
  first.session.stop();
  first.ui.persistNow();
  window.__sim!.restart();
  const host = window.__sim!.host;
  if (change === "lost phone storage") localStorage.clear();
  const second = makeApp();
  const stopOutbox = keepOutbox(platform.outbox, profile.profileId, profile.name, second.ui);
  try {
    second.session.start();
    await vi.advanceTimersByTimeAsync(1000);
    const restored = second.ui.draft(req.projectId, req.draftTaskId)!.attempt!;
    expect(restored.phase).toBe("unknown");
    const checking = check(second, req.projectId, req.draftTaskId, restored);
    await vi.advanceTimersByTimeAsync(1000);
    await checking;
    const attempt = second.ui.draft(req.projectId, req.draftTaskId)!.attempt!;
    expect(attempt.phase).toBe("unknown");
    expect(repeatable(attempt, second.session)).toBe(false);
    await expect(resend(second, req.projectId, req.draftTaskId, attempt, "Repeat")).rejects.toThrow("can’t be repeated safely");
    expect(host.receipts.has(id)).toBe(false);
    expect(disk.size).toBe(1);
  } finally { stopOutbox(); second.session.stop(); }
});
