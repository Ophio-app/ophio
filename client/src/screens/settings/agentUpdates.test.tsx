// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { capabilities, harnesses } from "../../dev/sim/scenario";
import { Toasts } from "../../design/Toast";
import { AgentUpdatesSim } from "../../dev/sim/agentUpdates";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import { AgentUpdates } from "./AgentUpdates";
import { CHECKING_POLL_MS } from "./useAgentUpdates";

let sim: AgentUpdatesSim;
let state: HostState;
let online = true;
const query = vi.fn(async () => ({ kind: "agent_updates", view: structuredClone(sim.view) }));
const command = vi.fn(async (c: { enabled: boolean }, o: { expectedRevision: number }) => { sim.set(c.enabled, o.expectedRevision); return { status: "accepted" }; });
// One session for the whole run, as the app keeps one per computer.
const session = { query, command };
vi.mock("../../app/context", () => ({
  useApp: () => ({ session, profile: { name: "Test computer" } }),
  useHost: <T,>(select: (s: HostState) => T) => select(state),
  useLink: () => ({ state: online ? "online" : "offline" }),
  describeError: (e: { message?: string }) => e.message ?? "Request failed",
}));
const grants = { view: true, tasks: true, admin: true, control_input: false, terminal: false, approvals: true, computer_use: false, library_read: true, library_write: false, transfers: false, accounts: false };
beforeEach(() => {
  sim = new AgentUpdatesSim();
  online = true;
  state = { ...emptyHostState, capabilities: capabilities(), harnesses, grants };
  query.mockClear(); command.mockClear();
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

const toggle = () => screen.getByRole("checkbox", { name: "Keep Claude Code and Codex up to date" }) as HTMLInputElement;

it("turns keeping the agents up to date off and on, with the revision it was shown", async () => {
  render(<AgentUpdates />);
  await waitFor(() => expect(toggle().checked).toBe(true));
  expect(screen.getByText(/Claude Code 2\.4\.1 · Checked/)).toBeTruthy();
  expect(screen.getByText("Couldn't reach the update site to check for a new version.")).toBeTruthy();
  fireEvent.click(toggle());
  await waitFor(() => expect(toggle().checked).toBe(false));
  expect(command).toHaveBeenLastCalledWith({ command: "set_agent_updates", enabled: false }, expect.objectContaining({ expectedRevision: 1 }));
  expect(screen.queryByText(/2\.4\.1/)).toBeNull();
  fireEvent.click(toggle());
  await waitFor(() => expect(command).toHaveBeenLastCalledWith({ command: "set_agent_updates", enabled: true }, expect.objectContaining({ expectedRevision: 2 })));
  expect(toggle().checked).toBe(true);
});

it("shows the choice on the tap and stays usable while the computer answers; a refusal puts it back", async () => {
  render(<><AgentUpdates /><Toasts /></>);
  await waitFor(() => expect(toggle().checked).toBe(true));
  let refuse!: (e: Error) => void;
  command.mockImplementationOnce(() => new Promise((_, reject) => { refuse = reject; }));
  fireEvent.click(toggle());
  expect(toggle().checked).toBe(false);
  expect(toggle().disabled).toBe(false);
  expect(screen.queryByText(/2\.4\.1/)).toBeNull();
  await act(async () => { refuse(new Error("The connection dropped.")); });
  expect(toggle().checked).toBe(true);
  expect(screen.getByText("Couldn’t change whether the agents are kept up to date. The connection dropped.")).toBeTruthy();
});

it("looks again while a check is under way, until it ends", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  sim.view = { ...sim.view, checking: true };
  render(<AgentUpdates />);
  await waitFor(() => expect(screen.getByRole("status").textContent).toBe("Checking for new versions…"));
  const looks = query.mock.calls.length;
  sim.view = { ...sim.view, checking: false, agents: sim.view.agents.map((a) => ({ ...a, version: a.harness === "codex" ? "0.161.0" : a.version, problem: null })) };
  await act(async () => { await vi.advanceTimersByTimeAsync(CHECKING_POLL_MS); });
  await waitFor(() => expect(screen.getByText(/Codex 0\.161\.0 · Checked/)).toBeTruthy());
  expect(query.mock.calls.length).toBe(looks + 1);
  expect(screen.queryByRole("status")).toBeNull();
  await act(async () => { await vi.advanceTimersByTimeAsync(CHECKING_POLL_MS * 3); });
  expect(query.mock.calls.length).toBe(looks + 1);
});

it("keeps looking after a look fails while a check is under way", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  sim.view = { ...sim.view, checking: true };
  render(<AgentUpdates />);
  await waitFor(() => expect(screen.getByRole("status").textContent).toBe("Checking for new versions…"));
  const looks = query.mock.calls.length;
  query.mockRejectedValueOnce(new Error("The connection dropped."));
  await act(async () => { await vi.advanceTimersByTimeAsync(CHECKING_POLL_MS); });
  await waitFor(() => expect(screen.getByText("The connection dropped.")).toBeTruthy());
  expect(query.mock.calls.length).toBe(looks + 1);
  sim.view = { ...sim.view, checking: false };
  await act(async () => { await vi.advanceTimersByTimeAsync(CHECKING_POLL_MS); });
  await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
  expect(query.mock.calls.length).toBe(looks + 2);
  expect(screen.queryByText("The connection dropped.")).toBeNull();
});

it("can't be changed without Manage access, offline, or where the agents publish no programs", async () => {
  state = { ...state, grants: { ...grants, admin: false } };
  const { unmount } = render(<AgentUpdates />);
  await waitFor(() => expect(toggle().checked).toBe(true));
  expect(toggle().disabled).toBe(true);
  expect(screen.getByText("This device needs the Manage access permission to change this.")).toBeTruthy();
  unmount();

  state = { ...state, grants };
  sim.view = { ...sim.view, enabled: false, supported: false, agents: [] };
  const again = render(<AgentUpdates />);
  await waitFor(() => expect(screen.getByText(/don’t publish programs for this computer’s system/)).toBeTruthy());
  expect(toggle().disabled).toBe(true);
  again.unmount();

  online = false;
  render(<AgentUpdates />);
  expect(toggle().disabled).toBe(true);
  expect(screen.getByText("Offline. Connect to change this.")).toBeTruthy();
});

it("shows nothing for a computer that can't keep the agents up to date", () => {
  state = { ...state, capabilities: { ...capabilities(), features: capabilities().features.filter((f) => f !== "agent_updates_v1") } };
  const { container } = render(<AgentUpdates />);
  expect(container.textContent).toBe("");
  expect(query).not.toHaveBeenCalled();
});
