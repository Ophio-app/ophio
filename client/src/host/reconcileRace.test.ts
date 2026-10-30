// SPDX-License-Identifier: Apache-2.0
// After a reconnect the session asks the computer about each command whose
// answer was lost. If the person repeats that command (same id) while the
// question is on its way, the stale "never received" answer must not
// overwrite the repeat's own state: the repeat may have arrived.
import { afterEach, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { createElement } from "react";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { resend, send, useAutoSettle } from "../screens/talk/send";
import { HostSession } from "./session";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import type { ClientFrame } from "../protocol/types";

afterEach(() => {
  window.__sim?.host.dispose();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it("a stale status answer does not mark a repeated command as never received", async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const control = platform.control(profile.profileId);
  const session = new HostSession(profile.profileId, control);
  const app: App = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId),
    switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start();
  try {
    await vi.advanceTimersByTimeAsync(900);
    const host = window.__sim!.host;
    const text = "Also check the legend.";
    const req = { projectId: ids.pTide, draftTaskId: ids.tDash, text, annotationId: null, label: "Send",
      build: () => ({ command: "send_message" as const, task_id: ids.tDash, text, bundle: null, delivery: "next_turn" as const }) };
    const received = () => host.receipts.size;
    const before = received();
    app.ui.saveDraft(req.projectId, req.draftTaskId, { text });

    // 1. The first send is lost on its way; the connection drops.
    const write = control.send.bind(control);
    let lost: Extract<ClientFrame, { type: "command" }> | null = null;
    const wire = vi.spyOn(control, "send").mockImplementation((frame) => {
      if (frame.type === "command" && !lost) { lost = frame; return true; }
      return write(frame);
    });
    const first = send(app, req, () => {});
    await vi.advanceTimersByTimeAsync(100);
    window.__sim!.offline("Connection interrupted");
    await first;
    const attempt = app.ui.draft(req.projectId, req.draftTaskId)!.attempt!;
    expect(attempt.phase).toBe("unknown");

    // 2. Back on a slow network: the session asks about it, and before that
    //    answer arrives the person chooses Send again (the same id).
    window.__sim!.latency(2000);
    window.__sim!.online();
    while (!session.online) await vi.advanceTimersByTimeAsync(50);
    let repeat: Extract<ClientFrame, { type: "command" }> | null = null;
    wire.mockImplementation((frame) => {
      if (frame.type === "command") { repeat = frame; return true; }
      return write(frame);
    });
    const again = resend(app, req.projectId, req.draftTaskId, attempt, "Send again");
    await vi.advanceTimersByTimeAsync(0);
    expect(repeat!.command_id).toBe(attempt.commandId);

    // 3. The stale answer ("not received yet") arrives, then the connection
    //    drops before the repeat is answered. The computer did get the repeat.
    await vi.advanceTimersByTimeAsync(2100);
    window.__sim!.offline("Connection interrupted");
    await host.command({ command_id: repeat!.command_id, issued_at: repeat!.issued_at, expires_at: repeat!.expires_at, expected_revision: repeat!.expected_revision, body: repeat!.body });
    expect(received()).toBe(before + 1);
    expect((await again).ok).toBe(false);

    // The repeat may have arrived: the phone must not say it never did.
    expect.soft(session.commands.get()[attempt.commandId!]?.phase).toBe("unknown");
    const hook = renderHook(() => useAutoSettle(req.projectId, req.draftTaskId, app.ui.draft(req.projectId, req.draftTaskId)!.attempt!), {
      wrapper: ({ children }) => createElement(AppProvider, { app, children }),
    });
    hook.unmount();
    expect(app.ui.draft(req.projectId, req.draftTaskId)?.attempt).toMatchObject({ phase: "unknown", commandId: attempt.commandId });
  } finally { session.stop(); }
});
