// SPDX-License-Identifier: Apache-2.0
import { StrictMode, type ReactNode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { commandResult } from "../../host/commandResult";
import { useControl } from "./useControl";
import SafetyStrip from "./SafetyStrip";
import type { Picture } from "./useMedia";
import { TerminalOffset } from "./terminalBuffer";

let app: App;
const picture = {
  current: { at: 0, revision: 1, connected: true, error: null } as Picture,
};
const advance = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
// The rail's Stop is the symbol alone; which task and display it ends is the
// quiet text beside it.
const stopText = (stop: HTMLElement) => stop.closest(".named-stop")?.querySelector(".named-stop__caption")?.textContent ?? stop.textContent ?? "";
const wrapper = ({ children }: { children: ReactNode }) => (
  <StrictMode>
    <AppProvider app={app}>{children}</AppProvider>
  </StrictMode>
);

beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(
    profile.profileId,
    platform.control(profile.profileId),
  );
  app = {
    platform,
    profile,
    profiles: [profile],
    session,
    ui: new UiStore(profile.profileId),
    switchTo: vi.fn(),
    pairAnother: vi.fn(),
    rename: vi.fn(),
    forget: async () => undefined,
  };
  session.start();
  await advance(900);
  picture.current = {
    at: Date.now(),
    revision: app.session.state.get().targets[0]?.geometry.revision ?? 1,
    connected: true,
    error: null,
  };
});
afterEach(async () => {
  cleanup();
  await act(async () => {
    await Promise.resolve();
  });
  app.session.stop();
  window.__sim?.host.dispose();
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it("opening under StrictMode does not acquire or pause; navigation pauses and returning never resumes", async () => {
  const { rerender } = renderHook(
    ({ visible }) => useControl(ids.tgDp1, picture, visible),
    { wrapper, initialProps: { visible: true } },
  );
  await advance(100);
  expect(Object.values(app.session.commands.get())).toHaveLength(0);
  expect(app.session.state.get().control[ids.tgDp1]?.input_state).toBe("agent");
  rerender({ visible: false });
  await advance(800);
  expect(
    Object.values(app.session.commands.get()).map((c) => c.body.command),
  ).toEqual(["pause_input"]);
  expect(app.session.state.get().control[ids.tgDp1]?.input_state).toBe(
    "paused",
  );
  picture.current.at = Date.now();
  rerender({ visible: true });
  await advance(150);
  expect(
    Object.values(app.session.commands.get()).map((c) => c.body.command),
  ).toEqual(["pause_input"]);
});

it("releases a grant received after navigating away and never injects an action", async () => {
  const target = app.session.state
    .get()
    .targets.find((t) => t.target_id === ids.tgHdmi);
  if (!target) throw new Error("Missing target");
  picture.current.revision = target.geometry.revision;
  const { result, rerender } = renderHook(
    ({ visible }) => useControl(ids.tgHdmi, picture, visible),
    { wrapper, initialProps: { visible: true } },
  );
  await advance(100);
  act(() => result.current.arm());
  // A host grant arriving after the screen became hidden must be relinquished.
  rerender({ visible: false });
  await advance(0);
  const granting = app.session.command(
    {
      command: "request_human_lease",
      target_id: ids.tgHdmi,
      geometry_revision: target.geometry.revision,
    },
    { label: "Requesting control" },
  );
  await advance(200);
  await granting;
  await advance(200);
  expect(
    Object.values(app.session.commands.get()).map((c) => c.body.command),
  ).toEqual(["request_human_lease", "release_lease"]);
  expect(app.session.state.get().control[ids.tgHdmi]?.lease?.state).toBe("revoked");
  expect(result.current.send({ action: "text", text: "must not send" })).toBe(
    false,
  );
  expect(
    window.__sim?.control
      .inputs()
      .every((i) => i.action.action === "release_all"),
  ).toBe(true);
});

it("unrelated control observations do not renew an expiring lease", async () => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing simulated computer");
  const view = host.control.find((v) => v.target_id === ids.tgDp1);
  if (!view?.lease) throw new Error("Missing agent lease");
  host.controlSim.renew(view.lease.lease_id, view.lease.generation);
  await advance(3000);
  host.emit({ type: "control_changed", view: { ...view } });
  await advance(2100);
  expect(
    host.control.find((v) => v.target_id === ids.tgDp1)?.lease?.state,
  ).toBe("expired");
});

it("reattaches an existing terminal from its offset without replaying offline input", async () => {
  const host = window.__sim?.host;
  const terminal = host?.terminals[0];
  if (!host || !terminal) throw new Error("Missing terminal");
  const count = host.terminals.length;
  const link = app.platform.terminal(app.profile.profileId);
  const offset = new TerminalOffset();
  let output = "";
  const decoder = new TextDecoder();
  const off = link.listen((event) => {
    if (event.type === "text" && event.frame.type === "attached")
      offset.attach(event.frame.replay_from, event.frame.replay_truncated);
    else if (event.type === "output")
      output += decoder.decode(offset.accept(event.data), { stream: true });
  });
  link.send({
    type: "attach",
    terminal_id: terminal.terminal_id,
    from_offset: null,
  });
  await advance(200);
  link.input(new TextEncoder().encode("echo unique-output\r"));
  await advance(200);
  const before = output;
  window.__sim?.offline();
  expect(link.input(new TextEncoder().encode("echo must-not-run\r"))).toBe(
    false,
  );
  window.__sim?.online();
  link.send({
    type: "attach",
    terminal_id: terminal.terminal_id,
    from_offset: offset.next,
  });
  await advance(200);
  expect(output).toBe(before);
  expect(output).toContain("unique-output");
  expect(output).not.toContain("must-not-run");
  link.send({ type: "detach" });
  off();
  link.close();
  expect(host.terminals.length).toBe(count);
  expect(host.terminals[0]?.state.state).toBe("running");
});

it("a target switch releases held input before relinquishing the old lease", async () => {
  const target = app.session.state
    .get()
    .targets.find((t) => t.target_id === ids.tgHdmi);
  if (!target) throw new Error("Missing display");
  picture.current.revision = target.geometry.revision;
  const { result, rerender } = renderHook(
    ({ targetId }) => useControl(targetId, picture, true),
    { wrapper, initialProps: { targetId: ids.tgHdmi } },
  );
  await advance(100);
  act(() => result.current.arm());
  const receipt = app.session.command(
    {
      command: "request_human_lease",
      target_id: target.target_id,
      geometry_revision: target.geometry.revision,
    },
    { label: "Requesting control" },
  );
  await advance(200);
  await receipt;
  act(() => {
    expect(
      result.current.send({ action: "key_press", key: "ctrl", pressed: true }),
    ).toBe(true);
  });
  rerender({ targetId: ids.tgDp1 });
  await advance(200);
  expect(app.session.state.get().control[ids.tgHdmi]?.lease?.state).toBe("revoked");
  expect(window.__sim?.control.inputs().map((i) => i.action)).toEqual([
    { action: "key_press", key: "ctrl", pressed: true },
    { action: "key_press", key: "ctrl", pressed: false },
    { action: "release_all" },
  ]);
});

it("an input the computer cannot confirm ends control and is never sent again", async () => {
  const target = app.session.state
    .get()
    .targets.find((t) => t.target_id === ids.tgHdmi);
  if (!target) throw new Error("Missing display");
  picture.current.revision = target.geometry.revision;
  const { result } = renderHook(() => useControl(ids.tgHdmi, picture, true), {
    wrapper,
  });
  await advance(100);
  act(() => result.current.arm());
  const granting = app.session.command(
    {
      command: "request_human_lease",
      target_id: target.target_id,
      geometry_revision: target.geometry.revision,
    },
    { label: "Requesting control" },
  );
  await advance(200);
  await granting;
  window.__sim?.control.uncertainInput();
  act(() => {
    expect(result.current.send({ action: "text", text: "once" })).toBe(true);
  });
  await advance(200);
  expect(result.current.error).toMatch(
    /couldn’t confirm whether that input happened/,
  );
  expect(app.session.state.get().control[ids.tgHdmi]?.input_state).toBe(
    "unknown",
  );
  act(() => {
    expect(result.current.send({ action: "text", text: "again" })).toBe(false);
  });
  await advance(200);
  expect(
    window.__sim?.control
      .inputs()
      .filter((i) => i.action.action === "text")
      .map((i) => i.action),
  ).toEqual([{ action: "text", text: "once" }]);
  // Control stays refused until Pause input establishes the barrier.
  const again = app.session
    .command(
      {
        command: "request_human_lease",
        target_id: target.target_id,
        geometry_revision: target.geometry.revision,
      },
      { label: "Requesting control" },
    )
    .then((receipt) => commandResult(app.session, receipt))
    .catch((e: unknown) => e);
  await advance(400);
  expect(await again).toMatchObject({ error: { code: "outcome_unknown" } });
  // The strip keeps that display, with Pause as the way to settle it.
  render(<SafetyStrip />, { wrapper });
  const strip = screen.getByRole("complementary", {
    name: "Safety controls",
  });
  fireEvent.click(within(strip).getByRole("button", { name: /Safety/ }));
  expect(strip.textContent).not.toMatch(/Leave control/);
  const targetRow = within(strip).getByText(/Screen 2 \(HDMI-A-1\) · Input needs checking/, { selector: "p" }).parentElement;
  if (!targetRow) throw new Error("Missing target row");
  const pause = within(targetRow).getByRole("button", { name: /^Pause/ });
  expect((pause as HTMLButtonElement).disabled).toBe(false);
  fireEvent.click(pause);
  await advance(400);
  expect(app.session.state.get().control[ids.tgHdmi]?.input_state).toBe(
    "paused",
  );
  const regained = app.session
    .command(
      {
        command: "request_human_lease",
        target_id: target.target_id,
        geometry_revision: target.geometry.revision,
      },
      { label: "Requesting control" },
    )
    .then((receipt) => commandResult(app.session, receipt));
  await advance(400);
  await expect(regained).resolves.toBeTruthy();
});

it("offline Stop stays named and leaves an inline retry state", async () => {
  render(<SafetyStrip />, { wrapper });
  window.__sim?.offline();
  await advance(200);
  const strip = screen.getByRole("complementary", { name: "Safety controls" });
  const stop = within(strip).getByRole("button", { name: /^Stop ·/ });
  expect((stop as HTMLButtonElement).disabled).toBe(false);
  fireEvent.click(stop);
  expect(strip.textContent).toContain("Stop not confirmed. It will be retried");
  window.__sim?.online();
  await advance(5000);
});

it("the expanded rail names every running task inside its Stop", async () => {
  render(<SafetyStrip />, { wrapper });
  const strip = screen.getByRole("complementary", { name: "Safety controls" });
  fireEvent.click(within(strip).getByRole("button", { name: /Safety/ }));
  const stops = within(strip).getAllByRole("button", { name: /^Stop ·/ });
  expect(stops.length).toBeGreaterThan(1);
  expect(stops.some((button) => stopText(button).includes("staging dashboard"))).toBe(true);
});

it("a hidden view of a display keeps the control another view asked for", async () => {
  const target = app.session.state
    .get()
    .targets.find((t) => t.target_id === ids.tgHdmi);
  if (!target) throw new Error("Missing display");
  picture.current.revision = target.geometry.revision;
  // The read-only view stays mounted behind Desktop, hidden.
  renderHook(() => useControl(ids.tgHdmi, picture, false), { wrapper });
  const { result } = renderHook(() => useControl(ids.tgHdmi, picture, true), {
    wrapper,
  });
  await advance(100);
  act(() => result.current.arm());
  const granting = app.session.command(
    {
      command: "request_human_lease",
      target_id: target.target_id,
      geometry_revision: target.geometry.revision,
    },
    { label: "Requesting control" },
  );
  await advance(200);
  await granting;
  await advance(400);
  expect(result.current.mine).toBe(true);
  expect(
    Object.values(app.session.commands.get()).map((c) => c.body.command),
  ).toEqual(["request_human_lease"]);
  act(() => result.current.leave());
  await advance(200);
  expect(app.session.state.get().control[ids.tgHdmi]?.lease?.state).toBe("revoked");
});

it("a lost Stop receipt stays unknown and retry reuses its admission identity", async () => {
  const { StopStore } = await import("./stops");
  const store = new StopStore(app.session);
  const task = app.session.state.get().tasks[ids.tFlaky];
  if (!task) throw new Error("Missing task");
  const command = vi.spyOn(app.session, "command").mockReturnValue(new Promise(() => {}));
  act(() => store.request(task));
  await advance(8100);
  expect(store.get()[task.task_id]?.phase).toBe("unknown");
  expect(store.get()[task.task_id]?.detail).toContain("Stop not confirmed");
  act(() => app.session.state.set((s) => ({ ...s, lastHeardAt: Date.now() })));
  await advance(10);
  expect(command).toHaveBeenCalledTimes(1);
  const identity = store.get()[task.task_id]?.id;
  act(() => store.request(task));
  expect(command).toHaveBeenCalledTimes(2);
  expect(command.mock.calls[1]?.[1]?.commandId).toBe(identity);
  store.dispose(); command.mockRestore();
});

it("a late not-running receipt corrects an unanswered Stop without a second command", async () => {
  const { StopStore } = await import("./stops");
  const store = new StopStore(app.session);
  const task = app.session.state.get().tasks[ids.tFlaky];
  if (!task) throw new Error("Missing task");
  const command = vi.spyOn(app.session, "command").mockReturnValue(new Promise(() => {}));
  store.request(task);
  await advance(8100);
  const intent = store.get()[task.task_id];
  if (!intent) throw new Error("Missing Stop intent");
  act(() => app.session.commands.set((s) => ({ ...s, [intent.id]: {
    id: intent.id, body: { command: "stop_task", task_id: task.task_id }, issuedAt: intent.at,
    label: "Stop", phase: "rejected", receipt: null, error: { code: "conflict", message: "This task is not running" },
  } })));
  expect(store.get()[task.task_id]?.phase).toBe("not_running");
  expect(command).toHaveBeenCalledTimes(1);
  store.dispose();
});

it("a not-running refusal removes the stale task from safety immediately", async () => {
  const { CommandError } = await import("../../host/session");
  const command = vi.spyOn(app.session, "command").mockRejectedValue(new CommandError({ code: "conflict", message: "This task is not running", detail: null }, null));
  render(<SafetyStrip />, { wrapper });
  const stop = screen.getByRole("button", { name: /^Stop ·/ });
  const name = stopText(stop);
  fireEvent.click(stop);
  await advance(1);
  expect(screen.queryAllByRole("button", { name: /^Stop ·/ }).some((b) => stopText(b) === name)).toBe(false);
  command.mockRestore();
});

it("Start review persists the goal and submits only the chosen eligible target and agent", async () => {
  const { default: ComputerReview } = await import("./ComputerReview");
  const { RouteProvider } = await import("../../shell/Screen");
  const route = { screen: "computer_use_review" as const, projectId: ids.pNotes };
  const draw = () => render(<RouteProvider value={{ tab: "control", route, depth: 1, active: true }}><ComputerReview route={route} /></RouteProvider>, { wrapper });
  const command = vi.spyOn(app.session, "command");
  app.ui.selectTab("control");
  const mounted = draw(); await advance(500);
  expect(command).not.toHaveBeenCalled();
  // The project's default agent, Codex, is chosen with its default model until another agent is.
  const agents = within(screen.getByRole("group", { name: "Eligible agents" }));
  expect(agents.getAllByRole("button").map((b) => b.querySelector("strong")?.textContent).slice(0, 2)).toEqual(["Claude Code", "Codex"]);
  expect(screen.getByRole("button", { name: /Codex/ }).getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByRole("button", { name: /GPT-6-Astra/ }).getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(screen.getByRole("button", { name: /HDMI-A-1/ }));
  fireEvent.click(screen.getByRole("button", { name: /Claude Code/ }));
  fireEvent.change(screen.getByLabelText("Goal"), { target: { value: "Review a chart" } });
  mounted.unmount(); draw();
  expect((screen.getByLabelText("Goal") as HTMLTextAreaElement).value).toBe("Review a chart");
  fireEvent.click(screen.getByRole("button", { name: /^Start computer use$/ }));
  await advance(700);
  expect(command.mock.calls[0]?.[0]).toMatchObject({ command: "create_task", kind: "computer_use", harness: "claude_code", target_id: ids.tgHdmi, prompt: "Review a chart" });
  expect(app.ui.get().stacks.control.at(-1)?.screen).toBe("computer_use");
  command.mockRestore();
});

it("an unanswered Start unlocks request checking and keeps its reviewed goal", async () => {
  const { default: ComputerReview } = await import("./ComputerReview");
  const { RouteProvider } = await import("../../shell/Screen");
  const route = { screen: "computer_use_review" as const, projectId: ids.pNotes };
  app.ui.selectTab("control");
  render(<RouteProvider value={{ tab: "control", route, depth: 1, active: true }}><ComputerReview route={route} /></RouteProvider>, { wrapper });
  await advance(500);
  const command = vi.spyOn(app.session, "command").mockReturnValue(new Promise(() => {}));
  fireEvent.click(screen.getByRole("button", { name: /HDMI-A-1/ }));
  fireEvent.click(screen.getByRole("button", { name: /Claude Code/ }));
  fireEvent.change(screen.getByLabelText("Goal"), { target: { value: "Review a chart" } });
  fireEvent.click(screen.getByRole("button", { name: /^Start computer use$/ }));
  await advance(8100);
  expect(screen.getByRole("alert").textContent).toContain("hasn’t confirmed Start");
  expect((screen.getByRole("button", { name: "Check Start request" }) as HTMLButtonElement).disabled).toBe(false);
  expect((screen.getByLabelText("Goal") as HTMLTextAreaElement).value).toBe("Review a chart");
  expect(command).toHaveBeenCalledTimes(1);
});

it("safety receipts expire on the next control change even while the sheet is closed", async () => {
  const { receiptWindow } = await import("./receiptWindow");
  const { InterventionDetails } = await import("./Feedback");
  receiptWindow(app.session);
  const target = app.session.state.get().control[ids.tgHdmi];
  if (!target) throw new Error("Missing target");
  const record = { seq: 9000, at: Date.now(), taskId: null, targetId: ids.tgHdmi,
    receipt: { kind: "pause_input" as const, requested_at: Date.now(), facts: [], summary: "Input release confirmed by the computer" } };
  act(() => app.session.state.set((s) => ({ ...s, control: { ...s.control, [ids.tgHdmi]: { ...target, input_state: "paused" } }, interventions: [record] })));
  const mounted = render(<InterventionDetails targetId={ids.tgHdmi} />, { wrapper });
  expect(screen.getByText(record.receipt.summary)).toBeTruthy();
  mounted.unmount();
  act(() => app.session.state.set((s) => ({ ...s, control: { ...s.control, [ids.tgHdmi]: { ...target, input_state: "ready" } } })));
  render(<InterventionDetails targetId={ids.tgHdmi} />, { wrapper });
  expect(screen.queryByText(record.receipt.summary)).toBeNull();
  act(() => app.session.state.set((s) => ({ ...s, bootEpoch: "00000000-0000-7000-8000-000000009999" })));
  expect(screen.queryByText(record.receipt.summary)).toBeNull();
  act(() => app.session.state.set((s) => ({ ...s, interventions: [{ ...record, seq: 9001, at: Date.now() }] })));
  expect(screen.getByText(record.receipt.summary)).toBeTruthy();
  await advance(31_000);
  expect(screen.queryByText(record.receipt.summary)).toBeNull();
});

it("a Stop's computer receipt names the held messages it withdrew, as its other outcomes read", async () => {
  const { receiptWindow } = await import("./receiptWindow");
  const { InterventionDetails } = await import("./Feedback");
  receiptWindow(app.session);
  const host = window.__sim!.host;
  const send = (body: Parameters<typeof host.command>[0]["body"]) =>
    host.command({ command_id: crypto.randomUUID(), issued_at: Date.now(), expires_at: null, expected_revision: null, body });
  await send({ command: "send_message", task_id: ids.tDash, text: "Also check the p99 line.", bundle: null, delivery: "next_turn" });
  await send({ command: "send_message", task_id: ids.tDash, text: "Then screenshot the legend.", bundle: null, delivery: "next_turn" });
  await send({ command: "stop_task", task_id: ids.tDash });
  await advance(1500);
  const stop = app.session.state.get().interventions.findLast((r) => r.taskId === ids.tDash && r.receipt.kind === "stop_task");
  if (!stop) throw new Error("Missing Stop receipt");
  // Show the receipt as the computer's newest, as it is right after a Stop.
  act(() => app.session.state.set((s) => ({ ...s, interventions: [{ ...stop, seq: 9100, at: Date.now() }] })));
  render(<InterventionDetails targetId={ids.tgDp1} taskId={ids.tDash} />, { wrapper });
  const outcomes = within(screen.getByRole("region", { name: "Computer receipt" })).getAllByRole("listitem").map((li) => li.textContent);
  expect(outcomes).toContain("Desktop access withdrawn from the agentConfirmed");
  expect(outcomes.at(-1)).toBe("Held messages withdrawn · 2Confirmed");
});

it("active input on another display leads the rail and its accessible name", async () => {
  act(() => app.session.state.set(s => {
    const agent = s.control[ids.tgDp1];
    if (!agent?.lease) throw new Error("Missing lease");
    return { ...s, control: {
      ...s.control,
      [ids.tgDp1]: { ...agent, input_state: "paused", lease: { ...agent.lease, state: "revoked" } },
      [ids.tgHdmi]: { ...agent, target_id: ids.tgHdmi, lease: { ...agent.lease, target_id: ids.tgHdmi } },
    } };
  }));
  render(<SafetyStrip />, { wrapper });
  const toggle = screen.getByRole("button", { name: /Safety · Screen 2 \(HDMI-A-1\) · Agent is in control/ });
  expect(toggle.textContent).toContain("Screen 2 (HDMI-A-1) · Agent is in control");
  // The one line under the task's name keeps to the display that needs a look.
  expect(document.querySelector(".safety-rail__line--warn")?.textContent).toBe("Agent paused · Screen 1 (DP-1)");
});

it("keeps the locked state and named Stop visible in both safety rail views", async () => {
  act(() => window.__sim?.control.lock(true, ids.tgDp1));
  await advance(150);
  render(<SafetyStrip />, { wrapper });
  expect(screen.getAllByText(/Computer locked/).length).toBeGreaterThan(0);
  expect(screen.getAllByRole("button", { name: /^Stop ·/ }).length).toBeGreaterThan(0);
  fireEvent.click(screen.getByRole("button", { name: /Safety ·.*Computer locked/ }));
  expect(screen.getAllByText(/Computer locked/).length).toBeGreaterThan(0);
  expect(screen.getAllByRole("button", { name: /^Stop ·/ }).length).toBeGreaterThan(0);
});

it("shows another display's lock only once the rail is expanded", async () => {
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  render(<SafetyStrip />, { wrapper });
  expect(stopText(screen.getByRole("button", { name: /^Stop · Check the staging/ }))).toContain("DP-1");
  expect(screen.queryByText(/Computer locked/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /^Safety ·/ }));
  expect(screen.getByText("Screen 2 (HDMI-A-1) · Computer locked")).toBeTruthy();
});

it("after a Stop the expanded rail keeps a Stopped row until the receipt, then lets it go", async () => {
  render(<SafetyStrip />, { wrapper });
  const task = app.session.state.get().tasks[ids.tFlaky]!;
  fireEvent.click(screen.getByRole("button", { name: /^Safety ·/ }));
  fireEvent.click(screen.getByRole("button", { name: `Stop · ${task.title} · not using a display` }));
  await advance(2000);
  expect(screen.queryByRole("button", { name: `Stop · ${task.title} · not using a display` })).toBeNull();
  const row = screen.getByText(new RegExp(`^Stopped · ${task.title} · `));
  expect(row.textContent).toBe(`Stopped · ${task.title} · ${app.session.state.get().interventions.at(-1)!.receipt.summary}`);
  await advance(10_000);
  expect(screen.queryByText(/^Stopped ·/)).toBeNull();
});

it("a Stopped row waits for the computer's receipt and goes once the Stop has had its time", async () => {
  const { STOP_WAIT_MS } = await import("./stops");
  const task = app.session.state.get().tasks[ids.tFlaky]!;
  const run = task.current_run!;
  const receipt = { kind: "stop_task" as const, requested_at: Date.now(), facts: [], summary: "The agent confirmed Stop." };
  const stopped = () => act(() => {
    app.session.commands.set((s) => ({ ...s, [task.task_id]: { id: task.task_id, body: { command: "stop_task", task_id: task.task_id },
      issuedAt: Date.now(), label: "Stop", phase: "accepted", receipt: null, error: null } }));
    app.session.state.set((s) => ({ ...s, tasks: { ...s.tasks, [task.task_id]: { ...task, state: "interrupted" } },
      runs: { ...s.runs, [run]: { ...s.runs[run]!, state: "interrupted", ended_at: Date.now(), holds_writer: false } } }));
  });
  render(<SafetyStrip />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: /^Safety ·/ }));
  stopped();
  expect(screen.getByText(`Stopped · ${task.title} · Waiting for the computer’s receipt…`)).toBeTruthy();
  await advance(STOP_WAIT_MS);
  expect(screen.queryByText(/^Stopped ·/)).toBeNull();
  // An earlier Stop's receipt does not answer this one; its own receipt does.
  act(() => app.session.state.set((s) => ({ ...s, interventions: [{ seq: 9000, at: Date.now() - 60_000, taskId: task.task_id, targetId: null, receipt }] })));
  stopped();
  expect(screen.getByText(`Stopped · ${task.title} · Waiting for the computer’s receipt…`)).toBeTruthy();
  act(() => app.session.state.set((s) => ({ ...s,
    interventions: [...s.interventions, { seq: 9001, at: Date.now(), taskId: task.task_id, targetId: null, receipt }],
    timelines: { ...s.timelines, [task.task_id]: { hasMore: false, loaded: true, items: [{ seq: 9001, at: Date.now(), run_id: run, item: { kind: "receipt", receipt } }] } } })));
  expect(screen.getByText(`Stopped · ${task.title} · The agent confirmed Stop.`)).toBeTruthy();
  await advance(10_000);
  expect(screen.queryByText(/^Stopped ·/)).toBeNull();
});
